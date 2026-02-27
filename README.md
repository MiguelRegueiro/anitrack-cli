# anitrack

AniTrack is a companion CLI for `ani-cli`. It adds watch-progress tracking and navigation while delegating search and playback to `ani-cli`.

## Screenshot
![AniTrack TUI showing tracked entries for Attack on Titan and Death Note](screenshots/anitrack-tui.png)

## Credit
AniTrack depends on [`ani-cli`](https://github.com/pystardust/ani-cli) for anime search and streaming playback.
This project does not replace `ani-cli`; it orchestrates and extends the workflow around it.

## Installation

### Arch Linux (AUR)
Recommended (prebuilt binary, no Rust toolchain required):
```bash
paru -S anitrack-bin
```

Source build (Rust toolchain required only while building):
```bash
paru -S anitrack
```

Both AUR packages declare `ani-cli` as a dependency, so it is installed automatically if missing.
Equivalent `yay` commands also work if you use `yay` instead of `paru`.

### crates.io (any distro with Rust)
Install from crates.io:
```bash
cargo install anitrack
```

This method does not install [`ani-cli`](https://github.com/pystardust/ani-cli), so install `ani-cli` separately and ensure it is on your `PATH`.

Verify installation:
```bash
anitrack --version
```

Upgrade to the latest release (crates.io):
```bash
cargo install anitrack --force
```

### Troubleshooting (Arch)
If `paru -S anitrack` fails to build on your system, install the prebuilt package instead:
```bash
paru -S anitrack-bin
```

### Uninstall
AUR:
```bash
paru -Rns anitrack-bin
# or
paru -Rns anitrack
```

crates.io:
```bash
cargo uninstall anitrack
```

## Quick Start
```bash
anitrack
anitrack start
anitrack next
anitrack replay
anitrack list
```

## Command Reference
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

<details>
<summary><strong>Run From Source (Development)</strong></summary>

For local development, run from the repository root:

```bash
cargo run
cargo run -- start
cargo run -- next
cargo run -- replay
cargo run -- list
cargo run -- tui
```

</details>

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

## License
This project is licensed under the GNU General Public License v3.0 or later (`GPL-3.0-or-later`).
See [LICENSE](LICENSE).

## Changelog
See [CHANGELOG.md](CHANGELOG.md) for release history and notable changes.

## Maintainer Notes
Release workflow details (CI, GitHub Releases, crates.io publish, and AUR update values) are documented in [docs/release.md](docs/release.md).
