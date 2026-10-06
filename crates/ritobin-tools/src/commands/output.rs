//! Prints the rows of list commands as a table or as JSON.

use std::fmt::Write as _;

use clap::{Args, ValueEnum};
use colored::Colorize;
use miette::{IntoDiagnostic, Result};
use serde::Serialize;

use crate::document::{STDIO, write_bytes};

/// The output format option shared by the list commands.
#[derive(Args, Debug, Clone, Copy)]
pub struct OutputArgs {
    /// Output format
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
}

/// Prints `rows` to standard output: as a JSON array, or as the text that `table` returns for
/// them.
pub fn print<T: Serialize>(
    rows: &[T],
    format: OutputFormat,
    table: impl FnOnce(&[T]) -> String,
) -> Result<()> {
    let out = match format {
        OutputFormat::Json => {
            let mut out = serde_json::to_string_pretty(rows).into_diagnostic()?;
            out.push('\n');
            out
        }
        OutputFormat::Table => table(rows),
    };
    write_bytes(STDIO.into(), out.as_bytes())
}

/// Formats `rows` as aligned columns under `header`. Each column has the width of its widest
/// cell.
pub fn columns<const N: usize>(header: [&str; N], rows: &[[String; N]]) -> String {
    let mut widths = header.map(str::len);
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let mut out = String::new();
    let mut line = |cells: [&str; N], bold: bool| {
        let mut text = String::new();
        for (index, (cell, width)) in cells.iter().zip(widths).enumerate() {
            match index + 1 == N {
                true => text.push_str(cell),
                false => {
                    let _ = write!(text, "{cell:<width$}  ");
                }
            }
        }
        let text = text.trim_end();
        let _ = match bold {
            true => writeln!(out, "{}", text.bold()),
            false => writeln!(out, "{text}"),
        };
    };
    line(header, true);
    for row in rows {
        line(std::array::from_fn(|index| row[index].as_str()), false);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_use_width_of_widest_cell() {
        colored::control::set_override(false);
        let rows = [
            [
                "0x00000001".to_owned(),
                "binfields".to_owned(),
                "mName".to_owned(),
            ],
            ["0x2".to_owned(), "-".to_owned(), "(unknown)".to_owned()],
        ];
        assert_eq!(
            columns(["HASH", "TABLE", "NAME"], &rows),
            "HASH        TABLE      NAME\n0x00000001  binfields  mName\n0x2         -          (unknown)\n"
        );
    }
}
