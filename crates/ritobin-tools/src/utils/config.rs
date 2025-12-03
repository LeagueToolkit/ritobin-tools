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
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Loads the application configuration from config.toml.
/// Returns default configuration if file doesn't exist or cannot be parsed.
pub fn load_config() -> AppConfig {
    if let Some(path) = default_config_path() {
        if Path::new(path.as_str()).exists() {
            if let Ok(content) = fs::read_to_string(path.as_str()) {
                if let Ok(cfg) = toml::from_str(&content) {
                    return cfg;
                }
            }
        }
    }
    AppConfig::default()
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
pub fn load_or_create_config() -> Result<(AppConfig, Utf8PathBuf)> {
    let path = default_config_path().ok_or(miette::miette!("Could not determine config path"))?;

    if Path::new(path.as_str()).exists() {
        let content = fs::read_to_string(path.as_str())
            .into_diagnostic()
            .wrap_err("Failed to read config file")?;
        let cfg = toml::from_str(&content)
            .into_diagnostic()
            .wrap_err("Failed to parse config file")?;
        Ok((cfg, path))
    } else {
        let cfg = AppConfig::default();
        save_config(&cfg)
            .into_diagnostic()
            .wrap_err("Failed to save config file")?;
        Ok((cfg, path))
    }
}

/// Reads JSON from a path into type T. Returns Ok(None) if file cannot be read or parsed.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<T>(&bytes) {
            Ok(v) => Ok(Some(v)),
            Err(_) => Ok(None),
        },
        Err(_) => Ok(None),
    }
}

/// Writes pretty-formatted JSON to the given path.
pub fn write_json_pretty<T: serde::Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let data = serde_json::to_vec_pretty(value).unwrap_or_else(|_| b"{}".to_vec());
    fs::write(path, data)
}

/// Returns current UNIX epoch seconds.
pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Returns the default directory where wad hashtables should be looked up.
/// Uses the user's Documents folder: Documents/LeagueToolkit/bin_hashtables
pub fn default_hashtable_dir() -> Option<Utf8PathBuf> {
    let user_dirs = directories_next::UserDirs::new()?;
    let doc_dir = user_dirs.document_dir()?;
    let mut path = doc_dir.to_path_buf();
    path.push("LeagueToolkit");
    path.push("bin_hashtables");
    Utf8PathBuf::from_path_buf(path).ok()
}
