# Release Automation

This repository uses GitHub Actions for CI and releases.
See [CHANGELOG.md](../CHANGELOG.md) for release notes content.

## What is automated

- `CI` workflow (`.github/workflows/ci.yml`)
  - Runs on pushes to `main` and pull requests
  - Checks formatting, clippy, and tests

- `Release` workflow (`.github/workflows/release.yml`)
  - Runs when you push a tag like `v0.1.3`
  - Verifies tag version matches `Cargo.toml`
  - Verifies `Cargo.toml` and `Cargo.lock` package versions match
  - Verifies a matching `CHANGELOG.md` section exists (for example `## [0.1.3] - YYYY-MM-DD`)
  - Builds Linux x86_64 release tarball + `.sha256`
  - Creates a GitHub Release using the matching `CHANGELOG.md` section as release notes
  - Uploads release assets
  - Publishes to crates.io using OIDC trusted publishing
  - Adds package-specific AUR update values to the workflow summary

## One-time setup

1. On crates.io, add GitHub trusted publisher for:
   - owner: `MiguelRegueiro`
   - repo: `anitrack-cli`
   - workflow: `release.yml`
   - environment: `release`
2. In GitHub repo settings, create environment `release`:
   - `Settings -> Environments -> New environment`
3. (Optional but recommended) Add protection rules for `release` environment (for example, required reviewers).

## Preflight checklist

Before cutting a release:

- Working tree is clean (`git status`).
- Latest `main` has passed CI.
- `Cargo.toml` and `Cargo.lock` are in sync for the target version.
- `CHANGELOG.md` has a matching section for the target version.
- crates.io trusted publisher setup is complete for this repository/environment.

## Release steps

1. Update `Cargo.toml` version.
2. Add/update the matching section in `CHANGELOG.md` (for example `## [0.1.3] - 2026-02-26`).
3. Ensure `Cargo.lock` is updated too (run `cargo test --all-features` locally, then commit `Cargo.lock` changes).
4. Commit and push changes to `main`.
5. Create and push an annotated matching tag:

```bash
git tag -a v0.1.3 -m "v0.1.3"
git push origin v0.1.3
```

6. Open GitHub Actions and watch the `Release` workflow.
7. After it completes:
   - GitHub Release is published with the Linux artifact
   - GitHub Release notes come from `CHANGELOG.md` for that version
   - crates.io publish is done via OIDC
   - AUR package-specific values are shown in the workflow summary

## Failure recovery

- If the workflow fails on version or changelog validation:
  - fix the version/changelog mismatch on `main`
  - delete and recreate the tag at the corrected commit
  - push the corrected tag again
- If crates.io publish fails after GitHub Release succeeds:
  - fix the publish issue, then rerun only the publish job if possible
  - if rerun is not possible, trigger a new patch release (`vX.Y.Z+1`) with the fix

## AUR update flow

Use the values from the `AUR update values` section in the `Release` workflow summary.

For `anitrack-bin`:
- `pkgver`
- `source URL` (GitHub release tarball)
- `sha256sums`

For `anitrack`:
- bump `pkgver`
- source is crates.io (`https://crates.io/api/v1/crates/anitrack/<version>/download`)
- run `updpkgsums` only if your `PKGBUILD` pins checksums

Then update your AUR package repo(s), regenerate `.SRCINFO`, and push:

```bash
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO
git commit -m "release: v0.1.3"
git push
```
