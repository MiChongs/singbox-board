//! Wire protocol between the root daemon and its clients (TUI / CLI).
//!
//! Every connection carries exactly one request: the client writes one JSON
//! line, the daemon answers with one JSON line. `logs` with `follow = true` is
//! the only streaming request; the daemon keeps writing `log` lines until the
//! client disconnects.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Upper bound for a single request line.
pub const MAX_REQUEST_BYTES: u64 = 64 * 1024;

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
    Status(Status),
    Done { message: String },
    Log(LogEntry),
    UpdateInfo(UpdateInfo),
    Error { message: String },
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
    pub fn label(self) -> &'static str {
        match self {
            CoreState::Stopped => "STOPPED",
            CoreState::Starting => "STARTING",
            CoreState::Running => "RUNNING",
            CoreState::Stopping => "STOPPING",
            CoreState::Backoff => "BACKOFF",
            CoreState::Failed => "FAILED",
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentStatus {
    pub component: Component,
    pub enabled: bool,
    pub installed: bool,
    /// A long operation in progress, e.g. "installing".
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    Core,
    Daemon,
    SubStore,
    HttpMeta,
}

impl LogSource {
    pub fn tag(self) -> &'static str {
        match self {
            LogSource::Core => "sing-box",
            LogSource::Daemon => "daemon",
            LogSource::SubStore => "sub-store",
            LogSource::HttpMeta => "http-meta",
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
    fn response_wire_format() {
        let json = serde_json::to_string(&Response::done("ok")).unwrap();
        assert_eq!(json, r#"{"type":"done","message":"ok"}"#);
    }
}
