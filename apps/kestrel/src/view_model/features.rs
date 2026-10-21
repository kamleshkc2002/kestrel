//! Feature hub presentation.

use super::*;

/// Feature-hub resource costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCostViewModel {
    pub idle: CostLevel,
    pub interaction: CostLevel,
    pub polling: CostLevel,
}

impl From<ResourceCost> for ResourceCostViewModel {
    fn from(cost: ResourceCost) -> Self {
        Self {
            idle: cost.idle,
            interaction: cost.interaction,
            polling: cost.polling,
        }
    }
}

impl ResourceCostViewModel {
    pub fn summary(self) -> String {
        format!(
            "Idle: {}; interaction: {}; polling: {}",
            cost_label(self.idle),
            cost_label(self.interaction),
            cost_label(self.polling)
        )
    }
}

fn cost_label(cost: CostLevel) -> &'static str {
    match cost {
        CostLevel::None => "none",
        CostLevel::Low => "low",
        CostLevel::Moderate => "moderate",
    }
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
}

impl QuickToggleViewModel {
    pub(super) fn from_snapshot(
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
        }
    }
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
            Self::Running => "Running",
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
            Self::Running => "Running",
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
            (FeatureIndicator::Running, _) => return "Running".to_owned(),
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
        lines.push(format!("Cost: {}", self.cost.summary()));
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

        assert_eq!(
            shown(&row(CapabilityStatus::Supported, None, true, true)),
            (FeatureIndicator::Running, "Running".to_owned())
        );
        assert_eq!(
            shown(&row(CapabilityStatus::Supported, None, false, false)),
            (FeatureIndicator::Off, "Off".to_owned())
        );
        assert_eq!(
            shown(&row(CapabilityStatus::Supported, None, true, false)),
            (
                FeatureIndicator::NeedsAttention,
                "On, but not running".to_owned()
            )
        );
        assert_eq!(
            shown(&row(
                CapabilityStatus::MissingDependency {
                    name: "Tesseract".to_owned()
                },
                Some("Install it with sudo apt install tesseract-ocr."),
                false,
                false
            )),
            (
                FeatureIndicator::NeedsAttention,
                "Needs Tesseract. Install it with sudo apt install tesseract-ocr.".to_owned()
            ),
            "a missing piece warns even while the feature is off"
        );
        assert_eq!(
            shown(&row(
                CapabilityStatus::NeedsPermission {
                    permission: Permission::HardwareControl
                },
                None,
                true,
                false
            )),
            (
                FeatureIndicator::NeedsAttention,
                "Needs hardware control permission".to_owned()
            )
        );
        assert_eq!(
            shown(&row(
                CapabilityStatus::Unsupported {
                    reason: "this session exports no portal.".to_owned()
                },
                Some("Bind kestrel --command instead."),
                false,
                false
            )),
            (
                FeatureIndicator::Unavailable,
                "Not available here. This session exports no portal.".to_owned()
            )
        );
        assert_eq!(
            shown(&row(
                CapabilityStatus::Limited {
                    reason: "There are no active playback streams.".to_owned()
                },
                None,
                true,
                true
            )),
            (FeatureIndicator::Running, "Running".to_owned()),
            "a partial capability without a fix is a plain fact"
        );
        assert_eq!(
            shown(&row(
                CapabilityStatus::Limited {
                    reason: "no insertion provider is installed".to_owned()
                },
                Some("Install wtype or xdotool."),
                true,
                true
            )),
            (
                FeatureIndicator::NeedsAttention,
                "Works partly: no insertion provider is installed. Install wtype or xdotool."
                    .to_owned()
            )
        );

        let details = row(CapabilityStatus::Supported, None, true, true).details();
        for part in [
            "Reads the screen.",
            "Backend: Portal",
            "Cost: Idle:",
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
}
