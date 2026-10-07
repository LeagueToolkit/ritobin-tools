//! Defines the command line interface: the global options, the subcommands and the argument
//! parsing.

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
        format::FormatArgs,
        gamedata::GameDataCommand,
        hashes::{HashesCommand, SyncArgs},
        patch::PatchArgs,
        search::SearchArgs,
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

    /// Hashtable cache directory. Overrides the config value, `MIMIR_DIR` and the default
    /// shared directory
    #[arg(long, value_name = "DIR", global = true, help_heading = GLOBAL_OPTIONS)]
    pub hashtable_dir: Option<Utf8PathBuf>,

    /// Directory of additional CDragon text hashtables (`hashes.binentries.txt`,
    /// `hashes.binfields.txt`, `hashes.binhashes.txt`, `hashes.bintypes.txt`). A name from this
    /// directory takes precedence over the cache
    #[arg(
        short = 'H',
        long,
        value_name = "DIR",
        global = true,
        help_heading = GLOBAL_OPTIONS
    )]
    pub hashtable: Option<Utf8PathBuf>,

    /// Wait for Enter before exiting. The Explorer menu entries and the drag-and-drop launch
    /// set this option, because Explorer runs the tool in a console window that closes on exit
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

    /// Format ritobin text files. Comments are kept
    #[command(visible_alias = "fmt")]
    Format(FormatArgs),

    /// Show the difference between two bins, and optionally save it as a PTCH patch
    Diff(DiffArgs),

    /// Apply PTCH patches to a bin
    Patch(PatchArgs),

    /// Search bin files or the bins of the game for names, values and references
    #[command(visible_alias = "grep")]
    Search(SearchArgs),

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

    /// Validate, apply and render game-data declarations
    #[command(name = "gamedata", visible_alias = "gd")]
    GameData {
        #[command(subcommand)]
        command: GameDataCommand,
    },

    /// Manage the Windows Explorer context menu
    #[cfg(windows)]
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },
}

/// Selects when the tool waits for Enter before it exits.
#[derive(Default, Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum PauseMode {
    /// Do not wait
    #[default]
    Never,
    /// Wait only if the command failed
    OnError,
    /// Always wait
    Always,
}

impl PauseMode {
    /// Returns `true` if the tool waits before it exits. `failed` is `true` if the command
    /// failed.
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

/// The text layout flags. Each flag overrides the matching `[print_config]` value for one run.
#[derive(Args, Debug, Clone, Copy, Default)]
#[command(next_help_heading = LAYOUT_OPTIONS)]
pub struct LayoutArgs {
    /// Spaces per indent level
    #[arg(long, value_name = "N")]
    pub indent_size: Option<usize>,

    /// Maximum line width, from 40 to 200. A block that exceeds it is printed on several lines
    #[arg(long, value_name = "N", value_parser = line_width)]
    pub line_width: Option<usize>,

    /// Print a struct on one line if it fits within the line width. Pass
    /// `--inline-structs=false` to disable
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true"
    )]
    pub inline_structs: Option<bool>,
}

/// Parses a line width. Fails if the value is not a number from [`MIN_LINE_WIDTH`] to
/// [`MAX_LINE_WIDTH`].
fn line_width(text: &str) -> Result<usize, String> {
    match text.parse() {
        Ok(width) if (MIN_LINE_WIDTH..=MAX_LINE_WIDTH).contains(&width) => Ok(width),
        _ => Err(format!(
            "expected a number from {MIN_LINE_WIDTH} to {MAX_LINE_WIDTH}"
        )),
    }
}

impl LayoutArgs {
    /// Returns `layout` with each field replaced by the value of its flag, if the flag is set.
    pub fn over(&self, layout: TextLayout) -> TextLayout {
        TextLayout {
            indent_size: self.indent_size.unwrap_or(layout.indent_size),
            line_width: self.line_width.unwrap_or(layout.line_width),
            inline_structs: self.inline_structs.unwrap_or(layout.inline_structs),
            ..layout
        }
    }
}

/// Returns the colors of the help output.
fn styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default().bold())
        .usage(AnsiColor::Green.on_default().bold())
        .literal(AnsiColor::Cyan.on_default())
        .placeholder(AnsiColor::Blue.on_default())
}

/// Parses `args` as a command line. The first item is the program name.
pub fn try_parse(args: impl IntoIterator<Item = OsString>) -> Result<Cli, clap::Error> {
    let matches = Cli::command()
        .styles(styles())
        .color(ColorChoice::Auto)
        .try_get_matches_from(args)?;
    Cli::from_arg_matches(&matches)
}

/// Parses the process arguments. Prints the usage error and exits if they are invalid.
///
/// If parsing fails and every argument is an existing path, the arguments are treated as files
/// dropped on the executable and are parsed as a `convert` command.
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

/// Returns `args` rewritten as a `convert` command with `--pause on-error`, if every argument is
/// an existing path. Otherwise returns `None`.
///
/// `--pause on-error` is added because Explorer runs a dropped file in a console window that
/// closes on exit.
///
/// Returns `None` if the first argument is the name of a subcommand, even if a file or directory
/// with that name exists.
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

/// Returns `true` if `name` is the name or an alias of a subcommand.
fn is_subcommand(name: &OsStr) -> bool {
    Cli::command().get_subcommands().any(|subcommand| {
        subcommand.get_name() == name || subcommand.get_all_aliases().any(|alias| alias == name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_definition_passes_debug_assert() {
        Cli::command().debug_assert();
    }

    #[test]
    fn layout_flags_override_only_set_fields() {
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
    fn dropped_files_rewrites_existing_paths_to_convert() {
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
    fn pause_mode_applies_by_mode_and_failure() {
        assert!(!PauseMode::Never.applies(true));
        assert!(!PauseMode::OnError.applies(false));
        assert!(PauseMode::OnError.applies(true));
        assert!(PauseMode::Always.applies(false));
    }

    #[test]
    fn dropped_files_ignores_subcommand_names() {
        for name in [
            "convert",
            "format",
            "fmt",
            "diff",
            "patch",
            "search",
            "grep",
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

        // `.` is an existing path, like a directory that has the name of a subcommand.
        let args = ["ritobin-tools", "hashes", "."].map(OsString::from);
        assert_eq!(dropped_files(&args), None);
    }

    #[test]
    fn inline_structs_flag_requires_equals_for_a_value() {
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
    fn line_width_outside_bounds_fails_to_parse() {
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
    fn dropped_files_ignores_missing_paths() {
        let args = vec![
            OsString::from("ritobin-tools"),
            OsString::from("no-such-subcommand"),
        ];
        assert_eq!(dropped_files(&args), None);
        assert_eq!(dropped_files(&[OsString::from("ritobin-tools")]), None);
    }
}
