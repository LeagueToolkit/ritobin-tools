use std::io::{IsTerminal as _, Write as _};

use camino::Utf8Path;
use colored::Colorize;

/// Waits for Enter, so a console window that closes when the run ends can be read first.
pub fn wait_for_enter() {
    eprint!("\nPress Enter to exit...");
    let _ = std::io::stderr().flush();
    let _ = std::io::stdin().read_line(&mut String::new());
}

/// Format a path as a clickable hyperlink using OSC 8 escape sequences.
/// Supported by modern terminals like Windows Terminal, iTerm2, VS Code terminal, etc.
///
/// The path is returned as it is when standard error is not a terminal, since that is where
/// paths are logged.
pub fn hyperlink_path(path: impl AsRef<Utf8Path>) -> String {
    let path = path.as_ref();
    if !std::io::stderr().is_terminal() {
        return path.to_string();
    }
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", file_url(path), path)
        .blue()
        .to_string()
}

/// The `file:` URL of `path`, made absolute first: a relative path would be read as a host name.
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

/// `count` followed by `singular`, or by `singular` with an `s` when there is not exactly one.
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
    fn plural_adds_an_s_unless_there_is_exactly_one() {
        assert_eq!(plural(0, "file"), "0 files");
        assert_eq!(plural(1, "file"), "1 file");
        assert_eq!(plural(2, "file"), "2 files");
    }

    #[test]
    fn a_file_url_is_absolute_and_has_an_empty_host() {
        let url = file_url(Utf8Path::new("out/skin0.rito"));
        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.ends_with("/out/skin0.rito"), "{url}");
        assert!(!url.contains('\\'), "{url}");
    }
}
