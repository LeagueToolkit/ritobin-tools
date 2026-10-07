//! The `search` command. It searches bin files, or the bins of the installed game, for entries,
//! classes, property names, values and dependencies.
//!
//! The documents are scanned on worker threads. The results are printed on the calling thread in
//! job order, so the output is the same for every thread count.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, BufReader, BufWriter, IsTerminal as _, Write},
    num::NonZeroUsize,
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, ValueEnum};
use colored::{ColoredString, Colorize};
use ltk_hash::{BinHash, WadHash};
use ltk_meta::PropertyKind;
use ltk_wad::Wad;
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;
use walkdir::WalkDir;

use crate::{
    commands::gamedata::GameArgs,
    context::Context,
    document::scanned_format,
    hashes::GameNames,
    search::{self, Hit, Matched, Matcher, Pattern, Query, Row, Target},
    utils::plural,
};

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Text to search for. It matches a substring, without regard to case. The text is also
    /// hashed, so it matches the hash of this name even if no hashtable resolves the hash. `0x`
    /// hex matches the hash with that value. A number matches the numeric values equal to it
    #[arg(value_name = "PATTERN")]
    pub pattern: Option<String>,

    /// Bin files, ritobin text files or directories to search. A directory is searched with its
    /// subdirectories. Defaults to the bins of the game
    #[arg(value_name = "PATHS")]
    pub paths: Vec<Utf8PathBuf>,

    /// Search with a regular expression instead of PATTERN. All positional arguments are then
    /// paths
    #[arg(short = 'e', long, value_name = "REGEX")]
    pub regex: Option<String>,

    /// List every value that passes the filters, without a pattern. All positional arguments
    /// are then paths
    #[arg(long, conflicts_with = "regex")]
    pub values: bool,

    /// Match the whole text instead of a substring
    #[arg(short = 'x', long)]
    pub exact: bool,

    /// Match text with regard to case
    #[arg(short = 's', long)]
    pub case_sensitive: bool,

    /// Parts of a bin to search, separated by commas. Defaults to all parts
    #[arg(
        long = "in",
        value_enum,
        value_delimiter = ',',
        value_name = "PARTS",
        help_heading = FILTERS
    )]
    pub targets: Vec<Target>,

    /// Search only values of these ritobin types, separated by commas: `string`, `hash`, `link`,
    /// `file`, `f32`, `u32`, `bool`, `vec3` and the other types without nested values
    #[arg(
        short = 't',
        long = "type",
        value_delimiter = ',',
        value_name = "TYPES",
        value_parser = search::value_kind,
        help_heading = FILTERS
    )]
    pub kinds: Vec<PropertyKind>,

    /// Search only the values of the property with this name or `0x` hash
    #[arg(long, value_name = "FIELD", value_parser = search::name_hash, help_heading = FILTERS)]
    pub field: Option<BinHash>,

    /// Search only the properties of objects and structs with this class name or `0x` hash
    #[arg(long, value_name = "CLASS", value_parser = search::name_hash, help_heading = FILTERS)]
    pub class: Option<BinHash>,

    /// Search only the object with this path or `0x` hash
    #[arg(long, value_name = "ENTRY", value_parser = search::name_hash, help_heading = FILTERS)]
    pub object: Option<BinHash>,

    /// Search only the objects with this class name or `0x` hash
    #[arg(long, value_name = "CLASS", value_parser = search::name_hash, help_heading = FILTERS)]
    pub object_class: Option<BinHash>,

    /// Search only the game archives whose name contains TEXT, without regard to case
    #[arg(long, value_name = "TEXT", help_heading = FILTERS)]
    pub wad: Option<String>,

    /// Search only the bins whose path contains TEXT, without regard to case
    #[arg(long, value_name = "TEXT", help_heading = FILTERS)]
    pub bin: Option<String>,

    #[command(flatten)]
    pub game: GameArgs,

    /// Output format
    #[arg(short, long, value_enum, default_value_t = SearchFormat::Text)]
    pub format: SearchFormat,

    /// Print only the path of each bin that has a match
    #[arg(short = 'l', long, conflicts_with_all = ["count", "format"])]
    pub files_with_matches: bool,

    /// Print only the path and the number of matches of each bin that has a match
    #[arg(short, long, conflicts_with = "format")]
    pub count: bool,

    /// Stop after this number of matches
    #[arg(short = 'm', long, value_name = "N")]
    pub limit: Option<NonZeroUsize>,

    /// Number of worker threads. Defaults to the number of processor cores
    #[arg(short = 'j', long, value_name = "N")]
    pub threads: Option<NonZeroUsize>,

    /// Disable colored output
    #[arg(long)]
    pub no_color: bool,
}

const FILTERS: &str = "Filters";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchFormat {
    /// Matches grouped by bin and by object
    Text,
    /// One JSON array with one object per match
    Json,
    /// One JSON object per match, one object per line
    Jsonl,
}

impl SearchArgs {
    /// Returns the query and the paths to search.
    ///
    /// With `--regex` or `--values` the first positional argument is a path, not a pattern.
    /// Fails if no pattern is given and neither option is set.
    fn query(&self) -> Result<(Query, Vec<Utf8PathBuf>)> {
        let mut paths = self.paths.clone();
        let pattern = match (&self.regex, self.values, &self.pattern) {
            (None, false, Some(pattern)) if !pattern.is_empty() => {
                Pattern::Literal(pattern.clone())
            }
            (None, false, _) => miette::bail!(
                "No pattern was given. Pass the text to search for, or --regex, or --values to list every value that passes the filters"
            ),
            (regex, ..) => {
                if let Some(first) = &self.pattern {
                    paths.insert(0, first.into());
                }
                match regex {
                    Some(regex) => Pattern::Regex(regex.clone()),
                    None => Pattern::Any,
                }
            }
        };
        let query = Query {
            pattern,
            exact: self.exact,
            case_sensitive: self.case_sensitive,
            targets: self.targets.clone(),
            kinds: self.kinds.clone(),
            field: self.field,
            class: self.class,
            object: self.object,
            object_class: self.object_class,
        };
        Ok((query, paths))
    }

    /// Returns what is printed for each bin that has a match.
    fn mode(&self) -> Mode {
        match (self.files_with_matches, self.count, self.format) {
            (true, ..) => Mode::Paths,
            (_, true, _) => Mode::Counts,
            (_, _, SearchFormat::Text) => Mode::Text,
            (_, _, SearchFormat::Json) => Mode::Json,
            (_, _, SearchFormat::Jsonl) => Mode::Jsonl,
        }
    }
}

/// What the command prints for each document that has a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Text,
    Json,
    Jsonl,
    /// The path of the document.
    Paths,
    /// The path of the document and its number of matches.
    Counts,
}

/// One unit of work for a worker thread.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Job {
    /// A bin file or a ritobin text file.
    File(Utf8PathBuf),
    /// The bin chunks of one game archive, as chunk hash and chunk path, in path order.
    Archive {
        /// The archive path relative to `DATA/FINAL`.
        name: String,
        path: Utf8PathBuf,
        bins: Vec<(WadHash, String)>,
    },
}

/// The hits of one document.
struct Found {
    /// The file path, or the chunk path of a game bin.
    source: String,
    /// The number of hits.
    count: usize,
    /// The hits. Empty if the search only counts them.
    hits: Vec<Hit>,
}

/// One message from a worker thread: the result of one document. A worker that cannot open an
/// archive sends one message for all bins of the archive.
struct Scanned {
    /// The number of documents that this message counts as searched.
    searched: usize,
    /// The number of documents that could not be read.
    failed: usize,
    /// The hits of the document. `None` if it has no hit or could not be read.
    found: Option<Found>,
}

/// The number of collected hits that the queued results of one job can reach before the worker
/// of the job waits for the calling thread to print them. The limit bounds the memory of a
/// search that matches most values. The tests use a low limit, so that small files reach it.
const QUEUED_HITS: usize = if cfg!(test) { 64 } else { 10_000 };

/// The interval at which a waiting worker checks whether the search was stopped.
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(50);

/// The number of collected hits in the queued results of one job. The worker of the job adds
/// to it, and the calling thread subtracts from it.
#[derive(Default)]
struct Gate {
    queued: Mutex<usize>,
    changed: Condvar,
}

impl Gate {
    /// Adds `hits` to the queued hits of the job. Waits first while [`QUEUED_HITS`] or more
    /// hits are queued. Returns `false` and adds nothing if `stop` is set during the wait.
    fn enter(&self, hits: usize, stop: &AtomicBool) -> bool {
        let mut queued = self.queued.lock().unwrap_or_else(PoisonError::into_inner);
        while *queued >= QUEUED_HITS {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            queued = self
                .changed
                .wait_timeout(queued, STOP_CHECK_INTERVAL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *queued += hits;
        true
    }

    /// Subtracts `hits` from the queued hits of the job and wakes the worker if it waits.
    fn leave(&self, hits: usize) {
        let mut queued = self.queued.lock().unwrap_or_else(PoisonError::into_inner);
        *queued = queued.saturating_sub(hits);
        self.changed.notify_one();
    }
}

/// The totals of a search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Totals {
    matches: usize,
    /// The number of documents with a match.
    sources: usize,
    searched: usize,
    failed: usize,
}

/// Runs the `search` command. Returns `true` if at least one match was found.
pub fn run(ctx: &Context, args: SearchArgs) -> Result<bool> {
    let (query, paths) = args.query()?;
    colored::control::set_override(
        !args.no_color && io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    );

    // The query is compiled before the game is opened. An invalid pattern therefore fails
    // before the game index is loaded or built. The game directory is checked first, because
    // compiling reads the hashtables.
    if paths.is_empty() {
        check_game_dir(ctx, &args)?;
    }
    let names = ctx.game_names();
    let matcher = Matcher::compile(&query, &names.bins, &names.paths)?;
    let jobs = match paths.is_empty() {
        true => game_jobs(ctx, &args)?,
        false => file_jobs(&paths, args.bin.as_deref())?,
    };

    let threads = args
        .threads
        .or_else(|| std::thread::available_parallelism().ok())
        .map_or(1, NonZeroUsize::get);
    let limit = args.limit.map_or(usize::MAX, NonZeroUsize::get);
    let started = Instant::now();
    let mut out = BufWriter::new(io::stdout().lock());
    let totals = search(
        &jobs,
        &matcher,
        &names,
        args.mode(),
        limit,
        threads,
        &mut out,
    )?;

    let found = match totals.matches {
        0 => "Found no match".to_owned(),
        1 => "Found 1 match in 1 bin".to_owned(),
        matches => format!(
            "Found {matches} matches in {}",
            plural(totals.sources, "bin")
        ),
    };
    let failed = match totals.failed {
        0 => String::new(),
        failed => format!(" {} could not be read.", plural(failed, "bin")),
    };
    tracing::info!(
        "{found}. Searched {} in {:.1} s.{failed}",
        plural(totals.searched, "bin"),
        started.elapsed().as_secs_f32()
    );
    Ok(totals.matches > 0)
}

/// Fails if no game directory is set by `--game-dir` or by the config.
fn check_game_dir(ctx: &Context, args: &SearchArgs) -> Result<()> {
    if args.game.dir(ctx).is_none() {
        miette::bail!(
            "No paths were given and no game directory is set. Pass files or directories to search, pass --game-dir, or run `ritobin-tools config set game_dir <DIR>`"
        );
    }
    Ok(())
}

/// Returns `true` if `filter` is `None` or `text` contains it, without regard to case.
fn passes(text: &str, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| text.to_lowercase().contains(&filter.to_lowercase()))
}

/// Builds one job per game archive that has bin chunks. Applies the `--wad` filter to the
/// archive names and the `--bin` filter to the chunk paths.
fn game_jobs(ctx: &Context, args: &SearchArgs) -> Result<Vec<Job>> {
    let game = args.game.open(ctx)?;

    let mut jobs = Vec::new();
    for archive in game.bin_archives() {
        if !passes(&archive.name, args.wad.as_deref()) {
            continue;
        }
        let mut bins: Vec<(WadHash, String)> = archive
            .chunks
            .iter()
            .map(|chunk| (*chunk, game.chunk_name(*chunk)))
            .filter(|(_, name)| passes(name, args.bin.as_deref()))
            .collect();
        if bins.is_empty() {
            continue;
        }
        bins.sort_by(|a, b| a.1.cmp(&b.1));
        jobs.push(Job::Archive {
            name: archive.name,
            path: archive.path,
            bins,
        });
    }

    // Without the `game` table a chunk is named by its hash, so a path filter selects no bin.
    if jobs.is_empty() && args.bin.is_some() && !ctx.wad_paths().is_loaded() {
        tracing::warn!(
            "--bin selected no bin, because the `game` hashtable is not installed and the bin paths are unknown. Run `ritobin-tools hashes sync` to download the hashtables."
        );
    }
    Ok(jobs)
}

/// Builds one job per file. A directory is replaced by the bin files and ritobin text files in
/// it and in its subdirectories, sorted by path. `filter` is the `--bin` filter.
///
/// Fails if a path does not exist.
fn file_jobs(paths: &[Utf8PathBuf], filter: Option<&str>) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for path in paths {
        if path.is_dir() {
            for entry in WalkDir::new(path).sort_by_file_name() {
                let entry = entry
                    .into_diagnostic()
                    .wrap_err_with(|| format!("Failed to read directory {path}"))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let Some(file) = Utf8Path::from_path(entry.path()) else {
                    tracing::warn!("Skipping non-UTF-8 path: {}", entry.path().display());
                    continue;
                };
                if scanned_format(file).is_some() {
                    jobs.push(Job::File(file.to_owned()));
                }
            }
        } else if path.exists() {
            jobs.push(Job::File(path.clone()));
        } else {
            miette::bail!("Input does not exist: {path}");
        }
    }
    jobs.retain(|job| match job {
        Job::File(path) => passes(&path.as_str().replace('\\', "/"), filter),
        Job::Archive { .. } => true,
    });
    Ok(jobs)
}

/// Runs `jobs` on `threads` worker threads and prints the results to `out` in job order. Stops
/// after `limit` matches. Returns the totals.
///
/// A closed output pipe stops the search without an error.
fn search(
    jobs: &[Job],
    matcher: &Matcher,
    names: &GameNames,
    mode: Mode,
    limit: usize,
    threads: usize,
    out: &mut impl Write,
) -> Result<Totals> {
    let mut printer = Printer {
        names,
        mode,
        remaining: limit,
        totals: Totals::default(),
        records: Vec::new(),
        out,
    };
    let mut failure = None;
    let scanner = Scanner {
        matcher,
        limit,
        // The `Paths` and `Counts` modes print no hit, so the workers only count the hits.
        count_only: matches!(mode, Mode::Paths | Mode::Counts),
    };
    run_jobs(jobs, scanner, threads, |index, scanned| {
        printer.totals.searched += scanned.searched;
        printer.totals.failed += scanned.failed;
        let Some(found) = scanned.found else {
            return true;
        };
        let archive = match &jobs[index] {
            Job::Archive { name, .. } => Some(name.as_str()),
            Job::File(_) => None,
        };
        match printer.document(&found, archive) {
            Ok(proceed) => proceed,
            Err(error) => {
                if error.kind() != io::ErrorKind::BrokenPipe {
                    failure = Some(error);
                }
                false
            }
        }
    });
    if let Some(error) = failure {
        return Err(error)
            .into_diagnostic()
            .wrap_err("Failed to write standard output");
    }

    let finished = printer.finish();
    match finished {
        Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error)
            .into_diagnostic()
            .wrap_err("Failed to write standard output"),
        _ => Ok(printer.totals),
    }
}

/// Runs `jobs` on `threads` worker threads. Calls `on_scanned` on the calling thread with the
/// job index and each result, in job order and in document order. The workers stop when
/// `on_scanned` returns `false`.
///
/// Each job has its own queue of results and its own [`Gate`]. The calling thread reads the
/// queues in job order. A worker waits when the queue of its job has [`QUEUED_HITS`] collected
/// hits, so a job that runs ahead of the output does not collect all of its hits in memory.
fn run_jobs(
    jobs: &[Job],
    scanner: Scanner<'_>,
    threads: usize,
    mut on_scanned: impl FnMut(usize, Scanned) -> bool,
) {
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (started, queues) = mpsc::channel();
        for _ in 0..threads.clamp(1, jobs.len().max(1)) {
            let started = started.clone();
            let (next, stop) = (&next, &stop);
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(index) else {
                        break;
                    };
                    let (results, queue) = mpsc::channel();
                    let gate = Arc::new(Gate::default());
                    if started.send((index, queue, Arc::clone(&gate))).is_err() {
                        break;
                    }
                    scanner.work(job, stop, &gate, &results);
                }
            });
        }
        drop(started);

        // The workers take the jobs in index order. The job with the lowest index that is not
        // finished therefore always has a worker, and this loop reads the queue of that job.
        // That worker never waits for long, because this loop removes its hits from the gate.
        // A queue that arrives before its turn is kept in `early`.
        let mut early = BTreeMap::new();
        'jobs: for expected in 0..jobs.len() {
            let (queue, gate) = loop {
                if let Some(job) = early.remove(&expected) {
                    break job;
                }
                match queues.recv() {
                    Ok((index, queue, gate)) => early.insert(index, (queue, gate)),
                    // All workers exited.
                    Err(_) => break 'jobs,
                };
            };
            for scanned in queue {
                let hits = scanned.found.as_ref().map_or(0, |found| found.hits.len());
                let proceed = on_scanned(expected, scanned);
                gate.leave(hits);
                if !proceed {
                    break 'jobs;
                }
            }
        }

        // A worker that waits at its gate checks `stop` at an interval and then exits. The
        // scope can then join the workers.
        stop.store(true, Ordering::Relaxed);
    });
}

/// The scan options that all worker threads use.
#[derive(Clone, Copy)]
struct Scanner<'a> {
    matcher: &'a Matcher,
    /// The maximum number of hits of one document.
    limit: usize,
    /// If `true`, the hits are counted and not collected.
    count_only: bool,
}

impl Scanner<'_> {
    /// Scans the document `source` from `data`. Logs a warning and counts the document as
    /// failed if `data` is an error or the document is invalid.
    fn scan(&self, source: &str, data: Result<Vec<u8>>) -> Scanned {
        let found = data.and_then(|data| match self.count_only {
            true => search::count(source, data, self.matcher, self.limit)
                .map(|count| (count, Vec::new())),
            false => {
                search::scan(source, data, self.matcher, self.limit).map(|hits| (hits.len(), hits))
            }
        });
        match found {
            Ok((count, hits)) => Scanned {
                searched: 1,
                failed: 0,
                found: (count > 0).then(|| Found {
                    source: source.to_owned(),
                    count,
                    hits,
                }),
            },
            Err(error) => {
                tracing::warn!("Skipped {source}: {error}");
                Scanned {
                    searched: 1,
                    failed: 1,
                    found: None,
                }
            }
        }
    }

    /// Scans the documents of `job` and sends one result per document to `results`. Waits at
    /// `gate` before it sends collected hits. Stops between two documents if `stop` is set or
    /// if the receiver of `results` was dropped.
    fn work(&self, job: &Job, stop: &AtomicBool, gate: &Gate, results: &mpsc::Sender<Scanned>) {
        let send = |scanned: Scanned| {
            let hits = scanned.found.as_ref().map_or(0, |found| found.hits.len());
            gate.enter(hits, stop) && results.send(scanned).is_ok()
        };

        match job {
            Job::File(path) => {
                send(self.scan(path.as_str(), std::fs::read(path).into_diagnostic()));
            }
            Job::Archive { name, path, bins } => {
                let mounted = File::open(path)
                    .into_diagnostic()
                    .and_then(|file| Wad::mount(BufReader::new(file)).into_diagnostic());
                let mut wad = match mounted {
                    Ok(wad) => wad,
                    Err(error) => {
                        tracing::warn!("Skipped the archive {name}: {error}");
                        send(Scanned {
                            searched: bins.len(),
                            failed: bins.len(),
                            found: None,
                        });
                        return;
                    }
                };
                for (chunk, bin) in bins {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let data = match wad.chunks().get(*chunk).copied() {
                        Some(entry) => wad
                            .load_chunk_decompressed(&entry)
                            .map(|data| data.into_vec())
                            .into_diagnostic(),
                        None => Err(miette::miette!(
                            "{name} does not contain the chunk. The archive changed after it was indexed"
                        )),
                    };
                    if !send(self.scan(bin, data)) {
                        break;
                    }
                }
            }
        }
    }
}

/// One match as a JSON object.
#[derive(Serialize)]
struct Record<'a> {
    /// The file path, or the chunk path of a game bin.
    source: &'a str,
    /// The game archive that contains the bin.
    #[serde(skip_serializing_if = "Option::is_none")]
    archive: Option<&'a str>,
    #[serde(flatten)]
    row: &'a Row,
}

/// Prints the hits of each document in the selected [`Mode`] and counts them.
struct Printer<'a, W: Write> {
    names: &'a GameNames,
    mode: Mode,
    /// The number of matches that can still be printed before the limit is reached.
    remaining: usize,
    totals: Totals,
    /// The matches of the `Json` mode, which are printed by `finish` as one array.
    records: Vec<serde_json::Value>,
    out: &'a mut W,
}

impl<W: Write> Printer<'_, W> {
    /// Prints the hits of one document. `archive` is the game archive that contains it. Returns
    /// `false` if the match limit is reached.
    fn document(&mut self, found: &Found, archive: Option<&str>) -> io::Result<bool> {
        let count = found.count.min(self.remaining);
        if count == 0 {
            return Ok(false);
        }
        self.remaining -= count;
        self.totals.matches += count;
        self.totals.sources += 1;
        let source = found.source.as_str();
        // `hits` is empty if the workers only count the hits.
        let hits = &found.hits[..found.hits.len().min(count)];

        match self.mode {
            Mode::Paths => writeln!(self.out, "{source}")?,
            Mode::Counts => writeln!(self.out, "{source}: {count}")?,
            Mode::Text => {
                if self.totals.sources > 1 {
                    writeln!(self.out)?;
                }
                match archive {
                    Some(archive) => writeln!(
                        self.out,
                        "{} {}",
                        source.magenta().bold(),
                        format!("[{archive}]").dimmed()
                    )?,
                    None => writeln!(self.out, "{}", source.magenta().bold())?,
                }
                let mut object = None;
                for hit in hits {
                    let row = Row::new(hit, self.names);
                    if row.object.is_some() && row.object != object {
                        writeln!(self.out, "{}", heading(&row))?;
                        object.clone_from(&row.object);
                    }
                    if let Some(line) = line(&row) {
                        writeln!(self.out, "{line}")?;
                    }
                }
            }
            Mode::Json | Mode::Jsonl => {
                for hit in hits {
                    let row = Row::new(hit, self.names);
                    let record = Record {
                        source,
                        archive,
                        row: &row,
                    };
                    match self.mode {
                        Mode::Json => self
                            .records
                            .push(serde_json::to_value(&record).map_err(io::Error::other)?),
                        _ => {
                            serde_json::to_writer(&mut *self.out, &record)
                                .map_err(io::Error::other)?;
                            writeln!(self.out)?;
                        }
                    }
                }
            }
        }
        self.out.flush()?;
        Ok(self.remaining > 0)
    }

    /// Prints the array of the `Json` mode and flushes the output.
    fn finish(&mut self) -> io::Result<()> {
        if self.mode == Mode::Json {
            serde_json::to_writer_pretty(&mut *self.out, &self.records)
                .map_err(io::Error::other)?;
            writeln!(self.out)?;
        }
        self.out.flush()
    }
}

/// Returns `text` in the color of a matched part if `matched` is `true`, otherwise in `plain`.
fn highlight(text: &str, matched: bool, plain: fn(&str) -> ColoredString) -> ColoredString {
    match matched {
        true => text.red().bold(),
        false => plain(text),
    }
}

/// Returns the line that starts the hits of one object in the `Text` mode: the object path, or
/// its hash if the path is unknown, and the class. A part is highlighted if `row` is a hit on
/// the object itself and that part matched.
fn heading(row: &Row) -> String {
    let on_object = |part| row.path.is_none() && row.matched.contains(&part);
    let name = row
        .object_name
        .as_deref()
        .or(row.object.as_deref())
        .unwrap_or_default();
    format!(
        "  {} : {}",
        highlight(name, on_object(Matched::Entry), |text| text.green()),
        highlight(
            row.class.as_deref().unwrap_or_default(),
            on_object(Matched::Class),
            |text| text.cyan()
        )
    )
}

/// Returns the line of `row` in the `Text` mode, with the matched parts highlighted. Returns
/// `None` for a hit on the object itself, which is printed by its heading.
fn line(row: &Row) -> Option<String> {
    let matched = |part| row.matched.contains(&part);
    let value_type = row.value_type.as_deref().unwrap_or_default().dimmed();
    let value = row.value.as_deref().map(|value| {
        highlight(
            value,
            matched(Matched::Value) || matched(Matched::Class) || matched(Matched::Dependency),
            |text| text.normal(),
        )
    });

    let mut line = match &row.path {
        Some(path) => format!(
            "    {}: {value_type}",
            highlight(
                path,
                matched(Matched::Field) || matched(Matched::Key),
                |text| text.normal()
            )
        ),
        None if row.object.is_none() => format!("  linked: {value_type}"),
        None => return None,
    };
    if let Some(value) = value {
        line.push_str(&format!(" = {value}"));
    }
    if let Some(count) = row.count {
        line.push_str(&format!(" ({})", plural(count, "item")));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use ltk_hash::Hash as _;
    use ltk_meta::{Bin, BinFile, BinObject, property::values};

    use super::*;
    use crate::{
        cli::{self, Commands},
        document::to_bin,
        game::testing::Installation,
    };

    /// Parses the arguments of a `search` command line.
    fn args(arguments: &[&str]) -> SearchArgs {
        let line = ["ritobin-tools", "search"]
            .into_iter()
            .chain(arguments.iter().copied())
            .map(OsString::from);
        match cli::try_parse(line).unwrap().command {
            Commands::Search(args) => args,
            _ => panic!("not the search command"),
        }
    }

    /// Returns a bin with one object that has the string property `name` and the number
    /// property `count`.
    fn bin(object: &str, name: &str, count: i32) -> Vec<u8> {
        let bin: BinFile = Bin::builder()
            .dependency("DATA/Shared.bin")
            .object(
                BinObject::builder(BinHash::hash_str(object), BinHash::hash_str("Item"))
                    .property(BinHash::hash_str("name"), values::String::from(name))
                    .property(BinHash::hash_str("count"), values::I32::new(count))
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

    /// Writes three bins to `dir`: `a.bin`, `b.bin` and `nested/c.bin`.
    fn write_bins(dir: &Utf8Path) {
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("a.bin"), bin("Items/Sword", "Long Sword", 3)).unwrap();
        std::fs::write(dir.join("b.bin"), bin("Items/Shield", "Round Shield", 1)).unwrap();
        std::fs::write(
            dir.join("nested").join("c.bin"),
            bin("Items/Bow", "Long Bow", 3),
        )
        .unwrap();
    }

    /// Runs the search of `arguments` over the files of `dir` without hashtables. Returns the
    /// output with `/` separators and with `dir` removed from the paths, and the totals. The
    /// output of a JSON format is not valid JSON after this change.
    fn run_in(dir: &Utf8Path, arguments: &[&str], threads: usize) -> (String, Totals) {
        let (out, totals) = run_raw(dir, arguments, threads);
        let out = out
            .replace('\\', "/")
            .replace(&format!("{}/", dir.as_str().replace('\\', "/")), "");
        (out, totals)
    }

    /// Runs the search of `arguments` over the files of `dir` without hashtables. Returns the
    /// output and the totals.
    fn run_raw(dir: &Utf8Path, arguments: &[&str], threads: usize) -> (String, Totals) {
        colored::control::set_override(false);
        let args = args(arguments);
        let (query, _) = args.query().unwrap();
        let names = GameNames::default();
        let matcher = Matcher::compile(&query, &names.bins, &names.paths).unwrap();
        let jobs = file_jobs(&[dir.to_owned()], args.bin.as_deref()).unwrap();

        let limit = args.limit.map_or(usize::MAX, NonZeroUsize::get);
        let mut out = Vec::new();
        let totals = search(
            &jobs,
            &matcher,
            &names,
            args.mode(),
            limit,
            threads,
            &mut out,
        )
        .unwrap();
        (String::from_utf8(out).unwrap(), totals)
    }

    #[test]
    fn query_uses_first_positional_as_pattern_unless_regex_or_values_is_set() {
        let (query, paths) = args(&["sword", "data"]).query().unwrap();
        assert_eq!(query.pattern, Pattern::Literal("sword".to_owned()));
        assert_eq!(paths, ["data"]);

        let (query, paths) = args(&["-e", "sw.rd", "data", "more"]).query().unwrap();
        assert_eq!(query.pattern, Pattern::Regex("sw.rd".to_owned()));
        assert_eq!(paths, ["data", "more"]);

        let (query, paths) = args(&["--values", "--field", "name", "data"])
            .query()
            .unwrap();
        assert_eq!(query.pattern, Pattern::Any);
        assert_eq!(query.field, Some(BinHash::hash_str("name")));
        assert_eq!(paths, ["data"]);

        assert!(args(&[]).query().is_err());
        assert!(args(&["--field", "name"]).query().is_err());
    }

    #[test]
    fn file_jobs_lists_documents_of_directory_and_subdirectories() {
        let (_guard, dir) = temp_dir();
        write_bins(&dir);
        std::fs::write(dir.join("text.rito"), "#PROP_text\n").unwrap();
        std::fs::write(dir.join("legacy.py"), "#PROP_text\n").unwrap();
        std::fs::write(dir.join("script.py"), "print('hi')\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();

        let names = |jobs: Vec<Job>| -> Vec<String> {
            jobs.into_iter()
                .map(|job| match job {
                    Job::File(path) => path.strip_prefix(&dir).unwrap().as_str().replace('\\', "/"),
                    Job::Archive { name, .. } => name,
                })
                .collect()
        };
        assert_eq!(
            names(file_jobs(std::slice::from_ref(&dir), None).unwrap()),
            ["a.bin", "b.bin", "legacy.py", "nested/c.bin", "text.rito"]
        );
        assert_eq!(
            names(file_jobs(std::slice::from_ref(&dir), Some("NESTED/")).unwrap()),
            ["nested/c.bin"]
        );

        // A file that is passed directly is searched whatever its extension is.
        assert_eq!(
            names(file_jobs(&[dir.join("notes.txt")], None).unwrap()),
            ["notes.txt"]
        );
        let error = file_jobs(&[dir.join("missing.bin")], None).unwrap_err();
        assert!(error.to_string().contains("Input does not exist"));
    }

    #[test]
    fn search_prints_matches_grouped_by_document_and_object() {
        let (_guard, dir) = temp_dir();
        write_bins(&dir);
        let sword = format!("0x{:08x}", BinHash::hash_str("Items/Sword").0);
        let bow = format!("0x{:08x}", BinHash::hash_str("Items/Bow").0);
        let item = format!("0x{:08x}", BinHash::hash_str("Item").0);
        let name = format!("{:08x}", BinHash::hash_str("name").0);

        let (out, totals) = run_in(&dir, &["long"], 2);
        assert_eq!(
            out,
            format!(
                "a.bin\n  {sword} : {item}\n    {name}: string = \"Long Sword\"\n\nnested/c.bin\n  {bow} : {item}\n    {name}: string = \"Long Bow\"\n"
            )
        );
        assert_eq!(
            totals,
            Totals {
                matches: 2,
                sources: 2,
                searched: 3,
                failed: 0,
            }
        );
    }

    #[test]
    fn search_prints_entry_match_as_object_line_and_dependency_as_linked_line() {
        let (_guard, dir) = temp_dir();
        write_bins(&dir);
        let shield = format!("0x{:08x}", BinHash::hash_str("Items/Shield").0);
        let item = format!("0x{:08x}", BinHash::hash_str("Item").0);

        let (out, _) = run_in(&dir, &["Items/Shield"], 1);
        assert_eq!(out, format!("b.bin\n  {shield} : {item}\n"));

        let (out, totals) = run_in(&dir, &["shared", "--bin", "a.bin"], 1);
        assert_eq!(out, "a.bin\n  linked: string = \"DATA/Shared.bin\"\n");
        assert_eq!(totals.searched, 1);
    }

    #[test]
    fn search_prints_paths_counts_and_json() {
        let (_guard, dir) = temp_dir();
        write_bins(&dir);

        let (out, _) = run_in(&dir, &["3", "-l"], 2);
        assert_eq!(out, "a.bin\nnested/c.bin\n");

        let (out, _) = run_in(&dir, &["-e", "sword|^3$", "-c"], 2);
        assert_eq!(out, "a.bin: 2\nnested/c.bin: 1\n");

        let (out, _) = run_raw(&dir, &["round", "-f", "jsonl"], 2);
        let record: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(record["source"], dir.join("b.bin").as_str());
        assert_eq!(record["matched"], serde_json::json!(["value"]));
        assert_eq!(record["type"], "string");
        assert_eq!(record["value"], "\"Round Shield\"");
        assert!(record.get("archive").is_none());

        let (out, _) = run_raw(&dir, &["long", "-f", "json"], 2);
        let records: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[1]["source"],
            dir.join("nested").join("c.bin").as_str()
        );

        let (out, totals) = run_raw(&dir, &["no such text", "-f", "json"], 2);
        assert_eq!(out, "[]\n");
        assert_eq!(totals.matches, 0);
    }

    #[test]
    fn search_output_is_the_same_for_every_thread_count() {
        let (_guard, dir) = temp_dir();
        for index in 0..40 {
            let path = dir.join(format!("{index:02}.bin"));
            std::fs::write(path, bin(&format!("Items/{index}"), "Long Sword", index)).unwrap();
        }

        let (single, totals) = run_in(&dir, &["sword", "-c"], 1);
        assert_eq!(single.lines().count(), 40);
        assert_eq!(totals.matches, 40);
        for threads in [2, 8, 64] {
            assert_eq!(run_in(&dir, &["sword", "-c"], threads).0, single);
        }

        let (limited, totals) = run_in(&dir, &["sword", "-c", "-m", "7"], 8);
        assert_eq!(
            limited,
            single
                .lines()
                .take(7)
                .fold(String::new(), |out, line| out + line + "\n")
        );
        assert_eq!(totals.matches, 7);
    }

    #[test]
    fn search_prints_all_hits_when_jobs_exceed_queued_hit_limit() {
        let (_guard, dir) = temp_dir();
        // Each file has more hits than half of `QUEUED_HITS`, so a worker that runs ahead of
        // the output waits at its gate.
        let per_file = QUEUED_HITS / 2 + 8;
        for file in 0..12 {
            let items: String = (0..per_file)
                .map(|item| format!("            \"Long Sword {file} {item}\"\n"))
                .collect();
            let text = format!(
                "#PROP_text\ntype: string = \"PROP\"\nversion: u32 = 3\nlinked: list[string] = {{ }}\nentries: map[hash,embed] = {{\n    0x1 = 0x2 {{\n        0x10: list[string] = {{\n{items}        }}\n    }}\n}}\n"
            );
            std::fs::write(dir.join(format!("{file:02}.rito")), text).unwrap();
        }

        let (single, totals) = run_raw(&dir, &["sword", "-f", "jsonl"], 1);
        assert_eq!(totals.matches, 12 * per_file);
        assert_eq!(single.lines().count(), 12 * per_file);
        let last: serde_json::Value = serde_json::from_str(single.lines().last().unwrap()).unwrap();
        assert_eq!(
            last["value"],
            format!("\"Long Sword 11 {}\"", per_file - 1).as_str()
        );
        for threads in [3, 16] {
            assert_eq!(run_raw(&dir, &["sword", "-f", "jsonl"], threads).0, single);
        }

        let (limited, totals) = run_raw(&dir, &["sword", "-f", "jsonl", "-m", "100"], 16);
        assert_eq!(totals.matches, 100);
        assert_eq!(limited.lines().count(), 100);
        assert!(single.starts_with(&limited));
    }

    #[test]
    fn gate_enter_waits_at_limit_until_leave_or_stop() {
        let gate = Gate::default();
        let stop = AtomicBool::new(false);
        assert!(gate.enter(QUEUED_HITS, &stop));

        // The gate is at the limit. A second `enter` waits until `leave` is called.
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| gate.enter(1, &stop));
            std::thread::sleep(Duration::from_millis(20));
            assert!(!waiting.is_finished());
            gate.leave(QUEUED_HITS);
            assert!(waiting.join().unwrap());
        });

        // `enter` returns `false` if the search is stopped during the wait.
        assert!(gate.enter(QUEUED_HITS, &stop));
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| gate.enter(1, &stop));
            stop.store(true, Ordering::Relaxed);
            assert!(!waiting.join().unwrap());
        });
    }

    #[test]
    fn search_counts_unreadable_document_as_failed() {
        let (_guard, dir) = temp_dir();
        write_bins(&dir);
        std::fs::write(dir.join("broken.bin"), b"PROP\x03\x00\x00\x00\x00").unwrap();

        let (out, totals) = run_in(&dir, &["shield", "-l"], 2);
        assert_eq!(out, "b.bin\n");
        assert_eq!(totals.searched, 4);
        assert_eq!(totals.failed, 1);
    }

    #[test]
    fn game_jobs_list_bins_by_archive_and_apply_filters() {
        let installation = Installation::new();
        installation.archive(
            "Champions/Teemo.wad.client",
            &[
                ("data/sword.bin", &bin("Items/Sword", "Long Sword", 3)),
                ("data/notes.txt", b"long text that is not a bin"),
            ],
        );
        installation.archive(
            "Maps/Map11.wad.client",
            &[("data/bow.bin", &bin("Items/Bow", "Long Bow", 3))],
        );
        let ctx = Context::for_tests(None);
        let game = [
            "--game-dir",
            installation.root.as_str(),
            "--index-dir",
            installation.root.join("index").as_str(),
        ]
        .map(str::to_owned);
        let with = |extra: &[&str]| {
            let line: Vec<&str> = extra
                .iter()
                .copied()
                .chain(game.iter().map(String::as_str))
                .collect();
            args(&line)
        };

        // No hashtable is loaded, so a chunk is named by its hash.
        let sword = format!("{:016x}", WadHash::hash_str("data/sword.bin").0);
        let bow = format!("{:016x}", WadHash::hash_str("data/bow.bin").0);
        let jobs = game_jobs(&ctx, &with(&["long"])).unwrap();
        let listed: Vec<(&str, Vec<&str>)> = jobs
            .iter()
            .map(|job| match job {
                Job::Archive { name, bins, .. } => (
                    name.as_str(),
                    bins.iter().map(|(_, bin)| bin.as_str()).collect(),
                ),
                Job::File(path) => (path.as_str(), Vec::new()),
            })
            .collect();
        assert_eq!(
            listed,
            [
                ("Champions/Teemo.wad.client", vec![sword.as_str()]),
                ("Maps/Map11.wad.client", vec![bow.as_str()]),
            ]
        );

        assert_eq!(
            game_jobs(&ctx, &with(&["long", "--wad", "map11"]))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            game_jobs(&ctx, &with(&["long", "--bin", &bow]))
                .unwrap()
                .len(),
            1
        );
        assert!(
            game_jobs(&ctx, &with(&["long", "--wad", "none"]))
                .unwrap()
                .is_empty()
        );

        colored::control::set_override(false);
        let names = GameNames::default();
        let (query, _) = with(&["long"]).query().unwrap();
        let matcher = Matcher::compile(&query, &names.bins, &names.paths).unwrap();
        let mut out = Vec::new();
        let totals = search(
            &jobs,
            &matcher,
            &names,
            Mode::Jsonl,
            usize::MAX,
            2,
            &mut out,
        )
        .unwrap();
        assert_eq!(totals.matches, 2);
        let records: Vec<serde_json::Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records[0]["source"], sword.as_str());
        assert_eq!(records[0]["archive"], "Champions/Teemo.wad.client");
        assert_eq!(records[1]["archive"], "Maps/Map11.wad.client");
        assert_eq!(records[1]["value"], "\"Long Bow\"");
    }

    #[test]
    fn check_game_dir_fails_without_game_directory() {
        let ctx = Context::for_tests(None);
        let error = check_game_dir(&ctx, &args(&["long"])).unwrap_err();
        assert!(error.to_string().contains("no game directory is set"));
        assert!(check_game_dir(&ctx, &args(&["long", "--game-dir", "League"])).is_ok());
    }
}
