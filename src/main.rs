//! `cargo-bay` — a terminal UI to browse, run, and rebuild every binary in a
//! Cargo workspace. Discovers apps at runtime via `cargo metadata`, so the
//! same binary works in any workspace. Invoke it as `cargo bay` (a cargo
//! subcommand) or directly as `cargo-bay`.
//!
//!   up/down · j/k · wheel   move
//!   Enter · left-click       run the newest build as-is (even if stale;
//!                            windowed apps log to the panel, terminal apps
//!                            take over the screen)
//!   r · d                    run release · run debug (as-is; build if absent)
//!   b · right-click          version picker (Enter runs as-is; f rebuilds + runs)
//!   R                        rebuild every not-fresh app (dev), in the background
//!   l                        toggle the log console      x  cancel builds
//!   PgUp/PgDn                scroll the log              q / Esc  quit

mod cli;
mod discover;
mod job;
mod theme;
mod ui;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
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
use discover::{discover, resolve, AppEntry, AppKind, BinKind, Catalog, Launch};
use job::Jobs;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Event-loop poll interval — also the spinner animation tick.
const TICK: Duration = Duration::from_millis(100);

/// One selectable line in the version picker.
pub struct RunChoice {
    pub label: String,
    /// What Enter on this row does — run the prebuilt as-is, or build + run.
    pub action: RunAction,
    /// The profile this row targets, so `f` can force-rebuild it. `None` for
    /// rows that can't be rebuilt in place (the installed copy).
    pub profile: Option<BinKind>,
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
    pub console_area: Rect,
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
            console_area: Rect::default(),
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

    /// Build the version picker: one row per profile. If a profile's binary
    /// exists, Enter runs it as-is (even stale); if not, Enter builds + runs it.
    /// `f` on any release/debug row forces a rebuild first. The installed copy
    /// is run-only (we don't reinstall it).
    fn open_run_picker(&mut self) {
        let Some(app) = self.selected() else {
            return;
        };
        let name = app.name.clone();
        let kind = app.kind;
        let prebuilt = |want: BinKind| app.prebuilts.iter().find(|p| p.kind == want);

        let mut choices: Vec<RunChoice> = Vec::new();
        for profile in [BinKind::Release, BinKind::Debug] {
            let release = matches!(profile, BinKind::Release);
            choices.push(match prebuilt(profile) {
                Some(pb) => RunChoice {
                    label: format!(
                        "run {:<9} {}{}",
                        profile.label(),
                        pb.path.display(),
                        ui::freshness_suffix(pb.freshness),
                    ),
                    action: RunAction::Exec(pb.path.clone()),
                    profile: Some(profile),
                },
                None => RunChoice {
                    label: format!(
                        "build {:<7} cargo run -p {name}{}",
                        profile.label(),
                        if release { " --release" } else { "" },
                    ),
                    action: RunAction::Cargo { release },
                    profile: Some(profile),
                },
            });
        }
        if let Some(pb) = prebuilt(BinKind::Installed) {
            choices.push(RunChoice {
                label: format!(
                    "run {:<9} {}{}",
                    BinKind::Installed.label(),
                    pb.path.display(),
                    ui::freshness_suffix(pb.freshness),
                ),
                action: RunAction::Exec(pb.path.clone()),
                profile: Some(BinKind::Installed),
            });
        }
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

    fn on_console(&self, col: u16, row: u16) -> bool {
        contains(self.console_area, col, row)
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
                ..
            } => ("fresh", path.display().to_string()),
            Launch::Prebuilt {
                path,
                freshness: discover::Freshness::Stale,
                ..
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

    // Scriptable path: discover synchronously (no TUI) and print.
    if cfg.list {
        let catalog = match discover(&cfg) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        let apps = resolve(&catalog);
        if apps.is_empty() {
            eprintln!("{}", no_apps_message(&cfg));
        } else {
            print_list(&apps);
        }
        return Ok(());
    }

    // TUI path: open the screen immediately and discover on a worker thread, so
    // a slow `cargo metadata` on a big workspace never leaves the user staring
    // at a blank terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let (tx, rx) = mpsc::channel::<DiscoverResult>();
    let cfg_worker = cfg.clone();
    std::thread::spawn(move || {
        let outcome = discover(&cfg_worker)
            .map(|cat| {
                let apps = resolve(&cat);
                (cat, apps)
            })
            .map_err(|e| e.to_string());
        let _ = tx.send(outcome);
    });

    let result = match loading_loop(&mut terminal, &rx)? {
        Loaded::Quit => Ok(()),
        Loaded::Failed(msg) => {
            restore(&mut terminal)?;
            eprintln!("{msg}");
            std::process::exit(1);
        }
        Loaded::Ready(_, apps) if apps.is_empty() => {
            restore(&mut terminal)?;
            eprintln!("{}", no_apps_message(&cfg));
            return Ok(());
        }
        Loaded::Ready(catalog, apps) => {
            let mut launcher = Launcher::new(apps, catalog);
            let r = run(&mut terminal, &mut launcher);
            launcher.jobs.kill_all();
            r
        }
    };

    restore(&mut terminal)?;
    result
}

/// Discovery handed back from the worker thread: the catalogue plus the first
/// resolve, or a ready-to-print error message.
type DiscoverResult = Result<(Catalog, Vec<AppEntry>), String>;

/// Outcome of the pre-launch loading screen.
enum Loaded {
    Ready(Catalog, Vec<AppEntry>),
    Failed(String),
    Quit,
}

/// Show the scanning splash until discovery finishes — or the user bails.
fn loading_loop(terminal: &mut Term, rx: &mpsc::Receiver<DiscoverResult>) -> io::Result<Loaded> {
    let mut tick: u64 = 0;
    loop {
        terminal.draw(|frame| ui::draw_loading(frame, tick))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && is_quit(&key) {
                    return Ok(Loaded::Quit);
                }
            }
        }

        match rx.try_recv() {
            Ok(Ok((catalog, apps))) => return Ok(Loaded::Ready(catalog, apps)),
            Ok(Err(msg)) => return Ok(Loaded::Failed(msg)),
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                return Ok(Loaded::Failed(
                    "cargo-bay: discovery thread stopped unexpectedly.".into(),
                ));
            }
        }
        tick = tick.wrapping_add(1);
    }
}

/// `q`, `Esc`, or `Ctrl-C` — bail out of the loading screen.
fn is_quit(key: &event::KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// Leave the alternate screen and hand the terminal back to the shell.
fn restore(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()
}

/// The "nothing to run" message, honoring any `--subfolder` scope.
fn no_apps_message(cfg: &cli::Config) -> String {
    let scope = cfg
        .subfolder
        .as_deref()
        .map(|s| format!(" under {s}/"))
        .unwrap_or_default();
    format!("cargo-bay: no runnable binaries found in this workspace{scope}.")
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
            KeyCode::Char('r') => run_profile(terminal, launcher, BinKind::Release)?,
            KeyCode::Char('d') => run_profile(terminal, launcher, BinKind::Debug)?,
            KeyCode::Char('R') => launcher.rebuild_all(),
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
            KeyCode::Char('f') => force_rebuild_choice(terminal, launcher)?,
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
                } else if launcher.on_detail(m.column, m.row) {
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
                if launcher.on_console(m.column, m.row) {
                    launcher.jobs.scroll(-3);
                } else {
                    launcher.move_selection(1);
                }
            }
            MouseEventKind::ScrollUp => {
                if launcher.on_console(m.column, m.row) {
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

/// Direct profile shortcut (`r` = release, `d` = debug): run that profile's
/// prebuilt as-is — even if stale — so a quick demo never waits on a compile.
/// Falls back to a `cargo run` that builds it only when no such binary exists.
fn run_profile(terminal: &mut Term, launcher: &mut Launcher, want: BinKind) -> io::Result<()> {
    let Some(app) = launcher.selected() else {
        return Ok(());
    };
    let name = app.name.clone();
    let kind = app.kind;
    let action = app
        .prebuilts
        .iter()
        .find(|p| p.kind == want)
        .map(|p| RunAction::Exec(p.path.clone()))
        .unwrap_or(RunAction::Cargo {
            release: matches!(want, BinKind::Release),
        });
    dispatch(terminal, launcher, name, kind, action)
}

/// `f` in the version picker: force-rebuild the highlighted profile, then run
/// it — the explicit "yes, I want to wait for a fresh build" path.
fn force_rebuild_choice(terminal: &mut Term, launcher: &mut Launcher) -> io::Result<()> {
    let (name, kind, release) = {
        let Screen::Run {
            app,
            kind,
            choices,
            sel,
        } = &launcher.screen
        else {
            return Ok(());
        };
        let release = match choices.get(*sel).and_then(|c| c.profile) {
            Some(BinKind::Release) => true,
            Some(BinKind::Debug) => false,
            _ => return Ok(()), // installed / none: nothing to rebuild in place
        };
        (app.clone(), *kind, release)
    };
    launcher.screen = Screen::List;
    dispatch(terminal, launcher, name, kind, RunAction::Cargo { release })
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
