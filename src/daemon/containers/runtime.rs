//! The kurumi-containerd executable: which one is used, and downloading
//! releases of it.
//!
//! Releases publish `kurumi-containerd-<version>-linux-<target>.tar.xz`
//! with a `SHA256SUMS` file. Static musl builds exist for x86_64 and
//! aarch64; armv7 and riscv64 only have glibc builds. A downloaded runtime
//! lives in `<containers>/runtime/kurumi-containerd` and is replaced by an
//! atomic rename, so running containers (whose monitors were forked from
//! the old binary) are not affected by an update.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::daemon::github::{Asset, GitHub, Release, parse_sha256sums, verify_sha256};
use crate::i18n::fl;
use crate::protocol::{Checksum, RuntimeOrigin};
use crate::util::{find_program, now_unix, random_token, write_atomic};

pub const BINARY: &str = "kurumi-containerd";
const MAX_DOWNLOAD: usize = 64 * 1024 * 1024;
const MAX_BINARY: u64 = 128 * 1024 * 1024;

/// What is known about a runtime the daemon downloaded or imported.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Meta {
    pub version: String,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub asset: Option<String>,
    pub sha256: String,
    #[serde(default)]
    pub checksum: Checksum,
    #[serde(default)]
    pub imported: bool,
    pub installed_at: u64,
}

/// The executable in use and where it comes from.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub binary: PathBuf,
    pub origin: RuntimeOrigin,
    pub meta: Option<Meta>,
}

/// Release target of this machine, e.g. `x86_64-unknown-linux-musl`.
/// glibc builds are only offered where the host has the glibc loader.
pub fn release_target() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64-unknown-linux-musl"),
        "aarch64" => Some("aarch64-unknown-linux-musl"),
        "arm" if Path::new("/lib/ld-linux-armhf.so.3").exists() => {
            Some("armv7-unknown-linux-gnueabihf")
        }
        "riscv64" if Path::new("/lib/ld-linux-riscv64-lp64d.so.1").exists() => {
            Some("riscv64gc-unknown-linux-gnu")
        }
        _ => None,
    }
}

/// The release asset built for `target`.
pub fn asset_for<'a>(release: &'a Release, target: &str) -> Option<&'a Asset> {
    let suffix = format!("-linux-{target}.tar.xz");
    release
        .assets
        .iter()
        .find(|asset| asset.name.starts_with("kurumi-containerd-") && asset.name.ends_with(&suffix))
}

pub struct RuntimeStore {
    dir: PathBuf,
    configured: Option<PathBuf>,
}

impl RuntimeStore {
    pub fn new(dir: PathBuf, configured: Option<PathBuf>) -> Self {
        Self { dir, configured }
    }

    pub fn binary_path(&self) -> PathBuf {
        self.dir.join(BINARY)
    }

    fn meta_path(&self) -> PathBuf {
        self.dir.join("meta.json")
    }

    pub fn meta(&self) -> Option<Meta> {
        let text = std::fs::read_to_string(self.meta_path()).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// `containers.runtime`, else the stored download, else PATH.
    pub fn resolve(&self) -> Option<Resolved> {
        if let Some(path) = &self.configured {
            return Some(Resolved {
                binary: path.clone(),
                origin: RuntimeOrigin::Configured,
                meta: None,
            });
        }
        let stored = self.binary_path();
        if stored.is_file() {
            let meta = self.meta();
            let origin = match &meta {
                Some(meta) if meta.imported => RuntimeOrigin::Imported,
                _ => RuntimeOrigin::Release,
            };
            return Some(Resolved {
                binary: stored,
                origin,
                meta,
            });
        }
        find_program(BINARY).map(|binary| Resolved {
            binary,
            origin: RuntimeOrigin::System,
            meta: None,
        })
    }

    /// Downloads `release`'s build for this machine and makes it the stored
    /// runtime. Returns the stored metadata.
    pub async fn install_release(&self, github: &GitHub, release: &Release) -> Result<Meta> {
        let target = release_target().ok_or_else(|| {
            anyhow!(fl!(
                "containers-runtime-unsupported",
                arch = std::env::consts::ARCH
            ))
        })?;
        let asset = asset_for(release, target).ok_or_else(|| {
            anyhow!(fl!(
                "containers-runtime-no-asset",
                tag = release.tag_name.clone(),
                target = target
            ))
        })?;
        let data = github.download(asset, MAX_DOWNLOAD).await?;
        let mut checksum = if asset.sha256().is_some() {
            Checksum::Digest
        } else {
            Checksum::None
        };
        if let Some(sums) = release.asset("SHA256SUMS") {
            let text = String::from_utf8(github.download(sums, 1024 * 1024).await?)
                .context(fl!("cores-sums-not-text", name = sums.name.clone()))?;
            if let Some(expected) = parse_sha256sums(&text).remove(&asset.name) {
                verify_sha256(&data, &expected, &asset.name)?;
                checksum = Checksum::Sums;
            }
        }
        let name = asset.name.clone();
        let lang = crate::i18n::current();
        let binary = tokio::task::spawn_blocking(move || {
            crate::i18n::in_language(lang, || extract(&name, &data))
        })
        .await
        .context(fl!("err-task-panicked"))??;
        let meta = Meta {
            version: String::new(),
            tag: Some(release.tag_name.clone()),
            asset: Some(asset.name.clone()),
            sha256: String::new(),
            checksum,
            imported: false,
            installed_at: now_unix(),
        };
        self.store(binary, meta).await
    }

    /// Stores a build from a local path or an http(s) URL: a binary or an
    /// archive containing `kurumi-containerd`.
    pub async fn import(
        &self,
        github: &GitHub,
        location: &str,
        sha256: Option<&str>,
    ) -> Result<Meta> {
        let location = location.trim();
        let (name, data) = if location.starts_with("http://") || location.starts_with("https://") {
            let data = github.fetch(location, MAX_DOWNLOAD).await?;
            let name = location
                .split(['?', '#'])
                .next()
                .and_then(|path| path.rsplit('/').next())
                .unwrap_or(BINARY)
                .to_owned();
            (name, data)
        } else {
            let path = Path::new(location);
            if !path.is_absolute() {
                bail!(fl!("containers-runtime-import-location"));
            }
            let data = tokio::fs::read(path)
                .await
                .with_context(|| fl!("err-read", path = path.display().to_string()))?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(BINARY)
                .to_owned();
            (name, data)
        };
        let checksum = match sha256.map(str::trim).filter(|s| !s.is_empty()) {
            Some(expected) => {
                verify_sha256(&data, expected, &name)?;
                Checksum::Pinned
            }
            None => Checksum::None,
        };
        let lang = crate::i18n::current();
        let binary = tokio::task::spawn_blocking(move || {
            crate::i18n::in_language(lang, || extract(&name, &data))
        })
        .await
        .context(fl!("err-task-panicked"))??;
        let meta = Meta {
            version: String::new(),
            tag: None,
            asset: None,
            sha256: String::new(),
            checksum,
            imported: true,
            installed_at: now_unix(),
        };
        self.store(binary, meta).await
    }

    /// Writes the binary next to its final place, proves it runs, then
    /// renames both the binary and its metadata into place.
    async fn store(&self, binary: Vec<u8>, mut meta: Meta) -> Result<Meta> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| fl!("err-create", path = self.dir.display().to_string()))?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o755))?;
        let staging = self.dir.join(format!(".{BINARY}-{}", random_token(8)));
        std::fs::write(&staging, &binary)
            .with_context(|| fl!("err-create", path = staging.display().to_string()))?;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))?;
        let version = match version(&staging).await {
            Ok(version) => version,
            Err(err) => {
                let _ = std::fs::remove_file(&staging);
                return Err(err.context(fl!("containers-runtime-not-runnable")));
            }
        };
        meta.version = version;
        meta.sha256 = hex::encode(Sha256::digest(&binary));
        let target = self.binary_path();
        std::fs::rename(&staging, &target).inspect_err(|_| {
            let _ = std::fs::remove_file(&staging);
        })?;
        write_atomic(&self.meta_path(), &serde_json::to_vec_pretty(&meta)?, 0o644)?;
        Ok(meta)
    }
}

/// `kurumi-containerd --version`: "kurumi-containerd 0.2.3" → "0.2.3".
pub async fn version(binary: &Path) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(binary)
            .arg("--version")
            .env("NO_COLOR", "1")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!(fl!("containers-runtime-version-timeout")))?
    .with_context(|| fl!("err-spawn", path = binary.display().to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_version(&stdout) {
        Some(version) if output.status.success() => Ok(version),
        _ => bail!(fl!(
            "containers-runtime-version-failed",
            error = String::from_utf8_lossy(&output.stderr).trim().to_owned()
        )),
    }
}

pub fn parse_version(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("kurumi-containerd "))
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// The `kurumi-containerd` executable from a release archive (`.tar.xz`,
/// `.tar.gz`, `.zip`) or a bare binary.
pub fn extract(name: &str, data: &[u8]) -> Result<Vec<u8>> {
    let lower = name.to_ascii_lowercase();
    if data.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        let reader = lzma_rust2::XzReader::new(data, true);
        return from_tar(name, reader);
    }
    if data.starts_with(&[0x1f, 0x8b]) {
        return from_tar(name, flate2::read::GzDecoder::new(data));
    }
    if data.starts_with(b"PK\x03\x04") || lower.ends_with(".zip") {
        let mut zip =
            zip::ZipArchive::new(std::io::Cursor::new(data)).context(fl!("err-open-zip"))?;
        for i in 0..zip.len() {
            let mut file = zip.by_index(i)?;
            let is_binary = file
                .enclosed_name()
                .and_then(|p| p.file_name().map(|n| n == BINARY))
                .unwrap_or(false);
            if file.is_file() && is_binary {
                return read_limited(&mut file, name);
            }
        }
        bail!(fl!("err-archive-missing", name = BINARY));
    }
    if data.starts_with(b"\x7fELF") {
        return Ok(data.to_vec());
    }
    bail!(fl!("containers-runtime-not-binary", name = name))
}

fn from_tar(name: &str, reader: impl Read) -> Result<Vec<u8>> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().context(fl!("archive-read-failed"))? {
        let mut entry = entry.context(fl!("archive-read-failed"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        if path.file_name().is_some_and(|n| n == BINARY) {
            return read_limited(&mut entry, name);
        }
    }
    bail!(fl!("err-archive-missing", name = BINARY))
}

fn read_limited(reader: &mut dyn Read, name: &str) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    reader.take(MAX_BINARY + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_BINARY {
        bail!(fl!(
            "archive-too-large",
            name = name,
            limit = crate::util::fmt_bytes(MAX_BINARY)
        ));
    }
    if !data.starts_with(b"\x7fELF") {
        bail!(fl!("containers-runtime-not-binary", name = name));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_owned(),
            browser_download_url: format!("https://example.com/{name}"),
            digest: None,
            size: 1,
        }
    }

    #[test]
    fn release_assets_by_target() {
        let release = Release {
            tag_name: "v0.2.3".to_owned(),
            prerelease: false,
            draft: false,
            published_at: None,
            assets: [
                "kurumi-containerd-0.2.3-android-aarch64-linux-android.tar.xz",
                "kurumi-containerd-0.2.3-linux-aarch64-unknown-linux-gnu.deb",
                "kurumi-containerd-0.2.3-linux-aarch64-unknown-linux-gnu.tar.xz",
                "kurumi-containerd-0.2.3-linux-aarch64-unknown-linux-musl.tar.xz",
                "kurumi-containerd-0.2.3-linux-x86_64-unknown-linux-musl.tar.xz",
                "SHA256SUMS",
            ]
            .into_iter()
            .map(asset)
            .collect(),
        };
        assert_eq!(
            asset_for(&release, "aarch64-unknown-linux-musl")
                .unwrap()
                .name,
            "kurumi-containerd-0.2.3-linux-aarch64-unknown-linux-musl.tar.xz"
        );
        assert!(asset_for(&release, "riscv64gc-unknown-linux-gnu").is_none());
        assert_eq!(
            parse_version("kurumi-containerd 0.2.3\n").as_deref(),
            Some("0.2.3")
        );
        assert!(parse_version("something else").is_none());
    }

    fn tar_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *content).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn binaries_come_out_of_release_archives() {
        let elf: &[u8] = b"\x7fELF kurumi";
        let tar = tar_with(&[
            (
                "kurumi-containerd-0.2.3-linux-x86_64-unknown-linux-musl/LICENSE",
                b"GPL",
            ),
            (
                "kurumi-containerd-0.2.3-linux-x86_64-unknown-linux-musl/kurumi-containerd",
                elf,
            ),
        ]);
        let mut xz =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::default()).unwrap();
        std::io::Write::write_all(&mut xz, &tar).unwrap();
        let xz = xz.finish().unwrap();
        assert_eq!(extract("k.tar.xz", &xz).unwrap(), elf);

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        assert_eq!(extract("k.tar.gz", &gz.finish().unwrap()).unwrap(), elf);

        assert_eq!(extract("kurumi-containerd", elf).unwrap(), elf);
        assert!(extract("notes.txt", b"hello").is_err());
        let without = tar_with(&[("README.md", b"hi")]);
        assert!(extract("x.tar", &without).is_err());
    }
}
