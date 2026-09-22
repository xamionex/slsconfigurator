# SLSconfigurator

A TUI editor for [SLSsteam](https://github.com/AceSLS/SLSsteam)'s `~/.config/SLSsteam/config.yaml`, written for the public SLSsteam build.

```
┌SLSsteam config──────────────────────────────────────────────────────────────┐
│/home/user/.config/SLSsteam/config.yaml                                      │
└─────────────────────────────────────────────────────────────────────────────┘
┌Settings─────────────────────────────────────────────────────────────────────┐
│> DisableFamilyShareLock      yes                                            │
│  UseWhitelist                no                                             │
│  -- App lists --                                                            │
│  AppIds                      [12 entries]                                   │
│  FakeAppIds                  {3 entries}                                    │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/xamionex/slsconfigurator/main/install.sh | sh
```

Downloads the latest release and installs it to `~/.local/bin/slsconfigurator`.
Make sure `~/.local/bin` is on your `PATH`.

Releases are built by GitHub Actions on every `v*` tag with cargo-zigbuild against glibc 2.28, so the binary also runs on SteamOS.
The release asset is named `slsconfigurator`.

## What it does

- Lists every SLSsteam setting with its live value, grouped the way the shipped file documents them.
- Booleans toggle with space or enter, numbers and text are edited inline.
- Collections (`AppIds`, `FakeAppIds`, `GameTitles`, `ManifestIds`, `DlcData`, `DenuvoGames`, `LaunchOptions`, ...) open a tree editor where entries can be added, edited and deleted, including nested ones.
- Help text is the comment block the shipped `config.yaml` puts above the setting, so the advice you see is SLSsteam's own.
- Values are checked while you type: AppId lists and AppId keyed maps only accept numbers, and a last line of defense refuses to write a file that is not valid YAML.

## How it writes the file

The shipped config is documentation as much as configuration, so the editor is line accurate: it keeps the file as lines and rewrites only the lines you touch.
Comments, inline notes, commented out entries and blank lines all survive, and inserted entries are appended in the same indent style the collection already uses.
SLSsteam watches the file and hot reloads it, so saving applies without restarting Steam.

## Build and run

```sh
cargo build --release
./target/release/slsconfigurator
# uses $XDG_CONFIG_HOME/SLSsteam/config.yaml
```

With target config:
```sh
./target/release/slsconfigurator --config /path/to/config.yaml
```

The file has to exist already; start Steam once with SLSsteam installed so it creates the file with its comments, or point at an existing one with `--config`.

## Keys

| Key | Action |
| --- | ------ |
| `up` / `down`, `k` / `j` | move |
| `enter` | toggle a boolean, edit a value, open a collection |
| `space` | toggle a boolean (in a collection too) |
| `tab` | edit the comment above the selected setting, or the inline comment of a collection entry |
| `g` | Steam picker: search games, DLCs and packages, space checks them in |
| `a` / `d` | add / delete an entry, or quick add an AppId to a list |
| `esc` / `b` | leave a collection or a comment; `esc` steps back through the picker and closes it |
| `s` | save (SLSsteam reloads it automatically) |
| `q` / `esc` | quit, with a save/discard prompt when there are changes |

### Steam picker (`g`)

`g` opens a Steam search next to the settings.
The box at the top is the search field and shows what you are typing: results follow along on their own as you type (after a short pause, so Steam is not hammered per keystroke). Plain letters always type, so a query like `souls` cannot trigger a save; `ctrl+s` saves, `ctrl+q` quits and `esc` steps back one level at a time (stop typing, leave a focused game, then close the panel), so the panel is never open without the focus.

- `r` (or `ctrl+r` while typing) opens a **remove games** list of everything already in the target, filtered by what you type; `space` takes a game out, `t` switches the target the list refers to and `esc` goes back to searching
- `space` checks a game in or out and writes it straight into the config; unchecking removes it again.
  Checking a game also takes its **DLCs** and its **packages** along in one go, so a single key press configures the whole game.
  Unchecking removes them again when they are known from the same session.
  SLSsteam reloads the file on save
- `space` on a group header toggles the whole group
- `enter` on a game **focuses** it: the results are replaced by that game, its **DLCs**, its **packages** and its **depots**, with names looked up from Steam, so nothing has to be scrolled past to reach them.
  `esc` goes back to the results, a second `esc` leaves the picker
- `t` switches where games and DLCs are written; the default is `AdditionalApps`, `t` switches to `AppIds`. Packages always go to `AdditionalPackages`
- `ctrl+s` saves and `ctrl+q` quits from anywhere in the picker, typing included; a plain letter always starts a new search, so `souls` never saves

### What the search looks at

Three sources, always merged:

1. **The local Steam client cache** (`appcache/appinfo.vdf`, parsed with its v29 string table format), which makes search instant and lets it work offline.
2. **Steam's store autocomplete** (`search/suggest`), and
3. **the store's own search backend** on top of that, which also covers DLCs and bundles.
   Both are cached for a day, so a repeated search is instant.

Matches are ranked in tiers, and a query only ever shows hits from its best tier, so real matches and vague ones never mix:

| Tier | Example |
| ---- | ------- |
| exact | `portal 2` -> `Portal 2` |
| name prefix | `hello neigh` -> `Hello Neighbor` |
| query words as word prefixes | `neighbor` -> `Hello Neighbor` |
| substring | `souls` -> `DARK SOULS™: REMASTERED` |
| initials / subsequence (fallback only) | `dsr` -> `DARK SOULS™: REMASTERED`, `hl2` -> `Half-Life 2` |

- punctuation and symbols are ignored: `DARK SOULS™:` is `dark souls`
- roman and arabic numerals are the same word, so `dark souls 3` finds `DARK SOULS™ III` and `ds3` finds it through the initials
- games and DLCs rank above tools/software of the same tier
- entries Steam ranked itself lead within their tier
- exact initials matches come along even when other games matched the words, fuzzy subsequences do not; when nothing matches strictly the results are capped at 25 and labelled `no exact match`, which is what keeps a query like `hello` from dragging in things like "Timelie - Hell Loop"

Names are learned from three places: the client cache, Steam, and **your own config annotations** (`- 570940 # DARK SOULS™: REMASTERED`), so an abbreviation resolves for a game the cache has never seen.

DLCs and packages come from `appdetails`, and **depot ids from the local client cache**, which is the only place they are exposed.
Opening a game therefore lists its `DLCs`, `Packages` and `Depots` groups, and each can be checked in or out; depots are written to `AdditionalDepots`.

Abbreviations only resolve for apps the client knows about (owned, wishlisted or recently viewed), since Steam publishes no alias data.

Looking a game up is cached in `~/.config/SLSconfigurator/cache/`: search results for a day, an app's DLCs and packages for a week, and looked-up names for a month.
Repeated searches are instant and keep working offline; delete that directory to refresh everything.

## Notes

- Settings the file does not contain are shown as `(missing from the file)` and are skipped;
- SLSsteam falls back to its built-in defaults for them.
- Keys this build does not know appear under `Other keys`, marked `(unknown key)`.
- They are never modified unless you edit them.
