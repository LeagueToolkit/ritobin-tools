//! Loads and saves the configuration file.

use std::{env, fs};

use camino::{Utf8Path, Utf8PathBuf};
use miette::{Context as _, IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};

use crate::document::TextLayout;

/// The name of the configuration file. The file is located in the directory of the executable.
pub const CONFIG_FILE_NAME: &str = "ritobin-tools.toml";

/// The previous name of the configuration file. A file with this name is still read.
const LEGACY_CONFIG_FILE_NAME: &str = "config.toml";

/// The settings stored in the configuration file. A command line flag overrides each setting.
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// The hashtable cache directory. If unset, the shared LeagueToolkit cache directory is
    /// used.
    pub hashtable_dir: Option<Utf8PathBuf>,
    /// The game directory used by the `gamedata` commands: the `Game` directory of an
    /// installation, or its parent directory.
    pub game_dir: Option<Utf8PathBuf>,
    /// The layout options for printing ritobin text.
    pub print_config: TextLayout,
}

/// Returns the directory that contains the executable.
fn install_dir() -> Option<Utf8PathBuf> {
    let exe = env::current_exe().ok()?;
    Utf8PathBuf::from_path_buf(exe.parent()?.to_path_buf()).ok()
}

/// Returns the path of the configuration file: `explicit` if it is set, otherwise
/// [`CONFIG_FILE_NAME`] in the directory of the executable.
///
/// Returns the path of the legacy file name if a file with that name exists and no file with the
/// current name exists. Returns `None` if the directory of the executable is unknown.
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

/// Loads the configuration at `path`. Returns the default configuration if the file does not
/// exist.
pub fn load(path: &Utf8Path) -> Result<AppConfig> {
    load_table(path)?
        .try_into()
        .into_diagnostic()
        .wrap_err_with(|| format!("Invalid config file {path}"))
}

/// Loads the configuration at `path` as a TOML table. Returns an empty table if the file does
/// not exist.
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
    fn load_returns_defaults_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join(CONFIG_FILE_NAME)).unwrap();
        assert_eq!(load(&path).unwrap(), AppConfig::default());
    }

    #[test]
    fn saved_config_loads_back_equal() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join(CONFIG_FILE_NAME)).unwrap();
        let config = AppConfig {
            hashtable_dir: Some("C:/hashes".into()),
            game_dir: Some("C:/Riot Games/League of Legends".into()),
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
    fn partial_layout_uses_defaults_for_missing_keys() {
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
