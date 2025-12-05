//! Application configuration management utilities.

use camino::Utf8PathBuf;
use miette::Context;
use miette::IntoDiagnostic;
use miette::Result;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io;
use std::path::Path;

/// Application-wide configuration stored in config.toml.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    /// Directory where ritobin hashtables are stored.
    pub hashtable_dir: Option<Utf8PathBuf>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            hashtable_dir: default_hashtable_dir(),
        }
    }
}

/// Returns the directory where the current executable resides.
pub fn install_dir() -> Option<Utf8PathBuf> {
    let exe = env::current_exe().ok()?;
    let parent = exe.parent()?;
    Utf8PathBuf::from_path_buf(parent.to_path_buf()).ok()
}

/// Returns a config file path located next to the executable.
pub fn config_path(file_name: &str) -> Option<Utf8PathBuf> {
    install_dir().map(|dir| dir.join(file_name))
}

/// Returns the default configuration file path (config.toml).
pub fn default_config_path() -> Option<Utf8PathBuf> {
    config_path("config.toml")
}

/// Normalizes a path to use forward slashes
fn normalize_path(path: &Utf8PathBuf) -> Utf8PathBuf {
    Utf8PathBuf::from(path.as_str().replace('\\', "/"))
}

/// Saves the application configuration to config.toml.
/// Paths are normalized to use forward slashes for consistency.
pub fn save_config(cfg: &AppConfig) -> io::Result<()> {
    if let Some(path) = default_config_path() {
        let normalized_cfg = AppConfig {
            hashtable_dir: cfg.hashtable_dir.as_ref().map(normalize_path),
        };

        let content = toml::to_string_pretty(&normalized_cfg).map_err(io::Error::other)?;
        fs::write(path.as_str(), content)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Could not determine config path",
        ))
    }
}

/// Loads existing configuration or creates a new one with defaults.
/// Missing fields in the config file are filled with default values.
pub fn load_or_create_config() -> Result<(AppConfig, Utf8PathBuf)> {
    let path = default_config_path().ok_or(miette::miette!("Could not determine config path"))?;

    if Path::new(path.as_str()).exists() {
        let content = fs::read_to_string(path.as_str())
            .into_diagnostic()
            .wrap_err("Failed to read config file")?;
        let mut cfg: AppConfig = toml::from_str(&content)
            .into_diagnostic()
            .wrap_err("Failed to parse config file")?;

        // Fill in defaults for missing optional fields
        let defaults = AppConfig::default();
        if cfg.hashtable_dir.is_none() {
            cfg.hashtable_dir = defaults.hashtable_dir;
        }

        Ok((cfg, path))
    } else {
        let cfg = AppConfig::default();
        save_config(&cfg)
            .into_diagnostic()
            .wrap_err("Failed to save config file")?;
        Ok((cfg, path))
    }
}

/// Loads configuration as a raw TOML table for flexible editing.
pub fn load_config_as_table() -> Result<toml::Table> {
    let path = default_config_path().ok_or(miette::miette!("Could not determine config path"))?;

    if Path::new(path.as_str()).exists() {
        let content = fs::read_to_string(path.as_str())
            .into_diagnostic()
            .wrap_err("Failed to read config file")?;

        toml::from_str(&content)
            .into_diagnostic()
            .wrap_err("Failed to parse config file")
    } else {
        let cfg = AppConfig::default();
        let content = toml::to_string_pretty(&cfg)
            .into_diagnostic()
            .wrap_err("Failed to serialize default config")?;
        toml::from_str(&content)
            .into_diagnostic()
            .wrap_err("Failed to parse default config")
    }
}

/// Saves a raw TOML table to the config file.
pub fn save_config_table(table: &toml::Table) -> io::Result<()> {
    if let Some(path) = default_config_path() {
        let content = toml::to_string_pretty(table).map_err(io::Error::other)?;
        fs::write(path.as_str(), content)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Could not determine config path",
        ))
    }
}

/// Returns the default directory where wad hashtables should be looked up.
/// Uses the user's Documents folder: Documents/LeagueToolkit/bin_hashtables
/// Falls back to ~/.local/share/LeagueToolkit/bin_hashtables on Linux if Documents isn't available
pub fn default_hashtable_dir() -> Option<Utf8PathBuf> {
    // Try Documents folder first (Windows, macOS, and some Linux setups)
    if let Some(doc_dir) =
        directories_next::UserDirs::new().and_then(|u| u.document_dir().map(|p| p.to_path_buf()))
    {
        let mut path = doc_dir;
        path.push("LeagueToolkit");
        path.push("bin_hashtables");
        if let Ok(utf8_path) = Utf8PathBuf::from_path_buf(path) {
            return Some(utf8_path);
        }
    }

    // Fallback: use data directory (~/.local/share on Linux, AppData on Windows)
    let data_dirs = directories_next::ProjectDirs::from("", "", "LeagueToolkit")?;
    let mut path = data_dirs.data_dir().to_path_buf();
    path.push("bin_hashtables");
    Utf8PathBuf::from_path_buf(path).ok()
}
