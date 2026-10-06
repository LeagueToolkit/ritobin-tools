//! The installed game: which archive holds each chunk, which chunk declares each bin object, and
//! the chunks themselves.
//!
//! Both indexes come from `ltk_game_index`. They are cached on disk, and a cache is used only
//! while the archives it was built from have not changed.

use std::{
    cell::{OnceCell, RefCell},
    collections::HashMap,
    fs::File,
    io::{BufReader, Cursor},
    rc::Rc,
};

use camino::{Utf8Path, Utf8PathBuf};
use ltk_game_index::{ArchiveId, BuildOptions, GameIndex, ObjectIndex, chunk_hash};
use ltk_hash::{BinHash, WadHash};
use ltk_meta::{BinFile, BinObject};
use ltk_wad::Wad;
use miette::{IntoDiagnostic, Result, WrapErr};

use crate::hashes::WadPaths;

/// The directory of the archives, under the game directory.
const ARCHIVES_DIR: &str = "DATA/FINAL";

const CHUNK_INDEX_FILE: &str = "game_index.bin";
const OBJECT_INDEX_FILE: &str = "object_index.bin";

pub struct Game {
    dir: Utf8PathBuf,
    index: GameIndex,
    /// Where the indexes of this game directory are cached.
    index_dir: Utf8PathBuf,
    paths: WadPaths,
    /// Built when the first object is asked for.
    objects: OnceCell<ObjectIndex>,
    archives: RefCell<HashMap<ArchiveId, Wad<BufReader<File>>>>,
    /// The decoded bins objects were read from.
    bins: RefCell<HashMap<(ArchiveId, WadHash), Rc<BinFile>>>,
}

impl Game {
    /// Opens the game at `dir`: the `Game` directory of an installation, or the directory that
    /// holds it.
    ///
    /// `index_dir` is where the indexes are cached, in place of the user's data directory.
    /// `paths` names chunks, which lets the object index skip the chunks that are not bins.
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
                    tracing::debug!("The chunk index is built again: {error}");
                }
                let index = GameIndex::build(&dir)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("{dir} is not a game directory"))?;
                if let Err(error) = index.save(&cache) {
                    tracing::warn!("The chunk index was not cached: {error}");
                }
                index
            }
        };
        for skipped in index.skipped() {
            tracing::warn!(
                "{} cannot be read and is left out: {}",
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

    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    /// The path of `chunk` when the hashtables have it, else its hash as 16 hex digits.
    pub fn chunk_name(&self, chunk: WadHash) -> String {
        self.paths
            .path(chunk)
            .unwrap_or_else(|| format!("{:016x}", chunk.0))
    }

    /// The game's copy of `chunk`: the one in the first archive that holds it. `None` for a
    /// chunk no archive holds.
    pub fn chunk(&self, chunk: WadHash) -> Result<Option<Vec<u8>>> {
        match self.index.row(chunk) {
            Some(row) => self.read(row.first_holder(), chunk).map(Some),
            None => Ok(None),
        }
    }

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

        // The index says the archive holds the chunk. One that does not has changed since.
        let entry = *wad.chunks().get(chunk).ok_or_else(|| {
            miette::miette!(
                "{name} no longer holds {}. The game changed while it was read",
                self.chunk_name(chunk)
            )
        })?;
        let data = wad
            .load_chunk_decompressed(&entry)
            .into_diagnostic()
            .wrap_err_with(|| format!("Failed to read {} from {name}", self.chunk_name(chunk)))?;
        Ok(data.into_vec())
    }

    /// Every bin object of the game with the chunks that declare it.
    ///
    /// The first call reads the cached index, or reads every bin of the game to build it.
    pub fn objects(&self) -> &ObjectIndex {
        self.objects.get_or_init(|| {
            let cache = self.index_dir.join(OBJECT_INDEX_FILE);
            match ObjectIndex::load_for(&cache, &self.index) {
                Ok(objects) => objects,
                Err(error) => {
                    if !error.is_missing_file() {
                        tracing::debug!("The object index is built again: {error}");
                    }
                    tracing::info!(
                        "Indexing the bin objects of {}. This is done once for each game patch.",
                        self.dir
                    );
                    let mut options = BuildOptions::default();
                    if self.paths.is_loaded() {
                        options.resolver = Some(&self.paths);
                    }
                    let objects = ObjectIndex::build_with(&self.index, &options)
                        .expect("a build that is never called off finishes");
                    if let Err(error) = objects.save(&cache) {
                        tracing::warn!("The object index was not cached: {error}");
                    }
                    let stats = objects.stats();
                    tracing::info!(
                        "Indexed {} objects of {} bins in {:.1} s",
                        objects.len(),
                        stats.bins,
                        stats.elapsed.as_secs_f32()
                    );
                    objects
                }
            }
        })
    }

    /// The chunks that declare `object`, in archive order, each one once.
    pub fn declaring_chunks(&self, object: BinHash) -> Vec<WadHash> {
        let mut chunks = Vec::new();
        for declaration in self.objects().declarations(object) {
            if !chunks.contains(&declaration.chunk) {
                chunks.push(declaration.chunk);
            }
        }
        chunks
    }

    /// The game's copy of `object`: the one in the first chunk that declares it. `None` for an
    /// object no chunk declares.
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

/// The game directory `dir` names: itself, or the `Game` directory in it when `dir` is the
/// directory of the whole installation.
fn game_dir(dir: &Utf8Path) -> Utf8PathBuf {
    let nested = dir.join("Game");
    match !dir.join(ARCHIVES_DIR).is_dir() && nested.join(ARCHIVES_DIR).is_dir() {
        true => nested,
        false => dir.to_owned(),
    }
}

/// Where the indexes of the game at `dir` are cached: a directory of its own, next to the
/// hashtable cache every LeagueToolkit tool shares.
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

    // Two game directories never share a cache, which would be built again on every switch.
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
    //! A game directory with archives written for a test.

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

        /// Writes the archive `name`, under `DATA/FINAL`, holding `chunks` as paths with bytes.
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

        /// The game, with its indexes cached inside the installation.
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
    fn a_chunk_is_read_from_the_first_archive_that_holds_it() {
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
    fn an_object_is_found_through_the_chunks_that_declare_it() {
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
    fn the_indexes_are_cached_and_read_back() {
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
    fn a_directory_without_archives_is_not_a_game() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let error = Game::open(dir, Some(&dir.join("index")), WadPaths::default())
            .err()
            .unwrap();
        assert!(error.to_string().contains("is not a game directory"));
    }
}
