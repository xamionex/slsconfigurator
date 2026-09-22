//! Steam lookups for the picker: store search, app details and the depots of locally installed games.
//!
//! Everything goes through the public store endpoints with `curl`, so there is no API key to configure: search uses the store's own search backend (the one the website uses, thousands of matches per term) and details come from `appdetails`, which reports an app's DLC appIds and package ids.
//!
//! Lookups are cached under `$XDG_CONFIG_HOME/SLSconfigurator/cache/` (search results for a day, details for a week, names for a month), so repeated searches are instant and keep working offline.
//! Delete that directory to refresh everything.
//!
//! Depot ids are not available here on purpose: no public endpoint exposes them, Steam's `appinfo.vdf` cache keeps its keys in a separate string table behind offsets since its v29 format, and the installed-game manifests do not carry them either.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const USER_AGENT: &str = "SLSconfigurator/0.1";

/// What an entry from a lookup is, which decides where it can be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    /// A game or application.
    App,
    /// A DLC, which is an appId of its own.
    Dlc,
    /// A store package (also what bundles are).
    Package,
    /// A depot id, read from the local Steam client cache.
    Depot,
}

/// One lookup result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub kind: Kind,
    pub id: u64,
    pub name: String,
    /// `common.type` when the source knows it, else empty.
    pub app_type: String,
    /// True for entries Steam itself ranked; they win ties within a tier.
    pub live: bool,
    /// Position in Steam's own result list (0 for local entries).
    pub live_rank: u32,
}

impl Item {
    fn new(kind: Kind, id: u64, name: &str) -> Item {
        Item {
            kind,
            id,
            name: name.trim().to_string(),
            app_type: String::new(),
            live: false,
            live_rank: 0,
        }
    }

    /// An entry that came from a live Steam search.
    fn live(kind: Kind, id: u64, name: &str) -> Item {
        Item {
            live: true,
            ..Item::new(kind, id, name)
        }
    }

    /// Same, for a game with a known type.
    pub fn typed(kind: Kind, id: u64, name: &str, app_type: &str) -> Item {
        let mut item = Item::new(kind, id, name);
        item.app_type = app_type.to_string();
        item
    }
}

/// Percent encode a query value.
fn encode(term: &str) -> String {
    let mut out = String::new();
    for byte in term.trim().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Where lookups are cached: `$XDG_CONFIG_HOME/SLSconfigurator/cache`.
pub fn cache_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::config_dir()?,
    };
    Some(base.join("SLSconfigurator").join("cache"))
}

/// How long a cached file stays usable.
const SEARCH_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const DETAILS_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const NAME_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Read a cache file, ignoring it when it is older than `ttl`.
fn cache_read_in(dir: &Path, name: &str, ttl: Duration) -> Option<String> {
    let path = dir.join(name);
    let metadata = std::fs::metadata(&path).ok()?;
    let age = metadata.modified().ok()?.elapsed().unwrap_or_default();
    if age > ttl {
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .filter(|body| !body.is_empty())
}

/// Write a cache file, best effort.
fn cache_write_in(dir: &Path, name: &str, body: &str) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let _ = std::fs::write(dir.join(name), body);
}

fn cache_read(name: &str, ttl: Duration) -> Option<String> {
    cache_read_in(&cache_dir()?, name, ttl)
}

fn cache_write(name: &str, body: &str) {
    if let Some(dir) = cache_dir() {
        cache_write_in(&dir, name, body);
    }
}

/// A stable, filesystem friendly key for a search term.
fn cache_key(term: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in term.trim().to_lowercase().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let slug: String = term
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(32)
        .collect();
    format!("search-{slug}-{hash:016x}.json")
}

/// Fetch a URL with curl, returning its body.
///
/// Tests never reach the network: parsing and the cache are covered with fixtures, and a request here would make the suite slow and flaky.
fn fetch(url: &str) -> Result<String, String> {
    #[cfg(test)]
    if std::env::var_os("SLSC_ALLOW_NETWORK").is_none() {
        return Err(format!("network disabled during tests: {url}"));
    }

    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", "25", "-A", USER_AGENT, url])
        .output()
        .map_err(|e| format!("curl could not run: {e}"))?;
    if !output.status.success() {
        return Err(format!("Steam request failed ({}): {url}", output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

/// Normalise a name or query for matching: lower case, trademark symbols and punctuation removed so `DARK SOULS™: REMASTERED` becomes `dark souls remastered`.
pub fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Match tiers, best first.
/// Results only fall through to the loose tiers when nothing matched strictly, so a query never shows unrelated fuzzy hits next to real ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Exact,
    Prefix,
    Words,
    Substring,
    /// Initials or a subsequence, anchored at the start of the name.
    Loose,
}

/// How well `name` matches `query`, with the tier it matched in.
///
/// * `Exact`/`Prefix`/`Substring` are literal matches.
/// * `Words`: every query word is a prefix of a name word, in order, so `hello neigh` finds `Hello Neighbor`.
/// * `Loose`: word initials (`dsr` -> `DARK SOULS: REMASTERED`) or a subsequence (`hl2` -> `Half-Life 2`), both anchored at the first word so `hello` cannot match "Timelie - Hell Loop".
pub fn match_score(name: &str, query: &str, app_type: &str) -> Option<(Tier, i32)> {
    // Roman and arabic numerals are the same word: `Dark Souls 3` has to find `DARK SOULS™ III`.
    let best = score_once(name, query, app_type);
    let variant = numeral_variant(query);
    match (best, variant) {
        (Some(best), Some(variant)) => {
            let other = score_once(name, &variant, app_type);
            Some(match other {
                Some(other) if other < best => other,
                _ => best,
            })
        }
        (Some(best), None) => Some(best),
        (None, Some(variant)) => score_once(name, &variant, app_type),
        (None, None) => None,
    }
}

/// Swap numerals for their other spelling (`dark souls 3` -> `dark souls iii`).
fn numeral_variant(query: &str) -> Option<String> {
    let mut changed = false;
    let words: Vec<String> = normalize(query)
        .split(' ')
        .map(|word| match numeral_value(word) {
            Some(value) => match roman(value) {
                Some(roman) if roman != word => {
                    changed = true;
                    roman.to_string()
                }
                _ => word.to_string(),
            },
            None => match roman_from_digits(word) {
                Some((value, roman)) => {
                    changed = true;
                    let _ = value;
                    roman.to_string()
                }
                None => word.to_string(),
            },
        })
        .collect();
    changed.then(|| words.join(" "))
}

/// The numeric value of an arabic or roman numeral word.
fn numeral_value(word: &str) -> Option<u32> {
    if let Ok(value) = word.parse::<u32>() {
        return (1..=30).contains(&value).then_some(value);
    }
    let roman = word.trim().to_ascii_uppercase();
    let values = [
        ("I", 1),
        ("II", 2),
        ("III", 3),
        ("IV", 4),
        ("V", 5),
        ("VI", 6),
        ("VII", 7),
        ("VIII", 8),
        ("IX", 9),
        ("X", 10),
        ("XI", 11),
        ("XII", 12),
    ];
    values
        .iter()
        .find(|(name, _)| *name == roman)
        .map(|(_, value)| *value)
}

/// Roman spelling of a numeral word, if it is one.
fn roman(value: u32) -> Option<&'static str> {
    let values = [
        (1, "i"),
        (2, "ii"),
        (3, "iii"),
        (4, "iv"),
        (5, "v"),
        (6, "vi"),
        (7, "vii"),
        (8, "viii"),
        (9, "ix"),
        (10, "x"),
        (11, "xi"),
        (12, "xii"),
    ];
    values
        .iter()
        .find(|(number, _)| *number == value)
        .map(|(_, name)| *name)
}

/// Roman spelling for a word made of digits, e.g. `3` -> `iii`.
fn roman_from_digits(word: &str) -> Option<(u32, &'static str)> {
    let value = word.parse::<u32>().ok()?;
    if !(1..=30).contains(&value) {
        return None;
    }
    roman(value).map(|name| (value, name))
}

/// Single attempt at scoring, without numeral handling.
fn score_once(name: &str, query: &str, app_type: &str) -> Option<(Tier, i32)> {
    let name = normalize(name);
    let query = normalize(query);
    if name.is_empty() || query.is_empty() {
        return None;
    }
    let name_words: Vec<&str> = name.split(' ').collect();
    let query_words: Vec<&str> = query.split(' ').collect();
    let penalty = type_penalty(app_type);
    let short = name.len() as i32;

    if name == query {
        return Some((Tier::Exact, 10_000 - short - penalty));
    }
    if name.starts_with(&query) {
        return Some((Tier::Prefix, 9_000 - short - penalty));
    }
    if let Some(position) = word_prefix_position(&name_words, &query_words) {
        // A match that starts at the first word is better than one further in.
        return Some((Tier::Words, 7_000 - position as i32 * 25 - short - penalty));
    }
    if name.contains(&query) {
        return Some((Tier::Substring, 6_000 - short - penalty));
    }

    // Loose matches are only interesting for games and DLCs; tool and music entries would otherwise flood them.
    if !is_playable(app_type) {
        return None;
    }
    if query.len() >= 3 {
        if let Some(gaps) = initials_gaps(&name_words, &query) {
            return Some((Tier::Loose, 4_000 - gaps * 150 - short - penalty));
        }
    }
    if query.len() >= 2 && subsequence_match(&name, &query) {
        return Some((Tier::Loose, 3_000 - short - penalty));
    }
    None
}

/// Games and DLCs, which are what a config wants, rank above everything else.
fn is_playable(app_type: &str) -> bool {
    matches!(
        app_type.trim().to_ascii_lowercase().as_str(),
        "game" | "dlc"
    )
}

/// Penalty added to the score for app types that are not games or DLCs.
fn type_penalty(app_type: &str) -> i32 {
    match app_type.trim().to_ascii_lowercase().as_str() {
        "game" => 0,
        "dlc" => 200,
        "" => 400,
        _ => 1_500,
    }
}

/// Offset of the first name word when every query word is a prefix of a name word, in order.
fn word_prefix_position(name_words: &[&str], query_words: &[&str]) -> Option<usize> {
    let mut name_index = 0;
    let mut first: Option<usize> = None;
    for query_word in query_words {
        loop {
            let name_word = name_words.get(name_index)?;
            if name_word.starts_with(query_word) {
                first.get_or_insert(name_index);
                name_index += 1;
                break;
            }
            name_index += 1;
        }
    }
    // Do not let a single very short word match half the list: the first query word has to match within the first few words of the name.
    if first? > 3 {
        return None;
    }
    first
}

/// Number of words skipped when every character of `query` matches the initial of a following name word (`dsr` -> `dark souls remastered`), starting at the first word.
/// `None` when it does not match.
fn initials_gaps(name_words: &[&str], query: &str) -> Option<i32> {
    let query: Vec<char> = query.chars().filter(|c| c.is_alphanumeric()).collect();
    let first = *query.first()?;
    if !word_matches_initial(name_words.first()?, first) {
        return None;
    }
    let mut word_index = 1;
    let mut gaps = 0;
    for character in query.iter().skip(1) {
        loop {
            let word = name_words.get(word_index)?;
            word_index += 1;
            if word_matches_initial(word, *character) {
                break;
            }
            gaps += 1;
        }
    }
    Some(gaps)
}

/// True when a name word can stand for a query character: its initial, or the numeral it spells (`iii` for `3`).
fn word_matches_initial(word: &str, character: char) -> bool {
    if word.starts_with(character) {
        return true;
    }
    match (character.to_digit(10), numeral_value(word)) {
        (Some(digit), Some(value)) => digit == value,
        _ => false,
    }
}

/// True when the query characters appear in `name` in order, starting with the first character of the name.
fn subsequence_match(name: &str, query: &str) -> bool {
    let mut characters = name.chars();
    let mut query = query.chars();
    let Some(first) = query.next() else {
        return false;
    };
    if characters.next() != Some(first) {
        return false;
    }
    query.all(|wanted| characters.any(|candidate| candidate == wanted))
}

/// At most this many loose (fuzzy) matches are shown, when nothing matched strictly.
const LOOSE_LIMIT: usize = 25;

/// Sort and filter candidates: strict matches always win, loose ones are only used when there is nothing better, and then only the best few.
pub fn rank_items(candidates: &[Item], term: &str, limit: usize) -> Vec<Item> {
    let mut strict: Vec<(Tier, i32, Item)> = Vec::new();
    let mut loose: Vec<(Tier, i32, Item)> = Vec::new();
    for item in candidates {
        if let Some((tier, mut score)) = match_score(&item.name, term, &item.app_type) {
            let _ = &mut score;
            let entry = (tier, score, item.clone());
            if tier == Tier::Loose {
                loose.push(entry);
            } else {
                strict.push(entry);
            }
        }
    }
    // When something matched strictly, exact initials matches still come along (after them): `dsr` belongs with `Dark Souls Remastered` even when other games matched the words.
    // Fuzzy subsequences stay out.
    let (mut clean, mut fuzzy): (Vec<_>, Vec<_>) =
        loose.into_iter().partition(|(_, score, _)| *score > 3_500);
    let mut scored = if strict.is_empty() {
        clean.append(&mut fuzzy);
        clean.truncate(LOOSE_LIMIT);
        clean
    } else {
        clean.extend(strict);
        clean
    };
    // Inside a tier, Steam's own order leads (its results are relevance ranked), then local matches by score, then shorter names.
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.2.live.cmp(&a.2.live))
            .then_with(|| a.2.live_rank.cmp(&b.2.live_rank))
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.name.len().cmp(&b.2.name.len()))
            .then_with(|| a.2.name.cmp(&b.2.name))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, item)| item)
        .collect()
}

/// Rank the local Steam app index for `term`, best first.
pub fn search_index(apps: &[crate::appinfo::SteamApp], term: &str, limit: usize) -> Vec<Item> {
    let candidates: Vec<Item> = apps
        .iter()
        .map(|app| Item::typed(Kind::App, app.appid, &app.name, &app.app_type))
        .collect();
    rank_items(&candidates, term, limit)
}

/// Search the Steam store, newest ranking, up to `limit` entries.
pub fn search(term: &str, limit: u32) -> Result<Vec<Item>, String> {
    if term.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut items = suggest_search(term).unwrap_or_default();
    match store_search(term, limit) {
        Ok(more) => {
            for item in more {
                if !items.iter().any(|known| known.id == item.id) {
                    items.push(item);
                }
            }
        }
        Err(e) if items.is_empty() => return Err(e),
        Err(_) => {}
    }
    // Rank whatever the two backends gave us the same way as the local index.
    items.sort_by(|a, b| {
        let score =
            |item: &Item| match_score(&item.name, term, &item.app_type).unwrap_or((Tier::Loose, 0));
        let (tier_a, score_a) = score(a);
        let (tier_b, score_b) = score(b);
        tier_a
            .cmp(&tier_b)
            .then_with(|| score_b.cmp(&score_a))
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.name.cmp(&b.name))
    });
    items.truncate(limit as usize);
    Ok(items)
}

/// Steam's store autocomplete, which matches partial words ("Hello Neigh") and some abbreviations ("hl2"), unlike the store's own result page.
fn suggest_search(term: &str) -> Result<Vec<Item>, String> {
    let url = format!(
        "https://store.steampowered.com/search/suggest?term={}&f=games&cc=US&l=en",
        encode(term)
    );
    let body = fetch(&url)?;
    Ok(parse_suggest(&body))
}

/// Pull the appIds and names out of the autocomplete HTML.
fn parse_suggest(html: &str) -> Vec<Item> {
    let mut out = Vec::new();
    for block in html.split("<a ").skip(1) {
        let Some(id) = attribute(block, "data-ds-appid=\"", "\"") else {
            continue;
        };
        let Some(name) = attribute(block, "class=\"match_name\">", "<") else {
            continue;
        };
        if let Ok(id) = id.parse::<u64>() {
            if !out.iter().any(|item: &Item| item.id == id) {
                out.push(Item::live(Kind::App, id, &unescape(&name)));
            }
        }
    }
    out
}

/// The store's own search backend, which also knows DLCs and bundles.
fn store_search(term: &str, limit: u32) -> Result<Vec<Item>, String> {
    // Cached results make repeated searches instant and work offline.
    let key = cache_key(term);
    if let Some(body) = cache_read(&key, SEARCH_TTL) {
        if let Ok(items) = serde_json::from_str::<Vec<Item>>(&body) {
            return Ok(items);
        }
    }

    let url = format!(
        "https://store.steampowered.com/search/results/?query&start=0&count={limit}&infinite=1&term={}&cc=US&l=en",
        encode(term)
    );
    let body = fetch(&url)?;

    #[derive(Deserialize)]
    struct Response {
        results_html: String,
    }
    let response: Response = serde_json::from_str(&body)
        .map_err(|e| format!("could not read the search results: {e}"))?;
    let items = parse_results(&response.results_html);
    if let Ok(json) = serde_json::to_string(&items) {
        cache_write(&key, &json);
    }
    Ok(items)
}

/// Parse the store search HTML fragment into items, in page order.
fn parse_results(html: &str) -> Vec<Item> {
    let mut out = Vec::new();
    for row in html.split("<a ").skip(1) {
        let title = attribute(row, "class=\"title\">", "<");
        let Some(title) = title else {
            continue;
        };
        if let Some(id) = attribute(row, "data-ds-appid=\"", "\"") {
            if let Ok(id) = id.parse::<u64>() {
                out.push(Item::live(Kind::App, id, &unescape(&title)));
            }
        } else if let Some(id) = attribute(row, "data-ds-bundleid=\"", "\"") {
            if let Ok(id) = id.parse::<u64>() {
                out.push(Item::live(Kind::Package, id, &unescape(&title)));
            }
        }
    }
    out
}

/// Text between `start` and `end`, trimmed, if both are present.
fn attribute(text: &str, start: &str, end: &str) -> Option<String> {
    let rest = text.split_once(start)?.1;
    let value = rest.split_once(end)?.0;
    Some(value.trim().to_string())
}

/// Undo the HTML escapes that show up in store names.
fn unescape(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// Details of an app: its DLC appIds and its package ids.
pub fn details(appid: u64) -> Result<(Vec<u64>, Vec<u64>), String> {
    #[derive(Serialize, Deserialize)]
    struct Cached {
        dlc: Vec<u64>,
        packages: Vec<u64>,
    }

    let key = format!("details-{appid}.json");
    if let Some(body) = cache_read(&key, DETAILS_TTL) {
        if let Ok(cached) = serde_json::from_str::<Cached>(&body) {
            return Ok((cached.dlc, cached.packages));
        }
    }

    let url = format!(
        "https://store.steampowered.com/api/appdetails?appids={appid}&filters=basic,dlc,packages&l=en&cc=US"
    );
    let body = fetch(&url)?;
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("could not read app details: {e}"))?;
    let data = value
        .get(appid.to_string())
        .and_then(|entry| entry.get("data"))
        .ok_or_else(|| format!("Steam has no details for appId {appid}"))?;

    let ids = |field: &str| -> Vec<u64> {
        data.get(field)
            .and_then(|list| list.as_array())
            .map(|list| list.iter().filter_map(|id| id.as_u64()).collect())
            .unwrap_or_default()
    };
    let (dlc, packages) = (ids("dlc"), ids("packages"));
    if let Ok(json) = serde_json::to_string(&Cached {
        dlc: dlc.clone(),
        packages: packages.clone(),
    }) {
        cache_write(&key, &json);
    }
    Ok((dlc, packages))
}

/// Names for up to `max` ids, looked up one by one in a single curl call.
///
/// Packages use `packagedetails`, everything else uses `appdetails`.
pub fn names(kind: Kind, ids: &[u64], max: usize) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for id in ids.iter().take(max) {
        let key = format!(
            "{}-{id}.json",
            match kind {
                Kind::Package => "pkgname",
                _ => "name",
            }
        );
        if let Some(body) = cache_read(&key, NAME_TTL) {
            if let Ok(name) = serde_json::from_str::<String>(&body) {
                out.push((*id, name));
                continue;
            }
        }

        let base = match kind {
            Kind::Package => format!("https://store.steampowered.com/api/packagedetails?packageids={id}&l=en&cc=US"),
            _ => format!("https://store.steampowered.com/api/appdetails?appids={id}&filters=basic&l=en&cc=US"),
        };
        let Ok(body) = fetch(&base) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        if let Some(name) = value
            .get(id.to_string())
            .and_then(|entry| entry.get("data"))
            .and_then(|data| data.get("name"))
            .and_then(|name| name.as_str())
        {
            if let Ok(json) = serde_json::to_string(name) {
                cache_write(&key, &json);
            }
            out.push((*id, name.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_search_html_is_parsed() {
        let html = r#"
<a class="search_result_row" data-ds-appid="400" href="...">
    <span class="title">Portal</span></a>
<a class="search_result_row" data-ds-appid="620" href="...">
    <span class="title">Portal 2</span></a>
<a class="search_result_row" data-ds-bundleid="7877" href="...">
    <span class="title">Portal Bundle</span></a>
"#;
        assert_eq!(
            parse_results(html),
            vec![
                Item::live(Kind::App, 400, "Portal"),
                Item::live(Kind::App, 620, "Portal 2"),
                Item::live(Kind::Package, 7877, "Portal Bundle"),
            ]
        );
    }

    #[test]
    fn names_with_html_escapes_are_cleaned() {
        let html = r#"<a data-ds-appid="1"><span class="title">Tom &amp; Jerry&#39;s</span></a>"#;
        assert_eq!(parse_results(html)[0].name, "Tom & Jerry's");
    }

    #[test]
    fn rows_without_a_title_or_id_are_skipped() {
        let html = r#"<a data-ds-appid="1"><span class="other">x</span></a>
<a class="x"><span class="title">No id</span></a>"#;
        assert!(parse_results(html).is_empty());
    }

    #[test]
    fn cache_files_are_written_and_respected_until_they_expire() {
        let dir = std::env::temp_dir().join(format!("slsc_steam_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        cache_write_in(&dir, "details-620.json", "{\"dlc\":[],\"packages\":[]}");
        assert_eq!(
            cache_read_in(&dir, "details-620.json", Duration::from_secs(60)).as_deref(),
            Some("{\"dlc\":[],\"packages\":[]}")
        );
        // an expired entry is ignored
        assert!(cache_read_in(&dir, "details-620.json", Duration::from_secs(0)).is_none());
        // and missing ones too
        assert!(cache_read_in(&dir, "details-1.json", Duration::from_secs(60)).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggest_html_is_parsed() {
        let html = r#"<a class="match "  data-ds-appid="220" href="..."><div class="match_name">Half-Life 2</div><div class="match_price">$9.99</div></a><a class="match "  data-ds-appid="320" href="..."><div class="match_name">Half-Life 2: Deathmatch</div></a>"#;
        assert_eq!(
            parse_suggest(html),
            vec![
                Item::live(Kind::App, 220, "Half-Life 2"),
                Item::live(Kind::App, 320, "Half-Life 2: Deathmatch"),
            ]
        );
    }

    fn score(name: &str, query: &str) -> Option<(Tier, i32)> {
        match_score(name, query, "game")
    }

    #[test]
    fn strict_tiers_match_partial_words() {
        // the case the store search gets wrong: a prefix of the whole name
        assert_eq!(
            score("Hello Neighbor", "Hello Neigh").map(|(t, _)| t),
            Some(Tier::Prefix)
        );
        assert_eq!(
            score("Hello Neighbor 2", "hello neigh").map(|(t, _)| t),
            Some(Tier::Prefix)
        );
        // a prefix of a later word
        assert_eq!(
            score("Hello Neighbor", "Neighbor").map(|(t, _)| t),
            Some(Tier::Words)
        );
        assert_eq!(
            score("Half-Life 2", "half life").map(|(t, _)| t),
            Some(Tier::Prefix)
        );
        assert_eq!(
            score("Portal 2", "portal 2").map(|(t, _)| t),
            Some(Tier::Exact)
        );
        assert_eq!(
            score("Portal 2 Soundtrack", "portal 2").map(|(t, _)| t),
            Some(Tier::Prefix)
        );
    }

    #[test]
    fn unrelated_names_are_not_strict_matches() {
        // the reported case: "Hello" must not drag in "Timelie - Hell Loop"
        assert!(score("Timelie - Hell Loop", "Hello").is_none());
        assert!(score("Timelie - Hell Loop", "hello").is_none());
        // and the loose tiers stay anchored to the start of the name
        assert!(score("Portal 2", "dsr").is_none());
        assert!(score("Portal 2", "zzzz").is_none());
        assert!(score("RPG Maker VX Ace - DS Resource Pack", "dsr").is_none());
    }

    #[test]
    fn numerals_are_the_same_word() {
        // the reported case
        assert_eq!(
            score("DARK SOULS™: REMASTERED", "dark souls remastered").map(|(t, _)| t),
            Some(Tier::Exact)
        );
        assert_eq!(
            score("DARK SOULS™ III", "dark souls 3").map(|(t, _)| t),
            Some(Tier::Exact)
        );
        assert_eq!(
            score("DARK SOULS™ III", "dark souls iii").map(|(t, _)| t),
            Some(Tier::Exact)
        );
        assert_eq!(
            score("DARK SOULS™ II", "dark souls 2").map(|(t, _)| t),
            Some(Tier::Exact)
        );
        // and in abbreviations
        assert_eq!(
            score("DARK SOULS™ III", "ds3").map(|(t, _)| t),
            Some(Tier::Loose)
        );
        assert!(score("DARK SOULS™ III", "ds2").is_none());
    }

    #[test]
    fn exact_initials_survive_next_to_strict_matches() {
        let candidates = vec![
            Item::typed(Kind::App, 1, "DSR - Dark Story Remake", "game"),
            Item::typed(Kind::App, 570940, "DARK SOULS™: REMASTERED", "game"),
        ];
        let hits = rank_items(&candidates, "dsr", 10);
        let ids: Vec<u64> = hits.iter().map(|item| item.id).collect();
        assert_eq!(
            ids,
            vec![1, 570940],
            "strict first, exact initials right after"
        );
    }

    #[test]
    fn loose_tiers_resolve_abbreviations() {
        assert_eq!(
            score("DARK SOULS™: REMASTERED", "DSR").map(|(t, _)| t),
            Some(Tier::Loose)
        );
        assert_eq!(
            score("Half-Life 2", "hl2").map(|(t, _)| t),
            Some(Tier::Loose)
        );
        assert_eq!(score("Portal 2", "p2").map(|(t, _)| t), Some(Tier::Loose));
        // a tool never matches loosely
        assert!(match_score("Steamworks Common Redistributables", "scr", "Tool").is_none());
    }

    #[test]
    fn scoring_ranks_exact_and_prefix_first() {
        let exact = score("Portal 2", "portal 2").unwrap();
        let prefix = score("Portal 2 Soundtrack", "portal 2").unwrap();
        assert!(exact.0 < prefix.0, "exact sorts before prefix");
        assert!(exact.1 > prefix.1, "and scores higher");

        // games beat tools with the same kind of match
        let game = score("DS Resource Pack", "ds resource").unwrap();
        let tool = match_score("DS Resource Pack", "ds resource", "Tool").unwrap();
        assert_eq!(game.0, tool.0);
        assert!(game.1 > tool.1, "{game:?} > {tool:?}");
    }

    #[test]
    fn steam_order_is_kept_inside_a_tier() {
        let mut first = Item::live(Kind::App, 1, "Hello Zzz Long Name");
        first.live_rank = 1;
        let mut second = Item::live(Kind::App, 2, "Hello A");
        second.live_rank = 2;
        let candidates = vec![second, first];
        let hits = rank_items(&candidates, "hello", 10);
        assert_eq!(
            hits[0].id, 1,
            "Steam's first hit leads despite the longer name"
        );
    }

    #[test]
    fn debug_live_vs_local_order() {
        let candidates = vec![
            Item::typed(Kind::App, 2261250, "HELLO EXO", "dlc"),
            Item::live(Kind::App, 521890, "Hello Neighbor"),
            Item::live(Kind::App, 2495100, "Hello Kitty Island Adventure"),
            Item::typed(Kind::App, 2180970, "Hello, Vic", "game"),
        ];
        let hits = rank_items(&candidates, "hello", 10);
        eprintln!(
            "{:?}",
            hits.iter().map(|i| i.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn loose_matches_are_capped_and_ranked_by_gaps() {
        let candidates = vec![
            Item::typed(Kind::App, 1, "Dishonored Shadow Rat Pack", "game"),
            Item::typed(Kind::App, 2, "DARK SOULS™: REMASTERED", "game"),
            Item::typed(
                Kind::App,
                3,
                "Dead by Daylight - Sadako Rising Chapter",
                "dlc",
            ),
        ];
        let hits = rank_items(&candidates, "dsr", 10);
        // no gaps between the initials wins
        assert_eq!(hits[0].id, 2);
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn rank_items_never_mixes_loose_hits_with_strict_ones() {
        // The reported noise: a loose hit next to a real match.
        let candidates = vec![
            Item::typed(Kind::App, 10, "Hello Neighbor", "game"),
            Item::typed(Kind::App, 11, "Heliborne - Navy Seals Camouflage", "game"),
            Item::typed(Kind::App, 12, "Timelie - Hell Loop", "game"),
        ];
        let hits = rank_items(&candidates, "hello", 10);
        let names: Vec<&str> = hits.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, vec!["Hello Neighbor"], "only real matches");
    }

    #[test]
    fn index_search_keeps_unrelated_games_out() {
        let apps = vec![
            app(521890, "Hello Neighbor", "game"),
            app(960420, "Hello Neighbor: Hide and Seek", "game"),
            app(1234, "Timelie - Hell Loop", "game"),
            app(629, "Portal 2 Authoring Tools", "Tool"),
        ];
        let hits = search_index(&apps, "hello", 10);
        let names: Vec<&str> = hits.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names[0], "Hello Neighbor");
        assert!(!names.contains(&"Timelie - Hell Loop"), "{names:?}");
        assert!(
            !names.contains(&"Timelie - Hell Loop"),
            "loose hits are dropped when strict ones exist"
        );
    }

    #[test]
    fn index_search_falls_back_to_loose_matches_only_alone() {
        let apps = vec![
            app(570940, "DARK SOULS™: REMASTERED", "game"),
            app(400, "Portal", "game"),
        ];
        let hits = search_index(&apps, "dsr", 10);
        assert_eq!(hits.first().map(|item| item.id), Some(570940));
        assert_eq!(hits.len(), 1, "Portal must not tag along");

        // with a strict match around, loose ones disappear
        let hits = search_index(&apps, "portal", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 400);
    }

    fn app(appid: u64, name: &str, app_type: &str) -> crate::appinfo::SteamApp {
        crate::appinfo::SteamApp {
            appid,
            name: name.to_string(),
            app_type: app_type.to_string(),
            dlc: Vec::new(),
            depots: Vec::new(),
        }
    }

    #[test]
    fn cache_keys_are_stable_and_safe() {
        let key = cache_key("Portal 2");
        assert_eq!(
            key,
            cache_key("  portal 2  "),
            "case and spaces do not matter"
        );
        assert!(key.starts_with("search-portal-2-"), "{key}");
        assert!(!key.contains('/'));
        assert_ne!(key, cache_key("Portal 3"));
    }

    #[test]
    fn query_encoding_covers_spaces_and_symbols() {
        assert_eq!(encode("portal 2"), "portal+2");
        assert_eq!(encode("Tom & Jerry"), "Tom+%26+Jerry");
        assert_eq!(encode("half-life"), "half-life");
    }
}
