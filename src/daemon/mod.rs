//! The root daemon: supervises sing-box and serves the control socket.

#[cfg(unix)]
mod auth;
#[cfg(windows)]
#[path = "auth_windows.rs"]
mod auth;
mod components;
#[cfg(unix)]
mod containers;
#[cfg(windows)]
#[path = "containers_windows.rs"]
mod containers;
mod cores;
mod github;
mod logs;
mod process;
mod profiles;
mod service;
mod singbox_config;
mod supervisor;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[cfg(unix)]
use anyhow::Context;
use anyhow::{Result, bail};
#[cfg(unix)]
use nix::sys::stat::{Mode, umask};
#[cfg(unix)]
use nix::unistd::{Gid, Uid, chown};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(windows)]
use tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;

use self::auth::Authorizer;
use self::components::ComponentsHandle;
use self::containers::ContainerManager;
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
    #[cfg(unix)]
    if !Uid::effective().is_root() {
        if !allow_non_root {
            bail!(fl!("daemon-needs-root"));
        }
        tracing::warn!("{}", fl_log!("daemon-non-root"));
    }
    #[cfg(windows)]
    {
        if crate::win::is_elevated() {
            secure_layout(&config);
        } else {
            if !allow_non_root {
                bail!(fl!("win-daemon-needs-admin"));
            }
            tracing::warn!("{}", fl_log!("win-daemon-non-admin"));
        }
        crate::win::process::ensure_console();
    }

    let logs = Arc::new(LogHub::new(config.log_buffer, config.forward_core_logs));
    let auth = Arc::new(Authorizer::new(
        config.socket_group.as_deref(),
        &config.allowed_uids,
    ));
    #[allow(unused_mut)]
    let mut listener = bind_socket(&config.socket, auth.group())?;
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
    let containers = ContainerManager::new(config.clone(), logs.clone());
    containers.spawn_autostart();
    let ctx = Arc::new(Ctx {
        supervisor: handle.clone(),
        components: components.clone(),
        cores: CoreManager::new(config.clone(), logs.clone()),
        profiles,
        containers: containers.clone(),
        logs: logs.clone(),
        auth,
        config: config.clone(),
    });
    let mut signals = Signals::new()?;

    loop {
        tokio::select! {
            accepted = accept(&mut listener) => match accepted {
                Ok(stream) => {
                    tokio::spawn(serve(stream, ctx.clone()));
                }
                Err(err) => {
                    tracing::warn!("{}", fl_log!("daemon-accept-failed", error = err.to_string()));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            event = signals.next() => match event {
                Event::Shutdown(signal) => {
                    logs.info(fl_log!("daemon-shutdown-signal", signal = signal));
                    break;
                }
                Event::Reload => {
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
    }

    #[cfg(windows)]
    crate::win::service::stopping(Duration::from_secs(
        config.core.stop_timeout_secs.max(1) + 20,
    ));
    // Containers are stopped (when they are) alongside sing-box and the
    // components, so that the slowest decides how long shutdown takes.
    tokio::join!(containers.shutdown(), async {
        components.shutdown().await;
        handle.shutdown().await;
    });
    let _ = supervisor.await;
    #[cfg(unix)]
    let _ = std::fs::remove_file(&config.socket);
    Ok(())
}

/// What a signal (or the service control manager) asks the daemon to do.
enum Event {
    Shutdown(&'static str),
    Reload,
}

#[cfg(unix)]
struct Signals {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn new() -> Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
            hup: signal(SignalKind::hangup())?,
        })
    }

    async fn next(&mut self) -> Event {
        tokio::select! {
            _ = self.term.recv() => Event::Shutdown("SIGTERM"),
            _ = self.int.recv() => Event::Shutdown("SIGINT"),
            _ = self.hup.recv() => Event::Reload,
        }
    }
}

/// Console control events of a daemon in a terminal, and the requests of
/// the service control manager.
#[cfg(windows)]
struct Signals {
    c: tokio::signal::windows::CtrlC,
    brk: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
    shutdown: tokio::signal::windows::CtrlShutdown,
    logoff: tokio::signal::windows::CtrlLogoff,
}

#[cfg(windows)]
impl Signals {
    fn new() -> Result<Self> {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
        Ok(Self {
            c: ctrl_c()?,
            brk: ctrl_break()?,
            close: ctrl_close()?,
            shutdown: ctrl_shutdown()?,
            logoff: ctrl_logoff()?,
        })
    }

    async fn next(&mut self) -> Event {
        let service = crate::win::service::events();
        loop {
            tokio::select! {
                _ = self.c.recv() => return Event::Shutdown("Ctrl-C"),
                _ = self.brk.recv() => return Event::Shutdown("Ctrl-Break"),
                _ = self.close.recv() => return Event::Shutdown("CTRL_CLOSE_EVENT"),
                _ = self.shutdown.recv() => return Event::Shutdown("CTRL_SHUTDOWN_EVENT"),
                // A service keeps running when a user signs out.
                _ = self.logoff.recv() => {}
                _ = service.stop.notified() => return Event::Shutdown("SERVICE_CONTROL_STOP"),
                _ = service.reload.notified() => return Event::Reload,
            }
        }
    }
}

/// Locks down the directories below ProgramData the daemon runs programs
/// from, so that only administrators can change what runs as SYSTEM.
#[cfg(windows)]
fn secure_layout(config: &DaemonConfig) {
    let root = crate::win::program_data();
    let mut dirs: Vec<std::path::PathBuf> = [
        crate::config::default_config_path()
            .parent()
            .map(Path::to_path_buf),
        Some(config.components.data_dir.clone()),
        config.core.working_dir.clone(),
        config.core.binary.parent().map(Path::to_path_buf),
        config
            .core
            .config_slot()
            .and_then(|slot| slot.parent().map(Path::to_path_buf)),
    ]
    .into_iter()
    .flatten()
    .filter(|dir| dir.starts_with(&root) && dir != &root)
    .collect();
    dirs.sort_by_key(|dir| dir.components().count());
    let mut done: Vec<std::path::PathBuf> = Vec::new();
    for dir in dirs {
        // Inherited by everything below a directory already done.
        if done.iter().any(|parent| dir.starts_with(parent)) {
            continue;
        }
        if let Err(err) = crate::win::fs::secure_dir(&dir) {
            tracing::warn!("{}", crate::util::error_chain(&err));
        }
        done.push(dir);
    }
}

#[cfg(unix)]
async fn accept(listener: &mut UnixListener) -> std::io::Result<UnixStream> {
    listener.accept().await.map(|(stream, _)| stream)
}

#[cfg(windows)]
async fn accept(listener: &mut crate::win::pipe::PipeListener) -> std::io::Result<NamedPipeServer> {
    listener.accept().await
}

#[cfg(windows)]
fn bind_socket(
    path: &Path,
    group: Option<&crate::win::Sid>,
) -> Result<crate::win::pipe::PipeListener> {
    let shown = path.display().to_string();
    crate::win::pipe::PipeListener::bind(path, group).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::anyhow!(fl!("daemon-already-running", socket = shown.as_str()))
        } else {
            anyhow::Error::new(err).context(fl!("daemon-bind-failed", socket = shown.as_str()))
        }
    })
}

#[cfg(unix)]
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
    containers: Arc<ContainerManager>,
    logs: Arc<LogHub>,
    auth: Arc<Authorizer>,
    config: DaemonConfig,
}

/// Who sent a request: a uid, or a SID on Windows.
struct Peer {
    name: String,
    /// May decide what runs as root (or SYSTEM).
    privileged: bool,
}

#[cfg(unix)]
async fn serve(stream: UnixStream, ctx: Arc<Ctx>) {
    if let Err(err) = serve_client(stream, &ctx).await {
        tracing::debug!("client connection: {err:#}");
    }
}

#[cfg(windows)]
async fn serve(stream: NamedPipeServer, ctx: Arc<Ctx>) {
    if let Err(err) = serve_client(stream, &ctx).await {
        tracing::debug!("client connection: {err:#}");
    }
}

#[cfg(unix)]
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
    let uid = cred.uid();
    let peer = Peer {
        name: uid.to_string(),
        privileged: uid == 0 || uid == Uid::effective().as_raw(),
    };
    handle_request(&line, reader, write, ctx, peer).await
}

/// Windows lets a server look at its client's token only once it read from
/// the pipe, so the request comes first here; the pipe's DACL has already
/// kept out everyone the authorizer would reject.
#[cfg(windows)]
async fn serve_client(pipe: NamedPipeServer, ctx: &Ctx) -> Result<()> {
    let mut pipe = BufReader::new(pipe);
    let mut line = String::new();
    (&mut pipe)
        .take(MAX_REQUEST_BYTES)
        .read_line(&mut line)
        .await?;
    let client = crate::win::pipe::client_identity(pipe.get_ref())?;
    let pid = crate::win::pipe::client_pid(pipe.get_ref());
    let (read, mut write) = tokio::io::split(pipe);
    let user = client.user.to_string_sid();
    if !ctx.auth.is_allowed(&client) {
        tracing::warn!(
            "{}",
            fl_log!(
                "win-daemon-client-rejected",
                user = user,
                pid = pid.map(|p| p.to_string()).unwrap_or_default()
            )
        );
        let message = fl!("win-daemon-permission-denied");
        return send(&mut write, &Response::error(message)).await;
    }
    let peer = Peer {
        name: user,
        privileged: ctx.auth.is_privileged(&client),
    };
    handle_request(&line, read, write, ctx, peer).await
}

async fn handle_request<R, W>(
    line: &str,
    reader: R,
    mut write: W,
    ctx: &Ctx,
    peer: Peer,
) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWrite + Unpin,
{
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
    i18n::scope(lang, dispatch(request, reader, write, ctx, &peer)).await
}

async fn dispatch<R, W>(
    request: Request,
    reader: R,
    mut write: W,
    ctx: &Ctx,
    peer: &Peer,
) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWrite + Unpin,
{
    let requested = |what: String| {
        #[cfg(unix)]
        let line = fl_log!("daemon-request", uid = peer.name.clone(), request = what);
        #[cfg(windows)]
        let line = fl_log!(
            "win-daemon-request",
            user = peer.name.clone(),
            request = what
        );
        tracing::info!("{line}");
    };
    let op = match request {
        Request::Status => {
            let mut status = ctx.supervisor.status();
            status.setup_required = ctx.components.setup_required();
            status.components = ctx.components.statuses();
            status.active_core = ctx.cores.active();
            status.active_profile = ctx.profiles.active();
            status.update_in_progress = ctx.cores.busy();
            // Clients take `None` for a daemon without containers.
            #[cfg(unix)]
            {
                status.containers = Some(ctx.containers.summary());
            }
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
        container_request @ (Request::ContainerList
        | Request::ContainerGet { .. }
        | Request::ContainerAdd { .. }
        | Request::ContainerSave { .. }
        | Request::ContainerSet { .. }
        | Request::ContainerRemove { .. }
        | Request::ContainerAdopt { .. }
        | Request::ContainerControl { .. }
        | Request::ContainerInstall { .. }
        | Request::ContainerExec { .. }
        | Request::ContainerImages { .. }
        | Request::ContainerCheck
        | Request::ContainerScan
        | Request::ContainerRuntimeUpdate { .. }
        | Request::ContainerRuntimeImport { .. }) => {
            if !matches!(
                container_request,
                Request::ContainerList
                    | Request::ContainerGet { .. }
                    | Request::ContainerImages { .. }
            ) {
                requested(request_name(&container_request));
            }
            let response =
                handle_container(&ctx.containers, container_request, peer.privileged).await;
            return send(&mut write, &response).await;
        }
        Request::Start => Op::Start,
        Request::Stop => Op::Stop,
        Request::Restart => Op::Restart,
        Request::Reload => Op::Reload,
        Request::Check => Op::Check,
        core_request => {
            // Adding sources and importing binaries decides what runs as root.
            requested(request_name(&core_request));
            let response = handle_core(ctx, core_request, peer.privileged).await;
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
    #[cfg(unix)]
    let root_only = |action: &str| Response::error(fl!("daemon-root-only", action = action));
    #[cfg(windows)]
    let root_only = |action: &str| Response::error(fl!("win-daemon-admin-only", action = action));
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

/// Container requests. Whatever decides what runs as root inside a
/// container (configurations, root filesystems, commands, the runtime
/// binary) is reserved for root; starting and stopping registered
/// containers is open to every client, like starting sing-box.
async fn handle_container(
    containers: &ContainerManager,
    request: Request,
    privileged: bool,
) -> Response {
    let root_only = matches!(
        request,
        Request::ContainerAdd { .. }
            | Request::ContainerSave { .. }
            | Request::ContainerRemove { .. }
            | Request::ContainerAdopt { .. }
            | Request::ContainerInstall { .. }
            | Request::ContainerExec { .. }
            | Request::ContainerScan
            | Request::ContainerRuntimeImport { .. }
    );
    if root_only && !privileged {
        #[cfg(unix)]
        return Response::error(fl!("containers-root-only", action = request_name(&request)));
        #[cfg(windows)]
        return Response::error(fl!("win-containers-unsupported"));
    }
    let saved = |(container, message)| Response::ContainerSaved {
        container: Box::new(container),
        message,
    };
    match request {
        Request::ContainerList => Response::Containers(Box::new(containers.overview().await)),
        Request::ContainerGet { id } => {
            result(containers.get(&id).await.map(|(container, content)| {
                Response::ContainerContent {
                    container: Box::new(container),
                    content,
                }
            }))
        }
        Request::ContainerAdd {
            name,
            content,
            file,
            network,
        } => result(
            containers
                .add(name, content, file, network)
                .await
                .map(saved),
        ),
        Request::ContainerSave { id, content, force } => {
            result(containers.save(&id, &content, force).await.map(saved))
        }
        Request::ContainerSet {
            id,
            name,
            autostart,
        } => result(containers.set(&id, name, autostart).map(Response::done)),
        Request::ContainerRemove { id, purge } => {
            result(containers.remove(&id, purge).await.map(Response::done))
        }
        Request::ContainerAdopt { path } => {
            result(containers.adopt(path).await.map(Response::done))
        }
        Request::ContainerControl { id, action } => {
            result(containers.control(&id, action).await.map(Response::done))
        }
        Request::ContainerInstall {
            id,
            source,
            size,
            sha256,
            force,
        } => result(
            containers
                .install(&id, &source, size, sha256, force)
                .await
                .map(Response::done),
        ),
        Request::ContainerExec {
            id,
            command,
            timeout,
        } => result(
            containers
                .exec(&id, command, timeout)
                .await
                .map(Response::ContainerExec),
        ),
        Request::ContainerImages { refresh } => result(
            containers
                .images(refresh)
                .await
                .map(Response::ContainerImages),
        ),
        Request::ContainerCheck => result(containers.check().await.map(Response::done)),
        Request::ContainerScan => result(containers.scan().await.map(Response::done)),
        Request::ContainerRuntimeUpdate { tag, force } => result(
            containers
                .update_runtime(tag.as_deref(), force)
                .await
                .map(Response::done),
        ),
        Request::ContainerRuntimeImport { location, sha256 } => result(
            containers
                .import_runtime(&location, sha256.as_deref())
                .await
                .map(Response::done),
        ),
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
