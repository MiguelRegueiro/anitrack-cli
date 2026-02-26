# Release Automation

This repository uses GitHub Actions for CI and releases.

## What is automated

- `CI` workflow (`.github/workflows/ci.yml`)
  - Runs on every push and pull request
  - Checks formatting, clippy, and tests

- `Release` workflow (`.github/workflows/release.yml`)
  - Runs when you push a tag like `v0.1.3`
  - Verifies tag version matches `Cargo.toml`
  - Builds Linux x86_64 release tarball + `.sha256`
  - Creates a GitHub Release and uploads assets
  - Publishes to crates.io using OIDC trusted publishing
  - Adds AUR update values to the workflow summary

## One-time setup

1. On crates.io, add GitHub trusted publisher for:
   - owner: `MiguelRegueiro`
   - repo: `anitrack-cli`
   - workflow: `release.yml`
   - environment: `release`
2. In GitHub repo settings, create environment `release`:
   - `Settings -> Environments -> New environment`
3. (Optional but recommended) Add protection rules for `release` environment (for example, required reviewers).

## Release steps

1. Update `Cargo.toml` version.
2. Commit and push changes to `main`.
3. Create and push a matching tag:

```bash
git tag v0.1.3
git push origin v0.1.3
```

4. Open GitHub Actions and watch the `Release` workflow.
5. After it completes:
   - GitHub Release is published with the Linux artifact
   - crates.io publish is done via OIDC
   - AUR values are shown in the workflow summary

## AUR update flow

Use the values from the `AUR update values` section in the `Release` workflow summary:

- `pkgver`
- `source URL`
- `sha256sums`

Then update your AUR package repo(s), regenerate `.SRCINFO`, and push:

```bash
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO
git commit -m "release: v0.1.3"
git push
```
