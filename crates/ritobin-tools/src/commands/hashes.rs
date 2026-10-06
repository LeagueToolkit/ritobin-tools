use std::{collections::HashMap, fmt::Write as _, sync::Mutex};

use camino::Utf8PathBuf;
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use ltk_hash::{BinHash, Hash as _};
use ltk_mimir_cache::{
    ManifestError, PlannedTable, ReleaseSource, Table, UpdateObserver, UpdateOptions,
    UpdateOutcome, UreqFetch,
};
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;

use crate::{
    context::Context,
    document::{STDIO, write_bytes},
    hashes::{BIN_TABLES, MIMIR_TABLES_REPO, format_hash, parse_hash},
    utils::{hyperlink_path, plural},
};

#[derive(Subcommand, Debug)]
pub enum HashesCommand {
    /// Download the latest hashtables into the shared cache
    #[command(visible_alias = "update")]
    Sync(SyncArgs),

    /// Compare the installed hashtables with the latest release, without downloading them
    Check {
        #[command(flatten)]
        remote: RemoteArgs,

        #[command(flatten)]
        output: OutputArgs,
    },

    /// Show the installed hashtables
    Status {
        #[command(flatten)]
        output: OutputArgs,
    },

    /// Print the hashtable cache directory
    Dir,

    /// Resolve bin hashes to their names
    Lookup {
        /// Hashes in hex, with or without `0x`
        #[arg(required = true, value_name = "HASH")]
        hashes: Vec<String>,

        /// Look in one table instead of all four
        #[arg(short, long, value_enum)]
        table: Option<BinTable>,

        #[command(flatten)]
        output: OutputArgs,
    },

    /// Hash names the way bins do, and show which tables know them
    Hash {
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,

        #[command(flatten)]
        output: OutputArgs,
    },

    /// Find the names that contain a text, ignoring case
    Search {
        /// The text to look for
        text: String,

        /// Search one table instead of all four
        #[arg(short, long, value_enum)]
        table: Option<BinTable>,

        /// The most names to print. 0 prints all of them
        #[arg(short = 'n', long, value_name = "N", default_value_t = 50)]
        limit: usize,

        #[command(flatten)]
        output: OutputArgs,
    },

    /// Write a table as a CDragon text list: one `<hash> <name>` per line
    Export {
        /// The table to write
        #[arg(value_enum)]
        table: BinTable,

        /// Write to a file instead of standard output
        #[arg(short, long, value_name = "FILE")]
        output: Option<Utf8PathBuf>,
    },
}

/// Where the hashtable releases are fetched from.
#[derive(Args, Debug, Clone)]
pub struct RemoteArgs {
    /// The GitHub repository whose latest release holds the tables
    #[arg(long, value_name = "OWNER/REPO", default_value = MIMIR_TABLES_REPO)]
    pub repo: String,

    /// A base URL to fetch the tables from instead of a GitHub release
    #[arg(long, value_name = "URL", conflicts_with = "repo")]
    pub url: Option<String>,
}

impl RemoteArgs {
    fn fetcher(&self) -> UreqFetch {
        UreqFetch::new(match &self.url {
            Some(url) => ReleaseSource::base_url(url.as_str()),
            None => ReleaseSource::github(&self.repo),
        })
    }

    fn describe(&self) -> &str {
        self.url.as_deref().unwrap_or(&self.repo)
    }
}

#[derive(Args, Debug, Clone)]
pub struct SyncArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,

    /// Download every table again, even the ones that are up to date
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug, Clone, Copy)]
pub struct OutputArgs {
    /// How to print the result
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
}

/// One of the four tables a bin's hashes resolve against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BinTable {
    /// Object paths
    Entries,
    /// Property names
    Fields,
    /// Values of `hash` and `link` properties
    Hashes,
    /// Class names
    Types,
}

impl From<BinTable> for Table {
    fn from(table: BinTable) -> Self {
        match table {
            BinTable::Entries => Table::BinEntries,
            BinTable::Fields => Table::BinFields,
            BinTable::Hashes => Table::BinHashes,
            BinTable::Types => Table::BinTypes,
        }
    }
}

/// The tables a query looks in: the one asked for, or all four.
fn tables(table: Option<BinTable>) -> Vec<Table> {
    match table {
        Some(table) => vec![table.into()],
        None => BIN_TABLES.to_vec(),
    }
}

pub fn run(ctx: &Context, command: HashesCommand) -> Result<()> {
    match command {
        HashesCommand::Sync(args) => sync(ctx, &args),
        HashesCommand::Check { remote, output } => check(ctx, &remote, output.format),
        HashesCommand::Status { output } => status(ctx, output.format),
        HashesCommand::Dir => dir(ctx),
        HashesCommand::Lookup {
            hashes,
            table,
            output,
        } => lookup(ctx, &hashes, table, output.format),
        HashesCommand::Hash { names, output } => hash(ctx, &names, output.format),
        HashesCommand::Search {
            text,
            table,
            limit,
            output,
        } => search(ctx, &text, table, limit, output.format),
        HashesCommand::Export { table, output } => export(ctx, table, output),
    }
}

/// Prints `rows` as JSON, or as the text `table` makes of them.
fn print<T: Serialize>(
    rows: &[T],
    format: OutputFormat,
    table: impl FnOnce(&[T]) -> String,
) -> Result<()> {
    let out = match format {
        OutputFormat::Json => {
            let mut out = serde_json::to_string_pretty(rows).into_diagnostic()?;
            out.push('\n');
            out
        }
        OutputFormat::Table => table(rows),
    };
    write_bytes(STDIO.into(), out.as_bytes())
}

/// Lays `rows` out in columns under `header`, each column as wide as its widest cell.
fn columns<const N: usize>(header: [&str; N], rows: &[[String; N]]) -> String {
    let mut widths = header.map(str::len);
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let mut out = String::new();
    let mut line = |cells: [&str; N], bold: bool| {
        let mut text = String::new();
        for (index, (cell, width)) in cells.iter().zip(widths).enumerate() {
            match index + 1 == N {
                true => text.push_str(cell),
                false => {
                    let _ = write!(text, "{cell:<width$}  ");
                }
            }
        }
        let text = text.trim_end();
        let _ = match bold {
            true => writeln!(out, "{}", text.bold()),
            false => writeln!(out, "{text}"),
        };
    };
    line(header, true);
    for row in rows {
        line(std::array::from_fn(|index| row[index].as_str()), false);
    }
    out
}

fn size(bytes: Option<u64>) -> String {
    match bytes {
        Some(bytes) => format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0)),
        None => "-".to_owned(),
    }
}

/// Moves one progress bar over the bytes of every table an update downloads.
struct DownloadProgress {
    bar: ProgressBar,
    done: Mutex<HashMap<Table, u64>>,
}

impl DownloadProgress {
    fn new() -> Self {
        let bar = ProgressBar::hidden();
        bar.set_style(
            ProgressStyle::with_template(
                "{msg}\n{wide_bar:40.cyan/blue} {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
            )
            .expect("the progress template is valid"),
        );
        Self {
            bar,
            done: Mutex::new(HashMap::new()),
        }
    }
}

impl UpdateObserver for DownloadProgress {
    fn planned(&self, tables: &[PlannedTable]) {
        if tables.is_empty() {
            return;
        }
        let total: u64 = tables.iter().filter_map(|table| table.size_bytes).sum();
        self.bar.set_length(total);
        self.bar
            .set_message(format!("Downloading {}", plural(tables.len(), "table")));
        self.bar
            .set_draw_target(indicatif::ProgressDrawTarget::stderr());
    }

    fn progressed(&self, table: Table, done: u64, _total: Option<u64>) {
        let mut all = self.done.lock().unwrap_or_else(|error| error.into_inner());
        all.insert(table, done);
        self.bar.set_position(all.values().sum());
    }
}

pub fn sync(ctx: &Context, args: &SyncArgs) -> Result<()> {
    let store = ctx.store()?;
    tracing::info!(
        "Syncing hashtables from {} into {}",
        args.remote.describe(),
        store.dir().display()
    );

    let progress = DownloadProgress::new();
    let mut options = UpdateOptions::default().observed_by(&progress);
    if args.force {
        options = options.forced();
    }
    let outcome = store.update(&args.remote.fetcher(), options);
    progress.bar.finish_and_clear();

    match outcome
        .into_diagnostic()
        .wrap_err("Failed to sync the hashtables")?
    {
        UpdateOutcome::Locked => match store.lock_holder().ok().flatten() {
            Some(holder) => tracing::info!(
                "Process {} is already syncing the hashtables (since {}). Nothing was done.",
                holder.pid,
                holder.since
            ),
            None => {
                tracing::info!(
                    "Another process is already syncing the hashtables. Nothing was done."
                )
            }
        },
        UpdateOutcome::Completed(report) => {
            if report.installed.is_empty() {
                tracing::info!("Every hashtable is up to date");
            } else {
                let installed: Vec<&str> =
                    report.installed.iter().map(|table| table.id()).collect();
                tracing::info!(
                    "Installed {}: {}",
                    plural(installed.len(), "table"),
                    installed.join(", ")
                );
            }
            for table in &report.unsupported_tables {
                tracing::warn!(
                    "The {} table is in format {}, which this version cannot read. Update ritobin-tools.",
                    table.table,
                    table.format_version
                );
            }
            if !report.unknown_tables.is_empty() {
                tracing::debug!(
                    "The release has tables this version does not know: {}",
                    report.unknown_tables.join(", ")
                );
            }
        }
        _ => tracing::info!("The sync finished"),
    }
    Ok(())
}

#[derive(Serialize)]
struct CheckRow {
    table: String,
    status: String,
    installed: Option<String>,
    latest: String,
    download_bytes: Option<u64>,
}

fn check(ctx: &Context, remote: &RemoteArgs, format: OutputFormat) -> Result<()> {
    let store = ctx.store()?;
    let report = store
        .check(&remote.fetcher())
        .into_diagnostic()
        .wrap_err("Failed to check the hashtables")?;

    let rows: Vec<CheckRow> = report
        .tables
        .iter()
        .map(|diff| CheckRow {
            table: diff.table.id().to_owned(),
            status: diff.status.to_string(),
            installed: diff.local.as_ref().map(|local| local.version.clone()),
            latest: diff.remote.version.clone(),
            download_bytes: diff
                .status
                .needs_update()
                .then_some(diff.remote.size_bytes)
                .flatten(),
        })
        .collect();

    print(&rows, format, |rows| {
        let cells: Vec<[String; 4]> = rows
            .iter()
            .map(|row| {
                [
                    row.table.clone(),
                    row.installed.clone().unwrap_or_else(|| "-".to_owned()),
                    row.latest.clone(),
                    row.status.clone(),
                ]
            })
            .collect();
        columns(["TABLE", "INSTALLED", "LATEST", "STATUS"], &cells)
    })?;

    match report.behind() {
        0 => tracing::info!("Every hashtable is up to date"),
        behind => tracing::info!(
            "{} out of date ({} to download). Run `ritobin-tools hashes sync`.",
            plural(behind, "table"),
            size(report.download_bytes())
        ),
    }
    Ok(())
}

#[derive(Serialize)]
struct StatusRow {
    table: String,
    version: String,
    entries: u64,
    size_bytes: Option<u64>,
    file: String,
    /// Whether the table's file is in the cache directory.
    present: bool,
}

fn status(ctx: &Context, format: OutputFormat) -> Result<()> {
    let store = ctx.store()?;
    let manifest = match store.manifest() {
        Ok(manifest) => manifest,
        Err(ManifestError::Missing(_)) => {
            tracing::warn!(
                "No hashtables are installed in {}. Run `ritobin-tools hashes sync`.",
                store.dir().display()
            );
            return print::<StatusRow>(&[], format, |_| String::new());
        }
        Err(error) => {
            return Err(error)
                .into_diagnostic()
                .wrap_err("Failed to read the hashtable manifest");
        }
    };

    let rows: Vec<StatusRow> = Table::ALL
        .iter()
        .filter_map(|table| {
            let entry = manifest.entry(*table)?;
            Some(StatusRow {
                table: table.id().to_owned(),
                version: entry.version.clone(),
                entries: entry.entries,
                size_bytes: entry.size_bytes,
                file: entry.file.clone(),
                present: store.dir().join(&entry.file).exists(),
            })
        })
        .collect();

    print(&rows, format, |rows| {
        let cells: Vec<[String; 5]> = rows
            .iter()
            .map(|row| {
                [
                    row.table.clone(),
                    row.version.clone(),
                    row.entries.to_string(),
                    size(row.size_bytes),
                    match row.present {
                        true => row.file.clone(),
                        false => format!("{} (missing)", row.file),
                    },
                ]
            })
            .collect();
        columns(["TABLE", "VERSION", "ENTRIES", "SIZE", "FILE"], &cells)
    })?;

    tracing::info!(
        "Cache directory: {} (written {})",
        store.dir().display(),
        manifest.generated_at
    );
    Ok(())
}

fn dir(ctx: &Context) -> Result<()> {
    let store = ctx.store()?;
    println!("{}", store.dir().display());
    Ok(())
}

#[derive(Serialize)]
struct LookupRow {
    hash: String,
    table: Option<String>,
    name: Option<String>,
}

fn lookup(
    ctx: &Context,
    inputs: &[String],
    table: Option<BinTable>,
    format: OutputFormat,
) -> Result<()> {
    let hashes: Vec<BinHash> = inputs
        .iter()
        .map(|input| {
            parse_hash(input)
                .ok_or_else(|| miette::miette!("`{input}` is not a 32-bit hash in hex"))
        })
        .collect::<Result<_>>()?;

    let names = ctx.hashes();
    let mut rows = Vec::new();
    for hash in hashes {
        let found: Vec<LookupRow> = tables(table)
            .into_iter()
            .filter_map(|table| {
                let name = names.lookup(table, hash)?;
                Some(LookupRow {
                    hash: format_hash(hash),
                    table: Some(table.id().to_owned()),
                    name: Some(name.into_owned()),
                })
            })
            .collect();
        if found.is_empty() {
            rows.push(LookupRow {
                hash: format_hash(hash),
                table: None,
                name: None,
            });
        }
        rows.extend(found);
    }

    print(&rows, format, |rows| {
        let cells: Vec<[String; 3]> = rows
            .iter()
            .map(|row| {
                [
                    row.hash.clone(),
                    row.table.clone().unwrap_or_else(|| "-".to_owned()),
                    row.name.clone().unwrap_or_else(|| "(unknown)".to_owned()),
                ]
            })
            .collect();
        columns(["HASH", "TABLE", "NAME"], &cells)
    })
}

#[derive(Serialize)]
struct HashRow {
    name: String,
    hash: String,
    /// The tables that hold this name.
    known_in: Vec<String>,
}

fn hash(ctx: &Context, names: &[String], format: OutputFormat) -> Result<()> {
    let known = ctx.hashes();
    let rows: Vec<HashRow> = names
        .iter()
        .map(|name| {
            let hash = BinHash::hash_str(name);
            HashRow {
                name: name.clone(),
                hash: format_hash(hash),
                known_in: BIN_TABLES
                    .into_iter()
                    .filter(|table| {
                        known
                            .lookup(*table, hash)
                            .is_some_and(|found| found.eq_ignore_ascii_case(name))
                    })
                    .map(|table| table.id().to_owned())
                    .collect(),
            }
        })
        .collect();

    print(&rows, format, |rows| {
        let cells: Vec<[String; 3]> = rows
            .iter()
            .map(|row| {
                [
                    row.hash.clone(),
                    match row.known_in.is_empty() {
                        true => "-".to_owned(),
                        false => row.known_in.join(","),
                    },
                    row.name.clone(),
                ]
            })
            .collect();
        columns(["HASH", "KNOWN IN", "NAME"], &cells)
    })
}

#[derive(Serialize)]
struct SearchRow {
    hash: String,
    table: String,
    name: String,
}

fn search(
    ctx: &Context,
    text: &str,
    table: Option<BinTable>,
    limit: usize,
    format: OutputFormat,
) -> Result<()> {
    let names = ctx.hashes();
    let needle = text.to_ascii_lowercase();
    let mut rows = Vec::new();
    let mut total = 0usize;
    let mut searched = 0usize;

    for table in tables(table) {
        let loaded = names.for_each_name(table, |hash, name| {
            if !name.to_ascii_lowercase().contains(&needle) {
                return;
            }
            total += 1;
            if limit == 0 || rows.len() < limit {
                rows.push(SearchRow {
                    hash: format_hash(hash),
                    table: table.id().to_owned(),
                    name: name.to_owned(),
                });
            }
        });
        searched += usize::from(loaded);
    }
    if searched == 0 {
        miette::bail!("No hashtable to search is loaded. Run `ritobin-tools hashes sync`");
    }

    print(&rows, format, |rows| {
        let cells: Vec<[String; 3]> = rows
            .iter()
            .map(|row| [row.hash.clone(), row.table.clone(), row.name.clone()])
            .collect();
        columns(["HASH", "TABLE", "NAME"], &cells)
    })?;

    if total > rows.len() {
        tracing::info!(
            "Showing {} of {total} matches. Pass --limit 0 for all of them.",
            rows.len()
        );
    }
    Ok(())
}

fn export(ctx: &Context, table: BinTable, output: Option<Utf8PathBuf>) -> Result<()> {
    let table = Table::from(table);
    let mut out = String::new();
    let mut count = 0usize;
    let loaded = ctx.hashes().for_each_name(table, |hash, name| {
        let _ = writeln!(out, "{:08x} {name}", hash.0);
        count += 1;
    });
    if !loaded {
        miette::bail!("The {table} hashtable is not loaded. Run `ritobin-tools hashes sync`");
    }
    write_bytes(output.as_deref().unwrap_or(STDIO.into()), out.as_bytes())?;

    if let Some(output) = &output {
        tracing::info!(
            "Wrote {} of {table} to {}",
            plural(count, "name"),
            hyperlink_path(output)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_are_as_wide_as_their_widest_cell() {
        colored::control::set_override(false);
        let rows = [
            [
                "0x00000001".to_owned(),
                "binfields".to_owned(),
                "mName".to_owned(),
            ],
            ["0x2".to_owned(), "-".to_owned(), "(unknown)".to_owned()],
        ];
        assert_eq!(
            columns(["HASH", "TABLE", "NAME"], &rows),
            "HASH        TABLE      NAME\n0x00000001  binfields  mName\n0x2         -          (unknown)\n"
        );
    }

    #[test]
    fn a_table_choice_narrows_the_tables_searched() {
        assert_eq!(tables(Some(BinTable::Fields)), [Table::BinFields]);
        assert_eq!(tables(None), BIN_TABLES);
    }

    #[test]
    fn sizes_are_printed_in_mebibytes() {
        assert_eq!(size(Some(3 * 1024 * 1024 / 2)), "1.5 MiB");
        assert_eq!(size(None), "-");
    }
}
