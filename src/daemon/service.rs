//! Supervises one auxiliary process (Sub-Store, http-meta): start with a
//! given spec, stop, and restart with exponential backoff after crashes.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant as Deadline, timeout};

use super::logs::LogHub;
use super::process::{describe, first_line, isolate, pipe_to_logs, sleep_until_opt, wait_child};
use crate::config::{RestartConfig, RestartPolicy};
use crate::i18n::{self, Lang, fl, fl_log};
use crate::protocol::{CoreState, LogSource};
use crate::util::{error_chain, now_unix};

const STARTUP_GRACE: Duration = Duration::from_millis(1500);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// uid/gid to drop to before exec.
    pub user: Option<(u32, u32)>,
    /// Executable of detached grandchildren to kill after the service exits
    /// (http-meta starts mihomo in its own session).
    pub reap_exe: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ServiceRuntime {
    pub state: CoreState,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub restarts: u32,
    pub last_exit: Option<String>,
    pub next_restart_at: Option<u64>,
}

/// Requests carry the language their answer is written in.
enum Msg {
    Start(Box<ServiceSpec>, Lang, oneshot::Sender<Result<u32, String>>),
    Stop(Lang, oneshot::Sender<Option<String>>),
    Shutdown(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct ServiceHandle {
    tx: mpsc::Sender<Msg>,
    status: watch::Receiver<ServiceRuntime>,
}

impl ServiceHandle {
    /// (Re)starts the service with `spec`; restarts after crashes reuse it.
    pub async fn start(&self, spec: ServiceSpec) -> Result<u32> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Start(Box::new(spec), i18n::current(), reply))
            .await
            .map_err(|_| anyhow::anyhow!(fl!("daemon-shutting-down")))?;
        rx.await
            .map_err(|_| anyhow::anyhow!(fl!("daemon-shutting-down")))?
            .map_err(anyhow::Error::msg)
    }

    /// Stops the service; returns its exit description when it was running.
    pub async fn stop(&self) -> Option<String> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Msg::Stop(i18n::current(), reply)).await.ok()?;
        rx.await.ok().flatten()
    }

    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Msg::Shutdown(reply)).await.is_ok() {
            let _ = rx.await;
        }
    }

    pub fn runtime(&self) -> ServiceRuntime {
        self.status.borrow().clone()
    }
}

pub struct Service {
    title: &'static str,
    source: LogSource,
    logs: Arc<LogHub>,
    restart: RestartConfig,
    rx: mpsc::Receiver<Msg>,
    status_tx: watch::Sender<ServiceRuntime>,
    spec: Option<ServiceSpec>,
    child: Option<Child>,
    pipes: Vec<JoinHandle<()>>,
    state: CoreState,
    want_running: bool,
    started_at: Option<(Instant, u64)>,
    restarts: u32,
    last_exit: Option<String>,
    backoff: Duration,
    restart_at: Option<Deadline>,
}

impl Service {
    pub fn spawn(
        title: &'static str,
        source: LogSource,
        logs: Arc<LogHub>,
        restart: RestartConfig,
    ) -> ServiceHandle {
        let (tx, rx) = mpsc::channel(8);
        let status_tx = watch::Sender::new(ServiceRuntime {
            state: CoreState::Stopped,
            pid: None,
            started_at: None,
            restarts: 0,
            last_exit: None,
            next_restart_at: None,
        });
        let status = status_tx.subscribe();
        let backoff = initial_backoff(&restart);
        let service = Service {
            title,
            source,
            logs,
            restart,
            rx,
            status_tx,
            spec: None,
            child: None,
            pipes: Vec::new(),
            state: CoreState::Stopped,
            want_running: false,
            started_at: None,
            restarts: 0,
            last_exit: None,
            backoff,
            restart_at: None,
        };
        tokio::spawn(service.run());
        ServiceHandle { tx, status }
    }

    async fn run(mut self) {
        loop {
            let restart_at = self.restart_at;
            tokio::select! {
                message = self.rx.recv() => match message {
                    Some(Msg::Start(spec, lang, reply)) => {
                        self.stop_child().await;
                        self.spec = Some(*spec);
                        self.want_running = true;
                        self.restart_at = None;
                        self.backoff = initial_backoff(&self.restart);
                        let result = i18n::scope(lang, self.launch())
                            .await
                            .map_err(|err| error_chain(&err));
                        if result.is_err() {
                            self.want_running = false;
                        }
                        let _ = reply.send(result);
                    }
                    Some(Msg::Stop(lang, reply)) => {
                        self.want_running = false;
                        self.restart_at = None;
                        let exit = i18n::scope(lang, self.stop_child()).await;
                        self.set_state(CoreState::Stopped);
                        let _ = reply.send(exit);
                    }
                    Some(Msg::Shutdown(reply)) => {
                        self.stop_child().await;
                        let _ = reply.send(());
                        return;
                    }
                    None => {
                        self.stop_child().await;
                        return;
                    }
                },
                status = wait_child(&mut self.child) => self.on_exit(status),
                _ = sleep_until_opt(restart_at) => {
                    self.restart_at = None;
                    self.restarts += 1;
                    if let Err(err) = self.launch().await {
                        self.logs.warn(fl_log!(
                            "service-restart-failed",
                            name = self.title,
                            error = first_line(&error_chain(&err))
                        ));
                        if self.want_running {
                            self.schedule_restart();
                        }
                    }
                }
            }
        }
    }

    fn publish(&self) {
        let now = Deadline::now();
        self.status_tx.send_replace(ServiceRuntime {
            state: self.state,
            pid: self.child.as_ref().and_then(Child::id),
            started_at: self.started_at.map(|(_, unix)| unix),
            restarts: self.restarts,
            last_exit: self.last_exit.clone(),
            next_restart_at: self
                .restart_at
                .map(|at| now_unix() + at.saturating_duration_since(now).as_secs()),
        });
    }

    fn set_state(&mut self, state: CoreState) {
        self.state = state;
        self.publish();
    }

    async fn launch(&mut self) -> Result<u32> {
        let result = self.try_launch().await;
        if let Err(err) = &result {
            self.last_exit = Some(first_line(&error_chain(err)));
            self.set_state(CoreState::Failed);
        }
        result
    }

    async fn try_launch(&mut self) -> Result<u32> {
        let Some(spec) = self.spec.clone() else {
            bail!(fl!("service-no-spec", name = self.title));
        };
        self.set_state(CoreState::Starting);
        if let Some(exe) = &spec.reap_exe {
            reap(exe, &self.logs);
        }
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some((uid, gid)) = spec.user {
            command.uid(uid).gid(gid);
        }
        isolate(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| fl!("err-spawn", path = spec.program.display().to_string()))?;
        let pid = child.id().unwrap_or_default();
        self.pipes.clear();
        if let Some(stdout) = child.stdout.take() {
            self.pipes
                .push(pipe_to_logs(stdout, self.logs.clone(), self.source));
        }
        if let Some(stderr) = child.stderr.take() {
            self.pipes
                .push(pipe_to_logs(stderr, self.logs.clone(), self.source));
        }
        self.logs.info(fl_log!(
            "service-started",
            name = self.title,
            pid = pid.to_string()
        ));

        if let Ok(status) = timeout(STARTUP_GRACE, child.wait()).await {
            self.drain_pipes().await;
            let recent = self.logs.recent_lines(self.source, 8).join("\n");
            self.logs.warn(fl_log!(
                "service-exited-startup",
                name = self.title,
                exit = i18n::in_log_language(|| describe(&status))
            ));
            bail!(
                "{}\n{recent}",
                fl!(
                    "service-exited-startup",
                    name = self.title,
                    exit = describe(&status)
                )
            );
        }
        self.child = Some(child);
        self.started_at = Some((Instant::now(), now_unix()));
        self.set_state(CoreState::Running);
        Ok(pid)
    }

    async fn drain_pipes(&mut self) {
        for pipe in self.pipes.drain(..) {
            let _ = timeout(Duration::from_secs(1), pipe).await;
        }
    }

    async fn stop_child(&mut self) -> Option<String> {
        let mut child = self.child.take()?;
        self.set_state(CoreState::Stopping);
        if let Some(pid) = child.id() {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
        let status = match timeout(STOP_TIMEOUT, child.wait()).await {
            Ok(status) => status,
            Err(_) => {
                self.logs.warn(fl_log!(
                    "service-kill",
                    name = self.title,
                    seconds = STOP_TIMEOUT.as_secs()
                ));
                let _ = child.start_kill();
                child.wait().await
            }
        };
        let exit = describe(&status);
        self.drain_pipes().await;
        self.reap_spec();
        self.logs.info(fl_log!(
            "service-stopped",
            name = self.title,
            exit = i18n::in_log_language(|| describe(&status))
        ));
        self.started_at = None;
        self.last_exit = Some(exit.clone());
        self.set_state(CoreState::Stopped);
        Some(exit)
    }

    fn reap_spec(&self) {
        if let Some(exe) = self.spec.as_ref().and_then(|s| s.reap_exe.as_ref()) {
            reap(exe, &self.logs);
        }
    }

    fn on_exit(&mut self, status: io::Result<ExitStatus>) {
        self.child = None;
        self.reap_spec();
        let exit = describe(&status);
        let failed = !matches!(&status, Ok(status) if status.success());
        let ran_for = self.started_at.take().map(|(at, _)| at.elapsed());
        self.last_exit = Some(exit.clone());
        self.logs
            .warn(fl_log!("service-exited", name = self.title, exit = exit));
        if ran_for.is_some_and(|d| d >= Duration::from_secs(self.restart.stable_after_secs)) {
            self.backoff = initial_backoff(&self.restart);
        }
        let restart = self.want_running
            && match self.restart.policy {
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

    fn schedule_restart(&mut self) {
        let delay = self.backoff;
        let max = Duration::from_secs(self.restart.max_backoff_secs.max(1));
        self.backoff = (self.backoff * 2).min(max);
        self.restart_at = Some(Deadline::now() + delay);
        self.logs.info(fl_log!(
            "service-restarting-in",
            name = self.title,
            seconds = format!("{:.1}", delay.as_secs_f64())
        ));
        self.set_state(CoreState::Backoff);
    }
}

fn initial_backoff(restart: &RestartConfig) -> Duration {
    Duration::from_millis(restart.initial_backoff_ms.max(100))
}

/// Terminates every process whose executable is `exe`.
fn reap(exe: &Path, logs: &LogHub) {
    let deleted = format!("{} (deleted)", exe.display());
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(target) = std::fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        if target == exe || target.as_os_str() == deleted.as_str() {
            logs.info(fl_log!(
                "service-reap",
                path = exe.display().to_string(),
                pid = pid.to_string()
            ));
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }
    }
}
