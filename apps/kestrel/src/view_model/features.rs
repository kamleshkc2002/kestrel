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

/// Capability status and optional detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityStatusViewModel {
    pub label: &'static str,
    pub detail: Option<String>,
}

impl From<&CapabilityStatus> for CapabilityStatusViewModel {
    fn from(status: &CapabilityStatus) -> Self {
        match status {
            CapabilityStatus::Supported => Self {
                label: "Supported",
                detail: None,
            },
            CapabilityStatus::Limited { reason } => Self {
                label: "Limited",
                detail: Some(reason.clone()),
            },
            CapabilityStatus::NeedsPermission { permission } => Self {
                label: "Needs permission",
                detail: Some(permission_label(*permission).to_string()),
            },
            CapabilityStatus::MissingDependency { name } => Self {
                label: "Missing dependency",
                detail: Some(name.clone()),
            },
            CapabilityStatus::Unsupported { reason } => Self {
                label: "Unsupported",
                detail: Some(reason.clone()),
            },
        }
    }
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
    fn maps_limited_and_missing_dependency_details() {
        assert_eq!(
            CapabilityStatusViewModel::from(&CapabilityStatus::Limited {
                reason: "Only one provider is available.".to_string(),
            }),
            CapabilityStatusViewModel {
                label: "Limited",
                detail: Some("Only one provider is available.".to_string()),
            }
        );
        assert_eq!(
            CapabilityStatusViewModel::from(&CapabilityStatus::MissingDependency {
                name: "example-service".to_string(),
            }),
            CapabilityStatusViewModel {
                label: "Missing dependency",
                detail: Some("example-service".to_string()),
            }
        );
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
