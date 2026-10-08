//! Reads and writes bin documents: binary `.bin`, ritobin text, and the declaration formats
//! of the `declaration` module.

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

use crate::{declaration, hashes::BinHashes};

/// The path argument that selects standard input or standard output.
pub const STDIO: &str = "-";

/// The default file extension of ritobin text output.
pub const DEFAULT_TEXT_EXTENSION: &str = "rito";

/// The file extensions that a directory scan treats as ritobin text.
pub const TEXT_EXTENSIONS: &[&str] = &["rito", "ritobin", "py"];

/// The file extension of a binary bin file.
pub const BIN_EXTENSION: &str = "bin";

/// The file extensions of a YAML declaration. The first one is used for output.
pub const YAML_EXTENSIONS: &[&str] = &["yaml", "yml"];

/// The file extension of JSON output.
pub const JSON_EXTENSION: &str = "json";

/// The maximum number of problems shown for one text file. The remaining problems are only
/// counted.
const MAX_SHOWN_PROBLEMS: usize = 20;

/// The storage format of a bin document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// The binary format used by the game.
    Bin,
    /// Ritobin text.
    #[value(alias = "text", alias = "ritobin")]
    Rito,
    /// A game-data declaration in YAML. Values are written without their types. Reading it
    /// requires the class schema of the installed game.
    #[value(alias = "yml")]
    Yaml,
    /// JSON for scripts. It is an output format and cannot be read back.
    Json,
}

impl Format {
    /// Returns the format of `data`, detected from its magic bytes. Returns `Rito` if `data`
    /// does not start with a bin magic. YAML and JSON are recognized by file extension only,
    /// see [`Format::from_extension`].
    pub fn detect(data: &[u8]) -> Self {
        match BinKind::identify_from_bytes(data) {
            Some(_) => Self::Bin,
            None => Self::Rito,
        }
    }

    /// Returns the format for a file extension. Returns `None` for an unknown extension.
    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.to_ascii_lowercase();
        if extension == BIN_EXTENSION {
            Some(Self::Bin)
        } else if TEXT_EXTENSIONS.contains(&extension.as_str()) {
            Some(Self::Rito)
        } else if YAML_EXTENSIONS.contains(&extension.as_str()) {
            Some(Self::Yaml)
        } else if extension == JSON_EXTENSION {
            Some(Self::Json)
        } else {
            None
        }
    }

    /// Returns the default output format of a conversion from this format: ritobin text for a
    /// binary bin, and a binary bin for every other format.
    pub fn opposite(self) -> Self {
        match self {
            Self::Bin => Self::Rito,
            Self::Rito | Self::Yaml | Self::Json => Self::Bin,
        }
    }

    /// Returns `true` for a format that prints hashes as names: every format except `Bin`.
    pub fn is_text(self) -> bool {
        self != Self::Bin
    }

    /// Returns the file extension of the format. `text_extension` is the extension of ritobin
    /// text.
    pub fn extension(self, text_extension: &str) -> &str {
        match self {
            Self::Bin => BIN_EXTENSION,
            Self::Rito => text_extension,
            Self::Yaml => YAML_EXTENSIONS[0],
            Self::Json => JSON_EXTENSION,
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bin => "bin",
            Self::Rito => "rito",
            Self::Yaml => "yaml",
            Self::Json => "json",
        })
    }
}

/// Layout options for printing ritobin text.
///
/// The config file stores them in the `[print_config]` table, in the same structure as the
/// serialized `ltk_ritobin` print config. A command line flag overrides each field for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "LayoutTable", into = "LayoutTable")]
pub struct TextLayout {
    /// Spaces per indent level.
    pub indent_size: usize,
    /// The maximum line width. A block that exceeds it is printed on several lines. The value
    /// is clamped to the range from [`MIN_LINE_WIDTH`] to [`MAX_LINE_WIDTH`].
    pub line_width: usize,
    /// If `true`, a struct that fits within the line width is printed on one line.
    pub inline_structs: bool,
    /// If `true`, a list that fits within the line width is printed on one line.
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

/// The serialized form of [`TextLayout`] in the config file. A missing key uses its default
/// value.
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

/// Options for reading ritobin text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadOptions {
    /// If `true`, text with problems is still converted. The problems are reported as
    /// warnings. The parts of the text that a problem refers to may be missing from the
    /// document.
    pub lenient: bool,
}

/// A bin document and the format it was read from.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub file: BinFile,
    pub format: Format,
}

impl Document {
    /// Reads the document at `path`, or from standard input if `path` is [`STDIO`]. The format
    /// is detected from the content. The file extension is not used.
    pub fn read(path: &Utf8Path, options: ReadOptions) -> Result<Self> {
        let data = read_bytes(path)?;
        Self::parse(path.as_str(), data, options)
    }

    /// Parses a document from `data`. `name` is the file name shown in diagnostics.
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
            // `Format::detect` returns `Bin` or `Rito`. Data without a bin magic is read as
            // ritobin text.
            Format::Rito | Format::Yaml | Format::Json => {
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

/// Returns the format of the file at `path`: `Yaml` or `Json` for a file with that extension,
/// otherwise the format detected from its first 4 bytes.
pub fn detect_file(path: &Utf8Path) -> Result<Format> {
    if let Some(format @ (Format::Yaml | Format::Json)) =
        path.extension().and_then(Format::from_extension)
    {
        return Ok(format);
    }
    let mut magic = Vec::with_capacity(4);
    std::fs::File::open(path)
        .and_then(|file| file.take(4).read_to_end(&mut magic))
        .into_diagnostic()
        .map_err(|error| error.wrap_err(format!("Failed to read {path}")))?;
    Ok(Format::detect(&magic))
}

/// Returns `true` if the file at `path` starts with a ritobin text header: `#PROP_text`, or
/// `#PTCH_text` for a patch. A leading UTF-8 byte order mark is skipped.
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

/// Returns the format that a directory scan assigns to the file at `path`, from its file
/// extension. Returns `None` for an unknown extension.
///
/// `.py` is also the extension of Python source files. The C++ ritobin treats a `.py` file as
/// ritobin text only if it starts with the ritobin header, and this function does the same. It
/// returns `None` for a `.py` file without the header. A `.py` file that cannot be read is
/// treated as ritobin text, so that the caller reports the read error.
pub fn scanned_format(path: &Utf8Path) -> Option<Format> {
    let extension = path.extension()?;
    let format = Format::from_extension(extension)?;
    if extension.eq_ignore_ascii_case("py") && !has_text_header(path).unwrap_or(true) {
        tracing::debug!("Skipped {path}: the file does not start with a ritobin text header");
        return None;
    }
    Some(format)
}

/// Reads all bytes of the file at `path`, or of standard input if `path` is [`STDIO`].
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

/// Writes `data` to the file at `path`, or to standard output if `path` is [`STDIO`]. Creates
/// missing parent directories.
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
        Format::Yaml => declaration::to_yaml(file, hashes).map(String::into_bytes),
        Format::Json => declaration::to_json(file, hashes).map(String::into_bytes),
    }
}

/// Encodes `file` in the binary format: `PROP` for a bin, `PTCH` for a patch.
pub fn to_bin(file: &BinFile) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    file.to_writer(&mut out)
        .into_diagnostic()
        .map_err(|error| error.wrap_err("Failed to encode the bin file"))?;
    Ok(out.into_inner())
}

/// Prints `file` as ritobin text. A hash that `hashes` resolves is printed as its name.
pub fn to_text(
    file: &BinFile,
    layout: TextLayout,
    hashes: &BinHashes,
) -> Result<String, PrintError> {
    // `Print::print_with_config` ignores the wrap settings of its config. The CST is built and
    // printed directly so that the settings are applied.
    let builder = CstBuilder::new().with_hashes(hashes.clone());
    let (cst, source) = match file {
        BinFile::Prop(bin) => builder.build(bin),
        BinFile::Override(patch) => builder.build_override(patch),
    };
    // The printer ignores `indent_size` and always indents by `PRINTER_INDENT`. `reindent`
    // converts the output to the requested indent size.
    let config = PrintConfig::from(TextLayout {
        indent_size: PRINTER_INDENT,
        line_width: layout.line_width.clamp(MIN_LINE_WIDTH, MAX_LINE_WIDTH),
        ..layout
    });
    let mut text = String::new();
    CstPrinter::new(&source, &mut text, config).print(&cst)?;

    // The tree that is built from a bin prints with two layout defects: a trailing comma in a
    // list on one line, and a closing brace of a list of structs that is indented one level too
    // deep. The tree that is parsed from that text prints without them, and it is the tree that
    // `format_text` prints. The second pass therefore gives `convert` and `format` the same
    // layout.
    let parsed = Cst::parse(&text);
    if parsed.errors.is_empty() {
        let mut normalized = String::new();
        CstPrinter::new(&text, &mut normalized, config).print(&parsed)?;
        text = normalized;
    }

    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(reindent(text, layout.indent_size))
}

/// Returns `true` if `text` parses without problems and the parsed document encodes to the same
/// bytes as `file`.
///
/// The printer writes some values in a form that the parser does not read back identically.
/// `convert` and `diff` use this function to verify text printed from a bin, and warn on a
/// mismatch.
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

/// The indent size the printer always uses. The printer ignores the indent size of its config.
const PRINTER_INDENT: usize = 4;

/// The minimum line width passed to the printer. With a smaller width the printer breaks a line
/// inside a type, and the output does not parse.
pub const MIN_LINE_WIDTH: usize = 40;

/// The maximum line width passed to the printer. With a much larger width the printer can
/// panic.
pub const MAX_LINE_WIDTH: usize = 200;

/// Converts the indentation of every line from [`PRINTER_INDENT`] spaces per level to
/// `indent_size` spaces per level.
///
/// The printer escapes line breaks inside strings, so the leading spaces of a line are always
/// indentation.
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

/// Returns the default output path of a conversion: `input` with the file extension of the
/// target format.
pub fn converted_path(input: &Utf8Path, to: Format, text_extension: &str) -> Utf8PathBuf {
    input.with_extension(to.extension(text_extension))
}

/// One problem in a ritobin text file, with the span of the text it refers to.
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

/// The problems found in one ritobin text file.
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

/// Parses ritobin text into a bin document. Fails if the text has a syntax error or a build
/// diagnostic other than a shadowed entry. If `options.lenient` is set, these problems are
/// printed as warnings and the parse succeeds.
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
        // A shadowed entry is a duplicate key. The later entry replaces the earlier one and the
        // document is complete, so it is a warning.
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
        Some("pass --lenient to skip the invalid parts and convert the rest".to_owned()),
    )
    .into())
}

/// Formats ritobin text: prints the syntax tree of `text` with `layout`. Comments are kept, and
/// names and values are printed as they are written in `text`. `name` is the file name shown in
/// diagnostics.
///
/// Fails if the text has a syntax error or a build diagnostic other than a shadowed entry. Fails
/// if the formatted text does not parse to the same document as `text`.
pub fn format_text(name: &str, text: &str, layout: TextLayout) -> Result<String> {
    let cst = Cst::parse(text);
    let (file, diagnostics) = cst.build(text);
    let errors: Vec<TextProblem> = cst
        .errors
        .iter()
        .map(|error| TextProblem::new(error, error.span))
        .chain(
            diagnostics
                .iter()
                .filter(|diagnostic| {
                    !matches!(diagnostic.diagnostic, Diagnostic::ShadowedEntry { .. })
                })
                .map(|diagnostic| TextProblem::new(diagnostic.diagnostic, diagnostic.span)),
        )
        .collect();
    if !errors.is_empty() {
        return Err(problems(name, text, errors, None).into());
    }

    let config = PrintConfig::from(TextLayout {
        indent_size: PRINTER_INDENT,
        line_width: layout.line_width.clamp(MIN_LINE_WIDTH, MAX_LINE_WIDTH),
        ..layout
    });
    let mut formatted = String::new();
    CstPrinter::new(text, &mut formatted, config)
        .print(&cst)
        .into_diagnostic()?;
    if !formatted.ends_with('\n') {
        formatted.push('\n');
    }
    let formatted = reindent(formatted, layout.indent_size);

    if !reads_back(&file, &formatted) {
        miette::bail!(
            "The formatted text of {name} does not parse to the same document as the input. The file is not changed. Report this as a defect of the formatter"
        );
    }
    Ok(formatted)
}

/// Builds the diagnostic for the problems `all` of the file `name`. Shows at most
/// [`MAX_SHOWN_PROBLEMS`] of them.
fn problems(
    name: &str,
    text: &str,
    mut all: Vec<TextProblem>,
    help: Option<String>,
) -> TextProblems {
    let count = all.len();
    all.truncate(MAX_SHOWN_PROBLEMS);
    let help = match (count > all.len(), help) {
        (true, Some(help)) => Some(format!(
            "only the first {MAX_SHOWN_PROBLEMS} problems are shown; {help}"
        )),
        (true, None) => Some(format!(
            "only the first {MAX_SHOWN_PROBLEMS} problems are shown"
        )),
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

/// Sets the severity of a report to warning.
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
    fn prop_bin_round_trips_in_both_formats() {
        let file = sample();
        assert_eq!(roundtrip(&file, Format::Bin), file);
        assert_eq!(roundtrip(&file, Format::Rito), file);
    }

    #[test]
    fn patch_bin_round_trips_in_both_formats() {
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
    fn to_text_applies_indent_size_and_inline_structs() {
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
    fn reindent_changes_only_leading_spaces() {
        let text = "a {\n    b: string = \"x    y\"\n        c\n}\n".to_owned();
        assert_eq!(
            reindent(text.clone(), 2),
            "a {\n  b: string = \"x    y\"\n    c\n}\n"
        );
        assert_eq!(reindent(text.clone(), 4), text);
    }

    const VALID_TEXT: &str = "#PROP_text\ntype: string = \"PROP\"\nversion: u32 = 3\nlinked: list[string] = { }\nentries: map[hash, embed] = {\n    0x1 = 0x2 {\n        0x10: u32 = 42\n    }\n}\n";

    #[test]
    fn parse_fails_on_syntax_error() {
        Document::parse("valid.rito", VALID_TEXT.into(), ReadOptions::default()).unwrap();

        let broken = VALID_TEXT.replace("= 42", "= 4!!2");
        let error =
            Document::parse("broken.rito", broken.into(), ReadOptions::default()).unwrap_err();
        assert!(error.to_string().contains("broken.rito has"));
    }

    #[test]
    fn parse_skips_byte_order_mark() {
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
    fn reads_back_accepts_printed_text_and_rejects_changed_text() {
        let file = sample();
        let text = to_text(&file, TextLayout::default(), &BinHashes::none()).unwrap();
        assert!(reads_back(&file, &text));
        assert!(!reads_back(&file, &text.replace("= 42", "= 43")));
        assert!(!reads_back(&file, "not ritobin"));
    }

    #[test]
    fn to_text_prints_link_and_hash_with_names_from_both_tables() {
        let dir = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(
            dir.join("hashes.binentries.txt"),
            "00000001 Entries/One\n00000003 Entries/Three\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("hashes.binhashes.txt"),
            "00000002 HashesTwo\n00000003 HashesThree\n",
        )
        .unwrap();
        let hashes = BinHashes::load(None, Some(dir));

        let file: BinFile = Bin::builder()
            .object(
                BinObject::builder(0x1111_0001u32, 0xaaaa_0001u32)
                    .property(0x10u32, values::ObjectLink::new(1))
                    .property(0x11u32, values::ObjectLink::new(2))
                    .property(0x12u32, values::ObjectLink::new(3))
                    .property(0x13u32, values::Hash::new(1))
                    .property(0x14u32, values::Hash::new(2))
                    .property(0x15u32, values::Hash::new(3))
                    .build(),
            )
            .build()
            .into();
        let text = to_text(&file, TextLayout::default(), &hashes).unwrap();
        for line in [
            r#"0x10: link = "Entries/One""#,
            r#"0x11: link = "HashesTwo""#,
            r#"0x12: link = "Entries/Three""#,
            r#"0x13: hash = "Entries/One""#,
            r#"0x14: hash = "HashesTwo""#,
            r#"0x15: hash = "HashesThree""#,
        ] {
            assert!(text.contains(line), "{line} is not in:\n{text}");
        }
    }

    #[test]
    fn to_text_clamps_line_width() {
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
    fn detect_file_uses_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("misnamed.rito")).unwrap();

        std::fs::write(&path, to_bin(&sample()).unwrap()).unwrap();
        assert_eq!(detect_file(&path).unwrap(), Format::Bin);

        std::fs::write(&path, "x").unwrap();
        assert_eq!(detect_file(&path).unwrap(), Format::Rito);
    }

    #[test]
    fn scanned_format_requires_text_header_for_py_file() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let file = |name: &str, content: &str| {
            let path = dir.join(name);
            std::fs::write(&path, content).unwrap();
            path
        };

        assert_eq!(scanned_format(&file("a.bin", "")), Some(Format::Bin));
        assert_eq!(scanned_format(&file("a.rito", "")), Some(Format::Rito));
        assert_eq!(
            scanned_format(&file("a.py", "#PROP_text\n")),
            Some(Format::Rito)
        );
        assert_eq!(scanned_format(&file("b.PY", "print('hi')\n")), None);
        assert_eq!(scanned_format(&file("a.json", "{}")), Some(Format::Json));
        assert_eq!(scanned_format(&file("a.yml", "{}")), Some(Format::Yaml));
        assert_eq!(scanned_format(&file("a.txt", "")), None);
        assert_eq!(scanned_format(&dir.join("no-extension")), None);
        // A `.py` file that cannot be read is treated as ritobin text.
        assert_eq!(scanned_format(&dir.join("missing.py")), Some(Format::Rito));
    }

    #[test]
    fn lenient_parse_keeps_valid_properties() {
        let text = "#PROP_text\ntype: string = \"PROP\"\nversion: u32 = 3\nlinked: list[string] = { }\nentries: map[hash, embed] = {\n    0x1 = 0x2 {\n        0x10: u32 = \"oops\"\n        0x11: u32 = 7\n    }\n}\n";
        assert!(Document::parse("a.rito", text.into(), ReadOptions::default()).is_err());

        let document =
            Document::parse("a.rito", text.into(), ReadOptions { lenient: true }).unwrap();
        let object = &document.file.objects()[&BinHash(1)];
        assert!(object.contains_property(BinHash(0x11)));
    }

    #[test]
    fn from_extension_maps_known_extensions() {
        assert_eq!(Format::from_extension("bin"), Some(Format::Bin));
        assert_eq!(Format::from_extension("RITO"), Some(Format::Rito));
        assert_eq!(Format::from_extension("py"), Some(Format::Rito));
        assert_eq!(Format::from_extension("yaml"), Some(Format::Yaml));
        assert_eq!(Format::from_extension("YML"), Some(Format::Yaml));
        assert_eq!(Format::from_extension("json"), Some(Format::Json));
        assert_eq!(Format::from_extension("txt"), None);
    }

    #[test]
    fn opposite_is_text_for_bin_and_bin_for_every_text_format() {
        assert_eq!(Format::Bin.opposite(), Format::Rito);
        for format in [Format::Rito, Format::Yaml, Format::Json] {
            assert_eq!(format.opposite(), Format::Bin);
            assert!(format.is_text());
        }
        assert!(!Format::Bin.is_text());
        assert_eq!(Format::Yaml.extension("rito"), "yaml");
        assert_eq!(Format::Rito.extension("py"), "py");
    }

    #[test]
    fn converted_path_replaces_extension() {
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
