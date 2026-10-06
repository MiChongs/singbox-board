//! System tray icon. Like the TUI, the tray is a client of the daemon: it
//! polls the status and turns menu clicks into requests.
//!
//! What the tray shows and offers is worked out here, independent of the
//! platform; [`platform`] draws it:
//!
//! - Linux: a StatusNotifierItem over D-Bus, which does not depend on the
//!   display server, so the tray works on Wayland and X11 alike, in every
//!   host of the protocol: KDE Plasma, GNOME with the AppIndicator
//!   extension, Cinnamon, XFCE, LXQt, and the tray modules of Waybar and
//!   other bars.
//! - Windows: an icon in the notification area (Shell_NotifyIcon) with a
//!   native context menu that follows the system's dark mode, notifications
//!   as balloons/toasts, a login item in the registry's Run key, and the
//!   option to start the service when the daemon is not running.

#[cfg(unix)]
mod desktop;
mod icon;
#[cfg(unix)]
#[path = "sni.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{Notify, mpsc};

use self::icon::Tone;
use self::platform::{Handle, Notifier};
use crate::clash::ClashClient;
use crate::client::DaemonClient;
use crate::i18n::{self, fl};
use crate::protocol::{ClashApi, Component, ContainerAction, CoreState, Request, Status};
use crate::util::error_chain;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const CLASH_TIMEOUT: Duration = Duration::from_secs(2);

/// Global options given on the command line, passed on to the login item.
pub struct Options {
    pub socket: Option<PathBuf>,
    pub lang: Option<String>,
}

pub async fn run(client: DaemonClient, options: Options) -> Result<()> {
    // One tray per desktop session, so a second start adds no second icon.
    let session = platform::Session::open().await?;
    let exe = std::env::current_exe().context(fl!("tray-no-executable"))?;
    let notifier = session.notifier();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = platform::spawn(|| BoardTray::new(tx.clone())).await?;
    let refresh = Arc::new(Notify::new());
    let poller = tokio::spawn(poll(
        client.clone(),
        handle.clone(),
        refresh.clone(),
        notifier.clone(),
        tx.clone(),
    ));
    let worker = Worker {
        client,
        handle: handle.clone(),
        notifier,
        refresh,
    };
    let terminated = platform::terminated();
    tokio::pin!(terminated);
    loop {
        let action = tokio::select! {
            action = rx.recv() => action,
            _ = &mut terminated => None,
        };
        match action {
            None | Some(Action::Quit) => break,
            Some(Action::Refresh) => worker.refresh.notify_one(),
            Some(Action::Run(op, job)) => {
                let worker = worker.clone();
                tokio::spawn(async move { worker.run(op, job).await });
            }
            Some(Action::Dashboard) => {
                let mut command: Vec<OsString> = vec![exe.clone().into()];
                command.extend([
                    "--socket".into(),
                    worker.client.socket().into(),
                    "--lang".into(),
                    i18n::current().tag().into(),
                    "tui".into(),
                ]);
                if let Err(err) = platform::open_dashboard(&command) {
                    worker.failed("dashboard", &err).await;
                }
            }
            Some(Action::Open(url)) => {
                let worker = worker.clone();
                tokio::spawn(async move {
                    if let Err(err) = platform::open_url(&url).await {
                        worker.failed("browser", &err).await;
                    }
                });
            }
            Some(Action::Autostart(enable)) => {
                let command = platform::login_command(tray_command(&exe, &options));
                let result = platform::set_autostart(enable, &command);
                let enabled = platform::autostart_enabled();
                handle.update(move |tray| tray.autostart = enabled).await;
                if let Err(err) = result {
                    worker.failed("autostart", &err).await;
                }
            }
            Some(Action::StartService) => {
                let worker = worker.clone();
                tokio::spawn(async move {
                    match platform::start_service().await {
                        Ok(()) => worker.refresh.notify_one(),
                        Err(err) => worker.failed("service", &err).await,
                    }
                });
            }
        }
    }
    poller.abort();
    handle.shutdown().await;
    drop(session);
    Ok(())
}

/// The command line that starts this tray again, for the login item.
fn tray_command(exe: &std::path::Path, options: &Options) -> Vec<OsString> {
    let mut command: Vec<OsString> = vec![exe.into()];
    if let Some(socket) = &options.socket {
        command.extend(["--socket".into(), socket.into()]);
    }
    if let Some(lang) = &options.lang {
        command.extend(["--lang".into(), lang.into()]);
    }
    command.push("tray".into());
    command
}

/// Everything an action needs once it left the menu.
#[derive(Clone)]
struct Worker {
    client: DaemonClient,
    handle: Handle,
    notifier: Notifier,
    refresh: Arc<Notify>,
}

impl Worker {
    async fn run(&self, op: Op, job: Job) {
        let result = match job {
            Job::Daemon(request) => self.client.command(request).await,
            Job::Mode(api, mode) => set_mode(&api, &mode).await,
        };
        // Show the new state together with the end of the request.
        self.refresh.notify_one();
        self.handle.update(|tray| tray.busy = None).await;
        match result {
            Ok(message) => {
                // The menu shows the new mode; other results are worth a note.
                if op != Op::Mode {
                    let tone = if op == Op::Stop {
                        Tone::Stopped
                    } else {
                        Tone::Running
                    };
                    self.notifier.send(tone, &message, "").await;
                }
            }
            Err(err) => self.failed(op.key(), &err).await,
        }
    }

    async fn failed(&self, op: &str, err: &anyhow::Error) {
        self.notifier
            .send(Tone::Error, &fl!("tray-failed", op = op), &error_chain(err))
            .await;
    }
}

async fn set_mode(api: &ClashApi, mode: &str) -> Result<String> {
    ClashClient::new(api)?.set_mode(mode).await?;
    Ok(fl!("tui-mode-set", mode = mode))
}

enum Action {
    /// A request that runs in the background; one at a time.
    Run(Op, Job),
    Dashboard,
    Open(String),
    Autostart(bool),
    /// Start the daemon's service (Windows).
    #[cfg_attr(unix, allow(dead_code))]
    StartService,
    Refresh,
    Quit,
}

enum Job {
    Daemon(Request),
    Mode(ClashApi, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Start,
    Stop,
    Restart,
    Profile,
    Update,
    Mode,
    Container,
}

impl Op {
    /// Selector of `tray-failed`.
    fn key(self) -> &'static str {
        match self {
            Op::Start => "start",
            Op::Stop => "stop",
            Op::Restart => "restart",
            Op::Profile => "profile",
            Op::Update => "update",
            Op::Mode => "mode",
            Op::Container => "container",
        }
    }

    fn busy(self) -> String {
        match self {
            Op::Start => fl!("busy-starting"),
            Op::Stop => fl!("busy-stopping"),
            Op::Restart => fl!("busy-restarting"),
            Op::Profile => fl!("busy-switching-profile"),
            Op::Update => fl!("busy-updating-profiles"),
            Op::Mode => fl!("busy-switching-mode"),
            Op::Container => fl!("tray-busy-container"),
        }
    }
}

/// What the tray shows, rebuilt from every poll.
#[derive(Clone, Debug, Default, PartialEq)]
struct View {
    link: Link,
    profiles: Vec<Choice>,
    mode: Option<Mode>,
    containers: Vec<TrayContainer>,
}

/// A container in the menu.
#[derive(Clone, Debug, PartialEq)]
struct TrayContainer {
    id: String,
    name: String,
    state: CoreState,
    /// It has a root filesystem and can be started.
    ready: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
enum Link {
    #[default]
    Connecting,
    /// Why the daemon cannot be reached.
    Down(String),
    Up(Box<Core>),
}

#[derive(Clone, Debug, PartialEq)]
struct Core {
    state: CoreState,
    version: Option<String>,
    profile: Option<String>,
    last_exit: Option<String>,
    /// Web UI of a running Sub-Store.
    sub_store: Option<String>,
}

impl Core {
    fn new(status: &Status) -> Self {
        Self {
            state: status.state,
            version: status.core_version.clone(),
            profile: status.active_profile.as_ref().map(|p| p.name.clone()),
            last_exit: status.last_exit.clone(),
            sub_store: status
                .components
                .iter()
                .find(|c| c.component == Component::SubStore && c.state == CoreState::Running)
                .and_then(|c| c.url.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Choice {
    id: String,
    name: String,
    active: bool,
    remote: bool,
}

/// The Clash API mode and the modes it offers.
#[derive(Clone, Debug, PartialEq)]
struct Mode {
    api: ClashApi,
    current: String,
    all: Vec<String>,
}

/// Polls the daemon and the Clash API and hands changes to the tray.
async fn poll(
    client: DaemonClient,
    handle: Handle,
    refresh: Arc<Notify>,
    notifier: Notifier,
    tx: mpsc::UnboundedSender<Action>,
) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut clash: Option<(ClashApi, ClashClient)> = None;
    let mut shown = View::default();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = refresh.notified() => {}
        }
        if handle.is_closed() {
            let _ = tx.send(Action::Quit);
            return;
        }
        let view = fetch(&client, &mut clash).await;
        if let (Link::Up(old), Link::Up(new)) = (&shown.link, &view.link)
            && old.state == CoreState::Running
            && matches!(new.state, CoreState::Backoff | CoreState::Failed)
        {
            let reason = new.last_exit.clone().unwrap_or_default();
            notifier
                .send(Tone::Error, &fl!("tray-core-exited"), &reason)
                .await;
        }
        if view != shown {
            let next = view.clone();
            if handle.update(move |tray| tray.view = next).await.is_none() {
                let _ = tx.send(Action::Quit);
                return;
            }
            shown = view;
        }
    }
}

async fn fetch(client: &DaemonClient, clash: &mut Option<(ClashApi, ClashClient)>) -> View {
    let status = match client.status().await {
        Ok(status) => status,
        Err(err) => {
            return View {
                link: Link::Down(error_chain(&err)),
                ..View::default()
            };
        }
    };
    let profiles = client
        .profiles()
        .await
        .map(|list| {
            list.profiles
                .into_iter()
                .map(|p| Choice {
                    remote: p.is_remote(),
                    id: p.id,
                    name: p.name,
                    active: p.active,
                })
                .collect()
        })
        .unwrap_or_default();
    let mode = match &status.clash_api {
        Some(api) if status.state == CoreState::Running => clash_mode(api, clash).await,
        _ => {
            *clash = None;
            None
        }
    };
    // Only daemons that know containers and have some are asked for them.
    let containers = match status.containers {
        Some(summary) if summary.total > 0 => client
            .containers()
            .await
            .map(|overview| {
                overview
                    .containers
                    .into_iter()
                    .map(|c| TrayContainer {
                        ready: c.spec.as_ref().is_some_and(|s| s.installed),
                        id: c.id,
                        name: c.name,
                        state: c.state,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    View {
        link: Link::Up(Box::new(Core::new(&status))),
        profiles,
        mode,
        containers,
    }
}

async fn clash_mode(api: &ClashApi, cache: &mut Option<(ClashApi, ClashClient)>) -> Option<Mode> {
    if cache.as_ref().is_none_or(|(have, _)| have != api) {
        *cache = ClashClient::new(api)
            .ok()
            .map(|client| (api.clone(), client));
    }
    let (_, client) = cache.as_ref()?;
    let configs = tokio::time::timeout(CLASH_TIMEOUT, client.configs())
        .await
        .ok()?
        .ok()?;
    Some(Mode {
        api: api.clone(),
        current: configs.mode,
        all: configs.mode_list,
    })
}

struct BoardTray {
    view: View,
    /// The request in progress; menu actions wait for it.
    busy: Option<Op>,
    autostart: bool,
    actions: mpsc::UnboundedSender<Action>,
}

/// A menu entry, independent of how the platform draws menus. Labels are
/// plain text; the platform escapes its access-key marks.
#[derive(Clone, Debug, PartialEq)]
enum Entry {
    Separator,
    /// A command, or a disabled line of text when there is none.
    Item {
        label: String,
        enabled: bool,
        /// Freedesktop icon name, for hosts that show icons in menus.
        icon: &'static str,
        command: Option<Command>,
    },
    Check {
        label: String,
        checked: bool,
        enabled: bool,
        command: Command,
    },
    /// One choice of a group of consecutive radio items; picking the
    /// checked one does nothing.
    Radio {
        label: String,
        checked: bool,
        enabled: bool,
        command: Option<Command>,
    },
    Sub {
        label: String,
        icon: &'static str,
        items: Vec<Entry>,
    },
}

/// What a menu entry does.
#[derive(Clone, Debug, PartialEq)]
enum Command {
    Start,
    Stop,
    Restart,
    Profile(String),
    UpdateProfiles,
    Mode(ClashApi, String),
    Container(String, ContainerAction),
    Dashboard,
    Open(String),
    Autostart(bool),
    StartService,
    Quit,
}

impl Entry {
    fn label(text: String) -> Self {
        Entry::Item {
            label: text,
            enabled: false,
            icon: "",
            command: None,
        }
    }

    fn item(label: String, enabled: bool, icon: &'static str, command: Command) -> Self {
        Entry::Item {
            label,
            enabled,
            icon,
            command: Some(command),
        }
    }
}

impl BoardTray {
    fn new(actions: mpsc::UnboundedSender<Action>) -> Self {
        Self {
            view: View::default(),
            busy: None,
            autostart: platform::autostart_enabled(),
            actions,
        }
    }

    fn send(&self, action: Action) {
        let _ = self.actions.send(action);
    }

    fn start(&mut self, op: Op, job: Job) {
        if self.busy.is_none() {
            self.busy = Some(op);
            self.send(Action::Run(op, job));
        }
    }

    /// Carries out a menu entry's command.
    fn invoke(&mut self, command: Command) {
        match command {
            Command::Start => self.start(Op::Start, Job::Daemon(Request::Start)),
            Command::Stop => self.start(Op::Stop, Job::Daemon(Request::Stop)),
            Command::Restart => self.start(Op::Restart, Job::Daemon(Request::Restart)),
            Command::Profile(id) => {
                let request = Request::ProfileActivate { id, force: false };
                self.start(Op::Profile, Job::Daemon(request));
            }
            Command::UpdateProfiles => {
                let request = Request::ProfileUpdate {
                    id: None,
                    force: false,
                };
                self.start(Op::Update, Job::Daemon(request));
            }
            Command::Mode(api, mode) => self.start(Op::Mode, Job::Mode(api, mode)),
            Command::Container(id, action) => {
                let request = Request::ContainerControl { id, action };
                self.start(Op::Container, Job::Daemon(request));
            }
            Command::Dashboard => self.send(Action::Dashboard),
            Command::Open(url) => self.send(Action::Open(url)),
            Command::Autostart(enable) => self.send(Action::Autostart(enable)),
            Command::StartService => self.send(Action::StartService),
            Command::Quit => self.send(Action::Quit),
        }
    }

    fn core(&self) -> Option<&Core> {
        match &self.view.link {
            Link::Up(core) => Some(core),
            _ => None,
        }
    }

    /// sing-box failed and nobody is doing anything about it yet.
    #[cfg_attr(windows, allow(dead_code))]
    fn needs_attention(&self) -> bool {
        self.busy.is_none()
            && self
                .core()
                .is_some_and(|core| core.state == CoreState::Failed)
    }

    fn tone(&self) -> Tone {
        if self.busy.is_some() {
            return Tone::Busy;
        }
        match &self.view.link {
            Link::Connecting => Tone::Stopped,
            Link::Down(_) => Tone::Error,
            Link::Up(core) => match core.state {
                CoreState::Running => Tone::Running,
                CoreState::Starting | CoreState::Stopping | CoreState::Backoff => Tone::Busy,
                CoreState::Stopped => Tone::Stopped,
                CoreState::Failed => Tone::Error,
            },
        }
    }

    fn headline(&self) -> String {
        if let Some(op) = self.busy {
            return op.busy();
        }
        match &self.view.link {
            Link::Connecting => fl!("tui-connecting"),
            Link::Down(_) => fl!("tray-daemon-down"),
            Link::Up(core) => fl!("tray-core-state", state = core.state.key()),
        }
    }

    /// Lines below the headline in the tooltip.
    fn details(&self) -> Vec<String> {
        let core = match &self.view.link {
            Link::Connecting => return Vec::new(),
            Link::Down(reason) => return vec![reason.clone()],
            Link::Up(core) => core,
        };
        let mut lines = vec![
            detail(
                fl!("ctl-label-profile"),
                core.profile
                    .clone()
                    .unwrap_or_else(|| fl!("ctl-profile-unmanaged-short")),
            ),
            detail(
                fl!("tray-label-core"),
                core.version.clone().unwrap_or_else(|| fl!("not-installed")),
            ),
        ];
        if let Some(mode) = &self.view.mode {
            lines.push(detail(fl!("tray-label-mode"), mode.current.clone()));
        }
        let containers = &self.view.containers;
        if !containers.is_empty() {
            let running = containers
                .iter()
                .filter(|c| c.state == CoreState::Running)
                .count();
            lines.push(detail(
                fl!("ctl-label-containers"),
                fl!(
                    "ctl-containers-summary",
                    running = running,
                    total = containers.len()
                ),
            ));
        }
        if matches!(core.state, CoreState::Backoff | CoreState::Failed)
            && let Some(exit) = &core.last_exit
        {
            lines.push(detail(fl!("ctl-label-last-exit"), exit.clone()));
        }
        lines
    }

    fn profile_menu(&self, idle: bool) -> Entry {
        let profiles = &self.view.profiles;
        let active = profiles.iter().position(|p| p.active);
        let label = match active {
            Some(index) => detail(fl!("ctl-label-profile"), profiles[index].name.clone()),
            None => fl!("ctl-label-profile"),
        };
        let mut items: Vec<Entry> = profiles
            .iter()
            .map(|p| Entry::Radio {
                label: p.name.clone(),
                checked: p.active,
                enabled: idle,
                command: (!p.active).then(|| Command::Profile(p.id.clone())),
            })
            .collect();
        if items.is_empty() {
            items.push(Entry::label(fl!("tray-no-profiles")));
        }
        if profiles.iter().any(|p| p.remote) {
            items.push(Entry::Separator);
            items.push(Entry::item(
                fl!("tray-update-profiles"),
                idle,
                "download",
                Command::UpdateProfiles,
            ));
        }
        Entry::Sub {
            label,
            icon: "",
            items,
        }
    }

    /// One checkbox per container: checked while it runs, a click starts
    /// or stops it.
    fn container_menu(&self, idle: bool) -> Option<Entry> {
        let containers = &self.view.containers;
        if containers.is_empty() {
            return None;
        }
        let running = containers
            .iter()
            .filter(|c| c.state == CoreState::Running)
            .count();
        let items = containers
            .iter()
            .map(|c| {
                let on = c.state == CoreState::Running;
                let settled = matches!(
                    c.state,
                    CoreState::Running | CoreState::Stopped | CoreState::Failed
                );
                let action = if on {
                    ContainerAction::Stop
                } else {
                    ContainerAction::Start
                };
                Entry::Check {
                    label: c.name.clone(),
                    checked: on,
                    enabled: idle && settled && (on || c.ready),
                    command: Command::Container(c.id.clone(), action),
                }
            })
            .collect();
        Some(Entry::Sub {
            label: detail(
                fl!("ctl-label-containers"),
                fl!(
                    "ctl-containers-summary",
                    running = running,
                    total = containers.len()
                ),
            ),
            icon: "utilities-system-monitor",
            items,
        })
    }

    fn mode_menu(&self, idle: bool) -> Option<Entry> {
        let mode = self.view.mode.as_ref().filter(|mode| mode.all.len() > 1)?;
        let items = mode
            .all
            .iter()
            .map(|m| {
                let checked = m.eq_ignore_ascii_case(&mode.current);
                Entry::Radio {
                    label: m.clone(),
                    checked,
                    enabled: idle,
                    command: (!checked).then(|| Command::Mode(mode.api.clone(), m.clone())),
                }
            })
            .collect();
        Some(Entry::Sub {
            label: detail(fl!("tray-label-mode"), mode.current.clone()),
            icon: "",
            items,
        })
    }

    fn menu(&self) -> Vec<Entry> {
        let idle = self.busy.is_none();
        let mut items = vec![Entry::label(self.headline()), Entry::Separator];
        if let Some(core) = self.core() {
            let stopped = matches!(core.state, CoreState::Stopped | CoreState::Failed);
            let stopping = core.state == CoreState::Stopping;
            items.push(if stopped {
                Entry::item(
                    fl!("tray-start"),
                    idle,
                    "media-playback-start",
                    Command::Start,
                )
            } else {
                Entry::item(
                    fl!("tray-stop"),
                    idle && !stopping,
                    "media-playback-stop",
                    Command::Stop,
                )
            });
            items.push(Entry::item(
                fl!("tray-restart"),
                idle && !stopped && !stopping,
                "view-refresh",
                Command::Restart,
            ));
            items.push(Entry::Separator);
            items.push(self.profile_menu(idle));
            items.extend(self.mode_menu(idle));
            items.extend(self.container_menu(idle));
            items.push(Entry::Separator);
        } else if cfg!(windows) && matches!(self.view.link, Link::Down(_)) {
            items.push(Entry::item(
                fl!("tray-start-service"),
                true,
                "",
                Command::StartService,
            ));
            items.push(Entry::Separator);
        }
        items.push(Entry::item(
            fl!("tray-open-dashboard"),
            true,
            "utilities-terminal",
            Command::Dashboard,
        ));
        if let Some(url) = self.core().and_then(|core| core.sub_store.clone()) {
            items.push(Entry::item(
                fl!("tray-open-sub-store"),
                true,
                "internet-web-browser",
                Command::Open(url),
            ));
        }
        items.extend([
            Entry::Separator,
            Entry::Check {
                label: fl!("tray-autostart"),
                checked: self.autostart,
                enabled: true,
                command: Command::Autostart(!self.autostart),
            },
            Entry::item(fl!("tray-quit"), true, "application-exit", Command::Quit),
        ]);
        items
    }
}

/// `label: value` in the current language.
fn detail(label: String, value: String) -> String {
    fl!("tray-detail", label = label, value = value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tray(link: Link) -> BoardTray {
        let (tx, _rx) = mpsc::unbounded_channel();
        BoardTray {
            view: View {
                link,
                ..View::default()
            },
            busy: None,
            autostart: false,
            actions: tx,
        }
    }

    fn core(state: CoreState) -> Link {
        Link::Up(Box::new(Core {
            state,
            version: Some("1.12.0".into()),
            profile: Some("home_lab".into()),
            last_exit: Some("exit status 1".into()),
            sub_store: None,
        }))
    }

    #[test]
    fn icon_follows_the_state() {
        assert_eq!(tray(Link::Connecting).tone(), Tone::Stopped);
        assert_eq!(tray(Link::Down("no socket".into())).tone(), Tone::Error);
        assert_eq!(tray(core(CoreState::Running)).tone(), Tone::Running);
        assert_eq!(tray(core(CoreState::Backoff)).tone(), Tone::Busy);
        assert_eq!(tray(core(CoreState::Failed)).tone(), Tone::Error);
        let mut busy = tray(core(CoreState::Running));
        busy.busy = Some(Op::Restart);
        assert_eq!(busy.tone(), Tone::Busy);
    }

    #[test]
    fn one_request_at_a_time() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tray = tray(core(CoreState::Running));
        tray.actions = tx;
        tray.start(Op::Restart, Job::Daemon(Request::Restart));
        tray.start(Op::Stop, Job::Daemon(Request::Stop));
        assert_eq!(tray.busy, Some(Op::Restart));
        assert!(matches!(rx.try_recv(), Ok(Action::Run(Op::Restart, _))));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn tooltip_explains_failures() {
        let failed = tray(core(CoreState::Failed));
        let details = failed.details();
        assert_eq!(details.len(), 3);
        assert!(details[2].contains("exit status 1"));
        assert_eq!(tray(core(CoreState::Running)).details().len(), 2);
        let down = tray(Link::Down("no socket".into()));
        assert_eq!(down.details(), vec!["no socket".to_owned()]);
    }

    #[test]
    fn containers_toggle_from_the_menu() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tray = tray(core(CoreState::Running));
        tray.actions = tx;
        assert!(tray.container_menu(true).is_none());
        tray.view.containers = vec![
            TrayContainer {
                id: "aa".into(),
                name: "dev_box".into(),
                state: CoreState::Running,
                ready: true,
            },
            TrayContainer {
                id: "bb".into(),
                name: "web".into(),
                state: CoreState::Stopped,
                ready: false,
            },
        ];
        let Some(Entry::Sub { label, items, .. }) = tray.container_menu(true) else {
            panic!("no container menu");
        };
        assert!(label.contains("1"));
        let Entry::Check {
            label,
            checked,
            enabled,
            command,
        } = &items[0]
        else {
            panic!("not a checkbox");
        };
        assert!(*checked && *enabled);
        assert_eq!(label, "dev_box");
        let Entry::Check { enabled, .. } = &items[1] else {
            panic!("not a checkbox");
        };
        assert!(!enabled, "no root filesystem yet");
        tray.invoke(command.clone());
        assert!(matches!(
            rx.try_recv(),
            Ok(Action::Run(
                Op::Container,
                Job::Daemon(Request::ContainerControl {
                    action: ContainerAction::Stop,
                    ..
                })
            ))
        ));
        assert_eq!(tray.details().len(), 3);
    }

    #[test]
    fn menu_follows_the_state() {
        let running = tray(core(CoreState::Running));
        let menu = running.menu();
        assert!(menu.contains(&Entry::item(
            fl!("tray-stop"),
            true,
            "media-playback-stop",
            Command::Stop
        )));
        let Some(Entry::Sub { items, .. }) = menu
            .iter()
            .find(|e| matches!(e, Entry::Sub { label, .. } if label.contains("home_lab") || label == &fl!("ctl-label-profile")))
        else {
            panic!("no profile menu: {menu:?}");
        };
        assert_eq!(items, &[Entry::label(fl!("tray-no-profiles"))]);
        let stopped = tray(core(CoreState::Stopped)).menu();
        assert!(stopped.iter().any(|e| matches!(
            e,
            Entry::Item {
                command: Some(Command::Start),
                ..
            }
        )));
        let down = tray(Link::Down("no socket".into())).menu();
        assert_eq!(
            down.iter().any(|e| matches!(
                e,
                Entry::Item {
                    command: Some(Command::StartService),
                    ..
                }
            )),
            cfg!(windows)
        );
    }

    #[test]
    fn picking_the_active_profile_does_nothing() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tray = tray(core(CoreState::Running));
        tray.actions = tx;
        tray.view.profiles = vec![
            Choice {
                id: "a".into(),
                name: "home".into(),
                active: true,
                remote: false,
            },
            Choice {
                id: "b".into(),
                name: "office".into(),
                active: false,
                remote: true,
            },
        ];
        let Entry::Sub { items, .. } = tray.profile_menu(true) else {
            panic!("not a submenu");
        };
        assert!(matches!(
            &items[0],
            Entry::Radio {
                checked: true,
                command: None,
                ..
            }
        ));
        let Entry::Radio {
            command: Some(command),
            ..
        } = &items[1]
        else {
            panic!("no command for the other profile");
        };
        tray.invoke(command.clone());
        assert!(matches!(
            rx.try_recv(),
            Ok(Action::Run(Op::Profile, Job::Daemon(Request::ProfileActivate { ref id, .. }))) if id == "b"
        ));
        assert!(
            items.contains(&Entry::Separator),
            "remote profiles can be updated"
        );
    }

    #[test]
    fn login_item_keeps_the_global_options() {
        let options = Options {
            socket: Some("/tmp/sb.sock".into()),
            lang: None,
        };
        let command = tray_command(std::path::Path::new("/usr/bin/singbox-board"), &options);
        assert_eq!(
            command,
            ["/usr/bin/singbox-board", "--socket", "/tmp/sb.sock", "tray"]
                .map(OsString::from)
                .to_vec()
        );
    }
}
