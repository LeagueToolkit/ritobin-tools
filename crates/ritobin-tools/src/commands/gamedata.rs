use std::cell::OnceCell;

use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Subcommand};
use indexmap::IndexMap;
use ltk_game_data::{EntryName, FieldNames, Reference, Value};
use ltk_hash::{BinHash, WadHash};
use ltk_meta::{BinFile, BinObject};
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;

use crate::{
    cli::LayoutArgs,
    commands::{
        input::not_found,
        output::{OutputArgs, OutputFormat, columns},
    },
    context::Context,
    document::{
        DEFAULT_TEXT_EXTENSION, Document, Format, ReadOptions, STDIO, converted_path, encode,
        reads_back, write_bytes,
    },
    game::{BinRef, Game},
    gamedata::{self, Changes, Layer, Outcome, Problem},
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

    /// Write bins of the game to files
    Extract(ExtractArgs),
}

/// Options that locate the game and its index cache.
#[derive(Args, Debug, Clone, Default)]
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

#[derive(Args, Debug)]
pub struct ExtractArgs {
    /// Bins to extract. Each is a bin path in the game
    /// (`data/characters/teemo/skins/skin0.bin`), a chunk hash of 16 hex digits, or an entry
    /// (`Characters/Teemo/Skins/Skin0` or `0x1234abcd`). An entry selects every bin that
    /// declares it
    #[arg(value_name = "BINS")]
    pub bins: Vec<String>,

    /// Also extract the bins of the archives whose name contains TEXT, without regard to case
    #[arg(long, value_name = "TEXT")]
    pub wad: Option<String>,

    /// Also extract the bins whose path contains TEXT, without regard to case. With `--wad`, a
    /// bin must pass both filters
    #[arg(long, value_name = "TEXT")]
    pub bin: Option<String>,

    /// Write the bin to this file. Requires that exactly one bin is selected. `-` writes
    /// standard output
    #[arg(short, long, value_name = "FILE", conflicts_with = "output_dir")]
    pub output: Option<Utf8PathBuf>,

    /// Write each bin at its game path under this directory
    #[arg(short = 'd', long, value_name = "DIR")]
    pub output_dir: Option<Utf8PathBuf>,

    #[command(flatten)]
    pub game: GameArgs,

    /// Output format. Defaults to the format of the `--output` file extension, or to `bin`
    #[arg(short, long, value_enum, value_name = "FORMAT")]
    pub to: Option<Format>,

    /// File extension for text output under `--output-dir`
    #[arg(long = "ext", value_name = "EXT", default_value = DEFAULT_TEXT_EXTENSION)]
    pub text_extension: String,

    /// Write hashes as hex in text output. Do not resolve them with the hashtables
    #[arg(short, long)]
    pub keep_hashed: bool,

    /// Do not overwrite an existing output file
    #[arg(long)]
    pub skip_existing: bool,

    /// Skip the check that printed text parses back to the same bin
    #[arg(long)]
    pub no_verify: bool,

    #[command(flatten)]
    pub layout: LayoutArgs,
}

/// Runs a `gamedata` command. Returns `false` if `check` or `apply` reported at least one
/// problem.
pub fn run(ctx: &Context, command: GameDataCommand) -> Result<bool> {
    match command {
        GameDataCommand::Check(args) => check(ctx, &args),
        GameDataCommand::Apply(args) => apply(ctx, &args),
        GameDataCommand::Render(args) => render(ctx, &args).map(|()| true),
        GameDataCommand::Extract(args) => extract(ctx, &args).map(|()| true),
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
        let path = output_name(&bin.target, bin.chunk, &game)
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

/// The maximum length of an output file name, in bytes. File systems allow 255 bytes. The
/// lower limit leaves room for a text extension that is longer than `bin`.
///
/// The game has bins with longer names, for example the bins that contain the shared objects
/// of many skins of one champion.
const MAX_FILE_NAME: usize = 240;

/// Returns the text after the last `/` of `path`.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Returns the output path of the bin `chunk`, relative to the output directory, with `/`
/// separators. `target` is the bin path from the manifest or from the command line.
///
/// Uses `target` if it is a safe relative path, then the chunk path from the hashtables. Falls
/// back to `<chunk hash>.bin`. If the file name of the path is longer than [`MAX_FILE_NAME`],
/// the file name is replaced by `<chunk hash>.bin` and the directory is kept.
fn output_name(target: &str, chunk: WadHash, game: &Game) -> Utf8PathBuf {
    let hash_name = format!("{:016x}", chunk.0);
    let is_hash = |name: &str| name.eq_ignore_ascii_case(&hash_name);
    // The check is on the string, not on a platform path, so the output tree is identical on
    // every platform. A drive letter or a backslash is rejected on Linux as well.
    let relative = |name: &str| {
        !is_hash(name)
            && name.split('/').all(|segment| {
                !matches!(segment, "" | "." | "..") && !segment.contains([':', '\\'])
            })
    };

    let named = game.chunk_name(chunk);
    let Some(name) = [target, named.as_str()]
        .into_iter()
        .find(|name| relative(name))
    else {
        return format!("{hash_name}.bin").into();
    };
    let file = file_name(name);
    match file.len() > MAX_FILE_NAME {
        true => format!("{}{hash_name}.bin", &name[..name.len() - file.len()]).into(),
        false => name.into(),
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

/// One bin that `extract` writes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Selected {
    chunk: WadHash,
    /// The bin path as written on the command line, or the chunk path from the hashtables.
    name: String,
}

/// Returns the bins that `args` selects: the bins of each `BINS` argument in argument order,
/// then the bins that pass the `--wad` and `--bin` filters, in archive order and path order.
/// Each chunk is listed once.
///
/// Fails if nothing is selected, or if a `BINS` argument selects no bin.
fn selected(ctx: &Context, args: &ExtractArgs, game: &Game) -> Result<Vec<Selected>> {
    let mut bins: IndexMap<WadHash, String> = IndexMap::new();

    let mut missing = Vec::new();
    for text in &args.bins {
        let bin_ref = BinRef::parse(text)?;
        let chunks = game.select(&bin_ref);
        if chunks.is_empty() {
            missing.push(not_found(&bin_ref));
        }
        for chunk in chunks {
            let name = match &bin_ref {
                BinRef::Chunk { name, .. } => name.clone(),
                BinRef::Entry { .. } => game.chunk_name(chunk),
            };
            bins.entry(chunk).or_insert(name);
        }
    }
    if !missing.is_empty() {
        miette::bail!("{}", missing.join(". "));
    }

    if args.wad.is_some() || args.bin.is_some() {
        let passes = |text: &str, filter: &Option<String>| {
            filter
                .as_ref()
                .is_none_or(|filter| text.to_lowercase().contains(&filter.to_lowercase()))
        };
        let before = bins.len();
        for archive in game.bin_archives() {
            if !passes(&archive.name, &args.wad) {
                continue;
            }
            let mut names: Vec<(String, WadHash)> = archive
                .chunks
                .iter()
                .map(|chunk| (game.chunk_name(*chunk), *chunk))
                .filter(|(name, _)| passes(name, &args.bin))
                .collect();
            names.sort();
            for (name, chunk) in names {
                bins.entry(chunk).or_insert(name);
            }
        }
        // Without the `game` table a chunk is named by its hash, so a path filter selects no
        // bin.
        if bins.len() == before && args.bin.is_some() && !ctx.wad_paths().is_loaded() {
            tracing::warn!(
                "--bin selected no bin, because the `game` hashtable is not installed and the bin paths are unknown. Run `ritobin-tools hashes sync` to download the hashtables."
            );
        }
    }

    if bins.is_empty() {
        miette::bail!(
            "No bin was selected. Pass a bin path such as data/characters/teemo/skins/skin0.bin, an entry, --wad or --bin"
        );
    }
    Ok(bins
        .into_iter()
        .map(|(chunk, name)| Selected { chunk, name })
        .collect())
}

/// Writes the selected game bins to `--output` or under `--output-dir`.
///
/// With `--output-dir`, a bin that cannot be read or printed does not stop the run. The command
/// fails after the last bin if any bin failed.
fn extract(ctx: &Context, args: &ExtractArgs) -> Result<()> {
    if args.output.is_none() && args.output_dir.is_none() {
        miette::bail!(
            "No output was given. Pass --output <FILE> for one bin, or --output-dir <DIR>"
        );
    }
    let game = args.game.open(ctx)?;
    let bins = selected(ctx, args, &game)?;

    let layout = args.layout.over(ctx.config.print_config);
    // The hashtables are loaded on first use. A run that writes binary bins does not load them.
    let tables = OnceCell::new();
    let hashes = || {
        tables.get_or_init(|| match args.keep_hashed {
            true => BinHashes::none(),
            false => ctx.hashes(),
        })
    };
    let write = |bin: &Selected, path: &Utf8Path, to: Format| -> Result<()> {
        let data = game
            .chunk(bin.chunk)?
            .ok_or_else(|| miette::miette!("No game archive contains the bin {}", bin.name))?;
        if Format::detect(&data) != Format::Bin {
            miette::bail!(
                "{} is not a bin file. `gamedata extract` writes only bin files",
                bin.name
            );
        }
        let data = match to {
            Format::Bin => data,
            Format::Rito => {
                let document = Document::parse(&bin.name, data, ReadOptions::default())?;
                let text = encode(&document.file, Format::Rito, layout, hashes())
                    .wrap_err_with(|| format!("Failed to print {}", bin.name))?;
                if !args.no_verify
                    && !std::str::from_utf8(&text)
                        .is_ok_and(|text| reads_back(&document.file, text))
                {
                    tracing::warn!(
                        "The text printed for {} does not parse back to the same bin, because the printer does not print every value exactly. Extract the bin as a .bin file instead.",
                        bin.name
                    );
                }
                text
            }
        };
        write_bytes(path, &data)
    };

    if let Some(output) = &args.output {
        let [bin] = bins.as_slice() else {
            let names: Vec<&str> = bins.iter().take(5).map(|bin| bin.name.as_str()).collect();
            miette::bail!(
                "--output requires exactly one bin, but {} were selected ({}{}). Pass --output-dir <DIR> to write all of them",
                bins.len(),
                names.join(", "),
                match bins.len() > names.len() {
                    true => ", ...",
                    false => "",
                }
            );
        };
        let to = args
            .to
            .or_else(|| output.extension().and_then(Format::from_extension))
            .unwrap_or(Format::Bin);
        let to_stdout = output.as_str() == STDIO;
        if args.skip_existing && !to_stdout && output.exists() {
            tracing::info!(
                "Skipped {}: {} already exists",
                bin.name,
                hyperlink_path(output)
            );
            return Ok(());
        }
        write(bin, output, to)?;
        if !to_stdout {
            tracing::info!("Extracted {} -> {}", bin.name, hyperlink_path(output));
        }
        return Ok(());
    }

    let Some(dir) = &args.output_dir else {
        return Ok(());
    };
    let to = args.to.unwrap_or(Format::Bin);
    let (mut extracted, mut skipped, mut failed, mut renamed) = (0, 0, 0, 0);
    for bin in &bins {
        let name = output_name(&bin.name, bin.chunk, &game);
        if file_name(&bin.name).len() > MAX_FILE_NAME
            && file_name(name.as_str()) != file_name(&bin.name)
        {
            renamed += 1;
            tracing::debug!(
                "The file name of {} is too long for a file. The bin is written as {name}",
                bin.name
            );
        }
        // `output_name` uses `/`. Joining component by component produces the platform
        // separator.
        let path = name
            .components()
            .fold(dir.clone(), |path, part| path.join(part));
        let path = match to {
            Format::Bin => path,
            Format::Rito => converted_path(
                &path,
                Format::Rito,
                args.text_extension.trim_start_matches('.'),
            ),
        };
        if args.skip_existing && path.exists() {
            tracing::debug!("Skipped {}: {path} already exists", bin.name);
            skipped += 1;
            continue;
        }
        match write(bin, &path, to) {
            Ok(()) => {
                extracted += 1;
                tracing::debug!("Extracted {} -> {path}", bin.name);
            }
            Err(error) => {
                failed += 1;
                eprintln!("{error:?}");
            }
        }
    }

    if renamed > 0 {
        tracing::info!(
            "{} a file name that is too long for a file. Each of them is written under its chunk hash in the directory of its game path. Pass `-L debug` to list them.",
            match renamed {
                1 => "1 bin has".to_owned(),
                renamed => format!("{renamed} bins have"),
            }
        );
    }
    tracing::info!(
        "Extracted {} to {}, {skipped} skipped, {failed} failed",
        plural(extracted, "bin"),
        hyperlink_path(dir)
    );
    if failed > 0 {
        miette::bail!(
            "{failed} of {} failed to extract",
            plural(bins.len(), "bin")
        );
    }
    Ok(())
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
        object_bin(0x1111_0001)
    }

    /// Returns a bin with one object that has the path hash `object`.
    fn object_bin(object: u32) -> Vec<u8> {
        let bin: BinFile = Bin::builder()
            .object(
                BinObject::builder(object, 0xaaaa_0001u32)
                    .property(0x10u32, values::I32::new(42))
                    .property(0x11u32, values::String::new("teemo".to_owned()))
                    .build(),
            )
            .build()
            .into();
        to_bin(&bin).unwrap()
    }

    #[test]
    fn output_name_uses_safe_path_or_chunk_hash() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/skin0.bin", &bin())]);
        let game = installation.open();

        assert_eq!(
            output_name("data/Skin0.bin", chunk_hash("data/skin0.bin"), &game),
            "data/Skin0.bin"
        );

        // The target is a chunk hash and no hashtable is loaded, so the path is unknown.
        let hash = format!("{:016x}", chunk_hash("data/skin0.bin").0);
        assert_eq!(
            output_name(&hash, chunk_hash("data/skin0.bin"), &game),
            format!("{hash}.bin")
        );

        // A file name that is longer than a file system allows.
        let long = format!("data/characters/{}.bin", "skin".repeat(70));
        assert_eq!(
            output_name(&long, chunk_hash(&long), &game),
            format!("data/characters/{:016x}.bin", chunk_hash(&long).0)
        );

        // Targets that are absolute, have a drive letter, or contain `..` or a backslash.
        for target in [
            "../outside.bin",
            "/root.bin",
            "C:/drive.bin",
            "data\\..\\..\\outside.bin",
        ] {
            let name = output_name(target, chunk_hash(target), &game);
            assert_eq!(
                name,
                format!("{:016x}.bin", chunk_hash(target).0),
                "{target}"
            );
        }
    }

    fn extract_args(installation: &Installation, bins: &[&str]) -> ExtractArgs {
        ExtractArgs {
            bins: bins.iter().map(|bin| (*bin).to_owned()).collect(),
            wad: None,
            bin: None,
            output: None,
            output_dir: None,
            game: GameArgs {
                game_dir: Some(installation.root.clone()),
                index_dir: Some(installation.root.join("index")),
            },
            to: None,
            text_extension: DEFAULT_TEXT_EXTENSION.to_owned(),
            keep_hashed: true,
            skip_existing: false,
            no_verify: false,
            layout: LayoutArgs::default(),
        }
    }

    /// Returns the path that `--output-dir` uses for a bin without a known path.
    fn hash_name(chunk: &str) -> String {
        format!("{:016x}.bin", chunk_hash(chunk).0)
    }

    #[test]
    fn extract_writes_bin_to_output_in_format_of_extension() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/skin0.bin", &bin())]);
        let ctx = Context::for_tests(None);
        let out = installation.root.join("out");

        let copy = out.join("copy.bin");
        extract(
            &ctx,
            &ExtractArgs {
                output: Some(copy.clone()),
                ..extract_args(&installation, &["DATA/Skin0.bin"])
            },
        )
        .unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), bin());

        let text = out.join("copy.rito");
        extract(
            &ctx,
            &ExtractArgs {
                output: Some(text.clone()),
                ..extract_args(&installation, &["0x11110001"])
            },
        )
        .unwrap();
        assert!(std::fs::read(&text).unwrap().starts_with(b"#PROP_text"));
        let document = Document::read(&text, ReadOptions::default()).unwrap();
        assert_eq!(document.file, decode(&bin()));
    }

    #[test]
    fn extract_writes_selected_bins_at_game_path_under_output_dir() {
        let installation = Installation::new();
        installation.archive(
            "A.wad.client",
            &[
                ("data/skin0.bin", &object_bin(1)),
                ("data/skin1.bin", &object_bin(2)),
            ],
        );
        installation.archive("Maps/B.wad.client", &[("data/other.bin", &object_bin(3))]);
        let ctx = Context::for_tests(None);
        let files = |dir: &Utf8Path| {
            let mut files: Vec<String> = walkdir::WalkDir::new(dir)
                .into_iter()
                .map(|entry| entry.unwrap())
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| {
                    let path = entry.path().strip_prefix(dir).unwrap();
                    path.to_string_lossy().replace('\\', "/")
                })
                .collect();
            files.sort();
            files
        };

        // A bin path keeps its spelling. The bin of an entry has no known path, because no
        // hashtable is loaded, so it is named by its chunk hash.
        let out = installation.root.join("out");
        extract(
            &ctx,
            &ExtractArgs {
                output_dir: Some(out.clone()),
                ..extract_args(&installation, &["data/Skin0.bin", "0x00000002"])
            },
        )
        .unwrap();
        let mut expected = vec!["data/Skin0.bin".to_owned(), hash_name("data/skin1.bin")];
        expected.sort();
        assert_eq!(files(&out), expected);
        assert_eq!(
            std::fs::read(out.join("data").join("Skin0.bin")).unwrap(),
            object_bin(1)
        );

        let filtered = installation.root.join("filtered");
        extract(
            &ctx,
            &ExtractArgs {
                output_dir: Some(filtered.clone()),
                wad: Some("maps/b".to_owned()),
                to: Some(Format::Rito),
                ..extract_args(&installation, &[])
            },
        )
        .unwrap();
        assert_eq!(
            files(&filtered),
            [hash_name("data/other.bin").replace(".bin", ".rito")]
        );
    }

    #[test]
    fn extract_fails_for_missing_bin_several_bins_or_no_selection() {
        let installation = Installation::new();
        installation.archive(
            "A.wad.client",
            &[
                ("data/skin0.bin", &object_bin(1)),
                ("data/copy.bin", &object_bin(1)),
                ("data/notes.bin", b"not a bin"),
            ],
        );
        let ctx = Context::for_tests(None);
        let out = installation.root.join("out");
        let error = |args: ExtractArgs| extract(&ctx, &args).unwrap_err().to_string();

        assert_eq!(
            error(ExtractArgs {
                output_dir: Some(out.clone()),
                ..extract_args(&installation, &["data/skin0.bin", "data/missing.bin"])
            }),
            "No game archive contains the bin data/missing.bin"
        );
        // Two bins declare the entry.
        assert!(
            error(ExtractArgs {
                output: Some(out.join("skin0.bin")),
                ..extract_args(&installation, &["0x00000001"])
            })
            .contains("--output requires exactly one bin, but 2 were selected")
        );
        assert!(
            error(ExtractArgs {
                output: Some(out.join("notes.bin")),
                ..extract_args(&installation, &["data/notes.bin"])
            })
            .contains("data/notes.bin is not a bin file")
        );
        assert!(
            error(ExtractArgs {
                output_dir: Some(out.clone()),
                ..extract_args(&installation, &[])
            })
            .contains("No bin was selected")
        );
        assert!(
            error(extract_args(&installation, &["data/skin0.bin"])).contains("No output was given")
        );
        assert!(!out.exists());
    }

    #[test]
    fn extract_skips_existing_file_with_skip_existing() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/skin0.bin", &bin())]);
        let ctx = Context::for_tests(None);
        let out = installation.root.join("out");
        let existing = out.join("data").join("skin0.bin");
        std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
        std::fs::write(&existing, b"keep").unwrap();

        let args = |skip_existing: bool| ExtractArgs {
            output_dir: Some(out.clone()),
            skip_existing,
            ..extract_args(&installation, &["data/skin0.bin"])
        };
        extract(&ctx, &args(true)).unwrap();
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep");
        extract(&ctx, &args(false)).unwrap();
        assert_eq!(std::fs::read(&existing).unwrap(), bin());
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
