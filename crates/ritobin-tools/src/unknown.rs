//! Collects the hashes of bin documents that no hashtable resolves.
//!
//! A [`Collector`] reads documents one at a time. It counts each hash without a name, and keeps
//! one example location for it. A worker thread has its own collector. [`merge`] combines the
//! results of the workers into a result that does not depend on the thread count.

use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
};

use clap::ValueEnum;
use ltk_hash::BinHash;
use ltk_meta::{
    BinKind, BinStream, Error as BinError,
    path::ValuePath,
    walk::{ChildSegment, Leaf, Node, TreeKind as _, TreeValue, Visit, Visitor},
};
use ltk_mimir_cache::Table;
use miette::Result;
use serde::Serialize;

use crate::{
    document::{Document, ReadOptions},
    hashes::GameNames,
};

/// A hashtable that a hash of a bin belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HashTable {
    /// Object paths, and the values of `link` properties
    Entries,
    /// Property names
    Fields,
    /// Values of `hash` properties
    Hashes,
    /// Class names
    Types,
    /// Paths of `file` values
    Game,
}

impl HashTable {
    /// Returns the name of the table, as the `--table` option accepts it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Entries => "entries",
            Self::Fields => "fields",
            Self::Hashes => "hashes",
            Self::Types => "types",
            Self::Game => "game",
        }
    }

    /// Formats `hash` as `0x` hex: 16 digits for a file path hash, 8 digits for a bin hash.
    pub fn format(self, hash: u64) -> String {
        match self {
            Self::Game => format!("0x{hash:016x}"),
            _ => format!("0x{hash:08x}"),
        }
    }

    /// Returns `true` if `names` has a name for `hash`. A `link` value or a `hash` value has a
    /// name if the entry table or the hash table has one, because both are printed with either.
    fn is_known(self, hash: u64, names: &GameNames) -> bool {
        let bin = |table| names.bins.lookup(table, BinHash(hash as u32)).is_some();
        match self {
            Self::Entries | Self::Hashes => names.bins.value_name(BinHash(hash as u32)).is_some(),
            Self::Fields => bin(Table::BinFields),
            Self::Types => bin(Table::BinTypes),
            Self::Game => names.paths.path(ltk_hash::WadHash(hash)).is_some(),
        }
    }
}

/// One place where a hash occurs.
#[derive(Debug, Clone, PartialEq)]
pub struct Example {
    /// The file path, or the path of the bin in the game or in a package.
    pub source: String,
    /// The game archive or the package that contains the bin.
    pub archive: Option<String>,
    /// The path hash of the object that contains the hash.
    pub object: BinHash,
    /// The path of the value inside the object. `None` if the hash is the path or the class of
    /// the object itself.
    pub path: Option<ValuePath>,
}

impl Example {
    /// Returns the key that orders examples: the archive, then the source.
    fn order(&self) -> (&str, &str) {
        (self.archive.as_deref().unwrap_or_default(), &self.source)
    }
}

/// A hash without a name.
#[derive(Debug, Clone, PartialEq)]
pub struct Unknown {
    /// The number of occurrences in all documents.
    pub count: usize,
    /// The number of documents that contain the hash.
    pub documents: usize,
    /// The first occurrence in the document with the smallest archive and source.
    pub example: Example,
}

/// The hashes without a name, by table and hash value.
pub type Found = HashMap<(HashTable, u64), Unknown>;

/// Adds the hashes of `other` to `found`. Adds the counts of a hash that both have, and keeps
/// the example of the document with the smaller archive and source.
pub fn merge(found: &mut Found, other: Found) {
    for (key, unknown) in other {
        match found.get_mut(&key) {
            Some(existing) => {
                existing.count += unknown.count;
                existing.documents += unknown.documents;
                if unknown.example.order() < existing.example.order() {
                    existing.example = unknown.example;
                }
            }
            None => {
                found.insert(key, unknown);
            }
        }
    }
}

/// Reads documents and collects their hashes without a name.
pub struct Collector<'n> {
    names: &'n GameNames,
    /// The tables whose hashes are collected. Empty collects all tables.
    tables: Vec<HashTable>,
    /// The result of the name lookup of each hash that was seen.
    known: HashMap<(HashTable, u64), bool>,
    pub found: Found,
    /// The source of the document that is being read.
    source: String,
    /// The archive of the document that is being read.
    archive: Option<String>,
    /// The hashes without a name that the current document has.
    in_document: HashSet<(HashTable, u64)>,
}

impl<'n> Collector<'n> {
    /// Creates a collector for the hashes of `tables`. An empty `tables` selects all tables.
    pub fn new(names: &'n GameNames, tables: &[HashTable]) -> Self {
        Self {
            names,
            tables: tables.to_vec(),
            known: HashMap::new(),
            found: Found::new(),
            source: String::new(),
            archive: None,
            in_document: HashSet::new(),
        }
    }

    /// Reads the document in `data`, which is a bin or ritobin text, and collects its hashes
    /// without a name. `source` and `archive` identify the document in the examples.
    ///
    /// Reads the objects of a `PROP` bin and of a `PTCH` bin. The records of a `PTCH` bin store
    /// property names as text, so they have no hash to collect.
    pub fn document(&mut self, source: &str, archive: Option<String>, data: Vec<u8>) -> Result<()> {
        self.source = source.to_owned();
        self.archive = archive;
        self.in_document.clear();
        let invalid =
            |error: BinError| miette::miette!("{source} is not a valid bin file: {error}");
        match BinKind::identify_from_bytes(&data) {
            Some(BinKind::Prop) => self.stream(&data).map_err(invalid),
            _ => {
                let document = Document::parse(source, data, ReadOptions::default())?;
                for object in document.file.objects().values() {
                    object.walk(self).map_err(invalid)?;
                }
                Ok(())
            }
        }
    }

    /// Walks the `PROP` bin in `data` without building its tree.
    fn stream(&mut self, data: &[u8]) -> Result<(), BinError> {
        let mut stream = BinStream::mount(Cursor::new(data))?;
        let mut objects = stream.objects();
        while let Some(mut object) = objects.next()? {
            object.walk(self)?;
        }
        Ok(())
    }

    /// Counts one occurrence of `hash` if it has no name. Returns `true` if the caller must
    /// pass the location of this occurrence to [`Collector::example`]: it is the first one of
    /// the hash, or the first one in a document that orders before the current example.
    fn count(&mut self, table: HashTable, hash: u64) -> bool {
        if !self.tables.is_empty() && !self.tables.contains(&table) {
            return false;
        }
        let key = (table, hash);
        let names = self.names;
        if *self
            .known
            .entry(key)
            .or_insert_with(|| table.is_known(hash, names))
        {
            return false;
        }

        let first_in_document = self.in_document.insert(key);
        match self.found.get_mut(&key) {
            Some(unknown) => {
                unknown.count += 1;
                unknown.documents += usize::from(first_in_document);
                let current = (self.archive.as_deref().unwrap_or_default(), &*self.source);
                first_in_document && current < unknown.example.order()
            }
            None => true,
        }
    }

    /// Stores the location of the occurrence that [`Collector::count`] asked for.
    fn example(&mut self, table: HashTable, hash: u64, object: BinHash, path: Option<ValuePath>) {
        let example = Example {
            source: self.source.clone(),
            archive: self.archive.clone(),
            object,
            path,
        };
        match self.found.get_mut(&(table, hash)) {
            Some(unknown) => unknown.example = example,
            None => {
                self.found.insert(
                    (table, hash),
                    Unknown {
                        count: 1,
                        documents: 1,
                        example,
                    },
                );
            }
        }
    }

    /// Counts the hash of `leaf` if `leaf` is a hash, a link or a file path. `path` builds the
    /// path of the value, and is called only if an example is stored. An 8-byte hash is not
    /// counted, because no hashtable stores names for 8-byte hashes.
    fn leaf(
        &mut self,
        leaf: &Leaf<'_>,
        object: BinHash,
        path: impl FnOnce() -> Result<ValuePath, BinError>,
    ) -> Result<(), BinError> {
        let (table, hash) = match leaf {
            Leaf::Hash(hash) => match hash.try_as_u32() {
                Some(hash) => (HashTable::Hashes, u64::from(hash)),
                None => return Ok(()),
            },
            Leaf::Link(hash) => (HashTable::Entries, u64::from(hash.0)),
            Leaf::File(hash) => (HashTable::Game, hash.0),
            _ => return Ok(()),
        };
        // The value 0 is an empty hash, an empty link or an empty file path. It has no name.
        if hash != 0 && self.count(table, hash) {
            self.example(table, hash, object, Some(path()?));
        }
        Ok(())
    }
}

impl<'a, V: TreeValue<'a>> Visitor<'a, V> for Collector<'_> {
    type Error = BinError;

    /// Counts the path of an object and the class of an object or of a nested struct.
    fn enter_node(&mut self, node: &Node<'_, 'a, V>) -> Result<Visit, BinError> {
        let object = node.object_hash();
        if node.is_root() && self.count(HashTable::Entries, u64::from(object.0)) {
            self.example(HashTable::Entries, u64::from(object.0), object, None);
        }
        // Class 0 is the null struct.
        let class = u64::from(node.class_hash().0);
        if class != 0 && self.count(HashTable::Types, class) {
            let path = match node.is_root() {
                true => None,
                false => Some(node.to_value_path()?),
            };
            self.example(HashTable::Types, class, object, path);
        }
        Ok(Visit::Continue)
    }

    /// Counts the name of a property and the hashes in its value. The walk descends only into
    /// nodes, so this function also reads the items of a list, an option or a map.
    fn enter_property(
        &mut self,
        field: BinHash,
        value: V,
        node: &Node<'_, 'a, V>,
    ) -> Result<Visit, BinError> {
        let object = node.object_hash();
        let property = || -> Result<ValuePath, BinError> {
            let mut path = node.to_value_path()?;
            path.push_field(field, node.class_hash());
            Ok(path)
        };
        if self.count(HashTable::Fields, u64::from(field.0)) {
            self.example(
                HashTable::Fields,
                u64::from(field.0),
                object,
                Some(property()?),
            );
        }

        if let Some(leaf) = value.as_leaf()? {
            self.leaf(&leaf, object, property)?;
            return Ok(Visit::Continue);
        }

        // The items of a list of structs are nodes, which the walk enters. Only the keys of a
        // map of structs are read here.
        let declaration = value.declaration()?;
        let holds_nodes = declaration.item_kind.is_some_and(|kind| kind.is_node());
        let reads_items = match holds_nodes {
            true => declaration.key_kind.is_some(),
            false => declaration.item_kind.is_some(),
        };
        if !reads_items {
            return Ok(Visit::Continue);
        }
        for child in value.children()? {
            let (segment, item) = child?;
            let at = |segment: &ChildSegment<V>| -> Result<ValuePath, BinError> {
                let mut path = property()?;
                match segment {
                    ChildSegment::Index(index) => path.push_index(*index),
                    ChildSegment::Key(key) => path.push_key(key.map_key()?),
                }
                Ok(path)
            };
            if let ChildSegment::Key(key) = &segment
                && let Some(key) = key.as_leaf()?
            {
                self.leaf(&key, object, || at(&segment))?;
            }
            if let Some(leaf) = item.as_leaf()? {
                self.leaf(&leaf, object, || at(&segment))?;
            }
        }
        Ok(Visit::Continue)
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use ltk_hash::Hash as _;

    use super::*;
    use crate::{
        document::to_bin,
        hashes::{BinHashes, WadPaths},
    };

    const SKIN: &str = r#"#PROP_text
type: string = "PROP"
version: u32 = 3
linked: list[string] = { }
entries: map[hash,embed] = {
    "Characters/Test/Skins/Skin0" = SkinCharacterDataProperties {
        knownField: link = 0x0
        newField: link = "Characters/Test/Animations/Skin0"
        mesh: embed = NewMeshClass {
            texture: file = "assets/mods/example.tex"
            joints: list[hash] = { "Root", "NewJoint", "NewJoint" }
        }
        sounds: map[hash,string] = {
            "newKey" = "Play"
        }
    }
}
"#;

    /// Returns names loaded from text tables that have some of the names of [`SKIN`].
    fn names() -> GameNames {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        for (table, names) in [
            ("entries", ["Characters/Test/Skins/Skin0"].as_slice()),
            ("types", &["SkinCharacterDataProperties"]),
            (
                "fields",
                &["knownField", "mesh", "texture", "joints", "sounds"],
            ),
            ("hashes", &["Root"]),
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

    /// Returns each hash without a name as its table, its count and the path of its example,
    /// sorted. The object path or class itself has the path `(object)`.
    fn listed(found: &Found, names: &GameNames) -> Vec<(HashTable, u64, usize, String)> {
        let mut rows: Vec<_> = found
            .iter()
            .map(|((table, hash), unknown)| {
                let path = match &unknown.example.path {
                    Some(path) => path.to_named(&names.bins).text,
                    None => "(object)".to_owned(),
                };
                (*table, *hash, unknown.count, path)
            })
            .collect();
        rows.sort();
        rows
    }

    fn bin_hash(name: &str) -> u64 {
        u64::from(BinHash::hash_str(name).0)
    }

    #[test]
    fn collector_counts_hashes_without_name_in_text_and_binary_document() {
        let names = names();
        let document = Document::parse("skin0.rito", SKIN.into(), ReadOptions::default()).unwrap();
        let binary = to_bin(&document.file).unwrap();

        let new_field = format!("{:08x}", bin_hash("newField"));
        let file = ltk_game_index::chunk_hash("assets/mods/example.tex").0;
        let mut expected = vec![
            (
                HashTable::Entries,
                bin_hash("Characters/Test/Animations/Skin0"),
                1,
                new_field.clone(),
            ),
            (HashTable::Fields, bin_hash("newField"), 1, new_field),
            (
                HashTable::Hashes,
                bin_hash("NewJoint"),
                2,
                "mesh.joints[1]".to_owned(),
            ),
            (
                HashTable::Hashes,
                bin_hash("newKey"),
                1,
                format!("sounds{{{:08x}}}", bin_hash("newKey")),
            ),
            (
                HashTable::Types,
                bin_hash("NewMeshClass"),
                1,
                "mesh".to_owned(),
            ),
            (HashTable::Game, file, 1, "mesh.texture".to_owned()),
        ];
        expected.sort();

        for data in [SKIN.as_bytes().to_vec(), binary] {
            let mut collector = Collector::new(&names, &[]);
            collector.document("skin0.bin", None, data).unwrap();
            assert_eq!(listed(&collector.found, &names), expected);
        }

        // A table filter collects only the hashes of that table.
        let mut collector = Collector::new(&names, &[HashTable::Types]);
        collector
            .document("skin0.rito", None, SKIN.as_bytes().to_vec())
            .unwrap();
        assert_eq!(collector.found.len(), 1);
    }

    #[test]
    fn merge_adds_counts_and_keeps_example_of_first_document() {
        let names = names();
        let read = |sources: &[&str]| {
            let mut collector = Collector::new(&names, &[HashTable::Hashes]);
            for source in sources {
                collector
                    .document(source, None, SKIN.as_bytes().to_vec())
                    .unwrap();
            }
            collector.found
        };
        let key = (HashTable::Hashes, bin_hash("NewJoint"));

        // One collector reads the documents in any order.
        let together = read(&["b.bin", "a.bin", "c.bin"]);
        assert_eq!(together[&key].count, 6);
        assert_eq!(together[&key].documents, 3);
        assert_eq!(together[&key].example.source, "a.bin");

        // Two collectors give the same result after the merge.
        let mut merged = read(&["c.bin"]);
        merge(&mut merged, read(&["b.bin", "a.bin"]));
        assert_eq!(merged, together);
    }

    #[test]
    fn document_fails_for_invalid_bin() {
        let names = GameNames::default();
        let mut collector = Collector::new(&names, &[]);
        let error = collector
            .document("broken.bin", None, b"PROP\x03\x00\x00\x00\x00".to_vec())
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("broken.bin is not a valid bin file")
        );
    }
}
