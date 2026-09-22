//! Reader for Steam's local `appinfo.vdf` cache.
//!
//! The client keeps every app it knows about (names, DLC lists and depot ids) in `appcache/appinfo.vdf`. That makes a fresh, offline search index possible without any Steam Web API key.
//!
//! The format, as of version 29:
//!
//! ```text
//! u32  magic (0x07564429)
//! u32  universe
//! u64  offset of the string table
//! entries until appid 0:
//!     u32 appid
//!     u32 size of the data that follows
//!     u32 info state, u32 last updated, u64 pics token, 20 bytes sha1,
//!     u32 change number, 20 bytes of padding, then a key/value tree
//! ```
//!
//! Key/value trees use a type byte and an index into the string table, with string values stored inline: `0x00` object (until `0x08`), `0x01` string, `0x02` int32, `0x03` float, `0x04`/`0x06` four bytes, `0x05` utf16 string, `0x07` uint64, `0x08` end of object.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAGIC: u32 = 0x0756_4429;

/// What we care about from an app entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamApp {
    pub appid: u64,
    pub name: String,
    /// `common.type`: `Game`, `Tool`, `Music`, `Video`, `Demo`, ...
    pub app_type: String,
    /// DLC appIds from `extended.listofdlc`.
    pub dlc: Vec<u64>,
    /// Depot ids from the `depots` object (numeric keys only).
    pub depots: Vec<u64>,
}

/// A decoded key/value tree.
#[derive(Debug, Clone, PartialEq)]
enum Kv {
    Object(BTreeMap<String, Kv>),
    Text(String),
    Int(i32),
    Float(f32),
    Bytes(u32),
    ULong(u64),
}

impl Kv {
    fn get(&self, key: &str) -> Option<&Kv> {
        match self {
            Kv::Object(map) => map.get(key),
            _ => None,
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            Kv::Text(text) => Some(text),
            _ => None,
        }
    }

    fn object(&self) -> Option<&BTreeMap<String, Kv>> {
        match self {
            Kv::Object(map) => Some(map),
            _ => None,
        }
    }
}

/// The appinfo cache, in the usual Steam locations.
pub fn default_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    for base in [".steam/steam", ".steam/root", ".local/share/Steam"] {
        let path = home.join(base).join("appcache/appinfo.vdf");
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Read and parse the app cache, returning an empty list when it is missing or in a format this build does not understand.
pub fn load() -> Vec<SteamApp> {
    default_path()
        .map(|path| load_from(&path))
        .unwrap_or_default()
}

/// Parse the app cache at `path`.
pub fn load_from(path: &Path) -> Vec<SteamApp> {
    match std::fs::read(path) {
        Ok(data) => parse(&data),
        Err(_) => Vec::new(),
    }
}

/// Parse an `appinfo.vdf` image.
pub fn parse(data: &[u8]) -> Vec<SteamApp> {
    if data.len() < 16 || read_u32(data, 0) != Some(MAGIC) {
        return Vec::new();
    }
    let Some(table_offset) = read_u64(data, 8).map(|offset| offset as usize) else {
        return Vec::new();
    };
    if table_offset >= data.len() {
        return Vec::new();
    }
    let strings = string_table(&data[table_offset..]);

    let mut apps = Vec::new();
    let mut offset = 16usize;
    while offset + 8 <= data.len() {
        let Some(appid) = read_u32(data, offset) else {
            break;
        };
        let Some(size) = read_u32(data, offset + 4).map(|size| size as usize) else {
            break;
        };
        if appid == 0 {
            break;
        }
        let body_start = offset + 8;
        let body_end = body_start.saturating_add(size).min(data.len());
        if size == 0 || body_end <= body_start {
            break;
        }

        if let Some(app) = parse_entry(appid as u64, &data[body_start..body_end], &strings) {
            apps.push(app);
        }
        offset = body_end;
    }
    apps
}

/// Everything after the fixed part of an entry is the key/value tree; the tree itself starts after the metadata (40 bytes) plus 20 bytes of padding.
fn parse_entry(appid: u64, body: &[u8], strings: &[String]) -> Option<SteamApp> {
    let start = 40 + 20;
    if body.len() <= start {
        return None;
    }
    let kv = decode(body, start, strings)?;
    // The tree root is the appinfo object itself, keyed by its name.
    let info = kv.get("appinfo").and_then(Kv::object)?;
    info.get("appid")?;

    let name = info
        .get("common")
        .and_then(|common| common.get("name"))
        .and_then(Kv::text)
        .unwrap_or_default()
        .to_string();

    let app_type = info
        .get("common")
        .and_then(|common| common.get("type"))
        .and_then(Kv::text)
        .unwrap_or_default()
        .to_string();

    let dlc = info
        .get("extended")
        .and_then(|extended| extended.get("listofdlc"))
        .and_then(Kv::text)
        .map(|list| {
            list.split(',')
                .filter_map(|id| id.trim().parse::<u64>().ok())
                .collect()
        })
        .unwrap_or_default();

    let depots = info
        .get("depots")
        .and_then(Kv::object)
        .map(|depots| {
            depots
                .keys()
                .filter_map(|key| key.parse::<u64>().ok())
                .collect()
        })
        .unwrap_or_default();

    Some(SteamApp {
        appid,
        name,
        app_type,
        dlc,
        depots,
    })
}

/// The interned strings, one list for the whole file.
fn string_table(table: &[u8]) -> Vec<String> {
    let Some(length) = read_u32(table, 0).map(|length| length as usize) else {
        return Vec::new();
    };
    let end = (4 + length).min(table.len());
    let mut strings = Vec::new();
    let mut offset = 4;
    while offset < end {
        let Some(rest) = table.get(offset..end) else {
            break;
        };
        let Some(zero) = rest.iter().position(|byte| *byte == 0) else {
            break;
        };
        strings.push(String::from_utf8_lossy(&rest[..zero]).into_owned());
        offset += zero + 1;
    }
    strings
}

/// Decode one key/value tree, returning it and the offset after it.
fn decode(data: &[u8], offset: usize, strings: &[String]) -> Option<Kv> {
    let (tree, _) = decode_object(data, offset, strings)?;
    Some(tree)
}

/// Decode an object (type byte `0x00`), returning it and the next offset.
fn decode_object(data: &[u8], offset: usize, strings: &[String]) -> Option<(Kv, usize)> {
    if data.get(offset) != Some(&0x00) {
        return None;
    }
    let key = string_at(data, offset + 1, strings)?;
    let mut map = BTreeMap::new();
    let mut cursor = offset + 5;
    loop {
        let kind = *data.get(cursor)?;
        cursor += 1;
        if kind == 0x08 {
            break;
        }
        if kind == 0x00 {
            let (child, next) = decode_object(data, cursor - 1, strings)?;
            if let Kv::Object(entries) = child {
                map.extend(entries);
            }
            cursor = next;
            continue;
        }
        let name = string_at(data, cursor, strings)?;
        cursor += 4;
        let value = match kind {
            0x01 => {
                let rest = data.get(cursor..)?;
                let zero = rest.iter().position(|byte| *byte == 0)?;
                cursor += zero + 1;
                Kv::Text(String::from_utf8_lossy(&rest[..zero]).into_owned())
            }
            0x02 => {
                let value = i32::from_le_bytes(data.get(cursor..cursor + 4)?.try_into().ok()?);
                cursor += 4;
                Kv::Int(value)
            }
            0x03 => {
                let value = f32::from_le_bytes(data.get(cursor..cursor + 4)?.try_into().ok()?);
                cursor += 4;
                Kv::Float(value)
            }
            0x04 | 0x06 => {
                let value = read_u32(data, cursor)?;
                cursor += 4;
                Kv::Bytes(value)
            }
            0x05 => {
                let rest = data.get(cursor..)?;
                let mut end = 0;
                while end + 1 < rest.len() && !(rest[end] == 0 && rest[end + 1] == 0) {
                    end += 2;
                }
                let text = String::from_utf16_lossy(
                    &rest[..end]
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect::<Vec<u16>>(),
                );
                cursor += end + 2;
                Kv::Text(text)
            }
            0x07 => {
                let value = u64::from_le_bytes(data.get(cursor..cursor + 8)?.try_into().ok()?);
                cursor += 8;
                Kv::ULong(value)
            }
            _ => return None,
        };
        map.insert(name, value);
    }
    let mut root = BTreeMap::new();
    root.insert(key, Kv::Object(map));
    Some((Kv::Object(root), cursor))
}

/// Resolve the interned string index at `offset`.
fn string_at(data: &[u8], offset: usize, strings: &[String]) -> Option<String> {
    let index = read_u32(data, offset)? as usize;
    strings.get(index).cloned()
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny appinfo image with one entry, so the parser is tested without depending on a Steam install.
    fn sample(appid: u32, name: &str, dlc: &str, depots: &[u32]) -> Vec<u8> {
        let mut strings = vec![
            "appinfo",
            "appid",
            "common",
            "name",
            "extended",
            "listofdlc",
            "depots",
            "type",
        ];
        for depot in depots {
            strings.push(Box::leak(depot.to_string().into_boxed_str()));
        }
        // string table: u32 length, then null terminated strings
        let mut table = Vec::new();
        let mut payload = Vec::new();
        for text in &strings {
            payload.extend_from_slice(text.as_bytes());
            payload.push(0);
        }
        table.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        table.extend_from_slice(&payload);

        // entry body: 40 bytes metadata, 20 bytes padding, then the tree
        let mut body = vec![0u8; 60];
        let index = |name: &str| strings.iter().position(|s| *s == name).unwrap() as u32;
        let key =
            |body: &mut Vec<u8>, name: &str| body.extend_from_slice(&index(name).to_le_bytes());
        let text = |body: &mut Vec<u8>, value: &str| {
            body.extend_from_slice(value.as_bytes());
            body.push(0);
        };

        body.push(0x00); // appinfo object
        key(&mut body, "appinfo");
        body.push(0x02); // appid (int32)
        key(&mut body, "appid");
        body.extend_from_slice(&(appid as i32).to_le_bytes());
        body.push(0x00); // common object
        key(&mut body, "common");
        body.push(0x01); // name
        key(&mut body, "name");
        text(&mut body, name);
        body.push(0x01); // type
        key(&mut body, "type");
        text(&mut body, "Game");
        body.push(0x08);
        body.push(0x00); // extended object
        key(&mut body, "extended");
        body.push(0x01); // listofdlc
        key(&mut body, "listofdlc");
        text(&mut body, dlc);
        body.push(0x08);
        if !depots.is_empty() {
            body.push(0x00); // depots object
            key(&mut body, "depots");
            for depot in depots {
                body.push(0x00); // a depot object, keyed by its id
                body.extend_from_slice(&index(&depot.to_string()).to_le_bytes());
                body.push(0x08);
            }
            body.push(0x08);
        }
        body.push(0x08);

        let mut image = Vec::new();
        image.extend_from_slice(&MAGIC.to_le_bytes());
        image.extend_from_slice(&1u32.to_le_bytes()); // universe
        image.extend_from_slice(&0u64.to_le_bytes()); // table offset, patched below
        image.extend_from_slice(&appid.to_le_bytes());
        image.extend_from_slice(&(body.len() as u32).to_le_bytes());
        image.extend_from_slice(&body);
        image.extend_from_slice(&0u32.to_le_bytes()); // terminator
        let table_offset = image.len() as u64;
        image.extend_from_slice(&table);
        image[8..16].copy_from_slice(&table_offset.to_le_bytes());
        image
    }

    #[test]
    fn parses_a_synthetic_entry() {
        let image = sample(620, "Portal 2", "323180,2012840", &[731, 732]);
        let apps = parse(&image);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].appid, 620);
        assert_eq!(apps[0].name, "Portal 2");
        assert_eq!(apps[0].dlc, vec![323180, 2012840]);
        assert_eq!(apps[0].depots, vec![731, 732]);
    }

    #[test]
    #[ignore = "needs a local Steam install"]
    fn reads_the_real_steam_cache() {
        let apps = load();
        eprintln!("apps parsed: {}", apps.len());
        for appid in [620u64, 323180, 629, 5, 400] {
            if let Some(app) = apps.iter().find(|app| app.appid == appid) {
                eprintln!("{appid}: type={:?} name={:?}", app.app_type, app.name);
            }
        }
        let mut types = std::collections::BTreeMap::new();
        for app in &apps {
            *types.entry(app.app_type.clone()).or_insert(0usize) += 1;
        }
        eprintln!("types: {types:?}");
    }

    #[test]
    fn rejects_other_magics_and_truncated_files() {
        assert!(parse(b"not an appinfo file").is_empty());
        assert!(parse(&[]).is_empty());
        let mut image = sample(1, "x", "", &[]);
        image[0] = 0;
        assert!(parse(&image).is_empty());
    }
}
