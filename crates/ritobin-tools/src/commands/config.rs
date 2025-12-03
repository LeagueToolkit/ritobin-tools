use crate::utils::config::{self, AppConfig};
use camino::Utf8PathBuf;
use colored::Colorize;
use miette::Result;

/// Format a path as a clickable hyperlink using OSC 8 escape sequence.
/// Falls back to underlined text if terminal doesn't support hyperlinks.
fn clickable_path(path: &Utf8PathBuf) -> String {
    let file_url = format!("file:///{}", path.as_str().replace('\\', "/"));
    let display = path.as_str().underline();
    // OSC 8 hyperlink: \x1b]8;;URL\x1b\\TEXT\x1b]8;;\x1b\\
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", file_url, display)
}

/// Print a config path entry with status indicator
fn print_path_config(
    name: &str,
    path: Option<&Utf8PathBuf>,
    validator: impl Fn(&Utf8PathBuf) -> bool,
) {
    match path {
        Some(p) => {
            let status = if validator(p) {
                "✓".bright_green()
            } else {
                "✗".bright_red()
            };
            println!(
                "  {} {} {}",
                format!("{}:", name).bright_white(),
                clickable_path(p),
                status
            );
        }
        None => {
            println!(
                "  {} {}",
                format!("{}:", name).bright_white(),
                "(not set)".bright_yellow()
            );
        }
    }
}

pub fn show_config() -> Result<()> {
    let cfg = config::load_config();
    let config_path = config::default_config_path();

    println!();
    match &config_path {
        Some(p) => println!("  {} {}", "config_file:".bright_white(), clickable_path(p)),
        None => println!(
            "  {} {}",
            "config_file:".bright_white(),
            "Unknown".bright_yellow()
        ),
    }

    print_path_config("hashtable_dir", cfg.hashtable_dir.as_ref(), |p| p.exists());

    println!();
    Ok(())
}

pub fn reset_config() -> Result<()> {
    let config_path = config::default_config_path()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    let default_cfg = AppConfig::default();
    config::save_config(&default_cfg)
        .map_err(|e| miette::miette!("Failed to reset config: {}", e))?;

    println!(
        "{}",
        "✓ Configuration reset to defaults".bright_green().bold()
    );
    println!();
    println!("  {} {}", "Config file:".bright_white().bold(), config_path);
    println!();
    println!(
        "  {}",
        "Run 'league-mod config auto-detect' to find your League installation".bright_cyan()
    );

    Ok(())
}

/// Ensures config.toml exists.
pub fn ensure_config_exists() -> Result<()> {
    let (_cfg, _path) = config::load_or_create_config()
        .map_err(|e| miette::miette!("Failed to initialize config: {}", e))?;

    Ok(())
}
