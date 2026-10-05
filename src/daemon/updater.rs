//! Installs and updates the sing-box binary from GitHub releases.
//!
//! Release archives are named `sing-box-<version>-linux-<arch>[-<variant>].tar.gz`
//! and contain a single executable; every release ships a `SHA256SUMS` file.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};

use super::github::{GitHub, Release, verify_sha256};
use crate::config::UpdateConfig;
use crate::protocol::UpdateInfo;

pub const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;

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
    github: GitHub,
}

impl Updater {
    pub fn new(config: UpdateConfig) -> Result<Self> {
        Ok(Self {
            github: GitHub::new(&config)?,
            config,
        })
    }

    fn arch(&self) -> Result<String> {
        match self.config.arch.as_deref().filter(|a| !a.is_empty()) {
            Some(arch) => Ok(arch.to_owned()),
            None => detect_arch().map(str::to_owned),
        }
    }

    pub async fn fetch_release(&self, tag: Option<&str>) -> Result<Release> {
        // sing-box tags always carry the `v` prefix.
        let tag = tag.filter(|t| !t.is_empty()).map(|t| {
            if t.starts_with('v') {
                t.to_owned()
            } else {
                format!("v{t}")
            }
        });
        self.github
            .release(&self.config.repo, tag.as_deref(), self.config.prerelease)
            .await
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

    /// Downloads, verifies and extracts the release next to `binary` as
    /// `<binary>.new`. The caller renames it into place.
    pub async fn stage(
        &self,
        release: &Release,
        asset_name: &str,
        binary: &Path,
    ) -> Result<StagedBinary> {
        let asset = release.require(asset_name)?;
        let sums_asset = release.asset("SHA256SUMS").ok_or_else(|| {
            anyhow!(
                "release {} has no SHA256SUMS; refusing to install unverified binary",
                release.tag_name
            )
        })?;
        let sums = String::from_utf8(self.github.download(sums_asset, 1024 * 1024).await?)
            .context("SHA256SUMS is not UTF-8")?;
        let expected = parse_sha256sums(&sums)
            .remove(asset_name)
            .ok_or_else(|| anyhow!("SHA256SUMS has no entry for {asset_name}"))?;

        let archive = self.github.download(asset, MAX_ARCHIVE_BYTES).await?;
        verify_sha256(&archive, &expected, asset_name)?;

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
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
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
