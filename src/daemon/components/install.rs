//! Downloads and verifies the files each component needs.
//!
//! - Sub-Store: `sub-store.bundle.js` (sub-store-org/Sub-Store) and the web UI
//!   `dist.zip` (sub-store-org/Sub-Store-Front-End)
//! - http-meta: `http-meta.bundle.js` + `tpl.yaml` (xream/http-meta) and the
//!   mihomo core (MetaCubeX/mihomo), installed as `meta/http-meta`
//! - Node.js: an official LTS build when no suitable `node` is available
//!
//! GitHub assets are checked against the sha256 digest GitHub publishes,
//! Node.js archives against `SHASUMS256.txt`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use tokio::process::Command;

use super::state::{Layout, State, write_atomic};
use crate::config::DaemonConfig;
use crate::daemon::github::{GitHub, verify_sha256};
use crate::daemon::logs::LogHub;
use crate::daemon::updater::parse_sha256sums;

/// Oldest Node.js major accepted from PATH; older ones trigger a download.
pub const MIN_NODE_MAJOR: u32 = 22;
const MAX_ASSET_BYTES: usize = 64 * 1024 * 1024;
const MAX_NODE_ARCHIVE_BYTES: usize = 160 * 1024 * 1024;

pub struct Installer<'a> {
    pub github: GitHub,
    pub config: &'a DaemonConfig,
    pub layout: &'a Layout,
    pub logs: &'a LogHub,
}

impl Installer<'_> {
    // ----- Node.js ------------------------------------------------------

    /// The Node.js executable to run components with, downloading one if needed.
    pub async fn node(&self, state: &mut State) -> Result<PathBuf> {
        if let Some(node) = self
            .config
            .components
            .node
            .as_ref()
            .filter(|p| !p.as_os_str().is_empty())
        {
            let (major, version) = node_version(node)
                .await
                .with_context(|| format!("components.node = {}", node.display()))?;
            if major < MIN_NODE_MAJOR {
                self.logs.warn(format!(
                    "{} is Node.js {version}; Sub-Store is tested with v24",
                    node.display()
                ));
            }
            return Ok(node.clone());
        }
        let bundled = self.layout.node();
        if let Ok((major, _)) = node_version(&bundled).await
            && major >= MIN_NODE_MAJOR
        {
            return Ok(bundled);
        }
        if let Some(system) = find_in_path("node")
            && let Ok((major, _)) = node_version(&system).await
            && major >= MIN_NODE_MAJOR
        {
            return Ok(system);
        }
        let version = self.download_node().await?;
        state.node_version = Some(version);
        Ok(bundled)
    }

    async fn download_node(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct NodeRelease {
            version: String,
            lts: serde_json::Value,
            files: Vec<String>,
        }
        let arch = node_arch()?;
        let mirror = self.config.components.node_mirror.trim_end_matches('/');
        let index: Vec<NodeRelease> = serde_json::from_slice(
            &self
                .github
                .fetch(&format!("{mirror}/index.json"), 16 * 1024 * 1024)
                .await?,
        )
        .context("parse Node.js release index")?;
        let platform = format!("linux-{arch}");
        let release = index
            .iter()
            .find(|r| r.lts != serde_json::Value::Bool(false) && r.files.contains(&platform))
            .ok_or_else(|| {
                anyhow!("no Node.js LTS build for {platform}; install node and set components.node")
            })?;
        let version = release.version.clone();
        let name = format!("node-{version}-{platform}.tar.gz");
        self.logs
            .info(format!("downloading Node.js {version} ({platform})"));
        let sums = String::from_utf8(
            self.github
                .fetch(&format!("{mirror}/{version}/SHASUMS256.txt"), 1024 * 1024)
                .await?,
        )?;
        let expected = parse_sha256sums(&sums)
            .remove(&name)
            .ok_or_else(|| anyhow!("SHASUMS256.txt has no entry for {name}"))?;
        let archive = self
            .github
            .fetch(
                &format!("{mirror}/{version}/{name}"),
                MAX_NODE_ARCHIVE_BYTES,
            )
            .await?;
        verify_sha256(&archive, &expected, &name)?;
        let member = format!("node-{version}-{platform}/bin/node");
        let dest = self.layout.node();
        blocking(move || extract_tar_member(&archive, &member, &dest)).await?;
        node_version(&self.layout.node())
            .await
            .context("downloaded Node.js does not run on this system")?;
        self.logs.info(format!("installed Node.js {version}"));
        Ok(version)
    }

    // ----- Sub-Store ----------------------------------------------------

    /// Installs or updates Sub-Store; returns whether anything changed.
    pub async fn sub_store(&self, state: &mut State) -> Result<bool> {
        let cfg = &self.config.sub_store;
        let dir = self.layout.sub_store();
        tokio::fs::create_dir_all(&dir).await?;
        let mut changed = false;

        let backend = self.github.release(&cfg.backend_repo, None, false).await?;
        let bundle = self.layout.sub_store_bundle();
        if !bundle.is_file()
            || state.sub_store.backend_version.as_deref() != Some(backend.version())
        {
            self.logs.info(format!(
                "downloading Sub-Store backend {}",
                backend.version()
            ));
            let data = self
                .github
                .download(backend.require("sub-store.bundle.js")?, MAX_ASSET_BYTES)
                .await?;
            write_atomic(&bundle, &data, 0o644)?;
            state.sub_store.backend_version = Some(backend.version().to_owned());
            changed = true;
        }

        let frontend = self.github.release(&cfg.frontend_repo, None, false).await?;
        let web = self.layout.sub_store_frontend();
        if !web.join("index.html").is_file()
            || state.sub_store.frontend_version.as_deref() != Some(frontend.version())
        {
            self.logs.info(format!(
                "downloading Sub-Store web UI {}",
                frontend.version()
            ));
            let data = self
                .github
                .download(frontend.require("dist.zip")?, MAX_ASSET_BYTES)
                .await?;
            blocking(move || replace_dir_from_zip(&data, &web)).await?;
            state.sub_store.frontend_version = Some(frontend.version().to_owned());
            changed = true;
        }
        Ok(changed)
    }

    // ----- http-meta ----------------------------------------------------

    /// Installs or updates http-meta and its mihomo core; returns whether anything changed.
    pub async fn http_meta(&self, state: &mut State) -> Result<bool> {
        let cfg = &self.config.http_meta;
        tokio::fs::create_dir_all(self.layout.meta_folder()).await?;
        let mut changed = false;

        let release = self.github.release(&cfg.repo, None, false).await?;
        if !self.layout.http_meta_bundle().is_file()
            || !self.layout.meta_template().is_file()
            || state.http_meta.version.as_deref() != Some(release.version())
        {
            self.logs
                .info(format!("downloading http-meta {}", release.version()));
            let bundle = self
                .github
                .download(release.require("http-meta.bundle.js")?, MAX_ASSET_BYTES)
                .await?;
            let template = self
                .github
                .download(release.require("tpl.yaml")?, 1024 * 1024)
                .await?;
            write_atomic(&self.layout.http_meta_bundle(), &bundle, 0o644)?;
            write_atomic(&self.layout.meta_template(), &template, 0o644)?;
            state.http_meta.version = Some(release.version().to_owned());
            changed = true;
        }

        let mihomo = self.github.release(&cfg.mihomo_repo, None, false).await?;
        if !self.layout.mihomo().is_file()
            || state.http_meta.mihomo_version.as_deref() != Some(mihomo.tag_name.as_str())
        {
            let arches: Vec<String> = match cfg.mihomo_arch.as_deref().filter(|a| !a.is_empty()) {
                Some(arch) => vec![arch.to_owned()],
                None => mihomo_arches().iter().map(|a| (*a).to_owned()).collect(),
            };
            let asset = arches
                .iter()
                .find_map(|arch| {
                    mihomo.asset(&format!("mihomo-linux-{arch}-{}.gz", mihomo.tag_name))
                })
                .ok_or_else(|| {
                    anyhow!(
                        "mihomo {} has no build for {}; set http_meta.mihomo_arch",
                        mihomo.tag_name,
                        std::env::consts::ARCH
                    )
                })?;
            self.logs.info(format!(
                "downloading mihomo {} ({})",
                mihomo.tag_name, asset.name
            ));
            let compressed = self.github.download(asset, MAX_ASSET_BYTES).await?;
            let dest = self.layout.mihomo();
            blocking(move || {
                let mut binary = Vec::new();
                flate2::read::GzDecoder::new(&compressed[..])
                    .take(256 * 1024 * 1024)
                    .read_to_end(&mut binary)
                    .context("decompress mihomo")?;
                write_atomic(&dest, &binary, 0o755)
            })
            .await?;
            let output = Command::new(self.layout.mihomo())
                .arg("-v")
                .kill_on_drop(true)
                .output()
                .await;
            if !output.as_ref().is_ok_and(|o| o.status.success()) {
                let _ = std::fs::remove_file(self.layout.mihomo());
                bail!("downloaded mihomo does not run on this system; set http_meta.mihomo_arch");
            }
            state.http_meta.mihomo_version = Some(mihomo.tag_name.clone());
            changed = true;
        }
        Ok(changed)
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .context("background task panicked")?
}

/// `node --version` → (major, "v24.21.0").
pub async fn node_version(node: &Path) -> Result<(u32, String)> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(node)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("{} --version timed out", node.display()))??;
    if !output.status.success() {
        bail!("{} --version failed", node.display());
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let major = version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .ok_or_else(|| anyhow!("unexpected Node.js version {version:?}"))?;
    Ok((major, version))
}

pub fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
}

fn node_arch() -> Result<&'static str> {
    Ok(match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "powerpc64" if cfg!(target_endian = "little") => "ppc64le",
        "s390x" => "s390x",
        other => {
            bail!("no official Node.js build for {other}; install node and set components.node")
        }
    })
}

/// mihomo release architectures to try, most compatible first.
fn mihomo_arches() -> &'static [&'static str] {
    match std::env::consts::ARCH {
        "x86_64" => &["amd64-v1", "amd64-compatible", "amd64"],
        "aarch64" => &["arm64"],
        "x86" => &["386"],
        "arm" => &["armv7"],
        "riscv64" => &["riscv64"],
        "loongarch64" => &["loong64-abi2", "loong64-abi1"],
        "mips" if cfg!(target_endian = "little") => &["mipsle-softfloat"],
        "mips64" if cfg!(target_endian = "little") => &["mips64le"],
        "powerpc64" if cfg!(target_endian = "little") => &["ppc64le"],
        "s390x" => &["s390x"],
        _ => &[],
    }
}

/// Extracts one regular file from a `.tar.gz` to `dest` (mode 0755).
fn extract_tar_member(archive: &[u8], member: &str, dest: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_str() != Some(member) {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        return write_atomic(dest, &data, 0o755);
    }
    bail!("archive does not contain {member}")
}

/// Unpacks a zip into a fresh directory and swaps it in for `dest`. A single
/// top-level directory (Sub-Store's `dist/`) is stripped.
fn replace_dir_from_zip(data: &[u8], dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).context("open zip")?;
    let names: Vec<PathBuf> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok()?.enclosed_name())
        .collect();
    let prefix = common_root(&names);

    let staging = sibling(dest, ".new");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        // enclosed_name rejects absolute paths and `..` traversal.
        let Some(name) = file.enclosed_name() else {
            continue;
        };
        let relative = match &prefix {
            Some(prefix) => name.strip_prefix(prefix).unwrap_or(&name).to_path_buf(),
            None => name,
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = staging.join(&relative);
        if file.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&target)
            .with_context(|| format!("create {}", target.display()))?;
        std::io::copy(&mut file.by_ref().take(MAX_ASSET_BYTES as u64), &mut out)?;
    }
    if !staging.join("index.html").is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("web UI archive has no index.html");
    }
    let old = sibling(dest, ".old");
    let _ = std::fs::remove_dir_all(&old);
    if dest.exists() {
        std::fs::rename(dest, &old)?;
    }
    std::fs::rename(&staging, dest)?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

fn common_root(names: &[PathBuf]) -> Option<PathBuf> {
    let first = names.first()?.components().next()?;
    let root = PathBuf::from(first.as_os_str());
    let all_under = names.iter().all(|n| n.starts_with(&root));
    let has_nested = names.iter().any(|n| n.components().count() > 1);
    (all_under && has_nested).then_some(root)
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(content).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn zip_strips_single_root_and_swaps() {
        let dir = std::env::temp_dir().join(format!("sbb-zip-{}", std::process::id()));
        let dest = dir.join("frontend");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("stale.js"), b"old").unwrap();
        let data = zip_with(&[
            ("dist/", b""),
            ("dist/index.html", b"<html>"),
            ("dist/assets/app.js", b"js"),
        ]);
        replace_dir_from_zip(&data, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("index.html")).unwrap(), b"<html>");
        assert_eq!(std::fs::read(dest.join("assets/app.js")).unwrap(), b"js");
        assert!(!dest.join("stale.js").exists());
        assert!(!sibling(&dest, ".old").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zip_without_index_is_rejected() {
        let dir = std::env::temp_dir().join(format!("sbb-zip-bad-{}", std::process::id()));
        let dest = dir.join("frontend");
        let data = zip_with(&[("readme.txt", b"x")]);
        assert!(replace_dir_from_zip(&data, &dest).is_err());
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tar_member_extraction() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, content) in [
            ("node-v1/README.md", &b"readme"[..]),
            ("node-v1/bin/node", &b"ELF"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, content).unwrap();
        }
        let archive = builder.into_inner().unwrap().finish().unwrap();
        let dest = std::env::temp_dir().join(format!("sbb-node-{}/bin/node", std::process::id()));
        extract_tar_member(&archive, "node-v1/bin/node", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"ELF");
        assert!(extract_tar_member(&archive, "node-v1/bin/npm", &dest).is_err());
        std::fs::remove_dir_all(dest.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn mihomo_arch_known_for_this_host() {
        assert!(!mihomo_arches().is_empty());
    }
}
