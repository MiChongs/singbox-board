//! Child-process helpers shared by the sing-box supervisor and the component services.

use std::io;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::Arc;

#[cfg(unix)]
use nix::sys::signal::Signal;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use super::logs::LogHub;
use crate::i18n::fl;
use crate::protocol::LogSource;
use crate::util::strip_ansi;

/// Puts the child in its own process group and makes the kernel SIGTERM it
/// when the daemon dies, so no unmanaged process is left behind.
#[cfg(unix)]
pub fn isolate(command: &mut Command) {
    // Keep terminal signals (Ctrl-C on a foreground daemon) away from the
    // child; the daemon stops it in an orderly way instead.
    command.process_group(0).kill_on_drop(true);
    // SAFETY: prctl is async-signal-safe and touches no shared state. It runs
    // after std's setuid/setgid, which would otherwise clear the setting.
    unsafe {
        command
            .pre_exec(|| nix::sys::prctl::set_pdeathsig(Signal::SIGTERM).map_err(io::Error::from));
    }
}

/// Puts the child in its own process group; [`adopt`] then ties it to the
/// daemon's lifetime.
#[cfg(windows)]
pub fn isolate(command: &mut Command) {
    command
        .creation_flags(crate::win::process::CHILD_FLAGS)
        .kill_on_drop(true);
}

/// Called right after spawning an [`isolate`]d child.
pub fn adopt(child: &Child) {
    #[cfg(windows)]
    crate::win::process::adopt(child);
    #[cfg(unix)]
    let _ = child;
}

/// Asks a child to stop: SIGTERM, or CTRL_BREAK to its process group on
/// Windows (terminating it right away when that cannot be delivered).
pub fn request_stop(child: &mut Child) {
    let Some(pid) = child.id() else {
        return;
    };
    #[cfg(unix)]
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), Signal::SIGTERM);
    #[cfg(windows)]
    if !crate::win::process::request_stop(pid) {
        let _ = child.start_kill();
    }
}

pub async fn wait_child(child: &mut Option<Child>) -> io::Result<ExitStatus> {
    match child {
        Some(child) => child.wait().await,
        None => std::future::pending().await,
    }
}

pub async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Copies a child's output line by line into the log hub.
pub fn pipe_to_logs<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    logs: Arc<LogHub>,
    source: LogSource,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => logs.push(source, &strip_ansi(&String::from_utf8_lossy(&buf))),
            }
        }
    })
}

/// How a process ended, in the current language.
#[cfg(unix)]
pub fn describe(status: &io::Result<ExitStatus>) -> String {
    match status {
        Ok(status) => match (status.code(), status.signal()) {
            (Some(code), _) => fl!("exit-code", code = code),
            (None, Some(signal)) => match Signal::try_from(signal) {
                Ok(signal) => fl!("exit-signal", signal = signal.as_str()),
                Err(_) => fl!("exit-signal", signal = signal.to_string()),
            },
            _ => fl!("exit-unknown"),
        },
        Err(err) => fl!("exit-wait-failed", error = err.to_string()),
    }
}

/// How a process ended, in the current language. Exit codes that are
/// NTSTATUS values (a crash, Ctrl-C) are shown in hex.
#[cfg(windows)]
pub fn describe(status: &io::Result<ExitStatus>) -> String {
    match status {
        Ok(status) => match status.code() {
            Some(code) if code < 0 => fl!("exit-code", code = format!("{:#010X}", code as u32)),
            Some(code) => fl!("exit-code", code = code),
            None => fl!("exit-unknown"),
        },
        Err(err) => fl!("exit-wait-failed", error = err.to_string()),
    }
}

pub fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .trim_end_matches(':')
        .to_owned()
}
