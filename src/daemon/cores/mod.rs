//! The core version store.
//!
//! Every installed sing-box build lives in its own directory,
//! `<data_dir>/cores/<source>/<tag>/<variant>/`, together with whatever the
//! release ships next to it (e.g. `libcronet.so`). `core.binary` is a symlink
//! to the active build, so switching versions is a single atomic rename and
//! sing-box still finds its companion libraries next to its real path.

mod archive;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use self::archive::{arch_tokens, match_assets};
use super::components::{random_token, write_atomic};
use super::github::{
    Asset, GitHub, Release, parse_sha256sums, parse_version_output, verify_sha256,
};
use super::logs::LogHub;
use crate::config::DaemonConfig;
use crate::i18n::{self, fl, fl_log};
use crate::protocol::{
    ADOPTED_NAME, BUILTIN_SOURCES, Checksum, CoreRelease, CoreReleasePage, CoreSource, CoreVariant,
    IMPORTED_NAME, StoredCore, builtin_source, core_store_id, variant_label,
};
use crate::util::{fmt_bytes, join_list, now_unix};

pub const LOCAL_SOURCE: &str = "local";
const PER_PAGE: u32 = 15;
const CACHE_TTL: Duration = Duration::from_secs(600);
const MAX_DOWNLOAD: usize = 512 * 1024 * 1024;
const SUMS_NAMES: [&str; 5] = [
    "SHA256SUMS",
    "SHA256SUMS.txt",
    "sha256sums.txt",
    "sha256sum.txt",
    "checksums.txt",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct SourcesFile {
    custom: Vec<CustomSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CustomSource {
    repo: String,
    name: String,
}

struct CachedPage {
    at: Instant,
    releases: Vec<Release>,
    has_more: bool,
}

pub struct CoreManager {
    config: DaemonConfig,
    logs: Arc<LogHub>,
    root: PathBuf,
    sources_path: PathBuf,
    custom: Mutex<Vec<CustomSource>>,
    cache: Mutex<HashMap<(String, u32), CachedPage>>,
    install_lock: Mutex<()>,
    busy: AtomicBool,
}

impl CoreManager {
    pub fn new(config: DaemonConfig, logs: Arc<LogHub>) -> Arc<Self> {
        let data_dir = config.components.data_dir.clone();
        let sources_path = data_dir.join("core-sources.json");
        let custom = match std::fs::read_to_string(&sources_path) {
            Ok(text) => serde_json::from_str::<SourcesFile>(&text)
                .map(|f| f.custom)
                .unwrap_or_else(|err| {
                    logs.warn(fl_log!(
                        "cores-sources-ignored",
                        path = sources_path.display().to_string(),
                        error = err.to_string()
                    ));
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        };
        Arc::new(Self {
            root: data_dir.join("cores"),
            sources_path,
            custom: Mutex::new(custom),
            cache: Mutex::new(HashMap::new()),
            install_lock: Mutex::new(()),
            busy: AtomicBool::new(false),
            config,
            logs,
        })
    }

    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }

    fn github(&self) -> Result<GitHub> {
        GitHub::new(&self.config.update)
    }

    // ----- sources ------------------------------------------------------

    pub async fn sources(&self) -> Vec<CoreSource> {
        let mut sources: Vec<CoreSource> = BUILTIN_SOURCES
            .iter()
            .filter_map(|id| {
                let (name, description) = builtin_source(id)?;
                Some(CoreSource {
                    id: (*id).to_owned(),
                    name,
                    description,
                    builtin: true,
                })
            })
            .collect();
        let configured = &self.config.update.repo;
        if !sources
            .iter()
            .any(|s| s.id.eq_ignore_ascii_case(configured))
        {
            sources.push(CoreSource {
                id: configured.clone(),
                name: configured.clone(),
                description: fl!("source-configured-description"),
                builtin: true,
            });
        }
        for custom in self.custom.lock().await.iter() {
            sources.push(CoreSource {
                id: custom.repo.clone(),
                name: custom.name.clone(),
                description: fl!("source-custom-description"),
                builtin: false,
            });
        }
        sources
    }

    async fn source(&self, id: &str) -> Result<CoreSource> {
        self.sources()
            .await
            .into_iter()
            .find(|s| s.id.eq_ignore_ascii_case(id))
            .ok_or_else(|| anyhow!(fl!("cores-unknown-source", id = id)))
    }

    pub async fn add_source(&self, repo: &str, name: Option<String>) -> Result<CoreSource> {
        let repo = repo
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_end_matches('/');
        let valid = repo.split('/').count() == 2
            && repo
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c));
        if !valid {
            bail!(fl!("cores-invalid-repo", repo = repo));
        }
        if self.source(repo).await.is_ok() {
            bail!(fl!("cores-source-exists", repo = repo));
        }
        self.github()?
            .check_repo(repo)
            .await
            .with_context(|| fl!("cores-lookup-failed", repo = repo))?;
        let name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| repo.to_owned());
        let mut custom = self.custom.lock().await;
        custom.push(CustomSource {
            repo: repo.to_owned(),
            name: name.clone(),
        });
        self.save_sources(&custom)?;
        self.logs
            .info(fl_log!("cores-source-added-log", repo = repo));
        Ok(CoreSource {
            id: repo.to_owned(),
            name,
            description: fl!("source-custom-description"),
            builtin: false,
        })
    }

    pub async fn remove_source(&self, id: &str) -> Result<()> {
        let mut custom = self.custom.lock().await;
        let before = custom.len();
        custom.retain(|s| !s.repo.eq_ignore_ascii_case(id));
        if custom.len() == before {
            bail!(fl!("cores-not-custom", id = id));
        }
        self.save_sources(&custom)?;
        self.logs.info(fl_log!("cores-source-removed-log", id = id));
        Ok(())
    }

    fn save_sources(&self, custom: &[CustomSource]) -> Result<()> {
        if let Some(dir) = self.sources_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = SourcesFile {
            custom: custom.to_vec(),
        };
        write_atomic(
            &self.sources_path,
            &serde_json::to_vec_pretty(&file)?,
            0o644,
        )
    }

    // ----- releases -----------------------------------------------------

    fn tokens(&self) -> Vec<String> {
        arch_tokens(self.config.update.arch.as_deref())
    }

    pub fn platform(&self) -> String {
        format!("linux-{}", self.tokens()[0])
    }

    async fn release_page(
        &self,
        repo: &str,
        page: u32,
        refresh: bool,
    ) -> Result<(Vec<Release>, bool)> {
        let key = (repo.to_ascii_lowercase(), page);
        if !refresh
            && let Some(cached) = self.cache.lock().await.get(&key)
            && cached.at.elapsed() < CACHE_TTL
        {
            return Ok((cached.releases.clone(), cached.has_more));
        }
        let (releases, has_more) = self.github()?.releases(repo, page, PER_PAGE).await?;
        self.cache.lock().await.insert(
            key,
            CachedPage {
                at: Instant::now(),
                releases: releases.clone(),
                has_more,
            },
        );
        Ok((releases, has_more))
    }

    pub async fn releases(
        &self,
        source: &str,
        page: u32,
        refresh: bool,
    ) -> Result<CoreReleasePage> {
        let source = self.source(source).await?;
        let page = page.max(1);
        let (releases, has_more) = self.release_page(&source.id, page, refresh).await?;
        let tokens = self.tokens();
        Ok(CoreReleasePage {
            source: source.id,
            platform: self.platform(),
            page,
            has_more,
            releases: releases.iter().map(|r| describe(r, &tokens)).collect(),
        })
    }

    /// A release by tag (with or without the `v` prefix), or the newest one.
    pub async fn release(&self, source: &str, tag: Option<&str>) -> Result<Release> {
        let github = self.github()?;
        let Some(tag) = tag.filter(|t| !t.is_empty()) else {
            return github
                .release(source, None, self.config.update.prerelease)
                .await;
        };
        let found = match github.release(source, Some(tag), false).await {
            Ok(release) => Ok(release),
            Err(err) if !tag.starts_with('v') => github
                .release(source, Some(&format!("v{tag}")), false)
                .await
                .map_err(|_| err),
            Err(err) => Err(err),
        };
        found.map_err(|err| {
            if err.to_string().contains("404") {
                anyhow!(fl!("cores-no-release", source = source, tag = tag))
            } else {
                err
            }
        })
    }

    // ----- store --------------------------------------------------------

    fn dir_of(&self, id: &str) -> Result<PathBuf> {
        let valid = id.split('/').count() == 3
            && id
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./+".contains(c));
        if !valid {
            bail!(fl!("cores-invalid-id", id = id));
        }
        Ok(self.root.join(id))
    }

    pub fn binary_of(&self, core: &StoredCore) -> PathBuf {
        self.root.join(&core.id).join("sing-box")
    }

    pub fn load(&self, id: &str) -> Option<StoredCore> {
        let dir = self.dir_of(id).ok()?;
        let text = std::fs::read_to_string(dir.join("meta.json")).ok()?;
        let mut core: StoredCore = serde_json::from_str(&text).ok()?;
        core.id = id.to_owned();
        core.active = false;
        dir.join("sing-box").is_file().then_some(core)
    }

    /// The stored core `core.binary` links to.
    pub fn active(&self) -> Option<StoredCore> {
        let target = std::fs::read_link(&self.config.core.binary).ok()?;
        let relative = target.parent()?.strip_prefix(&self.root).ok()?;
        let mut core = self.load(relative.to_str()?)?;
        core.active = true;
        Some(core)
    }

    pub fn installed(&self) -> Vec<StoredCore> {
        let active = self.active().map(|c| c.id);
        let mut cores = Vec::new();
        let level = |dir: &Path| -> Vec<PathBuf> {
            std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.is_dir()
                                && !p
                                    .file_name()
                                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        for source in level(&self.root) {
            for tag in level(&source) {
                for variant in level(&tag) {
                    let Some(id) = variant
                        .strip_prefix(&self.root)
                        .ok()
                        .and_then(|p| p.to_str())
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    if let Some(mut core) = self.load(&id) {
                        core.active = active.as_deref() == Some(id.as_str());
                        cores.push(core);
                    }
                }
            }
        }
        cores.sort_by(|a, b| b.installed_at.cmp(&a.installed_at).then(a.id.cmp(&b.id)));
        cores
    }

    /// Downloads a release build into the store (or returns it if present).
    pub async fn install(&self, source: &str, tag: &str, variant: &str) -> Result<StoredCore> {
        let source = self.source(source).await?;
        let id = store_id(&source.id, tag, variant);
        if let Some(core) = self.load(&id) {
            return Ok(core);
        }
        let _guard = self.install_lock.lock().await;
        if let Some(core) = self.load(&id) {
            return Ok(core);
        }
        let _busy = BusyFlag::set(&self.busy);
        let github = self.github()?;
        let release = self.release(&source.id, Some(tag)).await?;
        let tokens = self.tokens();
        let builds = match_assets(&release.assets, &tokens);
        let Some((_, asset)) = builds.iter().find(|(v, _)| v == variant) else {
            let available: Vec<String> = builds.iter().map(|(v, _)| variant_label(v)).collect();
            bail!(fl!(
                "cores-no-variant",
                source = source.id.clone(),
                tag = release.tag_name.clone(),
                platform = self.platform(),
                variant = variant_label(variant),
                available = if available.is_empty() {
                    fl!("none")
                } else {
                    join_list(&available)
                }
            ));
        };
        self.logs.info(fl_log!(
            "cores-downloading",
            asset = asset.name.clone(),
            size = fmt_bytes(asset.size),
            source = source.id.clone()
        ));
        let data = github.download(asset, MAX_DOWNLOAD).await?;
        let checksum = verify_release_asset(&github, &release, asset, &data).await?;
        if checksum == Checksum::None {
            self.logs.warn(fl_log!(
                "cores-no-checksum",
                source = source.id.clone(),
                asset = asset.name.clone()
            ));
        }
        let meta = StoredCore {
            id: id.clone(),
            source: source.id.clone(),
            source_name: source.name.clone(),
            version: String::new(),
            tag: Some(release.tag_name.clone()),
            variant: variant.to_owned(),
            size: data.len() as u64,
            sha256: hex::encode(Sha256::digest(&data)),
            checksum,
            installed_at: now_unix(),
            files: Vec::new(),
            active: false,
        };
        self.store(&asset.name, data, meta).await
    }

    /// Stores a custom core from a local path or an http(s) URL.
    pub async fn import(&self, location: &str, sha256: Option<&str>) -> Result<StoredCore> {
        let _guard = self.install_lock.lock().await;
        let _busy = BusyFlag::set(&self.busy);
        let location = location.trim();
        let (name, data) = if location.starts_with("http://") || location.starts_with("https://") {
            self.logs
                .info(fl_log!("cores-import-downloading", location = location));
            let data = self.github()?.fetch(location, MAX_DOWNLOAD).await?;
            let name = location
                .split(['?', '#'])
                .next()
                .and_then(|path| path.rsplit('/').next())
                .unwrap_or("sing-box")
                .to_owned();
            (name, data)
        } else {
            let path = Path::new(location);
            if !path.is_absolute() {
                bail!(fl!("cores-import-location"));
            }
            let data = tokio::fs::read(path)
                .await
                .with_context(|| fl!("err-read", path = path.display().to_string()))?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("sing-box")
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
        self.store_local(&name, data, IMPORTED_NAME, checksum).await
    }

    /// Keeps a hand-placed binary at `binary` selectable before it is replaced by a symlink.
    pub async fn adopt(&self, binary: &Path) -> Result<Option<StoredCore>> {
        match std::fs::symlink_metadata(binary) {
            Ok(meta) if meta.file_type().is_file() => {}
            _ => return Ok(None),
        }
        let data = tokio::fs::read(binary)
            .await
            .with_context(|| fl!("err-read", path = binary.display().to_string()))?;
        let core = self
            .store_local("sing-box", data, ADOPTED_NAME, Checksum::None)
            .await?;
        self.logs.info(fl_log!(
            "cores-adopted",
            path = binary.display().to_string(),
            version = core.version.clone(),
            id = core.id.clone()
        ));
        Ok(Some(core))
    }

    async fn store_local(
        &self,
        name: &str,
        data: Vec<u8>,
        label: &str,
        checksum: Checksum,
    ) -> Result<StoredCore> {
        let sha256 = hex::encode(Sha256::digest(&data));
        let id = format!("{LOCAL_SOURCE}/{}/default", &sha256[..12]);
        if let Some(core) = self.load(&id) {
            return Ok(core);
        }
        let meta = StoredCore {
            id,
            source: LOCAL_SOURCE.to_owned(),
            source_name: label.to_owned(),
            version: String::new(),
            tag: None,
            variant: String::new(),
            size: data.len() as u64,
            sha256,
            checksum,
            installed_at: now_unix(),
            files: Vec::new(),
            active: false,
        };
        self.store(name, data, meta).await
    }

    /// Unpacks into a staging directory, proves the binary runs, then moves it into place.
    async fn store(&self, name: &str, data: Vec<u8>, mut meta: StoredCore) -> Result<StoredCore> {
        let final_dir = self.dir_of(&meta.id)?;
        let staging = self.root.join(format!(".staging-{}", random_token(8)));
        let name_owned = name.to_owned();
        let staging_clone = staging.clone();
        let lang = i18n::current();
        let unpacked = tokio::task::spawn_blocking(move || {
            i18n::in_language(lang, || archive::unpack(&name_owned, &data, &staging_clone))
        })
        .await
        .context(fl!("err-task-panicked"))?;
        let files = match unpacked {
            Ok(files) => files,
            Err(err) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(err);
            }
        };
        let version = match core_version(&staging.join("sing-box")).await {
            Ok(version) => version,
            Err(err) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(err.context(fl!("cores-not-runnable", name = name)));
            }
        };
        meta.version = version;
        meta.files = files;
        std::fs::write(staging.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
        if let Some(parent) = final_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_dir_all(&final_dir);
        std::fs::rename(&staging, &final_dir)
            .with_context(|| fl!("cores-move-failed", path = final_dir.display().to_string()))?;
        self.logs.info(fl_log!(
            "cores-stored-log",
            version = meta.version.clone(),
            id = meta.id.clone(),
            size = fmt_bytes(meta.size),
            checksum = i18n::in_log_language(|| meta.checksum.description())
        ));
        Ok(meta)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let dir = self.dir_of(id)?;
        if !dir.join("meta.json").is_file() {
            bail!(fl!("cores-not-stored", id = id));
        }
        if self.active().is_some_and(|c| c.id == id) {
            bail!(fl!("cores-remove-active", id = id));
        }
        std::fs::remove_dir_all(&dir)
            .with_context(|| fl!("err-delete", path = dir.display().to_string()))?;
        // Drop now-empty tag/source directories.
        for parent in dir.ancestors().skip(1).take(2) {
            if parent.starts_with(&self.root) && parent != self.root {
                let _ = std::fs::remove_dir(parent);
            }
        }
        self.logs.info(fl_log!("cores-deleted-log", id = id));
        Ok(())
    }

    /// Source and variant `singbox-board update` follows: the active core's,
    /// falling back to `[update]` in daemon.toml.
    pub fn update_target(&self) -> (String, String) {
        match self.active() {
            Some(core) if core.source != LOCAL_SOURCE => (core.source, core.variant),
            _ => (
                self.config.update.repo.clone(),
                self.config
                    .update
                    .variant
                    .trim()
                    .trim_start_matches('-')
                    .to_owned(),
            ),
        }
    }

    /// The asset name of `variant` in `release`, for update reports.
    pub fn asset_for(&self, release: &Release, variant: &str) -> Option<String> {
        match_assets(&release.assets, &self.tokens())
            .into_iter()
            .find(|(v, _)| v == variant)
            .map(|(_, a)| a.name.clone())
    }
}

/// Clears the busy flag when an install ends, however it ends.
struct BusyFlag<'a>(&'a AtomicBool);

impl<'a> BusyFlag<'a> {
    fn set(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::Relaxed);
        Self(flag)
    }
}

impl Drop for BusyFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

fn describe(release: &Release, tokens: &[String]) -> CoreRelease {
    let sums = sums_asset(release).is_some();
    CoreRelease {
        tag: release.tag_name.clone(),
        version: release.version().to_owned(),
        published_at: release.published_at.clone(),
        prerelease: release.prerelease,
        variants: match_assets(&release.assets, tokens)
            .into_iter()
            .map(|(name, asset)| CoreVariant {
                name,
                asset: asset.name.clone(),
                size: asset.size,
                checksum: if sums {
                    Checksum::Sums
                } else if asset.sha256().is_some() {
                    Checksum::Digest
                } else {
                    Checksum::None
                },
            })
            .collect(),
    }
}

fn sums_asset(release: &Release) -> Option<&Asset> {
    SUMS_NAMES.iter().find_map(|name| release.asset(name))
}

/// Checks the download against the release's checksum file when there is
/// one; `GitHub::download` already compared it with the asset digest.
async fn verify_release_asset(
    github: &GitHub,
    release: &Release,
    asset: &Asset,
    data: &[u8],
) -> Result<Checksum> {
    if let Some(sums) = sums_asset(release) {
        let text = String::from_utf8(github.download(sums, 4 * 1024 * 1024).await?)
            .with_context(|| fl!("cores-sums-not-text", name = sums.name.clone()))?;
        if let Some(expected) = parse_sha256sums(&text).remove(&asset.name) {
            verify_sha256(data, &expected, &asset.name)?;
            return Ok(Checksum::Sums);
        }
    }
    Ok(if asset.sha256().is_some() {
        Checksum::Digest
    } else {
        Checksum::None
    })
}

async fn core_version(binary: &Path) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(binary)
            .arg("version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!(fl!("cores-version-timeout")))??;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_version_output(&stdout) {
        Some(version) if output.status.success() => Ok(version),
        _ => bail!(fl!(
            "cores-version-failed",
            error = String::from_utf8_lossy(&output.stderr).trim().to_owned()
        )),
    }
}

pub fn store_id(source: &str, tag: &str, variant: &str) -> String {
    core_store_id(source, tag, variant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_path_safe() {
        assert_eq!(
            store_id("MiChongs/sing-box", "v1.14.1-xiaobaf14g.1", ""),
            "MiChongs_sing-box/v1.14.1-xiaobaf14g.1/default"
        );
        assert_eq!(
            store_id("SagerNet/sing-box", "v1.12.0", "v3-glibc"),
            "SagerNet_sing-box/v1.12.0/v3-glibc"
        );
        assert_eq!(store_id("../x y", "v1", ""), ".._x_y/v1/default");
    }

    fn manager(name: &str) -> (Arc<CoreManager>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sbb-cores-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = DaemonConfig::default();
        config.components.data_dir = dir.clone();
        config.core.binary = dir.join("bin/sing-box");
        let logs = Arc::new(LogHub::new(100, false));
        (CoreManager::new(config, logs), dir)
    }

    fn fake_core(version: &str) -> Vec<u8> {
        format!("#!/bin/sh\necho 'sing-box version {version}'\n").into_bytes()
    }

    #[tokio::test]
    async fn import_adopt_activate_remove() {
        let (cores, dir) = manager("flow");
        assert!(cores.dir_of("../../etc/x").is_err());
        assert!(cores.dir_of("a/b").is_err());

        // A hand-placed binary is adopted into the store.
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let binary = dir.join("bin/sing-box");
        std::fs::write(&binary, fake_core("1.0.0-custom")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let adopted = cores.adopt(&binary).await.unwrap().unwrap();
        assert_eq!(adopted.version, "1.0.0-custom");
        assert_eq!(adopted.source, LOCAL_SOURCE);

        // Import from a path, pinned checksum.
        let file = dir.join("my-core");
        std::fs::write(&file, fake_core("2.0.0")).unwrap();
        let wrong = cores.import(file.to_str().unwrap(), Some("00")).await;
        assert!(wrong.is_err());
        let sha = hex::encode(Sha256::digest(fake_core("2.0.0")));
        let imported = cores
            .import(file.to_str().unwrap(), Some(&sha))
            .await
            .unwrap();
        assert_eq!(imported.checksum, Checksum::Pinned);
        assert_eq!(cores.installed().len(), 2);

        // Activate = symlink to the stored binary.
        std::fs::remove_file(&binary).unwrap();
        std::os::unix::fs::symlink(cores.binary_of(&imported), &binary).unwrap();
        assert_eq!(cores.active().unwrap().id, imported.id);
        assert!(cores.remove(&imported.id).is_err());
        cores.remove(&adopted.id).unwrap();
        assert_eq!(cores.installed().len(), 1);
        assert!(cores.installed()[0].active);
        std::fs::remove_dir_all(dir).unwrap();
    }

    use std::os::unix::fs::PermissionsExt;
}
