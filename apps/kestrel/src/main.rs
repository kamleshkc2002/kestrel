use adw::prelude::*;
use gtk::{Align, Orientation};
use kestrel::{
    configuration_path, load, ApplicationRuntime, ApplicationViewModel, CapabilityKindViewModel,
    FeatureViewModel, LoadedConfiguration, RemediationViewModel,
};

fn main() {
    let loaded = configuration_path()
        .as_deref()
        .map(load)
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("Kestrel could not load its configuration: {error}");
            Some(LoadedConfiguration::default())
        })
        .unwrap_or_default();
    let mut runtime =
        ApplicationRuntime::new(&loaded.configuration).expect("built-in features have valid IDs");
    runtime.start();
    runtime
        .refresh_capabilities()
        .expect("built-in capability probes are internally consistent");

    let application = adw::Application::builder()
        .application_id("io.github.kamleshkc2002.Kestrel")
        .build();
    let warnings = loaded.warnings;

    application.connect_activate(move |application| {
        let view_model = runtime.view_model(&warnings);
        build_window(application, &view_model);
    });
    application.run();
}

fn build_window(application: &adw::Application, view_model: &ApplicationViewModel) {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Kestrel")
        .default_width(720)
        .default_height(520)
        .build();
    let content = gtk::Box::new(Orientation::Vertical, 18);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let heading = gtk::Label::new(Some("Kestrel command surface"));
    heading.add_css_class("title-1");
    heading.set_halign(Align::Start);
    content.append(&heading);

    let description = gtk::Label::new(Some(
        "Use this normal window when tray or global shortcuts are unavailable.",
    ));
    description.set_halign(Align::Start);
    description.set_wrap(true);
    content.append(&description);

    let commands = gtk::SearchEntry::new();
    commands.set_placeholder_text(Some("Search commands and feature status"));
    commands.set_hexpand(true);
    content.append(&commands);

    let features = gtk::ListBox::new();
    features.add_css_class("boxed-list");
    for feature in &view_model.features {
        features.append(&build_feature_view(feature));
    }
    content.append(&features);

    for warning in &view_model.warnings {
        let warning = gtk::Label::new(Some(&format!(
            "Configuration warning for {}: {}",
            warning.feature_id, warning.message
        )));
        warning.set_halign(Align::Start);
        warning.set_wrap(true);
        warning.add_css_class("warning");
        content.append(&warning);
    }

    window.set_content(Some(&content));
    window.present();
}

fn build_feature_view(feature: &FeatureViewModel) -> gtk::Box {
    let content = gtk::Box::new(Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_tooltip_text(Some(&feature.id));

    let header = gtk::Box::new(Orientation::Horizontal, 8);
    let title = gtk::Label::new(Some(&feature.label));
    title.set_halign(Align::Start);
    title.set_hexpand(true);
    title.add_css_class("heading");
    header.append(&title);

    let lifecycle = gtk::Label::new(Some(feature.lifecycle.label()));
    lifecycle.set_valign(Align::Center);
    lifecycle.add_css_class("dim-label");
    header.append(&lifecycle);

    let status = gtk::Label::new(Some(feature.capability.status.label));
    status.set_valign(Align::Center);
    status.add_css_class("pill");
    status.add_css_class(capability_css_class(feature.capability.status.kind));
    header.append(&status);
    content.append(&header);

    let summary = gtk::Label::new(Some(&feature.capability.summary));
    configure_wrapping_label(&summary);
    content.append(&summary);

    if let Some(detail) = &feature.capability.status.detail {
        content.append(&build_labeled_value("Capability detail", detail));
    }
    if let Some(backend) = &feature.capability.selected_backend {
        content.append(&build_labeled_value("Selected backend", backend));
    }
    if let Some(remediation) = &feature.capability.remediation {
        content.append(&build_remediation_view(remediation));
    }

    content
}

fn build_labeled_value(label: &str, value: &str) -> gtk::Box {
    let content = gtk::Box::new(Orientation::Vertical, 2);
    let heading = gtk::Label::new(Some(label));
    heading.set_halign(Align::Start);
    heading.add_css_class("caption-heading");
    content.append(&heading);

    let value = gtk::Label::new(Some(value));
    configure_wrapping_label(&value);
    value.add_css_class("dim-label");
    content.append(&value);
    content
}

fn build_remediation_view(remediation: &RemediationViewModel) -> gtk::Box {
    let card = gtk::Box::new(Orientation::Horizontal, 10);
    card.add_css_class("card");
    card.set_margin_top(4);

    let icon = gtk::Image::from_icon_name("dialog-information-symbolic");
    icon.set_valign(Align::Start);
    icon.set_margin_top(10);
    icon.set_margin_start(10);
    icon.add_css_class("accent");
    card.append(&icon);

    let content = gtk::Box::new(Orientation::Vertical, 2);
    content.set_hexpand(true);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_end(10);
    let heading = gtk::Label::new(Some(remediation.title));
    heading.set_halign(Align::Start);
    heading.add_css_class("heading");
    content.append(&heading);
    let message = gtk::Label::new(Some(&remediation.message));
    configure_wrapping_label(&message);
    content.append(&message);
    card.append(&content);
    card
}

fn configure_wrapping_label(label: &gtk::Label) {
    label.set_halign(Align::Start);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
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
