//! Containers run by kurumi-containerd (Tools-cx-app/kurumi-containerd).
//!
//! The daemon keeps a registry of container configurations (kurumi TOML
//! files), drives the runtime's command line for every change (start, stop,
//! installing a root filesystem, running commands) and reads the live state
//! of running containers from the runtime's state files and procfs.
//!
//! Layout below `<data_dir>/containers/` (root only):
//!
//! ```text
//! registry.json                    registered containers
//! <id>/container.toml, <id>/rootfs  containers created by the daemon
//! .kurumi-containerd/config.json    the runtime's registry, from registry.json
//! runtime/kurumi-containerd         a downloaded runtime, with meta.json
//! .downloads/                       root filesystem archives being fetched
//! ```
//!
//! Configurations registered from elsewhere (`container add --link`,
//! `container adopt`) stay where they are.

mod images;
mod kurumi;
mod live;
mod runtime;

use std::collections::HashMap;
use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use nix::unistd::Uid;
use serde::{Deserialize, Serialize};

use self::images::IndexEntry;
use self::kurumi::{Kurumi, Output, Scope, args};
use self::live::Procfs;
use self::runtime::RuntimeStore;
use super::github::{GitHub, parse_sha256sums};
use super::logs::LogHub;
use crate::config::DaemonConfig;
use crate::container::{self as spec, TemplateNetwork};
use crate::i18n::{fl, fl_log};
use crate::protocol::{
    Container, ContainerAction, ContainerLive, ContainerOverview, ContainerRuntime, ContainerSpec,
    ContainerSummary, CoreState, ExecResult, ImageList, LogSource,
};
use crate::util::{error_chain, fmt_bytes, now_unix, now_unix_ms, random_token, write_atomic};

const REGISTRY: &str = "registry.json";
const CONFIG: &str = "container.toml";
const AUTOSTART_MARKER: &str = ".autostarted-boot";
const START_TIMEOUT: Duration = Duration::from_secs(120);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const IMAGE_INDEX_TTL: Duration = Duration::from_secs(600);
/// Root filesystem archives are a few hundred MiB; refuse absurd ones.
const MAX_ARCHIVE: u64 = 16 * 1024 * 1024 * 1024;
const DEFAULT_EXEC_TIMEOUT: u64 = 60;
const MAX_EXEC_TIMEOUT: u64 = 60 * 60;
/// Containers stopped in parallel when the system shuts down.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(25);

/// A registered container.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    name: String,
    /// A configuration registered where it is; `None` for one in the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<PathBuf>,
    #[serde(default)]
    autostart: bool,
    created_at: u64,
    updated_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    containers: Vec<Entry>,
}

/// One entry of the runtime's registry (`config.json`).
#[derive(Debug, Serialize, Deserialize)]
struct Pointer {
    name: String,
    file: PathBuf,
}

/// `--version` of a binary, and its mtime when it was asked.
type ProbedVersion = (Option<SystemTime>, Option<String>);

struct CachedSpec {
    modified: Option<SystemTime>,
    len: u64,
    result: Result<ContainerSpec, String>,
}

/// Why the last operation on a container failed.
#[derive(Debug, Clone)]
struct Failure {
    /// A start failed: the container is shown as failed until it runs.
    start: bool,
    message: String,
}

pub struct ContainerManager {
    config: DaemonConfig,
    logs: Arc<LogHub>,
    root: PathBuf,
    runtime: RuntimeStore,
    procfs: Procfs,
    /// The daemon runs as root; containers need it.
    privileged: bool,
    /// Never held across an await: the daemon runs on a single thread.
    index: Mutex<Vec<Entry>>,
    specs: Mutex<HashMap<PathBuf, CachedSpec>>,
    busy: Mutex<HashMap<String, String>>,
    failures: Mutex<HashMap<String, Failure>>,
    runtime_busy: Mutex<Option<String>>,
    runtime_lock: tokio::sync::Mutex<()>,
    /// `--version` of runtimes the daemon did not install, by path and mtime.
    versions: Mutex<HashMap<PathBuf, ProbedVersion>>,
    images: tokio::sync::Mutex<Option<(Instant, Vec<IndexEntry>)>>,
}

/// Clears a container's busy mark when an operation ends, however it ends.
struct Busy<'a> {
    map: &'a Mutex<HashMap<String, String>>,
    id: String,
}

impl Busy<'_> {
    fn set(&self, what: impl Into<String>) {
        self.map
            .lock()
            .expect("busy map")
            .insert(self.id.clone(), what.into());
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.map.lock().expect("busy map").remove(&self.id);
    }
}

impl ContainerManager {
    pub fn new(config: DaemonConfig, logs: Arc<LogHub>) -> Arc<Self> {
        let privileged = Uid::effective().is_root();
        Self::build(config, logs, Procfs::default(), privileged)
    }

    fn build(
        config: DaemonConfig,
        logs: Arc<LogHub>,
        procfs: Procfs,
        privileged: bool,
    ) -> Arc<Self> {
        let root = config.components.data_dir.join("containers");
        let path = root.join(REGISTRY);
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<RegistryFile>(&text)
                .map(|file| file.containers)
                .unwrap_or_else(|err| {
                    logs.warn(fl_log!(
                        "containers-registry-ignored",
                        path = path.display().to_string(),
                        error = err.to_string()
                    ));
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        };
        let runtime = RuntimeStore::new(root.join("runtime"), config.containers.runtime.clone());
        if config.containers.systemd_scope && privileged {
            // Probe systemd-run off the daemon's thread before the first start.
            tokio::task::spawn_blocking(kurumi::warm_up);
        }
        Arc::new(Self {
            runtime,
            procfs,
            privileged,
            index: Mutex::new(entries),
            specs: Mutex::new(HashMap::new()),
            busy: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            runtime_busy: Mutex::new(None),
            runtime_lock: tokio::sync::Mutex::new(()),
            versions: Mutex::new(HashMap::new()),
            images: tokio::sync::Mutex::new(None),
            config,
            logs,
            root,
        })
    }

    // ----- registry -----------------------------------------------------

    fn entries(&self) -> Vec<Entry> {
        self.index.lock().expect("container index").clone()
    }

    fn file_of(&self, entry: &Entry) -> PathBuf {
        entry
            .file
            .clone()
            .unwrap_or_else(|| self.root.join(&entry.id).join(CONFIG))
    }

    /// Configurations, root filesystems and the runtime's registry are root's.
    fn ensure_root_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| fl!("err-create", path = self.root.display().to_string()))?;
        std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn home(&self) -> PathBuf {
        self.root.clone()
    }

    fn save_index(&self, entries: &[Entry]) -> Result<()> {
        self.ensure_root_dir()?;
        let file = RegistryFile {
            containers: entries.to_vec(),
        };
        write_atomic(
            &self.root.join(REGISTRY),
            &serde_json::to_vec_pretty(&file)?,
            0o600,
        )?;
        self.write_pointer(entries)
    }

    /// The runtime's registry: one entry per container, named by its id.
    fn write_pointer(&self, entries: &[Entry]) -> Result<()> {
        let dir = self.home().join(".kurumi-containerd");
        std::fs::create_dir_all(&dir)
            .with_context(|| fl!("err-create", path = dir.display().to_string()))?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let pointers: Vec<Pointer> = entries
            .iter()
            .map(|entry| Pointer {
                name: entry.id.clone(),
                file: self.file_of(entry),
            })
            .collect();
        let path = dir.join("config.json");
        if pointers.is_empty() {
            // The runtime rejects an empty list; no command needs it then.
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        write_atomic(&path, &serde_json::to_vec_pretty(&pointers)?, 0o600)
    }

    /// Applies `change` to the entry `id` and saves the registry.
    fn modify(&self, id: &str, change: impl FnOnce(&mut Entry)) -> Result<Entry> {
        let mut index = self.index.lock().expect("container index");
        let entry = index
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| anyhow!(fl!("containers-not-found", query = id)))?;
        change(entry);
        entry.updated_at = now_unix();
        let updated = entry.clone();
        self.save_index(&index)?;
        Ok(updated)
    }

    /// A container by id, name (exact, then ignoring ASCII case), its
    /// `container.name`, or a unique id prefix.
    fn resolve(&self, query: &str) -> Result<Entry> {
        let query = query.trim();
        let entries = self.entries();
        let found = entries
            .iter()
            .find(|e| e.id == query)
            .or_else(|| unique(entries.iter().filter(|e| e.name == query)))
            .or_else(|| {
                unique(
                    entries
                        .iter()
                        .filter(|e| e.name.eq_ignore_ascii_case(query)),
                )
            })
            .or_else(|| {
                unique(entries.iter().filter(|e| {
                    self.spec_of(e)
                        .is_ok_and(|spec| spec.name.eq_ignore_ascii_case(query))
                }))
            })
            .or_else(|| {
                unique(
                    entries
                        .iter()
                        .filter(|e| query.len() >= 2 && e.id.starts_with(query)),
                )
            });
        match found {
            Some(entry) => Ok(entry.clone()),
            None => bail!(fl!("containers-not-found", query = query)),
        }
    }

    /// The parsed configuration, re-read when the file changes.
    fn spec_of(&self, entry: &Entry) -> Result<ContainerSpec, String> {
        let path = self.file_of(entry);
        let meta = std::fs::metadata(&path);
        let (modified, len) = match &meta {
            Ok(meta) => (meta.modified().ok(), meta.len()),
            Err(_) => (None, 0),
        };
        {
            let cache = self.specs.lock().expect("spec cache");
            if let Some(cached) = cache.get(&path)
                && meta.is_ok()
                && cached.modified == modified
                && cached.len == len
            {
                return cached.result.clone().map(|mut spec| {
                    spec.installed =
                        spec::installed(Path::new(&spec.rootfs), spec.image, &spec.init);
                    spec
                });
            }
        }
        let result = match std::fs::read_to_string(&path) {
            Ok(content) => spec::parse(&content, &path).map_err(|err| error_chain(&err)),
            Err(err) => Err(format!(
                "{}{}{err}",
                fl!("err-read", path = path.display().to_string()),
                fl!("chain-separator")
            )),
        };
        self.specs.lock().expect("spec cache").insert(
            path,
            CachedSpec {
                modified,
                len,
                result: result.clone(),
            },
        );
        result
    }

    fn live_of(&self, names: &[&str], usage: bool) -> HashMap<String, ContainerLive> {
        if usage {
            self.procfs.live(names)
        } else {
            self.procfs.running(names)
        }
    }

    fn describe(&self, entry: &Entry, live: &HashMap<String, ContainerLive>) -> Container {
        let spec = self.spec_of(entry);
        let live = spec
            .as_ref()
            .ok()
            .and_then(|spec| live.get(&spec.name).cloned());
        let busy = self.busy.lock().expect("busy map").get(&entry.id).cloned();
        let failure = self
            .failures
            .lock()
            .expect("failures")
            .get(&entry.id)
            .cloned();
        let state = match (busy.as_deref(), &live) {
            (Some("starting" | "restarting"), _) => CoreState::Starting,
            (Some("stopping"), _) => CoreState::Stopping,
            (_, Some(live)) if live.rebooting => CoreState::Starting,
            (_, Some(_)) => CoreState::Running,
            (_, None) if failure.as_ref().is_some_and(|f| f.start) => CoreState::Failed,
            (_, None) => CoreState::Stopped,
        };
        let (spec, spec_error) = match spec {
            Ok(spec) => (Some(spec), None),
            Err(err) => (None, Some(err)),
        };
        Container {
            id: entry.id.clone(),
            name: entry.name.clone(),
            file: self.file_of(entry).display().to_string(),
            managed: entry.file.is_none(),
            autostart: entry.autostart,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
            state,
            busy,
            last_error: failure.map(|f| f.message),
            spec,
            spec_error,
            live,
        }
    }

    fn container(&self, id: &str) -> Result<Container> {
        let entry = self.resolve(id)?;
        let names = self.spec_of(&entry).map(|s| s.name).unwrap_or_default();
        let live = self.live_of(&[names.as_str()], true);
        Ok(self.describe(&entry, &live))
    }

    /// `container.name`s of all registered containers but `except`.
    fn other_names(&self, except: Option<&str>) -> Vec<String> {
        self.entries()
            .iter()
            .filter(|e| Some(e.id.as_str()) != except)
            .filter_map(|e| self.spec_of(e).ok().map(|s| s.name))
            .collect()
    }

    fn check_unique_name(&self, spec: &ContainerSpec, except: Option<&str>) -> Result<()> {
        if self.other_names(except).contains(&spec.name) {
            bail!(fl!("containers-name-taken", name = spec.name.clone()));
        }
        Ok(())
    }

    // ----- overview -----------------------------------------------------

    pub fn summary(&self) -> ContainerSummary {
        let entries = self.entries();
        let names: Vec<String> = entries
            .iter()
            .filter_map(|e| self.spec_of(e).ok().map(|s| s.name))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        ContainerSummary {
            total: entries.len(),
            running: if self.privileged {
                self.live_of(&refs, false).len()
            } else {
                0
            },
            runtime: self.runtime.resolve().is_some(),
        }
    }

    pub async fn overview(&self) -> ContainerOverview {
        let entries = self.entries();
        let names: Vec<String> = entries
            .iter()
            .filter_map(|e| self.spec_of(e).ok().map(|s| s.name))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let live = self.live_of(&refs, true);
        ContainerOverview {
            runtime: self.runtime_info().await,
            containers: entries.iter().map(|e| self.describe(e, &live)).collect(),
            home: self.home().display().to_string(),
        }
    }

    async fn runtime_info(&self) -> ContainerRuntime {
        let resolved = self.runtime.resolve();
        let version = match &resolved {
            Some(resolved) => match &resolved.meta {
                Some(meta) => Some(meta.version.clone()),
                None => self.probe_version(&resolved.binary).await,
            },
            None => None,
        };
        let problem = if !self.privileged {
            Some(fl!("containers-need-root-daemon"))
        } else if resolved.is_none() && runtime::release_target().is_none() {
            Some(fl!(
                "containers-runtime-unsupported",
                arch = std::env::consts::ARCH
            ))
        } else {
            None
        };
        let meta = resolved.as_ref().and_then(|r| r.meta.clone());
        ContainerRuntime {
            binary: resolved.as_ref().map(|r| r.binary.display().to_string()),
            version,
            origin: resolved.as_ref().map(|r| r.origin),
            tag: meta.as_ref().and_then(|m| m.tag.clone()),
            checksum: meta.map(|m| m.checksum).unwrap_or_default(),
            busy: self.runtime_busy.lock().expect("runtime busy").clone(),
            target: runtime::release_target().map(str::to_owned),
            problem,
        }
    }

    /// `--version` of a runtime the daemon did not install, cached by mtime.
    async fn probe_version(&self, binary: &Path) -> Option<String> {
        let modified = std::fs::metadata(binary).and_then(|m| m.modified()).ok();
        if let Some((at, version)) = self.versions.lock().expect("versions").get(binary)
            && *at == modified
        {
            return version.clone();
        }
        let version = runtime::version(binary).await.ok();
        self.versions
            .lock()
            .expect("versions")
            .insert(binary.to_path_buf(), (modified, version.clone()));
        version
    }

    pub async fn get(&self, query: &str) -> Result<(Container, String)> {
        let entry = self.resolve(query)?;
        let path = self.file_of(&entry);
        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| fl!("err-read", path = path.display().to_string()))?;
        Ok((self.container(&entry.id)?, content))
    }

    // ----- runtime ------------------------------------------------------

    fn require_root(&self) -> Result<()> {
        if !self.privileged {
            bail!(fl!("containers-need-root-daemon"));
        }
        Ok(())
    }

    /// The runtime to run, downloading the newest release first when none
    /// is installed and `install` is set.
    async fn kurumi(&self, install: bool) -> Result<Kurumi> {
        self.require_root()?;
        if self.runtime.resolve().is_none() {
            if !install {
                bail!(fl!("containers-runtime-missing"));
            }
            self.logs.info(fl_log!("containers-runtime-auto-install"));
            self.update_runtime(None, false).await?;
        }
        let resolved = self
            .runtime
            .resolve()
            .ok_or_else(|| anyhow!(fl!("containers-runtime-missing")))?;
        Ok(Kurumi {
            binary: resolved.binary,
            home: self.home(),
        })
    }

    fn github(&self) -> Result<GitHub> {
        GitHub::new(&self.config.update)
    }

    /// Installs the newest (or `tag`) release of kurumi-containerd.
    pub async fn update_runtime(&self, tag: Option<&str>, force: bool) -> Result<String> {
        self.require_root()?;
        if let Some(path) = &self.config.containers.runtime {
            bail!(fl!(
                "containers-runtime-configured",
                path = path.display().to_string()
            ));
        }
        self.ensure_root_dir()?;
        let _guard = self.runtime_lock.lock().await;
        let current = self
            .runtime
            .meta()
            .filter(|_| self.runtime.binary_path().is_file());
        let label = if current.is_some() {
            "updating"
        } else {
            "installing"
        };
        *self.runtime_busy.lock().expect("runtime busy") = Some(label.to_owned());
        let result = async {
            let github = self.github()?;
            let repo = &self.config.containers.repo;
            let release = github
                .release(repo, tag.filter(|t| !t.is_empty()), false)
                .await
                .with_context(|| fl!("containers-runtime-lookup-failed", repo = repo.clone()))?;
            if !force
                && current
                    .as_ref()
                    .is_some_and(|meta| meta.tag.as_deref() == Some(release.tag_name.as_str()))
            {
                return Ok(fl!(
                    "up-to-date",
                    name = format!("kurumi-containerd {}", release.version())
                ));
            }
            self.logs.info(fl_log!(
                "containers-runtime-downloading",
                tag = release.tag_name.clone(),
                repo = repo.clone()
            ));
            let meta = self.runtime.install_release(&github, &release).await?;
            self.logs.info(fl_log!(
                "containers-runtime-installed-log",
                version = meta.version.clone(),
                checksum = crate::i18n::in_log_language(|| meta.checksum.description())
            ));
            Ok(fl!(
                "containers-runtime-installed",
                version = meta.version.clone(),
                checksum = meta.checksum.description()
            ))
        }
        .await;
        *self.runtime_busy.lock().expect("runtime busy") = None;
        result
    }

    pub async fn import_runtime(&self, location: &str, sha256: Option<&str>) -> Result<String> {
        self.require_root()?;
        self.ensure_root_dir()?;
        let _guard = self.runtime_lock.lock().await;
        *self.runtime_busy.lock().expect("runtime busy") = Some("installing".to_owned());
        let result = async {
            let meta = self
                .runtime
                .import(&self.github()?, location, sha256)
                .await?;
            self.logs.info(fl_log!(
                "containers-runtime-installed-log",
                version = meta.version.clone(),
                checksum = crate::i18n::in_log_language(|| meta.checksum.description())
            ));
            let mut message = fl!(
                "containers-runtime-installed",
                version = meta.version.clone(),
                checksum = meta.checksum.description()
            );
            if let Some(path) = &self.config.containers.runtime {
                message.push('\n');
                message.push_str(&fl!(
                    "containers-runtime-configured",
                    path = path.display().to_string()
                ));
            }
            Ok(message)
        }
        .await;
        *self.runtime_busy.lock().expect("runtime busy") = None;
        result
    }

    // ----- running the runtime ------------------------------------------

    fn begin<'a>(&'a self, entry: &Entry, what: &str) -> Result<Busy<'a>> {
        let mut busy = self.busy.lock().expect("busy map");
        if let Some(current) = busy.get(&entry.id) {
            bail!(fl!(
                "containers-busy",
                name = entry.name.clone(),
                action = crate::protocol::container_busy_label(current)
            ));
        }
        busy.insert(entry.id.clone(), what.to_owned());
        Ok(Busy {
            map: &self.busy,
            id: entry.id.clone(),
        })
    }

    fn record(&self, entry: &Entry, failure: Option<Failure>) {
        let mut failures = self.failures.lock().expect("failures");
        match failure {
            Some(failure) => failures.insert(entry.id.clone(), failure),
            None => failures.remove(&entry.id),
        };
    }

    /// Copies the runtime's diagnostics into the log, tagged with the container.
    fn log_output(&self, entry: &Entry, output: &Output) {
        for line in output.stderr_lines() {
            self.logs.push(
                LogSource::Containers,
                &format!("{}: {}", entry.name, kurumi::without_timestamp(&line)),
            );
        }
    }

    async fn run(
        &self,
        kurumi: &Kurumi,
        entry: &Entry,
        command: &[OsString],
        timeout: Duration,
        scope: Option<&Scope>,
    ) -> Result<Output> {
        self.write_pointer(&self.entries())?;
        let mut full = args(["--name", entry.id.as_str()]);
        full.extend(command.iter().cloned());
        let output = kurumi.run(&full, timeout, scope).await?;
        self.log_output(entry, &output);
        Ok(output)
    }

    fn running(&self, spec: &ContainerSpec) -> Option<ContainerLive> {
        self.live_of(&[spec.name.as_str()], false)
            .remove(&spec.name)
    }

    pub async fn control(&self, query: &str, action: ContainerAction) -> Result<String> {
        let entry = self.resolve(query)?;
        let spec = self.spec_of(&entry).map_err(|err| {
            anyhow!(fl!(
                "containers-config-broken",
                name = entry.name.clone(),
                error = err
            ))
        })?;
        let kurumi = self.kurumi(action != ContainerAction::Stop).await?;
        let busy = self.begin(
            &entry,
            match action {
                ContainerAction::Start => "starting",
                ContainerAction::Stop => "stopping",
                ContainerAction::Restart => "restarting",
            },
        )?;
        match action {
            ContainerAction::Start => self.start(&kurumi, &entry, &spec).await,
            ContainerAction::Stop => self.stop(&kurumi, &entry, &spec).await,
            ContainerAction::Restart => {
                let mut lines = Vec::new();
                if self.running(&spec).is_some() {
                    busy.set("stopping");
                    lines.push(self.stop(&kurumi, &entry, &spec).await?);
                }
                busy.set("starting");
                lines.push(self.start(&kurumi, &entry, &spec).await?);
                Ok(lines.join("\n"))
            }
        }
    }

    async fn start(&self, kurumi: &Kurumi, entry: &Entry, spec: &ContainerSpec) -> Result<String> {
        if let Some(live) = self.running(spec) {
            return Ok(fl!(
                "containers-already-running",
                name = entry.name.clone(),
                pid = live.init_pid.to_string()
            ));
        }
        if spec.foreground {
            bail!(fl!("containers-foreground", name = entry.name.clone()));
        }
        if !spec.installed {
            bail!(fl!(
                "containers-not-installed",
                name = entry.name.clone(),
                rootfs = spec.rootfs.clone()
            ));
        }
        let scope = self.config.containers.systemd_scope.then(|| Scope {
            unit: format!("singbox-board-container-{}-{}", entry.id, now_unix_ms()),
            description: format!("singbox-board container {}", entry.name),
        });
        self.logs.info(fl_log!(
            "containers-starting-log",
            name = entry.name.clone()
        ));
        let output = self
            .run(
                kurumi,
                entry,
                &args(["start"]),
                START_TIMEOUT,
                scope.as_ref(),
            )
            .await
            .inspect_err(|err| {
                self.record(
                    entry,
                    Some(Failure {
                        start: true,
                        message: error_chain(err),
                    }),
                )
            })?;
        if !output.success() {
            let message = output.failure();
            self.logs.warn(fl_log!(
                "containers-start-failed-log",
                name = entry.name.clone(),
                error = message.clone()
            ));
            self.record(
                entry,
                Some(Failure {
                    start: true,
                    message: message.clone(),
                }),
            );
            bail!(fl!(
                "containers-start-failed",
                name = entry.name.clone(),
                error = message
            ));
        }
        self.record(entry, None);
        let pid = self
            .running(spec)
            .map(|live| live.init_pid.to_string())
            .unwrap_or_else(|| "?".to_owned());
        Ok(fl!(
            "containers-started",
            name = entry.name.clone(),
            pid = pid
        ))
    }

    async fn stop(&self, kurumi: &Kurumi, entry: &Entry, spec: &ContainerSpec) -> Result<String> {
        if self.running(spec).is_none() {
            self.record(entry, None);
            return Ok(fl!("containers-not-running", name = entry.name.clone()));
        }
        self.logs.info(fl_log!(
            "containers-stopping-log",
            name = entry.name.clone()
        ));
        let timeout = Duration::from_secs(spec.stop_timeout.saturating_add(60));
        let output = self
            .run(kurumi, entry, &args(["stop"]), timeout, None)
            .await?;
        if !output.success() {
            let message = output.failure();
            self.record(
                entry,
                Some(Failure {
                    start: false,
                    message: message.clone(),
                }),
            );
            bail!(fl!(
                "containers-stop-failed",
                name = entry.name.clone(),
                error = message
            ));
        }
        self.record(entry, None);
        Ok(fl!("containers-stopped", name = entry.name.clone()))
    }

    /// Runs `command` in a running container and returns its output.
    pub async fn exec(
        &self,
        query: &str,
        command: Vec<String>,
        timeout: Option<u64>,
    ) -> Result<ExecResult> {
        let entry = self.resolve(query)?;
        let spec = self.spec_of(&entry).map_err(|err| {
            anyhow!(fl!(
                "containers-config-broken",
                name = entry.name.clone(),
                error = err
            ))
        })?;
        if command.first().is_none_or(|c| c.trim().is_empty()) {
            bail!(fl!("containers-exec-empty"));
        }
        let kurumi = self.kurumi(false).await?;
        if self.running(&spec).is_none() {
            bail!(fl!("containers-not-running", name = entry.name.clone()));
        }
        let seconds = timeout
            .unwrap_or(DEFAULT_EXEC_TIMEOUT)
            .clamp(1, MAX_EXEC_TIMEOUT);
        let mut full = args(["run", "--"]);
        full.extend(command.iter().map(OsString::from));
        self.logs.info(fl_log!(
            "containers-exec-log",
            name = entry.name.clone(),
            command = command.join(" ")
        ));
        self.write_pointer(&self.entries())?;
        let mut with_name = args(["--name", entry.id.as_str()]);
        with_name.extend(full);
        let output = kurumi
            .run(&with_name, Duration::from_secs(seconds), None)
            .await?;
        Ok(ExecResult {
            code: output.code.unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            truncated: output.truncated,
        })
    }

    pub async fn check(&self) -> Result<String> {
        let kurumi = self.kurumi(true).await?;
        let output = kurumi.run(&args(["check"]), CHECK_TIMEOUT, None).await?;
        let report = output.stdout_text();
        if output.success() {
            Ok(report)
        } else {
            bail!("{report}\n{}", output.failure())
        }
    }

    pub async fn scan(&self) -> Result<String> {
        let Some(entry) = self.entries().into_iter().next() else {
            bail!(fl!("containers-none"));
        };
        let kurumi = self.kurumi(false).await?;
        let output = self
            .run(&kurumi, &entry, &args(["scan"]), CHECK_TIMEOUT, None)
            .await?;
        if output.success() {
            Ok(output.stdout_text())
        } else {
            bail!(output.failure())
        }
    }

    // ----- root filesystems ---------------------------------------------

    /// Root filesystem images of the image server for this machine.
    pub async fn images(&self, refresh: bool) -> Result<ImageList> {
        let entries = self.image_index(refresh).await?;
        Ok(ImageList {
            server: self.image_server().to_owned(),
            arch: images::image_arch().to_owned(),
            images: entries.into_iter().map(|e| e.image).collect(),
        })
    }

    fn image_server(&self) -> &str {
        self.config.containers.image_server.trim_end_matches('/')
    }

    async fn image_index(&self, refresh: bool) -> Result<Vec<IndexEntry>> {
        let mut cache = self.images.lock().await;
        if !refresh
            && let Some((at, entries)) = cache.as_ref()
            && at.elapsed() < IMAGE_INDEX_TTL
        {
            return Ok(entries.clone());
        }
        let url = format!("{}/{}", self.image_server(), images::INDEX);
        let data = self.github()?.fetch(&url, 8 * 1024 * 1024).await?;
        let text = String::from_utf8_lossy(&data);
        let entries = images::parse_index(&text, images::image_arch());
        *cache = Some((Instant::now(), entries.clone()));
        Ok(entries)
    }

    /// Installs a root filesystem from an absolute path, an http(s) URL or
    /// an image (`distro/release`).
    pub async fn install(
        &self,
        query: &str,
        source: &str,
        size: Option<String>,
        sha256: Option<String>,
        force: bool,
    ) -> Result<String> {
        let entry = self.resolve(query)?;
        let spec = self.spec_of(&entry).map_err(|err| {
            anyhow!(fl!(
                "containers-config-broken",
                name = entry.name.clone(),
                error = err
            ))
        })?;
        let source = source.trim();
        let size = size.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        if spec.image && size.is_none() {
            bail!(fl!("containers-install-size", name = entry.name.clone()));
        }
        if !spec.image && size.is_some() {
            bail!(fl!("containers-install-size-directory"));
        }
        if spec.installed && !force {
            bail!(fl!(
                "containers-install-exists",
                name = entry.name.clone(),
                rootfs = spec.rootfs.clone()
            ));
        }
        let kurumi = self.kurumi(true).await?;
        if self.running(&spec).is_some() {
            bail!(fl!("containers-install-running", name = entry.name.clone()));
        }
        // Checked before downloading hundreds of MiB the runtime (which is
        // root) would then refuse.
        if Uid::effective().is_root() {
            trusted_install_target(Path::new(&spec.rootfs))?;
        }
        let busy = self.begin(&entry, "installing")?;
        let result = async {
            let (archive, cleanup, origin) = self
                .fetch_archive(&entry, &busy, source, sha256.as_deref())
                .await?;
            busy.set("installing");
            let archive_size = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
            self.logs.info(fl_log!(
                "containers-installing-log",
                name = entry.name.clone(),
                source = origin.clone(),
                size = fmt_bytes(archive_size)
            ));
            let mut command = args(["install".into(), archive.clone().into_os_string()]);
            if let Some(size) = &size {
                command.extend(args(["--size", size.as_str()]));
            }
            if force {
                command.push("--force".into());
            }
            let output = self
                .run(&kurumi, &entry, &command, INSTALL_TIMEOUT, None)
                .await;
            if cleanup {
                let _ = std::fs::remove_file(&archive);
            }
            let output = output?;
            if !output.success() {
                bail!(fl!(
                    "containers-install-failed",
                    name = entry.name.clone(),
                    error = output.failure()
                ));
            }
            Ok(fl!(
                "containers-installed",
                name = entry.name.clone(),
                source = origin,
                rootfs = spec.rootfs.clone()
            ))
        }
        .await;
        match &result {
            Ok(_) => self.record(&entry, None),
            Err(err) => self.record(
                &entry,
                Some(Failure {
                    start: false,
                    message: error_chain(err),
                }),
            ),
        }
        result
    }

    /// The archive to install: the local file itself, or a verified
    /// download (`true`: delete it afterwards) and how to name its origin.
    async fn fetch_archive(
        &self,
        entry: &Entry,
        busy: &Busy<'_>,
        source: &str,
        sha256: Option<&str>,
    ) -> Result<(PathBuf, bool, String)> {
        let sha256 = sha256.map(str::trim).filter(|s| !s.is_empty());
        if source.starts_with('/') {
            let path = PathBuf::from(source);
            if !path.is_file() {
                bail!(fl!("err-not-found", path = source));
            }
            if let Some(expected) = sha256 {
                let data_path = path.clone();
                let expected = expected.to_owned();
                let lang = crate::i18n::current();
                tokio::task::spawn_blocking(move || {
                    crate::i18n::in_language(lang, || verify_file(&data_path, &expected))
                })
                .await
                .context(fl!("err-task-panicked"))??;
            }
            return Ok((path, false, source.to_owned()));
        }
        let (url, expected, origin, name) =
            if source.starts_with("http://") || source.starts_with("https://") {
                let name = source
                    .split(['?', '#'])
                    .next()
                    .and_then(|p| p.rsplit('/').next())
                    .filter(|n| !n.is_empty())
                    .unwrap_or("rootfs")
                    .to_owned();
                (
                    source.to_owned(),
                    sha256.map(str::to_owned),
                    crate::util::shorten_url(source),
                    name,
                )
            } else if let Some((distro, release)) = images::image_spec(source) {
                let index = self.image_index(false).await?;
                let found = images::find(&index, &distro, &release)?.clone();
                let base = format!("{}{}", self.image_server(), found.path);
                let sums_url = format!("{base}SHA256SUMS");
                let sums = self.github()?.fetch(&sums_url, 1024 * 1024).await?;
                let published = parse_sha256sums(&String::from_utf8_lossy(&sums))
                    .remove(images::ROOTFS)
                    .ok_or_else(|| {
                        anyhow!(fl!("containers-image-no-checksum", url = sums_url.clone()))
                    })?;
                if let Some(pinned) = sha256
                    && !pinned.eq_ignore_ascii_case(&published)
                {
                    bail!(fl!(
                        "err-checksum-mismatch",
                        name = images::ROOTFS,
                        expected = pinned,
                        actual = published.clone()
                    ));
                }
                (
                    format!("{base}{}", images::ROOTFS),
                    Some(published),
                    format!("{} ({})", found.image.spec(), found.image.build),
                    images::ROOTFS.to_owned(),
                )
            } else {
                bail!(fl!("containers-install-source", source = source));
            };
        let dir = self.root.join(".downloads");
        std::fs::create_dir_all(&dir)
            .with_context(|| fl!("err-create", path = dir.display().to_string()))?;
        let path = dir.join(format!("{}-{name}", random_token(8)));
        self.logs.info(fl_log!(
            "containers-downloading-log",
            name = entry.name.clone(),
            source = origin.clone()
        ));
        busy.set("downloading");
        let mut logged = 0u64;
        let result = self
            .github()?
            .download_to_file(&url, &path, MAX_ARCHIVE, |received, total| match total {
                Some(total) if total > 0 => {
                    let percent = received.saturating_mul(100) / total;
                    busy.set(format!("downloading:{percent}"));
                    if percent >= logged + 20 {
                        logged = percent - percent % 20;
                        self.logs.info(fl_log!(
                            "containers-download-progress",
                            name = entry.name.clone(),
                            percent = percent,
                            received = fmt_bytes(received),
                            total = fmt_bytes(total)
                        ));
                    }
                }
                _ => busy.set("downloading"),
            })
            .await;
        let actual = match result {
            Ok(actual) => actual,
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                return Err(err);
            }
        };
        match &expected {
            Some(expected) if !expected.eq_ignore_ascii_case(&actual) => {
                let _ = std::fs::remove_file(&path);
                bail!(fl!(
                    "err-checksum-mismatch",
                    name = name,
                    expected = expected.clone(),
                    actual = actual
                ));
            }
            Some(_) => {}
            None => self.logs.warn(fl_log!(
                "containers-download-unverified",
                name = entry.name.clone(),
                source = origin.clone()
            )),
        }
        Ok((path, true, origin))
    }

    // ----- configurations -----------------------------------------------

    /// Asks the runtime whether it accepts `content` as the configuration
    /// at `file`: it is loaded from a sibling copy with the runtime's own
    /// strict schema. Only possible once the root filesystem exists (the
    /// runtime resolves it); `Ok(false)` when skipped.
    async fn verify(&self, file: &Path, content: &str, spec: &ContainerSpec) -> Result<bool> {
        if !spec.installed || !self.privileged {
            return Ok(false);
        }
        let Some(resolved) = self.runtime.resolve() else {
            return Ok(false);
        };
        let dir = file.parent().unwrap_or(Path::new("/"));
        let token = random_token(8);
        let copy = dir.join(format!(".check-{token}.toml"));
        let home = self.root.join(format!(".check-{token}"));
        let pointer_dir = home.join(".kurumi-containerd");
        let cleanup = || {
            let _ = std::fs::remove_file(&copy);
            let _ = std::fs::remove_file(dir.join(format!("..check-{token}.toml.lock")));
            let _ = std::fs::remove_dir_all(&home);
        };
        let prepared = (|| -> Result<()> {
            std::fs::create_dir_all(&pointer_dir)?;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
            // The directory of a linked configuration may be writable by
            // others: never follow or reuse whatever is at the name.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&copy)?;
            std::io::Write::write_all(&mut file, content.as_bytes())?;
            let pointer = [Pointer {
                name: "check".to_owned(),
                file: copy.clone(),
            }];
            std::fs::write(
                pointer_dir.join("config.json"),
                serde_json::to_vec_pretty(&pointer)?,
            )?;
            Ok(())
        })();
        if let Err(err) = prepared {
            cleanup();
            return Err(err);
        }
        let kurumi = Kurumi {
            binary: resolved.binary,
            home: home.clone(),
        };
        let output = kurumi
            .run(&args(["--name", "check", "info"]), CHECK_TIMEOUT, None)
            .await;
        cleanup();
        let output = output?;
        if output.success() {
            return Ok(true);
        }
        let message = output
            .failure()
            .replace(&copy.display().to_string(), &file.display().to_string());
        bail!(fl!("containers-config-rejected", error = message))
    }

    /// Registers a container: a configuration kept where it is (`file`), a
    /// copy of `content`, or a new one from the template.
    pub async fn add(
        &self,
        name: Option<String>,
        content: Option<String>,
        file: Option<String>,
        network: Option<String>,
    ) -> Result<(Container, String)> {
        self.require_root()?;
        self.ensure_root_dir()?;
        let id = new_id(&self.entries());
        let now = now_unix();
        let mut notes = Vec::new();
        let (entry_file, display, spec) = match (file, content) {
            (Some(_), Some(_)) => bail!(fl!("containers-add-file-and-content")),
            (Some(file), None) => {
                let path = PathBuf::from(file.trim());
                if !path.is_absolute() {
                    bail!(fl!("containers-add-relative"));
                }
                let path = path
                    .canonicalize()
                    .with_context(|| fl!("err-read", path = path.display().to_string()))?;
                if self.entries().iter().any(|e| self.file_of(e) == path) {
                    bail!(fl!(
                        "containers-already-registered",
                        path = path.display().to_string()
                    ));
                }
                let content = std::fs::read_to_string(&path)
                    .with_context(|| fl!("err-read", path = path.display().to_string()))?;
                let spec = spec::parse(&content, &path)?;
                self.check_unique_name(&spec, None)?;
                match self.verify(&path, &content, &spec).await {
                    Ok(true) => notes.push(fl!("containers-check-passed")),
                    Ok(false) => notes.push(fl!("containers-check-later")),
                    Err(err) => {
                        notes.push(fl!("containers-check-warning", error = error_chain(&err)))
                    }
                }
                if let Some(writable) = writable_by_others(&path) {
                    notes.push(fl!(
                        "containers-linked-writable",
                        path = writable.display().to_string()
                    ));
                }
                let display = name.clone().unwrap_or_else(|| spec.name.clone());
                (Some(path), display, spec)
            }
            (None, Some(content)) => {
                let path = self.root.join(&id).join(CONFIG);
                let content = spec::with_uuid(&content);
                let spec = spec::parse(&content, &path)?;
                self.check_unique_name(&spec, None)?;
                let display = name.clone().unwrap_or_else(|| spec.name.clone());
                self.write_config(&path, &content)?;
                match self.verify(&path, &content, &spec).await {
                    Ok(true) => notes.push(fl!("containers-check-passed")),
                    Ok(false) => notes.push(fl!("containers-check-later")),
                    Err(err) => {
                        notes.push(fl!("containers-check-warning", error = error_chain(&err)))
                    }
                }
                (None, display, spec)
            }
            (None, None) => {
                let display = name
                    .clone()
                    .map(|n| n.trim().to_owned())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| fl!("containers-default-name"));
                let taken = self.other_names(None);
                let base = spec::container_name(&display, &format!("container-{id}"));
                let container_name = (1..)
                    .map(|n| {
                        if n == 1 {
                            base.clone()
                        } else {
                            format!("{base}-{n}")
                        }
                    })
                    .find(|candidate| !taken.contains(candidate))
                    .unwrap_or(base);
                let network = match network.as_deref().map(str::trim).unwrap_or("host") {
                    "" | "host" => TemplateNetwork::Host,
                    "none" => TemplateNetwork::None,
                    "nat" => TemplateNetwork::Nat(self.free_address()),
                    other => bail!(fl!(
                        "containers-template-network",
                        network = other.to_owned()
                    )),
                };
                let path = self.root.join(&id).join(CONFIG);
                let content = spec::with_uuid(&spec::template(&display, &container_name, network));
                let spec = spec::parse(&content, &path)?;
                self.write_config(&path, &content)?;
                notes.push(fl!("containers-created-hint", rootfs = spec.rootfs.clone()));
                (None, display, spec)
            }
        };
        let display = unique_name(&self.entries(), display.trim(), None);
        let entry = Entry {
            id: id.clone(),
            name: display,
            file: entry_file,
            autostart: false,
            created_at: now,
            updated_at: now,
        };
        {
            let mut index = self.index.lock().expect("container index");
            index.push(entry.clone());
            self.save_index(&index)?;
        }
        self.logs.info(fl_log!(
            "containers-added-log",
            name = entry.name.clone(),
            id = id.clone(),
            file = self.file_of(&entry).display().to_string()
        ));
        let mut message = vec![fl!(
            "containers-added",
            name = entry.name.clone(),
            id = id.clone(),
            container = spec.name.clone()
        )];
        message.extend(notes);
        Ok((self.container(&id)?, message.join("\n")))
    }

    /// The first free address on the default NAT bridge.
    fn free_address(&self) -> Ipv4Addr {
        let taken: Vec<Ipv4Addr> = self
            .entries()
            .iter()
            .filter_map(|e| self.spec_of(e).ok())
            .filter(|s| s.network == "nat" && s.bridge.as_deref() == Some(spec::DEFAULT_BRIDGE))
            .filter_map(|s| s.address?.split('/').next()?.parse().ok())
            .collect();
        spec::free_nat_address(&taken)
    }

    fn write_config(&self, path: &Path, content: &str) -> Result<()> {
        let dir = path.parent().unwrap_or(&self.root);
        std::fs::create_dir_all(dir)
            .with_context(|| fl!("err-create", path = dir.display().to_string()))?;
        if dir.starts_with(&self.root) {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mode = std::fs::metadata(path)
            .map(|m| m.mode() & 0o777)
            .unwrap_or(0o600);
        write_atomic(path, content.as_bytes(), mode)?;
        self.specs.lock().expect("spec cache").remove(path);
        Ok(())
    }

    /// Replaces a container's configuration.
    pub async fn save(
        &self,
        query: &str,
        content: &str,
        force: bool,
    ) -> Result<(Container, String)> {
        self.require_root()?;
        let entry = self.resolve(query)?;
        let path = self.file_of(&entry);
        let content = spec::with_uuid(content);
        let spec = spec::parse(&content, &path)?;
        self.check_unique_name(&spec, Some(&entry.id))?;
        let previous = self.spec_of(&entry).ok();
        let running = previous.as_ref().and_then(|p| self.running(p));
        if running.is_some() && previous.as_ref().is_some_and(|p| p.name != spec.name) {
            bail!(fl!("containers-rename-running", name = entry.name.clone()));
        }
        let note = match self.verify(&path, &content, &spec).await {
            Ok(true) => fl!("containers-check-passed"),
            Ok(false) => fl!("containers-check-later"),
            Err(err) if force => fl!("containers-check-warning", error = error_chain(&err)),
            Err(err) => return Err(err.context(fl!("containers-save-rejected"))),
        };
        self.write_config(&path, &content)?;
        self.modify(&entry.id, |_| {})?;
        self.logs.info(fl_log!(
            "containers-saved-log",
            name = entry.name.clone(),
            file = path.display().to_string()
        ));
        let mut lines = vec![fl!("containers-saved", name = entry.name.clone()), note];
        if running.is_some() {
            lines.push(fl!("containers-restart-to-apply"));
        }
        Ok((self.container(&entry.id)?, lines.join("\n")))
    }

    pub fn set(
        &self,
        query: &str,
        name: Option<String>,
        autostart: Option<bool>,
    ) -> Result<String> {
        let entry = self.resolve(query)?;
        let name = name.map(|n| n.trim().to_owned());
        if name.as_deref().is_some_and(str::is_empty) {
            bail!(fl!("containers-empty-name"));
        }
        if let Some(name) = &name
            && unique_name(&self.entries(), name, Some(&entry.id)) != *name
        {
            bail!(fl!("containers-display-name-taken", name = name.clone()));
        }
        let updated = self.modify(&entry.id, |e| {
            if let Some(name) = name {
                e.name = name;
            }
            if let Some(autostart) = autostart {
                e.autostart = autostart;
            }
        })?;
        Ok(match autostart {
            Some(true) => fl!("containers-autostart-on", name = updated.name),
            Some(false) => fl!("containers-autostart-off", name = updated.name),
            None => fl!("containers-renamed", name = updated.name),
        })
    }

    /// Unregisters a stopped container; `purge` deletes its directory in the store.
    pub async fn remove(&self, query: &str, purge: bool) -> Result<String> {
        self.require_root()?;
        let entry = self.resolve(query)?;
        if let Ok(spec) = self.spec_of(&entry)
            && self.running(&spec).is_some()
        {
            bail!(fl!("containers-remove-running", name = entry.name.clone()));
        }
        let _busy = self.begin(&entry, "removing")?;
        let dir = self.root.join(&entry.id);
        if purge && entry.file.is_none() {
            let mounted = live::mounts_below(&dir);
            if let Some(point) = mounted.first() {
                bail!(fl!(
                    "containers-remove-mounted",
                    path = point.display().to_string()
                ));
            }
        }
        {
            let mut index = self.index.lock().expect("container index");
            index.retain(|e| e.id != entry.id);
            self.save_index(&index)?;
        }
        self.record(&entry, None);
        let message = match (purge, entry.file.is_none()) {
            (true, true) => {
                let target = dir.clone();
                tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&target))
                    .await
                    .context(fl!("err-task-panicked"))?
                    .with_context(|| fl!("err-delete", path = dir.display().to_string()))?;
                fl!("containers-removed-purged", name = entry.name.clone())
            }
            (false, true) => fl!(
                "containers-removed-kept",
                name = entry.name.clone(),
                path = dir.display().to_string()
            ),
            (_, false) => fl!(
                "containers-removed-linked",
                name = entry.name.clone(),
                path = self.file_of(&entry).display().to_string()
            ),
        };
        self.logs.info(fl_log!(
            "containers-removed-log",
            name = entry.name.clone(),
            id = entry.id.clone()
        ));
        Ok(message)
    }

    /// Registers the entries of a kurumi-containerd registry.
    pub async fn adopt(&self, path: Option<String>) -> Result<String> {
        self.require_root()?;
        let path = match path.map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()) {
            Some(path) => PathBuf::from(path),
            None => root_home().join(".kurumi-containerd/config.json"),
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| fl!("err-read", path = path.display().to_string()))?;
        let pointers: Vec<Pointer> = serde_json::from_str(&text)
            .with_context(|| fl!("err-parse", path = path.display().to_string()))?;
        let base = path.parent().unwrap_or(Path::new("/"));
        let mut lines = Vec::new();
        let mut adopted = 0;
        for pointer in pointers {
            let file = if pointer.file.is_relative() {
                base.join(&pointer.file)
            } else {
                pointer.file.clone()
            };
            let result = self
                .add(
                    Some(pointer.name.clone()),
                    None,
                    Some(file.display().to_string()),
                    None,
                )
                .await;
            match result {
                Ok((container, _)) => {
                    adopted += 1;
                    lines.push(format!(
                        "  {}",
                        fl!(
                            "containers-adopted-line",
                            name = container.name,
                            file = file.display().to_string()
                        )
                    ));
                }
                Err(err) => lines.push(format!(
                    "  {}",
                    fl!(
                        "containers-adopt-skipped",
                        name = pointer.name,
                        error = error_chain(&err)
                    )
                )),
            }
        }
        lines.insert(0, fl!("containers-adopted", count = adopted));
        Ok(lines.join("\n"))
    }

    // ----- daemon lifecycle ---------------------------------------------

    /// Starts containers marked for autostart, once per boot.
    pub fn spawn_autostart(self: &Arc<Self>) {
        if !self.config.containers.autostart || !self.privileged {
            return;
        }
        let wanted: Vec<Entry> = self.entries().into_iter().filter(|e| e.autostart).collect();
        if wanted.is_empty() {
            return;
        }
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let marker = self.root.join(AUTOSTART_MARKER);
        if !boot.is_empty()
            && std::fs::read_to_string(&marker).is_ok_and(|done| done.trim() == boot)
        {
            return;
        }
        let manager = self.clone();
        tokio::spawn(async move {
            for entry in wanted {
                match manager.control(&entry.id, ContainerAction::Start).await {
                    Ok(message) => manager.logs.info(message),
                    Err(err) => manager.logs.warn(fl_log!(
                        "containers-autostart-failed",
                        name = entry.name.clone(),
                        error = error_chain(&err)
                    )),
                }
            }
            if !boot.is_empty() {
                let _ = std::fs::write(&marker, boot);
            }
        });
    }

    /// Stops running containers when the daemon stops for good: always
    /// with `stop_on_shutdown`, else only when the system shuts down.
    pub async fn shutdown(&self) {
        if !self.privileged {
            return;
        }
        let running: Vec<(Entry, ContainerSpec)> = self
            .entries()
            .into_iter()
            .filter_map(|e| {
                let spec = self.spec_of(&e).ok()?;
                self.running(&spec).map(|_| (e, spec))
            })
            .collect();
        if running.is_empty() {
            return;
        }
        if !self.config.containers.stop_on_shutdown && !kurumi::system_stopping().await {
            self.logs
                .info(fl_log!("containers-left-running", count = running.len()));
            return;
        }
        let Ok(kurumi) = self.kurumi(false).await else {
            return;
        };
        let stops = running.iter().map(|(entry, spec)| async {
            match self.stop(&kurumi, entry, spec).await {
                Ok(message) => self.logs.info(message),
                Err(err) => self.logs.warn(error_chain(&err)),
            }
        });
        let _ = tokio::time::timeout(SHUTDOWN_LIMIT, futures::future::join_all(stops)).await;
    }
}

/// kurumi-containerd's rule for where a root filesystem may be installed:
/// its parent is a real directory of root's that only root can write to,
/// and no directory above it is writable by others (unless sticky, like
/// /tmp). Otherwise another user could swap the tree the runtime works in.
fn trusted_install_target(target: &Path) -> Result<()> {
    let Some(parent) = target.parent() else {
        bail!(fl!("err-not-found", path = target.display().to_string()));
    };
    let meta = std::fs::symlink_metadata(parent)
        .with_context(|| fl!("err-read", path = parent.display().to_string()))?;
    if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        bail!(fl!(
            "containers-install-untrusted-parent",
            path = parent.display().to_string()
        ));
    }
    for ancestor in parent.ancestors().skip(1) {
        let meta = std::fs::symlink_metadata(ancestor)
            .with_context(|| fl!("err-read", path = ancestor.display().to_string()))?;
        if meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0 {
            bail!(fl!(
                "containers-install-untrusted-ancestor",
                path = ancestor.display().to_string()
            ));
        }
    }
    Ok(())
}

/// The file, or a directory above it, that users other than root can
/// change. Whoever can edit a container's configuration decides what runs
/// as root in it.
fn writable_by_others(file: &Path) -> Option<PathBuf> {
    let unsafe_entry = |path: &Path, file: bool| {
        std::fs::symlink_metadata(path).is_ok_and(|meta| {
            let sticky = !file && meta.mode() & 0o1000 != 0;
            meta.uid() != 0 || (meta.mode() & 0o022 != 0 && !sticky)
        })
    };
    if unsafe_entry(file, true) {
        return Some(file.to_path_buf());
    }
    file.ancestors()
        .skip(1)
        .find(|dir| unsafe_entry(dir, false))
        .map(Path::to_path_buf)
}

/// The home directory of root, where kurumi-containerd keeps its registry
/// when run by hand.
fn root_home() -> PathBuf {
    nix::unistd::User::from_uid(Uid::from_raw(0))
        .ok()
        .flatten()
        .map_or_else(|| PathBuf::from("/root"), |user| user.dir)
}

fn verify_file(path: &Path, expected: &str) -> Result<()> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path)
        .with_context(|| fl!("err-read", path = path.display().to_string()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        bail!(fl!(
            "err-checksum-mismatch",
            name = path.display().to_string(),
            expected = expected.to_owned(),
            actual = actual
        ));
    }
    Ok(())
}

fn unique<'a>(mut matches: impl Iterator<Item = &'a Entry>) -> Option<&'a Entry> {
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn new_id(index: &[Entry]) -> String {
    loop {
        let id = random_token(8).to_ascii_lowercase();
        if !index.iter().any(|e| e.id == id) {
            return id;
        }
    }
}

/// `wanted`, or `wanted 2`, `wanted 3`, ... when another container has it.
fn unique_name(index: &[Entry], wanted: &str, except: Option<&str>) -> String {
    let taken = |name: &str| {
        index
            .iter()
            .any(|e| Some(e.id.as_str()) != except && e.name.eq_ignore_ascii_case(name))
    };
    if !taken(wanted) {
        return wanted.to_owned();
    }
    (2..)
        .map(|n| format!("{wanted} {n}"))
        .find(|name| !taken(name))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for kurumi-containerd: logs its calls, installs a root
    /// filesystem with an init, rejects configurations with
    /// `unknown_field`, and echoes commands it is asked to run.
    const FAKE_RUNTIME: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "kurumi-containerd 0.2.3"; exit 0; fi
if [ "$1" = "check" ]; then echo "Host: linux"; echo "Pidfd: available (process handles)"; exit 0; fi
if [ "$1" != "--name" ]; then echo "2026-10-07T00:00:00.000000Z ERROR command failed error=no entry selected" >&2; exit 1; fi
id="$2"; shift 2
echo "$id $*" >> "@LOG@"
file=$(awk -v id="$id" 'found && /"file"/ { sub(/.*"file": "/, ""); sub(/".*/, ""); print; exit } index($0, "\"name\": \"" id "\"") { found = 1 }' "$HOME/.kurumi-containerd/config.json")
case "$1" in
  install)
    dir=$(dirname "$file"); mkdir -p "$dir/rootfs/sbin"; touch "$dir/rootfs/sbin/init"
    echo "Rootfs installed: $2 -> $dir/rootfs" ;;
  info)
    if grep -q unknown_field "$file"; then
      echo "2026-10-07T00:00:00.000000Z ERROR command failed error=failed to parse TOML config $file: unknown field \`unknown_field\`" >&2
      exit 1
    fi
    echo "Name: x" ;;
  start) echo "2026-10-07T00:00:00.000000Z  INFO starting" >&2; echo "Container x started (PID 4242)" ;;
  stop) echo "Container x stopped" ;;
  run) shift; [ "$1" = "--" ] && shift; echo "ran: $*"; echo "warning from the command" >&2; exit 3 ;;
  scan) echo "No containers required recovery" ;;
esac
"#;

    fn setup(name: &str) -> (Arc<ContainerManager>, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("sbb-containers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("kurumi-containerd");
        let log = dir.join("calls.log");
        std::fs::write(
            &script,
            FAKE_RUNTIME.replace("@LOG@", &log.display().to_string()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (open(&dir), dir)
    }

    fn open(dir: &Path) -> Arc<ContainerManager> {
        let mut config = DaemonConfig::default();
        config.components.data_dir = dir.join("data");
        config.containers.runtime = Some(dir.join("kurumi-containerd"));
        config.containers.systemd_scope = false;
        config.containers.autostart = false;
        let logs = Arc::new(LogHub::new(200, false));
        let procfs = Procfs::at(dir.join("proc"), dir.join("run"));
        ContainerManager::build(config, logs, procfs, true)
    }

    fn calls(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default()
    }

    #[tokio::test]
    async fn containers_from_template_to_removal() {
        let (manager, dir) = setup("flow");

        // New containers from the template: NAT addresses and names stay unique.
        let (dev, message) = manager
            .add(Some("Dev Box".into()), None, None, Some("nat".into()))
            .await
            .unwrap();
        let spec = dev.spec.clone().unwrap();
        assert_eq!(
            (dev.name.as_str(), spec.name.as_str()),
            ("Dev Box", "dev-box")
        );
        assert_eq!(spec.address.as_deref(), Some("172.28.0.2/16"));
        assert!(spec.uuid.is_some() && !spec.installed && dev.managed);
        assert!(message.contains(&fl!(
            "containers-created-hint",
            rootfs = spec.rootfs.clone()
        )));
        let content = std::fs::read_to_string(&dev.file).unwrap();
        assert!(
            content.contains("# Root filesystem"),
            "comments survive the uuid"
        );
        let mode = std::fs::metadata(&dev.file).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let pointer: Vec<Pointer> = serde_json::from_slice(
            &std::fs::read(dir.join("data/containers/.kurumi-containerd/config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(pointer[0].name, dev.id);
        assert_eq!(pointer[0].file, PathBuf::from(&dev.file));

        let (second, _) = manager
            .add(Some("dev box".into()), None, None, Some("nat".into()))
            .await
            .unwrap();
        let second_spec = second.spec.clone().unwrap();
        assert_eq!(second.name, "dev box 2");
        assert_eq!(second_spec.name, "dev-box-2");
        assert_eq!(second_spec.address.as_deref(), Some("172.28.0.3/16"));
        let clash = "[runtime]\n[container]\nname = \"dev-box\"\nrootfs = \"./rootfs\"\n";
        let error = manager
            .add(None, Some(clash.into()), None, None)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            fl!("containers-name-taken", name = "dev-box")
        );
        assert!(
            manager.resolve("dev-box-2").is_ok(),
            "container.name resolves too"
        );
        assert!(manager.resolve(&dev.id[..3]).is_ok());

        // A root filesystem is needed before starting.
        let error = manager
            .control(&dev.id, ContainerAction::Start)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(&spec.rootfs));
        let archive = dir.join("rootfs.tar");
        std::fs::write(&archive, b"tar").unwrap();
        let archive = archive.display().to_string();
        let error = manager
            .install(&dev.id, "rootfs.tar", None, None, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("rootfs.tar"));
        let wrong_sum = manager
            .install(&dev.id, &archive, None, Some("00".into()), false)
            .await;
        assert!(wrong_sum.is_err());
        manager
            .install("Dev Box", &archive, None, None, false)
            .await
            .unwrap();
        assert!(manager.container(&dev.id).unwrap().spec.unwrap().installed);
        assert!(
            manager
                .install(&dev.id, &archive, None, None, false)
                .await
                .is_err()
        );
        assert!(
            manager
                .install(&dev.id, &archive, Some("8G".into()), None, true)
                .await
                .is_err(),
            "sizes are for images only"
        );

        // Saving asks the runtime, which rejects unknown settings unless forced.
        let broken = content.replace("volatile = false", "volatile = false\nunknown_field = 1");
        let error = manager.save(&dev.id, &broken, false).await.unwrap_err();
        let chain = error_chain(&error);
        assert!(
            chain.contains("unknown field") && chain.contains(&dev.file),
            "{chain}"
        );
        assert!(
            !chain.contains(".check-"),
            "the temporary copy is not shown: {chain}"
        );
        let (_, message) = manager.save(&dev.id, &broken, true).await.unwrap();
        assert!(message.contains("unknown field"));
        assert!(
            manager
                .save(&dev.id, &content, false)
                .await
                .unwrap()
                .1
                .contains(&fl!("containers-check-passed"))
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("data/containers"))
            .unwrap()
            .chain(std::fs::read_dir(Path::new(&dev.file).parent().unwrap()).unwrap())
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("check-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // Start, then pretend the runtime's monitor is up.
        let started = manager
            .control(&dev.id, ContainerAction::Start)
            .await
            .unwrap();
        assert_eq!(
            started,
            fl!("containers-started", name = "Dev Box", pid = "?")
        );
        live::fake_running(&dir.join("proc"), &dir.join("run"), "dev-box", 100);
        let overview = manager.overview().await;
        let shown = overview.containers.iter().find(|c| c.id == dev.id).unwrap();
        assert_eq!(shown.state, CoreState::Running);
        assert_eq!(shown.live.as_ref().unwrap().processes, 2);
        assert_eq!(overview.runtime.version.as_deref(), Some("0.2.3"));
        assert_eq!(
            manager.summary(),
            ContainerSummary {
                total: 2,
                running: 1,
                runtime: true
            }
        );
        assert!(
            manager
                .control(&dev.id, ContainerAction::Start)
                .await
                .unwrap()
                .contains("100")
        );

        let result = manager
            .exec(&dev.id, vec!["uname".into(), "-a".into()], None)
            .await
            .unwrap();
        assert_eq!(
            (result.code, result.stdout.as_str()),
            (3, "ran: uname -a\n")
        );
        assert!(
            manager
                .exec(&second.id, vec!["true".into()], None)
                .await
                .is_err()
        );
        assert!(manager.remove(&dev.id, true).await.is_err(), "running");
        assert!(
            manager
                .install(&dev.id, &archive, None, None, true)
                .await
                .is_err(),
            "running"
        );
        let renamed = content.replace("name = \"dev-box\"", "name = \"other\"");
        assert!(
            manager.save(&dev.id, &renamed, true).await.is_err(),
            "identity of a running container"
        );

        // Names and autostart.
        manager.set(&dev.id, None, Some(true)).unwrap();
        assert!(manager.entries()[0].autostart);
        assert!(
            manager
                .set(&second.id, Some("DEV BOX".into()), None)
                .is_err()
        );
        manager.set(&second.id, Some("Spare".into()), None).unwrap();

        // Registering configurations kept elsewhere.
        let external = dir.join("ext");
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(
            external.join("web.toml"),
            "[runtime]\n[container]\nname = \"web\"\nrootfs = \"./rootfs\"\n",
        )
        .unwrap();
        std::fs::write(
            external.join("config.json"),
            r#"[{"name": "web", "file": "web.toml"}, {"name": "gone", "file": "missing.toml"}]"#,
        )
        .unwrap();
        let adopted = manager
            .adopt(Some(external.join("config.json").display().to_string()))
            .await
            .unwrap();
        assert!(
            adopted.starts_with(&fl!("containers-adopted", count = 1)),
            "{adopted}"
        );
        assert!(adopted.contains("gone"));
        let again = manager
            .adopt(Some(external.join("config.json").display().to_string()))
            .await
            .unwrap();
        assert!(again.starts_with(&fl!("containers-adopted", count = 0)));
        let web = manager.resolve("web").unwrap();
        assert_eq!(
            web.file.as_deref(),
            Some(external.join("web.toml").as_path())
        );

        // Every runtime call named its container.
        let log = calls(&dir);
        assert!(
            log.contains(&format!("{} install {archive}", dev.id)),
            "{log}"
        );
        assert!(log.contains(&format!("{} start", dev.id)));
        assert!(log.contains(&format!("{} run -- uname -a", dev.id)));
        assert!(log.contains("check info"));

        // Removal: linked files stay, purged directories go; the registry persists.
        manager.remove("web", true).await.unwrap();
        assert!(external.join("web.toml").exists());
        let spare_dir = Path::new(&second.file).parent().unwrap().to_path_buf();
        manager.remove("Spare", false).await.unwrap();
        assert!(spare_dir.exists());
        let reopened = open(&dir);
        let names: Vec<String> = reopened.entries().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Dev Box"]);
        std::fs::remove_file(dir.join("run/state/dev-box.json")).unwrap();
        let removed = reopened.remove("Dev Box", true).await.unwrap();
        assert_eq!(removed, fl!("containers-removed-purged", name = "Dev Box"));
        assert!(!Path::new(&dev.file).exists());
        assert!(reopened.entries().is_empty());
        assert!(
            !dir.join("data/containers/.kurumi-containerd/config.json")
                .exists()
        );
        assert_eq!(reopened.check().await.unwrap().lines().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn configurations_others_can_change_are_named() {
        assert_eq!(writable_by_others(Path::new("/etc/hostname")), None);
        if !Uid::effective().is_root() {
            let mine = std::env::temp_dir().join(format!("sbb-writable-{}", std::process::id()));
            std::fs::create_dir_all(&mine).unwrap();
            let file = mine.join("c.toml");
            std::fs::write(&file, "").unwrap();
            assert_eq!(writable_by_others(&file), Some(file.clone()));
            std::fs::remove_dir_all(mine).unwrap();
        }
    }

    #[test]
    fn root_filesystems_go_where_only_root_writes() {
        assert!(trusted_install_target(Path::new("/usr/rootfs")).is_ok());
        // Sticky and world-writable: fine above, not as the parent.
        let tmp = trusted_install_target(Path::new("/tmp/rootfs")).unwrap_err();
        assert_eq!(
            tmp.to_string(),
            fl!("containers-install-untrusted-parent", path = "/tmp")
        );
        if !Uid::effective().is_root() {
            let mine = std::env::temp_dir().join(format!("sbb-trust-{}", std::process::id()));
            std::fs::create_dir_all(&mine).unwrap();
            assert!(trusted_install_target(&mine.join("rootfs")).is_err());
            std::fs::remove_dir_all(mine).unwrap();
        }
    }

    #[tokio::test]
    async fn unprivileged_daemons_explain_themselves() {
        let dir = std::env::temp_dir().join(format!("sbb-containers-user-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = DaemonConfig::default();
        config.components.data_dir = dir.join("data");
        config.containers.systemd_scope = false;
        let manager = ContainerManager::build(
            config,
            Arc::new(LogHub::new(10, false)),
            Procfs::at(dir.join("proc"), dir.join("run")),
            false,
        );
        let overview = manager.overview().await;
        assert_eq!(
            overview.runtime.problem,
            Some(fl!("containers-need-root-daemon"))
        );
        let error = manager.add(None, None, None, None).await.unwrap_err();
        assert_eq!(error.to_string(), fl!("containers-need-root-daemon"));
        assert_eq!(manager.summary().total, 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
