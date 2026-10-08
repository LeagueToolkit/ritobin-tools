//! Writes a bin as a game-data declaration in YAML or as JSON, and builds a bin from the YAML.
//!
//! A declaration writes each value without its type: a number, a string, a list or a mapping.
//! The YAML form is the body of a game-data edit: `links`, and `objects` with a `class` and a
//! `set` per object. [`from_yaml`] builds it back into a bin, and takes the type of each
//! property from a class schema. The JSON form is for scripts. It has no reader.

use std::borrow::Cow;

use indexmap::IndexMap;
use ltk_game_data::{
    ApplyDiagnostic, ClassName, Edit, EntryName, ErrorKind, Names, ObjectEdit, ObjectSkipReason,
    PropertySkipReason, Schema, Value, apply,
};
use ltk_hash::{BinHash, Hash as _, WadHash};
use ltk_meta::{
    Bin, BinFile,
    path::{FieldNames, PropertyPath},
};
use ltk_mimir_cache::Table;
use ltk_ritobin::HashProvider as _;
use miette::{IntoDiagnostic, Result};

use crate::{document::to_bin, hashes::BinHashes};

/// The first line of a YAML declaration that this tool writes.
pub const YAML_HEADER: &str = "# ritobin-tools bin declaration";

/// The key of the class of an object or of a struct in the JSON form. It cannot be the name of
/// a property, because a property name has no `~`.
pub const JSON_CLASS_KEY: &str = "~class";

/// The maximum number of problems that an error of [`from_yaml`] lists.
const MAX_LISTED_PROBLEMS: usize = 10;

/// The names that a declaration is written with.
///
/// It never answers the empty name. A declaration reads the empty string as the hash 0, so the
/// hash of the empty string is written as `0x` hex.
pub struct DeclarationNames<'a>(pub &'a BinHashes);

fn non_empty(name: Option<Cow<'_, str>>) -> Option<Cow<'_, str>> {
    name.filter(|name| !name.is_empty())
}

impl FieldNames for DeclarationNames<'_> {
    fn field(&self, field: BinHash, class: Option<BinHash>) -> Option<Cow<'_, str>> {
        non_empty(self.0.field(field, class))
    }

    fn hash(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        non_empty(self.0.hash(hash))
    }
}

impl Names for DeclarationNames<'_> {
    fn class(&self, class: BinHash) -> Option<Cow<'_, str>> {
        non_empty(self.0.lookup(Table::BinTypes, class))
    }

    fn entry(&self, entry: BinHash) -> Option<Cow<'_, str>> {
        non_empty(self.0.lookup(Table::BinEntries, entry))
    }

    fn file(&self, chunk: u64) -> Option<Cow<'_, str>> {
        non_empty(self.0.lookup_wad(WadHash(chunk)))
    }
}

/// Returns `name` if it is accepted by `accepts` and hashes to `hash`. Otherwise returns the
/// hash as `0x` and 8 hex digits.
fn spelled(hash: BinHash, name: Option<Cow<'_, str>>, accepts: impl Fn(&str) -> bool) -> String {
    match name {
        Some(name) if BinHash::hash_str(&name) == hash && accepts(&name) => name.into_owned(),
        _ => format!("0x{:08x}", hash.0),
    }
}

/// Returns `true` if `name` is one property name without a subscript, so that it is read back
/// as one path segment.
fn is_field_name(name: &str) -> bool {
    PropertyPath::new(name).is_ok_and(|path| {
        let mut segments = path.segments();
        matches!(
            (segments.next(), segments.next()),
            (Some(segment), None) if segment.subscript.is_none()
        )
    })
}

/// One object of a bin, with its names and its rendered properties.
struct Rendered {
    name: String,
    class: String,
    /// The property names and values, in bin order.
    properties: IndexMap<String, Value>,
}

/// Renders the objects of `bin` with the names of `hashes`.
///
/// Fails if a value cannot be written as a declaration: a map that has the same key twice, or a
/// map key of a type that has no text form. The error names the object and the path.
fn render(bin: &Bin, hashes: &BinHashes) -> Result<Vec<Rendered>> {
    let names = DeclarationNames(hashes);
    let mut objects = Vec::with_capacity(bin.objects.len());
    for object in bin.objects.values() {
        let name = spelled(object.path_hash, names.entry(object.path_hash), |name| {
            EntryName::try_from(name).is_ok()
        });
        let class = spelled(object.class_hash, names.class(object.class_hash), |name| {
            ClassName::try_from(name).is_ok()
        });
        let mut properties = IndexMap::with_capacity(object.properties.len());
        for (field, value) in &object.properties {
            let field_name = spelled(
                *field,
                names.field(*field, Some(object.class_hash)),
                is_field_name,
            );
            let rendered = Value::render(value, &names).map_err(|error| {
                let at = error.location.key.as_deref().unwrap_or_default();
                let reason = match error.kind {
                    ErrorKind::UnrenderableKey => {
                        "the map has this key twice, or the key has a type without a text form"
                            .to_owned()
                    }
                    kind => kind.to_string(),
                };
                miette::miette!(
                    "{name}: {field_name}{}{at} cannot be written as a declaration: {reason}. Use ritobin text for this bin",
                    match at.starts_with(['[', '{']) || at.is_empty() {
                        true => "",
                        false => ".",
                    },
                )
            })?;
            properties.insert(field_name, rendered);
        }
        objects.push(Rendered {
            name,
            class,
            properties,
        });
    }
    Ok(objects)
}

/// Returns the bin of `file`. Fails for a `PTCH` file, which has records that a declaration of
/// a bin cannot hold.
fn prop<'a>(file: &'a BinFile, format: &str) -> Result<&'a Bin> {
    match file {
        BinFile::Prop(bin) => Ok(bin),
        BinFile::Override(_) => miette::bail!(
            "A PTCH file cannot be written as {format}. Use `--to rito` or `--to bin`"
        ),
    }
}

/// Writes `file` as a YAML declaration: the dependency list under `links`, and each object
/// under `objects` with its `class` and its properties under `set`.
///
/// Fails for a `PTCH` file and for a value that a declaration cannot hold, see [`render`].
pub fn to_yaml(file: &BinFile, hashes: &BinHashes) -> Result<String> {
    let bin = prop(file, "yaml")?;
    let mut body = IndexMap::new();
    if !bin.dependencies.is_empty() {
        let links = bin.dependencies.iter().cloned().map(Value::String);
        body.insert("links".to_owned(), Value::List(links.collect()));
    }
    let objects: IndexMap<String, Value> = render(bin, hashes)?
        .into_iter()
        .map(|object| {
            let mut fields = IndexMap::from([("class".to_owned(), Value::String(object.class))]);
            if !object.properties.is_empty() {
                fields.insert("set".to_owned(), Value::Mapping(object.properties));
            }
            (object.name, Value::Mapping(fields))
        })
        .collect();
    if !objects.is_empty() {
        body.insert("objects".to_owned(), Value::Mapping(objects));
    }

    let yaml = Value::Mapping(body)
        .to_yaml()
        .map_err(|error| miette::miette!("Failed to write the declaration: {error}"))?;
    Ok(format!("{YAML_HEADER}\n{yaml}\n"))
}

/// Writes `file` as JSON for scripts: `links`, and `objects` with one object per entry. An
/// object and a struct are a JSON object with the property names as keys and the class under
/// [`JSON_CLASS_KEY`]. The JSON cannot be read back into a bin.
///
/// Fails for a `PTCH` file and for a value that a declaration cannot hold, see [`render`].
pub fn to_json(file: &BinFile, hashes: &BinHashes) -> Result<String> {
    let bin = prop(file, "json")?;
    let objects: IndexMap<String, Value> = render(bin, hashes)?
        .into_iter()
        .map(|object| {
            let mut fields =
                IndexMap::from([(JSON_CLASS_KEY.to_owned(), Value::String(object.class))]);
            fields.extend(
                object
                    .properties
                    .into_iter()
                    .map(|(name, value)| (name, flatten(value))),
            );
            (object.name, Value::Mapping(fields))
        })
        .collect();
    let links = bin.dependencies.iter().cloned().map(Value::String);
    let document = Value::Mapping(IndexMap::from([
        ("links".to_owned(), Value::List(links.collect())),
        ("objects".to_owned(), Value::Mapping(objects)),
    ]));
    let mut json = serde_json::to_string_pretty(&document).into_diagnostic()?;
    json.push('\n');
    Ok(json)
}

/// Replaces each struct of `value` by a mapping of its properties with the class under
/// [`JSON_CLASS_KEY`]. A null pointer stays null.
///
/// In a rendered value a struct is a mapping with the one key `pointer` or `embed`, which holds
/// `class` and `set`. A map of a bin never renders to that shape.
fn flatten(value: Value) -> Value {
    let is_struct = value.is_struct_pin();
    match value {
        Value::List(items) => Value::List(items.into_iter().map(flatten).collect()),
        Value::Mapping(mapping) if is_struct => {
            let Some(Value::Mapping(mut body)) = mapping.into_values().next() else {
                return Value::Null;
            };
            let mut fields = IndexMap::new();
            if let Some(class) = body.shift_remove("class") {
                fields.insert(JSON_CLASS_KEY.to_owned(), class);
            }
            if let Some(Value::Mapping(set)) = body.shift_remove("set") {
                fields.extend(set.into_iter().map(|(name, value)| (name, flatten(value))));
            }
            Value::Mapping(fields)
        }
        Value::Mapping(mapping) => Value::Mapping(
            mapping
                .into_iter()
                .map(|(key, value)| (key, flatten(value)))
                .collect(),
        ),
        scalar => scalar,
    }
}

/// Builds the bin of the YAML declaration `text`. `schema` gives the type of each property.
/// `name` is the file name shown in errors.
///
/// Fails if the text is not valid YAML, if it has anything other than `links` and constructed
/// `objects`, or if an object or a property cannot be built. A class or a property that
/// `schema` does not have cannot be built, because its type is unknown.
pub fn from_yaml(name: &str, text: &str, schema: &dyn Schema) -> Result<Bin> {
    let mut options = serde_saphyr::Options::default();
    options.strict_booleans = true;
    options.no_schema = true;
    options.reject_unsupported_tags = false;
    options.duplicate_keys = serde_saphyr::DuplicateKeyPolicy::Error;
    // The default budget stops at 250,000 nodes, which the declaration of a large bin exceeds.
    // The limits on aliases stay in place.
    options.budget = None;
    let edit: Edit = serde_saphyr::from_str_with_options(text, options)
        .map_err(|error| miette::miette!("{name} is not a valid bin declaration: {error}"))?;

    let only_constructs = edit
        .objects
        .values()
        .all(|object| matches!(object, ObjectEdit::Construct { .. }));
    if !edit.overrides.is_empty()
        || !edit.entries.is_empty()
        || !edit.links.remove.is_empty()
        || !only_constructs
    {
        miette::bail!(
            "{name} is not a bin declaration. A bin declaration has only `links` and `objects`, and each object has a `class`. To apply edits to a game bin, use `ritobin-tools gamedata apply`"
        );
    }

    let empty = to_bin(&BinFile::Prop(Bin::builder().build()))?;
    let edits = [edit];
    let result = apply(
        &empty,
        &edits,
        |_| -> Result<Vec<u8>, ltk_game_data::Error> {
            unreachable!("a bin declaration lists no override file")
        },
        // A reference reads the game. A bin declaration has no game to read from.
        |_| Ok(None),
        schema,
    )
    .map_err(|error| miette::miette!("Failed to build {name}: {error}"))?;

    if !result.diagnostics.is_empty() {
        let missing = Missing::of(&edits[0], schema);
        return Err(problems(name, &result.diagnostics, &missing));
    }
    match BinFile::from_reader(&mut std::io::Cursor::new(result.bytes)).into_diagnostic()? {
        BinFile::Prop(bin) => Ok(bin),
        BinFile::Override(_) => miette::bail!("Failed to build {name}: the result is a PTCH file"),
    }
}

/// Finds the classes and the properties of a declaration that a class schema does not have.
///
/// `apply` reports a property that it cannot type by the name of the top-level property that
/// contains it. This walk names the class or the property itself.
struct Missing<'a> {
    schema: &'a dyn Schema,
    /// The name of the object that is being walked.
    entry: &'a str,
    /// One line per class or property that the schema does not have.
    lines: Vec<String>,
}

impl<'a> Missing<'a> {
    /// Returns one line for each class and each property of the objects of `edit` that `schema`
    /// does not have. A line has the object, the path and the class.
    fn of(edit: &'a Edit, schema: &'a dyn Schema) -> Vec<String> {
        let mut missing = Self {
            schema,
            entry: "",
            lines: Vec::new(),
        };
        for (entry, object) in &edit.objects {
            let ObjectEdit::Construct { class, properties } = object else {
                continue;
            };
            missing.entry = entry.as_str();
            if !schema.has_class(class.class_hash()) {
                missing.class(None, class.as_str());
                continue;
            }
            for property in properties {
                let key = property.path.as_str();
                missing.property(key, key, class.as_str(), &property.value);
            }
        }
        missing.lines
    }

    fn class(&mut self, path: Option<&str>, class: &str) {
        self.lines.push(match path {
            Some(path) => format!("{}: {path} has the class {class}", self.entry),
            None => format!("{}: the object has the class {class}", self.entry),
        });
    }

    /// Checks the property `name` of `class`, whose value is at `path`, and the structs inside
    /// its value.
    fn property(&mut self, path: &str, name: &str, class: &str, value: &Value) {
        if self
            .schema
            .expected(name_hash(class), name_hash(name))
            .is_none()
        {
            self.lines.push(format!(
                "{}: {path} is a property of the class {class}",
                self.entry
            ));
            return;
        }
        self.value(path, value);
    }

    /// Checks the structs inside `value`, which is at `path`.
    fn value(&mut self, path: &str, value: &Value) {
        match value {
            Value::List(items) => {
                for (index, item) in items.iter().enumerate() {
                    self.value(&format!("{path}[{index}]"), item);
                }
            }
            Value::Mapping(mapping) if value.is_struct_pin() => {
                let Some(Value::Mapping(body)) = mapping.values().next() else {
                    return;
                };
                let Some(Value::String(class)) = body.get("class") else {
                    return;
                };
                if !self.schema.has_class(name_hash(class)) {
                    self.class(Some(path), class);
                    return;
                }
                if let Some(Value::Mapping(set)) = body.get("set") {
                    for (name, value) in set {
                        self.property(&format!("{path}.{name}"), name, class, value);
                    }
                }
            }
            Value::Mapping(mapping) => {
                for (key, value) in mapping {
                    self.value(&format!("{path}{{{key}}}"), value);
                }
            }
            _ => {}
        }
    }
}

/// Returns the hash that a declaration reads `name` as: the value of `0x` and 8 hex digits, or
/// the hash of the name.
fn name_hash(name: &str) -> BinHash {
    name.strip_prefix("0x")
        .filter(|digits| digits.len() == 8)
        .and_then(|digits| u32::from_str_radix(digits, 16).ok())
        .map_or_else(|| BinHash::hash_str(name), BinHash)
}

/// Returns the error for a declaration with objects or properties that were not built.
/// `missing` lists the classes and the properties that the class schema does not have.
fn problems(name: &str, diagnostics: &[ApplyDiagnostic], missing: &[String]) -> miette::Report {
    let mut untyped = false;
    let mut lines: Vec<String> = diagnostics
        .iter()
        .take(MAX_LISTED_PROBLEMS)
        .map(|diagnostic| {
            let reason = match (&diagnostic.property, &diagnostic.object) {
                (Some(property), _) => {
                    untyped |= matches!(
                        property.reason,
                        PropertySkipReason::Untypable | PropertySkipReason::UnknownClass
                    );
                    format!("{} ({:?})", property.entry.as_str(), property.reason)
                }
                (None, Some(object)) => {
                    untyped |= object.reason == ObjectSkipReason::UnknownClass;
                    format!("{:?}", object.reason)
                }
                (None, None) => diagnostic.detail.clone().unwrap_or_default(),
            };
            format!("  {}: {reason}", diagnostic.kind.message(&diagnostic.path))
        })
        .collect();
    if diagnostics.len() > lines.len() {
        lines.push(format!("  and {} more", diagnostics.len() - lines.len()));
    }
    if !missing.is_empty() {
        lines.push("No game bin uses:".to_owned());
        lines.extend(
            missing
                .iter()
                .take(MAX_LISTED_PROBLEMS)
                .map(|line| format!("  {line}")),
        );
        if missing.len() > MAX_LISTED_PROBLEMS {
            lines.push(format!(
                "  and {} more",
                missing.len() - MAX_LISTED_PROBLEMS
            ));
        }
    }
    let help = match untyped || !missing.is_empty() {
        true => {
            "\nThe type of a property comes from the class schema, which is observed from the bins of the installed game. A class or a property that no game bin uses has no known type. Use ritobin text for this bin."
        }
        false => "",
    };
    miette::miette!(
        "{name} cannot be built into a bin. {} not built:\n{}{help}",
        match diagnostics.len() {
            1 => "1 item was".to_owned(),
            count => format!("{count} items were"),
        },
        lines.join("\n")
    )
}

/// Returns `true` if the two bins encode to the same bytes. The comparison of the encoded
/// bytes treats two equal non-finite floats as equal.
pub fn same_bin(a: &Bin, b: &Bin) -> bool {
    match (
        to_bin(&BinFile::Prop(a.clone())),
        to_bin(&BinFile::Prop(b.clone())),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use ltk_meta::{PropertyValueEnum, property::values};

    use super::*;
    use crate::{
        document::{Document, ReadOptions},
        schema::ObservedSchema,
    };

    /// Returns the value of the property `name` of an object.
    fn property<'a>(bin: &'a Bin, object: u32, name: &str) -> &'a PropertyValueEnum {
        &bin.objects[&BinHash(object)].properties[&BinHash::hash_str(name)]
    }

    const SKIN: &str = r#"#PROP_text
type: string = "PROP"
version: u32 = 3
linked: list[string] = { "DATA/Shared.bin" }
entries: map[hash,embed] = {
    "Characters/Test/Skins/Skin0" = SkinCharacterDataProperties {
        championSkinId: i32 = 17000
        healthBarStyle: u8 = 12
        skinMeshProperties: embed = SkinMeshDataProperties {
            simpleSkin: string = "ASSETS/Test/Test.skn"
            texture: file = "assets/test/test.tex"
            selfIllumination: f32 = 0.7
            boundingBox: option[vec3] = { { 50, 150, 150 } }
            fresnelColor: rgba = { 0, 0, 0, 255 }
            materialOverride: list[embed] = {
                MaterialOverride {
                    submesh: string = "Mushroom"
                }
            }
        }
        animationGraphData: link = "Characters/Test/Animations/Skin0"
        joint: hash = "Hat"
        emptyName: hash = 0x811c9dc5
        nothing: pointer = 0x0 {}
        modifier: pointer = RigPoseModifier {
            joint: hash = "Hat"
        }
        sounds: map[hash,string] = {
            "attack" = "Play_Attack"
        }
        visible: bool = true
        unnamed_field_0123: flag = false
    }
}
"#;

    /// The names of [`SKIN`] that the tables have. `unnamed_field_0123` and the class
    /// `RigPoseModifier` are left out, and the empty name is in the hash table.
    fn hashes() -> (tempfile::TempDir, BinHashes) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        for (table, names) in [
            (
                "entries",
                [
                    "Characters/Test/Skins/Skin0",
                    "Characters/Test/Animations/Skin0",
                ]
                .as_slice(),
            ),
            (
                "types",
                &[
                    "SkinCharacterDataProperties",
                    "SkinMeshDataProperties",
                    "MaterialOverride",
                ],
            ),
            (
                "fields",
                &[
                    "championSkinId",
                    "healthBarStyle",
                    "skinMeshProperties",
                    "simpleSkin",
                    "texture",
                    "selfIllumination",
                    "boundingBox",
                    "fresnelColor",
                    "materialOverride",
                    "submesh",
                    "animationGraphData",
                    "joint",
                    "emptyName",
                    "nothing",
                    "modifier",
                    "sounds",
                    "visible",
                ],
            ),
            ("hashes", &["Hat", "attack", ""]),
        ] {
            let lines: String = names
                .iter()
                .map(|name| format!("{:08x} {name}\n", BinHash::hash_str(name).0))
                .collect();
            std::fs::write(path.join(format!("hashes.bin{table}.txt")), lines).unwrap();
        }
        std::fs::write(
            path.join("hashes.game.txt"),
            format!(
                "{:016x} assets/test/test.tex\n",
                ltk_game_index::chunk_hash("assets/test/test.tex").0
            ),
        )
        .unwrap();
        let hashes = BinHashes::load(None, Some(&path));
        (dir, hashes)
    }

    fn skin() -> BinFile {
        Document::parse("skin0.rito", SKIN.into(), ReadOptions::default())
            .unwrap()
            .file
    }

    fn bin(file: &BinFile) -> &Bin {
        match file {
            BinFile::Prop(bin) => bin,
            BinFile::Override(_) => panic!("not a PROP bin"),
        }
    }

    #[test]
    fn to_yaml_writes_values_without_types_and_structs_as_tags() {
        let (_guard, hashes) = hashes();
        let yaml = to_yaml(&skin(), &hashes).unwrap();
        let unnamed = format!("0x{:08x}", BinHash::hash_str("unnamed_field_0123").0);
        let modifier = format!("0x{:08x}", BinHash::hash_str("RigPoseModifier").0);
        assert_eq!(
            yaml,
            format!(
                r#"# ritobin-tools bin declaration
links: [DATA/Shared.bin]
objects:
  Characters/Test/Skins/Skin0:
    class: SkinCharacterDataProperties
    set:
      championSkinId: 17000
      healthBarStyle: 12
      skinMeshProperties: !embed(SkinMeshDataProperties)
        simpleSkin: ASSETS/Test/Test.skn
        texture: assets/test/test.tex
        selfIllumination: 0.7
        boundingBox:
        - [50.0, 150.0, 150.0]
        fresnelColor: [0, 0, 0, 255]
        materialOverride:
        - !embed(MaterialOverride)
          submesh: Mushroom
      animationGraphData: Characters/Test/Animations/Skin0
      joint: Hat
      emptyName: "0x811c9dc5"
      nothing: null
      modifier: !pointer({modifier})
        joint: Hat
      sounds:
        attack: Play_Attack
      visible: true
      "{unnamed}": false
"#
            )
        );
    }

    #[test]
    fn from_yaml_builds_the_same_bin_with_the_schema_of_the_bin() {
        let (_guard, hashes) = hashes();
        let file = skin();
        let schema = ObservedSchema::of(&file);

        // With names, and with every name written as a hash.
        for hashes in [&hashes, &BinHashes::none()] {
            let yaml = to_yaml(&file, hashes).unwrap();
            let built = from_yaml("skin0.yaml", &yaml, &schema).unwrap();
            assert!(same_bin(bin(&file), &built), "{yaml}");
        }

        // The types come from the schema, not from the text.
        let built = from_yaml("skin0.yaml", &to_yaml(&file, &hashes).unwrap(), &schema).unwrap();
        let object = BinHash::hash_str("Characters/Test/Skins/Skin0").0;
        assert_eq!(
            property(&built, object, "healthBarStyle"),
            &values::U8::new(12).into()
        );
        assert_eq!(
            property(&built, object, "emptyName"),
            &values::Hash::new(BinHash(0x811c_9dc5)).into()
        );
    }

    #[test]
    fn from_yaml_fails_for_class_or_property_that_schema_does_not_have() {
        let (_guard, hashes) = hashes();
        let file = skin();
        let yaml = to_yaml(&file, &hashes).unwrap();

        let error = from_yaml("skin0.yaml", &yaml, &ObservedSchema::default()).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("skin0.yaml cannot be built into a bin. 1 item was not built"),
            "{text}"
        );
        assert!(text.contains("UnknownClass"), "{text}");
        assert!(
            text.contains("observed from the bins of the installed game"),
            "{text}"
        );

        // A property that the class does not have in the schema.
        let schema = ObservedSchema::of(&file);
        let extra = yaml.replace(
            "      visible: true\n",
            "      visible: true\n      newField: 3\n",
        );
        let text = from_yaml("skin0.yaml", &extra, &schema)
            .unwrap_err()
            .to_string();
        assert!(
            text.contains(
                "Characters/Test/Skins/Skin0: newField is a property of the class SkinCharacterDataProperties"
            ),
            "{text}"
        );

        // A property and a class inside a struct are named with their path.
        let nested = yaml
            .replace(
                "          submesh: Mushroom\n",
                "          submesh: Mushroom\n          extra: 1\n",
            )
            .replace("!pointer(", "!pointer(New");
        let text = from_yaml("skin0.yaml", &nested, &schema)
            .unwrap_err()
            .to_string();
        assert!(
            text.contains("skinMeshProperties.materialOverride[0].extra is a property of the class MaterialOverride"),
            "{text}"
        );
        assert!(text.contains("modifier has the class New0x"), "{text}");
        assert!(text.contains("Untypable"), "{text}");
    }

    #[test]
    fn from_yaml_fails_for_edit_manifest_body_and_for_invalid_yaml() {
        let schema = ObservedSchema::default();
        let edits = "Characters/Test/Skins/Skin0:\n  visible: false\n";
        let error = from_yaml("edit.yaml", edits, &schema).unwrap_err();
        assert!(error.to_string().contains("is not a bin declaration"));

        let error = from_yaml("broken.yaml", "objects: [", &schema).unwrap_err();
        assert!(error.to_string().contains("is not a valid bin declaration"));

        assert!(
            from_yaml("empty.yaml", "{}", &schema)
                .unwrap()
                .objects
                .is_empty()
        );
    }

    #[test]
    fn to_json_writes_structs_as_objects_with_class_key() {
        let (_guard, hashes) = hashes();
        let json = to_json(&skin(), &hashes).unwrap();
        let document: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(document["links"], serde_json::json!(["DATA/Shared.bin"]));

        let object = &document["objects"]["Characters/Test/Skins/Skin0"];
        assert_eq!(object["~class"], "SkinCharacterDataProperties");
        assert_eq!(object["championSkinId"], 17000);
        assert_eq!(object["nothing"], serde_json::Value::Null);
        assert_eq!(
            object["sounds"],
            serde_json::json!({"attack": "Play_Attack"})
        );
        let mesh = &object["skinMeshProperties"];
        assert_eq!(mesh["~class"], "SkinMeshDataProperties");
        assert_eq!(mesh["texture"], "assets/test/test.tex");
        assert_eq!(mesh["fresnelColor"], serde_json::json!([0, 0, 0, 255]));
        assert_eq!(
            mesh["materialOverride"],
            serde_json::json!([{"~class": "MaterialOverride", "submesh": "Mushroom"}])
        );
        assert_eq!(object["modifier"]["joint"], "Hat");
    }

    #[test]
    fn declaration_fails_for_ptch_file_and_for_map_with_duplicate_key() {
        let patch: BinFile = ltk_meta::BinOverride::builder()
            .delete(0x1u32)
            .build()
            .into();
        let error = to_yaml(&patch, &BinHashes::none()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("A PTCH file cannot be written as yaml")
        );
        assert!(to_json(&patch, &BinHashes::none()).is_err());

        let duplicate = SKIN.replace(
            "\"attack\" = \"Play_Attack\"",
            "\"attack\" = \"Play_Attack\"\n            \"attack\" = \"Play_Other\"",
        );
        let document = Document::parse(
            "skin0.rito",
            duplicate.into(),
            ReadOptions { lenient: true },
        )
        .unwrap();
        let sounds = property(
            bin(&document.file),
            BinHash::hash_str("Characters/Test/Skins/Skin0").0,
            "sounds",
        );
        assert!(matches!(sounds, PropertyValueEnum::Map(map) if map.entries().len() == 2));

        let text = to_yaml(&document.file, &BinHashes::none())
            .unwrap_err()
            .to_string();
        assert!(text.contains("the map has this key twice"), "{text}");
        assert!(text.contains("Use ritobin text"), "{text}");
    }
}
