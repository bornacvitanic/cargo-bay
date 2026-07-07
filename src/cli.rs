//! Tiny hand-rolled argument parser (no clap dependency for a handful of
//! flags). Handles being invoked either directly (`cargo-bay`) or as a cargo
//! subcommand (`cargo bay`), where cargo injects `bay` as the first argument.

use std::path::PathBuf;

pub struct Config {
    /// Point cargo metadata at a specific workspace instead of the cwd.
    pub manifest_path: Option<PathBuf>,
    /// Only list members whose crate dir is under `<workspace>/<subfolder>`.
    pub subfolder: Option<String>,
    /// Only list apps whose package name contains this substring.
    pub filter: Option<String>,
    /// Prefer release binaries for the fast (Enter) launch.
    pub prefer_release: bool,
    /// Print the discovered apps and exit, instead of opening the TUI.
    pub list: bool,
}

pub enum Cli {
    Run(Config),
    Help,
    Error(String),
}

const USAGE: &str = "\
cargo-bay — browse, run, and rebuild the binaries in a Cargo workspace

USAGE:
    cargo bay [OPTIONS]          (or run the binary directly: cargo-bay [OPTIONS])

OPTIONS:
    --subfolder <DIR>     Only list members whose crate lives under <workspace>/<DIR> (e.g. apps)
    --filter <SUBSTR>     Only list apps whose package name contains <SUBSTR>
    --manifest-path <P>   Use the workspace that contains this Cargo.toml
    --release             Prefer release binaries for the fast (Enter) launch
    --list                Print the discovered apps and exit (no TUI)
    -h, --help            Show this help

TUI KEYS:
    up/down · j/k · wheel  move      Enter / click        run
    b / right-click        versions  r                    rebuild stale (background)
    l                      log       x                    cancel builds
    PgUp / PgDn            scroll    q / Esc              quit
";

pub fn usage() -> &'static str {
    USAGE
}

pub fn parse<I: Iterator<Item = String>>(mut args: I) -> Cli {
    args.next(); // executable name

    // `cargo bay ...` re-invokes us as `cargo-bay bay ...`; drop that token.
    let mut next = args.next();
    if next.as_deref() == Some("bay") {
        next = args.next();
    }

    let mut cfg = Config {
        manifest_path: None,
        subfolder: None,
        filter: None,
        prefer_release: false,
        list: false,
    };
    let value = |args: &mut I, flag: &str| args.next().ok_or(format!("{flag} needs a value"));

    let mut cur = next;
    while let Some(arg) = cur {
        match arg.as_str() {
            "-h" | "--help" => return Cli::Help,
            "--release" => cfg.prefer_release = true,
            "--list" => cfg.list = true,
            "--subfolder" => match value(&mut args, "--subfolder") {
                Ok(v) => cfg.subfolder = Some(v),
                Err(e) => return Cli::Error(e),
            },
            "--filter" => match value(&mut args, "--filter") {
                Ok(v) => cfg.filter = Some(v),
                Err(e) => return Cli::Error(e),
            },
            "--manifest-path" => match value(&mut args, "--manifest-path") {
                Ok(v) => cfg.manifest_path = Some(v.into()),
                Err(e) => return Cli::Error(e),
            },
            other => return Cli::Error(format!("unknown argument: {other}")),
        }
        cur = args.next();
    }
    Cli::Run(cfg)
}
