# ritobin-tools

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

The LeagueToolkit command line tool for League of Legends `.bin` files. It converts between the binary format and ritobin text (`.rito`), shows the difference between two bins, saves that difference as a `PTCH` patch, applies `PTCH` patches to a bin, and manages the hashtables that resolve hashes to names.

## Features

- **Convert** `.bin` to `.rito` and back, for both `PROP` bins and `PTCH` patch bins
- **YAML and JSON**: write a bin without types as YAML that builds back into a bin, or as JSON for scripts
- **Format** ritobin text files in place, with comments kept
- **Diff** two bins as a line diff, a per-object summary, JSON, JSON Lines or CSV
- **Patch**: save a diff as a `PTCH` bin or as `PTCH` text, and apply `PTCH` files to a bin
- **Merge** partial bins into a base bin
- **Search** bin files or every bin of the installed game for names, values and references
- **Game bins**: extract bins of the installed game to files, and compare or patch a game bin with `game:<BIN>`
- **Hashtables** from the shared [Mimir](https://github.com/LeagueToolkit/mimir) cache: sync, check, look up, search and export, and list the hashes of bins that have no name
- **Game-data declarations**: validate a manifest of bin edits, apply it to the installed game's bins, and print bin values as manifest YAML
- **Batch** conversion of directories, and `-` for standard input and output
- **Shell completions** for bash, zsh, fish, PowerShell and elvish
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

Converts between binary `.bin` and ritobin text, and to the [YAML and JSON](#yaml-and-json) forms. The input format is detected from the file content. The file extension is used only to recognize YAML. Both kinds of bin are supported: a `PROP` bin contains objects, and a `PTCH` bin contains a patch for another bin.

Common flags:

- `[INPUTS]...` or `-i, --input <PATH>...`: files or directories. `-` reads standard input. `game:<BIN>` reads a bin of the installed game, see [Game bins as inputs](#game-bins-as-inputs)
- `-o, --output <PATH>`: a file for a file input, a directory for a directory input, or `-` for standard output. Requires exactly one input
- `-r, --recursive`: include the subdirectories of a directory input
- `-t, --to <bin|rito|yaml|json>`: output format. Defaults to the format of the output file extension. Without one, a binary bin is converted to ritobin text and every other input to a binary bin
- `--from <bin|rito|yaml>`: input format that a directory input is scanned for. Defaults to `bin` if `--to` is a text format, and to `rito` if `--to` is `bin`
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

A `hash` value occupies 4 or 8 bytes in a bin. The PBE build of patch 16.21 stores `StaticMaterialDef.name` in 8 bytes. An 8-byte hash is read from a bin and printed as `0x` and 16 hex digits. No hashtable has names for 8-byte hashes. The text parser does not read that literal as a `hash`, so text that has an 8-byte hash does not convert back to a bin. `convert` logs the warning above for such a bin. YAML writes an 8-byte hash as the same literal, and the literal builds back to an 8-byte hash if the installed game declares the property as a `hash`. `search` matches an 8-byte hash by its `0x` literal.

A run never overwrites one of its inputs. The command fails before it writes any file if an output path equals an input path. For example, `skin0.bin` and `skin0.rito` cannot be passed together, because each would overwrite the other. Converting a file to its own format (`--to bin` on a bin) requires `-o`. Such a conversion decodes and re-encodes the file, so the comments of a text file are lost.

#### YAML and JSON

`--to yaml` writes a bin as a game-data declaration, and `--to json` writes it as JSON for scripts. Both write each value without its type, so they are shorter and easier to read than ritobin text.

```bash
# Write a bin as YAML, edit it, and build it back into a bin
ritobin-tools convert skin0.bin -o skin0.yaml
ritobin-tools convert skin0.yaml -o skin0.bin

# Write a bin as JSON for a script
ritobin-tools convert skin0.bin -o skin0.json
ritobin-tools convert game:data/characters/teemo/skins/skin0.bin --to json -o - | jq '.objects[]."~class"'
```

```yaml
# ritobin-tools bin declaration
links: [DATA/Characters/Teemo/Teemo.bin]
objects:
  Characters/Teemo/Skins/Skin0: !SkinCharacterDataProperties
    championSkinId: 17000
    skinMeshProperties: !embed(SkinMeshDataProperties)
      simpleSkin: ASSETS/Characters/Teemo/Skins/Base/Teemo_Base.skn
      texture: assets/characters/teemo/skins/base/teemo_base_tx_cm.tex
      selfIllumination: 0.7
      materialOverride:
      - !embed(SkinMeshDataProperties_MaterialOverride)
        submesh: Mushroom
    armorMaterial: Flesh
```

The YAML is the body of a [game-data](#gamedata) edit: `links` is the dependency list, and `objects` has one entry per object of the bin. An object is written as its name, its class as a YAML tag (`!<class>`), and its properties. A class that no hashtable has a name for is written as `0x` hex: `!0x1234abcd`. The tag has an uppercase first letter. A class name is hashed without regard to case, so the tag is the same class as a lowercase spelling in a hashtable.

A value is written as follows:

| Type | Written as |
| --- | --- |
| A number, a boolean, a string | The value |
| `hash`, `link`, `file` | The name as a string, or `0x` hex if no hashtable has the name |
| `vec2`, `vec3`, `vec4`, `mtx44`, `rgba` | A list of numbers |
| `list`, `list2` | A list |
| `map` | A mapping |
| `option` | The value, or `null` if the option is empty. A value that is itself a list, such as a `vec3`, is written as a list with that one item |
| `embed`, `pointer` | The properties under a `!embed(<class>)` or `!pointer(<class>)` tag. A null pointer is `null` |

**Building the YAML back into a bin requires the installed game.** The YAML has no types. The tool takes the type of each property from the class schema, which it reads from the bins of the game. Pass `--game-dir` or set `game_dir` in the config. The first run after a game patch reads every bin of the game, which takes a few seconds, and caches the schema with the game index.

- A file is read as YAML if its extension is `.yaml` or `.yml`. `convert`, `diff`, `patch` and `merge` accept such a file as an input. Standard input is not read as YAML.
- A class or a property that no game bin uses has no known type. A YAML file that has one fails to build, and the error names the object and the property. Use ritobin text for such a bin.
- The types are those of the installed game version. If a property has another type in the game version of a bin, the YAML of that bin builds with the type of the installed game. For example, `StaticMaterialDef.name` is a `string` in patch 16.20 and a `hash` in the PBE build of patch 16.21.
- A bin with a map that has the same key twice cannot be written as YAML or JSON. 40 of the 40,858 bins of the game have such a map. Use ritobin text for them.
- A `PTCH` file cannot be written as YAML or JSON.
- After `convert` writes YAML, it builds the YAML back into a bin and compares it with the input, if a game directory is set. It logs a warning if they differ. `--no-verify` skips the verification.

JSON is an output format. It cannot be read back. It has `links` and `objects` like the YAML. An object or a struct is a JSON object with the property names as keys and the class under `"~class"`:

```json
{
  "links": ["DATA/Characters/Teemo/Teemo.bin"],
  "objects": {
    "Characters/Teemo/Skins/Skin0": {
      "~class": "SkinCharacterDataProperties",
      "championSkinId": 17000,
      "skinMeshProperties": {
        "~class": "SkinMeshDataProperties",
        "selfIllumination": 0.7
      }
    }
  }
}
```

### format

Formats ritobin text files. The command prints the syntax tree of the text with the layout settings, so comments are kept and names are not changed. `convert` loses the comments of a text file, because it decodes the text to a bin first. The command alias is `fmt`.

```bash
ritobin-tools format <PATHS>... [OPTIONS]
```

```bash
# Rewrite a file in place
ritobin-tools format skin0.rito

# Format every text file in a directory tree
ritobin-tools format ./data -r

# Exit with 1 if a file is not formatted. Writes no file
ritobin-tools format ./data -r --check

# Write the formatted text to another file, or to standard output
ritobin-tools format skin0.rito -o formatted.rito
ritobin-tools format - < skin0.rito
```

Flags:

- `<PATHS>...`: ritobin text files or directories. `-` reads standard input and writes standard output
- `-o, --output <FILE>`: write the formatted text to a file and keep the input unchanged. Requires exactly one input file
- `-r, --recursive`: include the subdirectories of a directory
- `--check`: write no file. Print the path of each file that is not formatted, and exit with 1 if there is one
- `--indent-size <N>`, `--line-width <N>`, `--inline-structs[=BOOL]`: text layout for this run. The defaults come from `[print_config]` in the config file

A file is rewritten only if its text changes. Text that `convert` prints is already formatted.

The command fails for a file that has a syntax error or a type error, and shows the error with its source line. It checks that the formatted text parses to the same document as the input, and it does not write the file if that check fails. A run with several files formats the valid files, reports the invalid files, and exits with 1.

### diff

Shows the difference between two bins. Each input can be a `.bin` file, a ritobin text file, or a bin of the installed game written as `game:<BIN>`, see [Game bins as inputs](#game-bins-as-inputs).

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
| `kind` | `changed`, `added`, `removed`, `object_added`, `object_removed`, `object_replaced`, `dependency_added` or `dependency_removed`. For two `PTCH` files also `record_added`, `record_removed`, `record_changed`, `deletion_added` and `deletion_removed` |
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

[patch](#patch) applies the file to a bin.

A patch contains a list of records, a list of whole objects and a delete list. Each record sets one property of one object. This format has the following limitations:

- A record addresses its property by a path of field names, so the patch requires the hashtables. If a field has no known name, a record cannot address it, and the whole object is stored in the patch.
- A patch cannot remove a property or a map entry. It removes an object only with `--deletions`.
- A patch cannot add a single map entry. The whole map is stored in one record.
- A changed list is stored whole in one record.

The tool logs a warning if a limitation affects the patch. The `json` format lists each affected location under `patch.lifted`. `patch.exact` is `true` if applying the patch to `BASE` produces exactly `EDITED`.

#### Comparing two PTCH files

Every format except `rito` also compares two `PTCH` files. The changes are:

- `deletion_added` and `deletion_removed`: an object that only one delete list has.
- The object changes of two bins, for the whole objects of the two files.
- `record_added`, `record_removed` and `record_changed`: a record is identified by its object and its property path. `path` is the property path of the record, `old` and `new` are its values, and `class` is empty, because a record does not store the class of its object.

```
+ delete "Characters/Test/Skins/Skin9"
~ "Characters/Test/Skins/Skin0" (patch records)
  ~ skinMeshProperties.selfIllumination: f32 = 0.25 -> 0.5
  + skinScale: f32 = 1.1
```

The `json` format has `patch: null` for two `PTCH` files, because no patch is generated. `--patch` and the `rito` format require two `PROP` bins. To compare a `PROP` bin with a `PTCH` file, use the `unified` format, or apply the `PTCH` file with [patch](#patch) first.

### patch

Applies one or more `PTCH` files to a bin and writes the patched bin. `BASE` and each `PTCH` file can be a `.bin` file or a ritobin text file. `BASE` can also be a bin of the installed game written as `game:<BIN>`, see [Game bins as inputs](#game-bins-as-inputs). [diff](#saving-a-diff-as-a-patch) writes a `PTCH` file.

```bash
ritobin-tools patch <BASE> <PATCHES>... [OPTIONS]
```

Common flags:

- `-o, --output <FILE>`: write the patched bin to a file. `-` writes standard output
- `--in-place`: overwrite `BASE` with the patched bin, in the format of `BASE`
- `-n, --dry-run`: print the report only. Write no file
- `--partial`: write the patched bin even if a record cannot be applied
- `-t, --to <bin|rito|yaml|json>`: output format. Defaults to the format of the `-o` file extension, or to the format of `BASE`
- `-f, --format <table|json>`: format of the report (default `table`)
- `-k, --keep-hashed`, `--lenient`, and the text layout flags of `convert`

The command requires one of `--output`, `--in-place` and `--dry-run`. `--output` cannot be the path of an input.

Basic examples:

```bash
# Apply a patch and write the result to a new file
ritobin-tools patch skin0.bin edited.ptch -o skin0.patched.bin

# Apply several patches in order and overwrite the bin
ritobin-tools patch skin0.bin first.ptch second.ptch --in-place

# Check whether a patch still applies to a bin, for example after a game update
ritobin-tools patch skin0.bin edited.ptch --dry-run

# Print the patched bin as text
ritobin-tools patch skin0.bin edited.ptch -o - --to rito
```

A `PTCH` file is applied in three steps, in the same order as in the game:

1. The objects of its delete list are removed from the bin.
2. Its whole objects are added to the bin. An object replaces the object of the bin that has the same path.
3. Its records are applied in file order. Each record sets one property of one object.

Several `PTCH` files are applied in argument order. Each one is applied to the result of the previous one. A `PTCH` file does not store dependencies, so the dependency list of `BASE` is unchanged.

The command prints a report with one row per `PTCH` file:

```
PATCH        APPLIED  INSERTED  SKIPPED  DELETED  ADDED  REPLACED
edited.ptch  3        1         0        0        1      0
```

| Column | Meaning |
| --- | --- |
| `APPLIED` | Number of records that were applied |
| `INSERTED` | Number of applied records that added a missing property |
| `SKIPPED` | Number of records that could not be applied |
| `DELETED` | Number of objects that the delete list removed from the bin |
| `ADDED` | Number of objects that were added to the bin |
| `REPLACED` | Number of objects of the bin that were replaced |

A record cannot be applied if the bin has no object with its path hash, if its property path does not resolve in the object, or if the value at the path has a different type. The report then has a second table with the position, the object, the path and the reason of each such record:

```
PATCH        RECORD  OBJECT                       PATH  REASON
edited.ptch  0       Characters/Test/Skins/Skin0  Size  the bin has no object with this path hash
```

If a record cannot be applied, the command writes no file and exits with 1. The game skips such a record and applies the other records. `--partial` does the same: the command writes the patched bin without the values of the skipped records and exits with 0.

The `json` report is one document with the fields `base`, `output`, `written` and `patches`. `written` is `true` if the run writes the patched bin. Each item of `patches` has the counts `applied` and `inserted`, the lists `deleted`, `added` and `replaced` of object path hashes, and the list `skipped`. A skipped record has the fields `record`, `object`, `object_name`, `path` and `reason`.

The report is printed to standard output. With `-o -`, standard output contains the patched bin and the report is printed to standard error.

The patched bin is encoded from the parsed `BASE`, so the comments of a text `BASE` are lost.

### merge

Merges one or more bins into a base bin and writes the merged bin. A value of an edit bin replaces the value of the base bin, and a value that only the base bin has is kept. Each input can be a `.bin` file, a ritobin text file or `game:<BIN>`.

```bash
ritobin-tools merge <BASE> <EDITS>... [OPTIONS]
```

```bash
# Merge an edit into a bin and write the result to a new file
ritobin-tools merge skin0.bin edit.rito -o merged.bin

# Merge several edits in order. A later edit replaces the values of an earlier edit
ritobin-tools merge skin0.bin first.bin second.bin --in-place

# Merge an edit into the current game bin
ritobin-tools merge game:data/characters/teemo/skins/skin0.bin edit.rito -o ./out/skin0.bin

# Print the report only
ritobin-tools merge skin0.bin edit.rito --dry-run
```

The output flags are those of [patch](#patch): one of `-o, --output <FILE>`, `--in-place` and `-n, --dry-run` is required, and `-t, --to`, `-f, --format`, `-k`, `--lenient` and the text layout flags are accepted.

An edit bin can be a partial bin that has only the objects and the properties to change. The merge follows these rules:

- An object that only the edit has is added.
- An object of both bins with the same class is merged property by property. A property that only the edit has is added.
- An object of both bins with different classes is replaced by the object of the edit.
- A struct of both bins with the same class is merged property by property, at any depth.
- A map of both bins is merged entry by entry. An entry with a new key is added.
- Any other value, including a list, is replaced by the value of the edit.
- A dependency that only the edit has is added to the dependency list.

A merge cannot remove an object, a property, a map entry or a list item.

The command prints a report with one row per edit bin:

```
EDIT       ADDED  MERGED  REPLACED  VALUES  INSERTED  KEYS  LINKS  MISMATCHED
edit.rito  1      1       0         2       1         0     1      0
```

| Column | Meaning |
| --- | --- |
| `ADDED` | Number of objects that were added |
| `MERGED` | Number of objects that were merged property by property |
| `REPLACED` | Number of objects that were replaced, because the classes differ |
| `VALUES` | Number of values that the edit replaced in merged objects |
| `INSERTED` | Number of properties that the edit added to merged objects |
| `KEYS` | Number of map entries that the edit added |
| `LINKS` | Number of dependencies that were added |
| `MISMATCHED` | Number of replaced values whose type differs from the type of the base value |

A mismatched value is replaced like any other value. The report lists each of them in a second table and the command logs a warning, because the game ignores a value whose type differs from the type of its property. The `json` report has the same data, with the object lists as path hashes.

### search

Searches bins for names, values and references. Without a path, the command searches every bin of the installed game. With paths, it searches those bin files, ritobin text files, WAD archives, mod packages and directories. The command alias is `grep`.

```bash
# Find an entry and every reference to it in the game
ritobin-tools search Characters/Teemo/Animations/Skin0 -x

# List the bins that use an asset
ritobin-tools search Teemo_Base.skn -l

# Find the objects and the nested structs of a class
ritobin-tools search SkinMeshDataProperties --in classes -x

# List every value of a property
ritobin-tools search --values --field championSkinId

# Find a number in the values of one property
ritobin-tools search 0.7 --field selfIllumination

# Search strings with a regular expression, in one archive
ritobin-tools search -e '\.skn$' --type string --wad Champions/Teemo

# Search files and directories, and print JSON Lines
ritobin-tools search mushroom ./data skin0.rito -f jsonl

# Search the bins inside a mod package or a WAD archive
ritobin-tools search Characters/Teemo/Skins/Skin0 -x my-mod.fantome Teemo.wad.client
```

The output of the `text` format is grouped by bin and by object:

```text
data/characters/teemo/skins/skin2.bin [Champions/Teemo.wad.client]
  Characters/Teemo/Skins/Skin2 : SkinCharacterDataProperties
    skinAnimationProperties.animationGraphData: link = "Characters/Teemo/Animations/Skin0"
```

A search tests five parts of a bin. `--in` selects them:

| Part | Tested |
| --- | --- |
| `entries` | The path of each object. In a `PTCH` file also the object of each record and each item of the delete list |
| `classes` | The class of each object and of each nested struct |
| `fields` | The name of each property |
| `values` | Each value without nested values, each list item, and each map key and map value |
| `dependencies` | Each item of the dependency list of the bin |

How `PATTERN` is compared:

- With text: the pattern matches a substring of a string value or of a name, without regard to case. `-x` matches the whole text. `-s` matches with regard to case. The name of a hash comes from the hashtables.
- As a hash: the pattern is hashed as a bin name and as a file path. It therefore matches an entry, a class, a property or a reference with that hash, even if no hashtable resolves the hash.
- As `0x` hex: 1 to 8 digits match the bin hash with that value, and 9 to 16 digits match the file hash with that value.
- As a number: a pattern that is a number matches the numeric values equal to it. `true` and `false` match boolean values. A vector, a matrix or a color matches if one of its components is equal.

`-e, --regex <REGEX>` searches with a regular expression instead. It is matched against the text of each item. The text of a number is its ritobin text, for example `0.7` or `{ 50, 150, 150 }`.

`--values` lists every value that passes the filters, without a pattern. With `--regex` or `--values`, all positional arguments are paths.

Filters:

- `-t, --type <TYPES>`: search only values of these ritobin types, for example `string`, `link,hash`, `file` or `f32`
- `--field <FIELD>`: search only the values of this property
- `--class <CLASS>`: search only the properties of objects and structs of this class
- `--object <ENTRY>`, `--object-class <CLASS>`: search only this object, or only objects of this class
- `--wad <TEXT>`: search only the game archives whose name contains the text
- `--bin <TEXT>`: search only the bins whose path contains the text

A filter name is hashed, so it requires no hashtable. A filter also accepts a `0x` hash.

Other flags:

- `-f, --format <text|json|jsonl>`: `json` prints one array, `jsonl` prints one object per line
- `-l, --files-with-matches`: print only the path of each bin that has a match
- `-c, --count`: print the path and the number of matches of each bin that has a match
- `-m, --limit <N>`: stop after this number of matches
- `-j, --threads <N>`: number of worker threads. Defaults to the number of processor cores
- `--game-dir <DIR>`, `--index-dir <DIR>`: as for [gamedata](#gamedata)

Fields of the `json` and `jsonl` formats:

| Field | Meaning |
| --- | --- |
| `source` | Path of the file, or path of the bin in the game |
| `archive` | Game archive that contains the bin. Missing for a file |
| `matched` | The parts that matched: `entry`, `class`, `field`, `key`, `value`, `dependency` or `deleted` |
| `object`, `object_name`, `class` | The object that contains the match, as for [diff](#diff) |
| `path` | Path of the value inside the object. `null` for a match on the object itself or on a dependency |
| `type` | Ritobin type of the value |
| `value` | The value as ritobin text. For a struct, its class |
| `count` | Item count of a list, an option or a map |
| `record` | Position of the record of a `PTCH` file that contains the match, counting from 0. `null` for any other match |

A `PTCH` file is searched in three parts: its delete list, its whole objects and its records.

- A match in a record has `record` set. `class` is `null`, because a record does not store the class of its object. `path` is the property path of the record, followed by the path inside the value of the record. The text format prints these matches under the heading `<object> : patch records`.
- A record matches `entries` if the object that it addresses matches, and `fields` if the last property name of its path matches.
- A match in the delete list has `matched: ["deleted"]` and the deleted object in `value`. The text format prints it as `deleted: hash = "<object>"`.
- `--object-class` excludes the records and the delete list, because they store no class.

The exit code is 0 if at least one match was found, 1 if nothing matched, and 2 if the command failed.

A game search reads the list of bins from the object index, see [gamedata](#gamedata). It then reads every bin and takes a few seconds. `--wad` and `--bin` reduce the bins that are read. A bin that cannot be read is skipped with a warning.

#### Searching archives and mod packages

A path can be a WAD archive (`.wad.client`, `.wad.mobile`, `.wad`), a Fantome mod (`.fantome`) or a LeagueToolkit mod package (`.modpkg`). The command searches every bin inside it. A directory scan also searches the archives and the packages in the directory.

- The `source` of a match is the path of the bin inside the package. The `archive` is the package file. For a mod package, the WAD inside the package follows, for example `my-mod.fantome/WAD/Teemo.wad.client`. A `.modpkg` layer other than `base` follows the WAD in parentheses.
- A chunk is a bin if its path ends with `.bin`. The paths of a WAD archive come from the `game` hashtable. A chunk without a known path is read, and it is searched if its data starts with a bin magic. Such a bin is named by its chunk hash.
- `--bin <TEXT>` selects the bins inside a package by their path.
- A Fantome mod is read in all its forms: a packed WAD, a directory with the name of a WAD, the `RAW` directory, and the `WAD_<layer>` directories of layers.

Limitations:

- A packed WAD inside a Fantome mod is read into memory completely.
- A game bin that declares no object is not searched.
- A file path is matched by text only if the `game` hashtable is installed. It is always matched by its hash.

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

# List the hashes of the game, or of files, that no table resolves
ritobin-tools hashes unknown -n 50
ritobin-tools hashes unknown ./mod my-mod.fantome --table fields,types
```

`check`, `status`, `lookup`, `hash`, `search` and `unknown` accept `-f, --format <table|json>`.

A bin uses four tables. `--table` accepts their short names:

| Table | Contains |
| --- | --- |
| `entries` | Object paths |
| `fields` | Property names |
| `hashes` | Values of `hash` and `link` properties |
| `types` | Class names |

#### Hashes without a name

`hashes unknown` reads bins and lists every hash that no hashtable resolves. Without a path it reads every bin of the installed game. With paths it reads those bin files, ritobin text files, WAD archives, mod packages and directories, like [search](#search).

```
TABLE    HASH        COUNT   BINS   EXAMPLE
types    0x0a0eddc9  19188   1177   data/characters/aatrox/aatrox.bin: Characters/Aatrox/Spells/AatroxBasicAttack mSpell.Cooldown
fields   0x0a3e0478  18676   1177   data/characters/aatrox/aatrox.bin: Characters/Aatrox/Spells/AatroxBasicAttack mSpell.Cooldown.0a3e0478
```

- `-t, --table <TABLES>`: list only the hashes of these tables, separated by commas: `entries`, `fields`, `hashes`, `types` and `game`
- `-n, --limit <N>`: print at most N hashes. The most frequent hashes are printed first
- `--wad <TEXT>`, `--bin <TEXT>`: read only the game archives or the bins whose name contains the text
- `--game-dir <DIR>`, `--index-dir <DIR>`: as for [gamedata](#gamedata)
- `-j, --threads <N>`: number of worker threads

`COUNT` is the number of occurrences and `BINS` is the number of bins that contain the hash. `EXAMPLE` is one occurrence: the bin, the object and the path of the value. The `json` format has these as the fields `source`, `archive`, `object`, `object_name` and `path`.

The tables of the list differ from the hashtables in two points. The value of a `link` property is listed under `entries`, and it has a name if the `entries` table or the `hashes` table has one. `game` lists the values of `file` properties, as 16 hex digits.

The property names in the records of a `PTCH` file are stored as text, so the command reads only the objects of a `PTCH` file. A name that is written in a ritobin text file is listed if no hashtable has it.

### gamedata

Reads the bins of the installed game: validates, applies and renders [game-data declarations](https://wiki.leaguetoolkit.dev/reference/mod-packages/game-data/), and [extracts bins](#extracting-game-bins). A manifest (`game_data.yaml`, `.yml`, `.toml` or `.json`) lists edits to the game's bins. It is the format LeagueToolkit mods use. The command alias is `gd`.

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

# Write a game bin to a file
ritobin-tools gamedata extract data/characters/teemo/skins/skin0.bin -o skin0.rito
```

Common flags:

- `<MANIFEST>`: path to the manifest file or to the directory that contains it. Source files and override files are resolved relative to the manifest directory
- `--game-dir <DIR>`: path to the `Game` directory of an installation, or to its parent directory. Defaults to `game_dir` from the config
- `--index-dir <DIR>`: directory for the game index cache. Defaults to a directory under the user data directory
- `-f, --format <table|json>` on `check` and `apply`: format of the report
- `-o, --output <DIR>`, `-t, --to <bin|rito|yaml|json>`, `--ext`, `-k` and the text layout flags on `apply`

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

#### Extracting game bins

`gamedata extract` writes bins of the game to files. The game is not modified.

```bash
# One bin to a file. The file extension selects the format
ritobin-tools gamedata extract data/characters/teemo/skins/skin0.bin -o skin0.bin
ritobin-tools gamedata extract data/characters/teemo/skins/skin0.bin -o skin0.rito

# The bin that declares an entry
ritobin-tools gamedata extract Characters/Teemo/Skins/Skin0 -o skin0.rito

# Every bin whose path contains a text, at its game path under ./out
ritobin-tools gamedata extract --bin characters/teemo/skins/ -d ./out

# Every bin of one archive, as text
ritobin-tools gamedata extract --wad Champions/Teemo. -d ./teemo --to rito
```

A `BINS` argument has one of these forms:

| Form | Selects |
| --- | --- |
| A path that ends with `.bin`, for example `data/characters/teemo/skins/skin0.bin` | The bin with that path |
| 16 hex digits | The bin with that chunk hash |
| An entry path, or `0x` and 8 hex digits | Every bin that declares the entry |

Flags:

- `--wad <TEXT>`: also select the bins of the archives whose name contains the text
- `--bin <TEXT>`: also select the bins whose path contains the text. With `--wad`, a bin must pass both filters
- `-o, --output <FILE>`: write one bin to a file. `-` writes standard output. The command fails if more than one bin is selected
- `-d, --output-dir <DIR>`: write each bin at its game path under a directory
- `-t, --to <bin|rito|yaml|json>`: output format. Defaults to the format of the `-o` file extension, or to `bin`
- `--skip-existing`: do not overwrite an existing output file
- `--ext`, `-k`, `--no-verify` and the text layout flags of `convert`

The command requires `--output` or `--output-dir`. A binary output file has exactly the bytes that the game stores.

`--wad`, `--bin` and an entry use the object index, so they do not select a bin that declares no object. `--bin` requires the `game` hashtable, because a bin without a known path is named by its chunk hash.

Under `--output-dir`, three kinds of bin are not written at their game path:

- A bin without a known path is written as `<chunk hash>.bin` in the output directory.
- A bin whose file name is longer than 240 bytes is written as `<chunk hash>.bin` in the directory of its game path, because file systems limit a file name to 255 bytes. The game has such bins, for example the bins that contain the shared objects of many skins of one champion.
- A bin whose game path is also the directory of other selected bins is written as `<chunk hash>.bin` in the directory of its game path, because a file and a directory cannot have the same name. The game has such bins, for example `loadouts/companions` and the bins under `loadouts/companions/`.

The command logs the number of bins of the last two kinds, and `-L debug` lists them. Text output uses the text extension in place of `.bin` in all three cases.

With `--output-dir`, a bin that cannot be read or printed does not stop the run. The command then exits with 1 after the last bin.

#### Game bins as inputs

`convert`, `diff` and `patch` accept `game:<BIN>` in place of a file path. The bin is read from the game. `<BIN>` has the forms of a `BINS` argument of `gamedata extract`, and it must select exactly one bin.

```bash
# Print a game bin as text
ritobin-tools convert game:data/characters/teemo/skins/skin0.bin -o -

# Show what an edited bin changes, compared with the game
ritobin-tools diff game:data/characters/teemo/skins/skin0.bin ./mod/skin0.bin -f summary

# Save those changes as a patch
ritobin-tools diff game:Characters/Teemo/Skins/Skin0 ./mod/skin0.bin --patch edited.ptch

# Check whether the patch applies to the current game bin, for example after a game update
ritobin-tools patch game:data/characters/teemo/skins/skin0.bin edited.ptch --dry-run

# Apply the patch to the current game bin
ritobin-tools patch game:data/characters/teemo/skins/skin0.bin edited.ptch -o ./out/skin0.bin
```

These commands accept `--game-dir` and `--index-dir`. `convert` requires `--output` for a game bin, because a game bin has no directory for a default output path.

### config

```bash
ritobin-tools config show
ritobin-tools config set print_config.indent_size 2
ritobin-tools config set hashtable_dir "D:/hashes"
ritobin-tools config reset
```

### completions

Prints a completion script for a shell. The script completes the commands, the flags and the values of flags that have a fixed set of values.

```bash
ritobin-tools completions <bash|elvish|fish|powershell|zsh>
```

Install the script for your shell:

```bash
# bash
ritobin-tools completions bash > ~/.local/share/bash-completion/completions/ritobin-tools

# zsh. The directory must be in $fpath
ritobin-tools completions zsh > ~/.zfunc/_ritobin-tools

# fish
ritobin-tools completions fish > ~/.config/fish/completions/ritobin-tools.fish
```

```powershell
# PowerShell. Add this line to the file that $PROFILE names
ritobin-tools completions powershell | Out-String | Invoke-Expression
```

Generate the script again after you update the tool, so that it has the commands and the flags of the new version.

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

# Game directory used by the commands that read the game.
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

The four bin tables resolve entry paths, class names, property names and the values of `hash` and `link` properties. The `game` table resolves the paths of `file` values and of game chunks. Printed text therefore has `texture: file = "assets/characters/teemo/skins/base/teemo_base_tx_cm.tex"` if the table has the path, and `texture: file = 0x56e8cbde20856ea` if it does not.

To add your own names, put CDragon text tables in a directory and pass the directory with `-H, --hashtable <DIR>`. The file names are `hashes.binentries.txt`, `hashes.binfields.txt`, `hashes.binhashes.txt` and `hashes.bintypes.txt`, and `hashes.game.txt` for the paths of `file` values. Each file has one `<hex hash> <name>` per line. A name from these files takes precedence over the cache.

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
