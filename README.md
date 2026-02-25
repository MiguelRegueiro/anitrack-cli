# AniTrack

AniTrack is a companion CLI for `ani-cli`. It adds watch-progress tracking and navigation while delegating search and playback to `ani-cli`.

## Credit
AniTrack depends on [`ani-cli`](https://github.com/pystardust/ani-cli) for anime search and streaming playback.
This project does not replace `ani-cli`; it orchestrates and extends the workflow around it.

## Requirements
- [`ani-cli`](https://github.com/pystardust/ani-cli) installed and available on your `PATH` (required)
- Rust toolchain (`cargo`) for building/running AniTrack

### Install `ani-cli`
Follow the official installation instructions in the `ani-cli` repository.

After installation, verify it is available:
```bash
ani-cli --help
```

## Commands
- `anitrack`
  - Opens the TUI (default command)
- `anitrack start`
  - Runs `ani-cli`
  - Reads `ani-cli` history before and after playback
  - Stores the latest meaningful watch change (new show ID or updated episode/title)
  - If history content is unchanged for that run, tries a short-window `ani-cli` log match to resolve the watched entry
- `anitrack next`
  - Loads the most recently seen show from AniTrack DB
  - Plays the next episode using `ani-cli -c` with a seeded temporary history entry
  - Updates DB progress only if playback exits successfully
  - Persists the final episode reached in the `ani-cli` session (including `next/replay` actions from the in-session menu)
- `anitrack replay`
  - Replays the currently stored episode for the most recently seen show
  - Persists the final episode reached in the `ani-cli` session
  - Uses a safe fallback path for episode 1
- `anitrack list`
  - Lists tracked entries ordered by most recent update
- `anitrack tui`
  - Opens an interactive terminal UI with tracked shows (latest first)
  - `Up/Down` selects show
  - `Left/Right` selects action (`Next` / `Replay`, default `Next`)
  - `s` launches search (runs `ani-cli` UI and returns to the TUI after exit)
  - Search sync uses the same detection rules as `start` (history delta first, then log fallback)
  - `d` deletes selected tracked entry (with confirmation prompt)
  - `Enter` runs the selected action for the selected show
  - `q` quits

## Usage
```bash
cargo run
cargo run -- start
cargo run -- next
cargo run -- replay
cargo run -- list
cargo run -- tui
```

## Data and Paths
- AniTrack database path:
  - `${XDG_DATA_HOME:-$HOME/.local/share}/anitrack/anitrack.db` (Linux default behavior)
- `ani-cli` history path read by AniTrack:
  - `$ANI_CLI_HIST_DIR/ani-hsts` if `ANI_CLI_HIST_DIR` is set
  - otherwise `${XDG_STATE_HOME:-$HOME/.local/state}/ani-cli/ani-hsts`

History line format expected by AniTrack:
`episode<TAB>id<TAB>title`

AniTrack also accepts space-separated history lines when tabs are not present:
`episode id title...`

## Behavior Notes
- If the database or parent directory does not exist, AniTrack creates them automatically.
- If `anitrack next` or `anitrack replay` playback fails or is interrupted, progress is not updated.
- If you navigate episodes inside `ani-cli` after playback starts (for example using its `next` option), AniTrack stores the last episode reached when the session ends successfully.
- If no prior entry exists, `next` and `replay` instruct you to run `anitrack start` first.
- TUI/start sync only records entries tied to the current run and does not backfill arbitrary old history rows, so deleted DB entries are not resurrected unless watched again.
