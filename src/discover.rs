//! Runtime app discovery via `cargo metadata` — layout-agnostic, so cargo-bay
//! works in any Cargo workspace, not just one baked in at build time.
//!
//! One `cargo metadata` call yields the workspace root, the real target dir
//! (honoring CARGO_TARGET_DIR), every member package, its binary targets,
//! description, and dependencies (for the windowed/terminal split). We split
//! discovery into two steps: [`discover`] runs cargo once to build a static
//! [`Catalog`]; [`resolve`] is pure filesystem work (prebuilt lookup + mtime
//! freshness) and is cheap to re-run after every build.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use serde::Deserialize;

use crate::cli::Config;

/// A runnable app plus how the launcher will start it.
pub struct AppEntry {
    pub name: String,
    pub description: String,
    pub kind: AppKind,
    pub launch: Launch,
    pub prebuilts: Vec<Prebuilt>,
}

/// How an app uses the console.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    /// Opens its own OS window (Bevy) — stdout is log text we stream to a panel.
    Windowed,
    /// Draws a full-screen TUI or plain CLI — needs the real terminal.
    Terminal,
}

/// The default (fast) launch for an app.
pub enum Launch {
    /// A prebuilt binary exists — run it directly (instant, no compile).
    Prebuilt { path: PathBuf, freshness: Freshness },
    /// No binary yet — Enter falls back to `cargo run`.
    BuildOnly,
}

/// A concrete prebuilt binary the user can pick from the version picker.
pub struct Prebuilt {
    pub kind: BinKind,
    pub path: PathBuf,
    pub freshness: Freshness,
    pub mtime: SystemTime,
}

/// Which build of a binary a `Prebuilt` refers to.
#[derive(Clone, Copy)]
pub enum BinKind {
    /// `cargo install`ed into `~/.cargo/bin` — the "published" copy.
    Installed,
    Release,
    Debug,
}

impl BinKind {
    pub fn label(self) -> &'static str {
        match self {
            BinKind::Installed => "installed",
            BinKind::Release => "release",
            BinKind::Debug => "debug",
        }
    }
}

/// Is a prebuilt binary current with the workspace source?
#[derive(Clone, Copy)]
pub enum Freshness {
    Fresh,
    Stale,
}

/// Static, cargo-derived workspace facts. Cheap to keep and re-`resolve`.
pub struct Catalog {
    pub root: PathBuf,
    target_dir: PathBuf,
    /// Source dirs of every member (for the shared-source freshness check).
    member_dirs: Vec<PathBuf>,
    metas: Vec<AppMeta>,
    prefer_release: bool,
}

struct AppMeta {
    name: String,
    bin: String,
    description: String,
    kind: AppKind,
}

/// Why discovery couldn't proceed — each renders an actionable message.
pub enum DiscoverError {
    CargoNotFound,
    NotWorkspace(String),
    Metadata(String),
    Parse(String),
}

impl fmt::Display for DiscoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscoverError::CargoNotFound => write!(
                f,
                "cargo-bay needs Cargo, but `cargo` was not found on PATH.\n\
                 Install Rust from https://rustup.rs, then run cargo-bay inside a workspace."
            ),
            DiscoverError::NotWorkspace(msg) => write!(
                f,
                "cargo-bay must run inside a Cargo workspace.\n  cargo metadata: {msg}\n\
                 Run it from a directory with a Cargo.toml, or pass --manifest-path <path>."
            ),
            DiscoverError::Metadata(e) => write!(f, "failed to run cargo metadata: {e}"),
            DiscoverError::Parse(e) => write!(f, "failed to parse cargo metadata: {e}"),
        }
    }
}

// --- cargo metadata JSON (only the fields we use) ------------------------

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    workspace_root: String,
    target_directory: String,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    manifest_path: String,
    targets: Vec<MetaTarget>,
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct MetaTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
}

/// Run cargo once and build the static catalogue.
pub fn discover(cfg: &Config) -> Result<Catalog, DiscoverError> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--format-version", "1", "--no-deps"]);
    if let Some(mp) = &cfg.manifest_path {
        cmd.arg("--manifest-path").arg(mp);
    }
    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            DiscoverError::CargoNotFound
        } else {
            DiscoverError::Metadata(e.to_string())
        }
    })?;
    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(DiscoverError::NotWorkspace(msg));
    }
    let meta: Metadata =
        serde_json::from_slice(&output.stdout).map_err(|e| DiscoverError::Parse(e.to_string()))?;

    let members: HashSet<&str> = meta.workspace_members.iter().map(String::as_str).collect();
    let root = PathBuf::from(&meta.workspace_root);
    let target_dir = PathBuf::from(&meta.target_directory);
    let sub_root = cfg.subfolder.as_ref().map(|s| root.join(s));

    let mut member_dirs = Vec::new();
    let mut metas = Vec::new();
    for p in &meta.packages {
        if !members.contains(p.id.as_str()) {
            continue;
        }
        let dir = Path::new(&p.manifest_path)
            .parent()
            .unwrap_or(&root)
            .to_path_buf();
        // Every member dir feeds the freshness check, even lib-only ones.
        member_dirs.push(dir.clone());

        let Some(bin) = p.targets.iter().find(|t| t.kind.iter().any(|k| k == "bin")) else {
            continue; // lib-only member — not runnable
        };
        if let Some(sr) = &sub_root {
            if !dir.starts_with(sr) {
                continue;
            }
        }
        if let Some(f) = &cfg.filter {
            if !p.name.contains(f.as_str()) {
                continue;
            }
        }
        let kind = if p.dependencies.iter().any(|d| d.name == "bevy") {
            AppKind::Windowed
        } else {
            AppKind::Terminal
        };
        metas.push(AppMeta {
            name: p.name.clone(),
            bin: bin.name.clone(),
            description: p.description.clone().unwrap_or_default(),
            kind,
        });
    }
    metas.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Catalog {
        root,
        target_dir,
        member_dirs,
        metas,
        prefer_release: cfg.prefer_release,
    })
}

/// Filesystem-only pass: resolve each app's prebuilt binaries and freshness.
/// Safe and cheap to call after every build.
pub fn resolve(cat: &Catalog) -> Vec<AppEntry> {
    let src_newest = newest_mtime_in(&cat.member_dirs);
    cat.metas
        .iter()
        .map(|m| {
            let (launch, prebuilts) =
                resolve_launch(&cat.target_dir, &m.bin, src_newest, cat.prefer_release);
            AppEntry {
                name: m.name.clone(),
                description: m.description.clone(),
                kind: m.kind,
                launch,
                prebuilts,
            }
        })
        .collect()
}

fn resolve_launch(
    target_dir: &Path,
    bin: &str,
    src_newest: Option<SystemTime>,
    prefer_release: bool,
) -> (Launch, Vec<Prebuilt>) {
    let exe = format!("{bin}{}", std::env::consts::EXE_SUFFIX);
    let candidates = [
        (BinKind::Release, target_dir.join("release").join(&exe)),
        (BinKind::Debug, target_dir.join("debug").join(&exe)),
        (BinKind::Installed, cargo_bin_dir().join(&exe)),
    ];

    let mut prebuilts: Vec<Prebuilt> = candidates
        .into_iter()
        .filter_map(|(kind, path)| {
            let mtime = fs::metadata(&path).ok()?.modified().ok()?;
            let freshness = match src_newest {
                Some(s) if s > mtime => Freshness::Stale,
                _ => Freshness::Fresh,
            };
            Some(Prebuilt {
                kind,
                path,
                freshness,
                mtime,
            })
        })
        .collect();
    prebuilts.sort_by(|a, b| b.mtime.cmp(&a.mtime)); // newest first

    let default = if prefer_release {
        prebuilts
            .iter()
            .find(|p| matches!(p.kind, BinKind::Release))
            .or_else(|| prebuilts.first())
    } else {
        prebuilts.first()
    };
    let launch = match default {
        Some(pb) => Launch::Prebuilt {
            path: pb.path.clone(),
            freshness: pb.freshness,
        },
        None => Launch::BuildOnly,
    };
    (launch, prebuilts)
}

/// The cargo install bin dir: `$CARGO_HOME/bin`, else `~/.cargo/bin`.
pub fn cargo_bin_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        return PathBuf::from(home).join("bin");
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".cargo").join("bin")
}

/// Newest mtime among `*.rs` / `Cargo.toml` under any of the given dirs
/// (recursively). Stat-only — never reads file contents. `target/` and hidden
/// dirs are skipped. The root `Cargo.lock` is intentionally not considered:
/// a lock bump doesn't rebuild an unaffected app, which would strand it stale.
fn newest_mtime_in(dirs: &[PathBuf]) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut consider = |t: Option<SystemTime>| {
        if let Some(t) = t {
            if newest.is_none_or(|n| t > n) {
                newest = Some(t);
            }
        }
    };
    for dir in dirs {
        walk(dir, &mut consider);
    }
    newest
}

fn walk(dir: &Path, consider: &mut impl FnMut(Option<SystemTime>)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "target" || n.starts_with('.'));
            if !skip {
                walk(&path, consider);
            }
        } else if is_source_file(&path) {
            consider(entry.metadata().ok().and_then(|m| m.modified().ok()));
        }
    }
}

fn is_source_file(path: &Path) -> bool {
    match path.file_name().and_then(|n| n.to_str()) {
        Some("Cargo.toml") => true,
        _ => path.extension().and_then(|e| e.to_str()) == Some("rs"),
    }
}
