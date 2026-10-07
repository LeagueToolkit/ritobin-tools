//! Selects the output of a command that derives one bin from a base bin, and writes that bin.
//! `patch` and `merge` use it.

use camino::{Utf8Path, Utf8PathBuf};
use ltk_meta::{Bin, BinFile};
use miette::{Result, WrapErr};

use crate::{
    commands::input::game_bin,
    document::{Format, STDIO, TextLayout, encode, reads_back, write_bytes},
    hashes::BinHashes,
    utils::{hyperlink_path, same_file_key},
};

/// The output options of a command, and the inputs that the output is checked against.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub base: &'a Utf8PathBuf,
    /// The inputs other than the base bin.
    pub others: &'a [Utf8PathBuf],
    pub output: Option<&'a Utf8PathBuf>,
    pub in_place: bool,
    pub dry_run: bool,
    pub to: Option<Format>,
}

/// The output of a run that writes a bin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub path: Utf8PathBuf,
    /// The output format from `--to` or from the file extension of `path`. `None` selects the
    /// format of the base bin.
    pub format: Option<Format>,
}

impl Destination {
    /// Returns `true` if the bin is written to standard output.
    pub fn is_stdout(&self) -> bool {
        self.path.as_str() == STDIO
    }
}

/// Returns the output path and the requested output format of `request`. Returns `None` for
/// `--dry-run`.
///
/// Fails if more than one input is standard input, if no output option is set, if `--in-place`
/// is set while the base bin is standard input or a bin of the game, or if `--output` is the
/// path of an input.
pub fn destination(request: &Request) -> Result<Option<Destination>> {
    let from_stdin = std::iter::once(request.base)
        .chain(request.others)
        .filter(|input| input.as_str() == STDIO)
        .count();
    if from_stdin > 1 {
        miette::bail!(
            "Standard input can be read only once, but `-` was passed for {from_stdin} inputs. Pass a file path for the other inputs"
        );
    }

    if request.dry_run {
        return Ok(None);
    }
    if request.in_place {
        if request.base.as_str() == STDIO {
            miette::bail!(
                "--in-place requires a file path for BASE, but BASE is standard input. Pass --output instead"
            );
        }
        if game_bin(request.base).is_some() {
            miette::bail!(
                "--in-place requires a file path for BASE, but BASE is a bin of the game. Pass --output instead"
            );
        }
        return Ok(Some(Destination {
            path: request.base.clone(),
            format: None,
        }));
    }
    let Some(output) = request.output else {
        miette::bail!(
            "No output was given. Pass --output <FILE>, --in-place to overwrite BASE, or --dry-run to print the report only"
        );
    };

    if output.as_str() != STDIO {
        let key = same_file_key(output);
        let is_output =
            |input: &Utf8PathBuf| input.as_str() != STDIO && same_file_key(input) == key;
        if is_output(request.base) {
            miette::bail!(
                "The output path {output} is BASE itself. Pass --in-place to overwrite BASE"
            );
        }
        if let Some(input) = request.others.iter().find(|input| is_output(input)) {
            miette::bail!(
                "The output path {output} is the input file {input}. Pass a different path to --output"
            );
        }
    }
    Ok(Some(Destination {
        path: output.clone(),
        format: request
            .to
            .or_else(|| output.extension().and_then(Format::from_extension)),
    }))
}

/// Encodes `bin` in `format` and writes it to `path`. `what` names the bin in the log messages,
/// for example `patched bin`.
///
/// Logs a warning if the output is text that does not parse back to the same bin.
pub fn write_bin(
    path: &Utf8Path,
    format: Format,
    bin: Bin,
    layout: TextLayout,
    hashes: &BinHashes,
    what: &str,
) -> Result<()> {
    let file = BinFile::Prop(bin);
    let data = encode(&file, format, layout, hashes)
        .wrap_err_with(|| format!("Failed to encode the {what}"))?;
    if format == Format::Rito
        && !std::str::from_utf8(&data).is_ok_and(|text| reads_back(&file, text))
    {
        tracing::warn!(
            "The text printed for the {what} does not parse back to the same bin, because the printer does not print every value exactly. Write the {what} as a .bin file instead."
        );
    }

    write_bytes(path, &data)?;
    if path.as_str() != STDIO {
        tracing::info!("Wrote the {what} to {}", hyperlink_path(path));
    }
    Ok(())
}
