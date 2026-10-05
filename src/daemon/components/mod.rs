//! Optional components (Sub-Store, http-meta): first-run setup, install,
//! enable/disable and supervision.
//!
//! One manager task owns `state.json` and serialises installs; every component
//! process runs under its own [`Service`] so crashes restart independently.

mod install;
mod state;

pub use state::{random_token, write_atomic};

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use nix::unistd::{Uid, User, chown};
use tokio::sync::{mpsc, oneshot, watch};

use self::install::Installer;
use self::state::{Layout, State};
use super::github::GitHub;
use super::logs::LogHub;
use super::service::{Service, ServiceHandle, ServiceSpec};
use crate::config::DaemonConfig;
use crate::protocol::{Component, ComponentAction, ComponentStatus, Response};

/// What the manager knows about a component besides its process state.
#[derive(Debug, Clone, Default)]
struct Entry {
    enabled: bool,
    installed: bool,
    busy: Option<String>,
    versions: BTreeMap<String, String>,
    url: Option<String>,
    api: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Info {
    setup_required: bool,
    entries: HashMap<Component, Entry>,
}

enum Msg {
    Setup {
        sub_store: bool,
        http_meta: bool,
        reply: oneshot::Sender<Response>,
    },
    Action {
        component: Component,
        action: ComponentAction,
        reply: oneshot::Sender<Response>,
    },
}

/// How long to wait for a freshly started component to accept connections.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub struct ComponentsHandle {
    tx: mpsc::Sender<Msg>,
    info: watch::Receiver<Info>,
    services: HashMap<Component, ServiceHandle>,
    started: watch::Receiver<bool>,
}

impl ComponentsHandle {
    /// Resolves once enabled components were started at daemon startup, so
    /// sing-box providers pointing at Sub-Store can fetch on first start.
    pub fn startup_gate(&self) -> watch::Receiver<bool> {
        self.started.clone()
    }

    pub fn setup_required(&self) -> bool {
        self.info.borrow().setup_required
    }

    pub fn statuses(&self) -> Vec<ComponentStatus> {
        let info = self.info.borrow();
        Component::ALL
            .iter()
            .map(|component| {
                let entry = info.entries.get(component).cloned().unwrap_or_default();
                let runtime = self.services[component].runtime();
                ComponentStatus {
                    component: *component,
                    enabled: entry.enabled,
                    installed: entry.installed,
                    busy: entry.busy,
                    state: runtime.state,
                    pid: runtime.pid,
                    started_at: runtime.started_at,
                    restarts: runtime.restarts,
                    last_exit: runtime.last_exit,
                    next_restart_at: runtime.next_restart_at,
                    versions: entry.versions,
                    url: entry.url,
                    api: entry.api,
                }
            })
            .collect()
    }

    pub async fn setup(&self, sub_store: bool, http_meta: bool) -> Response {
        let (reply, rx) = oneshot::channel();
        self.send(
            Msg::Setup {
                sub_store,
                http_meta,
                reply,
            },
            rx,
        )
        .await
    }

    pub async fn action(&self, component: Component, action: ComponentAction) -> Response {
        let (reply, rx) = oneshot::channel();
        self.send(
            Msg::Action {
                component,
                action,
                reply,
            },
            rx,
        )
        .await
    }

    async fn send(&self, msg: Msg, rx: oneshot::Receiver<Response>) -> Response {
        if self.tx.send(msg).await.is_err() {
            return Response::error("daemon is shutting down");
        }
        rx.await
            .unwrap_or_else(|_| Response::error("daemon is shutting down"))
    }

    /// Stops all component processes, even while the manager is busy installing.
    pub async fn shutdown(&self) {
        for service in self.services.values() {
            service.shutdown().await;
        }
    }
}

struct Manager {
    config: DaemonConfig,
    logs: Arc<LogHub>,
    layout: Layout,
    state: State,
    services: HashMap<Component, ServiceHandle>,
    busy: HashMap<Component, String>,
    /// `node --version` of the runtime last used to start a component.
    node_runtime: Option<String>,
    info_tx: watch::Sender<Info>,
    started_tx: watch::Sender<bool>,
    rx: mpsc::Receiver<Msg>,
}

pub fn spawn(config: DaemonConfig, logs: Arc<LogHub>) -> ComponentsHandle {
    let layout = Layout::new(config.components.data_dir.clone());
    let state = State::load(&layout.state()).unwrap_or_else(|err| {
        logs.warn(format!("{err:#}; starting with empty component state"));
        State::default()
    });
    let services: HashMap<Component, ServiceHandle> = Component::ALL
        .iter()
        .map(|c| {
            let handle = Service::spawn(
                c.title(),
                c.log_source(),
                logs.clone(),
                config.restart.clone(),
            );
            (*c, handle)
        })
        .collect();
    let (tx, rx) = mpsc::channel(8);
    let manager = Manager {
        config,
        logs,
        layout,
        state,
        services: services.clone(),
        busy: HashMap::new(),
        node_runtime: None,
        info_tx: watch::Sender::new(Info::default()),
        started_tx: watch::Sender::new(false),
        rx,
    };
    manager.publish();
    let handle = ComponentsHandle {
        tx,
        info: manager.info_tx.subscribe(),
        services,
        started: manager.started_tx.subscribe(),
    };
    tokio::spawn(manager.run());
    handle
}

impl Manager {
    async fn run(mut self) {
        if !self.state.setup_done {
            self.logs.info(
                "optional components not configured yet; answer the first-run question in the TUI or run `singbox-board setup`",
            );
        }
        for component in Component::ALL {
            if self.state.enabled(component)
                && let Err(err) = self.ensure_running(component).await
            {
                self.logs
                    .warn(format!("{} failed to start: {err:#}", component.title()));
            }
        }
        self.started_tx.send_replace(true);
        while let Some(msg) = self.rx.recv().await {
            match msg {
                Msg::Setup {
                    sub_store,
                    http_meta,
                    reply,
                } => {
                    let response = self.setup(sub_store, http_meta).await;
                    let _ = reply.send(response);
                }
                Msg::Action {
                    component,
                    action,
                    reply,
                } => {
                    let response = match self.action(component, action).await {
                        Ok(message) => Response::done(message),
                        Err(err) => Response::error(format!("{err:#}")),
                    };
                    let _ = reply.send(response);
                }
            }
        }
    }

    fn publish(&self) {
        let host = |h: &str| local_host(h);
        let mut entries = HashMap::new();
        for component in Component::ALL {
            let mut versions = BTreeMap::new();
            let (url, api) = match component {
                Component::SubStore => {
                    let s = &self.state.sub_store;
                    insert(&mut versions, "backend", &s.backend_version);
                    insert(&mut versions, "frontend", &s.frontend_version);
                    let cfg = &self.config.sub_store;
                    if s.backend_path.is_empty() {
                        (None, None)
                    } else {
                        let base = format!("http://{}:{}", host(&cfg.host), cfg.port);
                        let api = format!("{base}/{}", s.backend_path);
                        (Some(format!("{base}/?api={api}")), Some(api))
                    }
                }
                Component::HttpMeta => {
                    let s = &self.state.http_meta;
                    insert(&mut versions, "http-meta", &s.version);
                    insert(&mut versions, "mihomo", &s.mihomo_version);
                    let cfg = &self.config.http_meta;
                    (
                        Some(format!("http://{}:{}", host(&cfg.host), cfg.port)),
                        None,
                    )
                }
            };
            insert(&mut versions, "node", &self.node_runtime);
            entries.insert(
                component,
                Entry {
                    enabled: self.state.enabled(component),
                    installed: self.layout.installed(component),
                    busy: self.busy.get(&component).cloned(),
                    versions,
                    url,
                    api,
                },
            );
        }
        self.info_tx.send_replace(Info {
            setup_required: !self.state.setup_done,
            entries,
        });
    }

    fn save(&self) -> Result<()> {
        self.state.save(&self.layout.state())
    }

    fn installer(&self) -> Result<Installer<'_>> {
        Ok(Installer {
            github: GitHub::new(&self.config.update)?,
            config: &self.config,
            layout: &self.layout,
            logs: &self.logs,
        })
    }

    async fn setup(&mut self, sub_store: bool, http_meta: bool) -> Response {
        self.state.setup_done = true;
        self.state.set_enabled(Component::SubStore, sub_store);
        self.state.set_enabled(Component::HttpMeta, http_meta);
        if let Err(err) = self.save() {
            return Response::error(format!("{err:#}"));
        }
        self.publish();
        self.logs.info(format!(
            "setup: Sub-Store {}, http-meta {}",
            on_off(sub_store),
            on_off(http_meta)
        ));
        let mut lines = Vec::new();
        let mut failed = false;
        for component in Component::ALL {
            if self.state.enabled(component) {
                match self.ensure_running(component).await {
                    Ok(()) => lines.push(self.describe(component)),
                    Err(err) => {
                        failed = true;
                        lines.push(format!("{}: {err:#}", component.title()));
                    }
                }
            } else {
                self.services[&component].stop().await;
                lines.push(format!("{}: disabled", component.title()));
            }
        }
        let message = lines.join("\n");
        if failed {
            Response::error(message)
        } else {
            Response::done(message)
        }
    }

    async fn action(&mut self, component: Component, action: ComponentAction) -> Result<String> {
        let title = component.title();
        match action {
            ComponentAction::Start | ComponentAction::Restart => {
                if !self.state.enabled(component) {
                    bail!(
                        "{title} is disabled; enable it with `singbox-board component {} enable`",
                        component.name()
                    );
                }
                self.ensure_running(component).await?;
                Ok(self.describe(component))
            }
            ComponentAction::Stop => Ok(match self.services[&component].stop().await {
                Some(exit) => format!("{title} stopped ({exit})"),
                None => format!("{title} is not running"),
            }),
            ComponentAction::Enable => {
                self.state.setup_done = true;
                self.state.set_enabled(component, true);
                self.save()?;
                self.publish();
                self.ensure_running(component).await?;
                Ok(self.describe(component))
            }
            ComponentAction::Disable => {
                self.state.set_enabled(component, false);
                self.save()?;
                self.publish();
                self.services[&component].stop().await;
                Ok(format!("{title} disabled"))
            }
            ComponentAction::Update => {
                let changed = self.install(component, "updating").await?;
                let running = self.services[&component].runtime().pid.is_some();
                if !changed {
                    return Ok(format!("{title} is up to date"));
                }
                if running {
                    self.ensure_running(component).await?;
                }
                Ok(format!("{title} updated\n{}", self.describe(component)))
            }
        }
    }

    /// Downloads missing or outdated files; returns whether anything changed.
    async fn install(&mut self, component: Component, label: &str) -> Result<bool> {
        self.busy.insert(component, label.to_owned());
        self.publish();
        let mut state = self.state.clone();
        let result = async {
            let installer = self.installer()?;
            match component {
                Component::SubStore => installer.sub_store(&mut state).await,
                Component::HttpMeta => installer.http_meta(&mut state).await,
            }
        }
        .await;
        self.state = state;
        self.busy.remove(&component);
        let saved = self.save();
        self.publish();
        let changed = result.with_context(|| format!("install {}", component.title()))?;
        saved?;
        Ok(changed)
    }

    async fn ensure_running(&mut self, component: Component) -> Result<()> {
        if !self.layout.installed(component) {
            self.install(component, "installing").await?;
        }
        let spec = self.spec(component).await?;
        self.services[&component].start(spec).await?;
        let (host, port) = match component {
            Component::SubStore => (&self.config.sub_store.host, self.config.sub_store.port),
            Component::HttpMeta => (&self.config.http_meta.host, self.config.http_meta.port),
        };
        let address = format!("{}:{port}", local_host(host));
        if !wait_for_port(&address, READY_TIMEOUT).await {
            self.logs.warn(format!(
                "{} is running but {address} did not accept connections within {}s",
                component.title(),
                READY_TIMEOUT.as_secs()
            ));
        }
        Ok(())
    }

    async fn spec(&mut self, component: Component) -> Result<ServiceSpec> {
        let mut state = self.state.clone();
        let node = {
            let installer = self.installer()?;
            installer.node(&mut state).await?
        };
        if state.node_version != self.state.node_version {
            self.state.node_version = state.node_version;
            self.save()?;
        }
        let runtime = install::node_version(&node).await.ok().map(|(_, v)| v);
        if runtime != self.node_runtime {
            self.node_runtime = runtime;
            self.publish();
        }
        let user = self.run_as()?;
        let layout = &self.layout;
        Ok(match component {
            Component::SubStore => {
                let cfg = &self.config.sub_store;
                let data = layout.sub_store_data();
                writable_dir(&data, user)?;
                let mut env = vec![
                    ("SUB_STORE_BACKEND_API_HOST", cfg.host.clone()),
                    ("SUB_STORE_BACKEND_API_PORT", cfg.port.to_string()),
                    ("SUB_STORE_BACKEND_MERGE", "true".to_owned()),
                    (
                        "SUB_STORE_FRONTEND_PATH",
                        path_str(&layout.sub_store_frontend()),
                    ),
                    (
                        "SUB_STORE_FRONTEND_BACKEND_PATH",
                        format!("/{}", self.state.sub_store.backend_path),
                    ),
                    ("SUB_STORE_DATA_BASE_PATH", path_str(&data)),
                    ("HOME", path_str(&data)),
                ];
                let optional = [
                    ("SUB_STORE_BACKEND_SYNC_CRON", &cfg.sync_cron),
                    ("SUB_STORE_PRODUCE_CRON", &cfg.produce_cron),
                    ("SUB_STORE_BACKEND_DEFAULT_PROXY", &cfg.default_proxy),
                ];
                for (key, value) in optional {
                    if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
                        env.push((key, value.clone()));
                    }
                }
                ServiceSpec {
                    program: node,
                    args: vec![path_str(&layout.sub_store_bundle())],
                    env: merge_env(env, &cfg.env),
                    cwd: data,
                    user,
                    reap_exe: None,
                }
            }
            Component::HttpMeta => {
                let cfg = &self.config.http_meta;
                let temp = layout.meta_temp();
                writable_dir(&temp, user)?;
                let mut env = vec![
                    ("META_FOLDER", path_str(&layout.meta_folder())),
                    ("META_TEMP_FOLDER", path_str(&temp)),
                    ("HOST", cfg.host.clone()),
                    ("PORT", cfg.port.to_string()),
                    ("HOME", path_str(&temp)),
                ];
                if let Some(auth) = cfg.authorization.as_ref().filter(|a| !a.is_empty()) {
                    env.push(("AUTHORIZATION", auth.clone()));
                }
                ServiceSpec {
                    program: node,
                    args: vec![path_str(&layout.http_meta_bundle())],
                    env: merge_env(env, &cfg.env),
                    cwd: temp,
                    user,
                    reap_exe: Some(layout.mihomo()),
                }
            }
        })
    }

    /// uid/gid of `components.run_as`; `None` when the daemon is not root.
    fn run_as(&self) -> Result<Option<(u32, u32)>> {
        let name = self.config.components.run_as.trim();
        if !Uid::effective().is_root() || name.is_empty() || name == "root" {
            return Ok(None);
        }
        let user = User::from_name(name)?
            .ok_or_else(|| anyhow!("user {name:?} does not exist; set components.run_as"))?;
        Ok(Some((user.uid.as_raw(), user.gid.as_raw())))
    }

    fn describe(&self, component: Component) -> String {
        let runtime = self.services[&component].runtime();
        let info = self.info_tx.borrow();
        let url = info
            .entries
            .get(&component)
            .and_then(|e| e.url.clone())
            .unwrap_or_default();
        match runtime.pid {
            Some(pid) => format!("{} running (pid {pid}) at {url}", component.title()),
            None => format!(
                "{} {}",
                component.title(),
                runtime.state.label().to_lowercase()
            ),
        }
    }
}

fn insert(map: &mut BTreeMap<String, String>, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_owned(), value.clone());
    }
}

fn on_off(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn path_str(path: &Path) -> String {
    path.display().to_string()
}

fn merge_env(base: Vec<(&str, String)>, extra: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = base.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    for (key, value) in extra {
        env.retain(|(k, _)| k != key);
        env.push((key.clone(), value.clone()));
    }
    env
}

/// Creates a directory the service user can write to.
fn writable_dir(dir: &Path, user: Option<(u32, u32)>) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    if let Some((uid, gid)) = user {
        chown(dir, Some(uid.into()), Some(gid.into()))
            .with_context(|| format!("chown {}", dir.display()))?;
    }
    Ok(())
}

async fn wait_for_port(address: &str, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// A host to reach a listener on from this machine.
fn local_host(host: &str) -> String {
    match host.trim_start_matches('[').trim_end_matches(']') {
        "" | "0.0.0.0" => "127.0.0.1".to_owned(),
        "::" => "[::1]".to_owned(),
        h if h.contains(':') => format!("[{h}]"),
        h => h.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hosts() {
        assert_eq!(local_host("0.0.0.0"), "127.0.0.1");
        assert_eq!(local_host("::"), "[::1]");
        assert_eq!(local_host("fd00::1"), "[fd00::1]");
        assert_eq!(local_host("192.168.1.2"), "192.168.1.2");
    }

    #[test]
    fn extra_env_overrides() {
        let mut extra = BTreeMap::new();
        extra.insert("PORT".to_owned(), "1".to_owned());
        extra.insert("X".to_owned(), "y".to_owned());
        let env = merge_env(
            vec![("PORT", "9876".to_owned()), ("HOST", "h".to_owned())],
            &extra,
        );
        assert_eq!(
            env,
            [
                ("HOST".to_owned(), "h".to_owned()),
                ("PORT".to_owned(), "1".to_owned()),
                ("X".to_owned(), "y".to_owned())
            ]
        );
    }
}
