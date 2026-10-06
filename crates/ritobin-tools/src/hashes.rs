//! Bin hash name resolution, backed by the shared Mimir hashtable cache.

use std::{borrow::Cow, collections::HashMap, sync::Arc};

use camino::Utf8Path;
use ltk_hash::{BinHash, WadHash};
use ltk_meta::path::FieldNames;
use ltk_mimir_cache::{HashStore, ManifestError, OpenError, Table, ltk_hashdb::HashDb};
use ltk_ritobin::{HashMapProvider, HashProvider};

/// The Mimir tables a bin file's hashes resolve against.
pub const BIN_TABLES: [Table; 4] = [
    Table::BinEntries,
    Table::BinFields,
    Table::BinHashes,
    Table::BinTypes,
];

/// The GitHub repository the hashtable releases are published from.
pub const MIMIR_TABLES_REPO: &str = "LeagueToolkit/mimir-tables";

/// Names for the four kinds of bin hash.
///
/// Each kind resolves against its own Mimir table. A directory of CDragon text tables can be laid
/// over them, and its names win. A table that is not loaded resolves nothing, so its hashes stay
/// hex.
///
/// Cloning is cheap: the tables are shared handles.
#[derive(Clone, Default)]
pub struct BinHashes {
    entries: Option<HashDb>,
    fields: Option<HashDb>,
    hashes: Option<HashDb>,
    types: Option<HashDb>,
    extra: Option<Arc<HashMapProvider>>,
}

impl BinHashes {
    /// Resolves nothing: every hash stays hex.
    pub fn none() -> Self {
        Self::default()
    }

    /// Opens the bin tables of `store`, and lays the text tables in `extra_dir` over them.
    ///
    /// A table that fails to open is skipped with a warning, so a missing or broken cache never
    /// stops a conversion.
    pub fn load(store: Option<&HashStore>, extra_dir: Option<&Utf8Path>) -> Self {
        let mut hashes = Self::none();

        if let Some(store) = store {
            let mut missing_cache = false;
            let mut open = |table: Table| match store.open_shared(table) {
                Ok(db) => Some(db),
                Err(OpenError::Manifest(ManifestError::Missing(_))) => {
                    missing_cache = true;
                    None
                }
                Err(error) => {
                    tracing::warn!("Could not open the {table} hashtable: {error}");
                    None
                }
            };
            hashes.entries = open(Table::BinEntries);
            hashes.fields = open(Table::BinFields);
            hashes.hashes = open(Table::BinHashes);
            hashes.types = open(Table::BinTypes);

            if missing_cache {
                tracing::warn!(
                    "No hashtables are installed, so hashes will not be named. Run `ritobin-tools hashes sync` to download them."
                );
            }
        }

        if let Some(dir) = extra_dir {
            let mut extra = HashMapProvider::new();
            extra.load_from_directory(dir);
            if extra.total_count() == 0 {
                tracing::warn!("No hashes were loaded from {dir}");
            }
            hashes.extra = Some(Arc::new(extra));
        }

        hashes
    }

    /// Whether no table is loaded at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_none()
            && self.fields.is_none()
            && self.hashes.is_none()
            && self.types.is_none()
            && self
                .extra
                .as_ref()
                .is_none_or(|extra| extra.total_count() == 0)
    }

    /// The handle on one of the [`BIN_TABLES`], if it is loaded.
    pub fn table(&self, table: Table) -> Option<&HashDb> {
        match table {
            Table::BinEntries => self.entries.as_ref(),
            Table::BinFields => self.fields.as_ref(),
            Table::BinHashes => self.hashes.as_ref(),
            Table::BinTypes => self.types.as_ref(),
            _ => None,
        }
    }

    /// The names of `table` that come from the text tables.
    fn extra(&self, table: Table) -> Option<&HashMap<BinHash, String>> {
        let extra = self.extra.as_ref()?;
        match table {
            Table::BinEntries => Some(&extra.entries),
            Table::BinFields => Some(&extra.fields),
            Table::BinHashes => Some(&extra.hashes),
            Table::BinTypes => Some(&extra.types),
            _ => None,
        }
    }

    /// Calls `visit` with every name known for `table`: the text tables' in name order, then the
    /// cache's in its own order, leaving out a hash the text tables already named.
    ///
    /// `false` when nothing is loaded for the table.
    pub fn for_each_name(&self, table: Table, mut visit: impl FnMut(BinHash, &str)) -> bool {
        let extra = self.extra(table).filter(|extra| !extra.is_empty());
        let db = self.table(table);
        if extra.is_none() && db.is_none() {
            return false;
        }

        if let Some(extra) = extra {
            let mut names: Vec<(&BinHash, &String)> = extra.iter().collect();
            names.sort_by(|a, b| a.1.cmp(b.1));
            for (hash, name) in names {
                visit(*hash, name);
            }
        }
        if let Some(db) = db {
            for (hash, name) in db.iter() {
                // A bin table is keyed by 32-bit hashes.
                let hash = BinHash(hash as u32);
                if !extra.is_some_and(|extra| extra.contains_key(&hash)) {
                    visit(hash, &name);
                }
            }
        }
        true
    }

    /// The name of `hash` in `table`, with the text tables consulted first.
    pub fn lookup(&self, table: Table, hash: BinHash) -> Option<Cow<'_, str>> {
        let extra = self.extra.as_ref().and_then(|extra| match table {
            Table::BinEntries => extra.lookup_entry(hash),
            Table::BinFields => extra.lookup_field(hash),
            Table::BinHashes => extra.lookup_hash(hash),
            Table::BinTypes => extra.lookup_type(hash),
            _ => None,
        });
        extra.or_else(|| {
            self.table(table)?
                .get(u64::from(hash.0))
                .map(|name| Cow::Owned(name.into_owned()))
        })
    }
}

impl HashProvider for BinHashes {
    fn lookup_entry(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinEntries, hash)
    }

    fn lookup_field(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinFields, hash)
    }

    fn lookup_hash(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinHashes, hash)
    }

    fn lookup_type(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinTypes, hash)
    }
}

/// The field table is keyed by field alone and ignores the class.
impl FieldNames for BinHashes {
    fn field(&self, field: BinHash, _class: Option<BinHash>) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinFields, field)
    }

    fn hash(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinHashes, hash)
    }
}

/// Names for the chunk hashes of the game's archives, from the Mimir table of game paths.
///
/// Cloning is cheap: the table is a shared handle.
#[derive(Clone, Default)]
pub struct WadPaths(Option<HashDb>);

impl WadPaths {
    /// Opens the table of game paths of `store`. A cache without it names nothing.
    pub fn load(store: Option<&HashStore>) -> Self {
        Self(
            store.and_then(|store| match store.open_shared(Table::Game) {
                Ok(db) => Some(db),
                Err(OpenError::Manifest(ManifestError::Missing(_))) => None,
                Err(error) => {
                    tracing::warn!("Could not open the {} hashtable: {error}", Table::Game);
                    None
                }
            }),
        )
    }

    /// Whether the table is loaded.
    pub fn is_loaded(&self) -> bool {
        self.0.is_some()
    }

    /// The path of `chunk`, if the table has it.
    pub fn path(&self, chunk: WadHash) -> Option<String> {
        Some(self.0.as_ref()?.get(chunk.0)?.into_owned())
    }
}

impl ltk_wad::PathResolver for WadPaths {
    fn resolve(&self, path_hash: WadHash) -> Option<String> {
        self.path(path_hash)
    }
}

/// Every name a value rendered as a game-data declaration carries.
#[derive(Clone, Default)]
pub struct GameNames {
    pub bins: BinHashes,
    pub paths: WadPaths,
}

impl FieldNames for GameNames {
    fn field(&self, field: BinHash, class: Option<BinHash>) -> Option<Cow<'_, str>> {
        self.bins.field(field, class)
    }

    fn hash(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.bins.hash(hash)
    }
}

impl ltk_game_data::Names for GameNames {
    fn class(&self, class: BinHash) -> Option<Cow<'_, str>> {
        self.bins.lookup(Table::BinTypes, class)
    }

    fn entry(&self, entry: BinHash) -> Option<Cow<'_, str>> {
        self.bins.lookup(Table::BinEntries, entry)
    }

    fn file(&self, chunk: u64) -> Option<Cow<'_, str>> {
        self.paths.path(WadHash(chunk)).map(Cow::Owned)
    }
}

/// Parses a bin hash written as hex, with or without a `0x` prefix.
pub fn parse_hash(text: &str) -> Option<BinHash> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    u32::from_str_radix(digits, 16).ok().map(BinHash)
}

/// Writes a bin hash the way ritobin text does.
pub fn format_hash(hash: BinHash) -> String {
    format!("0x{:08x}", hash.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hash_accepts_hex_with_and_without_a_prefix() {
        assert_eq!(parse_hash("0x4a47c414"), Some(BinHash(0x4a47_c414)));
        assert_eq!(parse_hash("4A47C414"), Some(BinHash(0x4a47_c414)));
        assert_eq!(parse_hash("0X1f"), Some(BinHash(0x1f)));
    }

    #[test]
    fn parse_hash_rejects_text_that_is_not_a_32_bit_hex_number() {
        assert_eq!(parse_hash(""), None);
        assert_eq!(parse_hash("0x"), None);
        assert_eq!(parse_hash("0x123456789"), None);
        assert_eq!(parse_hash("mName"), None);
    }

    #[test]
    fn an_empty_provider_names_nothing() {
        let hashes = BinHashes::none();
        assert!(hashes.is_empty());
        assert_eq!(hashes.lookup_field(BinHash(0x1234)), None);
        assert_eq!(FieldNames::field(&hashes, BinHash(0x1234), None), None);
    }
}
