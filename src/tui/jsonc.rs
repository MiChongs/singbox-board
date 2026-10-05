//! JSON analysis for the editor, built on jsonc-parser: the outline of the
//! document with the position of every value (validation, the path under
//! the cursor, finding what a sing-box error names) and per-line tokens for
//! highlighting. Comments, including the `#` comments sing-box accepts, are
//! blanked by json_comments first ([`profile::strip_json_comments`]), which
//! keeps every byte offset in place.

use std::collections::HashSet;

use jsonc_parser::ast;
use jsonc_parser::common::Ranged;
use jsonc_parser::tokens::{Token as JsonToken, TokenAndRange};
use jsonc_parser::{CollectOptions, CommentCollectionStrategy, Scanner, ScannerOptions};

use super::editor::{Path, Seg};
use crate::i18n::fl;
use crate::profile::{self, JsoncError};

/// A place in the text: line and character column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Key,
    String,
    Number,
    Bool,
    Null,
    Bracket,
    Punct,
    Comment,
    Other,
}

/// A value in the document; offsets are bytes into the text.
#[derive(Debug, Clone)]
pub struct Node {
    pub parent: Option<usize>,
    /// How the parent reaches it; `None` for the document itself.
    pub seg: Option<Seg>,
    /// The member name's opening quote.
    pub key: Option<usize>,
    pub start: usize,
    /// Just past the last byte.
    pub end: usize,
}

impl Node {
    /// Where the node starts, including its member name.
    pub fn first(&self) -> usize {
        self.key.unwrap_or(self.start)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub pos: Pos,
    pub message: String,
}

/// One version of the document, analysed.
#[derive(Debug, Default)]
pub struct Analysis {
    text: String,
    /// The text with comments blanked; `None` when json_comments could not
    /// read it (a stray `/`).
    stripped: Option<String>,
    line_starts: Vec<usize>,
    /// Every value in document order; parents come before their children.
    pub nodes: Vec<Node>,
    /// Why the text is not a valid configuration.
    pub error: Option<Problem>,
    /// Members that appear twice in one object.
    pub warnings: Vec<Problem>,
}

impl Analysis {
    pub fn new(text: String) -> Analysis {
        let stripped = profile::strip_json_comments(&text);
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let mut analysis = Analysis {
            text,
            stripped,
            line_starts,
            ..Analysis::default()
        };
        analysis.parse();
        analysis
    }

    fn parse(&mut self) {
        let source = self.stripped.as_deref().unwrap_or(&self.text);
        let collect = CollectOptions {
            comments: CommentCollectionStrategy::Off,
            tokens: true,
        };
        let broken = broken_string(source).map(|at| (at, fl!("json-unclosed-string")));
        let result = match jsonc_parser::parse_to_ast(source, &collect, &profile::jsonc_options()) {
            Ok(result) => result,
            Err(err) => {
                let err = JsoncError::new(&err, &self.text);
                // An open quote makes the parser stumble further on.
                let (offset, message) = broken
                    .filter(|(at, _)| *at <= err.offset)
                    .unwrap_or((err.offset, err.message));
                self.error = Some(self.problem(offset, message));
                return;
            }
        };
        let mut nodes = Vec::new();
        let mut warnings = Vec::new();
        let error = match &result.value {
            None => Some((0, fl!("profile-empty"))),
            Some(value) => {
                walk(value, None, None, None, &mut nodes, &mut warnings);
                (!matches!(value, ast::Value::Object(_)))
                    .then(|| (value.start(), fl!("profile-not-object")))
            }
        }
        .or(broken)
        .or_else(|| odd_whitespace(source, result.tokens.as_deref().unwrap_or_default()));
        self.nodes = nodes;
        self.warnings = warnings
            .into_iter()
            .map(|(offset, message)| self.problem(offset, message))
            .collect();
        self.error = error.map(|(offset, message)| self.problem(offset, message));
    }

    fn problem(&self, offset: usize, message: String) -> Problem {
        Problem {
            pos: self.pos(offset),
            message,
        }
    }

    /// The position of a byte offset.
    pub fn pos(&self, offset: usize) -> Pos {
        let offset = self.text.floor_char_boundary(offset);
        let line = self.line_starts.partition_point(|start| *start <= offset) - 1;
        let start = self.line_starts[line];
        Pos::new(line, self.text[start..offset].chars().count())
    }

    /// The byte offset of a position, clamped to its line.
    pub fn offset(&self, pos: Pos) -> usize {
        let Some(&start) = self.line_starts.get(pos.line) else {
            return self.text.len();
        };
        let end = self
            .line_starts
            .get(pos.line + 1)
            .map_or(self.text.len(), |next| next - 1);
        self.text[start..end]
            .char_indices()
            .nth(pos.col)
            .map_or(end, |(i, _)| start + i)
    }

    pub fn path(&self, index: usize) -> Path {
        let mut path = Vec::new();
        let mut at = Some(index);
        while let Some(i) = at {
            let node = &self.nodes[i];
            if let Some(seg) = &node.seg {
                path.push(seg.clone());
            }
            at = node.parent;
        }
        path.reverse();
        path
    }

    /// The innermost node whose member name or value contains `pos`.
    pub fn node_at(&self, pos: Pos) -> Option<usize> {
        let offset = self.offset(pos);
        self.nodes
            .iter()
            .rposition(|node| node.first() <= offset && offset <= node.end)
    }

    pub fn find(&self, path: &[Seg]) -> Option<usize> {
        let mut current = (!self.nodes.is_empty()).then_some(0)?;
        for seg in path {
            current = self.child(current, seg)?;
        }
        Some(current)
    }

    fn child(&self, parent: usize, seg: &Seg) -> Option<usize> {
        (parent + 1..self.nodes.len()).find(|&i| {
            self.nodes[i].parent == Some(parent) && self.nodes[i].seg.as_ref() == Some(seg)
        })
    }

    /// The node a sing-box error names, e.g. `route.rules[1].bogus` in
    /// "decode config at …: route.rules[1].bogus: json: unknown field",
    /// and the byte offset of that name in the message. Takes the
    /// candidate that resolves deepest; `outbound[2]` also finds
    /// `outbounds[2]`. Bare words such as `log` count only when they are
    /// top-level keys, and not at all with `structured_only`.
    pub fn locate(&self, message: &str, structured_only: bool) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, bool, usize)> = None;
        for (offset, candidate) in path_words(message) {
            let structured = candidate.contains(['.', '[']);
            if structured_only && !structured {
                continue;
            }
            let Some(segs) = parse_path(candidate) else {
                continue;
            };
            let Some((index, depth)) = self.resolve(&segs) else {
                continue;
            };
            if !structured && depth != segs.len() {
                continue;
            }
            let better = best.is_none_or(|(_, _, best_structured, best_depth)| {
                (structured, depth) > (best_structured, best_depth)
            });
            if better {
                best = Some((index, offset, structured, depth));
            }
        }
        best.map(|(index, offset, _, _)| (index, offset))
    }

    /// The deepest node along `segs` and how many of them matched.
    fn resolve(&self, segs: &[Seg]) -> Option<(usize, usize)> {
        let mut current = self.find(&[])?;
        let mut depth = 0;
        for seg in segs {
            let mut step = self.child(current, seg);
            if step.is_none()
                && let Seg::Key(key) = seg
            {
                step = self.child(current, &Seg::Key(format!("{key}s")));
            }
            match step {
                Some(next) => {
                    current = next;
                    depth += 1;
                }
                None => break,
            }
        }
        (depth > 0).then_some((current, depth))
    }

    /// The bracket at `at` and its partner, both as positions.
    pub fn bracket_pair(&self, at: Pos) -> Option<(Pos, Pos)> {
        let offset = self.offset(at);
        let bytes = self.text.as_bytes();
        let node = match bytes.get(offset)? {
            b'{' | b'[' => self.nodes.iter().find(|n| n.start == offset)?,
            b'}' | b']' => self.nodes.iter().find(|n| n.end == offset + 1)?,
            _ => return None,
        };
        let pair = matches!(
            (bytes.get(node.start), bytes.get(node.end.checked_sub(1)?)),
            (Some(b'{'), Some(b'}')) | (Some(b'['), Some(b']'))
        );
        pair.then(|| (self.pos(node.start), self.pos(node.end - 1)))
    }

    /// Highlighting of line `index`, whose text is `line`, as
    /// `(start, end, token)` character-column runs. Lines are scanned on
    /// their own, so an error on one line leaves the others coloured.
    pub fn highlight(&self, index: usize, line: &str) -> Vec<(usize, usize, Token)> {
        let start = self.line_starts.get(index).copied().unwrap_or(0);
        let blanked = self
            .stripped
            .as_deref()
            .and_then(|s| s.get(start..start + line.len()));
        let mut runs = Vec::new();
        // Comments are where json_comments blanked the text.
        if let Some(blanked) = blanked {
            let mut comment: Option<usize> = None;
            for (i, (a, b)) in line.bytes().zip(blanked.bytes()).enumerate() {
                match (a != b, comment) {
                    (true, None) => comment = Some(i),
                    (false, Some(from)) => {
                        runs.push((from, i, Token::Comment));
                        comment = None;
                    }
                    _ => {}
                }
            }
            if let Some(from) = comment {
                runs.push((from, line.len(), Token::Comment));
            }
        }
        let mut scanner = Scanner::new(blanked.unwrap_or(line), &ScannerOptions::default());
        let mut tokens: Vec<(usize, usize, Token)> = Vec::new();
        // An error leaves the rest of the line plain.
        while let Ok(Some(token)) = scanner.scan() {
            let kind = match token {
                JsonToken::OpenBrace
                | JsonToken::CloseBrace
                | JsonToken::OpenBracket
                | JsonToken::CloseBracket => Token::Bracket,
                JsonToken::Colon => {
                    if let Some(previous) = tokens.last_mut()
                        && previous.2 == Token::String
                    {
                        previous.2 = Token::Key;
                    }
                    Token::Punct
                }
                JsonToken::Comma => Token::Punct,
                JsonToken::String(_) => Token::String,
                JsonToken::Number(_) => Token::Number,
                JsonToken::Boolean(_) => Token::Bool,
                JsonToken::Null => Token::Null,
                JsonToken::Word(_) => Token::Other,
                JsonToken::CommentLine(_) | JsonToken::CommentBlock(_) => Token::Comment,
            };
            tokens.push((scanner.token_start(), scanner.token_end(), kind));
        }
        runs.extend(tokens);
        // Byte offsets to character columns.
        let mut columns = vec![0; line.len() + 1];
        let mut col = 0;
        for (i, c) in line.char_indices() {
            columns[i..i + c.len_utf8()].fill(col);
            col += 1;
        }
        columns[line.len()] = col;
        runs.into_iter()
            .map(|(s, e, t)| (columns[s.min(line.len())], columns[e.min(line.len())], t))
            .collect()
    }
}

/// Collects the nodes below `value` and duplicate member names.
fn walk(
    value: &ast::Value,
    parent: Option<usize>,
    seg: Option<Seg>,
    key: Option<usize>,
    nodes: &mut Vec<Node>,
    warnings: &mut Vec<(usize, String)>,
) {
    let index = nodes.len();
    nodes.push(Node {
        parent,
        seg,
        key,
        start: value.start(),
        end: value.end(),
    });
    match value {
        ast::Value::Object(object) => {
            let mut seen = HashSet::new();
            for prop in &object.properties {
                let name = prop.name.as_str();
                if !seen.insert(name) {
                    warnings.push((
                        prop.name.start(),
                        fl!("json-duplicate-key", key = name.to_owned()),
                    ));
                }
                walk(
                    &prop.value,
                    Some(index),
                    Some(Seg::Key(name.to_owned())),
                    Some(prop.name.start()),
                    nodes,
                    warnings,
                );
            }
        }
        ast::Value::Array(array) => {
            for (i, element) in array.elements.iter().enumerate() {
                walk(
                    element,
                    Some(index),
                    Some(Seg::Index(i)),
                    None,
                    nodes,
                    warnings,
                );
            }
        }
        _ => {}
    }
}

/// Where a string runs past the end of its line: jsonc-parser reads on,
/// sing-box does not.
fn broken_string(source: &str) -> Option<usize> {
    let mut scanner = Scanner::new(source, &ScannerOptions::default());
    while let Ok(Some(token)) = scanner.scan() {
        if matches!(token, JsonToken::String(_))
            && source[scanner.token_start()..scanner.token_end()].contains(['\n', '\r'])
        {
            return Some(scanner.token_start());
        }
    }
    None
}

/// jsonc-parser takes any Unicode space between tokens, sing-box only the
/// four JSON ones. A full-width space typed with an input method is the
/// usual culprit.
fn odd_whitespace(source: &str, tokens: &[TokenAndRange]) -> Option<(usize, String)> {
    let starts = tokens
        .iter()
        .map(|t| t.range.start)
        .chain(std::iter::once(source.len()));
    let ends = std::iter::once(0).chain(tokens.iter().map(|t| t.range.end));
    for (from, to) in ends.zip(starts) {
        let Some(gap) = source.get(from..to) else {
            continue;
        };
        if let Some((i, c)) = gap
            .char_indices()
            .find(|(_, c)| !matches!(c, ' ' | '\t' | '\n' | '\r'))
        {
            let name = format!("U+{:04X}", u32::from(c));
            return Some((from + i, fl!("json-odd-space", name = name)));
        }
    }
    None
}

/// Runs of characters that may form a path, with their byte offsets.
fn path_words(message: &str) -> Vec<(usize, &str)> {
    let part = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '[' | ']');
    let mut words = Vec::new();
    let mut start = None;
    for (i, c) in message.char_indices().chain([(message.len(), ' ')]) {
        match (part(c), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                let raw: &str = &message[s..i];
                let trimmed = raw.trim_start_matches('.');
                let offset = s + raw.len() - trimmed.len();
                words.push((offset, trimmed.trim_end_matches('.')));
                start = None;
            }
            _ => {}
        }
    }
    words
}

/// `route.rules[1].outbound` (how sing-box names a field in its errors) or
/// `outbounds.2.tag` as path segments.
pub fn parse_path(text: &str) -> Option<Vec<Seg>> {
    let text = text.trim().trim_start_matches(['/', '.', '$']);
    if text.is_empty() {
        return None;
    }
    let mut segs = Vec::new();
    for part in text.split(['.', '/']) {
        let (name, mut rest) = match part.find('[') {
            Some(i) => (&part[..i], &part[i..]),
            None => (part, ""),
        };
        if !name.is_empty() {
            match name.parse::<usize>() {
                Ok(index) => segs.push(Seg::Index(index)),
                Err(_) => segs.push(Seg::Key(name.to_owned())),
            }
        }
        while let Some(inner) = rest.strip_prefix('[') {
            let close = inner.find(']')?;
            segs.push(Seg::Index(inner[..close].parse().ok()?));
            rest = &inner[close + 1..];
        }
        if !rest.is_empty() || name.is_empty() && !part.starts_with('[') {
            return None;
        }
    }
    (!segs.is_empty()).then_some(segs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> Seg {
        Seg::Key(k.to_owned())
    }

    fn analysis(text: &str) -> Analysis {
        Analysis::new(text.to_owned())
    }

    #[test]
    fn highlighting() {
        let line = r#"  "tag": "a\"b", "n": -1.5e3, "x": [true, null] // c"#;
        let a = analysis(&format!("{{\n{line}\n}}"));
        let runs = a.highlight(1, line);
        let token = |col: usize| {
            runs.iter()
                .find(|(s, e, _)| *s <= col && col < *e)
                .map(|r| r.2)
        };
        assert_eq!(token(2), Some(Token::Key));
        assert_eq!(token(9), Some(Token::String));
        assert_eq!(token(15), Some(Token::Punct));
        assert_eq!(token(22), Some(Token::Number));
        assert_eq!(token(36), Some(Token::Bool));
        assert_eq!(token(42), Some(Token::Null));
        assert_eq!(token(49), Some(Token::Comment));
        // A block comment over several lines and a `#` comment.
        let a = analysis("{\n/* a\nb */ \"k\": 1, # c\n\"中\": 2}");
        assert!(a.highlight(1, "/* a").contains(&(0, 2, Token::Comment)));
        let runs = a.highlight(2, "b */ \"k\": 1, # c");
        assert!(runs.contains(&(0, 1, Token::Comment)));
        assert!(runs.contains(&(13, 14, Token::Comment)));
        assert!(runs.contains(&(5, 8, Token::Key)));
        // Columns count characters.
        assert!(a.highlight(3, "\"中\": 2}").contains(&(0, 3, Token::Key)));
        // An unterminated string only affects its own line.
        let a = analysis("{\n\"a\": \"x\n\"b\": 1}");
        assert!(a.highlight(2, "\"b\": 1}").contains(&(0, 3, Token::Key)));
    }

    #[test]
    fn outline_positions_and_paths() {
        let text = "{\n  // comment\n  \"log\": {\"level\": \"info\"},\n  \"outbounds\": [\n    {\"tag\": \"中文\", \"server_port\": 443},\n  ],\n}";
        let a = analysis(text);
        assert_eq!(a.error, None);
        let port = a
            .find(&[key("outbounds"), Seg::Index(0), key("server_port")])
            .unwrap();
        let node = &a.nodes[port];
        assert_eq!(a.pos(node.key.unwrap()), Pos::new(4, 18));
        assert_eq!(a.pos(node.start), Pos::new(4, 33));
        assert_eq!(a.pos(node.end), Pos::new(4, 36));
        assert_eq!(
            a.path(a.node_at(Pos::new(4, 34)).unwrap()),
            [key("outbounds"), Seg::Index(0), key("server_port")]
        );
        assert_eq!(
            a.path(a.node_at(Pos::new(2, 12)).unwrap()),
            [key("log"), key("level")]
        );
        assert_eq!(a.node_at(Pos::new(0, 0)), Some(0));
        assert_eq!(
            a.bracket_pair(Pos::new(4, 4)),
            Some((Pos::new(4, 4), Pos::new(4, 36)))
        );
        assert_eq!(a.pos(a.offset(Pos::new(4, 99))), Pos::new(4, 38));
    }

    #[test]
    fn errors() {
        let error = |text: &str| analysis(text).error.map(|p| p.pos);
        assert_eq!(error("{\"a\": [1,]}"), None);
        assert_eq!(error("{\n  # sing-box accepts these\n  \"a\": 1\n}"), None);
        assert_eq!(error("{\n  \"a\": tru\n}"), Some(Pos::new(1, 7)));
        assert!(error("{\"a\": 1").is_some());
        assert!(error("{} x").is_some());
        assert_eq!(error("[]"), Some(Pos::new(0, 0)));
        assert_eq!(error(""), Some(Pos::new(0, 0)));
        // Full-width and non-breaking spaces are not JSON whitespace.
        assert_eq!(error("{\"a\":\u{3000}1}"), Some(Pos::new(0, 5)));
        assert_eq!(error("{\"a\": \"x\u{3000}y\"}"), None);
        let a = analysis("{\"a\": 1, \"a\": 2}");
        assert_eq!(a.warnings.len(), 1);
        assert_eq!(a.warnings[0].pos, Pos::new(0, 9));
        // A string left open is reported where it starts, not on the next line.
        assert_eq!(
            error("{\n  \"level\": \"info\n  \"timestamp\": true\n}"),
            Some(Pos::new(1, 11))
        );
        // A stray slash is still found.
        assert!(error("{\"a\": 1 / 2}").is_some());
    }

    #[test]
    fn sing_box_errors() {
        let a = analysis(
            r#"{"route": {"rules": [{"outbound": "a"}, {"bogus": 1}]}, "outbounds": [{"tag": "x"}, {"tag": "y"}], "log": {}}"#,
        );
        let at = |message: &str| a.locate(message, false).map(|(i, _)| a.path(i));
        assert_eq!(
            at("decode config at /x/a.json: route.rules[1].bogus: json: unknown field \"bogus\""),
            Some(vec![
                key("route"),
                key("rules"),
                Seg::Index(1),
                key("bogus")
            ])
        );
        assert_eq!(
            at("initialize outbound[1]: missing server"),
            Some(vec![key("outbounds"), Seg::Index(1)])
        );
        assert_eq!(
            at("create log factory: unknown log level"),
            Some(vec![key("log")])
        );
        assert_eq!(at("nothing to see"), None);
        assert_eq!(a.locate("create log factory", true), None);
        let message = "saved; check: outbounds[1].server: missing";
        let (_, offset) = a.locate(message, true).unwrap();
        assert!(message[offset..].starts_with("outbounds[1].server"));
        assert_eq!(
            parse_path("outbounds.2.tag"),
            Some(vec![key("outbounds"), Seg::Index(2), key("tag")])
        );
        assert_eq!(parse_path("a[x]"), None);
    }
}
