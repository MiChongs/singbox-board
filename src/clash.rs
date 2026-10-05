//! Minimal client for the sing-box Clash API (`experimental.clash_api`).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, RequestBuilder, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::i18n::fl;
use crate::protocol::ClashApi;

pub const DEFAULT_TEST_URL: &str = "https://www.gstatic.com/generate_204";

#[derive(Clone)]
pub struct ClashClient {
    http: reqwest::Client,
    base: Url,
    secret: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Configs {
    #[serde(default)]
    pub mode: String,
    #[serde(default, rename = "mode-list")]
    pub mode_list: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Proxies {
    #[serde(default)]
    pub proxies: HashMap<String, Proxy>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Proxy {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub now: Option<String>,
    #[serde(default)]
    pub all: Option<Vec<String>>,
    #[serde(default)]
    pub history: Vec<DelayHistory>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DelayHistory {
    #[serde(default)]
    pub delay: u32,
}

impl Proxy {
    pub fn is_group(&self) -> bool {
        self.all.is_some()
    }

    /// Groups that accept `PUT /proxies/{name}` (manual pin for URLTest/Smart).
    pub fn is_selectable(&self) -> bool {
        matches!(self.kind.as_str(), "Selector" | "URLTest" | "Smart")
    }

    pub fn last_delay(&self) -> Option<u32> {
        self.history.last().map(|h| h.delay)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connections {
    #[serde(default)]
    pub download_total: u64,
    #[serde(default)]
    pub upload_total: u64,
    #[serde(default)]
    pub connections: Option<Vec<Connection>>,
    #[serde(default)]
    pub memory: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Connection {
    pub id: String,
    #[serde(default)]
    pub metadata: ConnectionMetadata,
    #[serde(default)]
    pub upload: u64,
    #[serde(default)]
    pub download: u64,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub chains: Vec<String>,
    #[serde(default)]
    pub rule: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionMetadata {
    #[serde(default)]
    pub network: String,
    #[serde(default, rename = "destinationIP")]
    pub destination_ip: String,
    #[serde(default)]
    pub destination_port: String,
    #[serde(default)]
    pub host: String,
}

impl Connection {
    pub fn target(&self) -> String {
        let host = if self.metadata.host.is_empty() {
            &self.metadata.destination_ip
        } else {
            &self.metadata.host
        };
        if host.contains(':') {
            format!("[{host}]:{}", self.metadata.destination_port)
        } else {
            format!("{host}:{}", self.metadata.destination_port)
        }
    }
}

#[derive(Debug, Deserialize)]
struct DelayResponse {
    delay: Option<u32>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    message: Option<String>,
}

impl ClashClient {
    pub fn new(api: &ClashApi) -> Result<Self> {
        let base = Url::parse(&api.url)
            .with_context(|| fl!("err-clash-api-url", url = api.url.clone()))?;
        crate::util::init_tls();
        let http = reqwest::Client::builder()
            // The controller is local; never route it through a proxy.
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            http,
            base,
            secret: api.secret.clone(),
        })
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

    fn request(&self, method: Method, segments: &[&str]) -> RequestBuilder {
        let builder = self.http.request(method, self.url(segments));
        if self.secret.is_empty() {
            builder
        } else {
            builder.bearer_auth(&self.secret)
        }
    }

    async fn send(&self, builder: RequestBuilder) -> Result<reqwest::Response> {
        let response = builder.send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let message = response
            .json::<ErrorResponse>()
            .await
            .ok()
            .and_then(|e| e.message)
            .unwrap_or_default();
        bail!("HTTP {status} {message}")
    }

    async fn get<T: DeserializeOwned>(&self, segments: &[&str]) -> Result<T> {
        Ok(self
            .send(self.request(Method::GET, segments))
            .await?
            .json()
            .await?)
    }

    pub async fn configs(&self) -> Result<Configs> {
        self.get(&["configs"]).await
    }

    pub async fn set_mode(&self, mode: &str) -> Result<()> {
        let body = serde_json::json!({ "mode": mode });
        self.send(self.request(Method::PATCH, &["configs"]).json(&body))
            .await?;
        Ok(())
    }

    pub async fn proxies(&self) -> Result<Proxies> {
        self.get(&["proxies"]).await
    }

    pub async fn select(&self, group: &str, name: &str) -> Result<()> {
        let body = serde_json::json!({ "name": name });
        self.send(self.request(Method::PUT, &["proxies", group]).json(&body))
            .await?;
        Ok(())
    }

    pub async fn delay(&self, name: &str, url: &str, timeout_ms: u32) -> Result<u32> {
        let mut endpoint = self.url(&["proxies", name, "delay"]);
        endpoint
            .query_pairs_mut()
            .append_pair("url", url)
            .append_pair("timeout", &timeout_ms.to_string());
        let mut builder = self
            .http
            .get(endpoint)
            // The daemon samples up to three times per test.
            .timeout(Duration::from_millis(u64::from(timeout_ms) * 4 + 2000));
        if !self.secret.is_empty() {
            builder = builder.bearer_auth(&self.secret);
        }
        let response = builder.send().await?;
        let body: DelayResponse = response.json().await?;
        match body.delay {
            Some(delay) if delay > 0 => Ok(delay),
            _ => Err(anyhow!(
                body.message.unwrap_or_else(|| "timeout".to_owned())
            )),
        }
    }

    pub async fn connections(&self) -> Result<Connections> {
        self.get(&["connections"]).await
    }

    pub async fn close_connection(&self, id: &str) -> Result<()> {
        self.send(self.request(Method::DELETE, &["connections", id]))
            .await?;
        Ok(())
    }

    pub async fn close_all_connections(&self) -> Result<()> {
        self.send(self.request(Method::DELETE, &["connections"]))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_are_escaped() {
        let client = ClashClient::new(&ClashApi {
            url: "http://127.0.0.1:9090".to_owned(),
            secret: String::new(),
        })
        .unwrap();
        assert_eq!(
            client.url(&["proxies", "sub/hk 01", "delay"]).as_str(),
            "http://127.0.0.1:9090/proxies/sub%2Fhk%2001/delay"
        );
    }

    #[test]
    fn parse_proxies() {
        let json = r#"{"proxies":{"proxy":{"type":"Selector","name":"proxy","udp":true,
            "history":[{"time":"2026-10-05T00:00:00Z","delay":120}],"now":"a","all":["a","b"]},
            "a":{"type":"Direct","name":"a","history":[]}}}"#;
        let proxies: Proxies = serde_json::from_str(json).unwrap();
        let group = &proxies.proxies["proxy"];
        assert!(group.is_group() && group.is_selectable());
        assert_eq!(group.last_delay(), Some(120));
        assert!(!proxies.proxies["a"].is_group());
    }

    #[test]
    fn parse_connections() {
        let json = r#"{"downloadTotal":10,"uploadTotal":5,"memory":1,"connections":[{"id":"x",
            "metadata":{"network":"tcp","type":"mixed/mixed-in","sourceIP":"127.0.0.1",
            "destinationIP":"","sourcePort":"5000","destinationPort":"443","host":"example.com",
            "processPath":""},"upload":1,"download":2,"start":"2026-10-05T00:00:00Z",
            "chains":["direct"],"rule":"final"}]}"#;
        let connections: Connections = serde_json::from_str(json).unwrap();
        let first = &connections.connections.unwrap()[0];
        assert_eq!(first.target(), "example.com:443");
        assert_eq!(first.chains, ["direct"]);
    }
}
