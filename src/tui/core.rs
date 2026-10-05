//! The "Core" tab: release sources, the full release list of a source with
//! per-release build variants, and the local version store.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap};

use super::app::{App, AppEvent, PendingAction, Popup};
use super::theme::{
    self, ACCENT, BLUE, GREEN, MARK, PEACH, SUBTEXT, TEXT, YELLOW, chip, dim, panel, pill, selected,
};
use crate::protocol::{
    Checksum, CoreRelease, CoreReleasePage, CoreSource, Request, StoredCore, core_store_id,
};
use crate::util::fmt_bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoreFocus {
    Sources,
    #[default]
    Releases,
    Installed,
}

/// What a free-text input popup is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPurpose {
    AddSource,
    ImportCore,
}

#[derive(Default)]
pub struct CoreView {
    pub requested: bool,
    pub sources: Vec<CoreSource>,
    pub default_source: String,
    pub source_state: ListState,
    /// Source the release list belongs to.
    pub releases_for: Option<String>,
    pub releases: Vec<CoreRelease>,
    pub platform: String,
    pub page: u32,
    pub has_more: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub release_state: TableState,
    /// Chosen variant index per release tag.
    pub variant_pick: HashMap<String, usize>,
    pub installed: Vec<StoredCore>,
    pub installed_state: TableState,
    pub focus: CoreFocus,
    pub stable_only: bool,
}

impl CoreView {
    pub fn selected_source(&self) -> Option<&CoreSource> {
        self.source_state
            .selected()
            .and_then(|i| self.sources.get(i))
    }

    /// Releases after the pre-release filter.
    pub fn visible(&self) -> Vec<&CoreRelease> {
        self.releases
            .iter()
            .filter(|r| !self.stable_only || !r.prerelease)
            .collect()
    }

    pub fn selected_release(&self) -> Option<&CoreRelease> {
        self.release_state
            .selected()
            .and_then(|i| self.visible().get(i).copied())
    }

    fn active(&self) -> Option<&StoredCore> {
        self.installed.iter().find(|c| c.active)
    }

    /// The variant index shown for a release: the user's pick, else the
    /// active core's variant when this release has it, else the plain build.
    pub fn variant_index(&self, release: &CoreRelease) -> usize {
        if let Some(index) = self.variant_pick.get(&release.tag) {
            return (*index).min(release.variants.len().saturating_sub(1));
        }
        self.active()
            .and_then(|active| {
                release
                    .variants
                    .iter()
                    .position(|v| v.name == active.variant)
            })
            .unwrap_or(0)
    }

    fn stored(&self, source: &str, tag: &str, variant: &str) -> Option<&StoredCore> {
        let id = core_store_id(source, tag, variant);
        self.installed.iter().find(|c| c.id == id)
    }
}

impl App {
    pub(super) fn core_tab_opened(&mut self) {
        if !self.core.requested {
            self.core.requested = true;
            self.core_load_sources();
            self.core_load_installed();
        }
    }

    pub(super) fn core_load_sources(&mut self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.core_sources().await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::CoreSources(result));
        });
    }

    pub(super) fn core_load_installed(&mut self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.core_installed().await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::CoreInstalled(result));
        });
    }

    fn core_load_releases(&mut self, source: String, page: u32, refresh: bool) {
        self.core.loading = true;
        self.core.error = None;
        if page <= 1 {
            self.core.releases_for = Some(source.clone());
        }
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .core_releases(&source, page, refresh)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::CoreReleases {
                source,
                page,
                result,
            });
        });
    }

    pub(super) fn core_sources_loaded(
        &mut self,
        result: Result<(Vec<CoreSource>, String), String>,
    ) {
        let (sources, default) = match result {
            Ok(value) => value,
            Err(err) => {
                self.core.error = Some(err);
                return;
            }
        };
        let keep = self.core.selected_source().map(|s| s.id.clone());
        self.core.sources = sources;
        self.core.default_source = default.clone();
        let wanted = keep.unwrap_or(default);
        let index = self
            .core
            .sources
            .iter()
            .position(|s| s.id.eq_ignore_ascii_case(&wanted))
            .unwrap_or(0);
        self.core.source_state.select(Some(index));
        if self.core.releases_for.is_none()
            && let Some(source) = self.core.selected_source().map(|s| s.id.clone())
        {
            self.core_load_releases(source, 1, false);
        }
    }

    pub(super) fn core_releases_loaded(
        &mut self,
        source: String,
        page: u32,
        result: Result<CoreReleasePage, String>,
    ) {
        // Ignore answers for a source the user already moved away from.
        if self.core.releases_for.as_deref() != Some(source.as_str()) {
            return;
        }
        self.core.loading = false;
        match result {
            Ok(loaded) => {
                if page <= 1 {
                    self.core.releases = loaded.releases;
                    self.core.variant_pick.clear();
                    self.core.release_state.select(Some(0));
                } else {
                    self.core.releases.extend(loaded.releases);
                }
                self.core.platform = loaded.platform;
                self.core.page = loaded.page;
                self.core.has_more = loaded.has_more;
                let visible = self.core.visible().len();
                clamp(&mut self.core.release_state, visible);
            }
            Err(err) => self.core.error = Some(err),
        }
    }

    pub(super) fn core_installed_loaded(&mut self, result: Result<Vec<StoredCore>, String>) {
        match result {
            Ok(installed) => {
                self.core.installed = installed;
                clamp(&mut self.core.installed_state, self.core.installed.len());
                if self.core.installed_state.selected().is_none() && !self.core.installed.is_empty()
                {
                    self.core.installed_state.select(Some(0));
                }
            }
            Err(err) => self.core.error = Some(err),
        }
    }

    /// Re-reads the store and sources after an action finished.
    pub(super) fn core_refresh_after_action(&mut self) {
        if self.core.requested {
            self.core_load_installed();
            self.core_load_sources();
        }
    }

    pub(super) fn core_on_key(&mut self, key: KeyEvent) {
        let focus = self.core.focus;
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.core.focus = match focus {
                    CoreFocus::Sources => CoreFocus::Installed,
                    CoreFocus::Releases => CoreFocus::Sources,
                    CoreFocus::Installed => CoreFocus::Releases,
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.core.focus = match focus {
                    CoreFocus::Sources => CoreFocus::Releases,
                    CoreFocus::Releases => CoreFocus::Installed,
                    CoreFocus::Installed => CoreFocus::Sources,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.core_move(-1),
            KeyCode::Down | KeyCode::Char('j') => self.core_move(1),
            KeyCode::PageUp => self.core_move(-10),
            KeyCode::PageDown => self.core_move(10),
            KeyCode::Home | KeyCode::Char('g') => self.core_move(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.core_move(isize::MAX / 2),
            KeyCode::Enter => match focus {
                CoreFocus::Sources => self.core.focus = CoreFocus::Releases,
                CoreFocus::Releases => self.core_confirm_install(true),
                CoreFocus::Installed => self.core_confirm_activate(),
            },
            KeyCode::Char('v') => self.core_cycle_variant(1),
            KeyCode::Char('V') => self.core_cycle_variant(-1),
            KeyCode::Char('i') if focus == CoreFocus::Releases => self.core_confirm_install(false),
            KeyCode::Char('d') | KeyCode::Delete => match focus {
                CoreFocus::Installed => self.core_confirm_remove(),
                CoreFocus::Sources => self.core_confirm_remove_source(),
                CoreFocus::Releases => {}
            },
            KeyCode::Char('p') => {
                self.core.stable_only = !self.core.stable_only;
                let visible = self.core.visible().len();
                clamp(&mut self.core.release_state, visible);
            }
            KeyCode::Char('n') => {
                if self.core.has_more
                    && !self.core.loading
                    && let Some(source) = self.core.releases_for.clone()
                {
                    let page = self.core.page + 1;
                    self.core_load_releases(source, page, false);
                }
            }
            KeyCode::Char('f') => {
                if let Some(source) = self.core.selected_source().map(|s| s.id.clone()) {
                    self.core_load_releases(source, 1, true);
                }
                self.core_load_installed();
            }
            KeyCode::Char('a') => {
                self.popup = Some(Popup::Input {
                    title: "Add a core source".to_owned(),
                    hint: "GitHub repository publishing sing-box-<version>-linux-<arch>.tar.gz (root only)"
                        .to_owned(),
                    value: String::new(),
                    purpose: InputPurpose::AddSource,
                })
            }
            KeyCode::Char('I') => {
                self.popup = Some(Popup::Input {
                    title: "Import a custom core".to_owned(),
                    hint: "absolute path or http(s) URL of a binary, .tar.gz, .zip or .gz, optionally followed by its sha256 (root only)"
                        .to_owned(),
                    value: String::new(),
                    purpose: InputPurpose::ImportCore,
                })
            }
            _ => {}
        }
    }

    fn core_move(&mut self, delta: isize) {
        match self.core.focus {
            CoreFocus::Sources => {
                let before = self.core.source_state.selected();
                let len = self.core.sources.len();
                self.core.source_state.select(step(before, len, delta));
                if self.core.source_state.selected() != before
                    && let Some(source) = self.core.selected_source().map(|s| s.id.clone())
                {
                    self.core.releases.clear();
                    self.core_load_releases(source, 1, false);
                }
            }
            CoreFocus::Releases => {
                let len = self.core.visible().len();
                let next = step(self.core.release_state.selected(), len, delta);
                self.core.release_state.select(next);
            }
            CoreFocus::Installed => {
                let len = self.core.installed.len();
                let next = step(self.core.installed_state.selected(), len, delta);
                self.core.installed_state.select(next);
            }
        }
    }

    fn core_cycle_variant(&mut self, delta: isize) {
        let Some(release) = self.core.selected_release() else {
            return;
        };
        let count = release.variants.len();
        if count < 2 {
            return;
        }
        let current = self.core.variant_index(release) as isize;
        let next = (current + delta).rem_euclid(count as isize) as usize;
        let tag = release.tag.clone();
        self.core.variant_pick.insert(tag, next);
    }

    fn core_confirm_install(&mut self, activate: bool) {
        let Some(source) = self.core.releases_for.clone() else {
            return;
        };
        let Some(release) = self.core.selected_release() else {
            return;
        };
        let Some(variant) = release.variants.get(self.core.variant_index(release)) else {
            self.notify(
                format!(
                    "{} has no build for {}",
                    release.version, self.core.platform
                ),
                true,
            );
            return;
        };
        let source_name = self
            .core
            .sources
            .iter()
            .find(|s| s.id == source)
            .map_or(source.clone(), |s| s.name.clone());
        let stored = self.core.stored(&source, &release.tag, &variant.name);
        if stored.is_some_and(|c| c.active) {
            self.notify(
                format!("{} is already the active core", release.version),
                false,
            );
            return;
        }
        let download = match stored {
            Some(_) => "already stored".to_owned(),
            None => format!("downloads {}", fmt_bytes(variant.size)),
        };
        let mut message = format!(
            "{} sing-box {}{}\n{} · {} · {} · {}",
            if activate { "Switch to" } else { "Download" },
            release.version,
            if release.prerelease {
                " (pre-release)"
            } else {
                ""
            },
            source_name,
            variant_label(&variant.name),
            download,
            checksum_text(variant.checksum),
        );
        if activate {
            message.push_str(
                "\nYour configuration is checked first; sing-box restarts on the new core.",
            );
        }
        self.popup = Some(Popup::Confirm {
            message,
            action: PendingAction::CoreInstall {
                source,
                tag: release.tag.clone(),
                variant: variant.name.clone(),
                activate,
            },
        });
    }

    fn core_confirm_activate(&mut self) {
        let Some(core) = self
            .core
            .installed_state
            .selected()
            .and_then(|i| self.core.installed.get(i))
        else {
            return;
        };
        if core.active {
            self.notify(
                format!("{} is already the active core", core.version),
                false,
            );
            return;
        }
        self.popup = Some(Popup::Confirm {
            message: format!(
                "Switch to sing-box {}\n{} · {}\nYour configuration is checked first; sing-box restarts on the new core.",
                core.version,
                core.source_name,
                variant_label(&core.variant)
            ),
            action: PendingAction::CoreActivate {
                id: core.id.clone(),
            },
        });
    }

    fn core_confirm_remove(&mut self) {
        let Some(core) = self
            .core
            .installed_state
            .selected()
            .and_then(|i| self.core.installed.get(i))
        else {
            return;
        };
        if core.active {
            self.notify(
                "the active core cannot be deleted; switch first".to_owned(),
                true,
            );
            return;
        }
        self.popup = Some(Popup::Confirm {
            message: format!(
                "Delete stored core sing-box {}?\n{} · {} · frees {}",
                core.version,
                core.source_name,
                variant_label(&core.variant),
                fmt_bytes(core.size)
            ),
            action: PendingAction::CoreRemove {
                id: core.id.clone(),
            },
        });
    }

    fn core_confirm_remove_source(&mut self) {
        let Some(source) = self.core.selected_source() else {
            return;
        };
        if source.builtin {
            self.notify(
                format!("{} is built in and cannot be removed", source.name),
                true,
            );
            return;
        }
        self.popup = Some(Popup::Confirm {
            message: format!(
                "Remove core source {}?\nStored cores from it are kept.",
                source.id
            ),
            action: PendingAction::CoreSourceRemove {
                id: source.id.clone(),
            },
        });
    }

    pub(super) fn core_submit_input(&mut self, purpose: InputPurpose, value: String) {
        let value = value.trim().to_owned();
        if value.is_empty() {
            return;
        }
        match purpose {
            InputPurpose::AddSource => self.daemon_action(
                "adding core source",
                Request::CoreSourceAdd {
                    repo: value,
                    name: None,
                },
            ),
            InputPurpose::ImportCore => {
                let mut parts = value.split_whitespace();
                let location = parts.next().unwrap_or_default().to_owned();
                let sha256 = parts.next().map(str::to_owned);
                self.daemon_action(
                    "importing core",
                    Request::CoreImport {
                        location,
                        sha256,
                        activate: false,
                    },
                );
            }
        }
    }

    pub(super) fn core_run_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::CoreInstall {
                source,
                tag,
                variant,
                activate,
            } => self.daemon_action(
                if activate {
                    "switching core"
                } else {
                    "downloading core"
                },
                Request::CoreInstall {
                    source,
                    tag,
                    variant,
                    activate,
                    force: false,
                },
            ),
            PendingAction::CoreActivate { id } => {
                self.daemon_action("switching core", Request::CoreActivate { id, force: false })
            }
            PendingAction::CoreRemove { id } => {
                self.daemon_action("deleting core", Request::CoreRemove { id })
            }
            PendingAction::CoreSourceRemove { id } => {
                self.daemon_action("removing source", Request::CoreSourceRemove { id })
            }
            _ => {}
        }
    }
}

fn step(selected: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = selected.unwrap_or(0) as isize;
    Some(current.saturating_add(delta).clamp(0, len as isize - 1) as usize)
}

fn clamp(state: &mut TableState, len: usize) {
    match state.selected() {
        Some(i) if i >= len => state.select(len.checked_sub(1)),
        None if len > 0 => state.select(Some(0)),
        _ => {}
    }
}

pub fn variant_label(variant: &str) -> &str {
    if variant.is_empty() {
        "default"
    } else {
        variant
    }
}

fn checksum_text(checksum: Checksum) -> &'static str {
    match checksum {
        Checksum::Sums => "SHA256SUMS",
        Checksum::Digest => "GitHub digest",
        Checksum::Pinned => "pinned sha256",
        Checksum::None => "unverified",
    }
}

fn checksum_chip(checksum: Checksum) -> Span<'static> {
    match checksum {
        Checksum::None => chip("⚠ unverified", YELLOW),
        other => chip(format!("✓ {}", checksum_text(other)), GREEN),
    }
}

fn date(text: Option<&str>) -> String {
    text.and_then(|t| t.get(..10)).unwrap_or("").to_owned()
}

fn local_date(unix: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(unix as i64, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

// ----- rendering -------------------------------------------------------------

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let installed_height = (app.core.installed.len() as u16 + 3).clamp(5, 10);
    let [active_area, middle, installed_area] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(8),
        Constraint::Length(installed_height),
    ])
    .areas(area);
    let [sources_area, releases_area] =
        Layout::horizontal([Constraint::Length(34), Constraint::Fill(1)]).areas(middle);

    draw_active(frame, active_area, app);
    draw_sources(frame, sources_area, app);
    draw_releases(frame, releases_area, app);
    draw_installed(frame, installed_area, app);
}

fn draw_active(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Active core", false);
    let status = app.status.as_deref();
    let state = status.map(|s| s.state);
    let lines = match status.and_then(|s| s.active_core.as_ref()) {
        Some(core) => vec![
            Line::from(vec![
                state.map(theme::state_pill).unwrap_or_else(|| dim("")),
                Span::raw("  sing-box "),
                Span::styled(core.version.clone(), Style::new().fg(TEXT).bold()),
                Span::raw("   "),
                Span::styled(core.source_name.clone(), Style::new().fg(ACCENT).bold()),
                dim("  ·  "),
                chip(variant_label(&core.variant), BLUE),
                Span::raw(" "),
                checksum_chip(core.checksum),
            ]),
            Line::from(vec![
                Span::styled(core.id.clone(), Style::new().fg(SUBTEXT)),
                dim(format!(
                    "  ·  {}  ·  installed {}{}  ·  linked at {}",
                    fmt_bytes(core.size),
                    local_date(core.installed_at),
                    if core.files.len() > 2 {
                        format!("  ·  {} files", core.files.len())
                    } else {
                        String::new()
                    },
                    status.map_or("", |s| s.binary.as_str())
                )),
            ]),
        ],
        None => match status {
            Some(s) if s.core_version.is_some() => vec![
                Line::from(vec![
                    state.map(theme::state_pill).unwrap_or_else(|| dim("")),
                    Span::raw("  sing-box "),
                    Span::styled(
                        s.core_version.clone().unwrap_or_default(),
                        Style::new().bold(),
                    ),
                    Span::raw("   "),
                    chip("unmanaged", YELLOW),
                ]),
                Line::from(dim(format!(
                    "{} is a plain file; switching keeps it in the store as \"Previously installed\"",
                    s.binary
                ))),
            ],
            Some(_) => vec![
                Line::from(vec![
                    pill("NO CORE", theme::RED),
                    Span::raw("  no sing-box core installed"),
                ]),
                Line::from(dim(
                    "pick a release below and press Enter to install and switch to it",
                )),
            ],
            None => vec![Line::from(dim(app
                .status_error
                .clone()
                .unwrap_or_else(|| "waiting for daemon…".to_owned())))],
        },
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_sources(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.core.focus == CoreFocus::Sources;
    let active_source = app.core.active().map(|c| c.source.clone());
    let items: Vec<ListItem> = app
        .core
        .sources
        .iter()
        .map(|source| {
            let in_use = active_source.as_deref() == Some(source.id.as_str());
            let mut title = vec![
                Span::styled(if in_use { "● " } else { "  " }, Style::new().fg(GREEN)),
                Span::styled(source.name.clone(), Style::new().bold()),
            ];
            if !source.builtin {
                title.push(Span::raw(" "));
                title.push(chip("custom", PEACH));
            }
            ListItem::new(vec![
                Line::from(title),
                Line::from(dim(format!("  {}", source.id))),
                Line::from(dim(format!("  {}", source.description))),
            ])
        })
        .collect();
    let list = List::new(items)
        .block(panel("Sources", focused))
        .highlight_style(selected(focused))
        .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(list, area, &mut app.core.source_state);
}

fn draw_releases(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.core.focus == CoreFocus::Releases;
    let source = app.core.releases_for.clone().unwrap_or_default();
    let filter = if app.core.stable_only {
        " · stable only"
    } else {
        ""
    };
    let mut block = panel(
        &format!("Releases · {source} · {}{filter}", app.core.platform),
        focused,
    );
    let visible_count = app.core.visible().len();
    let more = if app.core.has_more { " · n more" } else { "" };
    block =
        block.title_top(Line::from(dim(format!(" {visible_count} shown{more} "))).right_aligned());

    if let Some(release) = app.core.selected_release() {
        let chosen = app.core.variant_index(release);
        let mut spans = vec![dim(" variants ")];
        if release.variants.is_empty() {
            spans.push(dim("none for this platform "));
        }
        for (i, variant) in release.variants.iter().enumerate() {
            let label = variant_label(&variant.name).to_owned();
            spans.push(if i == chosen {
                Span::styled(
                    format!(" {label} "),
                    Style::new().fg(theme::CRUST).bg(BLUE).bold(),
                )
            } else {
                Span::styled(format!(" {label} "), Style::new().fg(SUBTEXT))
            });
        }
        if release.variants.len() > 1 {
            spans.push(dim(" v next "));
        }
        block = block.title_bottom(Line::from(spans));
    }

    if app.core.releases.is_empty() {
        let text = if let Some(err) = &app.core.error {
            Line::from(Span::styled(
                err.lines().next().unwrap_or_default().to_owned(),
                Style::new().fg(theme::RED),
            ))
        } else if app.core.loading {
            Line::from(Span::styled(
                format!(
                    "{} loading releases…",
                    theme::SPINNER[app.frame % theme::SPINNER.len()]
                ),
                Style::new().fg(YELLOW),
            ))
        } else {
            Line::from(dim("no releases"))
        };
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: true }).block(block),
            area,
        );
        return;
    }

    let rows: Vec<Row> = app
        .core
        .visible()
        .into_iter()
        .map(|release| {
            let chosen = app.core.variant_index(release);
            let variant = release.variants.get(chosen);
            let stored = variant.and_then(|v| app.core.stored(&source, &release.tag, &v.name));
            let version = Line::from(vec![
                Span::raw(release.version.clone()),
                if release.prerelease {
                    Span::styled("  pre", Style::new().fg(YELLOW))
                } else {
                    Span::raw("")
                },
            ]);
            let variant_cell = match variant {
                Some(v) => Line::from(vec![
                    Span::styled(variant_label(&v.name).to_owned(), Style::new().fg(BLUE)),
                    if release.variants.len() > 1 {
                        dim(format!(" +{}", release.variants.len() - 1))
                    } else {
                        Span::raw("")
                    },
                ]),
                None => Line::from(dim("—")),
            };
            let state = match (stored, variant) {
                (Some(core), _) if core.active => {
                    Span::styled("● active", Style::new().fg(GREEN).bold())
                }
                (Some(_), _) => Span::styled("✓ stored", Style::new().fg(BLUE)),
                (None, Some(v)) => dim(fmt_bytes(v.size)),
                (None, None) => dim("no build"),
            };
            Row::new(vec![
                Cell::from(version),
                Cell::from(dim(date(release.published_at.as_deref()))),
                Cell::from(variant_cell),
                Cell::from(state),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Fill(3),
            Constraint::Length(11),
            Constraint::Fill(2),
            Constraint::Length(11),
        ],
    )
    .header(Row::new(["Version", "Published", "Variant", "State"]).style(theme::header_row()))
    .block(block)
    .row_highlight_style(selected(focused))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, area, &mut app.core.release_state);
}

fn draw_installed(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.core.focus == CoreFocus::Installed;
    let total: u64 = app.core.installed.iter().map(|c| c.size).sum();
    let block = panel(
        &format!("Installed · {}", app.core.installed.len()),
        focused,
    )
    .title_top(Line::from(dim(format!(" {} on disk ", fmt_bytes(total)))).right_aligned());
    if app.core.installed.is_empty() {
        frame.render_widget(
            Paragraph::new(dim(
                "nothing stored yet — releases you switch to or download (i) appear here",
            ))
            .block(block),
            area,
        );
        return;
    }
    let rows: Vec<Row> = app
        .core
        .installed
        .iter()
        .map(|core| {
            Row::new(vec![
                Cell::from(Span::styled(
                    if core.active { "●" } else { " " },
                    Style::new().fg(GREEN),
                )),
                Cell::from(Span::styled(
                    core.version.clone(),
                    if core.active {
                        Style::new().fg(GREEN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new()
                    },
                )),
                Cell::from(Span::styled(
                    core.source_name.clone(),
                    Style::new().fg(ACCENT),
                )),
                Cell::from(Span::styled(
                    variant_label(&core.variant).to_owned(),
                    Style::new().fg(BLUE),
                )),
                Cell::from(dim(fmt_bytes(core.size))),
                Cell::from(match core.checksum {
                    Checksum::None => Span::styled("⚠ unverified", Style::new().fg(YELLOW)),
                    other => Span::styled(
                        format!("✓ {}", checksum_text(other)),
                        Style::new().fg(GREEN),
                    ),
                }),
                Cell::from(dim(local_date(core.installed_at))),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(2),
            Constraint::Fill(2),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(16),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new([
            "",
            "Version",
            "Source",
            "Variant",
            "Size",
            "Checksum",
            "Installed",
        ])
        .style(theme::header_row()),
    )
    .block(block)
    .row_highlight_style(selected(focused))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, area, &mut app.core.installed_state);
}

/// Footer hints for the Core tab, depending on the focused pane.
pub fn hints(app: &App) -> &'static [(&'static str, &'static str)] {
    match app.core.focus {
        CoreFocus::Sources => &[
            ("←→", "pane"),
            ("↑↓", "source"),
            ("a", "add"),
            ("d", "remove"),
            ("I", "import"),
        ],
        CoreFocus::Releases => &[
            ("←→", "pane"),
            ("⏎", "switch"),
            ("v", "variant"),
            ("i", "download"),
            ("p", "stable"),
            ("n", "more"),
            ("f", "refresh"),
        ],
        CoreFocus::Installed => &[
            ("←→", "pane"),
            ("⏎", "switch"),
            ("d", "delete"),
            ("I", "import"),
        ],
    }
}
