//! Capability probes shared by runtime features.

use super::*;

pub(super) struct SharedCaptureProbe(pub(super) Arc<Mutex<Option<CaptureDetection>>>);

impl CapabilityProbe for SharedCaptureProbe {
    fn probe(&self) -> CapabilityReport {
        let detection = CaptureDetection::detect();
        let report = detection.capability();
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(detection);
        report
    }
}

pub(super) struct SharedRecordingProbe(pub(super) Arc<Mutex<Option<RecordingDetection>>>);

impl CapabilityProbe for SharedRecordingProbe {
    fn probe(&self) -> CapabilityReport {
        let detection = RecordingDetection::detect();
        let report = detection.capability();
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(detection);
        report
    }
}

pub(super) struct ScreenToolProbe {
    pub(super) tool: ScreenTool,
    pub(super) detection: Arc<Mutex<Option<CaptureDetection>>>,
}

impl CapabilityProbe for ScreenToolProbe {
    fn probe(&self) -> CapabilityReport {
        // The capture probe registers first, so its detection is current.
        let mut detection = self
            .detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let detection = detection.get_or_insert_with(CaptureDetection::detect);
        screen_tool_capability(
            self.tool,
            detection,
            TesseractAdapter::discover().executable(),
        )
    }
}

/// Why text cannot be read from clipboard images, when Tesseract is missing.
pub(super) fn clipboard_ocr_unavailable() -> Option<String> {
    TesseractAdapter::discover()
        .executable()
        .is_none()
        .then(|| {
            "Install Tesseract (the tesseract-ocr package) to copy text from images.".to_owned()
        })
}

pub(super) fn ocr_recognizer(
    settings: &kestrel_core::ScreenToolsConfiguration,
) -> Arc<dyn TextRecognizer> {
    Arc::new(TesseractRecognizer {
        adapter: TesseractAdapter::discover(),
        language: settings.ocr_language.clone(),
        timeout: Duration::from_secs(u64::from(settings.ocr_timeout_seconds)),
    })
}

/// Reports the shelf, which reads other apps' files through a portal in a
/// sandbox.
pub(super) struct ShelfProbe(pub(super) bool);

impl CapabilityProbe for ShelfProbe {
    fn probe(&self) -> CapabilityReport {
        if self.0 {
            return kestrel_platform::sandbox::limited(
                SHELF_ID,
                "Dragged entries work; files from other apps arrive through the document portal.",
                kestrel_platform::sandbox::SandboxReason::ShelfFiles,
            );
        }
        CapabilityReport::new(
            SHELF_ID,
            CapabilityStatus::Supported,
            "Dragging entries in and out uses the window's own drag and drop.",
        )
    }
}

/// Reports available providers while preserving portable commands.
pub struct CommandBarProbe {
    configuration: kestrel_core::CommandBarConfiguration,
    sandboxed: bool,
}

impl CommandBarProbe {
    pub fn new(configuration: kestrel_core::CommandBarConfiguration) -> Self {
        Self::with_sandbox(configuration, kestrel_platform::sandbox::flatpak())
    }

    /// Sets the sandbox state explicitly, for fixtures outside a sandbox.
    pub fn with_sandbox(
        configuration: kestrel_core::CommandBarConfiguration,
        sandboxed: bool,
    ) -> Self {
        Self {
            configuration,
            sandboxed,
        }
    }
}

impl CapabilityProbe for CommandBarProbe {
    fn probe(&self) -> CapabilityReport {
        let applications = scan_applications(&application_directories(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("XDG_DATA_DIRS").as_deref(),
            std::env::var_os("HOME").as_deref(),
        ));
        let launcher = ExternalLauncher::discover();
        let roots = self.configuration.file_roots.len();
        let scripts = self.configuration.scripts.len();
        let terminal_entries = applications
            .iter()
            .filter(|application| application.terminal)
            .count();

        let mut report = CapabilityReport::new(
            COMMAND_BAR_ID,
            CapabilityStatus::Supported,
            format!(
                "Portable commands plus {} applications, {} scripts, and {} file roots.",
                if self.configuration.enable_applications {
                    applications.len().to_string()
                } else {
                    "0 (disabled)".to_string()
                },
                if self.configuration.enable_scripts {
                    scripts.to_string()
                } else {
                    "0 (disabled)".to_string()
                },
                if self.configuration.enable_files {
                    roots.to_string()
                } else {
                    "0 (disabled)".to_string()
                }
            ),
        )
        .with_selected_backend("In-process command index")
        .with_evidence(CapabilityEvidence::new("portable_commands", "always"))
        .with_evidence(CapabilityEvidence::new(
            "application_count",
            applications.len().to_string(),
        ))
        .with_evidence(CapabilityEvidence::new(
            "terminal_applications",
            terminal_entries.to_string(),
        ))
        .with_evidence(CapabilityEvidence::new(
            "launch_handler",
            launcher
                .as_ref()
                .map(|launcher| launcher.handler_name().to_string())
                .unwrap_or_else(|_| "unavailable".to_string()),
        ))
        .with_evidence(CapabilityEvidence::new("file_roots", roots.to_string()))
        .with_evidence(CapabilityEvidence::new("scripts", scripts.to_string()));

        if launcher.is_err() {
            report = report.with_remediation(
                "Install xdg-open to open links and files from the command bar. Applications \
                 launch through the desktop's own launcher, and commands that only use \
                 Kestrel data keep working without it.",
            );
        }
        if self.configuration.enable_files && roots == 0 {
            report = report.with_remediation(
                "File search needs at least one configured root; the whole filesystem is never \
                 indexed.",
            );
        }
        if self.sandboxed
            && (self.configuration.enable_applications
                || self.configuration.enable_files
                || self.configuration.enable_windows)
        {
            let reason = kestrel_platform::sandbox::SandboxReason::CommandSources;
            report.status = CapabilityStatus::Limited {
                reason: reason.reason().to_owned(),
            };
            report = report.with_evidence(CapabilityEvidence::new("sandbox", reason.key()));
            if let Some(remediation) = reason.remediation() {
                report = report.with_remediation(remediation);
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use kestrel_core::CapabilityStatus;

    use super::{CommandBarProbe, ShelfProbe};
    use kestrel_platform::CapabilityProbe;

    fn probe(applications: bool, files: bool, windows: bool) -> kestrel_core::CapabilityReport {
        let configuration = kestrel_core::CommandBarConfiguration {
            enable_applications: applications,
            enable_files: files,
            enable_windows: windows,
            ..kestrel_core::CommandBarConfiguration::default()
        };
        CommandBarProbe::with_sandbox(configuration, true).probe()
    }

    #[test]
    fn a_sandboxed_command_bar_limits_host_sources_only() {
        for report in [
            probe(true, false, false),
            probe(false, true, false),
            probe(false, false, true),
        ] {
            assert!(matches!(report.status, CapabilityStatus::Limited { .. }));
            assert!(
                report
                    .evidence
                    .iter()
                    .any(|item| item.key == "sandbox" && item.value == "command-sources")
            );
        }

        let portable = probe(false, false, false);
        assert!(!matches!(portable.status, CapabilityStatus::Limited { .. }));
        assert!(!portable.evidence.iter().any(|item| item.key == "sandbox"));
    }

    #[test]
    fn a_sandboxed_shelf_limits_files_from_other_apps() {
        let report = ShelfProbe(true).probe();
        assert!(matches!(report.status, CapabilityStatus::Limited { .. }));
        assert!(
            report
                .evidence
                .iter()
                .any(|item| item.key == "sandbox" && item.value == "shelf-files")
        );

        assert!(matches!(
            ShelfProbe(false).probe().status,
            CapabilityStatus::Supported
        ));
    }
}
