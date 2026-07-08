//! The one place colours and status glyphs are defined, so the whole TUI stays
//! visually consistent — and so this vocabulary can move into the shared
//! `freight` core (`portside`) unchanged when later tools reuse it.

use ratatui::style::Color;

/// The suite accent — selections, keys, the brandmark.
pub const ACCENT: Color = Color::Cyan;
/// Secondary/label text.
pub const MUTED: Color = Color::DarkGray;

/// A prebuilt that is up to date with its sources.
pub const FRESH: Color = Color::Green;
/// A prebuilt whose sources have changed since it was built.
pub const STALE: Color = Color::Yellow;
/// No prebuilt yet — first run will compile.
pub const NEW: Color = Color::Blue;
/// A build in progress.
pub const BUILDING: Color = Color::Yellow;
/// A launched app that is still running.
pub const RUNNING: Color = Color::Cyan;
/// A build that failed.
pub const FAILED: Color = Color::Red;
/// A queued (not yet started) build.
pub const QUEUED: Color = Color::DarkGray;

/// Left-column status glyphs. Kept to three cells so the list stays aligned.
pub const G_FRESH: &str = "[+]";
pub const G_STALE: &str = "[*]";
pub const G_NEW: &str = "[.]";
pub const G_QUEUED: &str = "[q]";
pub const G_RUNNING: &str = "[>]";
pub const G_FAILED: &str = "[!]";

/// The brandmark glyph. Unicode anchor; swap to `"#"` for strict-ASCII terminals.
pub const BRAND_MARK: &str = "⚓";
