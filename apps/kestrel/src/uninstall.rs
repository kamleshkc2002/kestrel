//! User-facing wording for the uninstall preview and its results.

use kestrel_core::format_size;
use kestrel_platform::uninstall::{
    BinaryInstallation, Uninstall, UninstallItem, UninstallItemKind, UninstallPreview,
};

/// This build's removable paths, resolved from the current environment.
pub fn preview() -> Option<UninstallPreview> {
    Uninstall::from_environment().map(|uninstall| uninstall.preview())
}

pub fn item_label(kind: UninstallItemKind) -> &'static str {
    match kind {
        UninstallItemKind::Configuration => "Settings",
        UninstallItemKind::Data => "Saved data",
        UninstallItemKind::State => "State and logs",
        UninstallItemKind::Cache => "Cache",
        UninstallItemKind::Recordings => "Your recordings (kept unless selected)",
        UninstallItemKind::Autostart => "Autostart entry",
        UninstallItemKind::DesktopEntry => "Desktop entry",
        UninstallItemKind::Icon => "Application icon",
        UninstallItemKind::LauncherScripts => "Launcher commands",
        UninstallItemKind::AppImage => "AppImage",
        UninstallItemKind::PreviousAppImage => "Previous AppImage",
        UninstallItemKind::LocalBinary => "Kestrel binary",
        UninstallItemKind::Executable => "Running executable",
    }
}

/// The `--uninstall-preview` list: one selected-or-not line per path, then
/// what the binary needs. Nothing is changed.
pub fn preview_text() -> Result<String, &'static str> {
    let preview = preview().ok_or("cannot resolve the user's folders; set HOME")?;
    Ok(format_preview(&preview))
}

fn format_preview(preview: &UninstallPreview) -> String {
    let mut text = String::from("Kestrel's files\n\n");
    for item in &preview.items {
        let selected = if item.kind.starts_selected() {
            "x"
        } else {
            " "
        };
        text.push_str(&format!(
            "  [{selected}] {:<20} {}  {}\n",
            item_label(item.kind),
            display_path(item),
            display_size(item),
        ));
    }
    text.push('\n');
    match &preview.binary {
        BinaryInstallation::PackageManager => text.push_str(
            "The installed kestrel package owns this binary; remove the package with your \
             package manager.\n",
        ),
        BinaryInstallation::Missing => {
            text.push_str("The running binary could not be found; it was already removed.\n");
        }
        BinaryInstallation::AppImage { .. }
        | BinaryInstallation::Local { .. }
        | BinaryInstallation::Executable { .. } => {}
    }
    text.push_str(
        "Your recordings, and the binary when it is one of the listed files, are unchecked by \
         default.\n",
    );
    text
}

fn display_path(item: &UninstallItem) -> String {
    if item.path.symlink_metadata().is_ok() {
        item.path.display().to_string()
    } else {
        format!("{} (not present)", item.path.display())
    }
}

fn display_size(item: &UninstallItem) -> String {
    format_size(item.size)
}
