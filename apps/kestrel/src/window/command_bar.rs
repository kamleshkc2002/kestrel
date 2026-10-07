//! Command bar panel.

use super::widgets::plain_row;
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::ApplicationCommand;

/// Reusable command-bar widgets.
pub(super) struct CommandPanel {
    pub(super) container: gtk::Box,
    pub(super) query: gtk::SearchEntry,
    pub(super) status: gtk::Label,
    pub(super) results: gtk::Box,
}

/// Builds the retained command-bar group.
pub(super) fn build_command_panel(
    command_bar: &kestrel::CommandBarViewModel,
    commands: &Sender<ApplicationCommand>,
) -> CommandPanel {
    let container = gtk::Box::new(Orientation::Vertical, 12);
    let group = adw::PreferencesGroup::builder()
        .title("Command bar")
        .description(
            "One keyboard-first surface for Kestrel commands, snippets, applications, computed \
             values, and configured scripts. Commands that need no desktop integration always \
             work.",
        )
        .build();

    let query = gtk::SearchEntry::builder()
        .placeholder_text("Run a command, search applications, compute a value")
        .hexpand(true)
        .build();
    query.update_property(&[
        Property::Label("Command bar query"),
        Property::Description(
            "Ranks Kestrel commands, snippets, applications, computed values, emoji, and \
             configured scripts",
        ),
    ]);
    let query_row = adw::ActionRow::new();
    query_row.set_title("Query");
    query_row.add_suffix(&query);
    query_row.set_activatable_widget(Some(&query));
    let sender = commands.clone();
    query.connect_search_changed(move |entry| {
        let _ = sender.try_send(ApplicationCommand::CommandQuery(entry.text().to_string()));
    });
    if !command_bar.query.is_empty() {
        query.set_text(&command_bar.query);
    }
    group.add(&query_row);

    let status = gtk::Label::new(Some(&command_bar.status));
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    let status_row = adw::ActionRow::new();
    status_row.set_title("State");
    status_row.add_suffix(&status);
    group.add(&status_row);

    for provider in &command_bar.providers {
        let row = plain_row()
            .title(provider.label)
            .subtitle(&provider.status)
            .subtitle_lines(0)
            .build();
        let toggle = gtk::Switch::builder()
            .active(provider.enabled)
            .valign(Align::Center)
            .build();
        toggle.update_property(&[Property::Label(&format!("{} results", provider.label))]);
        toggle.set_tooltip_text(Some(provider.provider.requirement()));
        let sender = commands.clone();
        let kind = provider.provider;
        toggle.connect_state_set(move |_, enabled| {
            let switch = match kind {
                kestrel::CommandProvider::Applications => {
                    kestrel::CommandProviderSwitch::Applications(enabled)
                }
                kestrel::CommandProvider::Files => kestrel::CommandProviderSwitch::Files(enabled),
                kestrel::CommandProvider::Scripts => {
                    kestrel::CommandProviderSwitch::Scripts(enabled)
                }
                kestrel::CommandProvider::Emoji => kestrel::CommandProviderSwitch::Emoji(enabled),
            };
            let _ = sender.try_send(ApplicationCommand::SetCommandProvider(switch));
            gtk::glib::Propagation::Proceed
        });
        row.add_suffix(&toggle);
        row.set_activatable_widget(Some(&toggle));
        group.add(&row);
    }

    // Ranking stores identifiers and counts, and can be reset.
    let ranking_summary = if command_bar.ranking_entries.is_empty() {
        "Nothing learned yet".to_string()
    } else {
        command_bar
            .ranking_entries
            .iter()
            .take(5)
            .map(|entry| {
                format!(
                    "{}{} ×{}",
                    if entry.pinned { "★ " } else { "" },
                    entry.id,
                    entry.uses
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let ranking_row = plain_row()
        .title("Learned ranking")
        .subtitle(format!(
            "{ranking_summary} · identifiers and counts only, never your queries"
        ))
        .subtitle_lines(0)
        .build();
    let reset = gtk::Button::with_label("Reset");
    reset.set_sensitive(command_bar.running);
    reset.update_property(&[Property::Label("Reset command ranking")]);
    reset.set_tooltip_text(Some("Drop every pin and learned count"));
    let sender = commands.clone();
    reset.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::ResetCommandRanking);
    });
    ranking_row.add_suffix(&reset);
    group.add(&ranking_row);

    let results = gtk::Box::new(Orientation::Vertical, 6);
    let panel = CommandPanel {
        container: container.clone(),
        query: query.clone(),
        status,
        results: results.clone(),
    };
    fill_command_panel(&panel, command_bar, commands);
    group.add(&results);
    container.append(&group);
    panel
}

/// Fills ranked result rows.
pub(super) fn fill_command_panel(
    panel: &CommandPanel,
    command_bar: &kestrel::CommandBarViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    if command_bar.results.is_empty() {
        let row = plain_row()
            .title(if command_bar.query.trim().is_empty() {
                "Type to search"
            } else {
                "No results"
            })
            .subtitle(if command_bar.running {
                "Commands, snippets, applications, computed values, and emoji are searched together."
            } else {
                "Enable commands.bar in the Feature Hub to use the command bar."
            })
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("No command results")]);
        panel.results.append(&row);
        return;
    }

    for result in &command_bar.results {
        let detail = if result.uses > 0 {
            format!("{} · used {} time(s)", result.detail, result.uses)
        } else {
            result.detail.clone()
        };
        let row = plain_row()
            .title(format!("{} · {}", result.source, result.title))
            .subtitle(detail)
            .subtitle_lines(0)
            .build();
        row.update_property(&[Property::Label(&format!(
            "{} result {}",
            result.source, result.title
        ))]);

        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let run = gtk::Button::with_label(result.action_label);
        run.set_sensitive(command_bar.running);
        run.update_property(&[Property::Label(&format!(
            "{} {}",
            result.action_label, result.title
        ))]);
        run.set_tooltip_text(Some(&format!("Matched on {}", result.matched_on)));
        let sender = commands.clone();
        let id = result.id.clone();
        run.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::CommandBar(
                kestrel::CommandBarCommand::Run(id.clone()),
            ));
        });
        controls.append(&run);

        let pin = gtk::Button::with_label(if result.pinned { "Unpin" } else { "Pin" });
        pin.set_sensitive(command_bar.running);
        pin.update_property(&[Property::Label(&format!("Pin {}", result.title))]);
        let sender = commands.clone();
        let id = result.id.clone();
        let pinned = result.pinned;
        pin.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::CommandBar(
                kestrel::CommandBarCommand::Pin {
                    id: id.clone(),
                    pinned: !pinned,
                },
            ));
        });
        controls.append(&pin);
        row.add_suffix(&controls);
        panel.results.append(&row);
    }
}
