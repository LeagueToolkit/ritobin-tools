# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
