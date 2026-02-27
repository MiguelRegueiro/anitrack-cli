# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Hardened release validation and clarified AUR update flow in release documentation and workflow behavior.
- Release workflow now validates matching `CHANGELOG.md` sections and uses them as GitHub Release notes.

## [0.1.3] - 2026-02-26

### Added
- GitHub Actions CI workflow (`fmt`, `clippy`, `test`) on pushes and pull requests.
- GitHub Actions release workflow for tagged releases, including:
  - Linux x86_64 tarball + SHA256 artifact generation
  - GitHub Release publishing
  - crates.io publish via OIDC trusted publishing
  - AUR update values in workflow summary
- TUI screenshot asset referenced in the README.

### Changed
- Improved release workflow validation to keep tag, `Cargo.toml`, and `Cargo.lock` versions aligned.
- Aligned codebase with stricter CI checks (`rustfmt` and clippy compliance).
- Ignored generated checksum sidecar files in `.gitignore`.

## [0.1.2] - 2026-02-26

### Fixed
- Fixed `anitrack list` to display human-readable `LAST SEEN` timestamps (`YYYY-MM-DD HH:MM`) instead of raw RFC3339 values.
- Unified timestamp display behavior between TUI views and CLI list output.

### Changed
- Reworked installation docs to clearly separate:
  - Arch Linux AUR (`anitrack-bin` recommended, `anitrack` source-build option)
  - crates.io install flow
- Clarified dependency behavior:
  - AUR packages auto-install `ani-cli`
  - `cargo install anitrack` does not install `ani-cli`
- Added Arch troubleshooting guidance (`anitrack-bin` fallback when source build fails).
- Added uninstall commands for AUR and crates.io installs.
- Added TUI screenshot reference support in README.

## [0.1.1] - 2026-02-25

### Added
- Published AniTrack on crates.io.
- Added package metadata for distribution (description, repository, homepage, keywords, categories).
- Added explicit `GPL-3.0-or-later` licensing metadata for package distribution.

### Changed
- Improved TUI timestamp readability for `Last Seen`.
- Refined README for published usage:
  - crates.io install/upgrade instructions
  - clearer quick start
  - cleaner separation between normal usage and source/development usage

## [0.1.0] - 2026-02-25

### Added
- First stable release focused on reliable tracking and replay behavior in real `ani-cli` workflows.
- Added fallback matching via recent `ani-cli` logs when history content is unchanged but a watch action occurred.
- Added support for both history formats:
  - tab-separated: `episode<TAB>id<TAB>title`
  - space-separated: `episode id title...`
- Added `No More Episodes` modal when pressing `Next` on the last available episode.
- Expanded automated test coverage for sync, replay, ordinal progress, and edge cases (`0`, `13.5`, unchanged history scenarios).
- Project license declared as `GPL-3.0-or-later`.

### Fixed
- Fixed TUI search sync for difficult cases including:
  - unseen show + episode `0`
  - decimal episodes (for example `13.5`)
- Prevented stale history entries from being reinserted into the DB after delete actions.

### Changed
- Improved replay behavior to avoid unexpected search-selection UX and make replay flow more deterministic.
- Progress now uses episode position (ordinal) when available, so shows with `0`/`13.5` display correctly.
- Right-side episode text now matches progress logic.
- Polished modal styling/layout for delete confirmation and last-episode notice.

[Unreleased]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/MiguelRegueiro/anitrack-cli/releases/tag/v0.1.0
