use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::broadcast;

use crate::protocol::{LogEntry, LogSource};
use crate::util::now_unix_ms;

/// Lines longer than this are truncated before they are buffered.
const MAX_LINE_CHARS: usize = 8 * 1024;

/// In-memory ring buffer of sing-box and daemon output with live fan-out.
pub struct LogHub {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<LogEntry>,
    forward_core: bool,
}

struct Inner {
    ring: VecDeque<LogEntry>,
    capacity: usize,
    seq: u64,
}

impl LogHub {
    pub fn new(capacity: usize, forward_core: bool) -> Self {
        let capacity = capacity.max(1);
        let (tx, _) = broadcast::channel(1024);
        Self {
            inner: Mutex::new(Inner {
                ring: VecDeque::with_capacity(capacity),
                capacity,
                seq: 0,
            }),
            tx,
            forward_core,
        }
    }

    pub fn push(&self, source: LogSource, line: &str) {
        let mut line = line.trim_end_matches(['\r', '\n']).to_owned();
        if let Some((index, _)) = line.char_indices().nth(MAX_LINE_CHARS) {
            line.truncate(index);
            line.push('…');
        }
        if source == LogSource::Core && self.forward_core {
            eprintln!("{line}");
        }
        let mut inner = self.inner.lock().unwrap();
        inner.seq += 1;
        let entry = LogEntry {
            seq: inner.seq,
            ts: now_unix_ms(),
            source,
            line,
        };
        if inner.ring.len() == inner.capacity {
            inner.ring.pop_front();
        }
        inner.ring.push_back(entry.clone());
        // Sent while holding the lock so `subscribe` never sees a gap or a duplicate.
        let _ = self.tx.send(entry);
    }

    /// Records a daemon event both in the hub and in the daemon's own log.
    pub fn info(&self, message: impl AsRef<str>) {
        tracing::info!("{}", message.as_ref());
        self.push(LogSource::Daemon, message.as_ref());
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        tracing::warn!("{}", message.as_ref());
        self.push(LogSource::Daemon, message.as_ref());
    }

    /// Returns the last `tail` lines and a receiver for everything after them.
    pub fn subscribe(&self, tail: usize) -> (Vec<LogEntry>, broadcast::Receiver<LogEntry>) {
        let inner = self.inner.lock().unwrap();
        let rx = self.tx.subscribe();
        let skip = inner.ring.len().saturating_sub(tail);
        let backlog = inner.ring.iter().skip(skip).cloned().collect();
        (backlog, rx)
    }

    /// The most recent core lines, used to explain an immediate exit.
    pub fn recent_core_lines(&self, count: usize) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let mut lines: Vec<String> = inner
            .ring
            .iter()
            .rev()
            .filter(|entry| entry.source == LogSource::Core)
            .take(count)
            .map(|entry| entry.line.clone())
            .collect();
        lines.reverse();
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_and_subscribe() {
        let hub = LogHub::new(3, false);
        for i in 0..5 {
            hub.push(LogSource::Core, &format!("line {i}\n"));
        }
        let (backlog, mut rx) = hub.subscribe(2);
        let lines: Vec<_> = backlog.iter().map(|e| e.line.as_str()).collect();
        assert_eq!(lines, ["line 3", "line 4"]);
        hub.push(LogSource::Daemon, "next");
        let entry = rx.try_recv().unwrap();
        assert_eq!(entry.line, "next");
        assert_eq!(entry.seq, 6);
        assert_eq!(hub.recent_core_lines(10), ["line 3", "line 4"]);
    }
}
