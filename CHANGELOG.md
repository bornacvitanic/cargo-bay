# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2026-07-07

### Bug Fixes

- Fix clippy unnecessary_sort_by; add release workflow

Use sort_by_key(Reverse(mtime)) to satisfy clippy 1.96 (-D warnings).
Add a tag-triggered workflow that builds linux/windows/macos binaries and
attaches them to a GitHub release.

### Features

- Add cargo-bay: a TUI to browse, run, and rebuild Cargo workspace binaries

Discovers every runnable binary in a workspace via `cargo metadata`, launches
the newest prebuilt exe directly (or `cargo run`), tags each app fresh/stale,
offers a version picker (installed/release/debug/rebuild), and runs background
dev rebuilds with a live log console — all on a non-blocking TUI. Windowed
(Bevy) apps stream logs to a panel; terminal apps get the real TTY.

