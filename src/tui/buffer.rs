//! The editor's text, kept by ratatui-textarea (lines, cursor, selection,
//! undo history, yank buffer), with the commands a JSON editor adds on top
//! of its API: closing brackets and quotes, keeping the indentation, line
//! and comment operations, and search with the regex crate.
//!
//! ratatui-textarea records every primitive edit as its own undo step. A
//! command made of several (formatting, moving lines, a pasted selection)
//! remembers a fingerprint of the text before and after, and undo and redo
//! walk the library's history until they reach it.

use std::hash::{DefaultHasher, Hash, Hasher};

use ratatui_textarea::{CursorMove, TextArea};
use regex::{NoExpand, Regex, RegexBuilder};
use unicode_segmentation::UnicodeSegmentation;

use super::jsonc::Pos;

const HISTORY: usize = 1000;
/// Most library steps one command is undone with.
const MAX_GROUP: usize = 10_000;
/// Columns ratatui-textarea cannot jump to (beyond `u16`) are walked to,
/// one step at a time, at most this far.
const WALK: usize = 256;

/// Fingerprints of the text around a command of several library edits.
#[derive(Debug, Clone, Copy)]
struct Group {
    before: u64,
    after: u64,
}

pub struct Buffer {
    area: TextArea<'static>,
    /// Changes with every edit, undo and redo.
    version: u64,
    undo_groups: Vec<Group>,
    redo_groups: Vec<Group>,
    /// One level of indentation: spaces or a tab.
    pub indent: String,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let lines: Vec<String> = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .split('\n')
            .map(str::to_owned)
            .collect();
        let indent = detect_indent(&lines);
        let mut area = TextArea::new(lines);
        area.set_max_histories(HISTORY);
        if indent == "\t" {
            area.set_hard_tab_indent(true);
        } else {
            area.set_tab_length(indent.len() as u8);
        }
        Self {
            area,
            version: 0,
            undo_groups: Vec::new(),
            redo_groups: Vec::new(),
            indent,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn lines(&self) -> &[String] {
        self.area.lines()
    }

    pub fn line(&self, index: usize) -> &str {
        self.lines().get(index).map_or("", String::as_str)
    }

    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    pub fn cursor(&self) -> Pos {
        let cursor = self.area.cursor();
        Pos::new(cursor.0, cursor.1)
    }

    /// The selected range in document order; `None` when nothing is.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let ((r1, c1), (r2, c2)) = self.area.selection_range()?;
        let (start, end) = (Pos::new(r1, c1), Pos::new(r2, c2));
        (start != end).then_some((start, end))
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(start, end)| self.slice(start, end))
    }

    /// Characters selected, line breaks included.
    pub fn selection_len(&self) -> usize {
        let Some((start, end)) = self.selection() else {
            return 0;
        };
        if start.line == end.line {
            return end.col - start.col;
        }
        let middle: usize = self.lines()[start.line + 1..end.line]
            .iter()
            .map(|l| l.chars().count() + 1)
            .sum();
        self.line_len(start.line) - start.col + 1 + middle + end.col
    }

    fn line_len(&self, line: usize) -> usize {
        self.line(line).chars().count()
    }

    pub fn clamp(&self, pos: Pos) -> Pos {
        let line = pos.line.min(self.lines().len() - 1);
        Pos::new(line, pos.col.min(self.line_len(line)))
    }

    pub fn char_at(&self, pos: Pos) -> Option<char> {
        self.line(pos.line).chars().nth(pos.col)
    }

    fn char_before(&self, pos: Pos) -> Option<char> {
        let col = pos.col.checked_sub(1)?;
        self.line(pos.line).chars().nth(col)
    }

    /// The text between two positions, lines joined with `\n`.
    pub fn slice(&self, start: Pos, end: Pos) -> String {
        let (start, end) = (self.clamp(start), self.clamp(end));
        let first = self.line(start.line);
        if start.line == end.line {
            return first[byte(first, start.col)..byte(first, end.col)].to_owned();
        }
        let mut out = first[byte(first, start.col)..].to_owned();
        for line in &self.lines()[start.line + 1..end.line] {
            out.push('\n');
            out.push_str(line);
        }
        let last = self.line(end.line);
        out.push('\n');
        out.push_str(&last[..byte(last, end.col)]);
        out
    }

    fn indent_width(&self) -> usize {
        if self.indent == "\t" {
            1
        } else {
            self.indent.len().max(1)
        }
    }

    fn bump(&mut self, changed: bool) {
        if changed {
            self.version += 1;
            // The library dropped its redo steps.
            self.redo_groups.clear();
        }
    }

    fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.lines().hash(&mut hasher);
        hasher.finish()
    }

    /// Runs a command of several library edits as one undo step.
    fn grouped(&mut self, edit: impl FnOnce(&mut Self)) {
        let before = self.fingerprint();
        edit(self);
        let after = self.fingerprint();
        if before != after {
            self.bump(true);
            self.undo_groups.push(Group { before, after });
        }
    }

    // ----- movement -----------------------------------------------------------

    /// A ratatui-textarea move that extends the selection or drops it.
    pub fn go(&mut self, movement: CursorMove, extend: bool) {
        if extend {
            if !self.area.is_selecting() {
                self.area.start_selection();
            }
        } else {
            self.area.cancel_selection();
        }
        self.area.move_cursor(movement);
    }

    /// Puts the cursor at `pos`, extending the selection or dropping it.
    pub fn set_cursor(&mut self, pos: Pos, extend: bool) {
        let pos = self.clamp(pos);
        if extend {
            if !self.area.is_selecting() {
                self.area.start_selection();
            }
        } else {
            self.area.cancel_selection();
        }
        // ratatui-textarea jumps within u16; walk the rest of the way.
        let max = usize::from(u16::MAX);
        self.area.move_cursor(CursorMove::Jump(
            pos.line.min(max) as u16,
            pos.col.min(max) as u16,
        ));
        if pos.line > max {
            for _ in max..pos.line {
                self.area.move_cursor(CursorMove::Down);
            }
            self.area.move_cursor(CursorMove::Head);
            for _ in 0..pos.col.min(WALK) {
                self.area.move_cursor(CursorMove::Forward);
            }
        } else if pos.col > max {
            for _ in max..pos.col.min(max + WALK) {
                self.area.move_cursor(CursorMove::Forward);
            }
        }
    }

    /// Selects `start..end` with the cursor at `end`.
    pub fn select(&mut self, start: Pos, end: Pos) {
        self.set_cursor(start, false);
        self.area.start_selection();
        self.set_cursor(end, true);
    }

    pub fn select_all(&mut self) {
        self.area.select_all();
    }

    /// Drops the selection; whether there was one.
    pub fn clear_selection(&mut self) -> bool {
        let had = self.selection().is_some();
        self.area.cancel_selection();
        had
    }

    /// Selects the word under `pos` (double click), by Unicode word bounds.
    pub fn select_word(&mut self, pos: Pos) {
        let pos = self.clamp(pos);
        let line = self.line(pos.line).to_owned();
        let at = byte(&line, pos.col);
        let found = line
            .split_word_bound_indices()
            .find(|(start, word)| *start <= at && at < start + word.len())
            .or_else(|| line.split_word_bound_indices().next_back());
        match found {
            Some((start, word)) => {
                let from = line[..start].chars().count();
                let to = from + word.chars().count();
                self.select(Pos::new(pos.line, from), Pos::new(pos.line, to));
            }
            None => self.set_cursor(pos, false),
        }
    }

    pub fn left(&mut self, extend: bool, word: bool) {
        if !extend && let Some((start, _)) = self.selection() {
            self.set_cursor(start, false);
            return;
        }
        let movement = if word {
            CursorMove::WordBack
        } else {
            CursorMove::Back
        };
        self.go(movement, extend);
    }

    pub fn right(&mut self, extend: bool, word: bool) {
        if !extend && let Some((_, end)) = self.selection() {
            self.set_cursor(end, false);
            return;
        }
        let movement = if word {
            CursorMove::WordForward
        } else {
            CursorMove::Forward
        };
        self.go(movement, extend);
    }

    pub fn vertical(&mut self, lines: isize, extend: bool) {
        let movement = if lines < 0 {
            CursorMove::Up
        } else {
            CursorMove::Down
        };
        for _ in 0..lines.unsigned_abs() {
            self.go(movement, extend);
        }
    }

    /// To the first non-blank character, or the line start when already there.
    pub fn home(&mut self, extend: bool) {
        let cursor = self.cursor();
        let first = self
            .line(cursor.line)
            .chars()
            .take_while(|c| c.is_whitespace())
            .count();
        let col = if cursor.col == first { 0 } else { first };
        self.set_cursor(Pos::new(cursor.line, col), extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.go(CursorMove::End, extend);
    }

    pub fn doc_start(&mut self, extend: bool) {
        self.set_cursor(Pos::default(), extend);
    }

    pub fn doc_end(&mut self, extend: bool) {
        let last = self.lines().len() - 1;
        self.set_cursor(Pos::new(last, self.line_len(last)), extend);
    }

    // ----- typing -------------------------------------------------------------

    /// Types a character: replaces the selection or wraps it in quotes or
    /// brackets, pairs `{`, `[` and `"`, steps over a closer that is
    /// already there and outdents a closer typed on an empty line.
    pub fn type_char(&mut self, c: char) {
        if c == '\n' {
            self.newline();
            return;
        }
        if let Some((start, end)) = self.selection() {
            self.grouped(|b| match closer(c) {
                Some(close) => {
                    b.set_cursor(end, false);
                    b.area.insert_char(close);
                    b.set_cursor(start, false);
                    b.area.insert_char(c);
                    let shift = usize::from(start.line == end.line);
                    b.select(
                        Pos::new(start.line, start.col + 1),
                        Pos::new(end.line, end.col + shift),
                    );
                }
                None => b.area.insert_char(c),
            });
            return;
        }
        // An empty selection must not swallow the character.
        self.area.cancel_selection();
        let at = self.cursor();
        let next = self.char_at(at);
        let (in_string, escaped) = string_state(self.line(at.line), at.col);
        let steps_over = if c == '"' {
            in_string && !escaped && next == Some('"')
        } else {
            matches!(c, '}' | ']') && next == Some(c)
        };
        if steps_over {
            self.area.move_cursor(CursorMove::Forward);
            return;
        }
        let free = next.is_none_or(|n| n.is_whitespace() || matches!(n, ',' | '}' | ']' | ':'));
        let word_before = self
            .char_before(at)
            .is_some_and(|p| p.is_alphanumeric() || p == '_');
        let pair = match c {
            '{' | '[' if free && !in_string => closer(c),
            '"' if free && !in_string && !word_before => Some('"'),
            _ => None,
        };
        let blank_before = self
            .line(at.line)
            .chars()
            .take(at.col)
            .all(|ch| ch == ' ' || ch == '\t');
        let dedent = matches!(c, '}' | ']') && blank_before && at.col > 0;
        if !dedent && pair.is_none() {
            self.area.insert_char(c);
            self.bump(true);
            return;
        }
        let unit = self.indent_width();
        self.grouped(|b| {
            if dedent {
                // Back to the level of the opening line.
                let remove = unit.min(at.col);
                b.set_cursor(Pos::new(at.line, at.col - remove), false);
                b.area.delete_str(remove);
            }
            match pair {
                Some(close) => {
                    b.area.insert_str(format!("{c}{close}"));
                    b.area.move_cursor(CursorMove::Back);
                }
                None => b.area.insert_char(c),
            }
        });
    }

    /// Breaks the line, keeping the indentation and opening a new level
    /// after `{` or `[`; between a pair the closer moves to its own line.
    pub fn newline(&mut self) {
        self.grouped(|b| {
            if b.selection().is_some() {
                b.area.delete_char();
            }
            b.area.cancel_selection();
            let at = b.cursor();
            let prefix: String = b.line(at.line).chars().take(at.col).collect();
            let indent: String = prefix
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let opens = prefix.trim_end().ends_with(['{', '[']);
            let closes = matches!(b.char_at(at), Some('}' | ']'));
            b.area.insert_newline();
            if opens {
                b.area.insert_str(format!("{indent}{}", b.indent));
                if closes {
                    let inside = b.cursor();
                    b.area.insert_newline();
                    b.area.insert_str(&indent);
                    b.set_cursor(inside, false);
                }
            } else if !indent.is_empty() {
                b.area.insert_str(&indent);
            }
        });
    }

    /// Inserts text as is (a paste), replacing the selection.
    pub fn insert_text(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.grouped(|b| {
            b.area.insert_str(text);
        });
    }

    /// Replaces the whole document, keeping the cursor where it was as far
    /// as possible.
    pub fn set_text(&mut self, text: &str) {
        if self.text() == text {
            return;
        }
        let keep = self.cursor();
        self.grouped(|b| {
            b.area.select_all();
            if text.is_empty() {
                b.area.delete_char();
            } else {
                b.area.insert_str(text);
            }
            b.set_cursor(keep, false);
        });
    }

    pub fn backspace(&mut self) {
        if self.selection().is_some() {
            let changed = self.area.delete_char();
            self.bump(changed);
            return;
        }
        self.area.cancel_selection();
        let at = self.cursor();
        if at.col > 0 {
            let paired = matches!(
                (self.char_before(at), self.char_at(at)),
                (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('"'), Some('"'))
            );
            if paired {
                self.grouped(|b| {
                    b.area.delete_next_char();
                    b.area.delete_char();
                });
                return;
            }
            let blank = self.line(at.line).chars().take(at.col).all(|c| c == ' ');
            if self.indent != "\t" && blank {
                let unit = self.indent_width();
                let to = (at.col - 1) / unit * unit;
                self.set_cursor(Pos::new(at.line, to), false);
                self.area.delete_str(at.col - to);
                self.bump(true);
                return;
            }
        }
        let changed = self.area.delete_char();
        self.bump(changed);
    }

    pub fn delete(&mut self) {
        let changed = if self.selection().is_some() {
            self.area.delete_char()
        } else {
            self.area.cancel_selection();
            self.area.delete_next_char()
        };
        self.bump(changed);
    }

    pub fn delete_word_left(&mut self) {
        let changed = if self.selection().is_some() {
            self.area.delete_char()
        } else {
            self.area.cancel_selection();
            self.area.delete_word()
        };
        self.bump(changed);
    }

    pub fn delete_word_right(&mut self) {
        let changed = if self.selection().is_some() {
            self.area.delete_char()
        } else {
            self.area.cancel_selection();
            self.area.delete_next_word()
        };
        self.bump(changed);
    }

    pub fn undo(&mut self) -> bool {
        let now = self.fingerprint();
        let group = self.undo_groups.last().copied().filter(|g| g.after == now);
        if !self.area.undo() {
            return false;
        }
        if let Some(group) = group {
            let mut steps = 1;
            while steps < MAX_GROUP && self.fingerprint() != group.before && self.area.undo() {
                steps += 1;
            }
            self.undo_groups.pop();
            self.redo_groups.push(group);
        }
        self.version += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        let now = self.fingerprint();
        let group = self.redo_groups.last().copied().filter(|g| g.before == now);
        if !self.area.redo() {
            return false;
        }
        if let Some(group) = group {
            let mut steps = 1;
            while steps < MAX_GROUP && self.fingerprint() != group.after && self.area.redo() {
                steps += 1;
            }
            self.redo_groups.pop();
            self.undo_groups.push(group);
        }
        self.version += 1;
        true
    }

    // ----- lines --------------------------------------------------------------

    /// First and last line the cursor or selection touches; a selection
    /// ending at the start of a line does not include that line.
    fn line_span(&self) -> (usize, usize) {
        match self.selection() {
            Some((start, end)) if end.col == 0 && end.line > start.line => {
                (start.line, end.line - 1)
            }
            Some((start, end)) => (start.line, end.line),
            None => (self.cursor().line, self.cursor().line),
        }
    }

    /// Runs line edits, then puts the cursor and selection back, moved by
    /// `adjust`.
    fn keep_selection(&mut self, edit: impl FnOnce(&mut Self), adjust: impl Fn(Pos) -> Pos) {
        let cursor = self.cursor();
        let anchor = self
            .selection()
            .map(|(start, end)| if start == cursor { end } else { start });
        self.grouped(|b| {
            edit(b);
            match anchor {
                Some(anchor) => {
                    b.set_cursor(adjust(anchor), false);
                    b.area.start_selection();
                    b.set_cursor(adjust(cursor), true);
                }
                None => b.set_cursor(adjust(cursor), false),
            }
        });
    }

    /// Tab: indents the selected lines, or inserts indentation up to the
    /// next stop.
    pub fn tab(&mut self) {
        if self
            .selection()
            .is_some_and(|(start, end)| end.line > start.line)
        {
            self.indent_lines();
        } else {
            self.grouped(|b| {
                b.area.insert_tab();
            });
        }
    }

    pub fn indent_lines(&mut self) {
        let (first, last) = self.line_span();
        let unit = self.indent.clone();
        let width = unit.chars().count();
        let filled: Vec<usize> = (first..=last)
            .filter(|&l| !self.line(l).trim().is_empty())
            .collect();
        self.keep_selection(
            |b| {
                for &line in &filled {
                    b.set_cursor(Pos::new(line, 0), false);
                    b.area.insert_str(&unit);
                }
            },
            |pos| {
                if filled.contains(&pos.line) && pos.col > 0 {
                    Pos::new(pos.line, pos.col + width)
                } else {
                    pos
                }
            },
        );
    }

    pub fn outdent_lines(&mut self) {
        let (first, last) = self.line_span();
        let unit = self.indent_width();
        let removed: Vec<(usize, usize)> = (first..=last)
            .map(|l| {
                let text = self.line(l);
                let n = if text.starts_with('\t') {
                    1
                } else {
                    text.chars().take(unit).take_while(|c| *c == ' ').count()
                };
                (l, n)
            })
            .filter(|(_, n)| *n > 0)
            .collect();
        if removed.is_empty() {
            return;
        }
        self.keep_selection(
            |b| {
                for &(line, n) in &removed {
                    b.set_cursor(Pos::new(line, 0), false);
                    b.area.delete_str(n);
                }
            },
            |pos| match removed.iter().find(|(l, _)| *l == pos.line) {
                Some((_, n)) => Pos::new(pos.line, pos.col.saturating_sub(*n)),
                None => pos,
            },
        );
    }

    /// Copies the touched lines below themselves.
    pub fn duplicate_lines(&mut self) {
        let (first, last) = self.line_span();
        let block = self.lines()[first..=last].join("\n");
        let count = last - first + 1;
        let end = Pos::new(last, self.line_len(last));
        self.keep_selection(
            |b| {
                b.set_cursor(end, false);
                b.area.insert_str(format!("\n{block}"));
            },
            |pos| Pos::new(pos.line + count, pos.col),
        );
    }

    /// Removes the touched lines.
    pub fn delete_lines(&mut self) {
        let (first, last) = self.line_span();
        let col = self.cursor().col;
        let count = self.lines().len();
        let (start, end) = if last + 1 < count {
            (Pos::new(first, 0), Pos::new(last + 1, 0))
        } else if first > 0 {
            (
                Pos::new(first - 1, self.line_len(first - 1)),
                Pos::new(last, self.line_len(last)),
            )
        } else {
            (Pos::default(), Pos::new(last, self.line_len(last)))
        };
        self.grouped(|b| {
            b.select(start, end);
            b.area.delete_char();
            let line = first.min(b.lines().len() - 1);
            b.set_cursor(Pos::new(line, col), false);
        });
    }

    /// Moves the touched lines up (`-1`) or down (`1`) past their neighbour.
    pub fn move_lines(&mut self, delta: isize) {
        let (first, last) = self.line_span();
        let up = delta < 0;
        if up && first == 0 || !up && last + 1 >= self.lines().len() {
            return;
        }
        let neighbour = if up { first - 1 } else { last + 1 };
        let moved = self.line(neighbour).to_owned();
        let (last_len, neighbour_len) = (self.line_len(last), self.line_len(neighbour));
        self.keep_selection(
            |b| {
                if up {
                    // The line above goes out and comes back below the span.
                    b.select(Pos::new(neighbour, 0), Pos::new(first, 0));
                    b.area.delete_char();
                    let end = Pos::new(last - 1, b.line_len(last - 1));
                    b.set_cursor(end, false);
                    b.area.insert_str(format!("\n{moved}"));
                } else {
                    // The line below goes out and comes back above the span.
                    b.select(Pos::new(last, last_len), Pos::new(neighbour, neighbour_len));
                    b.area.delete_char();
                    b.set_cursor(Pos::new(first, 0), false);
                    b.area.insert_str(format!("{moved}\n"));
                }
            },
            // set_cursor clamps what runs past the end.
            |pos| {
                let line = if up {
                    pos.line.saturating_sub(1)
                } else {
                    pos.line + 1
                };
                Pos::new(line, pos.col)
            },
        );
    }

    /// Comments the touched lines out with `//`, or back in when all of
    /// them are comments.
    pub fn toggle_comment(&mut self) {
        let (first, last) = self.line_span();
        let filled: Vec<usize> = (first..=last)
            .filter(|&l| !self.line(l).trim().is_empty())
            .collect();
        if filled.is_empty() {
            return;
        }
        let uncomment = filled
            .iter()
            .all(|&l| self.line(l).trim_start().starts_with("//"));
        let edits: Vec<(usize, usize, usize)> = filled
            .iter()
            .map(|&l| {
                let text = self.line(l);
                let at = text.chars().take_while(|c| c.is_whitespace()).count();
                if uncomment {
                    let space = text.chars().nth(at + 2) == Some(' ');
                    (l, at, if space { 3 } else { 2 })
                } else {
                    (l, 0, 0)
                }
            })
            .collect();
        let column = filled
            .iter()
            .map(|&l| {
                self.line(l)
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .count()
            })
            .min()
            .unwrap_or(0);
        self.keep_selection(
            |b| {
                for &(line, at, remove) in &edits {
                    if uncomment {
                        b.set_cursor(Pos::new(line, at), false);
                        b.area.delete_str(remove);
                    } else {
                        b.set_cursor(Pos::new(line, column), false);
                        b.area.insert_str("// ");
                    }
                }
            },
            |pos| match edits.iter().find(|(l, _, _)| *l == pos.line) {
                Some(&(_, at, remove)) if uncomment && pos.col >= at => {
                    Pos::new(pos.line, pos.col.saturating_sub(remove).max(at))
                }
                Some(_) if !uncomment && pos.col >= column => Pos::new(pos.line, pos.col + 3),
                _ => pos,
            },
        );
    }

    // ----- clipboard ------------------------------------------------------------

    /// Copies the selection, or the cursor line when nothing is selected.
    pub fn copy(&mut self) -> String {
        match self.selection() {
            Some(_) => {
                self.area.copy();
                self.area.yank_text()
            }
            None => format!("{}\n", self.line(self.cursor().line)),
        }
    }

    /// Cuts the selection, or the cursor line when nothing is selected.
    pub fn cut(&mut self) -> String {
        if self.selection().is_none() {
            let text = self.copy();
            self.delete_lines();
            return text;
        }
        self.area.cut();
        self.bump(true);
        self.area.yank_text()
    }

    /// Pastes `text` through ratatui-textarea's yank buffer.
    pub fn paste(&mut self, text: &str) {
        self.area.set_yank_text(text.replace("\r\n", "\n"));
        self.grouped(|b| {
            b.area.paste();
        });
    }

    // ----- search -----------------------------------------------------------------

    /// Every match of `query` taken literally, as character positions.
    pub fn matches(&self, query: &str, case: bool) -> Vec<(Pos, Pos)> {
        let Some(pattern) = literal(query, case) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for (index, line) in self.lines().iter().enumerate() {
            let (mut byte_at, mut col) = (0, 0);
            for m in pattern.find_iter(line).filter(|m| !m.is_empty()) {
                col += line[byte_at..m.start()].chars().count();
                let start = col;
                col += m.as_str().chars().count();
                byte_at = m.end();
                found.push((Pos::new(index, start), Pos::new(index, col)));
            }
        }
        found
    }

    /// The next match after the selection or cursor (or the previous one
    /// before it), wrapping around.
    pub fn find(&self, query: &str, case: bool, backwards: bool) -> Option<(Pos, Pos)> {
        let cursor = self.cursor();
        let (start, end) = self.selection().unwrap_or((cursor, cursor));
        self.find_from(query, case, if backwards { start } else { end }, backwards)
    }

    /// The first match starting at or after `from` (or the last one
    /// starting before it), wrapping around.
    pub fn find_from(
        &self,
        query: &str,
        case: bool,
        from: Pos,
        backwards: bool,
    ) -> Option<(Pos, Pos)> {
        let all = self.matches(query, case);
        let found = if backwards {
            all.iter().rev().find(|(s, _)| *s < from).or(all.last())
        } else {
            all.iter().find(|(s, _)| *s >= from).or(all.first())
        };
        found.copied()
    }

    /// Replaces the selection when it is a match; whether it was.
    pub fn replace_selection(&mut self, query: &str, replacement: &str, case: bool) -> bool {
        let Some(selected) = self.selected_text() else {
            return false;
        };
        let whole = literal(query, case)
            .and_then(|p| p.find(&selected))
            .is_some_and(|m| m.start() == 0 && m.end() == selected.len());
        if !whole {
            return false;
        }
        self.grouped(|b| {
            if replacement.is_empty() {
                b.area.delete_char();
            } else {
                b.area.insert_str(replacement);
            }
        });
        true
    }

    /// Replaces every match; returns how many were replaced.
    pub fn replace_all(&mut self, query: &str, replacement: &str, case: bool) -> usize {
        let Some(pattern) = literal(query, case) else {
            return 0;
        };
        let text = self.text();
        let count = pattern.find_iter(&text).filter(|m| !m.is_empty()).count();
        if count > 0 {
            let replaced = pattern.replace_all(&text, NoExpand(replacement));
            self.set_text(&replaced);
        }
        count
    }
}

/// A case-sensitive or -insensitive pattern matching `query` literally.
fn literal(query: &str, case: bool) -> Option<Regex> {
    if query.is_empty() {
        return None;
    }
    RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!case)
        .build()
        .ok()
}

fn closer(c: char) -> Option<char> {
    match c {
        '{' => Some('}'),
        '[' => Some(']'),
        '"' => Some('"'),
        _ => None,
    }
}

/// Whether column `col` of `line` is inside a string (after an odd number
/// of unescaped quotes), and right after a backslash in it.
fn string_state(line: &str, col: usize) -> (bool, bool) {
    let mut inside = false;
    let mut escaped = false;
    for c in line.chars().take(col) {
        match c {
            _ if escaped => escaped = false,
            '\\' if inside => escaped = true,
            '"' => inside = !inside,
            _ => {}
        }
    }
    (inside, escaped)
}

/// Byte offset of a character column.
fn byte(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

/// The indentation unit a document uses: a tab, or the smallest step of
/// leading spaces (2 when there is none).
fn detect_indent(lines: &[String]) -> String {
    if lines.iter().any(|l| l.starts_with('\t')) {
        return "\t".to_owned();
    }
    let step = lines
        .iter()
        .map(|l| l.chars().take_while(|c| *c == ' ').count())
        .filter(|n| *n > 0)
        .min()
        .unwrap_or(2)
        .clamp(1, 8);
    " ".repeat(step)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> Buffer {
        Buffer::new(text)
    }

    fn typed(b: &mut Buffer, text: &str) {
        for c in text.chars() {
            b.type_char(c);
        }
    }

    #[test]
    fn typing_pairs_and_steps_over() {
        let mut b = buffer("");
        typed(&mut b, "{\"a\": [1, 2]}");
        assert_eq!(b.text(), "{\"a\": [1, 2]}");
        assert_eq!(b.cursor(), Pos::new(0, 13));
        let mut b = buffer("x");
        b.end(false);
        typed(&mut b, "\"");
        assert_eq!(b.text(), "x\"");
        // An escaped quote is typed, not stepped over.
        let mut b = buffer("\"a\"");
        b.set_cursor(Pos::new(0, 2), false);
        typed(&mut b, "\\\"");
        assert_eq!(b.text(), "\"a\\\"\"");
    }

    #[test]
    fn enter_indents_and_splits_pairs() {
        let mut b = buffer("{\n  \"log\": {}\n}");
        b.set_cursor(Pos::new(1, 10), false);
        b.newline();
        assert_eq!(b.text(), "{\n  \"log\": {\n    \n  }\n}");
        assert_eq!(b.cursor(), Pos::new(2, 4));
        typed(&mut b, "\"level\": 1");
        b.newline();
        assert_eq!(b.line(3), "    ");
        typed(&mut b, "}");
        assert_eq!(b.line(3), "  }");
    }

    #[test]
    fn backspace_removes_pairs_and_indent() {
        let mut b = buffer("");
        typed(&mut b, "[");
        b.backspace();
        assert_eq!(b.text(), "");
        let mut b = buffer("{\n  \"a\": 1,\n    x");
        b.set_cursor(Pos::new(2, 4), false);
        b.backspace();
        assert_eq!(b.line(2), "  x");
        b.set_cursor(Pos::new(2, 0), false);
        b.backspace();
        assert_eq!(b.text(), "{\n  \"a\": 1,  x");
    }

    #[test]
    fn undo_and_versions() {
        let mut b = buffer("");
        let v = b.version();
        typed(&mut b, "ab");
        assert!(b.version() > v);
        assert!(b.undo() && b.undo());
        assert_eq!(b.text(), "");
        assert!(!b.undo());
        assert!(b.redo());
        assert_eq!(b.text(), "a");
        let v = b.version();
        b.right(false, false);
        assert_eq!(b.version(), v);
    }

    #[test]
    fn selection_replace_and_surround() {
        let mut b = buffer("hello world");
        b.select(Pos::new(0, 6), Pos::new(0, 11));
        typed(&mut b, "\"");
        assert_eq!(b.text(), "hello \"world\"");
        assert_eq!(b.selected_text().as_deref(), Some("world"));
        typed(&mut b, "x");
        assert_eq!(b.text(), "hello \"x\"");
        b.select_all();
        assert_eq!(b.selection_len(), b.text().chars().count());
        b.insert_text("a\r\nb");
        assert_eq!(b.lines(), ["a", "b"]);
    }

    #[test]
    fn commands_undo_in_one_step() {
        let mut b = buffer("{\n  // keep\n  \"a\": 1\n}");
        b.set_text("{\"a\": 2}");
        b.select(Pos::new(0, 1), Pos::new(0, 4));
        b.insert_text("\"b\"");
        b.type_char('x');
        assert_eq!(b.text(), "{\"b\"x: 2}");
        assert!(b.undo());
        assert_eq!(b.text(), "{\"b\": 2}");
        assert!(b.undo());
        assert_eq!(b.text(), "{\"a\": 2}");
        assert!(b.undo());
        assert_eq!(b.text(), "{\n  // keep\n  \"a\": 1\n}");
        assert!(b.redo() && b.redo());
        assert_eq!(b.text(), "{\"b\": 2}");
        b.set_cursor(Pos::new(0, 1), false);
        b.move_lines(1);
        b.newline();
        assert!(b.undo());
        assert_eq!(b.text(), "{\"b\": 2}");
    }

    #[test]
    fn empty_selection_does_not_swallow_typing() {
        // Shift+Home at the start of a line selects nothing.
        let mut b = buffer("{\n}");
        b.home(true);
        b.home(true);
        typed(&mut b, "ab");
        assert_eq!(b.text(), "ab{\n}");
    }

    #[test]
    fn line_operations() {
        let mut b = buffer("a\nb\nc");
        b.set_cursor(Pos::new(1, 1), false);
        b.duplicate_lines();
        assert_eq!(b.text(), "a\nb\nb\nc");
        assert_eq!(b.cursor(), Pos::new(2, 1));
        b.set_cursor(Pos::new(0, 0), false);
        b.move_lines(1);
        assert_eq!(b.text(), "b\na\nb\nc");
        assert_eq!(b.cursor().line, 1);
        b.move_lines(-1);
        b.move_lines(-1);
        assert_eq!(b.text(), "a\nb\nb\nc");
        assert_eq!(b.cursor().line, 0);
        b.delete_lines();
        assert_eq!(b.text(), "b\nb\nc");
        b.select(Pos::new(0, 0), Pos::new(2, 0));
        b.indent_lines();
        assert_eq!(b.text(), "  b\n  b\nc");
        b.outdent_lines();
        assert_eq!(b.text(), "b\nb\nc");
        b.toggle_comment();
        assert_eq!(b.text(), "// b\n// b\nc");
        b.toggle_comment();
        assert_eq!(b.text(), "b\nb\nc");
        b.doc_end(false);
        b.delete_lines();
        assert_eq!(b.text(), "b\nb");
    }

    #[test]
    fn moving_the_last_lines_stays_in_the_document() {
        // Selecting down to the empty last line and moving down.
        let mut b = buffer("a\nb\nc");
        b.set_cursor(Pos::new(1, 0), false);
        b.vertical(1, true);
        b.move_lines(1);
        assert_eq!(b.text(), "a\nc\nb");
        b.type_char('x');
        assert!(b.cursor().line < b.lines().len());
        let mut b = buffer("{\n}\n");
        b.set_cursor(Pos::new(1, 0), false);
        b.vertical(1, true);
        b.move_lines(1);
        b.type_char('x');
        assert!(b.cursor().line < b.lines().len());
    }

    #[test]
    fn clipboard_lines() {
        let mut b = buffer("one\ntwo");
        assert_eq!(b.cut(), "one\n");
        assert_eq!(b.text(), "two");
        b.paste("one\n");
        assert_eq!(b.text(), "one\ntwo");
        b.select(Pos::new(1, 0), Pos::new(1, 2));
        assert_eq!(b.copy(), "tw");
    }

    #[test]
    fn movement() {
        let mut b = buffer("  \"server_port\": 443,\n中文x");
        b.home(false);
        assert_eq!(b.cursor().col, 2);
        b.home(false);
        assert_eq!(b.cursor().col, 0);
        b.select_word(Pos::new(0, 5));
        assert_eq!(b.selected_text().as_deref(), Some("server_port"));
        b.right(false, false);
        assert_eq!(b.cursor(), Pos::new(0, 14));
        b.doc_end(false);
        assert_eq!(b.cursor(), Pos::new(1, 3));
    }

    #[test]
    fn jumps_beyond_u16() {
        let lines = vec!["x"; 70_000].join("\n");
        let mut b = buffer(&lines);
        b.set_cursor(Pos::new(69_999, 1), false);
        assert_eq!(b.cursor(), Pos::new(69_999, 1));
        let long = "y".repeat(66_000);
        let mut b = buffer(&long);
        b.set_cursor(Pos::new(0, 65_600), false);
        assert_eq!(b.cursor(), Pos::new(0, 65_600));
    }

    #[test]
    fn search_and_replace() {
        let mut b = buffer("Tag tag\nTAG 中tag");
        assert_eq!(b.matches("tag", false).len(), 4);
        assert_eq!(b.matches("tag", true).len(), 2);
        assert_eq!(b.matches("tag", true)[1], (Pos::new(1, 5), Pos::new(1, 8)));
        let first = b.find("tag", false, false).unwrap();
        assert_eq!(first, (Pos::new(0, 0), Pos::new(0, 3)));
        b.select(first.0, first.1);
        assert_eq!(b.find("tag", false, false).unwrap().0, Pos::new(0, 4));
        assert_eq!(b.find("tag", false, true).unwrap().0, Pos::new(1, 5));
        assert!(b.replace_selection("tag", "x", false));
        assert_eq!(b.text(), "x tag\nTAG 中tag");
        assert_eq!(b.replace_all("tag", "$1", false), 3);
        assert_eq!(b.text(), "x $1\n$1 中$1");
    }

    #[test]
    fn indent_detection() {
        assert_eq!(buffer("{\n    \"a\": 1\n}").indent, "    ");
        assert_eq!(buffer("{\n\t\"a\": 1\n}").indent, "\t");
        let mut b = buffer("{}");
        b.set_cursor(Pos::new(0, 1), false);
        b.tab();
        assert_eq!(b.text(), "{ }");
        assert_eq!(buffer("\u{feff}{}\r\n").lines(), ["{}", ""]);
    }
}
