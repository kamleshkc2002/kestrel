use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, PolicyType, accessible::Property};
use kestrel::{
    AlertKind, ApplicationCommand, ApplicationViewModel, AudioCycleDirection, AudioOutputViewModel,
    AudioStreamViewModel, AudioViewModel, ConfirmationViewModel, FeatureViewModel,
    MicrophoneViewModel, MonitorViewModel, PanelMoveDirection, PanelSection, QuickToggleCommand,
    QuickToggleControlViewModel, QuickToggleMutation, QuickToggleViewModel, ShortcutsViewModel,
    SpeedTestViewModel,
};
use kestrel_core::AppearancePreference;

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
    shortcuts_container: std::cell::RefCell<Option<gtk::Box>>,
    /// Retained so searches keep text and focus.
    clipboard_panel: std::cell::RefCell<Option<ClipboardPanel>>,
    snippet_panel: std::cell::RefCell<Option<SnippetPanel>>,
    /// Retained so searches keep text and focus.
    command_panel: std::cell::RefCell<Option<CommandPanel>>,
}

/// Reusable command-bar widgets.
struct CommandPanel {
    container: gtk::Box,
    status: gtk::Label,
    results: gtk::Box,
}

/// Reusable snippet widgets.
struct SnippetPanel {
    container: gtk::Box,
    status: gtk::Label,
    insertion: gtk::Label,
    draft_label: gtk::Label,
    editor: gtk::Box,
    results: gtk::Box,
}

/// Reusable clipboard widgets; search state survives row refreshes.
struct ClipboardPanel {
    container: gtk::Box,
    status: gtk::Label,
    results: gtk::Box,
    selected: std::rc::Rc<std::cell::RefCell<std::collections::BTreeSet<u64>>>,
}

/// Blocks searches triggered by programmatic updates.
type SearchGuard = std::rc::Rc<std::cell::Cell<bool>>;

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
        *self.shortcuts_container.borrow_mut() = Some(page.shortcuts_container);
        *self.clipboard_panel.borrow_mut() = page.clipboard_panel;
        *self.snippet_panel.borrow_mut() = page.snippet_panel;
        *self.command_panel.borrow_mut() = page.command_panel;
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
        self.toasts.add_toast(adw::Toast::new(message));
    }
}

struct PageBuild {
    clamp: adw::Clamp,
    monitor_container: Option<gtk::Box>,
    microphone_container: Option<gtk::Box>,
    speed_test_container: Option<gtk::Box>,
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

    let heading = gtk::Label::new(Some("Kestrel"));
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
        shortcuts_container,
        clipboard_panel,
        snippet_panel,
        command_panel,
    }
}

fn build_settings_group(
    view_model: &ApplicationViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    search: &gtk::SearchEntry,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Settings")
        .description("Presentation and startup preferences. Search filters this content and the feature hub.")
        .build();
    let mut filter_rows = Vec::<(gtk::Widget, String)>::new();

    let search_row = adw::ActionRow::new();
    search_row.set_title("Search");
    search_row.add_suffix(search);
    search_row.set_activatable_widget(Some(search));
    group.add(&search_row);

    let appearance = adw::ComboRow::builder()
        .title("Appearance")
        .subtitle("Choose the application color scheme.")
        .model(&gtk::StringList::new(&["System", "Light", "Dark"]))
        .selected(match view_model.appearance {
            AppearancePreference::System => 0,
            AppearancePreference::Light => 1,
            AppearancePreference::Dark => 2,
        })
        .build();
    let sender = commands.clone();
    appearance.connect_selected_notify(move |row| {
        let preference = match row.selected() {
            1 => AppearancePreference::Light,
            2 => AppearancePreference::Dark,
            _ => AppearancePreference::System,
        };
        let _ = sender.try_send(ApplicationCommand::SetAppearance(preference));
    });
    filter_rows.push((
        appearance.clone().upcast(),
        "appearance color scheme system light dark".to_owned(),
    ));
    group.add(&appearance);

    let autostart = adw::ActionRow::builder()
        .title("Start automatically")
        .subtitle("Launch Kestrel when your desktop session starts.")
        .build();
    let autostart_switch = gtk::Switch::builder()
        .active(view_model.autostart)
        .valign(Align::Center)
        .build();
    let sender = commands.clone();
    autostart_switch.connect_state_set(move |_, enabled| {
        let _ = sender.try_send(ApplicationCommand::SetAutostart(enabled));
        gtk::glib::Propagation::Proceed
    });
    autostart.add_suffix(&autostart_switch);
    autostart.set_activatable_widget(Some(&autostart_switch));
    filter_rows.push((
        autostart.clone().upcast(),
        "start automatically autostart startup session".to_owned(),
    ));
    group.add(&autostart);

    let preset_row = adw::ActionRow::builder()
        .title("Feature presets")
        .subtitle("Change enablement as one reversible operation.")
        .subtitle_lines(0)
        .build();
    let presets = gtk::Box::new(Orientation::Horizontal, 6);
    for (label, preset) in [
        ("Essentials", kestrel::FeaturePreset::Essentials),
        ("Balanced", kestrel::FeaturePreset::Balanced),
        ("Everything", kestrel::FeaturePreset::Everything),
    ] {
        let button = gtk::Button::with_label(label);
        button.set_tooltip_text(Some(&format!("Apply the {label} feature preset")));
        let sender = commands.clone();
        button.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::ApplyPreset(preset));
        });
        presets.append(&button);
    }
    let undo = gtk::Button::with_label("Undo");
    undo.set_sensitive(view_model.can_undo);
    let sender = commands.clone();
    undo.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::UndoPreset);
    });
    presets.append(&undo);
    preset_row.add_suffix(&presets);
    filter_rows.push((
        preset_row.clone().upcast(),
        "feature presets essentials balanced everything undo enablement".to_owned(),
    ));
    group.add(&preset_row);

    let panel_group = adw::PreferencesGroup::builder()
        .title("Panel sections")
        .description(
            "Choose visibility and keyboard-accessible order for Quick Controls, Feature Hub, and Monitoring.",
        )
        .build();
    for (index, panel) in view_model.panel_sections.iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(panel_title(panel.section))
            .subtitle("Visible in the main panel")
            .build();
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let visibility = gtk::Switch::builder()
            .active(panel.visible)
            .valign(Align::Center)
            .build();
        let panel_name = panel_title(panel.section);
        let visibility_label = format!("Show {panel_name}");
        let visibility_description = format!("Show or hide the {panel_name} panel");
        visibility.update_property(&[
            Property::Label(&visibility_label),
            Property::Description(&visibility_description),
        ]);
        let sender = commands.clone();
        let section = panel.section;
        visibility.connect_state_set(move |_, visible| {
            let _ = sender.try_send(ApplicationCommand::SetPanelVisibility { section, visible });
            gtk::glib::Propagation::Proceed
        });
        let up = gtk::Button::builder().icon_name("go-up-symbolic").build();
        let up_label = format!("Move {panel_name} up");
        up.update_property(&[Property::Label(&up_label), Property::Description(&up_label)]);
        up.set_tooltip_text(Some(&up_label));
        up.set_sensitive(index > 0);
        let down = gtk::Button::builder().icon_name("go-down-symbolic").build();
        let down_label = format!("Move {panel_name} down");
        down.update_property(&[
            Property::Label(&down_label),
            Property::Description(&down_label),
        ]);
        down.set_tooltip_text(Some(&down_label));
        down.set_sensitive(index + 1 < view_model.panel_sections.len());
        let sender = commands.clone();
        up.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::MovePanelSection {
                section,
                direction: PanelMoveDirection::Up,
            });
        });
        let sender = commands.clone();
        down.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::MovePanelSection {
                section,
                direction: PanelMoveDirection::Down,
            });
        });
        controls.append(&visibility);
        controls.append(&up);
        controls.append(&down);
        row.add_suffix(&controls);
        row.set_activatable_widget(Some(&visibility));
        panel_group.add(&row);
        filter_rows.push((
            row.clone().upcast(),
            format!("{} panel visibility order", panel_title(panel.section)),
        ));
    }
    group.add(&panel_group);

    let readout_group = adw::PreferencesGroup::builder()
        .title("Monitoring readouts")
        .description("Choose visible readouts and their order in the Monitoring panel.")
        .build();
    for setting in &view_model.monitor.readout_settings {
        let row = adw::ActionRow::builder()
            .title(setting.label)
            .subtitle(if setting.visible {
                "Show this readout and move it in the configured order."
            } else {
                "Hidden; show it to include it in the Monitoring panel."
            })
            .build();
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let visibility = gtk::Switch::builder()
            .active(setting.visible)
            .valign(Align::Center)
            .build();
        let visibility_label = format!("Show {} monitoring readout", setting.label);
        visibility.update_property(&[
            Property::Label(&visibility_label),
            Property::Description(&visibility_label),
        ]);
        visibility.set_tooltip_text(Some(&visibility_label));
        let sender = commands.clone();
        let readout = setting.readout;
        visibility.connect_state_set(move |_, visible| {
            let _ =
                sender.try_send(ApplicationCommand::SetMonitorReadoutVisible { readout, visible });
            gtk::glib::Propagation::Proceed
        });
        let up = gtk::Button::builder().icon_name("go-up-symbolic").build();
        let up_label = format!("Move {} readout up", setting.label);
        up.update_property(&[Property::Label(&up_label), Property::Description(&up_label)]);
        up.set_tooltip_text(Some(&up_label));
        up.set_sensitive(setting.can_move_up);
        let sender = commands.clone();
        up.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::MoveMonitorReadout {
                readout,
                direction: PanelMoveDirection::Up,
            });
        });
        let down = gtk::Button::builder().icon_name("go-down-symbolic").build();
        let down_label = format!("Move {} readout down", setting.label);
        down.update_property(&[
            Property::Label(&down_label),
            Property::Description(&down_label),
        ]);
        down.set_tooltip_text(Some(&down_label));
        down.set_sensitive(setting.can_move_down);
        let sender = commands.clone();
        down.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::MoveMonitorReadout {
                readout,
                direction: PanelMoveDirection::Down,
            });
        });
        controls.append(&visibility);
        controls.append(&up);
        controls.append(&down);
        row.add_suffix(&controls);
        row.set_activatable_widget(Some(&visibility));
        row.update_property(&[Property::Label(&format!(
            "Monitoring readout {}",
            setting.label
        ))]);
        readout_group.add(&row);
        filter_rows.push((
            row.clone().upcast(),
            format!("monitoring readouts {} order visibility", setting.label),
        ));
    }
    group.add(&readout_group);

    let alert_group = adw::PreferencesGroup::builder()
        .title("Monitoring alerts")
        .description("Enable sustained threshold alerts and choose each threshold.")
        .build();
    for rule in &view_model.monitor.alert_rules {
        let row = adw::ActionRow::builder()
            .title(format!("{} alerts", rule.label))
            .subtitle(&rule.summary)
            .build();
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let enabled = gtk::Switch::builder()
            .active(rule.enabled)
            .valign(Align::Center)
            .build();
        let enabled_label = format!("Enable {} alerts", rule.label);
        enabled.update_property(&[
            Property::Label(&enabled_label),
            Property::Description(&enabled_label),
        ]);
        enabled.set_tooltip_text(Some(&enabled_label));
        let sender = commands.clone();
        let kind = rule.kind;
        enabled.connect_state_set(move |_, value| {
            let _ = sender.try_send(ApplicationCommand::SetAlertEnabled {
                kind,
                enabled: value,
            });
            gtk::glib::Propagation::Proceed
        });
        let maximum = if kind == AlertKind::Temperature {
            150.0
        } else {
            100.0
        };
        let spin = adw::SpinRow::with_range(1.0, maximum, 1.0);
        spin.set_value(rule.threshold);
        let unit_label = gtk::Label::new(Some(rule.unit));
        unit_label.set_valign(Align::Center);
        spin.add_suffix(&unit_label);
        spin.set_numeric(true);
        spin.update_property(&[
            Property::Label(&format!("{} alert threshold", rule.label)),
            Property::Description(&rule.summary),
        ]);
        spin.set_tooltip_text(Some(&rule.summary));
        let sender = commands.clone();
        spin.connect_value_notify(move |spin| {
            let _ = sender.try_send(ApplicationCommand::SetAlertThreshold {
                kind,
                threshold: spin.value(),
            });
        });
        controls.append(&enabled);
        row.add_suffix(&controls);
        row.set_activatable_widget(Some(&enabled));
        alert_group.add(&row);
        alert_group.add(&spin);
        filter_rows.push((
            row.clone().upcast(),
            format!("monitoring alerts {} threshold", rule.label),
        ));
        filter_rows.push((
            spin.clone().upcast(),
            format!("monitoring alerts {} threshold", rule.label),
        ));
    }
    group.add(&alert_group);

    let audio = &view_model.audio;
    let audio_group = adw::PreferencesGroup::builder()
        .title("Audio")
        .description(
            "Mixer policy: how loud amplification may go, what an output switch moves, and how \
             lost output devices are handled.",
        )
        .build();
    let boost = adw::SpinRow::with_range(
        f64::from(kestrel_core::UNAMPLIFIED_AUDIO_VOLUME_PERCENT),
        f64::from(audio.policy.max_boost_percent),
        5.0,
    );
    boost.set_title("Boost ceiling");
    boost.set_subtitle(&format!(
        "The largest volume the mixer requests. Above {}% PulseAudio amplifies in software; the \
         hard cap is {}%.",
        kestrel_core::UNAMPLIFIED_AUDIO_VOLUME_PERCENT,
        audio.policy.max_boost_percent
    ));
    boost.set_value(f64::from(audio.policy.boost_percent));
    boost.set_numeric(true);
    let boost_unit = gtk::Label::new(Some("%"));
    boost_unit.set_valign(Align::Center);
    boost.add_suffix(&boost_unit);
    boost.update_property(&[
        Property::Label("Audio boost ceiling"),
        Property::Description("The largest volume the mixer requests, bounded by the hard cap"),
    ]);
    let sender = commands.clone();
    boost.connect_value_notify(move |spin| {
        let _ = sender.try_send(ApplicationCommand::SetAudioBoostPercent(spin.value() as u8));
    });
    audio_group.add(&boost);
    filter_rows.push((
        boost.clone().upcast(),
        "audio mixer boost ceiling amplification volume".to_owned(),
    ));

    let move_streams = adw::ActionRow::builder()
        .title("Move streams with the output")
        .subtitle(format!(
            "When switching the default output, also move playing streams. Current mode: {}.",
            audio.policy.output_switch_label
        ))
        .subtitle_lines(0)
        .build();
    let move_streams_switch = gtk::Switch::builder()
        .active(audio.policy.move_all_streams)
        .valign(Align::Center)
        .build();
    let move_streams_label = "Move playing streams when the default output changes";
    move_streams_switch.update_property(&[
        Property::Label(move_streams_label),
        Property::Description("Move every playing stream to the newly selected output"),
    ]);
    let sender = commands.clone();
    move_streams_switch.connect_state_set(move |_, moved| {
        let mode = if moved {
            kestrel::AudioOutputSwitch::AllStreams
        } else {
            kestrel::AudioOutputSwitch::DefaultOutput
        };
        let _ = sender.try_send(ApplicationCommand::SetAudioOutputSwitch(mode));
        gtk::glib::Propagation::Proceed
    });
    move_streams.add_suffix(&move_streams_switch);
    move_streams.set_activatable_widget(Some(&move_streams_switch));
    audio_group.add(&move_streams);
    filter_rows.push((
        move_streams.clone().upcast(),
        "audio output switch move streams routing".to_owned(),
    ));

    let disconnect = adw::ActionRow::builder()
        .title("Reset volume after output loss")
        .subtitle(format!(
            "Reapply a fixed volume when a stream loses its output device. Current policy: {}.",
            audio.policy.disconnect_policy_label
        ))
        .subtitle_lines(0)
        .build();
    let disconnect_switch = gtk::Switch::builder()
        .active(audio.policy.reset_volume_on_disconnect)
        .valign(Align::Center)
        .build();
    let disconnect_label = "Reset a stream volume when its output disappears";
    disconnect_switch.update_property(&[
        Property::Label(disconnect_label),
        Property::Description("Choose whether output loss preserves or resets the stream volume"),
    ]);
    let sender = commands.clone();
    disconnect_switch.connect_state_set(move |_, reset| {
        let policy = if reset {
            kestrel::AudioDisconnectPolicy::ResetVolume
        } else {
            kestrel::AudioDisconnectPolicy::PreserveVolume
        };
        let _ = sender.try_send(ApplicationCommand::SetAudioDisconnectPolicy(policy));
        gtk::glib::Propagation::Proceed
    });
    disconnect.add_suffix(&disconnect_switch);
    disconnect.set_activatable_widget(Some(&disconnect_switch));
    audio_group.add(&disconnect);
    filter_rows.push((
        disconnect.clone().upcast(),
        "audio disconnect policy output loss volume".to_owned(),
    ));

    let disconnect_volume = adw::SpinRow::with_range(0.0, 100.0, 5.0);
    disconnect_volume.set_title("Disconnect volume");
    disconnect_volume
        .set_subtitle("Applied to a stream after output loss when the reset policy is active.");
    disconnect_volume.set_value(f64::from(audio.policy.disconnect_volume_percent));
    disconnect_volume.set_numeric(true);
    let disconnect_unit = gtk::Label::new(Some("%"));
    disconnect_unit.set_valign(Align::Center);
    disconnect_volume.add_suffix(&disconnect_unit);
    disconnect_volume.update_property(&[
        Property::Label("Disconnect volume"),
        Property::Description("The volume reapplied after output loss, from 0 to 100 percent"),
    ]);
    let sender = commands.clone();
    disconnect_volume.connect_value_notify(move |spin| {
        let _ = sender.try_send(ApplicationCommand::SetAudioDisconnectVolumePercent(
            spin.value() as u8,
        ));
    });
    audio_group.add(&disconnect_volume);
    filter_rows.push((
        disconnect_volume.clone().upcast(),
        "audio disconnect volume output loss".to_owned(),
    ));

    let inactive = adw::ActionRow::builder()
        .title("Show inactive streams")
        .subtitle("List idle or corked streams next to playing ones.")
        .build();
    let inactive_switch = gtk::Switch::builder()
        .active(audio.policy.include_inactive_streams)
        .valign(Align::Center)
        .build();
    let inactive_label = "Show inactive playback streams";
    inactive_switch.update_property(&[
        Property::Label(inactive_label),
        Property::Description("Show or hide corked and idle playback streams"),
    ]);
    inactive_switch.set_tooltip_text(Some(inactive_label));
    let sender = commands.clone();
    inactive_switch.connect_state_set(move |_, include| {
        let _ = sender.try_send(ApplicationCommand::SetAudioIncludeInactiveStreams(include));
        gtk::glib::Propagation::Proceed
    });
    inactive.add_suffix(&inactive_switch);
    inactive.set_activatable_widget(Some(&inactive_switch));
    audio_group.add(&inactive);
    filter_rows.push((
        inactive.clone().upcast(),
        "audio inactive streams corked idle filter".to_owned(),
    ));

    group.add(&audio_group);

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
        "Seconds after Kestrel takes the selection before the live clipboard is cleared.          Saved entries are kept; 0 disables it.",
    );
    clear.set_value(clipboard.policy.clear_seconds as f64);
    clear.set_numeric(true);
    push_limit(
        &clear,
        kestrel::ClipboardLimit::ClearSeconds(clipboard.policy.clear_seconds),
        "Clipboard automatic selection clear interval",
    );
    clipboard_group.add(&clear);

    let filter = adw::ActionRow::builder()
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

    let patterns = adw::ActionRow::builder()
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

    let paste = adw::ActionRow::builder()
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

    let io_row = adw::ActionRow::builder()
        .title("Configuration files")
        .subtitle("Import or export configuration. Kestrel only emits the selected path command.")
        .subtitle_lines(0)
        .build();
    let io_buttons = gtk::Box::new(Orientation::Horizontal, 6);
    let import = gtk::Button::with_label("Import");
    connect_file_chooser(&import, window, commands, false);
    let export = gtk::Button::with_label("Export");
    connect_file_chooser(&export, window, commands, true);
    io_buttons.append(&import);
    io_buttons.append(&export);
    io_row.add_suffix(&io_buttons);
    filter_rows.push((
        io_row.clone().upcast(),
        "configuration files import export".to_owned(),
    ));
    let filter_rows = std::rc::Rc::new(filter_rows);
    search.connect_search_changed(move |entry| {
        let query = entry.text().trim().to_lowercase();
        for (row, content) in filter_rows.iter() {
            row.set_visible(query.is_empty() || content.contains(&query));
        }
    });
    group.add(&io_row);
    group
}

fn panel_title(section: PanelSection) -> &'static str {
    match section {
        PanelSection::QuickControls => "Quick Controls",
        PanelSection::FeatureHub => "Feature Hub",
        PanelSection::Monitoring => "Monitoring",
    }
}

fn connect_file_chooser(
    button: &gtk::Button,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    save: bool,
) {
    let window = window.clone();
    let sender = commands.clone();
    button.connect_clicked(move |_| {
        let action = if save {
            gtk::FileChooserAction::Save
        } else {
            gtk::FileChooserAction::Open
        };
        let chooser = gtk::FileChooserNative::builder()
            .title(if save {
                "Export configuration"
            } else {
                "Import configuration"
            })
            .accept_label(if save { "Export" } else { "Import" })
            .cancel_label("Cancel")
            .transient_for(&window)
            .action(action)
            .build();
        let sender = sender.clone();
        chooser.connect_response(move |chooser, response| {
            if response == gtk::ResponseType::Accept {
                if let Some(path) = chooser.file().and_then(|file| file.path()) {
                    let command = if save {
                        ApplicationCommand::ExportConfiguration(path)
                    } else {
                        ApplicationCommand::ImportConfiguration(path)
                    };
                    let _ = sender.try_send(command);
                }
            }
            chooser.destroy();
        });
        chooser.show();
    });
}

/// Builds microphone controls; mixed mute state leaves the switch usable.
fn build_microphone_group(
    microphone: &MicrophoneViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Microphone")
        .description(&microphone.status)
        .build();
    if !microphone.running {
        let row = adw::ActionRow::builder()
            .title("Microphone control is not running")
            .subtitle(&microphone.status)
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        group.add(&row);
        return group;
    }

    let controllable = microphone.muted.is_some() || microphone.mixed;
    let row = adw::ActionRow::builder()
        .title("Mute all inputs")
        .subtitle(&microphone.mute_label)
        .build();
    let mute = gtk::Switch::builder()
        .active(microphone.muted.unwrap_or(false))
        .valign(Align::Center)
        .sensitive(controllable)
        .build();
    mute.update_property(&[Property::Label("Mute all microphone inputs")]);
    mute.set_tooltip_text(Some("Also available as Ctrl+Shift+M and from the tray"));
    let sender = commands.clone();
    mute.connect_state_set(move |_, muted| {
        let _ = sender.try_send(ApplicationCommand::Microphone(
            kestrel::MicrophoneCommand::SetMuted(muted),
        ));
        // The next backend refresh supplies the switch state.
        gtk::glib::Propagation::Stop
    });
    row.add_suffix(&mute);
    row.set_activatable_widget(Some(&mute));
    group.add(&row);

    if !microphone.inputs.is_empty() {
        let labels = microphone
            .inputs
            .iter()
            .map(|input| input.label.as_str())
            .collect::<Vec<_>>();
        let model = gtk::StringList::new(&labels);
        let selected = microphone
            .default_input_id
            .and_then(|id| microphone.inputs.iter().position(|input| input.id == id))
            .and_then(|position| u32::try_from(position).ok())
            .unwrap_or(gtk::INVALID_LIST_POSITION);
        let combo = adw::ComboRow::builder()
            .title("Default input")
            .subtitle(if microphone.default_input_id.is_some() {
                "The audio server records from this input by default"
            } else {
                "No input is the server default"
            })
            .model(&model)
            .selected(selected)
            .build();
        combo.update_property(&[Property::Label("Default microphone input")]);
        let ids = microphone
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>();
        let sender = commands.clone();
        combo.connect_selected_notify(move |combo| {
            let Some(input_id) = usize::try_from(combo.selected())
                .ok()
                .and_then(|position| ids.get(position))
            else {
                return;
            };
            let _ = sender.try_send(ApplicationCommand::Microphone(
                kestrel::MicrophoneCommand::SetDefaultInput {
                    input_id: *input_id,
                },
            ));
        });
        group.add(&combo);
    }
    if let Some(message) = &microphone.message {
        let notice = adw::ActionRow::builder()
            .title("Input disconnected")
            .subtitle(message)
            .subtitle_lines(0)
            .build();
        group.add(&notice);
    }
    group
}

/// Builds the global-shortcuts group: one row per configured binding.
fn build_shortcuts_group(shortcuts: &ShortcutsViewModel) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Global shortcuts")
        .description(&shortcuts.status)
        .build();
    for row in &shortcuts.rows {
        let item = adw::ActionRow::builder()
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
            &adw::ActionRow::builder()
                .title("Some bindings were skipped")
                .subtitle(notice)
                .subtitle_lines(0)
                .build(),
        );
    }
    group
}

/// Builds the speed-test group.
fn build_speed_test_group(
    speed_test: &SpeedTestViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Network speed test")
        .description(&speed_test.disclosure)
        .build();
    let row = adw::ActionRow::builder()
        .title("Speed test")
        .subtitle(&speed_test.status)
        .subtitle_lines(0)
        .build();
    let start = gtk::Button::with_label("Start");
    start.set_valign(Align::Center);
    start.set_sensitive(speed_test.can_start);
    start.update_property(&[Property::Label("Start network speed test")]);
    let sender = commands.clone();
    start.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::StartSpeedTest);
    });
    let cancel = gtk::Button::with_label("Cancel");
    cancel.set_valign(Align::Center);
    cancel.set_sensitive(speed_test.can_cancel);
    cancel.update_property(&[Property::Label("Cancel network speed test")]);
    let sender = commands.clone();
    cancel.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::CancelSpeedTest);
    });
    row.add_suffix(&start);
    row.add_suffix(&cancel);
    group.add(&row);
    if let Some(progress) = speed_test.progress {
        let bar = gtk::ProgressBar::builder()
            .fraction(progress)
            .show_text(true)
            .margin_top(6)
            .build();
        bar.set_text(speed_test.phase_label.as_deref());
        group.add(&bar);
    }
    for line in &speed_test.result_lines {
        group.add(&adw::ActionRow::builder().title(line).build());
    }
    group
}

fn build_monitor_group(monitor: &MonitorViewModel) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Monitoring")
        .description(&monitor.status)
        .build();
    if monitor.readouts.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No monitoring readouts configured")
            .subtitle("Enable at least one readout in Settings → Monitoring readouts.")
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("No monitoring readouts are configured")]);
        group.add(&row);
    } else {
        for readout in &monitor.readouts {
            let row = adw::ActionRow::builder()
                .title(readout.label)
                .subtitle(&readout.detail)
                .subtitle_lines(0)
                .sensitive(readout.value.is_some())
                .build();
            if let Some(value) = &readout.value {
                let value_label = gtk::Label::new(Some(value));
                value_label.add_css_class("numeric");
                value_label.set_valign(Align::Center);
                row.add_suffix(&value_label);
            }
            row.update_property(&[Property::Label(&format!(
                "{}: {}",
                readout.label,
                readout.value.as_deref().unwrap_or("Unavailable")
            ))]);
            group.add(&row);
        }
    }
    if !monitor.alerts.is_empty() || !monitor.delivery_failures.is_empty() {
        let heading = adw::ActionRow::builder()
            .title("Active alerts")
            .subtitle("Sustained threshold alerts currently active or with delivery failures.")
            .subtitle_lines(0)
            .build();
        heading.update_property(&[Property::Label("Active alerts")]);
        group.add(&heading);
        for alert in &monitor.alerts {
            let row = adw::ActionRow::builder()
                .title(alert.label)
                .subtitle(&alert.message)
                .subtitle_lines(0)
                .build();
            row.update_property(&[Property::Label(&format!(
                "{} alert: {}",
                alert.label, alert.message
            ))]);
            group.add(&row);
        }
        for (kind, message) in &monitor.delivery_failures {
            let row = adw::ActionRow::builder()
                .title(format!("{} alert delivery failed", kind.label()))
                .subtitle(message)
                .subtitle_lines(0)
                .sensitive(false)
                .build();
            row.update_property(&[Property::Label(&format!(
                "{} alert delivery failed: {}",
                kind.label(),
                message
            ))]);
            group.add(&row);
        }
    }

    group
}

/// Requests an initial empty search when retained entries lack results.
fn request_initial_clipboard_listing(
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    if clipboard.running
        && clipboard.search_query.is_empty()
        && clipboard.items.is_empty()
        && clipboard.total_items > 0
    {
        let _ = commands.try_send(ApplicationCommand::ClipboardSearch(String::new()));
    }
}

/// Builds the retained clipboard group.
fn build_clipboard_panel(
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) -> ClipboardPanel {
    let container = gtk::Box::new(Orientation::Vertical, 12);
    let group = adw::PreferencesGroup::builder()
        .title("Clipboard")
        .description(
            "Memory-only retention. Content is never written to disk, and the window only \
             renders entries you ask to see.",
        )
        .build();

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search retained entries")
        .hexpand(true)
        .build();
    search.update_property(&[
        Property::Label("Search clipboard history"),
        Property::Description(
            "Searches retained text and file paths plus image labels; the result list updates \
             after you pause typing.",
        ),
    ]);
    let search_row = adw::ActionRow::new();
    search_row.set_title("Search");
    search_row
        .set_subtitle("Text and file paths match case-insensitively; images match on their label.");
    search_row.add_suffix(&search);
    search_row.set_activatable_widget(Some(&search));
    let sender = commands.clone();
    // Guard programmatic field updates from triggering searches.
    let guard: SearchGuard = std::rc::Rc::new(std::cell::Cell::new(false));
    let handler_guard = std::rc::Rc::clone(&guard);
    search.connect_search_changed(move |entry| {
        if handler_guard.get() {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::ClipboardSearch(
            entry.text().to_string(),
        ));
    });
    if !clipboard.search_query.is_empty() {
        guard.set(true);
        search.set_text(&clipboard.search_query);
        guard.set(false);
    }
    group.add(&search_row);

    let status = gtk::Label::new(Some(&clipboard.status));
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    let status_row = adw::ActionRow::new();
    status_row.set_title("Retained state");
    status_row.add_suffix(&status);
    group.add(&status_row);

    let actions = adw::ActionRow::builder()
        .title("History actions")
        .subtitle(
            "Clear the live selection without touching entries, or wipe every retained entry.",
        )
        .subtitle_lines(0)
        .build();
    let action_box = gtk::Box::new(Orientation::Horizontal, 6);
    let clear = gtk::Button::with_label("Clear selection");
    clear.set_tooltip_text(Some("Clear the live clipboard; retained entries stay"));
    let sender = commands.clone();
    clear.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Clipboard(
            kestrel::ClipboardCommand::ClearSelection,
        ));
    });
    let wipe = gtk::Button::with_label("Wipe history");
    wipe.set_tooltip_text(Some(
        "Clear the live clipboard and drop every retained entry",
    ));
    let sender = commands.clone();
    wipe.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Clipboard(
            kestrel::ClipboardCommand::Wipe,
        ));
    });
    action_box.append(&clear);
    action_box.append(&wipe);
    actions.add_suffix(&action_box);
    group.add(&actions);

    let results = gtk::Box::new(Orientation::Vertical, 6);
    request_initial_clipboard_listing(clipboard, commands);
    let selected = std::rc::Rc::new(std::cell::RefCell::new(std::collections::BTreeSet::new()));
    let panel = ClipboardPanel {
        container: container.clone(),
        status,
        results: results.clone(),
        selected: std::rc::Rc::clone(&selected),
    };
    fill_clipboard_results(&panel, clipboard, commands);
    group.add(&results);
    container.append(&group);

    if !clipboard.running {
        let row = adw::ActionRow::builder()
            .title("Clipboard history is not running")
            .subtitle(&clipboard.status)
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("Clipboard history is not running")]);
        group.add(&row);
        return panel;
    }

    let delete = gtk::Button::with_label("Delete selected");
    let selected_for_delete = std::rc::Rc::clone(&selected);
    let sender = commands.clone();
    let delete_row = adw::ActionRow::builder()
        .title("Multiple selection")
        .subtitle("Tick entries in the list and delete them together.")
        .build();
    delete.set_tooltip_text(Some("Delete every ticked entry"));
    delete.connect_clicked(move |_| {
        let ids = selected_for_delete
            .borrow()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::Clipboard(
            kestrel::ClipboardCommand::DeleteItems { item_ids: ids },
        ));
    });
    delete_row.add_suffix(&delete);
    group.add(&delete_row);

    panel
}

/// Rebuilds clipboard result rows.
fn fill_clipboard_results(
    panel: &ClipboardPanel,
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    if clipboard.items.is_empty() {
        let row = adw::ActionRow::builder()
            .title(if clipboard.search_query.trim().is_empty() {
                "No retained entries"
            } else {
                "No entries match the search"
            })
            .subtitle(if clipboard.running {
                "Copy something, or widen the search."
            } else {
                "Enable clipboard.history in the Feature Hub to retain entries."
            })
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("No clipboard entries to show")]);
        panel.results.append(&row);
        return;
    }

    for item in &clipboard.items {
        let row = adw::ActionRow::builder()
            .title(format!("{} · {}", item.kind, item.detail))
            .subtitle(if item.preview.is_empty() {
                "No inline preview for this entry kind".to_owned()
            } else {
                item.preview.clone()
            })
            .subtitle_lines(2)
            .build();
        row.update_property(&[Property::Label(&format!(
            "{} entry: {}",
            item.kind, item.preview
        ))]);

        let select = gtk::CheckButton::new();
        let select_label = format!("Select entry {}", item.id);
        select.update_property(&[Property::Label(&select_label)]);
        select.set_tooltip_text(Some(&format!("Select entry {} for deletion", item.id)));
        let selected = std::rc::Rc::clone(&panel.selected);
        let id = item.id;
        select.connect_toggled(move |button| {
            let mut selected = selected.borrow_mut();
            if button.is_active() {
                selected.insert(id);
            } else {
                selected.remove(&id);
            }
        });
        row.add_suffix(&select);

        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let pin = gtk::Button::with_label(if item.pinned { "Unpin" } else { "Pin" });
        let pin_label = format!(
            "{} entry {}",
            if item.pinned { "Unpin" } else { "Pin" },
            item.id
        );
        pin.update_property(&[Property::Label(&pin_label)]);
        pin.set_tooltip_text(Some(&pin_label));
        let sender = commands.clone();
        let id = item.id;
        let pinned = item.pinned;
        pin.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::SetPinned {
                    item_id: id,
                    pinned: !pinned,
                },
            ));
        });
        controls.append(&pin);

        let copy = gtk::Button::with_label("Copy");
        copy.update_property(&[Property::Label(&format!("Copy entry {}", item.id))]);
        copy.set_tooltip_text(Some("Put this entry on the clipboard"));
        let sender = commands.clone();
        let id = item.id;
        copy.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::CopyItem {
                    item_id: id,
                    plain_text: false,
                },
            ));
        });
        controls.append(&copy);

        let plain = gtk::Button::with_label("Plain");
        plain.update_property(&[Property::Label(&format!(
            "Copy entry {} as plain text",
            item.id
        ))]);
        plain.set_tooltip_text(Some(
            "Copy without ANSI escapes or trailing whitespace; file entries copy as paths",
        ));
        let sender = commands.clone();
        let id = item.id;
        plain.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::CopyItem {
                    item_id: id,
                    plain_text: true,
                },
            ));
        });
        controls.append(&plain);

        let preview = gtk::Button::with_label("Preview");
        preview.update_property(&[Property::Label(&format!("Preview entry {}", item.id))]);
        preview.set_tooltip_text(Some("Show a bounded, explicit preview of this entry"));
        let sender = commands.clone();
        let id = item.id;
        preview.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::ClipboardPreview(id));
        });
        controls.append(&preview);

        let delete = gtk::Button::with_label("Delete");
        delete.update_property(&[Property::Label(&format!("Delete entry {}", item.id))]);
        delete.set_tooltip_text(Some("Drop this retained entry"));
        let sender = commands.clone();
        let id = item.id;
        delete.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::DeleteItems { item_ids: vec![id] },
            ));
        });
        controls.append(&delete);
        row.add_suffix(&controls);
        panel.results.append(&row);

        if let Some(preview) = clipboard
            .preview
            .as_ref()
            .filter(|preview| preview.id == item.id)
        {
            panel
                .results
                .append(&build_clipboard_preview_row(preview, commands));
        }
    }
}

/// Builds the explicitly requested preview row.
fn build_clipboard_preview_row(
    preview: &kestrel::ClipboardPreviewViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::ActionRow {
    let mut description = vec![format!("{} entry", preview.kind)];
    if preview.truncated {
        description.push("preview truncated to the request bound".to_owned());
    }
    if let Some((width, height)) = preview.image_dimensions {
        description.push(format!("{width}×{height}"));
    }
    if let Some(count) = preview.file_count {
        description.push(format!("{count} paths"));
    }
    if preview.text.is_empty() {
        description.push("no inline text for this kind".to_owned());
    }
    let row = adw::ActionRow::builder()
        .title(format!("Preview {}", preview.id))
        .subtitle(description.join(" · "))
        .subtitle_lines(0)
        .build();
    row.update_property(&[Property::Label(&format!("Preview of entry {}", preview.id))]);
    if !preview.text.is_empty() {
        let text = gtk::Label::new(Some(&preview.text));
        text.set_wrap(true);
        text.set_xalign(0.0);
        text.set_selectable(true);
        text.set_valign(Align::Center);
        row.add_suffix(&text);
    }
    if preview.editable {
        let edit = gtk::Entry::builder()
            .text(&preview.text)
            .hexpand(true)
            .build();
        edit.update_property(&[Property::Label(&format!("Edit entry {} text", preview.id))]);
        let save = gtk::Button::with_label("Save edit");
        let sender = commands.clone();
        let id = preview.id;
        let edit_for_save = edit.clone();
        save.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::ReplaceText {
                    item_id: id,
                    text: edit_for_save.text().to_string(),
                },
            ));
        });
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        controls.append(&edit);
        controls.append(&save);
        row.add_suffix(&controls);
    }
    row
}

/// Builds the retained snippet group.
fn build_snippet_panel(
    snippets: &kestrel::SnippetsViewModel,
    commands: &Sender<ApplicationCommand>,
) -> SnippetPanel {
    let container = gtk::Box::new(Orientation::Vertical, 12);
    let group = adw::PreferencesGroup::builder()
        .title("Text snippets")
        .description(
            "Reusable text with local date, time, time zone, and clipboard variables. \
             Insertion is only offered when a provider is verified.",
        )
        .build();

    let status = gtk::Label::new(Some(&snippets.status));
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    let status_row = adw::ActionRow::new();
    status_row.set_title("Library");
    status_row.add_suffix(&status);
    group.add(&status_row);

    let insertion = gtk::Label::new(Some(&snippets.insertion_status));
    insertion.set_wrap(true);
    insertion.set_xalign(0.0);
    insertion.add_css_class("dim-label");
    let insertion_row = adw::ActionRow::builder()
        .title("Insertion")
        .subtitle(snippets.expansion_label)
        .subtitle_lines(0)
        .build();
    insertion_row.add_suffix(&insertion);
    group.add(&insertion_row);

    if let Some(directory) = &snippets.directory {
        let row = adw::ActionRow::builder()
            .title("Storage")
            .subtitle(format!("Private file directory: {directory}"))
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("Snippet storage directory")]);
        group.add(&row);
    }
    for warning in &snippets.warnings {
        let row = adw::ActionRow::builder()
            .title(&warning.feature_id)
            .subtitle(&warning.message)
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label(&format!(
            "{}: {}",
            warning.feature_id, warning.message
        ))]);
        group.add(&row);
    }

    let draft_label = gtk::Label::new(None);
    draft_label.set_wrap(true);
    draft_label.set_xalign(0.0);
    draft_label.add_css_class("dim-label");
    let draft_row = adw::ActionRow::builder()
        .title("Editor")
        .subtitle("Name, optional folder, optional trigger, and the snippet text.")
        .subtitle_lines(0)
        .build();
    draft_row.add_suffix(&draft_label);
    group.add(&draft_row);

    let editor = gtk::Box::new(Orientation::Vertical, 6);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search snippets")
        .hexpand(true)
        .build();
    search.update_property(&[
        Property::Label("Search snippets"),
        Property::Description("Searches names, folders, triggers, and snippet text"),
    ]);
    let search_row = adw::ActionRow::new();
    search_row.set_title("Search");
    search_row.add_suffix(&search);
    search_row.set_activatable_widget(Some(&search));
    let guard: SearchGuard = std::rc::Rc::new(std::cell::Cell::new(false));
    let handler_guard = std::rc::Rc::clone(&guard);
    let sender = commands.clone();
    search.connect_search_changed(move |entry| {
        if handler_guard.get() {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::SnippetSearch(entry.text().to_string()));
    });
    if !snippets.search_query.is_empty() {
        guard.set(true);
        search.set_text(&snippets.search_query);
        guard.set(false);
    }
    group.add(&search_row);
    // Keep widgets between updates; request the initial listing once.
    let results = gtk::Box::new(Orientation::Vertical, 6);

    if snippets.running
        && snippets.search_query.is_empty()
        && snippets.items.is_empty()
        && snippets.total > 0
    {
        let _ = commands.try_send(ApplicationCommand::SnippetSearch(String::new()));
    }
    let panel = SnippetPanel {
        container: container.clone(),
        status,
        insertion,
        draft_label,
        editor: editor.clone(),
        results: results.clone(),
    };
    group.add(&editor);
    fill_snippet_panel(&panel, snippets, commands);
    group.add(&results);
    container.append(&group);
    panel
}

/// Fills the snippet editor and results.
fn fill_snippet_panel(
    panel: &SnippetPanel,
    snippets: &kestrel::SnippetsViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    let draft = snippets
        .draft
        .clone()
        .unwrap_or(kestrel::SnippetDraftViewModel {
            name: String::new(),
            folder: String::new(),
            trigger: String::new(),
            content: String::new(),
            is_new: true,
        });
    panel.draft_label.set_text(if draft.is_new {
        "New snippet"
    } else {
        "Editing a stored snippet"
    });

    // Editing is available only while the feature runs.
    let editable = snippets.running;
    let name = entry_row(panel, commands, "Name", &draft.name, editable);
    let folder = entry_row(panel, commands, "Folder", &draft.folder, editable);
    let trigger = entry_row(panel, commands, "Trigger", &draft.trigger, editable);
    let content = text_view_row(panel, commands, &draft.content, editable);

    let actions = gtk::Box::new(Orientation::Horizontal, 6);
    let new_button = gtk::Button::with_label("New");
    new_button.set_sensitive(editable);
    new_button.update_property(&[Property::Label("New snippet")]);
    let sender = commands.clone();
    new_button.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::SnippetNew);
    });
    let save = gtk::Button::with_label("Save");
    save.set_sensitive(editable);
    save.update_property(&[Property::Label("Save snippet")]);
    save.set_tooltip_text(Some(
        "Validate and store the snippet in the private snippet file",
    ));
    let sender = commands.clone();
    let name_entry = name.clone();
    let folder_entry = folder.clone();
    let trigger_entry = trigger.clone();
    let content_view = content.clone();
    save.connect_clicked(move |_| {
        let buffer = content_view.buffer();
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        let _ = sender.try_send(ApplicationCommand::Snippet(kestrel::SnippetCommand::Save {
            name: name_entry.text().to_string(),
            folder: folder_entry.text().to_string(),
            trigger: trigger_entry.text().to_string(),
            content: text,
        }));
    });
    actions.append(&new_button);
    actions.append(&save);
    let action_row = adw::ActionRow::new();
    action_row.set_title("Save");
    action_row.set_subtitle("An empty folder or trigger stores no value for that field.");
    action_row.add_suffix(&actions);
    panel.editor.append(&action_row);

    if snippets.items.is_empty() {
        let row = adw::ActionRow::builder()
            .title(if snippets.search_query.trim().is_empty() {
                "No snippets yet"
            } else {
                "No snippets match the search"
            })
            .subtitle(if snippets.running {
                "Fill the editor and press Save, or edit the snippet file directly."
            } else {
                "Enable snippets.text in the Feature Hub to use the library."
            })
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("No snippets to show")]);
        panel.results.append(&row);
        return;
    }

    for item in &snippets.items {
        let row = adw::ActionRow::builder()
            .title(if let Some(folder) = &item.folder {
                format!("{} · {}", item.name, folder)
            } else {
                item.name.clone()
            })
            .subtitle(&item.preview)
            .subtitle_lines(2)
            .build();
        row.update_property(&[Property::Label(&format!("{}: {}", item.name, item.preview))]);

        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let insert = gtk::Button::with_label("Insert");
        let can_insert = snippets.running && snippets.insertion_available;
        insert.set_sensitive(can_insert);
        insert.update_property(&[Property::Label(&format!("Insert {}", item.name))]);
        insert.set_tooltip_text(Some(if can_insert {
            "Render the variables and type this snippet into the focused window"
        } else if !snippets.running {
            "Enable snippets.text to insert snippets"
        } else {
            "Insertion needs a verified provider"
        }));
        let sender = commands.clone();
        let name = item.name.clone();
        insert.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Snippet(
                kestrel::SnippetCommand::Insert(name.clone()),
            ));
        });
        controls.append(&insert);

        let edit = gtk::Button::with_label("Edit");
        edit.update_property(&[Property::Label(&format!("Edit {}", item.name))]);
        let sender = commands.clone();
        let name = item.name.clone();
        edit.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::SnippetEdit(name.clone()));
        });
        controls.append(&edit);

        let delete = gtk::Button::with_label("Delete");
        delete.set_sensitive(editable);
        delete.update_property(&[Property::Label(&format!("Delete {}", item.name))]);
        let sender = commands.clone();
        let name = item.name.clone();
        delete.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Snippet(
                kestrel::SnippetCommand::Delete(name.clone()),
            ));
        });
        controls.append(&delete);
        row.add_suffix(&controls);
        panel.results.append(&row);
    }
}

/// One single-line editor.
fn entry_row(
    panel: &SnippetPanel,
    _commands: &Sender<ApplicationCommand>,
    title: &str,
    value: &str,
    sensitive: bool,
) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .text(value)
        .hexpand(true)
        .sensitive(sensitive)
        .build();
    entry.update_property(&[Property::Label(&format!("Snippet {title}"))]);
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.add_suffix(&entry);
    panel.editor.append(&row);
    entry
}

/// Multi-line snippet editor.
fn text_view_row(
    panel: &SnippetPanel,
    _commands: &Sender<ApplicationCommand>,
    value: &str,
    editable: bool,
) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .editable(editable)
        .build();
    view.buffer().set_text(value);
    view.update_property(&[Property::Label("Snippet content")]);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .min_content_height(96)
        .child(&view)
        .build();
    let row = adw::ActionRow::new();
    row.set_title("Content");
    row.add_suffix(&scroller);
    panel.editor.append(&row);
    view
}

/// Builds the retained command-bar group.
fn build_command_panel(
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
        let row = adw::ActionRow::builder()
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
    let ranking_row = adw::ActionRow::builder()
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
        status,
        results: results.clone(),
    };
    fill_command_panel(&panel, command_bar, commands);
    group.add(&results);
    container.append(&group);
    panel
}

/// Fills ranked result rows.
fn fill_command_panel(
    panel: &CommandPanel,
    command_bar: &kestrel::CommandBarViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    if command_bar.results.is_empty() {
        let row = adw::ActionRow::builder()
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
        let row = adw::ActionRow::builder()
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

/// Target of a volume or mute control.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioLevelTarget {
    Output(u32),
    Stream(u32),
}

impl AudioLevelTarget {
    fn command(self, volume_percent: u8, muted: bool) -> (ApplicationCommand, ApplicationCommand) {
        match self {
            Self::Output(output_id) => (
                ApplicationCommand::Audio(kestrel::AudioCommand::SetOutputVolume {
                    output_id,
                    volume_percent,
                }),
                ApplicationCommand::Audio(kestrel::AudioCommand::SetOutputMute {
                    output_id,
                    muted,
                }),
            ),
            Self::Stream(stream_id) => (
                ApplicationCommand::Audio(kestrel::AudioCommand::SetStreamVolume {
                    stream_id,
                    volume_percent,
                }),
                ApplicationCommand::Audio(kestrel::AudioCommand::SetStreamMute {
                    stream_id,
                    muted,
                }),
            ),
        }
    }
}

/// Builds mute and volume controls.
fn build_audio_level_controls(
    target: AudioLevelTarget,
    label: &str,
    volume_percent: u8,
    muted: bool,
    volume_writable: bool,
    boost_ceiling_percent: u8,
    commands: &Sender<ApplicationCommand>,
) -> gtk::Box {
    let controls = gtk::Box::new(Orientation::Horizontal, 6);

    let mute = gtk::Switch::builder()
        .active(muted)
        .valign(Align::Center)
        .build();
    let mute_label = format!("Mute {label}");
    mute.update_property(&[
        Property::Label(&mute_label),
        Property::Description(&format!("Mute or unmute {label}")),
    ]);
    mute.set_tooltip_text(Some(&mute_label));
    let sender = commands.clone();
    mute.connect_state_set(move |_, muted| {
        let (_, mute_command) = target.command(volume_percent, muted);
        let _ = sender.try_send(mute_command);
        gtk::glib::Propagation::Proceed
    });
    controls.append(&mute);

    let volume = gtk::SpinButton::with_range(0.0, f64::from(boost_ceiling_percent), 1.0);
    volume.set_value(f64::from(volume_percent));
    volume.set_numeric(true);
    volume.set_width_chars(4);
    volume.set_sensitive(volume_writable);
    volume.set_valign(Align::Center);
    let volume_label = format!("{label} volume");
    volume.update_property(&[
        Property::Label(&volume_label),
        Property::Description(&format!(
            "Volume from 0 to {boost_ceiling_percent} percent, the configured amplification ceiling"
        )),
    ]);
    volume.set_tooltip_text(Some(&if volume_writable {
        format!("{label} volume, up to the {boost_ceiling_percent}% boost ceiling")
    } else {
        format!("{label} does not accept volume changes")
    }));
    let sender = commands.clone();
    volume.connect_value_changed(move |spin| {
        let (volume_command, _) = target.command(spin.value() as u8, muted);
        let _ = sender.try_send(volume_command);
    });
    controls.append(&volume);

    let unit = gtk::Label::new(Some("%"));
    unit.set_valign(Align::Center);
    controls.append(&unit);

    controls
}

fn build_audio_route_row(
    stream: &AudioStreamViewModel,
    outputs: &[AudioOutputViewModel],
    commands: &Sender<ApplicationCommand>,
) -> adw::ComboRow {
    let names = outputs
        .iter()
        .map(|output| output.title.as_str())
        .collect::<Vec<_>>();
    let selected = outputs
        .iter()
        .position(|output| output.id == stream.output_id)
        .unwrap_or(0);
    let row = adw::ComboRow::builder()
        .title(format!("Route {}", stream.title))
        .subtitle(&stream.detail)
        .model(&gtk::StringList::new(&names))
        .selected(selected as u32)
        .build();
    let sender = commands.clone();
    let stream_id = stream.id;
    let target_ids = outputs.iter().map(|output| output.id).collect::<Vec<_>>();
    let current_output_id = stream.output_id;
    row.connect_selected_notify(move |row| {
        let Some(&output_id) = target_ids.get(row.selected() as usize) else {
            return;
        };
        if output_id == current_output_id {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::Audio(
            kestrel::AudioCommand::MoveStream {
                stream_id,
                output_id,
            },
        ));
    });
    row
}

/// The audio mixer: master output, per-device grouping, cycling, and streams.
fn build_audio_controls(audio: &AudioViewModel, commands: &Sender<ApplicationCommand>) -> gtk::Box {
    let container = gtk::Box::new(Orientation::Vertical, 12);
    let group = adw::PreferencesGroup::builder()
        .title("Audio")
        .description(&audio.status)
        .build();

    if !audio.running {
        let row = adw::ActionRow::builder()
            .title("Audio mixer is not running")
            .subtitle(&audio.status)
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("The audio mixer is not running")]);
        group.add(&row);
        container.append(&group);
        return container;
    }

    let master = audio
        .default_output_id
        .and_then(|id| audio.outputs.iter().find(|output| output.id == id));
    let master_row = adw::ActionRow::builder()
        .title("Master output")
        .subtitle(match master {
            Some(output) => format!("{} · {}", output.title, output.detail),
            None => "No default output device is available.".to_owned(),
        })
        .subtitle_lines(0)
        .sensitive(master.is_some())
        .build();
    master_row.update_property(&[Property::Label("Master output volume and mute")]);
    if let Some(output) = master {
        master_row.add_suffix(&build_audio_level_controls(
            AudioLevelTarget::Output(output.id),
            "the master output",
            output.volume_percent,
            output.muted,
            true,
            audio.boost_ceiling_percent,
            commands,
        ));
    }
    group.add(&master_row);

    let cycle_row = adw::ActionRow::builder()
        .title("Switch output")
        .subtitle(match audio.policy.move_all_streams {
            true => "Cycle the default output; playing streams move with it.",
            false => "Cycle the default output; playing streams keep their device.",
        })
        .subtitle_lines(0)
        .build();
    let cycle = gtk::Box::new(Orientation::Horizontal, 6);
    let can_cycle = audio.outputs.len() > 1;
    for (direction, icon, label) in [
        (
            AudioCycleDirection::Previous,
            "go-previous-symbolic",
            "Switch to the previous output",
        ),
        (
            AudioCycleDirection::Next,
            "go-next-symbolic",
            "Switch to the next output",
        ),
    ] {
        let button = gtk::Button::builder()
            .icon_name(icon)
            .valign(Align::Center)
            .build();
        button.set_sensitive(can_cycle);
        button.update_property(&[
            Property::Label(label),
            Property::Description(&format!(
                "{label}. {}",
                if can_cycle {
                    "Cycles through the discovered outputs."
                } else {
                    "At least two output devices are required."
                }
            )),
        ]);
        button.set_tooltip_text(Some(&if can_cycle {
            label.to_owned()
        } else {
            format!("{label} (a second output device is required)")
        }));
        let sender = commands.clone();
        button.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Audio(
                kestrel::AudioCommand::CycleOutput { direction },
            ));
        });
        cycle.append(&button);
    }
    cycle_row.add_suffix(&cycle);
    group.add(&cycle_row);

    if let Some(message) = &audio.message {
        let row = adw::ActionRow::builder()
            .title("Last audio change")
            .subtitle(message)
            .subtitle_lines(0)
            .build();
        row.update_property(&[Property::Label(&format!("Last audio change: {message}"))]);
        group.add(&row);
    }
    container.append(&group);

    if audio.outputs.is_empty() {
        return container;
    }

    for device_group in &audio.output_groups {
        let card = adw::PreferencesGroup::builder()
            .title(&device_group.label)
            .build();
        for output in &device_group.outputs {
            let row = adw::ActionRow::builder()
                .title(&output.title)
                .subtitle(&output.name)
                .subtitle_lines(0)
                .build();
            row.update_property(&[Property::Label(&format!(
                "{} ({})",
                output.title, output.detail
            ))]);
            row.add_suffix(&build_audio_level_controls(
                AudioLevelTarget::Output(output.id),
                &output.title,
                output.volume_percent,
                output.muted,
                true,
                audio.boost_ceiling_percent,
                commands,
            ));
            if !output.is_default {
                let make_default = gtk::Button::with_label("Use");
                make_default.set_valign(Align::Center);
                let label = format!("Make {} the default output", output.title);
                make_default.update_property(&[
                    Property::Label(&label),
                    Property::Description(&format!(
                        "Make {} the default output without moving streams unless configured",
                        output.title
                    )),
                ]);
                make_default.set_tooltip_text(Some(&label));
                let sender = commands.clone();
                let output_id = output.id;
                make_default.connect_clicked(move |_| {
                    let _ = sender.try_send(ApplicationCommand::Audio(
                        kestrel::AudioCommand::SetDefaultOutput { output_id },
                    ));
                });
                row.add_suffix(&make_default);
            }
            card.add(&row);
        }
        container.append(&card);
    }

    let streams = adw::PreferencesGroup::builder()
        .title("Playback streams")
        .description(if audio.inactive_streams == 0 {
            "Per-stream volume, mute, and routing.".to_owned()
        } else {
            format!(
                "Per-stream volume, mute, and routing. {} inactive streams are hidden; show them \
                 from Settings → Audio.",
                audio.inactive_streams
            )
        })
        .build();
    if audio.streams.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No playing streams")
            .subtitle("Start playback in an application to control its volume.")
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("No playback streams are active")]);
        streams.add(&row);
    }
    let routable = audio.outputs.len() > 1;
    for stream in &audio.streams {
        let row = adw::ActionRow::builder()
            .title(&stream.title)
            .subtitle(&stream.detail)
            .subtitle_lines(0)
            .build();
        row.update_property(&[Property::Label(&format!(
            "{}: {}",
            stream.title, stream.detail
        ))]);
        row.add_suffix(&build_audio_level_controls(
            AudioLevelTarget::Stream(stream.id),
            &stream.title,
            stream.volume_percent.unwrap_or(0),
            stream.muted,
            stream.volume_writable,
            audio.boost_ceiling_percent,
            commands,
        ));
        streams.add(&row);
        if routable {
            streams.add(&build_audio_route_row(stream, &audio.outputs, commands));
        }
    }
    container.append(&streams);

    container
}

fn build_feature_hub_group(
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
        let row = adw::ActionRow::builder()
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

fn configure_wrapping_label(label: &gtk::Label) {
    label.set_halign(Align::Start);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
}
