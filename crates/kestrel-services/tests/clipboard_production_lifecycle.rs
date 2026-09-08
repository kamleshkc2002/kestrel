use std::{
    env, thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use kestrel_platform::clipboard::{
    discover_provider, ArboardClipboardBackend, ClipboardBackend, ClipboardError,
    ClipboardProvider, PrivacyEventSource,
};
use kestrel_services::clipboard::{ClipboardHistoryService, ClipboardLifecycle, ClipboardPolicy};

struct InertPrivacySource;

impl PrivacyEventSource for InertPrivacySource {
    fn poll_events(
        &mut self,
    ) -> Result<Vec<kestrel_platform::clipboard::SessionPrivacyEvent>, ClipboardError> {
        Ok(Vec::new())
    }
}

#[test]
fn production_backend_reowns_and_releases_a_clean_session_selection() {
    let Ok(expected_provider) = env::var("KESTREL_EXPECT_CLIPBOARD_PROVIDER") else {
        return;
    };
    let expected_provider = match expected_provider.as_str() {
        "x11" => ClipboardProvider::X11,
        "wayland" => ClipboardProvider::WaylandDataControl,
        value => panic!("unsupported expected clipboard provider: {value}"),
    };
    let provider = discover_provider().expect("clean session exposes a clipboard provider");
    assert_eq!(provider, expected_provider);
    let mut backend =
        ArboardClipboardBackend::new(provider).expect("production clipboard initializes");
    assert_eq!(
        backend.read_text().expect("initial selection is readable"),
        None,
        "the lifecycle test requires an initially empty clean session"
    );
    let marker = format!(
        "kestrel-lifecycle-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock follows the Unix epoch")
            .as_nanos()
    );
    backend
        .write_text(&marker)
        .expect("generated marker becomes the selection");

    let mut service = ClipboardHistoryService::new(ClipboardPolicy {
        poll_interval: Duration::from_millis(10),
        ..ClipboardPolicy::default()
    })
    .expect("production test policy is valid");
    service
        .start(backend, InertPrivacySource)
        .expect("clipboard history worker starts");
    wait_until(Duration::from_secs(2), || service.latest().items.len() == 1);
    assert_eq!(service.latest().lifecycle, ClipboardLifecycle::Running);

    service.stop();

    assert_eq!(service.latest().lifecycle, ClipboardLifecycle::Stopped);
    assert!(service.latest().items.is_empty());
    let mut verifier =
        ArboardClipboardBackend::new(provider).expect("verification clipboard initializes");
    assert_eq!(
        verifier
            .read_text()
            .expect("released selection is readable"),
        None
    );
}

fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("clipboard lifecycle condition was not met before timeout");
}
