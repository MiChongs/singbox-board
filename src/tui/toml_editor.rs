//! The configuration editor of the Containers tab: a container's TOML with
//! highlighting, validation while typing (the TOML syntax and what every
//! kurumi-containerd configuration needs), undo, the clipboard and the
//! mouse. The keys are those of the profile editor. The text is kept by
//! [`Buffer`]; saving goes through the daemon, which also asks
//! kurumi-containerd to check the result.

use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use super::app::{App, Popup, Tab};
use super::buffer::Buffer;
use super::code::{char_width, col_at_display, display_col};
use super::containers::ContainerSavePurpose;
use super::jsonc::Pos;
use super::popup::{Menu, MenuAction, MenuItem};
use super::theme::{
    ACCENT, BLUE, CRUST, CURRENT_LINE, DIM, GREEN, PEACH, RED, SELECTION, SUBTEXT, TEAL, TEXT,
    YELLOW, dim, key,
};
use crate::i18n::fl;
use crate::protocol::{Container, Request};
use crate::util::error_chain;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const SCROLL_MARGIN: usize = 3;
const WHEEL_LINES: usize = 3;

/// Editor commands, from keys and from the command menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TomlCommand {
    Save,
    SaveAndClose,
    /// Close without saving.
    Discard,
    Close,
    GoToProblem,
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
}

// ----- highlighting ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    Comment,
    Table,
    Key,
    String,
    Number,
    Bool,
    Punct,
    Other,
}

/// A string spanning lines, by its delimiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Multi {
    Basic,
    Literal,
}

impl Multi {
    fn delimiter(self) -> &'static str {
        match self {
            Multi::Basic => "\"\"\"",
            Multi::Literal => "'''",
        }
    }
}

/// What a line starts inside of.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LexState {
    string: Option<Multi>,
    /// Open `[` of arrays spanning lines.
    arrays: u32,
}

/// Character index where `needle` (ASCII) starts in `chars` at or after `from`.
fn find(chars: &[char], from: usize, needle: &str, escapes: bool) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    let mut i = from;
    while i + needle.len() <= chars.len() {
        if escapes && chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i..i + needle.len()] == needle[..] {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '.' | ':')
}

/// Highlight runs `(start, end, token)` of one line, in characters, and
/// the state the next line starts in.
pub fn lex_line(line: &str, state: LexState) -> (Vec<(usize, usize, Tok)>, LexState) {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut runs = Vec::new();
    let mut state = state;
    let mut i = 0;
    if let Some(multi) = state.string {
        match find(&chars, 0, multi.delimiter(), multi == Multi::Basic) {
            Some(end) => {
                runs.push((0, end + 3, Tok::String));
                i = end + 3;
                state.string = None;
            }
            None => return (vec![(0, len, Tok::String)], state),
        }
    }
    let skip_space = |mut at: usize| {
        while at < len && chars[at].is_whitespace() {
            at += 1;
        }
        at
    };
    let start = skip_space(i);
    if i == 0 && state.arrays == 0 && start < len {
        if chars[start] == '[' {
            // A table header: `[name]` or `[[name]]`.
            let double = chars.get(start + 1) == Some(&'[');
            let close = find(&chars, start, if double { "]]" } else { "]" }, false);
            let end = close.map_or(len, |c| c + if double { 2 } else { 1 });
            runs.push((start, end, Tok::Table));
            i = end;
        } else if chars[start] != '#' {
            // `key = value`: the key ends at an `=` outside quotes.
            let mut at = start;
            let mut equals = None;
            while at < len {
                match chars[at] {
                    '"' | '\'' => {
                        let quote = chars[at].to_string();
                        at = find(&chars, at + 1, &quote, chars[at] == '"').map_or(len, |e| e + 1);
                    }
                    '=' => {
                        equals = Some(at);
                        break;
                    }
                    '#' => break,
                    _ => at += 1,
                }
            }
            if let Some(equals) = equals {
                let mut key_end = equals;
                while key_end > start && chars[key_end - 1].is_whitespace() {
                    key_end -= 1;
                }
                if key_end > start {
                    runs.push((start, key_end, Tok::Key));
                }
                runs.push((equals, equals + 1, Tok::Punct));
                i = equals + 1;
            }
        }
    }
    // Values: strings, numbers, booleans, arrays and inline tables.
    let mut inline = 0u32;
    while i < len {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '#' => {
                runs.push((i, len, Tok::Comment));
                break;
            }
            '"' | '\'' => {
                let multi = if chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c) {
                    Some(if c == '"' {
                        Multi::Basic
                    } else {
                        Multi::Literal
                    })
                } else {
                    None
                };
                match multi {
                    Some(multi) => match find(&chars, i + 3, multi.delimiter(), c == '"') {
                        Some(end) => {
                            runs.push((i, end + 3, Tok::String));
                            i = end + 3;
                        }
                        None => {
                            runs.push((i, len, Tok::String));
                            state.string = Some(multi);
                            break;
                        }
                    },
                    None => {
                        let end =
                            find(&chars, i + 1, &c.to_string(), c == '"').map_or(len, |e| e + 1);
                        runs.push((i, end, Tok::String));
                        i = end;
                    }
                }
            }
            '[' | ']' | '{' | '}' | ',' | '=' => {
                match c {
                    '[' => state.arrays += 1,
                    ']' => state.arrays = state.arrays.saturating_sub(1),
                    '{' => inline += 1,
                    '}' => inline = inline.saturating_sub(1),
                    _ => {}
                }
                runs.push((i, i + 1, Tok::Punct));
                i += 1;
            }
            c if is_word(c) => {
                let start = i;
                while i < len && is_word(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let next = chars[i..].iter().find(|c| !c.is_whitespace());
                let token = if (inline > 0 || state.arrays > 0) && next == Some(&'=') {
                    Tok::Key
                } else if word == "true" || word == "false" {
                    Tok::Bool
                } else if word.starts_with(|c: char| c.is_ascii_digit())
                    || (word.starts_with(['+', '-'])
                        && word[1..]
                            .starts_with(|c: char| c.is_ascii_digit() || c == 'i' || c == 'n'))
                    || matches!(word.as_str(), "inf" | "nan")
                {
                    Tok::Number
                } else {
                    Tok::Other
                };
                runs.push((start, i, token));
            }
            _ => {
                runs.push((i, i + 1, Tok::Other));
                i += 1;
            }
        }
    }
    (runs, state)
}

fn tok_style(token: Tok) -> Style {
    match token {
        Tok::Comment => Style::new().fg(DIM).add_modifier(Modifier::ITALIC),
        Tok::Table => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        Tok::Key => Style::new().fg(BLUE),
        Tok::String => Style::new().fg(GREEN),
        Tok::Number => Style::new().fg(PEACH),
        Tok::Bool => Style::new().fg(TEAL),
        Tok::Punct => Style::new().fg(SUBTEXT),
        Tok::Other => Style::new().fg(TEXT),
    }
}

// ----- the editor -------------------------------------------------------------------

/// What the status line reports.
#[derive(Debug, Clone)]
struct Problem {
    pos: Option<Pos>,
    message: String,
    /// kurumi-containerd (through the daemon) refused the last save.
    rejected: bool,
}

pub struct TomlEditor {
    pub container: Container,
    pub text: Buffer,
    /// The text as stored by the daemon.
    saved: String,
    /// Text version the derived fields belong to.
    version: u64,
    dirty: bool,
    /// The state each line starts in.
    states: Vec<LexState>,
    problem: Option<Problem>,
    /// What the daemon said when it refused the last save, and the text
    /// version it refers to.
    rejected: Option<(u64, String)>,
    top: usize,
    left: usize,
    follow: bool,
    panel: Rect,
    view: Rect,
    pub saving: bool,
    close_after_save: bool,
    /// Store even when kurumi-containerd rejects the result (`--force`).
    pub force: bool,
    last_click: Option<(Instant, Pos)>,
    dragging: bool,
}

/// Where kurumi-containerd says a TOML error is (`at line N, column M`,
/// both from 1). The runtime checked exactly the text being edited.
fn reported_position(message: &str) -> Option<Pos> {
    let (_, rest) = message.split_once(" at line ")?;
    let (line, rest) = rest.split_once(", column ")?;
    let column: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let line: usize = line.trim().parse().ok()?;
    let column: usize = column.parse().ok()?;
    Some(Pos::new(line.checked_sub(1)?, column.saturating_sub(1)))
}

/// The line of a runtime error worth a status line: the one after the code
/// frame of a TOML error (`unknown field ...`), else the first.
fn reason(message: &str) -> String {
    let lines: Vec<&str> = message.lines().collect();
    lines
        .iter()
        .position(|line| line.trim_start().starts_with('|') && line.contains('^'))
        .and_then(|caret| lines.get(caret + 1))
        .or(lines.first())
        .map(|line| line.trim().to_owned())
        .unwrap_or_default()
}

/// Where a byte offset of `text` is, as a position in characters.
fn pos_of(text: &str, offset: usize) -> Pos {
    let (line, column) = crate::container::location(text, Some(offset..offset)).unwrap_or((1, 1));
    Pos::new(line - 1, column - 1)
}

impl TomlEditor {
    pub fn new(container: Container, content: &str) -> Self {
        let mut text = Buffer::new(content);
        text.comment = "#";
        let saved = text.text();
        Self {
            container,
            text,
            saved,
            version: u64::MAX,
            dirty: false,
            states: Vec::new(),
            problem: None,
            rejected: None,
            top: 0,
            left: 0,
            follow: true,
            panel: Rect::default(),
            view: Rect::default(),
            saving: false,
            close_after_save: false,
            force: false,
            last_click: None,
            dragging: false,
        }
    }

    /// Re-analyses the text after a change.
    fn refresh(&mut self) {
        let version = self.text.version();
        if self.version == version {
            return;
        }
        self.version = version;
        let text = self.text.text();
        self.dirty = text != self.saved;
        let mut state = LexState::default();
        self.states = self
            .text
            .lines()
            .iter()
            .map(|line| {
                let start = state;
                state = lex_line(line, state).1;
                start
            })
            .collect();
        self.problem =
            match text.parse::<toml::Table>() {
                Err(err) => Some(Problem {
                    pos: err.span().map(|span| pos_of(&text, span.start)),
                    message: err.message().trim().to_owned(),
                    rejected: false,
                }),
                Ok(_) => match crate::container::parse(&text, Path::new(&self.container.file)) {
                    Err(err) => Some(Problem {
                        pos: None,
                        message: error_chain(&err),
                        rejected: false,
                    }),
                    Ok(_) => self.rejected.as_ref().filter(|(at, _)| *at == version).map(
                        |(_, message)| Problem {
                            pos: reported_position(message),
                            message: reason(message),
                            rejected: true,
                        },
                    ),
                },
            };
    }

    pub fn dirty(&mut self) -> bool {
        self.refresh();
        self.dirty
    }

    fn mark_saved(&mut self, saved: String) {
        self.saved = saved;
        self.rejected = None;
        self.version = u64::MAX;
    }

    fn syntax_error(&mut self) -> Option<Problem> {
        self.refresh();
        self.problem.clone().filter(|p| !p.rejected)
    }

    fn jump(&mut self, pos: Pos) {
        let pos = self.text.clamp(pos);
        self.text.set_cursor(pos, false);
        self.follow = true;
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

    fn pos_at(&self, x: u16, y: u16) -> Pos {
        let view = self.view;
        let line = if y < view.y {
            self.top.saturating_sub(1)
        } else {
            self.top + usize::from(y - view.y)
        };
        let line = line.min(self.text.lines().len().saturating_sub(1));
        let column = self.left + usize::from(x.saturating_sub(view.x));
        Pos::new(line, col_at_display(self.text.line(line), column))
    }
}

impl App {
    pub(super) fn open_toml_editor(&mut self, container: Container, content: &str) {
        self.containers.editor = Some(TomlEditor::new(container, content));
    }

    /// The configuration editor is shown and takes the keys.
    pub(super) fn toml_active(&self) -> bool {
        self.tab == Tab::Containers && self.containers.editor.is_some()
    }

    pub(super) fn toml_on_key(&mut self, key: KeyEvent) {
        let Some(editor) = self.containers.editor.as_mut() else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let typed = ctrl == alt;
        let page = usize::from(editor.view.height.max(2) - 1);
        editor.follow = true;
        let text = &mut editor.text;
        let command = match key.code {
            KeyCode::Char(c) if typed => {
                text.type_char(c);
                None
            }
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                's' => Some(TomlCommand::Save),
                'q' => Some(TomlCommand::Close),
                'z' if shift => Some(TomlCommand::Redo),
                'z' => Some(TomlCommand::Undo),
                'y' => Some(TomlCommand::Redo),
                'a' => Some(TomlCommand::SelectAll),
                'c' => Some(TomlCommand::Copy),
                'x' => Some(TomlCommand::Cut),
                'v' => Some(TomlCommand::Paste),
                'd' => Some(TomlCommand::DuplicateLines),
                'k' => Some(TomlCommand::DeleteLines),
                'g' => Some(TomlCommand::GoToProblem),
                'p' => Some(TomlCommand::Commands),
                '/' | '7' => Some(TomlCommand::ToggleComment),
                'h' => {
                    text.backspace();
                    None
                }
                _ => None,
            },
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
                editor.top = editor.top.saturating_sub(page);
                None
            }
            KeyCode::PageDown => {
                text.vertical(page as isize, shift);
                editor.top += page;
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
                    Some(TomlCommand::Close)
                }
            }
            KeyCode::F(8) => Some(TomlCommand::GoToProblem),
            KeyCode::F(10) => Some(TomlCommand::Commands),
            _ => None,
        };
        if let Some(command) = command {
            self.toml_command(command);
        }
    }

    pub(super) fn toml_on_paste(&mut self, text: &str) {
        if let Some(editor) = self.containers.editor.as_mut() {
            editor.text.insert_text(text);
            editor.follow = true;
        }
    }

    pub(super) fn toml_on_mouse(&mut self, event: MouseEvent) {
        let Some(editor) = self.containers.editor.as_mut() else {
            return;
        };
        let shift = event.modifiers.contains(KeyModifiers::SHIFT);
        let last = editor.text.lines().len().saturating_sub(1);
        let inside = editor.panel.contains((event.column, event.row).into());
        match event.kind {
            MouseEventKind::ScrollUp if inside => {
                editor.top = editor.top.saturating_sub(WHEEL_LINES);
                editor.follow = false;
            }
            MouseEventKind::ScrollDown if inside => {
                editor.top = (editor.top + WHEEL_LINES).min(last);
                editor.follow = false;
            }
            MouseEventKind::Down(MouseButton::Left) if inside => {
                let pos = editor.pos_at(event.column, event.row);
                let double = editor
                    .last_click
                    .is_some_and(|(at, p)| p == pos && at.elapsed() < DOUBLE_CLICK);
                if double {
                    editor.text.select_word(pos);
                    editor.last_click = None;
                } else {
                    editor.text.set_cursor(pos, shift);
                    editor.last_click = Some((Instant::now(), pos));
                }
                editor.dragging = !double;
                editor.follow = true;
            }
            MouseEventKind::Drag(MouseButton::Left) if editor.dragging => {
                let pos = editor.pos_at(event.column, event.row);
                editor.text.set_cursor(pos, true);
                editor.follow = true;
            }
            MouseEventKind::Up(MouseButton::Left) => editor.dragging = false,
            _ => {}
        }
    }

    pub(super) fn toml_command(&mut self, command: TomlCommand) {
        let Some(editor) = self.containers.editor.as_mut() else {
            return;
        };
        editor.follow = true;
        match command {
            TomlCommand::Save => self.toml_save(false),
            TomlCommand::SaveAndClose => self.toml_save(true),
            TomlCommand::Discard => self.toml_close(),
            TomlCommand::Close => self.toml_close_request(),
            TomlCommand::GoToProblem => {
                editor.refresh();
                match editor.problem.clone() {
                    Some(Problem { pos: Some(pos), .. }) => editor.jump(pos),
                    Some(problem) => self.notify(problem.message, problem.rejected),
                    None => self.notify(fl!("tui-code-no-problems"), false),
                }
            }
            TomlCommand::ToggleComment => editor.text.toggle_comment(),
            TomlCommand::DuplicateLines => editor.text.duplicate_lines(),
            TomlCommand::DeleteLines => {
                editor.text.delete_lines();
            }
            TomlCommand::Undo => {
                if !editor.text.undo() {
                    self.notify(fl!("tui-nothing-to-undo"), false);
                }
            }
            TomlCommand::Redo => {
                if !editor.text.redo() {
                    self.notify(fl!("tui-nothing-to-redo"), false);
                }
            }
            TomlCommand::SelectAll => editor.text.select_all(),
            TomlCommand::Copy | TomlCommand::Cut => {
                let text = if command == TomlCommand::Cut {
                    editor.text.cut()
                } else {
                    editor.text.copy()
                };
                let lines = text.lines().count().max(1);
                self.containers.clip = Some(text.clone());
                self.copy(text, fl!("tui-code-copied", lines = lines));
            }
            TomlCommand::Paste => match self.containers.clip.clone() {
                Some(text) => editor.text.paste(&text),
                None => self.notify(fl!("tui-code-clipboard-empty"), false),
            },
            TomlCommand::Commands => self.toml_commands_menu(),
        }
    }

    fn toml_commands_menu(&mut self) {
        let item = |label: String, keys: &str, command: TomlCommand| {
            MenuItem::new(label, keys, MenuAction::Toml(command))
        };
        let items = vec![
            item(fl!("code-cmd-save"), "Ctrl+S", TomlCommand::Save),
            item(fl!("code-cmd-problem"), "F8", TomlCommand::GoToProblem),
            item(
                fl!("code-cmd-comment"),
                "Ctrl+/",
                TomlCommand::ToggleComment,
            ),
            item(
                fl!("code-cmd-duplicate"),
                "Ctrl+D",
                TomlCommand::DuplicateLines,
            ),
            item(
                fl!("code-cmd-delete-lines"),
                "Ctrl+K",
                TomlCommand::DeleteLines,
            ),
            item(fl!("code-cmd-undo"), "Ctrl+Z", TomlCommand::Undo),
            item(fl!("code-cmd-redo"), "Ctrl+Y", TomlCommand::Redo),
            item(fl!("code-cmd-select-all"), "Ctrl+A", TomlCommand::SelectAll),
            item(fl!("code-cmd-close"), "Esc", TomlCommand::Close),
        ];
        self.popup = Some(Popup::Menu(Menu::new(fl!("code-commands-title"), items)));
    }

    fn toml_close_request(&mut self) {
        let Some(editor) = self.containers.editor.as_mut() else {
            return;
        };
        if !editor.dirty() {
            self.toml_close();
            return;
        }
        let item = |label: String, command: TomlCommand| {
            MenuItem::new(label, "", MenuAction::Toml(command))
        };
        let menu = Menu::new(
            fl!(
                "tui-code-unsaved-title",
                name = editor.container.name.clone()
            ),
            vec![
                item(fl!("tui-code-save-close"), TomlCommand::SaveAndClose),
                item(fl!("tui-discard-changes"), TomlCommand::Discard),
                MenuItem::new(fl!("tui-keep-editing"), "", MenuAction::Dismiss),
            ],
        );
        self.popup = Some(Popup::Menu(menu));
    }

    /// Closes the editor without saving.
    pub(super) fn toml_close(&mut self) {
        self.containers.editor = None;
        if self.edit_only {
            self.should_quit = true;
            return;
        }
        self.containers_load();
    }

    /// Checks the syntax here, then lets the daemon store the text (it asks
    /// kurumi-containerd to check it).
    fn toml_save(&mut self, close_after: bool) {
        let Some(editor) = self.containers.editor.as_mut() else {
            return;
        };
        if editor.saving {
            self.notify(fl!("tui-code-save-in-progress"), false);
            return;
        }
        if !editor.dirty() {
            if close_after {
                self.toml_close();
            } else {
                self.notify(fl!("ctl-profile-no-changes"), false);
            }
            return;
        }
        if let Some(problem) = editor.syntax_error() {
            if let Some(pos) = problem.pos {
                editor.jump(pos);
            }
            self.notify(fl!("tui-toml-fix-first", error = problem.message), true);
            return;
        }
        let content = editor.text.text();
        editor.saving = true;
        editor.close_after_save = close_after;
        let request = Request::ContainerSave {
            id: editor.container.id.clone(),
            content: content.clone(),
            force: editor.force,
        };
        self.container_save_request(
            &fl!("busy-saving"),
            request,
            ContainerSavePurpose::Editor { saved: content },
        );
    }

    /// The daemon answered a save from the editor.
    pub(super) fn toml_saved(
        &mut self,
        saved: String,
        result: Result<(Container, String), String>,
    ) {
        let Some(editor) = self.containers.editor.as_mut() else {
            match result {
                Ok((_, message)) => self.notify(message, false),
                Err(err) => self.notify(err, true),
            }
            return;
        };
        editor.saving = false;
        match result {
            Ok((container, message)) => {
                if editor.container.id == container.id {
                    editor.mark_saved(saved);
                    editor.container = container;
                }
                let close = editor.close_after_save;
                self.exit_message = Some(message.clone());
                if close {
                    self.toml_close();
                }
                self.notify(message, false);
            }
            Err(err) => {
                editor.close_after_save = false;
                editor.refresh();
                editor.rejected = Some((editor.version, err.clone()));
                editor.version = u64::MAX;
                let force = MenuAction::ContainerSaveAnyway {
                    id: editor.container.id.clone(),
                    content: editor.text.text(),
                };
                self.popup = Some(Popup::Menu(
                    Menu::new(
                        fl!("tui-save-failed-title"),
                        vec![
                            MenuItem::new(fl!("tui-keep-editing"), "", MenuAction::Dismiss),
                            MenuItem::new(
                                fl!("tui-save-anyway"),
                                fl!("tui-toml-save-anyway-detail"),
                                force,
                            ),
                        ],
                    )
                    .body(err),
                ));
            }
        }
    }
}

// ----- drawing ------------------------------------------------------------------------

fn text_line(
    editor: &TomlEditor,
    index: usize,
    left: usize,
    width: usize,
    error: Option<Pos>,
) -> Line<'static> {
    let line = editor.text.line(index);
    let state = editor.states.get(index).copied().unwrap_or_default();
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = vec![Tok::Other; chars.len()];
    for (start, end, token) in lex_line(line, state).0 {
        for slot in tokens.iter_mut().take(end).skip(start) {
            *slot = token;
        }
    }
    let selection = editor.text.selection();
    let current = editor.text.cursor().line == index;
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
        let mut style = fill.patch(tok_style(tokens[i]));
        if error == Some(at) {
            style = Style::new().fg(CRUST).bg(RED);
        } else if selection.is_some_and(|(s, e)| s <= at && at < e) {
            style = style.bg(SELECTION);
        }
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
    let end = Pos::new(index, chars.len());
    if column >= left && column < right && error == Some(end) {
        push(" ".to_owned(), Style::new().bg(RED));
        column += 1;
    }
    if current {
        let shown = column.max(left).min(right) - left;
        push(" ".repeat(width - shown), fill);
    }
    Line::from(spans)
}

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_open = app.popup.is_some();
    let Some(editor) = app.containers.editor.as_mut() else {
        return;
    };
    editor.refresh();
    let [panel_area, status_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let cursor = editor.text.cursor();
    let mut title = vec![Span::styled(
        format!(
            " {} ",
            fl!("tui-toml-title", name = editor.container.name.clone())
        ),
        Style::new().fg(CRUST).bg(ACCENT).bold(),
    )];
    if editor.dirty {
        title.push(Span::raw(" "));
        title.push(Span::styled(
            format!(" ● {} ", fl!("tui-code-modified")),
            Style::new().fg(CRUST).bg(YELLOW),
        ));
    }
    if editor.saving {
        title.push(Span::raw(" "));
        title.push(dim(fl!("busy-saving")));
    }
    let validity = match &editor.problem {
        Some(problem) if problem.rejected => {
            Span::styled("✗ kurumi-containerd ", Style::new().fg(PEACH))
        }
        Some(_) => Span::styled(
            format!("✗ {} ", fl!("tui-code-invalid")),
            Style::new().fg(RED),
        ),
        None => Span::styled("✓ TOML ", Style::new().fg(GREEN)),
    };
    let mut position = fl!(
        "tui-code-position",
        line = (cursor.line + 1).to_string(),
        col = (cursor.col + 1).to_string()
    );
    let selected = editor.text.selection_len();
    if selected > 0 {
        position.push_str(&format!("  {}", fl!("tui-code-selected", count = selected)));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(title))
        .title_bottom(Line::from(vec![dim(format!(" {position}  ")), validity]).right_aligned())
        .title_bottom(
            Line::from(Span::styled(
                format!(" {} ", editor.container.file),
                Style::new().fg(SUBTEXT),
            ))
            .left_aligned(),
        );
    let block = if editor.container.running() {
        block.title_top(
            Line::from(Span::styled(
                format!(" ● {} ", fl!("tui-toml-running")),
                Style::new().fg(GREEN),
            ))
            .right_aligned(),
        )
    } else {
        block
    };
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    let count = editor.text.lines().len();
    let digits = count.to_string().len().max(3);
    let gutter = (digits + 2) as u16;
    let [gutter_area, text_area] = Layout::horizontal([
        Constraint::Length(gutter.min(inner.width)),
        Constraint::Min(0),
    ])
    .areas(inner);
    editor.panel = panel_area;
    editor.view = text_area;
    let height = usize::from(text_area.height);
    let width = usize::from(text_area.width);
    editor.scroll_into_view(height, width);

    let error = editor
        .problem
        .as_ref()
        .filter(|p| !p.rejected)
        .and_then(|p| p.pos);
    let mut numbers = Vec::with_capacity(height);
    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let index = editor.top + row;
        if index >= count {
            break;
        }
        let (marker, style) = if error.is_some_and(|e| e.line == index) {
            ("●", Style::new().fg(RED).bold())
        } else if index == cursor.line {
            (" ", Style::new().fg(TEXT).bold().bg(CURRENT_LINE))
        } else {
            (" ", Style::new().fg(DIM))
        };
        numbers.push(Line::from(Span::styled(
            format!("{marker}{:>digits$} ", index + 1),
            style,
        )));
        lines.push(text_line(editor, index, editor.left, width, error));
    }
    frame.render_widget(Paragraph::new(numbers), gutter_area);
    frame.render_widget(Paragraph::new(lines), text_area);

    if count > height && height > 0 {
        let mut state = ScrollbarState::new(count.saturating_sub(height) + 1)
            .position(editor.top)
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

    let status = match &editor.problem {
        Some(problem) => {
            let (icon, color) = if problem.rejected {
                ("✗ kurumi-containerd", PEACH)
            } else {
                ("✗", RED)
            };
            let mut spans = vec![Span::styled(
                format!(" {icon} "),
                Style::new().fg(color).bold(),
            )];
            if let Some(pos) = problem.pos {
                spans.push(dim(format!("{}:{} ", pos.line + 1, pos.col + 1)));
                spans.push(key("F8"));
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(
                problem
                    .message
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                Style::new().fg(color),
            ));
            Line::from(spans)
        }
        None => Line::from(vec![
            Span::styled(" ✓ ", Style::new().fg(GREEN)),
            dim(fl!("tui-toml-status-ok")),
        ]),
    };
    frame.render_widget(Paragraph::new(status), status_area);

    if popup_open {
        return;
    }
    let column = display_col(editor.text.line(cursor.line), cursor.col);
    if cursor.line >= editor.top
        && cursor.line < editor.top + height
        && column >= editor.left
        && column < editor.left + width
    {
        frame.set_cursor_position((
            text_area.x + (column - editor.left) as u16,
            text_area.y + (cursor.line - editor.top) as u16,
        ));
    }
}

pub fn hints() -> Vec<(&'static str, String)> {
    vec![
        ("^S", fl!("key-save")),
        ("^Z/^Y", fl!("key-undo-redo")),
        ("^/", fl!("key-comment")),
        ("F8", fl!("key-problem")),
        ("F10", fl!("key-commands")),
        ("Esc", fl!("key-close-editor")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(line: &str, state: LexState) -> (Vec<(String, Tok)>, LexState) {
        let chars: Vec<char> = line.chars().collect();
        let (runs, next) = lex_line(line, state);
        (
            runs.into_iter()
                .map(|(s, e, t)| (chars[s..e].iter().collect(), t))
                .collect(),
            next,
        )
    }

    fn tok(text: &str, t: Tok) -> (String, Tok) {
        (text.to_owned(), t)
    }

    #[test]
    fn lines_are_highlighted() {
        let start = LexState::default();
        assert_eq!(
            tokens("[container.network_options] # bridge", start).0,
            [
                tok("[container.network_options]", Tok::Table),
                tok("# bridge", Tok::Comment)
            ]
        );
        assert_eq!(
            tokens(r#"name = "dev # not a comment" # but this"#, start).0,
            [
                tok("name", Tok::Key),
                tok("=", Tok::Punct),
                tok(r#""dev # not a comment""#, Tok::String),
                tok("# but this", Tok::Comment)
            ]
        );
        assert_eq!(
            tokens("ports = [{ host = 8080, protocol = 'tcp' }]", start).0,
            [
                tok("ports", Tok::Key),
                tok("=", Tok::Punct),
                tok("[", Tok::Punct),
                tok("{", Tok::Punct),
                tok("host", Tok::Key),
                tok("=", Tok::Punct),
                tok("8080", Tok::Number),
                tok(",", Tok::Punct),
                tok("protocol", Tok::Key),
                tok("=", Tok::Punct),
                tok("'tcp'", Tok::String),
                tok("}", Tok::Punct),
                tok("]", Tok::Punct),
            ]
        );
        assert_eq!(
            tokens("volatile = false", start).0[2],
            tok("false", Tok::Bool)
        );
        assert_eq!(tokens("\"quoted.key\" = -1", start).0[0].1, Tok::Key);
        assert_eq!(
            tokens("\"quoted.key\" = -1", start).0[2],
            tok("-1", Tok::Number)
        );
    }

    #[test]
    fn strings_and_arrays_span_lines() {
        let start = LexState::default();
        let (first, state) = tokens(r#"script = """first"#, start);
        assert_eq!(first[2], tok(r#""""first"#, Tok::String));
        let (middle, state) = tokens("[not a table] = still text", state);
        assert_eq!(middle, [tok("[not a table] = still text", Tok::String)]);
        let (last, state) = tokens(r#"end""" # done"#, state);
        assert_eq!(
            last,
            [tok(r#"end""""#, Tok::String), tok("# done", Tok::Comment)]
        );
        assert_eq!(state, LexState::default());

        let (_, state) = tokens("dns = [", start);
        assert_eq!(state.arrays, 1);
        // Inside the array a leading `[` is a value, not a table header.
        let (inner, state) = tokens("  [1, 2],", state);
        assert_eq!(inner[0], tok("[", Tok::Punct));
        let (_, state) = tokens("]", state);
        assert_eq!(state.arrays, 0);
    }

    #[test]
    fn runtime_errors_point_at_the_text() {
        let message = "the configuration was not saved: kurumi-containerd rejects the configuration: failed to parse TOML config /x/c.toml: TOML parse error at line 19, column 1\n   |\n19 | bogus = 1\n   | ^^^^^\nunknown field `bogus`, expected one of `name`";
        assert_eq!(reported_position(message), Some(Pos::new(18, 0)));
        assert_eq!(
            reason(message),
            "unknown field `bogus`, expected one of `name`"
        );
        assert_eq!(reported_position("bind source does not exist: /srv"), None);
        assert_eq!(
            reason("bind source does not exist: /srv"),
            "bind source does not exist: /srv"
        );
    }

    #[test]
    fn problems_point_at_the_text() {
        let container = Container {
            id: "ab12cd34".into(),
            name: "dev".into(),
            file: "/var/lib/singbox-board/containers/ab12cd34/container.toml".into(),
            managed: true,
            autostart: false,
            created_at: 0,
            updated_at: 0,
            state: crate::protocol::CoreState::Stopped,
            busy: None,
            last_error: None,
            spec: None,
            spec_error: None,
            live: None,
        };
        let mut editor = TomlEditor::new(
            container,
            "[runtime]\n[container]\nname = \"dev\"\nrootfs = \"./rootfs\"\n",
        );
        editor.refresh();
        assert!(editor.problem.is_none() && !editor.dirty);
        editor
            .text
            .set_text("[runtime]\n[container\nname = \"dev\"\n");
        let problem = editor.syntax_error().unwrap();
        assert_eq!(problem.pos, Some(Pos::new(1, 10)));
        assert!(editor.dirty());
        editor
            .text
            .set_text("[runtime]\n[container]\nname = \"dev\"\n");
        let problem = editor.syntax_error().unwrap();
        assert_eq!(problem.pos, None);
        assert_eq!(problem.message, fl!("containers-config-rootfs"));
    }
}
