//! Live state of containers, read from kurumi-containerd's state files and
//! procfs without running the runtime.
//!
//! kurumi-containerd keeps one JSON file per running container in
//! `/run/kurumi-containerd/state/<container.name>.json`. A numeric PID alone
//! is not trusted: like the runtime itself, a state counts only while the
//! boot id matches and either init (start time, PID namespace and the name
//! the runtime wrote inside the container) or the monitor process (start
//! time) is still the process the file describes.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::container::valid_name;
use crate::protocol::ContainerLive;

/// The runtime's work directory on Linux.
pub const WORKDIR: &str = "/run/kurumi-containerd";

/// The parts of the runtime's `ContainerState` the daemon uses.
#[derive(Debug, Clone, Deserialize)]
pub struct State {
    pub name: String,
    pub init_pid: i32,
    pub monitor_pid: i32,
    pub host_boot_id: String,
    pub init_start_time: u64,
    pub pid_namespace_inode: u64,
    pub started_at_unix: u64,
    pub monitor_start_time: u64,
    #[serde(default)]
    pub init_system: String,
    #[serde(default)]
    pub generation: u64,
}

/// Reads procfs below `proc` (`/proc`, or a fake tree in tests).
#[derive(Debug, Clone)]
pub struct Procfs {
    proc: PathBuf,
    workdir: PathBuf,
}

impl Default for Procfs {
    fn default() -> Self {
        Self {
            proc: PathBuf::from("/proc"),
            workdir: PathBuf::from(WORKDIR),
        }
    }
}

/// Running totals of the processes in one PID namespace.
#[derive(Debug, Default, Clone, Copy)]
struct Usage {
    processes: u32,
    memory: u64,
    cpu_ticks: u64,
}

impl Procfs {
    #[cfg(test)]
    pub fn at(proc: PathBuf, workdir: PathBuf) -> Self {
        Self { proc, workdir }
    }

    fn boot_id(&self) -> Option<String> {
        std::fs::read_to_string(self.proc.join("sys/kernel/random/boot_id"))
            .ok()
            .map(|id| id.trim().to_owned())
    }

    /// The state file of the container named `name`, if it is trustworthy.
    fn read_state(&self, name: &str) -> Option<State> {
        if !valid_name(name) {
            return None;
        }
        let path = self.workdir.join("state").join(format!("{name}.json"));
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&path)
            .ok()?;
        let meta = file.metadata().ok()?;
        let parent = std::fs::metadata(path.parent()?).ok()?;
        if !meta.is_file() || meta.uid() != parent.uid() || meta.mode() & 0o022 != 0 {
            return None;
        }
        let state: State = serde_json::from_reader(file).ok()?;
        (state.name == name).then_some(state)
    }

    /// Field 22 of `/proc/<pid>/stat`: start time in clock ticks after boot.
    fn start_time(&self, pid: i32) -> Option<u64> {
        let stat = self.stat(pid)?;
        stat_field(&stat, 22)
    }

    fn stat(&self, pid: i32) -> Option<String> {
        if pid <= 0 {
            return None;
        }
        std::fs::read_to_string(self.proc.join(pid.to_string()).join("stat")).ok()
    }

    fn pid_namespace(&self, pid: &str) -> Option<u64> {
        std::fs::metadata(self.proc.join(pid).join("ns/pid"))
            .ok()
            .map(|meta| meta.ino())
    }

    fn init_matches(&self, state: &State) -> bool {
        let pid = state.init_pid.to_string();
        self.start_time(state.init_pid) == Some(state.init_start_time)
            && self.pid_namespace(&pid) == Some(state.pid_namespace_inode)
            && std::fs::read_to_string(self.proc.join(&pid).join("root/run/kurumi-containerd/name"))
                .is_ok_and(|name| name == state.name)
    }

    fn monitor_matches(&self, state: &State) -> bool {
        self.start_time(state.monitor_pid) == Some(state.monitor_start_time)
    }

    /// Live state of the containers named in `names` that are running,
    /// with their resource usage.
    pub fn live(&self, names: &[&str]) -> HashMap<String, ContainerLive> {
        self.scan(names, true)
    }

    /// Like [`Procfs::live`], without the pass over every process.
    pub fn running(&self, names: &[&str]) -> HashMap<String, ContainerLive> {
        self.scan(names, false)
    }

    fn scan(&self, names: &[&str], with_usage: bool) -> HashMap<String, ContainerLive> {
        let Some(boot_id) = self.boot_id() else {
            return HashMap::new();
        };
        let mut running: Vec<(State, bool)> = Vec::new();
        for name in names {
            let Some(state) = self.read_state(name) else {
                continue;
            };
            if state.host_boot_id != boot_id {
                continue;
            }
            let init = self.init_matches(&state);
            if init || self.monitor_matches(&state) {
                running.push((state, init));
            }
        }
        if running.is_empty() {
            return HashMap::new();
        }
        let wanted: Vec<u64> = running
            .iter()
            .filter(|(_, init)| *init && with_usage)
            .map(|(state, _)| state.pid_namespace_inode)
            .collect();
        let usage = self.usage(&wanted);
        let ticks = clock_ticks();
        running
            .into_iter()
            .map(|(state, init)| {
                let used = usage
                    .get(&state.pid_namespace_inode)
                    .copied()
                    .filter(|_| init)
                    .unwrap_or_default();
                let live = ContainerLive {
                    init_pid: state.init_pid,
                    monitor_pid: state.monitor_pid,
                    init_system: if state.init_system.is_empty() {
                        "unknown".to_owned()
                    } else {
                        state.init_system.clone()
                    },
                    started_at: state.started_at_unix,
                    generation: state.generation,
                    processes: used.processes,
                    memory: used.memory,
                    cpu_ms: used.cpu_ticks.saturating_mul(1000) / ticks,
                    rebooting: !init,
                };
                (state.name, live)
            })
            .collect()
    }

    /// One pass over procfs: process count, resident memory and CPU time
    /// per PID namespace in `namespaces`.
    fn usage(&self, namespaces: &[u64]) -> HashMap<u64, Usage> {
        let mut totals: HashMap<u64, Usage> = HashMap::new();
        if namespaces.is_empty() {
            return totals;
        }
        let Ok(entries) = std::fs::read_dir(&self.proc) else {
            return totals;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name
                .to_str()
                .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            let Some(namespace) = self.pid_namespace(pid) else {
                continue;
            };
            if !namespaces.contains(&namespace) {
                continue;
            }
            let total = totals.entry(namespace).or_default();
            total.processes += 1;
            let dir = self.proc.join(pid);
            if let Ok(status) = std::fs::read_to_string(dir.join("status")) {
                total.memory += vm_rss(&status).unwrap_or(0) * 1024;
            }
            if let Ok(stat) = std::fs::read_to_string(dir.join("stat")) {
                let utime = stat_field(&stat, 14).unwrap_or(0);
                let stime = stat_field(&stat, 15).unwrap_or(0);
                total.cpu_ticks += utime + stime;
            }
        }
        totals
    }
}

/// A field of `/proc/<pid>/stat`, numbered from 1 like proc(5). The
/// command name (field 2) may contain spaces and parentheses, so fields
/// are counted after its closing parenthesis.
fn stat_field(stat: &str, number: usize) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace()
        .nth(number.checked_sub(3)?)?
        .parse()
        .ok()
}

/// `VmRSS` of `/proc/<pid>/status`, in KiB.
fn vm_rss(status: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn clock_ticks() -> u64 {
    nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
        .ok()
        .flatten()
        .filter(|ticks| *ticks > 0)
        .map_or(100, |ticks| ticks as u64)
}

/// Mount points at or below `dir` in this mount namespace.
pub fn mounts_below(dir: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(|point| PathBuf::from(unescape_mount(point)))
        .filter(|point| point.starts_with(dir))
        .collect()
}

/// Mount points escape space, tab, newline and backslash as `\ooo`.
fn unescape_mount(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let value =
                (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(value);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Builds a fake procfs below `proc` and a state file below `workdir`
/// describing a running container `name` with init `pid` (one child at
/// `pid + 1`, the monitor at `pid - 1`). Returns the state file.
#[cfg(test)]
pub fn fake_running(proc: &Path, workdir: &Path, name: &str, pid: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(proc.join("sys/kernel/random")).unwrap();
    std::fs::write(proc.join("sys/kernel/random/boot_id"), "boot-1\n").unwrap();
    std::fs::create_dir_all(workdir.join("state")).unwrap();
    let ns_file = proc.join(format!("ns-{name}"));
    std::fs::write(&ns_file, "").unwrap();
    let ns_inode = std::fs::metadata(&ns_file).unwrap().ino();
    for (pid, start, rss) in [(pid, 500, 1024), (pid + 1, 600, 512)] {
        let dir = proc.join(pid.to_string());
        std::fs::create_dir_all(dir.join("ns")).unwrap();
        let _ = std::fs::remove_file(dir.join("ns/pid"));
        std::os::unix::fs::symlink(&ns_file, dir.join("ns/pid")).unwrap();
        std::fs::write(
            dir.join("stat"),
            format!("{pid} (init) S 1 1 1 0 -1 0 0 0 0 0 150 50 0 0 20 0 1 0 {start} 0 0"),
        )
        .unwrap();
        std::fs::write(dir.join("status"), format!("VmRSS:\t{rss} kB\n")).unwrap();
    }
    let marker = proc
        .join(pid.to_string())
        .join("root/run/kurumi-containerd");
    std::fs::create_dir_all(&marker).unwrap();
    std::fs::write(marker.join("name"), name).unwrap();
    let monitor = proc.join((pid - 1).to_string());
    std::fs::create_dir_all(&monitor).unwrap();
    std::fs::write(
        monitor.join("stat"),
        format!(
            "{} (kurumi) S 1 1 1 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 400 0 0",
            pid - 1
        ),
    )
    .unwrap();
    let state = serde_json::json!({
        "name": name, "init_pid": pid, "monitor_pid": pid - 1, "rootfs": "/x",
        "uuid": "00000000-0000-0000-0000-000000000000", "host_boot_id": "boot-1",
        "init_start_time": 500, "pid_namespace_inode": ns_inode,
        "started_at_unix": 1_700_000_000u64, "monitor_start_time": 400,
        "init_system": "systemd", "generation": 2
    });
    let path = workdir.join("state").join(format!("{name}.json"));
    std::fs::write(&path, state.to_string()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn stat_fields_survive_odd_command_names() {
        let stat = "4242 (my (odd) proc) S 1 4242 4242 0 -1 4194560 100 0 0 0 7 3 0 0 20 0 1 0 98765 1000 50";
        assert_eq!(stat_field(stat, 3), None);
        assert_eq!(stat_field(stat, 4), Some(1));
        assert_eq!(stat_field(stat, 14), Some(7));
        assert_eq!(stat_field(stat, 15), Some(3));
        assert_eq!(stat_field(stat, 22), Some(98765));
        assert_eq!(stat_field("garbage", 22), None);
    }

    #[test]
    fn rss_and_names() {
        assert_eq!(vm_rss("Name:\tsh\nVmRSS:\t    2048 kB\n"), Some(2048));
        assert_eq!(vm_rss("Name:\tkthreadd\n"), None);
        assert!(valid_name("debian-dev.1_x"));
        assert!(!valid_name("..") && !valid_name("a/b") && !valid_name("") && !valid_name("中文"));
        assert_eq!(unescape_mount("/mnt/with\\040space"), "/mnt/with space");
        assert_eq!(unescape_mount("/plain\\"), "/plain\\");
    }

    /// A fake procfs with one container whose init and monitor match.
    #[test]
    fn live_state_requires_matching_identity() {
        let root = std::env::temp_dir().join(format!("sbb-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proc = root.join("proc");
        let workdir = root.join("run");
        let path = fake_running(&proc, &workdir, "dev", 100);
        let ns_inode = std::fs::metadata(proc.join("ns-dev")).unwrap().ino();
        let state = |boot: &str, start: u64| {
            serde_json::json!({
                "name": "dev", "init_pid": 100, "monitor_pid": 99, "rootfs": "/x",
                "uuid": "00000000-0000-0000-0000-000000000000", "host_boot_id": boot,
                "init_start_time": start, "pid_namespace_inode": ns_inode,
                "started_at_unix": 1_700_000_000u64, "monitor_start_time": 400,
                "init_system": "systemd", "generation": 2
            })
            .to_string()
        };
        let procfs = Procfs::at(proc.clone(), workdir.clone());
        let live = procfs.live(&["dev", "other"]);
        let dev = &live["dev"];
        assert_eq!((dev.init_pid, dev.processes, dev.generation), (100, 2, 2));
        assert_eq!(dev.memory, (1024 + 512) * 1024);
        assert_eq!(dev.init_system, "systemd");
        assert!(!dev.rebooting && dev.cpu_ms > 0);
        assert!(!live.contains_key("other"));

        // A recycled init PID: only the monitor still matches (between reboots).
        std::fs::write(&path, state("boot-1", 501)).unwrap();
        assert!(procfs.live(&["dev"])["dev"].rebooting);
        // Another boot: stale.
        std::fs::write(&path, state("boot-0", 500)).unwrap();
        assert!(procfs.live(&["dev"]).is_empty());
        // Writable by others: not trusted.
        std::fs::write(&path, state("boot-1", 500)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(procfs.live(&["dev"]).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
