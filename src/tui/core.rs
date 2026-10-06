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
use super::popup::{Input, InputPurpose};
use super::theme::{
    self, ACCENT, BLUE, GREEN, MARK, PEACH, SUBTEXT, TEXT, YELLOW, chip, dim, panel, pill, selected,
};
use crate::i18n::fl;
use crate::protocol::{
    Checksum, CoreRelease, CoreReleasePage, CoreSource, Request, StoredCore, core_store_id,
    variant_label,
};
use crate::util::{error_chain, fmt_bytes};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoreFocus {
    Sources,
    #[default]
    Releases,
    Installed,
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
            let result = client.core_sources().await.map_err(|e| error_chain(&e));
            let _ = tx.send(AppEvent::CoreSources(result));
        });
    }

    pub(super) fn core_load_installed(&mut self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.core_installed().await.map_err(|e| error_chain(&e));
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
                .map_err(|e| error_chain(&e));
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
                self.popup = Some(Popup::Input(
                    Input::new(
                        fl!("tui-add-source-title"),
                        fl!("tui-add-source-hint"),
                        InputPurpose::AddSource,
                    )
                    .placeholder("owner/repo"),
                ))
            }
            KeyCode::Char('I') => {
                self.popup = Some(Popup::Input(
                    Input::new(
                        fl!("tui-import-title"),
                        fl!("tui-import-hint"),
                        InputPurpose::ImportCore,
                    )
                    .placeholder("/path/to/sing-box  [sha256]"),
                ))
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
                fl!(
                    "tui-no-build-for",
                    version = release.version.clone(),
                    platform = self.core.platform.clone()
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
                fl!("tui-already-active", version = release.version.clone()),
                false,
            );
            return;
        }
        let version = if release.prerelease {
            fl!("version-prerelease", version = release.version.clone())
        } else {
            release.version.clone()
        };
        let checksum = variant.checksum.label();
        let mut message = [
            if activate {
                fl!("tui-confirm-switch", version = version)
            } else {
                fl!("tui-confirm-download", version = version)
            },
            fl!(
                "tui-detail-build",
                source = source_name,
                variant = variant_label(&variant.name)
            ),
            match stored {
                Some(_) => fl!("tui-detail-stored", checksum = checksum),
                None => fl!(
                    "tui-detail-download",
                    size = fmt_bytes(variant.size),
                    checksum = checksum
                ),
            },
        ]
        .join("\n");
        if activate {
            message.push('\n');
            message.push_str(&fl!("tui-switch-note"));
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
                fl!("tui-already-active", version = core.version.clone()),
                false,
            );
            return;
        }
        self.popup = Some(Popup::Confirm {
            message: [
                fl!("tui-confirm-switch", version = core.version.clone()),
                fl!(
                    "tui-detail-build",
                    source = core.source_label(),
                    variant = variant_label(&core.variant)
                ),
                fl!("tui-switch-note"),
            ]
            .join("\n"),
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
            self.notify(fl!("tui-active-not-deletable"), true);
            return;
        }
        self.popup = Some(Popup::Confirm {
            message: [
                fl!("tui-confirm-delete", version = core.version.clone()),
                fl!(
                    "tui-detail-build",
                    source = core.source_label(),
                    variant = variant_label(&core.variant)
                ),
                fl!("tui-detail-frees", size = fmt_bytes(core.size)),
            ]
            .join("\n"),
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
            self.notify(fl!("tui-builtin-source", name = source.name.clone()), true);
            return;
        }
        self.popup = Some(Popup::Confirm {
            message: format!(
                "{}\n{}",
                fl!("tui-confirm-remove-source", source = source.id.clone()),
                fl!("tui-remove-source-note")
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
                &fl!("busy-adding-source"),
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
                    &fl!("busy-importing-core"),
                    Request::CoreImport {
                        location,
                        sha256,
                        activate: false,
                    },
                );
            }
            _ => {}
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
                &if activate {
                    fl!("busy-switching-core")
                } else {
                    fl!("busy-downloading-core")
                },
                Request::CoreInstall {
                    source,
                    tag,
                    variant,
                    activate,
                    force: false,
                },
            ),
            PendingAction::CoreActivate { id } => self.daemon_action(
                &fl!("busy-switching-core"),
                Request::CoreActivate { id, force: false },
            ),
            PendingAction::CoreRemove { id } => {
                self.daemon_action(&fl!("busy-deleting-core"), Request::CoreRemove { id })
            }
            PendingAction::CoreSourceRemove { id } => self.daemon_action(
                &fl!("busy-removing-source"),
                Request::CoreSourceRemove { id },
            ),
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

fn checksum_chip(checksum: Checksum) -> Span<'static> {
    match checksum {
        Checksum::None => chip(format!("⚠ {}", checksum.label()), YELLOW),
        verified => chip(format!("✓ {}", verified.label()), GREEN),
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
    let block = panel(&fl!("tui-panel-active-core"), false);
    let status = app.status.as_deref();
    let state = status.map(|s| s.state);
    let lines = match status.and_then(|s| s.active_core.as_ref()) {
        Some(core) => vec![
            Line::from(vec![
                state.map(theme::state_pill).unwrap_or_else(|| dim("")),
                Span::raw("  sing-box "),
                Span::styled(core.version.clone(), Style::new().fg(TEXT).bold()),
                Span::raw("   "),
                Span::styled(core.source_label(), Style::new().fg(ACCENT).bold()),
                Span::raw("  "),
                chip(variant_label(&core.variant), BLUE),
                Span::raw(" "),
                checksum_chip(core.checksum),
            ]),
            Line::from(vec![
                Span::styled(core.id.clone(), Style::new().fg(SUBTEXT)),
                Span::raw("   "),
                dim({
                    let mut details = vec![
                        fmt_bytes(core.size),
                        fl!("tui-installed-on", date = local_date(core.installed_at)),
                    ];
                    if core.files.len() > 2 {
                        details.push(fl!("tui-file-count", count = core.files.len()));
                    }
                    details.push(fl!(
                        "tui-linked-at",
                        path = status.map_or(String::new(), |s| s.binary.clone())
                    ));
                    details.join(&fl!("clause-separator"))
                }),
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
                    chip(fl!("tui-unmanaged"), YELLOW),
                ]),
                Line::from(dim(fl!(
                    "tui-unmanaged-note",
                    binary = s.binary.clone(),
                    name = fl!("source-adopted")
                ))),
            ],
            Some(_) => vec![
                Line::from(vec![
                    pill(fl!("tui-no-core"), theme::RED),
                    Span::raw("  "),
                    Span::raw(fl!("ctl-core-none")),
                ]),
                Line::from(dim(fl!("tui-no-core-hint"))),
            ],
            None => vec![Line::from(dim(app
                .status_error
                .clone()
                .unwrap_or_else(|| fl!("tui-waiting-daemon"))))],
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
                title.push(chip(fl!("source-custom-tag"), PEACH));
            }
            ListItem::new(vec![
                Line::from(title),
                Line::from(dim(format!("  {}", source.id))),
                Line::from(dim(format!("  {}", source.description))),
            ])
        })
        .collect();
    let total = items.len() * 3;
    let list = List::new(items)
        .block(panel(&fl!("tui-panel-sources"), focused))
        .highlight_style(selected(focused))
        .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(list, area, &mut app.core.source_state);
    theme::scrollbar(
        frame,
        area,
        (
            total,
            app.core.source_state.offset() * 3,
            usize::from(area.height.saturating_sub(2)),
        ),
        focused,
    );
}

fn draw_releases(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.core.focus == CoreFocus::Releases;
    let source = app.core.releases_for.clone().unwrap_or_default();
    let title = if source.is_empty() {
        fl!("tui-panel-releases-empty")
    } else if app.core.stable_only {
        fl!(
            "tui-panel-releases-stable",
            source = source.clone(),
            platform = app.core.platform.clone()
        )
    } else {
        fl!(
            "tui-panel-releases",
            source = source.clone(),
            platform = app.core.platform.clone()
        )
    };
    let mut block = panel(&title, focused);
    let visible_count = app.core.visible().len();
    let shown = if app.core.has_more {
        fl!("tui-releases-shown-more", count = visible_count)
    } else {
        fl!("tui-releases-shown", count = visible_count)
    };
    block = block.title_top(Line::from(dim(format!(" {shown} "))).right_aligned());

    if let Some(release) = app.core.selected_release() {
        let chosen = app.core.variant_index(release);
        let mut spans = vec![dim(format!(" {} ", fl!("tui-variants")))];
        if release.variants.is_empty() {
            spans.push(dim(format!("{} ", fl!("no-build-for-platform"))));
        }
        for (i, variant) in release.variants.iter().enumerate() {
            let label = variant_label(&variant.name);
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
            spans.push(dim(format!(" v {} ", fl!("key-next"))));
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
                    "{} {}",
                    theme::SPINNER[app.frame % theme::SPINNER.len()],
                    fl!("tui-loading-releases")
                ),
                Style::new().fg(YELLOW),
            ))
        } else {
            Line::from(dim(fl!("tui-no-releases")))
        };
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: true }).block(block),
            area,
        );
        return;
    }

    let viewport = usize::from(block.inner(area).height.saturating_sub(1));
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
                    Span::styled(
                        format!("  {}", fl!("prerelease-marker")),
                        Style::new().fg(YELLOW),
                    )
                } else {
                    Span::raw("")
                },
            ]);
            let variant_cell = match variant {
                Some(v) => Line::from(vec![
                    Span::styled(variant_label(&v.name), Style::new().fg(BLUE)),
                    if release.variants.len() > 1 {
                        dim(format!(" +{}", release.variants.len() - 1))
                    } else {
                        Span::raw("")
                    },
                ]),
                None => Line::from(dim("-")),
            };
            let state = match (stored, variant) {
                (Some(core), _) if core.active => Span::styled(
                    format!("● {}", fl!("tui-active")),
                    Style::new().fg(GREEN).bold(),
                ),
                (Some(_), _) => {
                    Span::styled(format!("✓ {}", fl!("tui-stored")), Style::new().fg(BLUE))
                }
                (None, Some(v)) => dim(fmt_bytes(v.size)),
                (None, None) => dim(fl!("tui-no-build")),
            };
            Row::new(vec![
                Cell::from(version),
                Cell::from(dim(date(release.published_at.as_deref()))),
                Cell::from(variant_cell),
                Cell::from(state),
            ])
        })
        .collect();
    let total = rows.len();
    let table = Table::new(
        rows,
        [
            Constraint::Fill(3),
            Constraint::Length(11),
            Constraint::Fill(2),
            Constraint::Length(11),
        ],
    )
    .header(
        Row::new([
            fl!("col-version"),
            fl!("col-published"),
            fl!("col-variant"),
            fl!("col-state"),
        ])
        .style(theme::header_row()),
    )
    .block(block)
    .row_highlight_style(selected(focused))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, area, &mut app.core.release_state);
    theme::scrollbar(
        frame,
        area,
        (total, app.core.release_state.offset(), viewport),
        focused,
    );
}

fn draw_installed(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.core.focus == CoreFocus::Installed;
    let total: u64 = app.core.installed.iter().map(|c| c.size).sum();
    let block = panel(
        &fl!("tui-panel-installed", count = app.core.installed.len()),
        focused,
    )
    .title_top(
        Line::from(dim(format!(
            " {} ",
            fl!("tui-on-disk", size = fmt_bytes(total))
        )))
        .right_aligned(),
    );
    if app.core.installed.is_empty() {
        frame.render_widget(
            Paragraph::new(dim(fl!("tui-store-empty")))
                .wrap(Wrap { trim: true })
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
                Cell::from(Span::styled(core.source_label(), Style::new().fg(ACCENT))),
                Cell::from(Span::styled(
                    variant_label(&core.variant),
                    Style::new().fg(BLUE),
                )),
                Cell::from(dim(fmt_bytes(core.size))),
                Cell::from(match core.checksum {
                    Checksum::None => Span::styled(
                        format!("⚠ {}", core.checksum.label()),
                        Style::new().fg(YELLOW),
                    ),
                    verified => {
                        Span::styled(format!("✓ {}", verified.label()), Style::new().fg(GREEN))
                    }
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
            String::new(),
            fl!("col-version"),
            fl!("col-source"),
            fl!("col-variant"),
            fl!("col-size"),
            fl!("col-checksum"),
            fl!("col-installed"),
        ])
        .style(theme::header_row()),
    )
    .block(block)
    .row_highlight_style(selected(focused))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, area, &mut app.core.installed_state);
    theme::scrollbar(
        frame,
        area,
        (
            app.core.installed.len(),
            app.core.installed_state.offset(),
            usize::from(area.height.saturating_sub(3)),
        ),
        focused,
    );
}

/// Footer hints for the Core tab, depending on the focused pane.
pub fn hints(app: &App) -> Vec<(&'static str, String)> {
    match app.core.focus {
        CoreFocus::Sources => vec![
            ("←→", fl!("key-pane")),
            ("↑↓", fl!("key-source")),
            ("a", fl!("key-add")),
            ("d", fl!("key-remove")),
            ("I", fl!("key-import")),
        ],
        CoreFocus::Releases => vec![
            ("←→", fl!("key-pane")),
            ("⏎", fl!("key-switch")),
            ("v", fl!("key-variant")),
            ("i", fl!("key-download")),
            ("p", fl!("key-stable")),
            ("n", fl!("key-more")),
            ("f", fl!("key-refresh")),
        ],
        CoreFocus::Installed => vec![
            ("←→", fl!("key-pane")),
            ("⏎", fl!("key-switch")),
            ("d", fl!("key-delete")),
            ("I", fl!("key-import")),
        ],
    }
}
