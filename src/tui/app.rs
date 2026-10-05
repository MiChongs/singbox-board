//! TUI state, input handling and actions.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::widgets::{ListState, TableState};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;

use super::tasks::{self, EventTx};
use crate::clash::{
    ClashClient, Configs, Connection, Connections, DEFAULT_TEST_URL, Proxies, Proxy,
};
use crate::client::DaemonClient;
use crate::protocol::{
    ClashApi, Component, ComponentAction, ComponentStatus, CoreState, LogEntry, Request, Status,
    UpdateInfo,
};
use crate::substore::{Entry, Overview, provider_snippet};

const MAX_LOG_LINES: usize = 5000;
const HISTORY_POINTS: usize = 300;
const TOAST_TTL: Duration = Duration::from_secs(5);
const DELAY_TIMEOUT_MS: u32 = 5000;
const DELAY_CONCURRENCY: usize = 8;

pub enum AppEvent {
    Status(Result<Status, String>),
    LogsReset,
    Log(LogEntry),
    LogsDisconnected,
    Connections(Connections),
    Proxies(Proxies),
    Configs(Configs),
    ClashError(String),
    SubStore(Result<Overview, String>),
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Proxies,
    Connections,
    Logs,
    SubStore,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Overview,
        Tab::Proxies,
        Tab::Connections,
        Tab::Logs,
        Tab::SubStore,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Proxies => "Proxies",
            Tab::Connections => "Connections",
            Tab::Logs => "Logs",
            Tab::SubStore => "Sub-Store",
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
    /// Actions for one component.
    Menu {
        component: Component,
        actions: Vec<ComponentAction>,
        selected: usize,
    },
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
    client: DaemonClient,
    tx: EventTx,
    clash_api_tx: watch::Sender<Option<ClashApi>>,
    refresh: Arc<Notify>,
    clash: Option<ClashClient>,

    pub status: Option<Status>,
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

    pub connections: Vec<Connection>,
    pub conn_state: TableState,
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
            connections: Vec::new(),
            conn_state: TableState::default(),
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
                self.set_connections(connections.connections.unwrap_or_default());
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
            AppEvent::Delay { name, result } => {
                self.testing.remove(&name);
                self.delays.insert(name, result);
            }
            AppEvent::ActionDone { id, result } => {
                self.busy.retain(|(busy, _)| *busy != id);
                self.refresh.notify_one();
                self.store_refresh.notify_one();
                match result {
                    Ok(message) => self.notify(message, false),
                    Err(err) => self.notify(err, true),
                }
            }
            AppEvent::UpdateChecked { id, result } => {
                self.busy.retain(|(busy, _)| *busy != id);
                match result {
                    Ok(info) if info.update_available => {
                        self.popup = Some(Popup::Confirm {
                            message: format!(
                                "Install sing-box {}{}?\ninstalled: {}",
                                info.latest,
                                if info.prerelease {
                                    " (pre-release)"
                                } else {
                                    ""
                                },
                                info.current.as_deref().unwrap_or("none")
                            ),
                            action: PendingAction::Update,
                        });
                    }
                    Ok(info) => {
                        self.notify(format!("sing-box {} is up to date", info.latest), false)
                    }
                    Err(err) => self.notify(err, true),
                }
            }
        }
    }

    /// Short messages go to the footer, long or multi-line ones to a popup.
    fn notify(&mut self, text: String, error: bool) {
        if text.contains('\n') || text.chars().count() > 90 {
            let title = if error { "Error" } else { "Result" };
            self.popup = Some(Popup::Message {
                title: title.to_owned(),
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

    fn set_connections(&mut self, mut connections: Vec<Connection>) {
        let selected = self
            .conn_state
            .selected()
            .and_then(|i| self.connections.get(i))
            .map(|c| c.id.clone());
        connections.sort_by(|a, b| b.start.cmp(&a.start));
        self.connections = connections;
        let index = selected
            .and_then(|id| self.connections.iter().position(|c| c.id == id))
            .or(self.conn_state.selected())
            .map(|i| i.min(self.connections.len().saturating_sub(1)));
        self.conn_state.select(if self.connections.is_empty() {
            None
        } else {
            index.or(Some(0))
        });
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
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }
        if let Some(popup) = self.popup.take() {
            self.on_popup_key(popup, key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Tab => self.tab = Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()],
            KeyCode::BackTab => {
                self.tab = Tab::ALL[(self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()]
            }
            KeyCode::Char(c @ '1'..='5') => self.tab = Tab::ALL[c as usize - '1' as usize],
            KeyCode::Char('s') => self.daemon_action("starting sing-box", Request::Start),
            KeyCode::Char('x') => self.confirm("Stop sing-box?", PendingAction::Stop),
            KeyCode::Char('r') => self.confirm("Restart sing-box?", PendingAction::Restart),
            KeyCode::Char('R') => self.daemon_action("reloading configuration", Request::Reload),
            KeyCode::Char('c') => self.daemon_action("checking configuration", Request::Check),
            KeyCode::Char('u') => self.check_update(),
            KeyCode::Char('m') => self.cycle_mode(),
            _ => match self.tab {
                Tab::Overview => {}
                Tab::Proxies => self.on_proxies_key(key),
                Tab::Connections => self.on_connections_key(key),
                Tab::Logs => self.on_logs_key(key),
                Tab::SubStore => self.on_store_key(key),
            },
        }
    }

    fn on_popup_key(&mut self, popup: Popup, key: KeyEvent) {
        match popup {
            Popup::Help => {}
            Popup::Message {
                copy: Some(text), ..
            } if key.code == KeyCode::Char('y') => {
                self.copy(text, "snippet");
            }
            Popup::Message { .. } => {
                if !matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    self.popup = Some(popup);
                }
            }
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
                        "setting up components",
                        Request::Setup {
                            sub_store,
                            http_meta: answer,
                        },
                    ),
                }
            }
            Popup::Menu {
                component,
                actions,
                selected,
            } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.popup = Some(Popup::Menu {
                        component,
                        selected: selected.saturating_sub(1),
                        actions,
                    })
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.popup = Some(Popup::Menu {
                        component,
                        selected: (selected + 1).min(actions.len().saturating_sub(1)),
                        actions,
                    })
                }
                KeyCode::Enter => {
                    if let Some(action) = actions.get(selected) {
                        self.component_action(component, *action);
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') => {}
                _ => {
                    self.popup = Some(Popup::Menu {
                        component,
                        actions,
                        selected,
                    })
                }
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

    fn on_connections_key(&mut self, key: KeyEvent) {
        let len = self.connections.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => move_table(&mut self.conn_state, len, -1),
            KeyCode::Down | KeyCode::Char('j') => move_table(&mut self.conn_state, len, 1),
            KeyCode::PageUp => move_table(&mut self.conn_state, len, -20),
            KeyCode::PageDown => move_table(&mut self.conn_state, len, 20),
            KeyCode::Home | KeyCode::Char('g') => {
                move_table(&mut self.conn_state, len, isize::MIN / 2)
            }
            KeyCode::End | KeyCode::Char('G') => {
                move_table(&mut self.conn_state, len, isize::MAX / 2)
            }
            KeyCode::Char('d') | KeyCode::Delete => self.close_selected_connection(),
            KeyCode::Char('D') => {
                self.confirm("Close all connections?", PendingAction::CloseAllConnections)
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
                        self.copy(url, "sing-box subscription URL");
                    }
                }
                StoreFocus::Components => {
                    let url = self
                        .component(self.selected_component())
                        .and_then(|c| c.url.clone());
                    match url {
                        Some(url) => self.copy(url, "URL"),
                        None => {
                            self.notify("no URL yet; enable the component first".to_owned(), true)
                        }
                    }
                }
            },
            KeyCode::Char('w') => {
                match self
                    .component(Component::SubStore)
                    .and_then(|c| c.url.clone())
                {
                    Some(url) => self.copy(url, "Sub-Store web UI URL"),
                    None => self.notify("Sub-Store is not set up".to_owned(), true),
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
            self.notify("daemon status is not available".to_owned(), true);
            return;
        };
        let actions = if !status.enabled {
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
        self.popup = Some(Popup::Menu {
            component,
            actions,
            selected: 0,
        });
    }

    fn component_action(&mut self, component: Component, action: ComponentAction) {
        let verb = match action {
            ComponentAction::Start => "starting",
            ComponentAction::Stop => "stopping",
            ComponentAction::Restart => "restarting",
            ComponentAction::Enable => "installing",
            ComponentAction::Disable => "disabling",
            ComponentAction::Update => "updating",
        };
        self.daemon_action(
            &format!("{verb} {}", component.title()),
            Request::Component { component, action },
        );
    }

    fn show_snippet(&mut self) {
        let Some(entry) = self.selected_entry() else {
            self.notify("no subscription selected".to_owned(), true);
            return;
        };
        let snippet = provider_snippet(&entry.name, &entry.singbox_url);
        self.popup = Some(Popup::Message {
            title: format!("sing-box provider for {} · y copy", entry.name),
            body: snippet.clone(),
            error: false,
            copy: Some(snippet),
        });
    }

    /// Queues `text` for the terminal clipboard (OSC 52).
    fn copy(&mut self, text: String, what: &str) {
        self.clipboard = Some(text);
        self.notify(format!("copied {what}"), false);
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

    fn confirm(&mut self, message: &str, action: PendingAction) {
        self.popup = Some(Popup::Confirm {
            message: message.to_owned(),
            action,
        });
    }

    fn run_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::Stop => self.daemon_action("stopping sing-box", Request::Stop),
            PendingAction::Restart => self.daemon_action("restarting sing-box", Request::Restart),
            PendingAction::Update => self.daemon_action(
                "downloading sing-box",
                Request::Update {
                    tag: None,
                    force: false,
                },
            ),
            PendingAction::CloseAllConnections => {
                self.clash_action("closing connections", |clash| async move {
                    clash.close_all_connections().await?;
                    Ok("all connections closed".to_owned())
                })
            }
        }
    }

    fn begin(&mut self, label: &str) -> u64 {
        self.next_action += 1;
        self.busy.push((self.next_action, label.to_owned()));
        self.next_action
    }

    fn daemon_action(&mut self, label: &str, request: Request) {
        let id = self.begin(label);
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .command(request)
                .await
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(AppEvent::ActionDone { id, result });
        });
    }

    fn clash_action<F, Fut>(&mut self, label: &str, action: F)
    where
        F: FnOnce(ClashClient) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<String>> + Send,
    {
        let Some(clash) = self.clash.clone() else {
            self.notify(
                "Clash API is not configured (experimental.clash_api)".to_owned(),
                true,
            );
            return;
        };
        let id = self.begin(label);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = action(clash).await.map_err(|err| format!("{err:#}"));
            let _ = tx.send(AppEvent::ActionDone { id, result });
        });
    }

    fn check_update(&mut self) {
        let id = self.begin("checking for updates");
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .check_update()
                .await
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(AppEvent::UpdateChecked { id, result });
        });
    }

    fn cycle_mode(&mut self) {
        let Some(configs) = &self.configs else {
            self.notify("mode list is not available yet".to_owned(), true);
            return;
        };
        if configs.mode_list.len() < 2 {
            self.notify("only one Clash mode is configured".to_owned(), true);
            return;
        }
        let index = configs
            .mode_list
            .iter()
            .position(|m| m.eq_ignore_ascii_case(&configs.mode))
            .map_or(0, |i| (i + 1) % configs.mode_list.len());
        let mode = configs.mode_list[index].clone();
        self.clash_action("switching mode", move |clash| async move {
            clash.set_mode(&mode).await?;
            Ok(format!("mode set to {mode}"))
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
            self.notify(format!("{group} does not accept manual selection"), true);
            return;
        }
        self.clash_action("selecting node", move |clash| async move {
            clash.select(&group, &member).await?;
            Ok(format!("{group} → {member}"))
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
            self.notify(
                "Clash API is not configured (experimental.clash_api)".to_owned(),
                true,
            );
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
                            .map_err(|err| format!("{err:#}"));
                        let _ = tx.send(AppEvent::Delay { name, result });
                    }
                })
                .await;
        });
    }

    fn close_selected_connection(&mut self) {
        let Some(connection) = self
            .conn_state
            .selected()
            .and_then(|i| self.connections.get(i))
        else {
            return;
        };
        let id = connection.id.clone();
        let target = connection.target();
        self.clash_action("closing connection", move |clash| async move {
            clash.close_connection(&id).await?;
            Ok(format!("closed {target}"))
        });
    }
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

fn move_table(state: &mut TableState, len: usize, delta: isize) {
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
