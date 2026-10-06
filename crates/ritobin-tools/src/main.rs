use std::process::ExitCode;

use miette::Result;

use crate::{
    cli::{Cli, Commands},
    commands::hashes::HashesCommand,
    context::Context,
};

mod cli;
mod commands;
mod config;
mod context;
mod diff;
mod document;
mod hashes;
mod logging;
mod utils;

/// What a command that fails exits with.
const EXIT_FAILURE: u8 = 1;

/// What `diff --exit-code` exits with when the bins differ.
const EXIT_DIFFERENT: u8 = 1;

/// What `diff --exit-code` exits with when it fails, so a failure is not read as a difference.
const EXIT_TROUBLE: u8 = 2;

fn main() -> ExitCode {
    let cli = cli::parse();
    logging::init(cli.verbosity);

    let failure = match &cli.command {
        Commands::Diff(args) if args.exit_code => EXIT_TROUBLE,
        _ => EXIT_FAILURE,
    };
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::from(failure)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let ctx = Context::new(&cli)?;

    match cli.command {
        Commands::Convert(args) => commands::convert::run(&ctx, args)?,
        Commands::Diff(args) => {
            let exit_code = args.exit_code;
            let differs = commands::diff::run(&ctx, args)?;
            if exit_code && differs {
                return Ok(ExitCode::from(EXIT_DIFFERENT));
            }
        }
        Commands::Hashes { command } => commands::hashes::run(&ctx, command)?,
        Commands::DownloadHashes(args) => commands::hashes::sync(&ctx, &args)?,
        Commands::HashtableDir => commands::hashes::run(&ctx, HashesCommand::Dir)?,
        Commands::Config { command } => commands::config::run(&ctx, command)?,
    }
    Ok(ExitCode::SUCCESS)
}
