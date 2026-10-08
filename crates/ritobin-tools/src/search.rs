//! Searches bin documents for entries, classes, property names, values and dependencies.
//!
//! [`Matcher::compile`] resolves a [`Query`] against the hashtables once. For each kind of hash
//! it stores the set of hashes whose name matches the pattern. [`scan`] then tests a hash with
//! one set lookup and reads no hashtable, so it can run on a worker thread.
//!
//! [`scan`] returns one [`Hit`] per match. A hit stores hashes and values. [`Row::new`] resolves
//! their names for output.

use std::{borrow::Cow, collections::HashSet, fmt::Write as _, io::Cursor, time::Instant};

use clap::ValueEnum;
use ltk_hash::{BinHash, Hash as _, WadHash};
use ltk_meta::{
    BinFile, BinKind, BinObject, BinStream, Error as BinError, PropertyKind as Kind, PropertyPatch,
    PropertyValueEnum,
    path::{PropertyPath, ValuePath},
    walk::{ChildSegment, Leaf, Node, TreeKind as _, TreeValue, Visit, Visitor},
};
use ltk_mimir_cache::Table;
use ltk_ritobin::{RitoType, RitobinName as _};
use miette::Result;
use regex::{Regex, RegexBuilder};
use serde::Serialize;

use crate::{
    document::{Document, ReadOptions},
    hashes::{BinHashes, GameNames, WadPaths, format_hash},
};

/// A part of a bin that a search tests against the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum Target {
    /// Object paths. In a PTCH file also the objects that the records and the delete list
    /// address
    #[value(name = "entries", alias = "entry")]
    Entry,
    /// Classes of objects and of nested structs
    #[value(name = "classes", alias = "class")]
    Class,
    /// Property names
    #[value(name = "fields", alias = "field")]
    Field,
    /// Values and map keys
    #[value(name = "values", alias = "value")]
    Value,
    /// The dependency list of a bin
    #[value(name = "dependencies", alias = "dependency")]
    Dependency,
}

/// The part of a [`Hit`] that matched the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Matched {
    /// The path of the object.
    Entry,
    /// The class of the object or of a nested struct.
    Class,
    /// The name of the property.
    Field,
    /// The key of the map entry.
    Key,
    /// The value.
    Value,
    /// An item of the dependency list.
    Dependency,
    /// An item of the delete list of a `PTCH` file.
    Deleted,
}

/// The pattern of a [`Query`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Pattern {
    /// No pattern. Every value passes, and only values are searched.
    #[default]
    Any,
    /// Text that is matched literally. It is also compared as a hash and as a number.
    Literal(String),
    /// A regular expression that is matched against the text of each item.
    Regex(String),
}

/// A search request: the pattern, the comparison options and the filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub pattern: Pattern,
    /// If `true`, the pattern must match the whole text. Otherwise it matches a substring.
    pub exact: bool,
    /// If `true`, text is compared with regard to case.
    pub case_sensitive: bool,
    /// The parts of a bin to search. Empty selects all parts.
    pub targets: Vec<Target>,
    /// The value kinds to search. Empty selects all kinds.
    pub kinds: Vec<Kind>,
    /// The name hash of the only property whose values are searched. `None` selects every
    /// property.
    pub field: Option<BinHash>,
    /// The class of the only objects and structs whose properties are searched. `None` selects
    /// every class.
    pub class: Option<BinHash>,
    /// The path hash of the only object that is searched. `None` selects every object.
    pub object: Option<BinHash>,
    /// The class of the only objects that are searched. `None` selects every class.
    pub object_class: Option<BinHash>,
}

/// The parts of a bin that a [`Matcher`] searches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Targets {
    entry: bool,
    class: bool,
    field: bool,
    value: bool,
    dependency: bool,
}

impl Targets {
    /// Returns the parts that `query` searches: the parts it selects, reduced to the parts that
    /// its filters apply to. Fails if no part remains.
    fn of(query: &Query) -> Result<Self> {
        let selected = |target| query.targets.is_empty() || query.targets.contains(&target);
        let all = Self {
            entry: selected(Target::Entry),
            class: selected(Target::Class),
            field: selected(Target::Field),
            value: selected(Target::Value),
            // The dependency list is not part of an object, so an object filter excludes it.
            dependency: selected(Target::Dependency)
                && query.object.is_none()
                && query.object_class.is_none(),
        };

        let values_only =
            !query.kinds.is_empty() || query.field.is_some() || query.pattern == Pattern::Any;
        let targets = match (values_only, query.class) {
            (true, _) => Self {
                value: all.value,
                ..Self::default()
            },
            (false, Some(_)) => Self {
                field: all.field,
                value: all.value,
                ..Self::default()
            },
            (false, None) => all,
        };
        if targets == Self::default() {
            miette::bail!(
                "--in selects no part that the other options search. --type, --field and --values search only values. --class searches only property names and values"
            );
        }
        Ok(targets)
    }
}

/// A number or a boolean that a literal pattern is parsed as.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Scalar {
    Bool(bool),
    Int(i128),
    Float(f64),
}

impl Scalar {
    /// Parses `text` as a boolean, an integer or a decimal number. Returns `None` for other
    /// text.
    fn parse(text: &str) -> Option<Self> {
        if let Ok(value) = text.parse() {
            return Some(Self::Bool(value));
        }
        if let Ok(value) = text.parse() {
            return Some(Self::Int(value));
        }
        text.parse().ok().map(Self::Float)
    }

    /// Returns `true` if the numeric or boolean `leaf` equals this value. A vector, a matrix or
    /// a color is equal if one of its components is equal. Returns `false` for a leaf of another
    /// kind.
    fn equals(self, leaf: &Leaf<'_>) -> bool {
        let int = |value: i128| match self {
            Self::Int(wanted) => wanted == value,
            Self::Float(wanted) => wanted == value as f64,
            Self::Bool(_) => false,
        };
        // The pattern is rounded to `f32` first. A decimal such as 0.1 has no exact `f32`
        // value, so comparing as `f64` would not match the value that the bin stores.
        let float = |value: f32| {
            let wanted = match self {
                Self::Int(wanted) => wanted as f32,
                Self::Float(wanted) => wanted as f32,
                Self::Bool(_) => return false,
            };
            wanted == value || (wanted.is_nan() && value.is_nan())
        };
        match *leaf {
            Leaf::Bool(value) | Leaf::Flag(value) => self == Self::Bool(value),
            Leaf::I8(value) => int(value.into()),
            Leaf::U8(value) => int(value.into()),
            Leaf::I16(value) => int(value.into()),
            Leaf::U16(value) => int(value.into()),
            Leaf::I32(value) => int(value.into()),
            Leaf::U32(value) => int(value.into()),
            Leaf::I64(value) => int(value.into()),
            Leaf::U64(value) => int(value.into()),
            Leaf::F32(value) => float(value),
            Leaf::Vector2(value) => value.to_array().into_iter().any(float),
            Leaf::Vector3(value) => value.to_array().into_iter().any(float),
            Leaf::Vector4(value) => value.to_array().into_iter().any(float),
            Leaf::Matrix44(value) => value.to_cols_array().into_iter().any(float),
            Leaf::Color(value) => [value.r, value.g, value.b, value.a]
                .into_iter()
                .any(|component| int(component.into())),
            _ => false,
        }
    }
}

/// A [`Query`] compiled for scanning.
pub struct Matcher {
    /// The pattern as a regular expression. `None` if the query has no pattern.
    text: Option<Regex>,
    /// The literal pattern as a number or a boolean.
    scalar: Option<Scalar>,
    /// `true` if the pattern is a regular expression. Numeric values are then matched by their
    /// text.
    regex: bool,
    /// The object paths that match. Also tested for `link` values.
    entries: HashSet<BinHash>,
    classes: HashSet<BinHash>,
    fields: HashSet<BinHash>,
    /// The hashes that match `hash` and `link` values.
    hashes: HashSet<BinHash>,
    /// The 8-byte values that match `hash` values. No hashtable has names for 8-byte
    /// hashes, so only a `0x` literal of more than 8 digits matches one.
    wide_hashes: HashSet<u64>,
    /// The chunk hashes that match `file` values.
    files: HashSet<WadHash>,
    targets: Targets,
    kinds: Vec<Kind>,
    field: Option<BinHash>,
    class: Option<BinHash>,
    object: Option<BinHash>,
    object_class: Option<BinHash>,
}

impl Matcher {
    /// Compiles `query`. Reads the names of `hashes` and `paths` and stores the hashes of the
    /// names that match the pattern.
    ///
    /// A literal pattern is also hashed as a bin name and as a chunk path, and `0x` hex is
    /// parsed as a hash. These hashes match even if no hashtable has a name for them.
    ///
    /// Fails if the pattern is an invalid regular expression, or if the filters of the query
    /// leave no part of a bin to search.
    pub fn compile(query: &Query, hashes: &BinHashes, paths: &WadPaths) -> Result<Self> {
        let targets = Targets::of(query)?;
        let (text, literal) = match &query.pattern {
            Pattern::Any => (None, None),
            Pattern::Literal(literal) => (
                Some(regex(&regex::escape(literal), query)?),
                Some(literal.as_str()),
            ),
            Pattern::Regex(pattern) => (Some(regex(pattern, query)?), None),
        };

        let hex = literal.and_then(|literal| {
            literal
                .strip_prefix("0x")
                .or_else(|| literal.strip_prefix("0X"))
        });
        let (mut bin_hashes, mut file_hashes) = (Vec::new(), Vec::new());
        if let Some(literal) = literal {
            match hex {
                // Up to 8 digits are a bin hash. More digits are a chunk hash or an 8-byte
                // `hash` value.
                Some(digits) if digits.len() <= 8 => {
                    bin_hashes.extend(u32::from_str_radix(digits, 16).map(BinHash));
                }
                Some(digits) => {
                    file_hashes.extend(u64::from_str_radix(digits, 16).map(WadHash));
                }
                None => {
                    bin_hashes.push(BinHash::hash_str(literal));
                    file_hashes.push(WadHash::hash_str(literal));
                }
            }
        }

        // The tables are not read in two cases, because the hashes above already select the
        // matching names. The first case is a literal that was parsed as a `0x` hash. The second
        // case is a literal that must match the whole name without regard to case: the hash of a
        // name ignores case, so a name matches exactly when its hash equals the hash of the
        // literal.
        let read_names = match hex {
            Some(_) => bin_hashes.is_empty() && file_hashes.is_empty(),
            None => literal.is_none() || !query.exact || query.case_sensitive,
        };
        let names = |table: Table, wanted: bool| {
            let mut matching: HashSet<BinHash> = HashSet::new();
            if !wanted {
                return matching;
            }
            matching.extend(&bin_hashes);
            if let Some(text) = text.as_ref().filter(|_| read_names) {
                hashes.for_each_name(table, |hash, name| {
                    if text.is_match(name) {
                        matching.insert(hash);
                    }
                });
            }
            matching
        };

        let value_kind =
            |kind: Kind| targets.value && (query.kinds.is_empty() || query.kinds.contains(&kind));
        let links = value_kind(Kind::ObjectLink);
        let wide_hashes: HashSet<u64> = match hex {
            Some(digits) if digits.len() > 8 && value_kind(Kind::Hash) => {
                file_hashes.iter().map(|hash| hash.0).collect()
            }
            _ => HashSet::new(),
        };
        let file_paths = || {
            let mut matching: HashSet<WadHash> = HashSet::new();
            if !value_kind(Kind::WadChunkLink) {
                return matching;
            }
            matching.extend(&file_hashes);
            if let Some(text) = text.as_ref().filter(|_| read_names) {
                paths.for_each_path(|hash, path| {
                    if text.is_match(path) {
                        matching.insert(hash);
                    }
                });
            }
            matching
        };

        // Each table is read on its own thread. Reading the names is the largest fixed cost of
        // a search, and the tables are independent.
        let started = Instant::now();
        let (entries, classes, fields, hashes, files) = std::thread::scope(|scope| {
            let entries = scope.spawn(|| names(Table::BinEntries, targets.entry || links));
            let classes = scope.spawn(|| names(Table::BinTypes, targets.class));
            let fields = scope.spawn(|| names(Table::BinFields, targets.field));
            let hashes = scope.spawn(|| names(Table::BinHashes, value_kind(Kind::Hash) || links));
            let files = file_paths();
            let join = |thread: std::thread::ScopedJoinHandle<'_, HashSet<BinHash>>| {
                thread
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            };
            (
                join(entries),
                join(classes),
                join(fields),
                join(hashes),
                files,
            )
        });
        tracing::debug!(
            "Matched {} entry names, {} class names, {} field names, {} hash names and {} file paths in {:.2} s",
            entries.len(),
            classes.len(),
            fields.len(),
            hashes.len(),
            files.len(),
            started.elapsed().as_secs_f32()
        );

        Ok(Self {
            entries,
            classes,
            fields,
            hashes,
            wide_hashes,
            files,
            scalar: literal.and_then(Scalar::parse),
            regex: matches!(query.pattern, Pattern::Regex(_)),
            text,
            targets,
            kinds: query.kinds.clone(),
            field: query.field,
            class: query.class,
            object: query.object,
            object_class: query.object_class,
        })
    }

    /// Returns `true` if the object with path hash `object` and class `class` passes the object
    /// filters.
    fn wants_object(&self, object: BinHash, class: BinHash) -> bool {
        self.object.is_none_or(|wanted| wanted == object)
            && self.object_class.is_none_or(|wanted| wanted == class)
    }

    /// Returns `true` if the search reads the objects of a bin. Returns `false` if it reads
    /// only the dependency list.
    fn reads_objects(&self) -> bool {
        self.targets.entry || self.targets.class || self.targets.field || self.targets.value
    }

    /// Returns `true` if the value `leaf` matches. `buffer` is scratch space for the text of a
    /// numeric value.
    fn leaf(&self, leaf: &Leaf<'_>, buffer: &mut String) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&leaf.kind()) {
            return false;
        }
        let Some(text) = &self.text else {
            return true;
        };
        match leaf {
            Leaf::String(value) => text.is_match(value),
            Leaf::Hash(hash) => match hash.try_as_bin_hash() {
                Some(hash) => self.hashes.contains(&hash),
                None => self.wide_hashes.contains(&hash.as_u64()),
            },
            Leaf::Link(hash) => self.hashes.contains(hash) || self.entries.contains(hash),
            Leaf::File(hash) => self.files.contains(hash),
            Leaf::None => false,
            number => match self.scalar {
                Some(scalar) => scalar.equals(number),
                None if self.regex => {
                    buffer.clear();
                    write_plain(number, buffer);
                    text.is_match(buffer)
                }
                None => false,
            },
        }
    }
}

/// Builds the regular expression for `pattern` with the case and whole-text options of `query`.
fn regex(pattern: &str, query: &Query) -> Result<Regex> {
    let pattern = match query.exact {
        true => format!("^(?:{pattern})$"),
        false => pattern.to_owned(),
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!query.case_sensitive)
        .build()
        .map_err(|error| miette::miette!("The regular expression is invalid: {error}"))
}

/// One match in a bin.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// The parts that matched. A property whose name and value both match is one hit.
    pub matched: Vec<Matched>,
    /// The path hash and the class of the object that contains the hit. `None` for a
    /// dependency.
    pub object: Option<(BinHash, BinHash)>,
    pub at: At,
}

/// The location of a [`Hit`].
#[derive(Debug, Clone, PartialEq)]
pub enum At {
    /// The object itself. Its path or its class matched.
    Object,
    /// An item of the dependency list of the bin.
    Dependency(String),
    /// An item of the delete list of a `PTCH` file: the path hash of the deleted object.
    Deleted(BinHash),
    /// A property, a list item or a map entry of the object, or the value of a record of a
    /// `PTCH` file.
    Value {
        /// The path of the value inside the object.
        path: ValuePath,
        kind: Kind,
        /// The item kind of a list or an option, or the value kind of a map.
        item_kind: Option<Kind>,
        /// The key kind of a map.
        key_kind: Option<Kind>,
        shown: Shown,
        /// The record that contains the value. `None` for a value of an object. If it is set,
        /// the first segment of `path` is the last property name of the record path.
        record: Option<RecordAt>,
    },
}

/// A record of a `PTCH` file.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordAt {
    /// The position of the record in the file, counting from 0.
    pub index: usize,
    /// The property path of the record.
    pub path: PropertyPath,
}

/// The content of a matched value that is printed.
#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    /// A value without nested values.
    Leaf(PropertyValueEnum),
    /// The class of a struct. Class 0 is the null struct.
    Class(BinHash),
    /// The item count of a list, an option or a map.
    Count(usize),
}

/// Walks the objects of a bin and counts the hits. Also collects them if `collect` is set.
struct Scan<'m> {
    matcher: &'m Matcher,
    /// The number of hits.
    count: usize,
    /// If `false`, the hits are only counted. No path and no value is copied.
    collect: bool,
    hits: Vec<Hit>,
    /// The maximum number of hits. The walk stops when it is reached.
    limit: usize,
    /// The class of the object that is being walked.
    object_class: BinHash,
    /// The record whose value is being walked. `None` while an object is walked.
    record: Option<RecordAt>,
    /// `true` if the object of the record that is being walked matches the pattern.
    record_entry: bool,
    /// `true` if the walk of the current record added a hit on the value of the record itself.
    record_root: bool,
    buffer: String,
}

impl<'m> Scan<'m> {
    fn new(matcher: &'m Matcher, limit: usize, collect: bool) -> Self {
        Self {
            matcher,
            count: 0,
            collect,
            hits: Vec::new(),
            limit,
            object_class: BinHash(0),
            record: None,
            record_entry: false,
            record_root: false,
            buffer: String::new(),
        }
    }

    fn is_full(&self) -> bool {
        self.count >= self.limit
    }

    /// Returns `Abort` if the hit limit is reached, otherwise `Continue`.
    fn proceed(&self) -> Visit {
        match self.is_full() {
            true => Visit::Abort,
            false => Visit::Continue,
        }
    }

    /// Adds a hit for each item of `dependencies` that matches.
    fn dependencies(&mut self, dependencies: &[String]) {
        let Some(text) = self
            .matcher
            .text
            .as_ref()
            .filter(|_| self.matcher.targets.dependency)
        else {
            return;
        };
        for dependency in dependencies {
            if self.is_full() || !text.is_match(dependency) {
                continue;
            }
            self.count += 1;
            if self.collect {
                self.hits.push(Hit {
                    matched: vec![Matched::Dependency],
                    object: None,
                    at: At::Dependency(dependency.clone()),
                });
            }
        }
    }

    /// Adds a hit for each item of the delete list of a `PTCH` file whose object matches the
    /// pattern.
    fn deleted(&mut self, deleted: &[BinHash]) {
        let matcher = self.matcher;
        // The delete list stores no class, so a class filter excludes every item.
        if !matcher.targets.entry || matcher.object_class.is_some() {
            return;
        }
        for object in deleted {
            if self.is_full()
                || !matcher.entries.contains(object)
                || matcher.object.is_some_and(|wanted| wanted != *object)
            {
                continue;
            }
            self.count += 1;
            if self.collect {
                self.hits.push(Hit {
                    matched: vec![Matched::Deleted],
                    object: None,
                    at: At::Deleted(*object),
                });
            }
        }
    }

    /// Tests the record at position `index` of a `PTCH` file: the object that it addresses, the
    /// last property name of its path, and its value.
    ///
    /// The value is walked as the only property of a temporary object. The items of a list and
    /// the properties of a struct are therefore tested like the values of an object.
    fn record(&mut self, index: usize, record: &PropertyPatch) -> Result<(), BinError> {
        let matcher = self.matcher;
        // A record stores no class of the object that it addresses, so a class filter excludes
        // every record.
        if matcher.object_class.is_some()
            || matcher
                .object
                .is_some_and(|wanted| wanted != record.object_hash)
        {
            return Ok(());
        }
        let Some(last) = record.path.segments().last() else {
            return Ok(());
        };
        let field = last.name_hash();
        let object = BinObject::builder(record.object_hash, BinHash(0))
            .property(field, record.value.clone())
            .build();

        self.record = Some(RecordAt {
            index,
            path: record.path.clone(),
        });
        self.record_entry = matcher.targets.entry && matcher.entries.contains(&record.object_hash);
        self.record_root = false;
        let first = self.hits.len();
        let walked = object.walk(self);
        let at = self.record.take();
        walked?;

        // The object of the record matches, and no hit is on the value of the record itself.
        // The record gets a hit of its own, before the hits inside its value.
        if self.record_entry && !self.record_root {
            self.count += 1;
            if self.collect {
                let mut path = ValuePath::new();
                path.push_field(field, BinHash(0));
                self.hits.insert(
                    first,
                    Hit {
                        matched: vec![Matched::Entry],
                        object: Some((record.object_hash, BinHash(0))),
                        at: value_at(path, &record.value, at)?,
                    },
                );
                self.hits.truncate(self.limit);
            }
            self.count = self.count.min(self.limit);
        }
        Ok(())
    }

    /// Adds a hit for `value`, which is the property `field` of `node`, or the item of that
    /// property at `child`.
    fn value_hit<'a, V: TreeValue<'a>>(
        &mut self,
        mut matched: Vec<Matched>,
        node: &Node<'_, 'a, V>,
        field: BinHash,
        child: Option<ChildSegment<V>>,
        value: V,
    ) -> Result<(), BinError> {
        // A hit on the value of a record itself also reports a match of the object that the
        // record addresses.
        if self.record.is_some() && node.is_root() && child.is_none() {
            self.record_root = true;
            if self.record_entry {
                matched.insert(0, Matched::Entry);
            }
        }
        self.count += 1;
        if !self.collect {
            return Ok(());
        }
        let mut path = node.to_value_path()?;
        path.push_field(field, node.class_hash());
        match child {
            Some(ChildSegment::Index(index)) => path.push_index(index),
            Some(ChildSegment::Key(key)) => path.push_key(key.map_key()?),
            None => {}
        }

        self.hits.push(Hit {
            matched,
            object: Some((node.object_hash(), self.object_class)),
            at: value_at(path, value, self.record.clone())?,
        });
        Ok(())
    }
}

/// Returns the location of a hit on `value`, which is at `path`. `record` is the record of a
/// `PTCH` file that contains the value.
fn value_at<'a, V: TreeValue<'a>>(
    path: ValuePath,
    value: V,
    record: Option<RecordAt>,
) -> Result<At, BinError> {
    let declaration = value.declaration()?;
    let shown = match (value.as_leaf()?, declaration.class) {
        (Some(_), _) => Shown::Leaf(value.to_value()?),
        (None, Some(class)) => Shown::Class(class),
        (None, None) => Shown::Count(declaration.count.unwrap_or(0)),
    };
    Ok(At::Value {
        path,
        kind: declaration.kind,
        item_kind: declaration.item_kind,
        key_kind: declaration.key_kind,
        shown,
        record,
    })
}

impl<'a, V: TreeValue<'a>> Visitor<'a, V> for Scan<'_> {
    type Error = BinError;

    /// Tests the path and the class of an object. A nested node is tested by `enter_property`
    /// of the property that contains it, which also knows its path and its kind.
    fn enter_node(&mut self, node: &Node<'_, 'a, V>) -> Result<Visit, BinError> {
        if !node.is_root() {
            return Ok(Visit::Continue);
        }
        let matcher = self.matcher;
        let (object, class) = (node.object_hash(), node.class_hash());
        self.object_class = class;
        // The root node of a record walk is a temporary object. `record` tests the object that
        // the record addresses.
        if self.record.is_some() {
            return Ok(self.proceed());
        }

        let mut matched = Vec::new();
        if matcher.targets.entry && matcher.entries.contains(&object) {
            matched.push(Matched::Entry);
        }
        if matcher.targets.class && matcher.classes.contains(&class) {
            matched.push(Matched::Class);
        }
        if !matched.is_empty() {
            self.count += 1;
            if self.collect {
                self.hits.push(Hit {
                    matched,
                    object: Some((object, class)),
                    at: At::Object,
                });
            }
        }
        Ok(self.proceed())
    }

    /// Tests the name and the value of a property. The walk descends only into nodes, so this
    /// function also tests the items of a list, an option or a map.
    fn enter_property(
        &mut self,
        field: BinHash,
        value: V,
        node: &Node<'_, 'a, V>,
    ) -> Result<Visit, BinError> {
        let matcher = self.matcher;
        if matcher
            .class
            .is_some_and(|wanted| wanted != node.class_hash())
        {
            return Ok(Visit::Continue);
        }
        let values = matcher.targets.value && matcher.field.is_none_or(|wanted| wanted == field);
        let field_hit = matcher.targets.field && matcher.fields.contains(&field);
        let mut matched = Vec::new();
        if field_hit {
            matched.push(Matched::Field);
        }

        if let Some(leaf) = value.as_leaf()? {
            if values && matcher.leaf(&leaf, &mut self.buffer) {
                matched.push(Matched::Value);
            }
            if !matched.is_empty() {
                self.value_hit(matched, node, field, None, value)?;
            }
            return Ok(self.proceed());
        }

        let declaration = value.declaration()?;
        let is_class = |class: Option<BinHash>| {
            matcher.targets.class && class.is_some_and(|class| matcher.classes.contains(&class))
        };
        if is_class(declaration.class) {
            matched.push(Matched::Class);
        }
        if !matched.is_empty() {
            self.value_hit(matched, node, field, None, value)?;
        }

        let holds_nodes = declaration.item_kind.is_some_and(|kind| kind.is_node());
        let reads_items = match holds_nodes {
            true => matcher.targets.class || (values && declaration.key_kind.is_some()),
            false => values && declaration.item_kind.is_some(),
        };
        if !reads_items {
            return Ok(self.proceed());
        }
        for child in value.children()? {
            if self.is_full() {
                break;
            }
            let (segment, item) = child?;
            let mut matched = Vec::new();
            if values
                && let ChildSegment::Key(key) = segment
                && let Some(key) = key.as_leaf()?
                && matcher.leaf(&key, &mut self.buffer)
            {
                matched.push(Matched::Key);
            }
            match item.as_leaf()? {
                Some(leaf) => {
                    if values && matcher.leaf(&leaf, &mut self.buffer) {
                        matched.push(Matched::Value);
                    }
                }
                None => {
                    if is_class(item.declaration()?.class) {
                        matched.push(Matched::Class);
                    }
                }
            }
            if !matched.is_empty() {
                self.value_hit(matched, node, field, Some(segment), item)?;
            }
        }
        Ok(self.proceed())
    }
}

/// Searches the document in `data`, which is a bin or ritobin text. Returns at most `limit`
/// hits, in document order. `name` is the file name shown in error messages.
///
/// A `PROP` bin is read as a stream, one object at a time. Ritobin text and a `PTCH` bin are
/// parsed completely first. The delete list, the objects and the records of a `PTCH` bin are
/// searched in this order.
pub fn scan(name: &str, data: Vec<u8>, matcher: &Matcher, limit: usize) -> Result<Vec<Hit>> {
    let mut scan = Scan::new(matcher, limit, true);
    scan_document(name, data, &mut scan)?;
    Ok(scan.hits)
}

/// Counts the hits of the document in `data`, up to `limit`. It reads the document as [`scan`]
/// does, but copies no path and no value, so it is faster and its memory use does not depend
/// on the number of hits.
pub fn count(name: &str, data: Vec<u8>, matcher: &Matcher, limit: usize) -> Result<usize> {
    let mut scan = Scan::new(matcher, limit, false);
    scan_document(name, data, &mut scan)?;
    Ok(scan.count)
}

/// Runs `scan` over the document in `data`.
fn scan_document(name: &str, data: Vec<u8>, scan: &mut Scan<'_>) -> Result<()> {
    let invalid = |error: BinError| miette::miette!("{name} is not a valid bin file: {error}");
    match BinKind::identify_from_bytes(&data) {
        Some(BinKind::Prop) => scan_stream(&data, scan).map_err(invalid),
        _ => {
            let document = Document::parse(name, data, ReadOptions::default())?;
            scan_file(&document.file, scan).map_err(invalid)
        }
    }
}

/// Runs `scan` over the `PROP` bin in `data` without building its tree.
fn scan_stream(data: &[u8], scan: &mut Scan<'_>) -> Result<(), BinError> {
    let mut stream = BinStream::mount(Cursor::new(data))?;
    scan.dependencies(stream.dependencies());
    if !scan.matcher.reads_objects() {
        return Ok(());
    }

    let mut objects = stream.objects();
    while !scan.is_full()
        && let Some(mut object) = objects.next()?
    {
        // An object that fails the filters is skipped before its bytes are read.
        if scan
            .matcher
            .wants_object(object.path_hash(), object.class_hash())
        {
            object.walk(scan)?;
        }
    }
    Ok(())
}

/// Runs `scan` over a parsed document.
fn scan_file(file: &BinFile, scan: &mut Scan<'_>) -> Result<(), BinError> {
    let matcher = scan.matcher;
    let objects = match file {
        BinFile::Prop(bin) => {
            scan.dependencies(&bin.dependencies);
            &bin.objects
        }
        BinFile::Override(patch) => &patch.objects,
    };
    if !matcher.reads_objects() {
        return Ok(());
    }
    if let BinFile::Override(patch) = file {
        scan.deleted(&patch.deleted);
    }

    let wanted = |object: &&BinObject| matcher.wants_object(object.path_hash, object.class_hash);
    for object in objects.values().filter(wanted) {
        if scan.is_full() {
            break;
        }
        object.walk(scan)?;
    }

    if let BinFile::Override(patch) = file {
        for (index, record) in patch.patches.iter().enumerate() {
            if scan.is_full() {
                break;
            }
            scan.record(index, record)?;
        }
    }
    Ok(())
}

/// A [`Hit`] with its hashes resolved to names, as it is printed.
///
/// Hashes are formatted as in ritobin text: the name if a hashtable has one, otherwise `0x` hex.
/// Values are formatted as ritobin text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub matched: Vec<Matched>,
    /// The path hash of the object that contains the hit. `None` for a dependency.
    pub object: Option<String>,
    /// The path of the object, if the entry table has it.
    pub object_name: Option<String>,
    /// The class of the object.
    pub class: Option<String>,
    /// The path of the value inside the object. `None` for a hit on the object itself or on a
    /// dependency.
    pub path: Option<String>,
    /// The ritobin type of the value.
    #[serde(rename = "type")]
    pub value_type: Option<String>,
    /// The value. For a struct it is the class of the struct. `None` for a list, an option or
    /// a map.
    pub value: Option<String>,
    /// The item count of a list, an option or a map.
    pub count: Option<usize>,
    /// The position of the record of a `PTCH` file that contains the hit, counting from 0.
    /// `None` for a hit that is not in a record. For a hit in a record, `class` is `None` and
    /// `path` starts with the property path of the record.
    pub record: Option<usize>,
}

impl Row {
    /// Resolves the hashes of `hit` through `names` and formats its value.
    pub fn new(hit: &Hit, names: &GameNames) -> Self {
        let class_name = |class: BinHash| match class.0 {
            0 => "null".to_owned(),
            _ => match names.bins.lookup(Table::BinTypes, class) {
                Some(name) => name.into_owned(),
                None => format_hash(class),
            },
        };
        let mut row = Self {
            matched: hit.matched.clone(),
            object: hit.object.map(|(object, _)| format_hash(object)),
            object_name: hit.object.and_then(|(object, _)| {
                names
                    .bins
                    .lookup(Table::BinEntries, object)
                    .map(Cow::into_owned)
            }),
            class: hit.object.map(|(_, class)| class_name(class)),
            path: None,
            value_type: None,
            value: None,
            count: None,
            record: None,
        };

        match &hit.at {
            At::Object => {}
            At::Deleted(object) => {
                row.value_type = Some(Kind::Hash.to_rito_name().to_owned());
                row.value = Some(match names.bins.lookup(Table::BinEntries, *object) {
                    Some(name) => quote(&name),
                    None => format_hash(*object),
                });
            }
            At::Dependency(dependency) => {
                row.value_type = Some(Kind::String.to_rito_name().to_owned());
                row.value = Some(quote(dependency));
            }
            At::Value {
                path,
                kind,
                item_kind,
                key_kind,
                shown,
                record,
            } => {
                let subtypes = match (key_kind, item_kind) {
                    (Some(key), Some(value)) => [Some(*key), Some(*value)],
                    (_, item) => [*item, None],
                };
                let named = path.to_named(&names.bins).text;
                row.path = Some(match record {
                    // The first segment of `path` is a property name. It stands for the whole
                    // record path. The text after it starts with `.`, `[` or `{`.
                    Some(record) => {
                        row.class = None;
                        row.record = Some(record.index);
                        let inside = named.find(['.', '[', '{']).map_or("", |at| &named[at..]);
                        format!("{}{inside}", record.path)
                    }
                    None => named,
                });
                row.value_type = Some(RitoType::new(*kind, subtypes).to_string());
                match shown {
                    // The owned tree does not fail to decode a leaf.
                    Shown::Leaf(value) => {
                        row.value = value
                            .as_leaf()
                            .ok()
                            .flatten()
                            .map(|leaf| leaf_text(&leaf, names));
                    }
                    Shown::Class(class) => row.value = Some(class_name(*class)),
                    Shown::Count(count) => row.count = Some(*count),
                }
            }
        }
        row
    }
}

/// Returns the ritobin text of `leaf`. A hash, a link or a file path is printed as its quoted
/// name if `names` has one, otherwise as `0x` hex. An 8-byte hash is printed as `0x` and 16
/// hex digits.
fn leaf_text(leaf: &Leaf<'_>, names: &GameNames) -> String {
    let named = |name: Option<Cow<'_, str>>, hex: String| match name {
        Some(name) => quote(&name),
        None => hex,
    };
    match leaf {
        Leaf::String(value) => quote(value),
        Leaf::Hash(hash) => match hash.try_as_bin_hash() {
            Some(hash) => named(names.bins.lookup(Table::BinHashes, hash), format_hash(hash)),
            None => format!("0x{:016x}", hash.as_u64()),
        },
        Leaf::Link(hash) => named(
            names
                .bins
                .lookup(Table::BinHashes, *hash)
                .or_else(|| names.bins.lookup(Table::BinEntries, *hash)),
            format_hash(*hash),
        ),
        Leaf::File(hash) => named(
            names.paths.path(*hash).map(Cow::Owned),
            format!("0x{:016x}", hash.0),
        ),
        other => {
            let mut text = String::new();
            write_plain(other, &mut text);
            text
        }
    }
}

/// Appends the ritobin text of a leaf that needs no hashtable: a number, a boolean, a vector, a
/// matrix or a color. Appends nothing for a string, a hash, a link or a file path.
fn write_plain(leaf: &Leaf<'_>, out: &mut String) {
    fn components<T: std::fmt::Display>(out: &mut String, items: &[T]) -> std::fmt::Result {
        out.push_str("{ ");
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            write!(out, "{item}")?;
        }
        out.push_str(" }");
        Ok(())
    }

    // Writing to a `String` does not fail.
    let _ = match *leaf {
        Leaf::None => write!(out, "null"),
        Leaf::Bool(value) | Leaf::Flag(value) => write!(out, "{value}"),
        Leaf::I8(value) => write!(out, "{value}"),
        Leaf::U8(value) => write!(out, "{value}"),
        Leaf::I16(value) => write!(out, "{value}"),
        Leaf::U16(value) => write!(out, "{value}"),
        Leaf::I32(value) => write!(out, "{value}"),
        Leaf::U32(value) => write!(out, "{value}"),
        Leaf::I64(value) => write!(out, "{value}"),
        Leaf::U64(value) => write!(out, "{value}"),
        Leaf::F32(value) => write!(out, "{value}"),
        Leaf::Vector2(value) => components(out, &value.to_array()),
        Leaf::Vector3(value) => components(out, &value.to_array()),
        Leaf::Vector4(value) => components(out, &value.to_array()),
        // The bin stores a matrix row by row.
        Leaf::Matrix44(value) => components(out, &value.transpose().to_cols_array()),
        Leaf::Color(value) => components(out, &[value.r, value.g, value.b, value.a]),
        _ => Ok(()),
    };
}

/// Returns `text` as a ritobin string literal, with the same escapes as the ritobin printer.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\x{:02x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Parses a ritobin type name of a value that has no nested values, for `--type`. Fails for an
/// unknown name and for the name of a struct, a list, an option or a map.
pub fn value_kind(name: &str) -> Result<Kind, String> {
    match Kind::from_rito_name(&name.to_ascii_lowercase()) {
        Some(kind @ (Kind::ObjectLink | Kind::BitBool)) => Ok(kind),
        Some(kind) if kind.is_primitive() => Ok(kind),
        _ => Err(format!(
            "expected the ritobin type of a value, such as `string`, `hash`, `link`, `file`, `f32`, `u32`, `bool` or `vec3`, but got `{name}`"
        )),
    }
}

/// Parses a name or `0x` hex as a bin hash, for the `--field`, `--class`, `--object` and
/// `--object-class` filters. A name is hashed. Fails for invalid hex.
pub fn name_hash(text: &str) -> Result<BinHash, String> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(digits) if digits.len() <= 8 => u32::from_str_radix(digits, 16)
            .map(BinHash)
            .map_err(|_| format!("`{text}` is not a hash of up to 8 hex digits")),
        Some(_) => Err(format!("`{text}` is not a hash of up to 8 hex digits")),
        None => Ok(BinHash::hash_str(text)),
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use ltk_meta::BinOverride;

    use super::*;
    use crate::document::to_bin;

    const SKIN: &str = r#"#PROP_text
type: string = "PROP"
version: u32 = 3
linked: list[string] = {
    "DATA/Characters/Teemo/Teemo.bin"
    "DATA/Characters/Teemo/Animations/Skin0.bin"
}
entries: map[hash,embed] = {
    "Characters/Teemo/Skins/Skin0" = SkinCharacterDataProperties {
        championSkinName: string = "TeemoBase"
        championSkinId: i32 = 17000
        skinMeshProperties: embed = SkinMeshDataProperties {
            simpleSkin: string = "ASSETS/Characters/Teemo/Skins/Base/Teemo_Base.skn"
            texture: file = "assets/characters/teemo/skins/base/teemo_base_tx_cm.tex"
            selfIllumination: f32 = 0.7
            overrideBoundingBox: option[vec3] = { { 50, 150, 150 } }
            materialOverride: list[embed] = {
                SkinMeshDataProperties_MaterialOverride {
                    submesh: string = "Mushroom"
                }
                SkinMeshDataProperties_MaterialOverride {
                    submesh: string = "Harmonica"
                }
            }
        }
        animationGraphData: link = "Characters/Teemo/Animations/Skin0"
        mStartingJointName: hash = "Hat_Feather1"
        emptyPointer: pointer = 0x0 {}
        sounds: map[hash,string] = {
            "attack" = "Play_sfx_Teemo_Attack"
            "death" = "Play_sfx_Teemo_Death"
        }
        visible: bool = true
    }
    "Characters/Teemo/Animations/Skin0" = AnimationGraphData {
        mUseCascadeBlend: bool = true
        mBlendTime: f32 = 0.2
    }
}
"#;

    const ENTRIES: &[&str] = &[
        "Characters/Teemo/Skins/Skin0",
        "Characters/Teemo/Animations/Skin0",
    ];
    const CLASSES: &[&str] = &[
        "SkinCharacterDataProperties",
        "SkinMeshDataProperties",
        "SkinMeshDataProperties_MaterialOverride",
        "AnimationGraphData",
    ];
    const FIELDS: &[&str] = &[
        "championSkinName",
        "championSkinId",
        "skinMeshProperties",
        "simpleSkin",
        "texture",
        "selfIllumination",
        "overrideBoundingBox",
        "materialOverride",
        "submesh",
        "animationGraphData",
        "mStartingJointName",
        "emptyPointer",
        "sounds",
        "visible",
        "mUseCascadeBlend",
        "mBlendTime",
    ];
    const HASHES: &[&str] = &["Hat_Feather1", "attack", "death"];

    const TEXTURE: &str = "assets/characters/teemo/skins/base/teemo_base_tx_cm.tex";

    /// Returns names loaded from text tables that have every name of [`SKIN`]. The chunk path
    /// table is not loaded.
    fn names() -> GameNames {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        for (table, names) in [
            ("entries", ENTRIES),
            ("types", CLASSES),
            ("fields", FIELDS),
            ("hashes", HASHES),
        ] {
            let lines: String = names
                .iter()
                .map(|name| format!("{:08x} {name}\n", BinHash::hash_str(name).0))
                .collect();
            std::fs::write(path.join(format!("hashes.bin{table}.txt")), lines).unwrap();
        }
        GameNames {
            bins: BinHashes::load(None, Some(&path)),
            paths: WadPaths::default(),
        }
    }

    fn literal(pattern: &str) -> Query {
        Query {
            pattern: Pattern::Literal(pattern.to_owned()),
            ..Query::default()
        }
    }

    fn regex_query(pattern: &str) -> Query {
        Query {
            pattern: Pattern::Regex(pattern.to_owned()),
            ..Query::default()
        }
    }

    /// Searches [`SKIN`] as ritobin text and as a binary bin. Fails if the two scans return
    /// different hits. Returns the rows.
    fn rows(query: &Query, names: &GameNames) -> Vec<Row> {
        let matcher = Matcher::compile(query, &names.bins, &names.paths).unwrap();
        let from_text = scan("skin0.rito", SKIN.into(), &matcher, usize::MAX).unwrap();

        let document = Document::parse("skin0.rito", SKIN.into(), ReadOptions::default()).unwrap();
        let bin = to_bin(&document.file).unwrap();
        let from_bin = scan("skin0.bin", bin, &matcher, usize::MAX).unwrap();
        assert_eq!(from_text, from_bin);

        from_text.iter().map(|hit| Row::new(hit, names)).collect()
    }

    /// Formats a row as one line: the object for a hit on the object, `linked` for a
    /// dependency, otherwise the path. The type, the value and the item count follow.
    fn show(row: &Row) -> String {
        let mut line = match (&row.path, &row.object) {
            (Some(path), _) => path.clone(),
            (None, Some(object)) => {
                let name = row.object_name.as_ref().unwrap_or(object);
                return format!("{name} : {}", row.class.as_deref().unwrap());
            }
            (None, None) if row.matched.contains(&Matched::Deleted) => "deleted".to_owned(),
            (None, None) => "linked".to_owned(),
        };
        if let Some(value_type) = &row.value_type {
            line.push_str(&format!(": {value_type}"));
        }
        if let Some(value) = &row.value {
            line.push_str(&format!(" = {value}"));
        }
        if let Some(count) = row.count {
            line.push_str(&format!(" ({count})"));
        }
        line
    }

    fn found(query: &Query) -> Vec<String> {
        rows(query, &names()).iter().map(show).collect()
    }

    #[test]
    fn literal_matches_substring_in_every_part() {
        assert_eq!(
            found(&literal("skin0")),
            [
                "linked: string = \"DATA/Characters/Teemo/Animations/Skin0.bin\"",
                "Characters/Teemo/Skins/Skin0 : SkinCharacterDataProperties",
                "animationGraphData: link = \"Characters/Teemo/Animations/Skin0\"",
                "Characters/Teemo/Animations/Skin0 : AnimationGraphData",
            ]
        );
        assert_eq!(
            found(&literal("teemo_base.skn")),
            [
                "skinMeshProperties.simpleSkin: string = \"ASSETS/Characters/Teemo/Skins/Base/Teemo_Base.skn\""
            ]
        );
    }

    #[test]
    fn literal_matches_hash_that_no_hashtable_resolves() {
        let names = GameNames::default();
        let link = format!("0x{:08x}", BinHash::hash_str(ENTRIES[1]).0);
        let object = format!("0x{:08x}", BinHash::hash_str(ENTRIES[0]).0);

        let rows = rows(&literal("characters/teemo/animations/skin0"), &names);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].matched, [Matched::Dependency]);
        assert_eq!(rows[1].object.as_deref(), Some(object.as_str()));
        assert_eq!(rows[1].value.as_deref(), Some(link.as_str()));
        assert_eq!(rows[1].value_type.as_deref(), Some("link"));
        assert_eq!(rows[2].matched, [Matched::Entry]);
        assert_eq!(rows[2].object.as_deref(), Some(link.as_str()));

        let rows = self::rows(&literal(TEXTURE), &names);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value_type.as_deref(), Some("file"));
        assert_eq!(
            rows[0].value,
            Some(format!("0x{:016x}", WadHash::hash_str(TEXTURE).0))
        );
    }

    #[test]
    fn hex_literal_matches_that_hash() {
        let hash = format!("0x{:08x}", BinHash::hash_str("Hat_Feather1").0);
        assert_eq!(
            found(&literal(&hash)),
            ["mStartingJointName: hash = \"Hat_Feather1\""]
        );

        // The hash of a name ignores case, so the class `AnimationGraphData` and the property
        // `animationGraphData` have the same hash. The hash matches in both roles.
        let class = format!("0X{:08X}", BinHash::hash_str("AnimationGraphData").0);
        let rows = rows(&literal(&class), &names());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].matched, [Matched::Field]);
        assert_eq!(rows[0].path.as_deref(), Some("animationGraphData"));
        assert_eq!(rows[1].matched, [Matched::Class]);
        assert_eq!(
            show(&rows[1]),
            "Characters/Teemo/Animations/Skin0 : AnimationGraphData"
        );

        let file = format!("0x{:016x}", WadHash::hash_str(TEXTURE).0);
        assert_eq!(
            found(&literal(&file)),
            [format!("skinMeshProperties.texture: file = {file}")]
        );
    }

    #[test]
    fn hex_literal_of_more_than_8_digits_matches_8_byte_hash() {
        use ltk_hash::HashValue;
        use ltk_meta::{Bin, BinObject, property::values};

        let bin = Bin::builder()
            .object(
                BinObject::builder(0x11u32, 0x22u32)
                    .property(
                        0x33u32,
                        values::Hash::new(HashValue::wide(0x0123_4567_89ab_cdef)),
                    )
                    .build(),
            )
            .build();
        let data = to_bin(&bin.into()).unwrap();
        let names = names();
        let found = |pattern: &str| -> Vec<String> {
            let matcher = Matcher::compile(&literal(pattern), &names.bins, &names.paths).unwrap();
            scan("material.bin", data.clone(), &matcher, usize::MAX)
                .unwrap()
                .iter()
                .map(|hit| show(&Row::new(hit, &names)))
                .collect()
        };

        assert_eq!(
            found("0x0123456789abcdef"),
            ["00000033: hash = 0x0123456789abcdef"]
        );
        // The low 4 bytes of an 8-byte hash are not a 4-byte hash.
        assert!(found("0x89abcdef").is_empty());
    }

    #[test]
    fn number_literal_matches_equal_numeric_values() {
        assert_eq!(found(&literal("17000")), ["championSkinId: i32 = 17000"]);
        assert_eq!(
            found(&literal("0.7")),
            ["skinMeshProperties.selfIllumination: f32 = 0.7"]
        );
        assert_eq!(
            found(&literal("150")),
            ["skinMeshProperties.overrideBoundingBox[0]: vec3 = { 50, 150, 150 }"]
        );
        assert_eq!(
            found(&literal("true")),
            ["visible: bool = true", "mUseCascadeBlend: bool = true"]
        );
        assert!(found(&literal("1700")).is_empty());
    }

    #[test]
    fn field_target_matches_property_names() {
        let query = Query {
            targets: vec![Target::Field],
            ..literal("override")
        };
        assert_eq!(
            found(&query),
            [
                "skinMeshProperties.overrideBoundingBox: option[vec3] (1)",
                "skinMeshProperties.materialOverride: list[embed] (2)",
            ]
        );

        let query = Query {
            targets: vec![Target::Field],
            exact: true,
            ..literal("emptyPointer")
        };
        assert_eq!(found(&query), ["emptyPointer: pointer = null"]);
    }

    #[test]
    fn class_target_matches_objects_and_nested_structs() {
        let query = Query {
            targets: vec![Target::Class],
            ..literal("SkinMeshDataProperties")
        };
        assert_eq!(
            found(&query),
            [
                "skinMeshProperties: embed = SkinMeshDataProperties",
                "skinMeshProperties.materialOverride[0]: embed = SkinMeshDataProperties_MaterialOverride",
                "skinMeshProperties.materialOverride[1]: embed = SkinMeshDataProperties_MaterialOverride",
            ]
        );

        let query = Query {
            targets: vec![Target::Class],
            ..literal("graphdata")
        };
        assert_eq!(
            found(&query),
            ["Characters/Teemo/Animations/Skin0 : AnimationGraphData"]
        );
    }

    #[test]
    fn map_entry_with_matching_key_and_value_is_one_hit() {
        let rows = rows(&literal("attack"), &names());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].matched, [Matched::Key, Matched::Value]);
        assert_eq!(
            show(&rows[0]),
            "sounds{\"attack\"}: string = \"Play_sfx_Teemo_Attack\""
        );

        let rows = self::rows(&literal("sfx_teemo_death"), &names());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].matched, [Matched::Value]);
    }

    #[test]
    fn property_with_matching_name_and_value_is_one_hit() {
        let rows = rows(&literal("skinname"), &names());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].matched, [Matched::Field]);

        let rows = self::rows(&literal("teemobase"), &names());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].matched, [Matched::Value]);

        // `submesh` is the name of two properties, and "Mushroom" is the value of one of them.
        let rows = self::rows(&regex_query("submesh|Mushroom"), &names());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].matched, [Matched::Field, Matched::Value]);
        assert_eq!(rows[1].matched, [Matched::Field]);
    }

    #[test]
    fn field_filter_restricts_search_to_values_of_that_property() {
        let query = Query {
            field: Some(BinHash::hash_str("submesh")),
            ..Query::default()
        };
        assert_eq!(
            found(&query),
            [
                "skinMeshProperties.materialOverride[0].submesh: string = \"Mushroom\"",
                "skinMeshProperties.materialOverride[1].submesh: string = \"Harmonica\"",
            ]
        );

        let query = Query {
            field: Some(BinHash::hash_str("sounds")),
            ..literal("teemo")
        };
        assert_eq!(found(&query).len(), 2);
    }

    #[test]
    fn class_filter_restricts_search_to_properties_of_that_class() {
        let query = Query {
            class: Some(BinHash::hash_str("SkinMeshDataProperties")),
            ..Query::default()
        };
        assert_eq!(
            found(&query),
            [
                "skinMeshProperties.simpleSkin: string = \"ASSETS/Characters/Teemo/Skins/Base/Teemo_Base.skn\"".to_owned(),
                format!(
                    "skinMeshProperties.texture: file = 0x{:016x}",
                    WadHash::hash_str(TEXTURE).0
                ),
                "skinMeshProperties.selfIllumination: f32 = 0.7".to_owned(),
                "skinMeshProperties.overrideBoundingBox[0]: vec3 = { 50, 150, 150 }".to_owned(),
            ]
        );
    }

    #[test]
    fn kind_filter_restricts_search_to_values_of_those_kinds() {
        let query = Query {
            kinds: vec![Kind::ObjectLink, Kind::Hash],
            ..Query::default()
        };
        assert_eq!(
            found(&query),
            [
                "animationGraphData: link = \"Characters/Teemo/Animations/Skin0\"",
                "mStartingJointName: hash = \"Hat_Feather1\"",
                "sounds{\"attack\"}: string = \"Play_sfx_Teemo_Attack\"",
                "sounds{\"death\"}: string = \"Play_sfx_Teemo_Death\"",
            ]
        );
    }

    #[test]
    fn object_filters_restrict_search_to_matching_objects() {
        let expected = ["mUseCascadeBlend: bool = true", "mBlendTime: f32 = 0.2"];
        let by_path = Query {
            object: Some(BinHash::hash_str(ENTRIES[1])),
            ..Query::default()
        };
        assert_eq!(found(&by_path), expected);

        let by_class = Query {
            object_class: Some(BinHash::hash_str("AnimationGraphData")),
            ..Query::default()
        };
        assert_eq!(found(&by_class), expected);
    }

    #[test]
    fn regex_matches_text_of_names_strings_and_numbers() {
        assert_eq!(
            found(&regex_query(r"^17\d+$")),
            ["championSkinId: i32 = 17000"]
        );
        assert_eq!(
            found(&regex_query(r"^0\.[27]$")),
            [
                "skinMeshProperties.selfIllumination: f32 = 0.7",
                "mBlendTime: f32 = 0.2",
            ]
        );
        assert_eq!(
            found(&regex_query(r"^m[A-Z]\w+Time$")),
            ["mBlendTime: f32 = 0.2"]
        );
        assert_eq!(
            found(&regex_query(r"\.skn$")),
            [
                "skinMeshProperties.simpleSkin: string = \"ASSETS/Characters/Teemo/Skins/Base/Teemo_Base.skn\""
            ]
        );
    }

    #[test]
    fn exact_and_case_sensitive_options_restrict_text_matches() {
        let exact = |pattern: &str| Query {
            exact: true,
            ..literal(pattern)
        };
        assert_eq!(found(&exact("mushroom")).len(), 1);
        assert!(found(&exact("mush")).is_empty());
        assert_eq!(
            found(&exact("characters/teemo/skins/skin0")),
            ["Characters/Teemo/Skins/Skin0 : SkinCharacterDataProperties"]
        );

        let sensitive = |pattern: &str| Query {
            case_sensitive: true,
            ..literal(pattern)
        };
        assert_eq!(found(&sensitive("Mushroom")).len(), 1);
        assert!(found(&sensitive("mushroom")).is_empty());
    }

    #[test]
    fn scan_returns_at_most_limit_hits() {
        let names = names();
        let matcher = Matcher::compile(&literal("teemo"), &names.bins, &names.paths).unwrap();
        let all = scan("skin0.rito", SKIN.into(), &matcher, usize::MAX).unwrap();
        assert!(all.len() > 4);
        for limit in [1, 2, 4] {
            let limited = scan("skin0.rito", SKIN.into(), &matcher, limit).unwrap();
            assert_eq!(limited, all[..limit]);
        }
    }

    #[test]
    fn count_returns_number_of_hits_of_scan() {
        let names = names();
        let document = Document::parse("skin0.rito", SKIN.into(), ReadOptions::default()).unwrap();
        let bin = to_bin(&document.file).unwrap();

        let queries = [
            literal("teemo"),
            literal("skin0"),
            literal("true"),
            regex_query("."),
            Query::default(),
            Query {
                targets: vec![Target::Class, Target::Field],
                ..regex_query("e")
            },
        ];
        for query in queries {
            let matcher = Matcher::compile(&query, &names.bins, &names.paths).unwrap();
            let hits = scan("skin0.bin", bin.clone(), &matcher, usize::MAX).unwrap();
            assert!(!hits.is_empty(), "{query:?}");
            for data in [bin.clone(), SKIN.into()] {
                assert_eq!(
                    count("skin0", data.clone(), &matcher, usize::MAX).unwrap(),
                    hits.len(),
                    "{query:?}"
                );
                assert_eq!(count("skin0", data, &matcher, 2).unwrap(), 2, "{query:?}");
            }
        }
    }

    #[test]
    fn object_filter_excludes_dependencies() {
        let all = found(&literal("animations/skin0"));
        assert!(all[0].starts_with("linked"), "{all:?}");

        let query = Query {
            object: Some(BinHash::hash_str(ENTRIES[0])),
            ..literal("animations/skin0")
        };
        assert_eq!(
            found(&query),
            ["animationGraphData: link = \"Characters/Teemo/Animations/Skin0\""]
        );
    }

    #[test]
    fn scan_reads_objects_of_patch_bin() {
        let document = Document::parse("skin0.rito", SKIN.into(), ReadOptions::default()).unwrap();
        let mut patch = BinOverride::builder();
        for object in document.file.objects().values() {
            patch = patch.object(object.clone());
        }
        let patch = to_bin(&patch.build().into()).unwrap();

        let names = names();
        let matcher = Matcher::compile(&literal("mushroom"), &names.bins, &names.paths).unwrap();
        let hits = scan("patch.bin", patch, &matcher, usize::MAX).unwrap();
        assert_eq!(hits.len(), 1);
    }

    const PATCH: &str = r#"#PTCH_text
type: string = "PTCH"
version: u32 = 3
linked: list[string] = { }
entries: map[hash, embed] = { }
patches: map[hash, embed] = {
    "Characters/Teemo/Skins/Skin0" = patch {
        path: string = "skinMeshProperties.selfIllumination"
        value: f32 = 0.25
    }
    "Characters/Teemo/Skins/Skin0" = patch {
        path: string = "skinMeshProperties.materialOverride"
        value: list[embed] = {
            SkinMeshDataProperties_MaterialOverride {
                submesh: string = "Cap"
            }
        }
    }
}
"#;

    /// Searches [`PATCH`] as a binary `PTCH` file whose delete list has the animation entry.
    /// Returns each row as its matched parts, its record position and its line.
    fn found_in_patch(query: &Query) -> Vec<(Vec<Matched>, Option<usize>, String)> {
        let document = Document::parse("edit.ptch", PATCH.into(), ReadOptions::default()).unwrap();
        let BinFile::Override(mut patch) = document.file else {
            panic!("not a PTCH file");
        };
        patch
            .deleted
            .push(BinHash::hash_str("Characters/Teemo/Animations/Skin0"));
        let data = to_bin(&patch.into()).unwrap();

        let names = names();
        let matcher = Matcher::compile(query, &names.bins, &names.paths).unwrap();
        let hits = scan("edit.ptch", data.clone(), &matcher, usize::MAX).unwrap();
        assert_eq!(
            count("edit.ptch", data, &matcher, usize::MAX).unwrap(),
            hits.len()
        );
        hits.iter()
            .map(|hit| {
                let row = Row::new(hit, &names);
                assert_eq!(
                    row.class.is_none(),
                    row.record.is_some() || row.object.is_none()
                );
                (row.matched.clone(), row.record, show(&row))
            })
            .collect()
    }

    #[test]
    fn scan_reads_delete_list_and_records_of_patch_bin() {
        // The pattern matches both entries: the deleted one, and the one that both records
        // address.
        assert_eq!(
            found_in_patch(&literal("skin0")),
            [
                (
                    vec![Matched::Deleted],
                    None,
                    "deleted: hash = \"Characters/Teemo/Animations/Skin0\"".to_owned()
                ),
                (
                    vec![Matched::Entry],
                    Some(0),
                    "skinMeshProperties.selfIllumination: f32 = 0.25".to_owned()
                ),
                (
                    vec![Matched::Entry],
                    Some(1),
                    "skinMeshProperties.materialOverride: list[embed] (1)".to_owned()
                ),
            ]
        );

        // A value inside the value of a record. Its path continues the record path.
        assert_eq!(
            found_in_patch(&literal("cap")),
            [(
                vec![Matched::Value],
                Some(1),
                "skinMeshProperties.materialOverride[0].submesh: string = \"Cap\"".to_owned()
            )]
        );

        // The last property name of a record path, and the value of the record.
        assert_eq!(
            found_in_patch(&literal("selfIllumination")),
            [(
                vec![Matched::Field],
                Some(0),
                "skinMeshProperties.selfIllumination: f32 = 0.25".to_owned()
            )]
        );
        assert_eq!(
            found_in_patch(&literal("0.25")),
            [(
                vec![Matched::Value],
                Some(0),
                "skinMeshProperties.selfIllumination: f32 = 0.25".to_owned()
            )]
        );
    }

    #[test]
    fn object_filters_apply_to_delete_list_and_records_of_patch_bin() {
        let animation = BinHash::hash_str("Characters/Teemo/Animations/Skin0");
        let with_object = |object| Query {
            object: Some(object),
            ..literal("skin0")
        };
        assert_eq!(found_in_patch(&with_object(animation)).len(), 1);
        assert_eq!(
            found_in_patch(&with_object(BinHash::hash_str(
                "Characters/Teemo/Skins/Skin0"
            )))
            .len(),
            2
        );

        // A record and the delete list store no class.
        let with_class = Query {
            object_class: Some(BinHash::hash_str("SkinCharacterDataProperties")),
            ..literal("skin0")
        };
        assert!(found_in_patch(&with_class).is_empty());
    }

    #[test]
    fn scan_fails_for_invalid_document() {
        let names = GameNames::default();
        let matcher = Matcher::compile(&literal("x"), &names.bins, &names.paths).unwrap();
        let truncated = b"PROP\x03\x00\x00\x00\x00".to_vec();
        let error = scan("broken.bin", truncated, &matcher, usize::MAX).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("broken.bin is not a valid bin file")
        );
        assert!(scan("notes.txt", b"not ritobin".to_vec(), &matcher, usize::MAX).is_err());
    }

    #[test]
    fn compile_fails_when_filters_leave_no_part_to_search() {
        let names = GameNames::default();
        let compile = |query: &Query| Matcher::compile(query, &names.bins, &names.paths);

        let entries_of_field = Query {
            targets: vec![Target::Entry],
            field: Some(BinHash(1)),
            ..literal("x")
        };
        assert!(compile(&entries_of_field).is_err());

        let names_without_pattern = Query {
            targets: vec![Target::Field],
            ..Query::default()
        };
        assert!(compile(&names_without_pattern).is_err());

        assert!(compile(&regex_query("(")).is_err());
        assert!(compile(&literal("x")).is_ok());
    }

    #[test]
    fn name_hash_parses_name_or_hex() {
        assert_eq!(name_hash("mName"), Ok(BinHash::hash_str("mName")));
        assert_eq!(name_hash("0x1F"), Ok(BinHash(0x1f)));
        // A name of hex digits without the `0x` prefix is hashed as a name.
        assert_eq!(name_hash("face"), Ok(BinHash::hash_str("face")));
        assert!(name_hash("0x123456789").is_err());
        assert!(name_hash("0xzz").is_err());
    }

    #[test]
    fn value_kind_accepts_only_types_without_nested_values() {
        assert_eq!(value_kind("string"), Ok(Kind::String));
        assert_eq!(value_kind("LINK"), Ok(Kind::ObjectLink));
        assert_eq!(value_kind("flag"), Ok(Kind::BitBool));
        assert_eq!(value_kind("file"), Ok(Kind::WadChunkLink));
        for name in ["list", "embed", "pointer", "map", "option", "text"] {
            assert!(value_kind(name).is_err(), "{name}");
        }
    }

    #[test]
    fn quote_escapes_like_ritobin_printer() {
        assert_eq!(quote("a\"b\\c\n\u{1}"), r#""a\"b\\c\n\x01""#);
    }
}
