//! The state shared by all commands: the configuration and the hashtable locations.

use camino::{Utf8Path, Utf8PathBuf};
use ltk_mimir_cache::HashStore;
use miette::{IntoDiagnostic, Result, WrapErr};

use crate::{
    cli::{Cli, Commands},
    config::{self, AppConfig},
    hashes::{BinHashes, GameNames, WadPaths},
};

/// The configuration and the hashtable locations for one run.
pub struct Context {
    pub config: AppConfig,
    /// The path of the config file. `None` if the directory of the executable is unknown and
    /// `--config` is not set.
    pub config_path: Option<Utf8PathBuf>,
    /// The cache directory from `--hashtable-dir` or the config. If `None`, the Mimir cache
    /// library selects the directory.
    hashtable_dir: Option<Utf8PathBuf>,
    /// A directory of text tables. Names from these tables take precedence over the cache.
    extra_hashtables: Option<Utf8PathBuf>,
}

impl Context {
    /// Builds the context from the command line and the config file.
    pub fn new(cli: &Cli) -> Result<Self> {
        let config_path = config::config_path(cli.config.as_deref());
        let config = match &config_path {
            // The `config` commands are used to fix an invalid config file. They must run when
            // the file fails to load, so they continue with the default config.
            Some(path) => match (config::load(path), &cli.command) {
                (Ok(config), _) => config,
                (Err(error), Commands::Config { .. }) => {
                    tracing::warn!("{error:?}");
                    AppConfig::default()
                }
                (Err(error), _) => return Err(error),
            },
            None => AppConfig::default(),
        };

        let mut hashtable_dir = cli
            .hashtable_dir
            .clone()
            .or_else(|| config.hashtable_dir.clone());
        let mut extra_hashtables = cli.hashtable.clone();

        if let Some(dir) = &extra_hashtables
            && !dir.is_dir()
        {
            miette::bail!(
                "--hashtable requires a directory of text hashtables, but {dir} is not a directory"
            );
        }

        // In older versions, the `hashtable_dir` config key was a directory of CDragon text
        // tables. If the configured directory still contains text tables, it is not used as the
        // cache directory. It is used as the `--hashtable` directory instead, unless `--hashtable`
        // is set.
        if cli.hashtable_dir.is_none()
            && let Some(dir) = hashtable_dir.take_if(|dir| is_text_table_dir(dir))
        {
            tracing::warn!(
                "`hashtable_dir` in the config is a directory of text hashtables ({dir}). This key now sets the hashtable cache directory. Pass a directory of text tables with --hashtable."
            );
            extra_hashtables.get_or_insert(dir);
        }

        Ok(Self {
            config,
            config_path,
            hashtable_dir,
            extra_hashtables,
        })
    }

    /// Builds a context for tests, with the default config and a cache directory that does not
    /// exist. Tests therefore do not read the hashtables installed on the machine.
    /// `extra_hashtables` is a directory of text tables.
    #[cfg(test)]
    pub fn for_tests(extra_hashtables: Option<Utf8PathBuf>) -> Self {
        Self {
            config: AppConfig::default(),
            config_path: None,
            hashtable_dir: Some("ritobin-tools-tests-no-such-cache".into()),
            extra_hashtables,
        }
    }

    /// Returns the hashtable cache. The directory is selected in this order: `--hashtable-dir`,
    /// the config, the `MIMIR_DIR` environment variable, the shared LeagueToolkit directory.
    pub fn store(&self) -> Result<HashStore> {
        match &self.hashtable_dir {
            Some(dir) => Ok(HashStore::at(dir.as_std_path())),
            None => HashStore::discover()
                .into_diagnostic()
                .wrap_err("Failed to find the hashtable cache directory. Pass --hashtable-dir"),
        }
    }

    /// Opens the bin hashtables. If the cache cannot be found, logs a warning and returns
    /// tables that resolve no hash.
    pub fn hashes(&self) -> BinHashes {
        let store = self
            .store()
            .inspect_err(|error| tracing::warn!("{error}"))
            .ok();
        BinHashes::load(store.as_ref(), self.extra_hashtables.as_deref())
    }

    /// Opens the Mimir `game` table, which resolves chunk hashes to paths.
    pub fn wad_paths(&self) -> WadPaths {
        WadPaths::load(self.store().ok().as_ref())
    }

    /// Loads the bin tables and the `game` table for rendering game-data declarations.
    pub fn game_names(&self) -> GameNames {
        GameNames {
            bins: self.hashes(),
            paths: self.wad_paths(),
        }
    }
}

/// Returns `true` if `dir` contains a CDragon text table and no cache manifest.
fn is_text_table_dir(dir: &Utf8Path) -> bool {
    !dir.join("manifest.json").exists()
        && ["entries", "fields", "hashes", "types"]
            .iter()
            .any(|table| dir.join(format!("hashes.bin{table}.txt")).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    #[test]
    fn is_text_table_dir_requires_text_table_and_no_manifest() {
        let (_guard, dir) = temp_dir();
        assert!(!is_text_table_dir(&dir));

        std::fs::write(dir.join("hashes.binfields.txt"), "").unwrap();
        assert!(is_text_table_dir(&dir));

        std::fs::write(dir.join("manifest.json"), "{}").unwrap();
        assert!(!is_text_table_dir(&dir));
    }
}
