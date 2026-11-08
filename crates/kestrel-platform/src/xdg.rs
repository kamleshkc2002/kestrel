//! User-session XDG base directories from the environment.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// The unprivileged user's home folder.
pub fn home() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn base_directory(variable: &str, fallback: &str) -> Option<PathBuf> {
    env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(fallback)))
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    base_directory("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME`, else `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    base_directory("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_STATE_HOME`, else `~/.local/state`.
pub fn state_home() -> Option<PathBuf> {
    base_directory("XDG_STATE_HOME", ".local/state")
}

/// `$XDG_CACHE_HOME`, else `~/.cache`.
pub fn cache_home() -> Option<PathBuf> {
    base_directory("XDG_CACHE_HOME", ".cache")
}

/// `$XDG_VIDEOS_DIR` from `user-dirs.dirs`, else `~/Videos`.
pub fn videos_directory() -> Option<PathBuf> {
    let home = home()?;
    let configured = fs::read_to_string(config_home()?.join("user-dirs.dirs"))
        .ok()
        .and_then(|contents| videos_from_user_dirs(&contents, &home));
    Some(configured.unwrap_or_else(|| home.join("Videos")))
}

/// `XDG_VIDEOS_DIR` from `user-dirs.dirs` contents; only `$HOME/…` and
/// absolute values are valid there.
fn videos_from_user_dirs(contents: &str, home: &Path) -> Option<PathBuf> {
    let value = contents
        .lines()
        .filter_map(|line| line.trim().strip_prefix("XDG_VIDEOS_DIR="))
        .next_back()?
        .trim()
        .strip_prefix('"')?
        .strip_suffix('"')?;
    if let Some(relative) = value.strip_prefix("$HOME") {
        let relative = relative.trim_start_matches('/');
        // `$HOME` alone means the directory is disabled.
        return (!relative.is_empty()).then(|| home.join(relative));
    }
    value.starts_with('/').then(|| PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::videos_from_user_dirs;

    #[test]
    fn videos_folder_follows_user_dirs() {
        let home = Path::new("/home/user");
        for (contents, expected) in [
            (
                "# comment\nXDG_VIDEOS_DIR=\"$HOME/Filme\"\n",
                Some(PathBuf::from("/home/user/Filme")),
            ),
            (
                "XDG_VIDEOS_DIR=\"/data/videos\"",
                Some(PathBuf::from("/data/videos")),
            ),
            ("XDG_VIDEOS_DIR=\"$HOME/\"", None),
            ("XDG_VIDEOS_DIR=\"relative\"", None),
            ("XDG_MUSIC_DIR=\"$HOME/Music\"", None),
        ] {
            assert_eq!(
                videos_from_user_dirs(contents, home),
                expected,
                "{contents:?}"
            );
        }
    }
}
