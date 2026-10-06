//! Containers on Windows: kurumi-containerd runs Linux system containers
//! and needs a Linux kernel, so every request explains that instead.

use std::sync::Arc;

use anyhow::{Result, bail};

use super::logs::LogHub;
use crate::config::DaemonConfig;
use crate::i18n::fl;
use crate::protocol::{
    Checksum, Container, ContainerAction, ContainerOverview, ContainerRuntime, ExecResult,
    ImageList,
};

pub struct ContainerManager;

impl ContainerManager {
    pub fn new(_config: DaemonConfig, _logs: Arc<LogHub>) -> Arc<Self> {
        Arc::new(Self)
    }

    pub fn spawn_autostart(self: &Arc<Self>) {}

    pub async fn shutdown(&self) {}

    pub async fn overview(&self) -> ContainerOverview {
        ContainerOverview {
            runtime: ContainerRuntime {
                binary: None,
                version: None,
                origin: None,
                tag: None,
                checksum: Checksum::None,
                busy: None,
                target: None,
                problem: Some(fl!("win-containers-unsupported")),
            },
            containers: Vec::new(),
            home: String::new(),
        }
    }

    pub async fn get(&self, _query: &str) -> Result<(Container, String)> {
        unsupported()
    }

    pub async fn add(
        &self,
        _name: Option<String>,
        _content: Option<String>,
        _file: Option<String>,
        _network: Option<String>,
    ) -> Result<(Container, String)> {
        unsupported()
    }

    pub async fn save(
        &self,
        _query: &str,
        _content: &str,
        _force: bool,
    ) -> Result<(Container, String)> {
        unsupported()
    }

    pub fn set(
        &self,
        _query: &str,
        _name: Option<String>,
        _autostart: Option<bool>,
    ) -> Result<String> {
        unsupported()
    }

    pub async fn remove(&self, _query: &str, _purge: bool) -> Result<String> {
        unsupported()
    }

    pub async fn adopt(&self, _path: Option<String>) -> Result<String> {
        unsupported()
    }

    pub async fn control(&self, _query: &str, _action: ContainerAction) -> Result<String> {
        unsupported()
    }

    pub async fn install(
        &self,
        _query: &str,
        _source: &str,
        _size: Option<String>,
        _sha256: Option<String>,
        _force: bool,
    ) -> Result<String> {
        unsupported()
    }

    pub async fn exec(
        &self,
        _query: &str,
        _command: Vec<String>,
        _timeout: Option<u64>,
    ) -> Result<ExecResult> {
        unsupported()
    }

    pub async fn images(&self, _refresh: bool) -> Result<ImageList> {
        unsupported()
    }

    pub async fn check(&self) -> Result<String> {
        unsupported()
    }

    pub async fn scan(&self) -> Result<String> {
        unsupported()
    }

    pub async fn update_runtime(&self, _tag: Option<&str>, _force: bool) -> Result<String> {
        unsupported()
    }

    pub async fn import_runtime(&self, _location: &str, _sha256: Option<&str>) -> Result<String> {
        unsupported()
    }
}

fn unsupported<T>() -> Result<T> {
    bail!(fl!("win-containers-unsupported"))
}
