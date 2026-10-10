//! The "Containers" tab: kurumi-containerd and the containers the daemon
//! manages, with their live state and load, actions (start, stop, a shell,
//! commands, root filesystems from images, files or URLs), and the
//! configuration editor (in `toml_editor`).

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Sparkline, Table, TableState, TitlePosition, Wrap};

use super::app::{App, AppEvent, PendingAction, Popup, Tab, move_table};
use super::mouse::{Pane, Target, below};
use super::popup::{ExternalEdit, Input, InputPurpose, Menu, MenuAction, MenuItem};
use super::theme::{
    self, ACCENT, BLUE, GREEN, MARK, PEACH, RED, SKY, SUBTEXT, TEAL, TEXT, YELLOW, chip, dim,
    pane_keys, panel, pill,
};
use super::toml_editor::{self, TomlEditor};
use crate::ctl_container::{network_text, runtime_text};
use crate::i18n::fl;
use crate::protocol::{
    Checksum, Container, ContainerAction, ContainerOverview, ExecResult, ImageList, Request,
};
use crate::util::{
    error_chain, external_editor_configured, fmt_bytes, fmt_duration, join_list, now_unix, truncate,
};

/// Ticks (250 ms) between refreshes while the tab is shown.
const REFRESH_TICKS: usize = 8;
/// Load samples kept per container for the graph.
const HISTORY: usize = 120;

/// Why a container's configuration was requested.
pub enum ContainerContentPurpose {
    Edit,
    External,
}

/// What to do once the daemon stored a container.
pub enum ContainerSavePurpose {
    /// Saved from the editor; `saved` becomes its clean state.
    Editor {
        saved: String,
    },
    /// A new container: pick its root filesystem next.
    Created,
    /// Saved from `$EDITOR`; kept to edit again if the save fails.
    External(ExternalEdit),
    Plain,
}

/// What an entry of a container's action menu does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerMenu {
    Start,
    Stop,
    Restart,
    Shell,
    Exec,
    Edit,
    EditExternal,
    InstallImage,
    InstallFile,
    Autostart(bool),
    Rename,
    CopyPath,
    Remove,
}

/// Actions that concern no single container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainersGlobal {
    New,
    Register,
    Adopt,
    Check,
    Runtime,
}

#[derive(Default)]
pub struct ContainersView {
    pub requested: bool,
    pub overview: Option<ContainerOverview>,
    pub error: Option<String>,
    pub state: TableState,
    /// The configuration editor, while open.
    pub editor: Option<TomlEditor>,
    /// What the editor copied or cut last.
    pub clip: Option<String>,
    pub images: Option<ImageList>,
    images_loading: bool,
    /// A container waiting for the image list to pick its root filesystem.
    images_for: Option<String>,
    /// The last CPU time sample per container: when, init PID, milliseconds.
    samples: HashMap<String, (Instant, i32, u64)>,
    /// CPU load per container in percent of one CPU.
    pub load: HashMap<String, f64>,
    /// Load history (tenths of a percent) for the graph.
    pub history: HashMap<String, VecDeque<u64>>,
    loading: bool,
    ticks: usize,
}

impl ContainersView {
    pub fn containers(&self) -> &[Container] {
        self.overview
            .as_ref()
            .map_or(&[], |o| o.containers.as_slice())
    }

    pub fn selected(&self) -> Option<&Container> {
        self.containers().get(self.state.selected()?)
    }

    fn find(&self, id: &str) -> Option<&Container> {
        self.containers().iter().find(|c| c.id == id)
    }

    /// Records the CPU time of running containers and derives their load.
    fn sample(&mut self, overview: &ContainerOverview) {
        let now = Instant::now();
        let mut seen = Vec::new();
        for container in &overview.containers {
            let Some(live) = &container.live else {
                continue;
            };
            seen.push(container.id.clone());
            if let Some((at, pid, cpu)) = self.samples.get(&container.id)
                && *pid == live.init_pid
                && live.cpu_ms >= *cpu
            {
                let elapsed = now.duration_since(*at).as_millis().max(1) as f64;
                let load = (live.cpu_ms - cpu) as f64 * 100.0 / elapsed;
                self.load.insert(container.id.clone(), load);
                let history = self.history.entry(container.id.clone()).or_default();
                if history.len() == HISTORY {
                    history.pop_front();
                }
                history.push_back((load * 10.0) as u64);
            }
            self.samples
                .insert(container.id.clone(), (now, live.init_pid, live.cpu_ms));
        }
        self.samples.retain(|id, _| seen.contains(id));
        self.load.retain(|id, _| seen.contains(id));
        self.history.retain(|id, _| seen.contains(id));
    }
}

/// This client runs as root: everything that changes what runs in a
/// container is reserved to root by the daemon.
#[cfg(unix)]
fn root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

/// Containers need Linux; the tab is not shown on Windows.
#[cfg(windows)]
fn root() -> bool {
    false
}

impl App {
    // ----- loading ------------------------------------------------------------------

    pub(super) fn containers_tab_opened(&mut self) {
        if !self.containers.requested {
            self.containers.requested = true;
            self.containers_load();
        }
    }

    pub(super) fn containers_load(&mut self) {
        if self.containers.loading {
            return;
        }
        self.containers.loading = true;
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.containers().await.map_err(|e| error_chain(&e));
            let _ = tx.send(AppEvent::Containers(result));
        });
    }

    pub(super) fn containers_tick(&mut self) {
        if self.tab != Tab::Containers || self.containers.editor.is_some() {
            return;
        }
        self.containers.ticks += 1;
        if self.containers.ticks >= REFRESH_TICKS {
            self.containers.ticks = 0;
            self.containers_load();
        }
    }

    pub(super) fn containers_refresh_after_action(&mut self) {
        if self.containers.requested {
            self.containers.loading = false;
            self.containers_load();
        }
    }

    pub(super) fn containers_loaded(&mut self, result: Result<ContainerOverview, String>) {
        self.containers.loading = false;
        match result {
            Ok(overview) => {
                let keep = self.containers.selected().map(|c| c.id.clone());
                self.containers.sample(&overview);
                self.containers.overview = Some(overview);
                self.containers.error = None;
                let len = self.containers.containers().len();
                let index = keep
                    .and_then(|id| self.containers.containers().iter().position(|c| c.id == id))
                    .or(if len > 0 { Some(0) } else { None })
                    .map(|i| i.min(len.saturating_sub(1)));
                self.containers.state.select(index);
            }
            Err(err) => self.containers.error = Some(err),
        }
    }

    fn containers_select(&mut self, id: &str) {
        if let Some(index) = self.containers.containers().iter().position(|c| c.id == id) {
            self.containers.state.select(Some(index));
        }
    }

    fn container_content(&mut self, id: String, purpose: ContainerContentPurpose) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.container(&id).await.map_err(|e| error_chain(&e));
            let _ = tx.send(AppEvent::ContainerContent { purpose, result });
        });
    }

    pub(super) fn container_content_loaded(
        &mut self,
        purpose: ContainerContentPurpose,
        result: Result<(Container, String), String>,
    ) {
        let (container, content) = match result {
            Ok(loaded) => loaded,
            Err(err) => {
                self.notify(err, true);
                return;
            }
        };
        match purpose {
            ContainerContentPurpose::Edit => self.open_toml_editor(container, &content),
            ContainerContentPurpose::External => {
                self.external = Some(ExternalEdit {
                    id: container.id,
                    name: container.name,
                    active: container.live.is_some(),
                    text: content,
                    container: true,
                })
            }
        }
    }

    /// Sends a request answered with `container_saved`.
    pub(super) fn container_save_request(
        &mut self,
        label: &str,
        request: Request,
        purpose: ContainerSavePurpose,
    ) {
        let id = self.begin(label);
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .container_saved(request)
                .await
                .map_err(|e| error_chain(&e));
            let _ = tx.send(AppEvent::ContainerSaved {
                id,
                purpose,
                result,
            });
        });
    }

    pub(super) fn container_saved(
        &mut self,
        id: u64,
        purpose: ContainerSavePurpose,
        result: Result<(Container, String), String>,
    ) {
        self.busy.retain(|(busy, _)| *busy != id);
        self.containers.loading = false;
        self.containers_load();
        match (purpose, result) {
            (ContainerSavePurpose::Editor { saved }, result) => self.toml_saved(saved, result),
            (ContainerSavePurpose::Created, Ok((container, message))) => {
                self.notify(message, false);
                self.containers_select(&container.id);
                if let Some(overview) = &mut self.containers.overview
                    && !overview.containers.iter().any(|c| c.id == container.id)
                {
                    overview.containers.push(container.clone());
                    self.containers_select(&container.id);
                }
                self.container_pick_image(container.id);
            }
            (ContainerSavePurpose::External(edit), Err(err)) => {
                self.popup = Some(Popup::Menu(
                    Menu::new(
                        fl!("tui-save-failed-title"),
                        vec![
                            MenuItem::new(
                                fl!("tui-edit-again"),
                                "",
                                MenuAction::EditAgain(edit.clone()),
                            ),
                            MenuItem::new(
                                fl!("tui-save-anyway"),
                                fl!("tui-toml-save-anyway-detail"),
                                MenuAction::ContainerSaveAnyway {
                                    id: edit.id.clone(),
                                    content: edit.text.clone(),
                                },
                            ),
                            MenuItem::new(fl!("tui-discard-changes"), "", MenuAction::Dismiss),
                        ],
                    )
                    .body(err),
                ));
            }
            (_, Ok((_, message))) => self.notify(message, false),
            (_, Err(err)) => self.notify(err, true),
        }
    }

    /// `$EDITOR` returned with a container's configuration.
    pub(super) fn container_external_done(
        &mut self,
        edit: ExternalEdit,
        result: anyhow::Result<String>,
    ) {
        let text = match result {
            Ok(text) => text,
            Err(err) => {
                self.notify(error_chain(&err), true);
                return;
            }
        };
        if text == edit.text {
            self.notify(fl!("ctl-profile-no-changes"), false);
            return;
        }
        let edited = ExternalEdit {
            text: text.clone(),
            ..edit
        };
        let request = Request::ContainerSave {
            id: edited.id.clone(),
            content: text,
            force: false,
        };
        self.container_save_request(
            &fl!("busy-saving"),
            request,
            ContainerSavePurpose::External(edited),
        );
    }

    // ----- images ---------------------------------------------------------------------

    fn containers_load_images(&mut self, refresh: bool) {
        if self.containers.images_loading {
            return;
        }
        self.containers.images_loading = true;
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .container_images(refresh)
                .await
                .map_err(|e| error_chain(&e));
            let _ = tx.send(AppEvent::ContainerImages(result));
        });
    }

    pub(super) fn container_images_loaded(&mut self, result: Result<ImageList, String>) {
        self.containers.images_loading = false;
        match result {
            Ok(list) => {
                self.containers.images = Some(list);
                if let Some(id) = self.containers.images_for.take()
                    && self.popup.is_none()
                {
                    self.container_pick_image(id);
                }
            }
            Err(err) => {
                self.containers.images_for = None;
                self.notify(err, true);
            }
        }
    }

    /// The root filesystem picker: images of the image server, a file or a
    /// URL, or later.
    fn container_pick_image(&mut self, id: String) {
        let Some(list) = &self.containers.images else {
            self.containers.images_for = Some(id);
            self.notify(fl!("tui-containers-loading-images"), false);
            self.containers_load_images(false);
            return;
        };
        let name = self
            .containers
            .find(&id)
            .map_or_else(|| id.clone(), |c| c.name.clone());
        let mut items = vec![MenuItem::new(
            fl!("menu-container-install-file"),
            fl!("menu-container-install-file-detail"),
            MenuAction::Container(id.clone(), ContainerMenu::InstallFile),
        )];
        // Distributions people usually want first.
        let preferred = ["debian", "ubuntu", "alpine", "archlinux", "fedora"];
        let mut images = list.images.clone();
        images.sort_by_key(|image| {
            (
                preferred
                    .iter()
                    .position(|p| *p == image.distro)
                    .unwrap_or(preferred.len()),
                image.distro.clone(),
                std::cmp::Reverse(image.release.clone()),
            )
        });
        items.extend(images.into_iter().map(|image| {
            MenuItem::new(
                image.spec(),
                image.build.clone(),
                MenuAction::ContainerImage {
                    id: id.clone(),
                    image: image.spec(),
                },
            )
        }));
        items.push(MenuItem::new(
            fl!("menu-container-install-later"),
            "",
            MenuAction::Dismiss,
        ));
        self.popup = Some(Popup::Menu(
            Menu::new(fl!("tui-containers-pick-image", name = name), items).body(fl!(
                "tui-containers-pick-image-body",
                server = list.server.clone(),
                arch = list.arch.clone()
            )),
        ));
    }

    // ----- keys -------------------------------------------------------------------------

    pub(super) fn containers_on_key(&mut self, key: KeyEvent) {
        let len = self.containers.containers().len();
        let selected = self.containers.selected().map(|c| c.id.clone());
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => move_table(&mut self.containers.state, len, -1),
            KeyCode::Down | KeyCode::Char('j') => move_table(&mut self.containers.state, len, 1),
            KeyCode::PageUp => move_table(&mut self.containers.state, len, -10),
            KeyCode::PageDown => move_table(&mut self.containers.state, len, 10),
            KeyCode::Home | KeyCode::Char('g') => {
                move_table(&mut self.containers.state, len, isize::MIN / 2)
            }
            KeyCode::End | KeyCode::Char('G') => {
                move_table(&mut self.containers.state, len, isize::MAX / 2)
            }
            KeyCode::Enter => self.container_menu(),
            KeyCode::Char('n') => self.containers_global(ContainersGlobal::New),
            KeyCode::Char('C') => self.containers_global(ContainersGlobal::Check),
            KeyCode::Char('U') => self.containers_global(ContainersGlobal::Runtime),
            KeyCode::Char('A') => self.containers_global(ContainersGlobal::Adopt),
            KeyCode::Char('f') => {
                self.containers.loading = false;
                self.containers_load();
                self.containers_load_images(true);
            }
            _ => {
                let Some(id) = selected else { return };
                let running = self.containers.find(&id).is_some_and(|c| c.live.is_some());
                let autostart = self.containers.find(&id).is_some_and(|c| c.autostart);
                let action = match key.code {
                    KeyCode::Char('t' | ' ') if running => ContainerMenu::Stop,
                    KeyCode::Char('t' | ' ') => ContainerMenu::Start,
                    KeyCode::Char('o') => ContainerMenu::Shell,
                    KeyCode::Char('!' | ':') => ContainerMenu::Exec,
                    KeyCode::Char('e') => ContainerMenu::Edit,
                    KeyCode::Char('E') => ContainerMenu::EditExternal,
                    KeyCode::Char('i') => ContainerMenu::InstallImage,
                    KeyCode::Char('a') => ContainerMenu::Autostart(!autostart),
                    KeyCode::Char('y') => ContainerMenu::CopyPath,
                    KeyCode::Char('d') | KeyCode::Delete => ContainerMenu::Remove,
                    _ => return,
                };
                self.container_action(id, action);
            }
        }
    }

    fn container_menu(&mut self) {
        let Some(container) = self.containers.selected().cloned() else {
            self.containers_global_menu();
            return;
        };
        let item = |label: String, detail: &str, action: ContainerMenu| {
            MenuItem::new(
                label,
                detail,
                MenuAction::Container(container.id.clone(), action),
            )
        };
        let installed = container.spec.as_ref().is_some_and(|s| s.installed);
        let mut items = Vec::new();
        if container.running() {
            items.push(item(fl!("menu-container-shell"), "o", ContainerMenu::Shell));
            items.push(item(fl!("menu-container-exec"), "!", ContainerMenu::Exec));
            items.push(item(fl!("menu-stop"), "t", ContainerMenu::Stop));
            items.push(item(fl!("menu-restart"), "", ContainerMenu::Restart));
        } else if installed {
            items.push(item(fl!("menu-start"), "t", ContainerMenu::Start));
        }
        items.push(item(fl!("menu-container-edit"), "e", ContainerMenu::Edit));
        if external_editor_configured() {
            items.push(item(
                fl!("menu-profile-edit-external"),
                "E",
                ContainerMenu::EditExternal,
            ));
        }
        if !container.running() {
            items.push(item(
                if installed {
                    fl!("menu-container-reinstall")
                } else {
                    fl!("menu-container-install")
                },
                "i",
                ContainerMenu::InstallImage,
            ));
        }
        items.push(item(
            if container.autostart {
                fl!("menu-container-autostart-off")
            } else {
                fl!("menu-container-autostart-on")
            },
            "a",
            ContainerMenu::Autostart(!container.autostart),
        ));
        items.push(item(fl!("menu-profile-rename"), "", ContainerMenu::Rename));
        items.push(item(
            fl!("menu-container-copy-path"),
            "y",
            ContainerMenu::CopyPath,
        ));
        if !container.running() {
            items.push(item(
                fl!("menu-container-remove"),
                "d",
                ContainerMenu::Remove,
            ));
        }
        let global = |label: String, detail: &str, action: ContainersGlobal| {
            MenuItem::new(label, detail, MenuAction::ContainersGlobal(action))
        };
        items.push(global(
            fl!("menu-container-new"),
            "n",
            ContainersGlobal::New,
        ));
        self.popup = Some(Popup::Menu(Menu::new(container.name.clone(), items)));
    }

    fn containers_global_menu(&mut self) {
        let item = |label: String, detail: &str, action: ContainersGlobal| {
            MenuItem::new(label, detail, MenuAction::ContainersGlobal(action))
        };
        let items = vec![
            item(fl!("menu-container-new"), "n", ContainersGlobal::New),
            item(
                fl!("menu-container-register"),
                "",
                ContainersGlobal::Register,
            ),
            item(fl!("menu-container-adopt"), "A", ContainersGlobal::Adopt),
            item(fl!("menu-container-check"), "C", ContainersGlobal::Check),
            item(
                fl!("menu-container-runtime"),
                "U",
                ContainersGlobal::Runtime,
            ),
        ];
        self.popup = Some(Popup::Menu(Menu::new(fl!("tab-containers"), items)));
    }

    pub(super) fn containers_global(&mut self, action: ContainersGlobal) {
        match action {
            ContainersGlobal::New => {
                self.popup = Some(Popup::Input(
                    Input::new(
                        fl!("tui-container-new-title"),
                        fl!("tui-container-new-hint"),
                        InputPurpose::NewContainer,
                    )
                    .placeholder(fl!("tui-container-new-placeholder")),
                ))
            }
            ContainersGlobal::Register => {
                self.popup = Some(Popup::Input(
                    Input::new(
                        fl!("tui-container-register-title"),
                        fl!("tui-container-register-hint"),
                        InputPurpose::RegisterContainer,
                    )
                    .placeholder("/srv/kurumi/debian.toml"),
                ))
            }
            ContainersGlobal::Adopt => self.daemon_action(
                &fl!("busy-registering"),
                Request::ContainerAdopt { path: None },
            ),
            ContainersGlobal::Check => {
                self.daemon_action(&fl!("busy-checking-host"), Request::ContainerCheck)
            }
            ContainersGlobal::Runtime => {
                let runtime = self.containers.overview.as_ref().map(|o| &o.runtime);
                let message = match runtime.and_then(|r| r.version.clone()) {
                    Some(version) => fl!("tui-confirm-runtime-update", version = version),
                    None => fl!("tui-confirm-runtime-install"),
                };
                self.popup = Some(Popup::Confirm {
                    message,
                    action: PendingAction::ContainerRuntime,
                });
            }
        }
    }

    pub(super) fn container_action(&mut self, id: String, action: ContainerMenu) {
        let Some(container) = self.containers.find(&id).cloned() else {
            return;
        };
        let name = container.name.clone();
        match action {
            ContainerMenu::Start => self.daemon_action(
                &fl!(
                    "busy-container-action",
                    action = ContainerAction::Start.key(),
                    name = name
                ),
                Request::ContainerControl {
                    id,
                    action: ContainerAction::Start,
                },
            ),
            ContainerMenu::Stop => self.confirm_container(
                fl!("tui-confirm-container-stop", name = name),
                PendingAction::ContainerStop { id },
            ),
            ContainerMenu::Restart => self.confirm_container(
                fl!("tui-confirm-container-restart", name = name),
                PendingAction::ContainerRestart { id },
            ),
            ContainerMenu::Shell => self.container_shell(&container, "root"),
            ContainerMenu::Exec => {
                if !container.running() {
                    self.notify(fl!("containers-not-running", name = name), true);
                    return;
                }
                self.popup = Some(Popup::Input(
                    Input::new(
                        fl!("tui-container-exec-title", name = name),
                        fl!("tui-container-exec-hint"),
                        InputPurpose::ContainerExec(id),
                    )
                    .placeholder("cat /etc/os-release"),
                ));
            }
            ContainerMenu::Edit => self.container_content(id, ContainerContentPurpose::Edit),
            ContainerMenu::EditExternal => {
                if external_editor_configured() {
                    self.container_content(id, ContainerContentPurpose::External);
                } else {
                    self.notify(fl!("tui-no-external-editor"), false);
                }
            }
            ContainerMenu::InstallImage => {
                if container.running() {
                    self.notify(fl!("containers-install-running", name = name), true);
                } else {
                    self.container_pick_image(id);
                }
            }
            ContainerMenu::InstallFile => {
                self.popup = Some(Popup::Input(
                    Input::new(
                        fl!("tui-container-install-title", name = name),
                        fl!("tui-container-install-hint"),
                        InputPurpose::ContainerInstall(id),
                    )
                    .placeholder("/srv/rootfs.tar.xz  |  https://…/rootfs.tar.gz"),
                ));
            }
            ContainerMenu::Autostart(on) => self.daemon_action(
                &fl!("busy-saving"),
                Request::ContainerSet {
                    id,
                    name: None,
                    autostart: Some(on),
                },
            ),
            ContainerMenu::Rename => {
                self.popup = Some(Popup::Input(
                    Input::new(
                        fl!("tui-rename-profile-title"),
                        String::new(),
                        InputPurpose::RenameContainer(id),
                    )
                    .value(name),
                ));
            }
            ContainerMenu::CopyPath => {
                let path = container
                    .spec
                    .as_ref()
                    .map_or(container.file.clone(), |s| s.rootfs.clone());
                self.copy(path.clone(), fl!("tui-copied-path", path = path));
            }
            ContainerMenu::Remove => {
                if container.running() {
                    self.notify(fl!("containers-remove-running", name = name), true);
                    return;
                }
                let mut items = Vec::new();
                if container.managed {
                    items.push(MenuItem::new(
                        fl!("menu-container-remove-purge"),
                        fl!("menu-container-remove-purge-detail"),
                        MenuAction::ContainerRemove {
                            id: id.clone(),
                            purge: true,
                        },
                    ));
                    items.push(MenuItem::new(
                        fl!("menu-container-remove-keep"),
                        fl!("menu-container-remove-keep-detail"),
                        MenuAction::ContainerRemove {
                            id: id.clone(),
                            purge: false,
                        },
                    ));
                } else {
                    items.push(MenuItem::new(
                        fl!("menu-container-remove-linked"),
                        container.file.clone(),
                        MenuAction::ContainerRemove {
                            id: id.clone(),
                            purge: false,
                        },
                    ));
                }
                items.push(MenuItem::new(fl!("key-cancel"), "", MenuAction::Dismiss));
                self.popup = Some(Popup::Menu(Menu::new(
                    fl!("tui-container-remove-title", name = name),
                    items,
                )));
            }
        }
    }

    fn confirm_container(&mut self, message: String, action: PendingAction) {
        self.popup = Some(Popup::Confirm { message, action });
    }

    pub(super) fn containers_run_pending(&mut self, action: PendingAction) {
        let name = |app: &App, id: &str| {
            app.containers
                .find(id)
                .map_or_else(|| id.to_owned(), |c| c.name.clone())
        };
        match action {
            PendingAction::ContainerStop { id } => {
                let name = name(self, &id);
                self.daemon_action(
                    &fl!(
                        "busy-container-action",
                        action = ContainerAction::Stop.key(),
                        name = name
                    ),
                    Request::ContainerControl {
                        id,
                        action: ContainerAction::Stop,
                    },
                )
            }
            PendingAction::ContainerRestart { id } => {
                let name = name(self, &id);
                self.daemon_action(
                    &fl!(
                        "busy-container-action",
                        action = ContainerAction::Restart.key(),
                        name = name
                    ),
                    Request::ContainerControl {
                        id,
                        action: ContainerAction::Restart,
                    },
                )
            }
            PendingAction::ContainerRuntime => self.daemon_action(
                &fl!("busy-downloading-runtime"),
                Request::ContainerRuntimeUpdate {
                    tag: None,
                    force: false,
                },
            ),
            _ => {}
        }
    }

    pub(super) fn containers_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::Container(id, action) => self.container_action(id, action),
            MenuAction::ContainersGlobal(action) => self.containers_global(action),
            MenuAction::ContainerNetwork { name, network } => self.container_save_request(
                &fl!("busy-creating-container"),
                Request::ContainerAdd {
                    name: Some(name),
                    content: None,
                    file: None,
                    network: Some(network),
                },
                ContainerSavePurpose::Created,
            ),
            MenuAction::ContainerImage { id, image } => {
                let name = self
                    .containers
                    .find(&id)
                    .map_or_else(|| id.clone(), |c| c.name.clone());
                let reinstall = self
                    .containers
                    .find(&id)
                    .and_then(|c| c.spec.as_ref())
                    .is_some_and(|s| s.installed);
                self.daemon_action(
                    &fl!("busy-installing-rootfs", name = name),
                    Request::ContainerInstall {
                        id,
                        source: image,
                        size: None,
                        sha256: None,
                        force: reinstall,
                    },
                );
            }
            MenuAction::ContainerRemove { id, purge } => {
                let name = self
                    .containers
                    .find(&id)
                    .map_or_else(|| id.clone(), |c| c.name.clone());
                self.daemon_action(
                    &fl!("busy-container-action", action = "remove", name = name),
                    Request::ContainerRemove { id, purge },
                );
            }
            MenuAction::ContainerSaveAnyway { id, content } => {
                let from_editor = self
                    .containers
                    .editor
                    .as_ref()
                    .is_some_and(|e| e.container.id == id);
                let purpose = if from_editor {
                    if let Some(editor) = &mut self.containers.editor {
                        editor.saving = true;
                    }
                    ContainerSavePurpose::Editor {
                        saved: content.clone(),
                    }
                } else {
                    ContainerSavePurpose::Plain
                };
                self.container_save_request(
                    &fl!("busy-saving"),
                    Request::ContainerSave {
                        id,
                        content,
                        force: true,
                    },
                    purpose,
                );
            }
            MenuAction::Toml(command) => self.toml_command(command),
            _ => {}
        }
    }

    pub(super) fn containers_submit_input(
        &mut self,
        purpose: InputPurpose,
        value: String,
    ) -> Result<(), String> {
        let trimmed = value.trim().to_owned();
        match purpose {
            InputPurpose::NewContainer => {
                if trimmed.is_empty() {
                    return Err(fl!("containers-empty-name"));
                }
                let item = |label: String, detail: String, network: &str| {
                    MenuItem::new(
                        label,
                        detail,
                        MenuAction::ContainerNetwork {
                            name: trimmed.clone(),
                            network: network.to_owned(),
                        },
                    )
                };
                self.popup = Some(Popup::Menu(Menu::new(
                    fl!("tui-container-network-title", name = trimmed.clone()),
                    vec![
                        item(
                            fl!("menu-network-host"),
                            fl!("menu-network-host-detail"),
                            "host",
                        ),
                        item(
                            fl!("menu-network-nat"),
                            fl!("menu-network-nat-detail"),
                            "nat",
                        ),
                        item(
                            fl!("menu-network-none"),
                            fl!("menu-network-none-detail"),
                            "none",
                        ),
                    ],
                )));
            }
            InputPurpose::RegisterContainer => {
                if !trimmed.starts_with('/') {
                    return Err(fl!("containers-add-relative"));
                }
                self.container_save_request(
                    &fl!("busy-registering"),
                    Request::ContainerAdd {
                        name: None,
                        content: None,
                        file: Some(trimmed),
                        network: None,
                    },
                    ContainerSavePurpose::Plain,
                );
            }
            InputPurpose::RenameContainer(id) => {
                if trimmed.is_empty() {
                    return Err(fl!("containers-empty-name"));
                }
                self.daemon_action(
                    &fl!("busy-saving"),
                    Request::ContainerSet {
                        id,
                        name: Some(trimmed),
                        autostart: None,
                    },
                );
            }
            InputPurpose::ContainerInstall(id) => {
                let mut parts = trimmed.split_whitespace();
                let Some(source) = parts.next().map(str::to_owned) else {
                    return Ok(());
                };
                let size = parts.next().map(str::to_owned);
                let source = if source.starts_with("~/") {
                    std::env::var("HOME")
                        .map(|home| format!("{home}/{}", &source[2..]))
                        .unwrap_or(source)
                } else {
                    source
                };
                let local = std::path::Path::new(&source);
                let source = match std::fs::canonicalize(local) {
                    Ok(path) if local.is_file() => path.display().to_string(),
                    _ => source,
                };
                let container = self.containers.find(&id).cloned();
                let needs_size = container
                    .as_ref()
                    .and_then(|c| c.spec.as_ref())
                    .is_some_and(|s| s.image);
                if needs_size && size.is_none() {
                    return Err(fl!("tui-container-install-size"));
                }
                let name = container
                    .as_ref()
                    .map_or_else(|| id.clone(), |c| c.name.clone());
                let reinstall = container
                    .as_ref()
                    .and_then(|c| c.spec.as_ref())
                    .is_some_and(|s| s.installed);
                self.daemon_action(
                    &fl!("busy-installing-rootfs", name = name),
                    Request::ContainerInstall {
                        id,
                        source,
                        size,
                        sha256: None,
                        force: reinstall,
                    },
                );
            }
            InputPurpose::ContainerExec(id) => {
                let command =
                    split_command(&trimmed).map_err(|_| fl!("tui-container-exec-quotes"))?;
                if command.is_empty() {
                    return Ok(());
                }
                self.container_exec(id, command);
            }
            _ => {}
        }
        Ok(())
    }

    fn container_exec(&mut self, id: String, command: Vec<String>) {
        let name = self
            .containers
            .find(&id)
            .map_or_else(|| id.clone(), |c| c.name.clone());
        let shown = command.join(" ");
        let action = self.begin(&fl!("busy-running-command", name = name.clone()));
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .container_exec(&id, command, None)
                .await
                .map_err(|e| error_chain(&e));
            let _ = tx.send(AppEvent::ContainerExec {
                id: action,
                name,
                command: shown,
                result,
            });
        });
    }

    pub(super) fn container_exec_done(
        &mut self,
        id: u64,
        name: String,
        command: String,
        result: Result<ExecResult, String>,
    ) {
        self.busy.retain(|(busy, _)| *busy != id);
        match result {
            Ok(result) => {
                let mut body = result.stdout.trim_end().to_owned();
                let stderr = result.stderr.trim_end();
                if !stderr.is_empty() {
                    if !body.is_empty() {
                        body.push_str("\n\n");
                    }
                    body.push_str(stderr);
                }
                if result.truncated {
                    body.push_str("\n\n");
                    body.push_str(&fl!("ctl-container-output-truncated"));
                }
                if body.is_empty() {
                    body = fl!("tui-container-exec-no-output");
                }
                self.popup = Some(Popup::Message {
                    title: fl!(
                        "tui-container-exec-result",
                        name = name,
                        command = truncate(&command, 40),
                        code = result.code
                    ),
                    body: body.clone(),
                    error: result.code != 0,
                    copy: Some(body),
                });
            }
            Err(err) => self.notify(err, true),
        }
    }

    /// Hands the terminal to an interactive login in the container: the
    /// runtime directly as root, otherwise `sudo singbox-board container enter`.
    fn container_shell(&mut self, container: &Container, user: &str) {
        if !container.running() {
            self.notify(
                fl!("containers-not-running", name = container.name.clone()),
                true,
            );
            return;
        }
        let Some(overview) = &self.containers.overview else {
            return;
        };
        let command = if root() {
            match crate::ctl_container::enter_command(overview, container) {
                Ok(enter) => ShellCommand {
                    program: enter.binary,
                    args: vec![
                        "--name".to_owned(),
                        enter.id,
                        "enter".to_owned(),
                        user.to_owned(),
                    ],
                    env: vec![("HOME".to_owned(), enter.home)],
                    name: container.name.clone(),
                },
                Err(err) => {
                    self.notify(error_chain(&err), true);
                    return;
                }
            }
        } else {
            let Some(sudo) = crate::util::find_program("sudo") else {
                self.notify(
                    fl!(
                        "ctl-container-enter-needs-root",
                        command = format!("sudo singbox-board container enter {}", container.id)
                    ),
                    true,
                );
                return;
            };
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "singbox-board".to_owned());
            ShellCommand {
                program: sudo.display().to_string(),
                args: vec![
                    "--".to_owned(),
                    exe,
                    "--socket".to_owned(),
                    self.socket(),
                    "--lang".to_owned(),
                    crate::i18n::current().tag().to_owned(),
                    "container".to_owned(),
                    "enter".to_owned(),
                    container.id.clone(),
                    user.to_owned(),
                ],
                env: Vec::new(),
                name: container.name.clone(),
            }
        };
        self.shell = Some(command);
    }
}

/// An interactive program the event loop runs in the terminal.
#[derive(Debug, Clone)]
#[cfg_attr(windows, allow(dead_code))]
pub struct ShellCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The container, for messages.
    pub name: String,
}

/// Splits a command line into words like a shell: whitespace separates
/// words, single quotes keep everything, double quotes and backslashes
/// escape. Fails on an unterminated quote.
pub fn split_command(line: &str) -> Result<Vec<String>, ()> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err(()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err(()),
                        },
                        Some(c) => word.push(c),
                        None => return Err(()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(c) = chars.next() {
                    word.push(c);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

// ----- rendering --------------------------------------------------------------------------

fn state_span(container: &Container) -> Span<'static> {
    // Download progress is the one long busy text; an arrow says it.
    if let Some(percent) = container
        .busy
        .as_deref()
        .and_then(|b| b.strip_prefix("downloading:"))
    {
        return pill(format!("↓ {percent}%"), YELLOW);
    }
    match container.busy_label() {
        Some(busy) => pill(busy, YELLOW),
        None => theme::state_pill(container.state),
    }
}

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.containers.editor.is_some() {
        toml_editor::draw(frame, area, app);
        return;
    }
    let [runtime_area, main] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(6)]).areas(area);
    draw_runtime(frame, runtime_area, app);
    let [list_area, details_area] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(main);
    draw_list(frame, list_area, app);
    draw_details(frame, details_area, app);
}

fn draw_runtime(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("kurumi-containerd", false);
    let lines = match (&app.containers.overview, &app.containers.error) {
        (Some(overview), _) => {
            let runtime = &overview.runtime;
            let installed = runtime.binary.is_some();
            let mut first = vec![
                if installed {
                    pill(fl!("tui-runtime-ready"), GREEN)
                } else if runtime.busy.is_some() {
                    pill(fl!("busy-installing"), YELLOW)
                } else {
                    pill(fl!("tui-runtime-missing"), PEACH)
                },
                Span::raw("  "),
                Span::styled(runtime_text(runtime), Style::new().fg(TEXT).bold()),
            ];
            if installed && runtime.checksum != Checksum::None {
                first.push(Span::raw("  "));
                first.push(chip(format!("✓ {}", runtime.checksum.label()), GREEN));
            }
            let running = overview.containers.iter().filter(|c| c.running()).count();
            first.push(Span::raw("  "));
            first.push(chip(
                fl!(
                    "ctl-containers-summary",
                    running = running,
                    total = overview.containers.len()
                ),
                SKY,
            ));
            let second = if let Some(problem) = &runtime.problem {
                Line::from(Span::styled(problem.clone(), Style::new().fg(YELLOW)))
            } else if !root() {
                Line::from(dim(fl!("tui-containers-not-root")))
            } else {
                Line::from(dim(match &runtime.binary {
                    Some(binary) => fl!(
                        "tui-runtime-details",
                        binary = binary.clone(),
                        home = overview.home.clone()
                    ),
                    None => fl!("tui-runtime-download-note"),
                }))
            };
            vec![Line::from(first), second]
        }
        (None, Some(err)) => vec![Line::from(Span::styled(
            err.lines().next().unwrap_or_default().to_owned(),
            Style::new().fg(RED),
        ))],
        (None, None) => vec![Line::from(dim(fl!("tui-loading")))],
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_list(frame: &mut Frame, area: Rect, app: &mut App) {
    app.hits.add(area, Target::Pane(Pane::Containers));
    let count = app.containers.containers().len();
    let hints = pane_keys(None, &[("⏎", fl!("key-actions"))]);
    hints.title(&app.hits, area, TitlePosition::Top, Alignment::Right);
    let block = panel(&fl!("tui-panel-containers", count = count), true)
        .title_top(hints.line().right_aligned());
    let inner = block.inner(area);
    if count == 0 {
        let text = if app.containers.overview.is_some() {
            vec![
                Line::from(Span::styled(
                    fl!("tui-containers-empty"),
                    Style::new().fg(TEXT).bold(),
                )),
                Line::raw(""),
                Line::from(dim(fl!("tui-containers-empty-hint"))),
            ]
        } else {
            vec![Line::from(dim(fl!("tui-loading")))]
        };
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: true }).block(block),
            area,
        );
        return;
    }
    let now = now_unix();
    let rows: Vec<Row> = app
        .containers
        .containers()
        .iter()
        .map(|c| {
            // The address says NAT; other modes are named.
            let network = c.spec.as_ref().map_or_else(
                || "-".to_owned(),
                |s| match &s.address {
                    Some(address) => address.split('/').next().unwrap_or(address).to_owned(),
                    None => s.network.clone(),
                },
            );
            let uptime = c
                .live
                .as_ref()
                .map(|l| fmt_duration(now.saturating_sub(l.started_at)))
                .unwrap_or_default();
            let load = app
                .containers
                .load
                .get(&c.id)
                .map(|l| format!("{l:.1}%"))
                .unwrap_or_default();
            let memory = c
                .live
                .as_ref()
                .map(|l| fmt_bytes(l.memory))
                .unwrap_or_default();
            let mut name = vec![
                Span::styled(
                    if c.running() { "● " } else { "  " },
                    Style::new().fg(GREEN),
                ),
                Span::styled(c.name.clone(), Style::new().fg(TEXT).bold()),
            ];
            if c.autostart {
                name.push(Span::styled(" ⟳", Style::new().fg(TEAL)));
            }
            if c.spec.as_ref().is_some_and(|s| !s.installed) {
                name.push(Span::styled(" ○", Style::new().fg(PEACH)));
            }
            if c.spec_error.is_some() || c.last_error.is_some() {
                name.push(Span::styled(" !", Style::new().fg(RED).bold()));
            }
            Row::new(vec![
                Cell::from(Line::from(name)),
                Cell::from(state_span(c)),
                Cell::from(Span::styled(network, Style::new().fg(SUBTEXT))),
                Cell::from(dim(uptime)),
                Cell::from(Span::styled(load, Style::new().fg(BLUE))),
                Cell::from(Span::styled(memory, Style::new().fg(SUBTEXT))),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Min(12),
            Constraint::Length(12),
            Constraint::Length(13),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(9),
        ],
    )
    .header(
        Row::new([
            fl!("col-name"),
            fl!("col-state"),
            fl!("col-network"),
            fl!("col-uptime"),
            "CPU".to_owned(),
            fl!("field-memory"),
        ])
        .style(theme::header_row()),
    )
    .block(block)
    // A background highlight would hide the state pills; bold only.
    .row_highlight_style(Style::new().add_modifier(Modifier::BOLD))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, area, &mut app.containers.state);
    app.hits.rows(
        Pane::Containers,
        area,
        below(inner, 1),
        (count, app.containers.state.offset(), 1),
    );
    theme::scrollbar(
        frame,
        area,
        (
            count,
            app.containers.state.offset(),
            usize::from(area.height.saturating_sub(3)),
        ),
        true,
    );
}

fn draw_details(frame: &mut Frame, area: Rect, app: &App) {
    let Some(container) = app.containers.selected() else {
        frame.render_widget(panel(&fl!("tui-panel-container-details"), false), area);
        return;
    };
    let block = panel(&container.name, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let history = app.containers.history.get(&container.id);
    let live = container.live.as_ref().filter(|_| container.running());
    let memory_limit = container.spec.as_ref().and_then(|s| s.memory_limit);
    // A gauge for the CPU, one for the memory when it is limited, and the
    // load over time when there is room.
    let gauge_rows = match live {
        Some(_) if inner.height > 12 => 2 + u16::from(memory_limit.is_some()),
        _ => 0,
    };

    // Rows of label and value; the labels are aligned to the longest one
    // that can appear, and paths are shortened to the width left.
    let labels = [
        fl!("ctl-label-state"),
        fl!("field-memory"),
        fl!("ctl-label-processes"),
        fl!("ctl-label-init"),
        fl!("ctl-label-network"),
        fl!("ctl-label-ports"),
        fl!("ctl-label-rootfs"),
        fl!("ctl-label-mount"),
        fl!("ctl-label-limits"),
        fl!("ctl-label-identity"),
        fl!("ctl-label-autostart"),
        fl!("ctl-label-config"),
        fl!("ctl-label-config-error"),
        fl!("ctl-label-last-error"),
    ];
    let width = labels
        .iter()
        .map(|label| crate::util::text_width(label))
        .max()
        .unwrap_or(0)
        + 2;
    let room = usize::from(inner.width).saturating_sub(width + 2).max(8);
    let path = |text: &str| crate::util::truncate_start(text, room);
    let mut rows: Vec<(String, Vec<Span<'static>>)> = Vec::new();
    let mut row = |label: String, value: Vec<Span<'static>>| rows.push((label, value));
    let now = now_unix();
    let mut state = vec![state_span(container)];
    if let Some(live) = &container.live {
        state.push(Span::raw("  "));
        state.push(dim(fl!(
            "tui-container-state-detail",
            pid = live.init_pid.to_string(),
            uptime = fmt_duration(now.saturating_sub(live.started_at))
        )));
    }
    row(fl!("ctl-label-state"), state);
    if let Some(live) = &container.live {
        let load = app
            .containers
            .load
            .get(&container.id)
            .map_or_else(|| "-".to_owned(), |l| format!("{l:.1}%"));
        // The gauge below shows it when there is room.
        if gauge_rows == 0 {
            row(
                "CPU".to_owned(),
                vec![Span::styled(load, Style::new().fg(BLUE).bold())],
            );
        }
        let memory = match container.spec.as_ref().and_then(|s| s.memory_limit) {
            Some(limit) => format!("{} / {}", fmt_bytes(live.memory), fmt_bytes(limit)),
            None => fmt_bytes(live.memory),
        };
        row(fl!("field-memory"), vec![Span::raw(memory)]);
        let processes = match container.spec.as_ref().and_then(|s| s.pids_limit) {
            Some(limit) => format!("{} / {limit}", live.processes),
            None => live.processes.to_string(),
        };
        row(fl!("ctl-label-processes"), vec![Span::raw(processes)]);
        let mut init = format!(
            "{} ({})",
            container.spec.as_ref().map_or("?", |s| s.init.as_str()),
            live.init_system
        );
        if live.generation > 0 {
            init.push_str(", ");
            init.push_str(&fl!("tui-container-reboots", count = live.generation));
        }
        row(fl!("ctl-label-init"), vec![Span::raw(init)]);
    }
    if let Some(spec) = &container.spec {
        row(
            fl!("ctl-label-network"),
            vec![Span::raw(network_text(spec))],
        );
        if !spec.ports.is_empty() {
            let ports: Vec<String> = spec
                .ports
                .iter()
                .map(|p| format!("{} → {}/{}", p.host, p.container, p.protocol))
                .collect();
            row(fl!("ctl-label-ports"), vec![Span::raw(join_list(&ports))]);
        }
        let mut rootfs = vec![
            if spec.installed {
                Span::styled("✓ ", Style::new().fg(GREEN))
            } else {
                Span::styled("○ ", Style::new().fg(PEACH))
            },
            Span::styled(path(&spec.rootfs), Style::new().fg(SUBTEXT)),
        ];
        if spec.image {
            rootfs.push(dim(format!("  {}", fl!("ctl-container-image-file"))));
        }
        row(fl!("ctl-label-rootfs"), rootfs);
        if !spec.installed {
            row(
                String::new(),
                vec![Span::styled(
                    fl!("tui-container-install-hint-short"),
                    Style::new().fg(PEACH),
                )],
            );
        }
        if !spec.mounts.is_empty() {
            let mounts: Vec<String> = spec.mounts.iter().map(|m| m.target.clone()).collect();
            row(fl!("ctl-label-mount"), vec![Span::raw(join_list(&mounts))]);
        }
        let mut limits = Vec::new();
        if let Some(memory) = spec.memory_limit {
            limits.push(fl!(
                "ctl-container-limit-memory",
                memory = fmt_bytes(memory)
            ));
        }
        if let Some(cpu) = spec.cpu_limit {
            limits.push(fl!(
                "ctl-container-limit-cpu",
                cpus = format!("{:.2}", cpu as f64 / 1000.0)
            ));
        }
        if let Some(pids) = spec.pids_limit {
            limits.push(fl!("ctl-container-limit-pids", pids = pids));
        }
        if !limits.is_empty() {
            row(fl!("ctl-label-limits"), vec![Span::raw(join_list(&limits))]);
        }
        row(
            fl!("ctl-label-identity"),
            vec![
                Span::styled(spec.name.clone(), Style::new().fg(TEXT)),
                dim(format!(
                    "  {}",
                    fl!("tui-container-hostname", hostname = spec.hostname.clone())
                )),
            ],
        );
    }
    row(
        fl!("ctl-label-autostart"),
        vec![if container.autostart {
            Span::styled(fl!("answer-yes"), Style::new().fg(TEAL))
        } else {
            dim(fl!("answer-no"))
        }],
    );
    row(
        fl!("ctl-label-config"),
        vec![Span::styled(
            if container.managed {
                path(&container.file)
            } else {
                fl!("ctl-container-linked", file = path(&container.file))
            },
            Style::new().fg(SUBTEXT),
        )],
    );
    for (label, problem) in [
        (fl!("ctl-label-config-error"), &container.spec_error),
        (fl!("ctl-label-last-error"), &container.last_error),
    ] {
        if let Some(problem) = problem {
            row(
                label,
                vec![Span::styled(problem.clone(), Style::new().fg(RED))],
            );
        }
    }
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|(label, value)| Line::from([vec![theme::label(&label, width)], value].concat()))
        .collect();
    // The gauges follow the text, and the load graph takes what is left.
    let text_rows = lines
        .iter()
        .map(|line| {
            line.width()
                .max(1)
                .div_ceil(usize::from(inner.width).max(1))
        })
        .sum::<usize>() as u16;
    let graph_rows = match inner.height.saturating_sub(text_rows + gauge_rows) {
        rows if gauge_rows > 0 && rows >= 5 => rows,
        _ => 0,
    };
    let [text_area, gauge_area, graph_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(gauge_rows),
        Constraint::Length(graph_rows),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text_area);

    if let Some(live) = live.filter(|_| gauge_rows > 0) {
        let rows = gauge_area.rows().skip(1);
        let load = app
            .containers
            .load
            .get(&container.id)
            .copied()
            .unwrap_or(0.0);
        // Percent of the CPUs the container may use, or of one.
        let capacity = container
            .spec
            .as_ref()
            .and_then(|s| s.cpu_limit)
            .map_or(100.0, |cpu| cpu as f64 / 10.0);
        let cpu = load / capacity;
        let mut label = vec![
            theme::label("CPU", width),
            Span::styled(format!("{load:>6.1}%"), Style::new().fg(BLUE).bold()),
        ];
        if capacity != 100.0 {
            label.push(dim(format!(" / {capacity:.0}%")));
        }
        let mut gauges = vec![theme::meter(
            cpu,
            theme::usage_color(cpu),
            Line::from(label),
        )];
        if let Some(limit) = memory_limit {
            let used = live.memory as f64 / limit.max(1) as f64;
            gauges.push(theme::meter(
                used,
                theme::usage_color(used),
                Line::from(vec![
                    theme::label(&fl!("field-memory"), width),
                    Span::styled(
                        format!("{:>6.1}%", used * 100.0),
                        Style::new().fg(TEAL).bold(),
                    ),
                    dim(format!(" / {}", fmt_bytes(limit))),
                ]),
            ));
        }
        for (gauge, row) in gauges.into_iter().zip(rows) {
            frame.render_widget(gauge, row);
        }
    }

    if graph_rows > 0 {
        let data: Vec<u64> = history
            .map(|h| h.iter().copied().collect())
            .unwrap_or_default();
        let block = theme::card(fl!("tui-container-load-graph"), BLUE);
        let spark = block.inner(graph_area);
        let width = usize::from(spark.width);
        let shown = &data[data.len().saturating_sub(width)..];
        frame.render_widget(
            Sparkline::default()
                .block(block)
                .data(shown)
                .max(1000)
                .style(Style::new().fg(BLUE)),
            graph_area,
        );
    }
}

/// Footer hints for the Containers tab.
pub fn hints(app: &App) -> Vec<(&'static str, String)> {
    if app.containers.editor.is_some() {
        return toml_editor::hints();
    }
    vec![
        ("⏎", fl!("key-actions")),
        ("n", fl!("key-new")),
        ("t", fl!("key-start-stop")),
        ("o", fl!("key-shell")),
        ("!", fl!("key-run")),
        ("e", fl!("key-edit")),
        ("i", fl!("key-install")),
        ("d", fl!("key-delete")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_split_like_a_shell() {
        assert_eq!(
            split_command("sh -c 'id && mount' \"two words\" a\\ b").unwrap(),
            ["sh", "-c", "id && mount", "two words", "a b"]
        );
        assert_eq!(split_command("  ").unwrap(), Vec::<String>::new());
        assert_eq!(
            split_command("echo \"a\\\"b\" ''").unwrap(),
            ["echo", "a\"b", ""]
        );
        assert!(split_command("echo 'open").is_err());
    }
}

#[cfg(test)]
mod render_tests {
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::client::DaemonClient;
    use crate::protocol::{
        ContainerLive, ContainerRuntime, ContainerSpec, CoreState, Image, PortForward,
        RuntimeOrigin,
    };

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| super::super::ui::draw(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn container(id: &str, name: &str, running: bool, installed: bool) -> Container {
        Container {
            id: id.into(),
            name: name.into(),
            file: format!("/var/lib/singbox-board/containers/{id}/container.toml"),
            managed: true,
            autostart: running,
            created_at: 1,
            updated_at: 1,
            state: if running {
                CoreState::Running
            } else {
                CoreState::Stopped
            },
            busy: None,
            last_error: None,
            spec: Some(ContainerSpec {
                name: name.to_ascii_lowercase(),
                hostname: name.to_ascii_lowercase(),
                uuid: None,
                rootfs: format!("/var/lib/singbox-board/containers/{id}/rootfs"),
                image: false,
                installed,
                init: "/sbin/init".into(),
                network: "nat".into(),
                address: Some("172.28.0.2/16".into()),
                bridge: Some("kurumi-br0".into()),
                ports: vec![PortForward {
                    host: 8080,
                    container: 80,
                    protocol: "tcp".into(),
                }],
                mounts: Vec::new(),
                memory_limit: Some(1 << 30),
                cpu_limit: None,
                pids_limit: None,
                foreground: false,
                volatile: false,
                user_namespaces: false,
                stop_timeout: 15,
            }),
            spec_error: None,
            live: running.then(|| ContainerLive {
                init_pid: 4242,
                monitor_pid: 4241,
                init_system: "systemd".into(),
                started_at: now_unix() - 3725,
                generation: 0,
                processes: 23,
                memory: 180 << 20,
                cpu_ms: 1000,
                rebooting: false,
            }),
        }
    }

    fn overview(containers: Vec<Container>) -> ContainerOverview {
        ContainerOverview {
            runtime: ContainerRuntime {
                binary: Some("/var/lib/singbox-board/containers/runtime/kurumi-containerd".into()),
                version: Some("0.2.3".into()),
                origin: Some(RuntimeOrigin::Release),
                tag: Some("v0.2.3".into()),
                checksum: Checksum::Sums,
                busy: None,
                target: Some("x86_64-unknown-linux-musl".into()),
                problem: None,
            },
            containers,
            home: "/var/lib/singbox-board/containers".into(),
        }
    }

    #[tokio::test]
    async fn tab_menus_picker_and_editor_render() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut app, _tasks) = App::new(DaemonClient::new("/nonexistent/socket".into()), tx);
        app.tab = Tab::Containers;
        app.containers.requested = true;
        let empty = screen(&mut app, 120, 30);
        assert!(empty.contains(&fl!("tui-loading")), "{empty}");
        app.on_event(AppEvent::Containers(Ok(overview(Vec::new()))));
        let empty = screen(&mut app, 120, 30);
        assert!(empty.contains(&fl!("tui-containers-empty")), "{empty}");

        let mut dev = container("ab12cd34", "Dev", true, true);
        app.on_event(AppEvent::Containers(Ok(overview(vec![
            dev.clone(),
            container("ef56gh78", "Web", false, false),
        ]))));
        // A second sample derives the CPU load.
        dev.live.as_mut().unwrap().cpu_ms = 3000;
        app.containers.samples.get_mut("ab12cd34").unwrap().0 -= std::time::Duration::from_secs(2);
        app.on_event(AppEvent::Containers(Ok(overview(vec![
            dev.clone(),
            container("ef56gh78", "Web", false, false),
        ]))));
        let shown = screen(&mut app, 130, 34);
        println!("{shown}");
        assert!(shown.contains("Dev") && shown.contains("Web"), "{shown}");
        assert!(
            shown.contains("0.2.3") && shown.contains("SHA256SUMS"),
            "{shown}"
        );
        assert!(shown.contains("172.28.0.2/16"), "{shown}");
        assert!(shown.contains("8080 → 80/tcp"), "{shown}");
        assert!(shown.contains("180.0 MiB / 1.0 GiB"), "{shown}");
        let load = app.containers.load["ab12cd34"];
        assert!((90.0..110.0).contains(&load), "{load}");

        app.on_key(KeyEvent::from(KeyCode::Enter));
        let Some(Popup::Menu(menu)) = &app.popup else {
            panic!("no menu");
        };
        assert_eq!(menu.items[0].label, fl!("menu-container-shell"));
        println!("{}", screen(&mut app, 80, 24));
        app.popup = None;

        // The image picker offers files and the server's images.
        app.containers.images = Some(ImageList {
            server: "https://images.linuxcontainers.org".into(),
            arch: "amd64".into(),
            images: vec![
                Image {
                    distro: "alpine".into(),
                    release: "3.22".into(),
                    build: "20261006_13:00".into(),
                },
                Image {
                    distro: "debian".into(),
                    release: "trixie".into(),
                    build: "20261006_05:24".into(),
                },
            ],
        });
        app.on_key(KeyEvent::from(KeyCode::Down));
        app.on_key(KeyEvent::from(KeyCode::Char('i')));
        let Some(Popup::Menu(menu)) = &app.popup else {
            panic!("no picker");
        };
        let labels: Vec<&str> = menu.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels[..3],
            [
                fl!("menu-container-install-file").as_str(),
                "debian/trixie",
                "alpine/3.22"
            ]
        );
        println!("{}", screen(&mut app, 90, 24));
        app.popup = None;

        // The editor: typing, validation, comments, closing with changes.
        app.open_toml_editor(
            dev.clone(),
            "[runtime]\nstop_timeout_seconds = 15\n\n[container]\nname = \"dev\"\nrootfs = \"./rootfs\" # here\n",
        );
        let editor = screen(&mut app, 100, 20);
        println!("{editor}");
        assert!(editor.contains("✓ TOML"), "{editor}");
        app.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        for c in "network = \"bridge\"".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let invalid = screen(&mut app, 100, 20);
        assert!(invalid.contains("bridge"), "{invalid}");
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL));
        assert!(
            app.containers
                .editor
                .as_ref()
                .unwrap()
                .text
                .text()
                .ends_with("# network = \"bridge\"")
        );
        app.on_key(KeyEvent::from(KeyCode::Esc));
        let Some(Popup::Menu(menu)) = &app.popup else {
            panic!("no unsaved-changes menu");
        };
        assert_eq!(menu.items[1].label, fl!("tui-discard-changes"));
        app.on_key(KeyEvent::from(KeyCode::Down));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.containers.editor.is_none());
    }
}
