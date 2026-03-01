# Contributing

Thanks for contributing to AniTrack.

## Local Development

Prerequisites:
- Rust toolchain (`stable`)
- `ani-cli` available on `PATH` for manual end-to-end checks

Common commands:
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked
```

Run locally:
```bash
cargo run
cargo run -- start
cargo run -- next
cargo run -- replay
cargo run -- list
cargo run -- tui
```

## Database Migration Rules

AniTrack uses `PRAGMA user_version` with forward-only migrations in `src/db.rs`.

When adding a schema change:
- Increase `SCHEMA_VERSION`.
- Add a new `match` arm in `Database::migrate()` for the next schema version.
- Apply only that step's SQL in the new arm.
- Keep prior migration arms unchanged (do not rewrite released migrations).
- Set `PRAGMA user_version = next_version` only after that migration step succeeds.
- Add/extend upgrade tests (fresh DB, legacy DB, previous-version-to-latest).

## Release Process

Before cutting a release:
- Ensure working tree is clean (`git status`).
- Ensure latest `main` CI is green.
- Ensure `Cargo.toml` and `Cargo.lock` versions match.
- Ensure `CHANGELOG.md` has a matching version section.

Required manual smoke checks:
1. `anitrack start` records a watched entry.
2. `anitrack next` updates progress after successful playback.
3. `anitrack replay` works for normal episodes.
4. `anitrack replay` on episode `0` stays on the tracked show.
5. `anitrack tui` -> `PREVIOUS` from `1` goes to `0`.
6. `anitrack tui` -> `PREVIOUS` from `0` shows no-op notice.
7. `anitrack tui` -> `SELECT` opens selection for the current tracked show.

Release steps:
1. Update version in `Cargo.toml`.
2. Update matching version section in `CHANGELOG.md`.
3. Commit and push to `main`.
4. Create and push a matching annotated tag:
```bash
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```
5. Watch GitHub Actions `Release` workflow until completion.

