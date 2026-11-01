//! XDG and Videos directory resolution.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// XDG path for the configuration file.
pub fn configuration_path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(
        config_home
            .join(kestrel_core::DATA_DIRECTORY_NAME)
            .join("config.toml"),
    )
}

/// `$XDG_STATE_HOME/<data directory>`, falling back to `~/.local/state`.
pub(crate) fn state_directory() -> Option<PathBuf> {
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(state_home.join(kestrel_core::DATA_DIRECTORY_NAME))
}

/// `$XDG_VIDEOS_DIR/Kestrel` from `user-dirs.dirs`, else `~/Videos/Kestrel`.
pub fn recordings_path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let videos = fs::read_to_string(config_home.join("user-dirs.dirs"))
        .ok()
        .and_then(|contents| videos_directory(&contents, &home))
        .unwrap_or_else(|| home.join("Videos"));
    Some(videos.join(if kestrel_core::DEVELOPMENT_BUILD {
        "Kestrel (Development)"
    } else {
        "Kestrel"
    }))
}

/// `XDG_VIDEOS_DIR` from `user-dirs.dirs` contents; only `$HOME/…` and
/// absolute values are valid there.
fn videos_directory(contents: &str, home: &Path) -> Option<PathBuf> {
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

    use super::videos_directory;

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
            assert_eq!(videos_directory(contents, home), expected, "{contents:?}");
        }
    }
}
