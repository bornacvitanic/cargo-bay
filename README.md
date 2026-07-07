[![Rust](https://github.com/bornacvitanic/cargo-bay/actions/workflows/rust.yml/badge.svg)](https://github.com/bornacvitanic/cargo-bay/actions/workflows/rust.yml)
[![dependency status](https://deps.rs/repo/github/bornacvitanic/cargo-bay/status.svg)](https://deps.rs/repo/github/bornacvitanic/cargo-bay)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Crates.io](https://img.shields.io/crates/v/cargo-bay.svg)](https://crates.io/crates/cargo-bay)
[![Download](https://img.shields.io/badge/download-releases-blue.svg)](https://github.com/bornacvitanic/cargo-bay/releases)

# cargo-bay

`cargo-bay` is a terminal UI for Cargo workspaces with lots of binaries. If you keep forgetting which apps your workspace contains — or you're tired of typing `cargo run -p <name>` and waiting for a recompile — `cargo bay` gives you a live, browsable list of every runnable binary, launches the one you pick, and gets out of your way.

It discovers apps at runtime with `cargo metadata`, so a single install works in **any** workspace. It runs the already-built executable directly (no recompile) when one exists, tells you at a glance whether that binary is stale, and can rebuild everything that's out of date in the background — without freezing the UI.

<!-- Add a screenshot/GIF here, e.g. ![cargo-bay](https://user-images.githubusercontent.com/.../cargo-bay.png) -->

## Features

- **Zero-config discovery** — lists every workspace member with a binary target, plus its `description`, via `cargo metadata`. Add a crate and it just appears.
- **Fast launch** — runs the newest prebuilt exe directly (`target/debug`, `target/release`, or `~/.cargo/bin`) with no compile step. Falls back to `cargo run` when there's no binary yet.
- **Freshness at a glance** — each app is tagged `fresh` / `stale` by comparing its binary's mtime against the workspace source, so you know whether the fast launch is up to date.
- **Version picker** — pick exactly which build to run: `installed` (your published copy), `release`, `debug`, or a fresh `cargo run` — handy for comparing versions.
- **Background rebuild-all** — one key queues a dev build of every not-fresh app, running sequentially with per-app status spinners while the UI stays responsive. Cancel any time; nothing is left running when you quit.
- **Live log console** — windowed (GUI) apps run in the background with their output streamed into a togglable, scrollable log panel; build output lands there too, and it auto-opens on a build failure.
- **Windowed vs terminal aware** — apps that open their own window (e.g. Bevy) run in the background; full-screen terminal apps (e.g. ratatui) get the real terminal handed over, then return you to the bay on exit.
- **Mouse + keyboard** — arrow keys/`hjkl`, wheel scroll, and left/right click all work.

## Installation

### From crates.io (recommended)

```sh
cargo install cargo-bay
```

This installs the `cargo-bay` binary, which Cargo exposes as the subcommand `cargo bay`.

### From Source

Ensure you have Rust and Cargo installed, then:

```sh
git clone https://github.com/bornacvitanic/cargo-bay.git
cd cargo-bay
cargo install --path .
```

### From GitHub Releases

Pre-built binaries are available on the [GitHub Releases](https://github.com/bornacvitanic/cargo-bay/releases) page — download the archive for your platform and place the binary on your `PATH`.

## Usage

Run it from anywhere inside a Cargo workspace:

```sh
cargo bay
```

(or invoke the binary directly as `cargo-bay`).

### Options

```
--subfolder <DIR>     Only list members whose crate lives under <workspace>/<DIR> (e.g. apps)
--filter <SUBSTR>     Only list apps whose package name contains <SUBSTR>
--manifest-path <P>   Use the workspace that contains this Cargo.toml
--release             Prefer release binaries for the fast (Enter) launch
--list                Print the discovered apps and exit (no TUI)
-h, --help            Show help
```

For example, in a workspace that keeps its apps under `apps/`, hide library crates and tooling with:

```sh
cargo bay --subfolder apps
```

Or script over the catalogue without opening the UI:

```sh
cargo bay --list
```

### Keys

| Keys | Action |
|------|--------|
| `↑`/`↓` · `j`/`k` · wheel | move selection |
| `Enter` · left-click | run the selected app (fast) |
| `b` · right-click | open the version picker |
| `r` | rebuild every not-fresh app (dev, background) |
| `l` | toggle the log console |
| `x` | cancel running/queued builds |
| `PgUp`/`PgDn` | scroll the log |
| `q` · `Esc` | quit |

## How it works

- **Discovery** is a single `cargo metadata` call at startup: it yields the workspace root, the real target directory (honoring `CARGO_TARGET_DIR`), and every member's binary targets, description, and dependencies.
- **App kind** is inferred from dependencies: a crate depending on `bevy` is treated as *windowed* (its logs stream to a panel); everything else is *terminal* (it gets the real TTY).
- **Freshness** is a fast mtime heuristic: an app is *stale* if any `.rs`/`Cargo.toml` under the workspace's member crates is newer than its binary. It deliberately ignores the root `Cargo.lock` so an unrelated dependency bump doesn't strand an app as permanently stale.

## Limitations

- Freshness is an mtime approximation, not Cargo's content-hash fingerprint. It errs toward "stale" (safe), and currently treats an edit to *any* member crate as affecting *all* apps — narrowing this to each app's real dependency graph is on the roadmap.
- One binary target per package is listed (the first one found).

## Roadmap

- Per-app dependency-graph freshness (only mark an app stale when a crate it actually links changes).
- Optional cached index for very large workspaces.
- Optional `.cargo-bay.toml` for include/exclude curation.

## Contributing

Contributions are welcome! Please open an issue or submit a pull request.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE.md) file for details.

## Acknowledgements

- [ratatui](https://crates.io/crates/ratatui) - Terminal user interface widgets and layout.
- [crossterm](https://crates.io/crates/crossterm) - Cross-platform terminal manipulation (raw mode, alternate screen, mouse, events).
- [serde](https://crates.io/crates/serde) - Deserialization framework.
- [serde_json](https://crates.io/crates/serde_json) - Parsing the `cargo metadata` output.

## Contact

- **Email**: [borna.cvitanic@gmail.com](mailto:borna.cvitanic@gmail.com)
- **GitHub Issues**: [GitHub Issues Page](https://github.com/bornacvitanic/cargo-bay/issues)