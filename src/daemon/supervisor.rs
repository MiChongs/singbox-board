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
#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant as Deadline, timeout};

use super::github::parse_version_output;
use super::logs::LogHub;
use super::process::{self, describe, first_line, pipe_to_logs, sleep_until_opt, wait_child};
use super::singbox_config::discover_clash_api;
use crate::config::{CoreConfig, DaemonConfig, RestartPolicy};
use crate::i18n::{self, Lang, fl, fl_log};
use crate::protocol::{ClashApi, CoreState, LogSource, Response, Status};
use crate::util::{error_chain, launch_path, now_unix, point_symlink, strip_ansi};

/// A process still alive after this long is considered successfully started.
const STARTUP_GRACE: Duration = Duration::from_millis(1500);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// How a core build is named in a reply and in the daemon's log, which may
/// use different languages.
#[derive(Debug, Clone)]
pub struct CoreLabel {
    pub reply: String,
    pub log: String,
}

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
        label: CoreLabel,
        /// Switch even if the new core rejects the configuration.
        force: bool,
        restart: bool,
        /// Core (binary, label) to return to if the new one fails to start.
        fallback: Option<(PathBuf, CoreLabel)>,
    },
    /// Point the configuration file at a stored profile and restart sing-box onto it.
    SwitchConfig {
        target: PathBuf,
        label: CoreLabel,
        /// Switch even if sing-box rejects the profile.
        force: bool,
        /// Profile (file, label) to return to if the new one fails to start.
        fallback: Option<(PathBuf, CoreLabel)>,
    },
}

enum Message {
    /// An operation and the language to answer in.
    Op(Op, Lang, oneshot::Sender<Response>),
    Shutdown(oneshot::Sender<()>),
}

/// Why sing-box cannot start at all.
enum Missing {
    Binary(PathBuf),
    Config(PathBuf),
}

impl Missing {
    fn message(&self) -> String {
        match self {
            Missing::Binary(path) => fl!(
                "supervisor-binary-missing",
                path = path.display().to_string()
            ),
            Missing::Config(path) => fl!(
                "supervisor-config-missing",
                path = path.display().to_string()
            ),
        }
    }
}

#[derive(Clone)]
pub struct SupervisorHandle {
    tx: mpsc::Sender<Message>,
    status: watch::Receiver<Status>,
}

impl SupervisorHandle {
    pub async fn request(&self, op: Op) -> Response {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Message::Op(op, i18n::current(), reply))
            .await
            .is_err()
        {
            return Response::error(fl!("daemon-shutting-down"));
        }
        rx.await
            .unwrap_or_else(|_| Response::error(fl!("daemon-shutting-down")))
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
    /// The automatic start was skipped for a missing binary or configuration;
    /// sing-box starts once a core or profile switch provides it.
    deferred_start: bool,
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
            deferred_start: false,
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
        if let Some(missing) = &missing
            && self.config.core.auto_start
        {
            // Fresh installs: nothing to retry until the user acts.
            self.logs.info(fl_log!(
                "supervisor-not-starting",
                reason = missing.message()
            ));
            self.deferred_start = true;
        }
        if self.config.core.auto_start && missing.is_none() {
            if !*startup_gate.borrow() {
                self.logs.info(fl_log!("supervisor-waiting-components"));
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
                self.logs.warn(fl_log!(
                    "supervisor-auto-start-failed",
                    error = first_line(&error_chain(&err))
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
                    Some(Message::Op(op, lang, reply)) => {
                        i18n::scope(lang, self.handle(op, reply)).await
                    }
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
                Ok(()) => Response::done(fl!("supervisor-config-valid")),
                Err(err) => Response::error(error_chain(&err)),
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
            Op::SwitchConfig {
                target,
                label,
                force,
                fallback,
            } => self.switch_config(&target, &label, force, fallback).await,
        };
        let _ = reply.send(response);
    }

    /// Why sing-box cannot start at all (binary or configuration absent).
    fn missing_prerequisite(&self) -> Option<Missing> {
        let core = &self.config.core;
        if !core.binary.exists() {
            return Some(Missing::Binary(core.binary.clone()));
        }
        core.config
            .iter()
            .map(|path| core.resolve(path))
            .find(|path| !path.exists())
            .map(Missing::Config)
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
            active_profile: None,
            containers: None,
        });
    }

    fn set_state(&mut self, state: CoreState) {
        self.state = state;
        self.publish();
    }

    async fn start(&mut self) -> Response {
        if let Some(pid) = self.child.as_ref().and_then(Child::id) {
            return Response::error(fl!("supervisor-already-running", pid = pid.to_string()));
        }
        self.restart_at = None;
        self.want_running = true;
        self.deferred_start = false;
        self.reset_backoff();
        match self.launch().await {
            Ok(pid) => Response::done(fl!("supervisor-started", pid = pid.to_string())),
            Err(err) => {
                self.want_running = false;
                Response::error(error_chain(&err))
            }
        }
    }

    async fn stop(&mut self) -> Response {
        self.want_running = false;
        self.deferred_start = false;
        let cancelled_restart = self.restart_at.take().is_some();
        match self.stop_child().await {
            Some(exit) => Response::done(fl!("supervisor-stopped", exit = exit)),
            None => {
                self.set_state(CoreState::Stopped);
                Response::done(if cancelled_restart {
                    fl!("supervisor-restart-cancelled")
                } else {
                    fl!("supervisor-not-running")
                })
            }
        }
    }

    async fn restart(&mut self) -> Response {
        if self.child.is_some() && self.config.core.check_before_start {
            // Do not take a working instance down for a broken configuration.
            if let Err(err) = self.check().await {
                return Response::error(format!(
                    "{}\n{}",
                    fl!("supervisor-check-failed-restart"),
                    error_chain(&err)
                ));
            }
        }
        self.stop_child().await;
        self.start().await
    }

    /// sing-box on Windows has no SIGHUP to reload on, so a reload is a
    /// restart there (after the same check).
    #[cfg(windows)]
    async fn reload(&mut self) -> Response {
        if self.child.is_none() {
            return Response::error(fl!("supervisor-not-running"));
        }
        if let Err(err) = self.check().await {
            return Response::error(format!(
                "{}\n{}",
                fl!("supervisor-check-failed-reload"),
                error_chain(&err)
            ));
        }
        self.logs.info(fl_log!("win-supervisor-reload-restarts"));
        self.stop_child().await;
        self.start().await
    }

    #[cfg(unix)]
    async fn reload(&mut self) -> Response {
        let Some(pid) = self.child.as_ref().and_then(Child::id) else {
            return Response::error(fl!("supervisor-not-running"));
        };
        if let Err(err) = self.check().await {
            return Response::error(format!(
                "{}\n{}",
                fl!("supervisor-check-failed-reload"),
                error_chain(&err)
            ));
        }
        if let Err(err) = kill(Pid::from_raw(pid as i32), Signal::SIGHUP) {
            return Response::error(fl!(
                "supervisor-sighup-failed",
                pid = pid.to_string(),
                error = err.to_string()
            ));
        }
        self.clash_api = discover_clash_api(&self.config.core, &self.config.clash_api);
        self.publish();
        self.logs.info(fl_log!("supervisor-reloaded-log"));
        Response::done(fl!("supervisor-reloaded"))
    }

    /// Runs `sing-box check` with the same flags as `run`.
    async fn check(&self) -> Result<()> {
        let binary = self.config.core.binary.clone();
        if !binary.exists() {
            bail!(fl!(
                "supervisor-binary-not-found",
                path = binary.display().to_string()
            ));
        }
        self.check_with(&binary).await
    }

    /// `sing-box check` of the configured files using a specific core build.
    async fn check_with(&self, binary: &Path) -> Result<()> {
        run_check(&self.config.core, binary, self.config.core.check_args()).await
    }

    /// Spawns sing-box and waits [`STARTUP_GRACE`] to catch immediate failures.
    async fn launch(&mut self) -> Result<u32> {
        let result = self.try_launch().await;
        if let Err(err) = &result {
            self.last_exit = Some(first_line(&error_chain(err)));
            self.set_state(CoreState::Failed);
        }
        result
    }

    async fn try_launch(&mut self) -> Result<u32> {
        let core = self.config.core.clone();
        self.set_state(CoreState::Starting);
        if core.check_before_start {
            self.check().await.map_err(|err| {
                anyhow!(
                    "{}\n{}",
                    fl!("supervisor-check-failed-start"),
                    error_chain(&err)
                )
            })?;
        } else if !core.binary.exists() {
            bail!(fl!(
                "supervisor-binary-not-found",
                path = core.binary.display().to_string()
            ));
        }
        if let Some(dir) = &core.working_dir {
            tokio::fs::create_dir_all(dir)
                .await
                .with_context(|| fl!("err-create-workdir", path = dir.display().to_string()))?;
        }
        self.clash_api = discover_clash_api(&core, &self.config.clash_api);

        let mut command = Command::new(launch_path(&core.binary));
        command
            .args(core.run_args())
            .envs(&core.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process::isolate(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| fl!("err-spawn", path = core.binary.display().to_string()))?;
        process::adopt(&child);
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
        self.logs
            .info(fl_log!("supervisor-started", pid = pid.to_string()));

        if let Ok(status) = timeout(STARTUP_GRACE, child.wait()).await {
            self.drain_pipes().await;
            let recent = self.logs.recent_lines(LogSource::Core, 8).join("\n");
            self.logs.warn(fl_log!(
                "supervisor-exited-startup",
                exit = i18n::in_log_language(|| describe(&status))
            ));
            bail!(
                "{}\n{recent}",
                fl!("supervisor-exited-startup", exit = describe(&status))
            );
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
        process::request_stop(&mut child);
        let grace = Duration::from_secs(self.config.core.stop_timeout_secs.max(1));
        let status = match timeout(grace, child.wait()).await {
            Ok(status) => status,
            Err(_) => {
                #[cfg(unix)]
                let line = fl_log!("supervisor-kill", seconds = grace.as_secs());
                #[cfg(windows)]
                let line = fl_log!("win-supervisor-kill", seconds = grace.as_secs());
                self.logs.warn(line);
                let _ = child.start_kill();
                child.wait().await
            }
        };
        let exit = describe(&status);
        self.drain_pipes().await;
        self.logs.info(fl_log!(
            "supervisor-stopped",
            exit = i18n::in_log_language(|| describe(&status))
        ));
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
        self.logs.warn(fl_log!("supervisor-exited", exit = exit));
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
        self.logs.info(fl_log!(
            "supervisor-restarting-in",
            seconds = format!("{:.1}", delay.as_secs_f64())
        ));
        self.set_state(CoreState::Backoff);
    }

    async fn restart_from_backoff(&mut self) {
        self.restart_at = None;
        self.restarts += 1;
        if let Err(err) = self.launch().await {
            self.logs.warn(fl_log!(
                "supervisor-restart-failed",
                error = first_line(&error_chain(&err))
            ));
            if self.want_running {
                self.schedule_restart();
            }
        }
    }

    async fn probe_version(&self) -> Option<String> {
        let binary = &self.config.core.binary;
        if !binary.exists() {
            self.logs.warn(fl_log!(
                "supervisor-binary-not-found",
                path = binary.display().to_string()
            ));
            return None;
        }
        let output = timeout(
            Duration::from_secs(10),
            Command::new(launch_path(binary))
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
        label: &CoreLabel,
        force: bool,
        restart: bool,
        fallback: Option<(PathBuf, CoreLabel)>,
    ) -> Response {
        if !target.is_file() {
            return Response::error(fl!("err-not-found", path = target.display().to_string()));
        }
        let has_config = !matches!(self.missing_prerequisite(), Some(Missing::Config(_)));
        if !force
            && has_config
            && let Err(err) = self.check_with(target).await
        {
            return Response::error(format!(
                "{}\n{}",
                fl!("supervisor-rejects-config", label = label.reply.clone()),
                error_chain(&err)
            ));
        }
        let binary = self.config.core.binary.clone();
        if let Err(err) = point_symlink(&binary, target) {
            return Response::error(fl!(
                "supervisor-switch-failed",
                path = binary.display().to_string(),
                error = error_chain(&err)
            ));
        }
        let previous = self.core_version.take();
        self.core_version = self.probe_version().await;
        self.publish();
        self.logs.info(fl_log!(
            "supervisor-switched-log",
            label = label.log.clone(),
            previous = previous.unwrap_or_else(|| fl_log!("none"))
        ));
        // Also bring sing-box back when it died on the previous core, and
        // start it when it only waited for a core.
        let start = self.child.is_some()
            || matches!(self.state, CoreState::Failed | CoreState::Backoff)
            || (self.deferred_start && self.missing_prerequisite().is_none());
        if !restart || !start {
            return Response::done(fl!("supervisor-switched", label = label.reply.clone()));
        }
        self.stop_child().await;
        self.restart_at = None;
        self.reset_backoff();
        self.want_running = true;
        self.deferred_start = false;
        let err = match self.launch().await {
            Ok(pid) => {
                return Response::done(fl!(
                    "supervisor-switched-restarted",
                    label = label.reply.clone(),
                    pid = pid.to_string()
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
            self.logs.warn(fl_log!(
                "supervisor-rolled-back-log",
                label = label.log.clone(),
                previous = previous_label.log.clone()
            ));
            self.restart_at = None;
            self.reset_backoff();
            let restored = match self.launch().await {
                Ok(pid) => fl!(
                    "supervisor-previous-running",
                    label = previous_label.reply.clone(),
                    pid = pid.to_string()
                ),
                Err(_) => {
                    self.want_running = false;
                    fl!(
                        "supervisor-previous-failed",
                        label = previous_label.reply.clone()
                    )
                }
            };
            return Response::error(format!(
                "{}\n{}",
                fl!(
                    "supervisor-rolled-back",
                    label = label.reply.clone(),
                    restored = restored
                ),
                error_chain(&err)
            ));
        }
        self.want_running = false;
        Response::error(format!(
            "{}\n{}",
            fl!(
                "supervisor-switched-start-failed",
                label = label.reply.clone(),
                error = error_chain(&err)
            ),
            fl!("supervisor-switch-back-hint")
        ))
    }

    /// Points the configuration file at `target` (an atomic symlink swap).
    /// sing-box must accept the profile unless `force` is set; a running
    /// sing-box is restarted onto it and returns to `fallback` if it fails.
    async fn switch_config(
        &mut self,
        target: &Path,
        label: &CoreLabel,
        force: bool,
        fallback: Option<(PathBuf, CoreLabel)>,
    ) -> Response {
        let Some(slot) = self.config.core.config_slot() else {
            return Response::error(fl!("supervisor-no-config-slot"));
        };
        if !target.is_file() {
            return Response::error(fl!("err-not-found", path = target.display().to_string()));
        }
        let binary = self.config.core.binary.clone();
        if !force && binary.exists() {
            let args = self.config.core.check_args_with(Some(target));
            if let Err(err) = run_check(&self.config.core, &binary, args).await {
                return Response::error(format!(
                    "{}\n{}",
                    fl!("supervisor-rejects-profile", label = label.reply.clone()),
                    error_chain(&err)
                ));
            }
        }
        if let Err(err) = point_symlink(&slot, target) {
            return Response::error(fl!(
                "supervisor-switch-failed",
                path = slot.display().to_string(),
                error = error_chain(&err)
            ));
        }
        self.clash_api = discover_clash_api(&self.config.core, &self.config.clash_api);
        self.publish();
        self.logs.info(fl_log!(
            "supervisor-profile-switched-log",
            label = label.log.clone()
        ));
        let start = self.child.is_some()
            || matches!(self.state, CoreState::Failed | CoreState::Backoff)
            || (self.deferred_start && self.missing_prerequisite().is_none());
        if !start {
            return Response::done(fl!(
                "supervisor-profile-switched",
                label = label.reply.clone()
            ));
        }
        let was_running = self.child.is_some();
        self.stop_child().await;
        self.restart_at = None;
        self.reset_backoff();
        self.want_running = true;
        self.deferred_start = false;
        let err = match self.launch().await {
            Ok(pid) if was_running => {
                return Response::done(fl!(
                    "supervisor-profile-restarted",
                    label = label.reply.clone(),
                    pid = pid.to_string()
                ));
            }
            Ok(pid) => {
                return Response::done(fl!(
                    "supervisor-profile-started",
                    label = label.reply.clone(),
                    pid = pid.to_string()
                ));
            }
            Err(err) => err,
        };
        if !force
            && let Some((previous, previous_label)) = fallback
            && point_symlink(&slot, &previous).is_ok()
        {
            self.clash_api = discover_clash_api(&self.config.core, &self.config.clash_api);
            self.logs.warn(fl_log!(
                "supervisor-profile-rolled-back-log",
                label = label.log.clone(),
                previous = previous_label.log.clone()
            ));
            self.restart_at = None;
            self.reset_backoff();
            let restored = match self.launch().await {
                Ok(pid) => fl!(
                    "supervisor-previous-profile-running",
                    label = previous_label.reply.clone(),
                    pid = pid.to_string()
                ),
                Err(_) => {
                    self.want_running = false;
                    fl!(
                        "supervisor-previous-profile-failed",
                        label = previous_label.reply.clone()
                    )
                }
            };
            return Response::error(format!(
                "{}\n{}",
                fl!(
                    "supervisor-profile-rolled-back",
                    label = label.reply.clone(),
                    restored = restored
                ),
                error_chain(&err)
            ));
        }
        self.want_running = false;
        Response::error(fl!(
            "supervisor-profile-start-failed",
            label = label.reply.clone(),
            error = error_chain(&err)
        ))
    }
}

/// Runs `sing-box check` with `args` using the given core build.
pub async fn run_check(core: &CoreConfig, binary: &Path, args: Vec<String>) -> Result<()> {
    let output = timeout(
        CHECK_TIMEOUT,
        Command::new(launch_path(binary))
            .args(args)
            .envs(&core.env)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!(fl!("supervisor-check-timeout")))?
    .with_context(|| {
        fl!(
            "supervisor-check-run-failed",
            path = binary.display().to_string()
        )
    })?;
    if output.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    let text = strip_ansi(text.trim());
    if text.is_empty() {
        bail!(fl!(
            "supervisor-check-failed",
            exit = describe(&Ok(output.status))
        ));
    }
    bail!("{text}")
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
        active_profile: None,
        containers: None,
    }
}
