//! Installs and updates the sing-box binary from GitHub releases.
//!
//! Release archives are named `sing-box-<version>-linux-<arch>[-<variant>].tar.gz`
//! and contain a single executable; every release ships a `SHA256SUMS` file.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use futures::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::UpdateConfig;
use crate::protocol::UpdateInfo;

const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

impl Release {
    pub fn version(&self) -> &str {
        self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)
    }

    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

/// A verified binary written next to its final location, ready to be renamed.
#[derive(Debug)]
pub struct StagedBinary {
    pub path: PathBuf,
    pub version: String,
}

/// Maps the Rust target architecture to the release naming scheme.
pub fn detect_arch() -> Result<&'static str> {
    Ok(match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        "arm" => "arm-v7",
        "riscv64" => "riscv64",
        "loongarch64" => "loong64",
        "mips" if cfg!(target_endian = "little") => "mipsle-softfloat",
        "mips64" if cfg!(target_endian = "little") => "mips64le-softfloat",
        other => bail!("no sing-box release for architecture {other}; set update.arch"),
    })
}

pub fn asset_name(version: &str, arch: &str, variant: &str) -> String {
    let variant = variant.trim().trim_start_matches('-');
    if variant.is_empty() {
        format!("sing-box-{version}-linux-{arch}.tar.gz")
    } else {
        format!("sing-box-{version}-linux-{arch}-{variant}.tar.gz")
    }
}

/// Parses `sing-box version 1.14.1-xiaobaf14g.1` from `sing-box version` output.
pub fn parse_version_output(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("sing-box version "))
        .map(|version| version.trim().to_owned())
}

/// Parses `SHA256SUMS` lines of the form `<hex> *<file>` or `<hex>  <file>`.
pub fn parse_sha256sums(content: &str) -> HashMap<String, String> {
    content
        .lines()
        .filter_map(|line| {
            let (hash, name) = line.trim().split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*');
            Some((name.to_owned(), hash.to_ascii_lowercase()))
        })
        .collect()
}

pub struct Updater {
    config: UpdateConfig,
    http: reqwest::Client,
}

impl Updater {
    pub fn new(config: UpdateConfig) -> Result<Self> {
        crate::util::init_tls();
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("singbox-board/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60));
        builder = match config.proxy.as_deref().filter(|p| !p.is_empty()) {
            Some(proxy) => {
                builder.proxy(reqwest::Proxy::all(proxy).context("invalid update.proxy")?)
            }
            None => builder.no_proxy(),
        };
        Ok(Self {
            config,
            http: builder.build()?,
        })
    }

    fn arch(&self) -> Result<String> {
        match self.config.arch.as_deref().filter(|a| !a.is_empty()) {
            Some(arch) => Ok(arch.to_owned()),
            None => detect_arch().map(str::to_owned),
        }
    }

    fn download_url(&self, asset: &Asset) -> String {
        match self.config.mirror.as_deref().filter(|m| !m.is_empty()) {
            Some(mirror) => format!(
                "{}/{}",
                mirror.trim_end_matches('/'),
                asset.browser_download_url
            ),
            None => asset.browser_download_url.clone(),
        }
    }

    async fn api_get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!(
            "{}/repos/{}{path}",
            self.config.api_url.trim_end_matches('/'),
            self.config.repo
        );
        let mut request = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .timeout(Duration::from_secs(30));
        if let Some(token) = self
            .config
            .github_token
            .as_deref()
            .filter(|t| !t.is_empty())
        {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.with_context(|| format!("GET {url}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!(
                "GET {url}: HTTP {status}: {}",
                body.chars().take(200).collect::<String>()
            );
        }
        response
            .json()
            .await
            .with_context(|| format!("decode {url}"))
    }

    pub async fn fetch_release(&self, tag: Option<&str>) -> Result<Release> {
        if let Some(tag) = tag.filter(|t| !t.is_empty()) {
            let tag = if tag.starts_with('v') {
                tag.to_owned()
            } else {
                format!("v{tag}")
            };
            return self.api_get(&format!("/releases/tags/{tag}")).await;
        }
        if !self.config.prerelease {
            return self.api_get("/releases/latest").await;
        }
        let releases: Vec<Release> = self.api_get("/releases?per_page=20").await?;
        releases
            .into_iter()
            .find(|release| !release.draft)
            .ok_or_else(|| anyhow!("{} has no releases", self.config.repo))
    }

    pub async fn check(
        &self,
        tag: Option<&str>,
        current: Option<&str>,
    ) -> Result<(Release, UpdateInfo)> {
        let release = self.fetch_release(tag).await?;
        let asset = asset_name(release.version(), &self.arch()?, &self.config.variant);
        ensure!(
            release.asset(&asset).is_some(),
            "release {} has no asset {asset}",
            release.tag_name
        );
        let info = UpdateInfo {
            current: current.map(str::to_owned),
            latest: release.version().to_owned(),
            tag: release.tag_name.clone(),
            asset,
            prerelease: release.prerelease,
            published_at: release.published_at.clone(),
            update_available: current != Some(release.version()),
        };
        Ok((release, info))
    }

    async fn download(&self, asset: &Asset, limit: usize) -> Result<Vec<u8>> {
        let url = self.download_url(asset);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("download {}", asset.name))?;
            ensure!(
                data.len() + chunk.len() <= limit,
                "{} is larger than {limit} bytes",
                asset.name
            );
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }

    /// Downloads, verifies and extracts the release next to `binary` as
    /// `<binary>.new`. The caller renames it into place.
    pub async fn stage(
        &self,
        release: &Release,
        asset_name: &str,
        binary: &Path,
    ) -> Result<StagedBinary> {
        let asset = release
            .asset(asset_name)
            .ok_or_else(|| anyhow!("release {} has no asset {asset_name}", release.tag_name))?;
        let sums_asset = release.asset("SHA256SUMS").ok_or_else(|| {
            anyhow!(
                "release {} has no SHA256SUMS; refusing to install unverified binary",
                release.tag_name
            )
        })?;
        let sums = String::from_utf8(self.download(sums_asset, 1024 * 1024).await?)
            .context("SHA256SUMS is not UTF-8")?;
        let expected = parse_sha256sums(&sums)
            .remove(asset_name)
            .ok_or_else(|| anyhow!("SHA256SUMS has no entry for {asset_name}"))?;

        let archive = self.download(asset, MAX_ARCHIVE_BYTES).await?;
        let actual = hex::encode(Sha256::digest(&archive));
        ensure!(
            actual == expected,
            "checksum mismatch for {asset_name}: expected {expected}, got {actual}"
        );

        let dir = binary
            .parent()
            .ok_or_else(|| anyhow!("invalid binary path {}", binary.display()))?
            .to_path_buf();
        tokio::fs::create_dir_all(&dir).await?;
        let staged = with_suffix(binary, ".new");
        let staged_clone = staged.clone();
        tokio::task::spawn_blocking(move || extract_binary(&archive, &staged_clone))
            .await
            .context("extract task panicked")??;

        let output = tokio::process::Command::new(&staged)
            .arg("version")
            .output()
            .await
            .with_context(|| format!("run {} version", staged.display()))?;
        let version = parse_version_output(&String::from_utf8_lossy(&output.stdout));
        let Some(version) = version.filter(|_| output.status.success()) else {
            let _ = tokio::fs::remove_file(&staged).await;
            bail!("downloaded binary does not run on this system (wrong arch or variant?)");
        };
        Ok(StagedBinary {
            path: staged,
            version,
        })
    }
}

/// Writes the single executable contained in the archive to `dest`.
fn extract_binary(archive: &[u8], dest: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries().context("read archive")? {
        let mut entry = entry.context("read archive entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        let is_binary = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("sing-box"));
        if !is_binary {
            continue;
        }
        let mut data = Vec::new();
        entry
            .by_ref()
            .take(MAX_ARCHIVE_BYTES as u64)
            .read_to_end(&mut data)?;
        let _ = std::fs::remove_file(dest);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(dest)
            .with_context(|| format!("create {}", dest.display()))?;
        std::io::Write::write_all(&mut file, &data)?;
        file.sync_all()?;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
        return Ok(());
    }
    bail!("archive does not contain a sing-box executable")
}

/// `/usr/bin/sing-box` + `.new` -> `/usr/bin/sing-box.new` (unlike `with_extension`,
/// dots already in the file name are kept).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

/// Atomically replaces `binary` with the staged file, keeping `<binary>.bak`.
pub fn install(staged: &StagedBinary, binary: &Path) -> Result<()> {
    if binary.exists() {
        let backup = with_suffix(binary, ".bak");
        std::fs::copy(binary, &backup)
            .with_context(|| format!("back up {} to {}", binary.display(), backup.display()))?;
    }
    std::fs::rename(&staged.path, binary)
        .with_context(|| format!("move {} to {}", staged.path.display(), binary.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names() {
        assert_eq!(
            asset_name("1.14.1-xiaobaf14g.1", "amd64", ""),
            "sing-box-1.14.1-xiaobaf14g.1-linux-amd64.tar.gz"
        );
        assert_eq!(
            asset_name("1.14.1-xiaobaf14g.1", "amd64", "v3-ebpf"),
            "sing-box-1.14.1-xiaobaf14g.1-linux-amd64-v3-ebpf.tar.gz"
        );
    }

    #[test]
    fn version_output() {
        let out = "sing-box version 1.14.1-xiaobaf14g.1\n\nEnvironment: go1.26.8 linux/amd64\n";
        assert_eq!(parse_version_output(out).unwrap(), "1.14.1-xiaobaf14g.1");
        assert!(parse_version_output("garbage").is_none());
    }

    #[test]
    fn sha256sums() {
        let sums = parse_sha256sums(
            "e18bcc6c *sing-box-1.14.1-linux-amd64.tar.gz\nABCDEF  other.tar.gz\n",
        );
        assert_eq!(sums["sing-box-1.14.1-linux-amd64.tar.gz"], "e18bcc6c");
        assert_eq!(sums["other.tar.gz"], "abcdef");
    }

    #[test]
    fn extract_single_binary() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let content = b"#!/bin/sh\necho hi\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "sing-box-1.0.0-linux-amd64", &content[..])
            .unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();
        let dest = std::env::temp_dir().join(format!("sbb-extract-{}", std::process::id()));
        extract_binary(&archive, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), content);
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        std::fs::remove_file(dest).unwrap();
    }
}
