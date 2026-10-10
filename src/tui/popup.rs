//! State and key handling of the popups that collect input: a single-line
//! text field and a menu. Drawing lives in `ui`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use super::code::CodeCommand;
use super::containers::{ContainerMenu, ContainersGlobal};
use super::editor::{InsertTarget, Path};
use super::toml_editor::TomlCommand;
use crate::protocol::{Component, ComponentAction};

/// What a text field is for.
#[derive(Debug, Clone, PartialEq)]
pub enum InputPurpose {
    AddSource,
    ImportCore,
    NewProfile,
    ImportProfile,
    RenameProfile(String),
    ProfileUrl(String),
    ProfileInterval(String),
    ExportProfile(String),
    /// A scalar of the profile open in the tree view.
    EditValue(Path),
    RenameKey(Path),
    /// Name of a new object member; its value is chosen next.
    NewKey(InsertTarget),
    Search,
    /// The filter of the connections; holds the one to restore on Esc.
    ConnectionFilter(String),
    NewContainer,
    /// The path of a kurumi-containerd TOML file to register in place.
    RegisterContainer,
    RenameContainer(String),
    /// A root filesystem archive or URL for a container.
    ContainerInstall(String),
    /// A command to run in a container.
    ContainerExec(String),
    /// A field drawn inside a view (the code editor's find bar), never
    /// submitted as a popup.
    Inline,
}

pub struct Input {
    pub title: String,
    pub hint: String,
    pub placeholder: String,
    pub value: String,
    /// Cursor position in characters.
    pub cursor: usize,
    pub error: Option<String>,
    pub purpose: InputPurpose,
}

pub enum InputOutcome {
    Editing,
    Submit,
    Cancel,
}

impl Input {
    pub fn new(title: String, hint: String, purpose: InputPurpose) -> Self {
        Self {
            title,
            hint,
            placeholder: String::new(),
            value: String::new(),
            cursor: 0,
            error: None,
            purpose,
        }
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self.cursor = self.value.chars().count();
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    fn byte(&self, chars: usize) -> usize {
        self.value
            .char_indices()
            .nth(chars)
            .map_or(self.value.len(), |(i, _)| i)
    }

    /// The text before and after the cursor.
    pub fn split(&self) -> (&str, &str) {
        self.value.split_at(self.byte(self.cursor))
    }

    /// Puts the cursor on the character drawn `column` columns after the
    /// start of the text, or after the text.
    pub fn click(&mut self, column: u16) {
        let mut width = 0;
        self.cursor = self
            .value
            .chars()
            .position(|c| {
                width += c.width().unwrap_or(0);
                width > usize::from(column)
            })
            .unwrap_or_else(|| self.value.chars().count());
    }

    /// Inserts pasted text; line breaks become spaces in a one-line field.
    pub fn paste(&mut self, text: &str) {
        let text: String = text
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        let at = self.byte(self.cursor);
        self.value.insert_str(at, &text);
        self.cursor += text.chars().count();
        self.error = None;
    }

    pub fn on_key(&mut self, key: KeyEvent) -> InputOutcome {
        let len = self.value.chars().count();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return InputOutcome::Submit,
            KeyCode::Esc => return InputOutcome::Cancel,
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Char('u') if ctrl => {
                let at = self.byte(self.cursor);
                self.value.replace_range(..at, "");
                self.cursor = 0;
            }
            KeyCode::Char('k') if ctrl => {
                let at = self.byte(self.cursor);
                self.value.truncate(at);
            }
            KeyCode::Char('w') if ctrl => {
                let (before, _) = self.split();
                let trimmed = before.trim_end();
                let start = trimmed
                    .char_indices()
                    .rfind(|(_, c)| c.is_whitespace() || "/.:,".contains(*c))
                    .map_or(0, |(i, c)| i + c.len_utf8());
                let removed = before[start..].chars().count();
                let end = self.byte(self.cursor);
                self.value.replace_range(start..end, "");
                self.cursor -= removed;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let start = self.byte(self.cursor - 1);
                let end = self.byte(self.cursor);
                self.value.replace_range(start..end, "");
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let start = self.byte(self.cursor);
                let end = self.byte(self.cursor + 1);
                self.value.replace_range(start..end, "");
            }
            KeyCode::Char(c) if !ctrl => {
                let at = self.byte(self.cursor);
                self.value.insert(at, c);
                self.cursor += 1;
            }
            _ => return InputOutcome::Editing,
        }
        self.error = None;
        InputOutcome::Editing
    }
}

/// What a profile action menu entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileAction {
    Use,
    Edit,
    EditExternal,
    Update,
    Rename,
    SetUrl,
    SetInterval,
    MakeLocal,
    Duplicate,
    Check,
    Export,
    CopyUrl,
    Delete,
}

/// A profile handed to `$VISUAL` or `$EDITOR`.
#[derive(Debug, Clone)]
pub struct ExternalEdit {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub text: String,
    /// A container's TOML configuration rather than a profile.
    pub container: bool,
}

impl InputPurpose {
    /// Fields of the Containers tab.
    pub fn for_containers(&self) -> bool {
        matches!(
            self,
            InputPurpose::NewContainer
                | InputPurpose::RegisterContainer
                | InputPurpose::RenameContainer(_)
                | InputPurpose::ContainerInstall(_)
                | InputPurpose::ContainerExec(_)
        )
    }
}

#[derive(Debug, Clone)]
pub enum MenuAction {
    Component(Component, ComponentAction),
    Profile(String, ProfileAction),
    /// Add a node in the editor; `edit` opens the value for typing.
    Insert {
        target: InsertTarget,
        key: Option<String>,
        value: Value,
        edit: bool,
    },
    /// Ask for the name of a new object member.
    CustomKey(InsertTarget),
    /// Set a scalar in the editor.
    SetValue(Path, Value),
    /// Type the value instead of picking one.
    TypeValue(Path),
    /// Store content sing-box rejected.
    SaveAnyway {
        id: String,
        content: String,
        from_editor: bool,
    },
    EditAgain(ExternalEdit),
    /// A command of the code editor.
    Code(CodeCommand),
    Container(String, ContainerMenu),
    ContainersGlobal(ContainersGlobal),
    /// Create a container with this network.
    ContainerNetwork {
        name: String,
        network: String,
    },
    /// Install an image (`distro/release`) as a container's root filesystem.
    ContainerImage {
        id: String,
        image: String,
    },
    ContainerRemove {
        id: String,
        purge: bool,
    },
    /// Store a configuration kurumi-containerd rejected.
    ContainerSaveAnyway {
        id: String,
        content: String,
    },
    /// A command of the configuration editor.
    Toml(TomlCommand),
    Dismiss,
}

impl MenuAction {
    /// Entries of the Containers tab and its editor.
    pub fn for_containers(&self) -> bool {
        matches!(
            self,
            MenuAction::Container(..)
                | MenuAction::ContainersGlobal(_)
                | MenuAction::ContainerNetwork { .. }
                | MenuAction::ContainerImage { .. }
                | MenuAction::ContainerRemove { .. }
                | MenuAction::ContainerSaveAnyway { .. }
                | MenuAction::Toml(_)
        )
    }
}

pub struct MenuItem {
    pub label: String,
    pub detail: String,
    pub action: MenuAction,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, detail: impl Into<String>, action: MenuAction) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            action,
        }
    }
}

pub struct Menu {
    pub title: String,
    /// Text above the entries, e.g. why the menu is shown.
    pub body: Option<String>,
    pub items: Vec<MenuItem>,
    pub selected: usize,
}

pub enum MenuOutcome {
    Open,
    Chosen(MenuAction),
    Cancel,
}

impl Menu {
    pub fn new(title: String, items: Vec<MenuItem>) -> Self {
        Self {
            title,
            body: None,
            items,
            selected: 0,
        }
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn on_key(&mut self, key: KeyEvent) -> MenuOutcome {
        let last = self.items.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(last),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = last,
            KeyCode::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if let Some(item) = self.items.get(index) {
                    return MenuOutcome::Chosen(item.action.clone());
                }
            }
            KeyCode::Enter => {
                if let Some(item) = self.items.get(self.selected) {
                    return MenuOutcome::Chosen(item.action.clone());
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => return MenuOutcome::Cancel,
            _ => {}
        }
        MenuOutcome::Open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut Input, code: KeyCode) {
        input.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn text_field_edits_at_the_cursor() {
        let mut input =
            Input::new(String::new(), String::new(), InputPurpose::Search).value("配置a");
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Char('文'));
        assert_eq!(input.value, "配置文a");
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Delete);
        assert_eq!(input.split(), ("", "置文a"));
        press(&mut input, KeyCode::End);
        press(&mut input, KeyCode::Backspace);
        input.paste("x\ny");
        assert_eq!(input.value, "置文x y");
        input.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(input.value, "置文x ");
        input.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(input.value.is_empty() && input.cursor == 0);
        // Words end at any space, full-width ones too.
        input.paste("a\u{3000}b");
        input.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(input.value, "a\u{3000}");
    }

    #[test]
    fn clicks_put_the_cursor_on_a_character() {
        let mut input =
            Input::new(String::new(), String::new(), InputPurpose::Search).value("a配置b");
        input.click(0);
        assert_eq!(input.cursor, 0);
        // Both columns of a wide character are that character.
        input.click(2);
        assert_eq!(input.cursor, 1);
        input.click(3);
        assert_eq!(input.cursor, 2);
        input.click(5);
        assert_eq!(input.cursor, 3);
        input.click(40);
        assert_eq!(input.cursor, 4);
    }
}
