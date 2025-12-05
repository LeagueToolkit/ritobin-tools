# ritobin-tools

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

A fast, modern CLI utility for working with League of Legends `.bin` files (binary property trees). Convert between binary and human-readable text formats, and diff files with ease.

## Features

- **Convert** — Transform `.bin` files to readable `.py`/`.ritobin` text format and vice versa
- **Diff** — Compare two bin files with colored unified diff output
- **Batch Processing** — Recursively convert entire directories
- **Hashtable Support** — Automatic hash resolution for readable property names
- **Cross-Platform** — Works on Windows, Linux, and macOS

## Installation

### Windows (PowerShell)

Run this command in PowerShell to install the latest release:

```powershell
irm https://raw.githubusercontent.com/LeagueToolkit/ritobin-tools/main/install.ps1 | iex
```

This will:
- Download the latest release
- Install to `%LOCALAPPDATA%\LeagueToolkit\ritobin-tools`
- Add to your PATH automatically

### From Source

Requires [Rust 1.85+](https://rustup.rs/) (edition 2024).

```bash
git clone https://github.com/LeagueToolkit/ritobin-tools.git
cd ritobin-tools
cargo build --release
```

The binary will be available at `target/release/ritobin-tools`.

## Usage

### Convert

Convert between binary `.bin` files and text `.py`/`.ritobin` formats.

```bash
# Binary to text
ritobin-tools convert input.bin
# → Creates input.py

# Text to binary
ritobin-tools convert input.py
# → Creates input.bin

# Specify output path
ritobin-tools convert input.bin -o output.py

# Convert all files in a directory
ritobin-tools convert ./data/

# Recursively convert all files
ritobin-tools convert ./data/ -r
```

### Diff

Compare two bin files and display differences in unified diff format.

```bash
# Basic diff
ritobin-tools diff old.bin new.bin

# Adjust context lines (default: 3)
ritobin-tools diff old.bin new.bin -C 5

# Disable colored output
ritobin-tools diff old.bin new.bin --no-color
```

Supports comparing any combination of `.bin`, `.py`, and `.ritobin` files.

## Configuration

A `config.toml` file is automatically created next to the executable on first run.

```toml
hashtable_dir = "/path/to/LeagueToolkit/bin_hashtables"
```

### Hashtables

Hashtables enable human-readable names for properties instead of raw hashes. By default, the tool looks for hashtables in:

```
~/Documents/LeagueToolkit/bin_hashtables/
```

You can override this with:
- The `--hashtable-dir` CLI flag
- The `hashtable_dir` setting in `config.toml`

## License

Licensed under either of:

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)
