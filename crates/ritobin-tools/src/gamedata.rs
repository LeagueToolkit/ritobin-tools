//! Applies game-data declarations to the installed game.
//!
//! `ltk_game_data` defines the manifest format and implements the edits. This module resolves
//! each module of a manifest to the game chunks it targets, applies the edits to those chunks in
//! manifest order, and collects a diagnostic for every edit that was skipped.

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

/// A loaded manifest and the directory its source and override files are resolved against.
pub struct Layer {
    dir: Utf8PathBuf,
    pub declarations: Declarations,
}

/// A manifest or source file load error, with the source span reported by the loader.
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
    /// Loads the manifest at `path`. If `path` is a directory, loads the manifest file in it.
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

        // File contents keyed by the document name the loader uses. A load error contains the
        // name of its document, and the matching text is attached to the error as source code.
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
                    // A syntax error message has a source excerpt after its first line. Only the
                    // first line is used, because the label already shows the source.
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

    /// Reads the file at the layer-relative `path`.
    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        read_inside(&self.dir, path)
    }
}

/// Returns the name of the manifest file in `dir`. Fails if `dir` contains no manifest file or
/// more than one.
fn manifest_in(dir: &Utf8Path) -> Result<String> {
    let found: Vec<&str> = MANIFEST_NAMES
        .into_iter()
        .filter(|name| dir.join(name).is_file())
        .collect();
    match found.as_slice() {
        [name] => Ok((*name).to_owned()),
        [] => miette::bail!(
            "{dir} contains no manifest file. Expected one of: {}",
            MANIFEST_NAMES.join(", ")
        ),
        several => miette::bail!(
            "{dir} contains more than one manifest file: {}",
            several.join(", ")
        ),
    }
}

/// Reads `dir/path`. Fails if `path` resolves to a location outside `dir`.
fn read_inside(dir: &Utf8Path, path: &str) -> Result<Vec<u8>, String> {
    let file = dir.join(path);
    let inside = |file: &Utf8Path| -> std::io::Result<bool> {
        Ok(file.canonicalize()?.starts_with(dir.canonicalize()?))
    };
    match inside(&file) {
        Ok(true) => std::fs::read(&file).map_err(|error| error.to_string()),
        Ok(false) => Err("the path resolves outside the manifest directory".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// Counts of the changes applied to one bin, summed over all edits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    /// Override file records applied.
    pub records: usize,
    /// Objects created or removed, plus objects changed by an override file.
    pub objects: usize,
    /// Property edits applied.
    pub properties: usize,
    pub links_added: usize,
    pub links_removed: usize,
}

impl Changes {
    /// Returns `true` if any count is nonzero.
    pub fn any(&self) -> bool {
        *self != Self::default()
    }
}

/// A game bin targeted by the manifest, with the result of applying all edits to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditedBin {
    pub chunk: WadHash,
    /// The target string from the manifest. For a bin resolved from an `entries` module, the
    /// chunk name.
    pub target: String,
    /// The bin after all applied edits. Equal to the game's bytes if no edit applied.
    pub bytes: Vec<u8>,
    pub changes: Changes,
}

/// A diagnostic for an edit or a module that was not applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// Zero-based index of the module in the manifest.
    pub module: usize,
    pub module_name: Option<String>,
    /// The target or entry name the diagnostic refers to.
    pub target: String,
    /// Zero-based index of the edit in its module.
    pub edit: Option<usize>,
    /// The diagnostic kind code. `ltk_game_data` and `ltk_overlay` use the same codes.
    pub kind: String,
    /// The link path, override path, object name or signed property key the diagnostic refers to.
    pub path: Option<String>,
    /// The skip reason code of a property, record or object edit.
    pub reason: Option<String>,
    pub message: String,
}

/// The result of applying a manifest to the game.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The targeted bins, in order of first reference in the manifest.
    pub bins: Vec<EditedBin>,
    pub problems: Vec<Problem>,
    /// `true` if any property edit was skipped with the `untypable` reason.
    pub untypable: bool,
}

/// The edits of one module for one chunk.
struct Application {
    module: usize,
    chunk: WadHash,
    target: String,
    edits: Vec<Edit>,
}

/// Applies every module of `layer` to the game, in manifest order.
///
/// The initial content of each bin is the game's copy. When several modules target the same bin,
/// each module is applied to the output of the previous one. An `entries` module is applied to
/// every game chunk that declares the entry.
///
/// Edits are applied with `NoSchema`, so the type of a property comes from its existing value in
/// the bin. An edit that adds a property missing from the bin is skipped as `untypable`, and an
/// object constructed from a class is skipped as `unknownClass`.
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
                            format!("{} was not found in the game", application.target),
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
                // With `NoSchema` the type of every property is taken from the bin, so this
                // diagnostic is emitted for every applied property edit. It is not reported.
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
                        message: format!("Entry {name} is not declared by any game bin"),
                    });
                }
                if let [_, _, ..] = chunks.as_slice() {
                    let names: Vec<String> =
                        chunks.iter().map(|chunk| game.chunk_name(*chunk)).collect();
                    tracing::info!(
                        "{name} is declared by {} bins. The edit is applied to each: {}",
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
                message: "Unsupported module selector".to_owned(),
            });
            Vec::new()
        }
    }
}

/// Returns the string `value` serializes to. `ltk_game_data` serializes its diagnostic kind and
/// skip reason enums as camelCase codes.
fn code(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(code)) => code,
        _ => "unknown".to_owned(),
    }
}

/// Returns the skip reason code of `diagnostic`, if it has a property, record or object skip.
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

    /// Returns the bin hash of `name`: FNV-1a of the lowercased name.
    fn hash(name: &str) -> BinHash {
        BinHash(
            name.to_ascii_lowercase()
                .bytes()
                .fold(0x811c_9dc5, |hash, byte| {
                    (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
                }),
        )
    }

    /// Builds a bin with one object named `entry`, which has an `f32` property `size` and a
    /// string list property `tags`.
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
    fn target_module_applies_edits_to_game_bin() {
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
    fn entries_module_applies_to_every_declaring_bin() {
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
    fn reference_resolves_to_game_value() {
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
    fn clone_object_applies_and_class_object_is_skipped() {
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

        // Constructing an object requires `Schema::has_class` to return `true`. With `NoSchema`
        // it returns `false`.
        let [problem] = outcome.problems.as_slice() else {
            panic!("{:?}", outcome.problems);
        };
        assert_eq!(
            (problem.kind.as_str(), problem.reason.as_deref()),
            ("objectSkipped", Some("unknownClass"))
        );
    }

    #[test]
    fn modules_on_same_bin_apply_in_order() {
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
    fn skipped_edits_are_reported_and_other_edits_apply() {
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
        // Both properties are missing from the bin. With `NoSchema` a missing property is
        // `untypable`, including the one with a type pin.
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

        // `data/missing.bin` is not in the game, so it is not in `outcome.bins`.
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
    fn invalid_manifest_fails_to_load() {
        let (_guard, layer) = layer(&[(
            "game_data.yaml",
            "version: 1\nmodules:\n  - target: data/skin0.bin\n    bogus: 1\n",
        )]);
        let error = layer.err().unwrap();
        assert!(error.to_string().contains("game_data.yaml"), "{error}");
    }

    #[test]
    fn read_inside_rejects_path_outside_dir() {
        let (_guard, layer) = layer(&[
            ("layer/game_data.yaml", "version: 1\nmodules: []\n"),
            ("outside.yaml", "version: 1\n+links: [x.bin]\n"),
        ]);
        drop(layer);
        let dir = Utf8Path::from_path(_guard.path()).unwrap();
        assert_eq!(
            read_inside(&dir.join("layer"), "../outside.yaml"),
            Err("the path resolves outside the manifest directory".to_owned())
        );
        assert!(read_inside(&dir.join("layer"), "game_data.yaml").is_ok());
    }

    #[test]
    fn directory_must_contain_exactly_one_manifest() {
        let (_guard, none) = layer(&[("notes.txt", "")]);
        assert!(
            none.err()
                .unwrap()
                .to_string()
                .contains("contains no manifest file")
        );

        let (_guard, several) = layer(&[
            ("game_data.yaml", "version: 1\nmodules: []\n"),
            ("game_data.json", "{\"version\": 1, \"modules\": []}"),
        ]);
        assert!(
            several
                .err()
                .unwrap()
                .to_string()
                .contains("more than one manifest file")
        );
    }
}
