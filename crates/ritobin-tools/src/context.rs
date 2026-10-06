//! What every command runs with: the configuration and the way to the hashtables.

use camino::{Utf8Path, Utf8PathBuf};
use ltk_mimir_cache::HashStore;
use miette::{IntoDiagnostic, Result, WrapErr};

use crate::{
    cli::{Cli, Commands},
    config::{self, AppConfig},
    hashes::BinHashes,
};

pub struct Context {
    pub config: AppConfig,
    /// The config file in use. `None` when its directory cannot be told.
    pub config_path: Option<Utf8PathBuf>,
    /// The cache directory, from the flag or the config. `None` leaves it to the cache's own
    /// discovery.
    hashtable_dir: Option<Utf8PathBuf>,
    /// A directory of text tables laid over the cache.
    extra_hashtables: Option<Utf8PathBuf>,
}

impl Context {
    pub fn new(cli: &Cli) -> Result<Self> {
        let config_path = config::config_path(cli.config.as_deref());
        let config = match &config_path {
            // The config commands are how a config that does not load is put right, so they run
            // on the defaults.
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
                "--hashtable expects a directory of text hashtables, and {dir} is not one"
            );
        }

        // The config key used to name a directory of CDragon text tables. One that still does is
        // never used as the cache, which is found the usual way. Its tables are read unless
        // `--hashtable` names others.
        if cli.hashtable_dir.is_none()
            && let Some(dir) = hashtable_dir.take_if(|dir| is_text_table_dir(dir))
        {
            tracing::warn!(
                "`hashtable_dir` in the config names a directory of text hashtables ({dir}). It now names the hashtable cache; pass text tables with --hashtable."
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

    /// A context with default settings and an empty cache, so a test never reads the tables
    /// installed on the machine. `extra_hashtables` is a directory of text tables.
    #[cfg(test)]
    pub fn for_tests(extra_hashtables: Option<Utf8PathBuf>) -> Self {
        Self {
            config: AppConfig::default(),
            config_path: None,
            hashtable_dir: Some("ritobin-tools-tests-no-such-cache".into()),
            extra_hashtables,
        }
    }

    /// The hashtable cache: the flag, then the config, then `MIMIR_DIR`, then the directory every
    /// LeagueToolkit tool shares.
    pub fn store(&self) -> Result<HashStore> {
        match &self.hashtable_dir {
            Some(dir) => Ok(HashStore::at(dir.as_std_path())),
            None => HashStore::discover()
                .into_diagnostic()
                .wrap_err("Could not find the hashtable cache directory; pass --hashtable-dir"),
        }
    }

    /// Opens the bin hashtables. Failing to is a warning, and the hashes stay hex.
    pub fn hashes(&self) -> BinHashes {
        let store = self
            .store()
            .inspect_err(|error| tracing::warn!("{error}"))
            .ok();
        BinHashes::load(store.as_ref(), self.extra_hashtables.as_deref())
    }
}

/// Whether `dir` holds CDragon text tables and no hashtable cache.
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
    fn a_directory_of_text_tables_is_told_from_a_cache() {
        let (_guard, dir) = temp_dir();
        assert!(!is_text_table_dir(&dir));

        std::fs::write(dir.join("hashes.binfields.txt"), "").unwrap();
        assert!(is_text_table_dir(&dir));

        std::fs::write(dir.join("manifest.json"), "{}").unwrap();
        assert!(!is_text_table_dir(&dir));
    }
}
