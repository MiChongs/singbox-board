//! Wire protocol between the root daemon and its clients (TUI / CLI).
//!
//! Every connection carries exactly one request: the client writes one JSON
//! line, the daemon answers with one JSON line. `logs` with `follow = true` is
//! the only streaming request; the daemon keeps writing `log` lines until the
//! client disconnects.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::i18n::fl;

/// Upper bound for a single request line; profile contents travel inline.
pub const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

/// A request line: the command plus the language the client wants replies
/// in. Daemons that predate `lang` ignore it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(flatten)]
    pub request: Request,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Start,
    Stop,
    Restart,
    /// Validate the configuration and send SIGHUP to sing-box.
    Reload,
    /// Run `sing-box check` against the configured files.
    Check,
    Logs {
        #[serde(default)]
        tail: usize,
        #[serde(default)]
        follow: bool,
    },
    /// Query GitHub for the newest release without installing it.
    CheckUpdate,
    /// Download, verify and install a release (latest when `tag` is empty).
    Update {
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        force: bool,
    },
    /// First-run answer: which optional components to install and enable.
    Setup {
        sub_store: bool,
        http_meta: bool,
    },
    /// Manage an optional component.
    Component {
        component: Component,
        action: ComponentAction,
    },
    /// Release sources the core can be installed from.
    CoreSources,
    /// One page (1-based) of a source's releases, newest first.
    CoreReleases {
        source: String,
        #[serde(default)]
        page: u32,
        #[serde(default)]
        refresh: bool,
    },
    /// Cores kept in the local version store.
    CoreInstalled,
    /// Download a release into the store and optionally switch to it.
    CoreInstall {
        source: String,
        tag: String,
        #[serde(default)]
        variant: String,
        #[serde(default)]
        activate: bool,
        /// Switch even if the new core rejects the configuration.
        #[serde(default)]
        force: bool,
    },
    /// Switch to a stored core.
    CoreActivate {
        id: String,
        #[serde(default)]
        force: bool,
    },
    CoreRemove {
        id: String,
    },
    /// Root only: add a GitHub repository publishing sing-box builds.
    CoreSourceAdd {
        repo: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Root only.
    CoreSourceRemove {
        id: String,
    },
    /// Root only: store a custom core from a local path or an http(s) URL
    /// (binary, .tar.gz, .zip or .gz).
    CoreImport {
        location: String,
        #[serde(default)]
        sha256: Option<String>,
        #[serde(default)]
        activate: bool,
    },
    /// Configuration profiles in the store.
    ProfileList,
    /// A profile and its content.
    ProfileGet {
        id: String,
    },
    /// Stores a new profile: `content` as given, downloaded from `url` (a
    /// remote profile), or the built-in template when both are absent.
    ProfileAdd {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        url: Option<String>,
        /// Minutes between automatic downloads of a remote profile.
        #[serde(default)]
        interval: Option<u64>,
        #[serde(default)]
        activate: bool,
    },
    /// Replaces a profile's content. The active profile has to pass
    /// `sing-box check` unless `force` is set, and sing-box is reloaded.
    ProfileSave {
        id: String,
        content: String,
        #[serde(default)]
        force: bool,
    },
    /// Renames a profile or changes where and how often it is downloaded;
    /// an empty `url` turns a remote profile into a local one.
    ProfileSet {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        interval: Option<u64>,
    },
    /// Downloads a remote profile again; every remote profile without `id`.
    ProfileUpdate {
        #[serde(default)]
        id: Option<String>,
        /// Store the download even if sing-box rejects it.
        #[serde(default)]
        force: bool,
    },
    /// Switches sing-box to a profile.
    ProfileActivate {
        id: String,
        #[serde(default)]
        force: bool,
    },
    ProfileRemove {
        id: String,
    },
    /// Runs `sing-box check` against a stored profile.
    ProfileCheck {
        id: String,
    },
    /// Moves the configuration sing-box uses now into the store.
    ProfileAdopt,
}

/// Optional services the daemon can install and supervise next to sing-box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Component {
    /// Subscription manager with web UI (sub-store-org/Sub-Store)
    SubStore,
    /// mihomo-based node checker used by Sub-Store scripts (xream/http-meta)
    HttpMeta,
}

impl Component {
    pub const ALL: [Component; 2] = [Component::SubStore, Component::HttpMeta];

    pub fn name(self) -> &'static str {
        match self {
            Component::SubStore => "sub-store",
            Component::HttpMeta => "http-meta",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Component::SubStore => "Sub-Store",
            Component::HttpMeta => "http-meta",
        }
    }

    pub fn log_source(self) -> LogSource {
        match self {
            Component::SubStore => LogSource::SubStore,
            Component::HttpMeta => LogSource::HttpMeta,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ComponentAction {
    Start,
    Stop,
    Restart,
    /// Install if needed, start now and on every daemon start
    Enable,
    /// Stop and keep stopped
    Disable,
    /// Download the latest releases and restart
    Update,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Status(Box<Status>),
    Done {
        message: String,
    },
    Log(LogEntry),
    UpdateInfo(UpdateInfo),
    CoreSources {
        sources: Vec<CoreSource>,
        /// Source `update` follows (the active core's, else daemon.toml's).
        default: String,
    },
    CoreReleases(CoreReleasePage),
    CoreInstalled {
        cores: Vec<StoredCore>,
    },
    Profiles(ProfileList),
    ProfileContent {
        profile: Box<Profile>,
        content: String,
    },
    /// A profile was created or its content replaced.
    ProfileSaved {
        profile: Box<Profile>,
        message: String,
    },
    Error {
        message: String,
    },
}

impl Response {
    pub fn done(message: impl Into<String>) -> Self {
        Response::Done {
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Response::Error {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreState {
    Stopped,
    Starting,
    Running,
    Stopping,
    /// Crashed and waiting before the next automatic restart.
    Backoff,
    /// Exited unexpectedly and will not be restarted.
    Failed,
}

impl CoreState {
    /// Badge text, e.g. "RUNNING".
    pub fn label(self) -> String {
        match self {
            CoreState::Stopped => fl!("state-stopped"),
            CoreState::Starting => fl!("state-starting"),
            CoreState::Running => fl!("state-running"),
            CoreState::Stopping => fl!("state-stopping"),
            CoreState::Backoff => fl!("state-backoff"),
            CoreState::Failed => fl!("state-failed"),
        }
    }

    /// Selector for messages that phrase each state differently.
    pub fn key(self) -> &'static str {
        match self {
            CoreState::Stopped => "stopped",
            CoreState::Starting => "starting",
            CoreState::Running => "running",
            CoreState::Stopping => "stopping",
            CoreState::Backoff => "backoff",
            CoreState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClashApi {
    pub url: String,
    #[serde(default)]
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub daemon_version: String,
    pub daemon_pid: u32,
    /// Unix seconds.
    pub daemon_started_at: u64,
    pub state: CoreState,
    pub pid: Option<u32>,
    /// Unix seconds of the current sing-box process start.
    pub started_at: Option<u64>,
    /// Automatic restarts since the daemon started.
    pub restarts: u32,
    pub last_exit: Option<String>,
    /// Unix seconds of the next automatic restart while in `backoff`.
    pub next_restart_at: Option<u64>,
    pub core_version: Option<String>,
    pub binary: String,
    pub args: Vec<String>,
    pub clash_api: Option<ClashApi>,
    pub update_in_progress: bool,
    /// The first-run question about optional components is still open.
    #[serde(default)]
    pub setup_required: bool,
    #[serde(default)]
    pub components: Vec<ComponentStatus>,
    /// The stored core `binary` points to; `None` for an unmanaged binary.
    #[serde(default)]
    pub active_core: Option<StoredCore>,
    /// The profile the configuration file links to.
    #[serde(default)]
    pub active_profile: Option<Profile>,
}

/// Release sources everyone may install from.
pub const BUILTIN_SOURCES: [&str; 2] = ["MiChongs/sing-box", "SagerNet/sing-box"];

/// `source_name` of cores imported by hand and of a binary that was in place
/// before the version store took over. Stored as is, shown translated.
pub const IMPORTED_NAME: &str = "Custom import";
pub const ADOPTED_NAME: &str = "Previously installed";

/// Display name and description of a built-in source.
pub fn builtin_source(id: &str) -> Option<(String, String)> {
    let index = BUILTIN_SOURCES
        .iter()
        .position(|builtin| builtin.eq_ignore_ascii_case(id))?;
    Some(match index {
        0 => (fl!("source-michongs"), fl!("source-michongs-description")),
        _ => (fl!("source-sagernet"), fl!("source-sagernet-description")),
    })
}

/// A GitHub repository publishing `sing-box-<version>-linux-<arch>[-<variant>]` archives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoreSource {
    /// `owner/repo`
    pub id: String,
    pub name: String,
    pub description: String,
    pub builtin: bool,
}

/// How a download is verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Checksum {
    /// A SHA256SUMS file published with the release.
    Sums,
    /// The sha256 digest GitHub records for the asset.
    Digest,
    /// Given explicitly (imports).
    Pinned,
    /// Nothing to verify against (TLS only).
    #[default]
    None,
}

impl Checksum {
    /// Short name of the verification, e.g. "SHA256SUMS".
    pub fn label(self) -> String {
        match self {
            Checksum::Sums => fl!("checksum-sums"),
            Checksum::Digest => fl!("checksum-digest"),
            Checksum::Pinned => fl!("checksum-pinned"),
            Checksum::None => fl!("checksum-none"),
        }
    }

    /// How a stored core was verified, for messages.
    pub fn description(self) -> String {
        match self {
            Checksum::Sums => fl!("checksum-verified-sums"),
            Checksum::Digest => fl!("checksum-verified-digest"),
            Checksum::Pinned => fl!("checksum-verified-pinned"),
            Checksum::None => fl!("checksum-unverified"),
        }
    }
}

/// Display name of a build variant; "" is the standard build.
pub fn variant_label(variant: &str) -> String {
    if variant.is_empty() {
        fl!("variant-default")
    } else {
        variant.to_owned()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreVariant {
    /// "" is the plain build, otherwise e.g. "ebpf", "glibc", "v3-glibc".
    pub name: String,
    pub asset: String,
    pub size: u64,
    pub checksum: Checksum,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreRelease {
    pub tag: String,
    pub version: String,
    pub published_at: Option<String>,
    pub prerelease: bool,
    /// Builds for this machine's architecture.
    pub variants: Vec<CoreVariant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreReleasePage {
    pub source: String,
    /// e.g. "linux-amd64"
    pub platform: String,
    pub page: u32,
    pub has_more: bool,
    pub releases: Vec<CoreRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredCore {
    /// `<source>/<tag>/<variant>` path below the version store.
    pub id: String,
    /// `owner/repo`, or "local" for imports.
    pub source: String,
    pub source_name: String,
    /// As reported by `sing-box version`.
    pub version: String,
    pub tag: Option<String>,
    pub variant: String,
    pub size: u64,
    pub sha256: String,
    pub checksum: Checksum,
    /// Unix seconds.
    pub installed_at: u64,
    /// Files unpacked next to the binary (e.g. libcronet.so).
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub active: bool,
}

impl StoredCore {
    /// Source name to display. Built-in and local names are translated; the
    /// stored name of a built-in source may predate the current wording.
    pub fn source_label(&self) -> String {
        if let Some((name, _)) = builtin_source(&self.source) {
            return name;
        }
        match self.source_name.as_str() {
            IMPORTED_NAME => fl!("source-imported"),
            ADOPTED_NAME => fl!("source-adopted"),
            name => name.to_owned(),
        }
    }
}

/// A sing-box configuration kept in the daemon's profile store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// Where a remote profile is downloaded from; `None` for local ones.
    #[serde(default)]
    pub url: Option<String>,
    /// Minutes between automatic downloads; 0 downloads only on request.
    #[serde(default)]
    pub interval: u64,
    /// Unix seconds.
    pub created_at: u64,
    /// Unix seconds of the last content change.
    pub updated_at: u64,
    /// Unix seconds of the last successful download.
    #[serde(default)]
    pub fetched_at: Option<u64>,
    /// Why the last download failed.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Traffic reported by the provider (`subscription-userinfo`).
    #[serde(default)]
    pub usage: Option<ProfileUsage>,
    pub size: u64,
    #[serde(default)]
    pub active: bool,
}

impl Profile {
    pub fn is_remote(&self) -> bool {
        self.url.is_some()
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileUsage {
    pub upload: u64,
    pub download: u64,
    pub total: u64,
    /// Unix seconds.
    #[serde(default)]
    pub expire: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileList {
    pub profiles: Vec<Profile>,
    /// The configuration file sing-box is started with, which links to the
    /// active profile; empty when `core.config` lists no file.
    pub slot: String,
    /// The file holds a configuration that is not in the store yet.
    pub unmanaged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentStatus {
    pub component: Component,
    pub enabled: bool,
    pub installed: bool,
    /// A long operation in progress: "installing" or "updating".
    pub busy: Option<String>,
    pub state: CoreState,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub restarts: u32,
    pub last_exit: Option<String>,
    pub next_restart_at: Option<u64>,
    /// e.g. {"backend": "2.42.2", "frontend": "2.34.0", "node": "v24.21.0"}
    pub versions: BTreeMap<String, String>,
    /// Web UI (Sub-Store) or service endpoint (http-meta).
    pub url: Option<String>,
    /// Sub-Store backend base URL, including the secret path.
    pub api: Option<String>,
}

impl ComponentStatus {
    /// Text for the operation in progress.
    pub fn busy_label(&self) -> Option<String> {
        self.busy.as_deref().map(|busy| match busy {
            "installing" => fl!("busy-installing"),
            "updating" => fl!("busy-updating"),
            other => other.to_owned(),
        })
    }
}

/// Display name of a key in [`ComponentStatus::versions`].
pub fn version_label(key: &str) -> String {
    match key {
        "backend" => fl!("version-backend"),
        "frontend" => fl!("version-frontend"),
        other => other.to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    Core,
    Daemon,
    SubStore,
    HttpMeta,
}

impl LogSource {
    /// Tag shown in front of a log line.
    pub fn label(self) -> String {
        match self {
            LogSource::Core => "sing-box".to_owned(),
            LogSource::Daemon => fl!("log-source-daemon"),
            LogSource::SubStore => "sub-store".to_owned(),
            LogSource::HttpMeta => "http-meta".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: u64,
    /// Unix milliseconds.
    pub ts: u64,
    pub source: LogSource,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub current: Option<String>,
    pub latest: String,
    pub tag: String,
    pub asset: String,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub update_available: bool,
}

/// Version store id of a release build: `<source>/<tag>/<variant>` with
/// path-unsafe characters replaced, shared by the daemon and its clients.
pub fn core_store_id(source: &str, tag: &str, variant: &str) -> String {
    fn slug(text: &str) -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._-+".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
    let variant = if variant.is_empty() {
        "default"
    } else {
        variant
    };
    format!("{}/{}/{}", slug(source), slug(tag), slug(variant))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format() {
        let json = serde_json::to_string(&Request::Logs {
            tail: 10,
            follow: true,
        })
        .unwrap();
        assert_eq!(json, r#"{"cmd":"logs","tail":10,"follow":true}"#);
        let parsed: Request = serde_json::from_str(r#"{"cmd":"update"}"#).unwrap();
        assert!(matches!(
            parsed,
            Request::Update {
                tag: None,
                force: false
            }
        ));
    }

    #[test]
    fn component_wire_format() {
        let json = serde_json::to_string(&Request::Component {
            component: Component::SubStore,
            action: ComponentAction::Enable,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"component","component":"sub-store","action":"enable"}"#
        );
        let setup: Request =
            serde_json::from_str(r#"{"cmd":"setup","sub_store":true,"http_meta":false}"#).unwrap();
        assert!(matches!(
            setup,
            Request::Setup {
                sub_store: true,
                http_meta: false
            }
        ));
    }

    #[test]
    fn core_wire_format() {
        let request: Request = serde_json::from_str(
            r#"{"cmd":"core_install","source":"SagerNet/sing-box","tag":"v1.12.0"}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            Request::CoreInstall {
                activate: false,
                force: false,
                ref variant,
                ..
            } if variant.is_empty()
        ));
        let json = serde_json::to_string(&Checksum::Digest).unwrap();
        assert_eq!(json, r#""digest""#);
    }

    #[test]
    fn envelope_carries_the_language() {
        let line = serde_json::to_string(&Envelope {
            request: Request::Start,
            lang: Some("zh-CN".to_owned()),
        })
        .unwrap();
        assert_eq!(line, r#"{"cmd":"start","lang":"zh-CN"}"#);
        // Older daemons parse the bare request and ignore `lang`.
        assert!(matches!(
            serde_json::from_str::<Request>(&line).unwrap(),
            Request::Start
        ));
        let logs: Request = serde_json::from_str(r#"{"cmd":"logs","tail":5,"lang":"en"}"#).unwrap();
        assert!(matches!(logs, Request::Logs { tail: 5, .. }));
        // Older clients send no language.
        let envelope: Envelope =
            serde_json::from_str(r#"{"cmd":"core_remove","id":"a/b/c"}"#).unwrap();
        assert!(envelope.lang.is_none());
        assert!(matches!(envelope.request, Request::CoreRemove { ref id } if id == "a/b/c"));
        let envelope: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(envelope.lang.as_deref(), Some("zh-CN"));
    }

    #[test]
    fn profile_wire_format() {
        let request: Request = serde_json::from_str(
            r#"{"cmd":"profile_add","url":"https://example.com/sub","activate":true}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            Request::ProfileAdd {
                name: None,
                content: None,
                url: Some(_),
                interval: None,
                activate: true,
            }
        ));
        let json = serde_json::to_string(&Request::ProfileUpdate {
            id: None,
            force: false,
        })
        .unwrap();
        assert_eq!(json, r#"{"cmd":"profile_update","id":null,"force":false}"#);
        let profile: Profile = serde_json::from_str(
            r#"{"id":"a1","name":"Home","created_at":1,"updated_at":2,"size":3}"#,
        )
        .unwrap();
        assert!(!profile.is_remote() && !profile.active && profile.usage.is_none());
    }

    #[test]
    fn response_wire_format() {
        let json = serde_json::to_string(&Response::done("ok")).unwrap();
        assert_eq!(json, r#"{"type":"done","message":"ok"}"#);
    }
}
