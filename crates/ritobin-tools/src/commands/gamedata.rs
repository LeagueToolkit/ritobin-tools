use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Subcommand};
use indexmap::IndexMap;
use ltk_game_data::{EntryName, FieldNames, Reference, Value};
use ltk_hash::BinHash;
use ltk_meta::{BinFile, BinObject};
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;

use crate::{
    cli::LayoutArgs,
    commands::output::{OutputArgs, OutputFormat, columns},
    context::Context,
    document::{
        DEFAULT_TEXT_EXTENSION, Document, Format, ReadOptions, STDIO, converted_path, encode,
        write_bytes,
    },
    game::Game,
    gamedata::{self, Changes, EditedBin, Layer, Outcome, Problem},
    hashes::{BinHashes, GameNames, format_hash},
    utils::{hyperlink_path, plural},
};

#[derive(Subcommand, Debug)]
pub enum GameDataCommand {
    /// Validate a game-data manifest and dry-run it against the game
    Check(CheckArgs),

    /// Apply a game-data manifest to the game's bins and write the modified bins
    Apply(ApplyArgs),

    /// Print a bin entry, or one of its values, as manifest YAML
    Render(RenderArgs),
}

/// Options that locate the game and its index cache.
#[derive(Args, Debug, Clone)]
pub struct GameArgs {
    /// Path to the `Game` directory of an installation, or to its parent directory. Defaults to
    /// `game_dir` from the config
    #[arg(long, value_name = "DIR")]
    pub game_dir: Option<Utf8PathBuf>,

    /// Directory for the game index cache. Defaults to a directory under the user data directory
    #[arg(long, value_name = "DIR")]
    pub index_dir: Option<Utf8PathBuf>,
}

impl GameArgs {
    /// Returns the game directory from `--game-dir`, falling back to the config.
    pub fn dir<'a>(&'a self, ctx: &'a Context) -> Option<&'a Utf8Path> {
        self.game_dir.as_deref().or(ctx.config.game_dir.as_deref())
    }

    /// Opens the game. Fails if no game directory is configured.
    pub fn open(&self, ctx: &Context) -> Result<Game> {
        let dir = self.dir(ctx).ok_or_else(|| {
            miette::miette!(
                "No game directory is set. Pass --game-dir, or run `ritobin-tools config set game_dir <DIR>`"
            )
        })?;
        let game = Game::open(dir, self.index_dir.as_deref(), ctx.wad_paths())?;
        tracing::debug!("Using the game at {}", game.dir());
        Ok(game)
    }
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    /// Path to the manifest file (`game_data.yaml`, `.yml`, `.toml` or `.json`), or to the
    /// directory that contains it
    pub manifest: Utf8PathBuf,

    #[command(flatten)]
    pub game: GameArgs,

    /// Validate the manifest only. Do not read the game
    #[arg(long, conflicts_with = "game_dir")]
    pub no_game: bool,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
pub struct ApplyArgs {
    /// Path to the manifest file (`game_data.yaml`, `.yml`, `.toml` or `.json`), or to the
    /// directory that contains it
    pub manifest: Utf8PathBuf,

    /// Output directory. Each modified bin is written at its game path under this directory
    #[arg(short, long, value_name = "DIR")]
    pub output: Utf8PathBuf,

    #[command(flatten)]
    pub game: GameArgs,

    /// Output format of the modified bins
    #[arg(short, long, value_enum, value_name = "FORMAT", default_value_t = Format::Bin)]
    pub to: Format,

    /// File extension for text output
    #[arg(long = "ext", value_name = "EXT", default_value = DEFAULT_TEXT_EXTENSION)]
    pub text_extension: String,

    /// Write hashes as hex in text output. Do not resolve them with the hashtables
    #[arg(short, long)]
    pub keep_hashed: bool,

    /// Format of the report
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    #[command(flatten)]
    pub layout: LayoutArgs,
}

#[derive(Args, Debug)]
pub struct RenderArgs {
    /// Entry to print, as a path (`Characters/Teemo/Skins/Skin0`) or a hash (`0x1234abcd`).
    /// Append `:<property path>` to print one value
    #[arg(value_name = "ENTRY[:PATH]")]
    pub value: String,

    /// Read the entry from a bin or ritobin text file. The game is not read
    #[arg(short, long, value_name = "FILE", conflicts_with_all = ["game_dir", "index_dir"])]
    pub bin: Option<Utf8PathBuf>,

    #[command(flatten)]
    pub game: GameArgs,

    /// Write hashes as hex. Do not resolve them with the hashtables
    #[arg(short, long)]
    pub keep_hashed: bool,
}

/// Runs a `gamedata` command. Returns `false` if `check` or `apply` reported at least one
/// problem.
pub fn run(ctx: &Context, command: GameDataCommand) -> Result<bool> {
    match command {
        GameDataCommand::Check(args) => check(ctx, &args),
        GameDataCommand::Apply(args) => apply(ctx, &args),
        GameDataCommand::Render(args) => render(ctx, &args).map(|()| true),
    }
}

/// Loads the manifest. If a game directory is set, also applies the manifest in memory and
/// prints the report. Writes no files.
fn check(ctx: &Context, args: &CheckArgs) -> Result<bool> {
    let layer = Layer::load(&args.manifest)?;
    let modules = plural(layer.declarations.modules.len(), "module");

    if args.no_game || args.game.dir(ctx).is_none() {
        if !args.no_game {
            tracing::info!(
                "No game directory is set. Only the manifest was validated. Pass --game-dir to dry-run it against the game."
            );
        }
        report(&Outcome::default(), args.output.format)?;
        tracing::info!("The manifest is valid: {modules}");
        return Ok(true);
    }

    let game = args.game.open(ctx)?;
    let outcome = gamedata::apply(&layer, &game)?;
    report(&outcome, args.output.format)?;
    Ok(summarize(&outcome, &modules, "would modify"))
}

/// Applies the manifest to the game's bins, writes each modified bin under the output directory
/// and prints the report.
fn apply(ctx: &Context, args: &ApplyArgs) -> Result<bool> {
    let layer = Layer::load(&args.manifest)?;
    let modules = plural(layer.declarations.modules.len(), "module");
    let game = args.game.open(ctx)?;
    let outcome = gamedata::apply(&layer, &game)?;

    let layout = args.layout.over(ctx.config.print_config);
    let hashes = match (args.to, args.keep_hashed) {
        (Format::Rito, false) => ctx.hashes(),
        _ => BinHashes::none(),
    };
    for bin in outcome.bins.iter().filter(|bin| bin.changes.any()) {
        // `output_name` uses `/`. Joining component by component produces the platform
        // separator.
        let path = output_name(bin, &game)
            .components()
            .fold(args.output.clone(), |path, part| path.join(part));
        let path = match args.to {
            Format::Bin => path,
            Format::Rito => converted_path(
                &path,
                Format::Rito,
                args.text_extension.trim_start_matches('.'),
            ),
        };
        let data = match args.to {
            Format::Bin => bin.bytes.clone(),
            Format::Rito => {
                let document =
                    Document::parse(&bin.target, bin.bytes.clone(), ReadOptions::default())?;
                encode(&document.file, Format::Rito, layout, &hashes)
                    .wrap_err_with(|| format!("Failed to print {}", bin.target))?
            }
        };
        write_bytes(&path, &data)?;
        tracing::info!("Wrote {}", hyperlink_path(&path));
    }

    report(&outcome, args.format)?;
    Ok(summarize(&outcome, &modules, "modified"))
}

/// Returns the output path of `bin`, relative to the output directory, with `/` separators.
///
/// Uses the manifest target if it is a safe relative path, then the chunk path from the
/// hashtables. Falls back to `<chunk hash>.bin`.
fn output_name(bin: &EditedBin, game: &Game) -> Utf8PathBuf {
    let hash_name = format!("{:016x}", bin.chunk.0);
    let is_hash = |name: &str| name.eq_ignore_ascii_case(&hash_name);
    // The check is on the string, not on a platform path, so the output tree is identical on
    // every platform. A drive letter or a backslash is rejected on Linux as well.
    let relative = |name: &str| {
        !is_hash(name)
            && name.split('/').all(|segment| {
                !matches!(segment, "" | "." | "..") && !segment.contains([':', '\\'])
            })
    };

    let named = game.chunk_name(bin.chunk);
    match [bin.target.as_str(), named.as_str()]
        .into_iter()
        .find(|name| relative(name))
    {
        Some(name) => name.into(),
        None => format!("{hash_name}.bin").into(),
    }
}

#[derive(Serialize)]
struct BinRow<'a> {
    target: &'a str,
    chunk: String,
    #[serde(flatten)]
    changes: Changes,
}

#[derive(Serialize)]
struct Report<'a> {
    bins: Vec<BinRow<'a>>,
    problems: &'a [Problem],
}

/// Prints the report to standard output: one row per targeted bin with its change counts, then
/// one row per problem.
fn report(outcome: &Outcome, format: OutputFormat) -> Result<()> {
    let bins: Vec<BinRow> = outcome
        .bins
        .iter()
        .map(|bin| BinRow {
            target: &bin.target,
            chunk: format!("{:016x}", bin.chunk.0),
            changes: bin.changes,
        })
        .collect();

    let out = match format {
        OutputFormat::Json => {
            let mut out = serde_json::to_string_pretty(&Report {
                bins,
                problems: &outcome.problems,
            })
            .into_diagnostic()?;
            out.push('\n');
            out
        }
        OutputFormat::Table => {
            let mut out = String::new();
            if !bins.is_empty() {
                let cells: Vec<[String; 5]> = bins
                    .iter()
                    .map(|bin| {
                        [
                            bin.target.to_owned(),
                            bin.changes.properties.to_string(),
                            bin.changes.objects.to_string(),
                            bin.changes.records.to_string(),
                            format!(
                                "+{} -{}",
                                bin.changes.links_added, bin.changes.links_removed
                            ),
                        ]
                    })
                    .collect();
                out.push_str(&columns(
                    ["BIN", "PROPERTIES", "OBJECTS", "RECORDS", "LINKS"],
                    &cells,
                ));
            }
            if !outcome.problems.is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                let cells: Vec<[String; 3]> = outcome
                    .problems
                    .iter()
                    .map(|problem| {
                        [
                            match &problem.module_name {
                                Some(name) => format!("{} ({name})", problem.module),
                                None => problem.module.to_string(),
                            },
                            problem.target.clone(),
                            problem.message.clone(),
                        ]
                    })
                    .collect();
                out.push_str(&columns(["MODULE", "TARGET", "PROBLEM"], &cells));
            }
            out
        }
    };
    write_bytes(STDIO.into(), out.as_bytes())
}

/// Logs a summary of `outcome`. Returns `false` if it has at least one problem.
///
/// `modules` is the module count as text. `modified` is the verb for the summary line:
/// "modified" for `apply`, "would modify" for `check`.
fn summarize(outcome: &Outcome, modules: &str, modified: &str) -> bool {
    let edited = outcome.bins.iter().filter(|bin| bin.changes.any()).count();
    for bin in outcome.bins.iter().filter(|bin| !bin.changes.any()) {
        tracing::warn!("No edit was applied to {}. It is unchanged.", bin.target);
    }
    if outcome.untypable {
        tracing::warn!(
            "Edits skipped as `untypable` add a property that is missing from the bin. Adding a property requires a class schema, which is not supported yet."
        );
    }

    match outcome.problems.len() {
        0 => {
            tracing::info!("{modules} {modified} {}", plural(edited, "bin"));
            true
        }
        problems => {
            tracing::warn!(
                "{modules} {modified} {}. {} reported.",
                plural(edited, "bin"),
                plural(problems, "problem")
            );
            false
        }
    }
}

/// Prints an entry, or the value at a property path of the entry, as manifest YAML. Reads the
/// entry from `--bin` if set, otherwise from the game.
fn render(ctx: &Context, args: &RenderArgs) -> Result<()> {
    let (entry, path) = match args.value.contains(':') {
        true => {
            let reference = Reference::parse(&args.value).map_err(|_| {
                miette::miette!(
                    "Invalid value `{}`. Expected `<entry>:<property path>`, for example `Characters/Teemo/Skins/Skin0:armorMaterial`",
                    args.value
                )
            })?;
            (reference.entry, Some(reference.path))
        }
        false => {
            let entry = EntryName::try_from(args.value.as_str())
                .map_err(|error| miette::miette!("Invalid entry `{}`: {error}", args.value))?;
            (entry, None)
        }
    };

    let object = match &args.bin {
        Some(file) => object_of(file, entry.object_hash())?
            .ok_or_else(|| miette::miette!("Entry {entry} was not found in {file}"))?,
        None => {
            let game = args.game.open(ctx)?;
            game.object(entry.object_hash())?
                .ok_or_else(|| miette::miette!("Entry {entry} is not declared by any game bin"))?
        }
    };

    let names = match args.keep_hashed {
        true => GameNames::default(),
        false => ctx.game_names(),
    };
    let value = match &path {
        Some(path) => {
            let value = object.resolve(path).map_err(|error| {
                miette::miette!("Failed to resolve `{}` in {entry}: {error}", path.as_str())
            })?;
            Value::render(value, &names)
        }
        None => entry_body(&object, &names),
    }
    .map_err(|error| miette::miette!("Failed to render {}: {error}", args.value))?;

    let mut text = value
        .to_yaml()
        .map_err(|error| miette::miette!("{error}"))?;
    text.push('\n');
    write_bytes(STDIO.into(), text.as_bytes())
}

/// Reads the bin or ritobin text file at `path` and returns its object with path hash `hash`.
/// Returns `None` if the file has no such object.
fn object_of(path: &Utf8Path, hash: BinHash) -> Result<Option<BinObject>> {
    let document = Document::read(path, ReadOptions::default())?;
    let objects = match document.file {
        BinFile::Prop(bin) => bin.objects,
        BinFile::Override(patch) => patch.objects,
    };
    Ok(objects.get(&hash).cloned())
}

/// Renders `object` as a manifest entry body: a mapping from each property name to its rendered
/// value. A property with no known name uses its hash as the key.
fn entry_body(object: &BinObject, names: &GameNames) -> Result<Value, ltk_game_data::Error> {
    let mut body = IndexMap::new();
    for (field, value) in &object.properties {
        let key = match names.field(*field, Some(object.class_hash)) {
            Some(name) => name.into_owned(),
            None => format_hash(*field),
        };
        body.insert(key, Value::render(value, names)?);
    }
    Ok(Value::Mapping(body))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ltk_game_index::chunk_hash;
    use ltk_meta::{Bin, property::values};

    use super::*;
    use crate::{document::to_bin, game::testing::Installation};

    fn decode(bytes: &[u8]) -> BinFile {
        BinFile::from_reader(&mut Cursor::new(bytes)).unwrap()
    }

    fn bin() -> Vec<u8> {
        let bin: BinFile = Bin::builder()
            .object(
                BinObject::builder(0x1111_0001u32, 0xaaaa_0001u32)
                    .property(0x10u32, values::I32::new(42))
                    .property(0x11u32, values::String::new("teemo".to_owned()))
                    .build(),
            )
            .build()
            .into();
        to_bin(&bin).unwrap()
    }

    fn edited(target: &str, chunk: &str) -> EditedBin {
        EditedBin {
            chunk: chunk_hash(chunk),
            target: target.to_owned(),
            bytes: Vec::new(),
            changes: Changes::default(),
        }
    }

    #[test]
    fn output_name_uses_safe_path_or_chunk_hash() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/skin0.bin", &bin())]);
        let game = installation.open();

        assert_eq!(
            output_name(&edited("data/Skin0.bin", "data/skin0.bin"), &game),
            "data/Skin0.bin"
        );

        // The target is a chunk hash and no hashtable is loaded, so the path is unknown.
        let hash = format!("{:016x}", chunk_hash("data/skin0.bin").0);
        assert_eq!(
            output_name(&edited(&hash, "data/skin0.bin"), &game),
            format!("{hash}.bin")
        );

        // Targets that are absolute, have a drive letter, or contain `..` or a backslash.
        for target in [
            "../outside.bin",
            "/root.bin",
            "C:/drive.bin",
            "data\\..\\..\\outside.bin",
        ] {
            let name = output_name(&edited(target, target), &game);
            assert_eq!(
                name,
                format!("{:016x}.bin", chunk_hash(target).0),
                "{target}"
            );
        }
    }

    #[test]
    fn entry_body_maps_property_keys_to_values() {
        let BinFile::Prop(bin) = decode(&bin()) else {
            panic!("not a PROP bin");
        };
        let object = &bin.objects[&BinHash(0x1111_0001)];
        let body = entry_body(object, &GameNames::default()).unwrap();
        assert_eq!(
            body.to_yaml().unwrap(),
            "\"0x00000010\": 42\n\"0x00000011\": teemo"
        );
    }
}
