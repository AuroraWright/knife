//! `knife.ini`: the command line options as `key = value` lines, read from the executable's folder (and, inside a
//! macOS `.app`, `Contents/Resources`), as shin reads `shin.ini`. Options given on the actual command line take
//! precedence.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use clap::CommandFactory;

use super::Cli;

pub const FILE_NAME: &str = "knife.ini";

/// Options whose relative values resolve against the ini's folder rather than the working directory.
const PATH_OPTIONS: &[&str] = &["assets-dir", "mod-assets-dir", "save-file"];

/// `knife.ini` candidates, most specific first.
pub fn candidates(executable: &Path) -> Vec<PathBuf> {
    super::executable_dirs(executable).into_iter().map(|dir| dir.join(FILE_NAME)).collect()
}

/// Long names of the options present on the command line.
fn explicit_options(args: &[OsString]) -> Vec<String> {
    let command = Cli::command();
    let mut names = Vec::new();
    for arg in args {
        let Some(arg) = arg.to_str() else { continue };
        if arg == "--" {
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            names.push(long.split('=').next().unwrap_or(long).to_owned());
        } else if let Some(short) = arg.strip_prefix('-').and_then(|s| s.chars().next())
            && let Some(option) = command.get_arguments().find(|a| a.get_short() == Some(short))
            && let Some(long) = option.get_long()
        {
            names.push(long.to_owned());
        }
    }
    names
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Convert ini text to command line arguments, skipping options in `explicit`.
pub fn to_args(text: &str, folder: &Path, explicit: &[String]) -> anyhow::Result<Vec<OsString>> {
    let command = Cli::command();
    let mut args = Vec::new();
    let mut seen = Vec::<String>::new();
    for (number, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let number = number + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with([';', '#']) || (line.starts_with('[') && line.ends_with(']')) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("line {number}: expected `option = value`, found `{line}`");
        };
        let key = key.trim().trim_start_matches("--").replace('_', "-").to_ascii_lowercase();
        let mut value = value.trim();
        for quote in ['"', '\''] {
            if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
                value = &value[1..value.len() - 1];
            }
        }
        let Some(option) = command
            .get_arguments()
            .find(|a| a.get_long() == Some(key.as_str()))
            .filter(|a| !matches!(a.get_long(), Some("help" | "version")))
        else {
            bail!("line {number}: unknown option `{key}`");
        };
        if seen.contains(&key) {
            bail!("line {number}: `{key}` is set more than once");
        }
        seen.push(key.clone());
        if explicit.contains(&key) {
            continue;
        }
        if option.get_action().takes_values() {
            if value.is_empty() {
                continue;
            }
            args.push(format!("--{key}").into());
            if PATH_OPTIONS.contains(&key.as_str()) {
                args.push(folder.join(value).into_os_string());
            } else {
                args.push(value.into());
            }
        } else {
            match parse_bool(value) {
                Some(true) => args.push(format!("--{key}").into()),
                Some(false) => {}
                None => bail!("line {number}: `{key}` takes true or false, found `{value}`"),
            }
        }
    }
    Ok(args)
}

/// Program name, then the first `knife.ini`'s options, then the command line.
pub fn merged_args() -> Vec<OsString> {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    let program = if args.is_empty() { "knife".into() } else { args.remove(0) };
    let mut merged = vec![program];
    let ini = crate::platform::executable().into_iter().flat_map(|exe| candidates(&exe)).find(|path| path.is_file());
    if let Some(path) = ini {
        let loaded = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))
            .and_then(|text| {
                let folder = path.parent().unwrap_or(Path::new("."));
                to_args(&text, folder, &explicit_options(&args)).with_context(|| format!("Invalid {}", path.display()))
            });
        match loaded {
            Ok(options) => {
                tracing::info!("Using {} ({} arguments)", path.display(), options.len());
                merged.extend(options);
            }
            Err(error) => {
                eprintln!("error: {error:#}");
                std::process::exit(2);
            }
        }
    }
    merged.extend(args);
    merged
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;

    use super::*;

    #[test]
    fn ini_fills_omitted_options_and_command_line_wins() {
        let folder = Path::new("/games/knife");
        let text = "\u{feff}; comment\n# comment\n[knife]\nlanguage = en\nassets-dir = \"assets dir\"\n\
                    mod_assets_dir = mods\nsave_file=/abs/savedata000\nskip-intro = true\nskip-movies = off\n\
                    --text-websocket = 127.0.0.1:6677\n";
        let mut argv = vec![OsString::from("knife")];
        argv.extend(to_args(text, folder, &[]).unwrap());
        let cli = Cli::try_parse_from(&argv).unwrap();
        assert_eq!(cli.language, Some(1));
        assert_eq!(cli.assets_dir(), folder.join("assets dir"));
        assert_eq!(cli.mod_assets_dir.as_deref(), Some(folder.join("mods").as_path()));
        assert_eq!(cli.save_file.as_deref(), Some(Path::new("/abs/savedata000")));
        assert!(cli.skip_intro && !cli.skip_movies);
        assert_eq!(cli.text_websocket.as_deref(), Some("127.0.0.1:6677"));

        // options on the command line replace the ini's; empty values stay unset
        let explicit = explicit_options(&["-l".into(), "ja".into(), "--assets-dir=b".into()]);
        let mut argv = vec![OsString::from("knife")];
        argv.extend(to_args("language = en\nassets-dir = a\nskip-intro = yes\nsave-file =\n", folder, &explicit).unwrap());
        argv.extend(["-l", "ja", "--assets-dir=b"].map(Into::into));
        let cli = Cli::try_parse_from(&argv).unwrap();
        assert_eq!(cli.language, Some(0));
        assert_eq!(cli.assets_dir(), Path::new("b"));
        assert_eq!(cli.save_file, None);
        assert!(cli.skip_intro);

        for bad in ["unknown = 1", "help = true", "language", "skip-intro = maybe", "language = en\nlanguage = ja"] {
            assert!(to_args(bad, folder, &[]).is_err(), "{bad}");
        }
        assert_eq!(
            candidates(Path::new("/b/Knife.app/Contents/MacOS/knife")),
            [Path::new("/b/Knife.app/Contents/MacOS/knife.ini"), Path::new("/b/Knife.app/Contents/Resources/knife.ini")]
        );
        assert_eq!(candidates(Path::new("/b/knife")), [Path::new("/b/knife.ini")]);

        // the packaged example parses as is
        let mut argv = vec![OsString::from("knife")];
        argv.extend(to_args(include_str!("../../knife.ini.example"), folder, &[]).unwrap());
        let cli = Cli::try_parse_from(&argv).unwrap();
        assert_eq!(cli.assets_dir(), folder.join("assets"));
        assert_eq!(cli.mod_assets_dir, Some(folder.join("mod_assets")));
        assert_eq!(cli.save_file, None);
    }
}
