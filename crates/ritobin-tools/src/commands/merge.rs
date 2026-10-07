use std::cell::OnceCell;

use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use ltk_hash::BinHash;
use ltk_meta::{BinFile, MergeReport};
use ltk_mimir_cache::Table;
use miette::{IntoDiagnostic, Result};
use serde::Serialize;

use crate::{
    cli::LayoutArgs,
    commands::{
        gamedata::GameArgs,
        input::Inputs,
        output::{OutputFormat, columns},
        write::{self, Destination, Request},
    },
    context::Context,
    document::{Format, ReadOptions, STDIO, write_bytes},
    hashes::{BinHashes, format_hash},
};

#[derive(Args, Debug)]
pub struct MergeArgs {
    /// The base bin: a .bin file, a ritobin text file, `-` for standard input, or `game:<BIN>`
    /// for a bin of the game. `<BIN>` is a bin path, a chunk hash or an entry, as for
    /// `gamedata extract`
    pub base: Utf8PathBuf,

    /// The bins to merge into BASE, in this order (.bin or ritobin text). A value of a later
    /// bin replaces the value of an earlier bin
    #[arg(value_name = "EDITS", required = true)]
    pub edits: Vec<Utf8PathBuf>,

    /// Write the merged bin to this file. `-` writes standard output
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<Utf8PathBuf>,

    /// Overwrite BASE with the merged bin, in the format of BASE
    #[arg(long, conflicts_with_all = ["output", "to"])]
    pub in_place: bool,

    /// Print the report only. Write no file
    #[arg(short = 'n', long, conflicts_with_all = ["output", "in_place"])]
    pub dry_run: bool,

    /// Output format of the merged bin. Defaults to the format of the output file extension, or
    /// to the format of BASE
    #[arg(short, long, value_enum, value_name = "FORMAT")]
    pub to: Option<Format>,

    /// Format of the report
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    /// Write hashes as hex. Do not resolve them with the hashtables
    #[arg(short, long)]
    pub keep_hashed: bool,

    /// Read text that has problems. The invalid parts are skipped
    #[arg(long)]
    pub lenient: bool,

    #[command(flatten)]
    pub game: GameArgs,

    #[command(flatten)]
    pub layout: LayoutArgs,
}

/// One merged bin.
struct Merged<'a> {
    edit: &'a Utf8Path,
    report: MergeReport,
}

/// Runs the `merge` command: merges each edit bin into the base bin in order, prints the report
/// and writes the merged bin.
pub fn run(ctx: &Context, args: MergeArgs) -> Result<()> {
    let destination = write::destination(&Request {
        base: &args.base,
        others: &args.edits,
        output: args.output.as_ref(),
        in_place: args.in_place,
        dry_run: args.dry_run,
        to: args.to,
    })?;

    let inputs = Inputs::new(
        ctx,
        &args.game,
        ReadOptions {
            lenient: args.lenient,
        },
    );
    let prop = |path: &Utf8Path, role: &str| match inputs.read(path)?.file {
        BinFile::Prop(bin) => Ok(bin),
        BinFile::Override(_) => Err(miette::miette!(
            "{path} is a PTCH file. {role} must be a PROP bin. Apply a PTCH file with `ritobin-tools patch`"
        )),
    };
    let base = inputs.read(&args.base)?;
    let base_format = base.format;
    let BinFile::Prop(mut bin) = base.file else {
        miette::bail!(
            "{} is a PTCH file. BASE must be a PROP bin. Apply a PTCH file with `ritobin-tools patch`",
            args.base
        );
    };

    let mut merged = Vec::with_capacity(args.edits.len());
    for path in &args.edits {
        let edit = prop(path, "Each of EDITS")?;
        merged.push(Merged {
            edit: path,
            report: bin.merge(&edit),
        });
    }

    // The hashtables are loaded on first use. A run that writes a binary bin and replaces no
    // value by a value of a different type does not load them.
    let tables = OnceCell::new();
    let hashes = || {
        tables.get_or_init(|| match args.keep_hashed {
            true => BinHashes::none(),
            false => ctx.hashes(),
        })
    };
    let no_hashes = BinHashes::none();

    let mismatched: usize = merged
        .iter()
        .flat_map(|merged| &merged.report.replaced)
        .filter(|replaced| replaced.mismatched)
        .count();
    let report = Report {
        base: args.base.as_str(),
        output: destination
            .as_ref()
            .map(|destination| destination.path.as_str()),
        written: destination.is_some(),
        edits: merged
            .iter()
            .map(|merged| {
                EditRow::new(
                    merged,
                    match mismatched {
                        0 => &no_hashes,
                        _ => hashes(),
                    },
                )
            })
            .collect(),
    };
    let to_stdout = destination.as_ref().is_some_and(Destination::is_stdout);
    print(&report, args.format, to_stdout)?;

    if mismatched > 0 {
        tracing::warn!(
            "{} replaced by a value of a different type. Check each of them: the game ignores a value whose type differs from the type of its property.",
            match mismatched {
                1 => "1 value was".to_owned(),
                mismatched => format!("{mismatched} values were"),
            }
        );
    }
    let Some(destination) = destination else {
        tracing::info!("Dry run. No file was written");
        return Ok(());
    };

    let format = destination.format.unwrap_or(base_format);
    write::write_bin(
        &destination.path,
        format,
        bin,
        args.layout.over(ctx.config.print_config),
        match format {
            Format::Rito => hashes(),
            Format::Bin => &no_hashes,
        },
        "merged bin",
    )
}

/// A value that was replaced by a value of a different type.
#[derive(Debug, Serialize)]
struct MismatchRow {
    /// The path hash of the object that contains the value, as `0x` hex.
    object: String,
    /// The path of the object, if the entry table has it.
    object_name: Option<String>,
    /// The path of the value inside the object.
    path: String,
}

/// The result of merging one bin into the base bin. The object lists contain path hashes as `0x`
/// hex.
#[derive(Debug, Serialize)]
struct EditRow<'a> {
    edit: &'a str,
    /// The objects that the base bin did not have.
    objects_added: Vec<String>,
    /// The number of objects of both bins with the same class. They are merged property by
    /// property.
    objects_merged: usize,
    /// The objects of both bins with different classes. The object of the edit replaces the
    /// object of the base bin.
    objects_replaced: Vec<String>,
    /// The number of values of merged objects that the edit replaced.
    values_replaced: usize,
    /// The number of properties that the edit added to merged objects.
    properties_inserted: usize,
    /// The number of map entries that the edit added to merged objects.
    keys_inserted: usize,
    /// The dependencies that the base bin did not have.
    dependencies_added: Vec<String>,
    mismatched: Vec<MismatchRow>,
}

impl<'a> EditRow<'a> {
    /// Builds the row of `merged`. The names of the mismatched values are resolved through
    /// `hashes`.
    fn new(merged: &'a Merged, hashes: &BinHashes) -> Self {
        let hex = |objects: &mut dyn Iterator<Item = BinHash>| -> Vec<String> {
            objects.map(format_hash).collect()
        };
        let report = &merged.report;
        Self {
            edit: merged.edit.as_str(),
            objects_added: hex(&mut report.objects_added.iter().copied()),
            objects_merged: report.objects_merged.len(),
            objects_replaced: hex(&mut report
                .objects_replaced
                .iter()
                .map(|object| object.path_hash)),
            values_replaced: report.replaced.len(),
            properties_inserted: report.inserted,
            keys_inserted: report.keys_inserted,
            dependencies_added: report.dependencies_added.clone(),
            mismatched: report
                .replaced
                .iter()
                .filter(|replaced| replaced.mismatched)
                .map(|replaced| MismatchRow {
                    object: format_hash(replaced.object_hash),
                    object_name: hashes
                        .lookup(Table::BinEntries, replaced.object_hash)
                        .map(Into::into),
                    path: replaced.at.to_named(hashes).text,
                })
                .collect(),
        }
    }
}

/// The report of a run.
#[derive(Debug, Serialize)]
struct Report<'a> {
    base: &'a str,
    /// The output path. `None` for a dry run.
    output: Option<&'a str>,
    /// `true` if the run writes the merged bin.
    written: bool,
    edits: Vec<EditRow<'a>>,
}

/// Prints `report` to standard output. Prints it to standard error if `bin_on_stdout` is `true`,
/// because standard output then contains the merged bin.
fn print(report: &Report, format: OutputFormat, bin_on_stdout: bool) -> Result<()> {
    let out = match format {
        OutputFormat::Json => {
            let mut out = serde_json::to_string_pretty(report).into_diagnostic()?;
            out.push('\n');
            out
        }
        OutputFormat::Table => table(report),
    };
    match bin_on_stdout {
        true => {
            eprint!("{out}");
            Ok(())
        }
        false => write_bytes(STDIO.into(), out.as_bytes()),
    }
}

/// Formats `report` as a table with one row per edit bin. If a value was replaced by a value of
/// a different type, a second table follows with one row per such value.
fn table(report: &Report) -> String {
    let cells: Vec<[String; 9]> = report
        .edits
        .iter()
        .map(|row| {
            [
                row.edit.to_owned(),
                row.objects_added.len().to_string(),
                row.objects_merged.to_string(),
                row.objects_replaced.len().to_string(),
                row.values_replaced.to_string(),
                row.properties_inserted.to_string(),
                row.keys_inserted.to_string(),
                row.dependencies_added.len().to_string(),
                row.mismatched.len().to_string(),
            ]
        })
        .collect();
    let mut out = columns(
        [
            "EDIT",
            "ADDED",
            "MERGED",
            "REPLACED",
            "VALUES",
            "INSERTED",
            "KEYS",
            "LINKS",
            "MISMATCHED",
        ],
        &cells,
    );

    let mismatched: Vec<[String; 3]> = report
        .edits
        .iter()
        .flat_map(|row| {
            row.mismatched.iter().map(|mismatch| {
                [
                    row.edit.to_owned(),
                    mismatch
                        .object_name
                        .clone()
                        .unwrap_or_else(|| mismatch.object.clone()),
                    mismatch.path.clone(),
                ]
            })
        })
        .collect();
    if !mismatched.is_empty() {
        out.push('\n');
        out.push_str(&columns(["EDIT", "OBJECT", "MISMATCHED PATH"], &mismatched));
    }
    out
}

#[cfg(test)]
mod tests {
    use ltk_meta::{Bin, BinObject, property::values};

    use super::*;
    use crate::document::{Document, to_bin};

    const OBJECT: u32 = 0x1111_0001;
    const OTHER: u32 = 0x1111_0002;
    const CLASS: u32 = 0xaaaa_0001;
    const SIZE: u32 = 0x10;
    const NAME: u32 = 0x11;

    fn object(path_hash: u32, size: i32) -> BinObject {
        BinObject::builder(path_hash, CLASS)
            .property(SIZE, values::I32::new(size))
            .build()
    }

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    fn write(path: &Utf8Path, bin: Bin) {
        std::fs::write(path, to_bin(&bin.into()).unwrap()).unwrap();
    }

    fn read(path: &Utf8Path) -> Bin {
        match Document::read(path, ReadOptions::default()).unwrap().file {
            BinFile::Prop(bin) => bin,
            BinFile::Override(_) => panic!("{path} is a PTCH file"),
        }
    }

    fn args(base: &Utf8Path, edits: &[&Utf8Path]) -> MergeArgs {
        MergeArgs {
            base: base.to_owned(),
            edits: edits.iter().map(|edit| edit.to_path_buf()).collect(),
            output: None,
            in_place: false,
            dry_run: false,
            to: None,
            format: OutputFormat::Table,
            keep_hashed: true,
            lenient: false,
            game: GameArgs::default(),
            layout: LayoutArgs::default(),
        }
    }

    fn context() -> Context {
        Context::for_tests(None)
    }

    #[test]
    fn run_merges_edits_in_argument_order_and_keeps_base_values() {
        let (_guard, dir) = temp_dir();
        let base = dir.join("base.bin");
        let (first, second) = (dir.join("first.bin"), dir.join("second.bin"));
        write(
            &base,
            Bin::builder()
                .dependency("shared.bin")
                .object(
                    BinObject::builder(OBJECT, CLASS)
                        .property(SIZE, values::I32::new(1))
                        .property(NAME, values::String::new("base".to_owned()))
                        .build(),
                )
                .build(),
        );
        // The first edit changes a value and adds an object. The second edit changes the same
        // value again and adds a dependency.
        write(
            &first,
            Bin::builder()
                .object(object(OBJECT, 2))
                .object(object(OTHER, 5))
                .build(),
        );
        write(
            &second,
            Bin::builder()
                .dependency("extra.bin")
                .object(object(OBJECT, 3))
                .build(),
        );

        let output = dir.join("out.bin");
        run(
            &context(),
            MergeArgs {
                output: Some(output.clone()),
                ..args(&base, &[&first, &second])
            },
        )
        .unwrap();
        assert_eq!(
            read(&output),
            Bin::builder()
                .dependency("shared.bin")
                .dependency("extra.bin")
                .object(
                    BinObject::builder(OBJECT, CLASS)
                        .property(SIZE, values::I32::new(3))
                        .property(NAME, values::String::new("base".to_owned()))
                        .build(),
                )
                .object(object(OTHER, 5))
                .build()
        );
    }

    #[test]
    fn dry_run_writes_no_file_and_in_place_overwrites_base() {
        let (_guard, dir) = temp_dir();
        let (base, edit) = (dir.join("base.bin"), dir.join("edit.bin"));
        write(&base, Bin::builder().object(object(OBJECT, 1)).build());
        write(&edit, Bin::builder().object(object(OBJECT, 2)).build());
        let before = std::fs::read(&base).unwrap();

        run(
            &context(),
            MergeArgs {
                dry_run: true,
                ..args(&base, &[&edit])
            },
        )
        .unwrap();
        assert_eq!(std::fs::read(&base).unwrap(), before);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);

        run(
            &context(),
            MergeArgs {
                in_place: true,
                ..args(&base, &[&edit])
            },
        )
        .unwrap();
        assert_eq!(
            read(&base),
            Bin::builder().object(object(OBJECT, 2)).build()
        );
    }

    #[test]
    fn run_fails_if_input_is_ptch() {
        let (_guard, dir) = temp_dir();
        let (base, patch) = (dir.join("base.bin"), dir.join("edit.ptch"));
        write(&base, Bin::builder().object(object(OBJECT, 1)).build());
        let ptch: BinFile = ltk_meta::BinOverride::builder()
            .delete(OBJECT)
            .build()
            .into();
        std::fs::write(&patch, to_bin(&ptch).unwrap()).unwrap();

        let error = run(
            &context(),
            MergeArgs {
                dry_run: true,
                ..args(&base, &[&patch])
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Each of EDITS must be a PROP bin")
        );

        let error = run(
            &context(),
            MergeArgs {
                dry_run: true,
                ..args(&patch, &[&base])
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("BASE must be a PROP bin"));
    }

    #[test]
    fn table_counts_changes_and_lists_mismatched_values() {
        colored::control::set_override(false);
        let mut base = Bin::builder()
            .object(
                BinObject::builder(OBJECT, CLASS)
                    .property(SIZE, values::I32::new(1))
                    .property(NAME, values::String::new("base".to_owned()))
                    .build(),
            )
            .build();
        // `Size` changes its type, `Name` changes its value, and one property and one object
        // are new.
        let edit = Bin::builder()
            .dependency("extra.bin")
            .object(
                BinObject::builder(OBJECT, CLASS)
                    .property(SIZE, values::F32::new(2.0))
                    .property(NAME, values::String::new("edit".to_owned()))
                    .property(0x12u32, values::Bool::new(true))
                    .build(),
            )
            .object(object(OTHER, 5))
            .build();
        let merged = Merged {
            edit: Utf8Path::new("edit.bin"),
            report: base.merge(&edit),
        };
        let report = Report {
            base: "base.bin",
            output: None,
            written: false,
            edits: vec![EditRow::new(&merged, &BinHashes::none())],
        };

        assert_eq!(
            table(&report),
            "EDIT      ADDED  MERGED  REPLACED  VALUES  INSERTED  KEYS  LINKS  MISMATCHED\n\
             edit.bin  1      1       0         2       1         0     1      1\n\
             \n\
             EDIT      OBJECT      MISMATCHED PATH\n\
             edit.bin  0x11110001  00000010\n"
        );

        let document = serde_json::to_value(&report).unwrap();
        assert_eq!(document["edits"][0]["objects_added"][0], "0x11110002");
        assert_eq!(document["edits"][0]["dependencies_added"][0], "extra.bin");
        assert_eq!(document["edits"][0]["mismatched"][0]["path"], "00000010");
    }
}
