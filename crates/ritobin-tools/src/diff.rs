//! The structural difference between two bins.
//!
//! [`BinDiff::between`] produces two things. The change list names every position where the two
//! bins differ, including what the edit removes. The patch is what [`Bin::diff_with`] makes of the
//! same difference: a `PTCH` that turns the base into the edit as far as patch records can say it.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use ltk_hash::BinHash;
use ltk_meta::{
    Bin, BinObject, BinOverride, DiffOptions, DiffReport, Lift, PropertyValueEnum,
    path::{MapKey, ValuePath},
    property::values,
};
use ltk_mimir_cache::Table;
use serde::Serialize;

use crate::{
    document::{TextLayout, to_text},
    hashes::{BinHashes, format_hash},
};

/// What one [`Change`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// The edit has an object the base does not.
    ObjectAdded,
    /// The base has an object the edit does not.
    ObjectRemoved,
    /// Both have the object, with different classes.
    ObjectReplaced,
    /// The edit has a value the base does not: a property, a list item or a map entry.
    Added,
    /// The base has a value the edit does not.
    Removed,
    /// Both have the value, and it differs.
    Changed,
    /// The edit links a bin the base does not.
    DependencyAdded,
    /// The base links a bin the edit does not.
    DependencyRemoved,
}

/// One difference between two bins.
///
/// Hashes are written as ritobin text writes them: the name where a table has one, `0x` hex
/// otherwise. Values are ritobin text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub kind: ChangeKind,
    /// The path hash of the object the change is in. `None` for a dependency.
    pub object: Option<String>,
    /// The object's path, where the entry table has it.
    pub object_name: Option<String>,
    /// The class of the object.
    pub class: Option<String>,
    /// Where the value is inside the object. `None` for a whole object or a dependency.
    pub path: Option<String>,
    /// The ritobin type of the value.
    #[serde(rename = "type")]
    pub value_type: Option<String>,
    /// The value in the base.
    pub old: Option<String>,
    /// The value in the edit.
    pub new: Option<String>,
}

/// A place the patch could not record the difference where it is, and recorded it higher up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lifted {
    pub object: String,
    pub object_name: Option<String>,
    pub path: String,
    pub reason: String,
}

/// How many changes of each kind a diff holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub objects_added: usize,
    pub objects_removed: usize,
    pub objects_replaced: usize,
    pub objects_changed: usize,
    pub values_added: usize,
    pub values_removed: usize,
    pub values_changed: usize,
    pub dependencies_added: usize,
    pub dependencies_removed: usize,
}

/// The difference between a base bin and an edited one.
#[derive(Debug, Clone)]
pub struct BinDiff {
    /// Every difference, in the edit's order, with what only the base has after it.
    pub changes: Vec<Change>,
    /// The patch that turns the base into the edit, as far as patch records can say it.
    pub patch: BinOverride,
    /// What the patch holds, and where it had to record more than the change.
    pub report: DiffReport,
    /// Whether the patch applied to the base gives exactly the edit's objects.
    ///
    /// It does not when the edit removes a property or a map entry, or removes an object while
    /// deletions are off. No patch record says any of those.
    pub patch_is_exact: bool,
}

impl BinDiff {
    /// Diffs `edited` against `base`. `deletions` puts the objects only the base has on the
    /// patch's delete list.
    pub fn between(base: &Bin, edited: &Bin, hashes: &BinHashes, deletions: bool) -> Self {
        let mut walker = Walker {
            hashes,
            changes: Vec::new(),
            object: BinHash(0),
            class: BinHash(0),
            path: ValuePath::new(),
        };
        walker.bins(base, edited);

        let mut options = DiffOptions::default();
        options.deletions = deletions;
        let (patch, report) = base.diff_with(edited, hashes, &options);

        let mut patched = base.clone();
        patch.clone().apply(&mut patched);
        let patch_is_exact = patched.objects.len() == edited.objects.len()
            && edited.objects.iter().all(|(object_hash, object)| {
                patched
                    .objects
                    .get(object_hash)
                    .is_some_and(|patched| patched == object || same_text(patched, object))
            });

        Self {
            changes: walker.changes,
            patch,
            report,
            patch_is_exact,
        }
    }

    /// Whether the two bins hold the same objects and dependencies.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn summary(&self) -> Summary {
        let mut summary = Summary::default();
        let mut changed_objects: Vec<&str> = Vec::new();
        for change in &self.changes {
            let counter = match change.kind {
                ChangeKind::ObjectAdded => &mut summary.objects_added,
                ChangeKind::ObjectRemoved => &mut summary.objects_removed,
                ChangeKind::ObjectReplaced => &mut summary.objects_replaced,
                ChangeKind::Added => &mut summary.values_added,
                ChangeKind::Removed => &mut summary.values_removed,
                ChangeKind::Changed => &mut summary.values_changed,
                ChangeKind::DependencyAdded => &mut summary.dependencies_added,
                ChangeKind::DependencyRemoved => &mut summary.dependencies_removed,
            };
            *counter += 1;
            if matches!(
                change.kind,
                ChangeKind::Added | ChangeKind::Removed | ChangeKind::Changed
            ) && let Some(object) = change.object.as_deref()
                && changed_objects.last() != Some(&object)
            {
                changed_objects.push(object);
            }
        }
        summary.objects_changed = changed_objects.len();
        summary
    }

    /// The places the patch recorded more than the change, named with `hashes`.
    pub fn lifted(&self, hashes: &BinHashes) -> Vec<Lifted> {
        self.report
            .lifted
            .iter()
            .map(|lift| Lifted {
                object: format_hash(lift.object_hash()),
                object_name: hashes
                    .lookup(Table::BinEntries, lift.object_hash())
                    .map(Into::into),
                path: lift.at().to_named(hashes).text,
                reason: match lift {
                    Lift::MapInsert { keys, .. } => format!("{keys} map entries inserted"),
                    Lift::Nameless { cause, .. } => cause.to_string(),
                    Lift::Mismatch { .. } => "shapes differ".to_owned(),
                    other => other.to_string(),
                },
            })
            .collect()
    }
}

struct Walker<'a> {
    hashes: &'a BinHashes,
    changes: Vec<Change>,
    object: BinHash,
    class: BinHash,
    path: ValuePath,
}

impl Walker<'_> {
    fn bins(&mut self, base: &Bin, edited: &Bin) {
        for dependency in &edited.dependencies {
            if !base.dependencies.contains(dependency) {
                self.dependency(ChangeKind::DependencyAdded, dependency);
            }
        }
        for dependency in &base.dependencies {
            if !edited.dependencies.contains(dependency) {
                self.dependency(ChangeKind::DependencyRemoved, dependency);
            }
        }

        for (object_hash, object) in &edited.objects {
            self.enter(object);
            match base.objects.get(object_hash) {
                None => self.whole_object(ChangeKind::ObjectAdded, None, None),
                Some(existing) if existing.class_hash != object.class_hash => self.whole_object(
                    ChangeKind::ObjectReplaced,
                    Some(self.class_name(existing.class_hash)),
                    Some(self.class_name(object.class_hash)),
                ),
                Some(existing) => {
                    self.properties(&existing.properties, &object.properties, object.class_hash)
                }
            }
        }
        for (object_hash, object) in &base.objects {
            if !edited.objects.contains_key(object_hash) {
                self.enter(object);
                self.whole_object(ChangeKind::ObjectRemoved, None, None);
            }
        }
    }

    fn enter(&mut self, object: &BinObject) {
        self.object = object.path_hash;
        self.class = object.class_hash;
        self.path = ValuePath::new();
    }

    fn class_name(&self, class: BinHash) -> String {
        match self.hashes.lookup(Table::BinTypes, class) {
            Some(name) => name.into_owned(),
            None => format_hash(class),
        }
    }

    fn dependency(&mut self, kind: ChangeKind, dependency: &str) {
        let (old, new) = match kind {
            ChangeKind::DependencyRemoved => (Some(dependency.to_owned()), None),
            _ => (None, Some(dependency.to_owned())),
        };
        self.changes.push(Change {
            kind,
            object: None,
            object_name: None,
            class: None,
            path: None,
            value_type: None,
            old,
            new,
        });
    }

    fn whole_object(&mut self, kind: ChangeKind, old: Option<String>, new: Option<String>) {
        let change = self.change(kind, None, None, old, new);
        self.changes.push(change);
    }

    fn change(
        &self,
        kind: ChangeKind,
        path: Option<String>,
        value_type: Option<String>,
        old: Option<String>,
        new: Option<String>,
    ) -> Change {
        Change {
            kind,
            object: Some(format_hash(self.object)),
            object_name: self
                .hashes
                .lookup(Table::BinEntries, self.object)
                .map(Into::into),
            class: Some(self.class_name(self.class)),
            path,
            value_type,
            old,
            new,
        }
    }

    /// A change of the value at the current path.
    fn value_change(
        &mut self,
        kind: ChangeKind,
        old: Option<&PropertyValueEnum>,
        new: Option<&PropertyValueEnum>,
    ) {
        let old = old.map(|value| value_text(value, self.hashes));
        let new = new.map(|value| value_text(value, self.hashes));
        let value_type = match (&old, &new) {
            (Some((old_type, _)), Some((new_type, _))) if old_type != new_type => {
                Some(format!("{old_type} -> {new_type}"))
            }
            (_, Some((value_type, _))) | (Some((value_type, _)), None) => Some(value_type.clone()),
            (None, None) => None,
        };
        let path = self.path.to_named(self.hashes).text;
        let change = self.change(
            kind,
            Some(path),
            value_type,
            old.map(|(_, text)| text),
            new.map(|(_, text)| text),
        );
        self.changes.push(change);
    }

    fn properties(
        &mut self,
        base: &IndexMap<BinHash, PropertyValueEnum>,
        edited: &IndexMap<BinHash, PropertyValueEnum>,
        class: BinHash,
    ) {
        for (field, value) in edited {
            self.path.push_field(*field, class);
            match base.get(field) {
                Some(existing) => self.value(existing, value),
                None => self.value_change(ChangeKind::Added, None, Some(value)),
            }
            self.path.pop();
        }
        for (field, value) in base {
            if !edited.contains_key(field) {
                self.path.push_field(*field, class);
                self.value_change(ChangeKind::Removed, Some(value), None);
                self.path.pop();
            }
        }
    }

    fn value(&mut self, base: &PropertyValueEnum, edited: &PropertyValueEnum) {
        use PropertyValueEnum as V;

        if base == edited {
            return;
        }
        match (base, edited) {
            (V::Struct(b), V::Struct(e))
            | (V::Embedded(values::Embedded(b)), V::Embedded(values::Embedded(e)))
                if b.class_hash == e.class_hash =>
            {
                self.properties(&b.properties, &e.properties, e.class_hash);
            }
            (V::Container(b), V::Container(e))
            | (
                V::UnorderedContainer(values::UnorderedContainer(b)),
                V::UnorderedContainer(values::UnorderedContainer(e)),
            ) if b.item_kind() == e.item_kind() => self.items(b.items(), e.items()),
            (V::Optional(b), V::Optional(e)) if b.item_kind() == e.item_kind() => {
                match (b.value(), e.value()) {
                    (Some(b), Some(e)) => {
                        self.path.push_index(0);
                        self.value(b, e);
                        self.path.pop();
                    }
                    _ => self.changed(base, edited),
                }
            }
            (V::Map(b), V::Map(e))
                if b.key_kind() == e.key_kind() && b.value_kind() == e.value_kind() =>
            {
                if !self.entries(b, e) {
                    self.changed(base, edited);
                }
            }
            _ => self.changed(base, edited),
        }
    }

    /// Reports two values that are not equal as changed, unless they only compare unequal: a NaN
    /// is not equal to itself.
    fn changed(&mut self, base: &PropertyValueEnum, edited: &PropertyValueEnum) {
        if !same_text(base, edited) {
            self.value_change(ChangeKind::Changed, Some(base), Some(edited));
        }
    }

    /// Diffs two lists item by item. An item past the end of the shorter list is added or removed.
    fn items(&mut self, base: &[PropertyValueEnum], edited: &[PropertyValueEnum]) {
        for index in 0..base.len().max(edited.len()) {
            self.path.push_index(index);
            match (base.get(index), edited.get(index)) {
                (Some(b), Some(e)) => self.value(b, e),
                (None, Some(e)) => self.value_change(ChangeKind::Added, None, Some(e)),
                (Some(b), None) => self.value_change(ChangeKind::Removed, Some(b), None),
                (None, None) => {}
            }
            self.path.pop();
        }
    }

    /// Diffs two maps entry by entry. `false` when they cannot be matched by key, which leaves
    /// the maps to be reported whole: a key has no [`MapKey`], or a map repeats a key.
    fn entries(&mut self, base: &values::Map, edited: &values::Map) -> bool {
        let keys = |map: &values::Map| {
            map.entries()
                .iter()
                .map(|(key, _)| MapKey::try_from(key).ok())
                .collect::<Option<Vec<_>>>()
        };
        let (Some(base_keys), Some(edited_keys)) = (keys(base), keys(edited)) else {
            return false;
        };
        let base_index: HashMap<&MapKey, &PropertyValueEnum> = base_keys
            .iter()
            .zip(base.entries())
            .map(|(key, (_, value))| (key, value))
            .collect();
        let edited_index: HashSet<&MapKey> = edited_keys.iter().collect();
        if base_index.len() != base_keys.len() || edited_index.len() != edited_keys.len() {
            return false;
        }

        for (key, (_, value)) in edited_keys.iter().zip(edited.entries()) {
            self.path.push_key(key.clone());
            match base_index.get(key) {
                Some(existing) => self.value(existing, value),
                None => self.value_change(ChangeKind::Added, None, Some(value)),
            }
            self.path.pop();
        }
        for (key, (_, value)) in base_keys.iter().zip(base.entries()) {
            if !edited_index.contains(key) {
                self.path.push_key(key.clone());
                self.value_change(ChangeKind::Removed, Some(value), None);
                self.path.pop();
            }
        }
        true
    }
}

/// Whether two values that compare unequal print the same. They do when the only thing between
/// them is a NaN, which is not equal to itself.
fn same_text<T: std::fmt::Debug>(a: &T, b: &T) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

/// The ritobin type and text of one value, as the printer writes them inside an object.
pub fn value_text(value: &PropertyValueEnum, hashes: &BinHashes) -> (String, String) {
    // The printer has no entry point for a lone value, so the value is printed as the only
    // property of a one-object bin and cut back out.
    let bin = Bin::builder()
        .object(
            BinObject::builder(1u32, 1u32)
                .property(1u32, value.clone())
                .build(),
        )
        .build();
    let layout = TextLayout {
        inline_structs: false,
        ..TextLayout::default()
    };
    to_text(&bin.into(), layout, hashes)
        .ok()
        .and_then(|text| cut_value(&text, layout.indent_size))
        .unwrap_or_else(|| (String::new(), format!("{value:?}")))
}

/// Cuts the single property out of a printed one-object bin: its type, and its value with the
/// indentation of the object removed.
fn cut_value(text: &str, indent_size: usize) -> Option<(String, String)> {
    let property_indent = " ".repeat(indent_size * 2);
    let object_close = format!("{}}}", " ".repeat(indent_size));

    let mut lines = text
        .lines()
        .skip_while(|line| !line.starts_with(&property_indent));
    let first = lines.next()?.strip_prefix(&property_indent)?;
    let (_, typed) = first.split_once(": ")?;
    let (value_type, head) = typed.split_once(" = ")?;

    let mut value = head.to_owned();
    for line in lines.take_while(|line| *line != object_close) {
        value.push('\n');
        value.push_str(line.strip_prefix(&property_indent).unwrap_or(line));
    }
    Some((value_type.to_owned(), value))
}

#[cfg(test)]
mod tests {
    use ltk_hash::Hash as _;
    use ltk_meta::property::Kind;

    use super::*;

    const OBJECT: u32 = 0x1111_0001;
    const CLASS: u32 = 0xaaaa_0001;

    fn bin(properties: impl IntoIterator<Item = (u32, PropertyValueEnum)>) -> Bin {
        let mut object = BinObject::new(OBJECT, CLASS);
        for (field, value) in properties {
            object.properties.insert(BinHash(field), value);
        }
        Bin::builder().object(object).build()
    }

    fn diff(base: &Bin, edited: &Bin) -> BinDiff {
        BinDiff::between(base, edited, &BinHashes::none(), false)
    }

    fn kinds(diff: &BinDiff) -> Vec<ChangeKind> {
        diff.changes.iter().map(|change| change.kind).collect()
    }

    #[test]
    fn value_text_gives_the_type_and_the_value() {
        let hashes = BinHashes::none();
        assert_eq!(
            value_text(&values::I32::new(42).into(), &hashes),
            ("i32".to_owned(), "42".to_owned())
        );
        assert_eq!(
            value_text(&values::String::from("a = b: c").into(), &hashes),
            ("string".to_owned(), "\"a = b: c\"".to_owned())
        );

        let optional = values::Optional::new(Kind::U32, Some(values::U32::new(7).into())).unwrap();
        assert_eq!(
            value_text(&optional.into(), &hashes),
            ("option[u32]".to_owned(), "{ 7 }".to_owned())
        );
    }

    #[test]
    fn value_text_removes_the_indentation_of_a_multi_line_value() {
        let mut inner = values::Struct {
            class_hash: BinHash(0xc1),
            properties: IndexMap::new(),
        };
        inner
            .properties
            .insert(BinHash(0x10), values::I32::new(1).into());
        inner
            .properties
            .insert(BinHash(0x11), values::I32::new(2).into());

        let (value_type, text) = value_text(&values::Embedded(inner).into(), &BinHashes::none());
        assert_eq!(value_type, "embed");
        assert_eq!(text, "0xc1 {\n    0x10: i32 = 1\n    0x11: i32 = 2\n}");
    }

    #[test]
    fn identical_bins_have_no_changes_and_an_empty_patch() {
        let base = bin([(0x10, values::I32::new(1).into())]);
        let diff = diff(&base, &base.clone());
        assert!(diff.is_empty());
        assert!(diff.patch.is_empty());
        assert!(diff.patch_is_exact);
    }

    #[test]
    fn a_changed_added_and_removed_property_are_each_reported() {
        let base = bin([
            (0x10, values::I32::new(1).into()),
            (0x11, values::Bool::new(true).into()),
        ]);
        let edited = bin([
            (0x10, values::I32::new(2).into()),
            (0x12, values::String::from("new").into()),
        ]);

        let diff = diff(&base, &edited);
        assert_eq!(
            kinds(&diff),
            [ChangeKind::Changed, ChangeKind::Added, ChangeKind::Removed]
        );

        let changed = &diff.changes[0];
        assert_eq!(changed.object.as_deref(), Some("0x11110001"));
        assert_eq!(changed.class.as_deref(), Some("0xaaaa0001"));
        assert_eq!(changed.path.as_deref(), Some("00000010"));
        assert_eq!(changed.value_type.as_deref(), Some("i32"));
        assert_eq!(
            (changed.old.as_deref(), changed.new.as_deref()),
            (Some("1"), Some("2"))
        );

        let summary = diff.summary();
        assert_eq!(summary.objects_changed, 1);
        assert_eq!(
            (
                summary.values_changed,
                summary.values_added,
                summary.values_removed
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn a_removed_property_makes_the_patch_inexact() {
        let base = bin([
            (0x10, values::I32::new(1).into()),
            (0x11, values::Bool::new(true).into()),
        ]);
        let edited = bin([(0x10, values::I32::new(1).into())]);

        let diff = diff(&base, &edited);
        assert_eq!(kinds(&diff), [ChangeKind::Removed]);
        assert!(!diff.patch_is_exact);
    }

    #[test]
    fn list_items_are_diffed_by_index() {
        let list = |items: &[u32]| -> PropertyValueEnum {
            values::Container::new(
                Kind::U32,
                items
                    .iter()
                    .map(|item| values::U32::new(*item).into())
                    .collect(),
            )
            .unwrap()
            .into()
        };
        let diff = diff(
            &bin([(0x10, list(&[1, 2, 3]))]),
            &bin([(0x10, list(&[1, 9]))]),
        );

        assert_eq!(kinds(&diff), [ChangeKind::Changed, ChangeKind::Removed]);
        assert_eq!(diff.changes[0].path.as_deref(), Some("00000010[1]"));
        assert_eq!(diff.changes[1].path.as_deref(), Some("00000010[2]"));
    }

    #[test]
    fn map_entries_are_diffed_by_key() {
        let map = |entries: &[(u32, i32)]| -> PropertyValueEnum {
            values::Map::new(
                Kind::Hash,
                Kind::I32,
                entries
                    .iter()
                    .map(|(key, value)| {
                        (
                            values::Hash::new(*key).into(),
                            values::I32::new(*value).into(),
                        )
                    })
                    .collect(),
            )
            .unwrap()
            .into()
        };
        let diff = diff(
            &bin([(0x10, map(&[(1, 10), (2, 20)]))]),
            &bin([(0x10, map(&[(2, 21), (3, 30)]))]),
        );

        assert_eq!(
            kinds(&diff),
            [ChangeKind::Changed, ChangeKind::Added, ChangeKind::Removed]
        );
        assert_eq!(
            (
                diff.changes[0].old.as_deref(),
                diff.changes[0].new.as_deref()
            ),
            (Some("20"), Some("21"))
        );
    }

    #[test]
    fn objects_and_dependencies_on_one_side_are_reported() {
        let base = Bin::builder()
            .dependency("shared.bin")
            .dependency("old.bin")
            .object(BinObject::new(0x1, 0xc1))
            .object(BinObject::new(0x2, 0xc1))
            .build();
        let edited = Bin::builder()
            .dependency("shared.bin")
            .dependency("new.bin")
            .object(BinObject::new(0x2, 0xc2))
            .object(BinObject::new(0x3, 0xc1))
            .build();

        let diff = diff(&base, &edited);
        assert_eq!(
            kinds(&diff),
            [
                ChangeKind::DependencyAdded,
                ChangeKind::DependencyRemoved,
                ChangeKind::ObjectReplaced,
                ChangeKind::ObjectAdded,
                ChangeKind::ObjectRemoved,
            ]
        );
    }

    #[test]
    fn deletions_put_removed_objects_on_the_patch() {
        let base = Bin::builder()
            .object(BinObject::new(0x1, 0xc1))
            .object(BinObject::new(0x2, 0xc1))
            .build();
        let edited = Bin::builder().object(BinObject::new(0x1, 0xc1)).build();

        let kept = BinDiff::between(&base, &edited, &BinHashes::none(), false);
        assert!(kept.patch.deleted.is_empty());
        assert!(!kept.patch_is_exact);

        let deleted = BinDiff::between(&base, &edited, &BinHashes::none(), true);
        assert_eq!(deleted.patch.deleted, [BinHash(0x2)]);
        assert!(deleted.patch_is_exact);
    }

    #[test]
    fn an_unnamed_field_is_lifted_to_the_whole_object() {
        let field = BinHash::hash_str("mNotInAnyTable").0;
        let base = bin([(field, values::I32::new(1).into())]);
        let edited = bin([(field, values::I32::new(2).into())]);

        let diff = diff(&base, &edited);
        assert_eq!(kinds(&diff), [ChangeKind::Changed]);
        assert!(diff.patch.patches.is_empty());
        assert_eq!(diff.patch.objects.len(), 1);
        assert!(diff.patch_is_exact);

        let lifted = diff.lifted(&BinHashes::none());
        assert_eq!(lifted.len(), 1);
        assert_eq!(lifted[0].object, "0x11110001");
    }

    #[test]
    fn a_nan_does_not_differ_from_itself() {
        let nan = |other: i32| {
            let list = values::Container::new(
                Kind::F32,
                vec![
                    values::F32::new(1.0).into(),
                    values::F32::new(f32::NAN).into(),
                ],
            )
            .unwrap();
            bin([
                (0x10, values::F32::new(f32::NAN).into()),
                (0x11, list.into()),
                (0x12, values::I32::new(other).into()),
            ])
        };

        let same = diff(&nan(1), &nan(1));
        assert!(same.is_empty());
        assert!(same.patch_is_exact);

        let other = diff(&nan(1), &nan(2));
        assert_eq!(kinds(&other), [ChangeKind::Changed]);
        assert_eq!(other.changes[0].path.as_deref(), Some("00000012"));
    }

    #[test]
    fn a_map_that_repeats_a_key_is_reported_whole() {
        let map = |entries: &[(u32, i32)]| -> PropertyValueEnum {
            values::Map::new(
                Kind::U32,
                Kind::I32,
                entries
                    .iter()
                    .map(|(key, value)| {
                        (
                            values::U32::new(*key).into(),
                            values::I32::new(*value).into(),
                        )
                    })
                    .collect(),
            )
            .unwrap()
            .into()
        };
        let diff = diff(
            &bin([(0x10, map(&[(1, 1), (1, 2)]))]),
            &bin([(0x10, map(&[(1, 2)]))]),
        );

        assert_eq!(kinds(&diff), [ChangeKind::Changed]);
        assert_eq!(diff.changes[0].path.as_deref(), Some("00000010"));
        assert_eq!(diff.changes[0].value_type.as_deref(), Some("map[u32, i32]"));
    }

    #[test]
    fn a_change_of_type_names_both_types() {
        let diff = diff(
            &bin([(0x10, values::I32::new(1).into())]),
            &bin([(0x10, values::U32::new(1).into())]),
        );
        assert_eq!(kinds(&diff), [ChangeKind::Changed]);
        assert_eq!(diff.changes[0].value_type.as_deref(), Some("i32 -> u32"));
    }
}
