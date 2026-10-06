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
mod game;
mod gamedata;
mod hashes;
mod logging;
mod utils;

/// The exit code of a failed command.
const EXIT_FAILURE: u8 = 1;

/// The exit code of `diff --exit-code` if the bins differ.
const EXIT_DIFFERENT: u8 = 1;

/// The exit code of `diff --exit-code` if the command fails. It differs from [`EXIT_DIFFERENT`]
/// so that a script can distinguish a failure from a difference.
const EXIT_TROUBLE: u8 = 2;

fn main() -> ExitCode {
    let cli = cli::parse();
    logging::init(cli.verbosity);

    let pause = cli.pause;
    let failure = match &cli.command {
        Commands::Diff(args) if args.exit_code => EXIT_TROUBLE,
        _ => EXIT_FAILURE,
    };
    let outcome = run(cli);
    let failed = outcome.is_err();
    let code = match outcome {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::from(failure)
        }
    };
    if pause.applies(failed) {
        utils::wait_for_enter();
    }
    code
}

/// Runs the selected command and returns its exit code.
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
        Commands::GameData { command } => {
            if !commands::gamedata::run(&ctx, command)? {
                return Ok(ExitCode::from(EXIT_FAILURE));
            }
        }
        #[cfg(windows)]
        Commands::Shell { command } => commands::shell::run(command)?,
    }
    Ok(ExitCode::SUCCESS)
}
