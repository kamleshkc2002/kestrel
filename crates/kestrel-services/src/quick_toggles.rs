//! Quick-toggle state, confirmation, and ownership policy.

use std::{collections::BTreeMap, time::Duration};

use kestrel_core::KeepAwakeDuration;
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
    /// Set while Kestrel holds the keep-awake inhibitor.
    pub keep_awake: Option<KeepAwakeSession>,
}

/// The chosen keep-awake length and, for a timed session, its boot-time end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepAwakeSession {
    pub duration: KeepAwakeDuration,
    /// `None` while the session runs until it is turned off.
    pub deadline: Option<Duration>,
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
    keep_awake_session: Option<KeepAwakeSession>,
    /// Changes whenever the session does; orders published state.
    keep_awake_revision: u64,
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
                        keep_awake: None,
                    },
                )
            })
            .collect();
        Self {
            backend,
            snapshots,
            keep_awake_session: None,
            keep_awake_revision: 0,
        }
    }

    pub fn refresh(&mut self, id: QuickToggleId) -> &QuickToggleSnapshot {
        match self.backend.observe(id) {
            Ok(observation) => {
                let snapshot = self
                    .snapshots
                    .get(&id)
                    .expect("every built-in quick toggle has a snapshot");
                let source = if id == QuickToggleId::KeepAwake {
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
        let session = if id == QuickToggleId::KeepAwake {
            self.keep_awake_session
        } else {
            None
        };
        self.snapshots
            .get_mut(&id)
            .expect("every built-in quick toggle has a snapshot")
            .keep_awake = session;
        &self.snapshots[&id]
    }

    pub fn execute(
        &mut self,
        command: QuickToggleCommand,
    ) -> Result<&QuickToggleSnapshot, QuickToggleError> {
        let disabled = matches!(&command.mutation, QuickToggleMutation::SetEnabled(false));
        let result = self.backend.apply(
            command.id,
            command.mutation,
            command.confirmation_token.as_deref(),
        );
        let session = if command.id == QuickToggleId::KeepAwake {
            if disabled && self.keep_awake_session.take().is_some() {
                self.bump_keep_awake_revision();
            }
            self.keep_awake_session
        } else {
            None
        };
        match result {
            Ok(observation) => {
                let snapshot = self
                    .snapshots
                    .get_mut(&command.id)
                    .expect("every built-in quick toggle has a snapshot");
                snapshot.observation = Some(observation);
                snapshot.source = ToggleStateSource::KestrelOwned;
                snapshot.error = None;
                snapshot.keep_awake = session;
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

    /// Holds the inhibitor and runs for `duration` from `now`, replacing any
    /// earlier deadline.
    pub fn start_keep_awake(
        &mut self,
        duration: KeepAwakeDuration,
        now: Duration,
    ) -> Result<&QuickToggleSnapshot, QuickToggleError> {
        self.execute(QuickToggleCommand {
            id: QuickToggleId::KeepAwake,
            mutation: QuickToggleMutation::SetEnabled(true),
            confirmation_token: None,
        })?;
        let deadline = match duration {
            KeepAwakeDuration::UntilStopped => None,
            KeepAwakeDuration::For(length) => Some(now.saturating_add(length)),
        };
        self.keep_awake_session = Some(KeepAwakeSession { duration, deadline });
        self.bump_keep_awake_revision();
        let snapshot = self
            .snapshots
            .get_mut(&QuickToggleId::KeepAwake)
            .expect("every built-in quick toggle has a snapshot");
        snapshot.keep_awake = self.keep_awake_session;
        Ok(snapshot)
    }

    /// Releases the inhibitor and clears the session; safe to repeat.
    pub fn stop_keep_awake(&mut self) {
        if self.keep_awake_session.take().is_some() {
            self.bump_keep_awake_revision();
        }
        let _ = self.backend.apply(
            QuickToggleId::KeepAwake,
            QuickToggleMutation::SetEnabled(false),
            None,
        );
        self.refresh(QuickToggleId::KeepAwake);
    }

    /// Ends a timed session at or after its deadline, returning true once.
    pub fn expire_keep_awake(&mut self, now: Duration) -> bool {
        let expired = self
            .keep_awake_session
            .and_then(|session| session.deadline)
            .is_some_and(|deadline| now >= deadline);
        if expired {
            self.stop_keep_awake();
        }
        expired
    }

    pub fn snapshots(&self) -> impl Iterator<Item = &QuickToggleSnapshot> {
        ALL_QUICK_TOGGLES.iter().map(|id| &self.snapshots[id])
    }

    /// Changes whenever the keep-awake session does, including restarts.
    pub fn keep_awake_revision(&self) -> u64 {
        self.keep_awake_revision
    }

    fn bump_keep_awake_revision(&mut self) {
        self.keep_awake_revision = self.keep_awake_revision.wrapping_add(1);
    }

    /// Provider-observed toggles remain untouched on stop.
    pub fn stop(&mut self, id: QuickToggleId) {
        if id == QuickToggleId::KeepAwake {
            if self.keep_awake_session.take().is_some() {
                self.bump_keep_awake_revision();
            }
            let _ = self
                .backend
                .apply(id, QuickToggleMutation::SetEnabled(false), None);
            self.refresh(id);
        }
    }

    pub fn stop_all(&mut self) {
        self.stop(QuickToggleId::KeepAwake);
    }
}

impl<B: QuickToggleBackend> Drop for QuickToggleService<B> {
    fn drop(&mut self) {
        self.stop_all();
    }
}

fn local_source(id: QuickToggleId) -> ToggleStateSource {
    if id == QuickToggleId::KeepAwake {
        ToggleStateSource::KestrelOwned
    } else {
        ToggleStateSource::ProviderObserved
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use kestrel_core::{CapabilityReport, CapabilityStatus, KeepAwakeDuration};
    use kestrel_platform::quick_toggles::{
        MutationConfirmation, QuickToggleBackend, QuickToggleControl, QuickToggleError,
        QuickToggleErrorKind, QuickToggleId, QuickToggleMutation, QuickToggleObservation,
    };

    use super::{KeepAwakeSession, QuickToggleCommand, QuickToggleService, ToggleStateSource};

    struct FakeBackend {
        controls: BTreeMap<QuickToggleId, QuickToggleControl>,
        /// Keep-awake release requests, to observe that an expiry releases once.
        keep_awake_releases: usize,
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
            ]
            .into_iter()
            .collect();
            Self {
                controls,
                keep_awake_releases: 0,
            }
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
            if id == QuickToggleId::KeepAwake && !enabled {
                self.keep_awake_releases += 1;
            }
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

    #[test]
    fn stopping_owned_keep_awake_does_not_disable_provider_wifi() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        service
            .execute(QuickToggleCommand {
                id: QuickToggleId::KeepAwake,
                mutation: QuickToggleMutation::SetEnabled(true),
                confirmation_token: None,
            })
            .expect("enabling Keep Awake succeeds");
        service.refresh(QuickToggleId::Wifi);

        service.stop(QuickToggleId::KeepAwake);
        service.stop(QuickToggleId::Wifi);

        assert!(matches!(
            service.backend.controls.get(&QuickToggleId::KeepAwake),
            Some(QuickToggleControl::Switch { enabled: false, .. })
        ));
        assert!(matches!(
            service.backend.controls.get(&QuickToggleId::Wifi),
            Some(QuickToggleControl::Switch { enabled: true, .. })
        ));
        let keep_awake = service
            .snapshots()
            .find(|snapshot| snapshot.id == QuickToggleId::KeepAwake)
            .expect("Keep Awake snapshot");
        assert_eq!(keep_awake.source, ToggleStateSource::KestrelOwned);
        assert!(matches!(
            keep_awake.observation.as_ref().unwrap().control,
            QuickToggleControl::Switch { enabled: false, .. }
        ));
        let wifi = service
            .snapshots()
            .find(|snapshot| snapshot.id == QuickToggleId::Wifi)
            .expect("Wi-Fi snapshot");
        assert_eq!(wifi.source, ToggleStateSource::ProviderObserved);
        assert!(matches!(
            wifi.observation.as_ref().unwrap().control,
            QuickToggleControl::Switch { enabled: true, .. }
        ));
    }

    fn keep_awake(service: &QuickToggleService<FakeBackend>) -> Option<KeepAwakeSession> {
        service
            .snapshots()
            .find(|snapshot| snapshot.id == QuickToggleId::KeepAwake)
            .expect("Keep Awake snapshot")
            .keep_awake
    }

    #[test]
    fn timed_session_expires_exactly_at_its_deadline_releasing_once() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        let start = Duration::from_secs(1_000);
        let duration = KeepAwakeDuration::timed(Duration::from_secs(60)).expect("valid");

        let snapshot = service
            .start_keep_awake(duration, start)
            .expect("the inhibitor is held")
            .clone();

        assert_eq!(
            snapshot.keep_awake.map(|session| session.deadline),
            Some(Some(Duration::from_secs(1_060)))
        );
        assert!(!service.expire_keep_awake(Duration::from_secs(1_059)));
        assert!(keep_awake(&service).is_some());
        assert!(service.expire_keep_awake(Duration::from_secs(1_060)));
        assert_eq!(service.backend.keep_awake_releases, 1);
        assert!(keep_awake(&service).is_none());
        assert!(matches!(
            service.backend.controls.get(&QuickToggleId::KeepAwake),
            Some(QuickToggleControl::Switch { enabled: false, .. })
        ));
        assert!(!service.expire_keep_awake(Duration::from_secs(2_000)));
        assert_eq!(service.backend.keep_awake_releases, 1);
    }

    #[test]
    fn restarting_replaces_the_deadline() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        let fifteen = KeepAwakeDuration::timed(Duration::from_secs(15 * 60)).expect("valid");
        let thirty = KeepAwakeDuration::timed(Duration::from_secs(30 * 60)).expect("valid");

        service
            .start_keep_awake(fifteen, Duration::ZERO)
            .expect("start");
        assert_eq!(
            keep_awake(&service).and_then(|session| session.deadline),
            Some(Duration::from_secs(900))
        );
        service
            .start_keep_awake(thirty, Duration::from_secs(100))
            .expect("restart");
        let session = keep_awake(&service).expect("session");
        assert_eq!(session.duration, thirty);
        assert_eq!(session.deadline, Some(Duration::from_secs(1_900)));
        assert_eq!(service.backend.keep_awake_releases, 0);
    }

    #[test]
    fn keep_awake_revision_tracks_each_session_change() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        let fifteen = KeepAwakeDuration::timed(Duration::from_secs(15 * 60)).expect("valid");
        let thirty = KeepAwakeDuration::timed(Duration::from_secs(30 * 60)).expect("valid");
        assert_eq!(service.keep_awake_revision(), 0);

        service
            .start_keep_awake(fifteen, Duration::ZERO)
            .expect("start");
        let started = service.keep_awake_revision();
        assert_eq!(started, 1);

        service
            .start_keep_awake(thirty, Duration::from_secs(100))
            .expect("restart");
        let restarted = service.keep_awake_revision();
        assert_eq!(restarted, started + 1);

        service.stop_keep_awake();
        let stopped = service.keep_awake_revision();
        assert_eq!(stopped, restarted + 1);

        // Stopping again while nothing runs changes nothing.
        service.stop_keep_awake();
        assert_eq!(service.keep_awake_revision(), stopped);
    }

    #[test]
    fn keep_awake_revision_changes_when_a_timed_session_expires() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        let duration = KeepAwakeDuration::timed(Duration::from_secs(60)).expect("valid");
        service
            .start_keep_awake(duration, Duration::from_secs(1_000))
            .expect("start");
        let started = service.keep_awake_revision();

        assert!(!service.expire_keep_awake(Duration::from_secs(1_059)));
        assert_eq!(service.keep_awake_revision(), started);
        assert!(service.expire_keep_awake(Duration::from_secs(1_060)));
        assert!(service.keep_awake_revision() > started);
    }

    #[test]
    fn manual_stop_is_idempotent_and_clears_the_session() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        let duration = KeepAwakeDuration::timed(Duration::from_secs(30 * 60)).expect("valid");
        service
            .start_keep_awake(duration, Duration::ZERO)
            .expect("start");

        service.stop_keep_awake();
        service.stop_keep_awake();

        assert!(keep_awake(&service).is_none());
        assert!(!service.expire_keep_awake(Duration::from_secs(60 * 60)));
        assert!(matches!(
            service.backend.controls.get(&QuickToggleId::KeepAwake),
            Some(QuickToggleControl::Switch { enabled: false, .. })
        ));
    }

    #[test]
    fn an_indefinite_session_never_expires() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        service
            .start_keep_awake(KeepAwakeDuration::UntilStopped, Duration::ZERO)
            .expect("start");

        assert!(!service.expire_keep_awake(Duration::from_secs(365 * 24 * 60 * 60)));
        let session = keep_awake(&service).expect("session");
        assert_eq!(session.duration, KeepAwakeDuration::UntilStopped);
        assert_eq!(session.deadline, None);
        assert_eq!(service.backend.keep_awake_releases, 0);
    }

    #[test]
    fn stop_all_clears_the_keep_awake_session() {
        let mut service = QuickToggleService::new(FakeBackend::new());
        let duration = KeepAwakeDuration::timed(Duration::from_secs(8 * 60 * 60)).expect("valid");
        service
            .start_keep_awake(duration, Duration::ZERO)
            .expect("start");

        service.stop_all();

        assert!(keep_awake(&service).is_none());
        assert!(matches!(
            service.backend.controls.get(&QuickToggleId::KeepAwake),
            Some(QuickToggleControl::Switch { enabled: false, .. })
        ));
    }
}
