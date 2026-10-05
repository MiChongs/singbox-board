//! Downloads from GitHub releases (and other HTTPS sources) shared by the
//! sing-box updater and the component installer.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use futures::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::UpdateConfig;

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
    /// `sha256:<hex>`, computed by GitHub for every uploaded asset.
    #[serde(default)]
    pub digest: Option<String>,
}

impl Release {
    pub fn version(&self) -> &str {
        self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }

    pub fn require(&self, name: &str) -> Result<&Asset> {
        self.asset(name)
            .ok_or_else(|| anyhow!("release {} has no asset {name}", self.tag_name))
    }
}

impl Asset {
    fn sha256(&self) -> Option<&str> {
        self.digest.as_deref()?.strip_prefix("sha256:")
    }
}

pub fn verify_sha256(data: &[u8], expected: &str, name: &str) -> Result<()> {
    let actual = hex::encode(Sha256::digest(data));
    ensure!(
        actual.eq_ignore_ascii_case(expected),
        "checksum mismatch for {name}: expected {expected}, got {actual}"
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
                builder.proxy(reqwest::Proxy::all(proxy).context("invalid update.proxy")?)
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
            .ok_or_else(|| anyhow!("{repo} has no releases"))
    }

    /// Downloads an arbitrary URL into memory, refusing bodies over `limit`.
    pub async fn fetch(&self, url: &str, limit: usize) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("download {url}"))?;
            ensure!(
                data.len() + chunk.len() <= limit,
                "{url} is larger than {limit} bytes"
            );
            data.extend_from_slice(&chunk);
        }
        Ok(data)
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
            None => tracing::warn!("GitHub published no digest for {}", asset.name),
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
