//! Reading and writing bin documents in either format: binary `.bin` and ritobin text.

use std::{
    fmt,
    io::{self, Cursor, Read, Write},
};

use camino::{Utf8Path, Utf8PathBuf};
use ltk_meta::{BinFile, BinKind};
use ltk_ritobin::{
    Cst,
    ast::diagnostics::Diagnostic,
    cst::CstBuilder,
    parse::Span,
    print::{CstPrinter, PrintConfig, PrintError, WrapConfig},
};
use miette::{Diagnostic as MietteDiagnostic, IntoDiagnostic, NamedSource, Result, SourceSpan};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hashes::BinHashes;

/// The path that stands for standard input or standard output.
pub const STDIO: &str = "-";

/// The extension written for ritobin text unless another is asked for.
pub const DEFAULT_TEXT_EXTENSION: &str = "rito";

/// The extensions a directory scan reads as ritobin text.
pub const TEXT_EXTENSIONS: &[&str] = &["rito", "ritobin", "py"];

/// The extension of a binary bin file.
pub const BIN_EXTENSION: &str = "bin";

/// How many problems of one text file are shown before the rest are counted.
const MAX_SHOWN_PROBLEMS: usize = 20;

/// The two formats a bin document is stored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Binary, as the game reads it.
    Bin,
    /// Ritobin text.
    #[value(alias = "text", alias = "ritobin")]
    Rito,
}

impl Format {
    /// The format of `data`, told by its content. Anything without a bin magic is text.
    pub fn detect(data: &[u8]) -> Self {
        match BinKind::identify_from_bytes(data) {
            Some(_) => Self::Bin,
            None => Self::Rito,
        }
    }

    /// The format a file with this extension holds, if the extension is a known one.
    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.to_ascii_lowercase();
        if extension == BIN_EXTENSION {
            Some(Self::Bin)
        } else if TEXT_EXTENSIONS.contains(&extension.as_str()) {
            Some(Self::Rito)
        } else {
            None
        }
    }

    /// The format a conversion produces when none is asked for.
    pub fn opposite(self) -> Self {
        match self {
            Self::Bin => Self::Rito,
            Self::Rito => Self::Bin,
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bin => "bin",
            Self::Rito => "rito",
        })
    }
}

/// How ritobin text is laid out.
///
/// This is the `[print_config]` table of the config file, in the shape `ltk_ritobin` serializes
/// its own print config. Every field has a command line flag that overrides it for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "LayoutTable", into = "LayoutTable")]
pub struct TextLayout {
    /// Spaces per indent level.
    pub indent_size: usize,
    /// The line width past which a block is broken over several lines. It is kept between
    /// [`MIN_LINE_WIDTH`] and [`MAX_LINE_WIDTH`].
    pub line_width: usize,
    /// Whether a struct that fits on one line is printed on one line.
    pub inline_structs: bool,
    /// Whether a list that fits on one line is printed on one line.
    pub inline_lists: bool,
}

impl Default for TextLayout {
    fn default() -> Self {
        let config = PrintConfig::default();
        Self {
            indent_size: config.indent_size,
            line_width: config.wrap.line_width,
            inline_structs: config.wrap.inline_structs,
            inline_lists: config.wrap.inline_lists,
        }
    }
}

/// [`TextLayout`] as the config file holds it. A key that is left out keeps its default.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct LayoutTable {
    indent_size: usize,
    wrap: WrapTable,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct WrapTable {
    line_width: usize,
    inline_structs: bool,
    inline_lists: bool,
}

impl Default for LayoutTable {
    fn default() -> Self {
        TextLayout::default().into()
    }
}

impl Default for WrapTable {
    fn default() -> Self {
        LayoutTable::default().wrap
    }
}

impl From<TextLayout> for LayoutTable {
    fn from(layout: TextLayout) -> Self {
        Self {
            indent_size: layout.indent_size,
            wrap: WrapTable {
                line_width: layout.line_width,
                inline_structs: layout.inline_structs,
                inline_lists: layout.inline_lists,
            },
        }
    }
}

impl From<LayoutTable> for TextLayout {
    fn from(table: LayoutTable) -> Self {
        Self {
            indent_size: table.indent_size,
            line_width: table.wrap.line_width,
            inline_structs: table.wrap.inline_structs,
            inline_lists: table.wrap.inline_lists,
        }
    }
}

impl From<TextLayout> for PrintConfig<()> {
    fn from(layout: TextLayout) -> Self {
        PrintConfig::default().indent_size(layout.indent_size).wrap(
            WrapConfig::default()
                .line_width(layout.line_width)
                .inline_structs(layout.inline_structs)
                .inline_lists(layout.inline_lists),
        )
    }
}

/// How ritobin text is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadOptions {
    /// Build a best-effort document from text that has problems, reporting them as warnings.
    ///
    /// The parts of the text a problem names may be missing from the result.
    pub lenient: bool,
}

/// A bin document and the format it was read from.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub file: BinFile,
    pub format: Format,
}

impl Document {
    /// Reads the document at `path`, or standard input for [`STDIO`]. The format is told by the
    /// content, not the extension.
    pub fn read(path: &Utf8Path, options: ReadOptions) -> Result<Self> {
        let data = read_bytes(path)?;
        Self::parse(path.as_str(), data, options)
    }

    /// Parses a document from memory. `name` is what diagnostics call it.
    pub fn parse(name: &str, data: Vec<u8>, options: ReadOptions) -> Result<Self> {
        match Format::detect(&data) {
            Format::Bin => {
                let file = BinFile::from_reader(&mut Cursor::new(data))
                    .map_err(|error| miette::miette!("{name} is not a valid bin file: {error}"))?;
                Ok(Self {
                    file,
                    format: Format::Bin,
                })
            }
            Format::Rito => {
                let text = String::from_utf8(data).map_err(|_| {
                    miette::miette!("{name} is neither a bin file nor UTF-8 ritobin text")
                })?;
                let text = match text.strip_prefix('\u{feff}') {
                    Some(without_mark) => without_mark.to_owned(),
                    None => text,
                };
                let file = parse_text(name, text, options)?;
                Ok(Self {
                    file,
                    format: Format::Rito,
                })
            }
        }
    }
}

/// The format of the file at `path`, told by its first bytes.
pub fn detect_file(path: &Utf8Path) -> Result<Format> {
    let mut magic = Vec::with_capacity(4);
    std::fs::File::open(path)
        .and_then(|file| file.take(4).read_to_end(&mut magic))
        .into_diagnostic()
        .map_err(|error| error.wrap_err(format!("Failed to read {path}")))?;
    Ok(Format::detect(&magic))
}

/// Whether the file at `path` starts with the line ritobin text is written with: `#PROP_text`, or
/// `#PTCH_text` for a patch.
pub fn has_text_header(path: &Utf8Path) -> io::Result<bool> {
    const BYTE_ORDER_MARK: &[u8] = b"\xef\xbb\xbf";
    const HEADERS: [&[u8]; 2] = [b"#PROP_text", b"#PTCH_text"];

    let mut start = Vec::with_capacity(16);
    std::fs::File::open(path)?
        .take((BYTE_ORDER_MARK.len() + HEADERS[0].len()) as u64)
        .read_to_end(&mut start)?;
    let start = start.strip_prefix(BYTE_ORDER_MARK).unwrap_or(&start);
    Ok(HEADERS.iter().any(|header| start.starts_with(header)))
}

/// Reads the whole of `path`, or of standard input for [`STDIO`].
pub fn read_bytes(path: &Utf8Path) -> Result<Vec<u8>> {
    if path == STDIO {
        let mut data = Vec::new();
        io::stdin()
            .lock()
            .read_to_end(&mut data)
            .into_diagnostic()
            .map_err(|error| error.wrap_err("Failed to read standard input"))?;
        return Ok(data);
    }
    std::fs::read(path)
        .into_diagnostic()
        .map_err(|error| error.wrap_err(format!("Failed to read {path}")))
}

/// Writes `data` to `path`, or to standard output for [`STDIO`], creating the parent directories.
pub fn write_bytes(path: &Utf8Path, data: &[u8]) -> Result<()> {
    if path == STDIO {
        let mut stdout = io::stdout().lock();
        return stdout
            .write_all(data)
            .and_then(|()| stdout.flush())
            .into_diagnostic()
            .map_err(|error| error.wrap_err("Failed to write standard output"));
    }
    if let Some(parent) = path.parent().filter(|parent| !parent.as_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .into_diagnostic()
            .map_err(|error| error.wrap_err(format!("Failed to create {parent}")))?;
    }
    std::fs::write(path, data)
        .into_diagnostic()
        .map_err(|error| error.wrap_err(format!("Failed to write {path}")))
}

/// Encodes `file` in `format`.
pub fn encode(
    file: &BinFile,
    format: Format,
    layout: TextLayout,
    hashes: &BinHashes,
) -> Result<Vec<u8>> {
    match format {
        Format::Bin => to_bin(file),
        Format::Rito => Ok(to_text(file, layout, hashes)
            .into_diagnostic()?
            .into_bytes()),
    }
}

/// Encodes `file` as a binary bin of its own kind.
pub fn to_bin(file: &BinFile) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    file.to_writer(&mut out)
        .into_diagnostic()
        .map_err(|error| error.wrap_err("Failed to encode the bin file"))?;
    Ok(out.into_inner())
}

/// Prints `file` as ritobin text, naming every hash `hashes` knows.
pub fn to_text(
    file: &BinFile,
    layout: TextLayout,
    hashes: &BinHashes,
) -> Result<String, PrintError> {
    // `Print::print_with_config` drops the layout of its config, so the tree is built and printed
    // here.
    let builder = CstBuilder::new().with_hashes(hashes.clone());
    let (cst, source) = match file {
        BinFile::Prop(bin) => builder.build(bin),
        BinFile::Override(patch) => builder.build_override(patch),
    };
    // The printer is always asked for its own indent, and the text is indented again after it.
    let config = PrintConfig::from(TextLayout {
        indent_size: PRINTER_INDENT,
        line_width: layout.line_width.clamp(MIN_LINE_WIDTH, MAX_LINE_WIDTH),
        ..layout
    });
    let mut text = String::new();
    CstPrinter::new(&source, &mut text, config).print(&cst)?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(reindent(text, layout.indent_size))
}

/// Whether `text` reads back as exactly `file`: it parses with no problem, and what it parses to
/// encodes to the same bytes.
///
/// The printer does not write every value in a form the parser reads back, so text printed from a
/// bin is checked with this before it is trusted.
pub fn reads_back(file: &BinFile, text: &str) -> bool {
    let cst = Cst::parse(text);
    if !cst.errors.is_empty() {
        return false;
    }
    let (parsed, diagnostics) = cst.build(text);
    if !diagnostics.is_empty() {
        return false;
    }
    match (to_bin(file), to_bin(&parsed)) {
        (Ok(original), Ok(parsed)) => original == parsed,
        _ => false,
    }
}

/// The indent the printer writes, whatever its config asks for.
const PRINTER_INDENT: usize = 4;

/// The narrowest line width the printer is given. Below it the printer breaks inside a type, and
/// the text no longer parses.
pub const MIN_LINE_WIDTH: usize = 40;

/// The widest line width the printer is given. Far above it the printer runs out of the room it
/// keeps for one line.
pub const MAX_LINE_WIDTH: usize = 200;

/// Rewrites the leading [`PRINTER_INDENT`] steps of every line as steps of `indent_size`.
///
/// A line break inside a string is written as an escape, so the spaces a line starts with are
/// always indentation.
fn reindent(text: String, indent_size: usize) -> String {
    if indent_size == PRINTER_INDENT {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let content = line.trim_start_matches(' ');
        let spaces = line.len() - content.len();
        let (levels, rest) = (spaces / PRINTER_INDENT, spaces % PRINTER_INDENT);
        out.extend(std::iter::repeat_n(' ', levels * indent_size + rest));
        out.push_str(content);
    }
    out
}

/// The output path of a conversion that names none: `input` with the extension of the target
/// format.
pub fn converted_path(input: &Utf8Path, to: Format, text_extension: &str) -> Utf8PathBuf {
    input.with_extension(match to {
        Format::Bin => BIN_EXTENSION,
        Format::Rito => text_extension,
    })
}

/// One problem in a ritobin text file, pointing at the text it is about.
#[derive(Debug, Error, MietteDiagnostic)]
#[error("{message}")]
struct TextProblem {
    message: String,
    #[label]
    span: SourceSpan,
}

impl TextProblem {
    fn new(message: impl fmt::Display, span: Span) -> Self {
        let start = span.start as usize;
        let len = span.end.saturating_sub(span.start) as usize;
        Self {
            message: message.to_string(),
            span: (start, len).into(),
        }
    }
}

/// The problems that stop a ritobin text file from being read.
#[derive(Debug, Error, MietteDiagnostic)]
#[error("{name} has {count} {}", if *.count == 1 { "problem" } else { "problems" })]
struct TextProblems {
    name: String,
    count: usize,
    #[source_code]
    source_code: NamedSource<String>,
    #[related]
    shown: Vec<TextProblem>,
    #[help]
    help: Option<String>,
}

fn parse_text(name: &str, text: String, options: ReadOptions) -> Result<BinFile> {
    let cst = Cst::parse(&text);
    let (file, diagnostics) = cst.build(&text);

    let mut errors: Vec<TextProblem> = cst
        .errors
        .iter()
        .map(|error| TextProblem::new(error, error.span))
        .collect();
    let mut warnings = Vec::new();
    for diagnostic in &diagnostics {
        let problem = TextProblem::new(diagnostic.diagnostic, diagnostic.span);
        // A later entry replacing an earlier one loses nothing the text asked to keep.
        match diagnostic.diagnostic {
            Diagnostic::ShadowedEntry { .. } => warnings.push(problem),
            _ => errors.push(problem),
        }
    }

    if options.lenient {
        warnings.append(&mut errors);
    }
    if !warnings.is_empty() {
        let report =
            miette::Report::new(problems(name, &text, warnings, None)).with_severity_warning();
        eprintln!("{report:?}");
    }
    if errors.is_empty() {
        return Ok(file);
    }
    Err(problems(
        name,
        &text,
        errors,
        Some("pass --lenient to convert what can be read".to_owned()),
    )
    .into())
}

fn problems(
    name: &str,
    text: &str,
    mut all: Vec<TextProblem>,
    help: Option<String>,
) -> TextProblems {
    let count = all.len();
    all.truncate(MAX_SHOWN_PROBLEMS);
    let help = match (count > all.len(), help) {
        (true, Some(help)) => Some(format!("the first {MAX_SHOWN_PROBLEMS} are shown; {help}")),
        (true, None) => Some(format!("the first {MAX_SHOWN_PROBLEMS} are shown")),
        (false, help) => help,
    };
    TextProblems {
        name: name.to_owned(),
        count,
        source_code: NamedSource::new(name, text.to_owned()),
        shown: all,
        help,
    }
}

/// Marks a report as a warning, so it renders as one.
trait ReportExt {
    fn with_severity_warning(self) -> Self;
}

impl ReportExt for miette::Report {
    fn with_severity_warning(self) -> Self {
        #[derive(Debug, Error)]
        #[error("{0}")]
        struct Warning(miette::Report);

        impl MietteDiagnostic for Warning {
            fn severity(&self) -> Option<miette::Severity> {
                Some(miette::Severity::Warning)
            }
            fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
                self.0.help()
            }
            fn source_code(&self) -> Option<&dyn miette::SourceCode> {
                self.0.source_code()
            }
            fn related<'a>(
                &'a self,
            ) -> Option<Box<dyn Iterator<Item = &'a dyn MietteDiagnostic> + 'a>> {
                self.0.related()
            }
        }

        miette::Report::new(Warning(self))
    }
}

#[cfg(test)]
mod tests {
    use ltk_hash::BinHash;
    use ltk_meta::{Bin, BinObject, BinOverride, path::PropertyPath, property::values};

    use super::*;

    fn sample() -> BinFile {
        Bin::builder()
            .dependency("base.bin")
            .object(
                BinObject::builder(0x1111_0001u32, 0xaaaa_0001u32)
                    .property(0x10u32, values::I32::new(42))
                    .property(0x11u32, values::String::from("hello"))
                    .build(),
            )
            .build()
            .into()
    }

    fn roundtrip(file: &BinFile, format: Format) -> BinFile {
        let data = encode(file, format, TextLayout::default(), &BinHashes::none()).unwrap();
        assert_eq!(Format::detect(&data), format);
        Document::parse("test", data, ReadOptions::default())
            .unwrap()
            .file
    }

    #[test]
    fn a_prop_bin_survives_both_formats() {
        let file = sample();
        assert_eq!(roundtrip(&file, Format::Bin), file);
        assert_eq!(roundtrip(&file, Format::Rito), file);
    }

    #[test]
    fn a_patch_bin_survives_both_formats() {
        let file: BinFile = BinOverride::builder()
            .delete(0xdead_beefu32)
            .object(BinObject::new(0x1234, 0x5678))
            .set(
                0xa4ed_cb0du32,
                PropertyPath::new("FlipX").unwrap(),
                values::Bool::new(true),
            )
            .build()
            .into();
        assert_eq!(roundtrip(&file, Format::Bin), file);
        assert_eq!(roundtrip(&file, Format::Rito), file);
    }

    #[test]
    fn the_layout_reaches_the_printer() {
        let narrow = TextLayout {
            indent_size: 2,
            ..TextLayout::default()
        };
        let text = to_text(&sample(), narrow, &BinHashes::none()).unwrap();
        assert!(text.contains("\n  0x11110001 = 0xaaaa0001 {\n    0x10: i32 = 42\n"));

        let inline = TextLayout {
            inline_structs: true,
            ..TextLayout::default()
        };
        let one_property: BinFile = Bin::builder()
            .object(
                BinObject::builder(0x1u32, 0x2u32)
                    .property(0x10u32, values::I32::new(42))
                    .build(),
            )
            .build()
            .into();
        let text = to_text(&one_property, inline, &BinHashes::none()).unwrap();
        assert!(text.contains("0x1 = 0x2 { 0x10: i32 = 42 }"));
    }

    #[test]
    fn reindent_rewrites_only_the_leading_spaces() {
        let text = "a {\n    b: string = \"x    y\"\n        c\n}\n".to_owned();
        assert_eq!(
            reindent(text.clone(), 2),
            "a {\n  b: string = \"x    y\"\n    c\n}\n"
        );
        assert_eq!(reindent(text.clone(), 4), text);
    }

    const VALID_TEXT: &str = "#PROP_text\ntype: string = \"PROP\"\nversion: u32 = 3\nlinked: list[string] = { }\nentries: map[hash, embed] = {\n    0x1 = 0x2 {\n        0x10: u32 = 42\n    }\n}\n";

    #[test]
    fn text_with_a_syntax_error_is_rejected() {
        Document::parse("valid.rito", VALID_TEXT.into(), ReadOptions::default()).unwrap();

        let broken = VALID_TEXT.replace("= 42", "= 4!!2");
        let error =
            Document::parse("broken.rito", broken.into(), ReadOptions::default()).unwrap_err();
        assert!(error.to_string().contains("broken.rito has"));
    }

    #[test]
    fn a_byte_order_mark_before_text_is_ignored() {
        let marked = format!("\u{feff}{VALID_TEXT}");
        let document = Document::parse("marked.rito", marked.into(), ReadOptions::default());
        assert_eq!(
            document.unwrap().file,
            Document::parse("valid.rito", VALID_TEXT.into(), ReadOptions::default())
                .unwrap()
                .file
        );
    }

    #[test]
    fn printed_text_reads_back_as_the_bin_it_was_printed_from() {
        let file = sample();
        let text = to_text(&file, TextLayout::default(), &BinHashes::none()).unwrap();
        assert!(reads_back(&file, &text));
        assert!(!reads_back(&file, &text.replace("= 42", "= 43")));
        assert!(!reads_back(&file, "not ritobin"));
    }

    #[test]
    fn the_line_width_given_to_the_printer_is_bounded() {
        let file = sample();
        for line_width in [0, 1, usize::MAX] {
            let layout = TextLayout {
                line_width,
                ..TextLayout::default()
            };
            let text = to_text(&file, layout, &BinHashes::none()).unwrap();
            assert!(reads_back(&file, &text), "line width {line_width}");
        }
    }

    #[test]
    fn the_format_of_a_file_is_told_by_its_first_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("misnamed.rito")).unwrap();

        std::fs::write(&path, to_bin(&sample()).unwrap()).unwrap();
        assert_eq!(detect_file(&path).unwrap(), Format::Bin);

        std::fs::write(&path, "x").unwrap();
        assert_eq!(detect_file(&path).unwrap(), Format::Rito);
    }

    #[test]
    fn lenient_reading_keeps_what_parses() {
        let text = "#PROP_text\ntype: string = \"PROP\"\nversion: u32 = 3\nlinked: list[string] = { }\nentries: map[hash, embed] = {\n    0x1 = 0x2 {\n        0x10: u32 = \"oops\"\n        0x11: u32 = 7\n    }\n}\n";
        assert!(Document::parse("a.rito", text.into(), ReadOptions::default()).is_err());

        let document =
            Document::parse("a.rito", text.into(), ReadOptions { lenient: true }).unwrap();
        let object = &document.file.objects()[&BinHash(1)];
        assert!(object.contains_property(BinHash(0x11)));
    }

    #[test]
    fn extensions_map_to_formats() {
        assert_eq!(Format::from_extension("bin"), Some(Format::Bin));
        assert_eq!(Format::from_extension("RITO"), Some(Format::Rito));
        assert_eq!(Format::from_extension("py"), Some(Format::Rito));
        assert_eq!(Format::from_extension("json"), None);
    }

    #[test]
    fn a_converted_path_swaps_the_extension() {
        let input = Utf8Path::new("data/skin0.bin");
        assert_eq!(
            converted_path(input, Format::Rito, "rito"),
            "data/skin0.rito"
        );
        assert_eq!(
            converted_path(Utf8Path::new("data/skin0.py"), Format::Bin, "rito"),
            "data/skin0.bin"
        );
    }
}
