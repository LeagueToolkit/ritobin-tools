use std::ffi::OsString;

use camino::{Utf8Path, Utf8PathBuf};
use clap::builder::{Styles, styling::AnsiColor};
use clap::{ColorChoice, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use ltk_mimir_cache::HashStore;
use miette::{IntoDiagnostic, Result, miette};
use tracing::Level;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{filter, fmt};

use crate::{
    commands::{config_cmd, convert, diff, download_hashes},
    hashes::HashProvider,
    utils::config::{AppConfig, load_or_create_config},
};

mod commands;
mod hashes;
mod utils;

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Show current configuration
    Show,
    /// Set a configuration value
    Set {
        /// Configuration key to set (e.g., 'hashtable_dir')
        key: String,
        /// Value to set for the configuration key
        value: String,
    },
    /// Reset configuration to defaults
    Reset,
}

#[derive(Default, Copy, Clone, Debug, ValueEnum)]
pub enum VerbosityLevel {
    /// Show errors and above
    Error,
    /// Show warnings and above
    Warning,
    /// Show info messages and above
    #[default]
    Info,
    /// Show debug messages and above
    Debug,
    /// Show all messages including trace
    Trace,
}

impl From<VerbosityLevel> for Level {
    fn from(level: VerbosityLevel) -> Self {
        match level {
            VerbosityLevel::Error => Level::ERROR,
            VerbosityLevel::Warning => Level::WARN,
            VerbosityLevel::Info => Level::INFO,
            VerbosityLevel::Debug => Level::DEBUG,
            VerbosityLevel::Trace => Level::TRACE,
        }
    }
}

impl VerbosityLevel {
    pub fn to_level_filter(&self) -> LevelFilter {
        LevelFilter::from_level((*self).into())
    }
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None, styles = cli_styles())]
struct Args {
    /// Set the verbosity level
    #[arg(short = 'L', long, value_enum, default_value_t = VerbosityLevel::Info)]
    verbosity: VerbosityLevel,

    /// Optional path to a config file (TOML). Defaults to `ritobin-tools.toml` if present
    #[arg(long)]
    config: Option<String>,

    /// Optional directory to load hashtable files from
    /// Overrides the default discovery directory and config value when provided
    #[arg(long, value_name = "DIR")]
    hashtable_dir: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Convert between .bin (binary) and .py/.ritobin (text) formats
    Convert {
        /// Path to the input file. The output format is automatically determined based on the file extension.
        input: String,

        #[arg(long, short)]
        /// Path to the output file. If not provided, the output will be written to the same directory as the input file.
        output: Option<String>,

        #[arg(long, short)]
        /// Whether to recursively convert all files in the input directory. Only valid if the input is a directory.
        /// If the input is a file, this option is ignored.
        recursive: bool,
    },

    /// Diff two .bin or .ritobin files and show the differences
    Diff {
        /// Path to the first file to compare
        file1: String,

        /// Path to the second file to compare
        file2: String,

        #[arg(long, short = 'C', default_value = "3")]
        /// Number of context lines to show around changes
        context: usize,

        #[arg(long)]
        /// Disable colored output
        no_color: bool,
    },

    /// Manage application configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Download latest hashtable files from Mimir
    #[command(alias = "dl")]
    DownloadHashes,
}

fn parse_args() -> Result<Args, clap::Error> {
    // Configure colored/styled help output
    let styles = Styles::styled()
        .header(AnsiColor::Yellow.on_default().bold())
        .usage(AnsiColor::Green.on_default().bold())
        .literal(AnsiColor::Cyan.on_default())
        .placeholder(AnsiColor::Blue.on_default());

    let matches = Args::command()
        .styles(styles)
        .color(ColorChoice::Auto)
        .try_get_matches()?;

    Args::from_arg_matches(&matches)
}

pub struct Context {
    pub config: AppConfig,
    pub config_path: Utf8PathBuf,
    pub hash_store: Option<HashStore>,
    pub hash_provider: HashProvider,
}

fn main() -> Result<()> {
    let args = parse_args();

    let (config, config_path) = load_or_create_config()?;

    let hash_store = HashStore::discover()
        .inspect_err(|e| tracing::error!("Failed to discover Mimir hashes - {e}"))
        .ok();

    let hash_provider = HashProvider::new(config.hashtable_dir.as_ref(), hash_store.as_ref());

    let ctx = Context {
        config,
        config_path,
        hash_store,
        hash_provider,
    };

    initialize_tracing(
        args.as_ref().map(|a| a.verbosity).unwrap_or_default(),
        false,
    )?;

    match args {
        Ok(args) => match args.command {
            Commands::Convert {
                input,
                output,
                recursive,
            } => convert::convert(ctx, input, output, recursive),
            Commands::Diff {
                file1,
                file2,
                context,
                no_color,
            } => diff::diff(ctx, file1, file2, context, no_color),
            Commands::Config { action } => match action {
                ConfigAction::Show => config_cmd::show_config(),
                ConfigAction::Set { key, value } => config_cmd::set_config_value(&key, &value),
                ConfigAction::Reset => config_cmd::reset_config(),
            },
            Commands::DownloadHashes => download_hashes::download_hashes(&ctx),
        },
        Err(e) => {
            let mut raw_args = std::env::args_os().skip(1);

            let process = |arg: OsString| {
                let path = Utf8PathBuf::from_os_string(arg)
                    .map_err(|p| miette!("File path {p:?} is not valid UTF-8!"))?;
                if !path.exists() {
                    miette::bail!("Invalid file {path:?}!");
                }
                convert::convert_file(&ctx, &path, None)?;
                Ok(())
            };

            // if first arg failed to convert, then it probably wasn't a drag and drop
            if raw_args.next().and_then(|arg| process(arg).ok()).is_none() {
                e.exit();
            }

            for arg in raw_args {
                process(arg)?;
            }
            Ok(())
        }
    }
}

fn initialize_tracing(verbosity: VerbosityLevel, show_progress: bool) -> Result<()> {
    let indicatif_layer = IndicatifLayer::new();

    let common_format = fmt::format()
        .with_ansi(true)
        .with_level(true)
        .with_source_location(false)
        .with_line_number(false)
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::time());

    // stdout: INFO/DEBUG/TRACE (when verbosity allows)
    let stdout_layer = fmt::layer()
        .with_writer(indicatif_layer.get_stdout_writer())
        .event_format(common_format.clone())
        .with_filter(filter::filter_fn(move |metadata| {
            let level = *metadata.level();
            // Show INFO and above on stdout for Info verbosity and above
            // Show DEBUG and above for Debug verbosity and above
            // Show TRACE for Trace verbosity
            match verbosity {
                VerbosityLevel::Error => {
                    false // Only stderr for this level
                }
                VerbosityLevel::Warning => level == Level::WARN || level == Level::ERROR,
                VerbosityLevel::Info => {
                    level == Level::INFO || level == Level::WARN || level == Level::ERROR
                }
                VerbosityLevel::Debug => {
                    level != Level::TRACE // Everything except TRACE
                }
                VerbosityLevel::Trace => {
                    true // Everything
                }
            }
        }));

    // stderr: WARN/ERROR (for Warning and above) or all high-priority messages
    let stderr_layer = fmt::layer()
        .with_writer(indicatif_layer.get_stderr_writer())
        .event_format(common_format)
        .with_filter(filter::filter_fn(move |metadata| {
            let level = *metadata.level();
            // Show ERROR and WARN on stderr for most verbosity levels
            // For very quiet levels, show only ERROR
            match verbosity {
                VerbosityLevel::Error => level == Level::ERROR,
                VerbosityLevel::Warning => level == Level::WARN || level == Level::ERROR,
                _ => level == Level::WARN || level == Level::ERROR,
            }
        }));

    let registry = tracing_subscriber::registry()
        .with(stdout_layer)
        .with(stderr_layer)
        .with(verbosity.to_level_filter());

    if show_progress {
        registry.with(indicatif_layer).init();
    } else {
        registry.init();
    }
    Ok(())
}

fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default().bold())
        .usage(AnsiColor::Green.on_default().bold())
        .literal(AnsiColor::Cyan.on_default())
        .placeholder(AnsiColor::Magenta.on_default())
}
