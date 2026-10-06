//! Running the kurumi-containerd command line.
//!
//! The runtime forks its monitor process and assumes a single-threaded
//! caller, so it is never linked into the daemon; every operation runs the
//! official executable. It finds its container registry in
//! `$HOME/.kurumi-containerd/config.json`, which the daemon writes from its
//! own registry, with the daemon's container ids as entry names.
//!
//! A started container's monitor outlives the `start` command. Under
//! systemd it is started in a transient scope in machine.slice, so that
//! stopping or restarting the daemon's service (whose cgroup is killed)
//! leaves containers running. The scope is ordered before the daemon's
//! service: at shutdown the daemon stops containers cleanly first.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::i18n::fl;
use crate::util::{find_program, random_token, strip_ansi};

/// Output kept of each stream; more is read and dropped.
pub const CAPTURE_LIMIT: usize = 1024 * 1024;
/// How long output may still arrive after the command exited. A started
/// monitor holds the pipes until it redirects them to /dev/null.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub struct Output {
    /// Exit code; `None` when the command was killed by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim_end().to_owned()
    }

    /// Diagnostic lines, without colours.
    pub fn stderr_lines(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.stderr)
            .lines()
            .map(strip_ansi)
            .filter(|line| !line.trim().is_empty())
            .collect()
    }

    /// Why the command failed: the `error=` field of the runtime's last
    /// ERROR record (with the lines that belong to it, such as the code
    /// frame of a TOML error), else the last diagnostic line, else the
    /// exit status.
    pub fn failure(&self) -> String {
        let text = strip_ansi(&String::from_utf8_lossy(&self.stderr));
        let lines: Vec<&str> = text.lines().collect();
        let record = |line: &str| without_timestamp(line).len() != line.len();
        let error = lines
            .iter()
            .rposition(|line| record(line) && line.contains(" ERROR "))
            .map(|start| {
                let (_, rest) = lines[start].split_once(" ERROR ").unwrap_or_default();
                let head = rest.split_once("error=").map_or(rest, |(_, error)| error);
                let mut parts = vec![head.trim_end()];
                parts.extend(
                    lines[start + 1..]
                        .iter()
                        .take_while(|line| !record(line))
                        .map(|line| line.trim_end()),
                );
                dedupe_chain(parts.join("\n").trim().trim_matches('"'))
            });
        error
            .or_else(|| self.stderr_lines().last().cloned())
            .or_else(|| Some(self.stdout_text()).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| match self.code {
                Some(code) => fl!("exit-code", code = code),
                None => fl!("exit-unknown"),
            })
    }
}

/// An error chain as anyhow prints it (causes joined by ": "), with the
/// causes a context already quotes left out: the runtime's TOML errors
/// would otherwise appear twice.
fn dedupe_chain(text: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for part in text.split("\n: ") {
        if !kept.iter().any(|earlier| earlier.contains(part.trim())) {
            kept.push(part);
        }
    }
    kept.join("\n: ")
}

/// A diagnostic line without the RFC 3339 timestamp the runtime starts it
/// with; the log has its own.
pub fn without_timestamp(line: &str) -> &str {
    match line.split_once(' ') {
        Some((first, rest))
            if first.len() >= 20
                && first.as_bytes()[0].is_ascii_digit()
                && first.contains('T')
                && (first.ends_with('Z') || first.contains('+')) =>
        {
            rest.trim_start()
        }
        _ => line,
    }
}

/// A transient systemd scope for a command whose processes outlive it.
pub struct Scope {
    pub unit: String,
    pub description: String,
}

pub struct Kurumi {
    pub binary: PathBuf,
    pub home: PathBuf,
}

impl Kurumi {
    fn base(&self, scope: Option<&Scope>, args: &[OsString]) -> Command {
        let mut command = match scope.and_then(|scope| scope_command(scope, &self.binary)) {
            Some(command) => command,
            None => Command::new(&self.binary),
        };
        command
            .args(args)
            .env("HOME", &self.home)
            .env("NO_COLOR", "1")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Ctrl-C on a daemon run in a terminal must not reach it.
            .process_group(0);
        command
    }

    /// Runs `kurumi-containerd <args>`. After `timeout` the command is
    /// killed (processes it started keep running).
    pub async fn run(
        &self,
        args: &[OsString],
        timeout: Duration,
        scope: Option<&Scope>,
    ) -> Result<Output> {
        let mut command = self.base(scope, args);
        command.kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| fl!("err-spawn", path = self.binary.display().to_string()))?;
        let stdout = tokio::spawn(capture(child.stdout.take()));
        let stderr = tokio::spawn(capture(child.stderr.take()));
        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                stdout.abort();
                stderr.abort();
                return Err(anyhow!(fl!(
                    "containers-command-timeout",
                    seconds = timeout.as_secs()
                )));
            }
        };
        let collect = |task: tokio::task::JoinHandle<(Vec<u8>, bool)>| async move {
            match tokio::time::timeout(DRAIN_GRACE, task).await {
                Ok(Ok(captured)) => captured,
                _ => (Vec::new(), false),
            }
        };
        let (stdout, cut_out) = collect(stdout).await;
        let (stderr, cut_err) = collect(stderr).await;
        Ok(Output {
            code: status.code(),
            stdout,
            stderr,
            truncated: cut_out || cut_err,
        })
    }
}

/// Reads a stream to its end, keeping the first [`CAPTURE_LIMIT`] bytes.
async fn capture(stream: Option<impl AsyncRead + Unpin>) -> (Vec<u8>, bool) {
    let Some(mut stream) = stream else {
        return (Vec::new(), false);
    };
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = CAPTURE_LIMIT.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
                truncated |= n > room;
            }
        }
    }
    (kept, truncated)
}

/// Whether transient scopes are available: systemd is the init system and
/// systemd-run can create a scope with the properties used here. Probed
/// once with a scope that runs `true`.
fn scope_support() -> Option<&'static (PathBuf, Option<String>)> {
    static SUPPORT: OnceLock<Option<(PathBuf, Option<String>)>> = OnceLock::new();
    SUPPORT
        .get_or_init(|| {
            if !Path::new("/run/systemd/system").is_dir()
                || !nix::unistd::Uid::effective().is_root()
            {
                return None;
            }
            let systemd_run = find_program("systemd-run")?;
            let truth = find_program("true")?;
            let own_unit = own_service();
            let probe = |before: Option<&str>| {
                let mut command = std::process::Command::new(&systemd_run);
                command.args(["--scope", "--quiet", "--collect"]);
                command.arg(format!(
                    "--unit=singbox-board-probe-{}",
                    random_token(8).to_ascii_lowercase()
                ));
                if let Some(unit) = before {
                    command.arg(format!("--property=Before={unit}"));
                }
                command
                    .arg("--")
                    .arg(&truth)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            };
            if let Some(unit) = &own_unit
                && probe(Some(unit))
            {
                return Some((systemd_run, own_unit));
            }
            probe(None).then_some((systemd_run, None))
        })
        .as_ref()
}

/// Probes for transient scopes ahead of the first start (blocking).
pub fn warm_up() {
    let _ = scope_support();
}

/// The systemd service the daemon runs in, from its cgroup path
/// (`0::/system.slice/singbox-board.service`).
fn own_service() -> Option<String> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = text.lines().find_map(|line| line.strip_prefix("0::"))?;
    path.rsplit('/')
        .find(|part| part.ends_with(".service"))
        .map(str::to_owned)
}

fn scope_command(scope: &Scope, binary: &Path) -> Option<Command> {
    let (systemd_run, before) = scope_support()?;
    let mut command = Command::new(systemd_run);
    command.args(["--scope", "--quiet", "--collect", "--slice=machine.slice"]);
    command.arg(format!("--unit={}", scope.unit));
    command.arg(format!("--description={}", scope.description));
    if let Some(unit) = before {
        command.arg(format!("--property=Before={unit}"));
    }
    command.arg("--").arg(binary);
    Some(command)
}

/// Whether the machine is shutting down or rebooting (systemd or OpenRC).
pub async fn system_stopping() -> bool {
    if let Ok(level) = tokio::fs::read_to_string("/run/openrc/softlevel").await
        && matches!(level.trim(), "shutdown" | "reboot")
    {
        return true;
    }
    if !Path::new("/run/systemd/system").is_dir() {
        return false;
    }
    let Some(systemctl) = find_program("systemctl") else {
        return false;
    };
    let output = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new(systemctl)
            .arg("is-system-running")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    matches!(output, Ok(Ok(output)) if String::from_utf8_lossy(&output.stdout).trim() == "stopping")
}

/// Arguments as the command takes them.
pub fn args<I, S>(items: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    items.into_iter().map(Into::into).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(stderr: &str, code: Option<i32>) -> Output {
        Output {
            code,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            truncated: false,
        }
    }

    #[test]
    fn failures_name_the_runtime_error() {
        let failed = output(
            "2026-10-07T01:02:03.000Z  INFO loading\n\
             2026-10-07T01:02:03.100Z ERROR command failed error=container 'dev' is already running\n",
            Some(1),
        );
        assert_eq!(failed.failure(), "container 'dev' is already running");
        assert_eq!(
            output("plain complaint\n", Some(2)).failure(),
            "plain complaint"
        );
        // A TOML error, as the runtime prints it: the code frame stays, the
        // repeated cause goes.
        let toml = output(
            "2026-10-06T16:48:20.196390Z ERROR command failed error=failed to parse TOML config /x/c.toml: TOML parse error at line 19, column 1\n   |\n19 | bogus = 1\n   | ^^^^^\nunknown field `bogus`, expected one of `name`\n: TOML parse error at line 19, column 1\n   |\n19 | bogus = 1\n   | ^^^^^\nunknown field `bogus`, expected one of `name`\n\n",
            Some(1),
        );
        assert_eq!(
            toml.failure(),
            "failed to parse TOML config /x/c.toml: TOML parse error at line 19, column 1\n   |\n19 | bogus = 1\n   | ^^^^^\nunknown field `bogus`, expected one of `name`"
        );
        assert_eq!(output("", Some(3)).failure(), fl!("exit-code", code = 3));
        assert!(output("\x1b[31mred\x1b[0m\n", Some(1)).stderr_lines() == ["red"]);
        assert_eq!(
            without_timestamp("2026-10-07T01:02:03.123456Z  INFO starting container"),
            "INFO starting container"
        );
        assert_eq!(without_timestamp("INFO plain"), "INFO plain");
    }

    #[tokio::test]
    async fn output_is_captured_and_bounded() {
        let dir = std::env::temp_dir().join(format!("sbb-kurumi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake");
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"home=$HOME args=$*\"\nhead -c 2000000 /dev/zero\necho oops >&2\nexit 4\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let kurumi = Kurumi {
            binary: script.clone(),
            home: dir.clone(),
        };
        let output = kurumi
            .run(
                &args(["--name", "x", "info"]),
                Duration::from_secs(10),
                None,
            )
            .await
            .unwrap();
        assert_eq!(output.code, Some(4));
        assert!(output.truncated);
        assert_eq!(output.stdout.len(), CAPTURE_LIMIT);
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .starts_with(&format!("home={} args=--name x info", dir.display()))
        );
        assert_eq!(output.failure(), "oops");

        std::fs::write(&script, "#!/bin/sh\nsleep 5\n").unwrap();
        let started = std::time::Instant::now();
        let error = kurumi
            .run(&args(["start"]), Duration::from_millis(200), None)
            .await
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(
            error.to_string(),
            fl!("containers-command-timeout", seconds = 0)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
