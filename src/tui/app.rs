//! TUI state, input handling and actions.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use futures::StreamExt;
use ratatui::widgets::{ListState, TableState};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;

use super::connections::ConnectionsView;
use super::containers::{
    ContainerContentPurpose, ContainerSavePurpose, ContainersView, ShellCommand,
};
use super::core::CoreView;
use super::popup::{
    ExternalEdit, Input, InputOutcome, InputPurpose, Menu, MenuAction, MenuItem, MenuOutcome,
};
use super::profiles::{ContentPurpose, ProfilesView, SavePurpose};
use super::tasks::{self, EventTx};
use crate::clash::{ClashClient, Configs, Connections, DEFAULT_TEST_URL, Proxies, Proxy};
use crate::client::DaemonClient;
use crate::i18n::fl;
use crate::protocol::{
    ClashApi, Component, ComponentAction, ComponentStatus, Container, ContainerOverview,
    CoreReleasePage, CoreSource, CoreState, ExecResult, ImageList, LogEntry, Profile, ProfileList,
    Request, Status, StoredCore, UpdateInfo,
};
use crate::substore::{Entry, Overview, provider_snippet};
use crate::util::{error_chain, text_width};

const MAX_LOG_LINES: usize = 5000;
/// Traffic samples kept, one a second.
pub(super) const HISTORY_POINTS: usize = 300;
const TOAST_TTL: Duration = Duration::from_secs(5);
const DELAY_TIMEOUT_MS: u32 = 5000;
const DELAY_CONCURRENCY: usize = 8;

pub enum AppEvent {
    Status(Result<Box<Status>, String>),
    LogsReset,
    Log(LogEntry),
    LogsDisconnected,
    Connections(Connections),
    Proxies(Proxies),
    Configs(Configs),
    ClashError(String),
    SubStore(Result<Overview, String>),
    CoreSources(Result<(Vec<CoreSource>, String), String>),
    CoreReleases {
        source: String,
        page: u32,
        result: Result<CoreReleasePage, String>,
    },
    CoreInstalled(Result<Vec<StoredCore>, String>),
    Profiles(Result<ProfileList, String>),
    ProfileContent {
        purpose: ContentPurpose,
        result: Result<(Profile, String), String>,
    },
    ProfileSaved {
        id: u64,
        purpose: SavePurpose,
        result: Result<(Profile, String), String>,
    },
    Delay {
        name: String,
        result: Result<u32, String>,
    },
    ActionDone {
        id: u64,
        result: Result<String, String>,
    },
    UpdateChecked {
        id: u64,
        result: Result<UpdateInfo, String>,
    },
    Containers(Result<ContainerOverview, String>),
    ContainerContent {
        purpose: ContainerContentPurpose,
        result: Result<(Container, String), String>,
    },
    ContainerSaved {
        id: u64,
        purpose: ContainerSavePurpose,
        result: Result<(Container, String), String>,
    },
    ContainerImages(Result<ImageList, String>),
    ContainerExec {
        id: u64,
        name: String,
        command: String,
        result: Result<ExecResult, String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Proxies,
    Connections,
    Logs,
    SubStore,
    Core,
    Profiles,
    Containers,
}

impl Tab {
    #[cfg(unix)]
    pub const ALL: &[Tab] = &[
        Tab::Overview,
        Tab::Proxies,
        Tab::Connections,
        Tab::Logs,
        Tab::SubStore,
        Tab::Core,
        Tab::Profiles,
        Tab::Containers,
    ];
    /// Containers need Linux.
    #[cfg(windows)]
    pub const ALL: &[Tab] = &[
        Tab::Overview,
        Tab::Proxies,
        Tab::Connections,
        Tab::Logs,
        Tab::SubStore,
        Tab::Core,
        Tab::Profiles,
    ];

    pub fn title(self) -> String {
        match self {
            Tab::Overview => fl!("tab-overview"),
            Tab::Proxies => fl!("tab-proxies"),
            Tab::Connections => fl!("tab-connections"),
            Tab::Logs => fl!("tab-logs"),
            Tab::SubStore => "Sub-Store".to_owned(),
            Tab::Core => fl!("tab-core"),
            Tab::Profiles => fl!("tab-profiles"),
            Tab::Containers => fl!("tab-containers"),
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Groups,
    Members,
}

/// Which pane of the Sub-Store tab has the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreFocus {
    Components,
    Entries,
}

#[derive(Debug, Clone)]
pub enum PendingAction {
    Stop,
    Restart,
    Update,
    CloseAllConnections,
    /// The connections the filter shows.
    CloseConnections {
        ids: Vec<String>,
    },
    CoreInstall {
        source: String,
        tag: String,
        variant: String,
        activate: bool,
    },
    CoreActivate {
        id: String,
    },
    CoreRemove {
        id: String,
    },
    CoreSourceRemove {
        id: String,
    },
    ProfileUse {
        id: String,
    },
    ProfileDelete {
        id: String,
    },
    ProfileAdopt,
    ContainerStop {
        id: String,
    },
    ContainerRestart {
        id: String,
    },
    /// Install or update kurumi-containerd.
    ContainerRuntime,
}

pub enum Popup {
    Help,
    Confirm {
        message: String,
        action: PendingAction,
    },
    Message {
        title: String,
        body: String,
        error: bool,
        /// Text `y` copies to the clipboard.
        copy: Option<String>,
    },
    /// First-run question; `sub_store` holds the first answer once given.
    Setup {
        sub_store: Option<bool>,
    },
    Menu(Menu),
    /// Single-line text input.
    Input(Input),
    /// Keys of the tree view.
    EditorHelp,
    /// Keys of the code editor.
    CodeHelp,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    at: Instant,
}

#[derive(Default)]
pub struct Traffic {
    pub up_speed: u64,
    pub down_speed: u64,
    pub up_total: u64,
    pub down_total: u64,
    pub memory: u64,
    pub up_history: VecDeque<u64>,
    pub down_history: VecDeque<u64>,
    last: Option<(Instant, u64, u64)>,
}

impl Traffic {
    fn update(&mut self, connections: &Connections) {
        let now = Instant::now();
        if let Some((at, up, down)) = self.last {
            let secs = now.duration_since(at).as_secs_f64().max(0.001);
            self.up_speed = (connections.upload_total.saturating_sub(up) as f64 / secs) as u64;
            self.down_speed =
                (connections.download_total.saturating_sub(down) as f64 / secs) as u64;
            push_bounded(&mut self.up_history, self.up_speed, HISTORY_POINTS);
            push_bounded(&mut self.down_history, self.down_speed, HISTORY_POINTS);
        }
        self.last = Some((now, connections.upload_total, connections.download_total));
        self.up_total = connections.upload_total;
        self.down_total = connections.download_total;
        self.memory = connections.memory;
    }

    fn reset(&mut self) {
        self.last = None;
        self.up_speed = 0;
        self.down_speed = 0;
    }
}

pub struct App {
    pub should_quit: bool,
    pub tab: Tab,
    pub popup: Option<Popup>,
    pub(super) client: DaemonClient,
    pub(super) tx: EventTx,
    clash_api_tx: watch::Sender<Option<ClashApi>>,
    refresh: Arc<Notify>,
    clash: Option<ClashClient>,

    pub status: Option<Box<Status>>,
    pub status_error: Option<String>,

    pub logs: VecDeque<LogEntry>,
    pub logs_connected: bool,
    /// Lines scrolled up from the bottom; 0 follows new output.
    pub log_scroll: usize,

    pub clash_error: Option<String>,
    pub configs: Option<Configs>,
    pub proxies: Proxies,
    pub groups: Vec<String>,
    pub focus: Focus,
    pub group_state: ListState,
    pub member_state: TableState,
    pub delays: HashMap<String, Result<u32, String>>,
    pub testing: HashSet<String>,

    pub connections: ConnectionsView,
    pub traffic: Traffic,

    pub sub_store: Option<Overview>,
    pub sub_store_error: Option<String>,
    sub_store_api_tx: watch::Sender<Option<String>>,
    store_refresh: Arc<Notify>,
    pub store_focus: StoreFocus,
    pub comp_state: TableState,
    pub entry_state: TableState,
    wizard_shown: bool,
    clipboard: Option<String>,

    pub core: CoreView,
    pub profiles: ProfilesView,
    pub containers: ContainersView,
    /// An interactive program waiting to get the terminal (a container shell).
    pub(super) shell: Option<ShellCommand>,
    /// Text waiting to be opened in `$EDITOR` by the event loop.
    pub(super) external: Option<ExternalEdit>,
    /// Started for one profile (`singbox-board profile edit`): quit when
    /// its editor closes.
    pub(super) edit_only: bool,
    /// The last save message, printed after an edit-only session.
    pub(super) exit_message: Option<String>,

    pub toast: Option<Toast>,
    pub busy: Vec<(u64, String)>,
    next_action: u64,
    pub frame: usize,
}

impl App {
    pub fn new(client: DaemonClient, tx: EventTx) -> (Self, JoinSet<()>) {
        let (clash_api_tx, clash_api_rx) = watch::channel(None);
        let (sub_store_api_tx, sub_store_api_rx) = watch::channel(None);
        let refresh = Arc::new(Notify::new());
        let store_refresh = Arc::new(Notify::new());
        let background = tasks::spawn_all(
            client.clone(),
            tx.clone(),
            clash_api_rx,
            refresh.clone(),
            sub_store_api_rx,
            store_refresh.clone(),
        );
        let app = Self {
            should_quit: false,
            tab: Tab::Overview,
            popup: None,
            client,
            tx,
            clash_api_tx,
            refresh,
            clash: None,
            status: None,
            status_error: None,
            logs: VecDeque::new(),
            logs_connected: false,
            log_scroll: 0,
            clash_error: None,
            configs: None,
            proxies: Proxies::default(),
            groups: Vec::new(),
            focus: Focus::Groups,
            group_state: ListState::default(),
            member_state: TableState::default(),
            delays: HashMap::new(),
            testing: HashSet::new(),
            connections: ConnectionsView::default(),
            traffic: Traffic::default(),
            sub_store: None,
            sub_store_error: None,
            sub_store_api_tx,
            store_refresh,
            store_focus: StoreFocus::Components,
            comp_state: TableState::default().with_selected(Some(0)),
            entry_state: TableState::default(),
            wizard_shown: false,
            clipboard: None,
            core: CoreView::default(),
            profiles: ProfilesView::default(),
            containers: ContainersView::default(),
            shell: None,
            external: None,
            edit_only: false,
            exit_message: None,
            toast: None,
            busy: Vec::new(),
            next_action: 0,
            frame: 0,
        };
        (app, background)
    }

    pub fn socket(&self) -> String {
        self.client.socket().display().to_string()
    }

    // ----- events -------------------------------------------------------

    pub fn on_tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.profiles_tick();
        self.containers_tick();
        if self
            .toast
            .as_ref()
            .is_some_and(|t| t.at.elapsed() > TOAST_TTL)
        {
            self.toast = None;
        }
    }

    pub fn on_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Status(Ok(status)) => {
                if self.clash_api_tx.borrow().as_ref() != status.clash_api.as_ref() {
                    self.clash = status
                        .clash_api
                        .as_ref()
                        .and_then(|api| ClashClient::new(api).ok());
                    self.clash_api_tx.send_replace(status.clash_api.clone());
                }
                let api = status
                    .components
                    .iter()
                    .find(|c| c.component == Component::SubStore && c.state == CoreState::Running)
                    .and_then(|c| c.api.clone());
                if *self.sub_store_api_tx.borrow() != api {
                    if api.is_none() {
                        self.sub_store = None;
                    }
                    self.sub_store_api_tx.send_replace(api);
                }
                if status.setup_required && !self.wizard_shown && self.popup.is_none() {
                    self.wizard_shown = true;
                    self.popup = Some(Popup::Setup { sub_store: None });
                }
                self.status = Some(status);
                self.status_error = None;
            }
            AppEvent::Status(Err(err)) => {
                self.status = None;
                self.status_error = Some(err);
            }
            AppEvent::LogsReset => {
                self.logs.clear();
                self.logs_connected = true;
            }
            AppEvent::Log(entry) => {
                push_bounded(&mut self.logs, entry, MAX_LOG_LINES);
                if self.log_scroll > 0 {
                    // Keep the viewport still while the user reads older lines.
                    self.log_scroll = (self.log_scroll + 1).min(self.logs.len());
                }
            }
            AppEvent::LogsDisconnected => self.logs_connected = false,
            AppEvent::Connections(connections) => {
                self.clash_error = None;
                self.traffic.update(&connections);
                self.connections
                    .take(connections.connections.unwrap_or_default());
            }
            AppEvent::Proxies(proxies) => self.set_proxies(proxies),
            AppEvent::Configs(configs) => self.configs = Some(configs),
            AppEvent::ClashError(err) => {
                self.clash_error = Some(err);
                self.connections.clear();
                self.traffic.reset();
            }
            AppEvent::SubStore(Ok(overview)) => {
                self.sub_store_error = None;
                let len = overview.entries.len();
                self.sub_store = Some(overview);
                let index = self
                    .entry_state
                    .selected()
                    .map(|i| i.min(len.saturating_sub(1)));
                self.entry_state
                    .select(if len == 0 { None } else { index.or(Some(0)) });
            }
            AppEvent::SubStore(Err(err)) => self.sub_store_error = Some(err),
            AppEvent::CoreSources(result) => self.core_sources_loaded(result),
            AppEvent::CoreReleases {
                source,
                page,
                result,
            } => self.core_releases_loaded(source, page, result),
            AppEvent::CoreInstalled(result) => self.core_installed_loaded(result),
            AppEvent::Profiles(result) => self.profiles_loaded(result),
            AppEvent::ProfileContent { purpose, result } => {
                self.profile_content_loaded(purpose, result)
            }
            AppEvent::ProfileSaved {
                id,
                purpose,
                result,
            } => self.profile_saved(id, purpose, result),
            AppEvent::Delay { name, result } => {
                self.testing.remove(&name);
                self.delays.insert(name, result);
            }
            AppEvent::ActionDone { id, result } => {
                self.busy.retain(|(busy, _)| *busy != id);
                self.refresh.notify_one();
                self.store_refresh.notify_one();
                self.core_refresh_after_action();
                self.profiles_refresh_after_action();
                self.containers_refresh_after_action();
                match result {
                    Ok(message) => self.notify(message, false),
                    Err(err) => self.notify(err, true),
                }
            }
            AppEvent::UpdateChecked { id, result } => {
                self.busy.retain(|(busy, _)| *busy != id);
                match result {
                    Ok(info) if info.update_available => {
                        let version = if info.prerelease {
                            fl!("version-prerelease", version = info.latest.clone())
                        } else {
                            info.latest.clone()
                        };
                        self.popup = Some(Popup::Confirm {
                            message: fl!(
                                "tui-confirm-update",
                                version = version,
                                current = info.current.clone().unwrap_or_else(|| fl!("none"))
                            ),
                            action: PendingAction::Update,
                        });
                    }
                    Ok(info) => self.notify(
                        fl!("up-to-date", name = format!("sing-box {}", info.latest)),
                        false,
                    ),
                    Err(err) => self.notify(err, true),
                }
            }
            AppEvent::Containers(result) => self.containers_loaded(result),
            AppEvent::ContainerContent { purpose, result } => {
                self.container_content_loaded(purpose, result)
            }
            AppEvent::ContainerSaved {
                id,
                purpose,
                result,
            } => self.container_saved(id, purpose, result),
            AppEvent::ContainerImages(result) => self.container_images_loaded(result),
            AppEvent::ContainerExec {
                id,
                name,
                command,
                result,
            } => self.container_exec_done(id, name, command, result),
        }
    }

    /// Short messages go to the footer, long or multi-line ones to a popup.
    /// A short success of a few lines (e.g. "saved" plus "check passed")
    /// still fits the footer on one line.
    pub(super) fn notify(&mut self, text: String, error: bool) {
        let joined = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("  ");
        let text = if !error && text_width(&joined) <= 90 {
            joined
        } else {
            text
        };
        if text.contains('\n') || text_width(&text) > 90 {
            let title = if error {
                fl!("tui-title-error")
            } else {
                fl!("tui-title-result")
            };
            self.popup = Some(Popup::Message {
                title,
                body: text,
                error,
                copy: None,
            });
        } else {
            self.toast = Some(Toast {
                text,
                error,
                at: Instant::now(),
            });
        }
    }

    fn set_proxies(&mut self, proxies: Proxies) {
        let selected = self.selected_group().map(str::to_owned);
        // GLOBAL lists every outbound in configuration order; use it to order groups.
        let mut groups: Vec<String> = proxies
            .proxies
            .get("GLOBAL")
            .and_then(|global| global.all.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|name| {
                proxies
                    .proxies
                    .get(name)
                    .is_some_and(|p| p.is_group() && !p.hidden)
            })
            .collect();
        let mut rest: Vec<String> = proxies
            .proxies
            .iter()
            .filter(|(name, p)| {
                p.is_group() && !p.hidden && *name != "GLOBAL" && !groups.contains(name)
            })
            .map(|(name, _)| name.clone())
            .collect();
        rest.sort();
        groups.extend(rest);
        if proxies.proxies.contains_key("GLOBAL") {
            groups.push("GLOBAL".to_owned());
        }
        self.proxies = proxies;
        self.groups = groups;
        let index = selected
            .and_then(|name| self.groups.iter().position(|g| *g == name))
            .or(if self.groups.is_empty() {
                None
            } else {
                Some(0)
            });
        self.group_state.select(index);
        let members = self.members().len();
        match self.member_state.selected() {
            Some(i) if i >= members => self.member_state.select(members.checked_sub(1)),
            None if members > 0 => self.member_state.select(Some(0)),
            _ => {}
        }
    }

    pub fn selected_group(&self) -> Option<&str> {
        self.group_state
            .selected()
            .and_then(|i| self.groups.get(i))
            .map(String::as_str)
    }

    pub fn group(&self, name: &str) -> Option<&Proxy> {
        self.proxies.proxies.get(name)
    }

    pub fn members(&self) -> Vec<String> {
        self.selected_group()
            .and_then(|g| self.group(g))
            .and_then(|g| g.all.clone())
            .unwrap_or_default()
    }

    /// Latest delay: a manual test result, else the Clash API history.
    pub fn delay_of(&self, name: &str) -> Option<Result<u32, String>> {
        if let Some(result) = self.delays.get(name) {
            return Some(result.clone());
        }
        let delay = self.group(name)?.last_delay()?;
        Some(if delay == 0 {
            Err("timeout".to_owned())
        } else {
            Ok(delay)
        })
    }

    // ----- input --------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl+C copies in an open editor; quitting would lose the edits.
        let editing =
            (self.tab == Tab::Profiles && self.profiles.code.is_some()) || self.toml_active();
        if ctrl_c && !editing && self.popup.is_none() {
            self.quit();
            return;
        }
        if let Some(popup) = self.popup.take() {
            let key = if ctrl_c {
                KeyEvent::from(KeyCode::Esc)
            } else {
                key
            };
            self.on_popup_key(popup, key);
            return;
        }
        // The code editor takes every key: digits and Tab are text there.
        if self.code_active() {
            self.code_on_key(key);
            return;
        }
        if self.toml_active() {
            self.toml_on_key(key);
            return;
        }
        // The tree view takes every key but tab switching, so letters
        // there never start or stop sing-box.
        let switches_tab = matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
            || matches!(key.code, KeyCode::Char(c) if tab_number(c).is_some());
        if self.tab == Tab::Profiles && self.profiles.editor.is_some() && !switches_tab {
            self.editor_on_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit(),
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Tab => self.tab = Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()],
            KeyCode::BackTab => {
                self.tab = Tab::ALL[(self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()]
            }
            KeyCode::Char(c) if tab_number(c).is_some() => {
                self.tab = Tab::ALL[tab_number(c).unwrap_or(0)]
            }
            KeyCode::Char('s') => self.daemon_action(&fl!("busy-starting"), Request::Start),
            KeyCode::Char('x') => self.confirm(fl!("tui-confirm-stop"), PendingAction::Stop),
            KeyCode::Char('r') => self.confirm(fl!("tui-confirm-restart"), PendingAction::Restart),
            KeyCode::Char('R') => self.daemon_action(&fl!("busy-reloading"), Request::Reload),
            KeyCode::Char('c') => self.daemon_action(&fl!("busy-checking"), Request::Check),
            KeyCode::Char('u') => self.check_update(),
            KeyCode::Char('m') => self.cycle_mode(),
            _ => match self.tab {
                Tab::Overview => {}
                Tab::Proxies => self.on_proxies_key(key),
                Tab::Connections => self.connections_on_key(key),
                Tab::Logs => self.on_logs_key(key),
                Tab::SubStore => self.on_store_key(key),
                Tab::Core => self.core_on_key(key),
                Tab::Profiles => self.profiles_on_key(key),
                Tab::Containers => self.containers_on_key(key),
            },
        }
        match self.tab {
            Tab::Core => self.core_tab_opened(),
            Tab::Profiles => self.profiles_tab_opened(),
            Tab::Containers => self.containers_tab_opened(),
            _ => {}
        }
    }

    /// Quits, unless the editor has unsaved changes: then it comes to the
    /// front and asks what to do with them.
    fn quit(&mut self) {
        if self
            .containers
            .editor
            .as_mut()
            .is_some_and(|editor| editor.dirty())
        {
            self.tab = Tab::Containers;
            self.toml_command(super::toml_editor::TomlCommand::Close);
            return;
        }
        // Edits in the tree view reach the text when it closes.
        self.code_close_tree(false);
        let unsaved = self.profiles.code.as_mut().is_some_and(|c| c.dirty());
        if !unsaved {
            self.should_quit = true;
            return;
        }
        self.tab = Tab::Profiles;
        self.code_close_tree(false);
        self.code_command(super::code::CodeCommand::Close);
    }

    /// Pasted text goes into an open text field or the code editor.
    pub fn on_paste(&mut self, text: &str) {
        match &mut self.popup {
            Some(Popup::Input(input)) => {
                input.paste(text);
                if matches!(input.purpose, InputPurpose::ConnectionFilter(_)) {
                    self.connections.set_filter(&input.value);
                }
            }
            Some(_) => {}
            None => {
                if self.code_active() {
                    self.code_on_paste(text);
                } else if self.toml_active() {
                    self.toml_on_paste(text);
                }
            }
        }
    }

    pub fn on_mouse(&mut self, event: MouseEvent) {
        if self.popup.is_none() && self.code_active() {
            self.code_on_mouse(event);
        } else if self.popup.is_none() && self.toml_active() {
            self.toml_on_mouse(event);
        }
    }

    /// The editors use the mouse; elsewhere the terminal keeps it for
    /// selecting text.
    pub fn wants_mouse(&self) -> bool {
        self.code_active() || self.toml_active()
    }

    /// Starts with one profile open in the editor and quits when it closes.
    pub fn start_editing(&mut self, profile: Profile, content: &str, force: bool) {
        self.edit_only = true;
        // The first-run questions can wait for the dashboard.
        self.wizard_shown = true;
        self.tab = Tab::Profiles;
        self.open_code_editor(profile, content);
        if let Some(code) = &mut self.profiles.code {
            code.force = force;
        }
    }

    /// Starts with one container's configuration open in the editor and
    /// quits when it closes.
    pub fn start_editing_container(&mut self, container: Container, content: &str, force: bool) {
        self.edit_only = true;
        self.wizard_shown = true;
        self.tab = Tab::Containers;
        self.open_toml_editor(container, content);
        if let Some(editor) = &mut self.containers.editor {
            editor.force = force;
        }
    }

    /// An interactive program that wants the terminal.
    pub fn take_shell(&mut self) -> Option<ShellCommand> {
        self.shell.take()
    }

    /// The shell of a container ended.
    pub fn shell_done(
        &mut self,
        shell: ShellCommand,
        result: anyhow::Result<std::process::ExitStatus>,
    ) {
        self.containers_refresh_after_action();
        match result {
            Ok(status) if status.success() => {
                self.notify(fl!("tui-container-shell-closed", name = shell.name), false)
            }
            Ok(status) => self.notify(
                fl!(
                    "tui-container-shell-failed",
                    name = shell.name,
                    status = status.to_string()
                ),
                true,
            ),
            Err(err) => self.notify(error_chain(&err), true),
        }
    }

    /// The last save message of an edit-only session.
    pub fn take_exit_message(&mut self) -> Option<String> {
        self.exit_message.take()
    }

    pub fn take_external_edit(&mut self) -> Option<ExternalEdit> {
        self.external.take()
    }

    fn on_popup_key(&mut self, popup: Popup, key: KeyEvent) {
        match popup {
            Popup::Help => {}
            Popup::Message {
                copy: Some(text), ..
            } if key.code == KeyCode::Char('y') => {
                self.copy(text, fl!("tui-copied-snippet"));
            }
            Popup::Message { .. } => {
                if !matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    self.popup = Some(popup);
                }
            }
            Popup::EditorHelp | Popup::CodeHelp => {}
            Popup::Input(mut input) => match input.on_key(key) {
                InputOutcome::Editing => {
                    self.connections_filter_typed(&input);
                    self.popup = Some(Popup::Input(input))
                }
                InputOutcome::Cancel => {
                    if let InputPurpose::ConnectionFilter(before) = &input.purpose {
                        self.connections.set_filter(before);
                    }
                }
                InputOutcome::Submit => {
                    let value = input.value.clone();
                    let result = match &input.purpose {
                        purpose if purpose.for_containers() => {
                            self.containers_submit_input(purpose.clone(), value)
                        }
                        InputPurpose::AddSource | InputPurpose::ImportCore => {
                            self.core_submit_input(input.purpose.clone(), value);
                            Ok(())
                        }
                        InputPurpose::ConnectionFilter(_) => {
                            self.connections.set_filter(&value);
                            Ok(())
                        }
                        purpose => self.profiles_submit_input(purpose.clone(), value),
                    };
                    if let Err(err) = result {
                        input.error = Some(err);
                        self.popup = Some(Popup::Input(input));
                    }
                }
            },
            Popup::Setup { sub_store } => {
                let answer = match key.code {
                    KeyCode::Char('y' | 'Y') => true,
                    KeyCode::Char('n' | 'N') => false,
                    // Skip for now; the question comes back on the next launch.
                    KeyCode::Esc => return,
                    _ => {
                        self.popup = Some(popup);
                        return;
                    }
                };
                match sub_store {
                    None => {
                        self.popup = Some(Popup::Setup {
                            sub_store: Some(answer),
                        })
                    }
                    Some(sub_store) => self.daemon_action(
                        &fl!("busy-setup"),
                        Request::Setup {
                            sub_store,
                            http_meta: answer,
                        },
                    ),
                }
            }
            Popup::Menu(mut menu) => match menu.on_key(key) {
                MenuOutcome::Open => self.popup = Some(Popup::Menu(menu)),
                MenuOutcome::Cancel => {}
                MenuOutcome::Chosen(MenuAction::Component(component, action)) => {
                    self.component_action(component, action)
                }
                MenuOutcome::Chosen(action) if action.for_containers() => {
                    self.containers_menu_action(action)
                }
                MenuOutcome::Chosen(action) => self.profiles_menu_action(action),
            },
            Popup::Confirm { action, .. }
                if matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter) =>
            {
                self.run_pending(action)
            }
            Popup::Confirm { .. }
                if matches!(key.code, KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc) => {}
            popup @ Popup::Confirm { .. } => self.popup = Some(popup),
        }
    }

    fn on_proxies_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_proxy_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_proxy_cursor(1),
            KeyCode::PageUp => self.move_proxy_cursor(-10),
            KeyCode::PageDown => self.move_proxy_cursor(10),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => self.focus = Focus::Groups,
            KeyCode::Right | KeyCode::Char('l') => self.focus_members(),
            KeyCode::Enter => match self.focus {
                Focus::Groups => self.focus_members(),
                Focus::Members => self.select_member(),
            },
            KeyCode::Char('t') => self.test_group(),
            KeyCode::Char('T') => {
                if let Some(name) = self
                    .member_state
                    .selected()
                    .and_then(|i| self.members().get(i).cloned())
                {
                    self.test_delays(vec![name]);
                }
            }
            _ => {}
        }
    }

    fn on_logs_key(&mut self, key: KeyEvent) {
        let max = self.logs.len().saturating_sub(1);
        self.log_scroll = match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.log_scroll.saturating_add(1),
            KeyCode::Down | KeyCode::Char('j') => self.log_scroll.saturating_sub(1),
            KeyCode::PageUp => self.log_scroll.saturating_add(20),
            KeyCode::PageDown => self.log_scroll.saturating_sub(20),
            KeyCode::Home | KeyCode::Char('g') => max,
            KeyCode::End | KeyCode::Char('G') | KeyCode::Char('f') => 0,
            _ => self.log_scroll,
        }
        .min(max);
    }

    fn on_store_key(&mut self, key: KeyEvent) {
        let entries = self.sub_store.as_ref().map_or(0, |o| o.entries.len());
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => match self.store_focus {
                StoreFocus::Components => {
                    move_table(&mut self.comp_state, Component::ALL.len(), -1)
                }
                StoreFocus::Entries => move_table(&mut self.entry_state, entries, -1),
            },
            KeyCode::Down | KeyCode::Char('j') => match self.store_focus {
                StoreFocus::Components => move_table(&mut self.comp_state, Component::ALL.len(), 1),
                StoreFocus::Entries => move_table(&mut self.entry_state, entries, 1),
            },
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => {
                self.store_focus = StoreFocus::Components
            }
            KeyCode::Right | KeyCode::Char('l') if entries > 0 => {
                self.store_focus = StoreFocus::Entries
            }
            KeyCode::Enter => match self.store_focus {
                StoreFocus::Components => self.open_component_menu(),
                StoreFocus::Entries => self.show_snippet(),
            },
            KeyCode::Char('p') => self.show_snippet(),
            KeyCode::Char('y') => match self.store_focus {
                StoreFocus::Entries => {
                    if let Some(url) = self.selected_entry().map(|e| e.singbox_url.clone()) {
                        self.copy(url, fl!("tui-copied-subscription-url"));
                    }
                }
                StoreFocus::Components => {
                    let url = self
                        .component(self.selected_component())
                        .and_then(|c| c.url.clone());
                    match url {
                        Some(url) => self.copy(url, fl!("tui-copied-url")),
                        None => self.notify(fl!("tui-no-url"), true),
                    }
                }
            },
            KeyCode::Char('w') => {
                match self
                    .component(Component::SubStore)
                    .and_then(|c| c.url.clone())
                {
                    Some(url) => self.copy(url, fl!("tui-copied-web-ui")),
                    None => self.notify(fl!("tui-sub-store-not-set-up"), true),
                }
            }
            _ => {}
        }
    }

    pub fn component(&self, component: Component) -> Option<&ComponentStatus> {
        self.status
            .as_ref()?
            .components
            .iter()
            .find(|c| c.component == component)
    }

    pub fn selected_component(&self) -> Component {
        Component::ALL[self
            .comp_state
            .selected()
            .unwrap_or(0)
            .min(Component::ALL.len() - 1)]
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.sub_store
            .as_ref()?
            .entries
            .get(self.entry_state.selected()?)
    }

    fn open_component_menu(&mut self) {
        let component = self.selected_component();
        let Some(status) = self.component(component) else {
            self.notify(fl!("tui-status-unavailable"), true);
            return;
        };
        let actions: Vec<ComponentAction> = if !status.enabled {
            vec![ComponentAction::Enable]
        } else if status.pid.is_some() {
            vec![
                ComponentAction::Restart,
                ComponentAction::Stop,
                ComponentAction::Update,
                ComponentAction::Disable,
            ]
        } else {
            vec![
                ComponentAction::Start,
                ComponentAction::Update,
                ComponentAction::Disable,
            ]
        };
        let items = actions
            .into_iter()
            .map(|action| {
                let label = match action {
                    ComponentAction::Start => fl!("menu-start"),
                    ComponentAction::Stop => fl!("menu-stop"),
                    ComponentAction::Restart => fl!("menu-restart"),
                    ComponentAction::Enable => fl!("menu-enable"),
                    ComponentAction::Disable => fl!("menu-disable"),
                    ComponentAction::Update => fl!("menu-update"),
                };
                MenuItem::new(label, "", MenuAction::Component(component, action))
            })
            .collect();
        self.popup = Some(Popup::Menu(Menu::new(component.title().to_owned(), items)));
    }

    fn component_action(&mut self, component: Component, action: ComponentAction) {
        let action_key = match action {
            ComponentAction::Start => "start",
            ComponentAction::Stop => "stop",
            ComponentAction::Restart => "restart",
            ComponentAction::Enable => "enable",
            ComponentAction::Disable => "disable",
            ComponentAction::Update => "update",
        };
        self.daemon_action(
            &fl!(
                "busy-component",
                action = action_key,
                component = component.title()
            ),
            Request::Component { component, action },
        );
    }

    fn show_snippet(&mut self) {
        let Some(entry) = self.selected_entry() else {
            self.notify(fl!("tui-no-subscription-selected"), true);
            return;
        };
        let snippet = provider_snippet(&entry.name, &entry.singbox_url);
        self.popup = Some(Popup::Message {
            title: fl!("tui-snippet-title", name = entry.name.clone()),
            body: snippet.clone(),
            error: false,
            copy: Some(snippet),
        });
    }

    /// Queues `text` for the terminal clipboard (OSC 52) and confirms with `message`.
    pub(super) fn copy(&mut self, text: String, message: String) {
        self.clipboard = Some(text);
        self.notify(message, false);
    }

    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    fn move_proxy_cursor(&mut self, delta: isize) {
        match self.focus {
            Focus::Groups => {
                let len = self.groups.len();
                let before = self.group_state.selected();
                move_list(&mut self.group_state, len, delta);
                if self.group_state.selected() != before {
                    let members = self.members().len();
                    self.member_state
                        .select(if members > 0 { Some(0) } else { None });
                }
            }
            Focus::Members => {
                let len = self.members().len();
                move_table(&mut self.member_state, len, delta);
            }
        }
    }

    fn focus_members(&mut self) {
        let members = self.members();
        if members.is_empty() {
            return;
        }
        // Start on the active member so Enter is a no-op unless the user moves.
        let now = self
            .selected_group()
            .and_then(|g| self.group(g))
            .and_then(|g| g.now.clone());
        let index = now
            .and_then(|now| members.iter().position(|m| *m == now))
            .unwrap_or(0);
        self.member_state.select(Some(index));
        self.focus = Focus::Members;
    }

    // ----- actions ------------------------------------------------------

    fn confirm(&mut self, message: String, action: PendingAction) {
        self.popup = Some(Popup::Confirm { message, action });
    }

    fn run_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::Stop => self.daemon_action(&fl!("busy-stopping"), Request::Stop),
            PendingAction::Restart => self.daemon_action(&fl!("busy-restarting"), Request::Restart),
            PendingAction::Update => self.daemon_action(
                &fl!("busy-downloading"),
                Request::Update {
                    tag: None,
                    force: false,
                },
            ),
            core @ (PendingAction::CoreInstall { .. }
            | PendingAction::CoreActivate { .. }
            | PendingAction::CoreRemove { .. }
            | PendingAction::CoreSourceRemove { .. }) => self.core_run_pending(core),
            profile @ (PendingAction::ProfileUse { .. }
            | PendingAction::ProfileDelete { .. }
            | PendingAction::ProfileAdopt) => self.profiles_run_pending(profile),
            container @ (PendingAction::ContainerStop { .. }
            | PendingAction::ContainerRestart { .. }
            | PendingAction::ContainerRuntime) => self.containers_run_pending(container),
            PendingAction::CloseAllConnections => {
                self.clash_action(&fl!("busy-closing-connections"), |clash| async move {
                    clash.close_all_connections().await?;
                    Ok(fl!("tui-connections-closed"))
                })
            }
            PendingAction::CloseConnections { ids } => {
                self.clash_action(&fl!("busy-closing-connections"), |clash| async move {
                    for id in &ids {
                        clash.close_connection(id).await?;
                    }
                    Ok(fl!("tui-connections-closed-count", count = ids.len()))
                })
            }
        }
    }

    pub(super) fn begin(&mut self, label: &str) -> u64 {
        self.next_action += 1;
        self.busy.push((self.next_action, label.to_owned()));
        self.next_action
    }

    pub(super) fn daemon_action(&mut self, label: &str, request: Request) {
        let id = self.begin(label);
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .command(request)
                .await
                .map_err(|err| error_chain(&err));
            let _ = tx.send(AppEvent::ActionDone { id, result });
        });
    }

    pub(super) fn clash_action<F, Fut>(&mut self, label: &str, action: F)
    where
        F: FnOnce(ClashClient) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<String>> + Send,
    {
        let Some(clash) = self.clash.clone() else {
            self.notify(fl!("tui-clash-api-missing"), true);
            return;
        };
        let id = self.begin(label);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = action(clash).await.map_err(|err| error_chain(&err));
            let _ = tx.send(AppEvent::ActionDone { id, result });
        });
    }

    fn check_update(&mut self) {
        let id = self.begin(&fl!("busy-checking-updates"));
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.check_update().await.map_err(|err| error_chain(&err));
            let _ = tx.send(AppEvent::UpdateChecked { id, result });
        });
    }

    fn cycle_mode(&mut self) {
        let Some(configs) = &self.configs else {
            self.notify(fl!("tui-mode-list-unavailable"), true);
            return;
        };
        if configs.mode_list.len() < 2 {
            self.notify(fl!("tui-single-mode"), true);
            return;
        }
        let index = configs
            .mode_list
            .iter()
            .position(|m| m.eq_ignore_ascii_case(&configs.mode))
            .map_or(0, |i| (i + 1) % configs.mode_list.len());
        let mode = configs.mode_list[index].clone();
        self.clash_action(&fl!("busy-switching-mode"), move |clash| async move {
            clash.set_mode(&mode).await?;
            Ok(fl!("tui-mode-set", mode = mode))
        });
    }

    fn select_member(&mut self) {
        let Some(group) = self.selected_group().map(str::to_owned) else {
            return;
        };
        let Some(member) = self
            .member_state
            .selected()
            .and_then(|i| self.members().get(i).cloned())
        else {
            return;
        };
        if !self.group(&group).is_some_and(Proxy::is_selectable) {
            self.notify(fl!("tui-not-selectable", group = group), true);
            return;
        }
        self.clash_action(&fl!("busy-selecting-node"), move |clash| async move {
            clash.select(&group, &member).await?;
            Ok(fl!("tui-node-selected", group = group, node = member))
        });
    }

    fn test_group(&mut self) {
        let members = self.members();
        if members.is_empty() {
            return;
        }
        self.test_delays(members);
    }

    fn test_delays(&mut self, names: Vec<String>) {
        let Some(clash) = self.clash.clone() else {
            self.notify(fl!("tui-clash-api-missing"), true);
            return;
        };
        for name in &names {
            self.testing.insert(name.clone());
            self.delays.remove(name);
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            futures::stream::iter(names)
                .for_each_concurrent(DELAY_CONCURRENCY, |name| {
                    let clash = clash.clone();
                    let tx = tx.clone();
                    async move {
                        let result = clash
                            .delay(&name, DEFAULT_TEST_URL, DELAY_TIMEOUT_MS)
                            .await
                            .map_err(|err| error_chain(&err));
                        let _ = tx.send(AppEvent::Delay { name, result });
                    }
                })
                .await;
        });
    }
}

/// Index of the tab a digit key selects.
fn tab_number(c: char) -> Option<usize> {
    let index = c.to_digit(10)?.checked_sub(1)? as usize;
    (index < Tab::ALL.len()).then_some(index)
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, capacity: usize) {
    if queue.len() == capacity {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn step(selected: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = selected.unwrap_or(0) as isize;
    Some(current.saturating_add(delta).clamp(0, len as isize - 1) as usize)
}

fn move_list(state: &mut ListState, len: usize, delta: isize) {
    state.select(step(state.selected(), len, delta));
}

pub(super) fn move_table(state: &mut TableState, len: usize, delta: isize) {
    state.select(step(state.selected(), len, delta));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_steps_are_clamped() {
        assert_eq!(step(None, 0, 1), None);
        assert_eq!(step(None, 3, 1), Some(1));
        assert_eq!(step(Some(2), 3, 5), Some(2));
        assert_eq!(step(Some(1), 3, isize::MIN / 2), Some(0));
    }
}
