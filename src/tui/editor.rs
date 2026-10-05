//! Tree editor for a profile: the JSON document as a collapsible tree whose
//! values can be changed, added, moved and removed, with undo.
//!
//! This module holds the document model; key handling and drawing live in
//! the profiles view.

use std::collections::HashSet;

use anyhow::{Result, bail};
use serde_json::{Map, Value};

use crate::i18n::fl;
use crate::profile;
use crate::protocol::Profile;

const UNDO_DEPTH: usize = 100;

/// One step from a node to a child.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// Location of a node; empty is the document itself.
pub type Path = Vec<Seg>;

/// Where a new member or item goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertTarget {
    Array { path: Path, index: usize },
    Object { path: Path, index: usize },
}

impl InsertTarget {
    /// The container that receives the new node.
    pub fn container(&self) -> &Path {
        match self {
            InsertTarget::Array { path, .. } | InsertTarget::Object { path, .. } => path,
        }
    }
}

pub fn get<'a>(root: &'a Value, path: &[Seg]) -> Option<&'a Value> {
    path.iter().try_fold(root, |node, seg| match (seg, node) {
        (Seg::Key(key), Value::Object(map)) => map.get(key),
        (Seg::Index(i), Value::Array(items)) => items.get(*i),
        _ => None,
    })
}

pub fn get_mut<'a>(root: &'a mut Value, path: &[Seg]) -> Option<&'a mut Value> {
    path.iter().try_fold(root, |node, seg| match (seg, node) {
        (Seg::Key(key), Value::Object(map)) => map.get_mut(key),
        (Seg::Index(i), Value::Array(items)) => items.get_mut(*i),
        _ => None,
    })
}

/// `outbounds › 3 › server` for the breadcrumb.
pub fn display_path(path: &[Seg]) -> String {
    path.iter()
        .map(|seg| match seg {
            Seg::Key(key) => key.clone(),
            Seg::Index(i) => i.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" › ")
}

pub fn is_container(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Array(_))
}

fn child_count(value: &Value) -> usize {
    match value {
        Value::Object(map) => map.len(),
        Value::Array(items) => items.len(),
        _ => 0,
    }
}

/// A visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: Path,
    pub depth: usize,
}

#[derive(Clone)]
struct Snapshot {
    root: Value,
    expanded: HashSet<Path>,
    cursor: Option<Path>,
}

pub struct Editor {
    pub profile: Profile,
    pub root: Value,
    saved: Value,
    /// The stored text had comments, which saving from the tree drops.
    pub had_comments: bool,
    pub expanded: HashSet<Path>,
    pub cursor: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    pub search: Option<String>,
    /// A save request is in flight.
    pub saving: bool,
}

impl Editor {
    pub fn new(profile: Profile, content: &str) -> Result<Self> {
        let root = profile::parse(content)?;
        Ok(Self {
            profile,
            saved: root.clone(),
            root,
            had_comments: profile::has_comments(content),
            expanded: HashSet::new(),
            cursor: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            search: None,
            saving: false,
        })
    }

    pub fn dirty(&self) -> bool {
        self.root != self.saved
    }

    /// The text that saving writes.
    pub fn text(&self) -> String {
        profile::to_text(&self.root)
    }

    /// `text` was stored; it becomes the clean state.
    pub fn mark_saved(&mut self, saved: Value) {
        self.saved = saved;
        self.had_comments = false;
    }

    // ----- rows ---------------------------------------------------------------

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.collect(&self.root, &mut Vec::new(), 0, &mut rows);
        rows
    }

    fn collect(&self, node: &Value, path: &mut Path, depth: usize, rows: &mut Vec<Row>) {
        let children: Vec<Seg> = match node {
            Value::Object(map) => map.keys().cloned().map(Seg::Key).collect(),
            Value::Array(items) => (0..items.len()).map(Seg::Index).collect(),
            _ => return,
        };
        for seg in children {
            path.push(seg);
            rows.push(Row {
                path: path.clone(),
                depth,
            });
            if self.expanded.contains(path)
                && let Some(child) = get(&self.root, path)
            {
                self.collect(child, path, depth + 1, rows);
            }
            path.pop();
        }
    }

    pub fn selected(&self) -> Option<Path> {
        self.rows().get(self.cursor).map(|row| row.path.clone())
    }

    /// Moves the cursor to `path`, opening its ancestors.
    pub fn select(&mut self, path: &[Seg]) {
        for end in 1..path.len() {
            self.expanded.insert(path[..end].to_vec());
        }
        if let Some(index) = self.rows().iter().position(|row| row.path == path) {
            self.cursor = index;
        }
    }

    fn clamp(&mut self) {
        let len = self.rows().len();
        self.cursor = self.cursor.min(len.saturating_sub(1));
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.rows().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        self.cursor = (self.cursor as isize)
            .saturating_add(delta)
            .clamp(0, len as isize - 1) as usize;
    }

    /// Opens the selected container, or steps into its first child.
    pub fn right(&mut self) {
        let Some(path) = self.selected() else { return };
        let Some(value) = get(&self.root, &path) else {
            return;
        };
        if !is_container(value) || child_count(value) == 0 {
            return;
        }
        if self.expanded.insert(path) {
            return;
        }
        self.move_cursor(1);
    }

    /// Closes the selected container, or steps out to the parent.
    pub fn left(&mut self) {
        let Some(path) = self.selected() else { return };
        if self.expanded.remove(&path) {
            return;
        }
        if path.len() > 1 {
            self.select(&path[..path.len() - 1]);
        }
    }

    pub fn toggle(&mut self) {
        let Some(path) = self.selected() else { return };
        if get(&self.root, &path).is_some_and(is_container) && !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
    }

    /// Opens the selected container and everything below it.
    pub fn expand_all(&mut self) {
        let path = self.selected().unwrap_or_default();
        let Some(node) = get(&self.root, &path).cloned() else {
            return;
        };
        let mut stack = vec![(path, node)];
        while let Some((path, node)) = stack.pop() {
            if !is_container(&node) {
                continue;
            }
            match &node {
                Value::Object(map) => {
                    for (key, child) in map {
                        let mut child_path = path.clone();
                        child_path.push(Seg::Key(key.clone()));
                        stack.push((child_path, child.clone()));
                    }
                }
                Value::Array(items) => {
                    for (i, child) in items.iter().enumerate() {
                        let mut child_path = path.clone();
                        child_path.push(Seg::Index(i));
                        stack.push((child_path, child.clone()));
                    }
                }
                _ => {}
            }
            if !path.is_empty() {
                self.expanded.insert(path);
            }
        }
    }

    pub fn collapse_all(&mut self) {
        let top = self.selected().and_then(|p| p.first().cloned());
        self.expanded.clear();
        if let Some(top) = top {
            self.select(&[top]);
        }
    }

    // ----- changes -------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            root: self.root.clone(),
            expanded: self.expanded.clone(),
            cursor: self.selected(),
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.root = snapshot.root;
        self.expanded = snapshot.expanded;
        match snapshot.cursor {
            Some(path) => self.select(&path),
            None => self.clamp(),
        }
    }

    /// Records the state before a change.
    fn checkpoint(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(previous);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.restore(next);
        true
    }

    /// Replaces the node at `path`.
    pub fn set(&mut self, path: &[Seg], value: Value) -> Result<()> {
        if get(&self.root, path) == Some(&value) {
            return Ok(());
        }
        self.checkpoint();
        let Some(node) = get_mut(&mut self.root, path) else {
            self.undo.pop();
            bail!(fl!("editor-node-gone"));
        };
        // A different shape invalidates the open state below the node.
        let reshaped = std::mem::discriminant(node) != std::mem::discriminant(&value)
            || child_count(node) != child_count(&value);
        *node = value;
        if reshaped {
            self.expanded
                .retain(|p| !(p.len() > path.len() && p.starts_with(path)));
        }
        if path.is_empty() {
            self.clamp();
        } else {
            self.select(path);
        }
        Ok(())
    }

    pub fn insert(
        &mut self,
        target: &InsertTarget,
        key: Option<String>,
        value: Value,
    ) -> Result<Path> {
        self.checkpoint();
        let result = self.insert_inner(target, key, value);
        if result.is_err() {
            self.undo.pop();
        }
        result
    }

    fn insert_inner(
        &mut self,
        target: &InsertTarget,
        key: Option<String>,
        value: Value,
    ) -> Result<Path> {
        let container = target.container().clone();
        let new_path = match (target, get_mut(&mut self.root, &container)) {
            (InsertTarget::Array { index, .. }, Some(Value::Array(items))) => {
                let index = (*index).min(items.len());
                items.insert(index, value);
                self.shift(&container, |i| Some(if i >= index { i + 1 } else { i }));
                let mut path = container.clone();
                path.push(Seg::Index(index));
                path
            }
            (InsertTarget::Object { index, .. }, Some(Value::Object(map))) => {
                let key = key.unwrap_or_default();
                if key.is_empty() {
                    bail!(fl!("editor-key-empty"));
                }
                if map.contains_key(&key) {
                    bail!(fl!("editor-key-taken", key = key));
                }
                let index = (*index).min(map.len());
                map.shift_insert(index, key.clone(), value);
                let mut path = container.clone();
                path.push(Seg::Key(key));
                path
            }
            _ => bail!(fl!("editor-node-gone")),
        };
        if !container.is_empty() {
            self.expanded.insert(container);
        }
        self.select(&new_path);
        Ok(new_path)
    }

    /// Removes the node at `path` and returns it.
    pub fn delete(&mut self, path: &[Seg]) -> Result<Value> {
        let Some((last, parent)) = path.split_last() else {
            bail!(fl!("editor-root-locked"));
        };
        self.checkpoint();
        let removed = match (last, get_mut(&mut self.root, parent)) {
            (Seg::Key(key), Some(Value::Object(map))) => map.shift_remove(key),
            (Seg::Index(i), Some(Value::Array(items))) if *i < items.len() => {
                Some(items.remove(*i))
            }
            _ => None,
        };
        let Some(removed) = removed else {
            self.undo.pop();
            bail!(fl!("editor-node-gone"));
        };
        match last {
            Seg::Index(index) => {
                let index = *index;
                self.shift(parent, |i| match i.cmp(&index) {
                    std::cmp::Ordering::Less => Some(i),
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Greater => Some(i - 1),
                });
            }
            Seg::Key(_) => self.expanded.retain(|p| !p.starts_with(path)),
        }
        self.clamp();
        Ok(removed)
    }

    pub fn rename(&mut self, path: &[Seg], new_key: &str) -> Result<()> {
        let Some((Seg::Key(old), parent)) = path.split_last() else {
            bail!(fl!("editor-not-a-member"));
        };
        let new_key = new_key.trim();
        if new_key.is_empty() {
            bail!(fl!("editor-key-empty"));
        }
        if new_key == old {
            return Ok(());
        }
        let Some(Value::Object(map)) = get(&self.root, parent) else {
            bail!(fl!("editor-node-gone"));
        };
        if map.contains_key(new_key) {
            bail!(fl!("editor-key-taken", key = new_key));
        }
        self.checkpoint();
        if let Some(Value::Object(map)) = get_mut(&mut self.root, parent) {
            let renamed: Map<String, Value> = std::mem::take(map)
                .into_iter()
                .map(|(k, v)| {
                    if &k == old {
                        (new_key.to_owned(), v)
                    } else {
                        (k, v)
                    }
                })
                .collect();
            *map = renamed;
        }
        let mut new_path = parent.to_vec();
        new_path.push(Seg::Key(new_key.to_owned()));
        self.expanded = std::mem::take(&mut self.expanded)
            .into_iter()
            .map(|p| {
                if p.starts_with(path) {
                    let mut moved = new_path.clone();
                    moved.extend_from_slice(&p[path.len()..]);
                    moved
                } else {
                    p
                }
            })
            .collect();
        self.select(&new_path);
        Ok(())
    }

    /// Swaps the node with its previous (`-1`) or next (`1`) sibling.
    pub fn move_by(&mut self, path: &[Seg], delta: isize) -> Result<()> {
        let Some((last, parent)) = path.split_last() else {
            bail!(fl!("editor-root-locked"));
        };
        let Some(container) = get(&self.root, parent) else {
            bail!(fl!("editor-node-gone"));
        };
        let len = child_count(container);
        let index = match (last, container) {
            (Seg::Index(i), _) => *i,
            (Seg::Key(key), Value::Object(map)) => index_of(map, key).unwrap_or(0),
            _ => bail!(fl!("editor-node-gone")),
        };
        let target = index as isize + delta;
        if target < 0 || target >= len as isize {
            return Ok(());
        }
        let target = target as usize;
        self.checkpoint();
        let new_path = match get_mut(&mut self.root, parent) {
            Some(Value::Array(items)) => {
                items.swap(index, target);
                self.shift(parent, |i| {
                    Some(if i == index {
                        target
                    } else if i == target {
                        index
                    } else {
                        i
                    })
                });
                let mut path = parent.to_vec();
                path.push(Seg::Index(target));
                path
            }
            Some(Value::Object(map)) => {
                let mut members: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
                members.swap(index, target);
                *map = members.into_iter().collect();
                path.to_vec()
            }
            _ => bail!(fl!("editor-node-gone")),
        };
        self.select(&new_path);
        Ok(())
    }

    /// Inserts a copy after the node: array items as is, object members
    /// under a new key.
    pub fn duplicate(&mut self, path: &[Seg]) -> Result<()> {
        let Some((last, parent)) = path.split_last() else {
            bail!(fl!("editor-root-locked"));
        };
        let Some(mut value) = get(&self.root, path).cloned() else {
            bail!(fl!("editor-node-gone"));
        };
        match (last, get(&self.root, parent)) {
            (Seg::Index(i), Some(Value::Array(items))) => {
                // Tags must stay unique among outbounds, inbounds, ...
                if let Some(Value::String(tag)) = value.get_mut("tag") {
                    let taken: HashSet<&str> = items
                        .iter()
                        .filter_map(|item| item.get("tag").and_then(Value::as_str))
                        .collect();
                    *tag = free_name(tag, |name| taken.contains(name));
                }
                let target = InsertTarget::Array {
                    path: parent.to_vec(),
                    index: i + 1,
                };
                self.insert(&target, None, value).map(drop)
            }
            (Seg::Key(key), Some(Value::Object(map))) => {
                let new_key = free_name(key, |name| map.contains_key(name));
                let target = InsertTarget::Object {
                    path: parent.to_vec(),
                    index: index_of(map, key).map_or(map.len(), |i| i + 1),
                };
                self.insert(&target, Some(new_key), value).map(drop)
            }
            _ => bail!(fl!("editor-node-gone")),
        }
    }

    /// Renumbers open array items below `array` after an insert, removal
    /// or swap.
    fn shift(&mut self, array: &[Seg], map: impl Fn(usize) -> Option<usize>) {
        let depth = array.len();
        self.expanded = std::mem::take(&mut self.expanded)
            .into_iter()
            .filter_map(|mut path| {
                if path.len() > depth
                    && path.starts_with(array)
                    && let Seg::Index(i) = path[depth]
                {
                    path[depth] = Seg::Index(map(i)?);
                }
                Some(path)
            })
            .collect();
    }

    // ----- search ---------------------------------------------------------------

    /// Selects the next node after the cursor (or before it, `backwards`)
    /// whose key or value contains `query`, opening collapsed parents.
    pub fn find(&mut self, query: &str, backwards: bool) -> bool {
        let query = query.to_lowercase();
        if query.is_empty() {
            return false;
        }
        let mut all = Vec::new();
        walk(&self.root, &mut Vec::new(), &mut all);
        let matches: Vec<&Path> = all
            .iter()
            .filter(|path| matches_query(&self.root, path, &query))
            .collect();
        if matches.is_empty() {
            return false;
        }
        let current = self.selected().unwrap_or_default();
        let position = all.iter().position(|p| *p == current).unwrap_or(0);
        let order = |p: &Path| all.iter().position(|q| q == p).unwrap_or(0);
        let next = if backwards {
            matches
                .iter()
                .rev()
                .find(|p| order(p) < position)
                .or(matches.last())
        } else {
            matches
                .iter()
                .find(|p| order(p) > position)
                .or(matches.first())
        };
        if let Some(path) = next.map(|p| (*p).clone()) {
            self.select(&path);
        }
        true
    }

    /// Whether the row matches the active search, for highlighting.
    pub fn is_match(&self, path: &[Seg]) -> bool {
        self.search
            .as_ref()
            .map(|q| q.to_lowercase())
            .is_some_and(|q| !q.is_empty() && matches_query(&self.root, path, &q))
    }
}

/// Every node in document order.
fn walk(node: &Value, path: &mut Path, out: &mut Vec<Path>) {
    let children: Vec<Seg> = match node {
        Value::Object(map) => map.keys().cloned().map(Seg::Key).collect(),
        Value::Array(items) => (0..items.len()).map(Seg::Index).collect(),
        _ => return,
    };
    for seg in children {
        path.push(seg);
        out.push(path.clone());
        if let Some(child) = get_child(node, path.last().unwrap()) {
            walk(child, path, out);
        }
        path.pop();
    }
}

fn get_child<'a>(node: &'a Value, seg: &Seg) -> Option<&'a Value> {
    get(node, std::slice::from_ref(seg))
}

fn matches_query(root: &Value, path: &[Seg], query: &str) -> bool {
    let key_matches =
        matches!(path.last(), Some(Seg::Key(key)) if key.to_lowercase().contains(query));
    let value_matches = match get(root, path) {
        Some(Value::String(s)) => s.to_lowercase().contains(query),
        Some(Value::Number(n)) => n.to_string().contains(query),
        Some(Value::Bool(b)) => b.to_string() == query,
        _ => false,
    };
    key_matches || value_matches
}

/// Position of a member in an object, which keeps insertion order.
pub fn index_of(map: &Map<String, Value>, key: &str) -> Option<usize> {
    map.keys().position(|k| k == key)
}

/// `name`, or `name-2`, `name-3`, ... when `taken`.
fn free_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    (2..)
        .map(|n| format!("{name}-{n}"))
        .find(|candidate| !taken(candidate))
        .unwrap_or_default()
}

/// Reads typed text for a scalar: strings stay text, numbers and booleans
/// must parse as such, and `null` accepts any JSON literal.
pub fn parse_scalar(old: &Value, text: &str) -> Result<Value> {
    match old {
        Value::String(_) => Ok(Value::String(text.to_owned())),
        Value::Number(_) => match serde_json::from_str::<Value>(text.trim()) {
            Ok(Value::Number(n)) => Ok(Value::Number(n)),
            _ => bail!(fl!("editor-not-a-number")),
        },
        Value::Bool(_) => match text.trim() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => bail!(fl!("editor-not-a-bool")),
        },
        _ => parse_json(text),
    }
}

/// Typed JSON, comments allowed.
pub fn parse_json(text: &str) -> Result<Value> {
    serde_json::from_str(&profile::strip_json_comments(text))
        .map_err(|err| anyhow::anyhow!(fl!("profile-invalid-json", error = err.to_string())))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn editor(value: Value) -> Editor {
        let profile = Profile {
            id: "abc".into(),
            name: "test".into(),
            url: None,
            interval: 0,
            created_at: 0,
            updated_at: 0,
            fetched_at: None,
            last_error: None,
            usage: None,
            size: 0,
            active: false,
        };
        Editor::new(profile, &value.to_string()).unwrap()
    }

    fn key(k: &str) -> Seg {
        Seg::Key(k.to_owned())
    }

    #[test]
    fn rows_follow_expansion() {
        let mut e =
            editor(json!({"log": {"level": "info"}, "outbounds": [{"tag": "a"}, {"tag": "b"}]}));
        assert_eq!(e.rows().len(), 2);
        e.cursor = 1;
        e.right();
        assert_eq!(e.rows().len(), 4);
        e.right();
        assert_eq!(e.selected(), Some(vec![key("outbounds"), Seg::Index(0)]));
        e.left();
        assert_eq!(e.selected(), Some(vec![key("outbounds")]));
        e.left();
        assert_eq!(e.rows().len(), 2);
        assert_eq!(display_path(&[key("route"), Seg::Index(2)]), "route › 2");
    }

    #[test]
    fn edits_undo_and_dirty_state() {
        let mut e = editor(json!({"log": {"level": "info"}, "route": {}}));
        let level = vec![key("log"), key("level")];
        e.set(&level, json!("debug")).unwrap();
        assert!(e.dirty());
        assert_eq!(e.root["log"]["level"], "debug");
        e.rename(&[key("route")], "router").unwrap();
        assert!(e.rename(&[key("router")], "log").is_err());
        assert_eq!(
            e.root.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["log", "router"]
        );
        assert!(e.undo());
        assert!(e.undo());
        assert!(!e.dirty());
        assert!(e.redo());
        assert_eq!(e.root["log"]["level"], "debug");
        e.mark_saved(e.root.clone());
        assert!(!e.dirty());
        assert!(e.text().ends_with("}\n"));
    }

    #[test]
    fn arrays_insert_delete_move_duplicate() {
        let mut e = editor(json!({"outbounds": [{"tag": "a", "type": "direct"}, {"tag": "b"}]}));
        let outbounds = vec![key("outbounds")];
        // An open item stays open while items move around it.
        e.expanded.insert(outbounds.clone());
        e.expanded.insert(vec![key("outbounds"), Seg::Index(1)]);
        let at = e
            .insert(
                &InsertTarget::Array {
                    path: outbounds.clone(),
                    index: 0,
                },
                None,
                json!({"tag": "new"}),
            )
            .unwrap();
        assert_eq!(at, vec![key("outbounds"), Seg::Index(0)]);
        assert!(e.expanded.contains(&vec![key("outbounds"), Seg::Index(2)]));
        e.move_by(&[key("outbounds"), Seg::Index(2)], -1).unwrap();
        assert_eq!(e.root["outbounds"][1]["tag"], "b");
        assert!(e.expanded.contains(&vec![key("outbounds"), Seg::Index(1)]));
        e.duplicate(&[key("outbounds"), Seg::Index(2)]).unwrap();
        assert_eq!(e.root["outbounds"][3]["tag"], "a-2");
        e.delete(&[key("outbounds"), Seg::Index(0)]).unwrap();
        let tags: Vec<&str> = e.root["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["tag"].as_str().unwrap())
            .collect();
        assert_eq!(tags, ["b", "a", "a-2"]);
        assert!(e.delete(&[]).is_err());
    }

    #[test]
    fn objects_keep_member_order() {
        let mut e = editor(json!({"log": {}, "route": {}}));
        e.insert(
            &InsertTarget::Object {
                path: Vec::new(),
                index: 1,
            },
            Some("dns".into()),
            json!({}),
        )
        .unwrap();
        assert!(
            e.insert(
                &InsertTarget::Object {
                    path: Vec::new(),
                    index: 0
                },
                Some("dns".into()),
                json!(1)
            )
            .is_err()
        );
        e.move_by(&[key("route")], -1).unwrap();
        e.duplicate(&[key("log")]).unwrap();
        let keys: Vec<&String> = e.root.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["log", "log-2", "route", "dns"]);
    }

    #[test]
    fn search_opens_parents() {
        let mut e = editor(
            json!({"outbounds": [{"tag": "hk"}, {"tag": "jp", "server": "jp.example.com"}]}),
        );
        assert!(e.find("example", false));
        assert_eq!(
            e.selected(),
            Some(vec![key("outbounds"), Seg::Index(1), key("server")])
        );
        assert!(e.find("tag", false));
        assert_eq!(
            e.selected(),
            Some(vec![key("outbounds"), Seg::Index(0), key("tag")])
        );
        assert!(e.find("tag", true));
        assert_eq!(
            e.selected(),
            Some(vec![key("outbounds"), Seg::Index(1), key("tag")])
        );
        assert!(!e.find("nothing", false));
    }

    #[test]
    fn typed_values() {
        assert_eq!(parse_scalar(&json!("a"), " b ").unwrap(), json!(" b "));
        assert_eq!(parse_scalar(&json!(1), "8080").unwrap(), json!(8080));
        assert!(parse_scalar(&json!(1), "x").is_err());
        assert_eq!(parse_scalar(&json!(true), "false").unwrap(), json!(false));
        assert_eq!(parse_json("[1, 2,]").unwrap(), json!([1, 2]));
        assert!(parse_json("hello").is_err());
    }
}
