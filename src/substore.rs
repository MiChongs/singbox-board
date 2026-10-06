//! Client for the local Sub-Store backend and the glue to sing-box providers.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::i18n::fl;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Subscription,
    Collection,
}

impl EntryKind {
    pub fn label(self) -> String {
        match self {
            EntryKind::Subscription => fl!("entry-subscription"),
            EntryKind::Collection => fl!("entry-collection"),
        }
    }
}

/// A subscription or collection with the URL sing-box can fetch it from.
#[derive(Debug, Clone)]
pub struct Entry {
    pub kind: EntryKind,
    pub name: String,
    pub display_name: String,
    /// Source URL of a subscription, member list of a collection.
    pub detail: String,
    pub singbox_url: String,
}

#[derive(Debug, Clone, Default)]
pub struct Overview {
    pub version: String,
    pub entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    #[serde(default)]
    status: String,
    data: Option<T>,
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct Env {
    #[serde(default)]
    version: String,
}

/// Sub-Store writes the display name twice, as `displayName` and as
/// `display-name` for older clients; either may be missing or null.
#[derive(Deserialize)]
struct DisplayName {
    #[serde(default, rename = "displayName")]
    camel: Option<String>,
    #[serde(default, rename = "display-name")]
    kebab: Option<String>,
}

impl DisplayName {
    fn into_string(self) -> String {
        self.camel
            .filter(|name| !name.is_empty())
            .or(self.kebab)
            .unwrap_or_default()
    }
}

#[derive(Deserialize)]
struct Subscription {
    name: String,
    #[serde(flatten)]
    display_name: DisplayName,
    #[serde(default)]
    source: String,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct Collection {
    name: String,
    #[serde(flatten)]
    display_name: DisplayName,
    #[serde(default)]
    subscriptions: Vec<String>,
}

#[derive(Clone)]
pub struct SubStoreClient {
    http: reqwest::Client,
    base: Url,
}

impl SubStoreClient {
    /// `api` is the backend base including the secret path, from the daemon status.
    pub fn new(api: &str) -> Result<Self> {
        crate::util::init_tls();
        let base = Url::parse(api).with_context(|| fl!("err-sub-store-url", url = api))?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { http, base })
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        url
    }

    async fn get<T: DeserializeOwned>(&self, segments: &[&str]) -> Result<T> {
        let url = self.url(segments);
        let envelope: Envelope<T> = self
            .http
            .get(url.clone())
            .send()
            .await
            .with_context(|| fl!("err-request", url = url.path()))?
            .json()
            .await
            .with_context(|| fl!("err-decode", url = url.path()))?;
        match envelope.data {
            Some(data) if envelope.status == "success" => Ok(data),
            _ => bail!(
                "Sub-Store: {}",
                envelope
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| envelope.status)
            ),
        }
    }

    pub async fn overview(&self) -> Result<Overview> {
        let env: Env = self.get(&["api", "utils", "env"]).await?;
        let subs: Vec<Subscription> = self.get(&["api", "subs"]).await?;
        let collections: Vec<Collection> = self.get(&["api", "collections"]).await?;
        let mut entries: Vec<Entry> = subs
            .into_iter()
            .map(|s| Entry {
                kind: EntryKind::Subscription,
                singbox_url: self.singbox_url(EntryKind::Subscription, &s.name),
                detail: if s.source == "local" {
                    "local".to_owned()
                } else {
                    s.url.lines().next().unwrap_or_default().to_owned()
                },
                display_name: s.display_name.into_string(),
                name: s.name,
            })
            .collect();
        entries.extend(collections.into_iter().map(|c| Entry {
            kind: EntryKind::Collection,
            singbox_url: self.singbox_url(EntryKind::Collection, &c.name),
            detail: c.subscriptions.join(", "),
            display_name: c.display_name.into_string(),
            name: c.name,
        }));
        Ok(Overview {
            version: env.version,
            entries,
        })
    }

    /// Download URL producing sing-box outbounds for a subscription or collection.
    pub fn singbox_url(&self, kind: EntryKind, name: &str) -> String {
        let mut url = match kind {
            EntryKind::Subscription => self.url(&["download", name]),
            EntryKind::Collection => self.url(&["download", "collection", name]),
        };
        url.query_pairs_mut().append_pair("target", "sing-box");
        url.to_string()
    }
}

/// A `providers` entry for MiChongs/sing-box pointing at a Sub-Store output.
pub fn provider_snippet(tag: &str, url: &str) -> String {
    let tag = serde_json::to_string(tag).unwrap_or_default();
    let url = serde_json::to_string(url).unwrap_or_default();
    let comment = fl!("snippet-group-comment");
    format!(
        r#"{{
  "providers": [
    {{
      "type": "remote",
      "tag": {tag},
      "url": {url},
      "update_interval": "1h",
      "health_check": {{
        "enabled": true,
        "url": "https://www.gstatic.com/generate_204",
        "interval": "10m"
      }}
    }}
  ]
}}
// {comment}
// {{ "type": "urltest", "tag": "auto", "providers": [{tag}] }}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_urls_keep_secret_path_and_escape_names() {
        let client = SubStoreClient::new("http://127.0.0.1:3001/s3cret").unwrap();
        assert_eq!(
            client.singbox_url(EntryKind::Subscription, "my sub"),
            "http://127.0.0.1:3001/s3cret/download/my%20sub?target=sing-box"
        );
        assert_eq!(
            client.singbox_url(EntryKind::Collection, "all"),
            "http://127.0.0.1:3001/s3cret/download/collection/all?target=sing-box"
        );
    }

    #[test]
    fn snippet_is_valid_json() {
        let snippet = provider_snippet("a\"b", "http://x/y?target=sing-box");
        let json: String = snippet.lines().filter(|l| !l.starts_with("//")).collect();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["providers"][0]["tag"], "a\"b");
        assert_eq!(value["providers"][0]["type"], "remote");
    }

    #[test]
    fn envelope_parsing() {
        let subs: Envelope<Vec<Subscription>> = serde_json::from_str(
            r#"{"status":"success","data":[{"name":"a","displayName":"A","source":"remote","url":"https://x"}]}"#,
        )
        .unwrap();
        assert_eq!(subs.data.unwrap().remove(0).display_name.into_string(), "A");
        let failed: Envelope<Vec<Subscription>> =
            serde_json::from_str(r#"{"status":"failed","error":{"message":"boom"}}"#).unwrap();
        assert_eq!(failed.error.unwrap().message, "boom");
    }

    #[test]
    fn display_name_in_either_or_both_spellings() {
        let names = |json: &str| -> Vec<String> {
            serde_json::from_str::<Vec<Collection>>(json)
                .unwrap()
                .into_iter()
                .map(|c| c.display_name.into_string())
                .collect()
        };
        // Current Sub-Store sends both keys.
        assert_eq!(
            names(r#"[{"name":"a","displayName":"A","display-name":"A","subscriptions":["x"]}]"#),
            ["A"]
        );
        assert_eq!(
            names(
                r#"[{"name":"a","display-name":"Old"},{"name":"b","displayName":"","display-name":"B"},{"name":"c","displayName":null},{"name":"d"}]"#
            ),
            ["Old", "B", "", ""]
        );
    }
}
