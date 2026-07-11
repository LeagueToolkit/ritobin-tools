use std::{borrow::Cow, rc::Rc};

use camino::Utf8PathBuf;
use ltk_hashdb::HashDb;
use ltk_mimir_cache::{HashStore, Table};
use ltk_ritobin::HashMapProvider;

#[derive(Clone)]
pub enum HashProvider {
    None,
    Mimir(Rc<MimirProvider>),
    CDragon(Rc<HashMapProvider>),
}

impl HashProvider {
    pub fn new(hashtable_dir: Option<&Utf8PathBuf>, store: Option<&HashStore>) -> Self {
        match store.map(|s| MimirProvider::new(s)) {
            Some(mimir) => Self::Mimir(mimir.into()),
            None => Self::CDragon(Rc::new({
                let Some(hashtable_dir) = hashtable_dir else {
                    return Self::None;
                };

                let mut hashtable_provider = HashMapProvider::new();
                hashtable_provider.load_from_directory(hashtable_dir);
                hashtable_provider
            })),
        }
    }
}

impl ltk_ritobin::HashProvider for HashProvider {
    fn lookup_entry(&self, hash: ltk_hash::BinHash) -> Option<Cow<'_, str>> {
        match self {
            Self::Mimir(p) => p.lookup_entry(hash),
            Self::CDragon(p) => p.lookup_entry(hash),
            Self::None => None,
        }
    }

    fn lookup_field(&self, hash: ltk_hash::BinHash) -> Option<Cow<'_, str>> {
        match self {
            Self::Mimir(p) => p.lookup_field(hash),
            Self::CDragon(p) => p.lookup_field(hash),
            Self::None => None,
        }
    }

    fn lookup_hash(&self, hash: ltk_hash::BinHash) -> Option<Cow<'_, str>> {
        match self {
            Self::Mimir(p) => p.lookup_hash(hash),
            Self::CDragon(p) => p.lookup_hash(hash),
            Self::None => None,
        }
    }

    fn lookup_type(&self, hash: ltk_hash::BinHash) -> Option<Cow<'_, str>> {
        match self {
            Self::Mimir(p) => p.lookup_type(hash),
            Self::CDragon(p) => p.lookup_type(hash),
            Self::None => None,
        }
    }
}

pub struct MimirProvider {
    pub entries: Option<HashDb>,
    pub fields: Option<HashDb>,
    pub hashes: Option<HashDb>,
    pub types: Option<HashDb>,
}

impl MimirProvider {
    pub fn new(store: &HashStore) -> Self {
        let open = |table: Table| {
            store
                .open(table)
                .inspect_err(|e| tracing::warn!("Failed to load hashes for {table:?} table - {e}"))
                .ok()
        };
        Self {
            entries: open(Table::BinEntries),
            fields: open(Table::BinFields),
            hashes: open(Table::BinHashes),
            types: open(Table::BinTypes),
        }
    }
}

impl ltk_ritobin::HashProvider for MimirProvider {
    fn lookup_entry(&self, hash: ltk_hash::BinHash) -> Option<Cow<'_, str>> {
        self.entries.as_ref().and_then(|h| h.get((*hash).into()))
    }

    fn lookup_field(&self, hash: ltk_hash::BinHash) -> Option<Cow<'_, str>> {
        self.fields.as_ref().and_then(|h| h.get((*hash).into()))
    }

    fn lookup_hash(&self, hash: ltk_hash::BinHash) -> Option<Cow<'_, str>> {
        self.hashes.as_ref().and_then(|h| h.get((*hash).into()))
    }

    fn lookup_type(&self, hash: ltk_hash::BinHash) -> Option<Cow<'_, str>> {
        self.types.as_ref().and_then(|h| h.get((*hash).into()))
    }
}
