use std::{cell::OnceCell, collections::HashMap};

use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use miette::{IntoDiagnostic, Result, WrapErr};
use walkdir::WalkDir;

use crate::{
    cli::LayoutArgs,
    context::Context,
    document::{
        DEFAULT_TEXT_EXTENSION, Document, Format, ReadOptions, STDIO, TEXT_EXTENSIONS, TextLayout,
        converted_path, detect_file, encode, has_text_header, reads_back, write_bytes,
    },
    hashes::BinHashes,
    utils::{hyperlink_path, plural},
};

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// Files or directories to convert. `-` reads standard input
    #[arg(value_name = "INPUTS")]
    pub inputs: Vec<Utf8PathBuf>,

    /// Files or directories to convert, as a flag
    #[arg(short, long = "input", value_name = "PATH", num_args = 1..)]
    pub input: Vec<Utf8PathBuf>,

    /// Where to write the result: a file for a file input, a directory for a directory input,
    /// `-` for standard output. Defaults to a file next to each input
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<Utf8PathBuf>,

    /// Convert the files in subdirectories of a directory input too
    #[arg(short, long)]
    pub recursive: bool,

    /// The format to convert to. Defaults to the opposite of each input, or to the format the
    /// output extension names
    #[arg(short, long, value_enum, value_name = "FORMAT")]
    pub to: Option<Format>,

    /// The format of the files a directory input is scanned for. Defaults to the opposite of
    /// `--to`, or to `bin`
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub from: Option<Format>,

    /// The extension given to text output
    #[arg(long = "ext", value_name = "EXT", default_value = DEFAULT_TEXT_EXTENSION)]
    pub text_extension: String,

    /// Leave hashes as hex instead of naming them from the hashtables
    #[arg(short, long)]
    pub keep_hashed: bool,

    /// Convert text that has problems, leaving out what cannot be read
    #[arg(long)]
    pub lenient: bool,

    /// Leave an output file that already exists as it is
    #[arg(long)]
    pub skip_existing: bool,

    /// Do not check that text printed from a bin reads back as the same bin
    #[arg(long)]
    pub no_verify: bool,

    #[command(flatten)]
    pub layout: LayoutArgs,
}

impl ConvertArgs {
    /// The extension for text output, without the dot it may have been given with.
    fn text_extension(&self) -> &str {
        self.text_extension.trim_start_matches('.')
    }

    /// The format the flags or the extension of `output` ask for, if either does.
    fn asked_format(&self, output: Option<&Utf8Path>) -> Option<Format> {
        self.to.or_else(|| {
            output
                .and_then(|output| output.extension())
                .and_then(Format::from_extension)
        })
    }
}

/// One file to convert, and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Job {
    input: Utf8PathBuf,
    output: Utf8PathBuf,
    /// The format to write. `None` is the opposite of the input, for standard input, whose format
    /// is not known until it is read.
    to: Option<Format>,
}

enum Outcome {
    Converted,
    Skipped,
}

pub fn run(ctx: &Context, args: ConvertArgs) -> Result<()> {
    let jobs = plan(&args)?;
    let layout = args.layout.over(ctx.config.print_config);
    // The tables are opened when the first text output needs them.
    let tables = OnceCell::new();
    let hashes = || {
        tables.get_or_init(|| match args.keep_hashed {
            true => BinHashes::none(),
            false => ctx.hashes(),
        })
    };

    if let [job] = jobs.as_slice() {
        return convert(job, &args, layout, &hashes).map(|_| ());
    }

    let (mut converted, mut skipped, mut failed) = (0, 0, 0);
    for job in &jobs {
        match convert(job, &args, layout, &hashes) {
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

/// The files `args` asks for, each with where it goes. Nothing is written when one of them would
/// be written over an input or over another output.
fn plan(args: &ConvertArgs) -> Result<Vec<Job>> {
    let inputs: Vec<&Utf8PathBuf> = args.inputs.iter().chain(&args.input).collect();
    if inputs.is_empty() {
        miette::bail!("No input given. Pass files or directories to convert");
    }
    if args.output.is_some() && inputs.len() > 1 {
        miette::bail!(
            "--output needs a single input, and {} were given",
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

/// Refuses a run in which a file would be written over an input, or two files over each other.
fn check_overwrites(jobs: &[Job]) -> Result<()> {
    let inputs: HashMap<String, &Utf8Path> = jobs
        .iter()
        .filter(|job| job.input.as_str() != STDIO)
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
                "Converting {} would write it over itself. Pass --output to write it somewhere else",
                job.input
            ),
            Some(input) => miette::bail!(
                "Converting {} would write over {input}, which is also an input. Convert them one at a time",
                job.input
            ),
            None => {}
        }
        if let Some(other) = outputs.insert(key, &job.input) {
            miette::bail!(
                "{other} and {} would both be written to {}. Convert them one at a time",
                job.input,
                job.output
            );
        }
    }
    Ok(())
}

/// A text that is equal for two paths to the same file: the absolute path, with one kind of
/// separator, and in one case where the file system ignores case.
fn same_file_key(path: &Utf8Path) -> String {
    let absolute = std::path::absolute(path)
        .map(|absolute| absolute.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    let key = absolute.replace('\\', "/");
    match cfg!(windows) {
        true => key.to_lowercase(),
        false => key,
    }
}

/// The files of one format under `dir`, sorted, each going next to itself or to the same place
/// under the output directory.
fn scan(dir: &Utf8Path, args: &ConvertArgs) -> Result<Vec<Job>> {
    let from = args
        .from
        .or(args.to.map(Format::opposite))
        .unwrap_or(Format::Bin);
    let to = args.to.unwrap_or(from.opposite());
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
        if path.extension().and_then(Format::from_extension) != Some(from) {
            continue;
        }
        // `.py` is also the extension of Python source. As in the C++ ritobin, a `.py` file is
        // ritobin text when it starts with the ritobin header. One that cannot be read is kept,
        // for the conversion to report.
        let python = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("py"));
        if python && !has_text_header(path).unwrap_or(true) {
            tracing::debug!("Skipped {path}: it does not start as ritobin text");
            continue;
        }
        files.push(path.to_owned());
    }

    if files.is_empty() {
        let wanted = match from {
            Format::Bin => ".bin".to_owned(),
            Format::Rito => TEXT_EXTENSIONS
                .iter()
                .map(|extension| format!(".{extension}"))
                .collect::<Vec<_>>()
                .join(", "),
        };
        let recursion = match args.recursive {
            true => "",
            false => " (pass --recursive to look in subdirectories)",
        };
        miette::bail!(
            "No {wanted} files in {dir}{recursion}. Pass --from {} to convert the other way",
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

fn convert<'h>(
    job: &Job,
    args: &ConvertArgs,
    layout: TextLayout,
    hashes: &impl Fn() -> &'h BinHashes,
) -> Result<Outcome> {
    let to_stdout = job.output.as_str() == STDIO;
    if args.skip_existing && !to_stdout && job.output.exists() {
        tracing::info!(
            "Skipped {}: {} exists",
            job.input,
            hyperlink_path(&job.output)
        );
        return Ok(Outcome::Skipped);
    }

    let document = Document::read(
        &job.input,
        ReadOptions {
            lenient: args.lenient,
        },
    )?;
    let to = job.to.unwrap_or(document.format.opposite());

    let empty = BinHashes::none();
    let hashes = match to {
        Format::Rito => hashes(),
        Format::Bin => &empty,
    };
    let data = encode(&document.file, to, layout, hashes)
        .wrap_err_with(|| format!("Failed to convert {}", job.input))?;

    if (document.format, to) == (Format::Bin, Format::Rito)
        && !args.no_verify
        && !std::str::from_utf8(&data).is_ok_and(|text| reads_back(&document.file, text))
    {
        tracing::warn!(
            "The text of {} does not read back as the same bin. The printer does not keep every value as it is: a string that starts or ends with a space and a NaN or infinite number are known cases. `ritobin-tools diff -f summary` on the bin and the text shows where.",
            job.input
        );
    }

    write_bytes(&job.output, &data)?;
    if !to_stdout {
        tracing::info!(
            "Converted {} -> {}",
            hyperlink_path(&job.input),
            hyperlink_path(&job.output)
        );
    }
    Ok(Outcome::Converted)
}

#[cfg(test)]
mod tests {
    use ltk_meta::{Bin, BinFile, BinObject, property::values};

    use super::*;
    use crate::document::to_bin;

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
        for job in plan(&args)? {
            convert(&job, &args, layout, &hashes)?;
        }
        Ok(())
    }

    fn read(path: &Utf8Path) -> BinFile {
        Document::read(path, ReadOptions::default()).unwrap().file
    }

    #[test]
    fn a_bin_converts_to_text_next_to_it_and_back() {
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
    fn the_format_is_told_by_content_and_not_by_extension() {
        let (_guard, dir) = temp_dir();
        let misnamed = dir.join("skin0.dat");
        write_sample(&misnamed);

        run_with(args(&[&misnamed])).unwrap();
        assert_eq!(read(&dir.join("skin0.rito")), sample());
    }

    #[test]
    fn the_output_extension_names_the_format() {
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
    fn a_file_is_never_written_over_itself() {
        let (_guard, dir) = temp_dir();
        let bin = dir.join("skin0.bin");
        write_sample(&bin);
        let before = std::fs::read(&bin).unwrap();

        let same_format = plan(&ConvertArgs {
            to: Some(Format::Bin),
            ..args(&[&bin])
        });
        assert!(same_format.unwrap_err().to_string().contains("over itself"));

        // Text under a `.bin` name converts to a bin, which would have the same name.
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
        assert!(extension.unwrap_err().to_string().contains("over itself"));
        assert_eq!(std::fs::read(&bin).unwrap(), before);
    }

    #[test]
    fn a_bin_and_its_text_are_not_converted_over_each_other() {
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
    fn two_inputs_are_not_written_to_one_output() {
        let (_guard, dir) = temp_dir();
        std::fs::write(dir.join("skin0.rito"), "").unwrap();
        std::fs::write(dir.join("skin0.py"), "#PROP_text\n").unwrap();

        let error = plan(&ConvertArgs {
            to: Some(Format::Bin),
            ..args(&[&dir])
        })
        .unwrap_err();
        assert!(error.to_string().contains("would both be written to"));
    }

    #[test]
    fn a_directory_scan_takes_a_py_file_only_when_it_starts_as_ritobin_text() {
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
    fn a_directory_scan_takes_one_format_and_honors_recursive() {
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
    fn a_directory_output_mirrors_the_input_tree() {
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
    fn a_directory_without_matching_files_is_an_error() {
        let (_guard, dir) = temp_dir();
        std::fs::write(dir.join("notes.rito"), "").unwrap();

        let error = plan(&args(&[&dir])).unwrap_err();
        assert!(error.to_string().contains("Pass --from rito"));
    }

    #[test]
    fn the_text_extension_is_used_with_or_without_its_dot() {
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
    fn skip_existing_leaves_the_output_alone() {
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
    fn output_with_several_inputs_is_rejected() {
        let (_guard, dir) = temp_dir();
        let (a, b) = (dir.join("a.bin"), dir.join("b.bin"));
        write_sample(&a);
        write_sample(&b);

        let error = plan(&ConvertArgs {
            output: Some(dir.join("out.rito")),
            ..args(&[&a, &b])
        })
        .unwrap_err();
        assert!(error.to_string().contains("--output needs a single input"));
    }

    #[test]
    fn two_spellings_of_one_path_are_the_same_file() {
        let (_guard, dir) = temp_dir();
        let plain = dir.join("skin0.bin");
        let dotted = dir.join(".").join("skin0.bin");
        assert_eq!(same_file_key(&plain), same_file_key(&dotted));
        assert_ne!(same_file_key(&plain), same_file_key(&dir.join("skin1.bin")));
    }
}
