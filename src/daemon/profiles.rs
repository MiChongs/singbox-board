//! The configuration profile store.
//!
//! Every configuration lives in `<data_dir>/profiles/<id>.json`, described by
//! `index.json`. The first `core.config` file is a symlink to the active
//! profile, so switching is a single atomic rename and sing-box keeps reading
//! the path it always did. A configuration that was in place before the first
//! switch is moved into the store first, like a hand-placed core binary.
//!
//! Remote profiles are downloaded from a URL and refreshed on a schedule. The
//! active profile only ever changes to content that `sing-box check` accepts
//! (unless forced), and sing-box is reloaded afterwards.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use reqwest::Url;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use super::logs::LogHub;
use super::supervisor::{CoreLabel, Op, SupervisorHandle, run_check};
use crate::config::DaemonConfig;
use crate::i18n::{self, fl, fl_log};
use crate::profile::{self, MAX_PROFILE_BYTES};
use crate::protocol::{CoreState, Profile, ProfileList, ProfileUsage, Response};
use crate::util::{error_chain, fmt_bytes, now_unix, point_symlink, random_token, write_atomic};

const INDEX: &str = "index.json";
/// Wait after a failed automatic download before the next attempt.
const RETRY_AFTER: u64 = 30 * 60;
/// Minutes between downloads when neither the user nor the provider says.
pub const DEFAULT_INTERVAL: u64 = 24 * 60;

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    profiles: Vec<Profile>,
}

/// A downloaded remote profile and what its provider said about it.
#[derive(Debug, Default)]
struct Download {
    content: String,
    usage: Option<ProfileUsage>,
    /// Minutes, from `profile-update-interval` (given in hours).
    interval: Option<u64>,
    /// From `content-disposition`.
    filename: Option<String>,
}

/// Outcome of `sing-box check` against a stored file.
enum Check {
    Passed,
    Failed(anyhow::Error),
    /// No sing-box binary to check with.
    Skipped,
}

pub struct ProfileManager {
    config: DaemonConfig,
    logs: Arc<LogHub>,
    supervisor: SupervisorHandle,
    root: PathBuf,
    /// Never held across an await: the daemon runs on a single thread.
    index: Mutex<Vec<Profile>>,
    /// Serialises changes to profile files, which may wait for sing-box.
    ops: tokio::sync::Mutex<()>,
    /// Unix seconds of the last automatic download attempt per profile.
    attempts: Mutex<HashMap<String, u64>>,
}

impl ProfileManager {
    pub fn new(config: DaemonConfig, logs: Arc<LogHub>, supervisor: SupervisorHandle) -> Arc<Self> {
        let root = config.components.data_dir.join("profiles");
        let path = root.join(INDEX);
        let profiles = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<IndexFile>(&text)
                .map(|file| file.profiles)
                .unwrap_or_else(|err| {
                    logs.warn(fl_log!(
                        "profiles-index-ignored",
                        path = path.display().to_string(),
                        error = err.to_string()
                    ));
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        };
        Arc::new(Self {
            config,
            logs,
            supervisor,
            root,
            index: Mutex::new(profiles),
            ops: tokio::sync::Mutex::new(()),
            attempts: Mutex::new(HashMap::new()),
        })
    }

    fn file(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    fn entries(&self) -> Vec<Profile> {
        self.index.lock().expect("profile index").clone()
    }

    /// Applies `change` to the entry `id` and saves the index.
    fn modify(&self, id: &str, change: impl FnOnce(&mut Profile)) -> Result<Profile> {
        let mut index = self.index.lock().expect("profile index");
        let entry = index
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| anyhow!(fl!("profiles-not-found", query = id)))?;
        change(entry);
        let updated = entry.clone();
        self.save_index(&index)?;
        Ok(updated)
    }

    fn save_index(&self, profiles: &[Profile]) -> Result<()> {
        self.ensure_root()?;
        let file = IndexFile {
            profiles: profiles
                .iter()
                .cloned()
                .map(|mut p| {
                    p.active = false;
                    p
                })
                .collect(),
        };
        write_atomic(
            &self.root.join(INDEX),
            &serde_json::to_vec_pretty(&file)?,
            0o600,
        )
    }

    /// Profiles hold credentials, so the store is readable by root only.
    fn ensure_root(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| fl!("err-create", path = self.root.display().to_string()))?;
        std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn write_content(&self, path: &Path, content: &str) -> Result<()> {
        self.ensure_root()?;
        write_atomic(path, content.as_bytes(), 0o600)
    }

    /// Id of the profile the configuration file links to.
    fn active_id(&self) -> Option<String> {
        let slot = self.config.core.config_slot()?;
        let target = std::fs::read_link(&slot).ok()?;
        let target = match target.is_relative() {
            true => slot.parent()?.join(target),
            false => target,
        };
        if target.parent()? != self.root || target.extension()? != "json" {
            return None;
        }
        let id = target.file_stem()?.to_str()?;
        valid_id(id).then(|| id.to_owned())
    }

    pub fn list(&self) -> ProfileList {
        let active = self.active_id();
        let slot = self.config.core.config_slot();
        ProfileList {
            profiles: self
                .entries()
                .into_iter()
                .map(|mut p| {
                    p.active = active.as_deref() == Some(p.id.as_str());
                    p
                })
                .collect(),
            unmanaged: active.is_none() && slot.as_ref().is_some_and(|s| s.exists()),
            slot: slot.map(|s| s.display().to_string()).unwrap_or_default(),
        }
    }

    pub fn active(&self) -> Option<Profile> {
        let id = self.active_id()?;
        let mut profile = self.entries().into_iter().find(|p| p.id == id)?;
        profile.active = true;
        Some(profile)
    }

    /// A profile by id, name (exact, then ignoring ASCII case) or a unique
    /// id prefix.
    pub fn resolve(&self, query: &str) -> Result<Profile> {
        let query = query.trim();
        let entries = self.entries();
        let found = entries
            .iter()
            .find(|p| p.id == query)
            .or_else(|| unique(entries.iter().filter(|p| p.name == query)))
            .or_else(|| {
                unique(
                    entries
                        .iter()
                        .filter(|p| p.name.eq_ignore_ascii_case(query)),
                )
            })
            .or_else(|| {
                unique(
                    entries
                        .iter()
                        .filter(|p| query.len() >= 2 && p.id.starts_with(query)),
                )
            });
        if let Some(profile) = found {
            let mut profile = profile.clone();
            profile.active = self.active_id().as_deref() == Some(profile.id.as_str());
            return Ok(profile);
        }
        let similar: Vec<String> = entries
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case(query) || p.id.starts_with(query))
            .map(|p| format!("{} ({})", p.name, p.id))
            .collect();
        if similar.len() > 1 {
            bail!(
                "{}\n  {}",
                fl!("profiles-ambiguous", query = query),
                similar.join("\n  ")
            );
        }
        bail!(fl!("profiles-not-found", query = query))
    }

    pub async fn read(&self, query: &str) -> Result<(Profile, String)> {
        let profile = self.resolve(query)?;
        let path = self.file(&profile.id);
        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| fl!("err-read", path = path.display().to_string()))?;
        Ok((profile, content))
    }

    /// Stores a new profile from `content`, from a download of `url`, or
    /// from the template. Returns it with a note about `sing-box check`.
    pub async fn add(
        &self,
        name: Option<String>,
        content: Option<String>,
        url: Option<String>,
        interval: Option<u64>,
    ) -> Result<(Profile, String)> {
        let (content, url, download) = match (content, url) {
            (Some(_), Some(_)) => bail!(fl!("profiles-content-and-url")),
            (Some(content), None) => {
                profile::parse(&content)?;
                (content, None, None)
            }
            (None, Some(url)) => {
                let url = remote_url(&url)?;
                let download = self.download(&url).await?;
                (download.content.clone(), Some(url), Some(download))
            }
            (None, None) => (
                profile::to_text(&profile::template(&random_token(16))),
                None,
                None,
            ),
        };
        check_size(&content)?;
        let _ops = self.ops.lock().await;
        let now = now_unix();
        let profile = {
            let mut index = self.index.lock().expect("profile index");
            let wanted = name
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty())
                .or_else(|| download.as_ref().and_then(|d| d.filename.clone()))
                .or_else(|| url.as_deref().and_then(url_host))
                .unwrap_or_else(|| {
                    fl!(
                        "profiles-default-name",
                        number = (index.len() + 1).to_string()
                    )
                });
            let id = new_id(&index);
            let profile = Profile {
                name: unique_name(&index, &wanted, None),
                interval: match &url {
                    Some(_) => interval
                        .or(download.as_ref().and_then(|d| d.interval))
                        .unwrap_or(DEFAULT_INTERVAL),
                    None => 0,
                },
                url,
                created_at: now,
                updated_at: now,
                fetched_at: download.as_ref().map(|_| now),
                last_error: None,
                usage: download.as_ref().and_then(|d| d.usage),
                size: content.len() as u64,
                active: false,
                id,
            };
            self.write_content(&self.file(&profile.id), &content)?;
            index.push(profile.clone());
            if let Err(err) = self.save_index(&index) {
                index.pop();
                let _ = std::fs::remove_file(self.file(&profile.id));
                return Err(err);
            }
            profile
        };
        self.logs.info(fl_log!(
            "profiles-added-log",
            name = profile.name.clone(),
            id = profile.id.clone()
        ));
        let note = check_note(self.check_file(&self.file(&profile.id)).await);
        Ok((
            profile.clone(),
            format!(
                "{}\n{note}",
                fl!(
                    "profiles-added",
                    name = profile.name.clone(),
                    id = profile.id.clone()
                )
            ),
        ))
    }

    /// Replaces a profile's content and reloads sing-box when it is active.
    pub async fn save(&self, query: &str, content: &str, force: bool) -> Result<(Profile, String)> {
        let profile = self.resolve(query)?;
        profile::parse(content)?;
        check_size(content)?;
        let _ops = self.ops.lock().await;
        let (profile, changed, note) = self.replace(&profile.id, content, force).await?;
        let mut message = if changed {
            fl!("profiles-saved", name = profile.name.clone())
        } else {
            fl!("profiles-unchanged", name = profile.name.clone())
        };
        if let Some(note) = note {
            message = format!("{message}\n{note}");
        }
        if changed && profile.active {
            message = format!("{message}\n{}", self.reload_note().await);
        }
        Ok((profile, message))
    }

    /// Writes new content. The active profile has to pass `sing-box check`
    /// first unless `force` is set; inactive ones are checked afterwards and
    /// the outcome is returned as a note.
    async fn replace(
        &self,
        id: &str,
        content: &str,
        force: bool,
    ) -> Result<(Profile, bool, Option<String>)> {
        let path = self.file(id);
        let changed = std::fs::read(&path).map_or(true, |old| old != content.as_bytes());
        let active = self.active_id().as_deref() == Some(id);
        let mut note = None;
        if changed {
            if active && !force {
                let candidate = self.root.join(format!(".{id}.candidate.json"));
                self.write_content(&candidate, content)?;
                let check = self.check_file(&candidate).await;
                if let Check::Failed(err) = check {
                    let _ = std::fs::remove_file(&candidate);
                    // Name the profile's own file in sing-box's message.
                    let text = error_chain(&err).replace(
                        &candidate.display().to_string(),
                        &path.display().to_string(),
                    );
                    return Err(anyhow!(text).context(fl!("profiles-rejected")));
                }
                std::fs::rename(&candidate, &path)
                    .with_context(|| fl!("err-replace", path = path.display().to_string()))?;
                if let Check::Skipped = check {
                    note = Some(fl!("profiles-check-skipped"));
                }
            } else {
                self.write_content(&path, content)?;
                if !active {
                    note = Some(check_note(self.check_file(&path).await));
                }
            }
        }
        let now = now_unix();
        let mut profile = self.modify(id, |p| {
            if changed {
                p.updated_at = now;
            }
            p.size = content.len() as u64;
        })?;
        profile.active = active;
        Ok((profile, changed, note))
    }

    async fn check_file(&self, path: &Path) -> Check {
        let core = &self.config.core;
        if !core.binary.exists() {
            return Check::Skipped;
        }
        match run_check(core, &core.binary, core.check_args_with(Some(path))).await {
            Ok(()) => Check::Passed,
            Err(err) => Check::Failed(err),
        }
    }

    /// Reloads a running sing-box after the active profile changed.
    async fn reload_note(&self) -> String {
        if self.supervisor.status().state != CoreState::Running {
            return fl!("profiles-not-running");
        }
        match self.supervisor.request(Op::Reload).await {
            Response::Done { .. } => fl!("profiles-reloaded"),
            Response::Error { message } => fl!("profiles-reload-failed", error = message),
            _ => String::new(),
        }
    }

    /// Renames a profile or changes its download URL and interval.
    pub async fn set(
        &self,
        query: &str,
        name: Option<String>,
        url: Option<String>,
        interval: Option<u64>,
    ) -> Result<Profile> {
        let profile = self.resolve(query)?;
        let _ops = self.ops.lock().await;
        let name = match name.map(|n| n.trim().to_owned()) {
            Some(name) if name.is_empty() => bail!(fl!("profiles-name-empty")),
            Some(name) => {
                let taken = self
                    .entries()
                    .iter()
                    .any(|p| p.id != profile.id && p.name.eq_ignore_ascii_case(&name));
                if taken {
                    bail!(fl!("profiles-name-taken", name = name.clone()));
                }
                Some(name)
            }
            None => None,
        };
        let url = match url.map(|u| u.trim().to_owned()) {
            Some(url) if url.is_empty() => Some(None),
            Some(url) => Some(Some(remote_url(&url)?)),
            None => None,
        };
        let remote = match &url {
            Some(url) => url.is_some(),
            None => profile.is_remote(),
        };
        if interval.is_some_and(|i| i > 0) && !remote {
            bail!(fl!("profiles-not-remote", name = profile.name.clone()));
        }
        let mut updated = self.modify(&profile.id, |p| {
            if let Some(name) = name {
                p.name = name;
            }
            match url {
                Some(Some(url)) => {
                    if p.url.as_deref() != Some(url.as_str()) {
                        p.fetched_at = None;
                        p.last_error = None;
                        p.usage = None;
                    }
                    if p.url.is_none() && p.interval == 0 {
                        p.interval = DEFAULT_INTERVAL;
                    }
                    p.url = Some(url);
                }
                Some(None) => {
                    p.url = None;
                    p.interval = 0;
                    p.fetched_at = None;
                    p.last_error = None;
                    p.usage = None;
                }
                None => {}
            }
            if let Some(interval) = interval {
                p.interval = interval;
            }
        })?;
        updated.active = profile.active;
        Ok(updated)
    }

    pub async fn remove(&self, query: &str) -> Result<Profile> {
        let profile = self.resolve(query)?;
        if profile.active {
            bail!(fl!("profiles-remove-active", name = profile.name.clone()));
        }
        let _ops = self.ops.lock().await;
        {
            let mut index = self.index.lock().expect("profile index");
            index.retain(|p| p.id != profile.id);
            self.save_index(&index)?;
        }
        let path = self.file(&profile.id);
        if let Err(err) = std::fs::remove_file(&path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            return Err(err).with_context(|| fl!("err-delete", path = path.display().to_string()));
        }
        self.attempts.lock().expect("attempts").remove(&profile.id);
        self.logs.info(fl_log!(
            "profiles-removed-log",
            name = profile.name.clone(),
            id = profile.id.clone()
        ));
        Ok(profile)
    }

    /// `sing-box check` of a stored profile.
    pub async fn check(&self, query: &str) -> Result<String> {
        let profile = self.resolve(query)?;
        match self.check_file(&self.file(&profile.id)).await {
            Check::Passed => Ok(fl!("profiles-check-passed", name = profile.name.clone())),
            Check::Failed(err) => {
                Err(err.context(fl!("profiles-check-failed", name = profile.name.clone())))
            }
            Check::Skipped => bail!(fl!(
                "supervisor-binary-not-found",
                path = self.config.core.binary.display().to_string()
            )),
        }
    }

    /// Downloads a remote profile again. The active profile only changes to
    /// a download sing-box accepts, unless `force` is set.
    pub async fn update(&self, query: &str, force: bool) -> Result<String> {
        let profile = self.resolve(query)?;
        let Some(url) = profile.url.clone() else {
            bail!(fl!("profiles-not-remote", name = profile.name.clone()));
        };
        self.attempts
            .lock()
            .expect("attempts")
            .insert(profile.id.clone(), now_unix());
        let result = async {
            let download = self.download(&url).await?;
            check_size(&download.content)?;
            let _ops = self.ops.lock().await;
            let (_, changed, note) = self.replace(&profile.id, &download.content, force).await?;
            let updated = self.modify(&profile.id, |p| {
                p.fetched_at = Some(now_unix());
                p.last_error = None;
                p.usage = download.usage;
            })?;
            Ok::<_, anyhow::Error>((updated, changed, note))
        }
        .await;
        let (updated, changed, note) = match result {
            Ok(done) => done,
            Err(err) => {
                let reason = error_chain(&err);
                let _ = self.modify(&profile.id, |p| {
                    p.last_error = Some(reason.lines().next().unwrap_or_default().to_owned())
                });
                return Err(err);
            }
        };
        self.logs.info(fl_log!(
            "profiles-updated-log",
            name = updated.name.clone(),
            size = fmt_bytes(updated.size)
        ));
        let mut message = if changed {
            fl!(
                "profiles-updated",
                name = updated.name.clone(),
                size = fmt_bytes(updated.size)
            )
        } else {
            fl!("profiles-update-unchanged", name = updated.name.clone())
        };
        if let Some(note) = note {
            message = format!("{message}\n{note}");
        }
        if changed && profile.active {
            message = format!("{message}\n{}", self.reload_note().await);
        }
        Ok(message)
    }

    /// Downloads every remote profile; fails if any download failed.
    pub async fn update_all(&self, force: bool) -> Result<String> {
        let remote: Vec<Profile> = self
            .entries()
            .into_iter()
            .filter(Profile::is_remote)
            .collect();
        if remote.is_empty() {
            bail!(fl!("profiles-no-remote"));
        }
        let mut lines = Vec::new();
        let mut failed = false;
        for profile in remote {
            match self.update(&profile.id, force).await {
                Ok(message) => lines.push(message),
                Err(err) => {
                    failed = true;
                    lines.push(fl!(
                        "profiles-update-failed",
                        name = profile.name.clone(),
                        error = error_chain(&err)
                    ));
                }
            }
        }
        let text = lines.join("\n");
        if failed { Err(anyhow!(text)) } else { Ok(text) }
    }

    /// Moves a configuration file that is not in the store yet into a new
    /// profile and links the file to it. The content stays the same, so
    /// sing-box needs no reload. `None` when there is nothing to adopt.
    pub async fn adopt(&self) -> Result<Option<Profile>> {
        let Some(slot) = self.config.core.config_slot() else {
            return Ok(None);
        };
        if self.active_id().is_some() {
            return Ok(None);
        }
        let content = match tokio::fs::read(&slot).await {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err)
                    .with_context(|| fl!("err-read", path = slot.display().to_string()));
            }
        };
        let content = String::from_utf8(content)
            .map_err(|_| anyhow!(fl!("profiles-not-text", path = slot.display().to_string())))?;
        check_size(&content)?;
        let _ops = self.ops.lock().await;
        let now = now_unix();
        let profile = {
            let mut index = self.index.lock().expect("profile index");
            let profile = Profile {
                id: new_id(&index),
                name: unique_name(&index, &fl!("profiles-adopted-name"), None),
                url: None,
                interval: 0,
                created_at: now,
                updated_at: now,
                fetched_at: None,
                last_error: None,
                usage: None,
                size: content.len() as u64,
                active: false,
            };
            self.write_content(&self.file(&profile.id), &content)?;
            index.push(profile.clone());
            self.save_index(&index)?;
            Profile {
                active: true,
                ..profile
            }
        };
        point_symlink(&slot, &self.file(&profile.id))
            .with_context(|| fl!("profiles-link-failed", path = slot.display().to_string()))?;
        self.logs.info(fl_log!(
            "profiles-adopted-log",
            path = slot.display().to_string(),
            name = profile.name.clone(),
            id = profile.id.clone()
        ));
        Ok(Some(profile))
    }

    /// Switches sing-box to a profile, keeping a hand-written configuration
    /// in the store first so that nothing is lost.
    pub async fn activate(&self, query: &str, force: bool) -> Result<Response> {
        let profile = self.resolve(query)?;
        if profile.active {
            return Ok(Response::done(fl!(
                "profiles-already-active",
                name = profile.name.clone()
            )));
        }
        if self.config.core.config_slot().is_none() {
            bail!(fl!("supervisor-no-config-slot"));
        }
        let previous = match self.active() {
            Some(active) => Some(active),
            None => self.adopt().await?,
        };
        let label = |p: &Profile| CoreLabel {
            reply: p.name.clone(),
            log: p.name.clone(),
        };
        let fallback = previous
            .filter(|p| p.id != profile.id)
            .map(|p| (self.file(&p.id), label(&p)));
        Ok(self
            .supervisor
            .request(Op::SwitchConfig {
                target: self.file(&profile.id),
                label: label(&profile),
                force,
                fallback,
            })
            .await)
    }

    async fn download(&self, url: &str) -> Result<Download> {
        crate::util::init_tls();
        let settings = &self.config.profiles;
        let agent = match settings.user_agent.trim() {
            "" => match self.supervisor.status().core_version {
                Some(version) => format!("sing-box/{version}"),
                None => "sing-box".to_owned(),
            },
            custom => custom.to_owned(),
        };
        let mut builder = reqwest::Client::builder()
            .user_agent(agent)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(90));
        builder = match settings.proxy.as_deref().filter(|p| !p.is_empty()) {
            Some(proxy) => {
                builder.proxy(reqwest::Proxy::all(proxy).context(fl!("profiles-invalid-proxy"))?)
            }
            None => builder.no_proxy(),
        };
        // Subscription URLs carry credentials; messages name the host only.
        let shown = redact(url);
        let response = builder
            .build()?
            .get(url)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .with_context(|| fl!("err-request", url = shown.clone()))?;
        let status = response.status();
        if !status.is_success() {
            bail!(fl!(
                "profiles-http-error",
                url = shown.clone(),
                status = status.to_string()
            ));
        }
        let headers = response.headers().clone();
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(reqwest::Error::without_url)
                .with_context(|| fl!("err-download", url = shown.clone()))?;
            if data.len() + chunk.len() > MAX_PROFILE_BYTES {
                bail!(fl!(
                    "err-too-large",
                    url = shown.clone(),
                    limit = fmt_bytes(MAX_PROFILE_BYTES as u64)
                ));
            }
            data.extend_from_slice(&chunk);
        }
        let content =
            String::from_utf8(data).map_err(|_| anyhow!(fl!("profiles-not-text", path = shown)))?;
        profile::parse(&content)?;
        Ok(Download {
            content,
            usage: header(&headers, "subscription-userinfo").and_then(|v| parse_usage(&v)),
            interval: header(&headers, "profile-update-interval")
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|hours| *hours > 0.0)
                .map(|hours| (hours * 60.0).round() as u64),
            filename: header(&headers, "content-disposition").and_then(|v| disposition_name(&v)),
        })
    }

    /// Downloads remote profiles whose interval has passed, in the background.
    pub fn spawn_updater(self: &Arc<Self>) -> JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            // Give sing-box and the components time to come up first; a
            // provider may be reachable only through the proxy.
            tokio::time::sleep(Duration::from_secs(30)).await;
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                for profile in manager.due() {
                    let update = manager.update(&profile.id, false);
                    if let Err(err) = i18n::scope(i18n::process_language(), update).await {
                        manager.logs.warn(fl_log!(
                            "profiles-auto-update-failed",
                            name = profile.name.clone(),
                            error = error_chain(&err)
                        ));
                    }
                }
            }
        })
    }

    fn due(&self) -> Vec<Profile> {
        let now = now_unix();
        let attempts = self.attempts.lock().expect("attempts").clone();
        self.entries()
            .into_iter()
            .filter(|p| p.is_remote() && p.interval > 0)
            .filter(|p| {
                let next = p.fetched_at.unwrap_or(0) + p.interval * 60;
                let retry = attempts.get(&p.id).map_or(0, |at| at + RETRY_AFTER);
                now >= next && now >= retry
            })
            .collect()
    }
}

fn unique<'a>(mut matches: impl Iterator<Item = &'a Profile>) -> Option<&'a Profile> {
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn new_id(index: &[Profile]) -> String {
    loop {
        let id = random_token(8).to_ascii_lowercase();
        if !index.iter().any(|p| p.id == id) {
            return id;
        }
    }
}

/// `wanted`, or `wanted 2`, `wanted 3`, ... when another profile has the name.
fn unique_name(index: &[Profile], wanted: &str, except: Option<&str>) -> String {
    let taken = |name: &str| {
        index
            .iter()
            .any(|p| Some(p.id.as_str()) != except && p.name.eq_ignore_ascii_case(name))
    };
    if !taken(wanted) {
        return wanted.to_owned();
    }
    (2..)
        .map(|n| format!("{wanted} {n}"))
        .find(|name| !taken(name))
        .unwrap_or_default()
}

fn check_size(content: &str) -> Result<()> {
    if content.len() > MAX_PROFILE_BYTES {
        bail!(fl!(
            "profiles-too-large",
            limit = fmt_bytes(MAX_PROFILE_BYTES as u64)
        ));
    }
    Ok(())
}

fn check_note(check: Check) -> String {
    match check {
        Check::Passed => fl!("profiles-check-ok"),
        Check::Failed(err) => fl!("profiles-check-warning", error = error_chain(&err)),
        Check::Skipped => fl!("profiles-check-skipped"),
    }
}

fn remote_url(url: &str) -> Result<String> {
    let url = url.trim();
    match Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.has_host() => {
            Ok(url.to_owned())
        }
        _ => bail!(fl!("profiles-invalid-url", url = redact(url))),
    }
}

fn url_host(url: &str) -> Option<String> {
    Url::parse(url).ok()?.host_str().map(str::to_owned)
}

/// `https://example.com/…`: subscription paths and queries carry tokens.
pub fn redact(url: &str) -> String {
    match Url::parse(url) {
        Ok(parsed) if parsed.has_host() => format!(
            "{}://{}/…",
            parsed.scheme(),
            parsed.host_str().unwrap_or_default()
        ),
        _ => url.chars().take(24).collect::<String>() + "…",
    }
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// `upload=1; download=2; total=3; expire=1700000000`.
fn parse_usage(value: &str) -> Option<ProfileUsage> {
    let mut usage = ProfileUsage::default();
    let mut seen = false;
    for part in value.split(';') {
        let Some((key, number)) = part.split_once('=') else {
            continue;
        };
        // Some panels send floats such as 1.2e10.
        let Some(number) = number.trim().parse::<f64>().ok().filter(|n| *n >= 0.0) else {
            continue;
        };
        let number = number as u64;
        match key.trim().to_ascii_lowercase().as_str() {
            "upload" => usage.upload = number,
            "download" => usage.download = number,
            "total" => usage.total = number,
            "expire" => usage.expire = (number > 0).then_some(number),
            _ => continue,
        }
        seen = true;
    }
    seen.then_some(usage)
}

/// The file name of `content-disposition`, without its extension.
fn disposition_name(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').map(str::trim) {
        if let Some(encoded) = part.strip_prefix("filename*=") {
            // RFC 5987: charset'language'percent-encoded
            let encoded = encoded.rsplit('\'').next().unwrap_or(encoded);
            return clean_name(&percent_decode(encoded.trim_matches('"')));
        }
        if let Some(name) = part.strip_prefix("filename=") {
            plain = Some(name.trim_matches('"').to_owned());
        }
    }
    clean_name(&percent_decode(&plain?))
}

fn clean_name(name: &str) -> Option<String> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let name = name
        .strip_suffix(".json")
        .or_else(|| name.strip_suffix(".jsonc"))
        .unwrap_or(name)
        .trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = text
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::supervisor::Supervisor;

    #[test]
    fn provider_headers() {
        let usage =
            parse_usage("upload=1024; download=2048; total=1e10; expire=1767225600").unwrap();
        assert_eq!(
            usage,
            ProfileUsage {
                upload: 1024,
                download: 2048,
                total: 10_000_000_000,
                expire: Some(1_767_225_600)
            }
        );
        assert_eq!(parse_usage("upload=1; expire=0").unwrap().expire, None);
        assert!(parse_usage("garbage").is_none());
        assert_eq!(
            disposition_name("attachment; filename*=UTF-8''%E6%9C%BA%E5%9C%BA.json").as_deref(),
            Some("机场")
        );
        assert_eq!(
            disposition_name(r#"attachment; filename="My Sub.json""#).as_deref(),
            Some("My Sub")
        );
        assert!(disposition_name("inline").is_none());
    }

    #[test]
    fn urls_are_checked_and_redacted() {
        assert!(remote_url("https://example.com/sub?token=x").is_ok());
        assert!(remote_url("file:///etc/passwd").is_err());
        assert!(remote_url("example.com").is_err());
        assert_eq!(
            redact("https://example.com/api/v1/sub?token=s3cret"),
            "https://example.com/…"
        );
        assert!(!redact("not a url but a secret token").contains("token"));
    }

    #[test]
    fn names_and_ids() {
        let entry = |id: &str, name: &str| Profile {
            id: id.to_owned(),
            name: name.to_owned(),
            url: None,
            interval: 0,
            created_at: 0,
            updated_at: 0,
            fetched_at: None,
            last_error: None,
            usage: None,
            size: 0,
            active: false,
        };
        let index = vec![entry("aaaa1111", "Home"), entry("bbbb2222", "home 2")];
        assert_eq!(unique_name(&index, "Work", None), "Work");
        assert_eq!(unique_name(&index, "HOME", None), "HOME 3");
        assert_eq!(unique_name(&index, "Home", Some("aaaa1111")), "Home");
        let id = new_id(&index);
        assert!(valid_id(&id) && id.len() == 8);
        assert!(!valid_id("../x") && !valid_id("ABC"));
    }

    fn open(dir: &Path) -> Arc<ProfileManager> {
        let mut config = DaemonConfig::default();
        config.components.data_dir = dir.join("data");
        config.core.binary = dir.join("missing/sing-box");
        config.core.config = vec![dir.join("etc/config.json")];
        config.core.auto_start = false;
        let logs = Arc::new(LogHub::new(100, false));
        let (_, gate) = tokio::sync::watch::channel(true);
        let (supervisor, _) = Supervisor::spawn(config.clone(), logs.clone(), gate);
        ProfileManager::new(config, logs, supervisor)
    }

    #[tokio::test]
    async fn add_resolve_adopt_save_remove() {
        let dir = std::env::temp_dir().join(format!("sbb-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        let profiles = open(&dir);
        let slot = dir.join("etc/config.json");
        std::fs::write(&slot, "{\"log\": {}} // hand-written\n").unwrap();
        assert!(profiles.list().unmanaged);

        // The template, then content, with names kept unique.
        let (home, note) = profiles
            .add(Some("Home".into()), None, None, None)
            .await
            .unwrap();
        assert!(note.contains(&fl!("profiles-check-skipped")));
        let (copy, _) = profiles
            .add(Some("home".into()), Some("{}".into()), None, None)
            .await
            .unwrap();
        assert_eq!(copy.name, "home 2");
        assert!(
            profiles
                .add(None, Some("[]".into()), None, None)
                .await
                .is_err()
        );
        assert_eq!(profiles.resolve("Home").unwrap().id, home.id);
        assert_eq!(profiles.resolve(&home.id[..4]).unwrap().id, home.id);
        assert!(profiles.resolve("nothing").is_err());

        // Adoption keeps the hand-written file and links the slot to it.
        let adopted = profiles.adopt().await.unwrap().unwrap();
        assert_eq!(profiles.active().unwrap().id, adopted.id);
        assert!(!profiles.list().unmanaged);
        assert_eq!(
            std::fs::read_to_string(&slot).unwrap(),
            "{\"log\": {}} // hand-written\n"
        );
        assert!(profiles.adopt().await.unwrap().is_none());
        assert!(profiles.remove(&adopted.id).await.is_err());

        // Saving the active profile replaces what sing-box reads.
        let (saved, message) = profiles
            .save(&adopted.id, "{\"log\": {\"level\": \"warn\"}}", false)
            .await
            .unwrap();
        assert!(saved.active, "{message}");
        assert!(std::fs::read_to_string(&slot).unwrap().contains("warn"));
        assert!(profiles.save(&adopted.id, "not json", true).await.is_err());

        // Rename, remote settings, removal; the index survives a restart.
        let renamed = profiles
            .set(
                &copy.id,
                Some("Office".into()),
                Some("https://example.com/s".into()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            (renamed.name.as_str(), renamed.interval),
            ("Office", DEFAULT_INTERVAL)
        );
        assert!(
            profiles
                .set(&home.id, Some("office".into()), None, None)
                .await
                .is_err()
        );
        assert!(profiles.set(&home.id, None, None, Some(60)).await.is_err());
        let local = profiles
            .set(&copy.id, None, Some(String::new()), None)
            .await
            .unwrap();
        assert!(!local.is_remote() && local.interval == 0);
        profiles.remove("Office").await.unwrap();
        assert!(!profiles.file(&copy.id).exists());
        let reopened = open(&dir).list();
        let names: Vec<&str> = reopened.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Home", fl!("profiles-adopted-name").as_str()]);
        assert!(reopened.profiles[1].active);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
