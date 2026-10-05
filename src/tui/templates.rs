//! What the editor offers when adding to a profile: sing-box building blocks
//! for the well-known lists and sections, plain JSON values, and the tags a
//! reference field can point at.

use serde_json::{Value, json};

use super::editor::{Path, Seg, get};
use crate::i18n::fl;
use crate::substore::Overview;

pub struct Template {
    pub label: String,
    pub detail: String,
    pub value: Value,
}

fn item(label: &str, detail: String, value: Value) -> Template {
    Template {
        label: label.to_owned(),
        detail,
        value,
    }
}

/// `/outbounds/3/outbounds` → `/outbounds/*/outbounds`.
fn pattern(path: &[Seg]) -> String {
    path.iter()
        .map(|seg| match seg {
            Seg::Key(key) => format!("/{key}"),
            Seg::Index(_) => "/*".to_owned(),
        })
        .collect()
}

const TLS: fn() -> Value = || json!({ "enabled": true, "server_name": "example.com" });

/// Building blocks for the array at `path`.
pub fn array_items(path: &[Seg], sub_store: Option<&Overview>) -> Vec<Template> {
    match pattern(path).as_str() {
        "/inbounds" => vec![
            item(
                "mixed",
                fl!("tpl-mixed"),
                json!({"type": "mixed", "tag": "mixed-in", "listen": "127.0.0.1", "listen_port": 7890}),
            ),
            item(
                "tun",
                fl!("tpl-tun"),
                json!({"type": "tun", "tag": "tun-in", "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"], "auto_route": true, "strict_route": true}),
            ),
            item(
                "socks",
                fl!("tpl-socks-in"),
                json!({"type": "socks", "tag": "socks-in", "listen": "127.0.0.1", "listen_port": 1080}),
            ),
            item(
                "http",
                fl!("tpl-http-in"),
                json!({"type": "http", "tag": "http-in", "listen": "127.0.0.1", "listen_port": 8080}),
            ),
            item(
                "tproxy",
                fl!("tpl-tproxy"),
                json!({"type": "tproxy", "tag": "tproxy-in", "listen": "::", "listen_port": 7893}),
            ),
            item(
                "redirect",
                fl!("tpl-redirect"),
                json!({"type": "redirect", "tag": "redirect-in", "listen": "::", "listen_port": 7892}),
            ),
        ],
        "/outbounds" => vec![
            item(
                "selector",
                fl!("tpl-selector"),
                json!({"type": "selector", "tag": "select", "outbounds": ["direct"]}),
            ),
            item(
                "urltest",
                fl!("tpl-urltest"),
                json!({"type": "urltest", "tag": "auto", "outbounds": ["direct"], "url": "https://www.gstatic.com/generate_204", "interval": "3m", "tolerance": 50}),
            ),
            item(
                "direct",
                fl!("tpl-direct"),
                json!({"type": "direct", "tag": "direct"}),
            ),
            item(
                "shadowsocks",
                fl!("tpl-node"),
                json!({"type": "shadowsocks", "tag": "ss", "server": "example.com", "server_port": 8388, "method": "aes-256-gcm", "password": ""}),
            ),
            item(
                "vless",
                fl!("tpl-vless"),
                json!({"type": "vless", "tag": "vless", "server": "example.com", "server_port": 443, "uuid": "", "flow": "xtls-rprx-vision", "tls": {"enabled": true, "server_name": "example.com", "utls": {"enabled": true, "fingerprint": "chrome"}, "reality": {"enabled": true, "public_key": "", "short_id": ""}}}),
            ),
            item(
                "vmess",
                fl!("tpl-node"),
                json!({"type": "vmess", "tag": "vmess", "server": "example.com", "server_port": 443, "uuid": "", "security": "auto", "tls": TLS()}),
            ),
            item(
                "trojan",
                fl!("tpl-node"),
                json!({"type": "trojan", "tag": "trojan", "server": "example.com", "server_port": 443, "password": "", "tls": TLS()}),
            ),
            item(
                "hysteria2",
                fl!("tpl-node"),
                json!({"type": "hysteria2", "tag": "hy2", "server": "example.com", "server_port": 443, "password": "", "tls": TLS()}),
            ),
            item(
                "tuic",
                fl!("tpl-node"),
                json!({"type": "tuic", "tag": "tuic", "server": "example.com", "server_port": 443, "uuid": "", "password": "", "congestion_control": "bbr", "tls": {"enabled": true, "server_name": "example.com", "alpn": ["h3"]}}),
            ),
            item(
                "anytls",
                fl!("tpl-node"),
                json!({"type": "anytls", "tag": "anytls", "server": "example.com", "server_port": 443, "password": "", "tls": TLS()}),
            ),
            item(
                "socks",
                fl!("tpl-upstream"),
                json!({"type": "socks", "tag": "socks-out", "server": "127.0.0.1", "server_port": 1080}),
            ),
            item(
                "http",
                fl!("tpl-upstream"),
                json!({"type": "http", "tag": "http-out", "server": "127.0.0.1", "server_port": 8080}),
            ),
        ],
        "/endpoints" => vec![item(
            "wireguard",
            fl!("tpl-wireguard"),
            json!({"type": "wireguard", "tag": "wg", "address": ["10.0.0.2/32"], "private_key": "", "peers": [{"address": "example.com", "port": 51820, "public_key": "", "allowed_ips": ["0.0.0.0/0", "::/0"]}]}),
        )],
        "/route/rules" => vec![
            item("sniff", fl!("tpl-sniff"), json!({"action": "sniff"})),
            item(
                "hijack-dns",
                fl!("tpl-hijack-dns"),
                json!({"protocol": "dns", "action": "hijack-dns"}),
            ),
            item(
                "ip_is_private",
                fl!("tpl-private-direct"),
                json!({"ip_is_private": true, "outbound": "direct"}),
            ),
            item(
                "domain_suffix",
                fl!("tpl-domain-rule"),
                json!({"domain_suffix": ["example.com"], "outbound": "proxy"}),
            ),
            item(
                "rule_set",
                fl!("tpl-rule-set-rule"),
                json!({"rule_set": ["geosite-cn"], "outbound": "direct"}),
            ),
            item(
                "process_name",
                fl!("tpl-process-rule"),
                json!({"process_name": ["example"], "outbound": "direct"}),
            ),
            item(
                "reject",
                fl!("tpl-reject"),
                json!({"rule_set": ["geosite-category-ads-all"], "action": "reject"}),
            ),
            item(
                "clash_mode",
                fl!("tpl-clash-mode"),
                json!({"clash_mode": "direct", "outbound": "direct"}),
            ),
        ],
        "/route/rule_set" => vec![
            item(
                "geosite-cn",
                fl!("tpl-remote-rule-set"),
                json!({"type": "remote", "tag": "geosite-cn", "format": "binary", "url": "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-cn.srs"}),
            ),
            item(
                "geoip-cn",
                fl!("tpl-remote-rule-set"),
                json!({"type": "remote", "tag": "geoip-cn", "format": "binary", "url": "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-cn.srs"}),
            ),
            item(
                "geosite-category-ads-all",
                fl!("tpl-remote-rule-set"),
                json!({"type": "remote", "tag": "geosite-category-ads-all", "format": "binary", "url": "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-category-ads-all.srs"}),
            ),
            item(
                "local",
                fl!("tpl-local-rule-set"),
                json!({"type": "local", "tag": "my-rules", "format": "source", "path": "rules.json"}),
            ),
            item(
                "inline",
                fl!("tpl-inline-rule-set"),
                json!({"type": "inline", "tag": "inline-rules", "rules": [{"domain_suffix": ["example.com"]}]}),
            ),
        ],
        "/dns/servers" => vec![
            item(
                "udp",
                fl!("tpl-dns-udp"),
                json!({"type": "udp", "tag": "dns-udp", "server": "223.5.5.5"}),
            ),
            item(
                "https",
                fl!("tpl-dns-https"),
                json!({"type": "https", "tag": "dns-https", "server": "1.1.1.1"}),
            ),
            item(
                "tls",
                fl!("tpl-dns-tls"),
                json!({"type": "tls", "tag": "dns-tls", "server": "1.1.1.1"}),
            ),
            item(
                "quic",
                fl!("tpl-dns-quic"),
                json!({"type": "quic", "tag": "dns-quic", "server": "94.140.14.140"}),
            ),
            item(
                "local",
                fl!("tpl-dns-local"),
                json!({"type": "local", "tag": "dns-local"}),
            ),
            item(
                "fakeip",
                fl!("tpl-dns-fakeip"),
                json!({"type": "fakeip", "tag": "fakeip", "inet4_range": "198.18.0.0/15", "inet6_range": "fc00::/18"}),
            ),
            item(
                "dhcp",
                fl!("tpl-dns-dhcp"),
                json!({"type": "dhcp", "tag": "dns-dhcp"}),
            ),
        ],
        "/dns/rules" => vec![
            item(
                "rule_set",
                fl!("tpl-dns-rule-set"),
                json!({"rule_set": ["geosite-cn"], "server": "dns-local"}),
            ),
            item(
                "query_type",
                fl!("tpl-dns-fakeip-rule"),
                json!({"query_type": ["A", "AAAA"], "server": "fakeip"}),
            ),
            item(
                "clash_mode",
                fl!("tpl-clash-mode"),
                json!({"clash_mode": "direct", "server": "dns-local"}),
            ),
        ],
        "/providers" => {
            let mut items = vec![item(
                "remote",
                fl!("tpl-provider"),
                provider("provider", ""),
            )];
            for entry in sub_store.map(|o| o.entries.as_slice()).unwrap_or_default() {
                items.push(item(
                    &entry.name,
                    fl!("tpl-sub-store-provider", kind = entry.kind.label()),
                    provider(&entry.name, &entry.singbox_url),
                ));
            }
            items
        }
        _ => Vec::new(),
    }
}

/// A `providers` entry of MiChongs/sing-box, as Sub-Store suggests it.
fn provider(tag: &str, url: &str) -> Value {
    json!({
        "type": "remote",
        "tag": tag,
        "url": url,
        "update_interval": "1h",
        "health_check": {
            "enabled": true,
            "url": "https://www.gstatic.com/generate_204",
            "interval": "10m"
        }
    })
}

/// Well-known members of the object at `path`, keyed by member name.
pub fn object_members(path: &[Seg]) -> Vec<(&'static str, Value)> {
    let pattern = pattern(path);
    match pattern.as_str() {
        "" => vec![
            ("log", json!({"level": "info", "timestamp": true})),
            ("dns", json!({"servers": [], "rules": []})),
            ("ntp", json!({"enabled": true, "server": "time.apple.com"})),
            ("endpoints", json!([])),
            ("inbounds", json!([])),
            ("outbounds", json!([])),
            ("route", json!({"rules": [], "rule_set": []})),
            ("experimental", json!({})),
            ("providers", json!([])),
        ],
        "/log" => vec![
            ("level", json!("info")),
            ("timestamp", json!(true)),
            ("disabled", json!(false)),
        ],
        "/experimental" => vec![
            ("cache_file", json!({"enabled": true})),
            (
                "clash_api",
                json!({"external_controller": "127.0.0.1:9090", "secret": ""}),
            ),
        ],
        "/experimental/clash_api" => vec![
            ("external_controller", json!("127.0.0.1:9090")),
            ("secret", json!("")),
            ("external_ui", json!("ui")),
            ("default_mode", json!("rule")),
        ],
        "/route" => vec![
            ("rules", json!([])),
            ("rule_set", json!([])),
            ("final", json!("proxy")),
            ("auto_detect_interface", json!(true)),
            ("default_domain_resolver", json!("dns-local")),
        ],
        "/dns" => vec![
            ("servers", json!([])),
            ("rules", json!([])),
            ("final", json!("")),
            ("strategy", json!("prefer_ipv4")),
        ],
        p if p.ends_with("/tls") => vec![
            ("enabled", json!(true)),
            ("server_name", json!("example.com")),
            ("insecure", json!(false)),
            ("alpn", json!(["h2", "http/1.1"])),
            ("utls", json!({"enabled": true, "fingerprint": "chrome"})),
            (
                "reality",
                json!({"enabled": true, "public_key": "", "short_id": ""}),
            ),
        ],
        p if p.starts_with("/outbounds/*") && p.matches('/').count() == 2 => vec![
            ("detour", json!("")),
            (
                "tls",
                json!({"enabled": true, "server_name": "example.com"}),
            ),
            ("transport", json!({"type": "ws", "path": "/"})),
            ("multiplex", json!({"enabled": true, "protocol": "h2mux"})),
        ],
        _ => Vec::new(),
    }
}

/// Plain JSON values with localized names.
pub fn generic() -> Vec<Template> {
    vec![
        item("\"\"", fl!("tpl-text"), json!("")),
        item("0", fl!("tpl-number"), json!(0)),
        item("true", fl!("tpl-switch"), json!(true)),
        item("{}", fl!("tpl-object"), json!({})),
        item("[]", fl!("tpl-list"), json!([])),
        item("null", fl!("tpl-null"), Value::Null),
    ]
}

/// The tags a reference at `path` can point at: outbounds for `outbound`,
/// `detour` and route `final`, DNS servers for DNS rules, rule sets for
/// `rule_set`, and the members of a group. `None` for other fields.
pub fn references(root: &Value, path: &[Seg]) -> Option<Vec<String>> {
    let pattern = pattern(path);
    let key = path.iter().rev().find_map(|seg| match seg {
        Seg::Key(key) => Some(key.as_str()),
        Seg::Index(_) => None,
    })?;
    let in_dns = pattern.starts_with("/dns");
    let tags = |pointers: &[&str]| -> Vec<String> {
        pointers
            .iter()
            .flat_map(|pointer| {
                root.pointer(pointer)
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
            })
            .filter_map(|item| item.get("tag").and_then(Value::as_str).map(str::to_owned))
            .collect()
    };
    let outbounds = || tags(&["/outbounds", "/endpoints"]);
    let found = match key {
        "outbound" | "detour" | "download_detour" | "default" => outbounds(),
        "final" if !in_dns => outbounds(),
        "final" | "server" if in_dns && !pattern.starts_with("/dns/servers") => {
            tags(&["/dns/servers"])
        }
        "domain_resolver" | "default_domain_resolver" => tags(&["/dns/servers"]),
        // `rule_set` inside a route or DNS rule, not the definitions list.
        "rule_set" if pattern.contains("/rules/") => tags(&["/route/rule_set"]),
        // Members of a selector or urltest group, but not the group itself.
        "outbounds" if pattern.starts_with("/outbounds/*/outbounds") => {
            let own = path
                .get(..2)
                .and_then(|group| get(root, group))
                .and_then(|group| group.get("tag"))
                .and_then(Value::as_str);
            outbounds()
                .into_iter()
                .filter(|tag| Some(tag.as_str()) != own)
                .collect()
        }
        "providers" if pattern.starts_with("/outbounds/*/providers") => tags(&["/providers"]),
        _ => return None,
    };
    Some(found)
}

/// Whether new items of the array at `path` are references (tags as strings).
pub fn holds_references(root: &Value, path: &Path) -> bool {
    let mut probe = path.clone();
    probe.push(Seg::Index(0));
    references(root, &probe).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> Seg {
        Seg::Key(k.to_owned())
    }

    #[test]
    fn templates_follow_the_location() {
        assert!(!array_items(&[key("outbounds")], None).is_empty());
        assert!(
            array_items(&[key("route"), key("rules")], None)
                .iter()
                .any(|t| t.value["action"] == "sniff")
        );
        assert!(array_items(&[key("log")], None).is_empty());
        assert!(
            object_members(&[])
                .iter()
                .any(|(k, _)| *k == "experimental")
        );
        assert!(
            object_members(&[key("outbounds"), Seg::Index(2), key("tls")])
                .iter()
                .any(|(k, _)| *k == "reality")
        );
    }

    #[test]
    fn references_offer_existing_tags() {
        let root = json!({
            "outbounds": [
                {"type": "selector", "tag": "proxy", "outbounds": ["hk"]},
                {"type": "vless", "tag": "hk"},
                {"type": "direct", "tag": "direct"}
            ],
            "dns": {"servers": [{"tag": "local"}], "rules": [{"server": "local"}]},
            "route": {"rules": [{"outbound": "direct", "rule_set": ["cn"]}], "rule_set": [{"tag": "cn"}], "final": "proxy"}
        });
        let rule = vec![key("route"), key("rules"), Seg::Index(0)];
        let mut outbound = rule.clone();
        outbound.push(key("outbound"));
        assert_eq!(
            references(&root, &outbound).unwrap(),
            ["proxy", "hk", "direct"]
        );
        let mut sets = rule.clone();
        sets.extend([key("rule_set"), Seg::Index(0)]);
        assert_eq!(references(&root, &sets).unwrap(), ["cn"]);
        let dns = vec![key("dns"), key("rules"), Seg::Index(0), key("server")];
        assert_eq!(references(&root, &dns).unwrap(), ["local"]);
        let members = vec![key("outbounds"), Seg::Index(0), key("outbounds")];
        assert!(holds_references(&root, &members));
        let mut member = members.clone();
        member.push(Seg::Index(0));
        assert_eq!(references(&root, &member).unwrap(), ["hk", "direct"]);
        assert!(references(&root, &[key("log"), key("level")]).is_none());
        assert!(!holds_references(
            &root,
            &vec![key("route"), key("rule_set")]
        ));
        assert!(
            references(
                &root,
                &[key("dns"), key("servers"), Seg::Index(0), key("server")]
            )
            .is_none()
        );
    }
}
