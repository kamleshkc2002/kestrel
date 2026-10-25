//! The dialog listing this release's notes.

use adw::prelude::*;
use gtk::glib;
use kestrel::WhatsNewViewModel;

pub(super) fn open_whats_new(window: &adw::ApplicationWindow, notes: &WhatsNewViewModel) {
    let dialog = adw::Window::builder()
        .title(format!("What's new in Kestrel {}", notes.version))
        .modal(true)
        .transient_for(window)
        .default_width(560)
        .default_height(560)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let page = adw::PreferencesPage::new();
    let (in_use, others): (Vec<_>, Vec<_>) =
        notes.sections.iter().partition(|section| section.in_use);
    for section in in_use {
        let group = adw::PreferencesGroup::builder()
            .title(glib::markup_escape_text(&section.title).as_str())
            .build();
        for change in &section.changes {
            group.add(&change_row(change));
        }
        page.add(&group);
    }
    if !others.is_empty() {
        let group = adw::PreferencesGroup::builder()
            .title("Other changes")
            .description("In features you have off.")
            .build();
        for section in others {
            let expander = adw::ExpanderRow::builder()
                .title(glib::markup_escape_text(&section.title).as_str())
                .build();
            for change in &section.changes {
                expander.add_row(&change_row(change));
            }
            group.add(&expander);
        }
        page.add(&group);
    }
    if notes.sections.is_empty() {
        let group = adw::PreferencesGroup::builder()
            .description("This release has no notes.")
            .build();
        page.add(&group);
    }
    toolbar.set_content(Some(&page));
    dialog.set_content(Some(&toolbar));
    dialog.present();
}

fn change_row(text: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(glib::markup_escape_text(text).as_str())
        .title_lines(0)
        .build()
}
