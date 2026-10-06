use std::{collections::HashMap, fmt::Write as _, sync::Mutex};

use camino::Utf8PathBuf;
use clap::{Args, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use ltk_hash::{BinHash, Hash as _};
use ltk_mimir_cache::{
    ManifestError, PlannedTable, ReleaseSource, Table, UpdateObserver, UpdateOptions,
    UpdateOutcome, UreqFetch,
};
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;

use crate::{
    commands::output::{OutputArgs, OutputFormat, columns, print},
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

    /// Compare the installed hashtables with the latest release. Downloads nothing
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

        /// Look up in this table only. Defaults to all four bin tables
        #[arg(short, long, value_enum)]
        table: Option<BinTable>,

        #[command(flatten)]
        output: OutputArgs,
    },

    /// Compute the bin hash of names, and list the tables that contain each name
    Hash {
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,

        #[command(flatten)]
        output: OutputArgs,
    },

    /// List the names that contain a text. The match is case-insensitive
    Search {
        /// Text to search for
        text: String,

        /// Search this table only. Defaults to all four bin tables
        #[arg(short, long, value_enum)]
        table: Option<BinTable>,

        /// Maximum number of names to print. 0 means no limit
        #[arg(short = 'n', long, value_name = "N", default_value_t = 50)]
        limit: usize,

        #[command(flatten)]
        output: OutputArgs,
    },

    /// Write a table in the CDragon text format: one `<hash> <name>` per line
    Export {
        /// Table to export
        #[arg(value_enum)]
        table: BinTable,

        /// Write to a file instead of standard output
        #[arg(short, long, value_name = "FILE")]
        output: Option<Utf8PathBuf>,
    },
}

/// The download source of the hashtable releases.
#[derive(Args, Debug, Clone)]
pub struct RemoteArgs {
    /// GitHub repository. The tables are downloaded from its latest release
    #[arg(long, value_name = "OWNER/REPO", default_value = MIMIR_TABLES_REPO)]
    pub repo: String,

    /// Base URL to download the tables from. Replaces the GitHub release
    #[arg(long, value_name = "URL", conflicts_with = "repo")]
    pub url: Option<String>,
}

impl RemoteArgs {
    /// Returns the fetcher for the selected source.
    fn fetcher(&self) -> UreqFetch {
        UreqFetch::new(match &self.url {
            Some(url) => ReleaseSource::base_url(url.as_str()),
            None => ReleaseSource::github(&self.repo),
        })
    }

    /// Returns the URL or the repository name, for log messages.
    fn describe(&self) -> &str {
        self.url.as_deref().unwrap_or(&self.repo)
    }
}

#[derive(Args, Debug, Clone)]
pub struct SyncArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,

    /// Download all tables again, including tables that are up to date
    #[arg(long)]
    pub force: bool,
}

/// One of the four hashtables that resolve the hashes of a bin.
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

/// Returns the tables to search: `table` if it is set, otherwise all four bin tables.
fn tables(table: Option<BinTable>) -> Vec<Table> {
    match table {
        Some(table) => vec![table.into()],
        None => BIN_TABLES.to_vec(),
    }
}

/// Runs a `hashes` command.
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

/// Formats a byte count in mebibytes. Returns `-` for `None`.
fn size(bytes: Option<u64>) -> String {
    match bytes {
        Some(bytes) => format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0)),
        None => "-".to_owned(),
    }
}

/// Shows one progress bar for the total download size of an update.
struct DownloadProgress {
    bar: ProgressBar,
    /// The number of bytes downloaded so far for each table.
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

/// Downloads the latest hashtables into the cache and logs the installed tables.
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
                tracing::info!("All hashtables are up to date");
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
                    "The {} table uses format version {}, which this version of ritobin-tools does not support. Update ritobin-tools.",
                    table.table,
                    table.format_version
                );
            }
            if !report.unknown_tables.is_empty() {
                tracing::debug!(
                    "The release contains tables that this version does not recognize: {}",
                    report.unknown_tables.join(", ")
                );
            }
        }
        _ => tracing::info!("The sync finished"),
    }
    Ok(())
}

/// One row of the `hashes check` output.
#[derive(Serialize)]
struct CheckRow {
    table: String,
    status: String,
    installed: Option<String>,
    latest: String,
    download_bytes: Option<u64>,
}

/// Prints the installed and the latest version of each table.
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
        0 => tracing::info!("All hashtables are up to date"),
        behind => tracing::info!(
            "{} out of date ({} to download). Run `ritobin-tools hashes sync`.",
            plural(behind, "table"),
            size(report.download_bytes())
        ),
    }
    Ok(())
}

/// One row of the `hashes status` output.
#[derive(Serialize)]
struct StatusRow {
    table: String,
    version: String,
    entries: u64,
    size_bytes: Option<u64>,
    file: String,
    /// `true` if the table file exists in the cache directory.
    present: bool,
}

/// Prints the tables listed in the cache manifest.
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
        "Cache directory: {} (manifest generated {})",
        store.dir().display(),
        manifest.generated_at
    );
    Ok(())
}

/// Prints the cache directory.
fn dir(ctx: &Context) -> Result<()> {
    let store = ctx.store()?;
    println!("{}", store.dir().display());
    Ok(())
}

/// One row of the `hashes lookup` output.
#[derive(Serialize)]
struct LookupRow {
    hash: String,
    table: Option<String>,
    name: Option<String>,
}

/// Prints the name of each hash in each table that contains it. A hash that no table contains
/// gets one row without a name.
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
                .ok_or_else(|| miette::miette!("`{input}` is not a valid 32-bit hex hash"))
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

/// One row of the `hashes hash` output.
#[derive(Serialize)]
struct HashRow {
    name: String,
    hash: String,
    /// The tables that contain the name.
    known_in: Vec<String>,
}

/// Prints the bin hash of each name and the tables that contain the name.
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

/// One row of the `hashes search` output.
#[derive(Serialize)]
struct SearchRow {
    hash: String,
    table: String,
    name: String,
}

/// Prints the names that contain `text`, compared case-insensitively. Prints at most `limit`
/// names, or all names if `limit` is 0.
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
        miette::bail!("No hashtable is loaded. Run `ritobin-tools hashes sync`");
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
            "Showing {} of {total} matches. Pass --limit 0 to show all matches.",
            rows.len()
        );
    }
    Ok(())
}

/// Writes all names of `table` in the CDragon text format to `output`, or to standard output.
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
            "Exported {} from {table} to {}",
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
    fn tables_returns_selected_table_or_all() {
        assert_eq!(tables(Some(BinTable::Fields)), [Table::BinFields]);
        assert_eq!(tables(None), BIN_TABLES);
    }

    #[test]
    fn size_formats_mebibytes() {
        assert_eq!(size(Some(3 * 1024 * 1024 / 2)), "1.5 MiB");
        assert_eq!(size(None), "-");
    }
}
