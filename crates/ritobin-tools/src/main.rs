use camino::Utf8Path;
use clap::builder::{Styles, styling::AnsiColor};
use clap::{ColorChoice, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use league_toolkit::file::LeagueFileKind;
use miette::Result;
use serde::Deserialize;
use serde::de::IntoDeserializer;
use serde::de::value::Error;
use tracing::Level;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{filter, fmt};
use utils::config::{default_config_path, load_or_create_config};

use crate::commands::convert;

mod commands;
mod utils;

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum VerbosityLevel {
    /// Show errors and above
    Error,
    /// Show warnings and above
    Warning,
    /// Show info messages and above
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
}

fn parse_args() -> Args {
    // Configure colored/styled help output
    let styles = Styles::styled()
        .header(AnsiColor::Yellow.on_default().bold())
        .usage(AnsiColor::Green.on_default().bold())
        .literal(AnsiColor::Cyan.on_default())
        .placeholder(AnsiColor::Blue.on_default());

    let matches = Args::command()
        .styles(styles)
        .color(ColorChoice::Auto)
        .get_matches();

    Args::from_arg_matches(&matches).expect("failed to parse arguments")
}

fn main() -> Result<()> {
    let _ = crate::commands::ensure_config_exists();

    let args = parse_args();

    initialize_tracing(args.verbosity, false)?;

    match args.command {
        Commands::Convert {
            input,
            output,
            recursive,
        } => convert::convert(input, output, recursive),
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

fn parse_filter_type(s: &str) -> Result<LeagueFileKind, String> {
    let deserializer: serde::de::value::StrDeserializer<Error> = s.into_deserializer();
    if let Ok(kind) = LeagueFileKind::deserialize(deserializer) {
        return Ok(kind);
    }

    // Fallback to extension
    match LeagueFileKind::from_extension(s) {
        LeagueFileKind::Unknown => Err(format!("Unknown file kind: {}", s)),
        other => Ok(other),
    }
}

fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default().bold())
        .usage(AnsiColor::Green.on_default().bold())
        .literal(AnsiColor::Cyan.on_default())
        .placeholder(AnsiColor::Magenta.on_default())
}
