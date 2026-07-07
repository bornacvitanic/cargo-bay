//! `cargo-bay` — a terminal UI to browse, run, and rebuild every binary in a
//! Cargo workspace. Discovers apps at runtime via `cargo metadata`, so the
//! same binary works in any workspace. Invoke it as `cargo bay` (a cargo
//! subcommand) or directly as `cargo-bay`.
//!
//!   up/down · j/k · wheel   move
//!   Enter · left-click       run (windowed apps stream to the log panel;
//!                            terminal apps take over the screen)
//!   b · right-click          version picker (installed / release / debug / rebuild)
//!   r                        rebuild every not-fresh app (dev), in the background
//!   l                        toggle the log console      x  cancel builds
//!   PgUp/PgDn                scroll the log              q / Esc  quit

mod cli;
mod discover;
mod job;
mod ui;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use ratatui::Terminal;

use cli::Cli;
use discover::{discover, resolve, AppEntry, AppKind, Catalog, Launch};
use job::Jobs;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Event-loop poll interval — also the spinner animation tick.
const TICK: Duration = Duration::from_millis(100);

/// One selectable line in the version picker.
pub struct RunChoice {
    pub label: String,
    pub action: RunAction,
}

/// What a picker choice does when confirmed.
#[derive(Clone)]
pub enum RunAction {
    Exec(PathBuf),
    Cargo { release: bool },
}

/// Which screen is showing. `Run` is a modal version picker over the app list.
pub enum Screen {
    List,
    Run {
        app: String,
        kind: AppKind,
        choices: Vec<RunChoice>,
        sel: usize,
    },
}

pub struct Launcher {
    pub apps: Vec<AppEntry>,
    pub catalog: Catalog,
    pub state: ListState,
    pub status: String,
    pub screen: Screen,
    pub jobs: Jobs,
    pub tick: u64,
    pub quit: bool,
    pub list_inner: Rect,
    pub picker_inner: Rect,
    pub detail_area: Rect,
}

impl Launcher {
    fn new(apps: Vec<AppEntry>, catalog: Catalog) -> Self {
        let mut state = ListState::default();
        if !apps.is_empty() {
            state.select(Some(0));
        }
        let jobs = Jobs::new(catalog.root.clone());
        Self {
            apps,
            catalog,
            state,
            status: String::new(),
            screen: Screen::List,
            jobs,
            tick: 0,
            quit: false,
            list_inner: Rect::default(),
            picker_inner: Rect::default(),
            detail_area: Rect::default(),
        }
    }

    fn root(&self) -> PathBuf {
        self.catalog.root.clone()
    }

    fn move_selection(&mut self, delta: isize) {
        if self.apps.is_empty() {
            return;
        }
        let len = self.apps.len() as isize;
        let cur = self.state.selected().unwrap_or(0) as isize;
        self.state
            .select(Some((cur + delta).rem_euclid(len) as usize));
    }

    fn selected(&self) -> Option<&AppEntry> {
        self.state.selected().and_then(|i| self.apps.get(i))
    }

    /// Re-resolve freshness (filesystem only, no cargo) after a build/launch.
    /// The app set is stable, so the selection index stays valid.
    fn refresh(&mut self) {
        self.apps = resolve(&self.catalog);
        if self.apps.is_empty() {
            self.state.select(None);
        } else if self.state.selected().is_none_or(|s| s >= self.apps.len()) {
            self.state.select(Some(self.apps.len() - 1));
        }
    }

    fn rebuild_all(&mut self) {
        let names: Vec<String> = self
            .apps
            .iter()
            .filter(|a| needs_build(a))
            .map(|a| a.name.clone())
            .collect();
        if names.is_empty() {
            self.status = "everything is already fresh".into();
            return;
        }
        for name in &names {
            self.jobs.enqueue_build(name);
        }
        self.status = format!("queued {} build(s)", names.len());
    }

    fn open_run_picker(&mut self) {
        let Some(app) = self.selected() else {
            return;
        };
        let name = app.name.clone();
        let kind = app.kind;
        let mut choices: Vec<RunChoice> = app
            .prebuilts
            .iter()
            .map(|pb| RunChoice {
                label: format!(
                    "run {:<9} {}{}",
                    pb.kind.label(),
                    pb.path.display(),
                    ui::freshness_suffix(pb.freshness),
                ),
                action: RunAction::Exec(pb.path.clone()),
            })
            .collect();
        choices.push(RunChoice {
            label: "build dev      cargo run -p".into(),
            action: RunAction::Cargo { release: false },
        });
        choices.push(RunChoice {
            label: "build release  cargo run -p --release".into(),
            action: RunAction::Cargo { release: true },
        });
        self.screen = Screen::Run {
            app: name,
            kind,
            choices,
            sel: 0,
        };
    }

    fn picker_len(&self) -> usize {
        match &self.screen {
            Screen::Run { choices, .. } => choices.len(),
            Screen::List => 0,
        }
    }

    fn picker_move(&mut self, delta: isize) {
        let len = self.picker_len() as isize;
        if let Screen::Run { sel, .. } = &mut self.screen {
            if len > 0 {
                *sel = ((*sel as isize + delta).rem_euclid(len)) as usize;
            }
        }
    }

    fn set_picker_sel(&mut self, i: usize) {
        if let Screen::Run { sel, .. } = &mut self.screen {
            *sel = i;
        }
    }

    fn list_hit(&self, col: u16, row: u16) -> Option<usize> {
        if !contains(self.list_inner, col, row) {
            return None;
        }
        let idx = self.state.offset() + (row - self.list_inner.y) as usize;
        (idx < self.apps.len()).then_some(idx)
    }

    fn picker_hit(&self, col: u16, row: u16) -> Option<usize> {
        if !contains(self.picker_inner, col, row) {
            return None;
        }
        let idx = (row - self.picker_inner.y) as usize;
        (idx < self.picker_len()).then_some(idx)
    }

    fn on_detail(&self, col: u16, row: u16) -> bool {
        contains(self.detail_area, col, row)
    }
}

/// An app worth a background rebuild: not already fresh.
fn needs_build(app: &AppEntry) -> bool {
    matches!(
        app.launch,
        Launch::BuildOnly
            | Launch::Prebuilt {
                freshness: discover::Freshness::Stale,
                ..
            }
    )
}

/// Point-in-rect test.
fn contains(a: Rect, col: u16, row: u16) -> bool {
    col >= a.x && col < a.x + a.width && row >= a.y && row < a.y + a.height
}

/// Plain-text listing for `--list` (scriptable; no TUI).
fn print_list(apps: &[AppEntry]) {
    for app in apps {
        let kind = match app.kind {
            AppKind::Windowed => "windowed",
            AppKind::Terminal => "terminal",
        };
        let (tag, runs) = match &app.launch {
            Launch::Prebuilt {
                path,
                freshness: discover::Freshness::Fresh,
            } => ("fresh", path.display().to_string()),
            Launch::Prebuilt {
                path,
                freshness: discover::Freshness::Stale,
            } => ("stale", path.display().to_string()),
            Launch::BuildOnly => ("build", format!("cargo run -p {}", app.name)),
        };
        println!("{:<7} {:<8} {:<22} {}", tag, kind, app.name, runs);
    }
}

fn main() -> io::Result<()> {
    let cfg = match cli::parse(std::env::args()) {
        Cli::Help => {
            print!("{}", cli::usage());
            return Ok(());
        }
        Cli::Error(e) => {
            eprintln!("error: {e}\n\n{}", cli::usage());
            std::process::exit(2);
        }
        Cli::Run(cfg) => cfg,
    };

    let catalog = match discover(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let apps = resolve(&catalog);
    if apps.is_empty() {
        let scope = cfg
            .subfolder
            .as_deref()
            .map(|s| format!(" under {s}/"))
            .unwrap_or_default();
        eprintln!("cargo-bay: no runnable binaries found in this workspace{scope}.");
        return Ok(());
    }
    if cfg.list {
        print_list(&apps);
        return Ok(());
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let mut launcher = Launcher::new(apps, catalog);
    let result = run(&mut terminal, &mut launcher);

    launcher.jobs.kill_all();
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Term, launcher: &mut Launcher) -> io::Result<()> {
    while !launcher.quit {
        terminal.draw(|frame| ui::draw(frame, launcher))?;

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(terminal, launcher, key.code)?
                }
                Event::Mouse(m) => handle_mouse(terminal, launcher, m)?,
                _ => {}
            }
        }

        let finished = launcher.jobs.pump();
        if finished.iter().any(|d| d.success) {
            launcher.refresh();
        }
        launcher.tick = launcher.tick.wrapping_add(1);
    }
    Ok(())
}

fn handle_key(terminal: &mut Term, launcher: &mut Launcher, code: KeyCode) -> io::Result<()> {
    if matches!(launcher.screen, Screen::List) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => launcher.quit = true,
            KeyCode::Up | KeyCode::Char('k') => launcher.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => launcher.move_selection(1),
            KeyCode::Enter => launch_selected(terminal, launcher)?,
            KeyCode::Char('b') => launcher.open_run_picker(),
            KeyCode::Char('r') => launcher.rebuild_all(),
            KeyCode::Char('l') => launcher.jobs.toggle_console(),
            KeyCode::Char('x') => launcher.jobs.cancel_builds(),
            KeyCode::PageUp => launcher.jobs.scroll(5),
            KeyCode::PageDown => launcher.jobs.scroll(-5),
            _ => {}
        }
    } else {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => launcher.screen = Screen::List,
            KeyCode::Up | KeyCode::Char('k') => launcher.picker_move(-1),
            KeyCode::Down | KeyCode::Char('j') => launcher.picker_move(1),
            KeyCode::Enter => run_choice(terminal, launcher)?,
            _ => {}
        }
    }
    Ok(())
}

fn handle_mouse(
    terminal: &mut Term,
    launcher: &mut Launcher,
    m: event::MouseEvent,
) -> io::Result<()> {
    if matches!(launcher.screen, Screen::List) {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(i) = launcher.list_hit(m.column, m.row) {
                    launcher.state.select(Some(i));
                    launch_selected(terminal, launcher)?;
                } else if launcher.on_detail(m.column, m.row) && !launcher.jobs.console_visible {
                    launch_selected(terminal, launcher)?;
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(i) = launcher.list_hit(m.column, m.row) {
                    launcher.state.select(Some(i));
                    launcher.open_run_picker();
                } else if launcher.on_detail(m.column, m.row) {
                    launcher.open_run_picker();
                }
            }
            MouseEventKind::ScrollDown => {
                if launcher.jobs.console_visible && launcher.on_detail(m.column, m.row) {
                    launcher.jobs.scroll(-3);
                } else {
                    launcher.move_selection(1);
                }
            }
            MouseEventKind::ScrollUp => {
                if launcher.jobs.console_visible && launcher.on_detail(m.column, m.row) {
                    launcher.jobs.scroll(3);
                } else {
                    launcher.move_selection(-1);
                }
            }
            _ => {}
        }
    } else {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => match launcher.picker_hit(m.column, m.row) {
                Some(i) => {
                    launcher.set_picker_sel(i);
                    run_choice(terminal, launcher)?;
                }
                None => launcher.screen = Screen::List,
            },
            MouseEventKind::Down(MouseButton::Right) => launcher.screen = Screen::List,
            MouseEventKind::ScrollDown => launcher.picker_move(1),
            MouseEventKind::ScrollUp => launcher.picker_move(-1),
            _ => {}
        }
    }
    Ok(())
}

/// Enter / left-click on the list: run the newest prebuilt exe, or `cargo run`
/// when none exists — routed to background or terminal by app kind.
fn launch_selected(terminal: &mut Term, launcher: &mut Launcher) -> io::Result<()> {
    let Some(app) = launcher.selected() else {
        return Ok(());
    };
    let (name, kind) = (app.name.clone(), app.kind);
    let action = match &app.launch {
        Launch::Prebuilt { path, .. } => RunAction::Exec(path.clone()),
        Launch::BuildOnly => RunAction::Cargo { release: false },
    };
    dispatch(terminal, launcher, name, kind, action)
}

/// Confirm a version-picker choice.
fn run_choice(terminal: &mut Term, launcher: &mut Launcher) -> io::Result<()> {
    let (name, kind, action) = {
        let Screen::Run {
            app,
            kind,
            choices,
            sel,
        } = &launcher.screen
        else {
            return Ok(());
        };
        let Some(choice) = choices.get(*sel) else {
            return Ok(());
        };
        (app.clone(), *kind, choice.action.clone())
    };
    launcher.screen = Screen::List;
    dispatch(terminal, launcher, name, kind, action)
}

/// Route a resolved launch: windowed apps run in the background (log panel),
/// terminal apps get the real terminal handed over (blocking).
fn dispatch(
    terminal: &mut Term,
    launcher: &mut Launcher,
    name: String,
    kind: AppKind,
    action: RunAction,
) -> io::Result<()> {
    let root = launcher.root();
    let (command, how, cargo) = match &action {
        RunAction::Exec(path) => (
            exec_command(&root, path),
            format!("ran {}", path.display()),
            false,
        ),
        RunAction::Cargo { release } => (
            cargo_run_command(&root, &name, *release),
            if *release {
                format!("cargo run -p {name} --release")
            } else {
                format!("cargo run -p {name}")
            },
            true,
        ),
    };

    match kind {
        AppKind::Windowed => {
            launcher.jobs.spawn_run(command, name.clone(), cargo);
            launcher.status = format!("launched {name} — press l for its log");
        }
        AppKind::Terminal => {
            launcher.status = run_blocking(terminal, &how, command, &name)?;
            launcher.refresh();
        }
    }
    Ok(())
}

fn exec_command(root: &Path, path: &Path) -> Command {
    let mut c = Command::new(path);
    c.current_dir(root);
    c
}

fn cargo_run_command(root: &Path, name: &str, release: bool) -> Command {
    let mut c = Command::new("cargo");
    c.args(["run", "-p", name]);
    if release {
        c.arg("--release");
    }
    c.current_dir(root);
    c
}

/// Terminal-app path: suspend the TUI, hand over stdio, resume when it exits.
fn run_blocking(
    terminal: &mut Term,
    how: &str,
    mut command: Command,
    name: &str,
) -> io::Result<String> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    println!("\n> {how}\n");
    let status = command.status();

    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.clear()?;

    Ok(match status {
        Ok(s) if s.success() => format!("{name} exited cleanly"),
        Ok(s) => format!("{name} exited ({s})"),
        Err(e) => format!("failed to launch {name}: {e}"),
    })
}
