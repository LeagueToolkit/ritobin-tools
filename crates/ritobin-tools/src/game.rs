//! Read access to an installed game: chunk lookup, bin object lookup and chunk data.
//!
//! Lookups use the chunk index and the object index of `ltk_game_index`. Both indexes are cached
//! on disk. A cached index is rebuilt when the archive set of the game has changed.

use std::{
    cell::{OnceCell, RefCell},
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{BufReader, Cursor},
    rc::Rc,
};

use camino::{Utf8Path, Utf8PathBuf};
use ltk_game_data::EntryName;
use ltk_game_index::{ArchiveId, BuildOptions, GameIndex, ObjectIndex, chunk_hash};
use ltk_hash::{BinHash, WadHash};
use ltk_meta::{BinFile, BinObject};
use ltk_wad::Wad;
use miette::{IntoDiagnostic, Result, WrapErr};

use crate::hashes::WadPaths;

/// The archive directory, relative to the game directory.
const ARCHIVES_DIR: &str = "DATA/FINAL";

const CHUNK_INDEX_FILE: &str = "game_index.bin";
const OBJECT_INDEX_FILE: &str = "object_index.bin";

/// One archive of the game and its bin chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveBins {
    /// The archive path relative to `DATA/FINAL`, with `/` separators.
    pub name: String,
    /// The absolute path of the archive file.
    pub path: Utf8PathBuf,
    pub chunks: Vec<WadHash>,
}

/// A selection of game bins, parsed from a command line argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinRef {
    /// The bin chunk with the hash `chunk`.
    Chunk {
        chunk: WadHash,
        /// The chunk path as written, with `/` separators, or the chunk hash as written.
        name: String,
    },
    /// Every bin that declares the object with the path hash `object`.
    Entry {
        object: BinHash,
        /// The entry path or the entry hash, as written.
        name: String,
    },
}

impl BinRef {
    /// Parses `text` as a selection of game bins.
    ///
    /// A text that ends with `.bin` is a chunk path. A text of 16 hex digits is a chunk hash.
    /// Any other text is an entry: an object path, or an object path hash written as `0x` and
    /// 8 hex digits. Fails if `text` is empty.
    pub fn parse(text: &str) -> Result<Self> {
        let is_chunk_hash = text.len() == 16 && text.bytes().all(|byte| byte.is_ascii_hexdigit());
        if is_chunk_hash && let Ok(hash) = u64::from_str_radix(text, 16) {
            return Ok(Self::Chunk {
                chunk: WadHash(hash),
                name: text.to_owned(),
            });
        }
        if text.to_ascii_lowercase().ends_with(".bin") {
            let name = text.replace('\\', "/");
            return Ok(Self::Chunk {
                chunk: chunk_hash(&name),
                name,
            });
        }
        let entry = EntryName::try_from(text)
            .map_err(|error| miette::miette!("Invalid game bin `{text}`: {error}"))?;
        Ok(Self::Entry {
            object: entry.object_hash(),
            name: text.to_owned(),
        })
    }
}

pub struct Game {
    dir: Utf8PathBuf,
    index: GameIndex,
    /// The cache directory of the index files.
    index_dir: Utf8PathBuf,
    paths: WadPaths,
    /// Loaded or built on the first call to [`Game::objects`].
    objects: OnceCell<ObjectIndex>,
    /// Mounted archives, kept open between chunk reads.
    archives: RefCell<HashMap<ArchiveId, Wad<BufReader<File>>>>,
    /// Decoded bins, cached by [`Game::object`].
    bins: RefCell<HashMap<(ArchiveId, WadHash), Rc<BinFile>>>,
}

impl Game {
    /// Opens the game at `dir` and loads or builds its chunk index.
    ///
    /// `dir` is the `Game` directory of an installation or its parent directory. `index_dir`
    /// overrides the default index cache directory. `paths` resolves chunk hashes to paths. The
    /// object index build uses it to skip chunks whose path is not a `.bin`.
    pub fn open(dir: &Utf8Path, index_dir: Option<&Utf8Path>, paths: WadPaths) -> Result<Self> {
        let dir = game_dir(dir);
        let index_dir = match index_dir {
            Some(index_dir) => index_dir.to_owned(),
            None => default_index_dir(&dir)?,
        };

        let cache = index_dir.join(CHUNK_INDEX_FILE);
        let index = match GameIndex::load_for(&cache, &dir) {
            Ok(index) => index,
            Err(error) => {
                if !error.is_missing_file() {
                    tracing::debug!("Rebuilding the chunk index: {error}");
                }
                let index = GameIndex::build(&dir)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("{dir} is not a game directory"))?;
                if let Err(error) = index.save(&cache) {
                    tracing::warn!("Failed to save the chunk index cache: {error}");
                }
                index
            }
        };
        for skipped in index.skipped() {
            tracing::warn!(
                "Skipped unreadable archive {}: {}",
                index.archive(skipped.archive).name,
                skipped.error
            );
        }

        Ok(Self {
            dir,
            index,
            index_dir,
            paths,
            objects: OnceCell::new(),
            archives: RefCell::default(),
            bins: RefCell::default(),
        })
    }

    /// Returns the resolved `Game` directory.
    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    /// Returns the path of `chunk` from the hashtables. Falls back to the hash as 16 hex digits.
    pub fn chunk_name(&self, chunk: WadHash) -> String {
        self.paths
            .path(chunk)
            .unwrap_or_else(|| format!("{:016x}", chunk.0))
    }

    /// Reads the decompressed data of `chunk` from the first archive that contains it, in
    /// archive name order. Returns `None` if no archive contains the chunk.
    pub fn chunk(&self, chunk: WadHash) -> Result<Option<Vec<u8>>> {
        match self.index.row(chunk) {
            Some(row) => self.read(row.first_holder(), chunk).map(Some),
            None => Ok(None),
        }
    }

    /// Reads the decompressed data of `chunk` from `archive`. Mounts the archive on first use.
    fn read(&self, archive: ArchiveId, chunk: WadHash) -> Result<Vec<u8>> {
        let name = &self.index.archive(archive).name;
        let mut archives = self.archives.borrow_mut();
        let wad = match archives.entry(archive) {
            std::collections::hash_map::Entry::Occupied(mounted) => mounted.into_mut(),
            std::collections::hash_map::Entry::Vacant(slot) => {
                let file = File::open(&self.index.archive(archive).path)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("Failed to open {name}"))?;
                let wad = Wad::mount(BufReader::new(file))
                    .into_diagnostic()
                    .wrap_err_with(|| format!("Failed to read {name}"))?;
                slot.insert(wad)
            }
        };

        // The index lists this chunk in this archive. If the archive does not contain it, the
        // archive was modified after the index was validated.
        let entry = *wad.chunks().get(chunk).ok_or_else(|| {
            miette::miette!(
                "{name} does not contain {}. The archive changed after it was indexed",
                self.chunk_name(chunk)
            )
        })?;
        let data = wad
            .load_chunk_decompressed(&entry)
            .into_diagnostic()
            .wrap_err_with(|| format!("Failed to read {} from {name}", self.chunk_name(chunk)))?;
        Ok(data.into_vec())
    }

    /// Returns the object index, which maps each bin object to the chunks that declare it.
    ///
    /// The first call loads the index from the cache. If the cache is missing or stale, it builds
    /// the index by reading every bin chunk of the game, then saves it.
    pub fn objects(&self) -> &ObjectIndex {
        self.objects.get_or_init(|| {
            let cache = self.index_dir.join(OBJECT_INDEX_FILE);
            match ObjectIndex::load_for(&cache, &self.index) {
                Ok(objects) => objects,
                Err(error) => {
                    if !error.is_missing_file() {
                        tracing::debug!("Rebuilding the object index: {error}");
                    }
                    tracing::info!(
                        "Building the object index of {}. It is rebuilt after each game patch.",
                        self.dir
                    );
                    let mut options = BuildOptions::default();
                    if self.paths.is_loaded() {
                        options.resolver = Some(&self.paths);
                    }
                    // `build_with` fails only if the `called_off` callback cancels the build.
                    // No callback is set.
                    let objects = ObjectIndex::build_with(&self.index, &options)
                        .expect("the build is not cancellable");
                    if let Err(error) = objects.save(&cache) {
                        tracing::warn!("Failed to save the object index cache: {error}");
                    }
                    let stats = objects.stats();
                    tracing::info!(
                        "Indexed {} objects from {} bins in {:.1} s",
                        objects.len(),
                        stats.bins,
                        stats.elapsed.as_secs_f32()
                    );
                    objects
                }
            }
        })
    }

    /// Returns the bin chunks of the game, grouped by archive, in archive name order. The chunks
    /// of an archive are in ascending hash order.
    ///
    /// The list comes from the object index. Each chunk is listed once, under the archive that
    /// the index read it from. A bin that declares no object is not listed.
    pub fn bin_archives(&self) -> Vec<ArchiveBins> {
        let objects = self.objects();
        let mut chunks = BTreeSet::new();
        for object in objects.objects() {
            for declaration in objects.declarations(object) {
                chunks.insert((declaration.archive, declaration.chunk));
            }
        }

        let mut archives: Vec<ArchiveBins> = Vec::new();
        let mut last = None;
        for (archive, chunk) in chunks {
            if last != Some(archive) {
                let entry = self.index.archive(archive);
                archives.push(ArchiveBins {
                    name: entry.name.clone(),
                    path: entry.path.clone(),
                    chunks: Vec::new(),
                });
                last = Some(archive);
            }
            if let Some(bins) = archives.last_mut() {
                bins.chunks.push(chunk);
            }
        }
        archives
    }

    /// Returns the chunks that declare `object`, deduplicated, in object index order.
    pub fn declaring_chunks(&self, object: BinHash) -> Vec<WadHash> {
        let mut chunks = Vec::new();
        for declaration in self.objects().declarations(object) {
            if !chunks.contains(&declaration.chunk) {
                chunks.push(declaration.chunk);
            }
        }
        chunks
    }

    /// Returns the chunks that `bins` selects. Returns an empty list if no archive contains the
    /// chunk, or if no chunk declares the entry.
    ///
    /// A chunk selection uses the chunk index only. An entry selection loads the object index.
    pub fn select(&self, bins: &BinRef) -> Vec<WadHash> {
        match bins {
            BinRef::Chunk { chunk, .. } => match self.index.contains(*chunk) {
                true => vec![*chunk],
                false => Vec::new(),
            },
            BinRef::Entry { object, .. } => self.declaring_chunks(*object),
        }
    }

    /// Reads `object` from the first chunk that declares it, in object index order. Returns
    /// `None` if no chunk declares the object.
    pub fn object(&self, object: BinHash) -> Result<Option<BinObject>> {
        let Some(declaration) = self.objects().declarations(object).first() else {
            return Ok(None);
        };
        let key = (declaration.archive, declaration.chunk);

        let cached = self.bins.borrow().get(&key).cloned();
        let bin = match cached {
            Some(bin) => bin,
            None => {
                let data = self.read(declaration.archive, declaration.chunk)?;
                let bin = BinFile::from_reader(&mut Cursor::new(data))
                    .into_diagnostic()
                    .wrap_err_with(|| {
                        format!(
                            "{} is not a valid bin file",
                            self.chunk_name(declaration.chunk)
                        )
                    })?;
                let bin = Rc::new(bin);
                self.bins.borrow_mut().insert(key, Rc::clone(&bin));
                bin
            }
        };
        let objects = match bin.as_ref() {
            BinFile::Prop(bin) => &bin.objects,
            BinFile::Override(patch) => &patch.objects,
        };
        Ok(objects.get(&object).cloned())
    }
}

/// Returns `dir/Game` if `dir` has no `DATA/FINAL` directory and `dir/Game` has one. Otherwise
/// returns `dir`.
fn game_dir(dir: &Utf8Path) -> Utf8PathBuf {
    let nested = dir.join("Game");
    match !dir.join(ARCHIVES_DIR).is_dir() && nested.join(ARCHIVES_DIR).is_dir() {
        true => nested,
        false => dir.to_owned(),
    }
}

/// Returns the default index cache directory for the game at `dir`:
/// `<user data dir>/LeagueToolkit/game-index/<hash of the absolute game path>`.
fn default_index_dir(dir: &Utf8Path) -> Result<Utf8PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| {
        miette::miette!("Could not find the user data directory; pass --index-dir")
    })?;
    let root = match cfg!(windows) {
        true => base.data_local_dir(),
        false => base.data_dir(),
    };
    let root = Utf8Path::from_path(root)
        .ok_or_else(|| miette::miette!("The user data directory is not UTF-8; pass --index-dir"))?;

    // Each game directory gets its own cache directory. With a shared one, switching between
    // two installations would invalidate and rebuild the cache every time.
    let absolute = std::path::absolute(dir)
        .map(|absolute| absolute.to_string_lossy().into_owned())
        .unwrap_or_else(|_| dir.to_string());
    Ok(root
        .join("LeagueToolkit")
        .join("game-index")
        .join(format!("{:016x}", chunk_hash(&absolute).0)))
}

#[cfg(test)]
pub mod testing {
    //! Test fixture: a temporary game directory with archives built by `ltk_wad`.

    use std::{collections::BTreeMap, io::Write as _};

    use ltk_wad::{WadBuilder, WadChunkBuilder, WadChunkCompression};

    use super::*;

    pub struct Installation {
        _guard: tempfile::TempDir,
        pub root: Utf8PathBuf,
    }

    impl Installation {
        pub fn new() -> Self {
            let guard = tempfile::tempdir().unwrap();
            let root = Utf8PathBuf::from_path_buf(guard.path().to_path_buf()).unwrap();
            std::fs::create_dir_all(root.join("Game").join(ARCHIVES_DIR)).unwrap();
            Self {
                _guard: guard,
                root,
            }
        }

        /// Writes the archive `DATA/FINAL/<name>` containing `chunks`, given as
        /// `(chunk path, data)` pairs.
        pub fn archive(&self, name: &str, chunks: &[(&str, &[u8])]) {
            let path = self.root.join("Game").join(ARCHIVES_DIR).join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();

            let mut builder = WadBuilder::default();
            for (chunk, _) in chunks {
                builder = builder.with_chunk(
                    WadChunkBuilder::default()
                        .with_path(*chunk)
                        .with_force_compression(WadChunkCompression::None),
                );
            }
            let data: BTreeMap<WadHash, Vec<u8>> = chunks
                .iter()
                .map(|(chunk, bytes)| (chunk_hash(chunk), bytes.to_vec()))
                .collect();
            let mut out = Cursor::new(Vec::new());
            builder
                .build_to_writer(&mut out, move |hash, writer| {
                    writer.write_all(&data[&hash])?;
                    Ok(())
                })
                .unwrap();
            std::fs::write(path, out.into_inner()).unwrap();
        }

        /// Opens the game with the index cache in `<root>/index`.
        pub fn open(&self) -> Game {
            Game::open(
                &self.root,
                Some(&self.root.join("index")),
                WadPaths::default(),
            )
            .unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use ltk_meta::{Bin, property::values};

    use super::{testing::Installation, *};
    use crate::document::to_bin;

    fn bin(object: u32, value: i32) -> Vec<u8> {
        let bin: BinFile = Bin::builder()
            .object(
                BinObject::builder(object, 0xaaaa_0001u32)
                    .property(0x10u32, values::I32::new(value))
                    .build(),
            )
            .build()
            .into();
        to_bin(&bin).unwrap()
    }

    #[test]
    fn chunk_reads_from_first_archive_containing_it() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/shared.bin", &bin(1, 10))]);
        installation.archive("B.wad.client", &[("data/shared.bin", &bin(1, 20))]);

        let game = installation.open();
        assert_eq!(game.dir(), installation.root.join("Game"));
        assert_eq!(
            game.chunk(chunk_hash("data/shared.bin")).unwrap(),
            Some(bin(1, 10))
        );
        assert_eq!(game.chunk(chunk_hash("data/missing.bin")).unwrap(), None);
    }

    #[test]
    fn object_lookup_uses_declaring_chunks() {
        let installation = Installation::new();
        installation.archive(
            "A.wad.client",
            &[
                ("data/one.bin", &bin(1, 10)),
                ("data/two.bin", &bin(2, 20)),
                ("data/notes.txt", b"not a bin"),
            ],
        );
        installation.archive("B.wad.client", &[("data/other.bin", &bin(2, 30))]);

        let game = installation.open();
        assert_eq!(
            game.declaring_chunks(BinHash(2)),
            [chunk_hash("data/two.bin"), chunk_hash("data/other.bin")]
        );
        assert!(game.declaring_chunks(BinHash(3)).is_empty());

        let object = game.object(BinHash(2)).unwrap().unwrap();
        assert_eq!(
            object.properties.get(&BinHash(0x10)),
            Some(&values::I32::new(20).into())
        );
        assert_eq!(game.object(BinHash(3)).unwrap(), None);
    }

    #[test]
    fn bin_ref_parse_classifies_chunk_path_chunk_hash_and_entry() {
        assert_eq!(
            BinRef::parse("DATA\\Characters\\Teemo\\Skins\\Skin0.BIN").unwrap(),
            BinRef::Chunk {
                chunk: chunk_hash("data/characters/teemo/skins/skin0.bin"),
                name: "DATA/Characters/Teemo/Skins/Skin0.BIN".to_owned(),
            }
        );
        assert_eq!(
            BinRef::parse("00000000000000ff").unwrap(),
            BinRef::Chunk {
                chunk: WadHash(0xff),
                name: "00000000000000ff".to_owned(),
            }
        );
        assert_eq!(
            BinRef::parse("Characters/Teemo/Skins/Skin0").unwrap(),
            BinRef::Entry {
                object: BinHash(0x591c_bdbd),
                name: "Characters/Teemo/Skins/Skin0".to_owned(),
            }
        );
        assert_eq!(
            BinRef::parse("0x591cbdbd").unwrap(),
            BinRef::Entry {
                object: BinHash(0x591c_bdbd),
                name: "0x591cbdbd".to_owned(),
            }
        );
        assert!(BinRef::parse("").is_err());
    }

    #[test]
    fn select_returns_chunk_of_path_and_declaring_chunks_of_entry() {
        let installation = Installation::new();
        installation.archive(
            "A.wad.client",
            &[("data/one.bin", &bin(1, 10)), ("data/two.bin", &bin(2, 20))],
        );
        installation.archive("B.wad.client", &[("data/other.bin", &bin(2, 30))]);
        let game = installation.open();
        let select = |text: &str| game.select(&BinRef::parse(text).unwrap());

        assert_eq!(select("data/One.bin"), [chunk_hash("data/one.bin")]);
        assert_eq!(
            select(&format!("{:016x}", chunk_hash("data/two.bin").0)),
            [chunk_hash("data/two.bin")]
        );
        assert_eq!(
            select("0x00000002"),
            [chunk_hash("data/two.bin"), chunk_hash("data/other.bin")]
        );
        assert!(select("data/missing.bin").is_empty());
        assert!(select("0x00000003").is_empty());
    }

    #[test]
    fn bin_archives_list_each_bin_chunk_under_one_archive() {
        let installation = Installation::new();
        installation.archive(
            "B.wad.client",
            &[
                ("data/shared.bin", &bin(1, 10)),
                ("data/two.bin", &bin(2, 20)),
            ],
        );
        installation.archive(
            "A.wad.client",
            &[
                ("data/shared.bin", &bin(1, 10)),
                ("data/notes.txt", b"not a bin"),
            ],
        );

        let game = installation.open();
        let archives = game.bin_archives();
        let listed: Vec<(&str, &[WadHash])> = archives
            .iter()
            .map(|archive| (archive.name.as_str(), archive.chunks.as_slice()))
            .collect();
        // `data/shared.bin` is in both archives. The index reads it from the first one.
        assert_eq!(
            listed,
            [
                ("A.wad.client", [chunk_hash("data/shared.bin")].as_slice()),
                ("B.wad.client", [chunk_hash("data/two.bin")].as_slice()),
            ]
        );
        assert!(archives[0].path.ends_with("A.wad.client"));
    }

    #[test]
    fn indexes_are_saved_and_reloaded_from_cache() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/one.bin", &bin(1, 10))]);

        let first = installation.open();
        assert_eq!(first.objects().len(), 1);
        drop(first);
        assert!(
            installation
                .root
                .join("index")
                .join(CHUNK_INDEX_FILE)
                .exists()
        );
        assert!(
            installation
                .root
                .join("index")
                .join(OBJECT_INDEX_FILE)
                .exists()
        );

        let second = installation.open();
        assert_eq!(second.declaring_chunks(BinHash(1)).len(), 1);
    }

    #[test]
    fn open_fails_without_archive_directory() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let error = Game::open(dir, Some(&dir.join("index")), WadPaths::default())
            .err()
            .unwrap();
        assert!(error.to_string().contains("is not a game directory"));
    }
}
