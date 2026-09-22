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
| `a` / `d` | add / delete an entry while editing a collection |
| `esc` / `b` | leave a collection |
| `s` | save (SLSsteam reloads it automatically) |
| `q` / `esc` | quit, with a save/discard prompt when there are changes |

## Notes

- Settings the file does not contain are shown as `(missing from the file)` and are skipped;
- SLSsteam falls back to its built-in defaults for them.
- Keys this build does not know appear under `Other keys`, marked `(unknown key)`.
- They are never modified unless you edit them.
