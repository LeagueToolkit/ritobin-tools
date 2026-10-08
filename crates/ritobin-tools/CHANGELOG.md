# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2](https://github.com/LeagueToolkit/ritobin-tools/compare/v0.2.1...v0.2.2) - 2026-10-08

### Fixed

- resolve hash and link values with the entry table

### Other

- update ltk_ritobin to 0.10.1 and resolve link values with the entry table first

## [0.2.1](https://github.com/LeagueToolkit/ritobin-tools/compare/v0.2.0...v0.2.1) - 2026-10-08

### Added

- write the class of an object as a YAML class tag
- write a bin as YAML or JSON and build a bin from YAML
- list the hashes of bins that no hashtable resolves
- search the bins inside WAD archives and mod packages
- search the records of PTCH files and diff two PTCH files
- add the completions command
- add the merge command
- add the format command and print the same layout from convert
- print file values as paths
- read a game bin as the input of convert
- extract game bins and read game bins as inputs of diff and patch
- add the patch command that applies PTCH files to a bin
- add the search command for names, values and references in bins

### Other

- update ltk_meta to 0.9, ltk_ritobin to 0.10, ltk_hash to 0.5 and ltk_game_data to 0.9
- compare the messages of the error chain instead of the rendered report

## [0.2.0](https://github.com/LeagueToolkit/ritobin-tools/compare/v0.1.0...v0.2.0) - 2026-10-06

### Added

- check, apply and render game-data declarations against the installed game
- add the Windows Explorer right-click menu
- [**breaking**] rebuild convert, diff and hashes on ltk_meta 0.8 and the Mimir cache
- support drag and drop conversion
- use mimir, support .rito
- update deps + migrate to latest ltk api
- use ritobin from crates

### Fixed

- refuse the same output names for an edited bin on every platform
- take a .py file in a directory scan only when it starts as ritobin text
- switch to ltk main branch
- make links blue

### Other

- rewrite comments, messages, help text and test names in literal language
- rewrite the game-data comments, messages and test names in literal language
- say what each game-data function does, verb first
- add version tags to git dependencies

## [0.1.0](https://github.com/LeagueToolkit/ritobin-tools/releases/tag/v0.1.0) - 2025-12-24

### Added

- add command to download hashtables
- add config command
- add primitive diff command
- add basic tool
