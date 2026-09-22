//! Line accurate view of SLSsteam's `config.yaml`.
//!
//! The shipped file is full of comments, inline notes and commented out entries, so instead of re-serializing it, edits touch only the lines that actually change and everything else stays byte for byte identical.
//! The resulting text is validated with a YAML parser before writing, and SLSsteam watches the file and hot reloads it after a save.

use std::path::PathBuf;

use crate::schema::{self, Shape, ValueKind};

/// A parsed top level entry.
#[derive(Debug, Clone)]
pub struct TopLevel {
    pub key: String,
    /// Index of the `Key:` line.
    pub line: usize,
    pub node: Node,
    /// Comment lines directly above the key, used as help text.
    pub comments: Vec<String>,
    /// Shape from the schema, or inferred for unknown keys.
    pub shape: Option<Shape>,
    pub known: bool,
}

/// A value in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// `Key:` with nothing after it.
    Null { line: usize },
    /// A scalar on `line`, stored raw (quotes and all).
    Scalar { line: usize, value: String },
    /// A block sequence.
    Seq { line: usize, items: Vec<Item> },
    /// A block mapping.
    Map { line: usize, items: Vec<Item> },
}

/// One entry of a sequence or mapping, including its subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Key for mapping entries, `None` for sequence entries.
    pub key: Option<String>,
    /// Index of the entry's own line.
    pub line: usize,
    /// Exclusive end of the entry's subtree.
    pub end: usize,
    pub node: Node,
    /// Inline comment, including the `#`.
    pub trailing: String,
}

impl Item {
    /// Raw scalar text of the entry; empty for nested collections.
    pub fn scalar(&self) -> &str {
        match &self.node {
            Node::Scalar { value, .. } => value,
            _ => "",
        }
    }
}

/// Where a new entry goes, and how it should be spelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertPoint {
    /// Line the new entry is inserted at.
    pub line: usize,
    /// Indentation to use.
    pub indent: String,
    /// Shape of the collection the entry is added to.
    pub shape: Shape,
}

/// The config file, kept as lines so unedited text survives untouched.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    pub path: PathBuf,
    lines: Vec<String>,
    trailing_newline: bool,
    dirty: bool,
}

impl ConfigFile {
    /// Read a config file from disk.
    pub fn load(path: impl Into<PathBuf>) -> std::io::Result<ConfigFile> {
        let path = path.into();
        let text = std::fs::read_to_string(&path)?;
        Ok(ConfigFile::parse_text(path, &text))
    }

    /// Build the model from text, e.g. for tests.
    pub fn parse_text(path: impl Into<PathBuf>, text: &str) -> ConfigFile {
        let trailing_newline = text.ends_with('\n');
        let lines = text
            .trim_end_matches('\n')
            .split('\n')
            .map(str::to_string)
            .collect();
        ConfigFile {
            path: path.into(),
            lines,
            trailing_newline,
            dirty: false,
        }
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// The file text.
    pub fn text(&self) -> String {
        let mut out = self.lines.join("\n");
        if self.trailing_newline {
            out.push('\n');
        }
        out
    }

    /// Write the file back, refusing to write text that is not valid YAML.
    pub fn save(&mut self) -> Result<(), String> {
        let text = self.text();
        if let Err(e) = text.parse::<yaml_edit::YamlFile>() {
            return Err(format!("refusing to save invalid YAML: {e}"));
        }
        std::fs::write(&self.path, text).map_err(|e| format!("{}: {e}", self.path.display()))?;
        self.dirty = false;
        Ok(())
    }

    /// Parse the current lines into top level entries.
    pub fn parse(&self) -> Vec<TopLevel> {
        let mut out = Vec::new();
        let mut index = 0;
        while index < self.lines.len() {
            if top_level_key(&self.lines[index]).is_none() {
                index += 1;
                continue;
            }
            let (node, next) = parse_block(&self.lines, index, 0);
            let key = top_level_key(&self.lines[index]).unwrap_or("").to_string();
            let known = schema::find(&key).is_some();
            let shape = schema::find(&key)
                .map(|setting| setting.shape)
                .or_else(|| infer_shape(&node));
            out.push(TopLevel {
                comments: comments_above(&self.lines, index),
                key,
                line: index,
                node,
                shape,
                known,
            });
            index = next.max(index + 1);
        }
        out
    }

    /// The top level entry for `key`.
    pub fn top_for(&self, key: &str) -> Option<TopLevel> {
        self.parse().into_iter().find(|entry| entry.key == key)
    }

    // ---- edits ----

    /// Replace the scalar on `line`, keeping indentation, key and comment.
    pub fn set_scalar(&mut self, line: usize, raw: &str) {
        let Some(original) = self.lines.get(line).cloned() else {
            return;
        };
        let (prefix, _, comment) = split_line(&original);
        let mut rebuilt = prefix;
        rebuilt.push_str(raw.trim());
        if !comment.is_empty() {
            rebuilt.push(' ');
            rebuilt.push_str(&comment);
        }
        self.lines[line] = rebuilt;
        self.dirty = true;
    }

    /// Where an entry added to the collection at `key` + `path` would go.
    pub fn insert_point(&self, key: &str, path: &[usize]) -> Option<InsertPoint> {
        self.insert_point_as(key, path, None)
    }

    /// Like [`insert_point`](Self::insert_point), with a shape to use when the key is not in the schema and its value is empty.
    pub fn insert_point_as(
        &self,
        key: &str,
        path: &[usize],
        hint: Option<Shape>,
    ) -> Option<InsertPoint> {
        let top = self.top_for(key)?;
        let mut node = top.node.clone();
        let mut owner_line = top.line;
        let mut shape = top.shape;

        for (depth, index) in path.iter().enumerate() {
            let item = match &node {
                Node::Seq { items, .. } | Node::Map { items, .. } => items.get(*index)?.clone(),
                _ => return None,
            };
            let last = depth + 1 == path.len();
            node = item.node.clone();
            owner_line = item.line;
            if last {
                let inferred = infer_shape(&node);
                let nested_shape = match top.shape {
                    // Inside a map of sequences / maps the children are known.
                    Some(Shape::MapOfSeq) => Some(Shape::Seq),
                    Some(Shape::MapOfMap) => Some(Shape::Map),
                    _ => None,
                };
                shape = inferred.or(nested_shape).or(shape);
            } else {
                shape = match shape {
                    Some(Shape::MapOfSeq) => Some(Shape::Seq),
                    Some(Shape::MapOfMap) => Some(Shape::Map),
                    other => other,
                };
            }
        }

        let shape = shape.or(hint)?;
        let owner_indent = self.line_indent(owner_line);
        match &node {
            Node::Seq { items, .. } | Node::Map { items, .. } => {
                let line = items.last().map(|item| item.end).unwrap_or(owner_line + 1);
                let indent = items
                    .first()
                    .map(|item| self.line_indent(item.line))
                    .unwrap_or_else(|| format!("{owner_indent}  "));
                Some(InsertPoint {
                    line,
                    indent,
                    shape,
                })
            }
            Node::Null { line } => Some(InsertPoint {
                line: line + 1,
                indent: format!("{owner_indent}  "),
                shape,
            }),
            Node::Scalar { line, .. } => Some(InsertPoint {
                line: line + 1,
                indent: format!("{owner_indent}  "),
                shape,
            }),
        }
    }

    /// Add an entry to the collection at `top_line` + `path`.
    ///
    /// `key` is required for mappings and ignored for sequences.
    /// An empty collection gets its first entry on a new indented line after its key.
    pub fn add_item(
        &mut self,
        setting: &str,
        path: &[usize],
        key: Option<&str>,
        raw: &str,
    ) -> Result<usize, String> {
        self.add_item_as(setting, path, key, raw, None)
    }

    /// Like [`add_item`](Self::add_item), with a shape hint for keys that are not in the schema (the picker adds to lists the public build does not know, for example).
    pub fn add_item_as(
        &mut self,
        setting: &str,
        path: &[usize],
        key: Option<&str>,
        raw: &str,
        hint: Option<Shape>,
    ) -> Result<usize, String> {
        let point = self
            .insert_point_as(setting, path, hint)
            .ok_or("cannot find where to add this entry")?;
        let new_line = match point.shape {
            Shape::Seq => format!("{}- {}", point.indent, raw.trim()),
            _ => {
                let key = key.unwrap_or("").trim();
                if key.is_empty() {
                    return Err("this collection needs a key".to_string());
                }
                if raw.trim().is_empty() {
                    format!("{}{}:", point.indent, key)
                } else {
                    format!("{}{}: {}", point.indent, key, raw.trim())
                }
            }
        };
        let at = point.line.min(self.lines.len());
        self.lines.insert(at, new_line);
        self.dirty = true;
        Ok(at)
    }

    /// The comment block directly above `setting`, without the leading `#`.
    pub fn comment_block(&self, setting: &str) -> Vec<String> {
        let Some(top) = self.top_for(setting) else {
            return Vec::new();
        };
        let start = self.comment_start(top.line);
        self.lines[start..top.line]
            .iter()
            .map(|line| strip_hash(line))
            .collect()
    }

    /// First line of the comment block directly above `line`.
    fn comment_start(&self, line: usize) -> usize {
        let mut index = line;
        while index > 0 && self.lines[index - 1].trim_start().starts_with('#') {
            index -= 1;
        }
        index
    }

    /// Replace comment line `index` of `setting`; new lines are appended.
    pub fn set_comment_line(&mut self, setting: &str, index: usize, text: &str) {
        let Some(top) = self.top_for(setting) else {
            return;
        };
        let start = self.comment_start(top.line);
        let count = top.line - start;
        let line = start + index.min(count);
        self.lines.insert(line, format!("#{text}"));
        if index < count {
            self.lines.remove(line + 1);
        }
        self.dirty = true;
    }

    /// Add a comment line at `index` (or at the end of the block).
    pub fn insert_comment_line(&mut self, setting: &str, index: Option<usize>, text: &str) {
        let Some(top) = self.top_for(setting) else {
            return;
        };
        let start = self.comment_start(top.line);
        let count = top.line - start;
        let at = start + index.unwrap_or(count).min(count);
        let text = text.trim_end().to_string();
        if text.is_empty() {
            return;
        }
        self.lines.insert(at, format!("#{text}"));
        self.dirty = true;
    }

    /// Remove comment line `index` of `setting`.
    pub fn remove_comment_line(&mut self, setting: &str, index: usize) {
        let Some(top) = self.top_for(setting) else {
            return;
        };
        let start = self.comment_start(top.line);
        let count = top.line - start;
        if index < count {
            self.lines.remove(start + index);
            self.dirty = true;
        }
    }

    /// Replace the inline comment on `line`, or drop it when `text` is empty.
    pub fn set_trailing_comment(&mut self, line: usize, text: &str) {
        let Some(original) = self.lines.get(line).cloned() else {
            return;
        };
        let (prefix, value, _) = split_line(&original);
        let mut rebuilt = prefix;
        rebuilt.push_str(&value);
        let text = text.trim().trim_start_matches('#').trim_end();
        if !text.is_empty() {
            rebuilt = format!("{} #{text}", rebuilt.trim_end());
        }
        self.lines[line] = rebuilt;
        self.dirty = true;
    }

    /// Split a quick add entry into its AppId and optional name comment.
    pub fn parse_add_entry(input: &str) -> Result<(String, Option<String>), String> {
        let mut parts = input.trim().splitn(2, char::is_whitespace);
        let appid = parts.next().unwrap_or("").trim();
        if !is_int(appid) {
            return Err(format!("'{appid}' is not an AppId (number)"));
        }
        let name = parts
            .next()
            .map(|name| name.trim().trim_start_matches('#').trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        Ok((appid.to_string(), name))
    }

    /// Delete an entry and everything nested under it.
    pub fn remove_item(&mut self, item: &Item) {
        let end = item.end.min(self.lines.len());
        if item.line < end {
            self.lines.drain(item.line..end);
            self.dirty = true;
        }
    }

    /// Give `IdleStatus` its two keys, matching the shipped template.
    pub fn seed_idle_status(&mut self, setting: &str) {
        let Some(top) = self.top_for(setting) else {
            return;
        };
        let top_line = top.line;
        if !matches!(top.node, Node::Null { .. }) {
            return;
        }
        let indent = format!("{}  ", self.line_indent(top_line));
        self.lines
            .insert(top_line + 1, format!("{indent}Title: \"\""));
        self.lines.insert(top_line + 1, format!("{indent}AppId: 0"));
        self.dirty = true;
    }

    fn line_indent(&self, line: usize) -> String {
        self.lines
            .get(line)
            .map(|l| l[..l.len() - l.trim_start().len()].to_string())
            .unwrap_or_default()
    }
}

/// Comment text without the leading `#` and at most one space.
fn strip_hash(line: &str) -> String {
    let text = line.trim_start();
    let text = text.strip_prefix('#').unwrap_or(text);
    text.strip_prefix(' ').unwrap_or(text).to_string()
}

/// True when `line` is a top level `Key:` line, returning the key.
fn top_level_key(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t', '#']) {
        return None;
    }
    let (key, _) = line.split_once(':')?;
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(key)
}

/// Comment lines directly above `line`. A blank line ends the block, so a comment further up belongs to whatever came before it.
fn comments_above(lines: &[String], line: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut index = line;
    while index > 0 {
        let previous = lines[index - 1].trim_start();
        if !previous.starts_with('#') {
            break;
        }
        out.push(previous.to_string());
        index -= 1;
    }
    out.reverse();
    out
}

/// Parse the block that starts on `line`, returning the node and the next line.
///
/// `depth` 0 means the line is a top level `Key: value` line, deeper values are entries (`  key: value` or `  - value`).
fn parse_block(lines: &[String], line: usize, depth: usize) -> (Node, usize) {
    let own = lines.get(line).cloned().unwrap_or_default();
    let inline = if depth == 0 {
        own.split_once(':').map(|(_, rest)| rest).unwrap_or("")
    } else {
        own.trim()
    };
    let inline = inline.strip_prefix(' ').unwrap_or(inline);
    let (value, _) = strip_comment(inline);
    let value = value.trim().to_string();

    let items = parse_items(lines, line, depth);
    if items.is_empty() {
        if value.is_empty() {
            return (Node::Null { line }, line + 1);
        }
        return (Node::Scalar { line, value }, line + 1);
    }

    let next = items.last().map(|item| item.end).unwrap_or(line + 1);
    let node = if items.iter().all(|item| item.key.is_none()) {
        Node::Seq { line, items }
    } else {
        Node::Map { line, items }
    };
    (node, next)
}

/// Parse the entries indented under `parent_line`.
fn parse_items(lines: &[String], parent_line: usize, parent_depth: usize) -> Vec<Item> {
    let base = lines
        .get(parent_line)
        .map(|l| l.len() - l.trim_start().len())
        .unwrap_or(0);
    let mut items = Vec::new();
    let mut index = parent_line + 1;
    let mut entry_indent: Option<usize> = None;

    while index < lines.len() {
        let current = &lines[index];
        if top_level_key(current).is_some() {
            break;
        }
        if current.trim().is_empty() {
            index += 1;
            continue;
        }
        let indent = current.len() - current.trim_start().len();
        if indent <= base {
            break;
        }
        if current.trim_start().starts_with('#') {
            index += 1;
            continue;
        }
        // Entries sit at one indentation level; deeper lines belong to the previous entry and are consumed by its recursion.
        match entry_indent {
            None => entry_indent = Some(indent),
            Some(expected) if indent > expected => {
                index += 1;
                continue;
            }
            Some(expected) if indent < expected => break,
            Some(_) => {}
        }
        let (item, next) = parse_item(lines, index, parent_depth + 1);
        items.push(item);
        index = next;
    }
    items
}

/// Parse one entry and its subtree.
fn parse_item(lines: &[String], line: usize, depth: usize) -> (Item, usize) {
    let trimmed = lines.get(line).cloned().unwrap_or_default();
    let trimmed = trimmed.trim_start().to_string();

    if let Some(rest) = trimmed.strip_prefix("- ") {
        let (value, comment) = strip_comment(rest);
        return (
            Item {
                key: None,
                line,
                end: line + 1,
                node: Node::Scalar {
                    line,
                    value: value.trim().to_string(),
                },
                trailing: comment,
            },
            line + 1,
        );
    }

    let (key, rest) = match trimmed.split_once(':') {
        Some((key, rest)) => (key.trim().to_string(), rest.to_string()),
        None => (trimmed.clone(), String::new()),
    };
    let rest = rest.strip_prefix(' ').unwrap_or(&rest).to_string();
    let (value, comment) = strip_comment(&rest);
    let value = value.trim().to_string();

    if value.is_empty() && has_deeper_following(lines, line) {
        let (node, next) = parse_block(lines, line, depth);
        return (
            Item {
                key: Some(key),
                line,
                end: next,
                node,
                trailing: String::new(),
            },
            next,
        );
    }

    (
        Item {
            key: Some(key),
            line,
            end: line + 1,
            node: node_for(line, value),
            trailing: comment,
        },
        line + 1,
    )
}

/// A scalar node when there is a value, a null node otherwise.
fn node_for(line: usize, value: String) -> Node {
    if value.is_empty() {
        Node::Null { line }
    } else {
        Node::Scalar { line, value }
    }
}

/// True when a deeper indented line follows, making this a nested block.
fn has_deeper_following(lines: &[String], line: usize) -> bool {
    let base = lines
        .get(line)
        .map(|l| l.len() - l.trim_start().len())
        .unwrap_or(0);
    for candidate in lines.iter().skip(line + 1) {
        if candidate.trim().is_empty() || candidate.trim_start().starts_with('#') {
            continue;
        }
        if top_level_key(candidate).is_some() {
            return false;
        }
        let indent = candidate.len() - candidate.trim_start().len();
        return indent > base;
    }
    false
}

/// Split a line into `prefix` (indent plus `- ` or `Key: `), value and comment.
fn split_line(line: &str) -> (String, String, String) {
    let indent_len = line.len() - line.trim_start().len();
    let indent = &line[..indent_len];
    let trimmed = line.trim_start();

    let (marker, rest) = if let Some(rest) = trimmed.strip_prefix("- ") {
        ("- ".to_string(), rest.to_string())
    } else if let Some((key, rest)) = trimmed.split_once(':') {
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        (format!("{key}: "), rest.to_string())
    } else {
        (String::new(), trimmed.to_string())
    };

    let (value, comment) = strip_comment(&rest);
    (
        format!("{indent}{marker}"),
        value.trim().to_string(),
        comment,
    )
}

/// Split a trailing comment off a scalar, ignoring `#` inside quotes.
pub fn strip_comment(text: &str) -> (String, String) {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let chars: Vec<char> = text.chars().collect();

    for (index, c) in chars.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_double => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double && (index == 0 || chars[index - 1].is_whitespace()) => {
                let value: String = chars[..index].iter().collect();
                let comment: String = chars[index..].iter().collect();
                return (value, comment);
            }
            _ => {}
        }
    }
    (text.to_string(), String::new())
}

/// True when `raw` looks like an integer (decimal, `0x`, `0b` or `0o`).
pub fn is_int(raw: &str) -> bool {
    let text = raw.trim();
    let text = text.strip_prefix('-').unwrap_or(text);
    let (digits, radix) =
        if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (rest, 16)
        } else if let Some(rest) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
            (rest, 2)
        } else if let Some(rest) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
            (rest, 8)
        } else {
            (text, 10)
        };
    !digits.is_empty() && u64::from_str_radix(digits, radix).is_ok()
}

/// Parse the boolean spellings yaml-cpp accepts.
pub fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" => Some(true),
        "no" | "false" | "off" => Some(false),
        _ => None,
    }
}

/// Text without its surrounding quotes.
pub fn unquote(raw: &str) -> String {
    let text = raw.trim();
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        text[1..text.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else if text.len() >= 2 && text.starts_with('\'') && text.ends_with('\'') {
        text[1..text.len() - 1].replace("''", "'")
    } else {
        text.to_string()
    }
}

/// Quote `text` the way the shipped config does, when that is needed.
pub fn quote(text: &str) -> String {
    let needs_quotes = text.is_empty()
        || text != text.trim()
        || parse_bool(text).is_some()
        || is_int(text)
        || text
            .chars()
            .next()
            .map(|c| "-?:,[]{}#&*!|>'\"%@`".contains(c))
            .unwrap_or(false)
        || text.contains(": ")
        || text.contains(" #")
        || text.contains('"')
        || text.contains('\\')
        || text.contains('\n');
    quote_when(text, needs_quotes)
}

/// Always quote, used when the original value was quoted as well.
pub fn quote_always(text: &str) -> String {
    quote_when(text, true)
}

fn quote_when(text: &str, quoted: bool) -> String {
    if quoted {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        text.to_string()
    }
}

/// Guess a shape for a node, used for keys the schema does not know.
pub fn infer_shape(node: &Node) -> Option<Shape> {
    match node {
        Node::Null { .. } => None,
        Node::Scalar { value, .. } => {
            if parse_bool(value).is_some() {
                Some(Shape::Bool)
            } else if is_int(value) {
                Some(Shape::Integer)
            } else {
                Some(Shape::Text)
            }
        }
        Node::Seq { .. } => Some(Shape::Seq),
        Node::Map { items, .. } => {
            for item in items {
                match item.node {
                    Node::Map { .. } => return Some(Shape::MapOfMap),
                    Node::Seq { .. } => return Some(Shape::MapOfSeq),
                    _ => {}
                }
            }
            Some(Shape::Map)
        }
    }
}

/// Check a new key for a map-like collection.
pub fn check_key(key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("this entry needs a key".to_string());
    }
    if !is_int(key) {
        return Err(format!("'{key}' is not an AppId (number)"));
    }
    Ok(())
}

/// Check a new entry against a setting's shape.
pub fn check_entry(
    shape: Shape,
    value_kind: Option<ValueKind>,
    key: Option<&str>,
    raw: &str,
) -> Result<(), String> {
    if !matches!(shape, Shape::Seq) {
        let key = key.unwrap_or("").trim();
        if key.is_empty() {
            return Err("this entry needs a key".to_string());
        }
        if !is_int(key) {
            return Err(format!("'{key}' is not an AppId (number)"));
        }
    }

    let wants_number = match shape {
        Shape::Seq => true,
        Shape::Map | Shape::MapOfMap => matches!(value_kind, Some(ValueKind::Integer)),
        _ => false,
    };
    if wants_number && !is_int(raw) {
        return Err(match shape {
            Shape::Seq => format!("'{raw}' is not an AppId (number)"),
            _ => format!("'{raw}' must be a number"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
#Example of FakeAppIds:
#FakeAppIds:
#  440: 480

#Disables Family Share license locking for self and others
DisableFamilyShareLock: yes

#List of AppIds to ex-/include
AppIds:
  - 0 #SteamApp0
  - 408490 #Hero Siege Soundtrack

FakeAppIds:
  0: 480 #Unowned -> Spacewar
  221410: 221410

#Extra Data for Dlcs belonging to a specific AppId
DlcData:
  123:
    456: \"Some DLC\"
    789: \"Other DLC\"

FakeName: \"\"
LogLevels: 0xff
IdleStatus:
  AppId: 0
  Title: \"\"
";

    fn sample() -> ConfigFile {
        ConfigFile::parse_text("/tmp/config.yaml", SAMPLE)
    }

    #[test]
    fn round_trip_is_lossless() {
        assert_eq!(sample().text(), SAMPLE);
    }

    #[test]
    fn top_level_keys_are_parsed_with_comments() {
        let tops = sample().parse();
        let keys: Vec<&str> = tops.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "DisableFamilyShareLock",
                "AppIds",
                "FakeAppIds",
                "DlcData",
                "FakeName",
                "LogLevels",
                "IdleStatus",
            ]
        );

        let family = &tops[0];
        assert_eq!(family.shape, Some(Shape::Bool));
        assert!(family.known);
        assert!(family.comments.iter().any(|c| c.contains("Family Share")));
    }

    #[test]
    fn shapes_are_read_from_the_file() {
        let tops = sample().parse();
        let shape = |key: &str| {
            tops.iter()
                .find(|t| t.key == key)
                .and_then(|t| t.shape)
                .unwrap()
        };
        assert_eq!(shape("AppIds"), Shape::Seq);
        assert_eq!(shape("FakeAppIds"), Shape::Map);
        assert_eq!(shape("DlcData"), Shape::MapOfMap);
        assert_eq!(shape("FakeName"), Shape::Text);
        assert_eq!(shape("LogLevels"), Shape::Integer);
        assert_eq!(shape("IdleStatus"), Shape::IdleStatus);
    }

    #[test]
    fn scalar_edits_keep_comment_and_formatting() {
        let mut file = sample();
        let line_of = |file: &ConfigFile, key: &str| file.top_for(key).unwrap().line;

        file.set_scalar(line_of(&file, "DisableFamilyShareLock"), "no");
        assert!(file.text().contains("DisableFamilyShareLock: no\n"));
        assert!(file.dirty());

        // Inline comments survive on sequence and map entries.
        file.set_scalar(line_of(&file, "AppIds") + 1, "730");
        assert!(file.text().contains("  - 730 #SteamApp0"));

        let fake = line_of(&file, "FakeAppIds");
        file.set_scalar(fake + 1, "480");
        assert!(file.text().contains("  0: 480 #Unowned -> Spacewar"));
        file.set_scalar(fake + 2, "480");
        assert!(file.text().contains("  221410: 480"));
    }

    #[test]
    fn adding_and_removing_entries() {
        let mut file = sample();

        file.add_item("AppIds", &[], None, "730").unwrap();
        assert!(file.text().contains("  - 730\n"));

        file.add_item("FakeAppIds", &[], Some("440"), "480")
            .unwrap();
        assert!(file.text().contains("  440: 480\n"));

        // Nested: add a DLC under 123, and a new appId with an empty value.
        file.add_item("DlcData", &[0], Some("999"), "\"New DLC\"")
            .unwrap();
        assert!(file.text().contains("    999: \"New DLC\"\n"));
        file.add_item("DlcData", &[], Some("777"), "").unwrap();
        assert!(file.text().contains("  777:\n"));

        // Removing one DLC under 123 leaves its sibling alone.
        let tops = file.parse();
        let dlc = tops.iter().find(|top| top.key == "DlcData").unwrap();
        let Node::Map { items, .. } = &dlc.node else {
            panic!("expected a map");
        };
        let Node::Map { items: dlcs, .. } = &items[0].node else {
            panic!("expected nested dlc entries");
        };
        file.remove_item(&dlcs[0].clone());
        assert!(!file.text().contains("\"Some DLC\""));
        assert!(file.text().contains("\"Other DLC\""));
        assert!(file.text().contains("  777:\n"));
    }

    #[test]
    fn empty_collections_get_their_first_entry_indented() {
        let text = "AppIds:\n\nFakeAppIds:\n\nDlcData:\n";
        let mut file = ConfigFile::parse_text("/tmp/config.yaml", text);
        file.add_item("AppIds", &[], None, "440").unwrap();
        file.add_item("FakeAppIds", &[], Some("440"), "480")
            .unwrap();
        file.add_item("DlcData", &[], Some("440"), "").unwrap();
        assert_eq!(
            file.text(),
            "AppIds:\n  - 440\n\nFakeAppIds:\n  440: 480\n\nDlcData:\n  440:\n"
        );
    }

    #[test]
    fn removing_an_entry_drops_its_subtree_only() {
        let mut file = sample();
        let tops = file.parse();
        let dlc = tops.iter().find(|top| top.key == "DlcData").unwrap();
        let Node::Map { items, .. } = &dlc.node else {
            panic!("expected a map");
        };
        // Removing the whole 123 entry drops both of its DLCs...
        file.remove_item(&items[0].clone());
        assert!(!file.text().contains("456"));
        assert!(!file.text().contains("789"));

        // ...while the key itself and the rest of the file stay.
        let mut file = sample();
        file.add_item("DlcData", &[], Some("777"), "").unwrap();
        let tops = file.parse();
        let dlc = tops.iter().find(|top| top.key == "DlcData").unwrap();
        let Node::Map { items, .. } = &dlc.node else {
            panic!("expected a map");
        };
        file.remove_item(&items[0].clone());
        assert!(file.text().contains("DlcData:\n"));
        assert!(file.text().contains("  777:\n"));
        assert!(file.text().contains("FakeName: \"\""));
    }

    #[test]
    fn booleans_and_numbers_are_recognised() {
        assert_eq!(parse_bool("yes"), Some(true));
        assert_eq!(parse_bool("NO"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
        assert!(is_int("440"));
        assert!(is_int("0xff"));
        assert!(is_int("4294967294"));
        assert!(is_int("18232185815196388220"));
        assert!(!is_int("440x"));
    }

    #[test]
    fn quoting_follows_yaml_rules() {
        assert_eq!(quote("Day of Defeat"), "Day of Defeat");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("yes"), "\"yes\"");
        assert_eq!(quote("480"), "\"480\"");
        assert_eq!(quote("a: b"), "\"a: b\"");
        assert_eq!(quote("Mission: Impossible"), "\"Mission: Impossible\"");
        assert_eq!(unquote("\"Some DLC\""), "Some DLC");
        assert_eq!(unquote("plain"), "plain");
    }

    #[test]
    fn comments_are_split_but_not_inside_quotes() {
        let (value, comment) = strip_comment("- 0 #SteamApp0");
        assert_eq!(value.trim(), "- 0");
        assert_eq!(comment, "#SteamApp0");

        let (value, comment) = strip_comment("  Title: \"A # B\"");
        assert_eq!(value, "  Title: \"A # B\"");
        assert!(comment.is_empty());
    }

    #[test]
    fn entries_are_validated_against_the_shape() {
        assert!(check_entry(Shape::Seq, None, None, "440").is_ok());
        assert!(check_entry(Shape::Seq, None, None, "abc").is_err());
        assert!(check_entry(Shape::Map, Some(ValueKind::Integer), Some("440"), "480").is_ok());
        assert!(check_entry(Shape::Map, Some(ValueKind::Integer), Some("440"), "text").is_err());
        assert!(check_entry(Shape::Map, Some(ValueKind::Text), Some("440"), "Some Title").is_ok());
        assert!(check_entry(
            Shape::Map,
            Some(ValueKind::Text),
            Some("nope"),
            "Some Title"
        )
        .is_err());
        assert!(check_entry(Shape::MapOfMap, Some(ValueKind::Text), Some("1"), "").is_ok());
    }

    #[test]
    fn comments_stop_at_blank_lines() {
        let text = "# belongs to whatever came before\n\n# direct comment\nKey: yes\n";
        let file = ConfigFile::parse_text("/tmp/config.yaml", text);
        assert_eq!(
            file.comment_block("Key"),
            vec!["direct comment".to_string()]
        );
        let tops = file.parse();
        let key = tops.iter().find(|top| top.key == "Key").unwrap();
        assert_eq!(key.comments, vec!["# direct comment".to_string()]);
    }

    #[test]
    fn comment_lines_can_be_added_edited_and_removed() {
        let mut file = ConfigFile::parse_text("/tmp/config.yaml", "#one\n#two\nKey: yes\n");
        assert_eq!(file.comment_block("Key"), vec!["one", "two"]);

        file.set_comment_line("Key", 0, "changed");
        assert_eq!(file.comment_block("Key"), vec!["changed", "two"]);

        file.insert_comment_line("Key", None, "three");
        assert_eq!(file.comment_block("Key"), vec!["changed", "two", "three"]);

        file.remove_comment_line("Key", 1);
        assert_eq!(file.comment_block("Key"), vec!["changed", "three"]);
        assert_eq!(file.text(), "#changed\n#three\nKey: yes\n");
    }

    #[test]
    fn a_key_without_comments_gets_one() {
        let mut file = ConfigFile::parse_text("/tmp/config.yaml", "Key: yes\n");
        file.insert_comment_line("Key", None, "note");
        assert_eq!(file.text(), "#note\nKey: yes\n");

        // editing the first line of an empty block creates it too
        let mut file = ConfigFile::parse_text("/tmp/config.yaml", "Key: yes\n");
        file.set_comment_line("Key", 0, "hello");
        assert_eq!(file.text(), "#hello\nKey: yes\n");
        file.set_comment_line("Key", 0, "bye");
        assert_eq!(file.text(), "#bye\nKey: yes\n");
    }

    #[test]
    fn trailing_comments_can_be_set_and_cleared() {
        let mut file = ConfigFile::parse_text("/tmp/config.yaml", "AppIds:\n  - 440 # old\n");
        file.set_trailing_comment(1, "Half-Life 2");
        assert_eq!(file.text(), "AppIds:\n  - 440 #Half-Life 2\n");

        file.set_trailing_comment(1, "");
        assert_eq!(file.text(), "AppIds:\n  - 440\n");

        file.set_trailing_comment(0, "games");
        assert_eq!(file.text(), "AppIds: #games\n  - 440\n");
    }

    #[test]
    fn quick_add_input_is_parsed() {
        assert_eq!(
            ConfigFile::parse_add_entry("440"),
            Ok(("440".to_string(), None))
        );
        assert_eq!(
            ConfigFile::parse_add_entry("  440  Half-Life 2 "),
            Ok(("440".to_string(), Some("Half-Life 2".to_string())))
        );
        assert_eq!(
            ConfigFile::parse_add_entry("440 #Half-Life 2"),
            Ok(("440".to_string(), Some("Half-Life 2".to_string())))
        );
        assert!(ConfigFile::parse_add_entry("half-life").is_err());
        assert!(ConfigFile::parse_add_entry("").is_err());
    }

    #[test]
    fn seed_idle_status_adds_the_two_keys() {
        let mut file = ConfigFile::parse_text("/tmp/config.yaml", "IdleStatus:\nFoo: bar\n");
        file.seed_idle_status("IdleStatus");
        assert_eq!(
            file.text(),
            "IdleStatus:\n  AppId: 0\n  Title: \"\"\nFoo: bar\n"
        );
        // Seeding twice changes nothing.
        let once = file.text();
        file.seed_idle_status("IdleStatus");
        assert_eq!(file.text(), once);
    }

    #[test]
    fn invalid_yaml_is_never_written() {
        let mut file = ConfigFile::parse_text("/tmp/does-not-matter.yaml", "AppIds:\n");
        file.lines.push("  - [broken".to_string());
        assert!(file.save().is_err());
    }
}
