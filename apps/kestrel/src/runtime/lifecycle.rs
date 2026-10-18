//! Runtime lifecycle state and event logging.

use super::*;

/// A feature's state as the log compares it across changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FeatureState {
    id: &'static str,
    running: bool,
    capability: String,
}

/// The capability status with its sanitized reason, as logged.
pub(super) fn capability_label(status: &CapabilityStatus) -> String {
    match status {
        CapabilityStatus::Supported => "supported".to_owned(),
        CapabilityStatus::Limited { reason } => format!("limited: {reason}"),
        CapabilityStatus::NeedsPermission { permission } => {
            format!("needs permission: {permission:?}")
        }
        CapabilityStatus::MissingDependency { name } => format!("missing {name}"),
        CapabilityStatus::Unsupported { reason } => format!("unsupported: {reason}"),
    }
}

/// Started, stopped, and capability-change events between two states. A
/// feature seen for the first time reports only that it runs, or why it is
/// unavailable.
pub(super) fn lifecycle_events(
    before: &[FeatureState],
    after: &[FeatureState],
) -> Vec<(log::Level, &'static str, String)> {
    let mut events = Vec::new();
    for state in after {
        let previous = before.iter().find(|previous| previous.id == state.id);
        let capability_changed = match previous {
            Some(previous) => previous.capability != state.capability,
            None => state.capability != "supported",
        };
        if capability_changed {
            events.push((
                log::Level::Info,
                state.id,
                format!("capability: {}", state.capability),
            ));
        }
        if previous.map_or(state.running, |previous| previous.running != state.running) {
            let change = if state.running { "started" } else { "stopped" };
            events.push((log::Level::Info, state.id, change.to_owned()));
        }
    }
    events
}

impl ApplicationRuntime {
    pub(super) fn feature_states(&self) -> Vec<FeatureState> {
        self.registry
            .registrations()
            .map(|registration| FeatureState {
                id: registration.feature.id,
                running: registration.running,
                capability: capability_label(&registration.capability.status),
            })
            .collect()
    }

    /// Logs features that started, stopped, or changed capability since `before`.
    pub(super) fn log_lifecycle(&self, before: &[FeatureState]) {
        for (level, feature_id, message) in lifecycle_events(before, &self.feature_states()) {
            log::log!(target: feature_id, level, "{message}");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lifecycle_events_report_starts_stops_and_capability_changes() {
        use super::{FeatureState, lifecycle_events};
        let state = |id: &'static str, running: bool, capability: &str| FeatureState {
            id,
            running,
            capability: capability.to_owned(),
        };
        let messages = |events: Vec<(log::Level, &'static str, String)>| {
            events
                .into_iter()
                .map(|(_, id, message)| format!("{id} {message}"))
                .collect::<Vec<_>>()
        };

        let first = [
            state("audio.mixer", true, "supported"),
            state("camera.preview", false, "unsupported: no camera"),
            state("network.listeners", false, "supported"),
        ];
        assert_eq!(
            messages(lifecycle_events(&[], &first)),
            vec![
                "audio.mixer started",
                "camera.preview capability: unsupported: no camera",
            ],
            "startup names running features and the reason others are unavailable"
        );

        let next = [
            state("audio.mixer", false, "supported"),
            state("camera.preview", true, "supported"),
            state("network.listeners", false, "supported"),
        ];
        assert_eq!(
            messages(lifecycle_events(&first, &next)),
            vec![
                "audio.mixer stopped",
                "camera.preview capability: supported",
                "camera.preview started",
            ],
        );
        assert!(
            lifecycle_events(&next, &next).is_empty(),
            "no change, no lines"
        );
    }
}
