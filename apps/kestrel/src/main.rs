use adw::prelude::*;
use gtk::{Align, Orientation};
use kestrel::{
    configuration_path, load, ApplicationRuntime, ApplicationViewModel, LoadedConfiguration,
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
        let row_content = gtk::Box::new(Orientation::Vertical, 4);
        row_content.set_margin_top(10);
        row_content.set_margin_bottom(10);
        row_content.set_margin_start(12);
        row_content.set_margin_end(12);
        let title = gtk::Label::new(Some(&format!(
            "{} — {}",
            feature.label,
            feature.lifecycle.label()
        )));
        title.set_halign(Align::Start);
        title.add_css_class("heading");
        row_content.append(&title);
        let capability = gtk::Label::new(Some(feature.capability.status.label));
        capability.set_halign(Align::Start);
        capability.add_css_class("caption");
        row_content.append(&capability);
        let detail = gtk::Label::new(Some(&feature.capability.summary));
        detail.set_halign(Align::Start);
        detail.set_wrap(true);
        row_content.append(&detail);
        if let Some(status_detail) = &feature.capability.status.detail {
            let status_detail = gtk::Label::new(Some(status_detail));
            status_detail.set_halign(Align::Start);
            status_detail.set_wrap(true);
            status_detail.add_css_class("dim-label");
            row_content.append(&status_detail);
        }
        if let Some(backend) = &feature.capability.selected_backend {
            let backend = gtk::Label::new(Some(&format!("Backend: {backend}")));
            backend.set_halign(Align::Start);
            backend.set_wrap(true);
            backend.add_css_class("dim-label");
            row_content.append(&backend);
        }
        if let Some(remediation) = &feature.capability.remediation {
            let remediation = gtk::Label::new(Some(remediation));
            remediation.set_halign(Align::Start);
            remediation.set_wrap(true);
            remediation.add_css_class("dim-label");
            row_content.append(&remediation);
        }
        features.append(&row_content);
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
