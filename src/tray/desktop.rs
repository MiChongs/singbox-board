//! Desktop integration of the tray: notifications, the login item, a
//! terminal for the dashboard and the browser.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use zbus::zvariant::{Structure, Value};

use super::icon::{self, Tone};
use crate::i18n::fl;
use crate::util::{find_program, write_atomic};

/// Name of the desktop entry (`singbox-board.desktop`) that packages install
/// and the login item reuses.
const DESKTOP_ID: &str = "singbox-board";
const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
/// Size of the image sent with notifications.
const IMAGE_SIZE: u32 = 48;

/// Sends desktop notifications; each one replaces the previous one so a
/// burst of actions does not pile up.
#[derive(Clone)]
pub struct Notifier {
    bus: zbus::Connection,
    last: Arc<AtomicU32>,
}

impl Notifier {
    pub fn new(bus: zbus::Connection) -> Self {
        Self {
            bus,
            last: Arc::new(AtomicU32::new(0)),
        }
    }

    pub async fn send(&self, tone: Tone, summary: &str, body: &str) {
        let body = (
            "singbox-board",
            self.last.load(Ordering::Relaxed),
            "",
            summary,
            escape_markup(body),
            Vec::<&str>::new(),
            hints(tone),
            -1i32,
        );
        let reply = self
            .bus
            .call_method(
                Some(NOTIFICATIONS),
                "/org/freedesktop/Notifications",
                Some(NOTIFICATIONS),
                "Notify",
                &body,
            )
            .await
            .and_then(|reply| reply.body().deserialize::<u32>());
        match reply {
            Ok(id) => self.last.store(id, Ordering::Relaxed),
            // The icon and its menu still show the state.
            Err(err) => eprintln!("{}", fl!("tray-notify-failed", error = err.to_string())),
        }
    }
}

/// The desktop entry, for the application's name and icon, and the cube in
/// `tone` as the image (width, height, row stride, alpha, bits per sample,
/// channels, RGBA bytes).
fn hints(tone: Tone) -> HashMap<&'static str, Value<'static>> {
    let size = IMAGE_SIZE as i32;
    let image = Structure::from((
        size,
        size,
        size * 4,
        true,
        8i32,
        4i32,
        icon::rgba(tone, IMAGE_SIZE),
    ));
    HashMap::from([
        ("desktop-entry", Value::from(DESKTOP_ID)),
        ("image-data", Value::from(image)),
    ])
}

/// Notification servers may read the body as markup.
fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `$XDG_CONFIG_HOME/autostart/singbox-board.desktop`
fn autostart_file() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".config")))?;
    Some(
        config
            .join("autostart")
            .join(format!("{DESKTOP_ID}.desktop")),
    )
}

/// Whether the tray starts on login. Desktop settings may keep the entry
/// but mark it hidden or disabled.
pub fn autostart_enabled() -> bool {
    let Some(text) = autostart_file().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return false;
    };
    !text.lines().any(|line| {
        matches!(
            line.split_once('=')
                .map(|(key, value)| (key.trim(), value.trim())),
            Some(("Hidden", "true") | ("X-GNOME-Autostart-enabled", "false"))
        )
    })
}

/// Adds or removes the login item that runs `command`.
pub fn set_autostart(enable: bool, command: &[OsString]) -> Result<()> {
    let path = autostart_file().ok_or_else(|| anyhow!(fl!("tray-no-home")))?;
    if !enable {
        return match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                Err(err).with_context(|| fl!("err-delete", path = path.display().to_string()))
            }
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| fl!("err-create", path = dir.display().to_string()))?;
    }
    write_atomic(&path, autostart_entry(command).as_bytes(), 0o644)
}

fn autostart_entry(command: &[OsString]) -> String {
    let exec: Vec<String> = command
        .iter()
        .map(|arg| exec_arg(&arg.to_string_lossy()))
        .collect();
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=singbox-board\n\
         Comment={}\n\
         Exec={}\n\
         Icon={DESKTOP_ID}\n\
         Terminal=false\n\
         Categories=Network;\n\
         X-GNOME-Autostart-enabled=true\n",
        fl!("tray-desktop-comment"),
        exec.join(" ")
    )
}

/// One argument of a desktop entry's `Exec` key, quoted when needed. The
/// value is unescaped as a string before the quoting rules apply, which
/// doubles the backslashes inside quotes.
fn exec_arg(arg: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    let arg = arg.replace('%', "%%");
    if !arg.is_empty() && !arg.contains(|c| RESERVED.contains(c)) {
        return arg;
    }
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(c);
            }
            '\\' => quoted.push_str("\\\\\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// Terminals and the arguments that make them run the rest of the command
/// line. `xdg-terminal-exec` (the user's choice) and `$TERMINAL` go first.
const TERMINALS: [(&str, &[&str]); 13] = [
    ("konsole", &["-e"]),
    ("ptyxis", &["--"]),
    ("kgx", &["--"]),
    ("gnome-terminal", &["--"]),
    ("xfce4-terminal", &["-x"]),
    ("mate-terminal", &["-x"]),
    ("x-terminal-emulator", &["-e"]),
    ("alacritty", &["-e"]),
    ("kitty", &[]),
    ("foot", &[]),
    ("wezterm", &["start", "--"]),
    ("ghostty", &["-e"]),
    ("xterm", &["-e"]),
];

/// The terminals of the running desktop, tried before the others.
fn native_terminals() -> &'static [&'static str] {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_uppercase();
    for name in desktop.split(':') {
        match name {
            "KDE" => return &["konsole"],
            "GNOME" | "UNITY" => return &["ptyxis", "kgx", "gnome-terminal"],
            "XFCE" => return &["xfce4-terminal"],
            "MATE" => return &["mate-terminal"],
            _ => {}
        }
    }
    &[]
}

/// A terminal program and the arguments that precede the command to run.
fn terminal() -> Option<(PathBuf, Vec<OsString>)> {
    if let Some(program) = find_program("xdg-terminal-exec") {
        return Some((program, Vec::new()));
    }
    if let Ok(value) = std::env::var("TERMINAL") {
        let mut words = value.split_whitespace();
        if let Some(program) = words.next().and_then(find_program) {
            let mut args: Vec<OsString> = words.map(OsString::from).collect();
            args.push("-e".into());
            return Some((program, args));
        }
    }
    let native = native_terminals();
    native
        .iter()
        .filter_map(|name| TERMINALS.iter().find(|(known, _)| known == name))
        .chain(TERMINALS.iter().filter(|(name, _)| !native.contains(name)))
        .find_map(|(name, args)| {
            let program = find_program(name)?;
            Some((program, args.iter().map(OsString::from).collect()))
        })
}

/// Runs `command` in a new terminal window.
pub fn open_terminal(command: &[OsString]) -> Result<()> {
    let (program, mut args) = terminal().ok_or_else(|| anyhow!(fl!("tray-no-terminal")))?;
    args.extend(command.iter().cloned());
    let mut child = detached(&program)
        .args(&args)
        .spawn()
        .with_context(|| fl!("err-spawn", path = program.display().to_string()))?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// Opens `url` in the default browser.
pub async fn open_url(url: &str) -> Result<()> {
    let status = detached("xdg-open")
        .arg(url)
        .status()
        .await
        .with_context(|| fl!("err-spawn", path = "xdg-open"))?;
    if !status.success() {
        bail!(fl!(
            "tray-open-failed",
            url = url,
            status = status.to_string()
        ));
    }
    Ok(())
}

/// A command whose window outlives the tray: no terminal I/O, and its own
/// process group so Ctrl-C on a tray started from a shell does not reach it.
fn detached(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_arguments_are_quoted_when_needed() {
        assert_eq!(exec_arg("/usr/bin/singbox-board"), "/usr/bin/singbox-board");
        assert_eq!(exec_arg("--lang"), "--lang");
        assert_eq!(exec_arg("/opt/my apps/sb"), "\"/opt/my apps/sb\"");
        assert_eq!(exec_arg("100%"), "100%%");
        assert_eq!(exec_arg("a$b"), "\"a\\\\$b\"");
        assert_eq!(exec_arg("a\\b"), "\"a\\\\\\\\b\"");
        assert_eq!(exec_arg(""), "\"\"");
    }

    #[test]
    fn notification_image_matches_the_specification() {
        let hints = hints(Tone::Running);
        let image = &hints["image-data"];
        assert_eq!(image.value_signature().to_string(), "(iiibiiay)");
        let Value::Structure(fields) = image else {
            panic!("{image:?}");
        };
        let Value::Array(data) = &fields.fields()[6] else {
            panic!("{fields:?}");
        };
        assert_eq!(data.len() as u32, IMAGE_SIZE * IMAGE_SIZE * 4);
        assert_eq!(hints["desktop-entry"], Value::from(DESKTOP_ID));
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(escape_markup("a <b> & c"), "a &lt;b&gt; &amp; c");
    }

    #[test]
    fn login_item_runs_the_command() {
        let entry = autostart_entry(&["/usr/bin/singbox-board".into(), "tray".into()]);
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(entry.contains("\nExec=/usr/bin/singbox-board tray\n"));
        assert!(entry.ends_with("X-GNOME-Autostart-enabled=true\n"));
    }
}
