//! Clipboard, snippet, scratchpad, and command-bar settings.

use crate::window::widgets::{on_spin_row_settled, plain_row, stacked_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, accessible::Property};
use kestrel::{ApplicationCommand, ApplicationViewModel};

pub(super) fn add_clipboard_limits_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let clipboard = &view_model.clipboard;
    let clipboard_group = adw::PreferencesGroup::builder()
        .title("Clipboard settings")
        .description(
            "Memory-only retention bounds. Lock, sleep, service stop, wipe, and shutdown always \
             drop retained entries regardless of these values.",
        )
        .build();

    let bounds = clipboard.policy.bounds;
    let push_limit = |row: &adw::SpinRow, limit: kestrel::ClipboardLimit, description: &str| {
        let sender = commands.clone();
        let description = description.to_owned();
        row.update_property(&[
            Property::Label(&description),
            Property::Description(&description),
        ]);
        row.set_tooltip_text(Some(&description));
        on_spin_row_settled(row, move |value| {
            // Bounds use kibibytes in the UI and bytes in storage.
            let value = value.max(0.0);
            let command = match limit {
                kestrel::ClipboardLimit::Items(_) => kestrel::ClipboardLimit::Items(value as u32),
                kestrel::ClipboardLimit::ItemBytes(_) => {
                    kestrel::ClipboardLimit::ItemBytes(value as u32 * 1024)
                }
                kestrel::ClipboardLimit::ImageBytes(_) => {
                    kestrel::ClipboardLimit::ImageBytes(value as u32 * 1024)
                }
                kestrel::ClipboardLimit::FileEntries(_) => {
                    kestrel::ClipboardLimit::FileEntries(value as u32)
                }
                kestrel::ClipboardLimit::MaxAgeHours(_) => {
                    kestrel::ClipboardLimit::MaxAgeHours(value as u32)
                }
                kestrel::ClipboardLimit::ClearSeconds(_) => {
                    kestrel::ClipboardLimit::ClearSeconds(value as u64)
                }
                kestrel::ClipboardLimit::PinnedMaxAgeDays(_) => {
                    kestrel::ClipboardLimit::PinnedMaxAgeDays(value as u32)
                }
            };
            let _ = sender.try_send(ApplicationCommand::SetClipboardLimit(command));
        });
    };

    let items = adw::SpinRow::with_range(1.0, f64::from(bounds.max_items), 1.0);
    items.set_title("Retained entries");
    items.set_subtitle("How many entries the bounded history keeps.");
    items.set_value(f64::from(clipboard.policy.max_items));
    items.set_numeric(true);
    push_limit(
        &items,
        kestrel::ClipboardLimit::Items(clipboard.policy.max_items),
        "Clipboard retained entries",
    );
    clipboard_group.add(&items);

    let item_kib = clipboard.policy.max_item_bytes / 1024;
    let item_bytes = adw::SpinRow::with_range(
        f64::from(bounds.min_item_bytes / 1024),
        f64::from(bounds.max_item_bytes / 1024),
        1.0,
    );
    item_bytes.set_title("Text entry bound");
    item_bytes.set_subtitle("Largest retained text entry, in kibibytes.");
    item_bytes.set_value(f64::from(item_kib));
    item_bytes.set_numeric(true);
    push_limit(
        &item_bytes,
        kestrel::ClipboardLimit::ItemBytes(clipboard.policy.max_item_bytes),
        "Clipboard text entry byte bound",
    );
    clipboard_group.add(&item_bytes);

    let image_kib = clipboard.policy.max_image_bytes / 1024;
    let image_bytes = adw::SpinRow::with_range(
        f64::from(bounds.min_item_bytes / 1024),
        f64::from(bounds.max_image_bytes / 1024),
        1.0,
    );
    image_bytes.set_title("Image entry bound");
    image_bytes.set_subtitle("Largest retained PNG payload, in kibibytes.");
    image_bytes.set_value(f64::from(image_kib));
    image_bytes.set_numeric(true);
    push_limit(
        &image_bytes,
        kestrel::ClipboardLimit::ImageBytes(clipboard.policy.max_image_bytes),
        "Clipboard image entry byte bound",
    );
    clipboard_group.add(&image_bytes);

    let files = adw::SpinRow::with_range(1.0, f64::from(bounds.max_file_entries), 1.0);
    files.set_title("File paths per entry");
    files.set_subtitle("Largest retained file list.");
    files.set_value(f64::from(clipboard.policy.max_file_entries));
    files.set_numeric(true);
    push_limit(
        &files,
        kestrel::ClipboardLimit::FileEntries(clipboard.policy.max_file_entries),
        "Clipboard file path bound",
    );
    clipboard_group.add(&files);

    let age = adw::SpinRow::with_range(1.0, f64::from(bounds.max_age_hours), 1.0);
    age.set_title("Retention age");
    age.set_subtitle("How long an entry stays retained, in hours.");
    age.set_value(f64::from(clipboard.policy.max_age_hours));
    age.set_numeric(true);
    push_limit(
        &age,
        kestrel::ClipboardLimit::MaxAgeHours(clipboard.policy.max_age_hours),
        "Clipboard retention age in hours",
    );
    clipboard_group.add(&age);

    let clear = adw::SpinRow::with_range(0.0, bounds.max_clear_seconds as f64, 5.0);
    clear.set_title("Automatic selection clear");
    clear.set_subtitle(
        "Seconds after the selection is taken before the live clipboard is cleared. \
         Saved entries are kept; 0 disables it.",
    );
    clear.set_value(clipboard.policy.clear_seconds as f64);
    clear.set_numeric(true);
    push_limit(
        &clear,
        kestrel::ClipboardLimit::ClearSeconds(clipboard.policy.clear_seconds),
        "Clipboard automatic selection clear interval",
    );
    clipboard_group.add(&clear);

    let pin_age = adw::SpinRow::with_range(1.0, f64::from(bounds.max_pinned_age_days), 1.0);
    pin_age.set_title("Saved-pin age");
    pin_age.set_subtitle(
        "Days a pinned text entry is kept on disk when keeping pins across restarts is on.",
    );
    pin_age.set_value(f64::from(clipboard.policy.pinned_max_age_days));
    pin_age.set_numeric(true);
    push_limit(
        &pin_age,
        kestrel::ClipboardLimit::PinnedMaxAgeDays(clipboard.policy.pinned_max_age_days),
        "Clipboard saved-pin age in days",
    );
    clipboard_group.add(&pin_age);

    let keep = plain_row()
        .title("Keep pinned entries across restarts")
        .subtitle(
            "Pinned text entries are encrypted with a key in your desktop keyring and written \
             to the data folder. Images and file pins stay in memory only.",
        )
        .subtitle_lines(0)
        .build();
    let keep_switch = gtk::Switch::builder()
        .active(clipboard.pin_store.keep_pinned)
        .valign(Align::Center)
        .build();
    keep_switch.update_property(&[
        Property::Label("Keep pinned clipboard entries across restarts"),
        Property::Description(
            "Save pinned text entries in an encrypted file whose key is in the desktop keyring",
        ),
    ]);
    let sender = commands.clone();
    let pin_store = clipboard.pin_store.clone();
    keep_switch.connect_state_set(move |switch, pinned| {
        if pinned {
            let _ = sender.try_send(ApplicationCommand::SetClipboardKeepPinned {
                keep_pinned: true,
                delete_saved: false,
            });
            return gtk::glib::Propagation::Proceed;
        }
        ask_delete_saved_pins(switch, &sender, &pin_store);
        gtk::glib::Propagation::Stop
    });
    keep.add_suffix(&keep_switch);
    keep.set_activatable_widget(Some(&keep_switch));
    clipboard_group.add(&keep);

    let saved = plain_row()
        .title("Saved pins")
        .subtitle(&clipboard.pin_store.status)
        .subtitle_lines(0)
        .sensitive(false)
        .build();
    saved.update_property(&[Property::Label("Saved pinned clipboard entries")]);
    clipboard_group.add(&saved);

    if let Some(remedy) = &clipboard.pin_store.remedy {
        let remedy_row = plain_row()
            .title("Saved pins need attention")
            .subtitle(remedy)
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        remedy_row.update_property(&[Property::Label("Saved pins need attention")]);
        clipboard_group.add(&remedy_row);
    }

    let pin_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    pin_actions.set_valign(Align::Center);
    let delete_pins = gtk::Button::with_label("Delete saved pins…");
    delete_pins.set_valign(Align::Center);
    delete_pins.set_sensitive(clipboard.pin_store.can_delete);
    delete_pins.set_tooltip_text(Some(
        "Remove the encrypted saved pins file and its key from the desktop keyring",
    ));
    delete_pins.update_property(&[Property::Label("Delete saved pins")]);
    let sender = commands.clone();
    let pin_store = clipboard.pin_store.clone();
    delete_pins.connect_clicked(move |button| {
        confirm_delete_saved_pins(button, &sender, &pin_store);
    });
    pin_actions.append(&delete_pins);
    if clipboard.pin_store.needs_reset {
        let reset = gtk::Button::with_label("Reset saved pins…");
        reset.set_valign(Align::Center);
        reset.add_css_class("destructive-action");
        reset.update_property(&[Property::Label("Reset saved pins")]);
        let sender = commands.clone();
        reset.connect_clicked(move |button| {
            ask_reset_saved_pins(button, &sender);
        });
        pin_actions.append(&reset);
    }
    let pin_actions_row = stacked_row(
        "Saved-pin store",
        "Deleting the saved pins never removes pinned entries from this session.",
        &pin_actions,
    );
    clipboard_group.add(&pin_actions_row);

    let filter = plain_row()
        .title("Filter sensitive patterns")
        .subtitle(
            "Skip capturing content that looks like a secret. Heuristics produce false \
             positives, so this is off by default.",
        )
        .subtitle_lines(0)
        .build();
    let filter_switch = gtk::Switch::builder()
        .active(clipboard.policy.filter_sensitive)
        .valign(Align::Center)
        .build();
    filter_switch.update_property(&[
        Property::Label("Filter sensitive clipboard patterns"),
        Property::Description(
            "Skip capturing text that matches a documented sensitive-content pattern",
        ),
    ]);
    let sender = commands.clone();
    filter_switch.connect_state_set(move |_, filter| {
        let _ = sender.try_send(ApplicationCommand::SetClipboardFilterSensitive(filter));
        gtk::glib::Propagation::Proceed
    });
    filter.add_suffix(&filter_switch);
    filter.set_activatable_widget(Some(&filter_switch));
    clipboard_group.add(&filter);

    let patterns = plain_row()
        .title("Documented sensitive patterns")
        .subtitle(
            kestrel_services::clipboard::SENSITIVE_PATTERNS
                .iter()
                .map(|pattern| pattern.description)
                .collect::<Vec<_>>()
                .join("; "),
        )
        .subtitle_lines(0)
        .sensitive(false)
        .build();
    patterns.update_property(&[Property::Label("Documented sensitive patterns")]);
    clipboard_group.add(&patterns);

    let paste = plain_row()
        .title("Quick paste as plain text")
        .subtitle(
            "Copying an entry strips ANSI escapes and trailing whitespace; file entries copy \
             as their paths.",
        )
        .subtitle_lines(0)
        .build();
    let paste_switch = gtk::Switch::builder()
        .active(clipboard.policy.paste_plain_text)
        .valign(Align::Center)
        .build();
    paste_switch.update_property(&[
        Property::Label("Quick paste as plain text"),
        Property::Description("Copy the plain-text form of an entry"),
    ]);
    let sender = commands.clone();
    paste_switch.connect_state_set(move |_, plain| {
        let _ = sender.try_send(ApplicationCommand::SetClipboardPastePlainText(plain));
        gtk::glib::Propagation::Proceed
    });
    paste.add_suffix(&paste_switch);
    paste.set_activatable_widget(Some(&paste_switch));
    clipboard_group.add(&paste);

    let layouts = kestrel::ClipboardLayout::ALL;
    let layout = adw::ComboRow::builder()
        .title("History layout")
        .subtitle("A resizable list, or a row of cards.")
        .model(&gtk::StringList::new(
            &layouts
                .iter()
                .map(|layout| layout.label())
                .collect::<Vec<_>>(),
        ))
        .selected(
            layouts
                .iter()
                .position(|layout| *layout == clipboard.layout)
                .unwrap_or(0) as u32,
        )
        .build();
    let sender = commands.clone();
    layout.connect_selected_notify(move |row| {
        if let Some(layout) = layouts.get(row.selected() as usize) {
            let _ = sender.try_send(ApplicationCommand::SetClipboardLayout(*layout));
        }
    });
    clipboard_group.add(&layout);

    let combinations = kestrel::ClipboardPasteKeys::ALL;
    let paste_keys = adw::ComboRow::builder()
        .title("Paste keystroke")
        .subtitle(
            "The keystroke sent after the window hides, so the previous application pastes; \
             the entry itself is never sent.",
        )
        .model(&gtk::StringList::new(
            &combinations
                .iter()
                .map(|keys| keys.label())
                .collect::<Vec<_>>(),
        ))
        .selected(
            combinations
                .iter()
                .position(|keys| *keys == clipboard.policy.paste_keys)
                .unwrap_or(0) as u32,
        )
        .build();
    let sender = commands.clone();
    paste_keys.connect_selected_notify(move |row| {
        if let Some(keys) = combinations.get(row.selected() as usize) {
            let _ = sender.try_send(ApplicationCommand::SetClipboardPasteKeys(*keys));
        }
    });
    clipboard_group.add(&paste_keys);

    for row in [
        items.upcast_ref::<gtk::Widget>().clone(),
        item_bytes.upcast_ref::<gtk::Widget>().clone(),
        image_bytes.upcast_ref::<gtk::Widget>().clone(),
        files.upcast_ref::<gtk::Widget>().clone(),
        age.upcast_ref::<gtk::Widget>().clone(),
        clear.upcast_ref::<gtk::Widget>().clone(),
        pin_age.upcast_ref::<gtk::Widget>().clone(),
        keep.clone().upcast(),
        saved.clone().upcast(),
        pin_actions_row.clone().upcast(),
        filter.clone().upcast(),
        paste.clone().upcast(),
    ] {
        filter_rows.push((
            row,
            "clipboard history retention bounds sensitive plain text".to_owned(),
        ));
    }
    filter_rows.push((
        layout.upcast(),
        "clipboard history layout list cards".to_owned(),
    ));
    group.add(&clipboard_group);
}

/// Turning the second opt-in off asks about the store; deleting is the default.
fn ask_delete_saved_pins(
    switch: &gtk::Switch,
    commands: &Sender<ApplicationCommand>,
    pin_store: &kestrel::ClipboardPinStoreViewModel,
) {
    let Some(window) = switch.root().and_downcast::<gtk::Window>() else {
        let _ = commands.try_send(ApplicationCommand::SetClipboardKeepPinned {
            keep_pinned: false,
            delete_saved: false,
        });
        return;
    };
    let dialog = adw::MessageDialog::builder()
        .heading("Keep pinned entries across restarts?")
        .body(format!(
            "Turning this off keeps pinned entries in memory only. {} saved pins on disk can be \
             deleted now, or kept for when you turn this back on.",
            pin_store.saved_count
        ))
        .build();
    dialog.set_transient_for(Some(&window));
    dialog.add_responses(&[("keep", "Keep saved pins"), ("delete", "Delete saved pins")]);
    dialog.set_close_response("keep");
    dialog.set_default_response(Some("delete"));
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let commands = commands.clone();
    dialog.connect_response(None, move |dialog, response| {
        let _ = commands.try_send(ApplicationCommand::SetClipboardKeepPinned {
            keep_pinned: false,
            delete_saved: response == "delete",
        });
        dialog.close();
    });
    dialog.present();
}

/// Confirms the count and byte estimate before removing the store and its key.
fn confirm_delete_saved_pins(
    button: &gtk::Button,
    commands: &Sender<ApplicationCommand>,
    pin_store: &kestrel::ClipboardPinStoreViewModel,
) {
    let Some(window) = button.root().and_downcast::<gtk::Window>() else {
        return;
    };
    let estimate = kestrel_core::format_bytes(pin_store.saved_bytes as f64);
    let dialog = adw::MessageDialog::builder()
        .heading("Delete saved pins?")
        .body(format!(
            "{} saved pins, about {estimate}, will be removed from the encrypted file and the \
             desktop keyring. Pinned entries stay in this session, and this cannot be undone.",
            pin_store.saved_count
        ))
        .build();
    dialog.set_transient_for(Some(&window));
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete saved pins")]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let commands = commands.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "delete" {
            let _ = commands.try_send(ApplicationCommand::WipeSavedPins);
        }
        dialog.close();
    });
    dialog.present();
}

/// Offers Reset or Keep after an unreadable or re-keyed store.
fn ask_reset_saved_pins(button: &gtk::Button, commands: &Sender<ApplicationCommand>) {
    let Some(window) = button.root().and_downcast::<gtk::Window>() else {
        return;
    };
    let dialog = adw::MessageDialog::builder()
        .heading("Saved pins could not be read")
        .body(
            "Reset deletes the unreadable file and its keyring item, so pinning starts fresh. \
             Keep leaves the file in place to try again later.",
        )
        .build();
    dialog.set_transient_for(Some(&window));
    dialog.add_responses(&[("keep", "Keep"), ("reset", "Reset saved pins")]);
    dialog.set_close_response("keep");
    dialog.set_default_response(Some("keep"));
    dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
    let commands = commands.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "reset" {
            let _ = commands.try_send(ApplicationCommand::ResetSavedPins);
        }
        dialog.close();
    });
    dialog.present();
}

pub(super) fn add_snippet_limits_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let snippets = &view_model.snippets;
    let snippet_group = adw::PreferencesGroup::builder()
        .title("Snippet settings")
        .description(
            "Library bounds and the insertion provider. Snippets are stored in a private file; \
             clipboard variables are read once at insert time and never stored.",
        )
        .build();

    let snippet_content_kib = snippets.policy.max_content_bytes / 1024;
    let content = adw::SpinRow::with_range(
        f64::from(snippets.policy.bounds.min_content_bytes / 1024),
        f64::from(snippets.policy.bounds.max_content_bytes / 1024),
        1.0,
    );
    content.set_title("Snippet size bound");
    content.set_subtitle("Largest stored snippet, in kibibytes.");
    content.set_value(f64::from(snippet_content_kib));
    content.set_numeric(true);
    let sender = commands.clone();
    on_spin_row_settled(&content, move |value| {
        let bytes = (value.max(0.0) as u32).saturating_mul(1024);
        let _ = sender.try_send(ApplicationCommand::SetSnippetLimit(
            kestrel::SnippetLimit::ContentBytes(bytes),
        ));
    });
    snippet_group.add(&content);

    let clipboard_variable = adw::SpinRow::with_range(
        f64::from(snippets.policy.bounds.min_clipboard_bytes),
        f64::from(snippets.policy.bounds.max_clipboard_bytes),
        64.0,
    );
    clipboard_variable.set_title("Clipboard variable bound");
    clipboard_variable
        .set_subtitle("How much of the live selection a {{clipboard}} variable may insert.");
    clipboard_variable.set_value(f64::from(snippets.policy.clipboard_variable_bytes));
    clipboard_variable.set_numeric(true);
    let sender = commands.clone();
    on_spin_row_settled(&clipboard_variable, move |value| {
        let bytes = value.max(0.0) as u32;
        let _ = sender.try_send(ApplicationCommand::SetSnippetLimit(
            kestrel::SnippetLimit::ClipboardBytes(bytes),
        ));
    });
    snippet_group.add(&clipboard_variable);

    let timeout = adw::SpinRow::with_range(
        snippets.policy.bounds.min_insert_timeout_millis as f64,
        snippets.policy.bounds.max_insert_timeout_millis as f64,
        250.0,
    );
    timeout.set_title("Insertion timeout");
    timeout.set_subtitle("How long one provider run may take, in milliseconds.");
    timeout.set_value(snippets.policy.insert_timeout_millis as f64);
    timeout.set_numeric(true);
    let sender = commands.clone();
    on_spin_row_settled(&timeout, move |value| {
        let millis = value.max(0.0) as u64;
        let _ = sender.try_send(ApplicationCommand::SetSnippetLimit(
            kestrel::SnippetLimit::InsertTimeoutMillis(millis),
        ));
    });
    snippet_group.add(&timeout);

    let provider_labels = kestrel::SnippetProviderPreference::ALL
        .iter()
        .map(|preference| preference.label())
        .collect::<Vec<_>>();
    let provider = adw::ComboRow::builder()
        .title("Insertion provider")
        .subtitle("Automatic selection never requests input-device access.")
        .model(&gtk::StringList::new(&provider_labels))
        .selected(
            kestrel::SnippetProviderPreference::ALL
                .iter()
                .position(|preference| *preference == snippets.policy.preferred_provider)
                .unwrap_or(0) as u32,
        )
        .build();
    let sender = commands.clone();
    provider.connect_selected_notify(move |row| {
        let preference = kestrel::SnippetProviderPreference::ALL
            .get(row.selected() as usize)
            .copied()
            .unwrap_or(kestrel::SnippetProviderPreference::Auto);
        let _ = sender.try_send(ApplicationCommand::SetSnippetProvider(preference));
    });
    snippet_group.add(&provider);

    let timing_labels = kestrel::SnippetExpansionTiming::ALL
        .iter()
        .map(|timing| timing.label())
        .collect::<Vec<_>>();
    let timing = adw::ComboRow::builder()
        .title("Expansion timing")
        .subtitle(
            "Delimiter expansion needs a key-capture provider, which is not available yet; \
             manual insertion always works when a provider exists.",
        )
        .subtitle_lines(0)
        .model(&gtk::StringList::new(&timing_labels))
        .selected(
            kestrel::SnippetExpansionTiming::ALL
                .iter()
                .position(|candidate| *candidate == snippets.policy.expansion_timing)
                .unwrap_or(0) as u32,
        )
        .build();
    let sender = commands.clone();
    timing.connect_selected_notify(move |row| {
        let timing = kestrel::SnippetExpansionTiming::ALL
            .get(row.selected() as usize)
            .copied()
            .unwrap_or(kestrel::SnippetExpansionTiming::Manual);
        let _ = sender.try_send(ApplicationCommand::SetSnippetExpansionTiming(timing));
    });
    snippet_group.add(&timing);

    for row in [
        content.upcast_ref::<gtk::Widget>().clone(),
        clipboard_variable.upcast_ref::<gtk::Widget>().clone(),
        timeout.upcast_ref::<gtk::Widget>().clone(),
        provider.clone().upcast(),
        timing.clone().upcast(),
    ] {
        filter_rows.push((
            row,
            "snippets text library bounds provider insertion".to_owned(),
        ));
    }
    group.add(&snippet_group);
}

pub(super) fn add_command_bar_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let command_bar = &view_model.command_bar;
    let command_group = adw::PreferencesGroup::builder()
        .title("Command bar settings")
        .description(
            "How many ranked results the bar shows. Provider switches and the learned ranking \
             live in the command bar panel; file roots and scripts are configured in the \
             configuration file.",
        )
        .build();
    let results = adw::SpinRow::with_range(
        f64::from(kestrel_core::MIN_COMMAND_RESULTS),
        f64::from(kestrel_core::MAX_COMMAND_RESULTS),
        1.0,
    );
    results.set_title("Result count");
    results.set_subtitle("Largest number of ranked results the bar shows.");
    results.set_value(f64::from(command_bar.max_results));
    results.set_numeric(true);
    let sender = commands.clone();
    on_spin_row_settled(&results, move |value| {
        let value = value.max(0.0) as u32;
        let _ = sender.try_send(ApplicationCommand::SetCommandResultLimit(value));
    });
    command_group.add(&results);
    filter_rows.push((
        results.clone().upcast(),
        "command bar results limit launcher".to_owned(),
    ));
    group.add(&command_group);
}

pub(super) fn add_scratchpad_limits_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let limits = view_model.scratchpad.limits;
    let scratchpad_group = adw::PreferencesGroup::builder()
        .title("Scratchpad settings")
        .description("Notes are saved privately on this computer and never exported.")
        .build();
    for (title, subtitle, (min, max), value, limit) in [
        (
            "Notes",
            "How many notes the scratchpad keeps.",
            (
                kestrel_core::MIN_SCRATCHPAD_MAX_NOTES,
                kestrel_core::MAX_SCRATCHPAD_MAX_NOTES,
            ),
            limits.max_notes,
            kestrel::ScratchpadLimit::MaxNotes as fn(u32) -> kestrel::ScratchpadLimit,
        ),
        (
            "Note size",
            "Largest note, in KiB.",
            (
                kestrel_core::MIN_SCRATCHPAD_NOTE_BYTES / 1024,
                kestrel_core::MAX_SCRATCHPAD_NOTE_BYTES / 1024,
            ),
            limits.max_note_bytes / 1024,
            |kib| kestrel::ScratchpadLimit::MaxNoteBytes(kib * 1024),
        ),
        (
            "Age limit",
            "Days before an unedited note is offered for removal; 0 keeps notes.",
            (0, kestrel_core::MAX_SCRATCHPAD_AGE_DAYS),
            limits.max_age_days,
            kestrel::ScratchpadLimit::MaxAgeDays,
        ),
    ] {
        let row = adw::SpinRow::with_range(f64::from(min), f64::from(max), 1.0);
        row.set_title(title);
        row.set_subtitle(subtitle);
        row.set_value(f64::from(value));
        row.set_numeric(true);
        row.update_property(&[Property::Label(&format!("Scratchpad: {title}"))]);
        let sender = commands.clone();
        on_spin_row_settled(&row, move |value| {
            let value = (value as u32).clamp(min, max);
            let _ = sender.try_send(ApplicationCommand::SetScratchpadLimit(limit(value)));
        });
        filter_rows.push((
            row.clone().upcast(),
            format!("scratchpad notes {title} {subtitle}").to_lowercase(),
        ));
        scratchpad_group.add(&row);
    }
    group.add(&scratchpad_group);
}
