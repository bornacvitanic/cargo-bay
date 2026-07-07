//! Runtime app discovery via `cargo metadata` — layout-agnostic, so cargo-bay
//! works in any Cargo workspace, not just one baked in at build time.
//!
//! One `cargo metadata` call yields the workspace root, the real target dir
//! (honoring CARGO_TARGET_DIR), every member package, its binary targets,
//! description, dependencies (for the windowed/terminal split), and the
//! resolved dependency graph. We split discovery into two steps: [`discover`]
//! runs cargo once to build a static [`Catalog`]; [`resolve`] is pure
//! filesystem work (prebuilt lookup + mtime freshness) and is cheap to re-run
//! after every build.
//!
//! Freshness is per-app: an app is stale only when a crate it actually links
//! (its transitive workspace-member dependencies) has source newer than the
//! app's binary — so editing an unrelated crate doesn't mark it stale.

use std::collections::{HashMap, HashSet, VecDeque};
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
    metas: Vec<AppMeta>,
    /// Every workspace member's source dir, keyed by package id.
    member_dirs: HashMap<String, PathBuf>,
    /// Member → its transitive workspace-member dependencies (incl. itself).
    /// Empty when the resolve graph is unavailable (see `dep_graph`).
    closures: HashMap<String, HashSet<String>>,
    /// Whether we have a real dependency graph. When false, freshness falls
    /// back to comparing against every member's source (safe over-approx).
    dep_graph: bool,
    prefer_release: bool,
}

struct AppMeta {
    id: String,
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
    resolve: Option<Resolve>,
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

#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
    #[serde(default)]
    deps: Vec<NodeDep>,
}

#[derive(Deserialize)]
struct NodeDep {
    pkg: String,
    #[serde(default)]
    dep_kinds: Vec<DepKind>,
}

#[derive(Deserialize)]
struct DepKind {
    #[serde(default)]
    kind: Option<String>,
}

/// Run cargo once and build the static catalogue.
pub fn discover(cfg: &Config) -> Result<Catalog, DiscoverError> {
    let mut cmd = Command::new("cargo");
    // No `--no-deps`: we want the resolved graph for per-app freshness.
    cmd.args(["metadata", "--format-version", "1"]);
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

    // Every member's source dir, keyed by id (for the per-member mtime walk).
    let mut member_dirs = HashMap::new();
    for p in &meta.packages {
        if members.contains(p.id.as_str()) {
            let dir = Path::new(&p.manifest_path)
                .parent()
                .unwrap_or(&root)
                .to_path_buf();
            member_dirs.insert(p.id.clone(), dir);
        }
    }

    // Member → transitive member-dependency closure (incl. self).
    let (closures, dep_graph) = build_closures(&meta.resolve, &members);

    let mut metas = Vec::new();
    for p in &meta.packages {
        if !members.contains(p.id.as_str()) {
            continue;
        }
        let Some(bin) = p.targets.iter().find(|t| t.kind.iter().any(|k| k == "bin")) else {
            continue; // lib-only member — not runnable
        };
        let dir = &member_dirs[&p.id];
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
            id: p.id.clone(),
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
        metas,
        member_dirs,
        closures,
        dep_graph,
        prefer_release: cfg.prefer_release,
    })
}

/// Build each member's transitive workspace-member dependency set (incl.
/// itself), skipping dev-only edges (they don't affect the built binary).
/// Returns `(closures, true)` on success, or `(empty, false)` if the resolve
/// graph is missing — the caller then over-approximates against all members.
fn build_closures(
    resolve: &Option<Resolve>,
    members: &HashSet<&str>,
) -> (HashMap<String, HashSet<String>>, bool) {
    let Some(resolve) = resolve else {
        return (HashMap::new(), false);
    };

    // Direct member-dependency edges between workspace members.
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in &resolve.nodes {
        if !members.contains(node.id.as_str()) {
            continue;
        }
        let deps = node
            .deps
            .iter()
            .filter(|d| members.contains(d.pkg.as_str()))
            // Keep a dep unless it is used *only* as a dev-dependency.
            .filter(|d| {
                d.dep_kinds.is_empty()
                    || d.dep_kinds.iter().any(|k| k.kind.as_deref() != Some("dev"))
            })
            .map(|d| d.pkg.as_str())
            .collect();
        edges.insert(node.id.as_str(), deps);
    }

    let mut closures = HashMap::new();
    for &start in members {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([start]);
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id.to_string()) {
                continue;
            }
            if let Some(neighbours) = edges.get(id) {
                queue.extend(neighbours.iter().copied());
            }
        }
        closures.insert(start.to_string(), seen);
    }
    (closures, true)
}

/// Filesystem-only pass: resolve each app's prebuilt binaries and freshness.
/// Safe and cheap to call after every build.
pub fn resolve(cat: &Catalog) -> Vec<AppEntry> {
    // Newest source mtime per member crate.
    let member_newest: HashMap<&str, Option<SystemTime>> = cat
        .member_dirs
        .iter()
        .map(|(id, dir)| (id.as_str(), newest_mtime_in(std::slice::from_ref(dir))))
        .collect();

    // Fallback bound used when we have no dependency graph.
    let all_newest = member_newest.values().copied().flatten().max();

    cat.metas
        .iter()
        .map(|m| {
            let src_newest = if cat.dep_graph {
                cat.closures
                    .get(&m.id)
                    .into_iter()
                    .flatten()
                    .filter_map(|id| member_newest.get(id.as_str()).copied().flatten())
                    .max()
            } else {
                all_newest
            };
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
    prebuilts.sort_by_key(|p| std::cmp::Reverse(p.mtime)); // newest first

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
