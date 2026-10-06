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
    /// Check a game-data manifest, and what applying it to the game would do
    Check(CheckArgs),

    /// Apply a game-data manifest to the game's bins and write the edited bins
    Apply(ApplyArgs),

    /// Print an entry of a bin, or one value of it, the way a manifest writes it
    Render(RenderArgs),
}

/// Where the game is.
#[derive(Args, Debug, Clone)]
pub struct GameArgs {
    /// The game: the `Game` directory of an installation, or the directory that holds it.
    /// Defaults to `game_dir` in the config
    #[arg(long, value_name = "DIR")]
    pub game_dir: Option<Utf8PathBuf>,

    /// Where the indexes of the game are cached, in place of the user's data directory
    #[arg(long, value_name = "DIR")]
    pub index_dir: Option<Utf8PathBuf>,
}

impl GameArgs {
    /// Returns the game directory, from the flag or the config.
    fn dir<'a>(&'a self, ctx: &'a Context) -> Option<&'a Utf8Path> {
        self.game_dir.as_deref().or(ctx.config.game_dir.as_deref())
    }

    fn open(&self, ctx: &Context) -> Result<Game> {
        let dir = self.dir(ctx).ok_or_else(|| {
            miette::miette!(
                "No game directory is known. Pass --game-dir, or run `ritobin-tools config set game_dir <DIR>`"
            )
        })?;
        let game = Game::open(dir, self.index_dir.as_deref(), ctx.wad_paths())?;
        tracing::debug!("Reading the game at {}", game.dir());
        Ok(game)
    }
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    /// The manifest (`game_data.yaml`, `.yml`, `.toml` or `.json`), or the directory it is in
    pub manifest: Utf8PathBuf,

    #[command(flatten)]
    pub game: GameArgs,

    /// Check the manifest alone, and do not read the game
    #[arg(long, conflicts_with = "game_dir")]
    pub no_game: bool,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
pub struct ApplyArgs {
    /// The manifest (`game_data.yaml`, `.yml`, `.toml` or `.json`), or the directory it is in
    pub manifest: Utf8PathBuf,

    /// The directory the edited bins are written to, each at its path in the game
    #[arg(short, long, value_name = "DIR")]
    pub output: Utf8PathBuf,

    #[command(flatten)]
    pub game: GameArgs,

    /// The format the edited bins are written in
    #[arg(short, long, value_enum, value_name = "FORMAT", default_value_t = Format::Bin)]
    pub to: Format,

    /// The extension given to text output
    #[arg(long = "ext", value_name = "EXT", default_value = DEFAULT_TEXT_EXTENSION)]
    pub text_extension: String,

    /// Leave hashes as hex in text output instead of naming them from the hashtables
    #[arg(short, long)]
    pub keep_hashed: bool,

    /// How to print the report
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    #[command(flatten)]
    pub layout: LayoutArgs,
}

#[derive(Args, Debug)]
pub struct RenderArgs {
    /// What to print: an entry (`Characters/Teemo/Skins/Skin0`, or its hash as `0x1234abcd`), or
    /// one value of it as `<entry>:<property path>`
    #[arg(value_name = "ENTRY[:PATH]")]
    pub value: String,

    /// Read the entry from this bin or ritobin text file instead of from the game
    #[arg(short, long, value_name = "FILE", conflicts_with_all = ["game_dir", "index_dir"])]
    pub bin: Option<Utf8PathBuf>,

    #[command(flatten)]
    pub game: GameArgs,

    /// Leave hashes as hex instead of naming them from the hashtables
    #[arg(short, long)]
    pub keep_hashed: bool,
}

/// Runs a `gamedata` command. Returns `false` when a manifest was applied and part of it did not
/// apply.
pub fn run(ctx: &Context, command: GameDataCommand) -> Result<bool> {
    match command {
        GameDataCommand::Check(args) => check(ctx, &args),
        GameDataCommand::Apply(args) => apply(ctx, &args),
        GameDataCommand::Render(args) => render(ctx, &args).map(|()| true),
    }
}

fn check(ctx: &Context, args: &CheckArgs) -> Result<bool> {
    let layer = Layer::load(&args.manifest)?;
    let modules = plural(layer.declarations.modules.len(), "module");

    if args.no_game || args.game.dir(ctx).is_none() {
        if !args.no_game {
            tracing::info!(
                "No game directory is known, so only the manifest was checked. Pass --game-dir to check it against the game."
            );
        }
        report(&Outcome::default(), args.output.format)?;
        tracing::info!("The manifest is valid: {modules}");
        return Ok(true);
    }

    let game = args.game.open(ctx)?;
    let outcome = gamedata::apply(&layer, &game)?;
    report(&outcome, args.output.format)?;
    Ok(summarize(&outcome, &modules, "would change"))
}

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
        // Joined part by part, so the path has the separators of the platform.
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
    Ok(summarize(&outcome, &modules, "changed"))
}

/// Chooses where an edited bin goes under the output directory: its path in the game, or its
/// chunk hash when the path is not known or is not one to write to.
fn output_name(bin: &EditedBin, game: &Game) -> Utf8PathBuf {
    let hash_name = format!("{:016x}", bin.chunk.0);
    let is_hash = |name: &str| name.eq_ignore_ascii_case(&hash_name);
    // Read as text, not as a path of this platform, so the tree written is the same on each: a
    // drive letter or a backslash is refused on Linux too.
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

/// Prints the bins a manifest edits and what did not apply.
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

/// Logs what an application came to. Returns `false` when part of the manifest did not apply.
fn summarize(outcome: &Outcome, modules: &str, changed: &str) -> bool {
    let edited = outcome.bins.iter().filter(|bin| bin.changes.any()).count();
    for bin in outcome.bins.iter().filter(|bin| !bin.changes.any()) {
        tracing::warn!(
            "No edit applied to {}. It is as the game has it.",
            bin.target
        );
    }
    if outcome.untypable {
        tracing::warn!(
            "An edit skipped as `untypable` is of a property the bin does not have. This version reads no class schema, so it cannot tell the type of a property to add."
        );
    }

    match outcome.problems.len() {
        0 => {
            tracing::info!("{modules} {changed} {}", plural(edited, "bin"));
            true
        }
        problems => {
            tracing::warn!(
                "{modules} {changed} {}, and {} did not apply",
                plural(edited, "bin"),
                plural(problems, "edit")
            );
            false
        }
    }
}

fn render(ctx: &Context, args: &RenderArgs) -> Result<()> {
    let (entry, path) = match args.value.contains(':') {
        true => {
            let reference = Reference::parse(&args.value).map_err(|_| {
                miette::miette!(
                    "`{}` is not `<entry>:<property path>`, such as `Characters/Teemo/Skins/Skin0:skinScale`",
                    args.value
                )
            })?;
            (reference.entry, Some(reference.path))
        }
        false => {
            let entry = EntryName::try_from(args.value.as_str())
                .map_err(|error| miette::miette!("`{}` is not an entry: {error}", args.value))?;
            (entry, None)
        }
    };

    let object = match &args.bin {
        Some(file) => object_of(file, entry.object_hash())?
            .ok_or_else(|| miette::miette!("{file} has no entry {entry}"))?,
        None => {
            let game = args.game.open(ctx)?;
            game.object(entry.object_hash())?
                .ok_or_else(|| miette::miette!("No bin of the game declares {entry}"))?
        }
    };

    let names = match args.keep_hashed {
        true => GameNames::default(),
        false => ctx.game_names(),
    };
    let value = match &path {
        Some(path) => {
            let value = object
                .resolve(path)
                .map_err(|error| miette::miette!("{entry} has no `{}`: {error}", path.as_str()))?;
            Value::render(value, &names)
        }
        None => entry_body(&object, &names),
    }
    .map_err(|error| {
        miette::miette!("{} cannot be written as a declaration: {error}", args.value)
    })?;

    let mut text = value
        .to_yaml()
        .map_err(|error| miette::miette!("{error}"))?;
    text.push('\n');
    write_bytes(STDIO.into(), text.as_bytes())
}

/// Reads the object `hash` of the bin or ritobin text file at `path`.
fn object_of(path: &Utf8Path, hash: BinHash) -> Result<Option<BinObject>> {
    let document = Document::read(path, ReadOptions::default())?;
    let objects = match document.file {
        BinFile::Prop(bin) => bin.objects,
        BinFile::Override(patch) => patch.objects,
    };
    Ok(objects.get(&hash).cloned())
}

/// Renders every property of `object` as the body a manifest gives an entry: one key for each
/// property.
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
    fn an_edited_bin_is_written_at_its_path_or_under_its_hash() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/skin0.bin", &bin())]);
        let game = installation.open();

        assert_eq!(
            output_name(&edited("data/Skin0.bin", "data/skin0.bin"), &game),
            "data/Skin0.bin"
        );

        // A target spelled as a hash, with no hashtable to name it.
        let hash = format!("{:016x}", chunk_hash("data/skin0.bin").0);
        assert_eq!(
            output_name(&edited(&hash, "data/skin0.bin"), &game),
            format!("{hash}.bin")
        );

        // A target that would be written outside the output directory.
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
    fn an_entry_renders_as_a_body_of_its_properties() {
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
