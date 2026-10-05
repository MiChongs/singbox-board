//! The root daemon: supervises sing-box and serves the control socket.

mod auth;
mod components;
mod github;
mod logs;
mod process;
mod service;
mod singbox_config;
mod supervisor;
mod updater;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use nix::sys::stat::{Mode, umask};
use nix::unistd::{Gid, Uid, chown};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast::error::RecvError;

use self::auth::Authorizer;
use self::components::ComponentsHandle;
use self::logs::LogHub;
use self::supervisor::{Op, Supervisor, SupervisorHandle};
use crate::config::DaemonConfig;
use crate::protocol::{LogEntry, LogSource, MAX_REQUEST_BYTES, Request, Response};
use crate::util::now_unix_ms;

pub async fn run(config: DaemonConfig, allow_non_root: bool) -> Result<()> {
    if !Uid::effective().is_root() {
        if !allow_non_root {
            bail!(
                "the daemon must run as root (TUN, auto_route, tproxy and eBPF need it); \
                 pass --allow-non-root for development"
            );
        }
        tracing::warn!("running without root privileges; TUN and routing features will fail");
    }

    let logs = Arc::new(LogHub::new(config.log_buffer, config.forward_core_logs));
    let auth = Arc::new(Authorizer::new(
        config.socket_group.as_deref(),
        &config.allowed_uids,
    ));
    let listener = bind_socket(&config.socket, auth.group())?;
    logs.info(format!(
        "singbox-board daemon {} (pid {}) listening on {}",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        config.socket.display()
    ));

    let components = components::spawn(config.clone(), logs.clone());
    let (handle, supervisor) =
        Supervisor::spawn(config.clone(), logs.clone(), components.startup_gate());
    let ctx = Arc::new(Ctx {
        supervisor: handle.clone(),
        components: components.clone(),
        logs: logs.clone(),
        auth,
    });
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sighup = signal(SignalKind::hangup())?;

    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(serve(stream, ctx.clone()));
                }
                Err(err) => {
                    tracing::warn!("accept failed: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            _ = sigterm.recv() => {
                logs.info("SIGTERM received, shutting down");
                break;
            }
            _ = sigint.recv() => {
                logs.info("SIGINT received, shutting down");
                break;
            }
            _ = sighup.recv() => {
                // `systemctl reload` → validate and hot-reload sing-box.
                let handle = handle.clone();
                let logs = logs.clone();
                tokio::spawn(async move {
                    if let Response::Error { message } = handle.request(Op::Reload).await {
                        logs.warn(format!("reload failed: {message}"));
                    }
                });
            }
        }
    }

    components.shutdown().await;
    handle.shutdown().await;
    let _ = supervisor.await;
    let _ = std::fs::remove_file(&config.socket);
    Ok(())
}

fn bind_socket(path: &Path, group: Option<Gid>) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    if path.symlink_metadata().is_ok() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            bail!("another daemon is already listening on {}", path.display());
        }
        std::fs::remove_file(path).with_context(|| format!("remove stale {}", path.display()))?;
    }
    // Create the socket owner-only; access is widened below if a group is set.
    let previous = umask(Mode::from_bits_truncate(0o177));
    let listener = UnixListener::bind(path);
    umask(previous);
    let listener = listener.with_context(|| format!("bind {}", path.display()))?;

    let mut mode = 0o600;
    if let Some(gid) = group {
        match chown(path, None, Some(gid)) {
            Ok(()) => mode = 0o660,
            Err(err) => tracing::warn!("chown {} to gid {gid}: {err}", path.display()),
        }
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(listener)
}

/// Everything a client connection may talk to.
struct Ctx {
    supervisor: SupervisorHandle,
    components: ComponentsHandle,
    logs: Arc<LogHub>,
    auth: Arc<Authorizer>,
}

async fn serve(stream: UnixStream, ctx: Arc<Ctx>) {
    if let Err(err) = serve_client(stream, &ctx).await {
        tracing::debug!("client connection: {err:#}");
    }
}

async fn serve_client(stream: UnixStream, ctx: &Ctx) -> Result<()> {
    let cred = stream.peer_cred()?;
    let (read, mut write) = stream.into_split();
    if !ctx.auth.is_allowed(cred.uid(), cred.gid()) {
        tracing::warn!(
            "rejected client uid={} gid={} pid={:?}",
            cred.uid(),
            cred.gid(),
            cred.pid()
        );
        let message = "permission denied: run as root or join the daemon's socket group";
        return send(&mut write, &Response::error(message)).await;
    }

    let mut reader = BufReader::new(read.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(request) => request,
        Err(err) => return send(&mut write, &Response::error(format!("bad request: {err}"))).await,
    };

    let op = match request {
        Request::Status => {
            let mut status = ctx.supervisor.status();
            status.setup_required = ctx.components.setup_required();
            status.components = ctx.components.statuses();
            return send(&mut write, &Response::Status(status)).await;
        }
        Request::Logs { tail, follow } => {
            return stream_logs(reader, write, &ctx.logs, tail, follow).await;
        }
        Request::Setup {
            sub_store,
            http_meta,
        } => {
            tracing::info!("uid {} requested setup", cred.uid());
            let response = ctx.components.setup(sub_store, http_meta).await;
            return send(&mut write, &response).await;
        }
        Request::Component { component, action } => {
            tracing::info!(
                "uid {} requested {action:?} {}",
                cred.uid(),
                component.name()
            );
            let response = ctx.components.action(component, action).await;
            return send(&mut write, &response).await;
        }
        Request::Start => Op::Start,
        Request::Stop => Op::Stop,
        Request::Restart => Op::Restart,
        Request::Reload => Op::Reload,
        Request::Check => Op::Check,
        Request::CheckUpdate => Op::CheckUpdate,
        Request::Update { tag, force } => Op::Update { tag, force },
    };
    tracing::info!("uid {} requested {op:?}", cred.uid());
    let response = ctx.supervisor.request(op).await;
    send(&mut write, &response).await
}

async fn stream_logs<R, W>(
    mut reader: R,
    mut write: W,
    logs: &LogHub,
    tail: usize,
    follow: bool,
) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWrite + Unpin,
{
    let (backlog, mut rx) = logs.subscribe(tail);
    for entry in backlog {
        send(&mut write, &Response::Log(entry)).await?;
    }
    if !follow {
        return Ok(());
    }
    let mut probe = [0u8; 64];
    loop {
        tokio::select! {
            entry = rx.recv() => match entry {
                Ok(entry) => send(&mut write, &Response::Log(entry)).await?,
                Err(RecvError::Lagged(skipped)) => {
                    let notice = LogEntry {
                        seq: 0,
                        ts: now_unix_ms(),
                        source: LogSource::Daemon,
                        line: format!("… {skipped} log lines skipped (client too slow)"),
                    };
                    send(&mut write, &Response::Log(notice)).await?;
                }
                Err(RecvError::Closed) => return Ok(()),
            },
            // The client never sends more data; EOF means it went away.
            read = reader.read(&mut probe) => match read {
                Ok(0) | Err(_) => return Ok(()),
                Ok(_) => {}
            },
        }
    }
}

async fn send<W: AsyncWrite + Unpin>(write: &mut W, response: &Response) -> Result<()> {
    let mut line = serde_json::to_vec(response)?;
    line.push(b'\n');
    write.write_all(&line).await?;
    write.flush().await?;
    Ok(())
}
