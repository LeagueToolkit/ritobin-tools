//! Resolves bin hashes and chunk hashes to names, using the shared Mimir hashtable cache.

use std::{borrow::Cow, collections::HashMap, sync::Arc};

use camino::Utf8Path;
use ltk_hash::{BinHash, WadHash};
use ltk_meta::path::FieldNames;
use ltk_mimir_cache::{HashStore, ManifestError, OpenError, Table, ltk_hashdb::HashDb};
use ltk_ritobin::{HashMapProvider, HashProvider};

/// The Mimir tables that resolve the hashes of a bin file.
pub const BIN_TABLES: [Table; 4] = [
    Table::BinEntries,
    Table::BinFields,
    Table::BinHashes,
    Table::BinTypes,
];

/// The GitHub repository that publishes the hashtable releases.
pub const MIMIR_TABLES_REPO: &str = "LeagueToolkit/mimir-tables";

/// Resolves the four kinds of bin hash to names.
///
/// Each kind uses its own Mimir table. An optional directory of CDragon text tables takes
/// precedence over the Mimir tables. If a table is not loaded, its lookups return `None` and the
/// hashes are printed as hex.
///
/// Cloning is cheap. The tables are shared handles.
#[derive(Clone, Default)]
pub struct BinHashes {
    entries: Option<HashDb>,
    fields: Option<HashDb>,
    hashes: Option<HashDb>,
    types: Option<HashDb>,
    extra: Option<Arc<HashMapProvider>>,
    /// The paths of `file` values.
    files: WadPaths,
}

impl BinHashes {
    /// Returns a `BinHashes` with no tables. Every lookup returns `None`.
    pub fn none() -> Self {
        Self::default()
    }

    /// Opens the bin tables of `store` and loads the text tables in `extra_dir`.
    ///
    /// If a table fails to open, a warning is logged and the table is skipped. A missing or
    /// corrupt cache therefore does not fail the command.
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
                    tracing::warn!("Failed to open the {table} hashtable: {error}");
                    None
                }
            };
            hashes.entries = open(Table::BinEntries);
            hashes.fields = open(Table::BinFields);
            hashes.hashes = open(Table::BinHashes);
            hashes.types = open(Table::BinTypes);
            hashes.files = WadPaths::load(Some(store));

            if missing_cache {
                tracing::warn!(
                    "No hashtables are installed. Hashes are printed as hex. Run `ritobin-tools hashes sync` to download the hashtables."
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

    /// Returns `true` if no Mimir table is loaded and the text tables have no names.
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

    /// Returns the Mimir table handle for one of the [`BIN_TABLES`]. Returns `None` if the
    /// table is not loaded.
    pub fn table(&self, table: Table) -> Option<&HashDb> {
        match table {
            Table::BinEntries => self.entries.as_ref(),
            Table::BinFields => self.fields.as_ref(),
            Table::BinHashes => self.hashes.as_ref(),
            Table::BinTypes => self.types.as_ref(),
            _ => None,
        }
    }

    /// Returns the names of `table` that were loaded from the text tables.
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

    /// Calls `visit` for every name of `table`. Names from the text tables come first, sorted
    /// by name. Names from the Mimir table follow in table order, without the hashes that the
    /// text tables already resolved.
    ///
    /// Returns `false` if neither source has `table` loaded.
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
                // The keys of a bin table are 32-bit hashes stored as `u64`.
                let hash = BinHash(hash as u32);
                if !extra.is_some_and(|extra| extra.contains_key(&hash)) {
                    visit(hash, &name);
                }
            }
        }
        true
    }

    /// Returns the name of `hash` in `table`. The text tables are checked first, then the
    /// Mimir table.
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

    /// Returns the name of a `hash` or `link` value. The hash table is checked first, then the
    /// entry table, because the name of a link target is in the entry table.
    pub fn value_name(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinHashes, hash)
            .or_else(|| self.lookup(Table::BinEntries, hash))
    }
}

impl HashProvider for BinHashes {
    fn lookup_entry(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinEntries, hash)
    }

    fn lookup_field(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinFields, hash)
    }

    /// Returns the name of a `hash` or `link` value. The printer of `ltk_ritobin` calls this
    /// function for both value types.
    fn lookup_hash(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.value_name(hash)
    }

    fn lookup_type(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinTypes, hash)
    }

    /// Returns the path of a `file` value. The text tables are checked first, then the Mimir
    /// `game` table.
    fn lookup_wad(&self, hash: WadHash) -> Option<Cow<'_, str>> {
        let extra = self.extra.as_ref().and_then(|extra| extra.lookup_wad(hash));
        extra.or_else(|| self.files.path(hash).map(Cow::Owned))
    }
}

/// The field table has one name per field hash. The class is not used.
impl FieldNames for BinHashes {
    fn field(&self, field: BinHash, _class: Option<BinHash>) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinFields, field)
    }

    fn hash(&self, hash: BinHash) -> Option<Cow<'_, str>> {
        self.lookup(Table::BinHashes, hash)
    }
}

/// Resolves chunk hashes of the game's archives to paths, using the Mimir `game` table.
///
/// Cloning is cheap. The table is a shared handle.
#[derive(Clone, Default)]
pub struct WadPaths(Option<HashDb>);

impl WadPaths {
    /// Opens the `game` table of `store`. If `store` is `None` or the table is not installed,
    /// returns a `WadPaths` that resolves no hash.
    pub fn load(store: Option<&HashStore>) -> Self {
        Self(
            store.and_then(|store| match store.open_shared(Table::Game) {
                Ok(db) => Some(db),
                Err(OpenError::Manifest(ManifestError::Missing(_))) => None,
                Err(error) => {
                    tracing::warn!("Failed to open the {} hashtable: {error}", Table::Game);
                    None
                }
            }),
        )
    }

    /// Returns `true` if the table is loaded.
    pub fn is_loaded(&self) -> bool {
        self.0.is_some()
    }

    /// Returns the path of `chunk`. Returns `None` if the table is not loaded or has no entry
    /// for the hash.
    pub fn path(&self, chunk: WadHash) -> Option<String> {
        Some(self.0.as_ref()?.get(chunk.0)?.into_owned())
    }

    /// Calls `visit` for every chunk hash and path of the table, in table order. Calls nothing
    /// if the table is not loaded.
    pub fn for_each_path(&self, mut visit: impl FnMut(WadHash, &str)) {
        if let Some(db) = &self.0 {
            for (hash, path) in db.iter() {
                visit(WadHash(hash), &path);
            }
        }
    }
}

impl ltk_wad::PathResolver for WadPaths {
    fn resolve(&self, path_hash: WadHash) -> Option<String> {
        self.path(path_hash)
    }
}

/// The name lookups used to render a bin value as a game-data declaration: field, hash, class
/// and entry names from the bin tables, and file paths from the `game` table.
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

/// Formats a bin hash as `0x` followed by 8 hex digits, as in ritobin text.
pub fn format_hash(hash: BinHash) -> String {
    format!("0x{:08x}", hash.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_wad_reads_game_text_table() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let hash = ltk_game_index::chunk_hash("assets/mods/example.tex");
        std::fs::write(
            dir.join("hashes.game.txt"),
            format!("{:016x} assets/mods/example.tex\n", hash.0),
        )
        .unwrap();

        let hashes = BinHashes::load(None, Some(dir));
        assert_eq!(
            hashes.lookup_wad(hash).as_deref(),
            Some("assets/mods/example.tex")
        );
        assert_eq!(hashes.lookup_wad(WadHash(1)), None);
        assert_eq!(BinHashes::none().lookup_wad(hash), None);
    }

    #[test]
    fn value_name_falls_back_to_entry_table() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let entry = BinHash(0x58a7_d43d);
        let both = BinHash(0xb19f_b4b4);
        std::fs::write(
            dir.join("hashes.binentries.txt"),
            format!(
                "{:08x} Characters/Rengar/CAC/Rengar_Base\n{:08x} entry table\n",
                entry.0, both.0
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("hashes.binhashes.txt"),
            format!("{:08x} hash table\n", both.0),
        )
        .unwrap();

        let hashes = BinHashes::load(None, Some(dir));
        assert_eq!(
            hashes.value_name(entry).as_deref(),
            Some("Characters/Rengar/CAC/Rengar_Base")
        );
        assert_eq!(hashes.lookup_hash(entry), hashes.value_name(entry));
        assert_eq!(hashes.value_name(both).as_deref(), Some("hash table"));
        assert_eq!(hashes.lookup(Table::BinHashes, entry), None);
        assert_eq!(hashes.value_name(BinHash(1)), None);
    }

    #[test]
    fn parse_hash_accepts_hex_with_or_without_prefix() {
        assert_eq!(parse_hash("0x4a47c414"), Some(BinHash(0x4a47_c414)));
        assert_eq!(parse_hash("4A47C414"), Some(BinHash(0x4a47_c414)));
        assert_eq!(parse_hash("0X1f"), Some(BinHash(0x1f)));
    }

    #[test]
    fn parse_hash_rejects_invalid_input() {
        assert_eq!(parse_hash(""), None);
        assert_eq!(parse_hash("0x"), None);
        assert_eq!(parse_hash("0x123456789"), None);
        assert_eq!(parse_hash("mName"), None);
    }

    #[test]
    fn empty_bin_hashes_resolve_no_hash() {
        let hashes = BinHashes::none();
        assert!(hashes.is_empty());
        assert_eq!(hashes.lookup_field(BinHash(0x1234)), None);
        assert_eq!(FieldNames::field(&hashes, BinHash(0x1234), None), None);
    }
}
