//! The "Profiles" tab: the configuration store with an overview of the
//! selected profile, actions to import, create, switch and update profiles,
//! and the tree editor.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap};
use serde_json::Value;

use super::app::{App, AppEvent, PendingAction, Popup, Tab};
use super::editor::{
    Editor, InsertTarget, Path, Seg, display_path, get, index_of, is_container, parse_json,
    parse_scalar,
};
use super::popup::{
    ExternalEdit, ExternalTarget, Input, InputPurpose, Menu, MenuAction, MenuItem, ProfileAction,
};
use super::templates;
use super::theme::{
    self, ACCENT, BLUE, DIM, GREEN, MARK, PEACH, RED, SKY, SUBTEXT, TEXT, YELLOW, chip, dim, field,
    panel, selected,
};
use crate::i18n::fl;
use crate::profile::{self, Summary, interval_label, item_label, local_time, usage_label};
use crate::protocol::{Profile, ProfileList, Request};
use crate::util::{error_chain, fmt_bytes, join_list, now_unix, text_width};

/// Seconds between list refreshes while the tab is open.
const REFRESH_TICKS: usize = 40;
/// Longest JSON typed inline; bigger nodes go to `$EDITOR`.
const INLINE_JSON: usize = 4000;

/// Why a profile's content was requested.
pub enum ContentPurpose {
    Preview,
    Edit,
    External,
    Duplicate,
    Export(PathBuf),
}

/// What to do once the daemon stored a profile.
pub enum SavePurpose {
    /// A new profile; open it in the editor when `edit` is set.
    Created {
        edit: bool,
    },
    /// Saved from the tree editor; `saved` becomes its clean state.
    Editor {
        saved: Value,
    },
    /// Saved from `$EDITOR`; kept to edit again if the save fails.
    External(ExternalEdit),
    Plain,
}

pub struct Preview {
    pub id: String,
    pub updated_at: u64,
    pub result: Result<PreviewInfo, String>,
}

pub struct PreviewInfo {
    pub summary: Summary,
    pub has_comments: bool,
}

#[derive(Default)]
pub struct ProfilesView {
    pub requested: bool,
    pub list: Option<ProfileList>,
    pub error: Option<String>,
    pub state: TableState,
    pub preview: Option<Preview>,
    /// (id, updated_at) of a preview request in flight.
    preview_pending: Option<(String, u64)>,
    pub editor: Option<Editor>,
    pub editor_state: ListState,
    ticks: usize,
}

impl ProfilesView {
    pub fn profiles(&self) -> &[Profile] {
        self.list.as_ref().map_or(&[], |l| l.profiles.as_slice())
    }

    pub fn selected(&self) -> Option<&Profile> {
        self.profiles().get(self.state.selected()?)
    }
}

impl App {
    // ----- loading ----------------------------------------------------------------

    pub(super) fn profiles_tab_opened(&mut self) {
        if !self.profiles.requested {
            self.profiles.requested = true;
            self.profiles_load();
        }
    }

    pub(super) fn profiles_load(&mut self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.profiles().await.map_err(|e| error_chain(&e));
            let _ = tx.send(AppEvent::Profiles(result));
        });
    }

    /// Re-reads the list after an action and every few seconds while shown.
    pub(super) fn profiles_tick(&mut self) {
        if self.tab != Tab::Profiles || self.profiles.editor.is_some() {
            return;
        }
        self.profiles.ticks += 1;
        if self.profiles.ticks >= REFRESH_TICKS {
            self.profiles.ticks = 0;
            self.profiles_load();
        }
    }

    pub(super) fn profiles_refresh_after_action(&mut self) {
        if self.profiles.requested {
            self.profiles_load();
        }
    }

    pub(super) fn profiles_loaded(&mut self, result: Result<ProfileList, String>) {
        match result {
            Ok(list) => {
                let keep = self.profiles.selected().map(|p| p.id.clone());
                let index = keep
                    .and_then(|id| list.profiles.iter().position(|p| p.id == id))
                    .or_else(|| list.profiles.iter().position(|p| p.active))
                    .or(self.profiles.state.selected())
                    .map(|i| i.min(list.profiles.len().saturating_sub(1)));
                let empty = list.profiles.is_empty();
                self.profiles.list = Some(list);
                self.profiles.error = None;
                self.profiles
                    .state
                    .select(if empty { None } else { index.or(Some(0)) });
                self.profiles_request_preview();
            }
            Err(err) => self.profiles.error = Some(err),
        }
    }

    fn profiles_request_preview(&mut self) {
        let Some(profile) = self.profiles.selected() else {
            self.profiles.preview = None;
            return;
        };
        let key = (profile.id.clone(), profile.updated_at);
        let current = self
            .profiles
            .preview
            .as_ref()
            .map(|p| (p.id.clone(), p.updated_at));
        if current.as_ref() == Some(&key) || self.profiles.preview_pending.as_ref() == Some(&key) {
            return;
        }
        let id = profile.id.clone();
        self.profiles.preview_pending = Some(key);
        self.profile_content(id, ContentPurpose::Preview);
    }

    fn profile_content(&mut self, id: String, purpose: ContentPurpose) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.profile(&id).await.map_err(|e| error_chain(&e));
            let _ = tx.send(AppEvent::ProfileContent { purpose, result });
        });
    }

    pub(super) fn profile_content_loaded(
        &mut self,
        purpose: ContentPurpose,
        result: Result<(Profile, String), String>,
    ) {
        match purpose {
            ContentPurpose::Preview => {
                self.profiles.preview_pending = None;
                let Ok((profile, content)) = result else {
                    return;
                };
                self.profiles.preview = Some(Preview {
                    id: profile.id,
                    updated_at: profile.updated_at,
                    result: profile::parse(&content)
                        .map(|value| PreviewInfo {
                            summary: profile::summarize(&value),
                            has_comments: profile::has_comments(&content),
                        })
                        .map_err(|e| error_chain(&e)),
                });
                // The selection may have moved on meanwhile.
                self.profiles_request_preview();
            }
            _ if result.is_err() => {
                if let Err(err) = result {
                    self.notify(err, true);
                }
            }
            ContentPurpose::Edit => {
                let Ok((profile, content)) = result else {
                    return;
                };
                match Editor::new(profile, &content) {
                    Ok(editor) => {
                        if editor.had_comments {
                            self.notify(fl!("tui-editor-comments"), false);
                        }
                        self.profiles.editor = Some(editor);
                        self.profiles.editor_state = ListState::default();
                    }
                    Err(err) => self.notify(
                        format!("{}\n{}", fl!("tui-editor-unparsable"), error_chain(&err)),
                        true,
                    ),
                }
            }
            ContentPurpose::External => {
                let Ok((profile, content)) = result else {
                    return;
                };
                self.external = Some(ExternalEdit {
                    name: profile.name.clone(),
                    text: content,
                    target: ExternalTarget::Profile {
                        id: profile.id,
                        active: profile.active,
                    },
                });
            }
            ContentPurpose::Duplicate => {
                let Ok((profile, content)) = result else {
                    return;
                };
                let request = Request::ProfileAdd {
                    name: Some(fl!("tui-profile-copy-name", name = profile.name)),
                    content: Some(content),
                    url: None,
                    interval: None,
                    activate: false,
                };
                self.profile_save_request(&fl!("busy-duplicating"), request, SavePurpose::Plain);
            }
            ContentPurpose::Export(path) => {
                let Ok((_, content)) = result else { return };
                let shown = path.display().to_string();
                match std::fs::write(&path, content) {
                    Ok(()) => self.notify(fl!("tui-exported", path = shown), false),
                    Err(err) => {
                        self.notify(format!("{}: {err}", fl!("err-create", path = shown)), true)
                    }
                }
            }
        }
    }

    /// Sends a request answered with `profile_saved`.
    fn profile_save_request(&mut self, label: &str, request: Request, purpose: SavePurpose) {
        let id = self.begin(label);
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .profile_saved(request)
                .await
                .map_err(|e| error_chain(&e));
            let _ = tx.send(AppEvent::ProfileSaved {
                id,
                purpose,
                result,
            });
        });
    }

    pub(super) fn profile_saved(
        &mut self,
        id: u64,
        purpose: SavePurpose,
        result: Result<(Profile, String), String>,
    ) {
        self.busy.retain(|(busy, _)| *busy != id);
        self.profiles_load();
        match (purpose, result) {
            (SavePurpose::Created { edit }, Ok((profile, message))) => {
                self.profiles_select(&profile.id);
                self.notify(message, false);
                if edit {
                    self.profile_content(profile.id, ContentPurpose::Edit);
                }
            }
            (SavePurpose::Editor { saved }, Ok((profile, message))) => {
                if let Some(editor) = &mut self.profiles.editor
                    && editor.profile.id == profile.id
                {
                    editor.saving = false;
                    editor.mark_saved(saved);
                    editor.profile = profile;
                }
                self.notify(message, false);
            }
            (SavePurpose::Editor { .. }, Err(err)) => {
                let Some(editor) = &mut self.profiles.editor else {
                    self.notify(err, true);
                    return;
                };
                editor.saving = false;
                let action = MenuAction::SaveAnyway {
                    id: editor.profile.id.clone(),
                    content: editor.text(),
                    from_editor: true,
                };
                self.popup = Some(Popup::Menu(
                    Menu::new(
                        fl!("tui-save-failed-title"),
                        vec![
                            MenuItem::new(fl!("tui-keep-editing"), "", MenuAction::Dismiss),
                            MenuItem::new(
                                fl!("tui-save-anyway"),
                                fl!("tui-save-anyway-detail"),
                                action,
                            ),
                        ],
                    )
                    .body(err),
                ));
            }
            (SavePurpose::External(edit), Err(err)) => {
                let ExternalTarget::Profile { id, .. } = &edit.target else {
                    return;
                };
                let force = MenuAction::SaveAnyway {
                    id: id.clone(),
                    content: edit.text.clone(),
                    from_editor: false,
                };
                self.popup = Some(Popup::Menu(
                    Menu::new(
                        fl!("tui-save-failed-title"),
                        vec![
                            MenuItem::new(fl!("tui-edit-again"), "", MenuAction::EditAgain(edit)),
                            MenuItem::new(
                                fl!("tui-save-anyway"),
                                fl!("tui-save-anyway-detail"),
                                force,
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

    fn profiles_select(&mut self, id: &str) {
        if let Some(index) = self.profiles.profiles().iter().position(|p| p.id == id) {
            self.profiles.state.select(Some(index));
        }
    }

    // ----- list keys ----------------------------------------------------------------

    pub(super) fn profiles_on_key(&mut self, key: KeyEvent) {
        let len = self.profiles.profiles().len();
        let selected = self.profiles.selected().cloned();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.profiles_move(len, -1),
            KeyCode::Down | KeyCode::Char('j') => self.profiles_move(len, 1),
            KeyCode::PageUp => self.profiles_move(len, -10),
            KeyCode::PageDown => self.profiles_move(len, 10),
            KeyCode::Home | KeyCode::Char('g') => self.profiles_move(len, isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.profiles_move(len, isize::MAX / 2),
            KeyCode::Enter => self.profile_menu(),
            KeyCode::Char('n') => self.profile_input(InputPurpose::NewProfile),
            KeyCode::Char('i') => self.profile_input(InputPurpose::ImportProfile),
            KeyCode::Char('F') => self.daemon_action(
                &fl!("busy-updating-profiles"),
                Request::ProfileUpdate {
                    id: None,
                    force: false,
                },
            ),
            KeyCode::Char('A') => {
                if self.profiles.list.as_ref().is_some_and(|l| l.unmanaged) {
                    self.popup = Some(Popup::Confirm {
                        message: fl!("tui-confirm-adopt"),
                        action: PendingAction::ProfileAdopt,
                    });
                } else {
                    self.notify(fl!("profiles-nothing-to-adopt"), false);
                }
            }
            _ => {
                let Some(profile) = selected else { return };
                let action = match key.code {
                    KeyCode::Char('e') => ProfileAction::Edit,
                    KeyCode::Char('E') => ProfileAction::EditExternal,
                    KeyCode::Char('f') => ProfileAction::Update,
                    KeyCode::Char('d') | KeyCode::Delete => ProfileAction::Delete,
                    KeyCode::Char('y') => ProfileAction::CopyUrl,
                    _ => return,
                };
                self.profile_action(profile.id, action);
            }
        }
    }

    fn profiles_move(&mut self, len: usize, delta: isize) {
        if len == 0 {
            return;
        }
        let current = self.profiles.state.selected().unwrap_or(0) as isize;
        let next = current.saturating_add(delta).clamp(0, len as isize - 1) as usize;
        self.profiles.state.select(Some(next));
        self.profiles_request_preview();
    }

    fn profile_menu(&mut self) {
        let Some(profile) = self.profiles.selected().cloned() else {
            self.profile_input(InputPurpose::NewProfile);
            return;
        };
        let item = |label: String, detail: String, action: ProfileAction| {
            MenuItem::new(
                label,
                detail,
                MenuAction::Profile(profile.id.clone(), action),
            )
        };
        let mut items = Vec::new();
        if !profile.active {
            items.push(item(
                fl!("menu-profile-use"),
                fl!("menu-profile-use-detail"),
                ProfileAction::Use,
            ));
        }
        items.push(item(
            fl!("menu-profile-edit"),
            "e".into(),
            ProfileAction::Edit,
        ));
        items.push(item(
            fl!("menu-profile-edit-external"),
            "E".into(),
            ProfileAction::EditExternal,
        ));
        if profile.is_remote() {
            items.push(item(
                fl!("menu-profile-update"),
                "f".into(),
                ProfileAction::Update,
            ));
        }
        items.push(item(
            fl!("menu-profile-rename"),
            String::new(),
            ProfileAction::Rename,
        ));
        items.push(item(
            fl!("menu-profile-url"),
            profile
                .url
                .as_deref()
                .map(crate::util::shorten_url)
                .unwrap_or_default(),
            ProfileAction::SetUrl,
        ));
        if profile.is_remote() {
            items.push(item(
                fl!("menu-profile-interval"),
                interval_label(profile.interval),
                ProfileAction::SetInterval,
            ));
            items.push(item(
                fl!("menu-profile-make-local"),
                fl!("menu-profile-make-local-detail"),
                ProfileAction::MakeLocal,
            ));
            items.push(item(
                fl!("menu-profile-copy-url"),
                "y".into(),
                ProfileAction::CopyUrl,
            ));
        }
        items.push(item(
            fl!("menu-profile-duplicate"),
            String::new(),
            ProfileAction::Duplicate,
        ));
        items.push(item(
            fl!("menu-profile-check"),
            "sing-box check".into(),
            ProfileAction::Check,
        ));
        items.push(item(
            fl!("menu-profile-export"),
            String::new(),
            ProfileAction::Export,
        ));
        if !profile.active {
            items.push(item(
                fl!("menu-profile-delete"),
                "d".into(),
                ProfileAction::Delete,
            ));
        }
        self.popup = Some(Popup::Menu(Menu::new(profile.name.clone(), items)));
    }

    fn profile_input(&mut self, purpose: InputPurpose) {
        let profile = self.profiles.selected().cloned();
        let input = match &purpose {
            InputPurpose::NewProfile => Input::new(
                fl!("tui-new-profile-title"),
                fl!("tui-new-profile-hint"),
                purpose,
            )
            .placeholder(fl!("tui-new-profile-placeholder")),
            InputPurpose::ImportProfile => Input::new(
                fl!("tui-import-profile-title"),
                fl!("tui-import-profile-hint"),
                purpose,
            )
            .placeholder("https://example.com/sing-box.json  |  ~/config.json"),
            InputPurpose::RenameProfile(_) => {
                Input::new(fl!("tui-rename-profile-title"), String::new(), purpose)
                    .value(profile.map(|p| p.name).unwrap_or_default())
            }
            InputPurpose::ProfileUrl(_) => Input::new(
                fl!("tui-profile-url-title"),
                fl!("tui-profile-url-hint"),
                purpose,
            )
            .value(profile.and_then(|p| p.url).unwrap_or_default())
            .placeholder("https://"),
            InputPurpose::ProfileInterval(_) => Input::new(
                fl!("tui-profile-interval-title"),
                fl!("tui-profile-interval-hint"),
                purpose,
            )
            .value(profile.map(|p| p.interval.to_string()).unwrap_or_default()),
            InputPurpose::ExportProfile(_) => {
                let name = profile.map(|p| p.name).unwrap_or_default();
                Input::new(fl!("tui-export-title"), fl!("tui-export-hint"), purpose)
                    .value(format!("~/{name}.json"))
            }
            _ => return,
        };
        self.popup = Some(Popup::Input(input));
    }

    pub(super) fn profile_action(&mut self, id: String, action: ProfileAction) {
        let Some(profile) = self
            .profiles
            .profiles()
            .iter()
            .find(|p| p.id == id)
            .cloned()
        else {
            return;
        };
        match action {
            ProfileAction::Use => {
                let mut message = vec![fl!("tui-confirm-use-profile", name = profile.name.clone())];
                if self.profiles.list.as_ref().is_some_and(|l| l.unmanaged) {
                    message.push(fl!("tui-use-adopts-note"));
                }
                message.push(fl!("tui-use-profile-note"));
                self.popup = Some(Popup::Confirm {
                    message: message.join("\n"),
                    action: PendingAction::ProfileUse { id },
                });
            }
            ProfileAction::Edit => self.profile_content(id, ContentPurpose::Edit),
            ProfileAction::EditExternal => self.profile_content(id, ContentPurpose::External),
            ProfileAction::Update => {
                if !profile.is_remote() {
                    self.notify(fl!("profiles-not-remote", name = profile.name), true);
                    return;
                }
                self.daemon_action(
                    &fl!("busy-updating-profile"),
                    Request::ProfileUpdate {
                        id: Some(id),
                        force: false,
                    },
                );
            }
            ProfileAction::Rename => self.profile_input(InputPurpose::RenameProfile(id)),
            ProfileAction::SetUrl => self.profile_input(InputPurpose::ProfileUrl(id)),
            ProfileAction::SetInterval => self.profile_input(InputPurpose::ProfileInterval(id)),
            ProfileAction::MakeLocal => self.daemon_action(
                &fl!("busy-saving"),
                Request::ProfileSet {
                    id,
                    name: None,
                    url: Some(String::new()),
                    interval: None,
                },
            ),
            ProfileAction::Duplicate => self.profile_content(id, ContentPurpose::Duplicate),
            ProfileAction::Check => {
                self.daemon_action(&fl!("busy-checking"), Request::ProfileCheck { id })
            }
            ProfileAction::Export => self.profile_input(InputPurpose::ExportProfile(id)),
            ProfileAction::CopyUrl => match profile.url {
                Some(url) => self.copy(url, fl!("tui-copied-profile-url")),
                None => self.notify(fl!("profiles-not-remote", name = profile.name), true),
            },
            ProfileAction::Delete => {
                if profile.active {
                    self.notify(fl!("profiles-remove-active", name = profile.name), true);
                    return;
                }
                self.popup = Some(Popup::Confirm {
                    message: fl!("tui-confirm-delete-profile", name = profile.name),
                    action: PendingAction::ProfileDelete { id },
                });
            }
        }
    }

    pub(super) fn profiles_run_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::ProfileUse { id } => self.daemon_action(
                &fl!("busy-switching-profile"),
                Request::ProfileActivate { id, force: false },
            ),
            PendingAction::ProfileDelete { id } => {
                self.daemon_action(&fl!("busy-deleting"), Request::ProfileRemove { id })
            }
            PendingAction::ProfileAdopt => {
                self.daemon_action(&fl!("busy-saving"), Request::ProfileAdopt)
            }
            PendingAction::DiscardEditor => self.editor_close(),
            _ => {}
        }
    }

    /// Handles a submitted text field; an error keeps the field open.
    pub(super) fn profiles_submit_input(
        &mut self,
        purpose: InputPurpose,
        value: String,
    ) -> Result<(), String> {
        let trimmed = value.trim().to_owned();
        match purpose {
            InputPurpose::NewProfile => {
                let request = Request::ProfileAdd {
                    name: Some(trimmed).filter(|n| !n.is_empty()),
                    content: None,
                    url: None,
                    interval: None,
                    activate: false,
                };
                self.profile_save_request(
                    &fl!("busy-saving"),
                    request,
                    SavePurpose::Created { edit: true },
                );
            }
            InputPurpose::ImportProfile => {
                if trimmed.is_empty() {
                    return Err(fl!("tui-import-empty"));
                }
                let request = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    Request::ProfileAdd {
                        name: None,
                        content: None,
                        url: Some(trimmed),
                        interval: None,
                        activate: false,
                    }
                } else {
                    let path = expand_home(&trimmed);
                    let content = std::fs::read_to_string(&path).map_err(|err| {
                        format!(
                            "{}: {err}",
                            fl!("err-read", path = path.display().to_string())
                        )
                    })?;
                    profile::parse(&content).map_err(|e| error_chain(&e))?;
                    Request::ProfileAdd {
                        name: path.file_stem().and_then(|s| s.to_str()).map(str::to_owned),
                        content: Some(content),
                        url: None,
                        interval: None,
                        activate: false,
                    }
                };
                self.profile_save_request(
                    &fl!("busy-importing-profile"),
                    request,
                    SavePurpose::Created { edit: false },
                );
            }
            InputPurpose::RenameProfile(id) => {
                if trimmed.is_empty() {
                    return Err(fl!("profiles-name-empty"));
                }
                self.daemon_action(
                    &fl!("busy-saving"),
                    Request::ProfileSet {
                        id,
                        name: Some(trimmed),
                        url: None,
                        interval: None,
                    },
                );
            }
            InputPurpose::ProfileUrl(id) => self.daemon_action(
                &fl!("busy-saving"),
                Request::ProfileSet {
                    id,
                    name: None,
                    url: Some(trimmed),
                    interval: None,
                },
            ),
            InputPurpose::ProfileInterval(id) => {
                let minutes = trimmed
                    .parse::<u64>()
                    .map_err(|_| fl!("tui-interval-invalid"))?;
                self.daemon_action(
                    &fl!("busy-saving"),
                    Request::ProfileSet {
                        id,
                        name: None,
                        url: None,
                        interval: Some(minutes),
                    },
                );
            }
            InputPurpose::ExportProfile(id) => {
                if trimmed.is_empty() {
                    return Err(fl!("tui-import-empty"));
                }
                self.profile_content(id, ContentPurpose::Export(expand_home(&trimmed)));
            }
            InputPurpose::EditValue(path) => {
                let editor = self.profiles.editor.as_mut().ok_or_default()?;
                let old = get(&editor.root, &path).cloned().unwrap_or(Value::Null);
                let value = parse_scalar(&old, &value).map_err(|e| error_chain(&e))?;
                editor.set(&path, value).map_err(|e| error_chain(&e))?;
            }
            InputPurpose::EditJson(path) => {
                let editor = self.profiles.editor.as_mut().ok_or_default()?;
                let value = parse_json(&value).map_err(|e| error_chain(&e))?;
                if path.is_empty() && !value.is_object() {
                    return Err(fl!("profile-not-object"));
                }
                editor.set(&path, value).map_err(|e| error_chain(&e))?;
            }
            InputPurpose::RenameKey(path) => {
                let editor = self.profiles.editor.as_mut().ok_or_default()?;
                editor
                    .rename(&path, &trimmed)
                    .map_err(|e| error_chain(&e))?;
            }
            InputPurpose::NewKey(target) => {
                let editor = self.profiles.editor.as_ref().ok_or_default()?;
                if trimmed.is_empty() {
                    return Err(fl!("editor-key-empty"));
                }
                if let Some(Value::Object(map)) = get(&editor.root, target.container())
                    && map.contains_key(&trimmed)
                {
                    return Err(fl!("editor-key-taken", key = trimmed));
                }
                let items = templates::generic()
                    .into_iter()
                    .map(|t| {
                        let edit = !is_container(&t.value);
                        MenuItem::new(
                            t.label,
                            t.detail,
                            MenuAction::Insert {
                                target: target.clone(),
                                key: Some(trimmed.clone()),
                                value: t.value,
                                edit,
                            },
                        )
                    })
                    .collect();
                self.popup = Some(Popup::Menu(Menu::new(
                    fl!("tui-value-type-title", key = trimmed.clone()),
                    items,
                )));
            }
            InputPurpose::Search => {
                let editor = self.profiles.editor.as_mut().ok_or_default()?;
                if trimmed.is_empty() {
                    editor.search = None;
                    return Ok(());
                }
                if !editor.find(&trimmed, false) {
                    return Err(fl!("tui-search-none"));
                }
                editor.search = Some(trimmed);
            }
            InputPurpose::AddSource | InputPurpose::ImportCore => {}
        }
        Ok(())
    }

    pub(super) fn profiles_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::Profile(id, action) => self.profile_action(id, action),
            MenuAction::Insert {
                target,
                key,
                value,
                edit,
            } => {
                let Some(editor) = &mut self.profiles.editor else {
                    return;
                };
                let open = is_container(&value) && value.as_object().is_some_and(|m| !m.is_empty());
                match editor.insert(&target, key, value) {
                    Ok(path) => {
                        if open {
                            editor.expanded.insert(path.clone());
                        }
                        if edit {
                            self.editor_type_value(path);
                        }
                    }
                    Err(err) => self.notify(error_chain(&err), true),
                }
            }
            MenuAction::CustomKey(target) => {
                self.popup = Some(Popup::Input(Input::new(
                    fl!("tui-new-key-title"),
                    fl!("tui-new-key-hint"),
                    InputPurpose::NewKey(target),
                )));
            }
            MenuAction::SetValue(path, value) => {
                if let Some(editor) = &mut self.profiles.editor
                    && let Err(err) = editor.set(&path, value)
                {
                    self.notify(error_chain(&err), true);
                }
            }
            MenuAction::TypeValue(path) => self.editor_type_value(path),
            MenuAction::SaveAnyway {
                id,
                content,
                from_editor,
            } => {
                let purpose = match (from_editor, profile::parse(&content)) {
                    (true, Ok(saved)) => SavePurpose::Editor { saved },
                    _ => SavePurpose::Plain,
                };
                if let Some(editor) = &mut self.profiles.editor
                    && from_editor
                {
                    editor.saving = true;
                }
                self.profile_save_request(
                    &fl!("busy-saving"),
                    Request::ProfileSave {
                        id,
                        content,
                        force: true,
                    },
                    purpose,
                );
            }
            MenuAction::EditAgain(edit) => self.external = Some(edit),
            MenuAction::Component(..) | MenuAction::Dismiss => {}
        }
    }

    /// `$EDITOR` returned: store a profile or replace a node in the editor.
    pub fn external_edit_done(&mut self, edit: ExternalEdit, result: anyhow::Result<String>) {
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
        let problem = match &edited.target {
            ExternalTarget::Profile { .. } => profile::parse(&text).err(),
            ExternalTarget::Node(_) => parse_json(&text).err(),
        };
        if let Some(err) = problem {
            self.popup = Some(Popup::Menu(
                Menu::new(
                    fl!("tui-invalid-edit-title"),
                    vec![
                        MenuItem::new(fl!("tui-edit-again"), "", MenuAction::EditAgain(edited)),
                        MenuItem::new(fl!("tui-discard-changes"), "", MenuAction::Dismiss),
                    ],
                )
                .body(error_chain(&err)),
            ));
            return;
        }
        match edited.target.clone() {
            ExternalTarget::Profile { id, active } => self.profile_save_request(
                &if active {
                    fl!("busy-saving-reloading")
                } else {
                    fl!("busy-saving")
                },
                Request::ProfileSave {
                    id,
                    content: text,
                    force: false,
                },
                SavePurpose::External(edited),
            ),
            ExternalTarget::Node(path) => {
                let Some(editor) = &mut self.profiles.editor else {
                    return;
                };
                let applied = parse_json(&text).and_then(|value| {
                    if path.is_empty() && !value.is_object() {
                        anyhow::bail!(fl!("profile-not-object"));
                    }
                    editor.set(&path, value)
                });
                if let Err(err) = applied {
                    self.notify(error_chain(&err), true);
                }
            }
        }
    }

    // ----- editor keys -----------------------------------------------------------------

    pub(super) fn editor_on_key(&mut self, key: KeyEvent) {
        let Some(editor) = &mut self.profiles.editor else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let path = editor.selected();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => editor.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => editor.move_cursor(1),
            KeyCode::PageUp => editor.move_cursor(-15),
            KeyCode::PageDown => editor.move_cursor(15),
            KeyCode::Home | KeyCode::Char('g') => editor.move_cursor(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => editor.move_cursor(isize::MAX / 2),
            KeyCode::Left | KeyCode::Char('h') => editor.left(),
            KeyCode::Right | KeyCode::Char('l') => editor.right(),
            KeyCode::Char(' ') => editor.toggle(),
            KeyCode::Char('*') => editor.expand_all(),
            KeyCode::Char('-') => editor.collapse_all(),
            KeyCode::Char('u') => {
                if !editor.undo() {
                    self.notify(fl!("tui-nothing-to-undo"), false);
                }
            }
            KeyCode::Char('U') | KeyCode::Char('r') if ctrl || key.code == KeyCode::Char('U') => {
                if !editor.redo() {
                    self.notify(fl!("tui-nothing-to-redo"), false);
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                let backwards = key.code == KeyCode::Char('N');
                match editor.search.clone() {
                    Some(query) => {
                        editor.find(&query, backwards);
                    }
                    None => self.editor_search(),
                }
            }
            KeyCode::Char('/') => self.editor_search(),
            KeyCode::Char('s') => self.editor_save(),
            KeyCode::Char('q') | KeyCode::Esc => {
                if editor.dirty() {
                    self.popup = Some(Popup::Confirm {
                        message: fl!("tui-confirm-discard", name = editor.profile.name.clone()),
                        action: PendingAction::DiscardEditor,
                    });
                } else {
                    self.editor_close();
                }
            }
            KeyCode::Char('?') => self.popup = Some(Popup::EditorHelp),
            KeyCode::Char('a') => self.editor_add(false),
            KeyCode::Char('A') => self.editor_add(true),
            _ => {
                let Some(path) = path else { return };
                match key.code {
                    KeyCode::Enter => {
                        let container = get(&editor.root, &path).is_some_and(is_container);
                        if container {
                            editor.toggle();
                        } else {
                            self.editor_edit(path);
                        }
                    }
                    KeyCode::Char('e') => self.editor_edit(path),
                    KeyCode::Char(':') => self.editor_json(path),
                    KeyCode::Char('E') => {
                        let text = get(&editor.root, &path)
                            .map(profile::to_text)
                            .unwrap_or_default();
                        self.external = Some(ExternalEdit {
                            name: editor.profile.name.clone(),
                            text,
                            target: ExternalTarget::Node(path),
                        });
                    }
                    KeyCode::Char('r') => {
                        let Some(Seg::Key(name)) = path.last().cloned() else {
                            self.notify(fl!("editor-not-a-member"), true);
                            return;
                        };
                        self.popup = Some(Popup::Input(
                            Input::new(
                                fl!("tui-rename-key-title"),
                                display_path(&path),
                                InputPurpose::RenameKey(path),
                            )
                            .value(name),
                        ));
                    }
                    KeyCode::Char('d') | KeyCode::Delete => match editor.delete(&path) {
                        Ok(_) => {
                            self.notify(fl!("tui-node-deleted", path = display_path(&path)), false)
                        }
                        Err(err) => self.notify(error_chain(&err), true),
                    },
                    KeyCode::Char('c') => {
                        if let Err(err) = editor.duplicate(&path) {
                            self.notify(error_chain(&err), true);
                        }
                    }
                    KeyCode::Char('K') | KeyCode::Char('J') => {
                        let delta = if key.code == KeyCode::Char('K') {
                            -1
                        } else {
                            1
                        };
                        if let Err(err) = editor.move_by(&path, delta) {
                            self.notify(error_chain(&err), true);
                        }
                    }
                    KeyCode::Char('y') => {
                        let text = get(&editor.root, &path)
                            .and_then(|v| serde_json::to_string_pretty(v).ok())
                            .unwrap_or_default();
                        self.copy(text, fl!("tui-copied-node"));
                    }
                    _ => {}
                }
            }
        }
    }

    fn editor_search(&mut self) {
        let current = self
            .profiles
            .editor
            .as_ref()
            .and_then(|e| e.search.clone())
            .unwrap_or_default();
        self.popup = Some(Popup::Input(
            Input::new(
                fl!("tui-search-title"),
                fl!("tui-search-hint"),
                InputPurpose::Search,
            )
            .value(current),
        ));
    }

    /// Changes a value: booleans flip, references offer the existing tags,
    /// other scalars open a text field, containers the JSON field.
    fn editor_edit(&mut self, path: Path) {
        let Some(editor) = &mut self.profiles.editor else {
            return;
        };
        let Some(value) = get(&editor.root, &path).cloned() else {
            return;
        };
        match value {
            Value::Bool(flag) => {
                if let Err(err) = editor.set(&path, Value::Bool(!flag)) {
                    self.notify(error_chain(&err), true);
                }
            }
            Value::String(_) | Value::Null => match templates::references(&editor.root, &path) {
                Some(tags) if !tags.is_empty() => {
                    let current = value.as_str().unwrap_or_default().to_owned();
                    let mut items: Vec<MenuItem> = tags
                        .into_iter()
                        .map(|tag| {
                            let detail = if tag == current {
                                "●".to_owned()
                            } else {
                                String::new()
                            };
                            MenuItem::new(
                                tag.clone(),
                                detail,
                                MenuAction::SetValue(path.clone(), Value::String(tag)),
                            )
                        })
                        .collect();
                    let selected = items.iter().position(|i| i.label == current).unwrap_or(0);
                    items.push(MenuItem::new(
                        fl!("tui-type-value"),
                        String::new(),
                        MenuAction::TypeValue(path.clone()),
                    ));
                    let mut menu = Menu::new(display_path(&path), items);
                    menu.selected = selected;
                    self.popup = Some(Popup::Menu(menu));
                }
                _ => self.editor_type_value(path),
            },
            Value::Number(_) => self.editor_type_value(path),
            Value::Object(_) | Value::Array(_) => self.editor_json(path),
        }
    }

    fn editor_type_value(&mut self, path: Path) {
        let Some(editor) = &self.profiles.editor else {
            return;
        };
        let text = match get(&editor.root, &path) {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => return,
        };
        self.popup = Some(Popup::Input(
            Input::new(
                fl!("tui-edit-value-title"),
                display_path(&path),
                InputPurpose::EditValue(path),
            )
            .value(text),
        ));
    }

    fn editor_json(&mut self, path: Path) {
        let Some(editor) = &self.profiles.editor else {
            return;
        };
        let text = get(&editor.root, &path)
            .map(Value::to_string)
            .unwrap_or_default();
        if text.len() > INLINE_JSON {
            self.notify(fl!("tui-json-too-long"), false);
            return;
        }
        self.popup = Some(Popup::Input(
            Input::new(
                fl!("tui-edit-json-title"),
                display_path(&path),
                InputPurpose::EditJson(path),
            )
            .value(text),
        ));
    }

    /// `a` adds after the selection, or into it when it is an open
    /// container; `A` always adds into the selected container.
    fn editor_add(&mut self, inside: bool) {
        let Some(editor) = &self.profiles.editor else {
            return;
        };
        let selected = editor.selected();
        let inside = inside
            || selected
                .as_ref()
                .is_some_and(|p| editor.expanded.contains(p));
        let container_of = |path: &Path| -> Option<InsertTarget> {
            match get(&editor.root, path)? {
                Value::Array(items) => Some(InsertTarget::Array {
                    path: path.clone(),
                    index: items.len(),
                }),
                Value::Object(map) => Some(InsertTarget::Object {
                    path: path.clone(),
                    index: map.len(),
                }),
                _ => None,
            }
        };
        let target = match &selected {
            Some(path) if inside => container_of(path),
            _ => None,
        }
        .or_else(|| {
            let path = selected.as_ref()?;
            let (last, parent) = path.split_last()?;
            match (last, get(&editor.root, parent)?) {
                (Seg::Index(i), Value::Array(_)) => Some(InsertTarget::Array {
                    path: parent.to_vec(),
                    index: i + 1,
                }),
                (Seg::Key(key), Value::Object(map)) => Some(InsertTarget::Object {
                    path: parent.to_vec(),
                    index: index_of(map, key).map_or(map.len(), |i| i + 1),
                }),
                _ => None,
            }
        })
        .or_else(|| container_of(&Vec::new()));
        let Some(target) = target else { return };
        let title = match target.container().is_empty() {
            true => fl!("tui-add-title-root"),
            false => fl!("tui-add-title", path = display_path(target.container())),
        };
        let insert = |value: Value, key: Option<String>| MenuAction::Insert {
            target: target.clone(),
            key,
            edit: !is_container(&value),
            value,
        };
        let items: Vec<MenuItem> = match &target {
            InsertTarget::Array { path, .. } => {
                if templates::holds_references(&editor.root, path) {
                    let mut probe = path.clone();
                    probe.push(Seg::Index(0));
                    let mut items: Vec<MenuItem> = templates::references(&editor.root, &probe)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|tag| {
                            let action = MenuAction::Insert {
                                target: target.clone(),
                                key: None,
                                value: Value::String(tag.clone()),
                                edit: false,
                            };
                            MenuItem::new(tag, "", action)
                        })
                        .collect();
                    items.push(MenuItem::new(
                        fl!("tui-type-value"),
                        "",
                        insert(Value::String(String::new()), None),
                    ));
                    items
                } else {
                    let sub_store = self.sub_store.as_ref();
                    templates::array_items(path, sub_store)
                        .into_iter()
                        .chain(templates::generic())
                        .map(|t| {
                            let action = MenuAction::Insert {
                                target: target.clone(),
                                key: None,
                                edit: !is_container(&t.value),
                                value: t.value,
                            };
                            MenuItem::new(t.label, t.detail, action)
                        })
                        .collect()
                }
            }
            InsertTarget::Object { path, .. } => {
                let present = |key: &str| matches!(get(&editor.root, path), Some(Value::Object(map)) if map.contains_key(key));
                let mut items: Vec<MenuItem> = templates::object_members(path)
                    .into_iter()
                    .filter(|(key, _)| !present(key))
                    .map(|(key, value)| {
                        let detail = value_brief(&value);
                        MenuItem::new(key, detail, insert(value, Some(key.to_owned())))
                    })
                    .collect();
                if items.is_empty() {
                    self.popup = Some(Popup::Input(Input::new(
                        fl!("tui-new-key-title"),
                        fl!("tui-new-key-hint"),
                        InputPurpose::NewKey(target.clone()),
                    )));
                    return;
                }
                items.push(MenuItem::new(
                    fl!("tui-custom-key"),
                    "",
                    MenuAction::CustomKey(target.clone()),
                ));
                items
            }
        };
        self.popup = Some(Popup::Menu(Menu::new(title, items)));
    }

    fn editor_save(&mut self) {
        let Some(editor) = &mut self.profiles.editor else {
            return;
        };
        if editor.saving {
            return;
        }
        if !editor.dirty() {
            self.notify(fl!("ctl-profile-no-changes"), false);
            return;
        }
        editor.saving = true;
        let request = Request::ProfileSave {
            id: editor.profile.id.clone(),
            content: editor.text(),
            force: false,
        };
        let purpose = SavePurpose::Editor {
            saved: editor.root.clone(),
        };
        let label = if editor.profile.active {
            fl!("busy-saving-reloading")
        } else {
            fl!("busy-saving")
        };
        self.profile_save_request(&label, request, purpose);
    }

    fn editor_close(&mut self) {
        self.profiles.editor = None;
        self.profiles.preview = None;
        self.profiles_load();
    }
}

trait OkOrDefault<T> {
    fn ok_or_default(self) -> Result<T, String>;
}

impl<T> OkOrDefault<T> for Option<T> {
    fn ok_or_default(self) -> Result<T, String> {
        self.ok_or_else(String::new)
    }
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest),
        None => PathBuf::from(path),
    }
}

/// `{3}`, `[12]` or the value itself, short.
fn value_brief(value: &Value) -> String {
    match value {
        Value::Object(map) => format!("{{{}}}", map.len()),
        Value::Array(items) => format!("[{}]", items.len()),
        Value::String(s) => format!("\"{}\"", truncate(s, 40)),
        other => other.to_string(),
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text_width(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        if text_width(&out) + 2 > width {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

fn relative(unix: u64) -> String {
    let age = now_unix().saturating_sub(unix);
    match age {
        0..60 => fl!("tui-ago-now"),
        60..3600 => fl!("tui-ago-minutes", minutes = (age / 60).to_string()),
        3600..86_400 => fl!("tui-ago-hours", hours = (age / 3600).to_string()),
        _ => local_time(unix, "%Y-%m-%d"),
    }
}

// ----- rendering ---------------------------------------------------------------------

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.profiles.editor.is_some() {
        draw_editor(frame, area, app);
        return;
    }
    let list = app.profiles.list.clone().unwrap_or_default();
    let banner = list
        .unmanaged
        .then(|| fl!("tui-profiles-unmanaged", path = list.slot.clone()));
    let banner_height = if banner.is_some() { 2 } else { 0 };
    let rows = list.profiles.len().max(1) as u16 + 3;
    let [banner_area, list_area, bottom] = Layout::vertical([
        Constraint::Length(banner_height),
        Constraint::Length(
            rows.min(area.height.saturating_sub(banner_height + 8))
                .max(4),
        ),
        Constraint::Min(6),
    ])
    .areas(area);
    if let Some(text) = banner {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ⚠ ", Style::new().fg(YELLOW).bold()),
                Span::styled(text, Style::new().fg(YELLOW)),
            ]))
            .wrap(Wrap { trim: true }),
            banner_area,
        );
    }
    draw_list(frame, list_area, app);
    let [summary_area, details_area] =
        Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)]).areas(bottom);
    draw_summary(frame, summary_area, app);
    draw_details(frame, details_area, app);
}

fn draw_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let count = app.profiles.profiles().len();
    let block = panel(&fl!("tui-panel-profiles", count = count), true).title_top(
        Line::from(dim(format!(
            " ⏎ {}  n {}  i {} ",
            fl!("key-actions"),
            fl!("key-new"),
            fl!("key-import")
        )))
        .right_aligned(),
    );
    if count == 0 {
        let text = match (&app.profiles.error, &app.profiles.list) {
            (Some(err), _) => Line::from(Span::styled(err.clone(), Style::new().fg(RED))),
            (None, None) => Line::from(dim(fl!("tui-loading"))),
            (None, Some(_)) => Line::from(dim(fl!("tui-profiles-empty"))),
        };
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: true }).block(block),
            area,
        );
        return;
    }
    let rows: Vec<Row> = app
        .profiles
        .profiles()
        .iter()
        .map(|p| {
            let (kind, color) = if p.is_remote() {
                (fl!("profile-kind-remote"), SKY)
            } else {
                (fl!("profile-kind-local"), BLUE)
            };
            let info = match (&p.last_error, &p.usage) {
                (Some(err), _) => Span::styled(format!("⚠ {}", err), Style::new().fg(YELLOW)),
                (None, Some(usage)) => dim(usage_label(usage)),
                (None, None) if p.is_remote() => dim(interval_label(p.interval)),
                (None, None) => dim(String::new()),
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    if p.active { "●" } else { " " },
                    Style::new().fg(GREEN),
                )),
                Cell::from(Span::styled(
                    p.name.clone(),
                    if p.active {
                        Style::new().fg(GREEN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(TEXT)
                    },
                )),
                Cell::from(chip(kind, color)),
                Cell::from(dim(relative(p.updated_at))),
                Cell::from(dim(fmt_bytes(p.size))),
                Cell::from(info),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(2),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Fill(3),
        ],
    )
    .header(
        Row::new([
            String::new(),
            fl!("col-name"),
            fl!("col-type"),
            fl!("col-updated"),
            fl!("col-size"),
            fl!("col-subscription"),
        ])
        .style(theme::header_row()),
    )
    .block(block)
    .row_highlight_style(selected(true))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, area, &mut app.profiles.state);
}

const SUMMARY_FIELD: usize = 10;

fn draw_summary(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel(&fl!("tui-panel-summary"), false);
    let Some(selected_profile) = app.profiles.selected() else {
        frame.render_widget(
            Paragraph::new(dim(fl!("tui-profiles-hint")))
                .wrap(Wrap { trim: true })
                .block(block),
            area,
        );
        return;
    };
    let preview = app
        .profiles
        .preview
        .as_ref()
        .filter(|p| p.id == selected_profile.id);
    let lines: Vec<Line> = match preview.map(|p| &p.result) {
        None => vec![Line::from(dim(fl!("tui-loading")))],
        Some(Err(err)) => vec![Line::from(Span::styled(err.clone(), Style::new().fg(RED)))],
        Some(Ok(info)) => summary_lines(&info.summary, info.has_comments),
    };
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(block),
        area,
    );
}

fn summary_lines(s: &Summary, has_comments: bool) -> Vec<Line<'static>> {
    let none = || dim(fl!("none"));
    let list = |items: &[String]| {
        if items.is_empty() {
            none()
        } else {
            Span::raw(join_list(items))
        }
    };
    let mut lines = vec![
        field(&fl!("summary-inbounds"), SUMMARY_FIELD, list(&s.inbounds)),
        field(
            &fl!("summary-outbounds"),
            SUMMARY_FIELD,
            Span::raw(if s.outbounds == 0 {
                fl!("none")
            } else {
                fl!(
                    "summary-outbound-count",
                    count = s.outbounds,
                    types = join_list(
                        &s.outbound_types
                            .iter()
                            .map(|(kind, n)| format!("{kind} {n}"))
                            .collect::<Vec<_>>()
                    )
                )
            }),
        ),
        field(&fl!("summary-groups"), SUMMARY_FIELD, list(&s.groups)),
    ];
    if s.endpoints > 0 {
        lines.push(field(
            &fl!("summary-endpoints"),
            SUMMARY_FIELD,
            Span::raw(s.endpoints.to_string()),
        ));
    }
    if !s.providers.is_empty() {
        lines.push(field(
            &fl!("summary-providers"),
            SUMMARY_FIELD,
            list(&s.providers),
        ));
    }
    let mut dns = s.dns_servers.clone();
    if let Some(final_server) = &s.dns_final {
        dns.push(fl!("summary-final", target = final_server.clone()));
    }
    lines.push(field(&fl!("summary-dns"), SUMMARY_FIELD, list(&dns)));
    lines.push(field(
        &fl!("summary-route"),
        SUMMARY_FIELD,
        Span::raw(fl!(
            "summary-route-detail",
            rules = s.rules,
            sets = s.rule_sets,
            target = s.route_final.clone().unwrap_or_else(|| fl!("none"))
        )),
    ));
    lines.push(field(
        "Clash API",
        SUMMARY_FIELD,
        match &s.clash_api {
            Some(address) => Span::styled(address.clone(), Style::new().fg(SUBTEXT)),
            None => Span::styled(fl!("summary-no-clash-api"), Style::new().fg(YELLOW)),
        },
    ));
    lines.push(field(
        &fl!("summary-log"),
        SUMMARY_FIELD,
        Span::raw(s.log_level.clone().unwrap_or_else(|| "info".to_owned())),
    ));
    if has_comments {
        lines.push(Line::from(dim(fl!("summary-has-comments"))));
    }
    lines
}

const DETAIL_FIELD: usize = 10;

fn draw_details(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel(&fl!("tui-panel-profile-details"), false);
    let Some(p) = app.profiles.selected() else {
        frame.render_widget(block, area);
        return;
    };
    let mut lines = vec![
        field(
            "ID",
            DETAIL_FIELD,
            Span::styled(p.id.clone(), Style::new().fg(SUBTEXT)),
        ),
        field(
            &fl!("detail-state"),
            DETAIL_FIELD,
            if p.active {
                Span::styled(fl!("tui-profile-in-use"), Style::new().fg(GREEN).bold())
            } else {
                dim(fl!("tui-profile-not-in-use"))
            },
        ),
        field(
            &fl!("detail-created"),
            DETAIL_FIELD,
            dim(local_time(p.created_at, "%Y-%m-%d %H:%M")),
        ),
        field(
            &fl!("detail-updated"),
            DETAIL_FIELD,
            dim(local_time(p.updated_at, "%Y-%m-%d %H:%M")),
        ),
    ];
    if let Some(url) = &p.url {
        lines.push(field(
            &fl!("detail-url"),
            DETAIL_FIELD,
            Span::styled(crate::util::shorten_url(url), Style::new().fg(SUBTEXT)),
        ));
        lines.push(field(
            &fl!("detail-interval"),
            DETAIL_FIELD,
            Span::raw(interval_label(p.interval)),
        ));
        lines.push(field(
            &fl!("detail-fetched"),
            DETAIL_FIELD,
            dim(p
                .fetched_at
                .map(|t| local_time(t, "%Y-%m-%d %H:%M"))
                .unwrap_or_else(|| fl!("none"))),
        ));
        if let Some(usage) = &p.usage {
            lines.push(field(
                &fl!("detail-usage"),
                DETAIL_FIELD,
                Span::raw(usage_label(usage)),
            ));
            if usage.total > 0 {
                lines.push(usage_bar(
                    usage.upload + usage.download,
                    usage.total,
                    area.width,
                ));
            }
        }
        if let Some(err) = &p.last_error {
            lines.push(field(
                &fl!("detail-last-error"),
                DETAIL_FIELD,
                Span::styled(err.clone(), Style::new().fg(YELLOW)),
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(block),
        area,
    );
}

fn usage_bar(used: u64, total: u64, width: u16) -> Line<'static> {
    let width = (width as usize)
        .saturating_sub(DETAIL_FIELD + 12)
        .clamp(8, 40);
    let ratio = (used as f64 / total as f64).clamp(0.0, 1.0);
    let filled = (ratio * width as f64).round() as usize;
    let color = match ratio {
        r if r >= 0.9 => RED,
        r if r >= 0.7 => YELLOW,
        _ => GREEN,
    };
    Line::from(vec![
        Span::raw(" ".repeat(DETAIL_FIELD)),
        Span::styled("█".repeat(filled), Style::new().fg(color)),
        Span::styled("░".repeat(width - filled), Style::new().fg(DIM)),
        dim(format!(" {:.0}%", ratio * 100.0)),
    ])
}

fn draw_editor(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(editor) = &app.profiles.editor else {
        return;
    };
    let [tree_area, detail_area] =
        Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)]).areas(area);
    let rows = editor.rows();
    let mut title = fl!("tui-editor-title", name = editor.profile.name.clone());
    if editor.dirty() {
        title.push_str(&format!(" {}", fl!("tui-editor-modified")));
    }
    let selected_path = rows
        .get(editor.cursor)
        .map(|r| r.path.clone())
        .unwrap_or_default();
    let mut block = panel(&title, true).title_bottom(
        Line::from(dim(format!(" {} ", display_path(&selected_path)))).left_aligned(),
    );
    if editor.profile.active {
        block = block.title_top(
            Line::from(Span::styled(
                format!(" ● {} ", fl!("tui-profile-in-use")),
                Style::new().fg(GREEN),
            ))
            .right_aligned(),
        );
    }
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| ListItem::new(tree_line(editor, &row.path, row.depth)))
        .collect();
    let cursor = editor.cursor;
    let detail = detail_lines(editor, &selected_path);
    let list = List::new(items)
        .block(block)
        .highlight_style(selected(true))
        .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    app.profiles
        .editor_state
        .select(if rows.is_empty() { None } else { Some(cursor) });
    frame.render_stateful_widget(list, tree_area, &mut app.profiles.editor_state);
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(panel(&fl!("tui-panel-node"), false)),
        detail_area,
    );
}

fn tree_line(editor: &Editor, path: &[Seg], depth: usize) -> Line<'static> {
    let value = get(&editor.root, path);
    let mut spans = vec![Span::raw("  ".repeat(depth))];
    let container = value.is_some_and(is_container);
    let open = editor.expanded.contains(path);
    spans.push(Span::styled(
        match (container, open) {
            (true, true) => "▾ ",
            (true, false) => "▸ ",
            _ => "  ",
        },
        Style::new().fg(DIM),
    ));
    let highlight = editor.is_match(path);
    match path.last() {
        Some(Seg::Key(key)) => spans.push(Span::styled(
            key.clone(),
            match (highlight, container) {
                (true, _) => Style::new().fg(theme::CRUST).bg(YELLOW),
                (false, true) => Style::new().fg(TEXT).bold(),
                (false, false) => Style::new().fg(TEXT),
            },
        )),
        Some(Seg::Index(i)) => spans.push(Span::styled(format!("#{i}"), Style::new().fg(DIM))),
        None => {}
    }
    let Some(value) = value else {
        return Line::from(spans);
    };
    spans.push(dim("  "));
    match value {
        Value::Object(map) => {
            if let Some(label) =
                item_label(value).filter(|_| matches!(path.last(), Some(Seg::Index(_))))
            {
                spans.push(Span::styled(label, Style::new().fg(ACCENT)));
                spans.push(Span::raw(" "));
            }
            if !open {
                spans.push(dim(format!("{{{}}}", map.len())));
            }
        }
        Value::Array(items) => {
            if !open {
                let preview: Vec<String> = items
                    .iter()
                    .take(6)
                    .filter_map(|item| match item {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => item_label(item),
                    })
                    .collect();
                spans.push(dim(format!("[{}]", items.len())));
                if !preview.is_empty() {
                    spans.push(Span::styled(
                        format!(" {}", truncate(&preview.join(", "), 60)),
                        Style::new().fg(SUBTEXT),
                    ));
                }
            }
        }
        scalar => spans.push(scalar_span(scalar, highlight)),
    }
    Line::from(spans)
}

fn scalar_span(value: &Value, highlight: bool) -> Span<'static> {
    let (text, color) = match value {
        Value::String(s) => (format!("\"{}\"", truncate(s, 72)), GREEN),
        Value::Number(n) => (n.to_string(), PEACH),
        Value::Bool(b) => (b.to_string(), BLUE),
        _ => ("null".to_owned(), DIM),
    };
    if highlight {
        Span::styled(text, Style::new().fg(theme::CRUST).bg(YELLOW))
    } else {
        Span::styled(text, Style::new().fg(color))
    }
}

fn detail_lines(editor: &Editor, path: &[Seg]) -> Vec<Line<'static>> {
    let Some(value) = get(&editor.root, path) else {
        return vec![Line::from(dim(fl!("tui-editor-empty")))];
    };
    let kind = match value {
        Value::Object(map) => fl!("node-object", count = map.len()),
        Value::Array(items) => fl!("node-array", count = items.len()),
        Value::String(_) => fl!("node-string"),
        Value::Number(_) => fl!("node-number"),
        Value::Bool(_) => fl!("node-bool"),
        Value::Null => "null".to_owned(),
    };
    let mut lines = vec![
        Line::from(Span::styled(
            if path.is_empty() {
                "/".to_owned()
            } else {
                display_path(path)
            },
            Style::new().fg(ACCENT).bold(),
        )),
        Line::from(dim(kind)),
    ];
    if let Some(label) = item_label(value).filter(|_| value.is_object()) {
        lines.push(Line::from(Span::styled(label, Style::new().fg(TEXT))));
    }
    if templates::references(&editor.root, path).is_some() && !is_container(value) {
        lines.push(Line::from(Span::styled(
            fl!("tui-reference-hint"),
            Style::new().fg(SKY),
        )));
    }
    lines.push(Line::raw(""));
    let text = match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    let style = match value {
        Value::String(_) => Style::new().fg(GREEN),
        _ => Style::new().fg(SUBTEXT),
    };
    lines.extend(
        text.lines()
            .take(400)
            .map(|line| Line::from(Span::styled(line.to_owned(), style))),
    );
    lines
}

/// Footer hints for the tab.
pub fn hints(app: &App) -> Vec<(&'static str, String)> {
    if app.profiles.editor.is_some() {
        return vec![
            ("↑↓←→", fl!("key-navigate")),
            ("⏎", fl!("key-edit")),
            ("a/A", fl!("key-add")),
            ("d", fl!("key-delete")),
            ("K/J", fl!("key-reorder")),
            ("u", fl!("key-undo")),
            ("/", fl!("key-search")),
            ("s", fl!("key-save")),
            ("q", fl!("key-close-editor")),
            ("?", fl!("key-help")),
        ];
    }
    vec![
        ("⏎", fl!("key-actions")),
        ("e/E", fl!("key-edit")),
        ("n", fl!("key-new")),
        ("i", fl!("key-import")),
        ("f/F", fl!("key-update-profile")),
        ("d", fl!("key-delete")),
    ]
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::client::DaemonClient;
    use crate::protocol::ProfileUsage;

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

    fn profile(id: &str, name: &str, active: bool, url: Option<&str>) -> Profile {
        Profile {
            id: id.into(),
            name: name.into(),
            url: url.map(Into::into),
            interval: 720,
            created_at: 1,
            updated_at: now_unix() - 7200,
            fetched_at: None,
            last_error: None,
            usage: None,
            size: 4321,
            active,
        }
    }

    fn press(app: &mut App, codes: &[KeyCode]) {
        for code in codes {
            app.on_key(KeyEvent::from(*code));
        }
    }

    #[tokio::test]
    async fn tab_menu_and_editor_render() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut app, _tasks) = App::new(DaemonClient::new("/nonexistent/socket".into()), tx);
        app.tab = Tab::Profiles;
        app.profiles.requested = true;
        let mut remote = profile(
            "bbbb",
            "Remote",
            false,
            Some("https://example.com/s?token=x"),
        );
        remote.usage = Some(ProfileUsage {
            upload: 1 << 30,
            download: 2 << 30,
            total: 100 << 30,
            expire: Some(1_798_761_600),
        });
        remote.last_error = Some("example.com answered 404".into());
        let home = profile("aaaa", "Home", true, None);
        app.on_event(AppEvent::Profiles(Ok(ProfileList {
            profiles: vec![home.clone(), remote],
            slot: "/etc/sing-box/config.json".into(),
            unmanaged: true,
        })));
        let content = profile::to_text(&profile::template("s3cret"));
        app.on_event(AppEvent::ProfileContent {
            purpose: ContentPurpose::Preview,
            result: Ok((home.clone(), content.clone())),
        });
        let list = screen(&mut app, 110, 30);
        assert!(list.contains("Home") && list.contains("Remote"), "{list}");
        assert!(list.contains("mixed 127.0.0.1:7890"), "{list}");
        assert!(list.contains("/etc/sing-box/config.json"), "{list}");
        assert!(!list.contains("token=x"), "{list}");

        press(&mut app, &[KeyCode::Down, KeyCode::Enter]);
        assert!(matches!(app.popup, Some(Popup::Menu(_))));
        screen(&mut app, 50, 12);
        app.popup = None;

        app.on_event(AppEvent::ProfileContent {
            purpose: ContentPurpose::Edit,
            result: Ok((home, content)),
        });
        // outbounds › 0 (the selector) › outbounds
        press(
            &mut app,
            &[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Right],
        );
        press(&mut app, &[KeyCode::Down, KeyCode::Right]);
        press(
            &mut app,
            &[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Right],
        );
        let editor = screen(&mut app, 110, 30);
        assert!(editor.contains("proxy (selector)"), "{editor}");
        // `a` on the open member list offers the other outbounds' tags.
        press(&mut app, &[KeyCode::Char('a')]);
        let Some(Popup::Menu(menu)) = &app.popup else {
            panic!("no menu");
        };
        assert_eq!(menu.items[0].label, "direct");
        press(&mut app, &[KeyCode::Enter]);
        assert!(app.popup.is_none());
        let members = &app.profiles.editor.as_ref().unwrap().root["outbounds"][0]["outbounds"];
        assert_eq!(members, &serde_json::json!(["direct", "direct"]));
        // Enter on a reference offers the tags; picking one sets it.
        press(&mut app, &[KeyCode::Enter]);
        let Some(Popup::Menu(menu)) = &app.popup else {
            panic!("no tag menu");
        };
        assert_eq!(menu.items.last().unwrap().label, fl!("tui-type-value"));
        app.popup = None;
        press(&mut app, &[KeyCode::Char('u'), KeyCode::Char('?')]);
        screen(&mut app, 40, 10);
        app.popup = None;
        // Global keys do not reach sing-box while the editor is open.
        press(&mut app, &[KeyCode::Char('x')]);
        assert!(app.popup.is_none());
        press(&mut app, &[KeyCode::Char('q')]);
        assert!(app.profiles.editor.is_none());
    }
}
