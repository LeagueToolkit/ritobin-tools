use std::{cell::OnceCell, collections::HashMap};

use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use ltk_meta::BinFile;
use miette::{IntoDiagnostic, Result, WrapErr};
use walkdir::WalkDir;

use crate::{
    cli::LayoutArgs,
    commands::{
        gamedata::GameArgs,
        input::{Inputs, game_bin},
    },
    context::Context,
    declaration,
    document::{
        BIN_EXTENSION, DEFAULT_TEXT_EXTENSION, Document, Format, JSON_EXTENSION, ReadOptions,
        STDIO, TEXT_EXTENSIONS, TextLayout, YAML_EXTENSIONS, converted_path, detect_file, encode,
        reads_back, scanned_format, write_bytes,
    },
    hashes::BinHashes,
    utils::{hyperlink_path, plural, same_file_key},
};

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// Files or directories to convert. `-` reads standard input. `game:<BIN>` reads a bin
    /// of the game, where `<BIN>` is a bin path, a chunk hash or an entry, as for
    /// `gamedata extract`. A `.yaml` file is a bin declaration
    #[arg(value_name = "INPUTS")]
    pub inputs: Vec<Utf8PathBuf>,

    /// Files or directories to convert. Same as the positional inputs
    #[arg(short, long = "input", value_name = "PATH", num_args = 1..)]
    pub input: Vec<Utf8PathBuf>,

    /// Output path: a file for a file input, a directory for a directory input, or `-` for
    /// standard output. Defaults to a file next to each input
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<Utf8PathBuf>,

    /// Include the subdirectories of a directory input
    #[arg(short, long)]
    pub recursive: bool,

    /// Output format. Defaults to the format of the output file extension. Without one, a
    /// binary bin is converted to ritobin text and every other input to a binary bin
    #[arg(short, long, value_enum, value_name = "FORMAT")]
    pub to: Option<Format>,

    /// Input format that a directory input is scanned for. Defaults to `bin` if `--to` is a
    /// text format, and to `rito` if `--to` is `bin`
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub from: Option<Format>,

    /// File extension for text output
    #[arg(long = "ext", value_name = "EXT", default_value = DEFAULT_TEXT_EXTENSION)]
    pub text_extension: String,

    /// Write hashes as hex. Do not resolve them with the hashtables
    #[arg(short, long)]
    pub keep_hashed: bool,

    /// Convert text that has problems. The invalid parts are skipped
    #[arg(long)]
    pub lenient: bool,

    /// Do not overwrite an existing output file
    #[arg(long)]
    pub skip_existing: bool,

    /// Skip the check that text printed from a bin parses back to the same bin
    #[arg(long)]
    pub no_verify: bool,

    #[command(flatten)]
    pub game: GameArgs,

    #[command(flatten)]
    pub layout: LayoutArgs,
}

impl ConvertArgs {
    /// Returns the text output extension without a leading dot.
    fn text_extension(&self) -> &str {
        self.text_extension.trim_start_matches('.')
    }

    /// Returns the output format requested by `--to`, or by the file extension of `output`.
    /// Returns `None` if neither specifies a format.
    fn asked_format(&self, output: Option<&Utf8Path>) -> Option<Format> {
        self.to.or_else(|| {
            output
                .and_then(|output| output.extension())
                .and_then(Format::from_extension)
        })
    }
}

/// One conversion: an input file and its output path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Job {
    input: Utf8PathBuf,
    output: Utf8PathBuf,
    /// The output format. `None` for standard input or a game bin without a requested format.
    /// The output format is then the other format than the input, which is known after the
    /// input is read.
    to: Option<Format>,
}

/// The result of one conversion that did not fail.
enum Outcome {
    Converted,
    Skipped,
}

/// Runs the `convert` command. With several inputs, a failed conversion does not stop the run.
/// The command fails after the last input if any conversion failed.
pub fn run(ctx: &Context, args: ConvertArgs) -> Result<()> {
    let jobs = plan(&args)?;
    let layout = args.layout.over(ctx.config.print_config);
    let inputs = Inputs::new(
        ctx,
        &args.game,
        ReadOptions {
            lenient: args.lenient,
        },
    );
    // The hashtables are loaded on first use. A run that only writes bins does not load them.
    let tables = OnceCell::new();
    let hashes = || {
        tables.get_or_init(|| match args.keep_hashed {
            true => BinHashes::none(),
            false => ctx.hashes(),
        })
    };

    if let [job] = jobs.as_slice() {
        return convert(job, &args, &inputs, layout, &hashes).map(|_| ());
    }

    let (mut converted, mut skipped, mut failed) = (0, 0, 0);
    for job in &jobs {
        match convert(job, &args, &inputs, layout, &hashes) {
            Ok(Outcome::Converted) => converted += 1,
            Ok(Outcome::Skipped) => skipped += 1,
            Err(error) => {
                failed += 1;
                eprintln!("{error:?}");
            }
        }
    }

    tracing::info!(
        "{} converted, {skipped} skipped, {failed} failed",
        plural(converted, "file")
    );
    if failed > 0 {
        miette::bail!(
            "{failed} of {} failed to convert",
            plural(jobs.len(), "file")
        );
    }
    Ok(())
}

/// Builds the list of conversions for `args`. Fails before any file is written if an output path
/// equals an input path or another output path.
fn plan(args: &ConvertArgs) -> Result<Vec<Job>> {
    let inputs: Vec<&Utf8PathBuf> = args.inputs.iter().chain(&args.input).collect();
    if inputs.is_empty() {
        miette::bail!("No input was given. Pass files or directories to convert");
    }
    if args.output.is_some() && inputs.len() > 1 {
        miette::bail!(
            "--output requires exactly one input, but {} were given",
            inputs.len()
        );
    }

    let mut jobs = Vec::new();
    for input in inputs {
        if input.as_str() == STDIO {
            jobs.push(Job {
                input: input.clone(),
                output: args.output.clone().unwrap_or_else(|| STDIO.into()),
                to: args.asked_format(args.output.as_deref()),
            });
        } else if game_bin(input).is_some() {
            let Some(output) = &args.output else {
                miette::bail!(
                    "{input} is a bin of the game and has no default output path. Pass --output <FILE>, or `--output -` for standard output"
                );
            };
            jobs.push(Job {
                input: input.clone(),
                output: output.clone(),
                to: args.asked_format(Some(output)),
            });
        } else if input.is_dir() {
            jobs.extend(scan(input, args)?);
        } else if input.exists() {
            let to = match args.asked_format(args.output.as_deref()) {
                Some(to) => to,
                None => detect_file(input)?.opposite(),
            };
            jobs.push(Job {
                input: input.clone(),
                output: args
                    .output
                    .clone()
                    .unwrap_or_else(|| converted_path(input, to, args.text_extension())),
                to: Some(to),
            });
        } else {
            miette::bail!("Input does not exist: {input}");
        }
    }

    check_overwrites(&jobs)?;
    Ok(jobs)
}

/// Fails if the output path of a job equals the input path of any job, or the output path of
/// another job.
fn check_overwrites(jobs: &[Job]) -> Result<()> {
    let inputs: HashMap<String, &Utf8Path> = jobs
        .iter()
        .filter(|job| job.input.as_str() != STDIO && game_bin(&job.input).is_none())
        .map(|job| (same_file_key(&job.input), job.input.as_path()))
        .collect();
    let mut outputs: HashMap<String, &Utf8Path> = HashMap::new();

    for job in jobs {
        if job.output.as_str() == STDIO {
            continue;
        }
        let key = same_file_key(&job.output);
        match inputs.get(&key) {
            Some(input) if *input == job.input => miette::bail!(
                "The output path of {} is the input file itself. Pass --output to write to a different path",
                job.input
            ),
            Some(input) => miette::bail!(
                "The output path of {} is {input}, which is also an input. Convert the files separately",
                job.input
            ),
            None => {}
        }
        if let Some(other) = outputs.insert(key, &job.input) {
            miette::bail!(
                "{other} and {} have the same output path {}. Convert the files separately",
                job.input,
                job.output
            );
        }
    }
    Ok(())
}

/// Builds the conversions for the files of one format in `dir`, sorted by path. The output of a
/// file is next to the file, or at the same relative path under the output directory.
fn scan(dir: &Utf8Path, args: &ConvertArgs) -> Result<Vec<Job>> {
    let from = args
        .from
        .or(args.to.map(Format::opposite))
        .unwrap_or(Format::Bin);
    let to = args.to.unwrap_or(from.opposite());
    if from == Format::Json {
        miette::bail!(
            "JSON is an output format and cannot be read back. Pass --from yaml, rito or bin"
        );
    }
    if let Some(output) = &args.output
        && output.as_str() == STDIO
    {
        miette::bail!("A directory cannot be converted to standard output");
    }

    let walker = match args.recursive {
        true => WalkDir::new(dir),
        false => WalkDir::new(dir).max_depth(1),
    };
    let mut files = Vec::new();
    for entry in walker.sort_by_file_name() {
        let entry = entry
            .into_diagnostic()
            .wrap_err_with(|| format!("Failed to read directory {dir}"))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(path) = Utf8Path::from_path(entry.path()) else {
            tracing::warn!("Skipping non-UTF-8 path: {}", entry.path().display());
            continue;
        };
        // The extension is tested first, because `scanned_format` reads the start of a `.py`
        // file.
        if path.extension().and_then(Format::from_extension) != Some(from)
            || scanned_format(path) != Some(from)
        {
            continue;
        }
        files.push(path.to_owned());
    }

    if files.is_empty() {
        let extensions = |extensions: &[&str]| {
            extensions
                .iter()
                .map(|extension| format!(".{extension}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let wanted = match from {
            Format::Bin => extensions(&[BIN_EXTENSION]),
            Format::Rito => extensions(TEXT_EXTENSIONS),
            Format::Yaml => extensions(YAML_EXTENSIONS),
            Format::Json => extensions(&[JSON_EXTENSION]),
        };
        let recursion = match args.recursive {
            true => "",
            false => " (pass --recursive to include subdirectories)",
        };
        miette::bail!(
            "No {wanted} files were found in {dir}{recursion}. Pass --from {} to convert in the other direction",
            from.opposite()
        );
    }

    Ok(files
        .into_iter()
        .map(|input| {
            let target = match &args.output {
                Some(output_dir) => output_dir.join(input.strip_prefix(dir).unwrap_or(&input)),
                None => input.clone(),
            };
            Job {
                output: converted_path(&target, to, args.text_extension()),
                input,
                to: Some(to),
            }
        })
        .collect())
}

/// Builds the YAML declaration `yaml`, which was written for `document`, back into a bin with
/// the class schema of the game. Logs a warning if the result is not the bin of `document`, or
/// if the declaration cannot be built.
///
/// Does nothing if no game directory is set, because the class schema comes from the game.
fn verify_yaml(document: &Document, yaml: &[u8], input: &Utf8Path, inputs: &Inputs) {
    let Some(schema) = inputs.schema() else {
        tracing::debug!(
            "The YAML written for {input} was not verified, because no game directory is set"
        );
        return;
    };
    let built = schema.and_then(|schema| {
        let text = std::str::from_utf8(yaml).into_diagnostic()?;
        declaration::from_yaml(input.as_str(), text, schema)
    });
    match (built, &document.file) {
        (Ok(built), BinFile::Prop(bin)) if declaration::same_bin(bin, &built) => {}
        (Ok(_), _) => tracing::warn!(
            "The YAML written for {input} does not build back to the same bin. Run `ritobin-tools diff -f summary` on the bin and the YAML to list the differences."
        ),
        (Err(error), _) => {
            tracing::warn!("The YAML written for {input} cannot be built back into a bin: {error}")
        }
    }
}

/// Converts one file. Returns `Skipped` if `--skip-existing` is set and the output file exists.
fn convert<'h>(
    job: &Job,
    args: &ConvertArgs,
    inputs: &Inputs,
    layout: TextLayout,
    hashes: &impl Fn() -> &'h BinHashes,
) -> Result<Outcome> {
    let to_stdout = job.output.as_str() == STDIO;
    if args.skip_existing && !to_stdout && job.output.exists() {
        tracing::info!(
            "Skipped {}: {} already exists",
            job.input,
            hyperlink_path(&job.output)
        );
        return Ok(Outcome::Skipped);
    }

    let document = inputs.read(&job.input)?;
    let to = job.to.unwrap_or(document.format.opposite());

    let empty = BinHashes::none();
    let hashes = match to.is_text() {
        true => hashes(),
        false => &empty,
    };
    let data = encode(&document.file, to, layout, hashes)
        .wrap_err_with(|| format!("Failed to convert {}", job.input))?;
    if to == Format::Yaml && document.format != Format::Yaml && !args.no_verify {
        verify_yaml(&document, &data, &job.input, inputs);
    }

    if (document.format, to) == (Format::Bin, Format::Rito)
        && !args.no_verify
        && !std::str::from_utf8(&data).is_ok_and(|text| reads_back(&document.file, text))
    {
        tracing::warn!(
            "The text printed for {} does not parse back to the same bin. The printer does not print every value exactly. Known cases are a string with a leading or trailing space, a NaN or infinite number, and a `hash` value of 8 bytes, which the text parser rejects. Run `ritobin-tools diff -f summary` on the bin and the text to list the differences.",
            job.input
        );
    }

    write_bytes(&job.output, &data)?;
    if !to_stdout {
        tracing::info!(
            "Converted {} -> {}",
            match game_bin(&job.input) {
                Some(_) => job.input.to_string(),
                None => hyperlink_path(&job.input),
            },
            hyperlink_path(&job.output)
        );
    }
    Ok(Outcome::Converted)
}

#[cfg(test)]
mod tests {
    use ltk_meta::{Bin, BinFile, BinObject, property::values};

    use super::*;
    use crate::document::{Document, to_bin};

    fn args(inputs: &[&Utf8Path]) -> ConvertArgs {
        ConvertArgs {
            inputs: inputs.iter().map(|input| input.to_path_buf()).collect(),
            input: Vec::new(),
            output: None,
            recursive: false,
            to: None,
            from: None,
            text_extension: DEFAULT_TEXT_EXTENSION.to_owned(),
            keep_hashed: true,
            lenient: false,
            skip_existing: false,
            no_verify: false,
            game: GameArgs::default(),
            layout: LayoutArgs::default(),
        }
    }

    fn sample() -> BinFile {
        Bin::builder()
            .object(
                BinObject::builder(0x1111_0001u32, 0xaaaa_0001u32)
                    .property(0x10u32, values::I32::new(42))
                    .build(),
            )
            .build()
            .into()
    }

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    fn write_sample(path: &Utf8Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, to_bin(&sample()).unwrap()).unwrap();
    }

    fn run_with(args: ConvertArgs) -> Result<()> {
        let layout = TextLayout::default();
        let none = BinHashes::none();
        let hashes = || &none;
        let ctx = Context::for_tests(None);
        let inputs = Inputs::new(
            &ctx,
            &args.game,
            ReadOptions {
                lenient: args.lenient,
            },
        );
        for job in plan(&args)? {
            convert(&job, &args, &inputs, layout, &hashes)?;
        }
        Ok(())
    }

    fn read(path: &Utf8Path) -> BinFile {
        Document::read(path, ReadOptions::default()).unwrap().file
    }

    #[test]
    fn plan_requires_output_for_game_input() {
        let input = Utf8Path::new("game:data/skin0.bin");

        let error = plan(&args(&[input])).unwrap_err();
        assert!(error.to_string().contains("has no default output path"));

        let jobs = plan(&ConvertArgs {
            output: Some("out/skin0.rito".into()),
            ..args(&[input])
        })
        .unwrap();
        assert_eq!(
            jobs,
            [Job {
                input: input.to_owned(),
                output: "out/skin0.rito".into(),
                to: Some(Format::Rito),
            }]
        );

        // Without a requested format, the output format is the other format than the game bin.
        let jobs = plan(&ConvertArgs {
            output: Some(STDIO.into()),
            ..args(&[input])
        })
        .unwrap();
        assert_eq!(jobs[0].to, None);
    }

    #[test]
    fn convert_reads_game_bin_and_writes_other_format() {
        use crate::game::testing::Installation;

        let installation = Installation::new();
        installation.archive(
            "A.wad.client",
            &[("data/skin0.bin", &to_bin(&sample()).unwrap())],
        );
        // The file extension is not a known format, so the output is the other format than the
        // game bin.
        let output = installation.root.join("out").join("skin0.txt");

        run_with(ConvertArgs {
            output: Some(output.clone()),
            game: GameArgs {
                game_dir: Some(installation.root.clone()),
                index_dir: Some(installation.root.join("index")),
            },
            ..args(&[Utf8Path::new("game:data/skin0.bin")])
        })
        .unwrap();
        assert!(std::fs::read(&output).unwrap().starts_with(b"#PROP_text"));
        assert_eq!(read(&output), sample());
    }

    #[test]
    fn bin_converts_to_text_and_back() {
        let (_guard, dir) = temp_dir();
        let bin = dir.join("skin0.bin");
        write_sample(&bin);

        run_with(args(&[&bin])).unwrap();
        let text = dir.join("skin0.rito");
        assert!(
            std::fs::read_to_string(&text)
                .unwrap()
                .starts_with("#PROP_text")
        );

        std::fs::remove_file(&bin).unwrap();
        run_with(args(&[&text])).unwrap();
        assert_eq!(read(&bin), sample());
    }

    #[test]
    fn input_format_is_detected_from_content() {
        let (_guard, dir) = temp_dir();
        let misnamed = dir.join("skin0.dat");
        write_sample(&misnamed);

        run_with(args(&[&misnamed])).unwrap();
        assert_eq!(read(&dir.join("skin0.rito")), sample());
    }

    #[test]
    fn output_extension_selects_format() {
        let (_guard, dir) = temp_dir();
        let bin = dir.join("skin0.bin");
        write_sample(&bin);

        let copy = dir.join("copy.bin");
        run_with(ConvertArgs {
            output: Some(copy.clone()),
            ..args(&[&bin])
        })
        .unwrap();
        assert_eq!(Format::detect(&std::fs::read(&copy).unwrap()), Format::Bin);
    }

    #[test]
    fn plan_fails_when_output_is_the_input() {
        let (_guard, dir) = temp_dir();
        let bin = dir.join("skin0.bin");
        write_sample(&bin);
        let before = std::fs::read(&bin).unwrap();

        let same_format = plan(&ConvertArgs {
            to: Some(Format::Bin),
            ..args(&[&bin])
        });
        assert!(
            same_format
                .unwrap_err()
                .to_string()
                .contains("is the input file itself")
        );

        // A text file with a `.bin` extension converts to a bin, and the default output path of
        // that conversion is the input path.
        let misnamed = dir.join("text.bin");
        run_with(ConvertArgs {
            output: Some(misnamed.clone()),
            to: Some(Format::Rito),
            ..args(&[&bin])
        })
        .unwrap();
        let text = std::fs::read(&misnamed).unwrap();
        assert!(run_with(args(&[&misnamed])).is_err());
        assert_eq!(std::fs::read(&misnamed).unwrap(), text);

        let extension = plan(&ConvertArgs {
            text_extension: "bin".to_owned(),
            ..args(&[&bin])
        });
        assert!(
            extension
                .unwrap_err()
                .to_string()
                .contains("is the input file itself")
        );
        assert_eq!(std::fs::read(&bin).unwrap(), before);
    }

    #[test]
    fn plan_fails_when_output_is_another_input() {
        let (_guard, dir) = temp_dir();
        let bin = dir.join("skin0.bin");
        write_sample(&bin);
        run_with(args(&[&bin])).unwrap();
        let text = dir.join("skin0.rito");
        let edited = std::fs::read_to_string(&text)
            .unwrap()
            .replace("= 42", "= 43");
        std::fs::write(&text, &edited).unwrap();

        let error = run_with(args(&[&bin, &text])).unwrap_err();
        assert!(error.to_string().contains("which is also an input"));
        assert_eq!(std::fs::read_to_string(&text).unwrap(), edited);
        assert_eq!(read(&bin), sample());
    }

    #[test]
    fn plan_fails_when_two_inputs_share_an_output() {
        let (_guard, dir) = temp_dir();
        std::fs::write(dir.join("skin0.rito"), "").unwrap();
        std::fs::write(dir.join("skin0.py"), "#PROP_text\n").unwrap();

        let error = plan(&ConvertArgs {
            to: Some(Format::Bin),
            ..args(&[&dir])
        })
        .unwrap_err();
        assert!(error.to_string().contains("have the same output path"));
    }

    #[test]
    fn scan_includes_py_file_only_with_text_header() {
        let (_guard, dir) = temp_dir();
        std::fs::write(dir.join("legacy.py"), "\u{feff}#PROP_text\n").unwrap();
        std::fs::write(dir.join("patch.PY"), "#PTCH_text\n").unwrap();
        std::fs::write(dir.join("script.py"), "print('hi')\n").unwrap();
        std::fs::write(dir.join("__init__.py"), "").unwrap();

        let jobs = plan(&ConvertArgs {
            to: Some(Format::Bin),
            ..args(&[&dir])
        })
        .unwrap();
        let inputs: Vec<&str> = jobs
            .iter()
            .filter_map(|job| job.input.file_name())
            .collect();
        assert_eq!(inputs, ["legacy.py", "patch.PY"]);
    }

    #[test]
    fn scan_selects_one_format_and_honors_recursive() {
        let (_guard, dir) = temp_dir();
        write_sample(&dir.join("a.bin"));
        write_sample(&dir.join("nested/b.bin"));
        std::fs::write(dir.join("notes.rito"), "").unwrap();

        let flat = plan(&args(&[&dir])).unwrap();
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].input, dir.join("a.bin"));
        assert_eq!(flat[0].output, dir.join("a.rito"));
        assert_eq!(flat[0].to, Some(Format::Rito));

        let deep = plan(&ConvertArgs {
            recursive: true,
            ..args(&[&dir])
        })
        .unwrap();
        assert_eq!(deep.len(), 2);
    }

    #[test]
    fn directory_output_keeps_relative_paths() {
        let (_guard, dir) = temp_dir();
        let source = dir.join("src");
        write_sample(&source.join("nested/b.bin"));

        let out = dir.join("out");
        run_with(ConvertArgs {
            recursive: true,
            output: Some(out.clone()),
            ..args(&[&source])
        })
        .unwrap();
        assert_eq!(read(&out.join("nested/b.rito")), sample());
    }

    #[test]
    fn scan_fails_without_matching_files() {
        let (_guard, dir) = temp_dir();
        std::fs::write(dir.join("notes.rito"), "").unwrap();

        let error = plan(&args(&[&dir])).unwrap_err();
        assert!(error.to_string().contains("Pass --from rito"));
    }

    #[test]
    fn text_extension_accepts_leading_dot() {
        let (_guard, dir) = temp_dir();
        let bin = dir.join("skin0.bin");
        write_sample(&bin);

        run_with(ConvertArgs {
            text_extension: ".py".to_owned(),
            ..args(&[&bin])
        })
        .unwrap();
        assert_eq!(read(&dir.join("skin0.py")), sample());
    }

    #[test]
    fn skip_existing_does_not_overwrite_output() {
        let (_guard, dir) = temp_dir();
        let bin = dir.join("skin0.bin");
        write_sample(&bin);
        let text = dir.join("skin0.rito");
        std::fs::write(&text, "mine").unwrap();

        run_with(ConvertArgs {
            skip_existing: true,
            ..args(&[&bin])
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&text).unwrap(), "mine");
    }

    #[test]
    fn output_with_several_inputs_fails() {
        let (_guard, dir) = temp_dir();
        let (a, b) = (dir.join("a.bin"), dir.join("b.bin"));
        write_sample(&a);
        write_sample(&b);

        let error = plan(&ConvertArgs {
            output: Some(dir.join("out.rito")),
            ..args(&[&a, &b])
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("--output requires exactly one input")
        );
    }

    #[test]
    fn same_file_key_is_equal_for_equivalent_paths() {
        let (_guard, dir) = temp_dir();
        let plain = dir.join("skin0.bin");
        let dotted = dir.join(".").join("skin0.bin");
        assert_eq!(same_file_key(&plain), same_file_key(&dotted));
        assert_ne!(same_file_key(&plain), same_file_key(&dir.join("skin1.bin")));
    }
}
