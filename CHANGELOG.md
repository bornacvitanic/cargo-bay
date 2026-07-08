# Changelog

All notable changes to this project will be documented in this file.

## [0.2.0] - 2026-07-08

### Features

- Add a brand header, a docked log console, and a shared theme

Add a two-line header (the freight brandmark, the workspace name + app count,
and a colour-coded status-glyph legend), dock the log console along the bottom
as a collapsible split so the app detail stays visible while builds run, and
route all colours and glyphs through a new theme module.

### Updates

- Update cargo-bay startup and run UX

Discover the workspace on a background thread behind a loading splash, so a
large `cargo metadata` no longer greets the user with a blank terminal. Rework
running so it never triggers a surprise compile: Enter runs the newest build
as-is (even when stale), `r`/`d` run release/debug directly, and the version
picker becomes a per-profile matrix where Enter runs a build as-is and `f`
force-rebuilds then runs. `R` (was `r`) now rebuilds all stale apps.

## [0.1.1] - 2026-07-07

### Documentation

- Add cliff.toml and CHANGELOG.md (git-cliff)

### Features

- Add per-app dependency-graph freshness

Use the resolved dependency graph from cargo metadata (drop --no-deps) to mark
an app stale only when a crate it transitively links has newer source than its
binary, instead of treating any member edit as affecting all apps. Falls back
to the all-members comparison if the resolve graph is unavailable.

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

