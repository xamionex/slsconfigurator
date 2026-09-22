//! Known SLSsteam settings, the shapes their values take and the help text for each of them.
//!
//! The shapes come from SLSsteam's `src/config.cpp` (`getSetting`, `getList` and `getMap` calls per key), so a key that is empty in the file can still be edited with the right shape.
//! The order and grouping follow`res/config.yaml`, which is what users see when they open the file.

/// How a setting's value is shaped in the YAML file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `yes` / `no`
    Bool,
    /// Decimal or `0x` / `0b` / `0o` integer, kept as text to avoid overflow.
    Integer,
    /// Free text, quoted when needed.
    Text,
    /// AppId list: `- 440` entries.
    Seq,
    /// AppId keyed map with a scalar value.
    Map,
    /// SteamId keyed map whose values are AppId lists (`DenuvoGames`).
    MapOfSeq,
    /// AppId keyed map whose values are maps (`DlcData`, `InventoryItems`).
    MapOfMap,
    /// The fixed `{AppId, Title}` mapping.
    IdleStatus,
}

/// Element type of a `Map` value or a `MapOfMap` inner value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Integer,
    Text,
}

/// A known top-level config key.
pub struct Setting {
    pub key: &'static str,
    pub section: &'static str,
    pub shape: Shape,
    pub help: &'static str,
}

/// Settings in the order the shipped `config.yaml` documents them.
pub const SETTINGS: &[Setting] = &[
    Setting {
        key: "DisableFamilyShareLock",
        section: "General",
        shape: Shape::Bool,
        help: "Disables Family Share license locking for yourself and others",
    },
    Setting {
        key: "UseWhitelist",
        section: "General",
        shape: Shape::Bool,
        help: "Switches AppIds to a whitelist instead of the default blacklist",
    },
    Setting {
        key: "AppIds",
        section: "App lists",
        shape: Shape::Seq,
        help: "AppIds to include (whitelist) or exclude (blacklist). Adding a game's AppId also covers its DLCs",
    },
    Setting {
        key: "AdditionalApps",
        section: "App lists",
        shape: Shape::Seq,
        help: "AppIds to inject for apps you got shared. Breaks downloads, so best used for games not in your library",
    },
    Setting {
        key: "FakeOffline",
        section: "App lists",
        shape: Shape::Seq,
        help: "AppIds to see Steam as offline for",
    },
    Setting {
        key: "DepotBlacklist",
        section: "App lists",
        shape: Shape::Seq,
        help: "Depots that should never be downloaded",
    },
    Setting {
        key: "FakeAppIds",
        section: "Overrides",
        shape: Shape::Map,
        help: "Change appIds so networking works. Key 0 applies to all unowned apps. Do not run two apps under the same appId at once",
    },
    Setting {
        key: "ManifestIds",
        section: "Overrides",
        shape: Shape::Map,
        help: "Override depot manifest IDs, e.g. to pin a game version",
    },
    Setting {
        key: "AppTokens",
        section: "Overrides",
        shape: Shape::Map,
        help: "AppId to token pairs, used to fetch ProductInfo from Steam for some games",
    },
    Setting {
        key: "CDKeys",
        section: "Overrides",
        shape: Shape::Map,
        help: "Legacy CD keys required by some games; a random one is generated when left empty",
    },
    Setting {
        key: "GameTitles",
        section: "Overrides",
        shape: Shape::Map,
        help: "Override game titles. Owned AppIds only; for injected AppIds use FakeAppIds",
    },
    Setting {
        key: "SubscriptionTimestamps",
        section: "Overrides",
        shape: Shape::Map,
        help: "Override purchase time stamps",
    },
    Setting {
        key: "DenuvoGames",
        section: "Overrides",
        shape: Shape::MapOfSeq,
        help: "SteamId to AppId lists: blocks those games from unlocking on the wrong accounts",
    },
    Setting {
        key: "SteamIdOverride",
        section: "Overrides",
        shape: Shape::Map,
        help: "AppId to SteamId: overrides the SteamId an app sees, e.g. when automatic spoofing fails. 0 uses the cached ticket's SteamId",
    },
    Setting {
        key: "LaunchOptions",
        section: "Overrides",
        shape: Shape::Map,
        help: "Per appId launch commands using %command%. Keys 4294967294 (unowned apps) and 4294967295 (all apps) are special",
    },
    Setting {
        key: "DlcData",
        section: "DLC",
        shape: Shape::MapOfMap,
        help: "DLC names per AppId, needed when a game is hit by Steam's 64 DLC limit",
    },
    Setting {
        key: "IdleStatus",
        section: "DLC",
        shape: Shape::IdleStatus,
        help: "Custom in-game status: AppId and Title. Set AppId to 0 to disable",
    },
    Setting {
        key: "SafeMode",
        section: "Client",
        shape: Shape::Bool,
        help: "Automatically disable SLSsteam when steamclient.so does not match a known good hash. Enable this in Steam Deck gaming mode",
    },
    Setting {
        key: "WarnHashMissmatch",
        section: "Client",
        shape: Shape::Bool,
        help: "Notify when the steamclient.so hash differs from a known safe hash",
    },
    Setting {
        key: "NotifyInit",
        section: "Client",
        shape: Shape::Bool,
        help: "Notify when SLSsteam finished initializing",
    },
    Setting {
        key: "API",
        section: "Client",
        shape: Shape::Bool,
        help: "Accept commands through the /tmp/SLSsteam.API socket",
    },
    Setting {
        key: "Plugins",
        section: "Client",
        shape: Shape::Bool,
        help: "Load Lua plugins from the plugins directory. They can run arbitrary code",
    },
    Setting {
        key: "DisableCloud",
        section: "Client",
        shape: Shape::Bool,
        help: "Disable cloud saves for unlocked games. Set to no when using CloudRedirect",
    },
    Setting {
        key: "DisableUpdates",
        section: "Client",
        shape: Shape::Bool,
        help: "Disable updates for AppIds in AdditionalApps. Unowned games only, use ManifestIds for owned ones",
    },
    Setting {
        key: "FakeName",
        section: "Identity",
        shape: Shape::Text,
        help: "Changes your persona name client-side; empty disables it",
    },
    Setting {
        key: "FakeEmail",
        section: "Identity",
        shape: Shape::Text,
        help: "Changes your account e-mail client-side; empty disables it",
    },
    Setting {
        key: "FakeWalletBalance",
        section: "Identity",
        shape: Shape::Integer,
        help: "Changes your wallet balance client-side; 0 turns it off",
    },
    Setting {
        key: "SmartTickets",
        section: "Logging",
        shape: Shape::Integer,
        help: "Bitwise flags: 0x1 SteamDRM, 0x2 Denuvo. Analysing a freshly connected executable makes spoofing smarter",
    },
    Setting {
        key: "MaxSchemaTries",
        section: "Logging",
        shape: Shape::Integer,
        help: "How often achievement schemas are fetched from Steam's CDN; 0 falls back to the offline cache",
    },
    Setting {
        key: "LogLevels",
        section: "Logging",
        shape: Shape::Integer,
        help: "Bitwise flags: 0x1 Trace, 0x2 Once, 0x4 Debug, 0x8 Warn, 0x10 Error, 0x20 Info, 0x40 NotifyShort, 0x80 NotifyLong (0xff is everything)",
    },
    Setting {
        key: "DumpClientInterfaces",
        section: "Logging",
        shape: Shape::Bool,
        help: "Dump all used IClientInterfaceMaps",
    },
    Setting {
        key: "ExtendedLogging",
        section: "Logging",
        shape: Shape::Bool,
        help: "Log every Steamworks call, which makes the log file huge",
    },
];

/// The setting for `key`, if it is a known one.
pub fn find(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|setting| setting.key == key)
}

/// Element type of the values of a map-like setting.
pub fn value_kind(key: &str) -> Option<ValueKind> {
    match key {
        "FakeAppIds"
        | "ManifestIds"
        | "AppTokens"
        | "SubscriptionTimestamps"
        | "SteamIdOverride"
        | "CloudProxies"
        | "InventoryItems" => Some(ValueKind::Integer),
        "CDKeys" | "GameTitles" | "LaunchOptions" | "DecryptionKeys" | "DlcData" => {
            Some(ValueKind::Text)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_are_unique() {
        let mut seen = HashSet::new();
        for setting in SETTINGS {
            assert!(seen.insert(setting.key), "duplicate key {}", setting.key);
        }
    }

    #[test]
    fn every_setting_has_help_and_a_section() {
        for setting in SETTINGS {
            assert!(!setting.help.is_empty(), "{} has no help", setting.key);
            assert!(
                !setting.section.is_empty(),
                "{} has no section",
                setting.key
            );
        }
    }

    #[test]
    fn shapes_match_slssteam() {
        // The non-obvious shapes, straight from src/config.cpp.
        assert_eq!(find("SteamIdOverride").unwrap().shape, Shape::Map);
        assert_eq!(find("DenuvoGames").unwrap().shape, Shape::MapOfSeq);
        assert_eq!(find("DlcData").unwrap().shape, Shape::MapOfMap);
        assert_eq!(find("IdleStatus").unwrap().shape, Shape::IdleStatus);
        assert_eq!(find("LogLevels").unwrap().shape, Shape::Integer);
        assert!(find("not a setting").is_none());
    }

    #[test]
    fn map_like_settings_declare_their_value_kind() {
        for setting in SETTINGS {
            if matches!(setting.shape, Shape::Map | Shape::MapOfMap) {
                assert!(
                    value_kind(setting.key).is_some(),
                    "{} needs a value kind",
                    setting.key
                );
            }
        }
    }
}
