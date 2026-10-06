//! Downloads from GitHub releases (and other HTTPS sources) shared by the
//! sing-box updater and the component installer.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use futures::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::UpdateConfig;
use crate::i18n::{fl, fl_log};

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
    /// `sha256:<hex>`, computed by GitHub for assets uploaded since mid 2025.
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub size: u64,
}

impl Release {
    pub fn version(&self) -> &str {
        self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }

    pub fn require(&self, name: &str) -> Result<&Asset> {
        self.asset(name).ok_or_else(|| {
            anyhow!(fl!(
                "github-no-asset",
                tag = self.tag_name.clone(),
                name = name
            ))
        })
    }
}

impl Asset {
    pub fn sha256(&self) -> Option<&str> {
        self.digest.as_deref()?.strip_prefix("sha256:")
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

pub fn verify_sha256(data: &[u8], expected: &str, name: &str) -> Result<()> {
    let actual = hex::encode(Sha256::digest(data));
    ensure!(
        actual.eq_ignore_ascii_case(expected),
        fl!(
            "err-checksum-mismatch",
            name = name,
            expected = expected,
            actual = actual.clone()
        )
    );
    Ok(())
}

pub struct GitHub {
    http: reqwest::Client,
    api_url: String,
    token: Option<String>,
    mirror: Option<String>,
}

impl GitHub {
    /// Uses the proxy, mirror, API URL and token from `[update]`.
    pub fn new(config: &UpdateConfig) -> Result<Self> {
        crate::util::init_tls();
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("singbox-board/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60));
        builder = match config.proxy.as_deref().filter(|p| !p.is_empty()) {
            Some(proxy) => {
                builder.proxy(reqwest::Proxy::all(proxy).context(fl!("github-invalid-proxy"))?)
            }
            None => builder.no_proxy(),
        };
        let non_empty = |value: &Option<String>| value.clone().filter(|v| !v.is_empty());
        Ok(Self {
            http: builder.build()?,
            api_url: config.api_url.trim_end_matches('/').to_owned(),
            token: non_empty(&config.github_token),
            mirror: non_empty(&config.mirror),
        })
    }

    async fn api_get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{path}", self.api_url);
        let mut request = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .timeout(Duration::from_secs(30));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .with_context(|| fl!("err-request", url = url.clone()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!(fl!(
                "github-http-error",
                url = url,
                status = status.to_string(),
                body = body.chars().take(200).collect::<String>()
            ));
        }
        response
            .json()
            .await
            .with_context(|| fl!("err-decode", url = url.clone()))
    }

    /// A release by exact tag, or the newest one (optionally including pre-releases).
    pub async fn release(
        &self,
        repo: &str,
        tag: Option<&str>,
        prerelease: bool,
    ) -> Result<Release> {
        if let Some(tag) = tag.filter(|t| !t.is_empty()) {
            return self
                .api_get(&format!("/repos/{repo}/releases/tags/{tag}"))
                .await;
        }
        if !prerelease {
            return self
                .api_get(&format!("/repos/{repo}/releases/latest"))
                .await;
        }
        let releases: Vec<Release> = self
            .api_get(&format!("/repos/{repo}/releases?per_page=20"))
            .await?;
        releases
            .into_iter()
            .find(|release| !release.draft)
            .ok_or_else(|| anyhow!(fl!("github-no-releases", repo = repo)))
    }

    /// One page (1-based) of releases, newest first, drafts removed. The
    /// second value tells whether another page may follow.
    pub async fn releases(
        &self,
        repo: &str,
        page: u32,
        per_page: u32,
    ) -> Result<(Vec<Release>, bool)> {
        let releases: Vec<Release> = self
            .api_get(&format!(
                "/repos/{repo}/releases?per_page={per_page}&page={}",
                page.max(1)
            ))
            .await?;
        let has_more = releases.len() as u32 == per_page;
        Ok((
            releases.into_iter().filter(|r| !r.draft).collect(),
            has_more,
        ))
    }

    /// Fails unless `repo` exists and is reachable.
    pub async fn check_repo(&self, repo: &str) -> Result<()> {
        let _: serde_json::Value = self.api_get(&format!("/repos/{repo}")).await?;
        Ok(())
    }

    /// Downloads an arbitrary URL into memory, refusing bodies over `limit`.
    pub async fn fetch(&self, url: &str, limit: usize) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| fl!("err-request", url = url))?
            .error_for_status()
            .with_context(|| fl!("err-request", url = url))?;
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| fl!("err-download", url = url))?;
            ensure!(
                data.len() + chunk.len() <= limit,
                fl!(
                    "err-too-large",
                    url = url,
                    limit = crate::util::fmt_bytes(limit as u64)
                )
            );
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }

    /// Streams `url` into a new file at `path`, refusing bodies over
    /// `limit` bytes, and returns the SHA-256 of what was written.
    /// `progress` sees the bytes received so far and the announced size.
    pub async fn download_to_file(
        &self,
        url: &str,
        path: &std::path::Path,
        limit: u64,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<String> {
        use tokio::io::AsyncWriteExt;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| fl!("err-request", url = url))?
            .error_for_status()
            .with_context(|| fl!("err-request", url = url))?;
        let total = response.content_length();
        let too_large = || {
            anyhow!(fl!(
                "err-too-large",
                url = url,
                limit = crate::util::fmt_bytes(limit)
            ))
        };
        if total.is_some_and(|total| total > limit) {
            return Err(too_large());
        }
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .await
            .with_context(|| fl!("err-create", path = path.display().to_string()))?;
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| fl!("err-download", url = url))?;
            received += chunk.len() as u64;
            if received > limit {
                return Err(too_large());
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .with_context(|| fl!("err-create", path = path.display().to_string()))?;
            progress(received, total);
        }
        file.flush().await?;
        file.sync_all().await?;
        Ok(hex::encode(hasher.finalize()))
    }

    /// Downloads a release asset (through the mirror when configured) and
    /// checks it against the digest GitHub publishes for it.
    pub async fn download(&self, asset: &Asset, limit: usize) -> Result<Vec<u8>> {
        let url = match &self.mirror {
            Some(mirror) => format!(
                "{}/{}",
                mirror.trim_end_matches('/'),
                asset.browser_download_url
            ),
            None => asset.browser_download_url.clone(),
        };
        let data = self.fetch(&url, limit).await?;
        match asset.sha256() {
            Some(expected) => verify_sha256(&data, expected, &asset.name)?,
            None => tracing::warn!("{}", fl_log!("github-no-digest", name = asset.name.clone())),
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn asset_digest() {
        let asset: Asset = serde_json::from_str(
            r#"{"name":"a","browser_download_url":"https://x/a","digest":"sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"}"#,
        )
        .unwrap();
        verify_sha256(b"hello", asset.sha256().unwrap(), "a").unwrap();
        assert!(verify_sha256(b"other", asset.sha256().unwrap(), "a").is_err());
        let legacy: Asset =
            serde_json::from_str(r#"{"name":"a","browser_download_url":"https://x/a"}"#).unwrap();
        assert!(legacy.sha256().is_none());
    }
}
