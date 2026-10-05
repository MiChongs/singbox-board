//! Persistent component state (`<data_dir>/state.json`) and on-disk layout.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::i18n::fl;
use crate::protocol::Component;
pub use crate::util::{random_token, write_atomic};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The first-run question has been answered.
    pub setup_done: bool,
    pub sub_store: SubStoreState,
    pub http_meta: HttpMetaState,
    /// Version of the Node.js build downloaded into `runtime/node`.
    pub node_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SubStoreState {
    pub enabled: bool,
    /// Secret path prefix of the backend API (Sub-Store has no other auth).
    pub backend_path: String,
    pub backend_version: Option<String>,
    pub frontend_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpMetaState {
    pub enabled: bool,
    pub version: Option<String>,
    pub mihomo_version: Option<String>,
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content)
                .with_context(|| fl!("err-parse", path = path.display().to_string())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => {
                Err(err).with_context(|| fl!("err-read", path = path.display().to_string()))
            }
        }
    }

    /// Atomic write, owner-only: the file holds the Sub-Store secret path.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        write_atomic(path, &json, 0o600)
    }

    pub fn enabled(&self, component: Component) -> bool {
        match component {
            Component::SubStore => self.sub_store.enabled,
            Component::HttpMeta => self.http_meta.enabled,
        }
    }

    pub fn set_enabled(&mut self, component: Component, enabled: bool) {
        match component {
            Component::SubStore => self.sub_store.enabled = enabled,
            Component::HttpMeta => self.http_meta.enabled = enabled,
        }
        if component == Component::SubStore && enabled && self.sub_store.backend_path.is_empty() {
            self.sub_store.backend_path = random_token(24);
        }
    }
}

/// Where each component lives below `data_dir`.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn state(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn node(&self) -> PathBuf {
        self.root.join("runtime/node/bin/node")
    }

    pub fn sub_store(&self) -> PathBuf {
        self.root.join("sub-store")
    }

    pub fn sub_store_bundle(&self) -> PathBuf {
        self.sub_store().join("sub-store.bundle.js")
    }

    pub fn sub_store_frontend(&self) -> PathBuf {
        self.sub_store().join("frontend")
    }

    pub fn sub_store_data(&self) -> PathBuf {
        self.sub_store().join("data")
    }

    pub fn http_meta(&self) -> PathBuf {
        self.root.join("http-meta")
    }

    pub fn http_meta_bundle(&self) -> PathBuf {
        self.http_meta().join("http-meta.bundle.js")
    }

    /// `META_FOLDER`: holds the mihomo binary (named `http-meta`) and `tpl.yaml`.
    pub fn meta_folder(&self) -> PathBuf {
        self.http_meta().join("meta")
    }

    pub fn mihomo(&self) -> PathBuf {
        self.meta_folder().join("http-meta")
    }

    pub fn meta_template(&self) -> PathBuf {
        self.meta_folder().join("tpl.yaml")
    }

    /// `META_TEMP_FOLDER`: per-check configs and logs, writable by the service user.
    pub fn meta_temp(&self) -> PathBuf {
        self.http_meta().join("tmp")
    }

    pub fn installed(&self, component: Component) -> bool {
        match component {
            Component::SubStore => {
                self.sub_store_bundle().is_file()
                    && self.sub_store_frontend().join("index.html").is_file()
            }
            Component::HttpMeta => {
                self.http_meta_bundle().is_file()
                    && self.mihomo().is_file()
                    && self.meta_template().is_file()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_alphanumeric() {
        let token = random_token(24);
        assert_eq!(token.len(), 24);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(token, random_token(24));
    }

    #[test]
    fn state_roundtrip_and_secret() {
        let dir = std::env::temp_dir().join(format!("sbb-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut state = State::load(&path).unwrap();
        assert!(!state.setup_done);
        state.setup_done = true;
        state.set_enabled(Component::SubStore, true);
        assert_eq!(state.sub_store.backend_path.len(), 24);
        state.save(&path).unwrap();
        let loaded = State::load(&path).unwrap();
        assert!(loaded.setup_done && loaded.sub_store.enabled);
        assert_eq!(loaded.sub_store.backend_path, state.sub_store.backend_path);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
