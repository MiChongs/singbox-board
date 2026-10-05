//! Owns the sing-box child process.
//!
//! All state lives in a single task; clients talk to it through
//! [`SupervisorHandle`] and read status snapshots from a watch channel, so a
//! slow operation (stop timeout, config check) never blocks `status`.

use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant as Deadline, sleep_until, timeout};

use super::logs::LogHub;
use super::singbox_config::discover_clash_api;
use super::updater::{self, StagedBinary, Updater};
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
    CheckUpdate,
    Update { tag: Option<String>, force: bool },
}

enum UpdateOutcome {
    UpToDate(String),
    Staged(StagedBinary),
}

enum Message {
    Op(Op, oneshot::Sender<Response>),
    UpdateFinished(Result<UpdateOutcome>, oneshot::Sender<Response>),
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
    tx: mpsc::Sender<Message>,
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
    updating: bool,
    daemon_started_at: u64,
}

impl Supervisor {
    pub fn spawn(config: DaemonConfig, logs: Arc<LogHub>) -> (SupervisorHandle, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(32);
        let backoff = Duration::from_millis(config.restart.initial_backoff_ms.max(100));
        let mut supervisor = Supervisor {
            config,
            logs,
            rx,
            tx: tx.clone(),
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
            updating: false,
            daemon_started_at: now_unix(),
        };
        supervisor.clash_api =
            discover_clash_api(&supervisor.config.core, &supervisor.config.clash_api);
        supervisor.publish();
        let handle = SupervisorHandle {
            tx,
            status: supervisor.status_tx.subscribe(),
        };
        (handle, tokio::spawn(supervisor.run()))
    }

    async fn run(mut self) {
        self.core_version = self.probe_version().await;
        self.publish();
        if self.config.core.auto_start
            && let Response::Error { message } = self.start().await
        {
            self.logs.warn(format!("auto start failed: {message}"));
        }
        loop {
            let restart_at = self.restart_at;
            tokio::select! {
                message = self.rx.recv() => match message {
                    Some(Message::Op(op, reply)) => self.handle(op, reply).await,
                    Some(Message::UpdateFinished(result, reply)) => {
                        let response = self.finish_update(result).await;
                        let _ = reply.send(response);
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
                Ok(()) => Response::done("configuration is valid"),
                Err(err) => Response::error(format!("{err:#}")),
            },
            Op::CheckUpdate => return self.spawn_check_update(reply),
            Op::Update { tag, force } => return self.spawn_update(tag, force, reply),
        };
        let _ = reply.send(response);
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
            update_in_progress: self.updating,
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
        let core = &self.config.core;
        if !core.binary.exists() {
            bail!(
                "{} not found; install it with `singbox-board update`",
                core.binary.display()
            );
        }
        let output = timeout(
            CHECK_TIMEOUT,
            Command::new(&core.binary)
                .args(core.check_args())
                .envs(&core.env)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| anyhow!("sing-box check timed out"))?
        .with_context(|| format!("run {} check", core.binary.display()))?;
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
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            // Keep terminal signals (Ctrl-C on a foreground daemon) away from
            // sing-box; the daemon stops it in an orderly way instead.
            .process_group(0);
        // SAFETY: prctl is async-signal-safe and touches no shared state.
        unsafe {
            command.pre_exec(|| {
                // Do not leave an unmanaged sing-box behind if the daemon dies.
                nix::sys::prctl::set_pdeathsig(Signal::SIGTERM).map_err(io::Error::from)
            });
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn {}", core.binary.display()))?;
        let pid = child.id().unwrap_or_default();
        self.pipes.clear();
        if let Some(stdout) = child.stdout.take() {
            self.pipes.push(pipe_to_logs(stdout, self.logs.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            self.pipes.push(pipe_to_logs(stderr, self.logs.clone()));
        }
        self.logs.info(format!("sing-box started (pid {pid})"));

        if let Ok(status) = timeout(STARTUP_GRACE, child.wait()).await {
            let exit = describe(&status);
            self.drain_pipes().await;
            let recent = self.logs.recent_core_lines(8).join("\n");
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
        updater::parse_version_output(&String::from_utf8_lossy(&output.stdout))
    }

    fn spawn_check_update(&self, reply: oneshot::Sender<Response>) {
        let config = self.config.update.clone();
        let current = self.core_version.clone();
        tokio::spawn(async move {
            let result = async {
                let updater = Updater::new(config)?;
                let (_, info) = updater.check(None, current.as_deref()).await?;
                anyhow::Ok(info)
            }
            .await;
            let _ = reply.send(match result {
                Ok(info) => Response::UpdateInfo(info),
                Err(err) => Response::error(format!("{err:#}")),
            });
        });
    }

    fn spawn_update(&mut self, tag: Option<String>, force: bool, reply: oneshot::Sender<Response>) {
        if self.updating {
            let _ = reply.send(Response::error("an update is already in progress"));
            return;
        }
        let updater = match Updater::new(self.config.update.clone()) {
            Ok(updater) => updater,
            Err(err) => {
                let _ = reply.send(Response::error(format!("{err:#}")));
                return;
            }
        };
        self.updating = true;
        self.publish();
        let current = self.core_version.clone();
        let binary = self.config.core.binary.clone();
        let logs = self.logs.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = async {
                let (release, info) = updater.check(tag.as_deref(), current.as_deref()).await?;
                if !info.update_available && !force {
                    return Ok(UpdateOutcome::UpToDate(info.latest));
                }
                logs.info(format!(
                    "downloading {} from {}",
                    info.asset, release.tag_name
                ));
                let staged = updater.stage(&release, &info.asset, &binary).await?;
                logs.info(format!("verified sing-box {} (sha256 ok)", staged.version));
                Ok(UpdateOutcome::Staged(staged))
            }
            .await;
            let _ = tx.send(Message::UpdateFinished(result, reply)).await;
        });
    }

    async fn finish_update(&mut self, result: Result<UpdateOutcome>) -> Response {
        self.updating = false;
        let response = match result {
            Err(err) => {
                self.logs.warn(format!("update failed: {err:#}"));
                Response::error(format!("update failed: {err:#}"))
            }
            Ok(UpdateOutcome::UpToDate(version)) => {
                Response::done(format!("sing-box {version} is already up to date"))
            }
            Ok(UpdateOutcome::Staged(staged)) => self.install(staged).await,
        };
        self.publish();
        response
    }

    async fn install(&mut self, staged: StagedBinary) -> Response {
        let binary = self.config.core.binary.clone();
        if let Err(err) = updater::install(&staged, &binary) {
            let _ = std::fs::remove_file(&staged.path);
            return Response::error(format!("install failed: {err:#}"));
        }
        let previous = self.core_version.replace(staged.version.clone());
        self.logs.info(format!(
            "installed sing-box {} (was {})",
            staged.version,
            previous.as_deref().unwrap_or("not installed")
        ));
        if self.child.is_none() || !self.config.update.restart_after_update {
            return Response::done(format!("installed sing-box {}", staged.version));
        }
        self.stop_child().await;
        self.want_running = true;
        match self.launch().await {
            Ok(pid) => Response::done(format!(
                "installed sing-box {} and restarted it (pid {pid})",
                staged.version
            )),
            Err(err) => {
                self.want_running = false;
                Response::error(format!(
                    "installed sing-box {} but it failed to start: {err:#}\nthe previous binary is kept as {}.bak",
                    staged.version,
                    binary.display()
                ))
            }
        }
    }
}

async fn wait_child(child: &mut Option<Child>) -> io::Result<ExitStatus> {
    match child {
        Some(child) => child.wait().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until_opt(deadline: Option<Deadline>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn pipe_to_logs<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    logs: Arc<LogHub>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => logs.push(LogSource::Core, &strip_ansi(&String::from_utf8_lossy(&buf))),
            }
        }
    })
}

fn describe(status: &io::Result<ExitStatus>) -> String {
    match status {
        Ok(status) => match (status.code(), status.signal()) {
            (Some(code), _) => format!("exit code {code}"),
            (None, Some(signal)) => match Signal::try_from(signal) {
                Ok(signal) => format!("killed by {}", signal.as_str()),
                Err(_) => format!("killed by signal {signal}"),
            },
            _ => "unknown exit status".to_owned(),
        },
        Err(err) => format!("wait failed: {err}"),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .trim_end_matches(':')
        .to_owned()
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
    }
}
