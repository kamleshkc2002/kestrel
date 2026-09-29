//! Production check for the PulseAudio-compatible audio adapter.
//!
//! `cargo test --workspace` must tolerate machines without an audio server, so
//! this test skips when no server is reachable. It is read-only: it never changes
//! the session's default output, device volumes, or stream routing.

use kestrel_platform::audio::{AudioBackend, PulseAudioBackend};
use kestrel_services::audio::{AudioCommand, AudioCommandError, AudioMixerService, AudioPolicy};

/// Returns a backend only when the real server answers discovery.
fn reachable_backend() -> Option<PulseAudioBackend> {
    let mut backend = PulseAudioBackend::new();
    match backend.discover() {
        Ok(_) => Some(backend),
        Err(error) => {
            eprintln!("skipping audio production check: no usable server ({error})");
            None
        }
    }
}

#[test]
fn discovery_and_policy_projection_work_against_a_live_server() {
    let Some(backend) = reachable_backend() else {
        return;
    };

    let mut service =
        AudioMixerService::with_policy(backend, AudioPolicy::default().with_boost_percent(120))
            .expect("the built-in policy is valid");
    let snapshot = service.refresh().expect("the live server is discoverable");

    // Structural invariants that must hold for any real session graph.
    assert!(
        snapshot
            .outputs
            .iter()
            .all(|output| !output.name.is_empty() && !output.description.is_empty()),
        "every discovered output must carry a name and description"
    );
    assert!(
        snapshot
            .outputs
            .iter()
            .filter(|output| output.is_default)
            .count()
            <= 1,
        "at most one discovered output may be the default"
    );
    assert_eq!(
        snapshot.boost_ceiling_percent, 120,
        "the configured ceiling must be projected into the snapshot"
    );
    assert!(
        snapshot.streams.iter().all(|stream| !stream.corked),
        "inactive streams are hidden by default"
    );
    for group in snapshot.outputs_by_card() {
        assert!(!group.1.is_empty());
    }

    // The configured ceiling must reject a boosted request before any backend call
    // and must report the effective maximum instead of clamping silently.
    if let Some(output) = snapshot.default_output() {
        let output_id = output.id;
        let failure = service
            .execute(AudioCommand::SetOutputVolume {
                output_id,
                volume_percent: 121,
            })
            .expect_err("a request above the configured ceiling must be rejected");
        assert_eq!(
            failure.error,
            AudioCommandError::VolumeOutOfRange {
                requested: 121,
                maximum: 120,
            }
        );
    }
}

#[test]
fn out_of_range_stream_requests_never_reach_the_server() {
    let Some(backend) = reachable_backend() else {
        return;
    };
    let mut service = AudioMixerService::new(backend);
    let output_count = service
        .refresh()
        .expect("the live server is discoverable")
        .outputs
        .len();

    // An unknown stream must be reported as unavailable rather than panicking or
    // mutating an unrelated stream.
    let failure = service
        .execute(AudioCommand::SetStreamMute {
            stream_id: u32::MAX,
            muted: true,
        })
        .expect_err("unknown streams must be rejected");
    assert_eq!(
        failure.error,
        AudioCommandError::UnknownStream {
            stream_id: u32::MAX
        }
    );
    assert_eq!(output_count, service.latest().outputs.len());
}
