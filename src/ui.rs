//! Rendering: a brand header (freight mark + workspace + glyph legend); the app
//! list (with run-status / freshness glyphs) beside the selected app's detail; a
//! collapsible log console docked along the bottom; a key-hint + build-summary
//! footer; and a modal version-picker overlay. All colours and glyphs come from
//! [`crate::theme`] so the look stays consistent across the suite.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::discover::{AppEntry, AppKind, Freshness, Launch};
use crate::job::{spinner, AppStatus, Jobs};
use crate::theme::*;
use crate::{Launcher, Screen};

/// The header block: brandmark + workspace/count line, then the glyph legend.
const HEADER_HEIGHT: u16 = 2;

/// Trailing note describing a binary's freshness (for picker labels).
pub fn freshness_suffix(f: Freshness) -> &'static str {
    match f {
        Freshness::Stale => "  (stale)",
        Freshness::Fresh => "  (fresh)",
    }
}

/// The left-column glyph: a live job state if one applies, else freshness.
fn list_tag(app: &AppEntry, jobs: &Jobs, tick: u64) -> Span<'static> {
    match jobs.status_of(&app.name) {
        Some(AppStatus::Queued) => Span::styled(G_QUEUED, Style::default().fg(QUEUED)),
        Some(AppStatus::Building) => Span::styled(
            format!("[{}]", spinner(tick)),
            Style::default().fg(BUILDING),
        ),
        Some(AppStatus::BuildFailed) => Span::styled(G_FAILED, Style::default().fg(FAILED)),
        Some(AppStatus::Running) => Span::styled(G_RUNNING, Style::default().fg(RUNNING)),
        None => freshness_tag(app),
    }
}

fn freshness_tag(app: &AppEntry) -> Span<'static> {
    let (glyph, color) = match &app.launch {
        Launch::Prebuilt {
            freshness: Freshness::Stale,
            ..
        } => (G_STALE, STALE),
        Launch::Prebuilt { .. } => (G_FRESH, FRESH),
        Launch::BuildOnly => (G_NEW, NEW),
    };
    Span::styled(glyph, Style::default().fg(color))
}

pub fn draw(frame: &mut Frame, launcher: &mut Launcher) {
    let area = frame.area();
    let console_on = launcher.jobs.console_visible;
    // Dock the log to the bottom third, within sane bounds, when it's open.
    let log_h = if console_on {
        (area.height / 3).clamp(6, 16)
    } else {
        0
    };

    let mut constraints = vec![Constraint::Length(HEADER_HEIGHT), Constraint::Min(3)];
    if console_on {
        constraints.push(Constraint::Length(log_h));
    }
    constraints.push(Constraint::Length(3));
    let rows = Layout::vertical(constraints).split(area);

    let header_area = rows[0];
    let body_area = rows[1];
    let (log_area, footer_area) = if console_on {
        (Some(rows[2]), rows[3])
    } else {
        (None, rows[2])
    };

    header(frame, launcher, header_area);

    let body = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(body_area);

    let list_block = Block::bordered().title(" apps ");
    launcher.list_inner = list_block.inner(body[0]);
    let items: Vec<ListItem> = launcher
        .apps
        .iter()
        .map(|a| {
            let mut spans = vec![
                list_tag(a, &launcher.jobs, launcher.tick),
                Span::raw(" "),
                Span::raw(a.name.clone()),
            ];
            // The profile Enter will run, so the fast path is never a mystery.
            if let Launch::Prebuilt { kind, .. } = &a.launch {
                spans.push(Span::styled(
                    format!("  ·{}", kind.label()),
                    Style::default().fg(MUTED),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(list_block)
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, body[0], &mut launcher.state);

    // Detail is always visible now — the log docks below rather than replacing it.
    launcher.detail_area = body[1];
    frame.render_widget(detail(launcher), body[1]);

    if let Some(log_area) = log_area {
        launcher.console_area = log_area;
        frame.render_widget(console(&launcher.jobs, log_area), log_area);
    } else {
        launcher.console_area = Rect::default();
    }

    frame.render_widget(footer(launcher), footer_area);

    // Modal version picker over the top.
    if let Screen::Run {
        app, choices, sel, ..
    } = &launcher.screen
    {
        let (app, sel) = (app.clone(), *sel);
        let area = centered(frame.area(), 72, choices.len() as u16 + 2);
        let block = Block::bordered().title(format!(" run: {app} "));
        launcher.picker_inner = block.inner(area);

        let items: Vec<ListItem> = choices
            .iter()
            .map(|c| ListItem::new(c.label.clone()))
            .collect();
        let mut pstate = ListState::default();
        pstate.select(Some(sel));
        let picker = List::new(items)
            .block(block)
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_widget(Clear, area);
        frame.render_stateful_widget(picker, area, &mut pstate);
    }
}

/// The top brand bar: `⚓ freight · cargo-bay` on the left, the workspace name
/// and app count on the right, with the status-glyph legend beneath.
fn header(frame: &mut Frame, launcher: &Launcher, area: Rect) {
    let lines = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    let top = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(lines[0]);

    let brand = Line::from(vec![
        Span::styled(
            format!("{BRAND_MARK} freight"),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · cargo-bay", Style::default().fg(MUTED)),
    ]);
    frame.render_widget(Paragraph::new(brand), top[0]);

    let workspace = launcher
        .catalog
        .root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    let info = Line::from(Span::styled(
        format!("{workspace} · {} apps", launcher.apps.len()),
        Style::default().fg(MUTED),
    ));
    frame.render_widget(Paragraph::new(info).alignment(Alignment::Right), top[1]);

    frame.render_widget(Paragraph::new(legend()), lines[1]);
}

/// The status-glyph key, colour-coded to match the list column.
fn legend() -> Line<'static> {
    let item = |glyph: &'static str, color, label: &'static str| {
        [
            Span::styled(glyph, Style::default().fg(color)),
            Span::styled(format!(" {label}  "), Style::default().fg(MUTED)),
        ]
    };
    Line::from(
        [
            item(G_FRESH, FRESH, "fresh"),
            item(G_STALE, STALE, "stale"),
            item(G_NEW, NEW, "new"),
            item(G_RUNNING, RUNNING, "running"),
            item(G_FAILED, FAILED, "failed"),
        ]
        .concat(),
    )
}

/// The scrollable log console (tail-following, one line per row, no wrap).
fn console(jobs: &Jobs, area: Rect) -> Paragraph<'static> {
    let log = jobs.log.lock().unwrap_or_else(|e| e.into_inner());
    let visible = area.height.saturating_sub(2) as usize;
    let end = log.len().saturating_sub(jobs.console_scroll as usize);
    let start = end.saturating_sub(visible);
    let lines: Vec<Line> = log[start..end.max(start)]
        .iter()
        .map(|l| Line::from(l.clone()))
        .collect();
    Paragraph::new(lines)
        .block(Block::bordered().title(" log — l to hide, PgUp/PgDn / wheel scroll "))
}

fn detail(launcher: &Launcher) -> Paragraph<'static> {
    let selected = launcher.state.selected().unwrap_or(0);
    let Some(app) = launcher.apps.get(selected) else {
        return Paragraph::new("No apps found.").block(Block::bordered());
    };

    let desc = if app.description.is_empty() {
        "(no description in Cargo.toml)"
    } else {
        &app.description
    };
    let kind = match app.kind {
        AppKind::Windowed => "windowed — runs in the background, logs to the console (press l)",
        AppKind::Terminal => "terminal — takes over the screen while it runs",
    };
    let lines = vec![
        Line::from(desc.to_string()),
        Line::from(""),
        Line::from(vec![
            Span::styled("runs: ", Style::default().fg(MUTED)),
            run_line(app),
        ]),
        Line::from(vec![
            Span::styled("kind: ", Style::default().fg(MUTED)),
            Span::styled(kind, Style::default().fg(MUTED)),
        ]),
    ];

    Paragraph::new(lines)
        .block(Block::bordered().title(format!(" {} ", app.name)))
        .wrap(Wrap { trim: true })
}

/// Human sentence describing what Enter / left-click will do for this app.
fn run_line(app: &AppEntry) -> Span<'static> {
    match &app.launch {
        Launch::Prebuilt {
            path,
            freshness,
            kind,
        } => {
            let (note, color) = match freshness {
                Freshness::Stale => (
                    "  (stale — Enter runs it as-is · b then f rebuilds + runs)",
                    STALE,
                ),
                Freshness::Fresh => ("  (fresh)", FRESH),
            };
            Span::styled(
                format!("{}  {}{note}", kind.label(), path.display()),
                Style::default().fg(color),
            )
        }
        Launch::BuildOnly => Span::styled(
            format!("cargo run -p {} (no prebuilt exe yet)", app.name),
            Style::default().fg(NEW),
        ),
    }
}

fn footer(launcher: &Launcher) -> Paragraph<'static> {
    let hint = |key: &'static str, label: &'static str| {
        [
            Span::styled(key, Style::default().fg(ACCENT)),
            Span::raw(format!(" {label}  ")),
        ]
    };
    let mut spans: Vec<Span> = if matches!(launcher.screen, Screen::List) {
        [
            hint("Enter", "run"),
            hint("r", "release"),
            hint("d", "debug"),
            hint("b", "versions"),
            hint("R", "rebuild"),
            hint("l", "log"),
            hint("x", "cancel"),
            hint("q", "quit"),
        ]
        .concat()
    } else {
        [
            hint("up/down", "move"),
            hint("Enter/click", "run as-is"),
            hint("f", "rebuild + run"),
            hint("Esc", "back"),
        ]
        .concat()
    };

    let (building, queued) = launcher.jobs.counts();
    if building + queued > 0 {
        spans.push(Span::styled(
            format!("building {building}, queued {queued}  "),
            Style::default().fg(BUILDING),
        ));
    }
    if !launcher.status.is_empty() {
        spans.push(Span::styled(
            launcher.status.clone(),
            Style::default().fg(MUTED),
        ));
    }
    Paragraph::new(Line::from(spans)).block(Block::bordered())
}

/// A `w`×`h` rect centered in `area`, clamped to fit.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let width = w.min(area.width);
    let height = h.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The pre-launch splash shown while `cargo metadata` runs on a worker thread,
/// so a big workspace never greets the user with a blank terminal.
pub fn draw_loading(frame: &mut Frame, tick: u64) {
    let area = centered(frame.area(), 46, 5);
    frame.render_widget(Clear, area);
    let sp = spinner(tick);
    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{BRAND_MARK} freight"),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · cargo-bay", Style::default().fg(MUTED)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            format!("{sp}  scanning the workspace…"),
            Style::default().fg(MUTED),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered())
            .alignment(Alignment::Center),
        area,
    );
}
