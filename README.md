# ritobin-tools

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

The LeagueToolkit command line tool for League of Legends `.bin` files. It converts between the binary format and ritobin text (`.rito`), shows the difference between two bins, saves that difference as a `PTCH` patch, and manages the hashtables that give hashes their names.

## Features

- **Convert** `.bin` to `.rito` and back, for both `PROP` bins and `PTCH` patch bins
- **Diff** two bins as a line diff, a per-object summary, JSON, JSON Lines or CSV
- **Patch**: save a diff as a `PTCH` bin or as `PTCH` text
- **Hashtables** from the shared [Mimir](https://github.com/LeagueToolkit/mimir) cache: sync, check, look up, search and export
- **Game-data declarations**: validate a manifest of bin edits, apply it to the installed game's bins, and print bin values as manifest YAML
- **Batch** conversion of directories, and `-` for standard input and output
- **Windows Explorer** right-click menu, and files dropped on the executable
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
- Add to your PATH automatically
- Ask whether to add the [Explorer right-click menu](#shell)

To answer that question ahead of time, download the script and pass `-ShellIntegration` or `-NoShellIntegration`.

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

# Binary to text, next to the input
ritobin-tools convert skin0.bin
# Creates skin0.rito

# Text to binary
ritobin-tools convert skin0.rito
# Creates skin0.bin
```

On Windows you can also drop files on `ritobin-tools.exe`. Each one is converted next to itself, and the window stays open when a conversion fails. For a right-click menu in Explorer, see [shell](#shell).

## Usage

```bash
ritobin-tools [GLOBAL OPTIONS] <COMMAND> [OPTIONS]
```

Global options:

- `-L, --verbosity <LEVEL>`: `error`, `warning`, `info` (default), `debug` or `trace`
- `--config <FILE>`: config file to use instead of `ritobin-tools.toml` next to the executable
- `--hashtable-dir <DIR>`: hashtable cache directory to use instead of the shared one
- `-H, --hashtable <DIR>`: directory of extra CDragon text hashtables, see [Hashtables](#hashtables)

Log messages go to standard error. Standard output carries only what a command was asked to print, so it is safe to pipe or redirect.

### convert

Converts between binary `.bin` and ritobin text. The format of an input is told by its content, not by its extension. Both kinds of bin are supported: a `PROP` bin holds objects, and a `PTCH` bin holds a patch over another bin.

Common flags:

- `[INPUTS]...` or `-i, --input <PATH>...`: files or directories. `-` reads standard input
- `-o, --output <PATH>`: a file for a file input, a directory for a directory input, `-` for standard output. Needs a single input
- `-r, --recursive`: include the subdirectories of a directory input
- `-t, --to <bin|rito>`: the format to write. Defaults to the opposite of the input, or to the format the output extension names
- `--from <bin|rito>`: which files a directory input is scanned for. Defaults to the opposite of `--to`, or to `bin`
- `--ext <EXT>`: extension for text output (default `rito`)
- `-k, --keep-hashed`: leave hashes as hex
- `--lenient`: convert text that has problems, leaving out what cannot be read
- `--skip-existing`: do not overwrite an output file that exists
- `--no-verify`: skip the read-back check of printed text, see below
- `--indent-size <N>`, `--line-width <N>` (40 to 200), `--inline-structs[=BOOL]`: text layout for this run

Basic examples:

```bash
# Choose the output path
ritobin-tools convert skin0.bin -o out/skin0.rito

# Convert every .bin in a directory tree to text
ritobin-tools convert ./data -r

# Convert every text file in a tree back to .bin, into another directory
ritobin-tools convert ./data -r --to bin -o ./build

# Write the legacy .py extension, with 2-space indentation
ritobin-tools convert skin0.bin --ext py --indent-size 2

# Read standard input, write standard output
ritobin-tools convert - < skin0.bin > skin0.rito
```

A directory scan reads `.bin` as binary and `.rito`, `.ritobin` and `.py` as text. It converts one direction per run, so a folder that holds both `skin0.bin` and `skin0.rito` is never converted over itself. A `.py` file is taken only when it starts with the `#PROP_text` or `#PTCH_text` line, so Python source in the same folder is left alone.

Text is checked before it is converted. A syntax or type error stops the conversion and is shown with the line it is on. A batch run converts the files that are valid, reports the others, and exits with 1.

Text printed from a bin is read back and compared with the bin before it is written. The tool warns when the two are not the same, which happens for a few values the text printer does not keep as they are, such as a string that starts or ends with a space. `--no-verify` skips the check.

No run writes over one of its own inputs. Giving `skin0.bin` and `skin0.rito` together is refused, because each would replace the other. So is anything that would write a file over itself: rewriting a file in its own format (`--to bin` on a bin) needs `-o`. Such a rewrite goes through the binary model, so the comments of a text file are not kept.

### diff

Shows what differs between two bins. Each side can be a `.bin` or a text file.

```bash
ritobin-tools diff <BASE> <EDITED> [OPTIONS]
```

Common flags:

- `-f, --format <FORMAT>`: how to print the difference, see below. Defaults to the format the `-o` extension names, or to `unified`
- `-o, --output <FILE>`: write the difference to a file
- `-p, --patch <FILE>`: also save the difference as a `PTCH` patch
- `--deletions`: put objects that are only in `BASE` on the patch's delete list
- `-C, --context <LINES>`: context lines for `unified` (default 3)
- `--exit-code`: exit with 1 when the bins differ, 0 when they do not, and 2 when the diff fails
- `--no-color`, `-k, --keep-hashed`, `--lenient`, and the text layout flags of `convert`

Formats:

| Format | Content |
| --- | --- |
| `unified` | A line diff of the two bins printed as ritobin text |
| `summary` | The changed values, grouped by object |
| `json` | One document: counts, every change, and what the patch holds |
| `jsonl` | One JSON object per change, one per line |
| `csv` | One row per change |
| `rito` | The `PTCH` patch as ritobin text |

`unified` compares the text, so it also shows objects, properties or map entries that only changed places. The other formats compare the values and ignore their order.

Every format except `unified` and `rito` lists changes. A change has these fields:

| Field | Meaning |
| --- | --- |
| `kind` | `changed`, `added`, `removed`, `object_added`, `object_removed`, `object_replaced`, `dependency_added` or `dependency_removed` |
| `object` | Path hash of the object, as `0x` hex |
| `object_name` | Path of the object, when the hashtables have it |
| `class` | Class of the object, as a name or `0x` hex |
| `path` | Where the value is inside the object, for example `Position.UIRect.Size`, `Elements[3]` or `Lookup{"weapon"}` |
| `type` | Ritobin type of the value, or `old -> new` when the type changed |
| `old`, `new` | The value in `BASE` and in `EDITED`, as ritobin text |

Basic examples:

```bash
# Line diff
ritobin-tools diff old.bin new.bin

# Per-object summary
ritobin-tools diff old.bin new.bin -f summary

# Machine-readable, written to a file. The extension picks the format
ritobin-tools diff old.bin new.bin -o changes.json

# Use in a script
ritobin-tools diff old.bin new.bin -f jsonl --exit-code > changes.jsonl
```

#### Saving a diff as a patch

`--patch` writes the difference as a `PTCH` file: binary when the path ends in `.bin`, text when it ends in `.rito`.

```bash
ritobin-tools diff base.bin edited.bin --patch edited.ptch.bin
```

A patch is a list of records, each setting one property of one object, plus whole objects and a delete list. A few things follow from that format:

- A record names its property by path, so the patch needs the hashtables. A field with no known name cannot be addressed, and the object it is in goes into the patch whole.
- A patch cannot remove a property or a map entry. It removes an object only with `--deletions`.
- A patch cannot add a single map entry. The whole map goes into one record.
- A list that changed goes into one record whole.

The tool warns when any of these applies. The `json` format lists every such place under `patch.lifted`, and `patch.exact` says whether the patch turns `BASE` into exactly `EDITED`.

Only two `PROP` bins have a structural difference. When either side is itself a `PTCH` file, use the `unified` format.

### hashes

Bins store names as hashes. The hashtables map those hashes back to names. ritobin-tools reads them from the Mimir cache that every LeagueToolkit tool shares.

```bash
# Download or update the tables (alias: ritobin-tools download-hashes, or dl)
ritobin-tools hashes sync

# See whether a newer release exists, without downloading
ritobin-tools hashes check

# List what is installed
ritobin-tools hashes status

# Print the cache directory (alias: ritobin-tools hashtable-dir, or hd)
ritobin-tools hashes dir

# Resolve hashes to names
ritobin-tools hashes lookup 0x19efbfdb 9b67e9f6

# Hash names, and see which tables know them
ritobin-tools hashes hash mName SkinCharacterDataProperties

# Find names that contain a text
ritobin-tools hashes search rollover --table fields

# Write a table as a CDragon text list
ritobin-tools hashes export types -o hashes.bintypes.txt
```

`check`, `status`, `lookup`, `hash` and `search` take `-f, --format <table|json>`.

A bin uses four tables, and `--table` takes their short names:

| Table | Holds |
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

Windows only. Adds a `ritobin-tools` submenu to the Explorer right-click menu:

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

The folder entries include the subfolders. Each entry opens a console window. After a file conversion the window stays open only when the conversion failed, and after a folder conversion or a hashtable update it always waits for Enter.

The menu is installed for the current user and needs no administrator rights. It is a classic menu, so on Windows 11 it is under "Show more options". It is on every `.bin` file, because Explorer cannot tell a League bin from another file with that extension.

The entries run the executable that installed them. Run `shell install` again after moving it. `shell status` marks the entries that run another command than the current version installs as `outdated`, and takes `-f, --format <table|json>`.

## Configuration

Settings are read from `ritobin-tools.toml` next to the executable, or from the file `--config` names. The file is optional.

```toml
# Hashtable cache directory. Leave it out to use the shared cache.
hashtable_dir = "D:/hashes"

# Game directory used by the gamedata commands.
game_dir = "C:/Riot Games/League of Legends"

# How ritobin text is laid out.
[print_config]
indent_size = 4

[print_config.wrap]
line_width = 120
inline_structs = false
inline_lists = true
```

A command line flag always wins over the config file.

## Hashtables

The cache directory is chosen in this order:

1. `--hashtable-dir`
2. `hashtable_dir` in the config file
3. The `MIMIR_DIR` environment variable
4. The shared default: `%LOCALAPPDATA%\LeagueToolkit\hashes` on Windows, `~/.local/share/LeagueToolkit/hashes` on Linux, `~/Library/Application Support/LeagueToolkit/hashes` on macOS

When no tables are installed the tool still works. It warns once and prints hashes as hex.

To add names of your own, put CDragon text tables in a directory and pass it with `-H, --hashtable <DIR>`. The files are `hashes.binentries.txt`, `hashes.binfields.txt`, `hashes.binhashes.txt` and `hashes.bintypes.txt`, each with one `<hex hash> <name>` per line. Their names win over the cache.

## Development

```bash
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

[AGENTS.md](AGENTS.md) has the writing rules for comments, test names, messages and docs.

## License

Licensed under either of:

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)
