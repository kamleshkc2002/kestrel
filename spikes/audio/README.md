# Phase 0 spike: audio stream discovery and controls

Disposable Rust probe for Kestrel issue #4. It is **not** a Cargo workspace
member and its `serde`/`serde_json` dependencies remain isolated from the main
project. The probe uses `pactl`'s structured JSON mode only to validate the
PulseAudio-compatible protocol available to the current user session.

```sh
cargo run --quiet --manifest-path Cargo.toml > /tmp/kestrel-audio-discovery.json
cargo run --quiet --manifest-path Cargo.toml -- --exercise-mutation > /tmp/kestrel-audio-mutation.json
```

## Scope

The read-only default mode reports:

- local PulseAudio/PipeWire server facts, without the Pulse cookie;
- output and input device discovery;
- per-application sink-input discovery, with application/media labels only;
- `CapabilityReport`-shaped entries for server connectivity, outputs, streams,
  and output-mutation eligibility;
- actionable missing-command, unavailable-server, malformed-output, and
  no-active-stream states.

The report intentionally excludes process IDs, command lines, network
identifiers, and the Pulse authentication cookie.

## Mutation safeguard

`--exercise-mutation` is intentionally separate from discovery. It runs only
when the default output has no active sink-inputs and all of its channel volumes
share one raw value that can be restored exactly. The probe then:

1. snapshots the default sink's mute and raw volume;
2. toggles mute and verifies the result;
3. changes volume by a small raw step and verifies the result;
4. restores the original volume and mute in a `finally` path;
5. verifies the final sink state equals the snapshot.

If playback is active, no default sink exists, or exact restoration is not
possible, mutation is reported as `skipped` and no audio setting is changed.

## Observed result

On the initial Pop!_OS session, `pactl` reached `PulseAudio (on PipeWire
1.5.85)` through protocol version 35. It discovered one built-in analog output,
two sources, and no active sink-inputs. The stream capability is therefore
reported as `Limited` until a real application stream is available for testing.

The explicit idle-sink test toggled mute, changed the output volume from raw
`0` to `655`, and restored both mute and volume to their original values.

## Initial adapter decision

This session exposes `pactl` through PipeWire's PulseAudio compatibility server,
so the initial Kestrel audio-service adapter should target the
**PulseAudio-compatible protocol** for stream discovery, volume, mute, and
basic routing. This probe's command execution and JSON parsing are Phase 0
evidence only; a production `kestrel-platform` adapter must use a typed
PulseAudio client/binding rather than parse `pactl` output. Native
PipeWire/WirePlumber remains a future adapter for advanced node and link
routing.

## Exit evidence

Record the server version, selected default device, active-stream count,
mutation outcome, restoration result, and capability report in issue #4. Run
the same probe on another target desktop/session before treating the selected
adapter as release-ready.
