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
