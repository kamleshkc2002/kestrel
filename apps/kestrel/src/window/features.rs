//! Feature hub, quick toggles, shortcuts, and warnings.

use super::widgets::plain_row;
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::{
    ApplicationCommand, ApplicationViewModel, ConfirmationViewModel, FeatureViewModel,
    QuickToggleCommand, QuickToggleControlViewModel, QuickToggleMutation, QuickToggleViewModel,
    ShortcutsViewModel,
};

pub(super) fn build_feature_hub_group(
    features: &[FeatureViewModel],
    commands: &Sender<ApplicationCommand>,
    search: &gtk::SearchEntry,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Feature Hub")
        .description("Every registered feature exposes enablement, lifecycle, capability, and conservative resource costs.")
        .build();
    let mut rows = Vec::<(adw::ActionRow, String)>::new();
    for feature in features {
        let row = plain_row()
            .title(&feature.label)
            .subtitle(feature_hub_subtitle(feature))
            .subtitle_lines(0)
            .build();
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let enabled = gtk::Switch::builder()
            .active(feature.enabled)
            .sensitive(feature.configurable)
            .valign(Align::Center)
            .build();
        let sender = commands.clone();
        let feature_id = feature.id.clone();
        enabled.connect_state_set(move |_, state| {
            let _ = sender.try_send(ApplicationCommand::SetFeatureEnabled {
                feature_id: feature_id.clone(),
                enabled: state,
            });
            gtk::glib::Propagation::Proceed
        });
        controls.append(&enabled);
        row.add_suffix(&controls);
        row.set_activatable_widget(Some(&enabled));
        let accessible = format!(
            "{}. Registered: {}. Enabled: {}. Available: {}. Running: {}. Configurable: {}. {}",
            feature.label,
            yes_no(feature.registered),
            yes_no(feature.enabled),
            yes_no(feature.available),
            yes_no(feature.running),
            yes_no(feature.configurable),
            feature_hub_subtitle(feature)
        );
        row.update_property(&[Property::Label(&accessible)]);
        group.add(&row);
        rows.push((
            row,
            format!(
                "{} {} {}",
                feature.label,
                feature.id,
                feature_hub_subtitle(feature)
            ),
        ));
    }
    let rows = std::rc::Rc::new(rows);
    let rows_for_search = rows.clone();
    search.connect_search_changed(move |entry| {
        let query = entry.text().trim().to_lowercase();
        for (row, content) in rows_for_search.iter() {
            row.set_visible(query.is_empty() || content.to_lowercase().contains(&query));
        }
    });
    group
}

fn feature_hub_subtitle(feature: &FeatureViewModel) -> String {
    let capability = feature
        .capability
        .status
        .detail
        .as_deref()
        .unwrap_or(feature.capability.status.label);
    let backend = feature
        .capability
        .selected_backend
        .as_deref()
        .map(|value| format!(" Backend: {value}."))
        .unwrap_or_default();
    let remediation = feature
        .capability
        .remediation
        .as_ref()
        .map(|value| format!(" Remediation: {}", value.message))
        .unwrap_or_default();
    format!(
        "{}\nState: {} (registered {}, enabled {}, available {}, running {}, configurable {})\nCost: {}. Capability: {}.{}{}",
        feature.capability.summary,
        feature.lifecycle.label(),
        yes_no(feature.registered),
        yes_no(feature.enabled),
        yes_no(feature.available),
        yes_no(feature.running),
        yes_no(feature.configurable),
        feature.cost.summary(),
        capability,
        backend,
        remediation
    )
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub(super) fn build_quick_toggle_group(
    toggles: &[QuickToggleViewModel],
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Quick toggles")
        .description(
            "Each control uses an independent user-session adapter. Requirements and externally changed state remain visible.",
        )
        .build();

    for toggle in toggles {
        let subtitle = match &toggle.error {
            Some(error) => format!(
                "{}\nRequirement: {}\nState source: {}\nLast error: {}",
                toggle.detail, toggle.requirement, toggle.source, error
            ),
            None => format!(
                "{}\nRequirement: {}\nState source: {}",
                toggle.detail, toggle.requirement, toggle.source
            ),
        };
        let row = plain_row()
            .title(toggle.label)
            .subtitle(subtitle)
            .subtitle_lines(0)
            .sensitive(toggle.available)
            .build();

        match &toggle.control {
            Some(QuickToggleControlViewModel::Switch {
                enabled,
                confirmation,
            }) => {
                let control = gtk::Switch::builder()
                    .active(*enabled)
                    .valign(Align::Center)
                    .build();
                let sender = commands.clone();
                let window = window.clone();
                let id = toggle.id;
                let label = toggle.label;
                let confirmation = confirmation.clone();
                control.connect_state_set(move |_, state| {
                    let command = QuickToggleCommand {
                        id,
                        mutation: QuickToggleMutation::SetEnabled(state),
                        confirmation_token: confirmation
                            .as_ref()
                            .map(|confirmation| confirmation.token.clone()),
                    };
                    send_with_confirmation(&window, label, confirmation.as_ref(), command, &sender);
                    gtk::glib::Propagation::Stop
                });
                row.add_suffix(&control);
                row.set_activatable_widget(Some(&control));
            }
            Some(QuickToggleControlViewModel::Level { percentage }) => {
                let controls = gtk::Box::new(Orientation::Horizontal, 6);
                controls.set_valign(Align::Center);
                let decrease = gtk::Button::with_label("−");
                decrease.set_tooltip_text(Some(&format!("Decrease {}", toggle.label)));
                decrease.set_sensitive(*percentage > 0);
                let value = gtk::Label::new(Some(&format!("{percentage}%")));
                value.add_css_class("numeric");
                let increase = gtk::Button::with_label("+");
                increase.set_tooltip_text(Some(&format!("Increase {}", toggle.label)));
                increase.set_sensitive(*percentage < 100);

                let sender = commands.clone();
                let id = toggle.id;
                let next = percentage.saturating_sub(10);
                decrease.connect_clicked(move |_| {
                    let _ = sender.try_send(ApplicationCommand::QuickToggle(QuickToggleCommand {
                        id,
                        mutation: QuickToggleMutation::SetLevel(next),
                        confirmation_token: None,
                    }));
                });
                let sender = commands.clone();
                let id = toggle.id;
                let next = percentage.saturating_add(10).min(100);
                increase.connect_clicked(move |_| {
                    let _ = sender.try_send(ApplicationCommand::QuickToggle(QuickToggleCommand {
                        id,
                        mutation: QuickToggleMutation::SetLevel(next),
                        confirmation_token: None,
                    }));
                });

                controls.append(&decrease);
                controls.append(&value);
                controls.append(&increase);
                row.add_suffix(&controls);
            }
            Some(QuickToggleControlViewModel::Actions(actions)) => {
                let controls = gtk::Box::new(Orientation::Vertical, 6);
                controls.set_valign(Align::Center);
                for action in actions {
                    let button = gtk::Button::with_label(&action.label);
                    let sender = commands.clone();
                    let window = window.clone();
                    let label = toggle.label;
                    let confirmation = action.confirmation.clone();
                    let command = QuickToggleCommand {
                        id: toggle.id,
                        mutation: QuickToggleMutation::Activate {
                            target: action.target.clone(),
                        },
                        confirmation_token: Some(confirmation.token.clone()),
                    };
                    button.connect_clicked(move |_| {
                        send_with_confirmation(
                            &window,
                            label,
                            Some(&confirmation),
                            command.clone(),
                            &sender,
                        );
                    });
                    controls.append(&button);
                }
                row.add_suffix(&controls);
            }
            None => {}
        }
        let accessible_label = format!(
            "{}. {} Requirement: {}. State source: {}.",
            toggle.label, toggle.detail, toggle.requirement, toggle.source
        );
        row.update_property(&[Property::Label(&accessible_label)]);
        group.add(&row);
    }
    group
}

fn send_with_confirmation(
    window: &adw::ApplicationWindow,
    label: &str,
    confirmation: Option<&ConfirmationViewModel>,
    command: QuickToggleCommand,
    commands: &Sender<ApplicationCommand>,
) {
    let Some(confirmation) = confirmation else {
        let _ = commands.try_send(ApplicationCommand::QuickToggle(command));
        return;
    };
    let dialog = adw::MessageDialog::builder()
        .transient_for(window)
        .heading(format!("Confirm {label}"))
        .body(&confirmation.scope)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", "Continue")]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    let commands = commands.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "confirm" {
            let _ = commands.try_send(ApplicationCommand::QuickToggle(command.clone()));
        }
        dialog.close();
    });
    dialog.present();
}

/// Builds the global-shortcuts group: one row per configured binding.
/// Text is plain: commands, triggers, and remediation contain `<` and `&`.
pub(super) fn build_shortcuts_group(shortcuts: &ShortcutsViewModel) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Global shortcuts")
        .description(gtk::glib::markup_escape_text(&shortcuts.status).as_str())
        .build();
    for row in &shortcuts.rows {
        let item = plain_row()
            .title(&row.description)
            .subtitle(format!("{} · {}", row.command, row.state))
            .subtitle_lines(0)
            .build();
        let trigger = gtk::Label::new(Some(&row.trigger));
        trigger.add_css_class(if row.active { "accent" } else { "dim-label" });
        item.add_suffix(&trigger);
        group.add(&item);
    }
    if let Some(notice) = &shortcuts.notice {
        group.add(
            &plain_row()
                .title("Some bindings were skipped")
                .subtitle(notice)
                .subtitle_lines(0)
                .build(),
        );
    }
    group
}

pub(super) fn build_warning_group(view_model: &ApplicationViewModel) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Configuration warnings")
        .description("Invalid feature settings are isolated and do not prevent other features from starting.")
        .build();

    for warning in &view_model.warnings {
        let row = plain_row()
            .title(format!("Warning for {}", warning.feature_id))
            .subtitle(&warning.message)
            .subtitle_lines(0)
            .build();
        let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        icon.add_css_class("warning");
        row.add_prefix(&icon);
        let label = format!(
            "Configuration warning for {}: {}",
            warning.feature_id, warning.message
        );
        row.update_property(&[Property::Label(&label)]);
        group.add(&row);
    }

    group
}
