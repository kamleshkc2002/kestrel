use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, PolicyType, accessible::Property};
use kestrel::{
    ApplicationCommand, ApplicationViewModel, CapabilityKindViewModel, ConfirmationViewModel,
    FeatureViewModel, QuickToggleCommand, QuickToggleControlViewModel, QuickToggleMutation,
    QuickToggleViewModel, RemediationViewModel,
};

const MINIMUM_WINDOW_WIDTH: i32 = 360;
const MINIMUM_WINDOW_HEIGHT: i32 = 360;
const DEFAULT_WINDOW_WIDTH: i32 = 760;
const DEFAULT_WINDOW_HEIGHT: i32 = 640;
const CONTENT_MAXIMUM_WIDTH: i32 = 760;

pub struct WindowView {
    pub window: adw::ApplicationWindow,
    content: gtk::ScrolledWindow,
    refresh_button: gtk::Button,
    toasts: adw::ToastOverlay,
    commands: Sender<ApplicationCommand>,
}

impl WindowView {
    pub fn new(
        application: &adw::Application,
        view_model: &ApplicationViewModel,
        commands: Sender<ApplicationCommand>,
    ) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Kestrel")
            .default_width(DEFAULT_WINDOW_WIDTH)
            .default_height(DEFAULT_WINDOW_HEIGHT)
            .build();
        window.set_size_request(MINIMUM_WINDOW_WIDTH, MINIMUM_WINDOW_HEIGHT);

        let title = adw::WindowTitle::new("Kestrel", "Feature capabilities");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh capability status")
            .action_name("app.refresh-capabilities")
            .build();
        refresh_button.update_property(&[
            Property::Label("Refresh capability status"),
            Property::Description(
                "Run the registered read-only capability probes and update every feature status.",
            ),
        ]);
        header.pack_end(&refresh_button);

        let content = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .kinetic_scrolling(true)
            .propagate_natural_height(false)
            .vexpand(true)
            .build();
        content.set_child(Some(&build_page(view_model, &window, &commands)));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&content));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&toasts));
        window.set_content(Some(&toolbar));

        Self {
            window,
            content,
            refresh_button,
            toasts,
            commands,
        }
    }

    pub fn set_view_model(&self, view_model: &ApplicationViewModel) {
        self.content
            .set_child(Some(&build_page(view_model, &self.window, &self.commands)));
    }

    pub fn set_refreshing(&self, refreshing: bool) {
        self.refresh_button.set_sensitive(!refreshing);
        self.refresh_button.set_tooltip_text(Some(if refreshing {
            "Refreshing capability status"
        } else {
            "Refresh capability status"
        }));
    }

    pub fn show_message(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }
}

fn build_page(
    view_model: &ApplicationViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> adw::Clamp {
    let page = gtk::Box::new(Orientation::Vertical, 24);
    page.set_margin_top(24);
    page.set_margin_bottom(24);
    page.set_margin_start(18);
    page.set_margin_end(18);

    let heading = gtk::Label::new(Some("Capability status"));
    heading.add_css_class("title-1");
    heading.set_halign(Align::Start);
    heading.set_wrap(true);
    heading.set_xalign(0.0);
    page.append(&heading);

    let description = gtk::Label::new(Some(
        "Kestrel keeps every feature visible and explains unavailable platform paths. The normal window remains available without a tray host or global shortcut provider.",
    ));
    configure_wrapping_label(&description);
    description.add_css_class("dim-label");
    page.append(&description);

    if !view_model.warnings.is_empty() {
        page.append(&build_warning_group(view_model));
    }
    if !view_model.quick_toggles.is_empty() {
        page.append(&build_quick_toggle_group(
            &view_model.quick_toggles,
            window,
            commands,
        ));
    }

    for feature in &view_model.features {
        page.append(&build_feature_group(feature));
    }

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(CONTENT_MAXIMUM_WIDTH);
    clamp.set_tightening_threshold(MINIMUM_WINDOW_WIDTH);
    clamp.set_child(Some(&page));
    clamp
}

fn build_quick_toggle_group(
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
        let row = adw::ActionRow::builder()
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

fn build_warning_group(view_model: &ApplicationViewModel) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Configuration warnings")
        .description("Invalid feature settings are isolated and do not prevent other features from starting.")
        .build();

    for warning in &view_model.warnings {
        let row = adw::ActionRow::builder()
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

fn build_feature_group(feature: &FeatureViewModel) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(&feature.label)
        .description(&feature.id)
        .build();
    group.set_tooltip_text(Some(&feature.id));

    let status_row = adw::ActionRow::builder()
        .title("Capability status")
        .subtitle(&feature.capability.summary)
        .subtitle_lines(0)
        .build();
    let icon = gtk::Image::from_icon_name(capability_icon_name(feature.capability.status.kind));
    icon.add_css_class(capability_css_class(feature.capability.status.kind));
    status_row.add_prefix(&icon);

    let badge = gtk::Label::new(Some(feature.capability.status.label));
    badge.add_css_class("pill");
    badge.add_css_class(capability_css_class(feature.capability.status.kind));
    badge.set_valign(Align::Center);
    status_row.add_suffix(&badge);

    let status_accessible_label = format!(
        "{} capability: {}. {}",
        feature.label, feature.capability.status.label, feature.capability.summary
    );
    status_row.update_property(&[Property::Label(&status_accessible_label)]);
    group.add(&status_row);

    group.add(&build_labeled_row(
        "Service state",
        feature.lifecycle.label(),
    ));

    if let Some(detail) = &feature.capability.status.detail {
        group.add(&build_labeled_row("Capability detail", detail));
    }
    if let Some(backend) = &feature.capability.selected_backend {
        group.add(&build_labeled_row("Selected backend", backend));
    }
    if let Some(remediation) = &feature.capability.remediation {
        group.add(&build_remediation_row(remediation));
    }

    group
}

fn build_labeled_row(title: &str, value: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .subtitle_lines(0)
        .build();
    let accessible_label = format!("{title}: {value}");
    row.update_property(&[Property::Label(&accessible_label)]);
    row
}

fn build_remediation_row(remediation: &RemediationViewModel) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(remediation.title)
        .subtitle(&remediation.message)
        .subtitle_lines(0)
        .build();
    let icon = gtk::Image::from_icon_name("dialog-information-symbolic");
    icon.add_css_class("accent");
    row.add_prefix(&icon);
    let accessible_label = format!("{}: {}", remediation.title, remediation.message);
    row.update_property(&[Property::Label(&accessible_label)]);
    row
}

fn configure_wrapping_label(label: &gtk::Label) {
    label.set_halign(Align::Start);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
}

fn capability_icon_name(kind: CapabilityKindViewModel) -> &'static str {
    match kind {
        CapabilityKindViewModel::Supported => "emblem-ok-symbolic",
        CapabilityKindViewModel::Limited => "dialog-warning-symbolic",
        CapabilityKindViewModel::NeedsPermission => "changes-prevent-symbolic",
        CapabilityKindViewModel::MissingDependency => "software-update-available-symbolic",
        CapabilityKindViewModel::Unsupported => "action-unavailable-symbolic",
    }
}

fn capability_css_class(kind: CapabilityKindViewModel) -> &'static str {
    match kind {
        CapabilityKindViewModel::Supported => "success",
        CapabilityKindViewModel::Limited
        | CapabilityKindViewModel::NeedsPermission
        | CapabilityKindViewModel::MissingDependency => "warning",
        CapabilityKindViewModel::Unsupported => "error",
    }
}
