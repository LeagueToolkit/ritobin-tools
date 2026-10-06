//! Configures log output. All log messages are written to standard error. Standard output
//! contains only the output a command was asked to print.

use std::io::IsTerminal as _;

use tracing::Level;
use tracing_subscriber::{filter::LevelFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::cli::VerbosityLevel;

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

/// Installs the global log subscriber. Messages below `verbosity` are discarded. Colors are
/// used only if standard error is a terminal.
pub fn init(verbosity: VerbosityLevel) {
    let format = fmt::format()
        .with_level(true)
        .with_source_location(false)
        .with_target(false)
        .with_timer(fmt::time::time());

    let layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .event_format(format);

    tracing_subscriber::registry()
        .with(layer)
        .with(LevelFilter::from_level(verbosity.into()))
        .init();
}
