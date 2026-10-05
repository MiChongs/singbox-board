use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use unicode_width::UnicodeWidthStr;

use crate::i18n::fl;

/// Installs the process-wide rustls crypto provider (idempotent).
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

pub fn now_unix() -> u64 {
    now_unix_ms() / 1000
}

/// Human readable byte size using binary prefixes, e.g. `1.5 MiB`.
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn fmt_speed(bytes_per_sec: u64) -> String {
    format!("{}/s", fmt_bytes(bytes_per_sec))
}

/// Compact duration, e.g. `3d 04:05:06`, `04:05:06` or `05:06`.
pub fn fmt_duration(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = secs % 86_400 / 3_600;
    let minutes = secs % 3_600 / 60;
    let seconds = secs % 60;
    if days > 0 {
        fl!(
            "duration-days",
            days = days,
            clock = format!("{hours:02}:{minutes:02}:{seconds:02}")
        )
    } else if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Local wall clock time `HH:MM:SS` for a unix timestamp in milliseconds.
pub fn fmt_clock(unix_ms: u64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(unix_ms as i64).single() {
        Some(time) => time.format("%H:%M:%S").to_string(),
        None => "--:--:--".to_owned(),
    }
}

/// An error and its causes, joined like `{:#}` but with the separator of the
/// current language.
pub fn error_chain(err: &anyhow::Error) -> String {
    let separator = fl!("chain-separator");
    err.chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(&separator)
}

/// Joins list items with the separator of the current language.
pub fn join_list<S: AsRef<str>>(items: &[S]) -> String {
    let separator = fl!("list-separator");
    items
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(&separator)
}

/// Width of `text` in terminal columns; CJK characters take two.
pub fn text_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Pads `text` with spaces to `width` terminal columns.
pub fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(text_width(text));
    format!("{text}{}", " ".repeat(fill))
}

/// Removes ANSI escape sequences (CSI and OSC) from a line of terminal output.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: parameters/intermediates until a final byte in 0x40..=0x7e
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC: terminated by BEL or ESC \
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

/// Writes `data` to a temporary sibling and renames it over `path`.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .with_context(|| fl!("err-create", path = tmp.display().to_string()))?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
        .with_context(|| fl!("err-replace", path = path.display().to_string()))
}

/// Atomically makes `link` a symlink to `target` (replacing a file or link).
pub fn point_symlink(link: &Path, target: &Path) -> Result<()> {
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = link.as_os_str().to_owned();
    tmp.push(".switching");
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    std::fs::rename(&tmp, link).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// A URL without its query, shortened for display; queries often carry
/// subscription tokens.
pub fn shorten_url(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    let mut text: String = base.chars().take(60).collect();
    if base.len() < url.len() || base.chars().count() > 60 {
        text.push('…');
    }
    text
}

/// Opens `text` in `$VISUAL` or `$EDITOR` and returns what was saved.
/// Blocks until the editor exits; the temporary file is readable by the
/// current user only.
pub fn edit_text(text: &str, name: &str) -> Result<String> {
    let (program, args) = editor_command()?;
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    let path = std::env::temp_dir().join(format!("singbox-board-{}-{name}.json", random_token(6)));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| fl!("err-create", path = path.display().to_string()))?;
    file.write_all(text.as_bytes())?;
    drop(file);
    let status = std::process::Command::new(&program)
        .args(&args)
        .arg(&path)
        .status()
        .with_context(|| fl!("editor-start-failed", editor = program.clone()));
    let result = match status {
        Ok(status) if status.success() => std::fs::read_to_string(&path)
            .with_context(|| fl!("err-read", path = path.display().to_string())),
        Ok(status) => Err(anyhow::anyhow!(fl!(
            "editor-failed",
            editor = program.clone(),
            status = status.to_string()
        ))),
        Err(err) => Err(err),
    };
    let _ = std::fs::remove_file(&path);
    result
}

/// The editor program from `$VISUAL` or `$EDITOR` and its arguments. There
/// is no fallback: without either, the built-in editor is used.
fn editor_command() -> Result<(String, Vec<String>)> {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(value) = std::env::var(var) {
            let mut parts = value.split_whitespace().map(str::to_owned);
            if let Some(program) = parts.next() {
                return Ok((program, parts.collect()));
            }
        }
    }
    bail!(fl!("editor-none"))
}

/// Whether `$VISUAL` or `$EDITOR` names an external editor.
pub fn external_editor_configured() -> bool {
    editor_command().is_ok()
}

/// An executable named `name` in `PATH`; a name with a slash is a path.
pub fn find_program(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &Path| {
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        let path = PathBuf::from(name);
        return executable(&path).then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| executable(candidate))
}

/// Alphanumeric token from the kernel CSPRNG.
pub fn random_token(len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = vec![0u8; len * 2];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("read /dev/urandom");
    // Rejection sampling keeps the distribution uniform (248 = 4 * 62).
    bytes
        .into_iter()
        .filter(|b| *b < 248)
        .take(len)
        .map(|b| ALPHABET[(b % 62) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(1023), "1023 B");
        assert_eq!(fmt_bytes(1536), "1.5 KiB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(5), "00:05");
        assert_eq!(fmt_duration(3_725), "01:02:05");
        assert_eq!(fmt_duration(90_061), "1d 01:01:01");
    }

    #[test]
    fn padding_counts_columns() {
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad("版本", 6), "版本  ");
        assert_eq!(pad("toolong", 3), "toolong");
    }

    #[test]
    fn ansi() {
        assert_eq!(strip_ansi("\x1b[36mINFO\x1b[0m hello"), "INFO hello");
        assert_eq!(strip_ansi("\x1b]0;title\x07text"), "text");
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
