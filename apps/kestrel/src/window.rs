use adw::prelude::*;
use gtk::{Align, Orientation, PolicyType, accessible::Property};
use kestrel::{
    ApplicationViewModel, CapabilityKindViewModel, FeatureViewModel, RemediationViewModel,
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
}

impl WindowView {
    pub fn new(application: &adw::Application, view_model: &ApplicationViewModel) -> Self {
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
        content.set_child(Some(&build_page(view_model)));

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
        }
    }

    pub fn set_view_model(&self, view_model: &ApplicationViewModel) {
        self.content.set_child(Some(&build_page(view_model)));
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

fn build_page(view_model: &ApplicationViewModel) -> adw::Clamp {
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

    for feature in &view_model.features {
        page.append(&build_feature_group(feature));
    }

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(CONTENT_MAXIMUM_WIDTH);
    clamp.set_tightening_threshold(MINIMUM_WINDOW_WIDTH);
    clamp.set_child(Some(&page));
    clamp
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
