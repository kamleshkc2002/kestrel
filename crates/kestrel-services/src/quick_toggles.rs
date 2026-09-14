//! Independent quick-toggle state, confirmation, and ownership policy.

use std::collections::BTreeMap;

use kestrel_platform::quick_toggles::{
    ALL_QUICK_TOGGLES, LinuxQuickToggleBackend, QuickToggleBackend, QuickToggleError,
    QuickToggleId, QuickToggleMutation, QuickToggleObservation,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleStateSource {
    KestrelOwned,
    ProviderObserved,
    ChangedElsewhere,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickToggleSnapshot {
    pub id: QuickToggleId,
    pub label: &'static str,
    pub requirement: &'static str,
    pub observation: Option<QuickToggleObservation>,
    pub source: ToggleStateSource,
    pub error: Option<QuickToggleError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickToggleCommand {
    pub id: QuickToggleId,
    pub mutation: QuickToggleMutation,
    pub confirmation_token: Option<String>,
}

pub struct QuickToggleService<B: QuickToggleBackend = LinuxQuickToggleBackend> {
    backend: B,
    snapshots: BTreeMap<QuickToggleId, QuickToggleSnapshot>,
}

impl<B: QuickToggleBackend> QuickToggleService<B> {
    pub fn new(backend: B) -> Self {
        let snapshots = ALL_QUICK_TOGGLES
            .into_iter()
            .map(|id| {
                (
                    id,
                    QuickToggleSnapshot {
                        id,
                        label: id.label(),
                        requirement: id.requirement(),
                        observation: None,
                        source: local_source(id),
                        error: None,
                    },
                )
            })
            .collect();
        Self { backend, snapshots }
    }

    pub fn refresh_all(&mut self) {
        for id in ALL_QUICK_TOGGLES {
            self.refresh(id);
        }
    }

    pub fn refresh(&mut self, id: QuickToggleId) -> &QuickToggleSnapshot {
        match self.backend.observe(id) {
            Ok(observation) => {
                let snapshot = self
                    .snapshots
                    .get(&id)
                    .expect("every built-in quick toggle has a snapshot");
                let source =
                    if matches!(id, QuickToggleId::KeepAwake | QuickToggleId::BatteryAlerts) {
                        ToggleStateSource::KestrelOwned
                    } else {
                        match snapshot.observation.as_ref() {
                            None => ToggleStateSource::ProviderObserved,
                            Some(previous) if previous.control == observation.control => {
                                snapshot.source
                            }
                            Some(_) => ToggleStateSource::ChangedElsewhere,
                        }
                    };
                let snapshot = self
                    .snapshots
                    .get_mut(&id)
                    .expect("every built-in quick toggle has a snapshot");
                snapshot.observation = Some(observation);
                snapshot.source = source;
                snapshot.error = None;
            }
            Err(error) => {
                self.snapshots
                    .get_mut(&id)
                    .expect("every built-in quick toggle has a snapshot")
                    .error = Some(error);
            }
        }
        &self.snapshots[&id]
    }

    pub fn execute(
        &mut self,
        command: QuickToggleCommand,
    ) -> Result<&QuickToggleSnapshot, QuickToggleError> {
        let result = self.backend.apply(
            command.id,
            command.mutation,
            command.confirmation_token.as_deref(),
        );
        match result {
            Ok(observation) => {
                let snapshot = self
                    .snapshots
                    .get_mut(&command.id)
                    .expect("every built-in quick toggle has a snapshot");
                snapshot.observation = Some(observation);
                snapshot.source = ToggleStateSource::KestrelOwned;
                snapshot.error = None;
                Ok(snapshot)
            }
            Err(error) => {
                let snapshot = self
                    .snapshots
                    .get_mut(&command.id)
                    .expect("every built-in quick toggle has a snapshot");
                snapshot.error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub fn snapshots(&self) -> impl Iterator<Item = &QuickToggleSnapshot> {
        ALL_QUICK_TOGGLES.iter().map(|id| &self.snapshots[id])
    }
    pub fn set_runtime_error(&mut self, id: QuickToggleId, error: Option<QuickToggleError>) {
        self.snapshots
            .get_mut(&id)
            .expect("every built-in quick toggle has a snapshot")
            .error = error;
    }

    pub fn stop(&mut self) {
        let _ = self.backend.apply(
            QuickToggleId::KeepAwake,
            QuickToggleMutation::SetEnabled(false),
            None,
        );
        let _ = self.backend.apply(
            QuickToggleId::BatteryAlerts,
            QuickToggleMutation::SetEnabled(false),
            None,
        );
        self.refresh(QuickToggleId::KeepAwake);
        self.refresh(QuickToggleId::BatteryAlerts);
    }
}

impl<B: QuickToggleBackend> Drop for QuickToggleService<B> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn local_source(id: QuickToggleId) -> ToggleStateSource {
    if matches!(id, QuickToggleId::KeepAwake | QuickToggleId::BatteryAlerts) {
        ToggleStateSource::KestrelOwned
    } else {
        ToggleStateSource::ProviderObserved
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kestrel_core::{CapabilityReport, CapabilityStatus};
    use kestrel_platform::quick_toggles::{
        MutationConfirmation, QuickToggleBackend, QuickToggleControl, QuickToggleError,
        QuickToggleErrorKind, QuickToggleId, QuickToggleMutation, QuickToggleObservation,
    };

    use super::{QuickToggleCommand, QuickToggleService, ToggleStateSource};

    struct FakeBackend {
        controls: BTreeMap<QuickToggleId, QuickToggleControl>,
    }

    impl FakeBackend {
        fn new() -> Self {
            let controls = [
                (
                    QuickToggleId::Wifi,
                    QuickToggleControl::Switch {
                        enabled: true,
                        confirmation: Some(MutationConfirmation {
                            scope: "Turn off Wi-Fi".to_owned(),
                            token: "wifi-off".to_owned(),
                        }),
                    },
                ),
                (
                    QuickToggleId::KeepAwake,
                    QuickToggleControl::Switch {
                        enabled: false,
                        confirmation: None,
                    },
                ),
                (
                    QuickToggleId::BatteryAlerts,
                    QuickToggleControl::Switch {
                        enabled: false,
                        confirmation: None,
                    },
                ),
            ]
            .into_iter()
            .collect();
            Self { controls }
        }
    }

    impl QuickToggleBackend for FakeBackend {
        fn capability(&self, id: QuickToggleId) -> CapabilityReport {
            CapabilityReport::new(id.feature_id(), CapabilityStatus::Supported, "available")
        }

        fn observe(&self, id: QuickToggleId) -> Result<QuickToggleObservation, QuickToggleError> {
            self.controls
                .get(&id)
                .cloned()
                .map(|control| QuickToggleObservation {
                    control,
                    detail: "observed".to_owned(),
                })
                .ok_or_else(|| QuickToggleError {
                    kind: QuickToggleErrorKind::Unavailable,
                    message: "not available".to_owned(),
                })
        }

        fn confirmation(
            &self,
            id: QuickToggleId,
            mutation: &QuickToggleMutation,
        ) -> Result<Option<MutationConfirmation>, QuickToggleError> {
            if id == QuickToggleId::Wifi
                && matches!(mutation, QuickToggleMutation::SetEnabled(false))
            {
                Ok(Some(MutationConfirmation {
                    scope: "Turn off Wi-Fi".to_owned(),
                    token: "wifi-off".to_owned(),
                }))
            } else {
                Ok(None)
            }
        }

        fn apply(
            &mut self,
            id: QuickToggleId,
            mutation: QuickToggleMutation,
            confirmation_token: Option<&str>,
        ) -> Result<QuickToggleObservation, QuickToggleError> {
            if let Some(expected) = self.confirmation(id, &mutation)? {
                if confirmation_token != Some(expected.token.as_str()) {
                    return Err(QuickToggleError {
                        kind: QuickToggleErrorKind::ConfirmationRequired,
                        message: expected.scope,
                    });
                }
            }
            let enabled = match mutation {
                QuickToggleMutation::SetEnabled(enabled) => enabled,
                _ => {
                    return Err(QuickToggleError {
                        kind: QuickToggleErrorKind::InvalidRequest,
                        message: "wrong command".to_owned(),
                    });
                }
            };
            let control = QuickToggleControl::Switch {
                enabled,
                confirmation: None,
            };
            self.controls.insert(id, control.clone());
            Ok(QuickToggleObservation {
                control,
                detail: "updated".to_owned(),
            })
        }
    }

    #[test]
    fn disruptive_radio_change_requires_matching_confirmation() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        service.refresh(QuickToggleId::Wifi);

        let failure = service
            .execute(QuickToggleCommand {
                id: QuickToggleId::Wifi,
                mutation: QuickToggleMutation::SetEnabled(false),
                confirmation_token: None,
            })
            .expect_err("confirmation is mandatory");

        assert_eq!(failure.kind, QuickToggleErrorKind::ConfirmationRequired);
        let snapshot = service
            .snapshots()
            .find(|snapshot| snapshot.id == QuickToggleId::Wifi)
            .expect("Wi-Fi snapshot");
        assert!(matches!(
            snapshot.observation.as_ref().unwrap().control,
            QuickToggleControl::Switch { enabled: true, .. }
        ));
    }

    #[test]
    fn refresh_reports_a_provider_change_after_kestrel_mutation() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        service
            .execute(QuickToggleCommand {
                id: QuickToggleId::Wifi,
                mutation: QuickToggleMutation::SetEnabled(false),
                confirmation_token: Some("wifi-off".to_owned()),
            })
            .expect("confirmed command succeeds");
        service.backend.controls.insert(
            QuickToggleId::Wifi,
            QuickToggleControl::Switch {
                enabled: true,
                confirmation: None,
            },
        );

        let snapshot = service.refresh(QuickToggleId::Wifi);

        assert_eq!(snapshot.source, ToggleStateSource::ChangedElsewhere);
    }

    #[test]
    fn provider_changes_remain_external_when_state_returns_to_a_prior_request() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        service
            .execute(QuickToggleCommand {
                id: QuickToggleId::Wifi,
                mutation: QuickToggleMutation::SetEnabled(false),
                confirmation_token: Some("wifi-off".to_owned()),
            })
            .expect("confirmed command succeeds");
        for enabled in [true, true, false] {
            service.backend.controls.insert(
                QuickToggleId::Wifi,
                QuickToggleControl::Switch {
                    enabled,
                    confirmation: None,
                },
            );

            let snapshot = service.refresh(QuickToggleId::Wifi);

            assert_eq!(snapshot.source, ToggleStateSource::ChangedElsewhere);
        }
    }

    #[test]
    fn one_unavailable_adapter_does_not_erase_other_toggle_state() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        service.refresh(QuickToggleId::Wifi);
        service.refresh(QuickToggleId::Appearance);

        let wifi = service
            .snapshots()
            .find(|snapshot| snapshot.id == QuickToggleId::Wifi)
            .expect("Wi-Fi snapshot");
        let appearance = service
            .snapshots()
            .find(|snapshot| snapshot.id == QuickToggleId::Appearance)
            .expect("appearance snapshot");
        assert!(wifi.observation.is_some());
        assert!(appearance.error.is_some());
    }
}
