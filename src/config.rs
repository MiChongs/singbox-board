use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/singbox-board/daemon.toml";
pub const DEFAULT_SOCKET: &str = "/run/singbox-board/daemon.sock";

/// Commented configuration template, also printed by `daemon --print-default-config`.
pub const TEMPLATE: &str = include_str!("../contrib/daemon.toml");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub socket: PathBuf,
    pub socket_group: Option<String>,
    pub allowed_uids: Vec<u32>,
    pub log_buffer: usize,
    pub forward_core_logs: bool,
    pub core: CoreConfig,
    pub restart: RestartConfig,
    pub clash_api: ClashApiOverride,
    pub update: UpdateConfig,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket: PathBuf::from(DEFAULT_SOCKET),
            socket_group: Some("singbox-board".to_owned()),
            allowed_uids: Vec::new(),
            log_buffer: 2000,
            forward_core_logs: true,
            core: CoreConfig::default(),
            restart: RestartConfig::default(),
            clash_api: ClashApiOverride::default(),
            update: UpdateConfig::default(),
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
    /// Global flags shared by `run` and `check`.
    fn global_args(&self) -> Vec<String> {
        let mut args = vec!["--disable-color".to_owned()];
        if let Some(dir) = &self.working_dir {
            args.push("-D".to_owned());
            args.push(dir.display().to_string());
        }
        for path in &self.config {
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
        let mut args = self.global_args();
        args.push("run".to_owned());
        args.extend(self.extra_args.iter().cloned());
        args
    }

    pub fn check_args(&self) -> Vec<String> {
        let mut args = self.global_args();
        args.push("check".to_owned());
        args
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

impl DaemonConfig {
    /// Loads the configuration file. A missing file at the default location
    /// yields the defaults; a missing explicitly given file is an error.
    pub fn load(path: &Path, explicit: bool) -> Result<(Self, bool)> {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let config = toml::from_str(&content)
                    .with_context(|| format!("parse {}", path.display()))?;
                Ok((config, true))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && !explicit => {
                Ok((Self::default(), false))
            }
            Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
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
