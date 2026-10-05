//! Owns the sing-box child process.
//!
//! All state lives in a single task; clients talk to it through
//! [`SupervisorHandle`] and read status snapshots from a watch channel, so a
//! slow operation (stop timeout, config check) never blocks `status`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant as Deadline, timeout};

use super::github::parse_version_output;
use super::logs::LogHub;
use super::process::{self, describe, first_line, pipe_to_logs, sleep_until_opt, wait_child};
use super::singbox_config::discover_clash_api;
use crate::config::{DaemonConfig, RestartPolicy};
use crate::protocol::{ClashApi, CoreState, LogSource, Response, Status};
use crate::util::{now_unix, strip_ansi};

/// A process still alive after this long is considered successfully started.
const STARTUP_GRACE: Duration = Duration::from_millis(1500);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum Op {
    Start,
    Stop,
    Restart,
    Reload,
    Check,
    /// Point `core.binary` at another core build and restart sing-box onto it.
    Activate {
        target: PathBuf,
        label: String,
        /// Switch even if the new core rejects the configuration.
        force: bool,
        restart: bool,
        /// Core (binary, label) to return to if the new one fails to start.
        fallback: Option<(PathBuf, String)>,
    },
}

enum Message {
    Op(Op, oneshot::Sender<Response>),
    Shutdown(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct SupervisorHandle {
    tx: mpsc::Sender<Message>,
    status: watch::Receiver<Status>,
}

impl SupervisorHandle {
    pub async fn request(&self, op: Op) -> Response {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Message::Op(op, reply)).await.is_err() {
            return Response::error("daemon is shutting down");
        }
        rx.await
            .unwrap_or_else(|_| Response::error("daemon is shutting down"))
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// Stops sing-box and ends the supervisor task.
    pub async fn shutdown(&self) {
        let (done, rx) = oneshot::channel();
        if self.tx.send(Message::Shutdown(done)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

pub struct Supervisor {
    config: DaemonConfig,
    logs: Arc<LogHub>,
    rx: mpsc::Receiver<Message>,
    status_tx: watch::Sender<Status>,
    child: Option<Child>,
    pipes: Vec<JoinHandle<()>>,
    state: CoreState,
    /// Whether the user wants sing-box running (drives automatic restarts).
    want_running: bool,
    started_at: Option<(Instant, u64)>,
    restarts: u32,
    last_exit: Option<String>,
    backoff: Duration,
    restart_at: Option<Deadline>,
    core_version: Option<String>,
    clash_api: Option<ClashApi>,
    daemon_started_at: u64,
}

impl Supervisor {
    /// `startup_gate` delays the automatic start until it turns true (or 90s pass).
    pub fn spawn(
        config: DaemonConfig,
        logs: Arc<LogHub>,
        startup_gate: watch::Receiver<bool>,
    ) -> (SupervisorHandle, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(32);
        let backoff = Duration::from_millis(config.restart.initial_backoff_ms.max(100));
        let mut supervisor = Supervisor {
            config,
            logs,
            rx,
            status_tx: watch::Sender::new(placeholder_status()),
            child: None,
            pipes: Vec::new(),
            state: CoreState::Stopped,
            want_running: false,
            started_at: None,
            restarts: 0,
            last_exit: None,
            backoff,
            restart_at: None,
            core_version: None,
            clash_api: None,
            daemon_started_at: now_unix(),
        };
        supervisor.clash_api =
            discover_clash_api(&supervisor.config.core, &supervisor.config.clash_api);
        supervisor.publish();
        let handle = SupervisorHandle {
            tx,
            status: supervisor.status_tx.subscribe(),
        };
        (handle, tokio::spawn(supervisor.run(startup_gate)))
    }

    async fn run(mut self, mut startup_gate: watch::Receiver<bool>) {
        self.core_version = self.probe_version().await;
        self.publish();
        let missing = self.missing_prerequisite();
        if let Some(reason) = &missing
            && self.config.core.auto_start
        {
            // Fresh installs: nothing to retry until the user acts.
            self.logs.info(format!("not starting sing-box: {reason}"));
        }
        if self.config.core.auto_start && missing.is_none() {
            if !*startup_gate.borrow() {
                self.logs
                    .info("waiting for components before starting sing-box");
                let _ = timeout(
                    Duration::from_secs(90),
                    startup_gate.wait_for(|ready| *ready),
                )
                .await;
            }
            // Unlike an explicit `start`, a failed boot start is retried with
            // backoff (e.g. network or a provider not reachable yet).
            self.want_running = true;
            if let Err(err) = self.launch().await {
                self.logs.warn(format!(
                    "auto start failed: {}",
                    first_line(&format!("{err:#}"))
                ));
                if self.config.restart.policy != RestartPolicy::Never {
                    self.schedule_restart();
                } else {
                    self.want_running = false;
                }
            }
        }
        loop {
            let restart_at = self.restart_at;
            tokio::select! {
                message = self.rx.recv() => match message {
                    Some(Message::Op(op, reply)) => self.handle(op, reply).await,
                    Some(Message::Shutdown(done)) => {
                        self.want_running = false;
                        self.restart_at = None;
                        self.stop_child().await;
                        let _ = done.send(());
                        return;
                    }
                    None => {
                        self.stop_child().await;
                        return;
                    }
                },
                status = wait_child(&mut self.child) => self.on_exit(status),
                _ = sleep_until_opt(restart_at) => self.restart_from_backoff().await,
            }
        }
    }

    async fn handle(&mut self, op: Op, reply: oneshot::Sender<Response>) {
        let response = match op {
            Op::Start => self.start().await,
            Op::Stop => self.stop().await,
            Op::Restart => self.restart().await,
            Op::Reload => self.reload().await,
            Op::Check => match self.check().await {
                Ok(()) => Response::done("configuration is valid"),
                Err(err) => Response::error(format!("{err:#}")),
            },
            Op::Activate {
                target,
                label,
                force,
                restart,
                fallback,
            } => {
                self.activate(&target, &label, force, restart, fallback)
                    .await
            }
        };
        let _ = reply.send(response);
    }

    /// Why sing-box cannot start at all (binary or configuration absent).
    fn missing_prerequisite(&self) -> Option<String> {
        let core = &self.config.core;
        if !core.binary.exists() {
            return Some(format!(
                "{} is not installed; run `singbox-board update`",
                core.binary.display()
            ));
        }
        let missing = core
            .config
            .iter()
            .map(|path| core.resolve(path))
            .find(|path| !path.exists());
        missing.map(|path| format!("configuration {} does not exist", path.display()))
    }

    fn publish(&self) {
        let now = Deadline::now();
        self.status_tx.send_replace(Status {
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            daemon_pid: std::process::id(),
            daemon_started_at: self.daemon_started_at,
            state: self.state,
            pid: self.child.as_ref().and_then(Child::id),
            started_at: self.started_at.map(|(_, unix)| unix),
            restarts: self.restarts,
            last_exit: self.last_exit.clone(),
            next_restart_at: self
                .restart_at
                .map(|at| now_unix() + at.saturating_duration_since(now).as_secs()),
            core_version: self.core_version.clone(),
            binary: self.config.core.binary.display().to_string(),
            args: self.config.core.run_args(),
            clash_api: self.clash_api.clone(),
            update_in_progress: false,
            setup_required: false,
            components: Vec::new(),
            active_core: None,
        });
    }

    fn set_state(&mut self, state: CoreState) {
        self.state = state;
        self.publish();
    }

    async fn start(&mut self) -> Response {
        if let Some(pid) = self.child.as_ref().and_then(Child::id) {
            return Response::error(format!("sing-box is already running (pid {pid})"));
        }
        self.restart_at = None;
        self.want_running = true;
        self.reset_backoff();
        match self.launch().await {
            Ok(pid) => Response::done(format!("sing-box started (pid {pid})")),
            Err(err) => {
                self.want_running = false;
                Response::error(format!("{err:#}"))
            }
        }
    }

    async fn stop(&mut self) -> Response {
        self.want_running = false;
        let cancelled_restart = self.restart_at.take().is_some();
        match self.stop_child().await {
            Some(exit) => Response::done(format!("sing-box stopped ({exit})")),
            None => {
                self.set_state(CoreState::Stopped);
                Response::done(if cancelled_restart {
                    "automatic restart cancelled"
                } else {
                    "sing-box is not running"
                })
            }
        }
    }

    async fn restart(&mut self) -> Response {
        if self.child.is_some() && self.config.core.check_before_start {
            // Do not take a working instance down for a broken configuration.
            if let Err(err) = self.check().await {
                return Response::error(format!(
                    "configuration check failed, not restarting:\n{err:#}"
                ));
            }
        }
        self.stop_child().await;
        self.start().await
    }

    async fn reload(&mut self) -> Response {
        let Some(pid) = self.child.as_ref().and_then(Child::id) else {
            return Response::error("sing-box is not running");
        };
        if let Err(err) = self.check().await {
            return Response::error(format!(
                "configuration check failed, not reloading:\n{err:#}"
            ));
        }
        if let Err(err) = kill(Pid::from_raw(pid as i32), Signal::SIGHUP) {
            return Response::error(format!("send SIGHUP to {pid}: {err}"));
        }
        self.clash_api = discover_clash_api(&self.config.core, &self.config.clash_api);
        self.publish();
        self.logs.info("configuration reloaded (SIGHUP)");
        Response::done("configuration reloaded")
    }

    /// Runs `sing-box check` with the same flags as `run`.
    async fn check(&self) -> Result<()> {
        let binary = self.config.core.binary.clone();
        if !binary.exists() {
            bail!(
                "{} not found; install it with `singbox-board update`",
                binary.display()
            );
        }
        self.check_with(&binary).await
    }

    /// `sing-box check` of the configured files using a specific core build.
    async fn check_with(&self, binary: &Path) -> Result<()> {
        let core = &self.config.core;
        let output = timeout(
            CHECK_TIMEOUT,
            Command::new(binary)
                .args(core.check_args())
                .envs(&core.env)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| anyhow!("sing-box check timed out"))?
        .with_context(|| format!("run {} check", binary.display()))?;
        if output.status.success() {
            return Ok(());
        }
        let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        let text = strip_ansi(text.trim());
        if text.is_empty() {
            bail!("sing-box check failed ({})", describe(&Ok(output.status)));
        }
        bail!("{text}")
    }

    /// Spawns sing-box and waits [`STARTUP_GRACE`] to catch immediate failures.
    async fn launch(&mut self) -> Result<u32> {
        let result = self.try_launch().await;
        if let Err(err) = &result {
            self.last_exit = Some(first_line(&format!("{err:#}")));
            self.set_state(CoreState::Failed);
        }
        result
    }

    async fn try_launch(&mut self) -> Result<u32> {
        let core = self.config.core.clone();
        self.set_state(CoreState::Starting);
        if core.check_before_start {
            self.check()
                .await
                .map_err(|err| anyhow!("configuration check failed:\n{err:#}"))?;
        } else if !core.binary.exists() {
            bail!(
                "{} not found; install it with `singbox-board update`",
                core.binary.display()
            );
        }
        if let Some(dir) = &core.working_dir {
            tokio::fs::create_dir_all(dir)
                .await
                .with_context(|| format!("create working directory {}", dir.display()))?;
        }
        self.clash_api = discover_clash_api(&core, &self.config.clash_api);

        let mut command = Command::new(&core.binary);
        command
            .args(core.run_args())
            .envs(&core.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process::isolate(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn {}", core.binary.display()))?;
        let pid = child.id().unwrap_or_default();
        self.pipes.clear();
        if let Some(stdout) = child.stdout.take() {
            self.pipes
                .push(pipe_to_logs(stdout, self.logs.clone(), LogSource::Core));
        }
        if let Some(stderr) = child.stderr.take() {
            self.pipes
                .push(pipe_to_logs(stderr, self.logs.clone(), LogSource::Core));
        }
        self.logs.info(format!("sing-box started (pid {pid})"));

        if let Ok(status) = timeout(STARTUP_GRACE, child.wait()).await {
            let exit = describe(&status);
            self.drain_pipes().await;
            let recent = self.logs.recent_lines(LogSource::Core, 8).join("\n");
            self.logs
                .warn(format!("sing-box exited during startup: {exit}"));
            bail!("sing-box exited during startup ({exit})\n{recent}");
        }
        self.child = Some(child);
        self.started_at = Some((Instant::now(), now_unix()));
        self.set_state(CoreState::Running);
        Ok(pid)
    }

    /// Waits briefly for the output readers so the final lines are buffered.
    async fn drain_pipes(&mut self) {
        for pipe in self.pipes.drain(..) {
            let _ = timeout(Duration::from_secs(1), pipe).await;
        }
    }

    /// SIGTERM, then SIGKILL after the stop timeout. Returns the exit description.
    async fn stop_child(&mut self) -> Option<String> {
        let mut child = self.child.take()?;
        self.set_state(CoreState::Stopping);
        if let Some(pid) = child.id() {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
        let grace = Duration::from_secs(self.config.core.stop_timeout_secs.max(1));
        let status = match timeout(grace, child.wait()).await {
            Ok(status) => status,
            Err(_) => {
                self.logs.warn(format!(
                    "sing-box did not exit within {}s, sending SIGKILL",
                    grace.as_secs()
                ));
                let _ = child.start_kill();
                child.wait().await
            }
        };
        let exit = describe(&status);
        self.drain_pipes().await;
        self.logs.info(format!("sing-box stopped ({exit})"));
        self.started_at = None;
        self.last_exit = Some(exit.clone());
        self.set_state(CoreState::Stopped);
        Some(exit)
    }

    fn on_exit(&mut self, status: io::Result<ExitStatus>) {
        self.child = None;
        let exit = describe(&status);
        let failed = !matches!(&status, Ok(status) if status.success());
        let ran_for = self.started_at.take().map(|(at, _)| at.elapsed());
        self.last_exit = Some(exit.clone());
        self.logs
            .warn(format!("sing-box exited unexpectedly ({exit})"));
        if ran_for.is_some_and(|d| d >= Duration::from_secs(self.config.restart.stable_after_secs))
        {
            self.reset_backoff();
        }
        let restart = self.want_running
            && match self.config.restart.policy {
                RestartPolicy::Always => true,
                RestartPolicy::OnFailure => failed,
                RestartPolicy::Never => false,
            };
        if restart {
            self.schedule_restart();
        } else {
            self.want_running = false;
            self.set_state(if failed {
                CoreState::Failed
            } else {
                CoreState::Stopped
            });
        }
    }

    fn reset_backoff(&mut self) {
        self.backoff = Duration::from_millis(self.config.restart.initial_backoff_ms.max(100));
    }

    fn schedule_restart(&mut self) {
        let delay = self.backoff;
        let max = Duration::from_secs(self.config.restart.max_backoff_secs.max(1));
        self.backoff = (self.backoff * 2).min(max);
        self.restart_at = Some(Deadline::now() + delay);
        self.logs.info(format!(
            "restarting sing-box in {:.1}s",
            delay.as_secs_f64()
        ));
        self.set_state(CoreState::Backoff);
    }

    async fn restart_from_backoff(&mut self) {
        self.restart_at = None;
        self.restarts += 1;
        if let Err(err) = self.launch().await {
            self.logs.warn(format!(
                "restart failed: {}",
                first_line(&format!("{err:#}"))
            ));
            if self.want_running {
                self.schedule_restart();
            }
        }
    }

    async fn probe_version(&self) -> Option<String> {
        let binary = &self.config.core.binary;
        if !binary.exists() {
            self.logs.warn(format!(
                "{} not found; install it with `singbox-board update`",
                binary.display()
            ));
            return None;
        }
        let output = timeout(
            Duration::from_secs(10),
            Command::new(binary)
                .arg("version")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        parse_version_output(&String::from_utf8_lossy(&output.stdout))
    }

    /// Switches `core.binary` to `target` (an atomic symlink swap). The new
    /// build must accept the configuration unless `force` is set; a running
    /// sing-box is restarted onto it when `restart` is set.
    async fn activate(
        &mut self,
        target: &Path,
        label: &str,
        force: bool,
        restart: bool,
        fallback: Option<(PathBuf, String)>,
    ) -> Response {
        if !target.is_file() {
            return Response::error(format!("{} does not exist", target.display()));
        }
        let has_config = self
            .missing_prerequisite()
            .is_none_or(|m| !m.starts_with("configuration"));
        if !force
            && has_config
            && let Err(err) = self.check_with(target).await
        {
            return Response::error(format!(
                "{label} rejects the current configuration, not switching (force to switch anyway):\n{err:#}"
            ));
        }
        let binary = self.config.core.binary.clone();
        if let Err(err) = point_symlink(&binary, target) {
            return Response::error(format!("switch {}: {err:#}", binary.display()));
        }
        let previous = self.core_version.take();
        self.core_version = self.probe_version().await;
        self.publish();
        self.logs.info(format!(
            "core switched to {label} (was {})",
            previous.as_deref().unwrap_or("none")
        ));
        // Also bring sing-box back when it died on the previous core.
        let start =
            self.child.is_some() || matches!(self.state, CoreState::Failed | CoreState::Backoff);
        if !restart || !start {
            return Response::done(format!("switched to {label}"));
        }
        self.stop_child().await;
        self.restart_at = None;
        self.reset_backoff();
        self.want_running = true;
        let err = match self.launch().await {
            Ok(pid) => {
                return Response::done(format!(
                    "switched to {label} and restarted sing-box (pid {pid})"
                ));
            }
            Err(err) => err,
        };
        // `check` cannot catch everything (e.g. deprecations that only fail at
        // run time), so return to the core that was working.
        if !force
            && let Some((previous, previous_label)) = fallback
            && point_symlink(&binary, &previous).is_ok()
        {
            self.core_version = self.probe_version().await;
            self.logs.warn(format!(
                "{label} failed to start; rolled back to {previous_label}"
            ));
            self.restart_at = None;
            self.reset_backoff();
            let restored = match self.launch().await {
                Ok(pid) => format!("{previous_label} is running again (pid {pid})"),
                Err(_) => {
                    self.want_running = false;
                    format!("{previous_label} did not start either")
                }
            };
            return Response::error(format!(
                "{label} failed to start, rolled back: {restored}\n{err:#}"
            ));
        }
        self.want_running = false;
        Response::error(format!(
            "switched to {label}, but sing-box failed to start: {err:#}\nswitch back with `singbox-board core use <id>`"
        ))
    }
}

fn placeholder_status() -> Status {
    Status {
        daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
        daemon_pid: std::process::id(),
        daemon_started_at: now_unix(),
        state: CoreState::Stopped,
        pid: None,
        started_at: None,
        restarts: 0,
        last_exit: None,
        next_restart_at: None,
        core_version: None,
        binary: String::new(),
        args: Vec::new(),
        clash_api: None,
        update_in_progress: false,
        setup_required: false,
        components: Vec::new(),
        active_core: None,
    }
}

/// Atomically makes `link` a symlink to `target` (replacing a file or link).
fn point_symlink(link: &Path, target: &Path) -> Result<()> {
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = link.as_os_str().to_owned();
    tmp.push(".switching");
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    std::fs::rename(&tmp, link).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}
