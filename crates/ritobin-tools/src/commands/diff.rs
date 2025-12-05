use std::fs::File;
use std::io::{BufReader, Read};

use camino::Utf8Path;
use colored::Colorize;
use ltk_meta::BinTree;
use ltk_ritobin::{HashMapProvider, HexHashProvider, WriterConfig};
use miette::{IntoDiagnostic, Result, WrapErr};
use similar::{ChangeTag, TextDiff};

use crate::utils::config::load_or_create_config;

/// Supported file extensions for diffing
const SUPPORTED_EXTENSIONS: &[&str] = &["bin", "py", "ritobin"];

/// Diff two .bin or .ritobin files against each other.
///
/// Both files are converted to the ritobin text format internally,
/// and a unified diff is displayed showing the differences.
pub fn diff(file1: String, file2: String, context_lines: usize, no_color: bool) -> Result<()> {
    let path1 = Utf8Path::new(&file1);
    let path2 = Utf8Path::new(&file2);

    // Validate file extensions
    validate_extension(path1)?;
    validate_extension(path2)?;

    // Load config for hashtable provider
    let (config, _) = load_or_create_config()?;

    // Convert both files to ritobin text format
    let text1 = file_to_ritobin_text(path1, &config)?;
    let text2 = file_to_ritobin_text(path2, &config)?;

    // Compute and display the diff
    display_diff(&text1, &text2, path1, path2, context_lines, no_color);

    Ok(())
}

/// Validate that the file has a supported extension
fn validate_extension(path: &Utf8Path) -> Result<()> {
    let extension = path.extension().unwrap_or("");
    if !SUPPORTED_EXTENSIONS.contains(&extension) {
        return Err(miette::miette!(
            "Unsupported file extension: .{}. Supported extensions: .bin, .py, .ritobin",
            extension
        ));
    }
    Ok(())
}

/// Load a file and convert it to ritobin text format
fn file_to_ritobin_text(
    path: &Utf8Path,
    config: &crate::utils::config::AppConfig,
) -> Result<String> {
    let extension = path.extension().unwrap_or("");

    match extension {
        "bin" => {
            let tree = load_bin_file(path)?;
            let ritobin_text = if let Some(hashtable_dir) = config.hashtable_dir.as_ref() {
                let mut hashtable_provider = HashMapProvider::new();
                hashtable_provider.load_from_directory(hashtable_dir);

                ltk_ritobin::write_with_config_and_hashes(
                    &tree,
                    WriterConfig::default(),
                    &hashtable_provider,
                )
            } else {
                ltk_ritobin::write_with_config_and_hashes(
                    &tree,
                    WriterConfig::default(),
                    &HexHashProvider,
                )
            }
            .into_diagnostic()
            .wrap_err_with(|| format!("Failed to convert {} to ritobin format", path))?;

            Ok(ritobin_text)
        }
        "py" | "ritobin" => read_text_file(path),
        _ => Err(miette::miette!(
            "Unsupported file extension: .{}",
            extension
        )),
    }
}

/// Load a .bin file into a BinTree
fn load_bin_file(path: &Utf8Path) -> Result<BinTree> {
    let file = File::open(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to open file: {}", path))?;
    let mut reader = BufReader::new(file);

    BinTree::from_reader(&mut reader)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to parse .bin file: {}", path))
}

/// Read a text file (.py/.ritobin) directly
fn read_text_file(path: &Utf8Path) -> Result<String> {
    let mut file = File::open(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to open file: {}", path))?;

    let mut content = String::new();
    file.read_to_string(&mut content)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to read file: {}", path))?;

    Ok(content)
}

/// Display the diff between two ritobin text representations
fn display_diff(
    text1: &str,
    text2: &str,
    path1: &Utf8Path,
    path2: &Utf8Path,
    context_lines: usize,
    no_color: bool,
) {
    let diff = TextDiff::from_lines(text1, text2);

    // Check if files are identical
    if diff.ratio() == 1.0 {
        if no_color {
            println!("Files are identical");
        } else {
            println!("{}", "Files are identical".green());
        }
        return;
    }

    // Count insertions and deletions
    let mut insertions = 0;
    let mut deletions = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => insertions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }

    // Print header
    if no_color {
        println!("--- {}", path1);
        println!("+++ {}", path2);
    } else {
        println!("{} {}", "---".red(), path1.to_string().red());
        println!("{} {}", "+++".green(), path2.to_string().green());
    }

    // Print unified diff with context
    for hunk in diff
        .unified_diff()
        .context_radius(context_lines)
        .iter_hunks()
    {
        // Print hunk header
        let header = hunk.header().to_string();
        if no_color {
            print!("{}", header);
        } else {
            print!("{}", header.cyan());
        }

        // Print changes
        for change in hunk.iter_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };

            let line = change.value();

            if no_color {
                print!("{}{}", sign, line);
            } else {
                match change.tag() {
                    ChangeTag::Delete => print!("{}{}", sign.red(), line.red()),
                    ChangeTag::Insert => print!("{}{}", sign.green(), line.green()),
                    ChangeTag::Equal => print!("{}{}", sign, line),
                }
            }

            // Handle missing newline at end of file
            if change.missing_newline() {
                println!();
                if no_color {
                    println!("\\ No newline at end of file");
                } else {
                    println!("{}", "\\ No newline at end of file".yellow());
                }
            }
        }
    }

    // Print summary statistics
    println!();
    if no_color {
        println!(
            "Summary: {} insertion(s), {} deletion(s)",
            insertions, deletions
        );
    } else {
        println!(
            "{} {} {}{} {} {}",
            "Summary:".bold(),
            insertions.to_string().green(),
            "insertion(s)".green(),
            ",".white(),
            deletions.to_string().red(),
            "deletion(s)".red(),
        );
    }
}
