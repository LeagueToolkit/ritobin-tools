//! The configuration file.

use std::{env, fs};

use camino::{Utf8Path, Utf8PathBuf};
use miette::{Context as _, IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};

use crate::document::TextLayout;

/// The name of the configuration file, which lives next to the executable.
pub const CONFIG_FILE_NAME: &str = "ritobin-tools.toml";

/// The name the configuration file had before it was named after the tool.
const LEGACY_CONFIG_FILE_NAME: &str = "config.toml";

/// The settings stored in the configuration file. Every one has a flag that overrides it.
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// The hashtable cache directory. Unset means the cache every LeagueToolkit tool shares.
    pub hashtable_dir: Option<Utf8PathBuf>,
    /// How ritobin text is laid out.
    pub print_config: TextLayout,
}

/// The directory the executable is in.
fn install_dir() -> Option<Utf8PathBuf> {
    let exe = env::current_exe().ok()?;
    Utf8PathBuf::from_path_buf(exe.parent()?.to_path_buf()).ok()
}

/// The configuration file to use: `explicit` when given, else the one next to the executable.
///
/// A file under the old name is still used while none under the new name exists.
pub fn config_path(explicit: Option<&Utf8Path>) -> Option<Utf8PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_owned());
    }
    let dir = install_dir()?;
    let path = dir.join(CONFIG_FILE_NAME);
    let legacy = dir.join(LEGACY_CONFIG_FILE_NAME);
    if !path.exists() && legacy.exists() {
        return Some(legacy);
    }
    Some(path)
}

/// Loads the configuration at `path`. A file that does not exist is the defaults.
pub fn load(path: &Utf8Path) -> Result<AppConfig> {
    load_table(path)?
        .try_into()
        .into_diagnostic()
        .wrap_err_with(|| format!("Invalid config file {path}"))
}

/// Loads the configuration at `path` as a raw table. A file that does not exist is empty.
pub fn load_table(path: &Utf8Path) -> Result<toml::Table> {
    if !path.exists() {
        return Ok(toml::Table::new());
    }
    let content = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to read config file {path}"))?;
    toml::from_str(&content)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to parse config file {path}"))
}

/// Writes `config` to `path`.
pub fn save(path: &Utf8Path, config: &AppConfig) -> Result<()> {
    let content = toml::to_string_pretty(config)
        .into_diagnostic()
        .wrap_err("Failed to serialize the config")?;
    fs::write(path, content)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to write config file {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join(CONFIG_FILE_NAME)).unwrap();
        assert_eq!(load(&path).unwrap(), AppConfig::default());
    }

    #[test]
    fn a_saved_config_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join(CONFIG_FILE_NAME)).unwrap();
        let config = AppConfig {
            hashtable_dir: Some("C:/hashes".into()),
            print_config: TextLayout {
                indent_size: 2,
                inline_structs: true,
                ..TextLayout::default()
            },
        };
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap(), config);
    }

    #[test]
    fn a_partial_layout_keeps_the_other_defaults() {
        let config: AppConfig = toml::from_str(
            "[print_config]\nindent_size = 2\n[print_config.wrap]\nline_width = 80\n",
        )
        .unwrap();
        assert_eq!(
            config.print_config,
            TextLayout {
                indent_size: 2,
                line_width: 80,
                ..TextLayout::default()
            }
        );
    }
}
