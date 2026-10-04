# Phase 0 spike: audio stream discovery and controls

Disposable Rust probe for Kestrel issue #4. It is outside the Cargo workspace;
`serde` and `serde_json` remain isolated. It uses `pactl` structured JSON only to
validate the current user's PulseAudio-compatible protocol session.

```sh
cargo run --quiet --manifest-path Cargo.toml > /tmp/kestrel-audio-discovery.json
cargo run --quiet --manifest-path Cargo.toml -- --exercise-mutation > /tmp/kestrel-audio-mutation.json
```

## Scope

Read-only mode reports:

- local PulseAudio/PipeWire server facts without the Pulse cookie;
- output/input devices and per-application sink inputs (labels only);
- `CapabilityReport`-shaped entries for connectivity, outputs, streams, and
  output-mutation eligibility;
- missing-command, unavailable-server, malformed-output, and no-active-stream states.

It excludes process IDs, command lines, network identifiers, and the Pulse
authentication cookie.

## Mutation safeguard

`--exercise-mutation` is separate and runs only when the default output has no
active sink inputs, and all channel volumes share one exactly restorable raw
value. It snapshots mute/volume, toggles mute, changes volume by a small raw
step, restores both in a `finally` path, and verifies the final state. Active
playback, no default sink, or non-restorable volume yields `skipped` without
changing an audio setting.

## Observed result

On the initial Pop!_OS session, `pactl` reached `PulseAudio (on PipeWire 1.5.85)`
(protocol 35), found one built-in analog output, two sources, and no active sink
inputs. Streams were `Limited` until a real application stream is available.
The idle-sink test changed raw volume `0` to `655` and restored mute and volume.

## Initial adapter decision

The initial Kestrel audio-service adapter should target the **PulseAudio-compatible
protocol** through a typed PulseAudio client/binding for discovery, volume, mute,
and basic routing. This probe's command execution and JSON parsing are Phase 0
evidence only; native PipeWire/WirePlumber remains a future advanced-routing
adapter.

## Exit evidence

Record server version, selected default device, active-stream count, mutation
outcome, restoration result, and capability report in issue #4. Repeat on another
target desktop/session before treating the adapter as release-ready.
