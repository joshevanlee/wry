# Agent notes for wry (fork)

## Overview

Wry is a cross-platform WebView rendering library. This repo is a fork of `tauri-apps/wry`. Its default branch is `dev`.

## Verify

CI runs the workflows in `.github/workflows/`:

- `clippy-fmt.yml`: `cargo clippy --all-targets` and `cargo fmt --all -- --check`.
- `build.yml`: `cargo build`, `cargo test --features linux-body`, nightly `cargo miri test`, and `cargo doc` for each target.
- `audit.yml` runs on a nightly cron.

## Shipping a verified change

Policy: fleet-docs `STANDARDS.md`, "Shipping a verified change".

- Merge to this fork's `dev`, not to `tauri-apps/wry`, once CI is green.
- A merge deploys nothing. The crate is released through the covector workflows (`covector-*.yml`, `.changes/`), which are outside the ship loop.
