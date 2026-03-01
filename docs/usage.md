# Usage and Behavior

This document covers command-level behavior, TUI controls, runtime notes, and data paths.

## Command Reference

### `anitrack`
- Opens the TUI (default command).

### `anitrack start`
- Runs `ani-cli`.
- Reads `ani-cli` history before and after playback.
- Stores the latest meaningful watch change (new show ID or updated episode/title).
- If history content is unchanged for that run, tries a short-window `ani-cli` log match to resolve the watched entry.

### `anitrack next`
- Loads the most recently seen show from AniTrack DB.
- Plays the next episode using `ani-cli -c` with a seeded temporary history entry.
- Updates DB progress only if playback exits successfully.
- Persists the final episode reached in the `ani-cli` session (including `next/replay` actions from the in-session menu).

### `anitrack replay`
- Replays the currently stored episode for the most recently seen show.
- Persists the final episode reached in the `ani-cli` session.
- Uses a safe fallback path for episode `1`.

### `anitrack list`
- Lists tracked entries ordered by most recent update.

### `anitrack tui`
- Opens an interactive terminal UI with tracked shows (latest first).
- `Up/Down` selects show.
- `Left/Right` selects action (`Next` / `Replay` / `Previous` / `Select`, default `Next`).
- `s` launches search (runs `ani-cli` UI and returns to the TUI after exit).
- Search sync uses the same detection rules as `start` (history delta first, then log fallback).
- `d` deletes selected tracked entry (with confirmation prompt).
- `Enter` runs the selected action for the selected show (`Select` launches `ani-cli` episode selection flow).
- `q` quits.

## Data and Paths

- AniTrack database path:
  - `${XDG_DATA_HOME:-$HOME/.local/share}/anitrack/anitrack.db` (Linux default behavior)
- `ani-cli` history path read by AniTrack:
  - `$ANI_CLI_HIST_DIR/ani-hsts` if `ANI_CLI_HIST_DIR` is set
  - otherwise `${XDG_STATE_HOME:-$HOME/.local/state}/ani-cli/ani-hsts`
- `ani-cli` binary path used by AniTrack:
  - `$ANI_TRACK_ANI_CLI_BIN` if set
  - otherwise `ani-cli` from your `PATH`

History line format expected by AniTrack:
`episode<TAB>id<TAB>title`

AniTrack also accepts space-separated history lines when tabs are not present:
`episode id title...`

## Behavior Notes

- If the database or parent directory does not exist, AniTrack creates them automatically.
- AniTrack sets a short SQLite busy timeout and attempts WAL mode when opening the DB to improve resilience under brief lock contention.
- AniTrack stores timestamps in UTC and displays them in your local timezone.
- `anitrack list` includes a UTC offset (`YYYY-MM-DD HH:MM +HH:MM`), while the TUI shows compact local time (`YYYY-MM-DD HH:MM`).
- If `anitrack next` or `anitrack replay` playback fails or is interrupted, progress is not updated.
- If you navigate episodes inside `ani-cli` after playback starts (for example using its `next` option), AniTrack stores the last episode reached when the session ends successfully.
- If no prior entry exists, `next` and `replay` instruct you to run `anitrack start` first.
- TUI/start sync only records entries tied to the current run and does not backfill arbitrary old history rows, so deleted DB entries are not resurrected unless watched again.
- The `journalctl` log-fallback path is Linux-only; on non-Linux systems AniTrack skips that fallback and relies on history-based detection.
- Metadata/search API calls use short retries for transient network failures.
- AniTrack performs metadata/search HTTP requests natively and no longer requires a separate `curl` binary.
- Metadata/search lookup failures are surfaced as warnings (instead of silent fallback), including in the TUI Selected panel metadata area.
- CI runs integration-harness tests on Linux, macOS, and Windows (`integration_` test subset).

## Database Migration Workflow (Contributors)

AniTrack uses `PRAGMA user_version` with forward-only migrations in `src/db.rs`.

When adding a new schema change:
- Increase `SCHEMA_VERSION`.
- Add a new `match` arm in `Database::migrate()` for that exact next version only.
- Apply SQL for the new step in that arm (`CREATE`, `ALTER`, backfill, etc.).
- Keep prior migration arms unchanged (never rewrite old migrations in released versions).
- Let migration set `PRAGMA user_version = next_version` only after that step succeeds.
- Add/extend tests for upgrade paths (fresh DB, legacy DB, and previous-version-to-latest).
