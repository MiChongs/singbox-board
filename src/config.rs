use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::i18n::fl;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/singbox-board/daemon.toml";
pub const DEFAULT_SOCKET: &str = "/run/singbox-board/daemon.sock";

/// Commented configuration template, also printed by `daemon --print-default-config`.
pub const TEMPLATE: &str = include_str!("../contrib/daemon.toml");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// Language of the daemon's log: "auto" (the locale), "en" or "zh-CN".
    /// Replies to clients use the language each client asks for.
    pub language: String,
    pub socket: PathBuf,
    pub socket_group: Option<String>,
    pub allowed_uids: Vec<u32>,
    pub log_buffer: usize,
    pub forward_core_logs: bool,
    pub core: CoreConfig,
    pub restart: RestartConfig,
    pub clash_api: ClashApiOverride,
    pub update: UpdateConfig,
    pub profiles: ProfilesConfig,
    pub components: ComponentsConfig,
    pub sub_store: SubStoreConfig,
    pub http_meta: HttpMetaConfig,
    pub containers: ContainersConfig,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            language: "auto".to_owned(),
            socket: PathBuf::from(DEFAULT_SOCKET),
            socket_group: Some("singbox-board".to_owned()),
            allowed_uids: Vec::new(),
            log_buffer: 2000,
            forward_core_logs: true,
            core: CoreConfig::default(),
            restart: RestartConfig::default(),
            clash_api: ClashApiOverride::default(),
            update: UpdateConfig::default(),
            profiles: ProfilesConfig::default(),
            components: ComponentsConfig::default(),
            sub_store: SubStoreConfig::default(),
            http_meta: HttpMetaConfig::default(),
            containers: ContainersConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CoreConfig {
    pub binary: PathBuf,
    pub config: Vec<PathBuf>,
    pub config_dir: Vec<PathBuf>,
    pub working_dir: Option<PathBuf>,
    pub extra_args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub auto_start: bool,
    pub check_before_start: bool,
    pub stop_timeout_secs: u64,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("/usr/local/bin/sing-box"),
            config: vec![PathBuf::from("/etc/sing-box/config.json")],
            config_dir: Vec::new(),
            working_dir: Some(PathBuf::from("/var/lib/sing-box")),
            extra_args: Vec::new(),
            env: BTreeMap::new(),
            auto_start: true,
            check_before_start: true,
            stop_timeout_secs: 10,
        }
    }
}

impl CoreConfig {
    /// Global flags shared by `run` and `check`; `primary` stands in for the
    /// first `config` entry.
    fn global_args(&self, primary: Option<&Path>) -> Vec<String> {
        let mut args = vec!["--disable-color".to_owned()];
        if let Some(dir) = &self.working_dir {
            args.push("-D".to_owned());
            args.push(dir.display().to_string());
        }
        let mut files: Vec<&Path> = self.config.iter().map(PathBuf::as_path).collect();
        if let Some(primary) = primary {
            match files.first_mut() {
                Some(first) => *first = primary,
                None => files.push(primary),
            }
        }
        for path in files {
            args.push("-c".to_owned());
            args.push(path.display().to_string());
        }
        for dir in &self.config_dir {
            args.push("-C".to_owned());
            args.push(dir.display().to_string());
        }
        args
    }

    pub fn run_args(&self) -> Vec<String> {
        let mut args = self.global_args(None);
        args.push("run".to_owned());
        args.extend(self.extra_args.iter().cloned());
        args
    }

    pub fn check_args(&self) -> Vec<String> {
        self.check_args_with(None)
    }

    /// `check` flags with `primary` in place of the first configuration
    /// file, to try a profile before it is switched to.
    pub fn check_args_with(&self, primary: Option<&Path>) -> Vec<String> {
        let mut args = self.global_args(primary);
        args.push("check".to_owned());
        args
    }

    /// The first configuration file, which links to the active profile.
    pub fn config_slot(&self) -> Option<PathBuf> {
        self.config.first().map(|path| self.resolve(path))
    }

    /// sing-box changes into `-D` before reading `-c`, so relative config
    /// paths are relative to the working directory.
    pub fn resolve(&self, path: &Path) -> PathBuf {
        match &self.working_dir {
            Some(dir) if path.is_relative() => dir.join(path),
            _ => path.to_path_buf(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Always,
    OnFailure,
    Never,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RestartConfig {
    pub policy: RestartPolicy,
    pub initial_backoff_ms: u64,
    pub max_backoff_secs: u64,
    pub stable_after_secs: u64,
}

impl Default for RestartConfig {
    fn default() -> Self {
        Self {
            policy: RestartPolicy::OnFailure,
            initial_backoff_ms: 1000,
            max_backoff_secs: 60,
            stable_after_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ClashApiOverride {
    pub url: Option<String>,
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateConfig {
    pub repo: String,
    pub api_url: String,
    pub variant: String,
    pub arch: Option<String>,
    pub prerelease: bool,
    pub proxy: Option<String>,
    pub mirror: Option<String>,
    pub github_token: Option<String>,
    pub restart_after_update: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            repo: "MiChongs/sing-box".to_owned(),
            api_url: "https://api.github.com".to_owned(),
            variant: String::new(),
            arch: None,
            prerelease: false,
            proxy: None,
            mirror: None,
            github_token: None,
            restart_after_update: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ProfilesConfig {
    /// User-Agent for downloading remote profiles; empty sends
    /// `sing-box/<core version>`, which providers use to pick the format.
    pub user_agent: String,
    /// Proxy for downloading remote profiles.
    pub proxy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ComponentsConfig {
    /// Install root of the optional components; also holds `state.json`.
    pub data_dir: PathBuf,
    /// Node.js executable. Unset: `node` from PATH when new enough, else an
    /// LTS build is downloaded into `data_dir`.
    pub node: Option<PathBuf>,
    /// Where Node.js builds are downloaded from (`index.json` + `SHASUMS256.txt`).
    pub node_mirror: String,
    /// Unprivileged user the components run as when the daemon is root.
    pub run_as: String,
}

impl Default for ComponentsConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("/var/lib/singbox-board"),
            node: None,
            node_mirror: "https://nodejs.org/dist".to_owned(),
            run_as: "nobody".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SubStoreConfig {
    pub host: String,
    pub port: u16,
    pub backend_repo: String,
    pub frontend_repo: String,
    /// `SUB_STORE_BACKEND_SYNC_CRON`, e.g. "55 23 * * *".
    pub sync_cron: Option<String>,
    /// `SUB_STORE_PRODUCE_CRON`.
    pub produce_cron: Option<String>,
    /// `SUB_STORE_BACKEND_DEFAULT_PROXY` for fetching remote subscriptions.
    pub default_proxy: Option<String>,
    pub env: BTreeMap<String, String>,
}

impl Default for SubStoreConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 3001,
            backend_repo: "sub-store-org/Sub-Store".to_owned(),
            frontend_repo: "sub-store-org/Sub-Store-Front-End".to_owned(),
            sync_cron: None,
            produce_cron: None,
            default_proxy: None,
            env: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HttpMetaConfig {
    pub host: String,
    pub port: u16,
    /// `AUTHORIZATION` header value required by http-meta when set.
    pub authorization: Option<String>,
    pub repo: String,
    pub mihomo_repo: String,
    /// mihomo release architecture, e.g. "amd64-v3"; detected when unset.
    pub mihomo_arch: Option<String>,
    pub env: BTreeMap<String, String>,
}

impl Default for HttpMetaConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 9876,
            authorization: None,
            repo: "xream/http-meta".to_owned(),
            mihomo_repo: "MetaCubeX/mihomo".to_owned(),
            mihomo_arch: None,
            env: BTreeMap::new(),
        }
    }
}

/// Containers run by kurumi-containerd (Tools-cx-app/kurumi-containerd).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ContainersConfig {
    /// kurumi-containerd executable. Unset: the release the daemon downloads
    /// into `<components.data_dir>/containers/runtime`, else
    /// `kurumi-containerd` from PATH.
    pub runtime: Option<PathBuf>,
    /// GitHub repository the runtime is downloaded from.
    pub repo: String,
    /// Root filesystem images (`singbox-board container images`): a server
    /// laid out like images.linuxcontainers.org.
    pub image_server: String,
    /// Start containers marked for autostart once per boot, when the daemon
    /// starts.
    pub autostart: bool,
    /// Stop running containers when the daemon stops. Otherwise they keep
    /// running across daemon restarts and are only stopped when the system
    /// shuts down.
    pub stop_on_shutdown: bool,
    /// Start every container in a transient systemd scope (machine.slice),
    /// so that stopping the daemon's service does not kill it.
    pub systemd_scope: bool,
}

impl Default for ContainersConfig {
    fn default() -> Self {
        Self {
            runtime: None,
            repo: "Tools-cx-app/kurumi-containerd".to_owned(),
            image_server: "https://images.linuxcontainers.org".to_owned(),
            autostart: true,
            stop_on_shutdown: false,
            systemd_scope: true,
        }
    }
}

impl DaemonConfig {
    /// Loads the configuration file. A missing file at the default location
    /// yields the defaults; a missing explicitly given file is an error.
    pub fn load(path: &Path, explicit: bool) -> Result<(Self, bool)> {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let config = toml::from_str(&content)
                    .with_context(|| fl!("err-parse", path = path.display().to_string()))?;
                Ok((config, true))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && !explicit => {
                Ok((Self::default(), false))
            }
            Err(err) => {
                Err(err).with_context(|| fl!("err-read", path = path.display().to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_matches_defaults() {
        let parsed: DaemonConfig = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(parsed, DaemonConfig::default());
    }

    #[test]
    fn run_args() {
        let core = CoreConfig {
            config_dir: vec![PathBuf::from("/etc/sing-box/conf.d")],
            extra_args: vec!["--foo".to_owned()],
            ..CoreConfig::default()
        };
        assert_eq!(
            core.run_args(),
            [
                "--disable-color",
                "-D",
                "/var/lib/sing-box",
                "-c",
                "/etc/sing-box/config.json",
                "-C",
                "/etc/sing-box/conf.d",
                "run",
                "--foo"
            ]
        );
        assert_eq!(core.check_args().last().unwrap(), "check");
        let candidate = core.check_args_with(Some(Path::new("/tmp/candidate.json")));
        assert_eq!(candidate[3..6], ["-c", "/tmp/candidate.json", "-C"]);
        let bare = CoreConfig {
            config: Vec::new(),
            ..CoreConfig::default()
        };
        assert!(bare.config_slot().is_none());
        assert_eq!(
            bare.check_args_with(Some(Path::new("/x.json")))[3..5],
            ["-c", "/x.json"]
        );
    }

    #[test]
    fn relative_paths_follow_working_dir() {
        let core = CoreConfig::default();
        assert_eq!(
            core.resolve(Path::new("config.json")),
            PathBuf::from("/var/lib/sing-box/config.json")
        );
        assert_eq!(
            core.resolve(Path::new("/etc/x.json")),
            PathBuf::from("/etc/x.json")
        );
    }
}
