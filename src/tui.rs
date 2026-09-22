//! The ratatui editor: a settings list, a tree editor for the collections and inline prompts for new entries.

use std::io::IsTerminal;
use std::path::PathBuf;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::config::{self, check_entry, ConfigFile, Item, Node, TopLevel};
use crate::picker::{self, Picker};
use crate::schema::{self, Shape, ValueKind};

/// A row of the settings list.
enum Row {
    Section(String),
    Setting(usize),
    /// A key the schema does not know, e.g. one a newer SLSsteam added.
    Other(String),
}

/// A row of the collection editor.
struct TreeRow {
    depth: usize,
    label: String,
    value: String,
    path: Vec<usize>,
    node: Node,
    /// Inline comment shown dimmed.
    trailing: String,
}

/// What an inline buffer is editing.
enum EditTarget {
    /// A scalar in the settings list, addressed by its line.
    Line(usize, Shape),
    /// A leaf in the collection editor, addressed by its line.
    Leaf(usize),
}

struct TextEdit {
    title: String,
    target: EditTarget,
    buffer: String,
    cursor: usize,
    /// Keep the double quotes the original value had.
    quoted: bool,
}

/// What a one line prompt is asking for.
enum PromptKind {
    AddKey {
        path: Vec<usize>,
    },
    AddValue {
        path: Vec<usize>,
        key: String,
    },
    /// Quick add of an AppId, optionally with a name comment.
    QuickAdd {
        key: String,
    },
    /// Edit comment line `index` of a setting, or append when it is `None`.
    Comment {
        key: String,
        index: Option<usize>,
    },
    /// Inline comment of a collection entry.
    Trailing {
        line: usize,
    },
}

struct Prompt {
    title: String,
    kind: PromptKind,
    buffer: String,
    cursor: usize,
}

enum Mode {
    List,
    Tree,
    Comments,
}

struct App {
    file: ConfigFile,
    mode: Mode,
    selection: usize,
    tree_selection: usize,
    /// Key of the collection open in the tree editor.
    tree_key: String,
    /// Key whose comment block is open in the comment editor.
    comments_key: String,
    comment_selection: usize,
    /// Steam search panel.
    picker: Picker,
    edit: Option<TextEdit>,
    prompt: Option<Prompt>,
    confirm_quit: bool,
    status: String,
}

impl App {
    fn new(file: ConfigFile) -> App {
        let mut app = App {
            file,
            mode: Mode::List,
            selection: 0,
            tree_selection: 0,
            tree_key: String::new(),
            comments_key: String::new(),
            comment_selection: 0,
            picker: Picker::new(),
            edit: None,
            prompt: None,
            confirm_quit: false,
            status: String::new(),
        };
        app.jump(true);
        app
    }

    // ---- parsed views ----

    fn tops(&self) -> Vec<TopLevel> {
        self.file.parse()
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut section = "";
        for (index, setting) in schema::SETTINGS.iter().enumerate() {
            if setting.section != section {
                section = setting.section;
                rows.push(Row::Section(section.to_string()));
            }
            rows.push(Row::Setting(index));
        }
        let unknown: Vec<String> = self
            .tops()
            .into_iter()
            .filter(|top| !top.known)
            .map(|top| top.key)
            .collect();
        if !unknown.is_empty() {
            rows.push(Row::Section("Other keys".to_string()));
            rows.extend(unknown.into_iter().map(Row::Other));
        }
        rows
    }

    /// Known settings that are missing from the file, plus unknown keys.
    fn review_rows(&self) -> (Vec<&'static schema::Setting>, Vec<TopLevel>) {
        let tops = self.tops();
        let present: Vec<&str> = tops.iter().map(|t| t.key.as_str()).collect();
        let missing = schema::SETTINGS
            .iter()
            .filter(|setting| !present.contains(&setting.key))
            .collect();
        let unknown = tops.into_iter().filter(|t| !t.known).collect();
        (missing, unknown)
    }

    /// The schema entry of the selected row, when it is a known setting.
    #[cfg(test)]
    fn selected_setting(&self) -> Option<&'static schema::Setting> {
        match self.rows().get(self.selection) {
            Some(Row::Setting(index)) => Some(&schema::SETTINGS[*index]),
            _ => None,
        }
    }

    /// Key and shape of the selected row, known or not.
    fn selected_key(&self) -> Option<(String, Shape)> {
        match self.rows().get(self.selection) {
            Some(Row::Setting(index)) => {
                let setting = &schema::SETTINGS[*index];
                Some((setting.key.to_string(), setting.shape))
            }
            Some(Row::Other(key)) => {
                let top = self.top_for(key)?;
                Some((key.clone(), top.shape?))
            }
            _ => None,
        }
    }

    fn top_for(&self, key: &str) -> Option<TopLevel> {
        self.tops().into_iter().find(|top| top.key == key)
    }

    /// The collection shown in the tree editor.
    fn tree_node(&self) -> Option<(TopLevel, Node)> {
        let top = self.file.top_for(&self.tree_key)?;
        Some((top.clone(), top.node))
    }

    fn tree_rows(&self) -> Vec<TreeRow> {
        let Some((_, node)) = self.tree_node() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        flatten(&node, &mut Vec::new(), 0, &mut rows);
        rows
    }

    // ---- key handling ----

    /// Handle a key press; returns `true` when the editor should exit.
    fn handle_key(&mut self, key: KeyEvent) -> Result<bool, String> {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }

        if self.confirm_quit {
            self.confirm_quit = false;
            return match key.code {
                KeyCode::Char('s') | KeyCode::Char('y') | KeyCode::Enter => {
                    self.save()?;
                    Ok(true)
                }
                KeyCode::Char('d') | KeyCode::Char('n') => Ok(true),
                _ => Ok(false),
            };
        }

        if self.prompt.is_some() {
            self.handle_prompt(key);
            return Ok(false);
        }
        if self.edit.is_some() {
            self.handle_edit(key);
            return Ok(false);
        }

        if self.picker.has_focus() {
            return match self.picker.handle_key(&mut self.file, key) {
                picker::Outcome::Handled => Ok(false),
                picker::Outcome::Pass(key) => self.handle_list(key),
            };
        }

        match self.mode {
            Mode::List => self.handle_list(key),
            Mode::Tree => self.handle_tree(key),
            Mode::Comments => self.handle_comments(key),
        }
    }

    fn handle_list(&mut self, key: KeyEvent) -> Result<bool, String> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.file.dirty() {
                    self.confirm_quit = true;
                    return Ok(false);
                }
                return Ok(true);
            }
            KeyCode::Char('s') => self.save()?,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home => self.jump(true),
            KeyCode::End => self.jump(false),
            KeyCode::Char(' ') | KeyCode::Enter => self.open(),
            KeyCode::Tab => self.open_comments(),
            KeyCode::Char('g') => self.picker.toggle(),
            KeyCode::Char('a') => self.quick_add(),
            _ => {}
        }
        Ok(false)
    }

    /// The selected setting, when it is an AppId list.
    fn list_key(&self) -> Option<String> {
        let (key, shape) = self.selected_key()?;
        (shape == Shape::Seq).then_some(key)
    }

    /// Quick add an AppId to the selected list, without opening the tree editor.
    fn quick_add(&mut self) {
        let Some(key) = self.list_key() else {
            self.status =
                "Quick add works on AppId lists (AppIds, AdditionalApps, ...)".to_string();
            return;
        };
        if self.top_for(&key).is_none() {
            self.status = format!("{key} is not in the config file yet");
            return;
        }
        self.prompt = Some(Prompt {
            title: format!("Add to {key} (AppId [name])"),
            kind: PromptKind::QuickAdd { key },
            buffer: String::new(),
            cursor: 0,
        });
    }

    /// Open the comment block of the selected setting.
    fn open_comments(&mut self) {
        let Some((key, _)) = self.selected_key() else {
            return;
        };
        if self.top_for(&key).is_none() {
            self.status = format!("{key} is not in the config file yet");
            return;
        }
        self.comments_key = key.clone();
        self.comment_selection = 0;
        self.mode = Mode::Comments;
        if self.file.comment_block(&key).is_empty() {
            self.prompt = Some(Prompt {
                title: format!("Comment for {key}"),
                kind: PromptKind::Comment { key, index: None },
                buffer: String::new(),
                cursor: 0,
            });
        }
    }

    /// Keys while the comment block editor is open.
    fn handle_comments(&mut self, key: KeyEvent) -> Result<bool, String> {
        let lines = self.file.comment_block(&self.comments_key);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('b') | KeyCode::Tab => {
                self.mode = Mode::List;
                self.status.clear();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.comment_selection = self.comment_selection.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.comment_selection + 1 < lines.len() {
                    self.comment_selection += 1;
                }
            }
            KeyCode::Enter => {
                if let Some(text) = lines.get(self.comment_selection) {
                    let key = self.comments_key.clone();
                    let index = self.comment_selection;
                    self.prompt = Some(Prompt {
                        title: "Comment line".to_string(),
                        kind: PromptKind::Comment {
                            key,
                            index: Some(index),
                        },
                        buffer: text.clone(),
                        cursor: text.chars().count(),
                    });
                }
            }
            KeyCode::Char('a') => {
                let key = self.comments_key.clone();
                self.prompt = Some(Prompt {
                    title: "New comment line".to_string(),
                    kind: PromptKind::Comment { key, index: None },
                    buffer: String::new(),
                    cursor: 0,
                });
            }
            KeyCode::Char('d') => {
                self.file
                    .remove_comment_line(&self.comments_key, self.comment_selection);
                let len = self.file.comment_block(&self.comments_key).len();
                self.comment_selection = self.comment_selection.min(len.saturating_sub(1));
            }
            KeyCode::Char('s') => self.save()?,
            _ => {}
        }
        Ok(false)
    }

    /// Keys while the collection editor is open.
    fn handle_tree(&mut self, key: KeyEvent) -> Result<bool, String> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('b') => {
                self.mode = Mode::List;
                self.status.clear();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.tree_selection = self.tree_selection.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let len = self.tree_rows().len();
                if self.tree_selection + 1 < len {
                    self.tree_selection += 1;
                }
            }
            KeyCode::Char(' ') if self.tree_selected_is_bool() => self.toggle_tree_bool(),
            KeyCode::Enter | KeyCode::Char(' ') => self.edit_tree_row(),
            KeyCode::Char('a') => self.start_add(),
            KeyCode::Char('d') => self.delete_tree_row(),
            KeyCode::Char('s') => self.save()?,
            KeyCode::Tab => self.edit_trailing_comment(),
            _ => {}
        }
        Ok(false)
    }

    /// Edit (or add) the inline comment of the selected collection entry.
    fn edit_trailing_comment(&mut self) {
        let Some(row) = self.tree_selected() else {
            return;
        };
        if matches!(row.node, Node::Seq { .. } | Node::Map { .. }) {
            self.status = "Only entries with a value can carry a comment".to_string();
            return;
        }
        let line = row.line();
        let text = row.trailing.trim_start_matches('#').trim().to_string();
        self.prompt = Some(Prompt {
            title: format!("Comment for {}", row.label),
            kind: PromptKind::Trailing { line },
            cursor: text.chars().count(),
            buffer: text,
        });
    }

    fn move_selection(&mut self, delta: isize) {
        let rows = self.rows();
        let mut index = self.selection as isize + delta;
        while index >= 0 && (index as usize) < rows.len() {
            if matches!(rows[index as usize], Row::Setting(_)) {
                self.selection = index as usize;
                return;
            }
            index += delta.signum();
        }
    }

    fn jump(&mut self, first: bool) {
        let rows = self.rows();
        let found = if first {
            rows.iter().position(|row| matches!(row, Row::Setting(_)))
        } else {
            rows.iter().rposition(|row| matches!(row, Row::Setting(_)))
        };
        if let Some(index) = found {
            self.selection = index;
        }
    }

    /// Toggle a boolean or open the editor for the selected setting.
    fn open(&mut self) {
        let Some((key, shape)) = self.selected_key() else {
            return;
        };
        let Some(top) = self.top_for(&key) else {
            self.status = format!("{key} is missing from the config file");
            return;
        };

        match shape {
            Shape::Bool => {
                if let Node::Scalar { line, value } = top.node {
                    let next = !config::parse_bool(&value).unwrap_or(false);
                    self.file.set_scalar(line, if next { "yes" } else { "no" });
                    self.status = format!(
                        "{key}: {} (SLSsteam reloads the file on save)",
                        if next { "yes" } else { "no" }
                    );
                }
            }
            Shape::Integer | Shape::Text => {
                if let Node::Scalar { line, value } = top.node {
                    self.start_edit_line(&key, line, &value, shape);
                }
            }
            shape => {
                self.tree_key = key.clone();
                if matches!(shape, Shape::IdleStatus) && matches!(top.node, Node::Null { .. }) {
                    self.file.seed_idle_status(&key);
                }
                self.mode = Mode::Tree;
                self.tree_selection = 0;
                self.status.clear();
            }
        }
    }

    fn start_edit_line(&mut self, label: &str, line: usize, value: &str, shape: Shape) {
        let quoted = value.trim().starts_with('"');
        let text = if matches!(shape, Shape::Text) {
            config::unquote(value)
        } else {
            value.trim().to_string()
        };
        self.edit = Some(TextEdit {
            title: label.to_string(),
            target: EditTarget::Line(line, shape),
            cursor: text.chars().count(),
            buffer: text,
            quoted,
        });
    }

    fn tree_selected(&self) -> Option<TreeRow> {
        let rows = self.tree_rows();
        rows.get(self.tree_selection).map(|row| TreeRow {
            depth: row.depth,
            label: row.label.clone(),
            value: row.value.clone(),
            path: row.path.clone(),
            node: row.node.clone(),
            trailing: row.trailing.clone(),
        })
    }

    fn tree_selected_is_bool(&self) -> bool {
        match self.tree_selected() {
            Some(row) => match &row.node {
                Node::Scalar { value, .. } => config::parse_bool(value).is_some(),
                _ => false,
            },
            None => false,
        }
    }

    fn toggle_tree_bool(&mut self) {
        if let Some(row) = self.tree_selected() {
            if let Node::Scalar { line, value } = row.node {
                let next = !config::parse_bool(&value).unwrap_or(false);
                self.file.set_scalar(line, if next { "yes" } else { "no" });
            }
        }
    }

    fn edit_tree_row(&mut self) {
        let Some(row) = self.tree_selected() else {
            self.status = "Nothing selected".to_string();
            return;
        };
        if let Node::Scalar { line, value } = row.node {
            let quoted = value.trim().starts_with('"');
            let text = config::unquote(&value);
            self.edit = Some(TextEdit {
                title: format!("{} =", row.label),
                target: EditTarget::Leaf(line),
                cursor: text.chars().count(),
                buffer: text,
                quoted,
            });
            return;
        }
        if matches!(row.node, Node::Null { .. }) && row.label != "-" {
            let text = String::new();
            self.edit = Some(TextEdit {
                title: format!("{} =", row.label),
                target: EditTarget::Leaf(row.line()),
                cursor: 0,
                buffer: text,
                quoted: false,
            });
        }
    }

    /// Add an entry to the collection the cursor is in.
    fn start_add(&mut self) {
        let Some(row) = self.tree_selected() else {
            // Empty collection: add at the top.
            if self.tree_node().is_none() {
                return;
            }
            self.prompt_for_add(Vec::new(), None);
            return;
        };
        let path = if is_collection(&row.node) {
            row.path.clone()
        } else {
            let mut parent = row.path.clone();
            parent.pop();
            parent
        };
        self.prompt_for_add(path, None);
    }

    fn prompt_for_add(&mut self, path: Vec<usize>, key: Option<String>) {
        let shape = self
            .file
            .insert_point(&self.tree_key, &path)
            .map(|point| point.shape);
        match (shape, key) {
            (Some(Shape::Seq), _) => {
                self.prompt = Some(Prompt {
                    title: "New entry".to_string(),
                    kind: PromptKind::AddValue {
                        path,
                        key: String::new(),
                    },
                    buffer: String::new(),
                    cursor: 0,
                });
            }
            (Some(_), None) => {
                self.prompt = Some(Prompt {
                    title: "New key (AppId)".to_string(),
                    kind: PromptKind::AddKey { path },
                    buffer: String::new(),
                    cursor: 0,
                });
            }
            (Some(_), Some(key)) => {
                self.prompt = Some(Prompt {
                    title: format!("Value for {key}"),
                    kind: PromptKind::AddValue { path, key },
                    buffer: String::new(),
                    cursor: 0,
                });
            }
            (None, _) => self.status = "Cannot tell what this entry should contain".to_string(),
        }
    }

    fn delete_tree_row(&mut self) {
        let Some(row) = self.tree_selected() else {
            return;
        };
        let span = match &row.node {
            Node::Seq { items, .. } | Node::Map { items, .. } => {
                items.last().map(|item| item.end).unwrap_or(row.line() + 1) - row.line()
            }
            _ => 1,
        };
        let item = Item {
            key: Some(row.label.clone()),
            line: row.line(),
            end: row.line() + span,
            node: row.node.clone(),
            trailing: row.trailing.clone(),
        };
        self.file.remove_item(&item);
        let len = self.tree_rows().len();
        self.tree_selection = self.tree_selection.min(len.saturating_sub(1));
    }

    // ---- editing buffers ----

    fn handle_edit(&mut self, key: KeyEvent) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.edit = None,
            KeyCode::Enter => self.commit_edit(),
            KeyCode::Backspace => {
                if edit.cursor > 0 {
                    edit.cursor -= 1;
                    let byte = char_byte_index(&edit.buffer, edit.cursor);
                    edit.buffer.remove(byte);
                }
            }
            KeyCode::Delete => {
                if edit.cursor < edit.buffer.chars().count() {
                    let byte = char_byte_index(&edit.buffer, edit.cursor);
                    edit.buffer.remove(byte);
                }
            }
            KeyCode::Left => edit.cursor = edit.cursor.saturating_sub(1),
            KeyCode::Right => edit.cursor = (edit.cursor + 1).min(edit.buffer.chars().count()),
            KeyCode::Home => edit.cursor = 0,
            KeyCode::End => edit.cursor = edit.buffer.chars().count(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let byte = char_byte_index(&edit.buffer, edit.cursor);
                edit.buffer.insert(byte, c);
                edit.cursor += 1;
            }
            _ => {}
        }
    }

    fn commit_edit(&mut self) {
        let Some(edit) = self.edit.take() else {
            return;
        };
        let text = edit.buffer.clone();
        match edit.target {
            EditTarget::Line(line, shape) => {
                let raw = match shape {
                    Shape::Integer if !config::is_int(&text) => {
                        self.status = format!("'{}' is not a number", text);
                        return;
                    }
                    Shape::Text => {
                        if edit.quoted {
                            config::quote_always(&text)
                        } else {
                            config::quote(&text)
                        }
                    }
                    _ => text.trim().to_string(),
                };
                self.file.set_scalar(line, &raw);
                self.status = format!("{} updated", edit.title);
            }
            EditTarget::Leaf(line) => {
                if matches!(self.tree_kind(), Some(ValueKind::Integer)) && !config::is_int(&text) {
                    self.status = format!("'{}' must be a number", text);
                    return;
                }
                let raw = if edit.quoted {
                    config::quote_always(&text)
                } else {
                    config::quote(&text)
                };
                self.file.set_scalar(line, &raw);
            }
        }
    }

    /// Value kind of the collection currently open in the tree editor.
    fn tree_kind(&self) -> Option<ValueKind> {
        schema::value_kind(&self.tree_key)
    }

    fn handle_prompt(&mut self, key: KeyEvent) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => self.commit_prompt(),
            KeyCode::Backspace => {
                if prompt.cursor > 0 {
                    prompt.cursor -= 1;
                    let byte = char_byte_index(&prompt.buffer, prompt.cursor);
                    prompt.buffer.remove(byte);
                }
            }
            KeyCode::Left => prompt.cursor = prompt.cursor.saturating_sub(1),
            KeyCode::Right => {
                prompt.cursor = (prompt.cursor + 1).min(prompt.buffer.chars().count())
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let byte = char_byte_index(&prompt.buffer, prompt.cursor);
                prompt.buffer.insert(byte, c);
                prompt.cursor += 1;
            }
            _ => {}
        }
    }

    fn commit_prompt(&mut self) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        let text = prompt.buffer.clone();
        match prompt.kind {
            PromptKind::AddKey { path } => {
                let shape = match self
                    .file
                    .insert_point(&self.tree_key, &path)
                    .map(|p| p.shape)
                {
                    Some(shape) => shape,
                    None => {
                        self.status = "Cannot add here".to_string();
                        return;
                    }
                };
                if let Err(message) = config::check_key(&text) {
                    self.status = message;
                    return;
                }
                // Nested collections are created as an empty key, the value prompt only makes sense for plain maps.
                if matches!(shape, Shape::Map) {
                    self.prompt_for_add(path, Some(text));
                } else if let Err(e) = self.file.add_item(&self.tree_key, &path, Some(&text), "") {
                    self.status = e;
                }
            }
            PromptKind::QuickAdd { key } => match ConfigFile::parse_add_entry(&text) {
                Ok((appid, name)) => match self.file.add_item(&key, &[], None, &appid) {
                    Ok(line) => {
                        if let Some(name) = name {
                            self.file.set_trailing_comment(line, &name);
                        }
                        self.status = format!("Added {appid} to {key}");
                    }
                    Err(e) => self.status = e,
                },
                Err(e) => self.status = e,
            },
            PromptKind::Comment { key, index } => {
                let text = text.trim_end();
                if text.is_empty() {
                    if let Some(index) = index {
                        self.file.remove_comment_line(&key, index);
                    }
                    self.status = format!("Comment cleared for {key}");
                } else if let Some(index) = index {
                    self.file.set_comment_line(&key, index, text);
                    self.status = format!("Comment updated for {key}");
                } else {
                    self.file.insert_comment_line(&key, None, text);
                    self.status = format!("Comment added to {key}");
                }
            }
            PromptKind::Trailing { line } => {
                self.file.set_trailing_comment(line, &text);
                self.status = "Comment updated".to_string();
            }
            PromptKind::AddValue { path, key } => {
                let kind = schema::value_kind(&self.tree_key);
                let shape = match self
                    .file
                    .insert_point(&self.tree_key, &path)
                    .map(|p| p.shape)
                {
                    Some(shape) => shape,
                    None => {
                        self.status = "Cannot add here".to_string();
                        return;
                    }
                };
                let key_arg = if matches!(shape, Shape::Seq) {
                    None
                } else {
                    Some(key.as_str())
                };
                if let Err(message) = check_entry(shape, kind, key_arg, &text) {
                    self.status = message;
                    return;
                }
                let raw = if matches!(shape, Shape::Seq) || kind == Some(ValueKind::Integer) {
                    text.trim().to_string()
                } else {
                    config::quote(&text)
                };
                if let Err(e) = self.file.add_item(&self.tree_key, &path, key_arg, &raw) {
                    self.status = e;
                }
            }
        }
    }

    // ---- saving ----

    fn save(&mut self) -> Result<(), String> {
        match self.file.save() {
            Ok(()) => {
                self.status = format!(
                    "Saved {} - SLSsteam reloads it automatically",
                    self.file.path.display()
                );
                Ok(())
            }
            Err(e) => {
                self.status = format!("Save failed: {e}");
                Err(e)
            }
        }
    }

    // ---- rendering ----

    fn render(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(5),
            ])
            .split(frame.area());

        self.render_header(frame, chunks[0]);
        if self.picker.has_focus() {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(40), Constraint::Length(52)])
                .split(chunks[1]);
            self.render_body(frame, body[0]);
            let file = self.file.clone();
            self.picker.render(&file, frame, body[1]);
        } else {
            self.render_body(frame, chunks[1]);
        }
        self.render_footer(frame, chunks[2]);

        if self.confirm_quit {
            self.render_confirm(frame);
        }
        if let Some(edit) = &self.edit {
            self.render_input(frame, &edit.title, &edit.buffer, edit.cursor);
        }
        if let Some(prompt) = &self.prompt {
            self.render_input(frame, &prompt.title, &prompt.buffer, prompt.cursor);
        }
    }

    fn render_body(&mut self, frame: &mut Frame, area: Rect) {
        match self.mode {
            Mode::List => self.render_list(frame, area),
            Mode::Tree => self.render_tree(frame, area),
            Mode::Comments => self.render_comments(frame, area),
        }
    }

    /// The comment block above a setting, one line per row.
    fn render_comments(&mut self, frame: &mut Frame, area: Rect) {
        let lines = self.file.comment_block(&self.comments_key);
        let items: Vec<ListItem> = if lines.is_empty() {
            vec![ListItem::new(
                Line::from("(no comment yet - press a to add one)").dim(),
            )]
        } else {
            lines
                .iter()
                .map(|line| ListItem::new(Line::from(line.clone())))
                .collect()
        };

        let mut state = ListState::default();
        if !lines.is_empty() {
            state.select(Some(self.comment_selection.min(lines.len() - 1)));
        }
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("Comment - {}", self.comments_key)),
            )
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let title = match self.mode {
            Mode::List => "SLSsteam config",
            Mode::Tree => "SLSsteam config - editing a collection",
            Mode::Comments => "SLSsteam config - editing a comment",
        };
        let modified = if self.file.dirty() { " (modified)" } else { "" };
        let text = Line::from(vec![
            Span::styled(title, Style::new().bold()),
            Span::raw(modified.to_string()),
        ]);
        let path = Line::from(self.file.path.display().to_string()).dim();
        frame.render_widget(
            Paragraph::new(vec![text, path]).block(Block::default().borders(Borders::ALL)),
            area,
        );
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let tops = self.tops();
        let rows = self.rows();
        let mut items: Vec<ListItem> = Vec::new();

        for row in &rows {
            match row {
                Row::Section(title) => {
                    items.push(ListItem::new(Line::from(format!("-- {title} --")).bold()))
                }
                Row::Setting(index) => {
                    let setting = &schema::SETTINGS[*index];
                    let node = tops
                        .iter()
                        .find(|top| top.key == setting.key)
                        .map(|t| &t.node);
                    items.push(ListItem::new(Line::from(vec![
                        Span::raw(format!("{:<26}", setting.key)),
                        Span::raw("  "),
                        value_span(node, Some(setting.shape)),
                    ])));
                }
                Row::Other(key) => {
                    let top = tops.iter().find(|top| &top.key == key);
                    items.push(ListItem::new(Line::from(vec![
                        Span::raw(format!("{:<26}", key)),
                        Span::raw("  "),
                        value_span(top.map(|top| &top.node), top.and_then(|top| top.shape)),
                        Span::styled("  (unknown key)", Style::new().dim()),
                    ])));
                }
            }
        }

        let mut state = ListState::default();
        state.select(Some(self.selection.min(items.len().saturating_sub(1))));
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Settings"))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_tree(&mut self, frame: &mut Frame, area: Rect) {
        let top = self.file.top_for(&self.tree_key);
        let rows = self.tree_rows();
        let items: Vec<ListItem> = rows
            .iter()
            .map(|row| {
                let indent = "  ".repeat(row.depth + 1);
                let mut spans = vec![Span::raw(indent), Span::raw(format!("{:<14}", row.label))];
                if !row.value.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        row.value.clone(),
                        Style::new().fg(Color::Cyan),
                    ));
                }
                if !row.trailing.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(row.trailing.clone(), Style::new().dim()));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let title = top
            .as_ref()
            .map(|top| format!("{} ({} entries)", top.key, rows.len()))
            .unwrap_or_else(|| "entries".to_string());
        let mut state = ListState::default();
        if !rows.is_empty() {
            state.select(Some(self.tree_selection.min(rows.len() - 1)));
        }
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let help = self.help_text();
        let keys = match self.mode {
            Mode::List if self.picker.has_focus() && self.picker.is_removing() => {
                "type to filter   space remove   r back to search   t target   ctrl+s save"
            }
            Mode::List if self.picker.has_focus() && self.picker.is_editing() => {
                "keep typing to search   enter details   esc stop editing   ctrl+r remove games   ctrl+s save"
            }
            Mode::List if self.picker.has_focus() => {
                "type to search   enter details   space toggle   r remove games   t target   esc close"
            }
            Mode::List => {
                "up/down select   enter open   tab comment   g steam   a quick add   s save   q quit"
            }
            Mode::Tree => {
                "up/down select   enter edit   space toggle   a add   d delete   tab comment   esc back"
            }
            Mode::Comments => "up/down select   enter edit   a add   d delete   esc back",
        };
        let status = if self.picker.has_focus() && !self.picker.status().is_empty() {
            self.picker.status().to_string()
        } else {
            self.status.clone()
        };
        let lines = vec![Line::from(help), Line::from(keys).dim(), Line::from(status)];
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    /// Help for the selection: the file's own comment when there is one, the built-in description otherwise.
    fn help_text(&self) -> String {
        if matches!(self.mode, Mode::Comments) {
            return format!(
                "Comment above {} - shown as help text while the setting is selected",
                self.comments_key
            );
        }
        if self.picker.has_focus() {
            return "Search Steam, then space-check the games, DLCs, packages and depots you want"
                .to_string();
        }
        if matches!(self.mode, Mode::Tree) {
            if let Some(setting) = schema::find(&self.tree_key) {
                return setting.help.to_string();
            }
            return "Editing a collection".to_string();
        }
        if self.picker.has_focus() {
            return "Search Steam, then space-check the games, DLCs, packages and depots you want"
                .to_string();
        }
        let Some((key, _)) = self.selected_key() else {
            let (missing, unknown) = self.review_rows();
            return format!(
                "{} settings missing from the file, {} unknown keys present",
                missing.len(),
                unknown.len()
            );
        };
        let fallback = schema::find(&key)
            .map(|setting| setting.help.to_string())
            .unwrap_or_else(|| {
                "Not a setting this build knows; it is kept as it is unless you edit it".to_string()
            });
        match self.top_for(&key) {
            Some(top) if !top.comments.is_empty() => top
                .comments
                .iter()
                .map(|line| line.trim_start_matches('#').trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            _ => fallback,
        }
    }

    fn render_input(&self, frame: &mut Frame, title: &str, buffer: &str, cursor: usize) {
        let area = centered_rect(frame.area(), 60, 3);
        let prefix: String = buffer.chars().take(cursor).collect();
        let suffix: String = buffer.chars().skip(cursor).collect();
        let line = Line::from(vec![
            Span::styled(prefix.clone(), Style::new().fg(Color::Cyan)),
            Span::styled(suffix, Style::new().fg(Color::Cyan)),
        ]);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(line).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title.to_string()),
            ),
            area,
        );
        let cursor_x = area.x + 1 + prefix.chars().count().min(u16::MAX as usize) as u16;
        frame.set_cursor_position((cursor_x, area.y + 1));
    }

    fn render_confirm(&self, frame: &mut Frame) {
        let area = centered_rect(frame.area(), 56, 5);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from("Unsaved changes").bold(),
                Line::from("s save and quit    d discard and quit    esc cancel"),
            ])
            .block(Block::default().borders(Borders::ALL).title("Quit")),
            area,
        );
    }

    fn run_loop(mut self, mut terminal: DefaultTerminal) -> Result<(), String> {
        loop {
            terminal
                .draw(|frame| self.render(frame))
                .map_err(|e| e.to_string())?;
            // Poll so the picker can search on its own while typing pauses.
            if event::poll(std::time::Duration::from_millis(80)).map_err(|e| e.to_string())? {
                let event = event::read().map_err(|e| e.to_string())?;
                if let Event::Key(key) = event {
                    if self.handle_key(key)? {
                        return Ok(());
                    }
                }
            } else {
                self.picker.tick(&self.file);
            }
        }
    }
}

impl App {
    /// Mode name, for tests.
    #[cfg(test)]
    fn mode_name(&self) -> &'static str {
        match self.mode {
            Mode::List => "list",
            Mode::Tree => "tree",
            Mode::Comments => "comments",
        }
    }
}

impl TreeRow {
    fn line(&self) -> usize {
        match &self.node {
            Node::Null { line }
            | Node::Scalar { line, .. }
            | Node::Seq { line, .. }
            | Node::Map { line, .. } => *line,
        }
    }
}

/// True when the node can contain entries.
fn is_collection(node: &Node) -> bool {
    matches!(
        node,
        Node::Seq { .. } | Node::Map { .. } | Node::Null { .. }
    )
}

/// Flatten a node into display rows.
fn flatten(node: &Node, path: &mut Vec<usize>, depth: usize, out: &mut Vec<TreeRow>) {
    let items = match node {
        Node::Seq { items, .. } | Node::Map { items, .. } => items,
        _ => return,
    };
    for (index, item) in items.iter().enumerate() {
        path.push(index);
        let label = item.key.clone().unwrap_or_else(|| "-".to_string());
        let (value, trailing) = match &item.node {
            Node::Scalar { value, .. } => (config::unquote(value), item.trailing.clone()),
            Node::Null { .. } => (String::new(), item.trailing.clone()),
            Node::Seq { items, .. } => (format!("[{} entries]", items.len()), String::new()),
            Node::Map { items, .. } => (format!("{{{} entries}}", items.len()), String::new()),
        };
        out.push(TreeRow {
            depth,
            label,
            value,
            path: path.clone(),
            node: item.node.clone(),
            trailing,
        });
        flatten(&item.node, path, depth + 1, out);
        path.pop();
    }
}

/// Render the summary of a setting's value.
fn value_span(node: Option<&Node>, shape: Option<Shape>) -> Span<'static> {
    let Some(node) = node else {
        return Span::styled("(missing from the file)", Style::new().fg(Color::Red));
    };
    match node {
        Node::Null { .. } => Span::styled("(not set)", Style::new().dim()),
        Node::Scalar { value, .. } => match shape.unwrap_or(Shape::Text) {
            Shape::Bool => match config::parse_bool(value) {
                Some(true) => Span::styled("yes", Style::new().fg(Color::Green)),
                Some(false) => Span::styled("no", Style::new().fg(Color::Red)),
                None => Span::styled(value.clone(), Style::new().fg(Color::Yellow)),
            },
            _ => Span::styled(value.clone(), Style::new().fg(Color::Cyan)),
        },
        Node::Seq { items, .. } => Span::styled(
            format!("[{} entries]", items.len()),
            Style::new().fg(Color::Magenta),
        ),
        Node::Map { items, .. } => Span::styled(
            format!("{{{} entries}}", items.len()),
            Style::new().fg(Color::Magenta),
        ),
    }
}

/// Byte index of character `index` in `text`.
fn char_byte_index(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// A rectangle of `width` x `height` centered in `area`.
fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Open the editor for `path`. Returns an error message for the caller.
pub fn run(path: PathBuf) -> Result<(), String> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err("the config editor needs an interactive terminal".to_string());
    }
    let file = ConfigFile::load(path).map_err(|e| format!("failed to read the config: {e}"))?;
    let app = App::new(file);
    let terminal = ratatui::init();
    let result = app.run_loop(terminal);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
#List of AppIds to ex-/include
AppIds:
  - 0 #SteamApp0
  - 408490 #Hero Siege Soundtrack

#Disables Family Share license locking for self and others
DisableFamilyShareLock: yes

FakeName: \"\"
LogLevels: 0xff
";

    fn app() -> App {
        App::new(ConfigFile::parse_text("/tmp/config.yaml", SAMPLE))
    }

    #[test]
    fn initial_selection_lands_on_the_first_setting() {
        let mut app = app();
        assert_eq!(app.selection, 1, "rows: {:?}", app.rows().len());
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(
            text.contains("> DisableFamilyShareLock"),
            "rendered: {text}"
        );
    }

    #[test]
    fn opening_an_empty_idle_status_seeds_its_keys() {
        let mut app = App::new(ConfigFile::parse_text(
            "/tmp/config.yaml",
            "IdleStatus:\nSafeMode: no\n",
        ));
        let index = app
            .rows()
            .iter()
            .position(|row| match row {
                Row::Setting(i) => schema::SETTINGS[*i].key == "IdleStatus",
                _ => false,
            })
            .unwrap();
        app.selection = index;
        app.open();
        assert_eq!(app.mode_name(), "tree");
        assert!(app
            .file
            .text()
            .contains("IdleStatus:\n  AppId: 0\n  Title: \"\"\n"));
        assert!(app.file.dirty());
    }

    #[test]
    fn home_and_end_land_on_settings_not_sections() {
        let mut app = app();
        app.jump(false);
        assert_eq!(app.selected_setting().unwrap().key, "ExtendedLogging");
        app.jump(true);
        assert_eq!(
            app.selected_setting().unwrap().key,
            "DisableFamilyShareLock"
        );
    }

    /// Move the settings selection to `key`.
    fn select(app: &mut App, key: &str) {
        app.selection = app
            .rows()
            .iter()
            .position(|row| match row {
                Row::Setting(index) => schema::SETTINGS[*index].key == key,
                Row::Other(other) => other == key,
                Row::Section(_) => false,
            })
            .unwrap();
    }

    #[test]
    fn tab_edits_the_comment_above_a_key() {
        let mut app = app();
        select(&mut app, "DisableFamilyShareLock");
        app.open_comments();
        assert_eq!(app.mode_name(), "comments");
        assert!(
            app.prompt.is_none(),
            "an existing comment is shown, not typed"
        );

        // a adds a line at the end of the block
        app.handle_comments(KeyEvent::from(KeyCode::Char('a')))
            .unwrap();
        app.prompt.as_mut().unwrap().buffer = "extra note".to_string();
        app.commit_prompt();
        assert!(app
            .file
            .text()
            .contains("#extra note\nDisableFamilyShareLock"));

        // enter edits the selected line
        app.comment_selection = 1;
        app.handle_comments(KeyEvent::from(KeyCode::Enter)).unwrap();
        app.prompt.as_mut().unwrap().buffer = "renamed".to_string();
        app.commit_prompt();
        assert!(app.file.text().contains("#renamed\nDisableFamilyShareLock"));

        // d removes it again
        app.handle_comments(KeyEvent::from(KeyCode::Char('d')))
            .unwrap();
        assert!(!app.file.text().contains("#renamed"));
    }

    #[test]
    fn tab_on_a_key_without_a_comment_starts_typing_one() {
        let mut app = App::new(ConfigFile::parse_text("/tmp/config.yaml", "SafeMode: no\n"));
        select(&mut app, "SafeMode");
        app.open_comments();
        assert_eq!(app.mode_name(), "comments");
        assert!(app.prompt.is_some(), "typing starts straight away");

        app.prompt.as_mut().unwrap().buffer = "deck note".to_string();
        app.commit_prompt();
        assert_eq!(app.file.text(), "#deck note\nSafeMode: no\n");
    }

    #[test]
    fn comments_and_quick_add_need_the_key_in_the_file() {
        let mut app = App::new(ConfigFile::parse_text("/tmp/config.yaml", "SafeMode: no\n"));
        app.jump(true); // DisableFamilyShareLock is missing here
        app.open_comments();
        assert_eq!(app.mode_name(), "list");
        assert!(app.status.contains("not in the config file"));

        select(&mut app, "AppIds");
        app.quick_add();
        assert!(app.prompt.is_none());
        assert!(app.status.contains("not in the config file"));
    }

    #[test]
    fn tab_in_the_tree_edits_an_inline_comment() {
        let text = "AppIds:\n  - 440 #Half-Life 2\n";
        let mut app = App::new(ConfigFile::parse_text("/tmp/config.yaml", text));
        app.mode = Mode::Tree;
        app.tree_key = "AppIds".to_string();
        app.tree_selection = 0;
        app.edit_trailing_comment();
        app.prompt.as_mut().unwrap().buffer = "Half-Life".to_string();
        app.commit_prompt();
        assert_eq!(app.file.text(), "AppIds:\n  - 440 #Half-Life\n");
    }

    #[test]
    fn quick_add_writes_appid_and_name() {
        let mut app = app();
        select(&mut app, "DisableFamilyShareLock");
        app.quick_add();
        assert!(app.prompt.is_none(), "only AppId lists can quick add");
        assert!(app.status.contains("AppId lists"));

        select(&mut app, "AppIds");
        app.quick_add();
        app.prompt.as_mut().unwrap().buffer = "408490 Hero Siege Soundtrack".to_string();
        app.commit_prompt();
        assert!(app
            .file
            .text()
            .contains("  - 408490 #Hero Siege Soundtrack\n"));
        assert!(app.status.contains("Added 408490"));

        // nonsense is rejected
        app.quick_add();
        app.prompt.as_mut().unwrap().buffer = "not-a-number".to_string();
        app.commit_prompt();
        assert!(app.status.contains("not an AppId"));
    }

    #[test]
    fn list_rows_cover_every_setting() {
        let app = app();
        let rows = app.rows();
        let settings = rows
            .iter()
            .filter(|row| matches!(row, Row::Setting(_)))
            .count();
        assert_eq!(settings, schema::SETTINGS.len());
    }

    #[test]
    fn toggling_a_boolean_edits_the_line() {
        let mut app = app();
        app.selection = app
            .rows()
            .iter()
            .position(|row| match row {
                Row::Setting(index) => schema::SETTINGS[*index].key == "DisableFamilyShareLock",
                _ => false,
            })
            .unwrap();
        app.open();
        assert!(app.file.text().contains("DisableFamilyShareLock: no"));
        assert!(app.file.dirty());
    }

    #[test]
    fn numbers_are_validated_before_writing() {
        let mut app = app();
        app.selection = app
            .rows()
            .iter()
            .position(|row| match row {
                Row::Setting(index) => schema::SETTINGS[*index].key == "LogLevels",
                _ => false,
            })
            .unwrap();
        app.open();
        let edit = app.edit.as_mut().unwrap();
        assert_eq!(edit.buffer, "0xff");
        edit.buffer = "not a number".to_string();
        app.commit_edit();
        assert!(app.file.text().contains("LogLevels: 0xff"));
        assert!(app.status.contains("not a number"));

        app.open();
        app.edit.as_mut().unwrap().buffer = "0x90".to_string();
        app.commit_edit();
        assert!(app.file.text().contains("LogLevels: 0x90"));
    }

    #[test]
    fn text_settings_are_quoted_when_needed() {
        let mut app = app();
        app.selection = app
            .rows()
            .iter()
            .position(|row| match row {
                Row::Setting(index) => schema::SETTINGS[*index].key == "FakeName",
                _ => false,
            })
            .unwrap();
        app.open();
        app.edit.as_mut().unwrap().buffer = "REAL Ratep".to_string();
        app.commit_edit();
        assert!(app.file.text().contains("FakeName: \"REAL Ratep\""));

        app.open();
        app.edit.as_mut().unwrap().buffer = "yes".to_string();
        app.commit_edit();
        assert!(app.file.text().contains("FakeName: \"yes\""));
    }

    #[test]
    fn tree_rows_flatten_nested_collections() {
        let text = "DlcData:\n  123:\n    456: \"Some DLC\"\n";
        let mut app = App::new(ConfigFile::parse_text("/tmp/config.yaml", text));
        app.tree_key = "DlcData".to_string();
        let rows = app.tree_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "123");
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].label, "456");
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[1].value, "Some DLC");
        assert_eq!(rows[1].path, vec![0, 0]);
    }

    #[test]
    fn adding_entries_through_prompts() {
        let mut app = app();
        app.mode = Mode::Tree;
        app.tree_key = "AppIds".to_string();
        app.tree_selection = 0;
        // Add an AppId to the sequence.
        app.start_add();
        assert!(matches!(
            app.prompt.as_ref().map(|p| &p.kind),
            Some(PromptKind::AddValue { .. })
        ));
        app.prompt.as_mut().unwrap().buffer = "730".to_string();
        app.commit_prompt();
        assert!(app.file.text().contains("  - 730\n"));

        // A wrong entry is rejected.
        app.start_add();
        app.prompt.as_mut().unwrap().buffer = "not-a-number".to_string();
        app.commit_prompt();
        assert!(app.status.contains("not an AppId"));
        assert!(!app.file.text().contains("not-a-number"));
    }

    #[test]
    fn adding_a_map_entry_takes_two_prompts_and_validates() {
        let text = "FakeAppIds:\n  0: 480\n";
        let mut app = App::new(ConfigFile::parse_text("/tmp/config.yaml", text));
        app.mode = Mode::Tree;
        app.tree_key = "FakeAppIds".to_string();
        app.start_add();
        assert!(matches!(
            app.prompt.as_ref().map(|p| &p.kind),
            Some(PromptKind::AddKey { .. })
        ));
        app.prompt.as_mut().unwrap().buffer = "221410".to_string();
        app.commit_prompt();
        assert!(matches!(
            app.prompt.as_ref().map(|p| &p.kind),
            Some(PromptKind::AddValue { .. })
        ));
        app.prompt.as_mut().unwrap().buffer = "480".to_string();
        app.commit_prompt();
        assert!(app.file.text().contains("  221410: 480\n"));

        // A text value where a number is expected is rejected.
        app.start_add();
        app.prompt.as_mut().unwrap().buffer = "440".to_string();
        app.commit_prompt();
        app.prompt.as_mut().unwrap().buffer = "hello".to_string();
        app.commit_prompt();
        assert!(app.status.contains("must be a number"));
    }

    #[test]
    fn deleting_an_entry_removes_only_that_entry() {
        let text = "FakeAppIds:\n  0: 480 #unowned\n  221410: 221410\n";
        let mut app = App::new(ConfigFile::parse_text("/tmp/config.yaml", text));
        app.mode = Mode::Tree;
        app.tree_key = "FakeAppIds".to_string();
        app.tree_selection = 0;
        app.delete_tree_row();
        assert_eq!(app.file.text(), "FakeAppIds:\n  221410: 221410\n");
    }

    #[test]
    fn help_prefers_the_files_own_comments() {
        let mut app = app();
        app.selection = app
            .rows()
            .iter()
            .position(|row| match row {
                Row::Setting(index) => schema::SETTINGS[*index].key == "DisableFamilyShareLock",
                _ => false,
            })
            .unwrap();
        assert!(app.help_text().contains("Family Share"));
    }

    #[test]
    fn unknown_keys_get_their_own_section() {
        let text = "SafeMode: no\nSomeNewSetting: 1\n";
        let mut app = App::new(ConfigFile::parse_text("/tmp/config.yaml", text));
        let rows = app.rows();
        assert!(rows
            .iter()
            .any(|row| matches!(row, Row::Other(key) if key == "SomeNewSetting")));
        app.selection = rows
            .iter()
            .position(|row| matches!(row, Row::Other(_)))
            .unwrap();
        assert_eq!(app.selected_key().unwrap().0, "SomeNewSetting");
        // It can be edited like any other setting.
        app.open();
        app.edit.as_mut().unwrap().buffer = "2".to_string();
        app.commit_edit();
        assert!(app.file.text().contains("SomeNewSetting: 2"));
    }

    #[test]
    fn missing_settings_are_reported() {
        let app = app();
        let (missing, unknown) = app.review_rows();
        assert!(!missing.iter().any(|setting| setting.key == "LogLevels"));
        assert!(!missing.iter().any(|setting| setting.key == "AppIds"));
        assert!(missing.iter().any(|setting| setting.key == "SafeMode"));
        assert!(unknown.is_empty());
    }
}
