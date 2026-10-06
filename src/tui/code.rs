//! The built-in text editor for profiles: the document as text with
//! highlighting, validation while typing, find and replace, and the tree
//! view one key away. Key and mouse handling and drawing live here; the
//! text is kept by ratatui-textarea ([`Buffer`]) and analysed by
//! jsonc-parser ([`Outline`]).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use serde::Serialize;
use serde_json::Value;

use super::app::App;
use super::app::Popup;
use super::buffer::Buffer;
use super::editor::{Editor, Path, display_path};
use super::jsonc::{self, Analysis as Outline, Pos, Token};
use super::popup::{Input, InputPurpose, Menu, MenuAction, MenuItem};
use super::profiles::SavePurpose;
use super::theme::{
    ACCENT, BLUE, CRUST, CURRENT_LINE, DIM, GREEN, MATCH, PEACH, RED, SELECTION, SUBTEXT, SURFACE,
    SURFACE2, TEXT, YELLOW, dim, key,
};
use crate::i18n::fl;
use crate::profile;
use crate::protocol::{Profile, Request};
use crate::util::{error_chain, text_width};
use unicode_width::UnicodeWidthChar;

/// Tab stops of the display.
const TAB_WIDTH: usize = 4;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Lines kept visible above and below the cursor.
const SCROLL_MARGIN: usize = 3;
const WHEEL_LINES: usize = 3;
/// Columns of a text field in the find, replace and go-to bar.
const FIELD_WIDTH: usize = 32;

/// Editor commands, from keys and from the command menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeCommand {
    Save,
    SaveAndClose,
    /// Close without saving.
    Discard,
    Close,
    Format,
    Find,
    Replace,
    FindNext,
    FindPrevious,
    GoTo,
    GoToProblem,
    TreeView,
    ToggleComment,
    DuplicateLines,
    DeleteLines,
    Undo,
    Redo,
    SelectAll,
    Copy,
    Cut,
    Paste,
    Commands,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    Find,
    Replace,
    GoTo,
}

/// The find, replace or go-to line below the text.
pub struct Bar {
    pub kind: BarKind,
    pub query: Input,
    pub replace: Input,
    /// The replacement field has the keys.
    pub on_replace: bool,
    /// Where incremental search starts: the cursor when the bar opened.
    origin: Pos,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Error,
    Check,
    Warning,
}

/// What the status line reports, most important first.
struct Problem {
    pos: Option<Pos>,
    message: String,
    level: Level,
}

/// Matches of a search in one version of the text.
struct Matches {
    version: u64,
    query: String,
    case: bool,
    found: Vec<(Pos, Pos)>,
}

/// Derived from one version of the text.
struct Analysis {
    version: u64,
    outline: Outline,
    dirty: bool,
}

pub struct CodeEditor {
    pub profile: Profile,
    pub text: Buffer,
    /// The text as stored by the daemon.
    saved: String,
    analysis: Analysis,
    pub bar: Option<Bar>,
    /// The last search, for F3 and to fill the find bar.
    query: String,
    replacement: String,
    case: bool,
    matches: Option<Matches>,
    /// First visible line and terminal column.
    top: usize,
    left: usize,
    /// Scroll to the cursor on the next draw.
    follow: bool,
    /// The editor panel and the text inside it, as last drawn.
    panel: Rect,
    view: Rect,
    pub saving: bool,
    close_after_save: bool,
    /// Store even when sing-box rejects the active profile (`--force`).
    pub force: bool,
    /// What sing-box reported about the last save: the node and message.
    check: Option<(Path, String)>,
    last_click: Option<(Instant, Pos)>,
    dragging: bool,
    /// The document when the tree view opened.
    tree_base: Option<Value>,
}

impl CodeEditor {
    pub fn new(profile: Profile, content: &str) -> Self {
        let text = Buffer::new(content);
        let saved = text.text();
        Self {
            profile,
            text,
            saved,
            analysis: Analysis {
                version: u64::MAX,
                outline: Outline::default(),
                dirty: false,
            },
            bar: None,
            query: String::new(),
            replacement: String::new(),
            case: false,
            matches: None,
            top: 0,
            left: 0,
            follow: true,
            panel: Rect::default(),
            view: Rect::default(),
            saving: false,
            close_after_save: false,
            force: false,
            check: None,
            last_click: None,
            dragging: false,
            tree_base: None,
        }
    }

    /// Re-analyses the text after a change.
    fn refresh(&mut self) {
        let version = self.text.version();
        if self.analysis.version == version {
            return;
        }
        let text = self.text.text();
        self.analysis = Analysis {
            version,
            dirty: text != self.saved,
            outline: Outline::new(text),
        };
    }

    pub fn dirty(&mut self) -> bool {
        self.refresh();
        self.analysis.dirty
    }

    fn mark_saved(&mut self, saved: String) {
        self.saved = saved;
        self.analysis.version = u64::MAX;
    }

    /// The syntax error, else what sing-box said, else a duplicate key.
    fn problem(&mut self) -> Option<Problem> {
        self.refresh();
        let outline = &self.analysis.outline;
        if let Some(error) = &outline.error {
            return Some(Problem {
                pos: Some(error.pos),
                message: error.message.clone(),
                level: Level::Error,
            });
        }
        if let Some((path, message)) = &self.check {
            return Some(Problem {
                pos: outline
                    .find(path)
                    .map(|i| outline.pos(outline.nodes[i].first())),
                message: message.clone(),
                level: Level::Check,
            });
        }
        outline.warnings.first().map(|warning| Problem {
            pos: Some(warning.pos),
            message: warning.message.clone(),
            level: Level::Warning,
        })
    }

    /// Path of the node under the cursor.
    fn cursor_path(&mut self) -> Path {
        self.refresh();
        let outline = &self.analysis.outline;
        outline
            .node_at(self.text.cursor())
            .map(|i| outline.path(i))
            .unwrap_or_default()
    }

    /// Puts the cursor on a node, or selects all of it.
    fn reveal(&mut self, path: &[super::editor::Seg], select: bool) -> bool {
        self.refresh();
        let outline = &self.analysis.outline;
        let Some(index) = outline.find(path) else {
            return false;
        };
        let node = &outline.nodes[index];
        let (first, end) = (outline.pos(node.first()), outline.pos(node.end));
        if select {
            self.text.select(first, end);
        } else {
            self.text.set_cursor(first, false);
        }
        self.follow = true;
        true
    }

    fn jump(&mut self, pos: Pos) {
        self.text.set_cursor(pos, false);
        self.follow = true;
    }

    /// The node an error message names, with the part of the message from
    /// there on. `structured` skips bare words such as `log`.
    fn locate(&mut self, message: &str, structured: bool) -> Option<(Path, String)> {
        self.refresh();
        let outline = &self.analysis.outline;
        let (index, offset) = outline.locate(message, structured)?;
        let detail = message[offset..].lines().next().unwrap_or_default().trim();
        Some((outline.path(index), detail.to_owned()))
    }

    fn matches(&mut self) -> &[(Pos, Pos)] {
        let version = self.text.version();
        let fresh = self
            .matches
            .as_ref()
            .is_some_and(|m| m.version == version && m.query == self.query && m.case == self.case);
        if !fresh {
            let found = self.text.matches(&self.query, self.case);
            self.matches = Some(Matches {
                version,
                query: self.query.clone(),
                case: self.case,
                found,
            });
        }
        self.matches.as_ref().map_or(&[], |m| &m.found)
    }

    /// Selects the first match from where the bar opened (typing in the
    /// find field).
    fn search_incremental(&mut self) {
        let Some(origin) = self.bar.as_ref().map(|b| b.origin) else {
            return;
        };
        let query = self.query.clone();
        let found = self.text.find_from(&query, self.case, origin, false);
        match found {
            Some((start, end)) => self.text.select(start, end),
            None => self.text.set_cursor(origin, false),
        }
        if let Some(bar) = &mut self.bar {
            bar.error = (found.is_none() && !query.is_empty()).then(|| fl!("tui-search-none"));
        }
        self.follow = true;
    }

    /// Selects the next (or previous) match.
    fn search_step(&mut self, backwards: bool) -> bool {
        let query = self.query.clone();
        match self.text.find(&query, self.case, backwards) {
            Some((start, end)) => {
                self.text.select(start, end);
                self.follow = true;
                true
            }
            None => false,
        }
    }

    fn open_bar(&mut self, kind: BarKind) {
        let selected = self
            .text
            .selected_text()
            .filter(|t| !t.is_empty() && !t.contains('\n'));
        let origin = self
            .text
            .selection()
            .map_or(self.text.cursor(), |(start, _)| start);
        let query = match kind {
            BarKind::GoTo => String::new(),
            _ => selected.unwrap_or_else(|| self.query.clone()),
        };
        let field = |value: String| {
            Input::new(String::new(), String::new(), InputPurpose::Inline).value(value)
        };
        self.bar = Some(Bar {
            kind,
            query: field(query.clone()),
            replace: field(self.replacement.clone()),
            on_replace: false,
            origin,
            error: None,
        });
        if kind != BarKind::GoTo {
            self.query = query;
            self.search_incremental();
        }
    }

    /// `12`, `12:5` or a path such as `outbounds[0].server`.
    fn go_to(&mut self, target: &str) -> Result<(), String> {
        let target = target.trim();
        if target.is_empty() {
            return Err(fl!("tui-goto-empty"));
        }
        let mut parts = target.splitn(2, [':', ',']);
        if let Some(Ok(line)) = parts.next().map(|l| l.trim().parse::<usize>()) {
            let col = parts
                .next()
                .and_then(|c| c.trim().parse::<usize>().ok())
                .unwrap_or(1);
            self.jump(Pos::new(line.saturating_sub(1), col.saturating_sub(1)));
            return Ok(());
        }
        match jsonc::parse_path(target) {
            Some(path) if self.reveal(&path, false) => Ok(()),
            _ => Err(fl!("tui-goto-not-found", target = target.to_owned())),
        }
    }

    /// Keeps the cursor on screen after it moved.
    fn scroll_into_view(&mut self, height: usize, width: usize) {
        let count = self.text.lines().len();
        if self.follow && height > 0 && width > 0 {
            let cursor = self.text.cursor();
            let margin = SCROLL_MARGIN.min(height.saturating_sub(1) / 2);
            if cursor.line < self.top + margin {
                self.top = cursor.line.saturating_sub(margin);
            } else if cursor.line + margin >= self.top + height {
                self.top = cursor.line + margin + 1 - height;
            }
            let col = display_col(self.text.line(cursor.line), cursor.col);
            if col < self.left {
                self.left = col.saturating_sub(width / 3);
            } else if col >= self.left + width {
                self.left = (col + 1).saturating_sub(width * 2 / 3);
            }
        }
        self.top = self.top.min(count.saturating_sub(1));
    }

    /// The text position under a terminal cell; above or below the text
    /// it is a line outside the view, so dragging there scrolls.
    fn pos_at(&self, x: u16, y: u16) -> Pos {
        let view = self.view;
        let line = if y < view.y {
            self.top.saturating_sub(1)
        } else {
            self.top + usize::from(y - view.y)
        };
        let line = line.min(self.text.lines().len() - 1);
        let column = self.left + usize::from(x.saturating_sub(view.x));
        Pos::new(line, col_at_display(self.text.line(line), column))
    }

    /// The bracket under or before the cursor and its partner.
    fn bracket_pair(&self) -> Option<(Pos, Pos)> {
        let cursor = self.text.cursor();
        let before = cursor.col.checked_sub(1).map(|c| Pos::new(cursor.line, c));
        [Some(cursor), before]
            .into_iter()
            .flatten()
            .find_map(|at| self.analysis.outline.bracket_pair(at))
    }
}

/// Pretty JSON with a final newline, indented like the document.
pub fn pretty(value: &Value, indent: &str) -> String {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    if value.serialize(&mut serializer).is_err() {
        return profile::to_text(value);
    }
    let mut text = String::from_utf8(out).unwrap_or_default();
    text.push('\n');
    text
}

impl App {
    pub(super) fn open_code_editor(&mut self, profile: Profile, content: &str) {
        self.profiles.editor = None;
        self.profiles.code = Some(CodeEditor::new(profile, content));
    }

    /// The code editor is shown and takes the keys (not the tree view).
    pub(super) fn code_active(&self) -> bool {
        self.tab == super::app::Tab::Profiles
            && self.profiles.code.is_some()
            && self.profiles.editor.is_none()
    }

    // ----- keys -----------------------------------------------------------------

    pub(super) fn code_on_key(&mut self, key: KeyEvent) {
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        if code.bar.is_some() {
            self.code_bar_key(key);
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // AltGr arrives as Ctrl+Alt on some keyboards.
        let typed = ctrl == alt;
        let page = usize::from(code.view.height.max(2) - 1);
        code.follow = true;
        let text = &mut code.text;
        let command = match key.code {
            KeyCode::Char(c) if typed => {
                text.type_char(c);
                None
            }
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                's' => Some(CodeCommand::Save),
                'q' => Some(CodeCommand::Close),
                'z' if shift => Some(CodeCommand::Redo),
                'z' => Some(CodeCommand::Undo),
                'y' => Some(CodeCommand::Redo),
                'a' => Some(CodeCommand::SelectAll),
                'c' => Some(CodeCommand::Copy),
                'x' => Some(CodeCommand::Cut),
                'v' => Some(CodeCommand::Paste),
                'd' => Some(CodeCommand::DuplicateLines),
                'k' => Some(CodeCommand::DeleteLines),
                'f' => Some(CodeCommand::Find),
                'r' => Some(CodeCommand::Replace),
                'g' => Some(CodeCommand::GoTo),
                'p' => Some(CodeCommand::Commands),
                // Ctrl+/ arrives as Ctrl+7 in terminals without key reporting.
                '/' | '7' => Some(CodeCommand::ToggleComment),
                // Some terminals send Backspace as Ctrl+H.
                'h' => {
                    text.backspace();
                    None
                }
                _ => None,
            },
            KeyCode::Char('f' | 'F') if alt => Some(CodeCommand::Format),
            KeyCode::Enter => {
                text.newline();
                None
            }
            KeyCode::Tab => {
                text.tab();
                None
            }
            KeyCode::BackTab => {
                text.outdent_lines();
                None
            }
            KeyCode::Backspace if ctrl || alt => {
                text.delete_word_left();
                None
            }
            KeyCode::Backspace => {
                text.backspace();
                None
            }
            KeyCode::Delete if ctrl || alt => {
                text.delete_word_right();
                None
            }
            KeyCode::Delete => {
                text.delete();
                None
            }
            KeyCode::Left => {
                text.left(shift, ctrl || alt);
                None
            }
            KeyCode::Right => {
                text.right(shift, ctrl || alt);
                None
            }
            KeyCode::Up | KeyCode::Down if alt => {
                text.move_lines(if key.code == KeyCode::Up { -1 } else { 1 });
                None
            }
            KeyCode::Up | KeyCode::Down if ctrl => {
                // Scroll the view, not the cursor.
                code.follow = false;
                code.top = if key.code == KeyCode::Up {
                    code.top.saturating_sub(1)
                } else {
                    code.top + 1
                };
                None
            }
            KeyCode::Up => {
                text.vertical(-1, shift);
                None
            }
            KeyCode::Down => {
                text.vertical(1, shift);
                None
            }
            KeyCode::PageUp => {
                text.vertical(-(page as isize), shift);
                code.top = code.top.saturating_sub(page);
                None
            }
            KeyCode::PageDown => {
                text.vertical(page as isize, shift);
                code.top += page;
                None
            }
            KeyCode::Home if ctrl => {
                text.doc_start(shift);
                None
            }
            KeyCode::End if ctrl => {
                text.doc_end(shift);
                None
            }
            KeyCode::Home => {
                text.home(shift);
                None
            }
            KeyCode::End => {
                text.end(shift);
                None
            }
            KeyCode::Esc => {
                if text.clear_selection() {
                    None
                } else {
                    Some(CodeCommand::Close)
                }
            }
            KeyCode::F(1) => Some(CodeCommand::Help),
            KeyCode::F(2) => Some(CodeCommand::TreeView),
            KeyCode::F(3) if shift => Some(CodeCommand::FindPrevious),
            KeyCode::F(3) => Some(CodeCommand::FindNext),
            KeyCode::F(8) => Some(CodeCommand::GoToProblem),
            KeyCode::F(10) => Some(CodeCommand::Commands),
            _ => None,
        };
        if let Some(command) = command {
            self.code_command(command);
        }
    }

    fn code_bar_key(&mut self, key: KeyEvent) {
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        let Some(bar) = code.bar.as_mut() else {
            return;
        };
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let kind = bar.kind;
        let searching = kind != BarKind::GoTo;
        match key.code {
            KeyCode::Esc => {
                code.bar = None;
                code.follow = true;
            }
            KeyCode::Tab | KeyCode::BackTab if kind == BarKind::Replace => {
                bar.on_replace = !bar.on_replace;
            }
            KeyCode::Char('c' | 'C') if alt && searching => {
                code.case = !code.case;
                code.search_incremental();
            }
            KeyCode::Char('a' | 'A') if alt && kind == BarKind::Replace => {
                let replacement = bar.replace.value.clone();
                code.replacement = replacement.clone();
                let query = code.query.clone();
                let count = code.text.replace_all(&query, &replacement, code.case);
                code.follow = true;
                self.notify(fl!("tui-replaced", count = count), false);
            }
            KeyCode::Up if searching => {
                code.search_step(true);
            }
            KeyCode::Down if searching => {
                code.search_step(false);
            }
            KeyCode::F(3) if searching => {
                code.search_step(shift);
            }
            KeyCode::Enter => match kind {
                BarKind::GoTo => {
                    let target = bar.query.value.clone();
                    match code.go_to(&target) {
                        Ok(()) => code.bar = None,
                        Err(err) => {
                            if let Some(bar) = &mut code.bar {
                                bar.error = Some(err);
                            }
                        }
                    }
                }
                BarKind::Replace if bar.on_replace => {
                    let replacement = bar.replace.value.clone();
                    code.replacement = replacement.clone();
                    let query = code.query.clone();
                    code.text.replace_selection(&query, &replacement, code.case);
                    if !code.search_step(false)
                        && let Some(bar) = &mut code.bar
                    {
                        bar.error = Some(fl!("tui-search-none"));
                    }
                }
                _ => {
                    code.search_step(shift);
                }
            },
            _ => {
                let on_replace = bar.on_replace;
                let field = if on_replace {
                    &mut bar.replace
                } else {
                    &mut bar.query
                };
                let before = field.value.clone();
                field.on_key(key);
                let changed = field.value != before;
                if kind == BarKind::GoTo {
                    bar.error = None;
                } else if on_replace {
                    code.replacement = bar.replace.value.clone();
                } else if changed {
                    code.query = bar.query.value.clone();
                    code.search_incremental();
                }
            }
        }
    }

    pub(super) fn code_on_paste(&mut self, text: &str) {
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        match &mut code.bar {
            Some(bar) => {
                let line = text.lines().next().unwrap_or_default();
                if bar.on_replace {
                    bar.replace.paste(line);
                    code.replacement = bar.replace.value.clone();
                } else {
                    bar.query.paste(line);
                    if bar.kind != BarKind::GoTo {
                        code.query = bar.query.value.clone();
                        code.search_incremental();
                    }
                }
            }
            None => {
                code.text.insert_text(text);
                code.follow = true;
            }
        }
    }

    pub(super) fn code_on_mouse(&mut self, event: MouseEvent) {
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        let shift = event.modifiers.contains(KeyModifiers::SHIFT);
        let last = code.text.lines().len().saturating_sub(1);
        let inside = code.panel.contains((event.column, event.row).into());
        match event.kind {
            MouseEventKind::ScrollUp if inside => {
                code.top = code.top.saturating_sub(WHEEL_LINES);
                code.follow = false;
            }
            MouseEventKind::ScrollDown if inside => {
                code.top = (code.top + WHEEL_LINES).min(last);
                code.follow = false;
            }
            MouseEventKind::Down(MouseButton::Left) if inside => {
                let pos = code.pos_at(event.column, event.row);
                let double = code
                    .last_click
                    .is_some_and(|(at, p)| p == pos && at.elapsed() < DOUBLE_CLICK);
                if double {
                    code.text.select_word(pos);
                    code.last_click = None;
                } else {
                    code.text.set_cursor(pos, shift);
                    code.last_click = Some((Instant::now(), pos));
                }
                code.dragging = !double;
                code.follow = true;
            }
            MouseEventKind::Drag(MouseButton::Left) if code.dragging => {
                let pos = code.pos_at(event.column, event.row);
                code.text.set_cursor(pos, true);
                code.follow = true;
            }
            MouseEventKind::Up(MouseButton::Left) => code.dragging = false,
            _ => {}
        }
    }

    // ----- commands -------------------------------------------------------------

    pub(super) fn code_command(&mut self, command: CodeCommand) {
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        code.follow = true;
        match command {
            CodeCommand::Save => self.code_save(false),
            CodeCommand::SaveAndClose => self.code_save(true),
            CodeCommand::Discard => self.editor_close(),
            CodeCommand::Close => self.code_close_request(),
            CodeCommand::Format => self.code_format(),
            CodeCommand::Find => code.open_bar(BarKind::Find),
            CodeCommand::Replace => code.open_bar(BarKind::Replace),
            CodeCommand::GoTo => code.open_bar(BarKind::GoTo),
            CodeCommand::FindNext | CodeCommand::FindPrevious => {
                if code.query.is_empty() {
                    code.open_bar(BarKind::Find);
                } else if !code.search_step(command == CodeCommand::FindPrevious) {
                    self.notify(fl!("tui-search-none"), false);
                }
            }
            CodeCommand::GoToProblem => match code.problem() {
                Some(Problem { pos: Some(pos), .. }) => code.jump(pos),
                Some(problem) => self.notify(problem.message, false),
                None => self.notify(fl!("tui-code-no-problems"), false),
            },
            CodeCommand::TreeView => self.code_open_tree(),
            CodeCommand::ToggleComment => code.text.toggle_comment(),
            CodeCommand::DuplicateLines => code.text.duplicate_lines(),
            CodeCommand::DeleteLines => {
                code.text.delete_lines();
            }
            CodeCommand::Undo => {
                if !code.text.undo() {
                    self.notify(fl!("tui-nothing-to-undo"), false);
                }
            }
            CodeCommand::Redo => {
                if !code.text.redo() {
                    self.notify(fl!("tui-nothing-to-redo"), false);
                }
            }
            CodeCommand::SelectAll => code.text.select_all(),
            CodeCommand::Copy | CodeCommand::Cut => {
                let text = if command == CodeCommand::Cut {
                    code.text.cut()
                } else {
                    code.text.copy()
                };
                let lines = text.lines().count().max(1);
                self.profiles.clip = Some(text.clone());
                self.copy(text, fl!("tui-code-copied", lines = lines));
            }
            CodeCommand::Paste => match self.profiles.clip.clone() {
                Some(text) => code.text.paste(&text),
                None => self.notify(fl!("tui-code-clipboard-empty"), false),
            },
            CodeCommand::Commands => self.code_commands_menu(),
            CodeCommand::Help => self.popup = Some(Popup::CodeHelp),
        }
    }

    fn code_commands_menu(&mut self) {
        let item = |label: String, keys: &str, command: CodeCommand| {
            MenuItem::new(label, keys, MenuAction::Code(command))
        };
        let items = vec![
            item(fl!("code-cmd-save"), "Ctrl+S", CodeCommand::Save),
            item(fl!("code-cmd-format"), "Alt+F", CodeCommand::Format),
            item(fl!("code-cmd-find"), "Ctrl+F", CodeCommand::Find),
            item(fl!("code-cmd-replace"), "Ctrl+R", CodeCommand::Replace),
            item(fl!("code-cmd-goto"), "Ctrl+G", CodeCommand::GoTo),
            item(fl!("code-cmd-problem"), "F8", CodeCommand::GoToProblem),
            item(fl!("code-cmd-tree"), "F2", CodeCommand::TreeView),
            item(
                fl!("code-cmd-comment"),
                "Ctrl+/",
                CodeCommand::ToggleComment,
            ),
            item(
                fl!("code-cmd-duplicate"),
                "Ctrl+D",
                CodeCommand::DuplicateLines,
            ),
            item(
                fl!("code-cmd-delete-lines"),
                "Ctrl+K",
                CodeCommand::DeleteLines,
            ),
            item(fl!("code-cmd-undo"), "Ctrl+Z", CodeCommand::Undo),
            item(fl!("code-cmd-redo"), "Ctrl+Y", CodeCommand::Redo),
            item(fl!("code-cmd-select-all"), "Ctrl+A", CodeCommand::SelectAll),
            item(fl!("code-cmd-help"), "F1", CodeCommand::Help),
            item(fl!("code-cmd-close"), "Esc", CodeCommand::Close),
        ];
        self.popup = Some(Popup::Menu(Menu::new(fl!("code-commands-title"), items)));
    }

    fn code_close_request(&mut self) {
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        if !code.dirty() {
            self.editor_close();
            return;
        }
        let item = |label: String, command: CodeCommand| {
            MenuItem::new(label, "", MenuAction::Code(command))
        };
        let menu = Menu::new(
            fl!("tui-code-unsaved-title", name = code.profile.name.clone()),
            vec![
                item(fl!("tui-code-save-close"), CodeCommand::SaveAndClose),
                item(fl!("tui-discard-changes"), CodeCommand::Discard),
                MenuItem::new(fl!("tui-keep-editing"), "", MenuAction::Dismiss),
            ],
        );
        self.popup = Some(Popup::Menu(menu));
    }

    /// Checks the syntax here, then lets the daemon store the text (it
    /// runs `sing-box check` and reloads an active profile).
    fn code_save(&mut self, close_after: bool) {
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        if code.saving {
            self.notify(fl!("tui-code-save-in-progress"), false);
            return;
        }
        if !code.dirty() {
            if close_after {
                self.editor_close();
            } else {
                self.notify(fl!("ctl-profile-no-changes"), false);
            }
            return;
        }
        if let Some(error) = code.analysis.outline.error.clone() {
            code.jump(error.pos);
            self.notify(
                fl!(
                    "tui-code-fix-first",
                    line = (error.pos.line + 1).to_string(),
                    error = error.message
                ),
                true,
            );
            return;
        }
        let content = code.text.text();
        if let Err(err) = profile::parse(&content) {
            self.notify(error_chain(&err), true);
            return;
        }
        code.saving = true;
        code.close_after_save = close_after;
        code.check = None;
        let request = Request::ProfileSave {
            id: code.profile.id.clone(),
            content: content.clone(),
            force: code.force,
        };
        let label = if code.profile.active {
            fl!("busy-saving-reloading")
        } else {
            fl!("busy-saving")
        };
        self.profile_save_request(&label, request, SavePurpose::Code { saved: content });
    }

    /// The daemon answered a save from the editor.
    pub(super) fn code_saved(&mut self, saved: String, result: Result<(Profile, String), String>) {
        let Some(code) = self.profiles.code.as_mut() else {
            match result {
                Ok((_, message)) => self.notify(message, false),
                Err(err) => self.notify(err, true),
            }
            return;
        };
        code.saving = false;
        match result {
            Ok((profile, message)) => {
                if code.profile.id == profile.id {
                    code.mark_saved(saved);
                    code.profile = profile;
                }
                // An inactive profile is stored even when sing-box rejects
                // it; the warning in the message names the node.
                code.check = code.locate(&message, true);
                let close = code.close_after_save;
                self.exit_message = Some(message.clone());
                if close {
                    self.editor_close();
                }
                self.notify(message, false);
            }
            Err(err) => {
                code.close_after_save = false;
                code.check = code.locate(&err, false);
                if let Some(Problem { pos: Some(pos), .. }) = code.problem() {
                    code.jump(pos);
                }
                let force = MenuAction::SaveAnyway {
                    id: code.profile.id.clone(),
                    content: code.text.text(),
                    from_editor: true,
                };
                let keep = if code.check.is_some() {
                    fl!("tui-code-keep-editing-at-error")
                } else {
                    fl!("tui-keep-editing")
                };
                self.popup = Some(Popup::Menu(
                    Menu::new(
                        fl!("tui-save-failed-title"),
                        vec![
                            MenuItem::new(keep, "", MenuAction::Dismiss),
                            MenuItem::new(
                                fl!("tui-save-anyway"),
                                fl!("tui-save-anyway-detail"),
                                force,
                            ),
                        ],
                    )
                    .body(err),
                ));
            }
        }
    }

    fn code_format(&mut self) {
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        code.refresh();
        if let Some(error) = code.analysis.outline.error.clone() {
            code.jump(error.pos);
            self.notify(
                fl!(
                    "tui-code-fix-first",
                    line = (error.pos.line + 1).to_string(),
                    error = error.message
                ),
                true,
            );
            return;
        }
        let content = code.text.text();
        let value = match profile::parse(&content) {
            Ok(value) => value,
            Err(err) => {
                self.notify(error_chain(&err), true);
                return;
            }
        };
        let path = code.cursor_path();
        let formatted = pretty(&value, &code.text.indent);
        if formatted == content {
            self.notify(fl!("tui-code-formatted-already"), false);
            return;
        }
        code.text.set_text(&formatted);
        code.reveal(&path, false);
        let message = if profile::has_comments(&content) {
            fl!("tui-code-formatted-comments")
        } else {
            fl!("tui-code-formatted")
        };
        self.notify(message, false);
    }

    /// Opens the tree view on the node under the cursor.
    fn code_open_tree(&mut self) {
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        code.refresh();
        if let Some(error) = code.analysis.outline.error.clone() {
            code.jump(error.pos);
            self.notify(
                fl!(
                    "tui-code-tree-invalid",
                    line = (error.pos.line + 1).to_string(),
                    error = error.message
                ),
                true,
            );
            return;
        }
        let content = code.text.text();
        let path = code.cursor_path();
        match Editor::new(code.profile.clone(), &content) {
            Ok(mut tree) => {
                code.tree_base = Some(tree.root.clone());
                if !path.is_empty() {
                    tree.select(&path);
                }
                self.profiles.editor = Some(tree);
                self.profiles.editor_state = ListState::default();
                if profile::has_comments(&content) {
                    self.notify(fl!("tui-tree-comments"), false);
                }
            }
            Err(err) => self.notify(error_chain(&err), true),
        }
    }

    /// Back from the tree view: its changes replace the text as one undo
    /// step and the cursor goes to the node selected there (all of it
    /// selected with `select`).
    pub(super) fn code_close_tree(&mut self, select: bool) {
        let Some(tree) = self.profiles.editor.take() else {
            return;
        };
        let Some(code) = self.profiles.code.as_mut() else {
            return;
        };
        let path = tree.selected().unwrap_or_default();
        // serde_json's map equality ignores key order; reordering counts.
        let base = code.tree_base.take().map(|v| v.to_string());
        if base != Some(tree.root.to_string()) {
            let text = pretty(&tree.root, &code.text.indent);
            code.text.set_text(&text);
        }
        if !code.reveal(&path, select) {
            code.follow = true;
        }
    }
}

// ----- rendering ---------------------------------------------------------------------

fn token_style(token: Token) -> Style {
    match token {
        Token::Key => Style::new().fg(BLUE),
        Token::String => Style::new().fg(GREEN),
        Token::Number => Style::new().fg(PEACH),
        Token::Bool | Token::Null => Style::new().fg(ACCENT),
        Token::Bracket => Style::new().fg(SUBTEXT),
        Token::Punct => Style::new().fg(DIM),
        Token::Comment => Style::new().fg(DIM).add_modifier(Modifier::ITALIC),
        Token::Other => Style::new().fg(TEXT),
    }
}

/// What is marked in the visible text.
struct Marks<'a> {
    selection: Option<(Pos, Pos)>,
    /// Matches of the search, per visible line.
    matches: &'a [(Pos, Pos)],
    brackets: Option<(Pos, Pos)>,
    error: Option<Pos>,
    warning: Option<Pos>,
}

impl Marks<'_> {
    fn style(&self, at: Pos, base: Style) -> Style {
        let within = |range: &(Pos, Pos)| range.0 <= at && at < range.1;
        if self.error == Some(at) {
            return Style::new().fg(CRUST).bg(RED);
        }
        let current = self
            .selection
            .is_some_and(|s| self.matches.contains(&s) && within(&s));
        if current {
            return Style::new().fg(CRUST).bg(YELLOW);
        }
        if self.selection.as_ref().is_some_and(within) {
            return base.bg(SELECTION);
        }
        if self.matches.iter().any(within) {
            return base.bg(MATCH);
        }
        if self
            .brackets
            .is_some_and(|(open, close)| open == at || close == at)
        {
            return base.bg(SURFACE2).add_modifier(Modifier::BOLD);
        }
        if self.warning == Some(at) {
            return base.add_modifier(Modifier::UNDERLINED);
        }
        base
    }
}

/// One line of text from terminal column `left`, `width` columns wide.
fn text_line(
    code: &CodeEditor,
    index: usize,
    left: usize,
    width: usize,
    marks: &Marks,
) -> Line<'static> {
    let line = code.text.line(index);
    let runs = code.analysis.outline.highlight(index, line);
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = vec![Token::Other; chars.len()];
    for (start, end, token) in runs {
        for slot in tokens.iter_mut().take(end).skip(start) {
            *slot = token;
        }
    }
    let current = code.text.cursor().line == index;
    let fill = if current {
        Style::new().bg(CURRENT_LINE)
    } else {
        Style::new()
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut push = |text: String, style: Style| match spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(&text),
        _ => spans.push(Span::styled(text, style)),
    };
    let right = left + width;
    let mut column = 0;
    for (i, c) in chars.iter().enumerate() {
        let w = char_width(*c, column);
        // Zero-width characters (such as the emoji style selector) are not
        // drawn: the terminal would merge them into the previous cell.
        if w == 0 {
            continue;
        }
        let start = column;
        column += w;
        if column <= left {
            continue;
        }
        if start >= right {
            break;
        }
        let at = Pos::new(index, i);
        let style = marks.style(at, fill.patch(token_style(tokens[i])));
        let visible = column.min(right) - start.max(left);
        let shown = if *c == '\t' || start < left || column > right {
            " ".repeat(visible)
        } else if c.is_control() {
            "\u{fffd}".to_owned()
        } else {
            c.to_string()
        };
        push(shown, style);
    }
    // The line break: selected, or where an error was found.
    let end = Pos::new(index, chars.len());
    let selected_break = marks
        .selection
        .is_some_and(|(s, e)| s <= end && end < e && index + 1 < code.text.lines().len());
    if column >= left && column < right {
        if marks.error == Some(end) {
            push(" ".to_owned(), Style::new().bg(RED));
            column += 1;
        } else if selected_break {
            push(" ".to_owned(), fill.bg(SELECTION));
            column += 1;
        }
    }
    if current {
        let shown = column.max(left).min(right) - left;
        push(" ".repeat(width - shown), fill);
    }
    Line::from(spans)
}

/// Terminal columns a character takes when it starts at column `at`.
pub(super) fn char_width(c: char, at: usize) -> usize {
    match c {
        '\t' => TAB_WIDTH - at % TAB_WIDTH,
        c if c.is_control() => 1,
        c => c.width().unwrap_or(0),
    }
}

/// Terminal column of a character column.
pub(super) fn display_col(line: &str, col: usize) -> usize {
    line.chars()
        .take(col)
        .fold(0, |at, c| at + char_width(c, at))
}

/// Character column at a terminal column; past the end gives the length.
pub(super) fn col_at_display(line: &str, target: usize) -> usize {
    let mut at = 0;
    for (col, c) in line.chars().enumerate() {
        let width = char_width(c, at);
        if width > 0 && target < at + width {
            return col;
        }
        at += width;
    }
    line.chars().count()
}

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_open = app.popup.is_some();
    let Some(code) = app.profiles.code.as_mut() else {
        return;
    };
    code.refresh();
    let bar_rows = match code.bar.as_ref().map(|b| b.kind) {
        Some(BarKind::Replace) => 2,
        Some(_) => 1,
        None => 0,
    };
    let [panel_area, status_area, bar_area] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(bar_rows),
    ])
    .areas(area);

    let path = code.cursor_path();
    let cursor = code.text.cursor();
    let mut title = vec![Span::styled(
        format!(
            " {} ",
            fl!("tui-editor-title", name = code.profile.name.clone())
        ),
        Style::new().fg(CRUST).bg(ACCENT).bold(),
    )];
    if code.analysis.dirty {
        title.push(Span::raw(" "));
        title.push(Span::styled(
            format!(" ● {} ", fl!("tui-code-modified")),
            Style::new().fg(CRUST).bg(YELLOW),
        ));
    }
    if code.saving {
        title.push(Span::raw(" "));
        title.push(dim(fl!("busy-saving")));
    }
    let validity = match (
        &code.analysis.outline.error,
        code.analysis.outline.warnings.len(),
    ) {
        (Some(_), _) => Span::styled(
            format!("✗ {} ", fl!("tui-code-invalid")),
            Style::new().fg(RED),
        ),
        (None, 0) => Span::styled("✓ JSON ", Style::new().fg(GREEN)),
        (None, n) => Span::styled(format!("⚠ {n} "), Style::new().fg(YELLOW)),
    };
    let mut position = fl!(
        "tui-code-position",
        line = (cursor.line + 1).to_string(),
        col = (cursor.col + 1).to_string()
    );
    let selected = code.text.selection_len();
    if selected > 0 {
        position.push_str(&format!("  {}", fl!("tui-code-selected", count = selected)));
    }
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(title))
        .title_bottom(Line::from(vec![dim(format!(" {position}  ")), validity]).right_aligned());
    if !path.is_empty() {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {} ", display_path(&path)),
                Style::new().fg(SUBTEXT),
            ))
            .left_aligned(),
        );
    }
    if code.profile.active {
        block = block.title_top(
            Line::from(Span::styled(
                format!(" ● {} ", fl!("tui-profile-in-use")),
                Style::new().fg(GREEN),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    let count = code.text.lines().len();
    let digits = count.to_string().len().max(3);
    let gutter = (digits + 2) as u16;
    let [gutter_area, text_area] = Layout::horizontal([
        Constraint::Length(gutter.min(inner.width)),
        Constraint::Min(0),
    ])
    .areas(inner);
    code.panel = panel_area;
    code.view = text_area;
    let height = usize::from(text_area.height);
    let width = usize::from(text_area.width);
    code.scroll_into_view(height, width);

    let problem = code.problem();
    let error = code.analysis.outline.error.as_ref().map(|e| e.pos);
    let error_line = problem
        .as_ref()
        .filter(|p| p.level != Level::Warning)
        .and_then(|p| p.pos)
        .map(|p| p.line);
    let warning_lines: Vec<usize> = code
        .analysis
        .outline
        .warnings
        .iter()
        .map(|w| w.pos.line)
        .collect();
    let showing_matches = code
        .bar
        .as_ref()
        .is_some_and(|b| b.kind != BarKind::GoTo && !code.query.is_empty());
    let (top, bottom) = (code.top, code.top + height);
    let all = if showing_matches { code.matches() } else { &[] };
    let total = all.len();
    let matches: Vec<(Pos, Pos)> = all
        .iter()
        .filter(|(s, _)| s.line >= top && s.line < bottom)
        .copied()
        .collect();
    let marks = Marks {
        selection: code.text.selection(),
        matches: &matches,
        brackets: code.bracket_pair(),
        error,
        warning: code.analysis.outline.warnings.first().map(|w| w.pos),
    };

    let mut numbers = Vec::with_capacity(height);
    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let index = code.top + row;
        if index >= count {
            break;
        }
        let (marker, style) = if error_line == Some(index) {
            ("●", Style::new().fg(RED).bold())
        } else if warning_lines.contains(&index) {
            ("●", Style::new().fg(YELLOW))
        } else if index == cursor.line {
            (" ", Style::new().fg(TEXT).bold().bg(CURRENT_LINE))
        } else {
            (" ", Style::new().fg(DIM))
        };
        numbers.push(Line::from(Span::styled(
            format!("{marker}{:>digits$} ", index + 1),
            style,
        )));
        lines.push(text_line(code, index, code.left, width, &marks));
    }
    frame.render_widget(Paragraph::new(numbers), gutter_area);
    frame.render_widget(Paragraph::new(lines), text_area);

    if count > height && height > 0 {
        let mut state = ScrollbarState::new(count.saturating_sub(height) + 1)
            .position(code.top)
            .viewport_content_length(height);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .track_style(Style::new().fg(super::theme::BORDER))
                .thumb_style(Style::new().fg(ACCENT)),
            panel_area.inner(Margin::new(0, 1)),
            &mut state,
        );
    }

    draw_status(frame, status_area, problem);
    let field_cursor = code
        .bar
        .as_ref()
        .and_then(|bar| draw_bar(frame, bar_area, bar, code.case, &code.query, total));

    if popup_open {
        return;
    }
    if code.bar.is_some() {
        if let Some(position) = field_cursor {
            frame.set_cursor_position(position);
        }
        return;
    }
    let column = display_col(code.text.line(cursor.line), cursor.col);
    if cursor.line >= code.top
        && cursor.line < code.top + height
        && column >= code.left
        && column < code.left + width
    {
        frame.set_cursor_position((
            text_area.x + (column - code.left) as u16,
            text_area.y + (cursor.line - code.top) as u16,
        ));
    }
}

fn draw_status(frame: &mut Frame, area: Rect, problem: Option<Problem>) {
    let line = match problem {
        Some(problem) => {
            let (icon, color) = match problem.level {
                Level::Error => ("✗", RED),
                Level::Check => ("✗ sing-box", PEACH),
                Level::Warning => ("⚠", YELLOW),
            };
            let mut spans = vec![Span::styled(
                format!(" {icon} "),
                Style::new().fg(color).bold(),
            )];
            // The key first: long sing-box messages run off the line.
            if let Some(pos) = problem.pos {
                spans.push(dim(format!("{}:{} ", pos.line + 1, pos.col + 1)));
                spans.push(key("F8"));
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(problem.message, Style::new().fg(color)));
            Line::from(spans)
        }
        None => Line::from(vec![
            Span::styled(" ✓ ", Style::new().fg(GREEN)),
            dim(fl!("tui-code-status-ok")),
        ]),
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// A text field of the bar; returns its spans and where the cursor goes.
fn field_spans(input: &Input, placeholder: &str, focused: bool) -> (Vec<Span<'static>>, usize) {
    let (before, after) = input.split();
    let prompt = Style::new().fg(if focused { ACCENT } else { DIM });
    let mut spans = vec![Span::styled("❯ ", prompt)];
    let used = text_width(&input.value);
    if input.value.is_empty() {
        let hint = format!(" {placeholder}");
        let pad = FIELD_WIDTH.saturating_sub(text_width(&hint));
        spans.push(Span::styled(
            format!("{hint}{}", " ".repeat(pad)),
            Style::new().fg(DIM).bg(SURFACE),
        ));
    } else {
        spans.push(Span::styled(
            format!(
                "{before}{after}{}",
                " ".repeat(FIELD_WIDTH.saturating_sub(used))
            ),
            Style::new().fg(TEXT).bg(SURFACE),
        ));
    }
    (spans, 2 + text_width(before))
}

fn draw_bar(
    frame: &mut Frame,
    area: Rect,
    bar: &Bar,
    case: bool,
    query: &str,
    total: usize,
) -> Option<(u16, u16)> {
    let labels = [
        fl!("tui-find-label"),
        fl!("tui-replace-label"),
        fl!("tui-goto-label"),
    ];
    let label_width = labels.iter().map(|l| text_width(l)).max().unwrap_or(0);
    let label = |text: &str| {
        Span::styled(
            format!(" {} ", crate::util::pad(text, label_width)),
            Style::new().fg(CRUST).bg(ACCENT).bold(),
        )
    };
    let hint = |k: &str, text: String| [Span::raw("  "), key(k), dim(format!(" {text}"))];
    let mut rows = Vec::new();
    let mut cursor = None;
    let placeholder = match bar.kind {
        BarKind::GoTo => fl!("tui-goto-placeholder"),
        _ => String::new(),
    };
    let title = match bar.kind {
        BarKind::Find | BarKind::Replace => &labels[0],
        BarKind::GoTo => &labels[2],
    };
    let (field, offset) = field_spans(&bar.query, &placeholder, !bar.on_replace);
    let label_span = label(title);
    let start = label_span.width() + 1;
    if !bar.on_replace {
        cursor = Some((area.x + (start + offset) as u16, area.y));
    }
    let mut first = vec![label_span, Span::raw(" ")];
    first.extend(field);
    match bar.kind {
        BarKind::GoTo => {
            if let Some(err) = &bar.error {
                first.push(Span::styled(format!("  {err}"), Style::new().fg(RED)));
            }
            first.extend(hint("⏎", fl!("key-goto")));
            first.extend(hint("Esc", fl!("key-cancel")));
        }
        BarKind::Find | BarKind::Replace => {
            let status = match &bar.error {
                Some(err) => Span::styled(format!("  {err}"), Style::new().fg(RED)),
                None if query.is_empty() => Span::raw(""),
                None => dim(format!("  {}", fl!("tui-find-count", count = total))),
            };
            first.push(status);
            first.push(Span::raw("  "));
            first.push(Span::styled(
                " Aa ",
                if case {
                    Style::new().fg(CRUST).bg(ACCENT)
                } else {
                    Style::new().fg(DIM).bg(SURFACE)
                },
            ));
            first.push(dim(" Alt+C"));
            first.extend(hint("↑↓", fl!("key-previous-next")));
            first.extend(hint("Esc", fl!("key-close")));
        }
    }
    rows.push(Line::from(first));
    if bar.kind == BarKind::Replace {
        let (field, offset) = field_spans(&bar.replace, "", bar.on_replace);
        let label_span = label(&labels[1]);
        let start = label_span.width() + 1;
        if bar.on_replace {
            cursor = Some((area.x + (start + offset) as u16, area.y + 1));
        }
        let mut second = vec![label_span, Span::raw(" ")];
        second.extend(field);
        second.extend(hint("⏎", fl!("key-replace")));
        second.extend(hint("Alt+A", fl!("key-replace-all")));
        second.extend(hint("Tab", fl!("key-switch-field")));
        rows.push(Line::from(second));
    }
    frame.render_widget(Paragraph::new(rows), area);
    cursor.filter(|(x, y)| *x < area.right() && *y < area.bottom())
}

/// Footer hints while the code editor is open.
pub fn hints(code: &CodeEditor) -> Vec<(&'static str, String)> {
    if code.bar.is_some() {
        return vec![
            ("⏎", fl!("key-next")),
            ("↑↓", fl!("key-previous-next")),
            ("Esc", fl!("key-close")),
        ];
    }
    vec![
        ("^S", fl!("key-save")),
        ("^F", fl!("key-search")),
        ("^R", fl!("key-replace")),
        ("^G", fl!("key-goto")),
        ("^Z/^Y", fl!("key-undo-redo")),
        ("F2", fl!("key-tree")),
        ("F10", fl!("key-commands")),
        ("F1", fl!("key-help")),
        ("Esc", fl!("key-close-editor")),
    ]
}

/// Rows of the editor's key help: two columns.
pub fn help_rows() -> [Vec<(&'static str, String)>; 2] {
    let blank = || ("", String::new());
    [
        vec![
            ("", fl!("help-code-file")),
            ("Ctrl+S", fl!("help-code-save")),
            ("Esc Ctrl+Q", fl!("help-code-close")),
            ("F2", fl!("help-code-tree")),
            ("F10 Ctrl+P", fl!("help-code-commands")),
            ("Alt+F", fl!("help-code-format")),
            blank(),
            ("", fl!("help-code-find")),
            ("Ctrl+F", fl!("help-code-search")),
            ("Ctrl+R", fl!("help-code-replace")),
            ("F3 Shift+F3", fl!("help-code-next")),
            ("Ctrl+G", fl!("help-code-goto")),
            ("F8", fl!("help-code-problem")),
        ],
        vec![
            ("", fl!("help-code-edit")),
            ("Ctrl+Z Ctrl+Y", fl!("help-code-undo")),
            ("Ctrl+C X V", fl!("help-code-clipboard")),
            ("Ctrl+A", fl!("help-code-select-all")),
            ("Ctrl+D Ctrl+K", fl!("help-code-lines")),
            ("Alt+↑↓", fl!("help-code-move-lines")),
            ("Tab Shift+Tab", fl!("help-code-indent")),
            ("Ctrl+/", fl!("help-code-comment")),
            blank(),
            ("", fl!("help-code-cursor")),
            ("Shift+arrows", fl!("help-code-select")),
            ("Ctrl+← →", fl!("help-code-words")),
            ("Ctrl+Home End", fl!("help-code-ends")),
            ("Mouse", fl!("help-code-mouse")),
        ],
    ]
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::client::DaemonClient;

    fn profile() -> Profile {
        Profile {
            id: "aaaa".into(),
            name: "Home".into(),
            url: None,
            interval: 0,
            created_at: 1,
            updated_at: 1,
            fetched_at: None,
            last_error: None,
            usage: None,
            size: 1,
            active: true,
        }
    }

    fn app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut app, _tasks) = App::new(DaemonClient::new("/nonexistent/socket".into()), tx);
        app.tab = super::super::app::Tab::Profiles;
        app
    }

    fn press(app: &mut App, key: KeyEvent) {
        app.on_key(key);
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyEvent::from(KeyCode::Char(c)));
        }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

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

    fn code(app: &App) -> &CodeEditor {
        app.profiles.code.as_ref().unwrap()
    }

    #[tokio::test]
    async fn typing_validation_and_search() {
        let mut app = app();
        app.open_code_editor(profile(), "{\n  \"log\": {\"level\": \"info\"}\n}\n");
        let shown = screen(&mut app, 100, 20);
        assert!(shown.contains("✓ JSON"), "{shown}");
        // Digits and Tab are text here, not tab switches.
        press(&mut app, KeyEvent::from(KeyCode::Down));
        press(&mut app, KeyEvent::from(KeyCode::End));
        typed(&mut app, ",1");
        press(&mut app, KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.tab, super::super::app::Tab::Profiles);
        let shown = screen(&mut app, 100, 20);
        assert!(shown.contains("●"), "{shown}");
        assert!(code(&app).analysis.outline.error.is_some());
        // ratatui-textarea undoes typing one character at a time.
        for _ in 0..3 {
            press(&mut app, ctrl('z'));
        }
        assert!(!app.profiles.code.as_mut().unwrap().dirty());
        // Find selects the match; Esc closes the bar, then the editor.
        press(&mut app, ctrl('f'));
        typed(&mut app, "INFO");
        assert_eq!(code(&app).text.selected_text().as_deref(), Some("info"));
        screen(&mut app, 100, 20);
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        assert!(code(&app).bar.is_none());
        // Ctrl+C copies instead of quitting.
        press(&mut app, ctrl('c'));
        assert!(!app.should_quit);
        assert_eq!(app.take_clipboard().as_deref(), Some("info"));
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        assert!(app.profiles.code.is_none());
    }

    #[tokio::test]
    async fn quitting_asks_about_unsaved_changes() {
        let mut app = app();
        app.open_code_editor(profile(), "{}\n");
        typed(&mut app, " ");
        press(&mut app, KeyEvent::from(KeyCode::F(2)));
        // Elsewhere in the dashboard, q would lose the edit.
        press(&mut app, KeyEvent::from(KeyCode::Char('1')));
        press(&mut app, KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.should_quit);
        assert_eq!(app.tab, super::super::app::Tab::Profiles);
        assert!(app.profiles.editor.is_none());
        assert!(matches!(app.popup, Some(Popup::Menu(_))));
        // Discard, then quitting works.
        press(&mut app, KeyEvent::from(KeyCode::Char('2')));
        assert!(app.profiles.code.is_none());
        press(&mut app, KeyEvent::from(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn tree_edits_count_as_unsaved_and_keep_member_order() {
        let mut app = app();
        app.open_code_editor(profile(), "{\"a\": 1, \"b\": 2}\n");
        press(&mut app, KeyEvent::from(KeyCode::F(2)));
        // Moving a member is a change, though the maps compare equal.
        press(&mut app, KeyEvent::from(KeyCode::Char('J')));
        press(&mut app, KeyEvent::from(KeyCode::Char('1')));
        press(&mut app, KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.should_quit);
        assert!(matches!(app.popup, Some(Popup::Menu(_))));
        let text = code(&app).text.text();
        assert!(
            text.find("\"b\"").unwrap() < text.find("\"a\"").unwrap(),
            "{text}"
        );
    }

    #[tokio::test]
    async fn tree_view_round_trip() {
        let mut app = app();
        app.open_code_editor(
            profile(),
            "{\n    // keep\n    \"outbounds\": [{\"tag\": \"a\", \"type\": \"direct\"}]\n}\n",
        );
        press(&mut app, ctrl('g'));
        typed(&mut app, "outbounds[0].tag");
        press(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(code(&app).text.cursor(), Pos::new(2, 19));
        press(&mut app, KeyEvent::from(KeyCode::F(2)));
        assert!(app.profiles.editor.is_some());
        // Unchanged: the text and its comment stay as they were.
        press(&mut app, KeyEvent::from(KeyCode::Char('q')));
        assert!(app.profiles.editor.is_none());
        assert!(code(&app).text.text().contains("// keep"));
        // A change in the tree rewrites the text, indented like before.
        press(&mut app, KeyEvent::from(KeyCode::F(2)));
        press(&mut app, KeyEvent::from(KeyCode::Char('d')));
        press(&mut app, KeyEvent::from(KeyCode::F(2)));
        let text = code(&app).text.text();
        assert!(
            text.contains("    \"outbounds\": [\n        {\n            \"type\""),
            "{text}"
        );
        press(&mut app, ctrl('z'));
        assert!(code(&app).text.text().contains("// keep"));
    }

    #[tokio::test]
    async fn invalid_documents_open_and_format() {
        let mut app = app();
        app.open_code_editor(profile(), "{\"a\": [1,2,]}");
        assert!(app.profiles.code.as_mut().unwrap().problem().is_none());
        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
        );
        assert_eq!(
            code(&app).text.text(),
            "{\n  \"a\": [\n    1,\n    2\n  ]\n}\n"
        );
        app.open_code_editor(profile(), "{\"a\": }");
        let shown = screen(&mut app, 100, 12);
        assert!(shown.contains("✗"), "{shown}");
        press(&mut app, KeyEvent::from(KeyCode::F(8)));
        assert_eq!(code(&app).text.cursor(), Pos::new(0, 6));
        typed(&mut app, " ");
        press(&mut app, ctrl('s'));
        assert!(!code(&app).saving);
    }
}
