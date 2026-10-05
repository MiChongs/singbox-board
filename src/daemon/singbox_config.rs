//! Reads just enough of the sing-box configuration to locate the Clash API.

use std::path::PathBuf;

use serde_json::Value;

use crate::config::{ClashApiOverride, CoreConfig};
use crate::profile::strip_json_comments;
use crate::protocol::ClashApi;

/// Configuration files in the order sing-box merges them.
fn config_files(core: &CoreConfig) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = core.config.iter().map(|p| core.resolve(p)).collect();
    for dir in &core.config_dir {
        let Ok(entries) = std::fs::read_dir(core.resolve(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Locates `experimental.clash_api` across all configuration files; later
/// files win, matching sing-box's merge order. Overrides take precedence.
pub fn discover_clash_api(core: &CoreConfig, overrides: &ClashApiOverride) -> Option<ClashApi> {
    let mut controller: Option<String> = None;
    let mut secret: Option<String> = None;
    for path in config_files(core) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&strip_json_comments(&content)) else {
            tracing::debug!("cannot parse {} for clash_api discovery", path.display());
            continue;
        };
        let Some(api) = value.pointer("/experimental/clash_api") else {
            continue;
        };
        if let Some(c) = api.get("external_controller").and_then(Value::as_str) {
            controller = Some(c.to_owned());
        }
        if let Some(s) = api.get("secret").and_then(Value::as_str) {
            secret = Some(s.to_owned());
        }
    }
    let url = overrides
        .url
        .clone()
        .or_else(|| controller.as_deref().and_then(controller_url))?;
    Some(ClashApi {
        url: url.trim_end_matches('/').to_owned(),
        secret: overrides.secret.clone().or(secret).unwrap_or_default(),
    })
}

/// Turns a listen address such as `0.0.0.0:9090`, `:9090` or `[::]:9090`
/// into a URL reachable from the local machine.
pub fn controller_url(listen: &str) -> Option<String> {
    let listen = listen.trim();
    if listen.is_empty() {
        return None;
    }
    let (host, port) = listen.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = match host {
        "" | "0.0.0.0" => "127.0.0.1".to_owned(),
        "::" => "[::1]".to_owned(),
        h if h.contains(':') => format!("[{h}]"),
        h => h.to_owned(),
    };
    Some(format!("http://{host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_urls() {
        assert_eq!(
            controller_url("127.0.0.1:9090").unwrap(),
            "http://127.0.0.1:9090"
        );
        assert_eq!(
            controller_url("0.0.0.0:9090").unwrap(),
            "http://127.0.0.1:9090"
        );
        assert_eq!(controller_url(":9090").unwrap(), "http://127.0.0.1:9090");
        assert_eq!(controller_url("[::]:9090").unwrap(), "http://[::1]:9090");
        assert_eq!(
            controller_url("[fd00::1]:9090").unwrap(),
            "http://[fd00::1]:9090"
        );
        assert!(controller_url("").is_none());
        assert!(controller_url("nonsense").is_none());
    }

    #[test]
    fn discovery_merges_files() {
        let dir = std::env::temp_dir().join(format!("sbb-discover-{}", std::process::id()));
        // Sorted after config.json, like sing-box orders merged files by path.
        let conf_d = dir.join("zz.d");
        std::fs::create_dir_all(&conf_d).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"experimental":{"clash_api":{"external_controller":":9090","secret":"a"}}}"#,
        )
        .unwrap();
        std::fs::write(
            conf_d.join("zz.json"),
            r#"{"experimental":{"clash_api":{"secret":"b"}}} // override"#,
        )
        .unwrap();
        let core = CoreConfig {
            config: vec![PathBuf::from("config.json")],
            config_dir: vec![conf_d.clone()],
            working_dir: Some(dir.clone()),
            ..CoreConfig::default()
        };
        let api = discover_clash_api(&core, &ClashApiOverride::default()).unwrap();
        assert_eq!(api.url, "http://127.0.0.1:9090");
        assert_eq!(api.secret, "b");
        let overridden = discover_clash_api(
            &core,
            &ClashApiOverride {
                url: Some("http://10.0.0.1:1234/".to_owned()),
                secret: None,
            },
        )
        .unwrap();
        assert_eq!(overridden.url, "http://10.0.0.1:1234");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
