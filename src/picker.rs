//! The Steam picker: search the Steam store, then check games, DLCs and packages in or out of the SLSsteam lists.
//!
//! Rows are checkboxes grouped by what they are.
//! Space toggles the highlighted row and writes straight into the config, space on a group header toggles the whole group, and `enter` on a game pulls in its DLCs and packages.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use std::collections::HashMap;

use crate::appinfo::SteamApp;
use crate::config::{ConfigFile, Node};
use crate::schema::Shape;
use crate::steam::{self, Item, Kind};

/// Target list for games and DLCs; the picker can switch between them.
const APP_TARGETS: [&str; 2] = ["AdditionalApps", "AppIds"];
/// How many result rows to ask the store for.
const RESULT_LIMIT: u32 = 100;
/// How many names to look up per DLC/package group.
const NAME_LOOKUPS: usize = 5;
/// How long typing has to pause before the search runs by itself.
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);

/// One entry of a group.
struct Group {
    title: String,
    /// Index into `entries` of the group's first item.
    start: usize,
    len: usize,
}

/// A selectable row: a group header or an entry.
enum Row {
    Header(usize),
    Entry(usize),
}

/// Which side of the picker is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// Search Steam for games to add.
    Search,
    /// List the games already in the target so they can be removed.
    Remove,
}

/// What the picker wants the app to do with a key it did not use.
pub enum Outcome {
    Handled,
    Pass(KeyEvent),
}

pub struct Picker {
    open: bool,
    editing: bool,
    query: String,
    /// Search results, kept while a game is focused.
    results: Vec<Item>,
    /// Rows backing the current groups.
    entries: Vec<Item>,
    groups: Vec<Group>,
    /// Index into `results` of the focused game, if any.
    focused: Option<usize>,
    /// Related entries per appId (DLCs, packages) seen this session.
    details: HashMap<u64, (Vec<Item>, Vec<Item>)>,
    /// Steam apps from the local client cache, for offline fuzzy search.
    index: Option<Vec<SteamApp>>,
    /// Query changed since the last search, with the moment it changed.
    pending: Option<std::time::Instant>,
    /// The term the current results belong to.
    searched: String,
    /// Search or remove view.
    view: View,
    selection: usize,
    app_target: usize,
    status: String,
}

/// AppIds the config names itself: numeric entries with a trailing comment (`- 570940 # DARK SOULS™: REMASTERED`) and `GameTitles` values.
fn config_names(file: &ConfigFile) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = Vec::new();
    for top in file.parse() {
        let items = match &top.node {
            Node::Seq { items, .. } | Node::Map { items, .. } => items,
            _ => continue,
        };
        for item in items {
            // Sequence entries hold the appId as their value, map entries as their key.
            let appid = match item.key.as_deref() {
                Some(key) => key.trim().parse::<u64>().ok(),
                None => item.scalar().trim().parse::<u64>().ok(),
            };
            let Some(appid) = appid else {
                continue;
            };
            let name = if top.key == "GameTitles" {
                // Here the value is the name: `10: "Cunt-Striker"`.
                item.scalar().trim().trim_matches('"').to_string()
            } else {
                item.trailing.trim_start_matches('#').trim().to_string()
            };
            if !name.is_empty() && !out.iter().any(|(id, _)| *id == appid) {
                out.push((appid, name));
            }
        }
    }
    out
}

impl Picker {
    pub fn new() -> Picker {
        Picker {
            open: false,
            editing: false,
            query: String::new(),
            results: Vec::new(),
            entries: Vec::new(),
            groups: Vec::new(),
            focused: None,
            details: HashMap::new(),
            index: None,
            pending: None,
            searched: String::new(),
            view: View::Search,
            selection: 0,
            app_target: 0,
            status: String::new(),
        }
    }

    /// True while the search box is being typed into.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// The panel is open, which also means it has the focus.
    pub fn has_focus(&self) -> bool {
        self.open
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    /// The remove view is showing.
    pub fn is_removing(&self) -> bool {
        self.view == View::Remove
    }

    /// Switch between searching Steam and removing configured games.
    pub fn toggle_remove(&mut self, file: &ConfigFile) {
        self.view = match self.view {
            View::Search => View::Remove,
            View::Remove => View::Search,
        };
        self.selection = 0;
        match self.view {
            View::Remove => self.refresh_remove(file),
            View::Search => {
                if self.query.trim().is_empty() {
                    self.results.clear();
                    self.entries.clear();
                    self.groups.clear();
                    self.status = "Type to search".to_string();
                } else {
                    self.run_search(file);
                }
            }
        }
    }

    /// List the games already in the target list, filtered by the query.
    fn refresh_remove(&mut self, file: &ConfigFile) {
        let target = self.app_target_key();
        let Some(top) = file.top_for(target) else {
            self.results.clear();
            self.entries.clear();
            self.groups.clear();
            self.status = format!("{target} is not in this config");
            return;
        };
        let config_items = match &top.node {
            Node::Seq { items, .. } => items.clone(),
            _ => Vec::new(),
        };

        let mut items: Vec<Item> = Vec::new();
        for entry in &config_items {
            let Some(id) = entry.scalar().trim().parse::<u64>().ok() else {
                continue;
            };
            let comment = entry.trailing.trim_start_matches('#').trim();
            let (name, app_type) = self.index_name(id, comment);
            items.push(Item::typed(Kind::App, id, &name, &app_type));
        }

        let len = items.len();
        let query = self.query.trim().to_string();
        let filtered = if query.is_empty() {
            items
        } else {
            steam::rank_items(&items, &query, RESULT_LIMIT as usize)
        };
        let shown = filtered.len();
        self.results = filtered;
        self.entries = self.results.clone();
        self.groups = vec![Group {
            title: format!("In {target} ({shown} of {len})"),
            start: 0,
            len: shown,
        }];
        self.focused = None;
        self.selection = usize::from(shown > 0);
        self.status = if shown == 0 {
            format!("Nothing in {target} matches '{}'", self.query.trim())
        } else {
            format!("space removes a game from {target}; t switches target")
        };
    }

    /// Name and type for an appId: the file's own comment first, then the index.
    fn index_name(&self, id: u64, comment: &str) -> (String, String) {
        let indexed = self
            .index
            .as_deref()
            .and_then(|apps| apps.iter().find(|app| app.appid == id));
        let name = if comment.is_empty() {
            indexed.map(|app| app.name.clone()).unwrap_or_default()
        } else {
            comment.to_string()
        };
        let app_type = indexed.map(|app| app.app_type.clone()).unwrap_or_default();
        (name, app_type)
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        if self.open {
            self.editing = true;
            self.status = "Type a game name and press enter".to_string();
        } else {
            self.editing = false;
        }
    }

    /// Close the panel; the settings list takes the focus back.
    fn close(&mut self) {
        self.open = false;
        self.editing = false;
        self.status.clear();
    }

    /// Target list for games and DLCs.
    fn app_target_key(&self) -> &'static str {
        APP_TARGETS[self.app_target]
    }

    /// Target list for an entry kind.
    fn target_for(&self, kind: Kind) -> &'static str {
        match kind {
            Kind::App | Kind::Dlc => self.app_target_key(),
            Kind::Package => "AdditionalPackages",
            Kind::Depot => "AdditionalDepots",
        }
    }

    /// Flat rows built from the groups.
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (index, group) in self.groups.iter().enumerate() {
            rows.push(Row::Header(index));
            rows.extend((0..group.len).map(|offset| Row::Entry(group.start + offset)));
        }
        rows
    }

    /// The search input, with the caret when it is being typed into.
    fn render_search_box(&self, frame: &mut Frame, area: Rect) {
        let title = if self.view == View::Remove {
            format!(
                "Remove games - from {} (r back, t switch)",
                self.app_target_key()
            )
        } else {
            format!("Steam - games and DLCs go to {} (t)", self.app_target_key())
        };
        let block = Block::default().borders(Borders::ALL).title(title);
        let shown = if self.query.is_empty() && !self.editing {
            "(press a letter to search)".to_string()
        } else {
            self.query.clone()
        };
        let line = if self.editing {
            Line::from(vec![
                Span::raw("search: "),
                Span::styled(shown, Style::new().fg(Color::Cyan)),
            ])
        } else {
            Line::from(vec![
                Span::raw("search: "),
                Span::styled(shown, Style::new().fg(Color::Cyan)),
            ])
            .dim()
        };
        frame.render_widget(Paragraph::new(line).block(block), area);
        if self.editing {
            let width = 8 + self.query.chars().count() as u16;
            frame.set_cursor_position((
                area.x + 1 + width.min(area.width.saturating_sub(2)),
                area.y + 1,
            ));
        }
    }

    /// The highlighted row, copied out so callers can mutate while matching.
    fn selected(&self) -> Option<Row> {
        let rows = self.rows();
        rows.get(self.selection).map(|row| match row {
            Row::Header(index) => Row::Header(*index),
            Row::Entry(index) => Row::Entry(*index),
        })
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.rows().len();
        if len == 0 {
            return;
        }
        let next = self.selection as isize + delta;
        self.selection = next.clamp(0, len as isize - 1) as usize;
    }

    /// True when the entry is already in its target list.
    fn is_checked(&self, file: &ConfigFile, item: &Item) -> bool {
        let target = self.target_for(item.kind);
        let Some(top) = file.top_for(target) else {
            return false;
        };
        match &top.node {
            Node::Seq { items, .. } => items
                .iter()
                .any(|entry| entry.scalar().trim() == item.id.to_string()),
            _ => false,
        }
    }

    /// Related entries of an app: DLCs and packages, from the session cache or from Steam.
    fn related_items(&mut self, appid: u64) -> Result<(Vec<Item>, Vec<Item>), String> {
        if let Some(cached) = self.details.get(&appid) {
            return Ok(cached.clone());
        }
        let (dlc_ids, package_ids) = steam::details(appid)?;
        let named = |kind: Kind, ids: &[u64]| -> Vec<Item> {
            let names = steam::names(kind, ids, NAME_LOOKUPS);
            ids.iter()
                .map(|id| Item {
                    kind,
                    id: *id,
                    name: names
                        .iter()
                        .find(|(named, _)| named == id)
                        .map(|(_, name)| name.clone())
                        .unwrap_or_default(),
                    app_type: String::new(),
                    live: false,
                    live_rank: 0,
                })
                .collect()
        };
        let dlcs = named(Kind::Dlc, &dlc_ids);
        let packages = named(Kind::Package, &package_ids);
        self.details.insert(appid, (dlcs.clone(), packages.clone()));
        Ok((dlcs, packages))
    }

    /// Add an entry to its target list.
    fn add_entry(&mut self, file: &mut ConfigFile, item: &Item) -> bool {
        let target = self.target_for(item.kind);
        if file.top_for(target).is_none() {
            self.status = format!("{target} is not in this config");
            return false;
        }
        // Picker targets are always AppId lists, even when the key is not in this build's schema (the private lists, for example).
        match file.add_item_as(target, &[], None, &item.id.to_string(), Some(Shape::Seq)) {
            Ok(line) => {
                if !item.name.is_empty() {
                    file.set_trailing_comment(line, &item.name);
                }
                self.status = format!("Added {} to {target}", item.id);
                true
            }
            Err(e) => {
                self.status = e;
                false
            }
        }
    }

    /// Remove an entry from its target list.
    fn remove_entry(&mut self, file: &mut ConfigFile, item: &Item) -> bool {
        let target = self.target_for(item.kind);
        let Some(top) = file.top_for(target) else {
            self.status = format!("{target} is not in this config");
            return false;
        };
        let Node::Seq { items, .. } = &top.node else {
            self.status = format!("{target} is not a list");
            return false;
        };
        let found = items
            .iter()
            .find(|entry| entry.scalar().trim() == item.id.to_string())
            .cloned();
        match found {
            Some(entry) => {
                file.remove_item(&entry);
                self.status = format!("Removed {} from {target}", item.id);
                true
            }
            None => false,
        }
    }

    /// Add or remove an entry.
    /// Checking a game also takes its DLCs and its packages along, so one key press configures the whole game.
    fn toggle_entry(&mut self, file: &mut ConfigFile, item: &Item) {
        if self.is_checked(file, item) {
            self.remove_entry(file, item);
            // Take what belongs to the game along, but only when it is known from this session, so nothing is fetched just to uncheck.
            if item.kind == Kind::App {
                if let Some((dlcs, packages)) = self.details.get(&item.id).cloned() {
                    let mut related = 0;
                    for entry in dlcs.iter().chain(packages.iter()) {
                        if self.is_checked(file, entry) && self.remove_entry(file, entry) {
                            related += 1;
                        }
                    }
                    self.status = if related == 0 {
                        format!("Removed {}", item.id)
                    } else {
                        format!("Removed {} and {related} related entries", item.id)
                    };
                }
            }
            return;
        }

        if !self.add_entry(file, item) {
            return;
        }
        if item.kind != Kind::App {
            return;
        }

        self.status = format!("Added {}. Looking up its DLCs and packages...", item.id);
        match self.related_items(item.id) {
            Ok((dlcs, packages)) => {
                let mut added = 0;
                for entry in dlcs.iter().chain(packages.iter()) {
                    if !self.is_checked(file, entry) && self.add_entry(file, entry) {
                        added += 1;
                    }
                    if entry.kind == Kind::Package && file.top_for("AdditionalPackages").is_none() {
                        self.status = format!(
                            "Added {} (packages need AdditionalPackages, which this config lacks)",
                            item.id
                        );
                        return;
                    }
                }
                self.status = format!("Added {} and {added} related entries", item.id);
            }
            Err(e) => {
                self.status = format!("Added {} ({e})", item.id);
            }
        }
    }

    /// Toggle the highlighted row: an entry is added or removed, a group header toggles all of its entries.
    fn toggle_selected(&mut self, file: &mut ConfigFile) {
        match self.selected() {
            Some(Row::Header(index)) => self.toggle_group(file, index),
            Some(Row::Entry(index)) => {
                let item = self.entries[index].clone();
                self.toggle_entry(file, &item);
                if self.view == View::Remove {
                    self.refresh_remove(file);
                }
            }
            None => {}
        }
    }

    /// Space on a header toggles every entry of that group.
    fn toggle_group(&mut self, file: &mut ConfigFile, group_index: usize) {
        let Some(group) = self
            .groups
            .get(group_index)
            .map(|group| (group.start, group.len, group.title.clone()))
        else {
            return;
        };
        let items: Vec<Item> = self.entries[group.0..group.0 + group.1].to_vec();
        let all_checked = items.iter().all(|item| self.is_checked(file, item));
        let mut touched = 0;
        for item in &items {
            if self.is_checked(file, item) != all_checked {
                continue;
            }
            let changed = if all_checked {
                self.remove_entry(file, item)
            } else {
                self.add_entry(file, item)
            };
            if changed {
                touched += 1;
            }
        }
        let action = if all_checked { "Removed" } else { "Added" };
        let extra = if items.iter().any(|item| item.kind == Kind::App) {
            " (open a game with enter to take its DLCs and packages along)"
        } else {
            ""
        };
        self.status = format!("{action} {touched} entries of {}{extra}", group.2);
    }

    /// Run the search for the current query.
    fn run_search(&mut self, file: &ConfigFile) {
        if self.query.trim().is_empty() {
            self.status = "Type a game name first".to_string();
            return;
        }
        self.ensure_index(file);
        self.status = format!("Searching for '{}'...", self.query.trim());

        let mut results = self
            .index
            .as_deref()
            .map(|apps| steam::search_index(apps, &self.query, RESULT_LIMIT as usize))
            .unwrap_or_default();
        // Loose hits from the local index are not a reason to skip Steam: it may well know the app we are actually looking for ("hello" matches many DLC names loosely, but Steam has Hello Neighbor).
        let from_index = results.len();
        let mut live = 0;
        // Steam is always asked: it knows apps the local cache does not (and ranks popular matches first). The cache makes it cheap after the first search of a term.
        {
            match steam::search(&self.query, RESULT_LIMIT) {
                Ok(found) => {
                    for item in found {
                        if !results.iter().any(|known| known.id == item.id) {
                            results.push(item);
                            live += 1;
                        }
                    }
                }
                Err(e) if results.is_empty() => {
                    self.searched = self.query.trim().to_string();
                    self.results.clear();
                    self.entries.clear();
                    self.groups.clear();
                    self.focused = None;
                    self.status = e;
                    return;
                }
                Err(_) => {}
            }
        }
        // Rank the merged candidates with the same rules, so a loose live hit can never sit next to a real match.
        results = steam::rank_items(&results, &self.query, RESULT_LIMIT as usize);

        // Whether the ranking found real matches or only fuzzy ones.
        let strict = results.iter().any(|item| {
            matches!(
                steam::match_score(&item.name, &self.query, &item.app_type),
                Some((tier, _)) if tier < steam::Tier::Loose
            )
        });
        {
            self.searched = self.query.trim().to_string();
            let len = results.len();
            self.results = results;
            self.entries = self.results.clone();
            self.groups = vec![Group {
                title: format!("Games ({len})"),
                start: 0,
                len,
            }];
            self.focused = None;
            self.selection = usize::from(len > 0);
            self.status = if len == 0 {
                format!("Nothing found for '{}'", self.query.trim())
            } else if !strict {
                format!(
                    "No exact match for '{}' - {len} similar names",
                    self.query.trim()
                )
            } else if from_index > 0 && live > 0 {
                format!(
                    "{len} results (local index + Steam) - enter shows DLCs, packages and depots"
                )
            } else if from_index > 0 {
                format!("{len} results from the local Steam index - enter shows DLCs, packages and depots")
            } else {
                format!("{len} results - enter shows a game's DLCs and packages")
            };
        }
        let _ = file;
    }

    /// Note that the query changed; `tick` searches once typing pauses.
    fn touch(&mut self) {
        self.pending = Some(std::time::Instant::now());
    }

    /// Search after a pause in typing, so results follow along as you type without hammering Steam on every keystroke.
    pub fn tick(&mut self, file: &ConfigFile) {
        if !self.open || self.view == View::Remove {
            return;
        }
        let Some(changed) = self.pending else {
            return;
        };
        if changed.elapsed() < SEARCH_DEBOUNCE {
            return;
        }
        let term = self.query.trim().to_string();
        if term.chars().count() < 2 {
            self.pending = None;
            return;
        }
        if term == self.searched && !self.editing {
            self.pending = None;
            return;
        }
        self.pending = None;
        self.run_search(file);
    }

    /// Load the local Steam app index once; it makes search instant and works offline.
    ///
    /// Besides the client cache, the config's own annotations are indexed: `- 570940 # DARK SOULS™: REMASTERED` teaches the launcher the name of a game the client cache may not know, which is what makes `dsr` resolve.
    fn ensure_index(&mut self, file: &ConfigFile) {
        if self.index.is_some() {
            return;
        }
        let mut apps = crate::appinfo::load();
        for (appid, name) in config_names(file) {
            let known = apps.iter_mut().find(|app| app.appid == appid);
            match known {
                Some(app) if app.name.is_empty() => app.name = name,
                Some(_) => {}
                None => apps.push(SteamApp {
                    appid,
                    name,
                    app_type: "game".to_string(),
                    dlc: Vec::new(),
                    depots: Vec::new(),
                }),
            }
        }
        self.index = Some(apps);
    }

    /// Focus a game: show only it and its DLCs and packages, so nothing has to be scrolled past to reach them.
    fn focus_app(&mut self, index: usize) {
        let Some(game) = self.results.get(index).cloned() else {
            return;
        };
        let name = if game.name.is_empty() {
            game.id.to_string()
        } else {
            game.name.clone()
        };
        self.status = format!("Looking up {name}...");

        let local = self
            .index
            .as_deref()
            .and_then(|apps| apps.iter().find(|app| app.appid == game.id))
            .cloned();

        let (dlcs, packages) = match self.related_items(game.id) {
            Ok(items) => items,
            // Offline: fall back to the DLC ids the client cache knows.
            Err(e) => match &local {
                Some(app) if !app.dlc.is_empty() => {
                    (self.named_from_index(Kind::Dlc, &app.dlc), Vec::new())
                }
                _ => {
                    self.status = e;
                    return;
                }
            },
        };

        let depots: Vec<Item> = local
            .as_ref()
            .map(|app| {
                app.depots
                    .iter()
                    .map(|id| Item {
                        kind: Kind::Depot,
                        id: *id,
                        name: String::new(),
                        app_type: String::new(),
                        live: false,
                        live_rank: 0,
                    })
                    .collect()
            })
            .unwrap_or_default();

        self.focused = Some(index);
        let mut entries: Vec<Item> = vec![game];
        let mut groups: Vec<Group> = vec![Group {
            title: "Selected game".to_string(),
            start: 0,
            len: 1,
        }];

        let mut push_group = |kind: Kind, title: String, items: Vec<Item>| {
            if items.is_empty() {
                return;
            }
            let start = entries.len();
            let len = items.len();
            entries.extend(items);
            let _ = kind;
            groups.push(Group { title, start, len });
        };

        push_group(Kind::Dlc, format!("DLCs of {name} ({})", dlcs.len()), dlcs);
        push_group(
            Kind::Package,
            format!("Packages of {name} ({})", packages.len()),
            packages,
        );
        let depot_count = depots.len();
        push_group(
            Kind::Depot,
            format!("Depots of {name} ({depot_count})"),
            depots,
        );

        self.entries = entries;
        self.groups = groups;
        // Land on the DLC group when there is one.
        self.selection = self
            .groups
            .get(1)
            .map(|_| 2)
            .filter(|index| *index < self.rows().len())
            .unwrap_or(0);
        self.status = if depot_count == 0 {
            format!("DLCs and packages of {name} - esc goes back to the results")
        } else {
            format!("DLCs, packages and {depot_count} depots of {name} - esc goes back")
        };
    }

    /// Names for ids the local index knows, used when Steam is unreachable.
    fn named_from_index(&self, kind: Kind, ids: &[u64]) -> Vec<Item> {
        ids.iter()
            .map(|id| Item {
                kind,
                id: *id,
                name: self
                    .index
                    .as_deref()
                    .and_then(|apps| apps.iter().find(|app| app.appid == *id))
                    .map(|app| app.name.clone())
                    .unwrap_or_default(),
                app_type: String::new(),
                live: false,
                live_rank: 0,
            })
            .collect()
    }

    /// Leave the focused game and show the search results again.
    fn back_to_results(&mut self) {
        if self.focused.take().is_none() {
            return;
        }
        let len = self.results.len();
        self.entries = self.results.clone();
        self.groups = vec![Group {
            title: format!("Games ({len})"),
            start: 0,
            len,
        }];
        self.selection = usize::from(len > 0);
        self.status = format!("{len} results");
    }

    /// Handle a key press; `Pass` hands it back to the app (save, quit).
    pub fn handle_key(&mut self, file: &mut ConfigFile, key: KeyEvent) -> Outcome {
        if key.kind != KeyEventKind::Press {
            return Outcome::Handled;
        }

        if self.editing {
            match key.code {
                KeyCode::Char('s') | KeyCode::Char('q')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    return Outcome::Pass(key)
                }
                // Ctrl+R opens the remove list even while typing, where a plain `r` has to stay a letter.
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.toggle_remove(file)
                }
                // In the remove view the filter stays active while games are taken out, so space keeps working.
                KeyCode::Char(' ') if self.view == View::Remove => self.toggle_selected(file),
                KeyCode::Enter if self.view == View::Remove => {
                    self.editing = false;
                    self.toggle_selected(file);
                }
                KeyCode::Enter => {
                    self.editing = false;
                    self.run_search(file);
                }
                KeyCode::Esc => self.editing = false,
                KeyCode::Backspace => {
                    self.query.pop();
                    if self.view == View::Remove {
                        self.refresh_remove(file);
                    } else {
                        self.touch();
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.push(c);
                    if self.view == View::Remove {
                        self.refresh_remove(file);
                    } else {
                        self.touch();
                    }
                }
                _ => {}
            }
            return Outcome::Handled;
        }

        match key.code {
            // Ctrl+S saves and Ctrl+Q quits; a plain letter always types, so a query like "souls" cannot trigger a save by accident.
            KeyCode::Char('s') | KeyCode::Char('q')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                return Outcome::Pass(key)
            }
            KeyCode::Esc => {
                if self.view == View::Remove {
                    self.toggle_remove(file);
                } else if self.focused.is_some() {
                    self.back_to_results();
                } else {
                    self.close();
                }
            }
            KeyCode::Char('r') => self.toggle_remove(file),
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char(' ') => self.toggle_selected(file),
            KeyCode::Enter => match self.selected() {
                Some(Row::Entry(index)) => {
                    let item = self.entries[index].clone();
                    if item.kind == Kind::App {
                        let result_index = self
                            .results
                            .iter()
                            .position(|result| result.id == item.id)
                            .unwrap_or(0);
                        self.focus_app(result_index);
                    } else if item.name.is_empty() {
                        let names = steam::names(item.kind, &[item.id], 1);
                        if let Some((_, name)) = names.into_iter().next() {
                            self.entries[index].name = name;
                        }
                    }
                }
                Some(Row::Header(index)) => self.toggle_group(file, index),
                None => {}
            },
            KeyCode::Char('t') => {
                self.app_target = (self.app_target + 1) % APP_TARGETS.len();
                if self.view == View::Remove {
                    // The removal list has to follow the target list.
                    self.refresh_remove(file);
                } else {
                    self.status = format!("Games and DLCs go to {}", self.app_target_key());
                }
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !c.is_whitespace() =>
            {
                self.query.clear();
                self.query.push(c);
                self.editing = true;
                self.touch();
            }
            _ => {}
        }
        Outcome::Handled
    }

    // ---- rendering ----

    pub fn render(&mut self, file: &ConfigFile, frame: &mut Frame, area: Rect) {
        let chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Length(3),
                ratatui::layout::Constraint::Min(3),
            ])
            .split(area);
        self.render_search_box(frame, chunks[0]);

        let area = chunks[1];
        let rows = self.rows();
        let mut items: Vec<ListItem> = Vec::new();
        for row in &rows {
            match row {
                Row::Header(index) => {
                    let group = &self.groups[*index];
                    items.push(ListItem::new(
                        Line::from(format!("-- {} --", group.title)).bold(),
                    ));
                }
                Row::Entry(index) => {
                    let item = &self.entries[*index];
                    let checked = self.is_checked(file, item);
                    let box_style = if checked {
                        Style::new().fg(Color::Green)
                    } else {
                        Style::new().dim()
                    };
                    items.push(ListItem::new(Line::from(vec![
                        Span::styled(if checked { "[x] " } else { "[ ] " }, box_style),
                        Span::styled(format!("{:<10}", item.id), Style::new().fg(Color::Cyan)),
                        Span::raw(item.name.clone()),
                    ])));
                }
            }
        }

        let mut state = ListState::default();
        if !rows.is_empty() {
            state.select(Some(self.selection.min(rows.len() - 1)));
        }
        let title = if self.view == View::Remove {
            format!("Remove games from {}", self.app_target_key())
        } else {
            match (self.focused, self.query.trim()) {
                (Some(_), _) => format!("Details for {}", self.query.trim()),
                (None, "") => "Results".to_string(),
                (None, term) => format!("Results for {term}"),
            }
        };
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, area, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigFile;

    fn picker_with(entries: Vec<Item>, groups: Vec<Group>) -> Picker {
        let mut picker = Picker::new();
        picker.open = true;
        picker.entries = entries;
        picker.groups = groups;
        picker
    }

    fn group(title: &str, _kind: Kind, start: usize, len: usize) -> Group {
        Group {
            title: title.to_string(),
            start,
            len,
        }
    }

    fn sample_file() -> ConfigFile {
        ConfigFile::parse_text(
            "/tmp/config.yaml",
            "AppIds:\n  - 400 #Portal\nAdditionalApps:\nAdditionalPackages:\nAdditionalDepots:\n",
        )
    }

    fn item(kind: Kind, id: u64, name: &str) -> Item {
        Item {
            kind,
            id,
            name: name.to_string(),
            app_type: String::new(),
            live: false,
            live_rank: 0,
        }
    }

    #[test]
    fn space_checks_and_unchecks_the_selected_game() {
        let mut file = sample_file();
        let mut picker = picker_with(
            vec![item(Kind::App, 620, "Portal 2")],
            vec![group("Games (1)", Kind::App, 0, 1)],
        );
        picker.selection = 1; // the entry, below the header
                              // No network in tests: pretend the game's details are already known.
        picker.details.insert(620, (Vec::new(), Vec::new()));

        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        assert!(file.text().contains("AdditionalApps:\n  - 620 #Portal 2\n"));
        assert!(picker.is_checked(&file, &item(Kind::App, 620, "Portal 2")));

        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        assert!(!file.text().contains("620"));
        assert!(picker.status.contains("Removed 620"));
    }

    #[test]
    fn space_on_a_header_toggles_the_whole_group() {
        let mut file = sample_file();
        let mut picker = picker_with(
            vec![
                item(Kind::App, 620, "Portal 2"),
                item(Kind::App, 400, "Portal"),
            ],
            vec![group("Games (2)", Kind::App, 0, 2)],
        );
        picker.selection = 0; // header

        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        assert!(file.text().contains("AdditionalApps:\n  - 620 #Portal 2"));
        assert!(file.text().contains("  - 400 #Portal"));
        assert!(picker.status.contains("Added"));

        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        assert!(!file.text().contains("Portal 2"));
        assert!(!file.text().contains("AdditionalApps:\n  - "));
        assert!(picker.status.contains("Removed"));
    }

    #[test]
    fn games_default_to_additional_apps_and_t_switches_to_appids() {
        let mut file = sample_file();
        let mut picker = picker_with(
            vec![item(Kind::App, 620, "Portal 2")],
            vec![group("Games (1)", Kind::App, 0, 1)],
        );
        picker.selection = 1;
        picker.details.insert(620, (Vec::new(), Vec::new()));
        assert_eq!(picker.app_target_key(), "AdditionalApps", "default target");
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char('t')));
        assert_eq!(picker.app_target_key(), "AppIds");
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        assert!(file.text().contains("  - 620 #Portal 2"));
        assert_eq!(picker.target_for(Kind::App), "AppIds");
    }

    #[test]
    fn packages_use_their_own_list() {
        let mut file = sample_file();
        let mut picker = picker_with(
            vec![item(Kind::Package, 7877, "Portal Bundle")],
            vec![group("Packages (1)", Kind::Package, 0, 1)],
        );
        picker.selection = 1;
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        assert!(file
            .text()
            .contains("AdditionalPackages:\n  - 7877 #Portal Bundle"));
        assert!(picker.status.contains("Added 7877"));
    }

    #[test]
    fn a_missing_target_list_is_reported() {
        let mut file = ConfigFile::parse_text("/tmp/config.yaml", "AppIds:\n");
        let mut picker = picker_with(
            vec![item(Kind::Package, 7877, "Portal Bundle")],
            vec![group("Packages (1)", Kind::Package, 0, 1)],
        );
        picker.selection = 1;
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        assert!(picker
            .status
            .contains("AdditionalPackages is not in this config"));
        assert_eq!(file.text(), "AppIds:\n");
    }

    #[test]
    fn checking_a_game_also_adds_its_dlcs_and_packages() {
        let mut file = sample_file();
        let mut picker = picker_with(
            vec![item(Kind::App, 620, "Portal 2")],
            vec![group("Games (1)", Kind::App, 0, 1)],
        );
        picker.selection = 1;
        // Pretend the details for Portal 2 are already known this session.
        picker.details.insert(
            620,
            (
                vec![item(Kind::Dlc, 323180, "Portal 2 Soundtrack")],
                vec![item(Kind::Package, 7877, "Portal 2")],
            ),
        );

        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        let text = file.text();
        assert!(text.contains("AdditionalApps:\n  - 620 #Portal 2"));
        assert!(text.contains("  - 323180 #Portal 2 Soundtrack"));
        assert!(text.contains("AdditionalPackages:\n  - 7877 #Portal 2"));
        assert!(
            picker.status.contains("and 2 related entries"),
            "{}",
            picker.status
        );

        // Unchecking takes the related entries along again.
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        let text = file.text();
        assert!(!text.contains("620"));
        assert!(!text.contains("323180"));
        assert!(!text.contains("7877"));
        assert!(
            picker.status.contains("related entries"),
            "{}",
            picker.status
        );
    }

    #[test]
    fn group_toggles_do_not_fetch_details() {
        let mut file = sample_file();
        let mut picker = picker_with(
            vec![
                item(Kind::App, 620, "Portal 2"),
                item(Kind::App, 400, "Portal"),
            ],
            vec![group("Games (2)", Kind::App, 0, 2)],
        );
        picker.selection = 0;
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        // Both games are checked in without any detail lookup (no network in tests: the call would have to fetch, so it must not happen here).
        assert!(file.text().contains("AdditionalApps:\n  - 620 #Portal 2"));
        assert!(file.text().contains("  - 400 #Portal"));
        assert!(
            picker.status.contains("open a game with enter"),
            "{}",
            picker.status
        );
    }

    #[test]
    fn focusing_a_game_replaces_the_results_and_esc_restores_them() {
        let mut file = sample_file();
        let results = vec![
            item(Kind::App, 400, "Portal"),
            item(Kind::App, 620, "Portal 2"),
        ];
        let mut picker = picker_with(results.clone(), vec![group("Games (2)", Kind::App, 0, 2)]);
        picker.results = results;
        // Pretend the details for Portal 2 came back.
        picker.focused = Some(1);
        picker.entries = vec![
            item(Kind::App, 620, "Portal 2"),
            item(Kind::Dlc, 323180, "Portal 2 Soundtrack"),
        ];
        picker.groups = vec![
            group("Selected game", Kind::App, 0, 1),
            group("DLCs of Portal 2 (1)", Kind::Dlc, 1, 1),
        ];
        picker.selection = 2;

        assert_eq!(picker.rows().len(), 4, "two headers and two entries");
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        assert!(file.text().contains("  - 323180 #Portal 2 Soundtrack"));

        // esc goes back to the search results, a second esc leaves the picker
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Esc));
        assert!(picker.focused.is_none());
        assert_eq!(picker.groups.len(), 1);
        assert_eq!(picker.entries.len(), 2);
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Esc));
        assert!(!picker.has_focus());
    }

    #[test]
    fn remove_view_lists_the_target_and_removes_games() {
        let mut file = ConfigFile::parse_text(
            "/tmp/config.yaml",
            "AdditionalApps:\n  - 570940   # DARK SOULS™: REMASTERED\n  - 730 #CS2\nAppIds:\n  - 400 #Portal\n",
        );
        let mut picker = Picker::new();
        picker.open = true;
        picker.index = Some(Vec::new());

        // r shows what is in the target (AdditionalApps by default)
        picker.toggle_remove(&file);
        assert!(picker.is_removing());
        let ids: Vec<u64> = picker.entries.iter().map(|item| item.id).collect();
        assert_eq!(ids, vec![570940, 730]);
        assert_eq!(picker.entries[0].name, "DARK SOULS™: REMASTERED");
        assert_eq!(picker.entries[1].name, "CS2");

        // typing filters the list instead of searching Steam
        picker.query = "cs".to_string();
        picker.refresh_remove(&file);
        assert_eq!(picker.entries.len(), 1);
        assert_eq!(picker.entries[0].id, 730);

        // space removes the highlighted game
        picker.selection = 1;
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(' ')));
        assert!(!file.text().contains("730"), "{}", file.text());
        assert!(
            file.text().contains("DARK SOULS™: REMASTERED"),
            "{}",
            file.text()
        );
        // with the filter cleared the remaining game is listed again
        picker.query.clear();
        picker.refresh_remove(&file);
        assert!(picker.status.contains("space removes"), "{}", picker.status);
        let ids: Vec<u64> = picker.entries.iter().map(|item| item.id).collect();
        assert_eq!(ids, vec![570940], "the removed game is gone from the list");

        // t switches the target the removal applies to
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char('t')));
        assert_eq!(picker.app_target_key(), "AppIds");
        let ids: Vec<u64> = picker.entries.iter().map(|item| item.id).collect();
        assert_eq!(ids, vec![400], "the remove list follows the target");

        // esc goes back to searching
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Esc));
        assert!(!picker.is_removing());
    }

    #[test]
    fn the_panel_is_never_open_without_focus() {
        let mut file = sample_file();
        let mut picker = Picker::new();
        assert!(!picker.has_focus() && !picker.has_focus());

        picker.toggle();
        assert!(picker.has_focus() && picker.has_focus());

        // esc first leaves the search box, then closes the panel outright
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Esc));
        assert!(
            !picker.is_editing() && picker.has_focus(),
            "editing stopped, still open"
        );
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Esc));
        assert!(!picker.has_focus(), "esc closes the panel");
        assert!(
            !picker.has_focus(),
            "and hands the focus back to the settings"
        );

        picker.toggle();
        picker.editing = false;
        picker.focused = Some(0);
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Esc));
        assert!(picker.has_focus(), "esc first leaves the focused game");
        picker.handle_key(&mut file, KeyEvent::from(KeyCode::Esc));
        assert!(!picker.has_focus(), "a second esc closes the panel");
    }

    #[test]
    fn typing_letters_never_saves_or_quits() {
        let mut file = sample_file();
        let before = file.text();
        let mut picker = Picker::new();
        picker.open = true;

        // "s" and "q" used to fall through to save/quit; now they type.
        for (character, expected) in [('s', "s"), ('o', "so")] {
            match picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(character))) {
                Outcome::Handled => {}
                Outcome::Pass(_) => panic!("'{character}' must not reach the app"),
            }
            assert_eq!(picker.query, expected);
            assert!(picker.editing);
        }
        assert_eq!(file.text(), before, "nothing was written");

        // Ctrl+S still saves (the app handles it).
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(matches!(
            picker.handle_key(&mut file, ctrl_s),
            Outcome::Pass(_)
        ));
    }

    #[test]
    fn typing_searches_after_a_pause() {
        let mut file = ConfigFile::parse_text(
            "/tmp/config.yaml",
            "AdditionalApps:\n  - 570940   # DARK SOULS™: REMASTERED\n",
        );
        let mut picker = Picker::new();
        picker.open = true;
        picker.ensure_index(&file);
        // Pin the index so the test does not depend on a local Steam install.
        picker.index = Some(vec![SteamApp {
            appid: 570940,
            name: "DARK SOULS™: REMASTERED".to_string(),
            app_type: "game".to_string(),
            dlc: Vec::new(),
            depots: Vec::new(),
        }]);

        for character in "dsr".chars() {
            picker.handle_key(&mut file, KeyEvent::from(KeyCode::Char(character)));
        }
        assert!(picker.results.is_empty(), "nothing searched yet");
        // too soon: the debounce has not elapsed
        picker.tick(&file);
        assert!(picker.results.is_empty());

        // once it has, the results follow without pressing enter
        picker.pending = Some(std::time::Instant::now() - SEARCH_DEBOUNCE);
        picker.tick(&file);
        assert_eq!(picker.results.first().map(|item| item.id), Some(570940));

        // a single character is not worth searching for
        picker.query = "d".to_string();
        picker.touch();
        picker.pending = Some(std::time::Instant::now() - SEARCH_DEBOUNCE);
        picker.tick(&file);
        assert_eq!(picker.results.first().map(|item| item.id), Some(570940));
        picker.query = "dx".to_string();
        picker.touch();
        picker.pending = Some(std::time::Instant::now() - SEARCH_DEBOUNCE);
        picker.tick(&file);
        assert!(picker.results.is_empty());
    }

    #[test]
    fn names_from_the_config_teach_the_index() {
        let file = ConfigFile::parse_text(
            "/tmp/config.yaml",
            "AdditionalApps:\n  - 570940   # DARK SOULS™: REMASTERED\nGameTitles:\n  10: \"Cunt-Striker\"  #Counter-Strike\n",
        );
        let names = config_names(&file);
        assert!(
            names.contains(&(570940, "DARK SOULS™: REMASTERED".to_string())),
            "{names:?}"
        );
        assert!(
            names.contains(&(10, "Cunt-Striker".to_string())),
            "{names:?}"
        );

        // and the abbreviation now resolves through the local index
        let mut picker = Picker::new();
        picker.ensure_index(&file);
        picker.query = "dsr".to_string();
        picker.run_search(&file);
        assert!(
            picker.results.iter().any(|item| item.id == 570940),
            "{:?}",
            picker.results.iter().map(|i| &i.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_uses_the_local_index_and_resolves_abbreviations() {
        let file = sample_file();
        let mut picker = Picker::new();
        picker.editing = false;
        picker.index = Some(vec![
            SteamApp {
                appid: 521890,
                name: "Hello Neighbor".to_string(),
                app_type: "Game".to_string(),
                dlc: Vec::new(),
                depots: Vec::new(),
            },
            SteamApp {
                appid: 570940,
                name: "DARK SOULS™: REMASTERED".to_string(),
                app_type: "Game".to_string(),
                dlc: Vec::new(),
                depots: Vec::new(),
            },
        ]);

        // partial words, no network involved
        picker.query = "Hello Neigh".to_string();
        picker.run_search(&file);
        assert_eq!(
            picker
                .results
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![521890]
        );

        // an abbreviation finds the game through its initials
        picker.query = "DSR".to_string();
        picker.run_search(&file);
        assert_eq!(picker.results.first().map(|item| item.id), Some(570940));
    }

    #[test]
    fn focusing_a_game_uses_the_local_cache_for_dlcs_and_depots() {
        let mut picker = picker_with(
            vec![item(Kind::App, 620, "Portal 2")],
            vec![group("Games (1)", Kind::App, 0, 1)],
        );
        picker.results = vec![item(Kind::App, 620, "Portal 2")];
        picker.index = Some(vec![SteamApp {
            appid: 620,
            name: "Portal 2".to_string(),
            app_type: "Game".to_string(),
            dlc: vec![323180],
            depots: vec![731, 732],
        }]);
        // Pretend Steam answered with no DLCs and no packages.
        picker.details.insert(620, (Vec::new(), Vec::new()));

        picker.focus_app(0);
        let titles: Vec<&str> = picker
            .groups
            .iter()
            .map(|group| group.title.as_str())
            .collect();
        assert!(
            titles
                .iter()
                .any(|title| title.starts_with("Depots of Portal 2")),
            "{titles:?}"
        );
        assert!(picker
            .entries
            .iter()
            .any(|entry| entry.kind == Kind::Depot && entry.id == 731));
        assert!(picker.status.contains("depots"), "{}", picker.status);
    }

    #[test]
    fn searching_without_a_query_only_sets_a_hint() {
        let mut picker = Picker::new();
        picker.editing = true;
        picker.handle_key(
            &mut ConfigFile::parse_text("/tmp/config.yaml", ""),
            KeyEvent::from(KeyCode::Enter),
        );
        assert_eq!(picker.status, "Type a game name first");
        assert!(picker.entries.is_empty());
    }

    #[test]
    fn rendering_an_empty_and_a_filled_picker_does_not_panic() {
        let file = sample_file();
        let mut picker = Picker::new();
        let backend = ratatui::backend::TestBackend::new(52, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| picker.render(&file, frame, frame.area()))
            .unwrap();

        let mut picker = picker_with(
            vec![item(Kind::App, 620, "Portal 2")],
            vec![group("Games (1)", Kind::App, 0, 1)],
        );
        terminal
            .draw(|frame| picker.render(&file, frame, frame.area()))
            .unwrap();
    }
}
