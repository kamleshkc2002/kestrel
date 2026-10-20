//! Feature hub, quick toggles, shortcuts, and warnings.

use super::widgets::{FilterRows, icon_button, plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property, gdk};
use kestrel::view_model::ShortcutChoiceViewModel;
use kestrel::{
    ApplicationCommand, ApplicationViewModel, ConfirmationViewModel, FeatureViewModel,
    QuickToggleCommand, QuickToggleControlViewModel, QuickToggleMutation, QuickToggleViewModel,
    ShortcutsViewModel, shortcut_label,
};
use kestrel_core::{ShortcutModifiers, ShortcutTrigger};
use std::{cell::RefCell, rc::Rc};

/// Builds the feature hub and returns its rows for the sidebar search.
pub(super) fn build_feature_hub_group(
    features: &[FeatureViewModel],
    commands: &Sender<ApplicationCommand>,
) -> (adw::PreferencesGroup, FilterRows) {
    let group = adw::PreferencesGroup::builder()
        .title("Feature Hub")
        .description("Every registered feature exposes enablement, lifecycle, capability, and conservative resource costs.")
        .build();
    let mut rows = FilterRows::new();
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
            row.upcast(),
            format!(
                "{} {} {}",
                feature.label,
                feature.id,
                feature_hub_subtitle(feature)
            )
            .to_lowercase(),
        ));
    }
    (group, rows)
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
        .description("Hover a toggle to see what it needs and where its state comes from.")
        .build();

    for toggle in toggles {
        let subtitle = match &toggle.error {
            Some(error) => format!("{}\nLast error: {error}", toggle.detail),
            None => toggle.detail.clone(),
        };
        let row = plain_row()
            .title(toggle.label)
            .subtitle(subtitle)
            .subtitle_lines(0)
            .sensitive(toggle.available)
            .build();
        row.set_tooltip_text(Some(&format!(
            "Requirement: {}\nState source: {}",
            toggle.requirement, toggle.source
        )));

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

/// Runtime labels contain `<` and `&`; keep them as plain text.
pub(super) fn build_shortcuts_group(
    shortcuts: &ShortcutsViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Global shortcuts")
        .description(gtk::glib::markup_escape_text(&shortcuts.status).as_str())
        .build();
    let add = gtk::Button::with_label("Add shortcut");
    add.set_valign(Align::Center);
    add.set_sensitive(!shortcuts.choices.is_empty());
    let choices = shortcuts.choices.clone();
    let (dialog_window, dialog_commands) = (window.clone(), commands.clone());
    add.connect_clicked(move |_| {
        shortcut_dialog(
            &dialog_window,
            &dialog_commands,
            ShortcutTarget::Choose(choices.clone()),
        );
    });
    group.set_header_suffix(Some(&add));

    if let Some(steps) = &shortcuts.desktop_steps {
        group.add(
            &plain_row()
                .title("Set them up in your desktop")
                .subtitle(steps)
                .subtitle_lines(0)
                .build(),
        );
    }
    for row in &shortcuts.rows {
        let item = plain_row()
            .title(&row.description)
            .subtitle(format!("{} · {}", row.command, row.state))
            .subtitle_lines(0)
            .build();
        let trigger = gtk::Label::new(Some(&row.trigger));
        trigger.add_css_class(if row.active { "accent" } else { "dim-label" });
        item.add_suffix(&trigger);

        let change = icon_button("document-edit-symbolic", "Change key", true);
        let target = ShortcutTarget::Fixed {
            command: row.command.clone(),
            description: row.description.clone(),
        };
        let (dialog_window, dialog_commands) = (window.clone(), commands.clone());
        change.connect_clicked(move |_| {
            shortcut_dialog(&dialog_window, &dialog_commands, target.clone());
        });
        item.add_suffix(&change);

        let copy = icon_button("edit-copy-symbolic", "Copy command", true);
        let line = row.copy_command.clone();
        copy.connect_clicked(move |button| {
            button.clipboard().set_text(&line);
            if let Some(toasts) = button
                .ancestor(adw::ToastOverlay::static_type())
                .and_downcast::<adw::ToastOverlay>()
            {
                let toast = adw::Toast::new(&format!("Copied “{line}”"));
                toast.set_use_markup(false);
                toasts.add_toast(toast);
            }
        });
        item.add_suffix(&copy);

        let remove = icon_button("user-trash-symbolic", "Remove shortcut", true);
        let (command, sender) = (row.command.clone(), commands.clone());
        remove.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::SetShortcut {
                command: command.clone(),
                trigger: None,
            });
        });
        item.add_suffix(&remove);
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

/// The action a shortcut dialog sets: picked from the free ones, or fixed.
#[derive(Clone)]
enum ShortcutTarget {
    Choose(Vec<ShortcutChoiceViewModel>),
    Fixed {
        command: String,
        description: String,
    },
}

/// Asks for an action (when adding) and a key combination, then saves it.
fn shortcut_dialog(
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    target: ShortcutTarget,
) {
    let content = gtk::Box::new(Orientation::Vertical, 12);
    let picker = match &target {
        ShortcutTarget::Choose(choices) => {
            let descriptions = choices
                .iter()
                .map(|choice| choice.description.as_str())
                .collect::<Vec<_>>();
            let picker = gtk::DropDown::from_strings(&descriptions);
            picker.set_expression(Some(gtk::PropertyExpression::new(
                gtk::StringObject::static_type(),
                None::<gtk::Expression>,
                "string",
            )));
            picker.set_enable_search(true);
            picker.update_property(&[Property::Label("Action")]);
            content.append(&picker);
            Some(picker)
        }
        ShortcutTarget::Fixed { .. } => None,
    };
    let keys = gtk::Label::new(Some("Waiting for keys…"));
    keys.add_css_class("title-2");
    keys.update_property(&[Property::Label("Captured key combination")]);
    content.append(&keys);

    let heading = match &target {
        ShortcutTarget::Choose(_) => "Add shortcut".to_owned(),
        ShortcutTarget::Fixed { description, .. } => format!("Shortcut for “{description}”"),
    };
    let dialog = adw::MessageDialog::builder()
        .transient_for(window)
        .modal(true)
        .heading(heading)
        .body(
            "Press the key combination. It needs Super, Ctrl, Alt, or Shift; the desktop may \
             keep combinations it already uses.",
        )
        .extra_child(&content)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("save", false);
    dialog.set_close_response("cancel");

    let captured: Rc<RefCell<Option<ShortcutTrigger>>> = Rc::default();
    let keys_controller = gtk::EventControllerKey::new();
    keys_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (held, shown, capturing) = (Rc::clone(&captured), keys.clone(), dialog.clone());
    keys_controller.connect_key_pressed(move |_, key, _, state| {
        let Some(trigger) = trigger_from_key(key, state) else {
            return gtk::glib::Propagation::Proceed;
        };
        shown.set_text(&shortcut_label(&trigger));
        held.replace(Some(trigger));
        capturing.set_response_enabled("save", true);
        gtk::glib::Propagation::Stop
    });
    dialog.add_controller(keys_controller);

    let sender = commands.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "save" {
            let command = match (&target, &picker) {
                (ShortcutTarget::Choose(choices), Some(picker)) => choices
                    .get(picker.selected() as usize)
                    .map(|choice| choice.command.clone()),
                (ShortcutTarget::Fixed { command, .. }, _) => Some(command.clone()),
                _ => None,
            };
            if let (Some(command), Some(trigger)) = (command, captured.borrow().clone()) {
                let _ = sender.try_send(ApplicationCommand::SetShortcut {
                    command,
                    trigger: Some(trigger),
                });
            }
        }
        dialog.close();
    });
    dialog.present();
}

/// A shortcut from a key press: a non-modifier key held with at least one
/// modifier, named as XKB names it.
fn trigger_from_key(key: gdk::Key, state: gdk::ModifierType) -> Option<ShortcutTrigger> {
    let modifiers = ShortcutModifiers {
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        logo: state.contains(gdk::ModifierType::SUPER_MASK),
    };
    let name = key.to_lower().name()?;
    let is_modifier = [
        "Shift",
        "Control",
        "Alt",
        "Super",
        "Meta",
        "Hyper",
        "ISO_",
        "Caps_Lock",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix));
    if is_modifier || modifiers == ShortcutModifiers::default() {
        return None;
    }
    let trigger = ShortcutTrigger {
        modifiers,
        key: name.to_string(),
    };
    ShortcutTrigger::parse(&trigger.to_string()).ok()
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
