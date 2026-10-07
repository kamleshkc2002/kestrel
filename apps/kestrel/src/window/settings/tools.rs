//! Clipboard, snippet, and command-bar settings.

use crate::window::widgets::plain_row;
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, accessible::Property};
use kestrel::{ApplicationCommand, ApplicationViewModel};

/// Adds clipboard retention bounds and filters.
pub(super) fn add_clipboard_limits_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let clipboard = &view_model.clipboard;
    let clipboard_group = adw::PreferencesGroup::builder()
        .title("Clipboard")
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
        row.connect_value_notify(move |spin| {
            // Bounds use kibibytes in the UI and bytes in storage.
            let value = spin.value().max(0.0);
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
        "Seconds after Kestrel takes the selection before the live clipboard is cleared. \
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

    for row in [
        items.upcast_ref::<gtk::Widget>().clone(),
        item_bytes.upcast_ref::<gtk::Widget>().clone(),
        image_bytes.upcast_ref::<gtk::Widget>().clone(),
        files.upcast_ref::<gtk::Widget>().clone(),
        age.upcast_ref::<gtk::Widget>().clone(),
        clear.upcast_ref::<gtk::Widget>().clone(),
        filter.clone().upcast(),
        paste.clone().upcast(),
    ] {
        filter_rows.push((
            row,
            "clipboard history retention bounds sensitive plain text".to_owned(),
        ));
    }
    group.add(&clipboard_group);
}

/// Adds snippet library bounds and provider choices.
pub(super) fn add_snippet_limits_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let snippets = &view_model.snippets;
    let snippet_group = adw::PreferencesGroup::builder()
        .title("Snippets")
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
    content.connect_value_notify(move |spin| {
        let bytes = (spin.value().max(0.0) as u32).saturating_mul(1024);
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
    clipboard_variable.connect_value_notify(move |spin| {
        let bytes = spin.value().max(0.0) as u32;
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
    timeout.connect_value_notify(move |spin| {
        let millis = spin.value().max(0.0) as u64;
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

/// Adds the command-bar result limit.
pub(super) fn add_command_bar_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let command_bar = &view_model.command_bar;
    let command_group = adw::PreferencesGroup::builder()
        .title("Command bar")
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
    results.connect_value_notify(move |spin| {
        let value = spin.value().max(0.0) as u32;
        let _ = sender.try_send(ApplicationCommand::SetCommandResultLimit(value));
    });
    command_group.add(&results);
    filter_rows.push((
        results.clone().upcast(),
        "command bar results limit launcher".to_owned(),
    ));
    group.add(&command_group);
}
