//! SLSconfigurator - a TUI editor for SLSsteam's config.

mod config;
mod schema;
mod tui;

use std::path::PathBuf;

fn usage() {
    println!(
        "\
slsconfigurator - edit the SLSsteam config

Usage: slsconfigurator [--config PATH]

Without --config the file SLSsteam itself uses is opened:
$XDG_CONFIG_HOME/SLSsteam/config.yaml, or ~/.config/SLSsteam/config.yaml.

Keys:
  up/down     select a setting or entry
  enter       toggle a boolean, edit a value, open a collection
  space       toggle a boolean in a collection
  a / d       add / delete an entry in a collection
  s           save (SLSsteam reloads the file automatically)
  q / esc     quit, with a save prompt when there are changes

If the file does not exist yet, start Steam once with SLSsteam installed so it creates the config with its comments."
    );
}

/// SLSsteam resolves its config through `XDG_CONFIG_HOME`, falling back to `$HOME/.config`; match that.
fn default_config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::config_dir()?,
    };
    Some(base.join("SLSsteam").join("config.yaml"))
}

fn main() {
    let mut config = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => match args.next() {
                Some(path) => config = Some(PathBuf::from(path)),
                None => {
                    eprintln!("--config needs a path");
                    std::process::exit(2);
                }
            },
            "--help" | "-h" => {
                usage();
                return;
            }
            other => {
                eprintln!("unknown argument: {other}\n");
                usage();
                std::process::exit(2);
            }
        }
    }

    let Some(path) = config.or_else(default_config_path) else {
        eprintln!("could not work out where the SLSsteam config lives");
        std::process::exit(1);
    };

    if !path.is_file() {
        eprintln!(
            "no config at {}\nStart Steam once with SLSsteam installed so it creates the file, or point at one with --config.",
            path.display()
        );
        std::process::exit(1);
    }

    if let Err(message) = tui::run(path.clone()) {
        eprintln!("slsconfigurator: {message}");
        std::process::exit(1);
    }
}
