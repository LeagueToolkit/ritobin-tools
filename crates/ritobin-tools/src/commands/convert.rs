use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Write};

use camino::{Utf8Path, Utf8PathBuf};
use colored::Colorize;
use ltk_meta::Bin;
use ltk_ritobin::{Cst, HashMapProvider, Print, print::PrintConfig};
use miette::{IntoDiagnostic, Result, WrapErr, miette};
use walkdir::WalkDir;

use crate::utils::hyperlink_path;
use crate::{Context, utils::config::load_or_create_config};

/// Supported file extensions for conversion
const SUPPORTED_EXTENSIONS: &[&str] = &["bin", "py", "ritobin", "rito"];

/// Convert between .bin (binary) and .py/.ritobin (text) formats.
///
/// - .bin -> .rito: Converts binary bin file to ritobin text format
/// - .py/.rito/.ritobin -> .bin: Parses ritobin text and converts to binary format
///
/// If input is a directory:
/// - With recursive=true: converts all matching files in subdirectories
/// - With recursive=false: converts only files in the immediate directory
pub fn convert(ctx: Context, input: String, output: Option<String>, recursive: bool) -> Result<()> {
    let input_path = Utf8Path::new(&input);

    if input_path.is_dir() {
        convert_directory(ctx, input_path, recursive)
    } else {
        convert_file(&ctx, input_path, output.map(Utf8PathBuf::from))
    }
}

/// Convert all matching files in a directory
fn convert_directory(ctx: Context, dir_path: &Utf8Path, recursive: bool) -> Result<()> {
    let walker = if recursive {
        WalkDir::new(dir_path)
    } else {
        WalkDir::new(dir_path).max_depth(1)
    };

    let mut converted_count = 0;
    let mut error_count = 0;

    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        let Some(path) = Utf8Path::from_path(entry.path()) else {
            tracing::warn!("Skipping non-UTF8 path: {}", entry.path().display());
            continue;
        };

        if path.is_dir() {
            continue;
        }

        let extension = path.extension().unwrap_or("");

        if !SUPPORTED_EXTENSIONS.contains(&extension) {
            continue;
        }

        match convert_file(&ctx, path, None) {
            Ok(()) => converted_count += 1,
            Err(e) => {
                tracing::error!("Failed to convert {}: {}", path, e);
                error_count += 1;
            }
        }
    }

    tracing::info!(
        "Conversion complete: {} files converted, {} errors",
        converted_count,
        error_count
    );

    if error_count > 0 {
        Err(miette::miette!("{} file(s) failed to convert", error_count))
    } else {
        Ok(())
    }
}

pub fn convert_file(
    ctx: &Context,
    input_path: &Utf8Path,
    output: Option<Utf8PathBuf>,
) -> Result<()> {
    let extension = input_path.extension().unwrap_or("");

    match extension {
        "bin" => convert_bin_to_ritobin(ctx, input_path, output),
        "py" | "rito" | "ritobin" => convert_ritobin_to_bin(input_path, output),
        _ => Err(miette::miette!(
            "Unsupported input file extension: .{}. Supported extensions: .bin, .py, .rito, .ritobin",
            extension
        )),
    }
}

fn convert_bin_to_ritobin(
    ctx: &Context,
    input_path: &Utf8Path,
    output: Option<Utf8PathBuf>,
) -> Result<()> {
    let file = File::open(input_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to open input file: {}", input_path))?;
    let mut reader = BufReader::new(file);

    let tree = Bin::from_reader(&mut reader)
        .into_diagnostic()
        .wrap_err("Failed to parse .bin file")?;

    let ritobin_text = tree
        .print_with_config(
            ctx.config
                .print_config
                .with_hashes(ctx.hash_provider.clone()),
        )
        .into_diagnostic()
        .wrap_err("Failed to convert to ritobin format")?;

    let output_path = output.unwrap_or_else(|| {
        let stem = input_path.file_stem().unwrap_or("output");
        let parent = input_path.parent().unwrap_or(Utf8Path::new("."));
        parent.join(format!("{}.rito", stem))
    });

    let output_file = File::create(&output_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to create output file: {}", output_path))?;
    let mut writer = BufWriter::new(output_file);

    writer
        .write_all(ritobin_text.as_bytes())
        .into_diagnostic()
        .wrap_err("Failed to write output file")?;

    println!(
        "{} {} {} {}",
        "Converted".green(),
        hyperlink_path(input_path),
        "->".green(),
        hyperlink_path(&output_path)
    );

    Ok(())
}

fn convert_ritobin_to_bin(input_path: &Utf8Path, output: Option<Utf8PathBuf>) -> Result<()> {
    let mut file = File::open(input_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to open input file: {}", input_path))?;

    let mut ritobin_text = String::new();
    file.read_to_string(&mut ritobin_text)
        .into_diagnostic()
        .wrap_err("Failed to read ritobin file")?;

    let cst = Cst::parse(&ritobin_text);
    let (bin, bin_errs) = cst.build_bin(&ritobin_text);
    if !bin_errs.is_empty() {
        return Err(miette!("Could not build bin"));
    }

    let output_path = output.unwrap_or_else(|| {
        let stem = input_path.file_stem().unwrap_or("output");
        let parent = input_path.parent().unwrap_or(Utf8Path::new("."));
        parent.join(format!("{}.bin", stem))
    });

    let mut cursor = Cursor::new(Vec::new());
    bin.to_writer(&mut cursor)
        .into_diagnostic()
        .wrap_err("Failed to convert to binary format")?;

    let output_file = File::create(&output_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to create output file: {}", output_path))?;
    let mut writer = BufWriter::new(output_file);

    writer
        .write_all(cursor.get_ref())
        .into_diagnostic()
        .wrap_err("Failed to write output file")?;

    tracing::info!(
        "Converted {} -> {}",
        hyperlink_path(input_path),
        hyperlink_path(&output_path)
    );

    Ok(())
}
