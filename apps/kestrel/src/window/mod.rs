//! GTK rendering of the application window.

mod audio;
mod camera;
mod capture;
mod clipboard;
mod command_bar;
mod features;
mod floating;
mod monitor;
mod processes;
mod recording;
mod scratchpad;
mod screen_tools;
mod settings;
mod snippets;
mod trim;
mod widgets;

use adw::prelude::*;
use async_channel::Sender;
use audio::{build_audio_controls, build_microphone_controls};
use camera::build_camera_group;
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
    ApplicationCommand, ApplicationViewModel, AudioViewModel, CameraFrameSlot, CaptureViewModel,
    FocusTarget, MicrophoneViewModel, MonitorViewModel, ShortcutsViewModel, SpeedTestViewModel,
};
use monitor::{build_monitor_group, build_speed_test_group};
use processes::{ProcessesPanel, build_processes_panel};
use recording::build_recording_group;
use scratchpad::{ScratchpadPanel, build_scratchpad_panel};
use screen_tools::build_screen_tools_group;
use settings::{SettingsPage, build_settings_group};
use snippets::{SnippetPanel, build_snippet_panel, fill_snippet_panel};
use widgets::configure_wrapping_label;

pub use floating::{FloatingSurface, QuickPanelEntry};

const MINIMUM_WINDOW_WIDTH: i32 = 360;
const MINIMUM_WINDOW_HEIGHT: i32 = 360;
const DEFAULT_WINDOW_WIDTH: i32 = 760;
const DEFAULT_WINDOW_HEIGHT: i32 = 640;
const CONTENT_MAXIMUM_WIDTH: i32 = 760;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SidebarPage {
    Overview,
    Monitoring,
    Audio,
    Clipboard,
    CommandBar,
    Capture,
    Notes,
    Processes,
    Shortcuts,
    FeatureHub,
    Settings,
}

impl SidebarPage {
    const ALL: [Self; 11] = [
        Self::Overview,
        Self::Monitoring,
        Self::Audio,
        Self::Clipboard,
        Self::CommandBar,
        Self::Capture,
        Self::Notes,
        Self::Processes,
        Self::Shortcuts,
        Self::FeatureHub,
        Self::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Monitoring => "Monitoring",
            Self::Audio => "Audio",
            Self::Clipboard => "Clipboard",
            Self::CommandBar => "Command bar",
            Self::Capture => "Capture",
            Self::Notes => "Notes",
            Self::Processes => "Processes",
            Self::Shortcuts => "Shortcuts",
            Self::FeatureHub => "Feature hub",
            Self::Settings => "Settings",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Overview => "view-grid-symbolic",
            Self::Monitoring => "utilities-system-monitor-symbolic",
            Self::Audio => "audio-volume-high-symbolic",
            Self::Clipboard => "edit-paste-symbolic",
            Self::CommandBar => "system-search-symbolic",
            Self::Capture => "camera-photo-symbolic",
            Self::Notes => "accessories-text-editor-symbolic",
            Self::Processes => "system-run-symbolic",
            Self::Shortcuts => "input-keyboard-symbolic",
            Self::FeatureHub => "view-app-grid-symbolic",
            Self::Settings => "emblem-system-symbolic",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Monitoring => "monitoring",
            Self::Audio => "audio",
            Self::Clipboard => "clipboard",
            Self::CommandBar => "command-bar",
            Self::Capture => "capture",
            Self::Notes => "notes",
            Self::Processes => "processes",
            Self::Shortcuts => "shortcuts",
            Self::FeatureHub => "feature-hub",
            Self::Settings => "settings",
        }
    }
}

pub struct WindowView {
    pub window: adw::ApplicationWindow,
    split_view: adw::NavigationSplitView,
    content_stack: gtk::Stack,
    content_page: adw::NavigationPage,
    header_title: adw::WindowTitle,
    sidebar_search: gtk::SearchEntry,
    sidebar_rows: std::rc::Rc<Vec<(gtk::ListBoxRow, SidebarPage)>>,
    refresh_button: gtk::Button,
    toasts: adw::ToastOverlay,
    commands: Sender<ApplicationCommand>,
    monitor_container: std::cell::RefCell<Option<gtk::Box>>,
    audio_container: std::cell::RefCell<Option<gtk::Box>>,
    microphone_container: std::cell::RefCell<Option<gtk::Box>>,
    speed_test_container: std::cell::RefCell<Option<gtk::Box>>,
    capture_container: std::cell::RefCell<Option<gtk::Box>>,
    screen_tools_container: std::cell::RefCell<Option<gtk::Box>>,
    camera_container: std::cell::RefCell<Option<gtk::Box>>,
    camera_frames: CameraFrameSlot,
    recording_container: std::cell::RefCell<Option<gtk::Box>>,
    shortcuts_container: std::cell::RefCell<Option<gtk::Box>>,
    clipboard_panel: std::cell::RefCell<Option<ClipboardPanel>>,
    snippet_panel: std::cell::RefCell<Option<SnippetPanel>>,
    command_panel: std::cell::RefCell<Option<CommandPanel>>,
    processes_panel: std::cell::RefCell<Option<ProcessesPanel>>,
    scratchpad_panel: std::cell::RefCell<Option<ScratchpadPanel>>,
}

impl WindowView {
    pub fn new(
        application: &adw::Application,
        view_model: &ApplicationViewModel,
        commands: Sender<ApplicationCommand>,
        camera_frames: CameraFrameSlot,
    ) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title(kestrel_core::APPLICATION_NAME)
            .default_width(DEFAULT_WINDOW_WIDTH)
            .default_height(DEFAULT_WINDOW_HEIGHT)
            .build();
        window.set_size_request(MINIMUM_WINDOW_WIDTH, MINIMUM_WINDOW_HEIGHT);

        let sidebar_search = gtk::SearchEntry::builder()
            .placeholder_text("Search pages and settings")
            .hexpand(true)
            .build();
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.set_title_widget(Some(&sidebar_search));
        let sidebar_list = gtk::ListBox::new();
        sidebar_list.add_css_class("navigation-sidebar");
        sidebar_list.set_selection_mode(gtk::SelectionMode::Single);

        let mut sidebar_rows = Vec::with_capacity(SidebarPage::ALL.len());
        for page in SidebarPage::ALL {
            let row = gtk::ListBoxRow::new();
            let line = gtk::Box::new(Orientation::Horizontal, 12);
            line.set_margin_top(8);
            line.set_margin_bottom(8);
            line.set_margin_start(12);
            line.set_margin_end(12);
            let icon = gtk::Image::from_icon_name(page.icon());
            icon.set_pixel_size(20);
            let label = gtk::Label::new(Some(page.label()));
            label.set_xalign(0.0);
            line.append(&icon);
            line.append(&label);
            row.set_child(Some(&line));
            sidebar_list.append(&row);
            sidebar_rows.push((row, page));
        }
        let sidebar_toolbar = adw::ToolbarView::new();
        sidebar_toolbar.add_top_bar(&sidebar_header);
        sidebar_toolbar.set_content(Some(&sidebar_list));
        let sidebar_page = adw::NavigationPage::builder()
            .title(kestrel_core::APPLICATION_NAME)
            .child(&sidebar_toolbar)
            .build();

        let title = adw::WindowTitle::new("Overview", "Feature capabilities");
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

        let search_for_pages = sidebar_search.clone();
        let rows_for_pages = std::rc::Rc::new(sidebar_rows.clone());
        let search_rows = rows_for_pages.clone();
        sidebar_search.connect_search_changed(move |entry| {
            let query = entry.text().trim().to_lowercase();
            for (row, page) in search_rows.iter() {
                let keywords = page_keywords(*page);
                row.set_visible(query.is_empty() || keywords.contains(&query));
            }
            if query.is_empty() {
                if let Some(row) = search_rows.first() {
                    row.0.set_visible(true);
                }
            }
        });

        let content_stack = gtk::Stack::new();
        content_stack.set_vexpand(true);
        content_stack.set_hexpand(true);
        let mut build = PageBuild::default();
        for page in SidebarPage::ALL {
            let page_box = build_page(
                page,
                view_model,
                &window,
                &commands,
                &camera_frames,
                &sidebar_search,
            );
            content_stack.add_named(&page_box.scrolled, Some(page.name()));
            build.absorb(page_box);
        }
        content_stack.set_visible_child_name(SidebarPage::Overview.name());

        let toasts = adw::ToastOverlay::new();
        let content_toolbar = adw::ToolbarView::new();
        content_toolbar.add_top_bar(&header);
        content_toolbar.set_content(Some(&content_stack));
        toasts.set_child(Some(&content_toolbar));
        let content_page = adw::NavigationPage::builder()
            .title("Overview")
            .child(&toasts)
            .build();
        let split_view = adw::NavigationSplitView::new();
        split_view.set_sidebar(Some(&sidebar_page));
        split_view.set_content(Some(&content_page));
        split_view.set_sidebar_width_fraction(0.27);
        let breakpoint = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 560sp")
                .expect("valid sidebar breakpoint condition"),
        );
        let collapsed = true.to_value();
        breakpoint.add_setter(&split_view, "collapsed", Some(&collapsed));
        window.add_breakpoint(breakpoint);
        window.set_content(Some(&split_view));

        let rows_for_selection = std::rc::Rc::new(sidebar_rows.clone());
        let split_for_selection = split_view.clone();
        let stack_for_selection = content_stack.clone();
        let page_for_selection = content_page.clone();
        let title_for_selection = title.clone();
        sidebar_list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let Some((_, page)) = rows_for_selection.iter().find(|(item, _)| item == row) else {
                return;
            };
            stack_for_selection.set_visible_child_name(page.name());
            page_for_selection.set_title(page.label());
            title_for_selection.set_title(page.label());
            if split_for_selection.is_collapsed() {
                split_for_selection.set_show_content(true);
            }
        });
        sidebar_list.select_row(Some(&sidebar_rows[0].0));

        Self {
            window,
            split_view,
            content_stack,
            content_page,
            header_title: title,
            sidebar_search: search_for_pages,
            sidebar_rows: rows_for_pages,
            refresh_button,
            toasts,
            commands,
            monitor_container: std::cell::RefCell::new(build.monitor_container),
            audio_container: std::cell::RefCell::new(build.audio_container),
            microphone_container: std::cell::RefCell::new(build.microphone_container),
            speed_test_container: std::cell::RefCell::new(build.speed_test_container),
            capture_container: std::cell::RefCell::new(build.capture_container),
            screen_tools_container: std::cell::RefCell::new(build.screen_tools_container),
            camera_container: std::cell::RefCell::new(build.camera_container),
            camera_frames,
            recording_container: std::cell::RefCell::new(build.recording_container),
            shortcuts_container: std::cell::RefCell::new(Some(build.shortcuts_container)),
            clipboard_panel: std::cell::RefCell::new(build.clipboard_panel),
            snippet_panel: std::cell::RefCell::new(build.snippet_panel),
            processes_panel: std::cell::RefCell::new(build.processes_panel),
            scratchpad_panel: std::cell::RefCell::new(build.scratchpad_panel),
            command_panel: std::cell::RefCell::new(build.command_panel),
        }
    }

    pub fn set_view_model(&self, view_model: &ApplicationViewModel) {
        let selected = self
            .content_stack
            .visible_child_name()
            .and_then(|name| {
                SidebarPage::ALL
                    .iter()
                    .find(|page| page.name() == name.as_str())
            })
            .copied()
            .unwrap_or(SidebarPage::Overview);
        let build = SidebarPage::ALL.map(|page| {
            build_page(
                page,
                view_model,
                &self.window,
                &self.commands,
                &self.camera_frames,
                &self.sidebar_search,
            )
        });
        for (page, built) in SidebarPage::ALL.into_iter().zip(build) {
            if let Some(old) = self.content_stack.child_by_name(page.name()) {
                old.unparent();
            }
            self.content_stack
                .add_named(&built.scrolled, Some(page.name()));
            self.replace_page_state(page, built);
        }
        self.select_page(selected);
    }

    fn replace_page_state(&self, page: SidebarPage, build: PageBuild) {
        match page {
            SidebarPage::Monitoring => {
                *self.monitor_container.borrow_mut() = build.monitor_container;
                *self.speed_test_container.borrow_mut() = build.speed_test_container;
            }
            SidebarPage::Audio => {
                *self.audio_container.borrow_mut() = build.audio_container;
                *self.microphone_container.borrow_mut() = build.microphone_container;
            }
            SidebarPage::Capture => {
                *self.capture_container.borrow_mut() = build.capture_container;
                *self.screen_tools_container.borrow_mut() = build.screen_tools_container;
                *self.camera_container.borrow_mut() = build.camera_container;
                *self.recording_container.borrow_mut() = build.recording_container;
            }
            SidebarPage::Shortcuts => {
                *self.shortcuts_container.borrow_mut() = Some(build.shortcuts_container);
            }
            SidebarPage::Clipboard => *self.clipboard_panel.borrow_mut() = build.clipboard_panel,
            SidebarPage::Notes => {
                *self.snippet_panel.borrow_mut() = build.snippet_panel;
                *self.scratchpad_panel.borrow_mut() = build.scratchpad_panel;
            }
            SidebarPage::CommandBar => *self.command_panel.borrow_mut() = build.command_panel,
            SidebarPage::Processes => *self.processes_panel.borrow_mut() = build.processes_panel,
            SidebarPage::Overview | SidebarPage::FeatureHub | SidebarPage::Settings => {}
        }
    }

    fn select_page(&self, page: SidebarPage) {
        self.content_stack.set_visible_child_name(page.name());
        self.content_page.set_title(page.label());
        self.header_title.set_title(page.label());
        if self.split_view.is_collapsed() {
            self.split_view.set_show_content(true);
        }
        if let Some((row, _)) = self.sidebar_rows.iter().find(|(_, item)| *item == page) {
            if let Some(list) = row
                .parent()
                .and_then(|parent| parent.downcast::<gtk::ListBox>().ok())
            {
                list.select_row(Some(row));
            }
        }
    }

    pub fn focus(&self, target: FocusTarget) -> bool {
        match target {
            FocusTarget::QuickPanel | FocusTarget::CommandBar => false,
            FocusTarget::Clipboard => {
                self.select_page(SidebarPage::Clipboard);
                self.clipboard_panel
                    .borrow()
                    .as_ref()
                    .is_some_and(|panel| panel.search.grab_focus())
            }
            FocusTarget::Processes => {
                self.select_page(SidebarPage::Processes);
                self.processes_panel
                    .borrow()
                    .as_ref()
                    .is_some_and(|panel| panel.search.grab_focus())
            }
            FocusTarget::Scratchpad => {
                self.select_page(SidebarPage::Notes);
                self.scratchpad_panel
                    .borrow()
                    .as_ref()
                    .is_some_and(|panel| panel.editor.grab_focus())
            }
        }
    }

    pub fn set_command_bar(&self, command_bar: &kestrel::CommandBarViewModel) {
        let panel = self.command_panel.borrow();
        let Some(panel) = panel.as_ref() else { return };
        panel.status.set_text(&command_bar.status);
        while let Some(child) = panel.results.first_child() {
            child.unparent();
        }
        fill_command_panel(panel, command_bar, &self.commands);
    }

    pub fn set_clipboard(&self, clipboard: &kestrel::ClipboardViewModel) {
        let panel = self.clipboard_panel.borrow();
        let Some(panel) = panel.as_ref() else { return };
        panel.status.set_text(&clipboard.status);
        fill_clipboard_results(panel, clipboard, &self.commands);
        request_initial_clipboard_listing(clipboard, &self.commands);
    }

    pub fn set_snippets(&self, snippets: &kestrel::SnippetsViewModel) {
        let panel = self.snippet_panel.borrow();
        let Some(panel) = panel.as_ref() else { return };
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

    pub fn set_scratchpad(&self, scratchpad: &kestrel::ScratchpadViewModel) {
        if let Some(panel) = self.scratchpad_panel.borrow().as_ref() {
            panel.set_model(scratchpad);
        }
    }

    pub fn set_scratchpad_preview(&self, scratchpad: &kestrel::ScratchpadViewModel) {
        if let Some(panel) = self.scratchpad_panel.borrow().as_ref() {
            panel.set_preview(scratchpad);
        }
    }

    pub fn set_monitor(&self, monitor: &MonitorViewModel) {
        let Some(container) = self.monitor_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_monitor_group(monitor, &self.commands));
    }

    pub fn set_audio(&self, audio: &AudioViewModel) {
        let Some(container) = self.audio_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_audio_controls(audio, &self.commands));
    }

    pub fn set_microphone(&self, microphone: &MicrophoneViewModel) {
        let Some(container) = self.microphone_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_microphone_controls(microphone, &self.commands));
    }

    pub fn set_speed_test(&self, speed_test: &SpeedTestViewModel) {
        let Some(container) = self.speed_test_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_speed_test_group(speed_test, &self.commands));
    }

    pub fn set_camera(&self, camera: &kestrel::CameraViewModel) {
        let Some(container) = self.camera_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_camera_group(
            camera,
            Some(&self.camera_frames),
            &self.commands,
        ));
    }

    pub fn set_recording(&self, recording: &kestrel::RecordingViewModel) {
        let Some(container) = self.recording_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_recording_group(recording, &self.commands));
    }

    pub fn open_trim(
        &self,
        take: std::path::PathBuf,
        duration: std::time::Duration,
        preview: kestrel::TrimPreview,
    ) {
        trim::open_trim_dialog(&self.window, take, duration, preview, self.commands.clone());
    }

    pub fn set_capture(&self, capture: &CaptureViewModel) {
        let Some(container) = self.capture_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_capture_group(capture, &self.window, &self.commands));
    }

    pub fn set_screen_tools(&self, tools: &kestrel::ScreenToolsViewModel) {
        let Some(container) = self.screen_tools_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_screen_tools_group(tools, &self.commands));
    }

    pub fn set_processes(&self, processes: &kestrel::ProcessesViewModel) {
        if let Some(panel) = self.processes_panel.borrow().as_ref() {
            panel.set_processes(processes, &self.commands);
        }
    }

    pub fn set_listeners(&self, listeners: &kestrel::ListenersViewModel) {
        if let Some(panel) = self.processes_panel.borrow().as_ref() {
            panel.set_listeners(listeners, &self.commands);
        }
    }

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
    scrolled: gtk::ScrolledWindow,
    monitor_container: Option<gtk::Box>,
    audio_container: Option<gtk::Box>,
    microphone_container: Option<gtk::Box>,
    speed_test_container: Option<gtk::Box>,
    capture_container: Option<gtk::Box>,
    screen_tools_container: Option<gtk::Box>,
    camera_container: Option<gtk::Box>,
    recording_container: Option<gtk::Box>,
    shortcuts_container: gtk::Box,
    clipboard_panel: Option<ClipboardPanel>,
    snippet_panel: Option<SnippetPanel>,
    processes_panel: Option<ProcessesPanel>,
    command_panel: Option<CommandPanel>,
    scratchpad_panel: Option<ScratchpadPanel>,
}

impl Default for PageBuild {
    fn default() -> Self {
        Self {
            scrolled: gtk::ScrolledWindow::new(),
            monitor_container: None,
            audio_container: None,
            microphone_container: None,
            speed_test_container: None,
            capture_container: None,
            screen_tools_container: None,
            camera_container: None,
            recording_container: None,
            shortcuts_container: gtk::Box::new(Orientation::Vertical, 0),
            clipboard_panel: None,
            snippet_panel: None,
            processes_panel: None,
            command_panel: None,
            scratchpad_panel: None,
        }
    }
}

impl PageBuild {
    fn absorb(&mut self, other: Self) {
        self.monitor_container = self.monitor_container.take().or(other.monitor_container);
        self.audio_container = self.audio_container.take().or(other.audio_container);
        self.microphone_container = self
            .microphone_container
            .take()
            .or(other.microphone_container);
        self.speed_test_container = self
            .speed_test_container
            .take()
            .or(other.speed_test_container);
        self.capture_container = self.capture_container.take().or(other.capture_container);
        self.screen_tools_container = self
            .screen_tools_container
            .take()
            .or(other.screen_tools_container);
        self.camera_container = self.camera_container.take().or(other.camera_container);
        self.recording_container = self
            .recording_container
            .take()
            .or(other.recording_container);
        self.shortcuts_container = if self.shortcuts_container.first_child().is_some() {
            self.shortcuts_container.clone()
        } else {
            other.shortcuts_container
        };
        self.clipboard_panel = self.clipboard_panel.take().or(other.clipboard_panel);
        self.snippet_panel = self.snippet_panel.take().or(other.snippet_panel);
        self.processes_panel = self.processes_panel.take().or(other.processes_panel);
        self.command_panel = self.command_panel.take().or(other.command_panel);
        self.scratchpad_panel = self.scratchpad_panel.take().or(other.scratchpad_panel);
    }
}

fn build_page(
    page: SidebarPage,
    view_model: &ApplicationViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    camera_frames: &CameraFrameSlot,
    search: &gtk::SearchEntry,
) -> PageBuild {
    let body = gtk::Box::new(Orientation::Vertical, 24);
    body.set_margin_top(24);
    body.set_margin_bottom(24);
    body.set_margin_start(18);
    body.set_margin_end(18);
    if page == SidebarPage::Overview {
        let heading = gtk::Label::new(Some(kestrel_core::APPLICATION_NAME));
        heading.add_css_class("title-1");
        heading.set_halign(Align::Start);
        heading.set_wrap(true);
        heading.set_xalign(0.0);
        body.append(&heading);
        let description = gtk::Label::new(Some(
            "Kestrel keeps registered features visible and explains unavailable platform paths.",
        ));
        configure_wrapping_label(&description);
        description.add_css_class("dim-label");
        body.append(&description);
        if !view_model.quick_toggles.is_empty() {
            body.append(&build_quick_toggle_group(
                &view_model.quick_toggles,
                window,
                commands,
            ));
        }
        if !view_model.warnings.is_empty() {
            body.append(&build_warning_group(view_model));
        }
    }
    let mut build = PageBuild::default();
    match page {
        SidebarPage::Monitoring => {
            let monitor = gtk::Box::new(Orientation::Vertical, 0);
            monitor.append(&build_monitor_group(&view_model.monitor, commands));
            let speed = gtk::Box::new(Orientation::Vertical, 0);
            speed.set_margin_top(24);
            speed.append(&build_speed_test_group(&view_model.speed_test, commands));
            monitor.append(&speed);
            body.append(&monitor);
            build.monitor_container = Some(monitor);
            build.speed_test_container = Some(speed);
            let settings = build_settings_group(
                view_model,
                window,
                commands,
                search,
                SettingsPage::Monitoring,
            );
            body.append(&settings);
        }
        SidebarPage::Audio => {
            let audio = gtk::Box::new(Orientation::Vertical, 0);
            audio.append(&build_audio_controls(&view_model.audio, commands));
            body.append(&audio);
            let microphone = gtk::Box::new(Orientation::Vertical, 0);
            microphone.append(&build_microphone_controls(&view_model.microphone, commands));
            body.append(&microphone);
            build.audio_container = Some(audio);
            build.microphone_container = Some(microphone);
            body.append(&build_settings_group(
                view_model,
                window,
                commands,
                search,
                SettingsPage::Audio,
            ));
        }
        SidebarPage::Clipboard => {
            let panel = build_clipboard_panel(&view_model.clipboard, commands);
            body.append(&panel.container);
            build.clipboard_panel = Some(panel);
            body.append(&build_settings_group(
                view_model,
                window,
                commands,
                search,
                SettingsPage::Clipboard,
            ));
        }
        SidebarPage::CommandBar => {
            let panel = build_command_panel(&view_model.command_bar, commands);
            body.append(&panel.container);
            build.command_panel = Some(panel);
            body.append(&build_settings_group(
                view_model,
                window,
                commands,
                search,
                SettingsPage::CommandBar,
            ));
        }
        SidebarPage::Capture => {
            let capture = gtk::Box::new(Orientation::Vertical, 0);
            capture.append(&build_capture_group(&view_model.capture, window, commands));
            body.append(&capture);
            let tools = gtk::Box::new(Orientation::Vertical, 0);
            tools.append(&build_screen_tools_group(
                &view_model.screen_tools,
                commands,
            ));
            body.append(&tools);
            let recording = gtk::Box::new(Orientation::Vertical, 0);
            recording.append(&build_recording_group(&view_model.recording, commands));
            body.append(&recording);
            let camera = gtk::Box::new(Orientation::Vertical, 0);
            camera.append(&build_camera_group(
                &view_model.camera,
                Some(camera_frames),
                commands,
            ));
            body.append(&camera);
            build.capture_container = Some(capture);
            build.screen_tools_container = Some(tools);
            build.recording_container = Some(recording);
            build.camera_container = Some(camera);
            body.append(&build_settings_group(
                view_model,
                window,
                commands,
                search,
                SettingsPage::Capture,
            ));
        }
        SidebarPage::Notes => {
            let snippets = build_snippet_panel(&view_model.snippets, commands);
            body.append(&snippets.container);
            build.snippet_panel = Some(snippets);
            let scratchpad = build_scratchpad_panel(&view_model.scratchpad, window, commands);
            body.append(&scratchpad.container);
            build.scratchpad_panel = Some(scratchpad);
            body.append(&build_settings_group(
                view_model,
                window,
                commands,
                search,
                SettingsPage::Notes,
            ));
        }
        SidebarPage::Processes => {
            let panel =
                build_processes_panel(&view_model.processes, &view_model.listeners, commands);
            body.append(&panel.container);
            build.processes_panel = Some(panel);
            body.append(&build_settings_group(
                view_model,
                window,
                commands,
                search,
                SettingsPage::Processes,
            ));
        }
        SidebarPage::Shortcuts => {
            let shortcuts = gtk::Box::new(Orientation::Vertical, 0);
            shortcuts.append(&build_shortcuts_group(&view_model.shortcuts));
            body.append(&shortcuts);
            build.shortcuts_container = shortcuts;
        }
        SidebarPage::FeatureHub => body.append(&build_feature_hub_group(
            &view_model.features,
            commands,
            search,
        )),
        SidebarPage::Settings => body.append(&build_settings_group(
            view_model,
            window,
            commands,
            search,
            SettingsPage::Settings,
        )),
        SidebarPage::Overview => {}
    }
    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(CONTENT_MAXIMUM_WIDTH);
    clamp.set_tightening_threshold(MINIMUM_WINDOW_WIDTH);
    clamp.set_child(Some(&body));
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .kinetic_scrolling(true)
        .propagate_natural_height(false)
        .vexpand(true)
        .child(&clamp)
        .build();
    build.scrolled = scrolled;
    build
}

fn page_keywords(page: SidebarPage) -> String {
    match page {
        SidebarPage::Overview => "overview status warning capability quick toggle".to_owned(),
        SidebarPage::Monitoring => {
            "monitor monitoring speed test readout alert network cpu temperature processes"
                .to_owned()
        }
        SidebarPage::Audio => "audio mixer microphone policy volume".to_owned(),
        SidebarPage::Clipboard => "clipboard history limits retention entries".to_owned(),
        SidebarPage::CommandBar => "command bar quick panel favorites search".to_owned(),
        SidebarPage::Capture => {
            "capture screenshot screen tools recording camera limits".to_owned()
        }
        SidebarPage::Notes => "notes snippets scratchpad limits editor".to_owned(),
        SidebarPage::Processes => "process processes listeners limits refresh rows".to_owned(),
        SidebarPage::Shortcuts => "shortcuts keyboard global".to_owned(),
        SidebarPage::FeatureHub => "feature hub capabilities enablement".to_owned(),
        SidebarPage::Settings => {
            "settings appearance close button autostart presets configuration import export"
                .to_owned()
        }
    }
}
