//! Rendering: app list (with run-status / build glyphs) on the left; the
//! selected app's description — or the live log console — on the right; a
//! key-hint + build-summary footer; and a modal version-picker overlay.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::discover::{AppEntry, AppKind, Freshness, Launch};
use crate::job::{spinner, AppStatus, Jobs};
use crate::{Launcher, Screen};

const ACCENT: Color = Color::Cyan;

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
        Some(AppStatus::Queued) => Span::styled("[q]", Style::default().fg(Color::DarkGray)),
        Some(AppStatus::Building) => Span::styled(
            format!("[{}]", spinner(tick)),
            Style::default().fg(Color::Yellow),
        ),
        Some(AppStatus::BuildFailed) => Span::styled("[!]", Style::default().fg(Color::Red)),
        Some(AppStatus::Running) => Span::styled("[>]", Style::default().fg(ACCENT)),
        None => freshness_tag(app),
    }
}

fn freshness_tag(app: &AppEntry) -> Span<'static> {
    let (glyph, color) = match &app.launch {
        Launch::Prebuilt {
            freshness: Freshness::Stale,
            ..
        } => ("[*]", Color::Yellow),
        Launch::Prebuilt { .. } => ("[+]", Color::Green),
        Launch::BuildOnly => ("[.]", Color::Blue),
    };
    Span::styled(glyph, Style::default().fg(color))
}

pub fn draw(frame: &mut Frame, launcher: &mut Launcher) {
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(frame.area());
    let body =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).split(rows[0]);

    let list_block = Block::bordered().title(format!(" apps ({}) ", launcher.apps.len()));
    launcher.list_inner = list_block.inner(body[0]);
    let items: Vec<ListItem> = launcher
        .apps
        .iter()
        .map(|a| {
            ListItem::new(Line::from(vec![
                list_tag(a, &launcher.jobs, launcher.tick),
                Span::raw(" "),
                Span::raw(a.name.clone()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(list_block)
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, body[0], &mut launcher.state);

    launcher.detail_area = body[1];
    if launcher.jobs.console_visible {
        frame.render_widget(console(&launcher.jobs, body[1]), body[1]);
    } else {
        frame.render_widget(detail(launcher), body[1]);
    }
    frame.render_widget(footer(launcher), rows[1]);

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
        AppKind::Windowed => "windowed — runs in the background, logs to this panel (press l)",
        AppKind::Terminal => "terminal — takes over the screen while it runs",
    };
    let lines = vec![
        Line::from(desc.to_string()),
        Line::from(""),
        Line::from(vec![
            Span::styled("runs: ", Style::default().fg(Color::DarkGray)),
            run_line(app),
        ]),
        Line::from(vec![
            Span::styled("kind: ", Style::default().fg(Color::DarkGray)),
            Span::styled(kind, Style::default().fg(Color::DarkGray)),
        ]),
    ];

    Paragraph::new(lines)
        .block(Block::bordered().title(format!(" {} ", app.name)))
        .wrap(Wrap { trim: true })
}

/// Human sentence describing what Enter / left-click will do for this app.
fn run_line(app: &AppEntry) -> Span<'static> {
    match &app.launch {
        Launch::Prebuilt { path, freshness } => {
            let (note, color) = match freshness {
                Freshness::Stale => (
                    "  (stale — b / right-click for other versions, r to rebuild)",
                    Color::Yellow,
                ),
                Freshness::Fresh => ("  (fresh)", Color::Green),
            };
            Span::styled(
                format!("{}{note}", path.display()),
                Style::default().fg(color),
            )
        }
        Launch::BuildOnly => Span::styled(
            format!("cargo run -p {} (no prebuilt exe yet)", app.name),
            Style::default().fg(Color::Blue),
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
            hint("Enter/click", "run"),
            hint("b", "versions"),
            hint("r", "rebuild stale"),
            hint("l", "log"),
            hint("x", "cancel"),
            hint("q", "quit"),
        ]
        .concat()
    } else {
        [
            hint("up/down", "move"),
            hint("Enter/click", "run this version"),
            hint("Esc/right-click", "cancel"),
        ]
        .concat()
    };

    let (building, queued) = launcher.jobs.counts();
    if building + queued > 0 {
        spans.push(Span::styled(
            format!("building {building}, queued {queued}  "),
            Style::default().fg(Color::Yellow),
        ));
    }
    if !launcher.status.is_empty() {
        spans.push(Span::styled(
            launcher.status.clone(),
            Style::default().fg(Color::DarkGray),
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
