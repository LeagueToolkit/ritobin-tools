use std::{fmt::Write as _, io::IsTerminal as _};

use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, ValueEnum};
use colored::Colorize;
use ltk_meta::BinFile;
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;
use similar::{ChangeTag, TextDiff};

use crate::{
    cli::LayoutArgs,
    context::Context,
    diff::{BinDiff, Change, ChangeKind, Lifted, Summary},
    document::{
        Document, Format, ReadOptions, STDIO, TextLayout, encode, reads_back, to_text, write_bytes,
    },
    hashes::BinHashes,
    utils::{hyperlink_path, plural},
};

/// The maximum number of characters of a value printed by the `summary` format. A longer value
/// is truncated.
const SUMMARY_VALUE_WIDTH: usize = 100;

#[derive(Args, Debug)]
pub struct DiffArgs {
    /// The base bin (.bin or ritobin text)
    pub base: Utf8PathBuf,

    /// The edited bin (.bin or ritobin text)
    pub edited: Utf8PathBuf,

    /// Output format. Defaults to the format of the output file extension, or to `unified`
    #[arg(short, long, value_enum, value_name = "FORMAT")]
    pub format: Option<DiffFormat>,

    /// Write the difference to a file instead of standard output
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<Utf8PathBuf>,

    /// Also save the difference as a PTCH file that patches BASE. The file is text if the path
    /// has a ritobin text extension, otherwise binary
    #[arg(short, long, value_name = "FILE")]
    pub patch: Option<Utf8PathBuf>,

    /// Add objects that exist in BASE but not in EDITED to the delete list of the patch
    #[arg(long)]
    pub deletions: bool,

    /// Number of context lines around each change in the `unified` format
    #[arg(short = 'C', long, value_name = "LINES", default_value_t = 3)]
    pub context: usize,

    /// Disable colored output
    #[arg(long)]
    pub no_color: bool,

    /// Exit with 1 if the bins differ, 0 if they are identical, and 2 if the command fails
    #[arg(long)]
    pub exit_code: bool,

    /// Write hashes as hex. Do not resolve them with the hashtables
    #[arg(short, long)]
    pub keep_hashed: bool,

    /// Read text that has problems. The invalid parts are skipped
    #[arg(long)]
    pub lenient: bool,

    #[command(flatten)]
    pub layout: LayoutArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DiffFormat {
    /// Line diff of the two bins printed as ritobin text
    Unified,
    /// Changed values, grouped by object
    Summary,
    /// One JSON document with the summary, all changes and the patch statistics
    Json,
    /// One JSON object per change, one object per line
    Jsonl,
    /// One row per change
    Csv,
    /// The PTCH patch as ritobin text
    Rito,
}

impl DiffFormat {
    /// Returns the diff format for an output file extension. Returns `None` for an unknown
    /// extension.
    fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "json" => Some(Self::Json),
            "jsonl" | "ndjson" => Some(Self::Jsonl),
            "csv" => Some(Self::Csv),
            "diff" | "patch" => Some(Self::Unified),
            extension if Format::from_extension(extension) == Some(Format::Rito) => {
                Some(Self::Rito)
            }
            _ => None,
        }
    }
}

/// Runs the `diff` command. Returns `true` if the two bins differ.
pub fn run(ctx: &Context, args: DiffArgs) -> Result<bool> {
    let options = ReadOptions {
        lenient: args.lenient,
    };
    let base = Document::read(&args.base, options)?;
    let edited = Document::read(&args.edited, options)?;

    let hashes = match args.keep_hashed {
        true => BinHashes::none(),
        false => ctx.hashes(),
    };
    let layout = args.layout.over(ctx.config.print_config);
    let format = args
        .format
        .or_else(|| {
            args.output
                .as_ref()
                .and_then(|output| output.extension())
                .and_then(DiffFormat::from_extension)
        })
        .unwrap_or(DiffFormat::Unified);

    if let Some(patch) = &args.patch
        && patch.as_str() == STDIO
    {
        miette::bail!("--patch requires a file path. Standard output is used for the difference");
    }

    let to_stdout = args.output.is_none();
    colored::control::set_override(
        !args.no_color
            && to_stdout
            && std::io::stdout().is_terminal()
            && std::env::var_os("NO_COLOR").is_none(),
    );

    let diff = match (&base.file, &edited.file) {
        (BinFile::Prop(base), BinFile::Prop(edited)) => {
            Some(BinDiff::between(base, edited, &hashes, args.deletions))
        }
        _ => None,
    };
    let structural = || {
        diff.as_ref().ok_or_else(|| {
            let patch = match base.file {
                BinFile::Override(_) => &args.base,
                BinFile::Prop(_) => &args.edited,
            };
            miette::miette!(
                "{patch} is a PTCH file. This format and --patch require two PROP bins. Use --format unified without --patch"
            )
        })
    };

    let (rendered, differs) = match format {
        DiffFormat::Unified => {
            let base_text = to_text(&base.file, layout, &hashes).into_diagnostic()?;
            let edited_text = to_text(&edited.file, layout, &hashes).into_diagnostic()?;
            let rendered = unified(
                &base_text,
                &edited_text,
                &args.base,
                &args.edited,
                args.context,
            );
            // The printer does not print every value exactly, so two different bins can have
            // identical text. Then the structural diff has changes and the text diff is empty.
            let hidden =
                base_text == edited_text && diff.as_ref().is_some_and(|diff| !diff.is_empty());
            if hidden {
                tracing::warn!(
                    "The bins differ, but their ritobin text is identical. Use `--format summary` to list the differences."
                );
            }
            (rendered, base_text != edited_text || hidden)
        }
        DiffFormat::Summary => {
            let diff = structural()?;
            (summary(diff), !diff.is_empty())
        }
        DiffFormat::Json => {
            let diff = structural()?;
            (
                json(diff, &hashes, &args.base, &args.edited)?,
                !diff.is_empty(),
            )
        }
        DiffFormat::Jsonl => {
            let diff = structural()?;
            (jsonl(diff)?, !diff.is_empty())
        }
        DiffFormat::Csv => {
            let diff = structural()?;
            (csv(diff)?, !diff.is_empty())
        }
        DiffFormat::Rito => {
            let diff = structural()?;
            let patch = BinFile::Override(diff.patch.clone());
            (
                to_text(&patch, layout, &hashes).into_diagnostic()?,
                !diff.is_empty(),
            )
        }
    };

    write_bytes(
        args.output.as_deref().unwrap_or(Utf8Path::new(STDIO)),
        rendered.as_bytes(),
    )?;
    if let Some(output) = &args.output {
        tracing::info!("Wrote the difference to {}", hyperlink_path(output));
    }
    if !differs {
        tracing::info!("The bins are identical");
    }

    if let Some(path) = &args.patch {
        save_patch(structural()?, path, layout, &hashes)?;
    }

    Ok(differs)
}

/// Writes the patch of `diff` to `path`. The file is text if `path` has a ritobin text extension,
/// otherwise binary. Logs a warning for each limitation that affects the patch.
fn save_patch(
    diff: &BinDiff,
    path: &Utf8Path,
    layout: TextLayout,
    hashes: &BinHashes,
) -> Result<()> {
    let format = path
        .extension()
        .and_then(Format::from_extension)
        .unwrap_or(Format::Bin);
    let data = encode(
        &BinFile::Override(diff.patch.clone()),
        format,
        layout,
        hashes,
    )
    .wrap_err("Failed to encode the patch")?;
    if format == Format::Rito
        && !std::str::from_utf8(&data)
            .is_ok_and(|text| reads_back(&BinFile::Override(diff.patch.clone()), text))
    {
        tracing::warn!(
            "The patch text does not parse back to the same patch, because the printer does not print every value exactly. Save the patch as a .bin file instead."
        );
    }
    write_bytes(path, &data)?;
    tracing::info!(
        "Saved the patch to {}: {}",
        hyperlink_path(path),
        diff.report
    );

    if !diff.report.lifted.is_empty() {
        tracing::warn!(
            "{} could not be addressed by a patch record. Each was recorded as a larger value. Applied to a different base, such a record overwrites more than the change. Use `--format json` to list them.",
            plural(diff.report.lifted.len(), "difference")
        );
        if hashes.is_empty() {
            tracing::warn!(
                "No hashtables are loaded. A patch record addresses a property by field name, so every changed object was recorded as a whole object."
            );
        }
    }
    if !diff.report.dependencies.is_empty() {
        tracing::warn!(
            "EDITED depends on {} that BASE does not depend on. A patch cannot store dependencies",
            plural(diff.report.dependencies.len(), "bin")
        );
    }
    if !diff.patch_is_exact {
        tracing::warn!(
            "Applying the patch to BASE does not produce EDITED exactly. A patch cannot remove a property or a map entry, and it removes an object only with --deletions."
        );
    }
    Ok(())
}

/// Returns the unified line diff of two ritobin texts. Returns an empty string if the texts are
/// equal.
fn unified(
    base: &str,
    edited: &str,
    base_path: &Utf8Path,
    edited_path: &Utf8Path,
    context: usize,
) -> String {
    let mut out = String::new();
    if base == edited {
        return out;
    }

    let diff = TextDiff::from_lines(base, edited);
    let _ = writeln!(out, "{}", format!("--- {base_path}").red());
    let _ = writeln!(out, "{}", format!("+++ {edited_path}").green());

    for hunk in diff.unified_diff().context_radius(context).iter_hunks() {
        let _ = writeln!(out, "{}", hunk.header().to_string().cyan());
        for change in hunk.iter_changes() {
            let line = change.value().trim_end_matches(['\r', '\n']);
            let line = match change.tag() {
                ChangeTag::Delete => format!("-{line}").red(),
                ChangeTag::Insert => format!("+{line}").green(),
                ChangeTag::Equal => format!(" {line}").normal(),
            };
            let _ = writeln!(out, "{line}");
            if change.missing_newline() {
                let _ = writeln!(out, "{}", "\\ No newline at end of file".yellow());
            }
        }
    }
    out
}

/// Renders the `summary` format: one line per change, grouped by object, followed by the change
/// counts.
fn summary(diff: &BinDiff) -> String {
    let mut out = String::new();
    if diff.is_empty() {
        return out;
    }

    let mut current: Option<&str> = None;
    for change in &diff.changes {
        let object = object_label(change);
        match change.kind {
            ChangeKind::DependencyAdded => {
                let dependency = change.new.as_deref().unwrap_or_default();
                let _ = writeln!(out, "{}", format!("+ dependency \"{dependency}\"").green());
            }
            ChangeKind::DependencyRemoved => {
                let dependency = change.old.as_deref().unwrap_or_default();
                let _ = writeln!(out, "{}", format!("- dependency \"{dependency}\"").red());
            }
            ChangeKind::ObjectAdded => {
                let _ = writeln!(out, "{}", format!("+ {object}").green());
            }
            ChangeKind::ObjectRemoved => {
                let _ = writeln!(out, "{}", format!("- {object}").red());
            }
            ChangeKind::ObjectReplaced => {
                let name = object_name(change);
                let old = change.old.as_deref().unwrap_or_default();
                let new = change.new.as_deref().unwrap_or_default();
                let _ = writeln!(out, "{}", format!("~ {name} ({old} -> {new})").yellow());
            }
            ChangeKind::Added | ChangeKind::Removed | ChangeKind::Changed => {
                if current != change.object.as_deref() {
                    current = change.object.as_deref();
                    let _ = writeln!(out, "{}", format!("~ {object}").yellow().bold());
                }
                let path = change.path.as_deref().unwrap_or_default();
                let value_type = change.value_type.as_deref().unwrap_or_default();
                let old = change.old.as_deref().map(one_line);
                let new = change.new.as_deref().map(one_line);
                let line = match change.kind {
                    ChangeKind::Added => {
                        format!("  + {path}: {value_type} = {}", new.unwrap_or_default()).green()
                    }
                    ChangeKind::Removed => {
                        format!("  - {path}: {value_type} = {}", old.unwrap_or_default()).red()
                    }
                    _ => format!(
                        "  ~ {path}: {value_type} = {} -> {}",
                        old.unwrap_or_default(),
                        new.unwrap_or_default()
                    )
                    .yellow(),
                };
                let _ = writeln!(out, "{line}");
            }
        }
    }

    let _ = writeln!(out, "\n{}", counts(&diff.summary()));
    out
}

/// Returns the quoted object path if it is known, otherwise the object hash.
fn object_name(change: &Change) -> String {
    match (&change.object_name, &change.object) {
        (Some(name), _) => format!("\"{name}\""),
        (None, Some(hash)) => hash.clone(),
        (None, None) => String::new(),
    }
}

/// Returns the object name followed by the class in parentheses.
fn object_label(change: &Change) -> String {
    let class = change.class.as_deref().unwrap_or_default();
    format!("{} ({class})", object_name(change))
}

/// Returns the first line of `value`, truncated to [`SUMMARY_VALUE_WIDTH`] characters. Appends
/// ` ...` if the value was shortened.
fn one_line(value: &str) -> String {
    let mut lines = value.lines();
    let first = lines.next().unwrap_or_default();
    let cut: String = first.chars().take(SUMMARY_VALUE_WIDTH).collect();
    match lines.next().is_some() || cut.len() < first.len() {
        true => format!("{cut} ..."),
        false => cut,
    }
}

/// Formats the change counts of `summary` as one line.
fn counts(summary: &Summary) -> String {
    let mut parts = vec![format!(
        "{} changed, {} added, {} removed, {} replaced",
        plural(summary.objects_changed, "object"),
        summary.objects_added,
        summary.objects_removed,
        summary.objects_replaced,
    )];
    parts.push(format!(
        "{} changed, {} added, {} removed",
        plural(summary.values_changed, "value"),
        summary.values_added,
        summary.values_removed,
    ));
    if summary.dependencies_added + summary.dependencies_removed > 0 {
        parts.push(format!(
            "{} added, {} removed",
            plural(summary.dependencies_added, "dependency link"),
            summary.dependencies_removed,
        ));
    }
    parts.join("; ")
}

/// The patch statistics in the JSON document.
#[derive(Serialize)]
struct PatchInfo {
    records: usize,
    objects: usize,
    deleted: usize,
    /// `true` if applying the patch to the base bin produces exactly the objects of the edited
    /// bin.
    exact: bool,
    lifted: Vec<Lifted>,
}

/// The document written by the `json` format.
#[derive(Serialize)]
struct JsonDiff<'a> {
    base: &'a str,
    edited: &'a str,
    identical: bool,
    summary: Summary,
    changes: &'a [Change],
    patch: PatchInfo,
}

/// Renders the `json` format.
fn json(diff: &BinDiff, hashes: &BinHashes, base: &Utf8Path, edited: &Utf8Path) -> Result<String> {
    let document = JsonDiff {
        base: base.as_str(),
        edited: edited.as_str(),
        identical: diff.is_empty(),
        summary: diff.summary(),
        changes: &diff.changes,
        patch: PatchInfo {
            records: diff.report.records,
            objects: diff.report.objects.len(),
            deleted: diff.report.deleted.len(),
            exact: diff.patch_is_exact,
            lifted: diff.lifted(hashes),
        },
    };
    let mut out = serde_json::to_string_pretty(&document).into_diagnostic()?;
    out.push('\n');
    Ok(out)
}

/// Renders the `jsonl` format.
fn jsonl(diff: &BinDiff) -> Result<String> {
    let mut out = String::new();
    for change in &diff.changes {
        out.push_str(&serde_json::to_string(change).into_diagnostic()?);
        out.push('\n');
    }
    Ok(out)
}

/// Renders the `csv` format, with a header row.
fn csv(diff: &BinDiff) -> Result<String> {
    let mut writer = ::csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(Vec::new());
    writer
        .write_record([
            "kind",
            "object",
            "object_name",
            "class",
            "path",
            "type",
            "old",
            "new",
        ])
        .into_diagnostic()?;
    for change in &diff.changes {
        writer.serialize(change).into_diagnostic()?;
    }
    let data = writer.into_inner().into_diagnostic()?;
    String::from_utf8(data).into_diagnostic()
}

#[cfg(test)]
mod tests {
    use ltk_hash::{BinHash, Hash as _};
    use ltk_meta::{Bin, BinObject, property::values};

    use super::*;
    use crate::document::to_bin;

    const OBJECT: u32 = 0x1111_0001;
    const CLASS: u32 = 0xaaaa_0001;

    fn bin(value: i32, extra: bool) -> Bin {
        let mut object =
            BinObject::builder(OBJECT, CLASS).property(0x10u32, values::I32::new(value));
        if extra {
            object = object.property(0x11u32, values::Bool::new(true));
        }
        Bin::builder().object(object.build()).build()
    }

    fn sample_diff() -> BinDiff {
        colored::control::set_override(false);
        BinDiff::between(&bin(1, true), &bin(2, false), &BinHashes::none(), false)
    }

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    fn args(base: &Utf8Path, edited: &Utf8Path) -> DiffArgs {
        DiffArgs {
            base: base.to_owned(),
            edited: edited.to_owned(),
            format: None,
            output: None,
            patch: None,
            deletions: false,
            context: 3,
            no_color: true,
            exit_code: false,
            keep_hashed: true,
            lenient: false,
            layout: LayoutArgs::default(),
        }
    }

    fn context() -> Context {
        Context::for_tests(None)
    }

    #[test]
    fn summary_groups_changes_by_object() {
        assert_eq!(
            summary(&sample_diff()),
            "~ 0x11110001 (0xaaaa0001)\n  ~ 00000010: i32 = 1 -> 2\n  - 00000011: bool = true\n\n1 object changed, 0 added, 0 removed, 0 replaced; 1 value changed, 0 added, 1 removed\n"
        );
    }

    #[test]
    fn csv_has_header_and_one_row_per_change() {
        assert_eq!(
            csv(&sample_diff()).unwrap(),
            "kind,object,object_name,class,path,type,old,new\nchanged,0x11110001,,0xaaaa0001,00000010,i32,1,2\nremoved,0x11110001,,0xaaaa0001,00000011,bool,true,\n"
        );
    }

    #[test]
    fn jsonl_has_one_object_per_change() {
        let out = jsonl(&sample_diff()).unwrap();
        let lines: Vec<serde_json::Value> = out
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["kind"], "changed");
        assert_eq!(lines[0]["type"], "i32");
        assert_eq!(lines[1]["new"], serde_json::Value::Null);
    }

    #[test]
    fn json_contains_summary_and_patch_statistics() {
        let out = json(
            &sample_diff(),
            &BinHashes::none(),
            Utf8Path::new("a.bin"),
            Utf8Path::new("b.bin"),
        )
        .unwrap();
        let document: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(document["identical"], false);
        assert_eq!(document["summary"]["values_removed"], 1);
        assert_eq!(document["changes"].as_array().unwrap().len(), 2);
        assert_eq!(document["patch"]["exact"], false);
    }

    #[test]
    fn unified_is_empty_for_equal_text() {
        colored::control::set_override(false);
        let (a, b) = (Utf8Path::new("a"), Utf8Path::new("b"));
        assert_eq!(unified("x\n", "x\n", a, b, 3), "");
        assert_eq!(
            unified("x\ny\n", "x\nz\n", a, b, 3),
            "--- a\n+++ b\n@@ -1,2 +1,2 @@\n x\n-y\n+z\n"
        );
    }

    #[test]
    fn one_line_truncates_long_and_multi_line_values() {
        assert_eq!(one_line("short"), "short");
        assert_eq!(one_line("Class {\n    a: u8 = 1\n}"), "Class { ...");
        assert_eq!(
            one_line(&"x".repeat(150)),
            format!("{} ...", "x".repeat(100))
        );
    }

    #[test]
    fn from_extension_maps_output_extensions() {
        assert_eq!(DiffFormat::from_extension("json"), Some(DiffFormat::Json));
        assert_eq!(DiffFormat::from_extension("CSV"), Some(DiffFormat::Csv));
        assert_eq!(DiffFormat::from_extension("rito"), Some(DiffFormat::Rito));
        assert_eq!(DiffFormat::from_extension("bin"), None);
    }

    #[test]
    fn saved_patch_applied_to_base_produces_edited() {
        let (_guard, dir) = temp_dir();
        let size = BinHash::hash_str("Size");
        let named = |value: i32| {
            Bin::builder()
                .object(
                    BinObject::builder(OBJECT, CLASS)
                        .property(size, values::I32::new(value))
                        .build(),
                )
                .build()
        };
        let (base, edited) = (named(1), named(2));
        let (base_path, edited_path) = (dir.join("base.bin"), dir.join("edited.bin"));
        std::fs::write(&base_path, to_bin(&base.clone().into()).unwrap()).unwrap();
        std::fs::write(&edited_path, to_bin(&edited.clone().into()).unwrap()).unwrap();

        // With the field name in a hashtable, the patch has one record for the property.
        // Without it, the patch has the whole object.
        let tables = dir.join("tables");
        std::fs::create_dir(&tables).unwrap();
        std::fs::write(
            tables.join("hashes.binfields.txt"),
            format!("{:08x} Size\n", size.0),
        )
        .unwrap();

        for patch_name in ["patch.bin", "patch.rito"] {
            let patch_path = dir.join(patch_name);
            let differs = run(
                &Context::for_tests(Some(tables.clone())),
                DiffArgs {
                    keep_hashed: false,
                    patch: Some(patch_path.clone()),
                    output: Some(dir.join("out.json")),
                    ..args(&base_path, &edited_path)
                },
            )
            .unwrap();
            assert!(differs);

            let patch = Document::read(&patch_path, ReadOptions::default()).unwrap();
            let BinFile::Override(patch) = patch.file else {
                panic!("{patch_name} is not a PTCH");
            };
            assert_eq!(patch.patches.len(), 1);
            assert_eq!(patch.patches[0].path.as_str(), "Size");

            let mut patched = base.clone();
            assert!(patch.apply(&mut patched).is_clean());
            assert_eq!(patched, edited);
        }
    }

    #[test]
    fn run_returns_false_for_identical_bins() {
        let (_guard, dir) = temp_dir();
        let path = dir.join("a.bin");
        std::fs::write(&path, to_bin(&bin(1, false).into()).unwrap()).unwrap();

        let differs = run(
            &context(),
            DiffArgs {
                output: Some(dir.join("out.diff")),
                ..args(&path, &path)
            },
        )
        .unwrap();
        assert!(!differs);
        assert_eq!(std::fs::read_to_string(dir.join("out.diff")).unwrap(), "");
    }

    #[test]
    fn structural_format_fails_for_ptch_input() {
        let (_guard, dir) = temp_dir();
        let prop = dir.join("a.bin");
        std::fs::write(&prop, to_bin(&bin(1, false).into()).unwrap()).unwrap();
        let ptch = dir.join("p.bin");
        let patch = ltk_meta::BinOverride::builder().delete(0x1u32).build();
        std::fs::write(&ptch, to_bin(&patch.into()).unwrap()).unwrap();

        let error = run(
            &context(),
            DiffArgs {
                format: Some(DiffFormat::Json),
                output: Some(dir.join("out.json")),
                ..args(&prop, &ptch)
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("is a PTCH file"));
    }
}
