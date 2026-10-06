//! The tray on Linux: a StatusNotifierItem published over D-Bus.

use std::ffi::OsString;

use anyhow::{Result, anyhow, bail};
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip, TrayMethods};

pub use super::desktop::{
    Notifier, autostart_enabled, open_terminal as open_dashboard, open_url, set_autostart,
};
use super::icon;
use super::{Action, BoardTray, Command, Entry};
use crate::i18n::fl;

/// Owned on the session bus while a tray runs, so that starting a second
/// one in the same desktop session does not add a second icon.
const BUS_NAME: &str = "io.github.MiChongs.SingboxBoard.Tray";

pub type Handle = ksni::Handle<BoardTray>;

/// The session bus, with the tray's name on it.
pub struct Session {
    bus: zbus::Connection,
}

impl Session {
    pub async fn open() -> Result<Self> {
        let bus = zbus::Connection::session()
            .await
            .map_err(|err| anyhow!(fl!("tray-no-session-bus", error = err.to_string())))?;
        match bus
            .request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
            .await
        {
            Ok(_) => Ok(Self { bus }),
            Err(zbus::Error::NameTaken) => bail!(fl!("tray-already-running")),
            Err(err) => Err(anyhow!(fl!("tray-no-session-bus", error = err.to_string()))),
        }
    }

    pub fn notifier(&self) -> Notifier {
        Notifier::new(self.bus.clone())
    }
}

pub async fn spawn(make: impl Fn() -> BoardTray) -> Result<Handle> {
    match make().spawn().await {
        Ok(handle) => Ok(handle),
        Err(err @ (ksni::Error::Watcher(_) | ksni::Error::WontShow)) => {
            // Started before the panel (at login), or on a desktop without a
            // tray: wait for one to appear instead of giving up.
            eprintln!("{}", fl!("tray-waiting-host", reason = err.to_string()));
            make()
                .assume_sni_available(true)
                .spawn()
                .await
                .map_err(|err| anyhow!(fl!("tray-start-failed", error = err.to_string())))
        }
        Err(err) => Err(anyhow!(fl!("tray-start-failed", error = err.to_string()))),
    }
}

/// Resolves on SIGTERM, SIGHUP or SIGINT.
pub async fn terminated() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut terminate), Ok(mut hangup), Ok(mut interrupt)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = terminate.recv() => {}
        _ = hangup.recv() => {}
        _ = interrupt.recv() => {}
    }
}

/// The login item runs the tray itself.
pub fn login_command(command: Vec<OsString>) -> Vec<OsString> {
    command
}

/// The daemon runs as a systemd or OpenRC service, started by the system.
pub async fn start_service() -> Result<()> {
    Ok(())
}

/// Menu hosts read `_` as the mark of an access key; `__` is a literal one.
fn mnemonic_free(text: &str) -> String {
    text.replace('_', "__")
}

/// The menu model as ksni items. Consecutive radio items form one group.
fn items(entries: Vec<Entry>) -> Vec<MenuItem<BoardTray>> {
    let mut items = Vec::new();
    let mut entries = entries.into_iter().peekable();
    while let Some(entry) = entries.next() {
        match entry {
            Entry::Separator => items.push(MenuItem::Separator),
            Entry::Item {
                label,
                enabled,
                icon,
                command,
            } => items.push(
                StandardItem {
                    label: mnemonic_free(&label),
                    enabled,
                    icon_name: icon.into(),
                    activate: Box::new(move |tray: &mut BoardTray| {
                        if let Some(command) = &command {
                            tray.invoke(command.clone());
                        }
                    }),
                    ..Default::default()
                }
                .into(),
            ),
            Entry::Check {
                label,
                checked,
                enabled,
                command,
            } => items.push(
                CheckmarkItem {
                    label: mnemonic_free(&label),
                    checked,
                    enabled,
                    activate: Box::new(move |tray: &mut BoardTray| tray.invoke(command.clone())),
                    ..Default::default()
                }
                .into(),
            ),
            radio @ Entry::Radio { .. } => {
                let mut group = vec![radio];
                while matches!(entries.peek(), Some(Entry::Radio { .. })) {
                    group.extend(entries.next());
                }
                let mut selected = usize::MAX;
                let mut commands: Vec<Option<Command>> = Vec::new();
                let mut options = Vec::new();
                for (index, entry) in group.into_iter().enumerate() {
                    let Entry::Radio {
                        label,
                        checked,
                        enabled,
                        command,
                    } = entry
                    else {
                        continue;
                    };
                    if checked {
                        selected = index;
                    }
                    commands.push(command);
                    options.push(RadioItem {
                        label: mnemonic_free(&label),
                        enabled,
                        ..Default::default()
                    });
                }
                items.push(
                    RadioGroup {
                        selected,
                        select: Box::new(move |tray: &mut BoardTray, index| {
                            if let Some(Some(command)) = commands.get(index) {
                                tray.invoke(command.clone());
                            }
                        }),
                        options,
                    }
                    .into(),
                );
            }
            Entry::Sub {
                label,
                icon,
                items: submenu,
            } => items.push(
                SubMenu {
                    label: mnemonic_free(&label),
                    icon_name: icon.into(),
                    submenu: self::items(submenu),
                    ..Default::default()
                }
                .into(),
            ),
        }
    }
    items
}

impl ksni::Tray for BoardTray {
    fn id(&self) -> String {
        "singbox-board".into()
    }

    fn title(&self) -> String {
        "singbox-board".into()
    }

    fn status(&self) -> ksni::Status {
        // Hosts move items that need attention out of the overflow area.
        if self.needs_attention() {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icon::pixmaps(self.tone())
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: self.headline(),
            description: self.details().join("\n"),
            ..Default::default()
        }
    }

    /// A left click opens the dashboard.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Action::Dashboard);
    }

    fn menu_about_to_show(&mut self) {
        self.send(Action::Refresh);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        items(BoardTray::menu(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underscores_are_not_access_keys() {
        assert_eq!(mnemonic_free("home_lab"), "home__lab");
    }

    #[test]
    fn radio_items_form_one_group() {
        let entries = vec![
            Entry::Radio {
                label: "rule".into(),
                checked: true,
                enabled: true,
                command: None,
            },
            Entry::Radio {
                label: "global".into(),
                checked: false,
                enabled: true,
                command: Some(Command::Quit),
            },
            Entry::Separator,
        ];
        let items = items(entries);
        assert_eq!(items.len(), 2);
        let MenuItem::RadioGroup(group) = &items[0] else {
            panic!("not a radio group");
        };
        assert_eq!(group.selected, 0);
        assert_eq!(group.options.len(), 2);
    }
}
