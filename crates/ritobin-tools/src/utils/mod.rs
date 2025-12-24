pub mod config;

use camino::Utf8Path;
use fancy_regex::Regex;
use miette::Result;

/// Format a path as a clickable hyperlink using OSC 8 escape sequences.
/// Supported by modern terminals like Windows Terminal, iTerm2, VS Code terminal, etc.
pub fn hyperlink_path(path: impl AsRef<Utf8Path>) -> String {
    let path = path.as_ref();
    let url = format!("file://{}", path.as_str().replace('\\', "/"));
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, path)
}

/// Creates a filter pattern from an optional regex string.
/// Defaults to case-insensitive matching unless the user explicitly sets (?i) or (?-i).
pub fn create_filter_pattern(pattern: Option<String>) -> Result<Option<Regex>> {
    match pattern {
        Some(mut p) => {
            // Default to case-insensitive unless the user explicitly sets (?i) or (?-i)
            let has_inline_flag = p.contains("(?i)") || p.contains("(?-i)");
            if !has_inline_flag {
                p = format!("(?i){p}");
            }
            Ok(Some(Regex::new(&p).map_err(|e| {
                miette::miette!("Failed to create filter pattern: {}", e)
            })?))
        }
        None => Ok(None),
    }
}

pub fn format_chunk_path_hash(path_hash: u64) -> String {
    format!("{:016x}", path_hash)
}

pub fn is_hex_chunk_path(path: &Utf8Path) -> bool {
    let file_name = path.file_name().unwrap_or("");
    file_name.len() == 16 && file_name.chars().all(|c| c.is_ascii_hexdigit())
}

/// Truncates a string in the middle
pub fn truncate_middle(input: &str, max_len: usize) -> String {
    if input.len() <= max_len {
        return input.to_string();
    }
    if max_len <= 3 {
        return "...".to_string();
    }
    let keep = max_len - 3;
    let left = keep / 2;
    let right = keep - left;
    let mut left_iter = input.chars();
    let mut left_str = String::with_capacity(left);
    for _ in 0..left {
        if let Some(c) = left_iter.next() {
            left_str.push(c);
        }
    }
    let mut right_iter = input.chars().rev();
    let mut right_str = String::with_capacity(right);
    for _ in 0..right {
        if let Some(c) = right_iter.next() {
            right_str.push(c);
        }
    }
    right_str = right_str.chars().rev().collect();
    format!("{}...{}", left_str, right_str)
}
