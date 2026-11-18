//! Replacing Kestrel's own selection must never leave the clipboard empty in
//! between: an empty moment lets bridges and managers take the selection over.
//! Runs only in the clean Wayland session of `scripts/clipboard-clean-session.sh`.

use std::{
    env,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use kestrel_platform::clipboard::{
    ArboardClipboardBackend, ClipboardBackend, ClipboardProvider, OwnedSelection, discover_provider,
};
use wl_clipboard_rs::{
    paste::Seat,
    watch::{ClipboardEvent, ClipboardType, Watcher},
};

/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Changed,
    Cleared,
}

#[test]
fn replacing_an_owned_selection_never_empties_the_clipboard() {
    if env::var("KESTREL_EXPECT_CLIPBOARD_PROVIDER").as_deref() != Ok("wayland") {
        return;
    }
    let provider = discover_provider().expect("clean session exposes a clipboard provider");
    assert_eq!(provider, ClipboardProvider::WaylandDataControl);
    let mut backend = ArboardClipboardBackend::new(provider).expect("production clipboard starts");

    let mut watcher = Watcher::new(ClipboardType::Regular, Seat::Unspecified)
        .expect("the session offers a data-control watcher");
    let cancel = watcher.cancel_handle();
    let (events, received) = mpsc::channel();
    let watching = thread::spawn(move || {
        while let Ok(Some(event)) = watcher.next_event() {
            let seen = match event {
                ClipboardEvent::Changed { .. } => Seen::Changed,
                ClipboardEvent::Cleared { .. } => Seen::Cleared,
            };
            if events.send(seen).is_err() {
                break;
            }
        }
    });
    // The first event reports the selection as it was before any write.
    received
        .recv_timeout(Duration::from_secs(5))
        .expect("the watcher reports the starting selection");

    let first = OwnedSelection::Text("kestrel-replace-first".to_owned());
    backend
        .write_text("kestrel-replace-first")
        .expect("the first text becomes the selection");
    assert_eq!(next(&received), Seen::Changed);

    backend
        .write_text("kestrel-replace-second")
        .expect("text replaces text");
    backend
        .write_file_list(&["/tmp/kestrel-replace-file".to_owned()])
        .expect("files replace text");
    backend
        .write_image_png(PNG)
        .expect("an image replaces files");
    backend
        .write_text("kestrel-replace-last")
        .expect("text replaces an image");

    // Every replacement is one change, never a clear followed by a change.
    let mut replacements = Vec::new();
    let quiet_after = Instant::now() + Duration::from_secs(2);
    while Instant::now() < quiet_after {
        if let Ok(seen) = received.recv_timeout(Duration::from_millis(100)) {
            replacements.push(seen);
        }
    }
    assert!(
        !replacements.contains(&Seen::Cleared),
        "a replacement emptied the clipboard: {replacements:?}"
    );
    assert_eq!(replacements, vec![Seen::Changed; 4]);

    // Leave the clean session empty for the next run.
    let last = OwnedSelection::Text("kestrel-replace-last".to_owned());
    assert_ne!(first, last);
    assert!(
        backend
            .clear_if_matches(&last)
            .expect("the owned selection can be cleared")
    );
    cancel.cancel();
    watching.join().expect("the watcher thread exits");
}

fn next(received: &mpsc::Receiver<Seen>) -> Seen {
    received
        .recv_timeout(Duration::from_secs(5))
        .expect("the write is observed")
}
