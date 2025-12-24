use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Write};

use camino::{Utf8Path, Utf8PathBuf};
use ltk_meta::BinTree;
use ltk_ritobin::{HashMapProvider, HexHashProvider, WriterConfig};
use miette::{IntoDiagnostic, Result, WrapErr};
use walkdir::WalkDir;

use crate::utils::config::load_or_create_config;
use crate::utils::hyperlink_path;

/// Supported file extensions for conversion
const SUPPORTED_EXTENSIONS: &[&str] = &["bin", "py", "ritobin"];

/// Convert between .bin (binary) and .py/.ritobin (text) formats.
///
/// - .bin -> .py: Converts binary bin file to ritobin text format
/// - .py/.ritobin -> .bin: Parses ritobin text and converts to binary format
///
/// If input is a directory:
/// - With recursive=true: converts all matching files in subdirectories
/// - With recursive=false: converts only files in the immediate directory
pub fn convert(input: String, output: Option<String>, recursive: bool) -> Result<()> {
    let input_path = Utf8Path::new(&input);

    if input_path.is_dir() {
        convert_directory(input_path, recursive)
    } else {
        convert_file(input_path, output.map(Utf8PathBuf::from))
    }
}

/// Convert all matching files in a directory
fn convert_directory(dir_path: &Utf8Path, recursive: bool) -> Result<()> {
    let walker = if recursive {
        WalkDir::new(dir_path)
    } else {
        WalkDir::new(dir_path).max_depth(1)
    };

    let mut converted_count = 0;
    let mut error_count = 0;

    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        // Convert to Utf8Path, skip non-UTF8 paths
        let Some(path) = Utf8Path::from_path(entry.path()) else {
            tracing::warn!("Skipping non-UTF8 path: {}", entry.path().display());
            continue;
        };

        // Skip directories
        if path.is_dir() {
            continue;
        }

        // Check if file has a supported extension
        let extension = path.extension().unwrap_or("");

        if !SUPPORTED_EXTENSIONS.contains(&extension) {
            continue;
        }

        // Convert the file
        match convert_file(path, None) {
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

/// Convert a single file based on its extension
fn convert_file(input_path: &Utf8Path, output: Option<Utf8PathBuf>) -> Result<()> {
    let extension = input_path.extension().unwrap_or("");

    match extension {
        "bin" => convert_bin_to_ritobin(input_path, output),
        "py" | "ritobin" => convert_ritobin_to_bin(input_path, output),
        _ => Err(miette::miette!(
            "Unsupported input file extension: .{}. Supported extensions: .bin, .py, .ritobin",
            extension
        )),
    }
}

/// Convert a .bin file to ritobin text format (.py)
fn convert_bin_to_ritobin(input_path: &Utf8Path, output: Option<Utf8PathBuf>) -> Result<()> {
    let (config, _) = load_or_create_config()?;

    // Load the .bin file
    let file = File::open(input_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to open input file: {}", input_path))?;
    let mut reader = BufReader::new(file);

    let tree = BinTree::from_reader(&mut reader)
        .into_diagnostic()
        .wrap_err("Failed to parse .bin file")?;

    // Convert to ritobin text format using hashtable provider if available,
    // otherwise fall back to hex hash provider
    let ritobin_text = if let Some(hashtable_dir) = config.hashtable_dir.as_ref() {
        let mut hashtable_provider = HashMapProvider::new();
        hashtable_provider.load_from_directory(hashtable_dir);

        ltk_ritobin::write_with_config_and_hashes(
            &tree,
            WriterConfig::default(),
            &hashtable_provider,
        )
    } else {
        ltk_ritobin::write_with_config_and_hashes(&tree, WriterConfig::default(), &HexHashProvider)
    }
    .into_diagnostic()
    .wrap_err("Failed to convert to ritobin format")?;

    // Determine output path
    let output_path = output.unwrap_or_else(|| {
        // Replace .bin extension with .py (ritobin text format)
        let stem = input_path.file_stem().unwrap_or("output");
        let parent = input_path.parent().unwrap_or(Utf8Path::new("."));
        parent.join(format!("{}.py", stem))
    });

    // Write output file
    let output_file = File::create(&output_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to create output file: {}", output_path))?;
    let mut writer = BufWriter::new(output_file);

    writer
        .write_all(ritobin_text.as_bytes())
        .into_diagnostic()
        .wrap_err("Failed to write output file")?;

    tracing::info!(
        "Converted {} -> {}",
        hyperlink_path(input_path),
        hyperlink_path(&output_path)
    );

    Ok(())
}

/// Convert a ritobin text file (.py/.ritobin) to binary .bin format
fn convert_ritobin_to_bin(input_path: &Utf8Path, output: Option<Utf8PathBuf>) -> Result<()> {
    // Read the ritobin text file
    let mut file = File::open(input_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to open input file: {}", input_path))?;

    let mut ritobin_text = String::new();
    file.read_to_string(&mut ritobin_text)
        .into_diagnostic()
        .wrap_err("Failed to read ritobin file")?;

    // Parse ritobin text to BinTree
    let tree = ltk_ritobin::parse_to_bin_tree(&ritobin_text)
        .into_diagnostic()
        .wrap_err("Failed to parse ritobin file")?;

    // Determine output path
    let output_path = output.unwrap_or_else(|| {
        // Replace .py/.ritobin extension with .bin
        let stem = input_path.file_stem().unwrap_or("output");
        let parent = input_path.parent().unwrap_or(Utf8Path::new("."));
        parent.join(format!("{}.bin", stem))
    });

    // Write binary output file
    // BinTree::to_writer requires Seek, so we write to a cursor first then to file
    let mut cursor = Cursor::new(Vec::new());
    tree.to_writer(&mut cursor)
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
