//! Hand-off of package and disk-image rows to the desktop's own app.

use super::widgets::{icon_button, plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, gio};
use kestrel::ApplicationCommand;
use kestrel::view_model::ShelfItemViewModel;
use kestrel_core::ShelfContent;
use kestrel_platform::install_handoff::{self, InstallTarget, InstallTargetKind, Provider};

const SUPPORTED_MIME_TYPES: [&str; 10] = [
    "application/vnd.debian.binary-package",
    "application/x-rpm",
    "application/vnd.flatpak.ref",
    "application/vnd.flatpak",
    "application/vnd.snap",
    "application/vnd.efi.iso",
    "application/vnd.efi.img",
    "application/x-cd-image",
    "application/x-iso9660-image",
    "application/x-raw-disk-image",
];

const SUPPORTED_PATTERNS: [&str; 7] = [
    "*.deb",
    "*.rpm",
    "*.flatpakref",
    "*.flatpak",
    "*.snap",
    "*.iso",
    "*.img",
];

/// Adds the hand-off button to a file row whose name promises a supported type.
pub(super) fn add_handoff_button(
    row: &adw::ActionRow,
    item: &ShelfItemViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    let Some(kind) = item.install else {
        return;
    };
    let ShelfContent::File { path, .. } = &item.content else {
        return;
    };
    let action = if kind.opens() { "Open" } else { "Install" };
    let tooltip = match provider_for_path(path) {
        Some(provider) => format!("{action} with {}", provider.name),
        None => format!("{action} with the default app"),
    };
    let button = icon_button(
        "system-software-install-symbolic",
        &tooltip,
        item.available && !item.directory,
    );
    let sender = commands.clone();
    let path = path.clone();
    button.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::InstallHandoff { path: path.clone() });
    });
    row.add_suffix(&button);
}

/// The app the desktop uses for this row's type, without reading the file.
fn provider_for_path(path: &std::path::Path) -> Option<Provider> {
    let (content_type, _) = gio::content_type_guess(Some(path), None::<&[u8]>);
    install_handoff::provider_for(&content_type)
}

/// The command bar row that opens the hand-off file chooser.
pub(super) fn install_entry_row(commands: &Sender<ApplicationCommand>) -> adw::ActionRow {
    let row = plain_row()
        .title("Install a package or open a disk image…")
        .subtitle(
            "Hands the file to the app that handles its type. A type with no app says so \
             instead of installing anything.",
        )
        .subtitle_lines(0)
        .build();
    let choose = gtk::Button::with_label("Choose File…");
    choose.set_valign(Align::Center);
    let commands = commands.clone();
    choose.connect_clicked(move |button| {
        let Some(window) = button.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("Choose a package or disk image")
            .accept_label("Choose")
            .modal(true)
            .build();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Packages and disk images"));
        for content_type in SUPPORTED_MIME_TYPES {
            filter.add_mime_type(content_type);
        }
        for pattern in SUPPORTED_PATTERNS {
            filter.add_pattern(pattern);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));
        let commands = commands.clone();
        dialog.open(Some(&window), gio::Cancellable::NONE, move |result| {
            if let Some(path) = result.ok().and_then(|file| file.path()) {
                let _ = commands.try_send(ApplicationCommand::InstallHandoff { path });
            }
        });
    });
    row.add_suffix(&choose);
    row
}

/// Names the file, its type, and the app before it takes over.
pub(crate) fn present_handoff(
    target: &InstallTarget,
    provider: Option<&Provider>,
    parent: Option<&gtk::Window>,
    commands: &Sender<ApplicationCommand>,
) {
    let Some(provider) = provider else {
        present_missing_provider(target.kind, parent);
        return;
    };
    let mut body = format!(
        "{} · {}\nOpens with {}.",
        target.display_name(),
        target.kind.label(),
        provider.name
    );
    if provider.archive_manager {
        body.push_str(
            " The default app is an archive manager, so it may list the package's contents \
             instead of installing it.",
        );
    }
    body.push_str(
        "\n\nThe app takes over from here and may ask for your password. The file is not changed.",
    );
    let dialog = adw::AlertDialog::builder()
        .heading(if target.kind.opens() {
            format!("Open {}?", target.display_name())
        } else {
            format!("Install {}?", target.display_name())
        })
        .body(body)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("open", &format!("Open with {}", provider.name));
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let commands = commands.clone();
    let target = target.clone();
    let provider = provider.clone();
    dialog.connect_response(None, move |dialog, response| {
        dialog.close();
        if response == "open" {
            let _ = commands.try_send(ApplicationCommand::ConfirmInstallHandoff {
                target: target.clone(),
                provider: provider.clone(),
            });
        }
    });
    dialog.present(parent);
}

/// A type with no registered app reports it and claims nothing.
fn present_missing_provider(kind: InstallTargetKind, parent: Option<&gtk::Window>) {
    let dialog = adw::AlertDialog::builder()
        .heading(format!("No app for {} files", kind.label()))
        .body(format!(
            "No app on this computer handles {} files. Install your distribution's software \
             center or an app for this file type.",
            kind.label()
        ))
        .build();
    dialog.add_response("close", "Close");
    dialog.set_close_response("close");
    dialog.present(parent);
}
