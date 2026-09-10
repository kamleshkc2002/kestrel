use crate::{ApplicationViewModel, ConfigurationWarning};
use kestrel_core::{ApplicationConfiguration, CapabilityReport, CapabilityStatus, FeatureSpec};
use kestrel_platform::{
    audio::{PulseAudioBackend, FEATURE_ID as AUDIO_MIXER_ID},
    clipboard::{
        discover_provider, ArboardClipboardBackend, ClipboardCapabilityProbe, LogindPrivacyMonitor,
        FEATURE_ID as CLIPBOARD_HISTORY_ID,
    },
    system_monitor::{ProcSysMonitor, FEATURE_ID as SYSTEM_MONITOR_ID},
    StaticCapabilityProbe,
};
use kestrel_services::{
    audio::{AudioCommand, AudioCommandResult, AudioMixerService, AudioSnapshot},
    clipboard::{
        ClipboardCommand, ClipboardHistoryService, ClipboardPolicy, ClipboardServiceError,
        ClipboardSnapshot,
    },
    system_monitor::{
        RefreshOutcome, SystemMonitorService, SystemSnapshot, DEFAULT_REFRESH_INTERVAL,
    },
    FeatureRegistry, RegistryError, ServiceRegistration,
};
use std::time::Duration;

/// UI-independent composition root for startup, enablement, and capability refresh.
pub struct ApplicationRuntime {
    registry: FeatureRegistry,
    audio_mixer: AudioMixerService<PulseAudioBackend>,
    clipboard_history: ClipboardHistoryService,
    system_monitor: SystemMonitorService<ProcSysMonitor>,
}

impl ApplicationRuntime {
    /// Registers the entry surfaces known before concrete feature services land.
    pub fn new(configuration: &ApplicationConfiguration) -> Result<Self, RegistryError> {
        let mut registry = FeatureRegistry::default();

        let command_surface = FeatureSpec::new(
            "app.command-surface",
            "Command surface",
            CapabilityStatus::Supported,
        );
        registry.register_probe(
            command_surface.clone(),
            true,
            StaticCapabilityProbe::new(
                CapabilityReport::new(
                    command_surface.id,
                    CapabilityStatus::Supported,
                    "The normal Kestrel window is always available as the command surface.",
                )
                .with_selected_backend("GTK/libadwaita normal window"),
            ),
        )?;

        let system_monitor = FeatureSpec::new(
            SYSTEM_MONITOR_ID,
            "System monitor",
            CapabilityStatus::Supported,
        );
        let monitor_source = ProcSysMonitor::default();
        registry.register_probe(
            system_monitor,
            configuration.feature_enabled(SYSTEM_MONITOR_ID),
            monitor_source.clone(),
        )?;
        let audio_mixer =
            FeatureSpec::new(AUDIO_MIXER_ID, "Audio mixer", CapabilityStatus::Supported);
        let audio_backend = PulseAudioBackend::new();
        registry.register_probe(
            audio_mixer,
            configuration.feature_enabled(AUDIO_MIXER_ID),
            audio_backend,
        )?;
        let clipboard_history = FeatureSpec::new(
            CLIPBOARD_HISTORY_ID,
            "Clipboard history",
            CapabilityStatus::Supported,
        );
        registry.register_probe(
            clipboard_history,
            configuration.feature_enabled(CLIPBOARD_HISTORY_ID),
            ClipboardCapabilityProbe::new(),
        )?;
        let global_shortcuts = FeatureSpec::new(
            "global.shortcuts",
            "Global shortcuts",
            CapabilityStatus::Unsupported {
                reason: "No portable global-shortcut adapter is registered yet.".to_string(),
            },
        );
        registry.register_probe(
            global_shortcuts.clone(),
            configuration.feature_enabled(global_shortcuts.id),
            StaticCapabilityProbe::new(
                CapabilityReport::new(
                    global_shortcuts.id,
                    CapabilityStatus::Unsupported {
                        reason: "No portable global-shortcut adapter is registered yet.".to_string(),
                    },
                    "Global shortcuts are unavailable, but Kestrel remains usable from its normal window.",
                )
                .with_remediation(
                    "Use the normal window or configure a desktop shortcut that launches Kestrel.",
                ),
            ),
        )?;
        Ok(Self {
            registry,
            audio_mixer: AudioMixerService::new(audio_backend),
            clipboard_history: ClipboardHistoryService::new(ClipboardPolicy::default())
                .expect("the built-in clipboard policy is valid"),
            system_monitor: SystemMonitorService::new(monitor_source, DEFAULT_REFRESH_INTERVAL)
                .expect("the built-in monitor refresh interval is valid"),
        })
    }

    /// Starts only the entries that are enabled and currently available.
    pub fn start(&mut self) {
        self.registry.start_enabled();
        if self.system_monitor_is_running() {
            self.system_monitor.refresh(Duration::ZERO);
        }
        if self.audio_mixer_is_running() {
            let _ = self.audio_mixer.refresh();
        }
        if self.clipboard_history_is_running() {
            let _ = self.start_clipboard_history();
        }
    }

    /// Refreshes every cached capability report through its feature-specific probe.
    pub fn refresh_capabilities(&mut self) -> Result<(), RegistryError> {
        let feature_ids = self
            .registry
            .registrations()
            .map(|registration| registration.feature.id)
            .collect::<Vec<_>>();
        for feature_id in feature_ids {
            self.registry.refresh_capability(feature_id)?;
        }
        Ok(())
    }

    /// Returns UI-independent registration snapshots for the active session.
    pub fn registrations(&self) -> impl Iterator<Item = &ServiceRegistration> {
        self.registry.registrations()
    }
    /// Extracts owned presentation state without exposing live service resources.
    pub fn view_model(&self, warnings: &[ConfigurationWarning]) -> ApplicationViewModel {
        ApplicationViewModel::new(self.registrations(), warnings)
    }

    /// Samples monitor metrics when the feature is running and its interval elapsed.
    pub fn refresh_system_monitor(&mut self, observed_at: Duration) -> RefreshOutcome {
        if self.system_monitor_is_running() {
            self.system_monitor.refresh(observed_at)
        } else {
            RefreshOutcome::Skipped
        }
    }

    /// Returns the latest immutable monitor snapshot, if sampling has started.
    pub fn system_monitor_snapshot(&self) -> Option<&SystemSnapshot> {
        self.system_monitor.latest()
    }
    /// Returns the latest audio snapshot, including structured unavailable/empty states.
    pub fn audio_snapshot(&self) -> &AudioSnapshot {
        self.audio_mixer.latest()
    }

    /// Applies an audio command only while the opt-in mixer service is running.
    pub fn execute_audio_command(&mut self, command: AudioCommand) -> Option<AudioCommandResult> {
        self.audio_mixer_is_running()
            .then(|| self.audio_mixer.execute(command))
    }
    /// Returns metadata about retained clipboard items without exposing their contents.
    pub fn clipboard_snapshot(&self) -> ClipboardSnapshot {
        self.clipboard_history.latest()
    }

    /// Applies a clipboard command only while the opt-in service is running.
    pub fn execute_clipboard_command(
        &self,
        command: ClipboardCommand,
    ) -> Option<Result<ClipboardSnapshot, ClipboardServiceError>> {
        self.clipboard_history_is_running()
            .then(|| self.clipboard_history.execute(command))
    }

    fn start_clipboard_history(&mut self) -> Result<(), ClipboardServiceError> {
        let provider = discover_provider().map_err(ClipboardServiceError::Backend)?;
        let backend =
            ArboardClipboardBackend::new(provider).map_err(ClipboardServiceError::Backend)?;
        let privacy = LogindPrivacyMonitor::new().map_err(ClipboardServiceError::Backend)?;
        self.clipboard_history.start(backend, privacy)
    }

    fn clipboard_history_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == CLIPBOARD_HISTORY_ID && registration.running
        })
    }

    fn audio_mixer_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == AUDIO_MIXER_ID && registration.running)
    }

    fn system_monitor_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == SYSTEM_MONITOR_ID && registration.running
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ApplicationRuntime;
    use kestrel_core::ApplicationConfiguration;
    use kestrel_platform::{
        audio::FEATURE_ID as AUDIO_MIXER_ID, clipboard::FEATURE_ID as CLIPBOARD_HISTORY_ID,
        system_monitor::FEATURE_ID as SYSTEM_MONITOR_ID,
    };
    use kestrel_services::{
        audio::{AudioAvailability, AudioCommand},
        clipboard::{ClipboardCommand, ClipboardLifecycle},
    };

    #[test]
    fn startup_keeps_unavailable_features_visible() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        runtime
            .refresh_capabilities()
            .expect("static capability refreshes");

        let registrations = runtime.registrations().collect::<Vec<_>>();
        assert_eq!(registrations.len(), 5);
        assert!(registrations[0].running);
        assert_eq!(registrations[1].feature.id, SYSTEM_MONITOR_ID);
        assert_eq!(registrations[2].feature.id, AUDIO_MIXER_ID);
        assert!(!registrations[2].running);
        assert_eq!(registrations[3].feature.id, CLIPBOARD_HISTORY_ID);
        assert!(!registrations[3].running);
        assert!(!registrations[4].available);
        assert!(registrations[4].capability.remediation.is_some());
    }

    #[test]
    fn enabled_monitor_starts_with_a_non_failing_live_snapshot() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");

        runtime.start();

        assert!(runtime.system_monitor_snapshot().is_some());
    }

    #[test]
    fn disabled_audio_service_does_not_accept_commands() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        assert_eq!(
            runtime.audio_snapshot().availability,
            AudioAvailability::Unavailable
        );
        assert!(runtime
            .execute_audio_command(AudioCommand::SetStreamMute {
                stream_id: 1,
                muted: true,
            })
            .is_none());
    }

    #[test]
    fn clipboard_history_is_inert_until_explicitly_enabled() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        assert_eq!(
            runtime.clipboard_snapshot().lifecycle,
            ClipboardLifecycle::Stopped
        );
        assert!(runtime
            .execute_clipboard_command(ClipboardCommand::Wipe)
            .is_none());
    }
}
