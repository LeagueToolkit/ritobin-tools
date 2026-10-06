//! The command line: global options, the subcommands and how arguments are parsed.

use std::ffi::{OsStr, OsString};

use camino::Utf8PathBuf;
use clap::{
    Args, ColorChoice, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum,
    builder::{Styles, styling::AnsiColor},
};

#[cfg(windows)]
use crate::commands::shell::ShellCommand;
use crate::{
    commands::{
        config::ConfigCommand,
        convert::ConvertArgs,
        diff::DiffArgs,
        gamedata::GameDataCommand,
        hashes::{HashesCommand, SyncArgs},
    },
    document::{MAX_LINE_WIDTH, MIN_LINE_WIDTH, TextLayout},
};

const GLOBAL_OPTIONS: &str = "Global options";
const LAYOUT_OPTIONS: &str = "Text layout";

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// Set the verbosity level
    #[arg(
        short = 'L',
        long,
        value_enum,
        default_value_t = VerbosityLevel::Info,
        global = true,
        help_heading = GLOBAL_OPTIONS
    )]
    pub verbosity: VerbosityLevel,

    /// Path to a config file (TOML). Defaults to `ritobin-tools.toml` next to the executable
    #[arg(long, value_name = "FILE", global = true, help_heading = GLOBAL_OPTIONS)]
    pub config: Option<Utf8PathBuf>,

    /// Hashtable cache directory. Overrides the config value, `MIMIR_DIR` and the shared default
    #[arg(long, value_name = "DIR", global = true, help_heading = GLOBAL_OPTIONS)]
    pub hashtable_dir: Option<Utf8PathBuf>,

    /// Directory of extra CDragon text hashtables (`hashes.binentries.txt`, `hashes.binfields.txt`,
    /// `hashes.binhashes.txt`, `hashes.bintypes.txt`). Its names win over the cache
    #[arg(
        short = 'H',
        long,
        value_name = "DIR",
        global = true,
        help_heading = GLOBAL_OPTIONS
    )]
    pub hashtable: Option<Utf8PathBuf>,

    /// Wait for Enter before exiting. The Explorer menu and files dropped on the executable set
    /// it, because the console window they open closes when the run ends
    #[arg(
        long,
        value_enum,
        value_name = "WHEN",
        default_value_t = PauseMode::Never,
        global = true,
        hide = true
    )]
    pub pause: PauseMode,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Convert between .bin (binary) and .rito (text)
    Convert(ConvertArgs),

    /// Show the difference between two bins, and save it as a PTCH patch
    Diff(DiffArgs),

    /// Manage and query the hashtables
    Hashes {
        #[command(subcommand)]
        command: HashesCommand,
    },

    /// Download the latest hashtables into the shared cache. Same as `hashes sync`
    #[command(visible_alias = "dl")]
    DownloadHashes(SyncArgs),

    /// Print the hashtable cache directory. Same as `hashes dir`
    #[command(visible_alias = "hd")]
    HashtableDir,

    /// Manage the configuration file
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Check and apply game-data declarations: a manifest of edits to the game's bins
    #[command(name = "gamedata", visible_alias = "gd")]
    GameData {
        #[command(subcommand)]
        command: GameDataCommand,
    },

    /// Manage the Windows Explorer right-click menu
    #[cfg(windows)]
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },
}

/// When a run waits for Enter before it exits.
#[derive(Default, Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum PauseMode {
    #[default]
    Never,
    /// Only after a run that failed
    OnError,
    Always,
}

impl PauseMode {
    /// Whether a run that `failed` or did not waits.
    pub fn applies(self, failed: bool) -> bool {
        match self {
            PauseMode::Never => false,
            PauseMode::OnError => failed,
            PauseMode::Always => true,
        }
    }
}

#[derive(Default, Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum VerbosityLevel {
    /// Show errors only
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

/// The flags that lay out ritobin text. Each overrides its `[print_config]` value for one run.
#[derive(Args, Debug, Clone, Copy, Default)]
#[command(next_help_heading = LAYOUT_OPTIONS)]
pub struct LayoutArgs {
    /// Spaces per indent level
    #[arg(long, value_name = "N")]
    pub indent_size: Option<usize>,

    /// Line width past which a block is broken over several lines, from 40 to 200
    #[arg(long, value_name = "N", value_parser = line_width)]
    pub line_width: Option<usize>,

    /// Print a struct that fits on one line on one line. `--inline-structs=false` turns it off
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true"
    )]
    pub inline_structs: Option<bool>,
}

fn line_width(text: &str) -> Result<usize, String> {
    match text.parse() {
        Ok(width) if (MIN_LINE_WIDTH..=MAX_LINE_WIDTH).contains(&width) => Ok(width),
        _ => Err(format!(
            "expected a number from {MIN_LINE_WIDTH} to {MAX_LINE_WIDTH}"
        )),
    }
}

impl LayoutArgs {
    /// `layout` with every flag that was given laid over it.
    pub fn over(&self, layout: TextLayout) -> TextLayout {
        TextLayout {
            indent_size: self.indent_size.unwrap_or(layout.indent_size),
            line_width: self.line_width.unwrap_or(layout.line_width),
            inline_structs: self.inline_structs.unwrap_or(layout.inline_structs),
            ..layout
        }
    }
}

fn styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default().bold())
        .usage(AnsiColor::Green.on_default().bold())
        .literal(AnsiColor::Cyan.on_default())
        .placeholder(AnsiColor::Blue.on_default())
}

pub fn try_parse(args: impl IntoIterator<Item = OsString>) -> Result<Cli, clap::Error> {
    let matches = Cli::command()
        .styles(styles())
        .color(ColorChoice::Auto)
        .try_get_matches_from(args)?;
    Cli::from_arg_matches(&matches)
}

/// Parses the process arguments, exiting with usage on a mistake.
///
/// Arguments that are nothing but existing paths are files dropped on the executable, and are
/// converted.
pub fn parse() -> Cli {
    let args: Vec<OsString> = std::env::args_os().collect();
    match try_parse(args.clone()) {
        Ok(cli) => cli,
        Err(error) => match dropped_files(&args) {
            Some(with_convert) => try_parse(with_convert).unwrap_or_else(|error| error.exit()),
            None => error.exit(),
        },
    }
}

/// `args` as a `convert` run of them, if every argument is an existing path.
///
/// The run waits for Enter when it fails: a drop opens a console window that closes with the run.
///
/// A first argument that is the name of a subcommand is never a dropped file, even when a file or
/// directory of that name exists.
fn dropped_files(args: &[OsString]) -> Option<Vec<OsString>> {
    let (program, paths) = args.split_first()?;
    let all_paths = !is_subcommand(paths.first()?)
        && paths.iter().all(|path| std::path::Path::new(path).exists());
    all_paths.then(|| {
        [program.clone()]
            .into_iter()
            .chain(["--pause", "on-error", "convert"].map(OsString::from))
            .chain(paths.iter().cloned())
            .collect()
    })
}

/// Whether `name` is a subcommand or one of its aliases.
fn is_subcommand(name: &OsStr) -> bool {
    Cli::command().get_subcommands().any(|subcommand| {
        subcommand.get_name() == name || subcommand.get_all_aliases().any(|alias| alias == name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn layout_flags_override_only_what_they_name() {
        let args = LayoutArgs {
            indent_size: Some(2),
            inline_structs: Some(true),
            ..LayoutArgs::default()
        };
        assert_eq!(
            args.over(TextLayout::default()),
            TextLayout {
                indent_size: 2,
                inline_structs: true,
                ..TextLayout::default()
            }
        );
    }

    #[test]
    fn existing_paths_alone_are_dropped_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("skin0.bin");
        std::fs::write(&file, b"").unwrap();

        let args = vec![OsString::from("ritobin-tools"), file.clone().into()];
        let dropped = dropped_files(&args).unwrap();
        assert_eq!(
            dropped,
            [
                OsString::from("ritobin-tools"),
                OsString::from("--pause"),
                OsString::from("on-error"),
                OsString::from("convert"),
                file.clone().into(),
            ]
        );

        let cli = try_parse(dropped).unwrap();
        assert_eq!(cli.pause, PauseMode::OnError);
        let Commands::Convert(args) = cli.command else {
            panic!("not the convert command");
        };
        assert_eq!(args.inputs, [file]);
    }

    #[test]
    fn a_run_waits_only_when_its_pause_mode_says_so() {
        assert!(!PauseMode::Never.applies(true));
        assert!(!PauseMode::OnError.applies(false));
        assert!(PauseMode::OnError.applies(true));
        assert!(PauseMode::Always.applies(false));
    }

    #[test]
    fn a_subcommand_name_is_not_a_dropped_file() {
        for name in [
            "convert",
            "diff",
            "hashes",
            "download-hashes",
            "dl",
            "hd",
            "config",
            "gamedata",
            "gd",
        ] {
            assert!(is_subcommand(OsStr::new(name)), "{name}");
        }
        assert!(!is_subcommand(OsStr::new("skin0.bin")));

        // `.` always exists, as a directory named after a subcommand could.
        let args = ["ritobin-tools", "hashes", "."].map(OsString::from);
        assert_eq!(dropped_files(&args), None);
    }

    #[test]
    fn an_inline_flag_does_not_take_the_next_argument_as_its_value() {
        let cli = try_parse(
            ["ritobin-tools", "convert", "--inline-structs", "skin0.bin"].map(OsString::from),
        )
        .unwrap();
        let Commands::Convert(args) = cli.command else {
            panic!("not the convert command");
        };
        assert_eq!(args.layout.inline_structs, Some(true));
        assert_eq!(args.inputs, ["skin0.bin"]);

        let cli = try_parse(
            [
                "ritobin-tools",
                "convert",
                "--inline-structs=false",
                "skin0.bin",
            ]
            .map(OsString::from),
        )
        .unwrap();
        let Commands::Convert(args) = cli.command else {
            panic!("not the convert command");
        };
        assert_eq!(args.layout.inline_structs, Some(false));
    }

    #[test]
    fn a_line_width_outside_the_printer_bounds_is_refused() {
        let parse = |width: &str| {
            try_parse(
                [
                    "ritobin-tools",
                    "convert",
                    "--line-width",
                    width,
                    "skin0.bin",
                ]
                .map(OsString::from),
            )
        };
        assert!(parse("80").is_ok());
        assert!(parse("10").is_err());
        assert!(parse("100000").is_err());
    }

    #[test]
    fn a_missing_path_is_not_a_dropped_file() {
        let args = vec![
            OsString::from("ritobin-tools"),
            OsString::from("no-such-subcommand"),
        ];
        assert_eq!(dropped_files(&args), None);
        assert_eq!(dropped_files(&[OsString::from("ritobin-tools")]), None);
    }
}
