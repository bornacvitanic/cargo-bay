//! Background process management: a serial build queue plus fire-and-forget
//! windowed-app runs, all with their stdout/stderr streamed into a shared log
//! so the TUI never blocks. Cargo commands are serialized because cargo locks
//! the workspace `target/` dir — parallel `cargo build`s would just contend.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Ring-buffer cap for the log console (oldest lines drop past this).
pub const MAX_LOG_LINES: usize = 4000;
const SPINNER: [char; 4] = ['|', '/', '-', '\\'];

pub type LogBuf = Arc<Mutex<Vec<String>>>;

/// The spinner frame for the current tick, for "building" indicators.
pub fn spinner(tick: u64) -> char {
    SPINNER[(tick as usize) % SPINNER.len()]
}

#[derive(Clone, Copy)]
pub enum JobKind {
    /// `cargo build -p <name>` — occupies the single cargo slot.
    Build,
    /// A launched windowed app. `cargo` marks whether it's a `cargo run`
    /// (also needs the cargo slot) vs a direct prebuilt-exe run (does not).
    Run { cargo: bool },
}

/// Per-app UI state while a job concerns it.
pub enum AppStatus {
    Queued,
    Building,
    BuildFailed,
    Running,
}

/// A finished build reported back so the caller can refresh freshness tags.
pub struct BuildDone {
    pub name: String,
    pub success: bool,
}

struct Job {
    name: String,
    kind: JobKind,
    child: Child,
    readers: Vec<JoinHandle<()>>,
}

impl Job {
    fn spawn(
        mut command: Command,
        name: String,
        kind: JobKind,
        log: &LogBuf,
    ) -> std::io::Result<Job> {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(spawn_reader(out, name.clone(), log.clone()));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(spawn_reader(err, name.clone(), log.clone()));
        }
        Ok(Job {
            name,
            kind,
            child,
            readers,
        })
    }

    /// Blocks the child if running; kills it and reaps it. Reader threads end
    /// when the pipes close, so we join them to leave no strays behind.
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for r in self.readers.drain(..) {
            let _ = r.join();
        }
    }

    fn is_cargo(&self) -> bool {
        matches!(self.kind, JobKind::Build | JobKind::Run { cargo: true })
    }
}

fn spawn_reader<R: Read + Send + 'static>(reader: R, name: String, log: LogBuf) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            push_line(&log, format!("[{name}] {line}"));
        }
    })
}

fn push_line(log: &LogBuf, line: String) {
    if let Ok(mut v) = log.lock() {
        v.push(line);
        let overflow = v.len().saturating_sub(MAX_LOG_LINES);
        if overflow > 0 {
            v.drain(0..overflow);
        }
    }
}

/// Owns the build queue, live jobs, per-app status, and the shared log.
pub struct Jobs {
    root: PathBuf,
    pub log: LogBuf,
    pub console_visible: bool,
    /// Lines scrolled up from the bottom (0 = follow the tail).
    pub console_scroll: u16,
    queue: VecDeque<String>,
    running: Vec<Job>,
    pub status: HashMap<String, AppStatus>,
}

impl Jobs {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            log: Arc::new(Mutex::new(Vec::new())),
            console_visible: false,
            console_scroll: 0,
            queue: VecDeque::new(),
            running: Vec::new(),
            status: HashMap::new(),
        }
    }

    pub fn status_of(&self, name: &str) -> Option<&AppStatus> {
        self.status.get(name)
    }

    /// (building, queued) counts for the footer summary.
    pub fn counts(&self) -> (usize, usize) {
        let building = self
            .running
            .iter()
            .filter(|j| matches!(j.kind, JobKind::Build))
            .count();
        (building, self.queue.len())
    }

    pub fn is_busy(&self) -> bool {
        !self.queue.is_empty() || !self.running.is_empty()
    }

    fn cargo_active(&self) -> bool {
        self.running.iter().any(Job::is_cargo)
    }

    /// Queue a dev build unless the app is already queued or building.
    pub fn enqueue_build(&mut self, name: &str) {
        let active = matches!(
            self.status.get(name),
            Some(AppStatus::Queued | AppStatus::Building)
        );
        if !active {
            self.queue.push_back(name.to_string());
            self.status.insert(name.to_string(), AppStatus::Queued);
        }
    }

    /// Launch a windowed app in the background, streaming its log to the panel.
    pub fn spawn_run(&mut self, command: Command, name: String, cargo: bool) {
        match Job::spawn(command, name.clone(), JobKind::Run { cargo }, &self.log) {
            Ok(job) => {
                self.status.insert(name.clone(), AppStatus::Running);
                self.running.push(job);
                self.console_visible = true;
                self.console_scroll = 0;
                push_line(&self.log, format!("[{name}] launched"));
            }
            Err(e) => push_line(&self.log, format!("[{name}] failed to launch: {e}")),
        }
    }

    pub fn toggle_console(&mut self) {
        self.console_visible = !self.console_visible;
    }

    pub fn scroll(&mut self, delta: i32) {
        self.console_scroll = (self.console_scroll as i32 + delta).max(0) as u16;
    }

    /// Kill in-flight builds and drop the queue; leave running apps alone.
    pub fn cancel_builds(&mut self) {
        self.queue.clear();
        let mut keep = Vec::new();
        for job in self.running.drain(..) {
            if matches!(job.kind, JobKind::Build) {
                self.status.remove(&job.name);
                push_line(&self.log, format!("[{}] build canceled", job.name));
                job.kill();
            } else {
                keep.push(job);
            }
        }
        self.running = keep;
        // Drop any still-queued statuses.
        self.status
            .retain(|_, s| !matches!(s, AppStatus::Queued | AppStatus::Building));
    }

    /// Kill everything on shutdown so no child outlives the launcher.
    pub fn kill_all(&mut self) {
        self.queue.clear();
        for job in self.running.drain(..) {
            job.kill();
        }
        self.status.clear();
    }

    /// Reap finished jobs and start the next queued build if the cargo slot is
    /// free. Returns any builds that just finished so the caller can refresh.
    pub fn pump(&mut self) -> Vec<BuildDone> {
        let mut done = Vec::new();
        let mut still = Vec::new();
        for mut job in std::mem::take(&mut self.running) {
            match job.child.try_wait() {
                Ok(Some(exit)) => {
                    let success = exit.success();
                    for r in job.readers.drain(..) {
                        let _ = r.join();
                    }
                    match job.kind {
                        JobKind::Build => {
                            push_line(
                                &self.log,
                                format!(
                                    "[{}] build {}",
                                    job.name,
                                    if success { "OK" } else { "FAILED" }
                                ),
                            );
                            if success {
                                self.status.remove(&job.name);
                            } else {
                                self.status.insert(job.name.clone(), AppStatus::BuildFailed);
                                self.console_visible = true;
                            }
                            done.push(BuildDone {
                                name: job.name.clone(),
                                success,
                            });
                        }
                        JobKind::Run { .. } => {
                            push_line(&self.log, format!("[{}] exited ({exit})", job.name));
                            self.status.remove(&job.name);
                        }
                    }
                }
                _ => still.push(job),
            }
        }
        self.running = still;

        if !self.cargo_active() {
            if let Some(name) = self.queue.pop_front() {
                let mut cmd = Command::new("cargo");
                cmd.args(["build", "-p", &name]).current_dir(&self.root);
                match Job::spawn(cmd, name.clone(), JobKind::Build, &self.log) {
                    Ok(job) => {
                        self.status.insert(name.clone(), AppStatus::Building);
                        self.running.push(job);
                        push_line(&self.log, format!("[{name}] building…"));
                    }
                    Err(e) => {
                        self.status.insert(name.clone(), AppStatus::BuildFailed);
                        push_line(&self.log, format!("[{name}] failed to start build: {e}"));
                    }
                }
            }
        }
        done
    }
}
