use std::io::{IsTerminal as _, Write as _};

use camino::Utf8Path;
use colored::Colorize;

/// Prints a prompt and waits for Enter. This keeps a console window open that would otherwise
/// close when the process exits.
pub fn wait_for_enter() {
    eprint!("\nPress Enter to exit...");
    let _ = std::io::stderr().flush();
    let _ = std::io::stdin().read_line(&mut String::new());
}

/// Formats `path` as a clickable hyperlink with OSC 8 escape sequences. Terminals such as
/// Windows Terminal, iTerm2 and the VS Code terminal support them.
///
/// Returns the plain path if standard error is not a terminal. Paths are printed in log
/// messages, which go to standard error.
pub fn hyperlink_path(path: impl AsRef<Utf8Path>) -> String {
    let path = path.as_ref();
    if !std::io::stderr().is_terminal() {
        return path.to_string();
    }
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", file_url(path), path)
        .blue()
        .to_string()
}

/// Returns the `file:` URL of `path`. The path is made absolute first, because the first
/// segment of a relative path would be parsed as the host name.
fn file_url(path: &Utf8Path) -> String {
    let absolute = std::path::absolute(path)
        .map(|absolute| absolute.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
        .replace('\\', "/");
    match absolute.starts_with('/') {
        true => format!("file://{absolute}"),
        false => format!("file:///{absolute}"),
    }
}

/// Returns a key that is equal for two paths to the same file: the absolute path with `/`
/// separators, lowercased on Windows.
pub fn same_file_key(path: &Utf8Path) -> String {
    let absolute = std::path::absolute(path)
        .map(|absolute| absolute.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    let key = absolute.replace('\\', "/");
    match cfg!(windows) {
        true => key.to_lowercase(),
        false => key,
    }
}

/// Formats `count` with a noun: `singular` if `count` is 1, otherwise `singular` with an `s`
/// appended.
pub fn plural(count: usize, singular: &str) -> String {
    match count {
        1 => format!("1 {singular}"),
        _ => format!("{count} {singular}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_appends_s_unless_count_is_one() {
        assert_eq!(plural(0, "file"), "0 files");
        assert_eq!(plural(1, "file"), "1 file");
        assert_eq!(plural(2, "file"), "2 files");
    }

    #[test]
    fn file_url_is_absolute_with_empty_host() {
        let url = file_url(Utf8Path::new("out/skin0.rito"));
        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.ends_with("/out/skin0.rito"), "{url}");
        assert!(!url.contains('\\'), "{url}");
    }
}
