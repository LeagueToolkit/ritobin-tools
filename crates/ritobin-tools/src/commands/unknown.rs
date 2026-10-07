//! The `hashes unknown` command. It lists the hashes of bins that no hashtable resolves.

use std::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Instant,
};

use camino::Utf8PathBuf;
use clap::Args;
use ltk_mimir_cache::Table;
use miette::Result;
use serde::Serialize;

use crate::{
    commands::{
        gamedata::GameArgs,
        output::{OutputArgs, columns, print},
        search::{Item, Job, check_game_dir, file_jobs, game_jobs},
    },
    context::Context,
    hashes::{GameNames, format_hash},
    unknown::{Collector, Found, HashTable, merge},
    utils::plural,
};

#[derive(Args, Debug)]
pub struct UnknownArgs {
    /// Bin files, ritobin text files, WAD archives, mod packages (.fantome, .modpkg) or
    /// directories to read. A directory is read with its subdirectories. Defaults to the bins
    /// of the game
    #[arg(value_name = "PATHS")]
    pub paths: Vec<Utf8PathBuf>,

    /// List only the hashes of these tables, separated by commas. Defaults to all tables
    #[arg(short, long, value_enum, value_delimiter = ',', value_name = "TABLES")]
    pub table: Vec<HashTable>,

    /// Read only the game archives whose name contains TEXT, without regard to case
    #[arg(long, value_name = "TEXT")]
    pub wad: Option<String>,

    /// Read only the bins whose path contains TEXT, without regard to case
    #[arg(long, value_name = "TEXT")]
    pub bin: Option<String>,

    #[command(flatten)]
    pub game: GameArgs,

    /// Maximum number of hashes to print. The most frequent hashes are printed first. 0 means
    /// no limit
    #[arg(short = 'n', long, value_name = "N", default_value_t = 0)]
    pub limit: usize,

    /// Number of worker threads. Defaults to the number of processor cores
    #[arg(short = 'j', long, value_name = "N")]
    pub threads: Option<NonZeroUsize>,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// One hash without a name, as it is printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Row {
    table: HashTable,
    /// The hash as `0x` hex: 16 digits for the `game` table, 8 digits for a bin table.
    hash: String,
    /// The number of occurrences.
    count: usize,
    /// The number of bins that contain the hash.
    bins: usize,
    /// The bin of the example occurrence.
    source: String,
    /// The game archive or the package that contains `source`.
    archive: Option<String>,
    /// The path hash of the object of the example occurrence, as `0x` hex.
    object: String,
    /// The path of that object, if the entry table has it.
    object_name: Option<String>,
    /// The path of the example occurrence inside the object. `None` if the hash is the path or
    /// the class of the object itself.
    path: Option<String>,
}

/// The number of documents that a run read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Read {
    documents: usize,
    failed: usize,
}

/// Runs the `hashes unknown` command.
pub fn run(ctx: &Context, args: &UnknownArgs) -> Result<()> {
    if args.paths.is_empty() {
        check_game_dir(ctx, &args.game)?;
    }
    let names = ctx.game_names();
    let jobs = match args.paths.is_empty() {
        true => game_jobs(ctx, &args.game, args.wad.as_deref(), args.bin.as_deref())?,
        false => file_jobs(&args.paths, args.bin.as_deref())?,
    };
    let threads = args
        .threads
        .or_else(|| std::thread::available_parallelism().ok())
        .map_or(1, NonZeroUsize::get);

    let started = Instant::now();
    let (found, read) = collect(&jobs, &names, &args.table, threads);
    let rows = rows(&found, &names, args.limit);
    print(&rows, args.output.format, table)?;

    let by_table: Vec<String> = [
        HashTable::Entries,
        HashTable::Fields,
        HashTable::Hashes,
        HashTable::Types,
        HashTable::Game,
    ]
    .into_iter()
    .filter_map(|table| {
        let count = found.keys().filter(|(of, _)| *of == table).count();
        (count > 0).then(|| format!("{count} {}", table.name()))
    })
    .collect();
    let failed = match read.failed {
        0 => String::new(),
        failed => format!(" {} could not be read.", plural(failed, "bin")),
    };
    tracing::info!(
        "Found {} without a name{}. Read {} in {:.1} s.{failed}",
        match found.len() {
            1 => "1 hash".to_owned(),
            count => format!("{count} hashes"),
        },
        match by_table.is_empty() {
            true => String::new(),
            false => format!(" ({})", by_table.join(", ")),
        },
        plural(read.documents, "bin"),
        started.elapsed().as_secs_f32()
    );
    Ok(())
}

/// Reads the documents of `jobs` on `threads` worker threads and collects the hashes of
/// `tables` that have no name. The result does not depend on the thread count.
///
/// A document that cannot be read is skipped with a warning and counted as failed.
fn collect(jobs: &[Job], names: &GameNames, tables: &[HashTable], threads: usize) -> (Found, Read) {
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let mut found = Found::new();
    let mut read = Read::default();

    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.clamp(1, jobs.len().max(1)))
            .map(|_| {
                let (next, stop) = (&next, &stop);
                scope.spawn(move || {
                    let mut collector = Collector::new(names, tables);
                    let mut read = Read::default();
                    while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                        job.read(&names.paths, stop, &mut |item| {
                            match item {
                                Item::Document {
                                    source,
                                    archive,
                                    data,
                                } => {
                                    read.documents += 1;
                                    let archive = archive.or(job.archive().map(str::to_owned));
                                    let collected = data
                                        .and_then(|data| collector.document(source, archive, data));
                                    if let Err(error) = collected {
                                        tracing::warn!("Skipped {source}: {error}");
                                        read.failed += 1;
                                    }
                                }
                                Item::Unreadable { documents } => {
                                    read.documents += documents;
                                    read.failed += documents;
                                }
                            }
                            true
                        });
                    }
                    (collector.found, read)
                })
            })
            .collect();
        for worker in workers {
            // A worker does not panic. If it does, its results are missing from the totals.
            if let Ok((other, other_read)) = worker.join() {
                merge(&mut found, other);
                read.documents += other_read.documents;
                read.failed += other_read.failed;
            }
        }
    });
    (found, read)
}

/// Returns the rows of `found`: the most frequent hash first, then by table and by hash value.
/// Returns at most `limit` rows, or all rows if `limit` is 0.
fn rows(found: &Found, names: &GameNames, limit: usize) -> Vec<Row> {
    let mut keys: Vec<_> = found.iter().collect();
    keys.sort_by_key(|((table, hash), unknown)| (std::cmp::Reverse(unknown.count), *table, *hash));
    if limit > 0 {
        keys.truncate(limit);
    }
    keys.into_iter()
        .map(|((table, hash), unknown)| {
            let example = &unknown.example;
            Row {
                table: *table,
                hash: table.format(*hash),
                count: unknown.count,
                bins: unknown.documents,
                source: example.source.clone(),
                archive: example.archive.clone(),
                object: format_hash(example.object),
                object_name: names
                    .bins
                    .lookup(Table::BinEntries, example.object)
                    .map(Into::into),
                path: example
                    .path
                    .as_ref()
                    .map(|path| path.to_named(&names.bins).text),
            }
        })
        .collect()
}

/// Formats `rows` as a table. The example column has the bin, the object and the path of one
/// occurrence.
fn table(rows: &[Row]) -> String {
    let cells: Vec<[String; 5]> = rows
        .iter()
        .map(|row| {
            let object = row.object_name.as_deref().unwrap_or(&row.object);
            let example = match &row.path {
                Some(path) => format!("{}: {object} {path}", row.source),
                None => format!("{}: {object}", row.source),
            };
            [
                row.table.name().to_owned(),
                row.hash.clone(),
                row.count.to_string(),
                row.bins.to_string(),
                example,
            ]
        })
        .collect();
    match cells.is_empty() {
        true => String::new(),
        false => columns(["TABLE", "HASH", "COUNT", "BINS", "EXAMPLE"], &cells),
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;
    use ltk_hash::{BinHash, Hash as _};
    use ltk_meta::{Bin, BinFile, BinObject, property::values};

    use super::*;
    use crate::document::to_bin;

    /// Returns a bin with one object that has a `name` property and `count` copies of a hash
    /// value.
    fn bin(entry: &str, joint: &str, count: usize) -> Vec<u8> {
        let mut object = BinObject::builder(BinHash::hash_str(entry), BinHash::hash_str("Item"))
            .property(
                BinHash::hash_str("name"),
                values::String::new(entry.to_owned()),
            );
        for index in 0..count {
            object = object.property(
                BinHash(0x100 + index as u32),
                values::Hash::new(BinHash::hash_str(joint)),
            );
        }
        let bin: BinFile = Bin::builder().object(object.build()).build().into();
        to_bin(&bin).unwrap()
    }

    fn write_bins(dir: &Utf8Path) {
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("a.bin"), bin("Items/Sword", "Hilt", 1)).unwrap();
        std::fs::write(dir.join("b.bin"), bin("Items/Shield", "Hilt", 2)).unwrap();
        std::fs::write(
            dir.join("nested").join("c.bin"),
            bin("Items/Bow", "String", 1),
        )
        .unwrap();
        std::fs::write(dir.join("broken.bin"), b"PROP\x03\x00\x00\x00\x00").unwrap();
    }

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    #[test]
    fn collect_result_is_the_same_for_every_thread_count() {
        let (_guard, dir) = temp_dir();
        write_bins(&dir);
        let jobs = file_jobs(std::slice::from_ref(&dir), None).unwrap();
        let names = GameNames::default();

        let (found, read) = collect(&jobs, &names, &[HashTable::Hashes], 1);
        assert_eq!(
            read,
            Read {
                documents: 4,
                failed: 1
            }
        );
        let hilt = &found[&(HashTable::Hashes, u64::from(BinHash::hash_str("Hilt").0))];
        assert_eq!((hilt.count, hilt.documents), (3, 2));
        assert!(hilt.example.source.replace('\\', "/").ends_with("/a.bin"));

        for threads in [2, 8] {
            assert_eq!(
                collect(&jobs, &names, &[HashTable::Hashes], threads),
                (found.clone(), read)
            );
        }
    }

    #[test]
    fn rows_are_sorted_by_count_and_limited() {
        colored::control::set_override(false);
        let (_guard, dir) = temp_dir();
        write_bins(&dir);
        let jobs = file_jobs(std::slice::from_ref(&dir), None).unwrap();
        let names = GameNames::default();
        let (found, _) = collect(&jobs, &names, &[HashTable::Hashes, HashTable::Types], 2);

        let all = rows(&found, &names, 0);
        let listed: Vec<(HashTable, usize, usize)> = all
            .iter()
            .map(|row| (row.table, row.count, row.bins))
            .collect();
        assert_eq!(
            listed,
            [
                (HashTable::Hashes, 3, 2),
                (HashTable::Types, 3, 3),
                (HashTable::Hashes, 1, 1),
            ]
        );
        assert_eq!(
            all[0].hash,
            format!("0x{:08x}", BinHash::hash_str("Hilt").0)
        );
        assert_eq!(all[0].path.as_deref(), Some("00000100"));
        assert_eq!(all[1].path, None);
        assert_eq!(rows(&found, &names, 1).len(), 1);

        let text = table(&all[..1]);
        assert!(
            text.starts_with("TABLE   HASH        COUNT  BINS  EXAMPLE\nhashes  0x"),
            "{text}"
        );
        assert!(text.trim_end().ends_with("00000100"), "{text}");
        assert_eq!(table(&[]), "");
    }
}
