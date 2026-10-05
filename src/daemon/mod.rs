//! The root daemon: supervises sing-box and serves the control socket.

mod auth;
mod components;
mod cores;
mod github;
mod logs;
mod process;
mod profiles;
mod service;
mod singbox_config;
mod supervisor;

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
use self::cores::CoreManager;
use self::logs::LogHub;
use self::profiles::ProfileManager;
use self::supervisor::{CoreLabel, Op, Supervisor, SupervisorHandle};
use crate::config::DaemonConfig;
use crate::i18n::{self, Lang, fl, fl_log};
use crate::protocol::{
    Envelope, LogEntry, LogSource, MAX_REQUEST_BYTES, Request, Response, StoredCore, UpdateInfo,
    variant_label,
};
use crate::util::{error_chain, now_unix_ms};

pub async fn run(config: DaemonConfig, allow_non_root: bool) -> Result<()> {
    if !Uid::effective().is_root() {
        if !allow_non_root {
            bail!(fl!("daemon-needs-root"));
        }
        tracing::warn!("{}", fl_log!("daemon-non-root"));
    }

    let logs = Arc::new(LogHub::new(config.log_buffer, config.forward_core_logs));
    let auth = Arc::new(Authorizer::new(
        config.socket_group.as_deref(),
        &config.allowed_uids,
    ));
    let listener = bind_socket(&config.socket, auth.group())?;
    logs.info(fl_log!(
        "daemon-listening",
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id().to_string(),
        socket = config.socket.display().to_string()
    ));

    let components = components::spawn(config.clone(), logs.clone());
    let (handle, supervisor) =
        Supervisor::spawn(config.clone(), logs.clone(), components.startup_gate());
    let profiles = ProfileManager::new(config.clone(), logs.clone(), handle.clone());
    profiles.spawn_updater();
    let ctx = Arc::new(Ctx {
        supervisor: handle.clone(),
        components: components.clone(),
        cores: CoreManager::new(config.clone(), logs.clone()),
        profiles,
        logs: logs.clone(),
        auth,
        config: config.clone(),
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
                    tracing::warn!("{}", fl_log!("daemon-accept-failed", error = err.to_string()));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            _ = sigterm.recv() => {
                logs.info(fl_log!("daemon-shutdown-signal", signal = "SIGTERM"));
                break;
            }
            _ = sigint.recv() => {
                logs.info(fl_log!("daemon-shutdown-signal", signal = "SIGINT"));
                break;
            }
            _ = sighup.recv() => {
                // `systemctl reload` → validate and hot-reload sing-box.
                let handle = handle.clone();
                let logs = logs.clone();
                tokio::spawn(async move {
                    if let Response::Error { message } = handle.request(Op::Reload).await {
                        logs.warn(fl_log!("daemon-reload-failed", error = message));
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
    let shown = path.display().to_string();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| fl!("err-create", path = dir.display().to_string()))?;
    }
    if path.symlink_metadata().is_ok() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            bail!(fl!("daemon-already-running", socket = shown.as_str()));
        }
        std::fs::remove_file(path)
            .with_context(|| fl!("daemon-stale-socket", socket = shown.as_str()))?;
    }
    // Create the socket owner-only; access is widened below if a group is set.
    let previous = umask(Mode::from_bits_truncate(0o177));
    let listener = UnixListener::bind(path);
    umask(previous);
    let listener = listener.with_context(|| fl!("daemon-bind-failed", socket = shown.as_str()))?;

    let mut mode = 0o600;
    if let Some(gid) = group {
        match chown(path, None, Some(gid)) {
            Ok(()) => mode = 0o660,
            Err(err) => tracing::warn!(
                "{}",
                fl_log!(
                    "daemon-socket-chown-failed",
                    socket = shown.as_str(),
                    gid = gid.to_string(),
                    error = err.to_string()
                )
            ),
        }
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(listener)
}

/// Everything a client connection may talk to.
struct Ctx {
    supervisor: SupervisorHandle,
    components: ComponentsHandle,
    cores: Arc<CoreManager>,
    profiles: Arc<ProfileManager>,
    logs: Arc<LogHub>,
    auth: Arc<Authorizer>,
    config: DaemonConfig,
}

async fn serve(stream: UnixStream, ctx: Arc<Ctx>) {
    if let Err(err) = serve_client(stream, &ctx).await {
        tracing::debug!("client connection: {err:#}");
    }
}

async fn serve_client(stream: UnixStream, ctx: &Ctx) -> Result<()> {
    let cred = stream.peer_cred()?;
    let (read, mut write) = stream.into_split();
    // Rejected before reading anything, so in the daemon's own language.
    if !ctx.auth.is_allowed(cred.uid(), cred.gid()) {
        tracing::warn!(
            "{}",
            fl_log!(
                "daemon-client-rejected",
                uid = cred.uid().to_string(),
                gid = cred.gid().to_string(),
                pid = cred.pid().map(|p| p.to_string()).unwrap_or_default()
            )
        );
        let message = fl!("daemon-permission-denied");
        return send(&mut write, &Response::error(message)).await;
    }

    let mut reader = BufReader::new(read.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let envelope: Envelope = match serde_json::from_str(line.trim()) {
        Ok(envelope) => envelope,
        Err(err) => {
            let message = fl!("daemon-bad-request", error = err.to_string());
            return send(&mut write, &Response::error(message)).await;
        }
    };
    // Answer in the client's language; clients that send none get the daemon's.
    let lang = envelope
        .lang
        .as_deref()
        .and_then(Lang::parse)
        .unwrap_or_else(i18n::process_language);
    let request = envelope.request;
    i18n::scope(lang, dispatch(request, reader, write, ctx, cred.uid())).await
}

async fn dispatch<R, W>(
    request: Request,
    reader: R,
    mut write: W,
    ctx: &Ctx,
    uid: u32,
) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWrite + Unpin,
{
    let requested = |what: String| {
        tracing::info!(
            "{}",
            fl_log!("daemon-request", uid = uid.to_string(), request = what)
        );
    };
    let op = match request {
        Request::Status => {
            let mut status = ctx.supervisor.status();
            status.setup_required = ctx.components.setup_required();
            status.components = ctx.components.statuses();
            status.active_core = ctx.cores.active();
            status.active_profile = ctx.profiles.active();
            status.update_in_progress = ctx.cores.busy();
            return send(&mut write, &Response::Status(Box::new(status))).await;
        }
        Request::Logs { tail, follow } => {
            return stream_logs(reader, write, &ctx.logs, tail, follow).await;
        }
        Request::Setup {
            sub_store,
            http_meta,
        } => {
            requested("setup".to_owned());
            let response = ctx.components.setup(sub_store, http_meta).await;
            return send(&mut write, &response).await;
        }
        Request::Component { component, action } => {
            requested(format!("{action:?} {}", component.name()));
            let response = ctx.components.action(component, action).await;
            return send(&mut write, &response).await;
        }
        profile_request @ (Request::ProfileList
        | Request::ProfileGet { .. }
        | Request::ProfileAdd { .. }
        | Request::ProfileSave { .. }
        | Request::ProfileSet { .. }
        | Request::ProfileUpdate { .. }
        | Request::ProfileActivate { .. }
        | Request::ProfileRemove { .. }
        | Request::ProfileCheck { .. }
        | Request::ProfileAdopt) => {
            if !matches!(
                profile_request,
                Request::ProfileList | Request::ProfileGet { .. }
            ) {
                requested(request_name(&profile_request));
            }
            let response = handle_profile(&ctx.profiles, profile_request).await;
            return send(&mut write, &response).await;
        }
        Request::Start => Op::Start,
        Request::Stop => Op::Stop,
        Request::Restart => Op::Restart,
        Request::Reload => Op::Reload,
        Request::Check => Op::Check,
        core_request => {
            // Adding sources and importing binaries decides what runs as root.
            let privileged = uid == 0 || uid == nix::unistd::Uid::effective().as_raw();
            requested(request_name(&core_request));
            let response = handle_core(ctx, core_request, privileged).await;
            return send(&mut write, &response).await;
        }
    };
    requested(format!("{op:?}"));
    let response = ctx.supervisor.request(op).await;
    send(&mut write, &response).await
}

fn request_name(request: &Request) -> String {
    let json = serde_json::to_value(request).unwrap_or_default();
    json.get("cmd")
        .and_then(|c| c.as_str())
        .unwrap_or("?")
        .to_owned()
}

fn result(response: anyhow::Result<Response>) -> Response {
    response.unwrap_or_else(|err| Response::error(error_chain(&err)))
}

fn stored_message(core: &StoredCore) -> Response {
    Response::done(fl!(
        "daemon-core-stored",
        version = core.version.clone(),
        id = core.id.clone(),
        checksum = core.checksum.description()
    ))
}

async fn handle_core(ctx: &Ctx, request: Request, privileged: bool) -> Response {
    let root_only = |action: &str| Response::error(fl!("daemon-root-only", action = action));
    match request {
        Request::CoreSources => Response::CoreSources {
            sources: ctx.cores.sources().await,
            default: ctx.cores.update_target().0,
        },
        Request::CoreReleases {
            source,
            page,
            refresh,
        } => result(
            ctx.cores
                .releases(&source, page, refresh)
                .await
                .map(Response::CoreReleases),
        ),
        Request::CoreInstalled => Response::CoreInstalled {
            cores: ctx.cores.installed(),
        },
        Request::CoreInstall {
            source,
            tag,
            variant,
            activate,
            force,
        } => match ctx.cores.install(&source, &tag, &variant).await {
            Ok(core) if activate => activate_core(ctx, &core.id, force, true).await,
            Ok(core) => stored_message(&core),
            Err(err) => Response::error(error_chain(&err)),
        },
        Request::CoreActivate { id, force } => activate_core(ctx, &id, force, true).await,
        Request::CoreRemove { id } => result(
            ctx.cores
                .remove(&id)
                .map(|()| Response::done(fl!("daemon-core-deleted", id = id.clone()))),
        ),
        Request::CoreSourceAdd { repo, name } => {
            if !privileged {
                return root_only("add-source");
            }
            result(
                ctx.cores
                    .add_source(&repo, name)
                    .await
                    .map(|s| Response::done(fl!("daemon-source-added", id = s.id, name = s.name))),
            )
        }
        Request::CoreSourceRemove { id } => {
            if !privileged {
                return root_only("remove-source");
            }
            result(
                ctx.cores
                    .remove_source(&id)
                    .await
                    .map(|()| Response::done(fl!("daemon-source-removed", id = id.clone()))),
            )
        }
        Request::CoreImport {
            location,
            sha256,
            activate,
        } => {
            if !privileged {
                return root_only("import");
            }
            match ctx.cores.import(&location, sha256.as_deref()).await {
                Ok(core) if activate => activate_core(ctx, &core.id, false, true).await,
                Ok(core) => stored_message(&core),
                Err(err) => Response::error(error_chain(&err)),
            }
        }
        Request::CheckUpdate => result(check_update(ctx).await.map(Response::UpdateInfo)),
        Request::Update { tag, force } => result(update(ctx, tag.as_deref(), force).await),
        other => Response::error(fl!(
            "daemon-unexpected-request",
            name = request_name(&other)
        )),
    }
}

async fn handle_profile(profiles: &ProfileManager, request: Request) -> Response {
    let saved = |(profile, message)| Response::ProfileSaved {
        profile: Box::new(profile),
        message,
    };
    match request {
        Request::ProfileList => Response::Profiles(profiles.list()),
        Request::ProfileGet { id } => {
            result(
                profiles
                    .read(&id)
                    .await
                    .map(|(profile, content)| Response::ProfileContent {
                        profile: Box::new(profile),
                        content,
                    }),
            )
        }
        Request::ProfileAdd {
            name,
            content,
            url,
            interval,
            activate,
        } => {
            let (profile, message) = match profiles.add(name, content, url, interval).await {
                Ok(added) => added,
                Err(err) => return Response::error(error_chain(&err)),
            };
            if !activate {
                return saved((profile, message));
            }
            match profiles.activate(&profile.id, false).await {
                Ok(Response::Done { message: switched }) => {
                    let profile = profiles.resolve(&profile.id).unwrap_or(profile);
                    saved((profile, format!("{message}\n{switched}")))
                }
                Ok(Response::Error { message: failed }) => {
                    Response::error(format!("{message}\n{failed}"))
                }
                Ok(other) => other,
                Err(err) => Response::error(format!("{message}\n{}", error_chain(&err))),
            }
        }
        Request::ProfileSave { id, content, force } => {
            result(profiles.save(&id, &content, force).await.map(saved))
        }
        Request::ProfileSet {
            id,
            name,
            url,
            interval,
        } => result(
            profiles
                .set(&id, name, url, interval)
                .await
                .map(|p| Response::done(fl!("profiles-settings-saved", name = p.name))),
        ),
        Request::ProfileUpdate {
            id: Some(id),
            force,
        } => result(profiles.update(&id, force).await.map(Response::done)),
        Request::ProfileUpdate { id: None, force } => {
            result(profiles.update_all(force).await.map(Response::done))
        }
        Request::ProfileActivate { id, force } => result(profiles.activate(&id, force).await),
        Request::ProfileRemove { id } => result(
            profiles
                .remove(&id)
                .await
                .map(|p| Response::done(fl!("profiles-removed", name = p.name))),
        ),
        Request::ProfileCheck { id } => result(profiles.check(&id).await.map(Response::done)),
        Request::ProfileAdopt => result(profiles.adopt().await.map(|adopted| match adopted {
            Some(p) => Response::done(fl!("profiles-adopted", name = p.name, id = p.id)),
            None => Response::done(fl!("profiles-nothing-to-adopt")),
        })),
        other => Response::error(fl!(
            "daemon-unexpected-request",
            name = request_name(&other)
        )),
    }
}

/// Keeps a hand-placed binary, then lets the supervisor swap the symlink.
async fn activate_core(ctx: &Ctx, id: &str, force: bool, restart: bool) -> Response {
    let Some(core) = ctx.cores.load(id) else {
        return Response::error(fl!("cores-not-stored", id = id));
    };
    if ctx.cores.active().is_some_and(|active| active.id == id) {
        return Response::done(fl!(
            "daemon-core-already-active",
            version = core.version.clone(),
            id = id
        ));
    }
    let adopted = match ctx.cores.adopt(&ctx.config.core.binary).await {
        Ok(adopted) => adopted,
        Err(err) => {
            return Response::error(fl!("daemon-adopt-failed", error = error_chain(&err)));
        }
    };
    let label = |core: &StoredCore| {
        let text = || {
            fl!(
                "core-label",
                version = core.version.clone(),
                source = core.source_label(),
                variant = variant_label(&core.variant)
            )
        };
        CoreLabel {
            reply: text(),
            log: i18n::in_log_language(text),
        }
    };
    let fallback = adopted
        .or_else(|| ctx.cores.active())
        .filter(|previous| previous.id != core.id)
        .map(|previous| (ctx.cores.binary_of(&previous), label(&previous)));
    ctx.supervisor
        .request(Op::Activate {
            target: ctx.cores.binary_of(&core),
            label: label(&core),
            force,
            restart,
            fallback,
        })
        .await
}

async fn check_update(ctx: &Ctx) -> anyhow::Result<UpdateInfo> {
    let (source, variant) = ctx.cores.update_target();
    let release = ctx.cores.release(&source, None).await?;
    let active = ctx.cores.active();
    Ok(UpdateInfo {
        current: ctx.supervisor.status().core_version,
        latest: release.version().to_owned(),
        tag: release.tag_name.clone(),
        asset: ctx.cores.asset_for(&release, &variant).unwrap_or_default(),
        prerelease: release.prerelease,
        published_at: release.published_at.clone(),
        update_available: active
            .is_none_or(|c| c.tag.as_deref() != Some(release.tag_name.as_str())),
    })
}

/// `singbox-board update`: newest (or given) release of the active core's
/// source and variant, stored and switched to.
async fn update(ctx: &Ctx, tag: Option<&str>, force: bool) -> anyhow::Result<Response> {
    let (source, variant) = ctx.cores.update_target();
    let release = ctx.cores.release(&source, tag).await?;
    let active = ctx.cores.active();
    let current = active.as_ref().is_some_and(|c| {
        c.source == source
            && c.variant == variant
            && c.tag.as_deref() == Some(release.tag_name.as_str())
    });
    if current && !force {
        return Ok(Response::done(fl!(
            "up-to-date",
            name = format!("sing-box {}", release.version())
        )));
    }
    let core = ctx
        .cores
        .install(&source, &release.tag_name, &variant)
        .await?;
    Ok(activate_core(ctx, &core.id, force, ctx.config.update.restart_after_update).await)
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
                        line: fl!("daemon-logs-skipped", count = skipped),
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
