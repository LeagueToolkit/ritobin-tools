//! Reads the bins inside a WAD archive or a mod package: a `.fantome` file or a `.modpkg` file.

use std::{
    fs::File,
    io::{BufReader, Cursor, Read, Seek},
};

use camino::Utf8Path;
use ltk_modpkg::Modpkg;
use ltk_wad::Wad;
use miette::{IntoDiagnostic, Result, WrapErr};
use zip::ZipArchive;

use crate::{
    document::{BIN_EXTENSION, Format},
    hashes::WadPaths,
};

/// The file name endings of a WAD archive.
const WAD_ENDINGS: [&str; 3] = [".wad.client", ".wad.mobile", ".wad"];

/// The kind of a file that contains bins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    /// A WAD archive: `.wad.client`, `.wad.mobile` or `.wad`.
    Wad,
    /// A Fantome mod: a `.fantome` zip file.
    Fantome,
    /// A LeagueToolkit mod package: a `.modpkg` file.
    Modpkg,
}

impl PackageKind {
    /// Returns the kind of the file at `path`, from the ending of its file name. Returns `None`
    /// for any other file.
    pub fn of(path: &Utf8Path) -> Option<Self> {
        let name = path.file_name()?.to_ascii_lowercase();
        if is_wad_name(&name) {
            Some(Self::Wad)
        } else if name.ends_with(".fantome") {
            Some(Self::Fantome)
        } else if name.ends_with(".modpkg") {
            Some(Self::Modpkg)
        } else {
            None
        }
    }
}

/// One bin of a package.
pub struct PackagedBin<'a> {
    /// The part of the package that contains the bin: the WAD inside a mod package, with the
    /// layer of a `.modpkg` file. `None` for a bin of a WAD archive.
    pub part: Option<&'a str>,
    /// The path of the bin, or its chunk hash as 16 hex digits if the path is unknown.
    pub name: &'a str,
    /// The decompressed data of the bin, or the error of reading it.
    pub data: Result<Vec<u8>>,
}

/// The callbacks of [`for_each_bin`].
pub struct Visit<'a> {
    /// Returns `false` for a bin name that is skipped. A skipped bin is not read.
    pub wants: &'a dyn Fn(&str) -> bool,
    /// Receives each bin. Returns `false` to stop.
    pub bin: &'a mut dyn FnMut(PackagedBin<'_>) -> bool,
}

/// Calls `visit.bin` for each bin of the package at `path`, in a fixed order.
///
/// A chunk with a known path is a bin if the path ends with `.bin`. `paths` resolves the chunk
/// hashes of a WAD archive. A chunk without a known path is read, and it is a bin if its data
/// starts with a bin magic.
///
/// Fails if the package cannot be opened.
pub fn for_each_bin(
    path: &Utf8Path,
    kind: PackageKind,
    paths: &WadPaths,
    visit: &mut Visit<'_>,
) -> Result<()> {
    let file = File::open(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to open {path}"))?;
    let invalid = |format: &str| format!("{path} is not a valid {format}");
    match kind {
        PackageKind::Wad => {
            let mut wad = Wad::mount(BufReader::new(file))
                .into_diagnostic()
                .wrap_err_with(|| invalid("WAD archive"))?;
            wad_bins(&mut wad, None, paths, visit);
        }
        PackageKind::Fantome => {
            let mut zip = ZipArchive::new(BufReader::new(file))
                .into_diagnostic()
                .wrap_err_with(|| invalid("Fantome file"))?;
            fantome_bins(&mut zip, paths, visit);
        }
        PackageKind::Modpkg => {
            let mut modpkg = Modpkg::mount_from_reader(BufReader::new(file))
                .into_diagnostic()
                .wrap_err_with(|| invalid("modpkg file"))?;
            modpkg_bins(&mut modpkg, visit);
        }
    }
    Ok(())
}

/// Returns `true` if the lowercased file name `name` has the ending of a WAD archive.
fn is_wad_name(name: &str) -> bool {
    WAD_ENDINGS.iter().any(|ending| name.ends_with(ending))
}

/// Returns `true` if `name` ends with the bin extension.
fn is_bin_name(name: &str) -> bool {
    name.to_ascii_lowercase()
        .ends_with(&format!(".{BIN_EXTENSION}"))
}

/// A chunk of a package before its data is read.
struct Listed<K> {
    key: K,
    /// The path, or the chunk hash as 16 hex digits.
    name: String,
    /// `true` if `name` is a path. `false` if the path is unknown.
    named: bool,
}

/// Returns the chunks of `listed` that can be bins and that `visit.wants` accepts, sorted by
/// name. A chunk with a known path that does not end with `.bin` is not a bin.
fn candidates<K>(listed: impl Iterator<Item = Listed<K>>, visit: &Visit<'_>) -> Vec<Listed<K>> {
    let mut chunks: Vec<Listed<K>> = listed
        .filter(|chunk| !chunk.named || is_bin_name(&chunk.name))
        .filter(|chunk| (visit.wants)(&chunk.name))
        .collect();
    chunks.sort_by(|a, b| a.name.cmp(&b.name));
    chunks
}

/// Passes one chunk to `visit.bin`. Returns `false` if the visit stops.
///
/// A chunk without a known path is passed only if its data starts with a bin magic. Such a
/// chunk that cannot be read is skipped, because it is most likely not a bin.
fn offer<K>(
    chunk: &Listed<K>,
    part: Option<&str>,
    data: Result<Vec<u8>>,
    visit: &mut Visit<'_>,
) -> bool {
    let is_bin = match &data {
        Ok(data) => chunk.named || Format::detect(data) == Format::Bin,
        Err(_) => chunk.named,
    };
    !is_bin
        || (visit.bin)(PackagedBin {
            part,
            name: &chunk.name,
            data,
        })
}

/// Visits the bins of a mounted WAD archive. `part` is the name of the archive inside a mod
/// package. Returns `false` if the visit stops.
fn wad_bins<R: Read + Seek>(
    wad: &mut Wad<R>,
    part: Option<&str>,
    paths: &WadPaths,
    visit: &mut Visit<'_>,
) -> bool {
    let listed = wad.chunks().iter().map(|chunk| {
        let hash = chunk.path_hash();
        let path = paths.path(hash);
        Listed {
            key: hash,
            named: path.is_some(),
            name: path.unwrap_or_else(|| format!("{:016x}", hash.0)),
        }
    });
    for chunk in candidates(listed, visit) {
        let data = match wad.chunks().get(chunk.key).copied() {
            Some(entry) => wad
                .load_chunk_decompressed(&entry)
                .map(|data| data.into_vec())
                .into_diagnostic(),
            None => continue,
        };
        if !offer(&chunk, part, data, visit) {
            return false;
        }
    }
    true
}

/// Visits the bins of a Fantome file.
///
/// A Fantome file has one directory per layer: `WAD` for the base layer and `WAD_<layer>` for
/// any other layer. Each entry below it is a packed WAD archive, or a file inside a directory
/// that has the name of a WAD archive. The `RAW` directory has files by their game path.
fn fantome_bins<R: Read + Seek>(zip: &mut ZipArchive<R>, paths: &WadPaths, visit: &mut Visit<'_>) {
    let mut names: Vec<(usize, String)> = (0..zip.len())
        .filter_map(|index| {
            let entry = zip.by_index_raw(index).ok()?;
            entry
                .is_file()
                .then(|| (index, entry.name().replace('\\', "/")))
        })
        .collect();
    names.sort_by(|a, b| a.1.cmp(&b.1));

    for (index, name) in names {
        let lower = name.to_ascii_lowercase();
        let Some((area, inside)) = lower.split_once('/') else {
            continue;
        };
        let in_wads = area == "wad" || area.starts_with("wad_");
        let read = |zip: &mut ZipArchive<R>| -> Result<Vec<u8>> {
            let mut data = Vec::new();
            zip.by_index(index)
                .into_diagnostic()?
                .read_to_end(&mut data)
                .into_diagnostic()?;
            Ok(data)
        };

        if in_wads && is_wad_name(inside) && !inside.contains('/') {
            // A packed WAD archive. A WAD that cannot be read is reported as one failed bin.
            let mounted = read(zip).and_then(|data| {
                Wad::mount(Cursor::new(data))
                    .into_diagnostic()
                    .wrap_err_with(|| format!("{name} is not a valid WAD archive"))
            });
            let proceed = match mounted {
                Ok(mut wad) => wad_bins(&mut wad, Some(&name), paths, visit),
                Err(error) => (visit.bin)(PackagedBin {
                    part: None,
                    name: &name,
                    data: Err(error),
                }),
            };
            if !proceed {
                return;
            }
            continue;
        }

        // A file by its path: below a directory with the name of a WAD archive, or below
        // `RAW`. The part is the directory, and the bin name is the path below it.
        let split = match in_wads {
            true => WAD_ENDINGS.iter().find_map(|ending| {
                let at = lower.find(&format!("{ending}/"))? + ending.len();
                Some(at)
            }),
            false if area == "raw" => Some(area.len()),
            false => None,
        };
        let Some(at) = split else {
            continue;
        };
        let (part, bin) = (&name[..at], &name[at + 1..]);
        if !is_bin_name(bin) || !(visit.wants)(bin) {
            continue;
        }
        let data = read(zip);
        if !(visit.bin)(PackagedBin {
            part: Some(part),
            name: bin,
            data,
        }) {
            return;
        }
    }
}

/// Visits the bins of a `.modpkg` file. The part of a bin is its WAD, followed by the layer in
/// parentheses if the layer is not the base layer.
fn modpkg_bins<R: Read + Seek>(modpkg: &mut Modpkg<R>, visit: &mut Visit<'_>) {
    // A meta chunk belongs to no WAD and is not a game file.
    let listed: Vec<Listed<_>> = modpkg
        .chunks()
        .iter()
        .filter(|(_, chunk)| chunk.wad().is_some())
        .map(|(key, chunk)| {
            let path = modpkg.chunk_paths().get(&chunk.path_hash);
            let wad = modpkg
                .wad_name_for_index(chunk.wad_index)
                .unwrap_or_default();
            let part = match modpkg.layer_name_for_index(chunk.layer_index) {
                Some(layer) if !layer.eq_ignore_ascii_case("base") => format!("{wad} ({layer})"),
                _ => wad.to_owned(),
            };
            Listed {
                key: (*key, part),
                named: path.is_some(),
                name: path.cloned().unwrap_or_else(|| chunk.path_hash.to_string()),
            }
        })
        .collect();

    let mut chunks = candidates(listed.into_iter(), visit);
    // The same path can be in several WADs and layers.
    chunks.sort_by(|a, b| (&a.key.1, &a.name).cmp(&(&b.key.1, &b.name)));
    for chunk in chunks {
        let data = modpkg
            .load_chunk_decompressed(chunk.key.0)
            .map(|data| data.into_vec())
            .into_diagnostic();
        if !offer(&chunk, Some(&chunk.key.1), data, visit) {
            return;
        }
    }
}

/// Returns the chunk hash of `path`, as the hashtables and the archives store it.
#[cfg(test)]
fn chunk_hash(path: &str) -> ltk_hash::WadHash {
    ltk_game_index::chunk_hash(path)
}

#[cfg(test)]
pub mod testing {
    //! Test fixtures: a WAD archive and a Fantome file built in memory.

    use std::{collections::BTreeMap, io::Write as _};

    use ltk_hash::WadHash;
    use ltk_wad::{WadBuilder, WadChunkBuilder, WadChunkCompression};

    use super::*;

    /// Returns the bytes of a WAD archive with `chunks`, given as `(chunk path, data)` pairs.
    pub fn wad(chunks: &[(&str, &[u8])]) -> Vec<u8> {
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
        out.into_inner()
    }

    /// Returns the bytes of a zip file with `entries`, given as `(entry name, data)` pairs.
    pub fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use ltk_meta::{Bin, BinFile, BinObject, property::values};

    use super::*;
    use crate::document::to_bin;

    fn bin(object: u32) -> Vec<u8> {
        let bin: BinFile = Bin::builder()
            .object(
                BinObject::builder(object, 0xaaaa_0001u32)
                    .property(0x10u32, values::I32::new(1))
                    .build(),
            )
            .build()
            .into();
        to_bin(&bin).unwrap()
    }

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    /// Returns the part and the name of each bin of the package at `path`, and whether its
    /// data was read.
    fn bins(path: &Utf8Path, wants: &dyn Fn(&str) -> bool) -> Vec<(Option<String>, String, bool)> {
        let mut found = Vec::new();
        for_each_bin(
            path,
            PackageKind::of(path).unwrap(),
            &WadPaths::default(),
            &mut Visit {
                wants,
                bin: &mut |bin| {
                    found.push((
                        bin.part.map(str::to_owned),
                        bin.name.to_owned(),
                        bin.data.is_ok(),
                    ));
                    true
                },
            },
        )
        .unwrap();
        found
    }

    #[test]
    fn package_kind_of_uses_file_name_ending() {
        let kind = |name: &str| PackageKind::of(Utf8Path::new(name));
        assert_eq!(kind("mods/Teemo.wad.client"), Some(PackageKind::Wad));
        assert_eq!(kind("Map11.WAD"), Some(PackageKind::Wad));
        assert_eq!(kind("skin.fantome"), Some(PackageKind::Fantome));
        assert_eq!(kind("skin.modpkg"), Some(PackageKind::Modpkg));
        assert_eq!(kind("skin0.bin"), None);
        assert_eq!(kind("archive.zip"), None);
    }

    #[test]
    fn wad_bins_are_chunks_whose_data_starts_with_bin_magic() {
        let (_guard, dir) = temp_dir();
        let path = dir.join("Teemo.wad.client");
        std::fs::write(
            &path,
            testing::wad(&[
                ("data/skin0.bin", &bin(1)),
                ("assets/texture.dds", b"DDS not a bin"),
                ("data/skin1.bin", &bin(2)),
            ]),
        )
        .unwrap();

        // No hashtable is loaded, so every chunk is named by its hash and tested by its magic.
        let mut expected = vec![
            (
                None,
                format!("{:016x}", chunk_hash("data/skin0.bin").0),
                true,
            ),
            (
                None,
                format!("{:016x}", chunk_hash("data/skin1.bin").0),
                true,
            ),
        ];
        expected.sort();
        assert_eq!(bins(&path, &|_| true), expected);
        assert!(bins(&path, &|_| false).is_empty());
    }

    #[test]
    fn fantome_bins_come_from_packed_wads_wad_directories_and_raw_files() {
        let (_guard, dir) = temp_dir();
        let path = dir.join("skin.fantome");
        let packed = testing::wad(&[("data/packed.bin", &bin(1))]);
        std::fs::write(
            &path,
            testing::zip(&[
                ("META/info.json", b"{}"),
                ("WAD/Teemo.wad.client", &packed),
                ("WAD/Annie.wad.client/data/annie.bin", &bin(2)),
                ("WAD/Annie.wad.client/assets/annie.dds", b"DDS"),
                ("WAD_chroma/Annie.wad.client/data/chroma.bin", &bin(3)),
                ("RAW/data/raw.bin", &bin(4)),
                ("RAW/readme.txt", b"text"),
            ]),
        )
        .unwrap();

        let found = bins(&path, &|_| true);
        let packed_name = format!("{:016x}", chunk_hash("data/packed.bin").0);
        assert_eq!(
            found,
            [
                (Some("RAW".to_owned()), "data/raw.bin".to_owned(), true),
                (
                    Some("WAD/Annie.wad.client".to_owned()),
                    "data/annie.bin".to_owned(),
                    true
                ),
                (Some("WAD/Teemo.wad.client".to_owned()), packed_name, true),
                (
                    Some("WAD_chroma/Annie.wad.client".to_owned()),
                    "data/chroma.bin".to_owned(),
                    true
                ),
            ]
        );
        assert_eq!(bins(&path, &|name| name.contains("annie")).len(), 1);
    }

    #[test]
    fn modpkg_bins_are_named_by_path_with_wad_and_layer_as_part() {
        use ltk_modpkg::builder::{ModpkgBuilder, ModpkgChunkBuilder, ModpkgLayerBuilder};

        let (_guard, dir) = temp_dir();
        let path = dir.join("skin.modpkg");
        let chunk = |chunk_path: &str, layer: &str| {
            ModpkgChunkBuilder::new()
                .with_path(chunk_path)
                .with_wad("Teemo.wad.client")
                .with_layer(layer)
        };
        let mut out = Cursor::new(Vec::new());
        ModpkgBuilder::default()
            .with_layer(ModpkgLayerBuilder::base())
            .with_layer(ModpkgLayerBuilder::new("chroma").unwrap().with_priority(1))
            .with_chunk(chunk("data/skin0.bin", "base"))
            .with_chunk(chunk("assets/texture.dds", "base"))
            .with_chunk(chunk("data/skin0.bin", "chroma"))
            .build_to_writer(&mut out, |chunk| {
                Ok(match chunk.path().ends_with(".bin") {
                    true => bin(1),
                    false => b"DDS".to_vec(),
                })
            })
            .unwrap();
        std::fs::write(&path, out.into_inner()).unwrap();

        // A modpkg file stores the name of a WAD in lower case.
        assert_eq!(
            bins(&path, &|_| true),
            [
                (
                    Some("teemo.wad.client".to_owned()),
                    "data/skin0.bin".to_owned(),
                    true
                ),
                (
                    Some("teemo.wad.client (chroma)".to_owned()),
                    "data/skin0.bin".to_owned(),
                    true
                ),
            ]
        );
    }

    #[test]
    fn for_each_bin_fails_for_invalid_package_and_stops_when_visit_returns_false() {
        let (_guard, dir) = temp_dir();
        let broken = dir.join("broken.fantome");
        std::fs::write(&broken, b"not a zip").unwrap();
        let mut visit = Visit {
            wants: &|_| true,
            bin: &mut |_| true,
        };
        let error = for_each_bin(
            &broken,
            PackageKind::Fantome,
            &WadPaths::default(),
            &mut visit,
        )
        .unwrap_err();
        assert!(error.to_string().contains("is not a valid Fantome file"));

        let path = dir.join("Teemo.wad.client");
        std::fs::write(
            &path,
            testing::wad(&[("data/a.bin", &bin(1)), ("data/b.bin", &bin(2))]),
        )
        .unwrap();
        let mut count = 0;
        for_each_bin(
            &path,
            PackageKind::Wad,
            &WadPaths::default(),
            &mut Visit {
                wants: &|_| true,
                bin: &mut |_| {
                    count += 1;
                    false
                },
            },
        )
        .unwrap();
        assert_eq!(count, 1);
    }
}
