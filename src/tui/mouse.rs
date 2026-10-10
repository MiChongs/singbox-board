//! Mouse input. Drawing records what each spot of the screen does
//! ([`Hits`]); a click looks up what is under it on the frame the user
//! sees and does what the keys would: tabs and key hints are buttons, a
//! click on a row selects it and a double click opens it, the wheel
//! scrolls and the scrollbars can be dragged. The text editors handle
//! clicks in their text themselves.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::TitlePosition;

use super::app::{App, Focus, Popup, StoreFocus, Tab, move_table};
use super::connections::SortKey;
use super::core::CoreFocus;
use super::popup::InputPurpose;
use crate::i18n::fl;
use crate::protocol::Component;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rows a notch of the wheel moves through a list of one-line rows.
const WHEEL_ROWS: isize = 3;

/// A pane with a list (or the log) the mouse can pick from and scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Groups,
    Members,
    Components,
    Entries,
    Connections,
    Sources,
    Releases,
    Installed,
    Profiles,
    Containers,
    /// The tree view of the profile editor.
    Tree,
    /// The entries of a menu dialog.
    Menu,
    Logs,
}

/// What a spot of the screen does when clicked.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Tab(Tab),
    /// Presses a key: the hints of the footer and of dialogs.
    Key(KeyEvent),
    /// Focuses a pane, then presses a key: the hints on a pane's border.
    PaneKey(Pane, KeyEvent),
    /// A click focuses the pane, the wheel scrolls it.
    Pane(Pane),
    /// An item of a list: a click selects it, a double click opens it.
    Row(Pane, usize),
    /// The scrollbar of a pane, along `track`, over `len` positions.
    Scrollbar {
        pane: Pane,
        track: Rect,
        len: usize,
    },
    /// Sorts the connections by a column; again reverses the order.
    Sort(SortKey),
    /// A build variant of the selected release.
    Variant(usize),
    /// The fold marker of a row of the tree view.
    Fold(usize),
    /// A dialog: a click inside keeps it open, one beside it closes it.
    Dialog,
    /// The text field of a dialog, whose text starts at this column.
    Field(u16),
}

/// The clickable spots of the frame on screen, recorded while drawing.
#[derive(Default)]
pub struct Hits(RefCell<Vec<(Rect, Target)>>);

impl Hits {
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    pub fn add(&self, area: Rect, target: Target) {
        if !area.is_empty() {
            self.0.borrow_mut().push((area, target));
        }
    }

    /// What was drawn last at the spot, i.e. what is on top.
    pub fn at(&self, column: u16, row: u16) -> Option<Target> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find(|(area, _)| area.contains(Position::new(column, row)))
            .map(|(_, target)| target.clone())
    }

    /// Records the items of a list in the bordered `area`: `len` items
    /// from `offset`, `height` rows each, drawn in `rows` (the inside
    /// below the header). A scrollbar on the right border jumps.
    pub fn rows(
        &self,
        pane: Pane,
        area: Rect,
        rows: Rect,
        (len, offset, height): (usize, usize, u16),
    ) {
        let height = height.max(1);
        let mut y = rows.y;
        for index in offset..len {
            if y.saturating_add(height) > rows.bottom() {
                break;
            }
            self.add(
                Rect::new(area.x + 1, y, area.width.saturating_sub(2), height),
                Target::Row(pane, index),
            );
            y += height;
        }
        if len > usize::from(rows.height / height) {
            self.scrollbar(pane, area, len);
        }
    }

    /// Records the scrollbar on the right border of the bordered `area`.
    pub fn scrollbar(&self, pane: Pane, area: Rect, len: usize) {
        let track = Rect::new(
            area.right().saturating_sub(1),
            area.y + 1,
            1,
            area.height.saturating_sub(2),
        );
        self.add(track, Target::Scrollbar { pane, track, len });
    }
}

/// The area `rows` rows below the top of `area`: the rows of a table
/// under its header.
pub fn below(area: Rect, rows: u16) -> Rect {
    let rows = rows.min(area.height);
    Rect {
        y: area.y + rows,
        height: area.height - rows,
        ..area
    }
}

/// A line whose parts do something when clicked.
#[derive(Default)]
pub struct Parts {
    spans: Vec<Span<'static>>,
    width: u16,
    /// Offset, width and target of each clickable part.
    hits: Vec<(u16, u16, Target)>,
}

impl Parts {
    pub fn text(&mut self, span: Span<'static>) {
        self.width = self.width.saturating_add(span.width() as u16);
        self.spans.push(span);
    }

    pub fn button(
        &mut self,
        spans: impl IntoIterator<Item = Span<'static>>,
        target: Option<Target>,
    ) {
        let start = self.width;
        for span in spans {
            self.text(span);
        }
        if let Some(target) = target {
            self.hits.push((start, self.width - start, target));
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn line(&self) -> Line<'static> {
        Line::from(self.spans.clone())
    }

    /// Records the clickable parts of the line drawn from `x`, `y`,
    /// as far as they are inside `clip`.
    pub fn register(&self, hits: &Hits, x: u16, y: u16, clip: Rect) {
        for (offset, width, target) in &self.hits {
            let area = Rect::new(x.saturating_add(*offset), y, *width, 1).intersection(clip);
            hits.add(area, target.clone());
        }
    }

    /// Records the line drawn as the only title of its alignment on the
    /// top or bottom border of `area`, where ratatui puts it.
    pub fn title(&self, hits: &Hits, area: Rect, position: TitlePosition, alignment: Alignment) {
        let y = match position {
            TitlePosition::Top => area.y,
            TitlePosition::Bottom => area.bottom().saturating_sub(1),
        };
        let clip = Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1);
        let x = match alignment {
            Alignment::Left => clip.x,
            Alignment::Center => clip.x + clip.width.saturating_sub(self.width) / 2,
            Alignment::Right => clip.right().saturating_sub(self.width).max(clip.x),
        };
        self.register(hits, x, y, clip);
    }
}

/// The key a hint stands for: `⏎`, `Esc`, `^S`, `F2`, `t`, the first of
/// `e/E`. Arrows only move and are not buttons.
pub fn hint_key(text: &str) -> Option<KeyEvent> {
    let first = match text {
        "/" | "^/" => text,
        _ => text.split('/').next()?,
    };
    let plain = |code| Some(KeyEvent::new(code, KeyModifiers::NONE));
    match first {
        "⏎" => plain(KeyCode::Enter),
        "Esc" => plain(KeyCode::Esc),
        "Tab" => plain(KeyCode::Tab),
        "Home" => plain(KeyCode::Home),
        "End" => plain(KeyCode::End),
        "PgUp" => plain(KeyCode::PageUp),
        "PgDn" => plain(KeyCode::PageDown),
        _ => {
            if let Some(n) = first.strip_prefix('F').and_then(|n| n.parse().ok()) {
                return plain(KeyCode::F(n));
            }
            let (ctrl, key) = match first.strip_prefix('^') {
                Some(key) => (true, key),
                None => (false, first),
            };
            let mut chars = key.chars();
            let c = chars.next()?;
            if chars.next().is_some() || "←→↑↓".contains(c) {
                return None;
            }
            Some(if ctrl {
                KeyEvent::new(KeyCode::Char(c.to_ascii_lowercase()), KeyModifiers::CONTROL)
            } else if c.is_uppercase() {
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
            } else {
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
            })
        }
    }
}

/// Where the next click counts as a double click.
pub struct Click {
    at: Instant,
    target: Target,
}

fn enter() -> KeyEvent {
    KeyEvent::from(KeyCode::Enter)
}

impl App {
    pub fn on_mouse(&mut self, event: MouseEvent) {
        let target = self.hits.at(event.column, event.row);
        match event.kind {
            MouseEventKind::Drag(MouseButton::Left) if self.dragging.is_some() => {
                if let Some(Target::Scrollbar { pane, track, len }) = self.dragging.clone() {
                    self.scroll_to(pane, track, len, event.row);
                }
                return;
            }
            MouseEventKind::Up(_) => self.dragging = None,
            _ => {}
        }
        if self.popup.is_some() {
            self.popup_mouse(event, target);
            return;
        }
        // The editors take what happens in their text; the tabs and the
        // key hints around them stay buttons.
        let chrome = matches!(target, Some(Target::Tab(_) | Target::Key(_)));
        if self.code_active() && !chrome {
            self.code_on_mouse(event);
            return;
        }
        if self.toml_active() && !chrome {
            self.toml_on_mouse(event);
            return;
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(event, target, false),
            MouseEventKind::Down(MouseButton::Right) => self.click(event, target, true),
            MouseEventKind::ScrollUp => self.wheel(target, -1),
            MouseEventKind::ScrollDown => self.wheel(target, 1),
            _ => {}
        }
        self.tab_opened();
    }

    /// Mouse reporting is on unless switched off for the terminal's own
    /// text selection.
    pub fn wants_mouse(&self) -> bool {
        self.mouse
    }

    pub(super) fn toggle_mouse(&mut self) {
        self.mouse = !self.mouse;
        let message = if self.mouse {
            fl!("tui-mouse-on")
        } else {
            fl!("tui-mouse-off")
        };
        self.notify(message, false);
    }

    /// A left click (or a right click, `context`, which opens the menu of
    /// a row instead of counting towards a double click).
    fn click(&mut self, event: MouseEvent, target: Option<Target>, context: bool) {
        let Some(target) = target else {
            self.last_click = None;
            return;
        };
        let double = !context
            && self
                .last_click
                .as_ref()
                .is_some_and(|last| last.target == target && last.at.elapsed() < DOUBLE_CLICK);
        self.last_click = (!double && !context).then(|| Click {
            at: Instant::now(),
            target: target.clone(),
        });
        match target {
            Target::Tab(tab) => self.tab = tab,
            Target::Key(key) => self.on_key(key),
            Target::PaneKey(pane, key) => {
                self.focus_pane(pane);
                self.on_key(key);
            }
            Target::Pane(pane) => self.focus_pane(pane),
            Target::Row(pane, index) => {
                self.select_row(pane, index);
                let menu = matches!(pane, Pane::Components | Pane::Profiles | Pane::Containers);
                if double || (context && menu) {
                    self.on_key(enter());
                }
            }
            Target::Scrollbar { pane, track, len } => {
                self.dragging = Some(target.clone());
                self.scroll_to(pane, track, len, event.row);
            }
            Target::Sort(key) => self.connections.sort_by(key),
            Target::Variant(index) => self.core_pick_variant(index),
            Target::Fold(index) => {
                self.select_row(Pane::Tree, index);
                if let Some(editor) = &mut self.profiles.editor {
                    editor.toggle();
                }
            }
            Target::Dialog | Target::Field(_) => {}
        }
    }

    fn wheel(&mut self, target: Option<Target>, direction: isize) {
        let pane = match target {
            Some(Target::Pane(pane) | Target::Row(pane, _) | Target::Scrollbar { pane, .. }) => {
                pane
            }
            Some(Target::Fold(_)) => Pane::Tree,
            _ => return,
        };
        // Lists of tall items move one item a notch.
        let rows = if matches!(pane, Pane::Groups | Pane::Sources) {
            1
        } else {
            WHEEL_ROWS
        };
        self.move_pane(pane, direction * rows);
    }

    fn popup_mouse(&mut self, event: MouseEvent, target: Option<Target>) {
        let direction = match event.kind {
            MouseEventKind::ScrollUp => -1,
            MouseEventKind::ScrollDown => 1,
            MouseEventKind::Down(MouseButton::Left) => 0,
            _ => return,
        };
        if direction != 0 {
            self.move_pane(Pane::Menu, direction);
            return;
        }
        match target {
            Some(Target::Key(key)) => self.on_key(key),
            // A menu entry is chosen with a single click.
            Some(Target::Row(Pane::Menu, index)) => {
                self.select_row(Pane::Menu, index);
                self.on_key(enter());
            }
            Some(Target::Field(start)) => {
                if let Some(Popup::Input(input)) = &mut self.popup {
                    input.click(event.column.saturating_sub(start));
                }
            }
            Some(target @ Target::Scrollbar { pane, track, len }) => {
                self.dragging = Some(target);
                self.scroll_to(pane, track, len, event.row);
            }
            // "Press any key to close."
            Some(Target::Dialog)
                if matches!(
                    self.popup,
                    Some(Popup::Help | Popup::EditorHelp | Popup::CodeHelp)
                ) =>
            {
                self.popup = None
            }
            Some(Target::Dialog) => {}
            // Beside the dialog.
            _ => match &self.popup {
                // The first-run questions want an answer.
                Some(Popup::Setup { .. }) => {}
                // The filter applies as it is typed; keep what is shown.
                Some(Popup::Input(input))
                    if matches!(input.purpose, InputPurpose::ConnectionFilter(_)) =>
                {
                    self.on_key(enter())
                }
                _ => self.on_key(KeyEvent::from(KeyCode::Esc)),
            },
        }
    }

    fn focus_pane(&mut self, pane: Pane) {
        match pane {
            Pane::Groups => self.focus = Focus::Groups,
            Pane::Members if !self.members().is_empty() => {
                self.focus = Focus::Members;
                if self.member_state.selected().is_none() {
                    self.member_state.select(Some(0));
                }
            }
            Pane::Components => self.store_focus = StoreFocus::Components,
            Pane::Entries
                if self
                    .sub_store
                    .as_ref()
                    .is_some_and(|o| !o.entries.is_empty()) =>
            {
                self.store_focus = StoreFocus::Entries
            }
            Pane::Sources => self.core.focus = CoreFocus::Sources,
            Pane::Releases => self.core.focus = CoreFocus::Releases,
            Pane::Installed => self.core.focus = CoreFocus::Installed,
            _ => {}
        }
    }

    fn selected_in(&self, pane: Pane) -> Option<usize> {
        match pane {
            Pane::Groups => self.group_state.selected(),
            Pane::Members => self.member_state.selected(),
            Pane::Components => self.comp_state.selected(),
            Pane::Entries => self.entry_state.selected(),
            Pane::Connections => self.connections.state.selected(),
            Pane::Sources => self.core.source_state.selected(),
            Pane::Releases => self.core.release_state.selected(),
            Pane::Installed => self.core.installed_state.selected(),
            Pane::Profiles => self.profiles.state.selected(),
            Pane::Containers => self.containers.state.selected(),
            Pane::Tree => self.profiles.editor.as_ref().map(|e| e.cursor),
            Pane::Menu => match &self.popup {
                Some(Popup::Menu(menu)) => Some(menu.selected),
                _ => None,
            },
            Pane::Logs => None,
        }
    }

    fn select_row(&mut self, pane: Pane, index: usize) {
        let current = self.selected_in(pane).unwrap_or(0);
        self.move_pane(pane, index as isize - current as isize);
    }

    /// Focuses the pane and moves its cursor, as the arrow keys would.
    fn move_pane(&mut self, pane: Pane, delta: isize) {
        match pane {
            Pane::Groups | Pane::Members => {
                self.focus_pane(pane);
                if (pane == Pane::Groups) == (self.focus == Focus::Groups) {
                    self.move_proxy_cursor(delta);
                }
            }
            Pane::Components => {
                self.store_focus = StoreFocus::Components;
                move_table(&mut self.comp_state, Component::ALL.len(), delta);
            }
            Pane::Entries => {
                let len = self.sub_store.as_ref().map_or(0, |o| o.entries.len());
                if len > 0 {
                    self.store_focus = StoreFocus::Entries;
                    move_table(&mut self.entry_state, len, delta);
                }
            }
            Pane::Connections => {
                let len = self.connections.rows.len();
                move_table(&mut self.connections.state, len, delta);
            }
            Pane::Sources | Pane::Releases | Pane::Installed => {
                self.focus_pane(pane);
                self.core_move(delta);
            }
            Pane::Profiles => self.profiles_move(self.profiles.profiles().len(), delta),
            Pane::Containers => {
                let len = self.containers.containers().len();
                move_table(&mut self.containers.state, len, delta);
            }
            Pane::Tree => {
                if let Some(editor) = &mut self.profiles.editor {
                    editor.move_cursor(delta);
                }
            }
            Pane::Menu => {
                if let Some(Popup::Menu(menu)) = &mut self.popup {
                    let last = menu.items.len().saturating_sub(1) as isize;
                    menu.selected = (menu.selected as isize + delta).clamp(0, last) as usize;
                }
            }
            // Up the wheel goes back in time.
            Pane::Logs => {
                let max = self.logs.len().saturating_sub(1) as isize;
                self.log_scroll = (self.log_scroll as isize - delta).clamp(0, max) as usize;
            }
        }
    }

    /// Jumps to where `row` is along the scrollbar `track` of `len`
    /// positions.
    fn scroll_to(&mut self, pane: Pane, track: Rect, len: usize, row: u16) {
        if len == 0 {
            return;
        }
        let span = usize::from(track.height.saturating_sub(1));
        let at = usize::from(
            row.min(track.bottom().saturating_sub(1))
                .saturating_sub(track.y),
        );
        let index = (at * (len - 1) + span / 2)
            .checked_div(span)
            .unwrap_or(0)
            .min(len - 1);
        match pane {
            // The log's positions count from the oldest line.
            Pane::Logs => self.log_scroll = len - 1 - index,
            pane => self.select_row(pane, index),
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::app::AppEvent;
    use super::*;
    use crate::clash::{Connection, Proxies, Proxy};
    use crate::client::DaemonClient;
    use crate::i18n::Lang;
    use crate::protocol::{LogEntry, LogSource, Profile, ProfileList, Status};

    const WIDTH: u16 = 120;
    const HEIGHT: u16 = 40;

    fn app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(DaemonClient::new("/nonexistent/socket".into()), tx).0
    }

    /// Draws the app and returns the screen, one string per row, in which
    /// the width of the text before a character is its column.
    fn draw(app: &mut App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        terminal
            .draw(|frame| super::super::ui::draw(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..HEIGHT)
            .map(|y| {
                let mut line = String::new();
                let mut x = 0;
                while x < WIDTH {
                    let symbol = buffer[(x, y)].symbol();
                    line.push_str(symbol);
                    // A wide character hides the cell after it.
                    x += crate::util::text_width(symbol).max(1) as u16;
                }
                line
            })
            .collect()
    }

    /// The cell where `text` first appears on a row containing `with`,
    /// from column `from` on.
    fn find_with(screen: &[String], text: &str, with: &str, from: u16) -> (u16, u16) {
        for (y, line) in screen.iter().enumerate() {
            if !line.contains(with) {
                continue;
            }
            let mut start = 0;
            while let Some(byte) = line[start..].find(text).map(|b| b + start) {
                let x = crate::util::text_width(&line[..byte]) as u16;
                if x >= from {
                    return (x, y as u16);
                }
                start = byte + text.len();
            }
        }
        panic!("{text:?} is not on the screen:\n{}", screen.join("\n"));
    }

    fn find(screen: &[String], text: &str) -> (u16, u16) {
        find_with(screen, text, "", 0)
    }

    fn mouse(app: &mut App, kind: MouseEventKind, (column, row): (u16, u16)) {
        app.on_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    fn click(app: &mut App, at: (u16, u16)) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), at);
        mouse(app, MouseEventKind::Up(MouseButton::Left), at);
    }

    /// Draws, then clicks where `text` first is.
    fn click_on(app: &mut App, text: &str) {
        let screen = draw(app);
        click(app, find(&screen, text));
    }

    fn profile(id: &str, name: &str) -> Profile {
        Profile {
            id: id.into(),
            name: name.into(),
            url: None,
            interval: 0,
            created_at: 1,
            updated_at: 1,
            fetched_at: None,
            last_error: None,
            usage: None,
            size: 1,
            active: false,
        }
    }

    /// Runs a test in every language: the text, and so the layout, differ.
    fn in_every_language(test: impl Fn()) {
        for lang in Lang::ALL {
            crate::i18n::in_language(lang, &test);
        }
    }

    fn running_with_clash_api() -> Box<Status> {
        serde_json::from_value(serde_json::json!({
            "daemon_version": "0.1.9",
            "daemon_pid": 1,
            "daemon_started_at": 0,
            "state": "running",
            "restarts": 0,
            "binary": "/usr/local/bin/sing-box",
            "args": [],
            "clash_api": { "url": "http://127.0.0.1:9090" },
            "update_in_progress": false
        }))
        .unwrap()
    }

    #[test]
    fn hints_name_their_keys() {
        let key = |code, modifiers| Some(KeyEvent::new(code, modifiers));
        let none = KeyModifiers::NONE;
        assert_eq!(hint_key("⏎"), key(KeyCode::Enter, none));
        assert_eq!(hint_key("e/E"), key(KeyCode::Char('e'), none));
        assert_eq!(hint_key("/"), key(KeyCode::Char('/'), none));
        assert_eq!(hint_key("D"), key(KeyCode::Char('D'), KeyModifiers::SHIFT));
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(hint_key("^Z/^Y"), key(KeyCode::Char('z'), ctrl));
        assert_eq!(hint_key("^/"), key(KeyCode::Char('/'), ctrl));
        assert_eq!(hint_key("q/F2"), key(KeyCode::Char('q'), none));
        assert_eq!(hint_key("F10"), key(KeyCode::F(10), none));
        assert_eq!(hint_key("PgUp/Dn"), key(KeyCode::PageUp, none));
        assert_eq!(hint_key("←→"), None);
        assert_eq!(hint_key("↑↓←→"), None);
    }

    #[tokio::test]
    async fn tabs_hints_and_dialogs() {
        in_every_language(|| {
            let mut app = app();
            // The footer's hints press their keys.
            click_on(&mut app, &fl!("key-help"));
            assert!(matches!(app.popup, Some(Popup::Help)));
            click_on(&mut app, &fl!("help-title"));
            assert!(app.popup.is_none());

            click_on(&mut app, &fl!("key-stop"));
            assert!(matches!(app.popup, Some(Popup::Confirm { .. })));
            // Hints behind a dialog are covered by it.
            let screen = draw(&mut app);
            click(&mut app, find(&screen, &fl!("key-restart")));
            assert!(app.popup.is_none());
            click_on(&mut app, &fl!("key-restart"));
            assert!(matches!(app.popup, Some(Popup::Confirm { .. })));
            click_on(&mut app, &fl!("key-cancel"));
            assert!(app.popup.is_none());

            click_on(&mut app, &Tab::Logs.title());
            assert_eq!(app.tab, Tab::Logs);
        });
    }

    #[tokio::test]
    async fn proxy_groups_and_nodes() {
        in_every_language(|| {
            let mut app = app();
            app.tab = Tab::Proxies;
            let group = |members: &[&str]| Proxy {
                kind: "Selector".into(),
                now: Some(members[0].into()),
                all: Some(members.iter().map(|m| m.to_string()).collect()),
                ..Proxy::default()
            };
            let mut proxies = Proxies::default();
            proxies.proxies.insert("proxy".into(), group(&["hk", "jp"]));
            proxies.proxies.insert("ai".into(), group(&["jp", "us"]));
            for name in ["hk", "jp", "us"] {
                let node = Proxy {
                    kind: "Shadowsocks".into(),
                    ..Proxy::default()
                };
                proxies.proxies.insert(name.into(), node);
            }
            app.on_event(AppEvent::Proxies(proxies));
            assert_eq!(app.selected_group(), Some("ai"));
            click_on(&mut app, "proxy");
            assert_eq!(app.selected_group(), Some("proxy"));
            assert_eq!(app.focus, Focus::Groups);

            // The members table, right of the groups.
            let screen = draw(&mut app);
            let jp = find_with(&screen, "jp", "Shadowsocks", WIDTH / 3);
            click(&mut app, jp);
            assert_eq!(app.focus, Focus::Members);
            assert_eq!(app.member_state.selected(), Some(1));
            // A double click selects the node: without a Clash API it says so.
            click(&mut app, jp);
            assert!(app.toast.as_ref().is_some_and(|t| t.error));

            // The wheel over the groups moves through them.
            let group = find_with(&screen, "proxy", "Selector", 0);
            mouse(&mut app, MouseEventKind::ScrollUp, group);
            assert_eq!(app.focus, Focus::Groups);
            assert_eq!(app.selected_group(), Some("ai"));
        });
    }

    #[tokio::test]
    async fn menus_open_and_choose() {
        in_every_language(|| {
            let mut app = app();
            app.tab = Tab::Profiles;
            app.profiles.requested = true;
            app.on_event(AppEvent::Profiles(Ok(ProfileList {
                profiles: vec![profile("aaaa", "Home"), profile("bbbb", "Work")],
                slot: "/etc/sing-box/config.json".into(),
                unmanaged: false,
            })));
            // A right click opens the menu of the row under it.
            let screen = draw(&mut app);
            let work = find(&screen, "Work");
            mouse(&mut app, MouseEventKind::Down(MouseButton::Right), work);
            assert_eq!(app.profiles.state.selected(), Some(1));
            assert!(matches!(app.popup, Some(Popup::Menu(_))));
            // The wheel moves through the entries, a click chooses one.
            mouse(&mut app, MouseEventKind::ScrollDown, work);
            let Some(Popup::Menu(menu)) = &app.popup else {
                panic!("no menu");
            };
            assert_eq!(menu.selected, 1);
            click_on(&mut app, &fl!("menu-profile-rename"));
            let Some(Popup::Input(input)) = &app.popup else {
                panic!("no text field");
            };
            assert_eq!(input.value, "Work");
            // A click in the field moves the cursor.
            let (x, y) = find(&draw(&mut app), "❯ Work");
            click(&mut app, (x + 4, y));
            let Some(Popup::Input(input)) = &app.popup else {
                panic!("no text field");
            };
            assert_eq!(input.cursor, 2);
            click_on(&mut app, &fl!("key-cancel"));
            assert!(app.popup.is_none());
            // A double click opens the menu too.
            let home = find(&draw(&mut app), "Home");
            click(&mut app, home);
            click(&mut app, home);
            assert_eq!(app.profiles.state.selected(), Some(0));
            assert!(matches!(app.popup, Some(Popup::Menu(_))));
        });
    }

    #[tokio::test]
    async fn connections_sort_by_their_headers() {
        in_every_language(|| {
            let mut app = app();
            app.tab = Tab::Connections;
            app.status = Some(running_with_clash_api());
            let connection = |id: &str, host: &str| {
                let mut c = Connection {
                    id: id.into(),
                    start: "2026-10-05T00:00:00Z".into(),
                    ..Connection::default()
                };
                c.metadata.host = host.into();
                c.metadata.network = "tcp".into();
                c.metadata.destination_port = "443".into();
                c
            };
            app.connections.take(vec![
                connection("a", "b.example"),
                connection("b", "a.example"),
            ]);
            let header = |app: &mut App| {
                let screen = draw(app);
                find_with(&screen, &fl!("col-destination"), &fl!("col-age"), 0)
            };
            let at = header(&mut app);
            click(&mut app, at);
            assert_eq!(app.connections.sort, SortKey::Host);
            assert!(!app.connections.reverse);
            let at = header(&mut app);
            click(&mut app, at);
            assert!(app.connections.reverse);
            // The second row, then the wheel back to the first.
            click_on(&mut app, "a.example");
            let selected = app.connections.selected().map(|c| c.id.clone());
            assert_eq!(selected.as_deref(), Some("b"));
            let screen = draw(&mut app);
            mouse(
                &mut app,
                MouseEventKind::ScrollUp,
                find(&screen, "a.example"),
            );
            assert_eq!(app.connections.state.selected(), Some(0));
        });
    }

    #[tokio::test]
    async fn the_log_scrolls_and_its_scrollbar_drags() {
        in_every_language(|| {
            let mut app = app();
            app.tab = Tab::Logs;
            for seq in 0..200 {
                app.on_event(AppEvent::Log(LogEntry {
                    seq,
                    ts: 0,
                    source: LogSource::Core,
                    line: format!("INFO[0000] line {seq}"),
                }));
            }
            let screen = draw(&mut app);
            let newest = find(&screen, "line 199");
            mouse(&mut app, MouseEventKind::ScrollUp, newest);
            assert_eq!(app.log_scroll, 3);
            mouse(&mut app, MouseEventKind::ScrollDown, newest);
            assert_eq!(app.log_scroll, 0);
            // Dragged up the scrollbar to the oldest lines.
            let right = WIDTH - 1;
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                (right, 30),
            );
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                (right, 0),
            );
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (right, 0));
            assert!(app.log_scroll > 0);
            let screen = draw(&mut app);
            assert!(screen.iter().any(|line| line.contains("line 0 ")));
            // Back to the newest by the badge that says it is scrolled.
            let badge = fl!("tui-logs-scrolled", lines = app.log_scroll);
            click(&mut app, find(&screen, &badge));
            assert_eq!(app.log_scroll, 0);
        });
    }

    #[tokio::test]
    async fn core_sources_releases_and_variants() {
        use crate::protocol::{Checksum, CoreRelease, CoreSource, CoreVariant, variant_label};
        in_every_language(|| {
            let mut app = app();
            app.tab = Tab::Core;
            app.core.requested = true;
            app.core.sources = ["MiChongs/sing-box", "example/fork"]
                .map(|id| CoreSource {
                    id: id.into(),
                    name: id.into(),
                    description: String::new(),
                    builtin: false,
                })
                .to_vec();
            app.core.source_state.select(Some(0));
            app.core.releases_for = Some("MiChongs/sing-box".into());
            let release = |version: &str| CoreRelease {
                tag: format!("v{version}"),
                version: version.into(),
                published_at: None,
                prerelease: false,
                variants: ["", "glibc"]
                    .map(|name| CoreVariant {
                        name: name.into(),
                        asset: String::new(),
                        size: 1,
                        checksum: Checksum::None,
                    })
                    .to_vec(),
            };
            app.core.releases = vec![release("1.12.0"), release("1.11.0")];
            app.core.release_state.select(Some(0));

            // A variant on the border picks it, `v` there goes on to the next.
            click_on(&mut app, "glibc");
            let picked = |app: &App| {
                let release = app.core.selected_release().unwrap();
                app.core.variant_index(release)
            };
            assert_eq!(picked(&app), 1);
            click_on(&mut app, &format!("v {}", fl!("key-next")));
            assert_eq!(picked(&app), 0);
            assert!(
                draw(&mut app)
                    .iter()
                    .any(|line| line.contains(&format!(" {} ", variant_label(""))))
            );

            // A double click on a release asks to switch to it.
            let screen = draw(&mut app);
            let older = find(&screen, "1.11.0");
            click(&mut app, older);
            assert_eq!(app.core.release_state.selected(), Some(1));
            click(&mut app, older);
            assert!(matches!(app.popup, Some(Popup::Confirm { .. })));
            app.popup = None;

            // Sources take three rows each.
            let screen = draw(&mut app);
            let (x, y) = find(&screen, "example/fork");
            click(&mut app, (x, y + 1));
            assert_eq!(app.core.focus, CoreFocus::Sources);
            assert_eq!(app.core.source_state.selected(), Some(1));
        });
    }

    #[tokio::test]
    async fn the_tree_view_folds_where_clicked() {
        use super::super::profiles::ContentPurpose;
        in_every_language(|| {
            let mut app = app();
            app.tab = Tab::Profiles;
            app.profiles.requested = true;
            let content = crate::profile::to_text(&crate::profile::template("s3cret"));
            app.on_event(AppEvent::ProfileContent {
                purpose: ContentPurpose::Edit,
                result: Ok((profile("aaaa", "Home"), content)),
            });
            app.on_key(KeyEvent::from(KeyCode::F(2)));
            assert!(app.profiles.editor.is_some());

            // The name selects the row, the marker opens it.
            click_on(&mut app, "outbounds");
            let editor = app.profiles.editor.as_ref().unwrap();
            let outbounds = editor.cursor;
            assert!(
                !draw(&mut app)
                    .iter()
                    .any(|line| line.contains("▾ outbounds"))
            );
            click_on(&mut app, "▸ outbounds");
            assert!(
                draw(&mut app)
                    .iter()
                    .any(|line| line.contains("▾ outbounds"))
            );
            click_on(&mut app, "▾ outbounds");
            assert!(
                draw(&mut app)
                    .iter()
                    .any(|line| line.contains("▸ outbounds"))
            );
            assert_eq!(app.profiles.editor.as_ref().unwrap().cursor, outbounds);
            // The wheel moves the cursor.
            let at = find(&draw(&mut app), "▸ outbounds");
            mouse(&mut app, MouseEventKind::ScrollUp, at);
            assert_eq!(app.profiles.editor.as_ref().unwrap().cursor, outbounds - 3);
        });
    }

    #[tokio::test]
    async fn the_editor_takes_clicks_in_its_text() {
        in_every_language(|| {
            let mut app = app();
            app.tab = Tab::Profiles;
            app.profiles.requested = true;
            let text = "{\n  \"log\": {\"level\": \"info\"}\n}\n";
            app.open_code_editor(profile("aaaa", "Home"), text);
            let screen = draw(&mut app);
            let (x, y) = find(&screen, "\"level\"");
            click(&mut app, (x + 1, y));
            let cursor = app.profiles.code.as_ref().unwrap().text.cursor();
            assert_eq!((cursor.line, cursor.col), (1, 11));
            // The footer's hints are still buttons: F2 opens the tree view.
            click_on(&mut app, &fl!("key-tree"));
            assert!(app.profiles.editor.is_some());
            click_on(&mut app, &Tab::Logs.title());
            assert_eq!(app.tab, Tab::Logs);
        });
    }
}
