//! Feature hub, quick toggles, shortcuts, and warnings.

use super::widgets::{FilterRows, icon_button, plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property, gdk};
use kestrel::view_model::{FeatureHubSection, FeatureIndicator, ShortcutChoiceViewModel};
use kestrel::{
    ApplicationCommand, ApplicationViewModel, ConfirmationViewModel, FeatureViewModel,
    QuickToggleCommand, QuickToggleControlViewModel, QuickToggleMutation, QuickToggleViewModel,
    ShortcutsViewModel, shortcut_label,
};
use kestrel_core::{KeepAwakeDuration, ShortcutModifiers, ShortcutTrigger};
use std::{cell::RefCell, rc::Rc};

/// Builds the feature hub's header and one group per section, returning every
/// row for the sidebar search.
pub(super) fn build_feature_hub(
    features: &[FeatureViewModel],
    commands: &Sender<ApplicationCommand>,
) -> (gtk::Box, FilterRows) {
    let body = gtk::Box::new(Orientation::Vertical, 24);
    let header = adw::PreferencesGroup::builder()
        .title("Feature Hub")
        .description(
            "Turning a tool on makes it available for use; actions such as locking the screen \
             stay separate buttons. Turning a tool off stops its background work. Open a tool's \
             details for what it costs and needs.",
        )
        .build();
    body.append(&header);

    let mut rows = FilterRows::new();
    for section in FeatureHubSection::ALL {
        let group = adw::PreferencesGroup::builder().title(section.title());
        let group = match section.description() {
            Some(description) => group.description(description).build(),
            None => group.build(),
        };
        let mut empty = true;
        for feature in features.iter().filter(|feature| {
            feature.configurable && FeatureHubSection::of(&feature.id) == Some(section)
        }) {
            let (row, keywords) = build_feature_row(feature, commands);
            group.add(&row);
            rows.push((row.upcast(), keywords));
            empty = false;
        }
        if !empty {
            body.append(&group);
        }
    }
    (body, rows)
}

/// One feature's switch, state icon, and details.
fn build_feature_row(
    feature: &FeatureViewModel,
    commands: &Sender<ApplicationCommand>,
) -> (adw::ActionRow, String) {
    let indicator = feature.indicator();
    let status = feature.status_line();
    let details = feature.details();
    let row = plain_row()
        .title(&feature.label)
        .subtitle(&status)
        .subtitle_lines(2)
        .build();
    row.add_prefix(&indicator_icon(indicator));

    let controls = gtk::Box::new(Orientation::Horizontal, 6);
    controls.append(&details_icon(&details));
    let enabled = gtk::Switch::builder()
        .active(feature.enabled)
        .sensitive(feature.switchable())
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
    row.update_property(&[Property::Label(&format!(
        "{}. {}. {status}",
        feature.label,
        indicator.label()
    ))]);
    row.update_property(&[Property::Description(&details)]);
    let keywords = format!("{} {} {status} {details}", feature.label, feature.id).to_lowercase();
    (row, keywords)
}

/// The row's state icon: running, needs attention, unavailable, or off.
fn indicator_icon(indicator: FeatureIndicator) -> gtk::Image {
    let (icon, style) = match indicator {
        FeatureIndicator::Running => ("emblem-ok-symbolic", "success"),
        FeatureIndicator::NeedsAttention => ("dialog-warning-symbolic", "warning"),
        FeatureIndicator::Unavailable => ("action-unavailable-symbolic", "error"),
        FeatureIndicator::Off => ("system-shutdown-symbolic", "dim-label"),
    };
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class(style);
    image.set_tooltip_text(Some(indicator.label()));
    image.update_property(&[Property::Label(indicator.label())]);
    image
}

/// An (i) icon whose tooltip holds the row's details; screen readers get
/// them as the row's description.
fn details_icon(details: &str) -> gtk::Image {
    let icon = gtk::Image::from_icon_name("help-about-symbolic");
    icon.set_valign(Align::Center);
    icon.set_tooltip_text(Some(details));
    icon.update_state(&[gtk::accessible::State::Hidden(true)]);
    icon
}

pub(super) fn build_quick_toggle_group(
    toggles: &[QuickToggleViewModel],
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> (adw::PreferencesGroup, Option<adw::ActionRow>) {
    let group = adw::PreferencesGroup::builder()
        .title("Quick toggles")
        .description("Hover a toggle to see what it needs and where its state comes from.")
        .build();
    let mut keep_awake_row = None;

    for toggle in toggles {
        let session_text = toggle
            .keep_awake
            .as_ref()
            .and_then(|controls| controls.session_text.as_deref());
        let subtitle = match (session_text, &toggle.error) {
            (Some(session_text), _) => session_text.to_owned(),
            (None, Some(error)) => format!("{}\nLast error: {error}", toggle.detail),
            (None, None) => toggle.detail.clone(),
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
                if let Some(keep_awake) = &toggle.keep_awake {
                    let selection = Rc::new(std::cell::Cell::new(keep_awake.selected));
                    row.add_suffix(&keep_awake_picker(
                        keep_awake,
                        window,
                        commands,
                        Rc::clone(&selection),
                    ));
                    let sender = commands.clone();
                    control.connect_state_set(move |_, state| {
                        let command = if state {
                            ApplicationCommand::StartKeepAwake(selection.get())
                        } else {
                            ApplicationCommand::StopKeepAwake
                        };
                        let _ = sender.try_send(command);
                        gtk::glib::Propagation::Stop
                    });
                } else {
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
                        send_with_confirmation(
                            &window,
                            label,
                            confirmation.as_ref(),
                            command,
                            &sender,
                        );
                        gtk::glib::Propagation::Stop
                    });
                }
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
        if toggle.id == kestrel::QuickToggleId::KeepAwake {
            keep_awake_row = Some(row.clone());
        }
        group.add(&row);
    }
    (group, keep_awake_row)
}

/// The keep-awake duration picker, with a custom length dialog.
fn keep_awake_picker(
    controls: &kestrel::view_model::KeepAwakeControlsViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    selection: Rc<std::cell::Cell<KeepAwakeDuration>>,
) -> gtk::DropDown {
    let mut labels = controls
        .duration_choices
        .iter()
        .map(|(_, label)| label.as_str())
        .collect::<Vec<_>>();
    let custom_index = labels.len() as u32;
    labels.push("Custom…");
    let picker = gtk::DropDown::from_strings(&labels);
    picker.set_valign(Align::Center);
    picker.set_tooltip_text(Some("How long Keep awake runs"));
    picker.update_property(&[Property::Label("Keep awake duration")]);
    let selected = controls
        .duration_choices
        .iter()
        .position(|(duration, _)| *duration == controls.selected)
        .unwrap_or_default() as u32;
    picker.set_selected(selected);

    let choices = controls.duration_choices.clone();
    let sender = commands.clone();
    let window = window.clone();
    let updating = Rc::new(std::cell::Cell::new(false));
    let last = Rc::new(std::cell::Cell::new(selected));
    picker.connect_selected_notify(move |picker| {
        if updating.get() {
            return;
        }
        let index = picker.selected();
        if index == custom_index {
            updating.set(true);
            picker.set_selected(last.get());
            updating.set(false);
            let current = choices
                .get(last.get() as usize)
                .map(|(duration, _)| *duration)
                .unwrap_or(KeepAwakeDuration::UntilStopped);
            keep_awake_custom_dialog(&window, &sender, current, Rc::clone(&selection));
            return;
        }
        last.set(index);
        if let Some((duration, _)) = choices.get(index as usize) {
            // A reset or a re-cancel can notify with the unchanged value.
            if *duration != selection.get() {
                selection.set(*duration);
                let _ = sender.try_send(ApplicationCommand::SetKeepAwakeChoice(*duration));
            }
        }
    });
    picker
}

/// Asks for a whole-minute length from one minute to 24 hours.
fn keep_awake_custom_dialog(
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    current: KeepAwakeDuration,
    selection: Rc<std::cell::Cell<KeepAwakeDuration>>,
) {
    let (hours, minutes) = match current {
        KeepAwakeDuration::For(duration) => (
            (duration.as_secs() / 3600).min(24),
            (duration.as_secs() % 3600) / 60,
        ),
        KeepAwakeDuration::UntilStopped => (1, 0),
    };
    let content = gtk::Box::new(Orientation::Vertical, 12);
    let hour_spin = gtk::SpinButton::with_range(0.0, 24.0, 1.0);
    hour_spin.set_digits(0);
    hour_spin.set_value(hours as f64);
    hour_spin.update_property(&[Property::Label("Hours")]);
    content.append(&keep_awake_spin_row("Hours", &hour_spin));
    let minute_spin = gtk::SpinButton::with_range(0.0, 59.0, 1.0);
    minute_spin.set_digits(0);
    minute_spin.set_value(minutes as f64);
    minute_spin.update_property(&[Property::Label("Minutes")]);
    content.append(&keep_awake_spin_row("Minutes", &minute_spin));

    let dialog = adw::AlertDialog::builder()
        .heading("Custom length")
        .body("Choose how long to keep the computer awake.")
        .extra_child(&content)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("use", "Use");
    dialog.set_response_appearance("use", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("use"));
    dialog.set_close_response("cancel");

    let validate: Rc<dyn Fn() -> Option<KeepAwakeDuration>> = {
        let dialog = dialog.clone();
        let hour_spin = hour_spin.clone();
        let minute_spin = minute_spin.clone();
        Rc::new(move || {
            let seconds = hour_spin.value() as u64 * 3_600 + minute_spin.value() as u64 * 60;
            match KeepAwakeDuration::timed(std::time::Duration::from_secs(seconds)) {
                Ok(duration) => {
                    dialog.set_body("Choose how long to keep the computer awake.");
                    dialog.set_response_enabled("use", true);
                    Some(duration)
                }
                Err(error) => {
                    dialog.set_body(&error.to_string());
                    dialog.set_response_enabled("use", false);
                    None
                }
            }
        })
    };
    for spin in [&hour_spin, &minute_spin] {
        let validate = Rc::clone(&validate);
        spin.connect_value_changed(move |_| {
            let _ = validate();
        });
    }
    let _ = validate();

    let sender = commands.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response != "use" {
            dialog.close();
            return;
        }
        if let Some(duration) = validate() {
            if duration != selection.get() {
                selection.set(duration);
                let _ = sender.try_send(ApplicationCommand::SetKeepAwakeChoice(duration));
            }
            dialog.close();
        }
    });
    dialog.present(Some(window));
}

fn keep_awake_spin_row(label: &str, spin: &gtk::SpinButton) -> gtk::Box {
    let row = gtk::Box::new(Orientation::Horizontal, 12);
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_hexpand(true);
    row.append(&text);
    row.append(spin);
    row
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
        .title("Keyboard shortcuts")
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
