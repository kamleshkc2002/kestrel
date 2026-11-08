//! XDG and Videos directory resolution.

use std::path::PathBuf;

/// XDG path for the configuration file.
pub fn configuration_path() -> Option<PathBuf> {
    Some(
        kestrel_platform::xdg::config_home()?
            .join(kestrel_core::DATA_DIRECTORY_NAME)
            .join("config.toml"),
    )
}

/// `$XDG_STATE_HOME/<data directory>`, falling back to `~/.local/state`.
pub(crate) fn state_directory() -> Option<PathBuf> {
    Some(kestrel_platform::xdg::state_home()?.join(kestrel_core::DATA_DIRECTORY_NAME))
}

/// `$XDG_DATA_HOME/<data directory>`, falling back to `~/.local/share`.
pub(crate) fn data_directory() -> Option<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(data_home.join(kestrel_core::DATA_DIRECTORY_NAME))
}

/// The encrypted store for pinned clipboard text.
pub(crate) fn clipboard_pins_path() -> Option<PathBuf> {
    Some(data_directory()?.join(kestrel_platform::pin_store::PIN_FILE_NAME))
}

/// `$XDG_VIDEOS_DIR/Kestrel` from `user-dirs.dirs`, else `~/Videos/Kestrel`.
pub fn recordings_path() -> Option<PathBuf> {
    Some(
        kestrel_platform::xdg::videos_directory()?.join(if kestrel_core::DEVELOPMENT_BUILD {
            "Kestrel (Development)"
        } else {
            "Kestrel"
        }),
    )
}
