use std::cell::OnceCell;

use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use ltk_hash::BinHash;
use ltk_meta::{
    ApplyReport, BinFile,
    path::{PatchError, ResolveErrorKind},
};
use ltk_mimir_cache::Table;
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;

use crate::{
    cli::LayoutArgs,
    commands::{
        gamedata::GameArgs,
        input::{Inputs, game_bin},
        output::{OutputFormat, columns},
    },
    context::Context,
    document::{Format, ReadOptions, STDIO, encode, reads_back, write_bytes},
    hashes::{BinHashes, format_hash},
    utils::{hyperlink_path, plural, same_file_key},
};

#[derive(Args, Debug)]
pub struct PatchArgs {
    /// The bin to patch: a .bin file, a ritobin text file, `-` for standard input, or
    /// `game:<BIN>` for a bin of the game. `<BIN>` is a bin path, a chunk hash or an entry, as
    /// for `gamedata extract`
    pub base: Utf8PathBuf,

    /// The PTCH files to apply to BASE, in this order (.bin or ritobin text)
    #[arg(value_name = "PATCHES", required = true)]
    pub patches: Vec<Utf8PathBuf>,

    /// Write the patched bin to this file. `-` writes standard output
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<Utf8PathBuf>,

    /// Overwrite BASE with the patched bin, in the format of BASE
    #[arg(long, conflicts_with_all = ["output", "to"])]
    pub in_place: bool,

    /// Print the report only. Write no file
    #[arg(short = 'n', long, conflicts_with_all = ["output", "in_place"])]
    pub dry_run: bool,

    /// Write the patched bin even if a record cannot be applied. Such a record is skipped. The
    /// game also skips it
    #[arg(long)]
    pub partial: bool,

    /// Output format of the patched bin. Defaults to the format of the output file extension, or
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

/// The output of a run that writes the patched bin.
#[derive(Debug, PartialEq, Eq)]
struct Destination {
    path: Utf8PathBuf,
    /// The output format from `--to` or from the file extension of `path`. `None` selects the
    /// format of the base bin.
    format: Option<Format>,
}

/// One applied `PTCH` file.
struct Applied<'a> {
    patch: &'a Utf8Path,
    report: ApplyReport,
}

/// Runs the `patch` command: applies each `PTCH` file to the base bin in order, prints the
/// report and writes the patched bin.
///
/// Fails without writing a file if a record cannot be applied and `--partial` is not set.
pub fn run(ctx: &Context, args: PatchArgs) -> Result<()> {
    let destination = destination(&args)?;
    let options = ReadOptions {
        lenient: args.lenient,
    };

    let inputs = Inputs::new(ctx, &args.game, options);
    let base = inputs.read(&args.base)?;
    let BinFile::Prop(mut bin) = base.file else {
        miette::bail!(
            "{} is a PTCH file. BASE must be a PROP bin. Pass the bin first, then the PTCH files that patch it",
            args.base
        );
    };

    let mut applied = Vec::with_capacity(args.patches.len());
    for path in &args.patches {
        let patch = match inputs.read(path)?.file {
            BinFile::Override(patch) => patch,
            BinFile::Prop(_) => miette::bail!(
                "{path} is a PROP bin, not a PTCH file. Create a PTCH file with `ritobin-tools diff <BASE> <EDITED> --patch <FILE>`"
            ),
        };
        applied.push(Applied {
            patch: path,
            report: patch.apply(&mut bin),
        });
    }

    // The hashtables are loaded on first use. A run that writes a binary bin and skips no record
    // does not load them.
    let tables = OnceCell::new();
    let hashes = || {
        tables.get_or_init(|| match args.keep_hashed {
            true => BinHashes::none(),
            false => ctx.hashes(),
        })
    };
    let no_hashes = BinHashes::none();

    let skipped: usize = applied
        .iter()
        .map(|applied| applied.report.skipped.len())
        .sum();
    let report = Report {
        base: args.base.as_str(),
        output: destination
            .as_ref()
            .map(|destination| destination.path.as_str()),
        written: destination.is_some() && (skipped == 0 || args.partial),
        patches: applied
            .iter()
            .map(|applied| {
                PatchRow::new(
                    applied,
                    match skipped {
                        0 => &no_hashes,
                        _ => hashes(),
                    },
                )
            })
            .collect(),
    };
    let to_stdout = destination
        .as_ref()
        .is_some_and(|destination| destination.path.as_str() == STDIO);
    print(&report, args.format, to_stdout)?;

    if skipped > 0 && !args.partial {
        miette::bail!(
            "{} could not be applied. No file was written. Pass --partial to write the patched bin without the values of the skipped records",
            plural(skipped, "record")
        );
    }
    let Some(destination) = destination else {
        tracing::info!("Dry run. No file was written");
        return Ok(());
    };
    if skipped > 0 {
        tracing::warn!(
            "{} could not be applied. The patched bin does not contain the values of the skipped records.",
            plural(skipped, "record")
        );
    }

    let format = destination.format.unwrap_or(base.format);
    let file = BinFile::Prop(bin);
    let data = encode(
        &file,
        format,
        args.layout.over(ctx.config.print_config),
        match format {
            Format::Rito => hashes(),
            Format::Bin => &no_hashes,
        },
    )
    .wrap_err("Failed to encode the patched bin")?;
    if format == Format::Rito
        && !std::str::from_utf8(&data).is_ok_and(|text| reads_back(&file, text))
    {
        tracing::warn!(
            "The text printed for the patched bin does not parse back to the same bin, because the printer does not print every value exactly. Write the patched bin as a .bin file instead."
        );
    }

    write_bytes(&destination.path, &data)?;
    if !to_stdout {
        tracing::info!(
            "Wrote the patched bin to {}",
            hyperlink_path(&destination.path)
        );
    }
    Ok(())
}

/// Returns the output path and the requested output format of `args`. Returns `None` for
/// `--dry-run`.
///
/// Fails if more than one input is standard input, if no output option is set, if `--in-place`
/// is set while the base bin is standard input or a bin of the game, or if `--output` is the
/// path of an input.
fn destination(args: &PatchArgs) -> Result<Option<Destination>> {
    let from_stdin = std::iter::once(&args.base)
        .chain(&args.patches)
        .filter(|input| input.as_str() == STDIO)
        .count();
    if from_stdin > 1 {
        miette::bail!(
            "Standard input can be read only once, but `-` was passed for {from_stdin} inputs. Pass a file path for the other inputs"
        );
    }

    if args.dry_run {
        return Ok(None);
    }
    if args.in_place {
        if args.base.as_str() == STDIO {
            miette::bail!(
                "--in-place requires a file path for BASE, but BASE is standard input. Pass --output instead"
            );
        }
        if game_bin(&args.base).is_some() {
            miette::bail!(
                "--in-place requires a file path for BASE, but BASE is a bin of the game. Pass --output instead"
            );
        }
        return Ok(Some(Destination {
            path: args.base.clone(),
            format: None,
        }));
    }
    let Some(output) = &args.output else {
        miette::bail!(
            "No output was given. Pass --output <FILE>, --in-place to overwrite BASE, or --dry-run to print the report only"
        );
    };

    if output.as_str() != STDIO {
        let key = same_file_key(output);
        let is_output =
            |input: &Utf8PathBuf| input.as_str() != STDIO && same_file_key(input) == key;
        if is_output(&args.base) {
            miette::bail!(
                "The output path {output} is BASE itself. Pass --in-place to overwrite BASE"
            );
        }
        if let Some(patch) = args.patches.iter().find(|patch| is_output(patch)) {
            miette::bail!(
                "The output path {output} is the PTCH file {patch}. Pass a different path to --output"
            );
        }
    }
    Ok(Some(Destination {
        path: output.clone(),
        format: args
            .to
            .or_else(|| output.extension().and_then(Format::from_extension)),
    }))
}

/// A record that was not applied.
#[derive(Debug, Serialize)]
struct SkippedRow {
    /// The position of the record in its `PTCH` file, counting from 0.
    record: usize,
    /// The path hash of the object that the record addresses, as `0x` hex.
    object: String,
    /// The path of the object, if the entry table has it.
    object_name: Option<String>,
    /// The property path of the record.
    path: String,
    reason: String,
}

/// The result of applying one `PTCH` file. The object lists contain path hashes as `0x` hex.
#[derive(Debug, Serialize)]
struct PatchRow<'a> {
    patch: &'a str,
    /// The number of records that were applied.
    applied: usize,
    /// The number of applied records that added a missing property.
    inserted: usize,
    /// The objects that the delete list removed from the bin.
    deleted: Vec<String>,
    /// The objects that the `PTCH` file added to the bin.
    added: Vec<String>,
    /// The objects of the bin that an object of the `PTCH` file replaced.
    replaced: Vec<String>,
    skipped: Vec<SkippedRow>,
}

impl<'a> PatchRow<'a> {
    /// Builds the row of `applied`. The object of a skipped record is resolved through `hashes`.
    fn new(applied: &'a Applied, hashes: &BinHashes) -> Self {
        let hex = |objects: &[BinHash]| -> Vec<String> {
            objects.iter().copied().map(format_hash).collect()
        };
        let report = &applied.report;
        Self {
            patch: applied.patch.as_str(),
            applied: report.applied,
            inserted: report.inserted,
            deleted: hex(&report.deleted),
            added: hex(&report.added),
            replaced: hex(&report.replaced),
            skipped: report
                .skipped
                .iter()
                .map(|skipped| SkippedRow {
                    record: skipped.index,
                    object: format_hash(skipped.object_hash),
                    object_name: hashes
                        .lookup(Table::BinEntries, skipped.object_hash)
                        .map(Into::into),
                    path: skipped.path.to_string(),
                    reason: reason(&skipped.error),
                })
                .collect(),
        }
    }
}

/// Returns the reason why a record was skipped, as text for the report.
fn reason(error: &PatchError) -> String {
    match error {
        PatchError::Resolve(error)
            if matches!(error.kind(), ResolveErrorKind::MissingObject(_)) =>
        {
            "the bin has no object with this path hash".to_owned()
        }
        error => error.to_string(),
    }
}

/// The report of a run.
#[derive(Debug, Serialize)]
struct Report<'a> {
    base: &'a str,
    /// The output path. `None` for a dry run.
    output: Option<&'a str>,
    /// `true` if the run writes the patched bin.
    written: bool,
    patches: Vec<PatchRow<'a>>,
}

/// Prints `report` to standard output. Prints it to standard error if `bin_on_stdout` is `true`,
/// because standard output then contains the patched bin.
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

/// Formats `report` as a table with one row per `PTCH` file. If a record was skipped, a second
/// table follows with one row per skipped record.
fn table(report: &Report) -> String {
    let cells: Vec<[String; 7]> = report
        .patches
        .iter()
        .map(|row| {
            [
                row.patch.to_owned(),
                row.applied.to_string(),
                row.inserted.to_string(),
                row.skipped.len().to_string(),
                row.deleted.len().to_string(),
                row.added.len().to_string(),
                row.replaced.len().to_string(),
            ]
        })
        .collect();
    let mut out = columns(
        [
            "PATCH", "APPLIED", "INSERTED", "SKIPPED", "DELETED", "ADDED", "REPLACED",
        ],
        &cells,
    );

    let skipped: Vec<[String; 5]> = report
        .patches
        .iter()
        .flat_map(|row| {
            row.skipped.iter().map(|skipped| {
                [
                    row.patch.to_owned(),
                    skipped.record.to_string(),
                    skipped
                        .object_name
                        .clone()
                        .unwrap_or_else(|| skipped.object.clone()),
                    skipped.path.clone(),
                    skipped.reason.clone(),
                ]
            })
        })
        .collect();
    if !skipped.is_empty() {
        out.push('\n');
        out.push_str(&columns(
            ["PATCH", "RECORD", "OBJECT", "PATH", "REASON"],
            &skipped,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use ltk_hash::Hash as _;
    use ltk_meta::{Bin, BinObject, BinOverride, path::PropertyPath, property::values};

    use super::*;
    use crate::document::{Document, to_bin};

    const OBJECT: u32 = 0x1111_0001;
    const OTHER: u32 = 0x1111_0002;
    const CLASS: u32 = 0xaaaa_0001;

    fn object(path_hash: u32, size: i32) -> BinObject {
        BinObject::builder(path_hash, CLASS)
            .property(BinHash::hash_str("Size"), values::I32::new(size))
            .build()
    }

    fn bin(size: i32) -> Bin {
        Bin::builder().object(object(OBJECT, size)).build()
    }

    /// Returns a patch with one record that sets `Size` of the object `path_hash`.
    fn set_size(path_hash: u32, size: i32) -> BinOverride {
        BinOverride::builder()
            .set(
                path_hash,
                PropertyPath::new("Size").unwrap(),
                values::I32::new(size),
            )
            .build()
    }

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    fn write(path: &Utf8Path, file: impl Into<BinFile>) {
        std::fs::write(path, to_bin(&file.into()).unwrap()).unwrap();
    }

    fn read(path: &Utf8Path) -> Bin {
        match Document::read(path, ReadOptions::default()).unwrap().file {
            BinFile::Prop(bin) => bin,
            BinFile::Override(_) => panic!("{path} is a PTCH file"),
        }
    }

    fn args(base: &Utf8Path, patches: &[&Utf8Path]) -> PatchArgs {
        PatchArgs {
            base: base.to_owned(),
            patches: patches.iter().map(|patch| patch.to_path_buf()).collect(),
            output: None,
            in_place: false,
            dry_run: false,
            partial: false,
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
    fn run_writes_patched_bin_to_output_in_format_of_extension() {
        let (_guard, dir) = temp_dir();
        let (base, patch) = (dir.join("base.bin"), dir.join("size.ptch"));
        write(&base, bin(1));
        write(&patch, set_size(OBJECT, 2));

        for name in ["out.bin", "out.rito"] {
            let output = dir.join(name);
            run(
                &context(),
                PatchArgs {
                    output: Some(output.clone()),
                    ..args(&base, &[&patch])
                },
            )
            .unwrap();
            assert_eq!(read(&output), bin(2));
        }
        assert!(
            std::fs::read(dir.join("out.bin"))
                .unwrap()
                .starts_with(b"PROP")
        );
        assert!(
            std::fs::read(dir.join("out.rito"))
                .unwrap()
                .starts_with(b"#PROP_text")
        );
        assert_eq!(read(&base), bin(1));
    }

    #[test]
    fn run_applies_patches_in_argument_order() {
        let (_guard, dir) = temp_dir();
        let base = dir.join("base.bin");
        let (first, second) = (dir.join("first.ptch"), dir.join("second.ptch"));
        write(&base, bin(1));
        // The first patch adds the object that the record of the second patch addresses.
        write(
            &first,
            BinOverride::builder()
                .object(object(OTHER, 5))
                .set(
                    OBJECT,
                    PropertyPath::new("Size").unwrap(),
                    values::I32::new(2),
                )
                .build(),
        );
        write(&second, set_size(OTHER, 6));

        let output = dir.join("out.bin");
        run(
            &context(),
            PatchArgs {
                output: Some(output.clone()),
                ..args(&base, &[&first, &second])
            },
        )
        .unwrap();
        assert_eq!(
            read(&output),
            Bin::builder()
                .object(object(OBJECT, 2))
                .object(object(OTHER, 6))
                .build()
        );

        let error = run(
            &context(),
            PatchArgs {
                output: Some(dir.join("reversed.bin")),
                ..args(&base, &[&second, &first])
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("1 record could not be applied"));
    }

    #[test]
    fn in_place_overwrites_base_in_format_of_base() {
        let (_guard, dir) = temp_dir();
        let (base, patch) = (dir.join("base.rito"), dir.join("size.ptch"));
        let text = encode(
            &bin(1).into(),
            Format::Rito,
            Default::default(),
            &BinHashes::none(),
        )
        .unwrap();
        std::fs::write(&base, text).unwrap();
        write(&patch, set_size(OBJECT, 2));

        run(
            &context(),
            PatchArgs {
                in_place: true,
                ..args(&base, &[&patch])
            },
        )
        .unwrap();
        assert!(std::fs::read(&base).unwrap().starts_with(b"#PROP_text"));
        assert_eq!(read(&base), bin(2));
    }

    #[test]
    fn run_patches_base_with_game_prefix_from_game() {
        use crate::game::testing::Installation;

        let installation = Installation::new();
        installation.archive(
            "A.wad.client",
            &[("data/skin0.bin", &to_bin(&bin(1).into()).unwrap())],
        );
        let patch = installation.root.join("size.ptch");
        write(&patch, set_size(OBJECT, 2));
        let output = installation.root.join("out").join("skin0.bin");

        run(
            &context(),
            PatchArgs {
                output: Some(output.clone()),
                game: GameArgs {
                    game_dir: Some(installation.root.clone()),
                    index_dir: Some(installation.root.join("index")),
                },
                ..args(Utf8Path::new("game:data/skin0.bin"), &[&patch])
            },
        )
        .unwrap();
        // The base is read as a binary bin, so the output is binary.
        assert!(std::fs::read(&output).unwrap().starts_with(b"PROP"));
        assert_eq!(read(&output), bin(2));
    }

    #[test]
    fn run_writes_no_file_if_record_is_skipped() {
        let (_guard, dir) = temp_dir();
        let (base, patch) = (dir.join("base.bin"), dir.join("stale.ptch"));
        write(&base, bin(1));
        write(
            &patch,
            BinOverride::builder()
                .set(
                    OBJECT,
                    PropertyPath::new("Size").unwrap(),
                    values::I32::new(2),
                )
                .set(
                    OTHER,
                    PropertyPath::new("Size").unwrap(),
                    values::I32::new(3),
                )
                .build(),
        );
        let before = std::fs::read(&base).unwrap();

        let output = dir.join("out.bin");
        let error = run(
            &context(),
            PatchArgs {
                output: Some(output.clone()),
                ..args(&base, &[&patch])
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("1 record could not be applied"));
        assert!(!output.exists());

        let error = run(
            &context(),
            PatchArgs {
                in_place: true,
                ..args(&base, &[&patch])
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("No file was written"));
        assert_eq!(std::fs::read(&base).unwrap(), before);
    }

    #[test]
    fn partial_writes_bin_without_skipped_record() {
        let (_guard, dir) = temp_dir();
        let (base, patch) = (dir.join("base.bin"), dir.join("stale.ptch"));
        write(&base, bin(1));
        write(
            &patch,
            BinOverride::builder()
                .set(
                    OBJECT,
                    PropertyPath::new("Size").unwrap(),
                    values::I32::new(2),
                )
                .set(
                    OTHER,
                    PropertyPath::new("Size").unwrap(),
                    values::I32::new(3),
                )
                .build(),
        );

        let output = dir.join("out.bin");
        run(
            &context(),
            PatchArgs {
                output: Some(output.clone()),
                partial: true,
                ..args(&base, &[&patch])
            },
        )
        .unwrap();
        assert_eq!(read(&output), bin(2));
    }

    #[test]
    fn dry_run_writes_no_file_and_fails_if_record_is_skipped() {
        let (_guard, dir) = temp_dir();
        let base = dir.join("base.bin");
        let (fits, stale) = (dir.join("fits.ptch"), dir.join("stale.ptch"));
        write(&base, bin(1));
        write(&fits, set_size(OBJECT, 2));
        write(&stale, set_size(OTHER, 2));
        let before = std::fs::read(&base).unwrap();

        let dry_run = |patch: &Utf8Path| {
            run(
                &context(),
                PatchArgs {
                    dry_run: true,
                    ..args(&base, &[patch])
                },
            )
        };
        dry_run(&fits).unwrap();
        assert!(dry_run(&stale).is_err());
        assert_eq!(std::fs::read(&base).unwrap(), before);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
    }

    #[test]
    fn run_fails_if_base_is_ptch_or_patch_is_prop() {
        let (_guard, dir) = temp_dir();
        let (base, patch) = (dir.join("base.bin"), dir.join("size.ptch"));
        write(&base, bin(1));
        write(&patch, set_size(OBJECT, 2));
        let output = Some(dir.join("out.bin"));

        let error = run(
            &context(),
            PatchArgs {
                output: output.clone(),
                ..args(&patch, &[&patch])
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("BASE must be a PROP bin"));

        let error = run(
            &context(),
            PatchArgs {
                output,
                ..args(&base, &[&base])
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("is a PROP bin, not a PTCH file"));
        assert!(!dir.join("out.bin").exists());
    }

    #[test]
    fn destination_rejects_output_that_is_an_input() {
        let (base, patch) = (Utf8Path::new("base.bin"), Utf8Path::new("size.ptch"));
        let with_output = |output: &str| PatchArgs {
            output: Some(output.into()),
            ..args(base, &[patch])
        };

        let error = destination(&with_output("./base.bin")).unwrap_err();
        assert!(error.to_string().contains("Pass --in-place"));
        let error = destination(&with_output("size.ptch")).unwrap_err();
        assert!(error.to_string().contains("is the PTCH file size.ptch"));
        let error = destination(&args(base, &[patch])).unwrap_err();
        assert!(error.to_string().contains("No output was given"));

        assert_eq!(
            destination(&with_output("out.rito")).unwrap(),
            Some(Destination {
                path: "out.rito".into(),
                format: Some(Format::Rito),
            })
        );
        assert_eq!(
            destination(&PatchArgs {
                to: Some(Format::Bin),
                ..with_output("-")
            })
            .unwrap(),
            Some(Destination {
                path: STDIO.into(),
                format: Some(Format::Bin),
            })
        );
    }

    #[test]
    fn destination_rejects_stdin_for_in_place_and_for_two_inputs() {
        let patch = Utf8Path::new("size.ptch");
        let stdin = Utf8Path::new(STDIO);

        let error = destination(&PatchArgs {
            in_place: true,
            ..args(stdin, &[patch])
        })
        .unwrap_err();
        assert!(error.to_string().contains("BASE is standard input"));

        let error = destination(&PatchArgs {
            in_place: true,
            ..args(Utf8Path::new("game:data/skin0.bin"), &[patch])
        })
        .unwrap_err();
        assert!(error.to_string().contains("BASE is a bin of the game"));

        let error = destination(&PatchArgs {
            dry_run: true,
            ..args(stdin, &[stdin])
        })
        .unwrap_err();
        assert!(error.to_string().contains("can be read only once"));

        assert_eq!(
            destination(&PatchArgs {
                in_place: true,
                ..args(Utf8Path::new("base.bin"), &[patch])
            })
            .unwrap(),
            Some(Destination {
                path: "base.bin".into(),
                format: None,
            })
        );
    }

    #[test]
    fn table_lists_patches_and_skipped_records() {
        colored::control::set_override(false);
        let mut base = bin(1);
        let patch = BinOverride::builder()
            .delete(OBJECT)
            .object(object(OTHER, 5))
            .set(
                OTHER,
                PropertyPath::new("Size").unwrap(),
                values::I32::new(6),
            )
            .set(
                OBJECT,
                PropertyPath::new("Size").unwrap(),
                values::I32::new(2),
            )
            .build();
        let applied = Applied {
            patch: Utf8Path::new("edit.ptch"),
            report: patch.apply(&mut base),
        };
        let report = Report {
            base: "base.bin",
            output: None,
            written: false,
            patches: vec![PatchRow::new(&applied, &BinHashes::none())],
        };

        assert_eq!(
            table(&report),
            "PATCH      APPLIED  INSERTED  SKIPPED  DELETED  ADDED  REPLACED\n\
             edit.ptch  1        0         1        1        1      0\n\
             \n\
             PATCH      RECORD  OBJECT      PATH  REASON\n\
             edit.ptch  1       0x11110001  Size  the bin has no object with this path hash\n"
        );

        let document = serde_json::to_value(&report).unwrap();
        assert_eq!(document["output"], serde_json::Value::Null);
        assert_eq!(document["patches"][0]["deleted"][0], "0x11110001");
        assert_eq!(document["patches"][0]["added"][0], "0x11110002");
        assert_eq!(document["patches"][0]["skipped"][0]["record"], 1);
        assert_eq!(document["patches"][0]["skipped"][0]["path"], "Size");
    }
}
