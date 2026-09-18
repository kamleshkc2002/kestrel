use kestrel_core::{
    AlertKind, AppearancePreference, ApplicationConfiguration, CapabilityStatus, CostLevel,
    MonitorConfiguration, MonitorReadout, PanelSection, Permission, ResourceCost,
};
use kestrel_platform::quick_toggles::{
    MutationConfirmation, QuickToggleControl, QuickToggleId, ToggleAction,
};
use kestrel_services::{
    ServiceLifecycle, ServiceRegistration,
    alerts::{ActiveAlert, AlertPolicy, AlertSnapshot},
    quick_toggles::{QuickToggleSnapshot, ToggleStateSource},
    system_monitor::{HistorySummary, SystemSnapshot},
};

use crate::ConfigurationWarning;

/// Owned inputs used to construct monitoring presentation state.
pub(crate) struct MonitorPresentation<'a> {
    pub snapshot: Option<&'a SystemSnapshot>,
    pub history: Option<HistorySummary>,
    pub alerts: AlertSnapshot,
    pub running: bool,
    /// Effective alert rules, including gates applied outside the configuration
    /// (such as the `power.battery-alerts` quick toggle). The configuration stays
    /// the user's intent; the policy is what the engine actually enforces.
    pub policy: Option<&'a AlertPolicy>,
}

/// Immutable presentation state for one system-monitor panel.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorViewModel {
    pub running: bool,
    pub status: String,
    pub readouts: Vec<MonitorReadoutViewModel>,
    pub readout_settings: Vec<MonitorReadoutSettingViewModel>,
    pub alerts: Vec<ActiveAlertViewModel>,
    pub delivery_failures: Vec<(AlertKind, String)>,
    pub alert_rules: Vec<AlertRuleViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorReadoutViewModel {
    pub readout: MonitorReadout,
    pub label: &'static str,
    pub value: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorReadoutSettingViewModel {
    pub readout: MonitorReadout,
    pub label: &'static str,
    pub visible: bool,
    pub can_move_up: bool,
    pub can_move_down: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveAlertViewModel {
    pub kind: AlertKind,
    pub label: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertRuleViewModel {
    pub kind: AlertKind,
    pub label: &'static str,
    pub enabled: bool,
    pub threshold: f64,
    pub unit: &'static str,
    pub summary: String,
}

impl MonitorViewModel {
    /// Single source of truth for monitoring presentation.
    pub(crate) fn from_configuration(
        configuration: &MonitorConfiguration,
        presentation: MonitorPresentation<'_>,
    ) -> Self {
        let snapshot = presentation.snapshot;
        let readouts = configuration
            .readouts
            .iter()
            .copied()
            .map(|readout| monitor_readout(snapshot, readout))
            .collect();
        // Settings rows follow the configured order first, then the readouts that are
        // currently hidden, so the move arrows describe the list they actually reorder.
        let visible_count = configuration.readouts.len();
        let mut ordered = configuration.readouts.clone();
        ordered.extend(
            MonitorReadout::ALL
                .into_iter()
                .filter(|readout| !configuration.readouts.contains(readout)),
        );
        let readout_settings = ordered
            .into_iter()
            .map(|readout| {
                let position = configuration
                    .readouts
                    .iter()
                    .position(|entry| *entry == readout);
                MonitorReadoutSettingViewModel {
                    readout,
                    label: readout.label(),
                    visible: position.is_some(),
                    can_move_up: position.is_some_and(|position| position > 0),
                    can_move_down: position.is_some_and(|position| position + 1 < visible_count),
                }
            })
            .collect();
        let alerts = presentation
            .alerts
            .active
            .iter()
            .map(active_alert)
            .collect();
        let delivery_failures = presentation
            .alerts
            .delivery_failures
            .iter()
            .map(|failure| (failure.kind, failure.message.clone()))
            .collect();
        let alert_rules = AlertKind::ALL
            .into_iter()
            .map(|kind| {
                let configured = configuration.alerts.rule(kind);
                let effective = presentation.policy.and_then(|policy| policy.rule(kind));
                let enabled = effective.map_or(configured.enabled, |rule| rule.enabled);
                let threshold = effective.map_or(configured.threshold, |rule| rule.threshold);
                let sustain_samples =
                    effective.map_or(configured.sustain_samples, |rule| rule.sustain_samples);
                let cooldown_seconds =
                    effective.map_or(configured.cooldown_seconds, |rule| rule.cooldown.as_secs());
                let mut summary = format!(
                    "Alert {} {}{} for {} samples; repeat at most every {} s",
                    if kind.alerts_above() {
                        "at or above"
                    } else {
                        "at or below"
                    },
                    format_threshold(threshold),
                    kind.unit(),
                    sustain_samples,
                    cooldown_seconds
                );
                if configured.enabled && !enabled {
                    summary.push_str("; currently paused by its quick toggle");
                }
                AlertRuleViewModel {
                    kind,
                    label: kind.label(),
                    enabled,
                    threshold,
                    unit: kind.unit(),
                    summary,
                }
            })
            .collect();
        let status = monitor_status(
            presentation.running,
            snapshot,
            presentation.history.as_ref(),
        );
        Self {
            running: presentation.running,
            status,
            readouts,
            readout_settings,
            alerts,
            delivery_failures,
            alert_rules,
        }
    }
}

fn format_threshold(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn monitor_status(
    running: bool,
    snapshot: Option<&SystemSnapshot>,
    history: Option<&HistorySummary>,
) -> String {
    if snapshot.is_none() {
        return if running {
            "Monitoring is unavailable: no system snapshot has been sampled.".to_owned()
        } else {
            "Monitoring is unavailable: the monitor service is not running.".to_owned()
        };
    }
    if !running {
        return "Monitoring is unavailable: the monitor service is not running.".to_owned();
    }
    history
        .map(|summary| {
            format!(
                "{} samples retained over {} s.",
                summary.samples,
                summary.span.as_secs()
            )
        })
        .unwrap_or_else(|| "Monitoring is running; history is not available yet.".to_owned())
}

fn active_alert(alert: &ActiveAlert) -> ActiveAlertViewModel {
    let value = format!("{:.0}", alert.value);
    let subject = alert
        .subject
        .as_deref()
        .map(|subject| format!(" ({subject})"))
        .unwrap_or_default();
    ActiveAlertViewModel {
        kind: alert.kind,
        label: alert.kind.label(),
        message: format!(
            "{}{} is {}{}",
            alert.kind.label(),
            subject,
            value,
            alert.kind.unit()
        ),
    }
}
fn format_bytes(bytes: f64) -> String {
    if !bytes.is_finite() || bytes < 0.0 {
        return "Unavailable".to_owned();
    }
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn percentage(value: f64) -> String {
    format!("{:.0}%", value.clamp(0.0, 100.0))
}

fn temperature(value: f64) -> String {
    format!("{:.0}°C", value)
}

fn issue_reason(snapshot: &SystemSnapshot, readout: MonitorReadout) -> Option<String> {
    // Exact platform source strings: a substring match would attribute another
    // family's failure (for example `/proc/diskstats`) to this readout.
    let matches = |source: &str| match readout {
        MonitorReadout::Cpu => source == "/proc/stat",
        MonitorReadout::Memory | MonitorReadout::Swap => source == "/proc/meminfo",
        MonitorReadout::Network => source == "/proc/net/dev",
        MonitorReadout::Temperature => source == "/sys/class/hwmon + /sys/class/thermal",
        MonitorReadout::Battery => source == "/sys/class/power_supply",
        MonitorReadout::Disk => {
            source == "/proc/diskstats" || source.starts_with("/proc/self/mounts")
        }
        MonitorReadout::Gpu => source == "/sys/class/drm",
    };
    snapshot
        .issues
        .iter()
        .find(|issue| matches(&issue.source))
        .map(|issue| issue.reason.clone())
}

fn unavailable(
    snapshot: Option<&SystemSnapshot>,
    readout: MonitorReadout,
    fallback: &str,
) -> MonitorReadoutViewModel {
    let detail = snapshot
        .and_then(|snapshot| issue_reason(snapshot, readout))
        .unwrap_or_else(|| fallback.to_owned());
    MonitorReadoutViewModel {
        readout,
        label: readout.label(),
        value: None,
        detail,
    }
}

fn monitor_readout(
    snapshot: Option<&SystemSnapshot>,
    readout: MonitorReadout,
) -> MonitorReadoutViewModel {
    let Some(snapshot) = snapshot else {
        return unavailable(None, readout, "No system snapshot is available yet.");
    };
    match readout {
        MonitorReadout::Cpu => snapshot
            .cpu_usage_percent
            .map(|value| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value)),
                detail: snapshot
                    .logical_cpus
                    .map(|cpus| format!("{cpus} logical CPUs"))
                    .unwrap_or_else(|| "Current CPU utilization".to_owned()),
            })
            .unwrap_or_else(|| unavailable(Some(snapshot), readout, "CPU usage is unavailable.")),
        MonitorReadout::Memory => snapshot
            .memory
            .as_ref()
            .and_then(|memory| memory.used_percent)
            .map(|value| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value)),
                detail: snapshot
                    .memory
                    .as_ref()
                    .map(|memory| {
                        format!(
                            "{} used of {}",
                            format_bytes(memory.used_bytes as f64),
                            format_bytes(memory.total_bytes as f64)
                        )
                    })
                    .unwrap_or_default(),
            })
            .unwrap_or_else(|| {
                unavailable(Some(snapshot), readout, "Memory usage is unavailable.")
            }),
        MonitorReadout::Swap => snapshot
            .memory
            .as_ref()
            .and_then(|memory| memory.swap_used_percent)
            .map(|value| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value)),
                detail: snapshot
                    .memory
                    .as_ref()
                    .map(|memory| {
                        format!(
                            "{} used of {}",
                            format_bytes(memory.swap_used_bytes as f64),
                            format_bytes(memory.swap_total_bytes as f64)
                        )
                    })
                    .unwrap_or_default(),
            })
            .unwrap_or_else(|| unavailable(Some(snapshot), readout, "Swap usage is unavailable.")),
        MonitorReadout::Disk => snapshot
            .disk_usage
            .iter()
            .filter_map(|disk| disk.used_percent.map(|value| (disk, value)))
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(disk, value)| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value)),
                detail: format!("{} mounted at {}", disk.filesystem, disk.mount_point),
            })
            .unwrap_or_else(|| unavailable(Some(snapshot), readout, "Disk usage is unavailable.")),
        MonitorReadout::Network => {
            let rates = snapshot
                .network
                .iter()
                .flat_map(|network| {
                    [
                        network.receive_bytes_per_second,
                        network.transmit_bytes_per_second,
                    ]
                })
                .flatten()
                .collect::<Vec<_>>();
            let rate = rates.iter().copied().fold(0.0, f64::max);
            if !rates.is_empty() {
                MonitorReadoutViewModel {
                    readout,
                    label: readout.label(),
                    value: Some(format!("{}/s", format_bytes(rate))),
                    detail: format!("{} interfaces", snapshot.network.len()),
                }
            } else {
                unavailable(Some(snapshot), readout, "Network rate is unavailable.")
            }
        }
        MonitorReadout::Temperature => snapshot
            .temperatures
            .iter()
            .max_by_key(|reading| reading.millidegrees_celsius)
            .map(|reading| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(temperature(reading.millidegrees_celsius as f64 / 1000.0)),
                detail: reading.label.clone(),
            })
            .unwrap_or_else(|| {
                unavailable(
                    Some(snapshot),
                    readout,
                    "Temperature readings are unavailable.",
                )
            }),
        MonitorReadout::Battery => snapshot
            .power_supplies
            .iter()
            .filter(|supply| {
                supply
                    .kind
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("battery"))
            })
            .filter_map(|supply| supply.capacity_percent.map(|capacity| (supply, capacity)))
            .min_by_key(|(_, capacity)| *capacity)
            .map(|(supply, capacity)| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(capacity as f64)),
                detail: supply.name.clone(),
            })
            .unwrap_or_else(|| {
                unavailable(Some(snapshot), readout, "Battery capacity is unavailable.")
            }),
        MonitorReadout::Gpu => snapshot
            .gpu
            .iter()
            .filter_map(|gpu| gpu.busy_percent)
            .max()
            .map(|value| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value as f64)),
                detail: "GPU activity".to_owned(),
            })
            .unwrap_or_else(|| {
                unavailable(Some(snapshot), readout, "GPU activity is unavailable.")
            }),
    }
}

/// Immutable presentation state for the normal Kestrel window.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplicationViewModel {
    pub features: Vec<FeatureViewModel>,
    pub quick_toggles: Vec<QuickToggleViewModel>,
    pub monitor: MonitorViewModel,
    pub warnings: Vec<ConfigurationWarningViewModel>,
    pub appearance: AppearancePreference,
    pub autostart: bool,
    pub panel_sections: Vec<PanelSectionViewModel>,
    pub can_undo: bool,
}

impl ApplicationViewModel {
    pub(crate) fn new<'a>(
        registrations: impl Iterator<Item = &'a ServiceRegistration>,
        quick_toggles: impl Iterator<Item = &'a QuickToggleSnapshot>,
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
            monitor: MonitorViewModel::from_configuration(&configuration.monitoring, monitor),
            warnings: warnings
                .iter()
                .map(ConfigurationWarningViewModel::from)
                .collect(),
            appearance: configuration.ui.appearance,
            autostart: configuration.startup.autostart,
            panel_sections: configuration
                .ui
                .panel_sections
                .iter()
                .map(PanelSectionViewModel::from)
                .collect(),
            can_undo,
        }
    }
}

/// Presentation state for configured panel placement and visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelSectionViewModel {
    pub section: PanelSection,
    pub visible: bool,
}

impl From<&kestrel_core::PanelSectionConfiguration> for PanelSectionViewModel {
    fn from(configuration: &kestrel_core::PanelSectionConfiguration) -> Self {
        Self {
            section: configuration.section,
            visible: configuration.visible,
        }
    }
}

/// Conservative resource-use labels disclosed by the feature hub.
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
    fn from_snapshot(
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
/// A distinct action users can take to improve a capability state.
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

/// Presentation state for one registered feature.
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
    fn from_registration(
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

/// User-facing lifecycle state, independent of service enum formatting.
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

/// Presentation state for one capability report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityViewModel {
    pub status: CapabilityStatusViewModel,
    pub summary: String,
    pub selected_backend: Option<String>,
    pub remediation: Option<RemediationViewModel>,
}

/// Stable, user-facing capability status and its optional detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityStatusViewModel {
    pub kind: CapabilityKindViewModel,
    pub label: &'static str,
    pub detail: Option<String>,
}
/// Semantic capability state used by presentation layers without string matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityKindViewModel {
    Supported,
    Limited,
    NeedsPermission,
    MissingDependency,
    Unsupported,
}

impl From<&CapabilityStatus> for CapabilityStatusViewModel {
    fn from(status: &CapabilityStatus) -> Self {
        match status {
            CapabilityStatus::Supported => Self {
                kind: CapabilityKindViewModel::Supported,
                label: "Supported",
                detail: None,
            },
            CapabilityStatus::Limited { reason } => Self {
                kind: CapabilityKindViewModel::Limited,
                label: "Limited",
                detail: Some(reason.clone()),
            },
            CapabilityStatus::NeedsPermission { permission } => Self {
                kind: CapabilityKindViewModel::NeedsPermission,
                label: "Needs permission",
                detail: Some(permission_label(*permission).to_string()),
            },
            CapabilityStatus::MissingDependency { name } => Self {
                kind: CapabilityKindViewModel::MissingDependency,
                label: "Missing dependency",
                detail: Some(name.clone()),
            },
            CapabilityStatus::Unsupported { reason } => Self {
                kind: CapabilityKindViewModel::Unsupported,
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

/// Presentation state for one isolated configuration warning.
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
    use super::{
        ApplicationViewModel, CapabilityKindViewModel, CapabilityStatusViewModel,
        FeatureLifecycleViewModel, FeatureViewModel, MonitorPresentation, MonitorViewModel,
    };
    use kestrel_core::{
        AlertKind, ApplicationConfiguration, CapabilityReport, CapabilityStatus, FeatureSpec,
        MonitorConfiguration, MonitorReadout, Permission,
    };
    use kestrel_platform::quick_toggles::QuickToggleId;
    use kestrel_services::{
        ServiceRegistration,
        quick_toggles::{QuickToggleSnapshot, ToggleStateSource},
    };

    use crate::ConfigurationWarning;

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
        assert_eq!(
            view_model.capability.status.kind,
            CapabilityKindViewModel::Supported
        );
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
        assert_eq!(
            view_model.capability.status.kind,
            CapabilityKindViewModel::NeedsPermission
        );
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
                kind: CapabilityKindViewModel::Limited,
                label: "Limited",
                detail: Some("Only one provider is available.".to_string()),
            }
        );
        assert_eq!(
            CapabilityStatusViewModel::from(&CapabilityStatus::MissingDependency {
                name: "example-service".to_string(),
            }),
            CapabilityStatusViewModel {
                kind: CapabilityKindViewModel::MissingDependency,
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
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
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
    fn copies_configuration_warnings_into_owned_presentation_state() {
        let warnings = vec![ConfigurationWarning {
            feature_id: "audio.mixer".to_string(),
            reason: "Invalid volume preference.".to_string(),
        }];
        let configuration = ApplicationConfiguration::default();
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &warnings,
            &configuration,
            false,
        );

        assert!(view_model.features.is_empty());
        assert_eq!(view_model.warnings[0].feature_id, "audio.mixer");
        assert_eq!(view_model.warnings[0].message, "Invalid volume preference.");
    }

    #[test]
    fn exposes_presentation_preferences_and_reversible_state() {
        use kestrel_core::{AppearancePreference, PanelSection, PanelSectionConfiguration};

        let mut configuration = ApplicationConfiguration::default();
        configuration.ui.appearance = AppearancePreference::Dark;
        configuration.startup.autostart = true;
        configuration.ui.panel_sections = vec![
            PanelSectionConfiguration {
                section: PanelSection::FeatureHub,
                visible: false,
            },
            PanelSectionConfiguration {
                section: PanelSection::QuickControls,
                visible: true,
            },
        ];
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &[],
            &configuration,
            true,
        );

        assert_eq!(view_model.appearance, AppearancePreference::Dark);
        assert!(view_model.autostart);
        assert!(view_model.can_undo);
        assert_eq!(
            view_model.panel_sections[0].section,
            PanelSection::FeatureHub
        );
        assert!(!view_model.panel_sections[0].visible);
    }
    #[test]
    fn monitoring_configured_order_and_hidden_readouts_are_honoured() {
        let configuration = MonitorConfiguration {
            readouts: vec![MonitorReadout::Gpu, MonitorReadout::Cpu],
            ..MonitorConfiguration::default()
        };
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
        );
        assert_eq!(
            monitor
                .readouts
                .iter()
                .map(|readout| readout.readout)
                .collect::<Vec<_>>(),
            vec![MonitorReadout::Gpu, MonitorReadout::Cpu]
        );
        assert!(
            !monitor
                .readout_settings
                .iter()
                .find(|setting| setting.readout == MonitorReadout::Memory)
                .expect("all settings are represented")
                .visible
        );
        // Settings rows follow the configured order first, then the hidden readouts, and
        // their arrows describe the list the move commands actually reorder.
        let settings = &monitor.readout_settings;
        assert_eq!(settings[0].readout, MonitorReadout::Gpu);
        assert_eq!(settings[1].readout, MonitorReadout::Cpu);
        assert!(!settings[0].can_move_up && settings[0].can_move_down);
        assert!(settings[1].can_move_up && !settings[1].can_move_down);
        assert_eq!(settings.len(), MonitorReadout::ALL.len());
        for hidden in settings.iter().skip(2) {
            assert!(!hidden.visible && !hidden.can_move_up && !hidden.can_move_down);
        }
    }

    #[test]
    fn alert_rules_expose_the_gated_effective_state() {
        use kestrel_services::alerts::AlertPolicy;

        // The configuration keeps the user's intent; the policy carries the gate that
        // the engine enforces (for example the battery-alerts quick toggle).
        let configuration = MonitorConfiguration::default();
        assert!(configuration.alerts.battery.enabled);
        let mut policy = AlertPolicy::from_configuration(&configuration.alerts);
        policy.set_enabled(AlertKind::Battery, false);
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: Some(&policy),
            },
        );
        let battery = monitor
            .alert_rules
            .iter()
            .find(|rule| rule.kind == AlertKind::Battery)
            .expect("battery rule is always reported");
        assert!(!battery.enabled);
        assert!(battery.summary.contains("paused by its quick toggle"));
        let cpu = monitor
            .alert_rules
            .iter()
            .find(|rule| rule.kind == AlertKind::Cpu)
            .expect("cpu rule is always reported");
        assert!(cpu.enabled);
        assert!(!cpu.summary.contains("quick toggle"));
    }

    #[test]
    fn unavailable_readout_never_borrows_another_family_reason() {
        use kestrel_platform::system_monitor::SourceIssue;
        use kestrel_services::system_monitor::SystemSnapshot;
        use std::time::Duration;

        // `/proc/diskstats` contains "stat"; a substring match would show the disk
        // failure as the CPU explanation.
        let snapshot = SystemSnapshot {
            observed_at: Duration::from_secs(1),
            cpu_usage_percent: None,
            logical_cpus: None,
            memory: None,
            disk_usage: Vec::new(),
            disk_activity: Vec::new(),
            network: Vec::new(),
            temperatures: Vec::new(),
            power_supplies: Vec::new(),
            gpu: Vec::new(),
            issues: vec![SourceIssue {
                source: "/proc/diskstats".to_owned(),
                reason: "no whole-disk statistics are readable".to_owned(),
            }],
        };
        let configuration = MonitorConfiguration {
            readouts: vec![MonitorReadout::Cpu, MonitorReadout::Disk],
            ..MonitorConfiguration::default()
        };
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: Some(&snapshot),
                history: None,
                alerts: Default::default(),
                running: true,
                policy: None,
            },
        );

        assert_eq!(monitor.readouts[0].value, None);
        assert!(!monitor.readouts[0].detail.contains("disk"));
        assert_eq!(
            monitor.readouts[1].detail,
            "no whole-disk statistics are readable"
        );
    }

    #[test]
    fn monitoring_unavailable_readout_keeps_source_reason() {
        use kestrel_platform::system_monitor::SourceIssue;
        use kestrel_services::system_monitor::SystemSnapshot;
        use std::time::Duration;

        let snapshot = SystemSnapshot {
            observed_at: Duration::from_secs(1),
            cpu_usage_percent: None,
            logical_cpus: None,
            memory: None,
            disk_usage: Vec::new(),
            disk_activity: Vec::new(),
            network: Vec::new(),
            temperatures: Vec::new(),
            power_supplies: Vec::new(),
            gpu: Vec::new(),
            issues: vec![SourceIssue {
                source: "/proc/meminfo".to_owned(),
                reason: "MemAvailable is missing".to_owned(),
            }],
        };
        let configuration = MonitorConfiguration {
            readouts: vec![MonitorReadout::Memory],
            ..MonitorConfiguration::default()
        };
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: Some(&snapshot),
                history: None,
                alerts: Default::default(),
                running: true,
                policy: None,
            },
        );
        assert_eq!(monitor.readouts[0].value, None);
        assert_eq!(monitor.readouts[0].detail, "MemAvailable is missing");
    }

    #[test]
    fn active_alerts_and_rules_map_labels_units_and_threshold_summary() {
        use kestrel_services::alerts::{ActiveAlert, AlertSnapshot};
        use std::time::Duration;

        let mut configuration = MonitorConfiguration::default();
        configuration.alerts.temperature.threshold = 80.0;
        configuration.alerts.temperature.sustain_samples = 3;
        configuration.alerts.temperature.cooldown_seconds = 120;
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: AlertSnapshot {
                    active: vec![ActiveAlert {
                        kind: AlertKind::Temperature,
                        value: 91.0,
                        threshold: 80.0,
                        subject: Some("CPU package".to_owned()),
                        raised_at: Duration::from_secs(2),
                    }],
                    delivery_failures: Vec::new(),
                },
                running: true,
                policy: None,
            },
        );
        assert_eq!(monitor.alerts[0].label, "Temperature");
        assert_eq!(monitor.alert_rules[1].unit, "°C");
        assert!(monitor.alert_rules[1].summary.contains("80°C"));
        assert!(monitor.alert_rules[1].summary.contains("3 samples"));
        assert!(monitor.alert_rules[1].summary.contains("120 s"));
    }

    #[test]
    fn empty_snapshot_yields_explicit_unavailable_state() {
        let monitor = MonitorViewModel::from_configuration(
            &MonitorConfiguration::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
        );
        assert!(!monitor.running);
        assert!(monitor.status.contains("unavailable"));
        assert!(
            monitor
                .readouts
                .iter()
                .all(|readout| readout.value.is_none())
        );
    }
}
