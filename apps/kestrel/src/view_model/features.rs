//! Feature hub presentation.

use super::*;
use kestrel_core::{KEEP_AWAKE_PRESETS, KeepAwakeDuration};
use kestrel_platform::{
    audio::{FEATURE_ID as AUDIO_MIXER_FEATURE_ID, MICROPHONE_FEATURE_ID},
    camera::FEATURE_ID as CAMERA_FEATURE_ID,
    capture::FEATURE_ID as CAPTURE_FEATURE_ID,
    clipboard::FEATURE_ID as CLIPBOARD_FEATURE_ID,
    global_shortcuts::FEATURE_ID as GLOBAL_SHORTCUTS_FEATURE_ID,
    processes::{FEATURE_ID as PROCESSES_FEATURE_ID, LISTENERS_FEATURE_ID},
    quick_toggles::ALL_QUICK_TOGGLES,
    recording::FEATURE_ID as RECORDING_FEATURE_ID,
    screen_tools::{COLOR_FEATURE_ID, QR_FEATURE_ID, TEXT_FEATURE_ID},
    snippets::FEATURE_ID as SNIPPETS_FEATURE_ID,
    speed_test::FEATURE_ID as SPEED_TEST_FEATURE_ID,
    system_monitor::FEATURE_ID as SYSTEM_MONITOR_FEATURE_ID,
};
use kestrel_services::quick_toggles::KeepAwakeSession;

/// What a feature costs while it is not being used: background work and how
/// often it wakes up on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCostViewModel {
    pub idle: CostLevel,
    pub polling: CostLevel,
}

impl From<ResourceCost> for ResourceCostViewModel {
    fn from(cost: ResourceCost) -> Self {
        Self {
            idle: cost.idle,
            polling: cost.polling,
        }
    }
}

impl ResourceCostViewModel {
    /// One plain sentence, such as "Uses a little power in the background
    /// and wakes up often."
    pub fn power_use(self) -> String {
        let background = match self.idle {
            CostLevel::None => "no",
            CostLevel::Low => "a little",
            CostLevel::Moderate => "some",
        };
        let wakes = match self.polling {
            CostLevel::None => "never wakes up on its own",
            CostLevel::Low => "wakes up now and then",
            CostLevel::Moderate => "wakes up often",
        };
        format!("Uses {background} power in the background and {wakes}.")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepAwakeControlsViewModel {
    /// The presets, plus the current choice when it is custom.
    pub duration_choices: Vec<(KeepAwakeDuration, String)>,
    pub selected: KeepAwakeDuration,
    /// The running session's subtitle; `None` while it is off.
    pub session_text: Option<String>,
    /// A timed session has a deadline the tick checks.
    pub timed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickToggleViewModel {
    pub id: QuickToggleId,
    pub feature_id: &'static str,
    pub label: &'static str,
    pub requirement: &'static str,
    pub available: bool,
    pub detail: String,
    pub source: &'static str,
    pub control: Option<QuickToggleControlViewModel>,
    pub error: Option<String>,
    /// Present on the keep-awake row while its feature runs.
    pub keep_awake: Option<KeepAwakeControlsViewModel>,
}

impl QuickToggleViewModel {
    pub(crate) fn from_snapshot(
        snapshot: &QuickToggleSnapshot,
        registration: Option<&ServiceRegistration>,
    ) -> Self {
        let detail = snapshot
            .observation
            .as_ref()
            .map(|observation| observation.detail.clone())
            .or_else(|| snapshot.error.as_ref().map(|error| error.message.clone()))
            .or_else(|| registration.map(|registration| registration.capability.summary.clone()))
            .unwrap_or_else(|| "No capability report is available.".to_owned());
        Self {
            id: snapshot.id,
            feature_id: snapshot.id.feature_id(),
            label: snapshot.label,
            requirement: snapshot.requirement,
            available: registration.is_some_and(|registration| registration.running)
                && snapshot.observation.is_some(),
            detail,
            source: match snapshot.source {
                ToggleStateSource::KestrelOwned => "Kestrel-owned state",
                ToggleStateSource::ProviderObserved => "Provider-observed state",
                ToggleStateSource::ChangedElsewhere => "Changed outside Kestrel",
            },
            control: snapshot
                .observation
                .as_ref()
                .map(|observation| QuickToggleControlViewModel::from(&observation.control)),
            error: snapshot
                .observation
                .as_ref()
                .and(snapshot.error.as_ref())
                .map(|error| error.message.clone()),
            keep_awake: None,
        }
    }

    /// Adds the keep-awake duration picker and session line while it can run.
    pub(crate) fn attach_keep_awake(
        &mut self,
        snapshot: &QuickToggleSnapshot,
        selected: KeepAwakeDuration,
        now: std::time::Duration,
    ) {
        if self.id != QuickToggleId::KeepAwake || !self.available {
            return;
        }
        let mut duration_choices = KEEP_AWAKE_PRESETS
            .into_iter()
            .map(|duration| (duration, duration.label()))
            .collect::<Vec<_>>();
        if !duration_choices
            .iter()
            .any(|(duration, _)| *duration == selected)
        {
            duration_choices.push((selected, selected.label()));
        }
        let session = snapshot.keep_awake;
        self.keep_awake = Some(KeepAwakeControlsViewModel {
            duration_choices,
            selected,
            session_text: session.map(|session| session_text(&session, now)),
            timed: session.is_some_and(|session| session.deadline.is_some()),
        });
    }
}

/// The keep-awake row subtitle while a session runs.
fn session_text(session: &KeepAwakeSession, now: std::time::Duration) -> String {
    let Some(deadline) = session.deadline else {
        return "On until you turn it off".to_owned();
    };
    let remaining = deadline.saturating_sub(now);
    let minutes = remaining.as_secs().div_ceil(60);
    let label = if minutes == 0 {
        "less than a minute".to_owned()
    } else if minutes <= 60 {
        if minutes == 1 {
            "1 minute".to_owned()
        } else {
            format!("{minutes} minutes")
        }
    } else {
        let hours = minutes / 60;
        let rest = minutes % 60;
        if rest == 0 {
            format!("{hours} h")
        } else {
            format!("{hours} h {rest} min")
        }
    };
    let ends_at = kestrel_platform::snippets::LocalTime::from_system_time(
        std::time::SystemTime::now() + remaining,
    );
    format!(
        "On for {label} more · ends at {:02}:{:02}",
        ends_at.hour, ends_at.minute
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickToggleControlViewModel {
    Switch {
        enabled: bool,
        confirmation: Option<ConfirmationViewModel>,
    },
    Level {
        percentage: u8,
    },
    Actions(Vec<QuickToggleActionViewModel>),
}

impl From<&QuickToggleControl> for QuickToggleControlViewModel {
    fn from(control: &QuickToggleControl) -> Self {
        match control {
            QuickToggleControl::Switch {
                enabled,
                confirmation,
            } => Self::Switch {
                enabled: *enabled,
                confirmation: confirmation.as_ref().map(ConfirmationViewModel::from),
            },
            QuickToggleControl::Level { percentage } => Self::Level {
                percentage: *percentage,
            },
            QuickToggleControl::Actions(actions) => Self::Actions(
                actions
                    .iter()
                    .map(QuickToggleActionViewModel::from)
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationViewModel {
    pub scope: String,
    pub token: String,
}

impl From<&MutationConfirmation> for ConfirmationViewModel {
    fn from(confirmation: &MutationConfirmation) -> Self {
        Self {
            scope: confirmation.scope.clone(),
            token: confirmation.token.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickToggleActionViewModel {
    pub target: Option<String>,
    pub label: String,
    pub confirmation: ConfirmationViewModel,
}

impl From<&ToggleAction> for QuickToggleActionViewModel {
    fn from(action: &ToggleAction) -> Self {
        Self {
            target: action.target.clone(),
            label: action.label.clone(),
            confirmation: ConfirmationViewModel::from(&action.confirmation),
        }
    }
}
/// An action improving a capability state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemediationViewModel {
    pub title: &'static str,
    pub message: String,
}

fn remediation_title(status: &CapabilityStatus) -> &'static str {
    match status {
        CapabilityStatus::NeedsPermission { .. } => "Permission required",
        CapabilityStatus::MissingDependency { .. } => "Dependency required",
        CapabilityStatus::Unsupported { .. } => "Alternative action",
        CapabilityStatus::Limited { .. } => "Improve support",
        CapabilityStatus::Supported => "Suggested action",
    }
}

/// A disabled feature that can be enabled from the current panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOnViewModel {
    pub title: String,
    pub feature_id: String,
    pub description: String,
}

/// A one-click action on the Overview page for a feature that is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewTool {
    Screenshot,
    ReadText,
    ReadQrCode,
    PickColour,
    Record,
    Camera,
    Scratchpad,
    Snippets,
    ClipboardHistory,
    CommandBar,
    Monitoring,
    SpeedTest,
    AudioMixer,
    Processes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewToolViewModel {
    pub tool: OverviewTool,
    pub label: &'static str,
    pub icon: &'static str,
    pub feature_id: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OverviewToolsViewModel {
    pub tools: Vec<OverviewToolViewModel>,
}

impl OverviewToolsViewModel {
    pub fn from_features(features: &[FeatureViewModel]) -> Self {
        const TOOLS: [(OverviewTool, &str, &str, &str); 14] = [
            (
                OverviewTool::Screenshot,
                "Screenshot",
                "camera-photo-symbolic",
                "capture.screenshot",
            ),
            (
                OverviewTool::ReadText,
                "Read text",
                "insert-text-symbolic",
                "capture.text",
            ),
            (
                OverviewTool::ReadQrCode,
                "Read QR code",
                "scanner-symbolic",
                "capture.qr",
            ),
            (
                OverviewTool::PickColour,
                "Pick color",
                "color-select-symbolic",
                "capture.color",
            ),
            (
                OverviewTool::Record,
                "Record",
                "media-record-symbolic",
                "capture.recording",
            ),
            (
                OverviewTool::Camera,
                "Camera preview",
                "camera-web-symbolic",
                "camera.preview",
            ),
            (
                OverviewTool::Scratchpad,
                "Scratchpad",
                "document-edit-symbolic",
                "notes.scratchpad",
            ),
            (
                OverviewTool::Snippets,
                "Snippets",
                "text-x-generic-symbolic",
                "snippets.text",
            ),
            (
                OverviewTool::ClipboardHistory,
                "Clipboard history",
                "edit-paste-symbolic",
                "clipboard.history",
            ),
            (
                OverviewTool::CommandBar,
                "Command bar",
                "system-search-symbolic",
                "commands.bar",
            ),
            (
                OverviewTool::Monitoring,
                "Monitoring",
                "power-profile-performance-symbolic",
                "system.monitor",
            ),
            (
                OverviewTool::SpeedTest,
                "Run speed test",
                "network-transmit-receive-symbolic",
                "network.speed_test",
            ),
            (
                OverviewTool::AudioMixer,
                "Audio mixer",
                "audio-volume-high-symbolic",
                "audio.mixer",
            ),
            (
                OverviewTool::Processes,
                "Processes",
                "system-run-symbolic",
                "system.processes",
            ),
        ];
        Self {
            tools: TOOLS
                .into_iter()
                .filter_map(|(tool, label, icon, feature_id)| {
                    features
                        .iter()
                        .find(|feature| feature.id == feature_id)
                        // A tool that cannot run here keeps its reason in the hub.
                        .filter(|feature| feature.enabled && feature.available)
                        .map(|_| OverviewToolViewModel {
                            tool,
                            label,
                            icon,
                            feature_id,
                        })
                })
                .collect(),
        }
    }

    /// The Overview's opening line, matching whether tools appear below it.
    pub fn intro(&self) -> &'static str {
        if self.tools.is_empty() {
            "Turn features on in the Feature Hub. Each one says what it needs before it runs."
        } else {
            "Your tools are below. The Feature Hub turns on more, and each feature says what it needs."
        }
    }
}

/// Creates an in-place enable action only for a disabled, switchable feature.
pub fn turn_on_for(
    features: &[FeatureViewModel],
    feature_id: &str,
    title: &str,
    description: &str,
) -> Option<TurnOnViewModel> {
    features
        .iter()
        .find(|feature| feature.id == feature_id)
        .and_then(|feature| {
            (!feature.enabled && feature.switchable()).then(|| TurnOnViewModel {
                title: title.to_owned(),
                feature_id: feature.id.clone(),
                description: description.to_owned(),
            })
        })
}

/// A tool's status while its feature is off: why it cannot run here, or
/// just `off` when its Turn on button is offered.
pub fn disabled_status_for(
    features: &[FeatureViewModel],
    feature_id: &str,
    off: &str,
) -> Option<String> {
    features
        .iter()
        .find(|feature| feature.id == feature_id && !feature.enabled)
        .map(|feature| {
            if feature.switchable() {
                format!("{off}.")
            } else {
                feature.status_line()
            }
        })
}

/// The Feature Hub's titled groups, in the order it shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureHubSection {
    App,
    Monitoring,
    Audio,
    Capture,
    ClipboardAndNotes,
    OverviewControls,
}

impl FeatureHubSection {
    /// Every section, in display order.
    pub const ALL: [Self; 6] = [
        Self::App,
        Self::Monitoring,
        Self::Audio,
        Self::Capture,
        Self::ClipboardAndNotes,
        Self::OverviewControls,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Monitoring => "Monitoring",
            Self::Audio => "Audio",
            Self::Capture => "Capture",
            Self::ClipboardAndNotes => "Clipboard and notes",
            Self::OverviewControls => "Overview controls",
        }
    }

    /// A section's one-line explanation, where its rows need one.
    pub const fn description(self) -> Option<&'static str> {
        match self {
            Self::OverviewControls => Some(
                "Each adds a control to Overview's quick toggles. Turning one on doesn't change \
                 the setting itself.",
            ),
            _ => None,
        }
    }

    /// The section a feature belongs to. Every registered feature has one;
    /// the hub still hides the ones that are not a choice.
    pub fn of(feature_id: &str) -> Option<Self> {
        match feature_id {
            "app.command-surface"
            | crate::STATUS_NOTIFIER_ID
            | crate::OVERLAY_ID
            | GLOBAL_SHORTCUTS_FEATURE_ID
            | crate::runtime::COMMAND_BAR_ID => Some(Self::App),
            SYSTEM_MONITOR_FEATURE_ID
            | PROCESSES_FEATURE_ID
            | LISTENERS_FEATURE_ID
            | SPEED_TEST_FEATURE_ID => Some(Self::Monitoring),
            AUDIO_MIXER_FEATURE_ID | MICROPHONE_FEATURE_ID => Some(Self::Audio),
            CAPTURE_FEATURE_ID | RECORDING_FEATURE_ID | CAMERA_FEATURE_ID | TEXT_FEATURE_ID
            | QR_FEATURE_ID | COLOR_FEATURE_ID => Some(Self::Capture),
            CLIPBOARD_FEATURE_ID | SNIPPETS_FEATURE_ID | crate::runtime::SCRATCHPAD_ID => {
                Some(Self::ClipboardAndNotes)
            }
            _ if ALL_QUICK_TOGGLES
                .iter()
                .any(|toggle| toggle.feature_id() == feature_id) =>
            {
                Some(Self::OverviewControls)
            }
            _ => None,
        }
    }
}

/// State for one registered feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureViewModel {
    pub id: String,
    pub label: String,
    pub registered: bool,
    pub enabled: bool,
    pub available: bool,
    pub running: bool,
    pub configurable: bool,
    pub cost: ResourceCostViewModel,
    pub lifecycle: FeatureLifecycleViewModel,
    pub capability: CapabilityViewModel,
}

impl FeatureViewModel {
    pub(super) fn from_registration(
        registration: &ServiceRegistration,
        _configuration: &ApplicationConfiguration,
    ) -> Self {
        Self {
            id: registration.feature.id.to_string(),
            label: registration.feature.label.to_string(),
            registered: true,
            enabled: registration.enabled,
            available: registration.available,
            running: registration.running,
            configurable: registration.feature.configurable,
            cost: registration.feature.cost.into(),
            lifecycle: registration.lifecycle().into(),
            capability: CapabilityViewModel {
                status: CapabilityStatusViewModel::from(&registration.capability.status),
                summary: registration.capability.summary.clone(),
                selected_backend: registration.capability.selected_backend.clone(),
                remediation: registration.capability.remediation.as_ref().map(|message| {
                    RemediationViewModel {
                        title: remediation_title(&registration.capability.status),
                        message: message.clone(),
                    }
                }),
            },
        }
    }
}

impl From<&ServiceRegistration> for FeatureViewModel {
    fn from(registration: &ServiceRegistration) -> Self {
        Self::from_registration(registration, &ApplicationConfiguration::default())
    }
}

/// User-facing lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureLifecycleViewModel {
    Registered,
    Available,
    Unavailable,
    Running,
}

impl FeatureLifecycleViewModel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Registered => "Disabled",
            Self::Available => "Ready",
            Self::Unavailable => "Unavailable",
            Self::Running => "On",
        }
    }
}

impl From<ServiceLifecycle> for FeatureLifecycleViewModel {
    fn from(lifecycle: ServiceLifecycle) -> Self {
        match lifecycle {
            ServiceLifecycle::Registered => Self::Registered,
            ServiceLifecycle::Available => Self::Available,
            ServiceLifecycle::Unavailable => Self::Unavailable,
            ServiceLifecycle::Running => Self::Running,
        }
    }
}

/// State for one capability report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityViewModel {
    pub status: CapabilityStatusViewModel,
    pub summary: String,
    pub selected_backend: Option<String>,
    pub remediation: Option<RemediationViewModel>,
}

/// Which kind of capability report a feature has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityKind {
    Supported,
    Limited,
    NeedsPermission,
    MissingDependency,
    Unsupported,
}

/// Capability status and optional detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityStatusViewModel {
    pub kind: CapabilityKind,
    pub label: &'static str,
    pub detail: Option<String>,
}

impl From<&CapabilityStatus> for CapabilityStatusViewModel {
    fn from(status: &CapabilityStatus) -> Self {
        let (kind, label, detail) = match status {
            CapabilityStatus::Supported => (CapabilityKind::Supported, "Supported", None),
            CapabilityStatus::Limited { reason } => {
                (CapabilityKind::Limited, "Limited", Some(reason.clone()))
            }
            CapabilityStatus::NeedsPermission { permission } => (
                CapabilityKind::NeedsPermission,
                "Needs permission",
                Some(permission_label(*permission).to_string()),
            ),
            CapabilityStatus::MissingDependency { name } => (
                CapabilityKind::MissingDependency,
                "Missing dependency",
                Some(name.clone()),
            ),
            CapabilityStatus::Unsupported { reason } => (
                CapabilityKind::Unsupported,
                "Unsupported",
                Some(reason.clone()),
            ),
        };
        Self {
            kind,
            label,
            detail,
        }
    }
}

/// A feature's state at a glance on the Feature Hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureIndicator {
    /// On and running.
    Running,
    /// Missing a dependency or permission, working partly with a fix
    /// offered, or on but not running.
    NeedsAttention,
    /// This desktop cannot run it.
    Unavailable,
    /// Turned off, with nothing standing in its way.
    Off,
}

impl FeatureIndicator {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "On",
            Self::NeedsAttention => "Needs attention",
            Self::Unavailable => "Unavailable",
            Self::Off => "Off",
        }
    }
}

impl FeatureViewModel {
    /// Missing pieces warn even while the feature is off, so they are known
    /// before it is turned on. A partial capability warns only when it comes
    /// with a fix; otherwise it is a plain fact such as "nothing is playing".
    pub fn indicator(&self) -> FeatureIndicator {
        match self.capability.status.kind {
            CapabilityKind::Unsupported => FeatureIndicator::Unavailable,
            CapabilityKind::MissingDependency | CapabilityKind::NeedsPermission => {
                FeatureIndicator::NeedsAttention
            }
            CapabilityKind::Limited if self.capability.remediation.is_some() => {
                FeatureIndicator::NeedsAttention
            }
            _ if !self.enabled => FeatureIndicator::Off,
            _ if self.running => FeatureIndicator::Running,
            _ => FeatureIndicator::NeedsAttention,
        }
    }

    /// One line under the feature's name: its state, or what it needs.
    pub fn status_line(&self) -> String {
        let detail = self.capability.status.detail.as_deref().unwrap_or_default();
        let fix = self
            .capability
            .remediation
            .as_ref()
            .map(|remediation| remediation.message.as_str());
        let need = match (self.indicator(), self.capability.status.kind) {
            (FeatureIndicator::Running, _) => return "On".to_owned(),
            (FeatureIndicator::Off, _) => return "Off".to_owned(),
            (FeatureIndicator::Unavailable, _) => {
                return sentences("Not available here", Some(detail));
            }
            (_, CapabilityKind::MissingDependency) => format!("Needs {detail}"),
            (_, CapabilityKind::NeedsPermission) => {
                format!("Needs {} permission", detail.to_lowercase())
            }
            (_, CapabilityKind::Limited) => format!("Works partly: {detail}"),
            _ => "On, but not running".to_owned(),
        };
        sentences(&need, fix)
    }

    /// The switch works while the feature can run here, and to turn it off;
    /// a feature that cannot run is never offered as something to turn on.
    pub fn switchable(&self) -> bool {
        self.configurable && (self.available || self.enabled)
    }

    /// Everything technical, for the row's (i) tooltip.
    pub fn details(&self) -> String {
        let status = &self.capability.status;
        let mut lines = vec![
            self.capability.summary.clone(),
            format!("State: {}", self.lifecycle.label()),
            match &status.detail {
                Some(detail) => format!("Capability: {} ({detail})", status.label),
                None => format!("Capability: {}", status.label),
            },
        ];
        if let Some(backend) = &self.capability.selected_backend {
            lines.push(format!("Backend: {backend}"));
        }
        lines.push(self.cost.power_use());
        if let Some(remediation) = &self.capability.remediation {
            lines.push(format!("{}: {}", remediation.title, remediation.message));
        }
        if !self.configurable {
            lines.push("This feature cannot be turned off here.".to_owned());
        }
        lines.push(format!("Feature ID: {}", self.id));
        lines.join("\n")
    }
}

/// `first`, then `second` as its own capitalised sentence when there is one.
fn sentences(first: &str, second: Option<&str>) -> String {
    let Some(second) = second.map(str::trim).filter(|second| !second.is_empty()) else {
        return first.to_owned();
    };
    let mut characters = second.chars();
    let capital: String = characters
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .collect();
    format!(
        "{}. {capital}{}",
        first.trim_end_matches('.'),
        characters.as_str()
    )
}

fn permission_label(permission: Permission) -> &'static str {
    match permission {
        Permission::ScreenCapture => "Screen capture",
        Permission::GlobalShortcut => "Global shortcut",
        Permission::InputInjection => "Input injection",
        Permission::HardwareControl => "Hardware control",
        Permission::SessionControl => "Session control",
        Permission::DesktopSettings => "Desktop settings",
        Permission::NetworkControl => "Network control",
        Permission::FileDeletion => "File deletion",
        Permission::RemovableMedia => "Removable media",
        Permission::Camera => "Camera",
        Permission::Notifications => "Notifications",
    }
}

/// One isolated configuration warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationWarningViewModel {
    pub feature_id: String,
    pub message: String,
}

impl From<&ConfigurationWarning> for ConfigurationWarningViewModel {
    fn from(warning: &ConfigurationWarning) -> Self {
        Self {
            feature_id: warning.feature_id.clone(),
            message: warning.reason.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_model::test_support::*;
    use kestrel_core::{CapabilityReport, CapabilityStatus, FeatureSpec};

    #[test]
    fn in_place_enable_actions_follow_switchability() {
        let make = |id: &'static str, status: CapabilityStatus, enabled: bool, running: bool| {
            let spec = FeatureSpec::new(id, id, status.clone());
            let report = CapabilityReport::new(id, status, "Ready.");
            let registration =
                ServiceRegistration::new(spec, report, enabled).expect("matching feature IDs");
            let mut view = FeatureViewModel::from(&registration);
            view.running = running;
            view
        };
        let supported_off = make(
            "capture.screenshot",
            CapabilityStatus::Supported,
            false,
            false,
        );
        assert_eq!(
            turn_on_for(
                std::slice::from_ref(&supported_off),
                "capture.screenshot",
                "Screenshots are off",
                "Take screenshots."
            )
            .as_ref()
            .map(|action| action.feature_id.as_str()),
            Some("capture.screenshot")
        );
        let unsupported = make(
            "capture.screenshot",
            CapabilityStatus::Unsupported {
                reason: "No provider.".to_owned(),
            },
            false,
            false,
        );
        assert!(
            turn_on_for(
                std::slice::from_ref(&unsupported),
                "capture.screenshot",
                "Screenshots are off",
                "Take screenshots."
            )
            .is_none()
        );
        let on = make(
            "capture.screenshot",
            CapabilityStatus::Supported,
            true,
            true,
        );
        assert!(
            turn_on_for(
                std::slice::from_ref(&on),
                "capture.screenshot",
                "Screenshots are off",
                "Take screenshots."
            )
            .is_none()
        );
    }
    #[test]
    fn off_tools_show_their_reason_or_a_plain_off_status() {
        let status = CapabilityStatus::MissingDependency {
            name: "tesseract".to_owned(),
        };
        let registration = ServiceRegistration::new(
            FeatureSpec::new("capture.text", "Read text", status.clone()),
            CapabilityReport::new("capture.text", status, "Text recognition is unavailable."),
            false,
        )
        .expect("matching feature IDs");
        let feature = FeatureViewModel::from(&registration);
        assert!(
            turn_on_for(
                std::slice::from_ref(&feature),
                "capture.text",
                "Read text is off",
                "Read text."
            )
            .is_none()
        );
        let reason = disabled_status_for(
            std::slice::from_ref(&feature),
            "capture.text",
            "Read text is off",
        )
        .expect("missing dependency has a reason");
        assert!(reason.contains("Needs tesseract"), "{reason}");

        let registration = ServiceRegistration::new(
            FeatureSpec::new("capture.qr", "Read QR code", CapabilityStatus::Supported),
            CapabilityReport::new("capture.qr", CapabilityStatus::Supported, "Ready."),
            false,
        )
        .expect("matching feature IDs");
        let feature = FeatureViewModel::from(&registration);
        assert_eq!(
            disabled_status_for(
                std::slice::from_ref(&feature),
                "capture.qr",
                "Read QR code is off"
            )
            .as_deref(),
            Some("Read QR code is off.")
        );
    }

    #[test]
    fn overview_tools_are_on_in_fixed_order() {
        // (feature, on): shuffled, with one feature off and Read text on but
        // missing its dependency.
        let features_on = [
            ("network.speed_test", true),
            ("notes.scratchpad", true),
            ("audio.mixer", true),
            ("capture.color", true),
            ("system.processes", false),
            ("capture.screenshot", true),
            ("system.monitor", true),
            ("commands.bar", true),
            ("capture.recording", true),
            ("clipboard.history", true),
            ("capture.qr", true),
            ("camera.preview", true),
            ("snippets.text", true),
            ("capture.text", true),
        ];
        let registrations = features_on
            .into_iter()
            .map(|(id, on)| {
                let status = if id == "capture.text" {
                    CapabilityStatus::MissingDependency {
                        name: "tesseract".to_owned(),
                    }
                } else {
                    CapabilityStatus::Supported
                };
                ServiceRegistration::new(
                    FeatureSpec::new(id, id, status.clone()),
                    CapabilityReport::new(id, status, "Ready."),
                    on,
                )
                .expect("matching feature IDs")
            })
            .collect::<Vec<_>>();
        let features = registrations
            .iter()
            .map(FeatureViewModel::from)
            .collect::<Vec<_>>();
        let tools = OverviewToolsViewModel::from_features(&features);
        assert_eq!(
            tools
                .tools
                .iter()
                .map(|tool| tool.label)
                .collect::<Vec<_>>(),
            [
                "Screenshot",
                "Read QR code",
                "Pick color",
                "Record",
                "Camera preview",
                "Scratchpad",
                "Snippets",
                "Clipboard history",
                "Command bar",
                "Monitoring",
                "Run speed test",
                "Audio mixer",
            ]
        );
        assert_eq!(
            tools.intro(),
            "Your tools are below. The Feature Hub turns on more, and each feature says what it needs."
        );
        assert_eq!(
            OverviewToolsViewModel::default().intro(),
            "Turn features on in the Feature Hub. Each one says what it needs before it runs."
        );
    }

    #[test]
    fn extracts_disabled_supported_feature_without_runtime_ownership() {
        let feature = FeatureSpec::new("test.feature", "Test feature", CapabilityStatus::Supported);
        let report = CapabilityReport::new(
            "test.feature",
            CapabilityStatus::Supported,
            "The feature is ready.",
        )
        .with_selected_backend("Test backend");
        let registration =
            ServiceRegistration::new(feature, report, false).expect("matching feature IDs");

        let view_model = FeatureViewModel::from(&registration);

        assert_eq!(view_model.id, "test.feature");
        assert!(!view_model.enabled);
        assert!(view_model.registered);
        assert!(view_model.configurable);
        assert_eq!(view_model.lifecycle, FeatureLifecycleViewModel::Registered);
        assert_eq!(view_model.lifecycle.label(), "Disabled");
        assert_eq!(view_model.capability.status.label, "Supported");
        assert_eq!(
            view_model.capability.selected_backend.as_deref(),
            Some("Test backend")
        );
    }

    #[test]
    fn preserves_unavailable_capability_detail_and_remediation() {
        let feature = FeatureSpec::new(
            "test.shortcuts",
            "Shortcuts",
            CapabilityStatus::NeedsPermission {
                permission: Permission::GlobalShortcut,
            },
        );
        let report = CapabilityReport::new(
            "test.shortcuts",
            CapabilityStatus::NeedsPermission {
                permission: Permission::GlobalShortcut,
            },
            "Shortcut permission is required.",
        )
        .with_remediation("Grant access or use the normal window.");
        let registration =
            ServiceRegistration::new(feature, report, true).expect("matching feature IDs");

        let view_model = FeatureViewModel::from(&registration);

        assert_eq!(view_model.lifecycle, FeatureLifecycleViewModel::Unavailable);
        assert_eq!(view_model.capability.status.label, "Needs permission");
        assert_eq!(
            view_model.capability.status.detail.as_deref(),
            Some("Global shortcut")
        );
        assert_eq!(
            view_model
                .capability
                .remediation
                .as_ref()
                .map(|value| (value.title, value.message.as_str())),
            Some((
                "Permission required",
                "Grant access or use the normal window."
            ))
        );
    }

    #[test]
    fn unavailable_features_can_be_turned_off_but_not_on() {
        let feature = |status: CapabilityStatus, enabled: bool| {
            let spec = FeatureSpec::new("test.feature", "Test feature", status.clone());
            let report = CapabilityReport::new("test.feature", status, "Summary.");
            FeatureViewModel::from(
                &ServiceRegistration::new(spec, report, enabled).expect("matching feature IDs"),
            )
        };
        let missing = || CapabilityStatus::MissingDependency {
            name: "Tesseract".to_owned(),
        };
        let unsupported = || CapabilityStatus::Unsupported {
            reason: "No keyboard backlight device was detected.".to_owned(),
        };

        assert!(feature(CapabilityStatus::Supported, false).switchable());
        assert!(
            !feature(unsupported(), false).switchable(),
            "nothing to turn on"
        );
        assert!(!feature(missing(), false).switchable());
        assert!(
            feature(unsupported(), true).switchable(),
            "a feature left on can still be turned off"
        );
        let mut fixed = feature(CapabilityStatus::Supported, false);
        fixed.configurable = false;
        assert!(!fixed.switchable(), "a fixed feature never switches");
    }

    #[test]
    fn feature_hub_rows_show_state_or_need_with_details_aside() {
        let row = |status: CapabilityStatus, fix: Option<&str>, enabled: bool, running: bool| {
            let feature = FeatureSpec::new("test.feature", "Test feature", status.clone());
            let mut report = CapabilityReport::new("test.feature", status, "Reads the screen.")
                .with_selected_backend("Portal");
            if let Some(fix) = fix {
                report = report.with_remediation(fix);
            }
            let registration =
                ServiceRegistration::new(feature, report, enabled).expect("matching feature IDs");
            let mut view = FeatureViewModel::from(&registration);
            view.running = running;
            view
        };
        let shown = |view: &FeatureViewModel| (view.indicator(), view.status_line());
        for (case, status, fix, enabled, running, expected) in [
            (
                "running",
                CapabilityStatus::Supported,
                None,
                true,
                true,
                (FeatureIndicator::Running, "On"),
            ),
            (
                "off",
                CapabilityStatus::Supported,
                None,
                false,
                false,
                (FeatureIndicator::Off, "Off"),
            ),
            (
                "on but stopped",
                CapabilityStatus::Supported,
                None,
                true,
                false,
                (FeatureIndicator::NeedsAttention, "On, but not running"),
            ),
            (
                "missing dependency",
                CapabilityStatus::MissingDependency {
                    name: "Tesseract".to_owned(),
                },
                Some("Install it with sudo apt install tesseract-ocr."),
                false,
                false,
                (
                    FeatureIndicator::NeedsAttention,
                    "Needs Tesseract. Install it with sudo apt install tesseract-ocr.",
                ),
            ),
            (
                "missing permission",
                CapabilityStatus::NeedsPermission {
                    permission: Permission::HardwareControl,
                },
                None,
                true,
                false,
                (
                    FeatureIndicator::NeedsAttention,
                    "Needs hardware control permission",
                ),
            ),
            (
                "unsupported",
                CapabilityStatus::Unsupported {
                    reason: "this session exports no portal.".to_owned(),
                },
                Some("Bind kestrel --command instead."),
                false,
                false,
                (
                    FeatureIndicator::Unavailable,
                    "Not available here. This session exports no portal.",
                ),
            ),
            (
                "limited without fix",
                CapabilityStatus::Limited {
                    reason: "There are no active playback streams.".to_owned(),
                },
                None,
                true,
                true,
                (FeatureIndicator::Running, "On"),
            ),
            (
                "limited with fix",
                CapabilityStatus::Limited {
                    reason: "no insertion provider is installed".to_owned(),
                },
                Some("Install wtype or xdotool."),
                true,
                true,
                (
                    FeatureIndicator::NeedsAttention,
                    "Works partly: no insertion provider is installed. Install wtype or xdotool.",
                ),
            ),
        ] {
            assert_eq!(
                shown(&row(status, fix, enabled, running)),
                (expected.0, expected.1.to_owned()),
                "{case}"
            );
        }

        let details = row(CapabilityStatus::Supported, None, true, true).details();
        for part in [
            "Reads the screen.",
            "Backend: Portal",
            "Uses no power in the background and never wakes up on its own.",
            "Feature ID: test.feature",
        ] {
            assert!(details.contains(part), "{part:?} in {details:?}");
        }
    }

    #[test]
    fn unavailable_quick_toggle_uses_its_capability_summary() {
        let id = QuickToggleId::Brightness;
        let status = CapabilityStatus::Unsupported {
            reason: "No internal backlight was found.".to_owned(),
        };
        let report = CapabilityReport::new(
            id.feature_id(),
            status.clone(),
            "No backlight adapter exists.",
        );
        let feature = FeatureSpec::new(id.feature_id(), id.label(), status);
        let registration =
            ServiceRegistration::new(feature, report, true).expect("matching feature IDs");
        let snapshot = QuickToggleSnapshot {
            id,
            label: id.label(),
            requirement: id.requirement(),
            observation: None,
            source: ToggleStateSource::ProviderObserved,
            error: None,
            keep_awake: None,
        };

        let configuration = ApplicationConfiguration::default();
        let view_model = ApplicationViewModel::new(
            std::iter::once(&registration),
            std::iter::once(&snapshot),
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
            &[],
            &configuration,
            false,
        );
        assert!(!view_model.quick_toggles[0].available);
        assert_eq!(
            view_model.quick_toggles[0].detail,
            "No backlight adapter exists."
        );
    }

    #[test]
    fn every_registered_feature_has_a_hub_section() {
        let runtime = crate::ApplicationRuntime::new(&ApplicationConfiguration::default())
            .expect("runtime builds");
        for registration in runtime.registrations() {
            assert!(
                FeatureHubSection::of(registration.feature.id).is_some(),
                "{} has a hub section",
                registration.feature.id
            );
        }
        for id in ALL_QUICK_TOGGLES {
            assert_eq!(
                FeatureHubSection::of(id.feature_id()),
                Some(FeatureHubSection::OverviewControls),
                "{} is an Overview control",
                id.feature_id()
            );
        }
    }
}
