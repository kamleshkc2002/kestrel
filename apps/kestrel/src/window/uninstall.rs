//! The uninstall confirmation dialog and its per-path result report.

use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use async_channel::Sender;
use gtk::Orientation;
use kestrel::{ApplicationCommand, BinaryInstallation, UninstallItem, UninstallPreview, uninstall};

use super::widgets::plain_row;

/// Lists every path Kestrel owns for the user to choose, then forwards the
/// selection. Nothing is deleted here; the sizes are read on a worker.
pub(crate) fn open_uninstall(
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) {
    let (preview, receiver) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("kestrel-uninstall-preview".to_owned())
        .spawn(move || {
            let _ = preview.send_blocking(uninstall::preview());
        });
    if spawned.is_err() {
        log::warn!("kestrel-uninstall-preview could not start");
        show_unavailable(window);
        return;
    }
    let window = window.clone();
    let commands = commands.clone();
    gtk::glib::spawn_future_local(async move {
        let Ok(preview) = receiver.recv().await else {
            return;
        };
        match preview {
            Some(preview) => show_uninstall_dialog(&window, &preview, &commands),
            None => show_unavailable(&window),
        }
    });
}

fn show_unavailable(window: &adw::ApplicationWindow) {
    let dialog = adw::AlertDialog::builder()
        .heading("Kestrel's files could not be listed")
        .body("Set HOME and the XDG folders, then try again.")
        .build();
    dialog.add_response("close", "Close");
    dialog.present(Some(window));
}

fn show_uninstall_dialog(
    window: &adw::ApplicationWindow,
    preview: &UninstallPreview,
    commands: &Sender<ApplicationCommand>,
) {
    let dialog = adw::Dialog::builder()
        .title("Remove Kestrel's data")
        .content_width(600)
        .content_height(560)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("Kestrel's files")
        .description(
            "Kestrel's settings and data start selected; your recordings and the binary stay \
             unchecked unless you choose them. Deleting cannot be undone, and Kestrel quits \
             when it finishes. No step asks for root.",
        )
        .build();
    let checks = Rc::new(RefCell::new(Vec::<(UninstallItem, gtk::CheckButton)>::new()));
    for item in &preview.items {
        let present = item.path.symlink_metadata().is_ok();
        let check = gtk::CheckButton::new();
        check.set_valign(gtk::Align::Center);
        check.set_active(present && item.kind.starts_selected());
        check.set_sensitive(present);
        let subtitle = if present {
            format!(
                "{}  {}",
                item.path.display(),
                kestrel_core::format_size(item.size)
            )
        } else {
            format!("Not present: {}", item.path.display())
        };
        let row = adw::ActionRow::builder()
            .title(uninstall::item_label(item.kind))
            .subtitle(subtitle)
            .subtitle_lines(0)
            .build();
        row.add_prefix(&check);
        row.set_activatable_widget(Some(&check));
        group.add(&row);
        checks.borrow_mut().push((item.clone(), check));
    }
    page.add(&group);

    if let Some(note) = binary_note(preview) {
        let binary_group = adw::PreferencesGroup::builder().title("Binary").build();
        binary_group.add(
            &plain_row()
                .title(note.0)
                .subtitle(note.1)
                .subtitle_lines(0)
                .build(),
        );
        page.add(&binary_group);
    }
    toolbar.set_content(Some(&page));

    let footer = gtk::Box::new(Orientation::Horizontal, 6);
    footer.set_halign(gtk::Align::End);
    footer.set_margin_top(12);
    footer.set_margin_bottom(12);
    footer.set_margin_start(12);
    footer.set_margin_end(12);
    let cancel = gtk::Button::with_label("Cancel");
    let dialog_for_cancel = dialog.clone();
    cancel.connect_clicked(move |_| {
        dialog_for_cancel.close();
    });
    let remove = gtk::Button::with_label("Remove");
    remove.add_css_class("destructive-action");
    let dialog_for_remove = dialog.clone();
    let commands = commands.clone();
    let checks_for_remove = Rc::clone(&checks);
    remove.connect_clicked(move |_| {
        let selected = checks_for_remove
            .borrow()
            .iter()
            .filter(|(_, check)| check.is_active())
            .map(|(item, _)| item.clone())
            .collect::<Vec<_>>();
        let _ = commands.try_send(ApplicationCommand::Uninstall(selected));
        dialog_for_remove.close();
    });
    footer.append(&cancel);
    footer.append(&remove);
    toolbar.add_bottom_bar(&footer);
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(window));
}

/// What the dialog says when the binary is not one of the listed files.
fn binary_note(preview: &UninstallPreview) -> Option<(&'static str, String)> {
    match &preview.binary {
        BinaryInstallation::PackageManager => Some((
            "Installed by your package manager",
            "Remove the kestrel package there; Kestrel leaves this copy in place.".to_owned(),
        )),
        BinaryInstallation::Missing => Some((
            "Already removed",
            "The running binary could not be found, so there is nothing to delete.".to_owned(),
        )),
        BinaryInstallation::AppImage { .. }
        | BinaryInstallation::Local { .. }
        | BinaryInstallation::Executable { .. } => None,
    }
}

/// Reports each selected path, then quits when the report is closed.
pub(crate) fn present_uninstall_results(
    results: &[(UninstallItem, Option<String>)],
    parent: Option<&gtk::Window>,
    commands: &Sender<ApplicationCommand>,
) {
    let removed = results.iter().filter(|(_, error)| error.is_none()).count();
    let failed = results.len() - removed;
    let heading = if failed == 0 {
        "Kestrel's files were removed"
    } else {
        "Kestrel could not remove every file"
    };
    let dialog = adw::Dialog::builder()
        .title(heading)
        .content_width(600)
        .content_height(480)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("Results")
        .description(format!(
            "{removed} removed, {failed} still there. Kestrel quits when this closes."
        ))
        .build();
    for (item, error) in results {
        let subtitle = match error {
            None => format!("Removed {}", item.path.display()),
            Some(error) => format!("{error}. Remove {} by hand.", item.path.display()),
        };
        group.add(
            &plain_row()
                .title(uninstall::item_label(item.kind))
                .subtitle(subtitle)
                .subtitle_lines(0)
                .build(),
        );
    }
    page.add(&group);
    toolbar.set_content(Some(&page));
    let close = gtk::Button::with_label("Quit");
    close.add_css_class("suggested-action");
    let commands = commands.clone();
    let commands_for_close = commands.clone();
    close.connect_clicked(move |_| {
        let _ = commands.try_send(ApplicationCommand::Quit);
    });
    let footer = gtk::Box::new(Orientation::Horizontal, 6);
    footer.set_halign(gtk::Align::End);
    footer.set_margin_top(12);
    footer.set_margin_bottom(12);
    footer.set_margin_start(12);
    footer.set_margin_end(12);
    footer.append(&close);
    toolbar.add_bottom_bar(&footer);
    dialog.connect_closed(move |_| {
        let _ = commands_for_close.try_send(ApplicationCommand::Quit);
    });
    dialog.set_child(Some(&toolbar));
    match parent {
        Some(parent) => dialog.present(Some(parent)),
        None => dialog.present(None::<&gtk::Window>),
    }
}
