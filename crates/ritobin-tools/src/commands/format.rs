use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use miette::{IntoDiagnostic, Result, WrapErr};
use walkdir::WalkDir;

use crate::{
    cli::LayoutArgs,
    context::Context,
    document::{Format, STDIO, TextLayout, format_text, read_bytes, scanned_format, write_bytes},
    utils::{hyperlink_path, plural},
};

#[derive(Args, Debug)]
pub struct FormatArgs {
    /// Ritobin text files or directories to format. A file is rewritten in place. `-` reads
    /// standard input and writes standard output
    #[arg(value_name = "PATHS", required = true)]
    pub paths: Vec<Utf8PathBuf>,

    /// Write the formatted text to this file and keep the input file unchanged. Requires
    /// exactly one input file. `-` writes standard output
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<Utf8PathBuf>,

    /// Include the subdirectories of a directory
    #[arg(short, long)]
    pub recursive: bool,

    /// Write no file. Print the path of each file that is not formatted, and exit with 1 if
    /// there is one
    #[arg(long, conflicts_with = "output")]
    pub check: bool,

    #[command(flatten)]
    pub layout: LayoutArgs,
}

/// The result of one file that did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The text already has the formatted layout.
    Unchanged,
    /// The text differs from the formatted layout. The file was rewritten, unless `--check` is
    /// set.
    Changed,
}

/// Runs the `format` command. Returns `false` if `--check` is set and a file is not formatted.
///
/// With several files, a file that fails does not stop the run. The command fails after the
/// last file if any file failed.
pub fn run(ctx: &Context, args: FormatArgs) -> Result<bool> {
    let files = files(&args)?;
    let layout = args.layout.over(ctx.config.print_config);

    if let [file] = files.as_slice() {
        let outcome = format(file, &args, layout)?;
        return Ok(!(args.check && outcome == Outcome::Changed));
    }

    let (mut changed, mut unchanged, mut failed) = (0, 0, 0);
    for file in &files {
        match format(file, &args, layout) {
            Ok(Outcome::Changed) => changed += 1,
            Ok(Outcome::Unchanged) => unchanged += 1,
            Err(error) => {
                failed += 1;
                eprintln!("{error:?}");
            }
        }
    }

    let verb = match args.check {
        true => "not formatted",
        false => "formatted",
    };
    tracing::info!(
        "{} {verb}, {unchanged} unchanged, {failed} failed",
        plural(changed, "file")
    );
    if failed > 0 {
        miette::bail!(
            "{failed} of {} failed to format",
            plural(files.len(), "file")
        );
    }
    Ok(!(args.check && changed > 0))
}

/// Returns the files that `args` selects. A directory is replaced by its ritobin text files,
/// sorted by path.
///
/// Fails if a path does not exist, if a directory has no ritobin text file, or if `--output` is
/// set and the selection is not exactly one file.
fn files(args: &FormatArgs) -> Result<Vec<Utf8PathBuf>> {
    let mut files = Vec::new();
    for path in &args.paths {
        if path.as_str() == STDIO || path.is_file() {
            files.push(path.clone());
        } else if path.is_dir() {
            let walker = match args.recursive {
                true => WalkDir::new(path),
                false => WalkDir::new(path).max_depth(1),
            };
            let before = files.len();
            for entry in walker.sort_by_file_name() {
                let entry = entry
                    .into_diagnostic()
                    .wrap_err_with(|| format!("Failed to read directory {path}"))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let Some(file) = Utf8Path::from_path(entry.path()) else {
                    tracing::warn!("Skipping non-UTF-8 path: {}", entry.path().display());
                    continue;
                };
                if scanned_format(file) == Some(Format::Rito) {
                    files.push(file.to_owned());
                }
            }
            if files.len() == before {
                let recursion = match args.recursive {
                    true => "",
                    false => " (pass --recursive to include subdirectories)",
                };
                miette::bail!("No ritobin text files were found in {path}{recursion}");
            }
        } else {
            miette::bail!("Input does not exist: {path}");
        }
    }

    if args.output.is_some() && files.len() != 1 {
        miette::bail!(
            "--output requires exactly one input file, but {} were selected",
            files.len()
        );
    }
    Ok(files)
}

/// Formats one file. Writes the formatted text to `--output`, to standard output for standard
/// input, or to the file itself if the text changed. Writes nothing if `--check` is set.
///
/// Fails if the file is a binary bin or is not valid ritobin text.
fn format(file: &Utf8Path, args: &FormatArgs, layout: TextLayout) -> Result<Outcome> {
    let data = read_bytes(file)?;
    if Format::detect(&data) == Format::Bin {
        miette::bail!(
            "{file} is a binary bin. `format` formats ritobin text. Use `ritobin-tools convert` to print a bin as text"
        );
    }
    let text =
        String::from_utf8(data).map_err(|_| miette::miette!("{file} is not UTF-8 ritobin text"))?;
    // A byte order mark is kept out of the parsed text and is not written back.
    let source = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let formatted = format_text(file.as_str(), source, layout)?;
    let outcome = match formatted == text {
        true => Outcome::Unchanged,
        false => Outcome::Changed,
    };

    if args.check {
        if outcome == Outcome::Changed {
            write_bytes(STDIO.into(), format!("{file}\n").as_bytes())?;
        }
        return Ok(outcome);
    }

    let from_stdin = file.as_str() == STDIO;
    match (&args.output, from_stdin) {
        (Some(output), _) => {
            write_bytes(output, formatted.as_bytes())?;
            if output.as_str() != STDIO {
                tracing::info!("Formatted {file} -> {}", hyperlink_path(output));
            }
        }
        (None, true) => write_bytes(STDIO.into(), formatted.as_bytes())?,
        (None, false) => match outcome {
            Outcome::Changed => {
                write_bytes(file, formatted.as_bytes())?;
                tracing::info!("Formatted {}", hyperlink_path(file));
            }
            Outcome::Unchanged => tracing::debug!("{file} is already formatted"),
        },
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNFORMATTED: &str = "#PROP_text\n# The skin of the test champion\ntype: string = \"PROP\"\nversion: u32 = 3\nlinked: list[string] = {\n}\nentries: map[hash, embed] = {\n  \"Characters/Test/Skins/Skin0\" = SkinCharacterDataProperties {\n      Size: f32 = 1 # scale of the model\n      # The tags are read by the audio system\n      Tags: list[string] = { \"a\"\n        \"b\" }\n  }\n}\n";

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    fn args(paths: &[&Utf8Path]) -> FormatArgs {
        FormatArgs {
            paths: paths.iter().map(|path| path.to_path_buf()).collect(),
            output: None,
            recursive: false,
            check: false,
            layout: LayoutArgs::default(),
        }
    }

    fn context() -> Context {
        Context::for_tests(None)
    }

    #[test]
    fn format_text_keeps_comments_and_content() {
        let formatted = format_text("skin0.rito", UNFORMATTED, TextLayout::default()).unwrap();
        for comment in [
            "# The skin of the test champion",
            "# scale of the model",
            "# The tags are read by the audio system",
        ] {
            assert!(formatted.contains(comment), "{formatted}");
        }
        assert!(
            formatted.contains("\n    \"Characters/Test/Skins/Skin0\" = "),
            "{formatted}"
        );
        assert!(formatted.contains("\n        Size: f32 = 1"), "{formatted}");

        // Formatting the formatted text changes nothing.
        let again = format_text("skin0.rito", &formatted, TextLayout::default()).unwrap();
        assert_eq!(again, formatted);
    }

    #[test]
    fn format_text_fails_for_invalid_text() {
        let broken = UNFORMATTED.replace("Size: f32 = 1", "Size: f32 = \"big\"");
        assert!(format_text("skin0.rito", &broken, TextLayout::default()).is_err());
        assert!(format_text("skin0.rito", "entries: {", TextLayout::default()).is_err());
    }

    #[test]
    fn run_rewrites_file_in_place_and_leaves_formatted_file_unchanged() {
        let (_guard, dir) = temp_dir();
        let file = dir.join("skin0.rito");
        std::fs::write(&file, UNFORMATTED).unwrap();

        assert!(run(&context(), args(&[&file])).unwrap());
        let formatted = std::fs::read_to_string(&file).unwrap();
        assert_ne!(formatted, UNFORMATTED);
        assert!(formatted.contains("# scale of the model"));

        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        assert!(run(&context(), args(&[&file])).unwrap());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), formatted);
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            modified
        );
    }

    #[test]
    fn check_returns_false_for_unformatted_file_and_writes_nothing() {
        let (_guard, dir) = temp_dir();
        let file = dir.join("skin0.rito");
        std::fs::write(&file, UNFORMATTED).unwrap();

        let check = || {
            run(
                &context(),
                FormatArgs {
                    check: true,
                    ..args(&[&file])
                },
            )
            .unwrap()
        };
        assert!(!check());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), UNFORMATTED);

        run(&context(), args(&[&file])).unwrap();
        assert!(check());
    }

    #[test]
    fn output_writes_other_file_and_keeps_input() {
        let (_guard, dir) = temp_dir();
        let (file, output) = (dir.join("skin0.rito"), dir.join("out").join("skin0.rito"));
        std::fs::write(&file, UNFORMATTED).unwrap();

        run(
            &context(),
            FormatArgs {
                output: Some(output.clone()),
                ..args(&[&file])
            },
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), UNFORMATTED);
        assert!(
            std::fs::read_to_string(&output)
                .unwrap()
                .contains("# scale of the model")
        );
    }

    #[test]
    fn files_scan_directory_for_text_files_and_reject_missing_path() {
        let (_guard, dir) = temp_dir();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.rito"), UNFORMATTED).unwrap();
        std::fs::write(dir.join("a.ritobin"), UNFORMATTED).unwrap();
        std::fs::write(dir.join("skin0.bin"), b"PROP").unwrap();
        std::fs::write(dir.join("sub").join("c.rito"), UNFORMATTED).unwrap();

        assert_eq!(
            files(&args(&[&dir])).unwrap(),
            [dir.join("a.ritobin"), dir.join("b.rito")]
        );
        let recursive = files(&FormatArgs {
            recursive: true,
            ..args(&[&dir])
        })
        .unwrap();
        assert_eq!(recursive.len(), 3);

        let error = files(&args(&[&dir.join("missing.rito")])).unwrap_err();
        assert!(error.to_string().contains("Input does not exist"));
        let error = files(&FormatArgs {
            output: Some(dir.join("out.rito")),
            ..args(&[&dir])
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requires exactly one input file")
        );
    }

    #[test]
    fn format_fails_for_binary_bin() {
        let (_guard, dir) = temp_dir();
        let file = dir.join("skin0.bin");
        std::fs::write(&file, b"PROP\x03\x00\x00\x00").unwrap();
        let error = run(&context(), args(&[&file])).unwrap_err();
        assert!(error.to_string().contains("is a binary bin"));
    }
}
