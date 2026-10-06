# ritobin-tools

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

The LeagueToolkit command line tool for League of Legends `.bin` files. It converts between the binary format and ritobin text (`.rito`), shows the difference between two bins, saves that difference as a `PTCH` patch, and manages the hashtables that resolve hashes to names.

## Features

- **Convert** `.bin` to `.rito` and back, for both `PROP` bins and `PTCH` patch bins
- **Diff** two bins as a line diff, a per-object summary, JSON, JSON Lines or CSV
- **Patch**: save a diff as a `PTCH` bin or as `PTCH` text
- **Hashtables** from the shared [Mimir](https://github.com/LeagueToolkit/mimir) cache: sync, check, look up, search and export
- **Game-data declarations**: validate a manifest of bin edits, apply it to the installed game's bins, and print bin values as manifest YAML
- **Batch** conversion of directories, and `-` for standard input and output
- **Windows Explorer** context menu, and drag-and-drop of files onto the executable
- Works on Windows, Linux and macOS

## Installation

### Windows (PowerShell)

Run this command in PowerShell to install the latest release:

```powershell
irm https://raw.githubusercontent.com/LeagueToolkit/ritobin-tools/main/install.ps1 | iex
```

This will:

- Download the latest release
- Install to `%LOCALAPPDATA%\LeagueToolkit\ritobin-tools`
- Add the install directory to your PATH
- Ask whether to add the [Explorer context menu](#shell)

To skip the prompt, download the script and run it with `-ShellIntegration` or `-NoShellIntegration`.

### From Source

Requires [Rust 1.89+](https://rustup.rs/).

```bash
git clone https://github.com/LeagueToolkit/ritobin-tools.git
cd ritobin-tools
cargo build --release
```

The binary will be available at `target/release/ritobin-tools`.

## Quick start

```bash
# Download the hashtables once, so hashes are printed as names
ritobin-tools hashes sync

# Convert binary to text. The output is written next to the input
ritobin-tools convert skin0.bin
# Creates skin0.rito

# Convert text to binary
ritobin-tools convert skin0.rito
# Creates skin0.bin
```

On Windows, you can also drag files onto `ritobin-tools.exe`. Each file is converted, and the output is written next to it. The console window stays open if a conversion fails. For a context menu in Explorer, see [shell](#shell).

## Usage

```bash
ritobin-tools [GLOBAL OPTIONS] <COMMAND> [OPTIONS]
```

Global options:

- `-L, --verbosity <LEVEL>`: `error`, `warning`, `info` (default), `debug` or `trace`
- `--config <FILE>`: path to the config file. Defaults to `ritobin-tools.toml` in the directory of the executable
- `--hashtable-dir <DIR>`: hashtable cache directory. Defaults to the shared cache directory
- `-H, --hashtable <DIR>`: directory of additional CDragon text hashtables, see [Hashtables](#hashtables)

Log messages are written to standard error. Standard output contains only the output of the command, so it can be piped or redirected.

### convert

Converts between binary `.bin` and ritobin text. The input format is detected from the file content. The file extension is not used. Both kinds of bin are supported: a `PROP` bin contains objects, and a `PTCH` bin contains a patch for another bin.

Common flags:

- `[INPUTS]...` or `-i, --input <PATH>...`: files or directories. `-` reads standard input
- `-o, --output <PATH>`: a file for a file input, a directory for a directory input, or `-` for standard output. Requires exactly one input
- `-r, --recursive`: include the subdirectories of a directory input
- `-t, --to <bin|rito>`: output format. Defaults to the format of the output file extension, or to the other format than the input
- `--from <bin|rito>`: input format that a directory input is scanned for. Defaults to the other format than `--to`, or to `bin`
- `--ext <EXT>`: file extension for text output (default `rito`)
- `-k, --keep-hashed`: write hashes as hex
- `--lenient`: convert text that has problems. The invalid parts are skipped
- `--skip-existing`: do not overwrite an existing output file
- `--no-verify`: skip the verification of printed text, see below
- `--indent-size <N>`, `--line-width <N>` (40 to 200), `--inline-structs[=BOOL]`: text layout for this run

Basic examples:

```bash
# Set the output path
ritobin-tools convert skin0.bin -o out/skin0.rito

# Convert every .bin in a directory tree to text
ritobin-tools convert ./data -r

# Convert every text file in a directory tree to .bin, into another directory
ritobin-tools convert ./data -r --to bin -o ./build

# Use the legacy .py extension and 2-space indentation
ritobin-tools convert skin0.bin --ext py --indent-size 2

# Read standard input, write standard output
ritobin-tools convert - < skin0.bin > skin0.rito
```

A directory scan selects files by extension: `.bin` for binary, and `.rito`, `.ritobin` and `.py` for text. One run converts in one direction only. In a folder that contains both `skin0.bin` and `skin0.rito`, a run therefore never overwrites one with the other. A `.py` file is included only if it starts with the `#PROP_text` or `#PTCH_text` header, so Python source files in the same folder are skipped.

Text is validated before it is converted. A syntax error or a type error fails the conversion and is shown with its source line. A run with several inputs converts the valid files, reports the invalid files, and exits with 1.

Text printed from a bin is verified: the tool parses the text again and compares the result with the bin. It logs a warning if they differ. The text printer does not print a few values exactly, for example a string with a leading or trailing space. `--no-verify` skips the verification.

A run never overwrites one of its inputs. The command fails before it writes any file if an output path equals an input path. For example, `skin0.bin` and `skin0.rito` cannot be passed together, because each would overwrite the other. Converting a file to its own format (`--to bin` on a bin) requires `-o`. Such a conversion decodes and re-encodes the file, so the comments of a text file are lost.

### diff

Shows the difference between two bins. Each input can be a `.bin` file or a ritobin text file.

```bash
ritobin-tools diff <BASE> <EDITED> [OPTIONS]
```

Common flags:

- `-f, --format <FORMAT>`: output format, see below. Defaults to the format of the `-o` file extension, or to `unified`
- `-o, --output <FILE>`: write the difference to a file
- `-p, --patch <FILE>`: also save the difference as a `PTCH` patch
- `--deletions`: add objects that exist only in `BASE` to the delete list of the patch
- `-C, --context <LINES>`: number of context lines for `unified` (default 3)
- `--exit-code`: exit with 1 if the bins differ, 0 if they are identical, and 2 if the command fails
- `--no-color`, `-k, --keep-hashed`, `--lenient`, and the text layout flags of `convert`

Formats:

| Format | Content |
| --- | --- |
| `unified` | Line diff of the two bins printed as ritobin text |
| `summary` | Changed values, grouped by object |
| `json` | One document with the summary, all changes and the patch statistics |
| `jsonl` | One JSON object per change, one object per line |
| `csv` | One row per change |
| `rito` | The `PTCH` patch as ritobin text |

`unified` compares the printed text, so it also reports objects, properties and map entries that only changed position. The other formats compare values and ignore their order.

Every format except `unified` and `rito` lists changes. A change has these fields:

| Field | Meaning |
| --- | --- |
| `kind` | `changed`, `added`, `removed`, `object_added`, `object_removed`, `object_replaced`, `dependency_added` or `dependency_removed` |
| `object` | Path hash of the object, as `0x` hex |
| `object_name` | Path of the object, if the hashtables have it |
| `class` | Class of the object, as a name or `0x` hex |
| `path` | Path of the value inside the object, for example `Position.UIRect.Size`, `Elements[3]` or `Lookup{"weapon"}` |
| `type` | Ritobin type of the value, or `old -> new` if the type changed |
| `old`, `new` | The value in `BASE` and in `EDITED`, as ritobin text |

Basic examples:

```bash
# Line diff
ritobin-tools diff old.bin new.bin

# Per-object summary
ritobin-tools diff old.bin new.bin -f summary

# Write JSON to a file. The file extension selects the format
ritobin-tools diff old.bin new.bin -o changes.json

# Use in a script
ritobin-tools diff old.bin new.bin -f jsonl --exit-code > changes.jsonl
```

#### Saving a diff as a patch

`--patch` writes the difference as a `PTCH` file. The file is text if the path has a ritobin text extension such as `.rito`, otherwise binary.

```bash
ritobin-tools diff base.bin edited.bin --patch edited.ptch.bin
```

A patch contains a list of records, a list of whole objects and a delete list. Each record sets one property of one object. This format has the following limitations:

- A record addresses its property by a path of field names, so the patch requires the hashtables. If a field has no known name, a record cannot address it, and the whole object is stored in the patch.
- A patch cannot remove a property or a map entry. It removes an object only with `--deletions`.
- A patch cannot add a single map entry. The whole map is stored in one record.
- A changed list is stored whole in one record.

The tool logs a warning if a limitation affects the patch. The `json` format lists each affected location under `patch.lifted`. `patch.exact` is `true` if applying the patch to `BASE` produces exactly `EDITED`.

The other formats and `--patch` require two `PROP` bins. If either input is a `PTCH` file, use the `unified` format.

### hashes

Bins store names as hashes. The hashtables map the hashes back to names. ritobin-tools reads the hashtables from the Mimir cache, which all LeagueToolkit tools share.

```bash
# Download or update the tables (alias: ritobin-tools download-hashes, or dl)
ritobin-tools hashes sync

# Check for a newer release. Downloads nothing
ritobin-tools hashes check

# List the installed tables
ritobin-tools hashes status

# Print the cache directory (alias: ritobin-tools hashtable-dir, or hd)
ritobin-tools hashes dir

# Resolve hashes to names
ritobin-tools hashes lookup 0x19efbfdb 9b67e9f6

# Compute the hash of names and list the tables that contain them
ritobin-tools hashes hash mName SkinCharacterDataProperties

# Search for names that contain a text
ritobin-tools hashes search rollover --table fields

# Export a table in the CDragon text format
ritobin-tools hashes export types -o hashes.bintypes.txt
```

`check`, `status`, `lookup`, `hash` and `search` accept `-f, --format <table|json>`.

A bin uses four tables. `--table` accepts their short names:

| Table | Contains |
| --- | --- |
| `entries` | Object paths |
| `fields` | Property names |
| `hashes` | Values of `hash` and `link` properties |
| `types` | Class names |

### gamedata

Validates, applies and renders [game-data declarations](https://wiki.leaguetoolkit.dev/reference/mod-packages/game-data/). A manifest (`game_data.yaml`, `.yml`, `.toml` or `.json`) lists edits to the game's bins. It is the format LeagueToolkit mods use. The command alias is `gd`.

```yaml
version: 1
modules:
  # Edits one bin
  - target: data/characters/teemo/skins/skin0.bin
    +links: [data/mods/example.bin]
    Characters/Teemo/Skins/Skin0:
      skinMeshProperties.selfIllumination: 0.25
      +skinAudioProperties.tagEventList: [Example]
  # Edits an entry in every game bin that declares it
  - entries:
      Characters/Teemo/Skins/Skin0:
        armorMaterial: Metal
```

```bash
# Validate the manifest only
ritobin-tools gamedata check ./layer --no-game

# Dry-run the manifest against the game and print the report
ritobin-tools gamedata check ./layer --game-dir "C:/Riot Games/League of Legends"

# Apply the manifest and write each modified bin at its game path under ./out
ritobin-tools gamedata apply ./layer -o ./out

# Print a game entry, or one of its values, as manifest YAML
ritobin-tools gamedata render Characters/Teemo/Skins/Skin0
ritobin-tools gamedata render Characters/Teemo/Skins/Skin0:skinMeshProperties.texture

# Print an entry from a file
ritobin-tools gamedata render Characters/Teemo/Skins/Skin0 --bin skin0.bin
```

Common flags:

- `<MANIFEST>`: path to the manifest file or to the directory that contains it. Source files and override files are resolved relative to the manifest directory
- `--game-dir <DIR>`: path to the `Game` directory of an installation, or to its parent directory. Defaults to `game_dir` from the config
- `--index-dir <DIR>`: directory for the game index cache. Defaults to a directory under the user data directory
- `-f, --format <table|json>` on `check` and `apply`: format of the report
- `-o, --output <DIR>`, `-t, --to <bin|rito>`, `--ext`, `-k` and the text layout flags on `apply`

How a manifest is applied:

- The game is never modified. `apply` writes the modified bins under `--output`. A bin with no applied edit is not written.
- The initial content of a bin is the game's copy, read from the first archive that contains it.
- Modules are applied in manifest order. When several modules target the same bin, each one is applied to the output of the previous one.
- A reference (`!ref <entry>:<path>`) is resolved against the game's copy of the entry.
- An override file is a `PTCH` file with the `.ptch` extension. `diff --patch` writes one:

```bash
ritobin-tools diff base.bin edited.bin --patch ./layer/edited.ptch
```

The report lists each targeted bin with the number of applied changes, and each edit that was skipped. `check` with a game and `apply` exit with 1 if the report contains a problem. In the `json` report, the `kind` and `reason` fields use the diagnostic codes of `ltk_game_data`, the same codes LTK Manager reports.

`entries` modules and references require the object index, which maps every bin object of the game to the chunks that declare it. The index is built on first use and rebuilt after a game patch. The build reads every bin chunk of the game and takes tens of seconds. The index is cached under `LeagueToolkit/game-index` in the user data directory.

Limitations:

- No class schema is loaded. The type of a property is taken from its existing value in the bin. An edit that adds a property missing from the bin is skipped as `untypable`. An object constructed from a class is skipped as `unknownClass`. Cloning an object is supported.
- Only one manifest is applied, and only to the game's copy of each bin. Layering several mods and reading a bin from mod content are not supported.

### config

```bash
ritobin-tools config show
ritobin-tools config set print_config.indent_size 2
ritobin-tools config set hashtable_dir "D:/hashes"
ritobin-tools config reset
```

### shell

Windows only. Adds a `ritobin-tools` submenu to the Explorer context menu:

```powershell
ritobin-tools shell install     # add the menu
ritobin-tools shell status      # show each entry and the command it runs
ritobin-tools shell uninstall   # remove the menu
```

| Right-click on | Entries |
| --- | --- |
| A `.bin` file | Convert to .rito, Update hashtables |
| A `.rito` or `.ritobin` file | Convert to .bin |
| A folder | Convert all .bin to .rito, Convert all .rito to .bin, Update hashtables |

The folder entries include subfolders. Each entry runs in a new console window. After a file conversion, the window stays open only if the conversion failed. After a folder conversion or a hashtable update, the window always waits for Enter.

The menu is installed for the current user and does not require administrator rights. It is a classic context menu, so on Windows 11 it is under "Show more options". The menu appears on every `.bin` file, because a registry menu is selected by file extension and cannot check whether a file is a League bin.

The entries run the executable that installed them. Run `shell install` again after moving the executable. `shell status` reports an entry as `outdated` if its command differs from the command the current version installs. It accepts `-f, --format <table|json>`.

## Configuration

Settings are read from `ritobin-tools.toml` in the directory of the executable, or from the file passed with `--config`. The file is optional.

```toml
# Hashtable cache directory. Omit it to use the shared cache.
hashtable_dir = "D:/hashes"

# Game directory used by the gamedata commands.
game_dir = "C:/Riot Games/League of Legends"

# Layout of printed ritobin text.
[print_config]
indent_size = 4

[print_config.wrap]
line_width = 120
inline_structs = false
inline_lists = true
```

A command line flag overrides the matching config value.

## Hashtables

The cache directory is selected in this order:

1. `--hashtable-dir`
2. `hashtable_dir` in the config file
3. The `MIMIR_DIR` environment variable
4. The shared default: `%LOCALAPPDATA%\LeagueToolkit\hashes` on Windows, `~/.local/share/LeagueToolkit/hashes` on Linux, `~/Library/Application Support/LeagueToolkit/hashes` on macOS

The tool works without installed tables. It logs a warning and prints hashes as hex.

To add your own names, put CDragon text tables in a directory and pass the directory with `-H, --hashtable <DIR>`. The file names are `hashes.binentries.txt`, `hashes.binfields.txt`, `hashes.binhashes.txt` and `hashes.bintypes.txt`. Each file has one `<hex hash> <name>` per line. A name from these files takes precedence over the cache.

## Development

```bash
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

[AGENTS.md](AGENTS.md) contains the writing rules for comments, test names, messages and docs.

## License

Licensed under either of:

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)
