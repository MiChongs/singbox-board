//! sing-box configuration documents shared by the daemon's profile store and
//! its clients: lenient parsing, the template for new profiles and the
//! overview shown next to a profile.

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::i18n::fl;
use crate::protocol::ProfileUsage;
use crate::util::fmt_bytes;

/// Upper bound for the content of one profile.
pub const MAX_PROFILE_BYTES: usize = 8 * 1024 * 1024;

/// Removes `//`, `#` and `/* */` comments plus trailing commas, which the
/// sing-box parser accepts but strict JSON does not. Line breaks inside
/// block comments are kept so parse errors point at the right line.
pub fn strip_json_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            out.push(c);
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    if bytes[i] == b'\n' {
                        out.push(b'\n');
                    }
                    i += 1;
                }
                i += 2;
            }
            b']' | b'}' => {
                // Drop a trailing comma (and the whitespace after it) before the closer.
                let mut end = out.len();
                while end > 0 && out[end - 1].is_ascii_whitespace() {
                    end -= 1;
                }
                if end > 0 && out[end - 1] == b',' {
                    out.remove(end - 1);
                }
                out.push(c);
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Whether saving `text` as plain JSON would lose comments or trailing commas.
pub fn has_comments(text: &str) -> bool {
    strip_json_comments(text) != text
}

/// Parses a configuration the way sing-box does (comments allowed) and
/// requires a JSON object at the top level.
pub fn parse(text: &str) -> Result<Value> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim().is_empty() {
        bail!(fl!("profile-empty"));
    }
    let value: Value = serde_json::from_str(&strip_json_comments(text)).map_err(|err| {
        if looks_like_subscription(text) {
            anyhow!(fl!("profile-node-list"))
        } else {
            anyhow!(fl!("profile-invalid-json", error = err.to_string()))
        }
    })?;
    if !value.is_object() {
        bail!(fl!("profile-not-object"));
    }
    Ok(value)
}

/// A base64 node list or share links, which providers serve to clients that
/// do not identify as sing-box.
fn looks_like_subscription(text: &str) -> bool {
    let text = text.trim();
    let links = [
        "vmess://",
        "vless://",
        "ss://",
        "trojan://",
        "hysteria2://",
        "tuic://",
    ];
    if text
        .lines()
        .any(|line| links.iter().any(|l| line.starts_with(l)))
    {
        return true;
    }
    text.len() > 32
        && !text.starts_with(['{', '['])
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=_-\r\n".contains(c))
}

/// Pretty JSON with a final newline, the format profiles edited in the
/// dashboard are saved in.
pub fn to_text(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// A working starting point: a local mixed proxy port, a selector to put
/// nodes in, split DNS and the Clash API the dashboard talks to.
pub fn template(secret: &str) -> Value {
    json!({
        "log": { "level": "info", "timestamp": true },
        "dns": {
            "servers": [
                { "type": "https", "tag": "remote", "server": "1.1.1.1", "detour": "proxy" },
                { "type": "udp", "tag": "local", "server": "223.5.5.5" }
            ],
            "final": "remote",
            "strategy": "prefer_ipv4"
        },
        "inbounds": [
            { "type": "mixed", "tag": "mixed-in", "listen": "127.0.0.1", "listen_port": 7890 }
        ],
        "outbounds": [
            { "type": "selector", "tag": "proxy", "outbounds": ["direct"] },
            { "type": "direct", "tag": "direct" }
        ],
        "route": {
            "rules": [
                { "action": "sniff" },
                { "protocol": "dns", "action": "hijack-dns" },
                { "ip_is_private": true, "outbound": "direct" }
            ],
            "final": "proxy",
            "auto_detect_interface": true,
            "default_domain_resolver": "local"
        },
        "experimental": {
            "cache_file": { "enabled": true },
            "clash_api": { "external_controller": "127.0.0.1:9090", "secret": secret }
        }
    })
}

/// How often a remote profile is downloaded, e.g. "every 24 h".
pub fn interval_label(minutes: u64) -> String {
    match minutes {
        0 => fl!("profile-interval-manual"),
        m if m % 60 == 0 => fl!("profile-interval-hours", hours = (m / 60).to_string()),
        m => fl!("profile-interval-minutes", minutes = m.to_string()),
    }
}

/// "12.0 GiB of 200.0 GiB used, expires 2026-12-01".
pub fn usage_label(usage: &ProfileUsage) -> String {
    let used = fmt_bytes(usage.upload.saturating_add(usage.download));
    let mut text = if usage.total > 0 {
        fl!("profile-usage", used = used, total = fmt_bytes(usage.total))
    } else {
        fl!("profile-usage-unlimited", used = used)
    };
    if let Some(expire) = usage.expire {
        text = format!(
            "{text}{}{}",
            fl!("clause-separator"),
            fl!("profile-expires", date = local_time(expire, "%Y-%m-%d"))
        );
    }
    text
}

/// A unix timestamp in local time with a chrono format.
pub fn local_time(unix: u64, format: &str) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(unix as i64, 0)
        .single()
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}

/// What a configuration sets up, for the overview next to a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub log_level: Option<String>,
    /// e.g. "mixed 127.0.0.1:7890", "tun".
    pub inbounds: Vec<String>,
    pub outbounds: usize,
    /// Outbound types and how often each occurs, most frequent first.
    pub outbound_types: Vec<(String, usize)>,
    /// Tags of selector and urltest groups.
    pub groups: Vec<String>,
    pub endpoints: usize,
    /// Tags of `providers` (MiChongs/sing-box).
    pub providers: Vec<String>,
    pub dns_servers: Vec<String>,
    pub dns_final: Option<String>,
    pub rules: usize,
    pub rule_sets: usize,
    pub route_final: Option<String>,
    pub clash_api: Option<String>,
    pub cache_file: bool,
}

fn items<'a>(value: &'a Value, pointer: &str) -> &'a [Value] {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn text(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A short label for one array item: its tag or name and its type.
pub fn item_label(item: &Value) -> Option<String> {
    let object = item.as_object()?;
    let name = ["tag", "name"].iter().find_map(|k| text(item, k));
    let kind = text(item, "type");
    match (name, kind) {
        (Some(name), Some(kind)) => Some(format!("{name} ({kind})")),
        (Some(name), None) => Some(name),
        (None, Some(kind)) => Some(kind),
        (None, None) => rule_label(object),
    }
}

/// `domain_suffix, rule_set → proxy` for route and DNS rules.
fn rule_label(rule: &Map<String, Value>) -> Option<String> {
    let target = ["outbound", "server"]
        .iter()
        .find_map(|k| rule.get(*k).and_then(Value::as_str).map(str::to_owned))
        .or_else(|| {
            rule.get("action")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let matchers: Vec<&str> = rule
        .keys()
        .map(String::as_str)
        .filter(|k| !matches!(*k, "outbound" | "server" | "action" | "invert"))
        .take(3)
        .collect();
    match (matchers.is_empty(), target) {
        (true, Some(target)) => Some(target),
        (false, Some(target)) => Some(format!("{} → {target}", matchers.join(", "))),
        (false, None) => Some(matchers.join(", ")),
        (true, None) => None,
    }
}

pub fn summarize(config: &Value) -> Summary {
    let inbounds = items(config, "/inbounds")
        .iter()
        .map(|inbound| {
            let kind = text(inbound, "type").unwrap_or_else(|| "?".to_owned());
            match (text(inbound, "listen"), text(inbound, "listen_port")) {
                (Some(listen), Some(port)) if listen.contains(':') => {
                    format!("{kind} [{listen}]:{port}")
                }
                (Some(listen), Some(port)) => format!("{kind} {listen}:{port}"),
                (None, Some(port)) => format!("{kind} :{port}"),
                _ => kind,
            }
        })
        .collect();
    let outbounds = items(config, "/outbounds");
    let mut outbound_types: Vec<(String, usize)> = Vec::new();
    for kind in outbounds.iter().filter_map(|o| text(o, "type")) {
        match outbound_types.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, count)) => *count += 1,
            None => outbound_types.push((kind, 1)),
        }
    }
    outbound_types.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let groups = outbounds
        .iter()
        .filter(|o| matches!(text(o, "type").as_deref(), Some("selector" | "urltest")))
        .filter_map(|o| text(o, "tag"))
        .collect();
    let tags = |pointer: &str| -> Vec<String> {
        items(config, pointer)
            .iter()
            .filter_map(|item| text(item, "tag"))
            .collect()
    };
    Summary {
        log_level: config
            .pointer("/log/level")
            .and_then(Value::as_str)
            .map(str::to_owned),
        inbounds,
        outbounds: outbounds.len(),
        outbound_types,
        groups,
        endpoints: items(config, "/endpoints").len(),
        providers: tags("/providers"),
        dns_servers: tags("/dns/servers"),
        dns_final: config
            .pointer("/dns/final")
            .and_then(Value::as_str)
            .map(str::to_owned),
        rules: items(config, "/route/rules").len(),
        rule_sets: items(config, "/route/rule_set").len(),
        route_final: config
            .pointer("/route/final")
            .and_then(Value::as_str)
            .map(str::to_owned),
        clash_api: config
            .pointer("/experimental/clash_api/external_controller")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cache_file: config
            .pointer("/experimental/cache_file/enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_trailing_commas() {
        let input = r#"{
            // line comment
            "a": "http://x/#not-a-comment", # hash comment
            /* block
               comment */
            "b": [1, 2,],
        }"#;
        let value: Value = serde_json::from_str(&strip_json_comments(input)).unwrap();
        assert_eq!(value["a"], "http://x/#not-a-comment");
        assert_eq!(value["b"], json!([1, 2]));
        assert!(has_comments(input));
        assert!(!has_comments(r#"{"a": "// not a comment"}"#));
    }

    #[test]
    fn block_comments_keep_line_numbers() {
        let input = "{\n/* one\ntwo\nthree */\n\"a\": oops\n}";
        let err = parse(input).unwrap_err().to_string();
        assert!(err.contains("line 5"), "{err}");
    }

    #[test]
    fn parse_requires_an_object() {
        assert!(parse("[1, 2]").is_err());
        assert!(parse("  ").is_err());
        assert!(parse("\u{feff}{\"log\": {}}").is_ok());
        let nodes = parse("dm1lc3M6Ly9leGFtcGxlLmNvbTo0NDM/dHlwZT10Y3AjZXhhbXBsZQ==").unwrap_err();
        assert_eq!(nodes.to_string(), fl!("profile-node-list"));
        assert!(parse("vless://uuid@host:443#a\nss://abc@host:1#b").is_err());
    }

    #[test]
    fn template_summary() {
        let config = template("s3cret");
        let summary = summarize(&config);
        assert_eq!(summary.inbounds, ["mixed 127.0.0.1:7890"]);
        assert_eq!(summary.outbounds, 2);
        assert_eq!(summary.groups, ["proxy"]);
        assert_eq!(summary.dns_servers, ["remote", "local"]);
        assert_eq!(summary.rules, 3);
        assert_eq!(summary.route_final.as_deref(), Some("proxy"));
        assert_eq!(summary.clash_api.as_deref(), Some("127.0.0.1:9090"));
        assert!(summary.cache_file);
        // Keys keep their order through a round trip.
        let text = to_text(&config);
        assert!(text.find("\"log\"").unwrap() < text.find("\"dns\"").unwrap());
        assert_eq!(parse(&text).unwrap(), config);
    }

    #[test]
    fn item_labels() {
        assert_eq!(
            item_label(&json!({"type": "vless", "tag": "hk-1"})).as_deref(),
            Some("hk-1 (vless)")
        );
        assert_eq!(
            item_label(&json!({"domain_suffix": ["cn"], "outbound": "direct"})).as_deref(),
            Some("domain_suffix → direct")
        );
        assert_eq!(
            item_label(&json!({"action": "sniff"})).as_deref(),
            Some("sniff")
        );
        assert_eq!(item_label(&json!(3)), None);
    }
}
