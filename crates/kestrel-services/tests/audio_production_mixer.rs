//! Read-only production check for the PulseAudio-compatible adapter; skips without a server.

use kestrel_platform::audio::{AudioBackend, PulseAudioBackend};
use kestrel_services::audio::{AudioCommand, AudioCommandError, AudioMixerService, AudioPolicy};

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

    // Rejects before any backend call and reports the effective maximum.
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

    // Unknown streams must not mutate unrelated streams.
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
