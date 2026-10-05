//! Lifecycle for global shortcuts: one backend registration at a time, with a
//! shared status the window can poll.

use std::sync::{Arc, Mutex};

use kestrel_platform::global_shortcuts::{
    ActivationSink, BindingState, BindingStatus, SharedShortcutStatus, ShortcutBackend,
    ShortcutError, ShortcutPhase, ShortcutRequest, ShortcutStatus, lock_status, update_status,
};

pub struct GlobalShortcutService {
    backend: Option<Box<dyn ShortcutBackend>>,
    status: SharedShortcutStatus,
}

impl Default for GlobalShortcutService {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalShortcutService {
    pub fn new() -> Self {
        Self {
            backend: None,
            status: Arc::new(Mutex::new(ShortcutStatus::default())),
        }
    }

    pub fn is_started(&self) -> bool {
        self.backend.is_some()
    }

    /// Starts `backend`, stopping any earlier registration first.
    pub fn start(
        &mut self,
        mut backend: Box<dyn ShortcutBackend>,
        requests: Vec<ShortcutRequest>,
        sink: ActivationSink,
    ) -> Result<(), ShortcutError> {
        self.stop();
        let provider = backend.provider();
        let bindings = requests
            .iter()
            .map(|request| BindingStatus {
                id: request.id.clone(),
                requested: request.trigger.to_string(),
                state: BindingState::Pending,
            })
            .collect();
        update_status(&self.status, |status| {
            status.provider = Some(provider);
            status.phase = ShortcutPhase::Starting;
            status.bindings = bindings;
        });
        match backend.start(requests, sink, Arc::clone(&self.status)) {
            Ok(()) => {
                self.backend = Some(backend);
                Ok(())
            }
            Err(error) => {
                backend.stop();
                update_status(&self.status, |status| {
                    status.phase = ShortcutPhase::Failed(error.clone());
                });
                Err(error)
            }
        }
    }

    /// Releases every binding; the backend joins its worker before this returns.
    pub fn stop(&mut self) {
        let Some(mut backend) = self.backend.take() else {
            return;
        };
        backend.stop();
        update_status(&self.status, |status| {
            status.phase = ShortcutPhase::Stopped;
            status.bindings.clear();
        });
    }

    pub fn snapshot(&self) -> ShortcutStatus {
        lock_status(&self.status).clone()
    }
}

impl Drop for GlobalShortcutService {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use kestrel_core::ShortcutTrigger;
    use kestrel_platform::global_shortcuts::{
        ActivationSink, BindingState, SharedShortcutStatus, ShortcutBackend, ShortcutError,
        ShortcutErrorKind, ShortcutPhase, ShortcutProvider, ShortcutRequest, update_status,
    };

    use super::GlobalShortcutService;

    /// Binds everything at once, records stops, and exposes the sink.
    struct FakeBackend {
        fail: bool,
        stops: Arc<AtomicUsize>,
        sink: Arc<Mutex<Option<ActivationSink>>>,
    }

    impl ShortcutBackend for FakeBackend {
        fn provider(&self) -> ShortcutProvider {
            ShortcutProvider::Portal
        }

        fn start(
            &mut self,
            _requests: Vec<ShortcutRequest>,
            sink: ActivationSink,
            status: SharedShortcutStatus,
        ) -> Result<(), ShortcutError> {
            if self.fail {
                return Err(ShortcutError::new(ShortcutErrorKind::Denied, "denied"));
            }
            update_status(&status, |status| {
                status.phase = ShortcutPhase::Active;
                for binding in &mut status.bindings {
                    binding.state = BindingState::Bound { trigger: None };
                }
            });
            *self.sink.lock().expect("sink lock") = Some(sink);
            Ok(())
        }

        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn backend(
        fail: bool,
    ) -> (
        Box<FakeBackend>,
        Arc<AtomicUsize>,
        Arc<Mutex<Option<ActivationSink>>>,
    ) {
        let stops = Arc::new(AtomicUsize::new(0));
        let sink = Arc::new(Mutex::new(None));
        (
            Box::new(FakeBackend {
                fail,
                stops: Arc::clone(&stops),
                sink: Arc::clone(&sink),
            }),
            stops,
            sink,
        )
    }

    fn request(id: &str) -> ShortcutRequest {
        ShortcutRequest {
            id: id.to_owned(),
            description: id.to_owned(),
            trigger: ShortcutTrigger::parse("LOGO+ALT+k").expect("valid trigger"),
        }
    }

    #[test]
    fn a_started_registration_routes_activations_and_stops_once() {
        let mut service = GlobalShortcutService::new();
        let (fake, stops, sink) = backend(false);
        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = Arc::clone(&received);

        service
            .start(
                fake,
                vec![request("window.show")],
                Arc::new(move |id: &str| log.lock().expect("log").push(id.to_owned())),
            )
            .expect("the fake binds");
        let status = service.snapshot();
        assert_eq!(status.phase, ShortcutPhase::Active);
        assert_eq!(status.bindings[0].requested, "ALT+LOGO+k");
        assert!(matches!(
            status.bindings[0].state,
            BindingState::Bound { .. }
        ));

        (sink
            .lock()
            .expect("sink")
            .as_ref()
            .expect("sink was handed over"))("window.show");
        assert_eq!(
            *received.lock().expect("log"),
            vec!["window.show".to_owned()]
        );

        service.stop();
        service.stop();
        assert_eq!(stops.load(Ordering::SeqCst), 1, "stop is idempotent");
        assert_eq!(service.snapshot().phase, ShortcutPhase::Stopped);
        assert!(service.snapshot().bindings.is_empty());
        assert!(!service.is_started());
    }

    #[test]
    fn a_failed_start_reports_the_error_and_holds_nothing() {
        let mut service = GlobalShortcutService::new();
        let (fake, stops, _sink) = backend(true);

        let error = service
            .start(fake, vec![request("window.show")], Arc::new(|_: &str| {}))
            .expect_err("the fake refuses");

        assert_eq!(error.kind, ShortcutErrorKind::Denied);
        assert!(matches!(service.snapshot().phase, ShortcutPhase::Failed(_)));
        assert!(!service.is_started());
        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "a failed backend is cleaned up"
        );
    }

    #[test]
    fn restarting_releases_the_previous_registration_first() {
        let mut service = GlobalShortcutService::new();
        let (first, first_stops, _) = backend(false);
        let (second, second_stops, _) = backend(false);

        service
            .start(first, vec![request("window.show")], Arc::new(|_: &str| {}))
            .expect("first binds");
        service
            .start(second, vec![request("app.quit")], Arc::new(|_: &str| {}))
            .expect("second binds");

        assert_eq!(first_stops.load(Ordering::SeqCst), 1);
        assert_eq!(second_stops.load(Ordering::SeqCst), 0);
        assert_eq!(service.snapshot().bindings[0].id, "app.quit");
        drop(service);
        assert_eq!(
            second_stops.load(Ordering::SeqCst),
            1,
            "drop releases bindings"
        );
    }
}
