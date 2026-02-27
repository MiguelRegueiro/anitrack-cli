# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Displayed `Last Seen` timestamps now render in the user's local timezone while DB storage remains UTC.
- `anitrack list` displays local time with UTC offset; TUI displays compact local time without offset.
- Refreshed the README TUI screenshot (`screenshots/anitrack-tui.png`).
- Clarified platform support policy in README (Linux supported, macOS CI-tested with dependency caveats, Windows not officially supported).
- Refined README information architecture for end users and moved advanced usage/runtime details into `docs/usage.md`.

## [0.1.6] - 2026-02-27

### Added
- Added CI dependency-audit enforcement with `cargo audit --deny unsound`.
- Added regression tests for temp history directory cleanup and parser edge cases (escaped titles and malformed payload handling).
- Added support for overriding the `ani-cli` binary path via `ANI_TRACK_ANI_CLI_BIN` (useful for integration testing and custom installs).
- Added an initial integration test harness with fake `ani-cli` subprocess coverage for `start`, `next`, `replay`, `previous`, and `select` success/failure/no-op flows.
- Added a dedicated Linux CI integration job (`cargo test --locked integration_`) for harness scenarios.
- Added a dedicated macOS CI integration job (`cargo test --locked integration_`) for harness scenarios.

### Changed
- Upgraded terminal UI stack to `ratatui 0.30.0` and aligned `crossterm` usage to the current backend path.
- Constrained `ratatui` features to `crossterm` only, reducing unnecessary dependency surface.
- Documented explicit runtime requirements in README (`ani-cli`, `curl`, optional Linux `journalctl` enhancement).
- Replaced ad-hoc API response string scanning with structured `serde_json` decoding for search results and episode metadata.
- Added lightweight retry policy to metadata/search `curl` calls to reduce transient network failures.

### Fixed
- Fixed potential temp history directory leaks by introducing scoped temp-dir cleanup guards.
- Fixed Unix terminal/signal restoration edge cases by using scoped signal and foreground-terminal guards across interactive subprocess execution.
- Improved JSON string unescaping behavior in search-result parsing (including escaped Unicode/control sequences) for more robust API response handling.
- Improved SQLite runtime robustness by setting a connection `busy_timeout` and attempting WAL mode on database open.
- Improved interactive subprocess handling in non-TTY contexts by falling back to plain process status execution when no controlling terminal is available.

## [0.1.5] - 2026-02-27

### Added
- Added replay regression coverage for first-episode fallback behavior, including explicit planning tests that verify deterministic replay flow for episode `0`.
- Added a required manual smoke-test checklist to release docs covering `start`, `next`, `replay` (including episode `0`), TUI `PREVIOUS` edge cases, and TUI `SELECT`.

### Changed
- Clarified README behavior notes: `journalctl` log fallback is Linux-only, and non-Linux systems rely on history-based detection.
- Refactored replay execution into an explicit replay-plan step to keep fallback behavior deterministic and easier to test.

### Fixed
- Fixed replay for episode `0` / first-entry fallback so AniTrack resolves the tracked show (`-S` when available) instead of dropping into ambiguous ani-cli show selection.

## [0.1.4] - 2026-02-27

### Added
- Added `Previous` and `Select` actions to the TUI action bar, with left/right navigation and Enter execution.
- Added tracked-show episode selection support in TUI (`Select`) so ani-cli opens episode selection for the currently selected show.

### Changed
- Refactored the monolithic `src/app.rs` into focused modules under `src/app/` (`mod`, `tui`, `tracking`, `episode`, `tests`) to improve maintainability and reduce coupling.
- Improved TUI responsiveness by moving episode-list metadata fetches to a background worker and showing loading state in the Selected panel.
- Improved release workflow safety and reliability:
  - added workflow concurrency control for tag releases
  - validated `Cargo.toml`/`Cargo.lock` version alignment
  - required matching `CHANGELOG.md` version sections
  - generated GitHub Release notes from changelog content
  - gated crates.io publishing on successful validate/build/release jobs
- Expanded CI checks from Linux-only to an OS matrix (`ubuntu-latest`, `macos-latest`, `windows-latest`).
- Limited `journalctl` log-fallback probing to Linux targets; non-Linux targets now skip that path cleanly.

### Fixed
- Fixed TUI `Select` flow to target episode selection for the current tracked show instead of falling back to generic show search.
- Fixed `Previous` episode handling across edge cases (`0`, decimal labels like `15.5`, and special numbering) by using resolved episode lists when available and safer numeric fallback behavior.
- Fixed inconsistent `Previous` no-op UX by normalizing backend `no previous episode available` errors to the same `No More Episodes` info popup.
- Fixed ani-cli log-key normalization for titles with missing space before trailing episode-count parentheses (for example `Naruto(220 episodes)`).

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

[Unreleased]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.6...HEAD
[0.1.6]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/MiguelRegueiro/anitrack-cli/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/MiguelRegueiro/anitrack-cli/releases/tag/v0.1.0
