//! GTK rendering of the application window.

mod audio;
mod capture;
mod clipboard;
mod command_bar;
mod features;
mod monitor;
mod settings;
mod snippets;
mod widgets;

use adw::prelude::*;
use async_channel::Sender;
use audio::{build_audio_controls, build_microphone_group};
use capture::build_capture_group;
use clipboard::{
    ClipboardPanel, build_clipboard_panel, fill_clipboard_results,
    request_initial_clipboard_listing,
};
use command_bar::{CommandPanel, build_command_panel, fill_command_panel};
use features::{
    build_feature_hub_group, build_quick_toggle_group, build_shortcuts_group, build_warning_group,
};
use gtk::{Align, Orientation, PolicyType, accessible::Property};
use kestrel::{
    ApplicationCommand, ApplicationViewModel, CaptureViewModel, FocusTarget, MicrophoneViewModel,
    MonitorViewModel, PanelSection, ShortcutsViewModel, SpeedTestViewModel,
};
use monitor::{build_monitor_group, build_speed_test_group};
use settings::build_settings_group;
use snippets::{SnippetPanel, build_snippet_panel, fill_snippet_panel};
use widgets::configure_wrapping_label;

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
    monitor_container: std::cell::RefCell<Option<gtk::Box>>,
    microphone_container: std::cell::RefCell<Option<gtk::Box>>,
    speed_test_container: std::cell::RefCell<Option<gtk::Box>>,
    capture_container: std::cell::RefCell<Option<gtk::Box>>,
    shortcuts_container: std::cell::RefCell<Option<gtk::Box>>,
    /// Retained so searches keep text and focus.
    clipboard_panel: std::cell::RefCell<Option<ClipboardPanel>>,
    snippet_panel: std::cell::RefCell<Option<SnippetPanel>>,
    /// Retained so searches keep text and focus.
    command_panel: std::cell::RefCell<Option<CommandPanel>>,
}

impl WindowView {
    pub fn new(
        application: &adw::Application,
        view_model: &ApplicationViewModel,
        commands: Sender<ApplicationCommand>,
    ) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title(kestrel_core::APPLICATION_NAME)
            .default_width(DEFAULT_WINDOW_WIDTH)
            .default_height(DEFAULT_WINDOW_HEIGHT)
            .build();
        window.set_size_request(MINIMUM_WINDOW_WIDTH, MINIMUM_WINDOW_HEIGHT);

        let title = adw::WindowTitle::new(kestrel_core::APPLICATION_NAME, "Feature capabilities");
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
        let page = build_page(view_model, &window, &commands);
        content.set_child(Some(&page.clamp));

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
            monitor_container: std::cell::RefCell::new(page.monitor_container),
            microphone_container: std::cell::RefCell::new(page.microphone_container),
            speed_test_container: std::cell::RefCell::new(page.speed_test_container),
            capture_container: std::cell::RefCell::new(page.capture_container),
            shortcuts_container: std::cell::RefCell::new(Some(page.shortcuts_container)),
            clipboard_panel: std::cell::RefCell::new(page.clipboard_panel),
            snippet_panel: std::cell::RefCell::new(page.snippet_panel),
            command_panel: std::cell::RefCell::new(page.command_panel),
        }
    }

    pub fn set_view_model(&self, view_model: &ApplicationViewModel) {
        let page = build_page(view_model, &self.window, &self.commands);
        self.content.set_child(Some(&page.clamp));
        *self.monitor_container.borrow_mut() = page.monitor_container;
        *self.microphone_container.borrow_mut() = page.microphone_container;
        *self.speed_test_container.borrow_mut() = page.speed_test_container;
        *self.capture_container.borrow_mut() = page.capture_container;
        *self.shortcuts_container.borrow_mut() = Some(page.shortcuts_container);
        *self.clipboard_panel.borrow_mut() = page.clipboard_panel;
        *self.snippet_panel.borrow_mut() = page.snippet_panel;
        *self.command_panel.borrow_mut() = page.command_panel;
    }

    /// Focuses a control; false when its panel is hidden.
    pub fn focus(&self, target: FocusTarget) -> bool {
        let entry = match target {
            FocusTarget::CommandBar => self
                .command_panel
                .borrow()
                .as_ref()
                .map(|panel| panel.query.clone()),
            FocusTarget::Clipboard => self
                .clipboard_panel
                .borrow()
                .as_ref()
                .map(|panel| panel.search.clone()),
        };
        entry.is_some_and(|entry| entry.grab_focus())
    }

    /// Updates the command bar in place.
    pub fn set_command_bar(&self, command_bar: &kestrel::CommandBarViewModel) {
        let panel = self.command_panel.borrow();
        let Some(panel) = panel.as_ref() else {
            return;
        };
        panel.status.set_text(&command_bar.status);
        while let Some(child) = panel.results.first_child() {
            child.unparent();
        }
        fill_command_panel(panel, command_bar, &self.commands);
    }

    /// Updates the clipboard group in place.
    pub fn set_clipboard(&self, clipboard: &kestrel::ClipboardViewModel) {
        let panel = self.clipboard_panel.borrow();
        let Some(panel) = panel.as_ref() else {
            return;
        };
        panel.status.set_text(&clipboard.status);
        while let Some(child) = panel.results.first_child() {
            child.unparent();
        }
        fill_clipboard_results(panel, clipboard, &self.commands);
        // Background capture may create entries before the first search.
        request_initial_clipboard_listing(clipboard, &self.commands);
    }

    /// Updates the snippet group in place.
    pub fn set_snippets(&self, snippets: &kestrel::SnippetsViewModel) {
        let panel = self.snippet_panel.borrow();
        let Some(panel) = panel.as_ref() else {
            return;
        };
        panel.status.set_text(&snippets.status);
        panel.insertion.set_text(&snippets.insertion_status);
        while let Some(child) = panel.results.first_child() {
            child.unparent();
        }
        while let Some(child) = panel.editor.first_child() {
            child.unparent();
        }
        fill_snippet_panel(panel, snippets, &self.commands);
    }

    pub fn set_monitor(&self, monitor: &MonitorViewModel) {
        let Some(container) = self.monitor_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_monitor_group(monitor));
    }

    /// Replaces the microphone group.
    pub fn set_microphone(&self, microphone: &MicrophoneViewModel) {
        let Some(container) = self.microphone_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_microphone_group(microphone, &self.commands));
    }

    /// Replaces the speed-test group.
    pub fn set_speed_test(&self, speed_test: &SpeedTestViewModel) {
        let Some(container) = self.speed_test_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_speed_test_group(speed_test, &self.commands));
    }

    /// Replaces the capture group.
    pub fn set_capture(&self, capture: &CaptureViewModel) {
        let Some(container) = self.capture_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_capture_group(capture, &self.window, &self.commands));
    }

    /// Replaces the global-shortcuts group.
    pub fn set_shortcuts(&self, shortcuts: &ShortcutsViewModel) {
        let Some(container) = self.shortcuts_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_shortcuts_group(shortcuts));
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
        let toast = adw::Toast::new(message);
        toast.set_use_markup(false);
        self.toasts.add_toast(toast);
    }
}

struct PageBuild {
    clamp: adw::Clamp,
    monitor_container: Option<gtk::Box>,
    microphone_container: Option<gtk::Box>,
    speed_test_container: Option<gtk::Box>,
    capture_container: Option<gtk::Box>,
    shortcuts_container: gtk::Box,
    clipboard_panel: Option<ClipboardPanel>,
    snippet_panel: Option<SnippetPanel>,
    command_panel: Option<CommandPanel>,
}

fn build_page(
    view_model: &ApplicationViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> PageBuild {
    let page = gtk::Box::new(Orientation::Vertical, 24);
    page.set_margin_top(24);
    page.set_margin_bottom(24);
    page.set_margin_start(18);
    page.set_margin_end(18);

    let heading = gtk::Label::new(Some(kestrel_core::APPLICATION_NAME));
    heading.add_css_class("title-1");
    heading.set_halign(Align::Start);
    heading.set_wrap(true);
    heading.set_xalign(0.0);
    page.append(&heading);

    let description = gtk::Label::new(Some(
        "Kestrel keeps registered features visible, explains unavailable platform paths, and lets you change presentation preferences without hiding warnings.",
    ));
    configure_wrapping_label(&description);
    description.add_css_class("dim-label");
    page.append(&description);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search settings and features")
        .hexpand(true)
        .build();
    page.append(&build_settings_group(view_model, window, commands, &search));
    let shortcuts_container = gtk::Box::new(Orientation::Vertical, 0);
    shortcuts_container.append(&build_shortcuts_group(&view_model.shortcuts));
    page.append(&shortcuts_container);
    if !view_model.warnings.is_empty() {
        page.append(&build_warning_group(view_model));
    }
    let mut monitor_container = None;
    let mut microphone_container = None;
    let mut speed_test_container = None;
    let mut capture_container = None;
    let mut clipboard_panel = None;
    let mut snippet_panel = None;
    let mut command_panel = None;
    for panel in &view_model.panel_sections {
        if !panel.visible {
            continue;
        }
        match panel.section {
            PanelSection::QuickControls => {
                let panel = build_command_panel(&view_model.command_bar, commands);
                page.append(&panel.container);
                command_panel = Some(panel);
                if !view_model.quick_toggles.is_empty() {
                    page.append(&build_quick_toggle_group(
                        &view_model.quick_toggles,
                        window,
                        commands,
                    ));
                }
                page.append(&build_audio_controls(&view_model.audio, commands));
                let microphone = gtk::Box::new(Orientation::Vertical, 0);
                microphone.append(&build_microphone_group(&view_model.microphone, commands));
                page.append(&microphone);
                microphone_container = Some(microphone);
                let capture = gtk::Box::new(Orientation::Vertical, 0);
                capture.append(&build_capture_group(&view_model.capture, window, commands));
                page.append(&capture);
                capture_container = Some(capture);
                let panel = build_clipboard_panel(&view_model.clipboard, commands);
                page.append(&panel.container);
                clipboard_panel = Some(panel);
                let panel = build_snippet_panel(&view_model.snippets, commands);
                page.append(&panel.container);
                snippet_panel = Some(panel);
            }
            PanelSection::FeatureHub => {
                page.append(&build_feature_hub_group(
                    &view_model.features,
                    commands,
                    &search,
                ));
            }
            PanelSection::Monitoring => {
                let container = gtk::Box::new(Orientation::Vertical, 0);
                container.append(&build_monitor_group(&view_model.monitor));
                let speed_test = gtk::Box::new(Orientation::Vertical, 0);
                speed_test.set_margin_top(24);
                speed_test.append(&build_speed_test_group(&view_model.speed_test, commands));
                container.append(&speed_test);
                page.append(&container);
                monitor_container = Some(container);
                speed_test_container = Some(speed_test);
            }
        }
    }

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(CONTENT_MAXIMUM_WIDTH);
    clamp.set_tightening_threshold(MINIMUM_WINDOW_WIDTH);
    clamp.set_child(Some(&page));
    PageBuild {
        clamp,
        monitor_container,
        microphone_container,
        speed_test_container,
        capture_container,
        shortcuts_container,
        clipboard_panel,
        snippet_panel,
        command_panel,
    }
}
