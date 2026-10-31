//! User GTK style sheets and the window's colour scheme.
//!
//! COSMIC keeps `gtk.css` linked to one of its own style sheets and rewrites
//! the link when the desktop mode changes. Kestrel applies the matching sheet
//! above the user priority so its Appearance setting is not defeated.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use kestrel_core::AppearancePreference;

/// Largest user style sheet Kestrel reads.
const MAX_STYLESHEET_BYTES: usize = 256 * 1024;

/// Light or dark mode named by a COSMIC style sheet pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

/// What the user style sheet directory holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserStylesheet {
    /// No `gtk.css` and no complete COSMIC pair.
    Absent,
    /// COSMIC's light and dark style sheets, with `gtk.css` linked to one.
    Cosmic {
        light: PathBuf,
        dark: PathBuf,
        current: Option<Mode>,
    },
    /// A style sheet Kestrel does not own and cannot follow.
    Custom,
}

/// Resolves the GTK 4 style sheet directory from the XDG environment.
pub fn gtk_config_dir() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("gtk-4.0"))
        .or_else(|| {
            env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".config").join("gtk-4.0"))
        })
}

/// Classifies the style sheets COSMIC or the user placed in `gtk_config_dir`.
pub fn user_stylesheet(gtk_config_dir: &Path) -> UserStylesheet {
    let gtk_css = gtk_config_dir.join("gtk.css");
    let light = gtk_config_dir.join("cosmic").join("light.css");
    let dark = gtk_config_dir.join("cosmic").join("dark.css");
    let cosmic_pair = light.is_file() && dark.is_file();
    match fs::symlink_metadata(&gtk_css) {
        Err(_) if cosmic_pair => UserStylesheet::Cosmic {
            light,
            dark,
            current: None,
        },
        Err(_) => UserStylesheet::Absent,
        Ok(metadata) => {
            // A plain file or a link elsewhere is not COSMIC's to change.
            let current = if cosmic_pair && metadata.file_type().is_symlink() {
                linked_mode(&gtk_css, &light, &dark)
            } else {
                None
            };
            match current {
                Some(current) => UserStylesheet::Cosmic {
                    light,
                    dark,
                    current: Some(current),
                },
                None => UserStylesheet::Custom,
            }
        }
    }
}

/// Returns the COSMIC sheet whose colours a preference asks for.
pub fn stylesheet_file(
    stylesheet: &UserStylesheet,
    preference: AppearancePreference,
) -> Option<&Path> {
    let UserStylesheet::Cosmic {
        light,
        dark,
        current,
    } = stylesheet
    else {
        return None;
    };
    match preference {
        AppearancePreference::Light => Some(light),
        AppearancePreference::Dark => Some(dark),
        AppearancePreference::System => match current {
            Some(Mode::Light) => Some(light),
            Some(Mode::Dark) => Some(dark),
            None => None,
        },
    }
}

/// Whether a custom style sheet can override an explicit light or dark choice.
pub fn colors_are_overridden(preference: AppearancePreference) -> bool {
    if preference == AppearancePreference::System {
        return false;
    }
    gtk_config_dir().is_some_and(|directory| user_stylesheet(&directory) == UserStylesheet::Custom)
}

fn linked_mode(gtk_css: &Path, light: &Path, dark: &Path) -> Option<Mode> {
    let target = fs::canonicalize(gtk_css).ok()?;
    if fs::canonicalize(light).is_ok_and(|path| path == target) {
        return Some(Mode::Light);
    }
    if fs::canonicalize(dark).is_ok_and(|path| path == target) {
        return Some(Mode::Dark);
    }
    None
}

#[cfg(feature = "gui")]
mod styled {
    use std::{
        cell::{Cell, OnceCell, RefCell},
        fs,
        io::{self, Read},
        path::{Path, PathBuf},
        rc::Rc,
        time::Duration,
    };

    use adw::{gio, glib, gtk, prelude::*};

    use super::{MAX_STYLESHEET_BYTES, gtk_config_dir, stylesheet_file, user_stylesheet};
    use kestrel_core::AppearancePreference;

    /// Delay that folds a burst of directory events into one reload.
    const RELOAD_DELAY: Duration = Duration::from_millis(200);

    /// Applies the user's COSMIC style sheet above the user priority.
    pub struct AppearanceStyles {
        provider: OnceCell<gtk::CssProvider>,
        directory: Option<PathBuf>,
        preference: Cell<AppearancePreference>,
        generation: Cell<u64>,
        monitors: RefCell<Vec<gio::FileMonitor>>,
        installed: Cell<bool>,
    }

    impl AppearanceStyles {
        pub fn new() -> Self {
            Self {
                provider: OnceCell::new(),
                directory: gtk_config_dir(),
                preference: Cell::new(AppearancePreference::System),
                generation: Cell::new(0),
                monitors: RefCell::new(Vec::new()),
                installed: Cell::new(false),
            }
        }

        /// Adds the provider to the display and starts following changes.
        pub fn install(self: &Rc<Self>) {
            if self.installed.replace(true) {
                return;
            }
            let Some(display) = gtk::gdk::Display::default() else {
                log::debug!("no display is available for the user style sheet");
                return;
            };
            gtk::style_context_add_provider_for_display(
                &display,
                self.provider.get_or_init(gtk::CssProvider::new),
                gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
            );
            let weak = Rc::downgrade(self);
            adw::StyleManager::default().connect_dark_notify(move |_| {
                if let Some(styles) = weak.upgrade() {
                    styles.apply(styles.preference.get());
                }
            });
            self.watch();
        }

        /// Applies a new preference, skipping a reload when it did not change.
        pub fn follow(&self, preference: AppearancePreference) {
            if self.preference.get() != preference {
                self.apply(preference);
            }
        }

        /// Loads the sheet a preference selects, or clears the provider.
        pub fn apply(&self, preference: AppearancePreference) {
            self.preference.set(preference);
            let Some(provider) = self.provider.get() else {
                return;
            };
            let Some(directory) = self.directory.as_deref() else {
                provider.load_from_data("");
                return;
            };
            let stylesheet = user_stylesheet(directory);
            let Some(path) = stylesheet_file(&stylesheet, preference) else {
                provider.load_from_data("");
                return;
            };
            match read_stylesheet(path) {
                Ok(text) => provider.load_from_data(&text),
                Err(error) => {
                    provider.load_from_data("");
                    log::debug!("could not load the user style sheet: {error}");
                }
            }
        }

        fn watch(self: &Rc<Self>) {
            let Some(directory) = self.directory.clone() else {
                return;
            };
            self.monitor(&directory, true);
            let cosmic = directory.join("cosmic");
            if cosmic.is_dir() {
                self.monitor(&cosmic, false);
            }
        }

        fn monitor(self: &Rc<Self>, directory: &Path, filter: bool) {
            let monitor = match gio::File::for_path(directory)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            {
                Ok(monitor) => monitor,
                Err(_) => {
                    log::debug!("could not watch the user style sheet directory");
                    return;
                }
            };
            let weak = Rc::downgrade(self);
            monitor.connect_changed(move |_, file, other, _| {
                if filter && !is_stylesheet_change(Some(file)) && !is_stylesheet_change(other) {
                    return;
                }
                if let Some(styles) = weak.upgrade() {
                    styles.schedule();
                }
            });
            self.monitors.borrow_mut().push(monitor);
        }

        fn schedule(self: &Rc<Self>) {
            let generation = self.generation.get().wrapping_add(1);
            self.generation.set(generation);
            let weak = Rc::downgrade(self);
            glib::timeout_add_local_once(RELOAD_DELAY, move || {
                let Some(styles) = weak.upgrade() else {
                    return;
                };
                if styles.generation.get() == generation {
                    styles.apply(styles.preference.get());
                }
            });
        }
    }

    impl Default for AppearanceStyles {
        fn default() -> Self {
            Self::new()
        }
    }

    fn is_stylesheet_change(file: Option<&gio::File>) -> bool {
        file.is_some_and(|file| {
            file.basename()
                .is_some_and(|name| name == *"gtk.css" || name == *"cosmic")
        })
    }

    fn read_stylesheet(path: &Path) -> io::Result<String> {
        let file = fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(MAX_STYLESHEET_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_STYLESHEET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the style sheet is too large",
            ));
        }
        String::from_utf8(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the style sheet is not UTF-8"))
    }
}

#[cfg(feature = "gui")]
pub use styled::AppearanceStyles;

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink, path::Path};

    use tempfile::TempDir;

    use super::{Mode, UserStylesheet, stylesheet_file, user_stylesheet};
    use kestrel_core::AppearancePreference;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Expected {
        Absent,
        Cosmic(Option<Mode>),
        Custom,
    }

    fn write(path: &Path) {
        fs::create_dir_all(path.parent().expect("style sheet parent")).expect("style sheet folder");
        fs::write(path, "/* test */").expect("style sheet");
    }

    fn write_pair(directory: &Path) {
        write(&directory.join("cosmic").join("light.css"));
        write(&directory.join("cosmic").join("dark.css"));
    }

    fn classify(directory: &Path) -> Expected {
        match user_stylesheet(directory) {
            UserStylesheet::Absent => Expected::Absent,
            UserStylesheet::Custom => Expected::Custom,
            UserStylesheet::Cosmic { current, .. } => Expected::Cosmic(current),
        }
    }

    type Setup = fn(&Path);
    type Case = (&'static str, Setup, Expected);

    #[test]
    fn classifies_the_gtk_style_sheet_directory() {
        let cases: [Case; 8] = [
            ("nothing", |_| {}, Expected::Absent),
            (
                "cosmic linked to dark",
                |directory| {
                    write_pair(directory);
                    symlink("cosmic/dark.css", directory.join("gtk.css")).expect("dark link");
                },
                Expected::Cosmic(Some(Mode::Dark)),
            ),
            (
                "cosmic linked to light",
                |directory| {
                    write_pair(directory);
                    symlink("cosmic/light.css", directory.join("gtk.css")).expect("light link");
                },
                Expected::Cosmic(Some(Mode::Light)),
            ),
            (
                "cosmic without a link",
                |directory| {
                    write_pair(directory);
                },
                Expected::Cosmic(None),
            ),
            (
                "plain gtk.css",
                |directory| {
                    write(&directory.join("gtk.css"));
                },
                Expected::Custom,
            ),
            (
                "link elsewhere",
                |directory| {
                    write_pair(directory);
                    write(&directory.join("elsewhere.css"));
                    symlink("elsewhere.css", directory.join("gtk.css")).expect("other link");
                },
                Expected::Custom,
            ),
            (
                "half a cosmic pair with gtk.css",
                |directory| {
                    write(&directory.join("cosmic").join("light.css"));
                    write(&directory.join("gtk.css"));
                },
                Expected::Custom,
            ),
            (
                "half a cosmic pair alone",
                |directory| {
                    write(&directory.join("cosmic").join("light.css"));
                },
                Expected::Absent,
            ),
        ];
        for (name, setup, expected) in cases {
            let temp = TempDir::new().expect("temporary directory");
            let directory = temp.path().join("gtk-4.0");
            fs::create_dir_all(&directory).expect("gtk-4.0 directory");
            setup(&directory);
            assert_eq!(classify(&directory), expected, "{name}");
            if let UserStylesheet::Cosmic { light, dark, .. } = user_stylesheet(&directory) {
                assert_eq!(light, directory.join("cosmic").join("light.css"), "{name}");
                assert_eq!(dark, directory.join("cosmic").join("dark.css"), "{name}");
            }
        }
    }

    #[test]
    fn chooses_the_sheet_the_preference_names() {
        let light = std::path::PathBuf::from("/styles/light.css");
        let dark = std::path::PathBuf::from("/styles/dark.css");
        let cosmic = |current| UserStylesheet::Cosmic {
            light: light.clone(),
            dark: dark.clone(),
            current,
        };
        let cases = [
            (
                cosmic(Some(Mode::Light)),
                AppearancePreference::System,
                Some(light.as_path()),
            ),
            (
                cosmic(Some(Mode::Dark)),
                AppearancePreference::System,
                Some(dark.as_path()),
            ),
            (cosmic(None), AppearancePreference::System, None),
            (
                cosmic(None),
                AppearancePreference::Light,
                Some(light.as_path()),
            ),
            (
                cosmic(None),
                AppearancePreference::Dark,
                Some(dark.as_path()),
            ),
            (UserStylesheet::Absent, AppearancePreference::Light, None),
            (UserStylesheet::Custom, AppearancePreference::Dark, None),
        ];
        for (stylesheet, preference, expected) in cases {
            assert_eq!(
                stylesheet_file(&stylesheet, preference),
                expected,
                "{stylesheet:?} {preference:?}"
            );
        }
    }
}
