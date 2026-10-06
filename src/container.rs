//! kurumi-containerd container configurations (TOML), shared by the daemon
//! and the configuration editor.
//!
//! The runtime enforces its strict schema itself (and the daemon asks it to
//! check every configuration it stores); this module reads only what is
//! needed to show and track a container, and leniently, so that a newer
//! runtime with more settings still works.

use std::net::Ipv4Addr;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use toml::{Table, Value};

use crate::i18n::fl;
use crate::protocol::{BindMount, ContainerSpec, PortForward};

pub const NETWORKS: [&str; 5] = ["host", "none", "nat", "gateway", "dhcp"];
pub const DEFAULT_BRIDGE: &str = "kurumi-br0";
pub const DEFAULT_ADDRESS: Ipv4Addr = Ipv4Addr::new(172, 28, 0, 2);
pub const DEFAULT_GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 28, 0, 1);
pub const DEFAULT_PREFIX: u8 = 16;

/// Upper bound for a configuration; they are a few hundred bytes.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// Parses `content`, the configuration at `file` (relative host paths are
/// relative to its directory), and checks what every configuration needs.
pub fn parse(content: &str, file: &Path) -> Result<ContainerSpec> {
    if content.len() > MAX_CONFIG_BYTES {
        bail!(fl!(
            "containers-config-too-large",
            limit = crate::util::fmt_bytes(MAX_CONFIG_BYTES as u64)
        ));
    }
    let document: Table = content.parse().map_err(|err: toml::de::Error| {
        let error = err.message().trim().to_owned();
        anyhow::anyhow!(match location(content, err.span()) {
            Some((line, column)) => fl!(
                "containers-config-invalid-toml",
                error = error,
                line = line,
                column = column
            ),
            None => fl!("containers-config-invalid-toml-plain", error = error),
        })
    })?;
    let base = file.parent().unwrap_or(Path::new("/"));
    let runtime = match document.get("runtime") {
        Some(Value::Table(table)) => table,
        Some(_) => bail!(fl!("containers-config-not-table", key = "runtime")),
        None => bail!(fl!("containers-config-missing", key = "[runtime]")),
    };
    let container = match document.get("container") {
        Some(Value::Table(table)) => table,
        Some(_) => bail!(fl!("containers-config-not-table", key = "container")),
        None => bail!(fl!("containers-config-missing", key = "[container]")),
    };
    let name = string(container, "name")
        .ok_or_else(|| anyhow::anyhow!(fl!("containers-config-missing", key = "container.name")))?;
    if !valid_name(&name) {
        bail!(fl!("containers-config-bad-name", name = name.clone()));
    }
    let (rootfs, image) = match (
        string(container, "rootfs"),
        string(container, "rootfs_image"),
    ) {
        (Some(dir), None) => (resolve(base, &dir), false),
        (None, Some(image)) => (resolve(base, &image), true),
        _ => bail!(fl!("containers-config-rootfs")),
    };
    let init = string(container, "init").unwrap_or_else(|| "/sbin/init".to_owned());
    if !init.starts_with('/') {
        bail!(fl!("containers-config-init", init = init.clone()));
    }
    let network = string(container, "network").unwrap_or_else(|| "host".to_owned());
    if !NETWORKS.contains(&network.as_str()) {
        bail!(fl!(
            "containers-config-network",
            network = network.clone(),
            valid = NETWORKS.join(", ")
        ));
    }
    let options = table(container, "network_options");
    let address = options
        .and_then(|o| string(o, "address"))
        .unwrap_or_else(|| DEFAULT_ADDRESS.to_string());
    let prefix = options
        .and_then(|o| integer(o, "prefix"))
        .unwrap_or(i64::from(DEFAULT_PREFIX));
    let bridge = options
        .and_then(|o| string(o, "bridge"))
        .unwrap_or_else(|| DEFAULT_BRIDGE.to_owned());
    let gateway_bridge = options
        .and_then(|o| string(o, "gateway_bridge"))
        .unwrap_or_default();
    let ports = options
        .and_then(|o| o.get("ports"))
        .and_then(Value::as_array)
        .map(|ports| {
            ports
                .iter()
                .filter_map(Value::as_table)
                .filter_map(|port| {
                    Some(PortForward {
                        host: u16::try_from(integer(port, "host")?).ok()?,
                        container: u16::try_from(integer(port, "container")?).ok()?,
                        protocol: string(port, "protocol").unwrap_or_else(|| "tcp".to_owned()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mounts = container
        .get("mounts")
        .and_then(Value::as_array)
        .map(|mounts| {
            mounts
                .iter()
                .filter_map(Value::as_table)
                .filter_map(|mount| {
                    Some(BindMount {
                        source: resolve(base, &string(mount, "source")?)
                            .display()
                            .to_string(),
                        target: string(mount, "target")?,
                        read_only: boolean(mount, "read_only").unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let resources = table(container, "resources");
    let limit = |key: &str| {
        resources
            .and_then(|r| integer(r, key))
            .and_then(|v| u64::try_from(v).ok())
    };
    let cpu_limit = match (limit("cpu_quota"), limit("cpu_period")) {
        (Some(quota), Some(period)) if period > 0 => Some(quota.saturating_mul(1000) / period),
        _ => None,
    };
    let hostname = string(container, "hostname")
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| name.clone());
    let installed = installed(&rootfs, image, &init);
    let (address, bridge) = match network.as_str() {
        "nat" => (Some(format!("{address}/{prefix}")), Some(bridge)),
        "gateway" | "dhcp" => (None, Some(gateway_bridge).filter(|b| !b.is_empty())),
        _ => (None, None),
    };
    Ok(ContainerSpec {
        hostname,
        uuid: string(container, "uuid"),
        rootfs: rootfs.display().to_string(),
        image,
        installed,
        init,
        network,
        address,
        bridge,
        ports,
        mounts,
        memory_limit: limit("memory_bytes"),
        cpu_limit,
        pids_limit: limit("pids"),
        foreground: boolean(container, "foreground").unwrap_or(false),
        volatile: boolean(container, "volatile").unwrap_or(false),
        user_namespaces: table(container, "security")
            .and_then(|s| boolean(s, "allow_user_namespaces"))
            .unwrap_or(false),
        stop_timeout: integer(runtime, "stop_timeout_seconds")
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(15),
        name,
    })
}

/// kurumi-containerd's rule for `container.name`: ASCII letters, digits,
/// `.`, `_` and `-`, at most 128 bytes.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether the root filesystem exists: the image file, or the directory
/// with its init.
pub fn installed(rootfs: &Path, image: bool, init: &str) -> bool {
    if image {
        rootfs.is_file()
    } else {
        rootfs.is_dir() && resolve_in_rootfs(rootfs, init).is_some_and(|p| p.exists())
    }
}

/// Adds a random `uuid` to the `[container]` table of a configuration that
/// has none. kurumi-containerd would otherwise add one itself on first use
/// and rewrite the whole file, dropping its comments.
pub fn with_uuid(content: &str) -> String {
    let Ok(document) = content.parse::<Table>() else {
        return content.to_owned();
    };
    let has_uuid = document
        .get("container")
        .and_then(Value::as_table)
        .is_some_and(|c| c.contains_key("uuid"));
    if has_uuid {
        return content.to_owned();
    }
    let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();
    let Some(header) = lines.iter().position(|line| {
        let line = line.trim();
        let line = line.split('#').next().unwrap_or_default().trim_end();
        line == "[container]"
    }) else {
        return content.to_owned();
    };
    lines.insert(header + 1, format!("uuid = \"{}\"", random_uuid()));
    let mut text = lines.join("\n");
    if content.ends_with('\n') {
        text.push('\n');
    }
    // Only keep the change if it still says the same thing plus the uuid.
    match text.parse::<Table>() {
        Ok(changed)
            if changed
                .get("container")
                .and_then(Value::as_table)
                .is_some_and(|c| c.contains_key("uuid")) =>
        {
            text
        }
        _ => content.to_owned(),
    }
}

/// A random (version 4) UUID.
pub fn random_uuid() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("read /dev/urandom");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn table<'a>(table: &'a Table, key: &str) -> Option<&'a Table> {
    table.get(key)?.as_table()
}

fn string(table: &Table, key: &str) -> Option<String> {
    table.get(key)?.as_str().map(str::to_owned)
}

fn integer(table: &Table, key: &str) -> Option<i64> {
    table.get(key)?.as_integer()
}

fn boolean(table: &Table, key: &str) -> Option<bool> {
    table.get(key)?.as_bool()
}

/// Line and column (from 1) where a byte span starts.
pub fn location(content: &str, span: Option<std::ops::Range<usize>>) -> Option<(usize, usize)> {
    let start = span?.start.min(content.len());
    let before = content.get(..start)?;
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    Some((line, column))
}

/// A host path of the configuration, made absolute against its directory
/// and normalised without touching the file system.
pub fn resolve(base: &Path, path: &str) -> PathBuf {
    let joined = base.join(path);
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// `path` inside `rootfs`, following symbolic links the way the container
/// sees them (absolute targets are relative to the rootfs).
pub fn resolve_in_rootfs(rootfs: &Path, path: &str) -> Option<PathBuf> {
    let mut pending: std::collections::VecDeque<std::ffi::OsString> = Path::new(path)
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let mut relative = PathBuf::new();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == "/" {
            relative.clear();
            continue;
        }
        if part == "." {
            continue;
        }
        if part == ".." {
            relative.pop();
            continue;
        }
        relative.push(&part);
        let candidate = rootfs.join(&relative);
        let Ok(meta) = std::fs::symlink_metadata(&candidate) else {
            // Missing components are fine; the caller checks existence.
            continue;
        };
        if meta.file_type().is_symlink() {
            links += 1;
            if links > 40 {
                return None;
            }
            let target = std::fs::read_link(&candidate).ok()?;
            relative.pop();
            for component in target.components().rev() {
                pending.push_front(component.as_os_str().to_owned());
            }
        }
    }
    Some(rootfs.join(relative))
}

/// A `container.name` derived from a display name: lower-case ASCII
/// letters, digits and dashes; `fallback` when nothing is left.
pub fn container_name(display: &str, fallback: &str) -> String {
    let mut name = String::new();
    for c in display.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            name.push(c.to_ascii_lowercase());
        } else if !name.ends_with('-') && !name.is_empty() {
            name.push('-');
        }
    }
    let name: String = name.trim_matches(['-', '.']).chars().take(48).collect();
    if valid_name(&name) {
        name
    } else {
        fallback.to_owned()
    }
}

/// Network of a new container from the template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateNetwork {
    Host,
    Nat(Ipv4Addr),
    None,
}

/// A commented configuration for a new container whose root filesystem
/// will be `./rootfs` next to it.
pub fn template(display: &str, name: &str, network: TemplateNetwork) -> String {
    let (mode, address) = match network {
        TemplateNetwork::Host => ("host", DEFAULT_ADDRESS),
        TemplateNetwork::Nat(address) => ("nat", address),
        TemplateNetwork::None => ("none", DEFAULT_ADDRESS),
    };
    let title = display.replace(['\n', '\r'], " ");
    format!(
        r#"# Container "{title}", run by kurumi-containerd and managed by singbox-board.
# Reference: https://github.com/Tools-cx-app/kurumi-containerd/blob/master/docs/configuration.md

[runtime]
# Seconds to wait for a clean shutdown before the container is killed.
stop_timeout_seconds = 15

[container]
# Identity kurumi-containerd tracks the container by: ASCII letters, digits, '.', '_', '-'.
name = "{name}"
hostname = "{name}"
# Root filesystem: a directory, or an ext4 image (rootfs_image) installed with a size.
rootfs = "./rootfs"
# rootfs_image = "./rootfs.img"
init = "/sbin/init"
# Writable layers on tmpfs: changes are lost when the container stops.
volatile = false
# host: share the host's network, including sing-box's TUN routes.
# nat: own address behind the kurumi-br0 bridge; none: loopback only.
# gateway / dhcp: attach to an existing bridge (network_options.gateway_bridge).
network = "{mode}"

[container.network_options]
address = "{address}"
gateway = "{DEFAULT_GATEWAY}"
prefix = {DEFAULT_PREFIX}
bridge = "{DEFAULT_BRIDGE}"
dns = ["1.1.1.1", "8.8.8.8"]

# Publish a container port on the host (nat mode).
# [[container.network_options.ports]]
# host = 8080
# container = 80
# protocol = "tcp"

[container.resources]
# memory_bytes = 1073741824
# cpu_quota = 100000
# cpu_period = 100000
# pids = 1024

[container.security]
read_only_sys = true
# Needed by Docker (userns-remap), Podman, Flatpak or Bubblewrap inside the
# container; weakens isolation.
allow_user_namespaces = false

[container.environment]
LANG = "C.UTF-8"

# Share a host directory with the container.
# [[container.mounts]]
# source = "/srv/shared"
# target = "/mnt/shared"
# read_only = false
"#
    )
}

/// The first free address on the default NAT bridge, given the addresses
/// other containers use there.
pub fn free_nat_address(taken: &[Ipv4Addr]) -> Ipv4Addr {
    let [a, b, _, _] = DEFAULT_ADDRESS.octets();
    (0u8..=255)
        .flat_map(|third| (2u8..=254).map(move |fourth| Ipv4Addr::new(a, b, third, fourth)))
        .find(|candidate| *candidate != DEFAULT_GATEWAY && !taken.contains(candidate))
        .unwrap_or(DEFAULT_ADDRESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_back() {
        let file = Path::new("/var/lib/singbox-board/containers/ab12cd34/container.toml");
        let text = template(
            "Dev box",
            "dev-box",
            TemplateNetwork::Nat(Ipv4Addr::new(172, 28, 0, 3)),
        );
        let spec = parse(&text, file).unwrap();
        assert_eq!(spec.name, "dev-box");
        assert_eq!(spec.hostname, "dev-box");
        assert_eq!(
            spec.rootfs,
            "/var/lib/singbox-board/containers/ab12cd34/rootfs"
        );
        assert!(!spec.image && !spec.installed && !spec.foreground);
        assert_eq!(spec.network, "nat");
        assert_eq!(spec.address.as_deref(), Some("172.28.0.3/16"));
        assert_eq!(spec.bridge.as_deref(), Some("kurumi-br0"));
        assert_eq!(spec.stop_timeout, 15);
        let host = parse(&template("x", "x", TemplateNetwork::Host), file).unwrap();
        assert_eq!((host.network.as_str(), host.address), ("host", None));
    }

    #[test]
    fn settings_are_read_leniently() {
        let text = r#"
            [runtime]
            stop_timeout_seconds = 30
            some_future_setting = true
            [container]
            name = "web"
            rootfs_image = "../images/web.img"
            network = "nat"
            foreground = true
            [container.network_options]
            address = "10.9.0.5"
            prefix = 24
            ports = [{ host = 8080, container = 80 }, { host = 53, container = 53, protocol = "udp" }]
            [container.resources]
            memory_bytes = 536870912
            cpu_quota = 50000
            cpu_period = 100000
            [[container.mounts]]
            source = "./shared"
            target = "/mnt/shared"
            read_only = true
        "#;
        let spec = parse(text, Path::new("/srv/kurumi/web/web.toml")).unwrap();
        assert_eq!(spec.rootfs, "/srv/kurumi/images/web.img");
        assert!(spec.image && spec.foreground);
        assert_eq!(spec.address.as_deref(), Some("10.9.0.5/24"));
        assert_eq!(spec.ports.len(), 2);
        assert_eq!(spec.ports[1].protocol, "udp");
        assert_eq!(spec.mounts[0].source, "/srv/kurumi/web/shared");
        assert!(spec.mounts[0].read_only);
        assert_eq!(
            (spec.memory_limit, spec.cpu_limit),
            (Some(536_870_912), Some(500))
        );
        assert_eq!(spec.stop_timeout, 30);
        assert_eq!(spec.hostname, "web");
    }

    #[test]
    fn broken_configurations_are_explained() {
        let file = Path::new("/x/c.toml");
        let error = |text: &str| crate::util::error_chain(&parse(text, file).unwrap_err());
        assert_eq!(location("ab\ncd", Some(4..5)), Some((2, 2)));
        assert_eq!(
            error("[runtime]\n[container\nname = 1"),
            fl!(
                "containers-config-invalid-toml",
                error = "unclosed table, expected `]`",
                line = 2,
                column = 11
            )
        );
        assert!(error("[container]\nname = \"a\"\nrootfs = \"r\"").contains("[runtime]"));
        assert!(error("[runtime]\n[container]\nrootfs = \"r\"").contains("container.name"));
        assert!(error("[runtime]\n[container]\nname = \"a b\"\nrootfs = \"r\"").contains("a b"));
        assert_eq!(
            error("[runtime]\n[container]\nname = \"a\""),
            fl!("containers-config-rootfs")
        );
        assert!(
            error("[runtime]\n[container]\nname = \"a\"\nrootfs = \"r\"\nnetwork = \"bridge\"")
                .contains("bridge")
        );
    }

    #[test]
    fn uuids_are_added_without_losing_comments() {
        let text =
            "# keep me\n[runtime]\n\n[container] # the container\nname = \"a\"\nrootfs = \"r\"\n";
        let changed = with_uuid(text);
        assert!(
            changed.starts_with("# keep me\n[runtime]\n\n[container] # the container\nuuid = \"")
        );
        assert!(changed.ends_with("name = \"a\"\nrootfs = \"r\"\n"));
        let spec = parse(&changed, Path::new("/x/c.toml")).unwrap();
        let uuid = spec.uuid.unwrap();
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
        // Already there, or nowhere to put it: unchanged.
        assert_eq!(with_uuid(&changed), changed);
        let inline = "runtime = {}\ncontainer = { name = \"a\", rootfs = \"r\" }\n";
        assert_eq!(with_uuid(inline), inline);
        assert_eq!(with_uuid("not toml ["), "not toml [");
    }

    #[test]
    fn names_and_addresses() {
        assert_eq!(container_name("My Dev Box", "c1"), "my-dev-box");
        assert_eq!(container_name("家里的 debian 12", "c1"), "debian-12");
        assert_eq!(container_name("容器", "c1"), "c1");
        assert_eq!(container_name("--a..", "c1"), "a");
        let taken = [Ipv4Addr::new(172, 28, 0, 2), Ipv4Addr::new(172, 28, 0, 3)];
        assert_eq!(free_nat_address(&taken), Ipv4Addr::new(172, 28, 0, 4));
        assert_eq!(free_nat_address(&[]), DEFAULT_ADDRESS);
    }

    #[test]
    fn paths_resolve_inside_the_rootfs() {
        let root = std::env::temp_dir().join(format!("sbb-rootfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("lib/systemd")).unwrap();
        std::fs::write(root.join("lib/systemd/systemd"), "").unwrap();
        std::fs::create_dir_all(root.join("sbin")).unwrap();
        // An absolute link target means the container's root, not the host's.
        std::os::unix::fs::symlink("/lib/systemd/systemd", root.join("sbin/init")).unwrap();
        let resolved = resolve_in_rootfs(&root, "/sbin/init").unwrap();
        assert_eq!(resolved, root.join("lib/systemd/systemd"));
        assert!(resolved.exists());
        assert_eq!(
            resolve_in_rootfs(&root, "/../../etc/passwd").unwrap(),
            root.join("etc/passwd")
        );
        std::os::unix::fs::symlink("loop", root.join("loop")).unwrap();
        assert!(resolve_in_rootfs(&root, "/loop").is_none());
        assert_eq!(
            resolve(Path::new("/a/b"), "../c/./d"),
            PathBuf::from("/a/c/d")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
