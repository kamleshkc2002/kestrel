//! Application view-model projections.

use kestrel_core::{
    AlertKind, AppearancePreference, ApplicationConfiguration, CapabilityStatus, CostLevel,
    MonitorConfiguration, MonitorReadout, Permission, ResourceCost, SnippetExpansionTiming,
    SnippetProviderPreference,
};
use kestrel_platform::camera::CameraInfo;
use kestrel_platform::capture::{CaptureErrorKind, CaptureMode, CaptureProvider};
use kestrel_platform::global_shortcuts::{
    BindingState, ShortcutErrorKind, ShortcutPhase, ShortcutStatus,
};
use kestrel_platform::processes::{
    Ineligible, Listener, ListenerProtocol, NO_PIDFD_EXPLANATION, ProcessIdentity, ProcfsProcesses,
    RestartTarget,
};
use kestrel_platform::quick_toggles::{
    MutationConfirmation, QuickToggleControl, QuickToggleId, ToggleAction,
};
use kestrel_platform::recording::{AudioTrack, RecordingSource};
use kestrel_platform::screen_tools::ScreenTool;
use kestrel_platform::snippets::InsertionProvider;
use kestrel_platform::speed_test::SpeedTestPhase;
use kestrel_services::{
    ServiceLifecycle, ServiceRegistration,
    alerts::{ActiveAlert, AlertSnapshot},
    audio::{AudioAvailability, AudioPolicy, AudioSnapshot},
    camera::{CameraPhase, CameraSnapshot},
    capture::{CaptureId, CapturePhase, CaptureSnapshot},
    clipboard::{ClipboardMatch, ClipboardPreview, ClipboardSnapshot},
    microphone::{MicrophoneAvailability, MicrophoneMuteState, MicrophoneSnapshot},
    processes::{ListenerSnapshot, ProcessRow, ProcessSnapshot, search as search_processes},
    quick_toggles::{QuickToggleSnapshot, ToggleStateSource},
    recording::{ExportPhase, RecordingClock, RecordingPhase, RecordingSnapshot},
    scratchpad::ScratchpadSnapshot,
    screen_tools::{QrPayload, ScreenToolResult, ScreenToolsPhase, ScreenToolsSnapshot},
    snippets::{SnippetLibrary, SnippetMatch, SnippetPolicy},
    speed_test::{SpeedTestSnapshot, SpeedTestStatus},
    system_monitor::{HistorySummary, SystemSnapshot},
};

use crate::{ConfigurationWarning, ProcessActionKind, command_line::CommandLineAction};

mod audio;
mod camera;
mod capture;
mod clipboard;
mod command_bar;
mod features;
mod microphone;
mod monitor;
mod overlay;
mod processes;
mod recording;
mod scratchpad;
mod screen_tools;
mod shortcuts;
mod snippets;
mod speed_test;
mod whats_new;

#[cfg(test)]
mod test_support;

pub(crate) use audio::AudioPresentation;
pub use audio::{
    AudioOutputGroupViewModel, AudioOutputViewModel, AudioPolicyViewModel, AudioStreamViewModel,
    AudioViewModel, DevicePriorityViewModel, PriorityDeviceViewModel, output_icon,
};
pub(crate) use camera::CameraPresentation;
pub use camera::{CameraChoiceViewModel, CameraViewModel};
pub(crate) use capture::CapturePresentation;
pub use capture::{CaptureEntryViewModel, CaptureModeViewModel, CaptureViewModel};
pub(crate) use clipboard::ClipboardPresentation;
pub use clipboard::{
    ClipboardBoundsViewModel, ClipboardItemViewModel, ClipboardPolicyViewModel,
    ClipboardPreviewViewModel, ClipboardQuery, ClipboardViewModel,
};
use clipboard::{format_age, format_clipboard_bytes};
pub(crate) use command_bar::CommandBarPresentation;
pub use command_bar::{
    CommandBarQuery, CommandBarViewModel, CommandProvider, CommandProviderViewModel,
    CommandRankingViewModel, CommandResultViewModel,
};
pub use features::{
    CapabilityKind, CapabilityStatusViewModel, CapabilityViewModel, ConfigurationWarningViewModel,
    ConfirmationViewModel, FeatureIndicator, FeatureLifecycleViewModel, FeatureViewModel,
    OverviewTool, OverviewToolViewModel, OverviewToolsViewModel, QuickToggleActionViewModel,
    QuickToggleControlViewModel, QuickToggleViewModel, RemediationViewModel, ResourceCostViewModel,
    TurnOnViewModel, disabled_status_for, turn_on_for,
};
pub use kestrel_platform::audio::OutputKind;
pub(crate) use microphone::MicrophonePresentation;
pub use microphone::{CaptureStreamViewModel, MicrophoneInputViewModel, MicrophoneViewModel};
pub(crate) use monitor::MonitorPresentation;
use monitor::format_bytes;
pub use monitor::{
    AccessoriesViewModel, AccessoryViewModel, ActiveAlertViewModel, AlertRuleViewModel,
    CoreGroupViewModel, MonitorReadoutSettingViewModel, MonitorReadoutViewModel, MonitorViewModel,
    accessory_icon,
};
pub use overlay::{
    AccessoryLevel, AccessoryReading, CONTROL_FEEDBACK, ControlFeedback, ControlReadings,
    MicrophoneReading, OverlayItem, OverlaySources, OverlayViewModel, VolumeReading,
    accessory_level, overlay_items,
};
pub use processes::{
    ListenerRowViewModel, ListenersViewModel, ProcessActionViewModel, ProcessRowViewModel,
    ProcessesViewModel,
};
pub(crate) use processes::{ListenersPresentation, ProcessesPresentation};
pub(crate) use recording::RecordingPresentation;
pub use recording::{
    RecordingControls, RecordingExportViewModel, RecordingTakeViewModel,
    RecordingTrackOptionViewModel, RecordingTrackViewModel, RecordingViewModel, format_clock,
};
pub(crate) use scratchpad::ScratchpadPresentation;

pub use scratchpad::{ScratchpadExpiredViewModel, ScratchpadTabViewModel, ScratchpadViewModel};
pub use screen_tools::{
    QrRowViewModel, ScreenToolButtonViewModel, ScreenToolResultViewModel, ScreenToolsViewModel,
};
pub(crate) use screen_tools::{ScreenToolGate, ScreenToolsPresentation};
pub(crate) use shortcuts::ShortcutsPresentation;
pub use shortcuts::{
    ShortcutChoiceViewModel, ShortcutRowViewModel, ShortcutsViewModel, shortcut_label,
};
pub(crate) use snippets::{SNIPPETS_OFF, SnippetsPresentation};
pub use snippets::{
    SnippetBoundsViewModel, SnippetDraft, SnippetDraftViewModel, SnippetItemViewModel,
    SnippetPolicyViewModel, SnippetQuery, SnippetsViewModel,
};
pub(crate) use speed_test::SpeedTestPresentation;
pub use speed_test::SpeedTestViewModel;
pub use whats_new::{WhatsNewSection, WhatsNewViewModel};

#[derive(Debug, Clone, PartialEq)]
pub struct ApplicationViewModel {
    pub features: Vec<FeatureViewModel>,
    pub quick_toggles: Vec<QuickToggleViewModel>,
    pub audio: AudioViewModel,
    pub microphone: MicrophoneViewModel,
    pub speed_test: SpeedTestViewModel,
    pub shortcuts: ShortcutsViewModel,
    pub capture: CaptureViewModel,
    pub processes: ProcessesViewModel,
    pub screen_tools: ScreenToolsViewModel,
    pub camera: CameraViewModel,
    pub recording: RecordingViewModel,
    pub listeners: ListenersViewModel,
    pub clipboard: ClipboardViewModel,
    pub snippets: SnippetsViewModel,
    pub scratchpad: ScratchpadViewModel,
    pub command_bar: CommandBarViewModel,
    pub quick_panel_favorites: Vec<String>,
    pub monitor: MonitorViewModel,
    pub warnings: Vec<ConfigurationWarningViewModel>,
    pub appearance: AppearancePreference,
    pub close_action: kestrel_core::CloseAction,
    pub autostart: bool,
    pub logging: LoggingViewModel,
    pub overlay: OverlayViewModel,
    pub can_undo: bool,
    pub overview_tools: OverviewToolsViewModel,
    pub whats_new: WhatsNewViewModel,
}

/// Logging settings and the log folder for the Settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingViewModel {
    pub debug: bool,
    /// `KESTREL_LOG=debug` keeps debug detail on whatever the setting says.
    pub debug_forced: bool,
    pub retention_days: u32,
    pub max_total_megabytes: u32,
    /// The log folder, when Kestrel can log.
    pub directory: Option<String>,
}

impl LoggingViewModel {
    fn new(
        configuration: &kestrel_core::LoggingConfiguration,
        logger: Option<&crate::logging::FileLogger>,
    ) -> Self {
        Self {
            debug: configuration.debug,
            debug_forced: logger.is_some_and(crate::logging::FileLogger::debug_forced),
            retention_days: configuration.retention_days,
            max_total_megabytes: configuration.max_total_megabytes,
            directory: logger.map(|logger| logger.directory().display().to_string()),
        }
    }
}
impl ApplicationViewModel {
    // Separate borrowed projections keep the service fields direct.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<'a>(
        registrations: impl Iterator<Item = &'a ServiceRegistration>,
        quick_toggles: impl Iterator<Item = &'a QuickToggleSnapshot>,
        audio: AudioPresentation<'a>,
        microphone: MicrophonePresentation<'a>,
        speed_test: SpeedTestPresentation,
        shortcuts: ShortcutsPresentation<'a>,
        capture: CapturePresentation,
        clipboard: ClipboardPresentation<'a>,
        snippets: SnippetsPresentation<'a>,
        command_bar: CommandBarPresentation<'a>,
        monitor: MonitorPresentation<'a>,
        warnings: &[ConfigurationWarning],
        configuration: &ApplicationConfiguration,
        can_undo: bool,
    ) -> Self {
        let registrations = registrations.collect::<Vec<_>>();
        Self {
            features: registrations
                .iter()
                .map(|registration| {
                    FeatureViewModel::from_registration(registration, configuration)
                })
                .collect(),
            quick_toggles: quick_toggles
                .map(|snapshot| {
                    let registration = registrations
                        .iter()
                        .find(|registration| registration.feature.id == snapshot.id.feature_id())
                        .copied();
                    QuickToggleViewModel::from_snapshot(snapshot, registration)
                })
                .collect(),
            audio: AudioViewModel::from_presentation(&configuration.audio, audio),
            microphone: MicrophoneViewModel::from_presentation(&configuration.audio, microphone),
            speed_test: SpeedTestViewModel::from_presentation(speed_test),
            shortcuts: ShortcutsViewModel::from_presentation(shortcuts),
            capture: CaptureViewModel::from_presentation(capture),
            processes: ProcessesViewModel::default(),
            screen_tools: ScreenToolsViewModel::default(),
            camera: CameraViewModel::default(),
            recording: RecordingViewModel::default(),
            listeners: ListenersViewModel::default(),
            clipboard: ClipboardViewModel::from_presentation(&configuration.clipboard, clipboard),
            snippets: SnippetsViewModel::from_presentation(snippets),
            scratchpad: ScratchpadViewModel::from_presentation(ScratchpadPresentation {
                warning: None,
                running: false,
                snapshot: None,
                limits: configuration.scratchpad,
            }),
            command_bar: CommandBarViewModel::from_presentation(command_bar),
            quick_panel_favorites: configuration.quick_panel.favorites.clone(),
            monitor: MonitorViewModel::from_configuration(&configuration.monitoring, monitor),
            warnings: warnings
                .iter()
                .map(ConfigurationWarningViewModel::from)
                .collect(),
            appearance: configuration.ui.appearance,
            close_action: configuration.ui.close_action,
            autostart: configuration.startup.autostart,
            logging: LoggingViewModel::new(&configuration.logging, crate::logging::logger()),
            overlay: OverlayViewModel::new(
                registrations
                    .iter()
                    .find(|registration| registration.feature.id == crate::OVERLAY_ID)
                    .copied(),
                configuration.overlay,
            ),
            can_undo,
            overview_tools: OverviewToolsViewModel::default(),
            whats_new: WhatsNewViewModel::default(),
        }
    }

    pub(crate) fn apply_tool_actions(&mut self) {
        let features = &self.features;
        self.capture.turn_on = turn_on_for(
            features,
            "capture.screenshot",
            "Screenshots are off",
            "Take screenshots of a window, area, or screen and keep recent captures.",
        );
        if let Some(status) =
            disabled_status_for(features, "capture.screenshot", "Screenshots are off")
        {
            self.capture.status = status;
        }
        for button in &mut self.screen_tools.buttons {
            let (feature_id, title, description) = match button.tool {
                ScreenTool::Text => (
                    "capture.text",
                    "Read text is off",
                    "Read text from a selected area of the screen.",
                ),
                ScreenTool::Qr => (
                    "capture.qr",
                    "Read QR code is off",
                    "Read QR codes from a selected area of the screen.",
                ),
                ScreenTool::Color => (
                    "capture.color",
                    "Pick colour is off",
                    "Pick a colour from any pixel on the screen.",
                ),
            };
            button.turn_on = turn_on_for(features, feature_id, title, description);
            if !button.enabled {
                if let Some(status) = disabled_status_for(features, feature_id, title) {
                    button.reason = Some(status);
                }
            }
        }
        if !self.screen_tools.available {
            let reasons = self
                .screen_tools
                .buttons
                .iter()
                .filter(|button| !button.enabled)
                .filter_map(|button| button.reason.as_deref())
                .collect::<Vec<_>>();
            if !reasons.is_empty() {
                self.screen_tools.status = reasons.join(" ");
            }
        }
        self.recording.turn_on = turn_on_for(
            features,
            "capture.recording",
            "Screen recording is off",
            "Record a screen, window, or area to a local video.",
        );
        if let Some(status) =
            disabled_status_for(features, "capture.recording", "Screen recording is off")
        {
            self.recording.status = status;
        }
        self.camera.turn_on = turn_on_for(
            features,
            "camera.preview",
            "Camera preview is off",
            "Preview a connected camera without saving frames.",
        );
        if let Some(status) =
            disabled_status_for(features, "camera.preview", "Camera preview is off")
        {
            self.camera.status = status;
        }
        self.snippets.turn_on = turn_on_for(
            features,
            "snippets.text",
            "Text snippets are off",
            "Store reusable text snippets and insert them quickly.",
        );
        if let Some(status) =
            disabled_status_for(features, "snippets.text", "Text snippets are off")
        {
            self.snippets.status = status;
        }
        self.scratchpad.turn_on = turn_on_for(
            features,
            "notes.scratchpad",
            "Scratchpad is off",
            "Keep private notes in tabs with Markdown preview.",
        );
        if let Some(status) = disabled_status_for(features, "notes.scratchpad", "Scratchpad is off")
        {
            self.scratchpad.status = status;
        }
        self.overview_tools = OverviewToolsViewModel::from_features(features);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConfigurationWarning;
    use crate::view_model::test_support::*;
    use kestrel_core::ApplicationConfiguration;

    use kestrel_services::{
        audio::AudioPolicy,
        microphone::MicrophoneSnapshot,
        snippets::{SnippetLibrary, SnippetPolicy},
    };

    #[test]
    fn copies_configuration_warnings_into_owned_presentation_state() {
        let warnings = vec![ConfigurationWarning {
            feature_id: "audio.mixer".to_string(),
            reason: "Invalid volume preference.".to_string(),
        }];
        let configuration = ApplicationConfiguration::default();
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            AudioPresentation {
                snapshot: None,
                running: false,
                policy: AudioPolicy::default(),
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            test_shortcuts_presentation(),
            test_capture_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                stopped_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
            },
            &warnings,
            &configuration,
            false,
        );

        assert!(view_model.features.is_empty());
        assert_eq!(view_model.warnings[0].feature_id, "audio.mixer");
        assert_eq!(view_model.warnings[0].message, "Invalid volume preference.");
    }
}
