//! Game-data declarations: a manifest of edits to the game's bins, and what applying it to the
//! installed game gives.
//!
//! The manifest format and the edits themselves are `ltk_game_data`'s. This module finds the bins
//! the edits are about in the game, runs the edits over them in manifest order, and keeps what
//! did not apply.

use std::{cell::RefCell, collections::HashMap};

use camino::{Utf8Path, Utf8PathBuf};
use indexmap::IndexMap;
use ltk_game_data::{
    ApplyDiagnostic, ApplyDiagnosticKind, Declarations, Edit, MANIFEST_NAMES, Module, NoSchema,
    PropertySkipReason, Selector, load_declarations,
};
use ltk_hash::WadHash;
use miette::{Diagnostic, NamedSource, Result, SourceSpan};
use serde::Serialize;
use thiserror::Error;

use crate::game::Game;

/// A manifest with the directory its source and override files are read from.
pub struct Layer {
    dir: Utf8PathBuf,
    pub declarations: Declarations,
}

/// A manifest or source file that does not load, shown at the place the loader names.
#[derive(Debug, Error, Diagnostic)]
#[error("{message}")]
struct ManifestProblem {
    message: String,
    #[source_code]
    source_code: NamedSource<String>,
    #[label("here")]
    span: SourceSpan,
}

impl Layer {
    /// Loads the manifest at `path`, or the one in the directory `path`.
    pub fn load(path: &Utf8Path) -> Result<Self> {
        let (dir, name) = match path.is_dir() {
            true => (path.to_owned(), manifest_in(path)?),
            false => {
                let name = path
                    .file_name()
                    .ok_or_else(|| miette::miette!("{path} is not a manifest file"))?;
                let dir = path.parent().filter(|dir| !dir.as_str().is_empty());
                (
                    dir.unwrap_or(Utf8Path::new(".")).to_owned(),
                    name.to_owned(),
                )
            }
        };

        // What was read, by the name the loader knows it under, for showing a problem in it.
        let texts: RefCell<HashMap<String, String>> = RefCell::default();
        let read = |name: &str| {
            let text = read_inside(&dir, name)
                .and_then(|data| {
                    String::from_utf8(data).map_err(|_| "the file is not UTF-8".to_owned())
                })
                .map_err(|error| ltk_game_data::Error::io(name, &error))?;
            texts.borrow_mut().insert(name.to_owned(), text.clone());
            Ok(text)
        };

        let text = read(&name).map_err(|error| miette::miette!("{error}"))?;
        let declarations = load_declarations(&name, &text, read).map_err(|error| {
            let texts = texts.borrow();
            let document = error.location.document.as_deref().unwrap_or(&name);
            match (texts.get(document), error.location.span) {
                (Some(text), Some(span)) => {
                    let start = span.start.min(text.len());
                    let end = span.end.clamp(start, text.len());
                    // A parser's statement goes on to quote the text, which the label shows.
                    let statement = error.kind.to_string();
                    miette::Report::new(ManifestProblem {
                        message: statement.lines().next().unwrap_or_default().to_owned(),
                        source_code: NamedSource::new(dir.join(document), text.clone()),
                        span: (start..end).into(),
                    })
                }
                _ => miette::miette!("{error}"),
            }
        })?;

        Ok(Self { dir, declarations })
    }

    /// Reads the layer's file at the layer-relative `path`.
    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        read_inside(&self.dir, path)
    }
}

/// Finds the manifest of the layer directory `dir`, which must hold exactly one.
fn manifest_in(dir: &Utf8Path) -> Result<String> {
    let found: Vec<&str> = MANIFEST_NAMES
        .into_iter()
        .filter(|name| dir.join(name).is_file())
        .collect();
    match found.as_slice() {
        [name] => Ok((*name).to_owned()),
        [] => miette::bail!(
            "{dir} has no manifest. One is named {}",
            MANIFEST_NAMES.join(", ")
        ),
        several => miette::bail!(
            "{dir} has several manifests ({}), and a layer has one",
            several.join(", ")
        ),
    }
}

/// Reads `dir/path`, refusing a path that leads out of `dir`.
fn read_inside(dir: &Utf8Path, path: &str) -> Result<Vec<u8>, String> {
    let file = dir.join(path);
    let inside = |file: &Utf8Path| -> std::io::Result<bool> {
        Ok(file.canonicalize()?.starts_with(dir.canonicalize()?))
    };
    match inside(&file) {
        Ok(true) => std::fs::read(&file).map_err(|error| error.to_string()),
        Ok(false) => Err("the path leads out of the manifest's directory".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// What the edits of a manifest changed in one bin, counted over every edit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    /// Records of override files that applied.
    pub records: usize,
    /// Objects created, removed, or changed by an override file.
    pub objects: usize,
    /// Property edits that applied.
    pub properties: usize,
    pub links_added: usize,
    pub links_removed: usize,
}

impl Changes {
    pub fn any(&self) -> bool {
        *self != Self::default()
    }
}

/// One bin of the game the manifest edits, after every edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditedBin {
    pub chunk: WadHash,
    /// The target as the manifest spells it, or the name of the chunk for a bin an `entries`
    /// module reached.
    pub target: String,
    /// The bin with every edit that applied. The game's own bytes when none did.
    pub bytes: Vec<u8>,
    pub changes: Changes,
}

/// An edit, or a whole module, that did not apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// The position of the module in the manifest, from zero.
    pub module: usize,
    pub module_name: Option<String>,
    /// The target or the entry the problem is about.
    pub target: String,
    /// The position of the edit in its module, from zero.
    pub edit: Option<usize>,
    /// The code of the problem, as `ltk_game_data` and the mod tools name it.
    pub kind: String,
    /// The link, override file, object or signed property key the problem is about.
    pub path: Option<String>,
    /// Why a property, record or object edit was skipped.
    pub reason: Option<String>,
    pub message: String,
}

/// What applying a manifest to the game gives.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The bins the manifest edits, in the order it first names them.
    pub bins: Vec<EditedBin>,
    pub problems: Vec<Problem>,
    /// Whether a property edit was skipped for want of a type.
    pub untypable: bool,
}

/// The edits one module makes to one chunk.
struct Application {
    module: usize,
    chunk: WadHash,
    target: String,
    edits: Vec<Edit>,
}

/// Applies every module of `layer` to the game's copy of the bins it names, in manifest order.
///
/// The base of a bin is the game's copy, and each module reads what the modules before it left.
/// An `entries` module edits its entries in every chunk of the game that declares them. No class
/// schema is consulted: a property is typed by the value the bin already has for it, so an edit
/// of a property the bin does not have is a problem, as is an object made from a class.
pub fn apply(layer: &Layer, game: &Game) -> Result<Outcome> {
    let mut outcome = Outcome::default();
    let mut bins: IndexMap<WadHash, EditedBin> = IndexMap::new();

    for (index, module) in layer.declarations.modules.iter().enumerate() {
        for application in lower(index, module, game, &mut outcome.problems) {
            let problem = |kind: &str, message: String| Problem {
                module: application.module,
                module_name: module.name.as_ref().map(|name| name.as_str().to_owned()),
                target: application.target.clone(),
                edit: None,
                kind: kind.to_owned(),
                path: None,
                reason: None,
                message,
            };

            let bin = match bins.entry(application.chunk) {
                indexmap::map::Entry::Occupied(bin) => bin.into_mut(),
                indexmap::map::Entry::Vacant(slot) => match game.chunk(application.chunk)? {
                    Some(bytes) => slot.insert(EditedBin {
                        chunk: application.chunk,
                        target: application.target.clone(),
                        bytes,
                        changes: Changes::default(),
                    }),
                    None => {
                        outcome.problems.push(problem(
                            "targetSkipped",
                            format!("The game has no {}", application.target),
                        ));
                        continue;
                    }
                },
            };

            let applied = ltk_game_data::apply(
                &bin.bytes,
                &application.edits,
                |path| {
                    layer
                        .read(path.as_str())
                        .map_err(|error| ltk_game_data::Error::io(path.as_str(), &error))
                },
                |entry| {
                    game.object(entry.object_hash()).map_err(|error| {
                        let causes: Vec<String> =
                            error.chain().map(|cause| cause.to_string()).collect();
                        ltk_game_data::Error::io(entry.as_str(), &causes.join(": "))
                    })
                },
                &NoSchema,
            );
            let applied = match applied {
                Ok(applied) => applied,
                Err(error) => {
                    outcome
                        .problems
                        .push(problem("targetSkipped", error.to_string()));
                    continue;
                }
            };

            for diagnostic in &applied.diagnostics {
                // Without a schema every property is typed from the bin, so this says nothing.
                if diagnostic.kind == ApplyDiagnosticKind::SchemaFallback {
                    continue;
                }
                outcome.untypable |= diagnostic
                    .property
                    .as_ref()
                    .is_some_and(|property| property.reason == PropertySkipReason::Untypable);
                let reason = reason(diagnostic);
                outcome.problems.push(Problem {
                    edit: Some(diagnostic.edit_index),
                    kind: code(&diagnostic.kind),
                    path: Some(diagnostic.path.clone()),
                    message: match &reason {
                        Some(reason) => format!("{diagnostic}: {reason}"),
                        None => diagnostic.to_string(),
                    },
                    reason,
                    ..problem("", String::new())
                });
            }

            if applied.changed() {
                bin.bytes = applied.bytes;
                bin.changes.records += applied.applied.records;
                bin.changes.objects += applied.applied.objects;
                bin.changes.properties += applied.applied.properties;
                bin.changes.links_added += applied.applied.links_added;
                bin.changes.links_removed += applied.applied.links_removed;
            }
        }
    }

    outcome.bins = bins.into_values().collect();
    Ok(outcome)
}

/// Lowers the `module` representation into a deterministic program.
fn lower(
    index: usize,
    module: &Module,
    game: &Game,
    problems: &mut Vec<Problem>,
) -> Vec<Application> {
    match &module.selector {
        Selector::Target { target, edits } => vec![Application {
            module: index,
            chunk: WadHash(target.chunk_hash()),
            target: target.as_str().to_owned(),
            edits: edits.clone(),
        }],
        Selector::Entries(entries) => {
            let mut applications = Vec::new();
            for (name, entry) in entries {
                let chunks = game.declaring_chunks(name.object_hash());
                if chunks.is_empty() {
                    problems.push(Problem {
                        module: index,
                        module_name: module.name.as_ref().map(|name| name.as_str().to_owned()),
                        target: name.as_str().to_owned(),
                        edit: None,
                        kind: "entryUnresolved".to_owned(),
                        path: None,
                        reason: None,
                        message: format!("No bin of the game declares {name}"),
                    });
                }
                if let [_, _, ..] = chunks.as_slice() {
                    let names: Vec<String> =
                        chunks.iter().map(|chunk| game.chunk_name(*chunk)).collect();
                    tracing::info!(
                        "{name} is declared by {} bins and is edited in each: {}",
                        names.len(),
                        names.join(", ")
                    );
                }
                for chunk in chunks {
                    let mut edit = Edit::default();
                    edit.entries.insert(name.clone(), entry.properties.clone());
                    edit.links = entry.links.clone();
                    applications.push(Application {
                        module: index,
                        chunk,
                        target: game.chunk_name(chunk),
                        edits: vec![edit],
                    });
                }
            }
            applications
        }
        _ => {
            problems.push(Problem {
                module: index,
                module_name: module.name.as_ref().map(|name| name.as_str().to_owned()),
                target: String::new(),
                edit: None,
                kind: "unknown".to_owned(),
                path: None,
                reason: None,
                message: "This version does not know the selector of the module".to_owned(),
            });
            Vec::new()
        }
    }
}

/// Returns the code a value of `ltk_game_data` serializes as.
fn code(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(code)) => code,
        _ => "unknown".to_owned(),
    }
}

/// Returns why the property, record or object edit of `diagnostic` was skipped.
fn reason(diagnostic: &ApplyDiagnostic) -> Option<String> {
    if let Some(property) = &diagnostic.property {
        return Some(code(&property.reason));
    }
    if let Some(record) = &diagnostic.record {
        return Some(code(&record.reason));
    }
    diagnostic
        .object
        .as_ref()
        .map(|object| code(&object.reason))
}

#[cfg(test)]
mod tests {
    use ltk_game_index::chunk_hash;
    use ltk_hash::BinHash;
    use ltk_meta::{Bin, BinFile, BinObject, PropertyValueEnum, property::values};

    use super::*;
    use crate::{
        document::{Document, ReadOptions, to_bin},
        game::testing::Installation,
    };

    /// Hashes `name` the way a manifest's names reach a bin: FNV-1a of the lowercased name.
    fn hash(name: &str) -> BinHash {
        BinHash(
            name.to_ascii_lowercase()
                .bytes()
                .fold(0x811c_9dc5, |hash, byte| {
                    (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
                }),
        )
    }

    /// Builds a bin with one object named `entry` that has a `size` and a `tags` list.
    fn skin(entry: &str, size: f32) -> Vec<u8> {
        let bin: BinFile = Bin::builder()
            .dependency("shared.bin")
            .object(
                BinObject::builder(hash(entry), hash("SkinData"))
                    .property(hash("size"), values::F32::new(size))
                    .property(
                        hash("tags"),
                        values::Container::from(vec![values::String::new("a".to_owned())]),
                    )
                    .build(),
            )
            .build()
            .into();
        to_bin(&bin).unwrap()
    }

    fn layer(files: &[(&str, &str)]) -> (tempfile::TempDir, Result<Layer>) {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let loaded = Layer::load(Utf8Path::from_path(dir.path()).unwrap());
        (dir, loaded)
    }

    fn property(bytes: &[u8], entry: &str, name: &str) -> PropertyValueEnum {
        let document = Document::parse("edited", bytes.to_vec(), ReadOptions::default()).unwrap();
        let BinFile::Prop(bin) = document.file else {
            panic!("not a PROP bin");
        };
        bin.objects[&hash(entry)].properties[&hash(name)].clone()
    }

    #[test]
    fn a_target_module_edits_the_games_copy_of_the_bin() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/skin0.bin", &skin("a/skin0", 1.0))]);
        let (_guard, layer) = layer(&[(
            "game_data.yaml",
            "version: 1\nmodules:\n  - target: data/skin0.bin\n    +links: [mods/extra.bin]\n    a/skin0:\n      size: 2.5\n      +tags: [b]\n",
        )]);

        let outcome = apply(&layer.unwrap(), &installation.open()).unwrap();
        assert_eq!(outcome.problems, []);
        let [bin] = outcome.bins.as_slice() else {
            panic!("{} bins", outcome.bins.len());
        };
        assert_eq!(bin.target, "data/skin0.bin");
        assert_eq!(
            bin.changes,
            Changes {
                properties: 2,
                links_added: 1,
                ..Changes::default()
            }
        );
        assert_eq!(
            property(&bin.bytes, "a/skin0", "size"),
            values::F32::new(2.5).into()
        );
        assert_eq!(
            property(&bin.bytes, "a/skin0", "tags"),
            values::Container::from(vec![
                values::String::new("a".to_owned()),
                values::String::new("b".to_owned()),
            ])
            .into()
        );
    }

    #[test]
    fn an_entries_module_edits_every_bin_that_declares_the_entry() {
        let installation = Installation::new();
        installation.archive(
            "A.wad.client",
            &[
                ("data/one.bin", &skin("a/shared", 1.0)),
                ("data/two.bin", &skin("a/shared", 1.0)),
                ("data/other.bin", &skin("a/other", 1.0)),
            ],
        );
        let (_guard, layer) = layer(&[(
            "game_data.yaml",
            "version: 1\nmodules:\n  - entries:\n      a/shared:\n        size: 3\n      a/nowhere:\n        size: 4\n",
        )]);

        let outcome = apply(&layer.unwrap(), &installation.open()).unwrap();
        let mut edited: Vec<WadHash> = outcome.bins.iter().map(|bin| bin.chunk).collect();
        edited.sort();
        let mut declaring = [chunk_hash("data/one.bin"), chunk_hash("data/two.bin")];
        declaring.sort();
        assert_eq!(edited, declaring);
        for bin in &outcome.bins {
            assert_eq!(
                property(&bin.bytes, "a/shared", "size"),
                values::F32::new(3.0).into()
            );
        }

        let [problem] = outcome.problems.as_slice() else {
            panic!("{:?}", outcome.problems);
        };
        assert_eq!(problem.kind, "entryUnresolved");
        assert_eq!(problem.target, "a/nowhere");
    }

    #[test]
    fn a_reference_reads_the_games_copy_of_another_entry() {
        let installation = Installation::new();
        installation.archive(
            "A.wad.client",
            &[
                ("data/skin0.bin", &skin("a/skin0", 1.0)),
                ("data/skin1.bin", &skin("a/skin1", 7.0)),
            ],
        );
        let (_guard, layer) = layer(&[(
            "game_data.yaml",
            "version: 1\nmodules:\n  - target: data/skin0.bin\n    a/skin0:\n      size: !ref a/skin1:size\n",
        )]);

        let outcome = apply(&layer.unwrap(), &installation.open()).unwrap();
        assert_eq!(outcome.problems, []);
        assert_eq!(
            property(&outcome.bins[0].bytes, "a/skin0", "size"),
            values::F32::new(7.0).into()
        );
    }

    #[test]
    fn an_object_is_cloned_but_not_made_from_a_class() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/skin0.bin", &skin("a/skin0", 1.0))]);
        let (_guard, layer) = layer(&[(
            "game_data.yaml",
            "version: 1\nmodules:\n  - target: data/skin0.bin\n    objects:\n      mods/x/copy:\n        clone: a/skin0\n        set:\n          size: 9\n      mods/x/made:\n        class: SkinData\n",
        )]);

        let outcome = apply(&layer.unwrap(), &installation.open()).unwrap();
        let bin = &outcome.bins[0];
        assert_eq!(bin.changes.objects, 1);
        assert_eq!(
            property(&bin.bytes, "mods/x/copy", "size"),
            values::F32::new(9.0).into()
        );
        assert_eq!(
            property(&bin.bytes, "a/skin0", "size"),
            values::F32::new(1.0).into()
        );

        // A class is known from a schema, and none is read.
        let [problem] = outcome.problems.as_slice() else {
            panic!("{:?}", outcome.problems);
        };
        assert_eq!(
            (problem.kind.as_str(), problem.reason.as_deref()),
            ("objectSkipped", Some("unknownClass"))
        );
    }

    #[test]
    fn a_later_module_reads_what_an_earlier_one_left() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/skin0.bin", &skin("a/skin0", 1.0))]);
        let (_guard, layer) = layer(&[(
            "game_data.yaml",
            "version: 1\nmodules:\n  - target: data/skin0.bin\n    a/skin0:\n      +tags: [b]\n  - target: data/skin0.bin\n    a/skin0:\n      -tags: [a]\n",
        )]);

        let outcome = apply(&layer.unwrap(), &installation.open()).unwrap();
        assert_eq!(outcome.problems, []);
        assert_eq!(outcome.bins.len(), 1);
        assert_eq!(
            property(&outcome.bins[0].bytes, "a/skin0", "tags"),
            values::Container::from(vec![values::String::new("b".to_owned())]).into()
        );
    }

    #[test]
    fn what_does_not_apply_is_a_problem_and_the_rest_applies() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/skin0.bin", &skin("a/skin0", 1.0))]);
        let (_guard, layer) = layer(&[(
            "game_data.yaml",
            "version: 1\nmodules:\n  - name: tweaks\n    target: data/skin0.bin\n    a/skin0:\n      size: 2\n      unknownField: 1\n      pinned: !u32 5\n  - target: data/missing.bin\n    +links: [x.bin]\n",
        )]);

        let outcome = apply(&layer.unwrap(), &installation.open()).unwrap();
        assert!(outcome.untypable);
        let kinds: Vec<(&str, Option<&str>)> = outcome
            .problems
            .iter()
            .map(|problem| (problem.kind.as_str(), problem.reason.as_deref()))
            .collect();
        // A property the bin does not have has no type without a schema, pinned or not.
        assert_eq!(
            kinds,
            [
                ("propertyEditSkipped", Some("untypable")),
                ("propertyEditSkipped", Some("untypable")),
                ("targetSkipped", None)
            ]
        );
        assert_eq!(outcome.problems[0].module_name.as_deref(), Some("tweaks"));
        assert_eq!(outcome.problems[0].path.as_deref(), Some("unknownField"));
        assert_eq!(outcome.problems[1].path.as_deref(), Some("pinned"));

        // The bin the game lacks is not among the edited ones.
        let [bin] = outcome.bins.as_slice() else {
            panic!("{} bins", outcome.bins.len());
        };
        assert_eq!(bin.changes.properties, 1);
        assert_eq!(
            property(&bin.bytes, "a/skin0", "size"),
            values::F32::new(2.0).into()
        );
    }

    #[test]
    fn a_manifest_that_does_not_load_is_shown_where_it_is_wrong() {
        let (_guard, layer) = layer(&[(
            "game_data.yaml",
            "version: 1\nmodules:\n  - target: data/skin0.bin\n    bogus: 1\n",
        )]);
        let error = layer.err().unwrap();
        assert!(error.to_string().contains("game_data.yaml"), "{error}");
    }

    #[test]
    fn a_source_file_outside_the_layer_is_refused() {
        let (_guard, layer) = layer(&[
            ("layer/game_data.yaml", "version: 1\nmodules: []\n"),
            ("outside.yaml", "version: 1\n+links: [x.bin]\n"),
        ]);
        drop(layer);
        let dir = Utf8Path::from_path(_guard.path()).unwrap();
        assert_eq!(
            read_inside(&dir.join("layer"), "../outside.yaml"),
            Err("the path leads out of the manifest's directory".to_owned())
        );
        assert!(read_inside(&dir.join("layer"), "game_data.yaml").is_ok());
    }

    #[test]
    fn a_directory_needs_exactly_one_manifest() {
        let (_guard, none) = layer(&[("notes.txt", "")]);
        assert!(none.err().unwrap().to_string().contains("has no manifest"));

        let (_guard, several) = layer(&[
            ("game_data.yaml", "version: 1\nmodules: []\n"),
            ("game_data.json", "{\"version\": 1, \"modules\": []}"),
        ]);
        assert!(
            several
                .err()
                .unwrap()
                .to_string()
                .contains("several manifests")
        );
    }
}
