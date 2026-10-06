//! System tray icon, published as a StatusNotifierItem over D-Bus.
//!
//! StatusNotifierItem does not depend on the display server, so the tray
//! works on Wayland and X11 alike, in every host of the protocol: KDE
//! Plasma, GNOME with the AppIndicator extension, Cinnamon, XFCE, LXQt, and
//! the tray modules of Waybar and other bars. Like the TUI, the tray is a
//! client of the daemon: it polls the status and turns menu clicks into
//! requests.

mod desktop;
mod icon;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip, TrayMethods};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, mpsc};

use self::desktop::Notifier;
use self::icon::Tone;
use crate::clash::ClashClient;
use crate::client::DaemonClient;
use crate::i18n::{self, fl};
use crate::protocol::{ClashApi, Component, ContainerAction, CoreState, Request, Status};
use crate::util::error_chain;

/// Owned on the session bus while a tray runs, so that starting a second
/// one in the same desktop session does not add a second icon.
const BUS_NAME: &str = "io.github.MiChongs.SingboxBoard.Tray";
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const CLASH_TIMEOUT: Duration = Duration::from_secs(2);

/// Global options given on the command line, passed on to the login item.
pub struct Options {
    pub socket: Option<PathBuf>,
    pub lang: Option<String>,
}

pub async fn run(client: DaemonClient, options: Options) -> Result<()> {
    let bus = zbus::Connection::session()
        .await
        .map_err(|err| anyhow!(fl!("tray-no-session-bus", error = err.to_string())))?;
    match bus
        .request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(_) => {}
        Err(zbus::Error::NameTaken) => bail!(fl!("tray-already-running")),
        Err(err) => {
            return Err(anyhow!(fl!("tray-no-session-bus", error = err.to_string())));
        }
    }
    let exe = std::env::current_exe().context(fl!("tray-no-executable"))?;
    let notifier = Notifier::new(bus);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = spawn_tray(&tx).await?;
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
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    loop {
        let action = tokio::select! {
            action = rx.recv() => action,
            _ = terminate.recv() => None,
            _ = hangup.recv() => None,
            _ = interrupt.recv() => None,
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
                if let Err(err) = desktop::open_terminal(&command) {
                    worker.failed("dashboard", &err).await;
                }
            }
            Some(Action::Open(url)) => {
                let worker = worker.clone();
                tokio::spawn(async move {
                    if let Err(err) = desktop::open_url(&url).await {
                        worker.failed("browser", &err).await;
                    }
                });
            }
            Some(Action::Autostart(enable)) => {
                let result = desktop::set_autostart(enable, &tray_command(&exe, &options));
                let enabled = desktop::autostart_enabled();
                handle.update(|tray| tray.autostart = enabled).await;
                if let Err(err) = result {
                    worker.failed("autostart", &err).await;
                }
            }
        }
    }
    poller.abort();
    handle.shutdown().await;
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

async fn spawn_tray(tx: &mpsc::UnboundedSender<Action>) -> Result<ksni::Handle<BoardTray>> {
    match BoardTray::new(tx.clone()).spawn().await {
        Ok(handle) => Ok(handle),
        Err(err @ (ksni::Error::Watcher(_) | ksni::Error::WontShow)) => {
            // Started before the panel (at login), or on a desktop without a
            // tray: wait for one to appear instead of giving up.
            eprintln!("{}", fl!("tray-waiting-host", reason = err.to_string()));
            BoardTray::new(tx.clone())
                .assume_sni_available(true)
                .spawn()
                .await
                .map_err(|err| anyhow!(fl!("tray-start-failed", error = err.to_string())))
        }
        Err(err) => Err(anyhow!(fl!("tray-start-failed", error = err.to_string()))),
    }
}

/// Everything an action needs once it left the menu.
#[derive(Clone)]
struct Worker {
    client: DaemonClient,
    handle: ksni::Handle<BoardTray>,
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
    handle: ksni::Handle<BoardTray>,
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

impl BoardTray {
    fn new(actions: mpsc::UnboundedSender<Action>) -> Self {
        Self {
            view: View::default(),
            busy: None,
            autostart: desktop::autostart_enabled(),
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

    fn core(&self) -> Option<&Core> {
        match &self.view.link {
            Link::Up(core) => Some(core),
            _ => None,
        }
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

    fn profile_menu(&self, idle: bool) -> MenuItem<Self> {
        let profiles = &self.view.profiles;
        let active = profiles.iter().position(|p| p.active);
        let label = match active {
            Some(index) => detail(fl!("ctl-label-profile"), profiles[index].name.clone()),
            None => fl!("ctl-label-profile"),
        };
        let mut submenu = Vec::new();
        if profiles.is_empty() {
            submenu.push(
                StandardItem {
                    label: fl!("tray-no-profiles"),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            let ids: Vec<String> = profiles.iter().map(|p| p.id.clone()).collect();
            submenu.push(
                RadioGroup {
                    selected: active.unwrap_or(usize::MAX),
                    select: Box::new(move |tray: &mut Self, index| {
                        if Some(index) != active
                            && let Some(id) = ids.get(index)
                        {
                            let request = Request::ProfileActivate {
                                id: id.clone(),
                                force: false,
                            };
                            tray.start(Op::Profile, Job::Daemon(request));
                        }
                    }),
                    options: profiles
                        .iter()
                        .map(|p| RadioItem {
                            label: mnemonic_free(&p.name),
                            enabled: idle,
                            ..Default::default()
                        })
                        .collect(),
                }
                .into(),
            );
        }
        if profiles.iter().any(|p| p.remote) {
            submenu.push(MenuItem::Separator);
            submenu.push(
                StandardItem {
                    label: fl!("tray-update-profiles"),
                    enabled: idle,
                    icon_name: "download".into(),
                    activate: Box::new(|tray: &mut Self| {
                        let request = Request::ProfileUpdate {
                            id: None,
                            force: false,
                        };
                        tray.start(Op::Update, Job::Daemon(request));
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        SubMenu {
            label: mnemonic_free(&label),
            submenu,
            ..Default::default()
        }
        .into()
    }

    /// One checkbox per container: checked while it runs, a click starts
    /// or stops it.
    fn container_menu(&self, idle: bool) -> Option<MenuItem<Self>> {
        let containers = &self.view.containers;
        if containers.is_empty() {
            return None;
        }
        let running = containers
            .iter()
            .filter(|c| c.state == CoreState::Running)
            .count();
        let submenu = containers
            .iter()
            .map(|c| {
                let id = c.id.clone();
                let on = c.state == CoreState::Running;
                let settled = matches!(
                    c.state,
                    CoreState::Running | CoreState::Stopped | CoreState::Failed
                );
                CheckmarkItem {
                    label: mnemonic_free(&c.name),
                    checked: on,
                    enabled: idle && settled && (on || c.ready),
                    activate: Box::new(move |tray: &mut Self| {
                        let action = if on {
                            ContainerAction::Stop
                        } else {
                            ContainerAction::Start
                        };
                        let request = Request::ContainerControl {
                            id: id.clone(),
                            action,
                        };
                        tray.start(Op::Container, Job::Daemon(request));
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        Some(
            SubMenu {
                label: mnemonic_free(&detail(
                    fl!("ctl-label-containers"),
                    fl!(
                        "ctl-containers-summary",
                        running = running,
                        total = containers.len()
                    ),
                )),
                icon_name: "utilities-system-monitor".into(),
                submenu,
                ..Default::default()
            }
            .into(),
        )
    }

    fn mode_menu(&self, idle: bool) -> Option<MenuItem<Self>> {
        let mode = self.view.mode.as_ref().filter(|mode| mode.all.len() > 1)?;
        let selected = mode
            .all
            .iter()
            .position(|m| m.eq_ignore_ascii_case(&mode.current));
        let api = mode.api.clone();
        let modes = mode.all.clone();
        let radio = RadioGroup {
            selected: selected.unwrap_or(usize::MAX),
            select: Box::new(move |tray: &mut Self, index| {
                if Some(index) != selected
                    && let Some(mode) = modes.get(index)
                {
                    tray.start(Op::Mode, Job::Mode(api.clone(), mode.clone()));
                }
            }),
            options: mode
                .all
                .iter()
                .map(|m| RadioItem {
                    label: mnemonic_free(m),
                    enabled: idle,
                    ..Default::default()
                })
                .collect(),
        };
        Some(
            SubMenu {
                label: mnemonic_free(&detail(fl!("tray-label-mode"), mode.current.clone())),
                submenu: vec![radio.into()],
                ..Default::default()
            }
            .into(),
        )
    }
}

/// `label: value` in the current language.
fn detail(label: String, value: String) -> String {
    fl!("tray-detail", label = label, value = value)
}

/// Menu hosts read `_` as the mark of an access key; `__` is a literal one.
fn mnemonic_free(text: &str) -> String {
    text.replace('_', "__")
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
        match self.core() {
            Some(core) if core.state == CoreState::Failed && self.busy.is_none() => {
                ksni::Status::NeedsAttention
            }
            _ => ksni::Status::Active,
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
        let idle = self.busy.is_none();
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: mnemonic_free(&self.headline()),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];
        if let Some(core) = self.core() {
            let stopped = matches!(core.state, CoreState::Stopped | CoreState::Failed);
            let stopping = core.state == CoreState::Stopping;
            items.extend([
                StandardItem {
                    label: fl!("tray-start"),
                    visible: stopped,
                    enabled: idle,
                    icon_name: "media-playback-start".into(),
                    activate: Box::new(|tray: &mut Self| {
                        tray.start(Op::Start, Job::Daemon(Request::Start));
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: fl!("tray-stop"),
                    visible: !stopped,
                    enabled: idle && !stopping,
                    icon_name: "media-playback-stop".into(),
                    activate: Box::new(|tray: &mut Self| {
                        tray.start(Op::Stop, Job::Daemon(Request::Stop));
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: fl!("tray-restart"),
                    enabled: idle && !stopped && !stopping,
                    icon_name: "view-refresh".into(),
                    activate: Box::new(|tray: &mut Self| {
                        tray.start(Op::Restart, Job::Daemon(Request::Restart));
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                self.profile_menu(idle),
            ]);
            items.extend(self.mode_menu(idle));
            items.extend(self.container_menu(idle));
            items.push(MenuItem::Separator);
        }
        items.push(
            StandardItem {
                label: fl!("tray-open-dashboard"),
                icon_name: "utilities-terminal".into(),
                activate: Box::new(|tray: &mut Self| tray.send(Action::Dashboard)),
                ..Default::default()
            }
            .into(),
        );
        if let Some(url) = self.core().and_then(|core| core.sub_store.clone()) {
            items.push(
                StandardItem {
                    label: fl!("tray-open-sub-store"),
                    icon_name: "internet-web-browser".into(),
                    activate: Box::new(move |tray: &mut Self| tray.send(Action::Open(url.clone()))),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.extend([
            MenuItem::Separator,
            CheckmarkItem {
                label: fl!("tray-autostart"),
                checked: self.autostart,
                activate: Box::new(|tray: &mut Self| tray.send(Action::Autostart(!tray.autostart))),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: fl!("tray-quit"),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| tray.send(Action::Quit)),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
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
        let Some(MenuItem::SubMenu(menu)) = tray.container_menu(true) else {
            panic!("no container menu");
        };
        assert!(menu.label.contains("1"));
        let MenuItem::Checkmark(first) = &menu.submenu[0] else {
            panic!("not a checkbox");
        };
        assert!(first.checked && first.enabled);
        assert_eq!(first.label, "dev__box");
        let MenuItem::Checkmark(second) = &menu.submenu[1] else {
            panic!("not a checkbox");
        };
        assert!(!second.enabled, "no root filesystem yet");
        (first.activate)(&mut tray);
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
    fn underscores_are_not_access_keys() {
        assert_eq!(mnemonic_free("home_lab"), "home__lab");
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
