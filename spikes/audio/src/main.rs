//! Disposable Phase 0 probe for Kestrel Issue #4.
//!
//! This package is intentionally outside the main workspace. It validates the
//! PulseAudio-compatible protocol through pactl's structured JSON output; it
//! is not a production audio adapter.

use serde_json::{json, Map, Value};
use std::{
    env,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Debug)]
struct ProbeError {
    category: &'static str,
    detail: String,
}

impl ProbeError {
    fn new(category: &'static str, detail: impl Into<String>) -> Self {
        Self {
            category,
            detail: detail.into(),
        }
    }
}

struct Discovery {
    server: Value,
    sinks: Vec<Value>,
    sources: Vec<Value>,
    sink_inputs: Vec<Value>,
    default_sink: Option<Value>,
}

struct RestoreGuard {
    pactl: PathBuf,
    sink: String,
    mute: bool,
    volume: i64,
    restoration_attempted: bool,
}

impl RestoreGuard {
    fn restore(&mut self) -> Vec<String> {
        self.restoration_attempted = true;
        let mut errors = Vec::new();
        if let Err(error) = pactl_command(
            &self.pactl,
            &["set-sink-volume", &self.sink, &self.volume.to_string()],
        ) {
            errors.push(format!("volume: {}", error.detail));
        }
        if let Err(error) = pactl_command(
            &self.pactl,
            &[
                "set-sink-mute",
                &self.sink,
                if self.mute { "1" } else { "0" },
            ],
        ) {
            errors.push(format!("mute: {}", error.detail));
        }
        errors
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if !self.restoration_attempted {
            let _ = self.restore();
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn read_limited<R: Read>(mut reader: R) -> io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0_u8; 8192];

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if output.len() < MAX_OUTPUT_BYTES {
            let remaining = MAX_OUTPUT_BYTES - output.len();
            output.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        if output.len() == MAX_OUTPUT_BYTES && read > 0 {
            exceeded = true;
        }
    }

    Ok((output, exceeded))
}

fn run_command(program: &Path, arguments: &[&str]) -> Result<String, ProbeError> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ProbeError::new("missing_command", error.to_string()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProbeError::new("spawn_failed", "stdout pipe was unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ProbeError::new("spawn_failed", "stderr pipe was unavailable"))?;
    let stdout_reader = thread::spawn(move || read_limited(stdout));
    let stderr_reader = thread::spawn(move || read_limited(stderr));
    let deadline = Instant::now() + COMMAND_TIMEOUT;

    let status = loop {
        match child
            .try_wait()
            .map_err(|error| ProbeError::new("wait_failed", error.to_string()))?
        {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                child
                    .kill()
                    .map_err(|error| ProbeError::new("timeout", error.to_string()))?;
                break child
                    .wait()
                    .map_err(|error| ProbeError::new("wait_failed", error.to_string()))?;
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };

    let (stdout, stdout_exceeded) = stdout_reader
        .join()
        .map_err(|_| ProbeError::new("read_failed", "stdout reader panicked"))?
        .map_err(|error| ProbeError::new("read_failed", error.to_string()))?;
    let (stderr, stderr_exceeded) = stderr_reader
        .join()
        .map_err(|_| ProbeError::new("read_failed", "stderr reader panicked"))?
        .map_err(|error| ProbeError::new("read_failed", error.to_string()))?;

    if stdout_exceeded || stderr_exceeded {
        return Err(ProbeError::new(
            "output_limit",
            format!(
                "{} exceeded the {} byte output limit",
                program.display(),
                MAX_OUTPUT_BYTES
            ),
        ));
    }
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr);
        return Err(ProbeError::new(
            "command_failed",
            format!("{} exited {}: {}", program.display(), status, detail.trim()),
        ));
    }

    String::from_utf8(stdout).map_err(|error| ProbeError::new("invalid_output", error.to_string()))
}

fn pactl_json(pactl: &Path, arguments: &[&str]) -> Result<Value, ProbeError> {
    let mut command = vec!["--format=json"];
    command.extend_from_slice(arguments);
    let output = run_command(pactl, &command)?;
    serde_json::from_str(&output)
        .map_err(|error| ProbeError::new("malformed_json", error.to_string()))
}

fn pactl_command(pactl: &Path, arguments: &[&str]) -> Result<(), ProbeError> {
    run_command(pactl, arguments).map(|_| ())
}

fn field(value: &Value, key: &str) -> Value {
    value.get(key).cloned().unwrap_or(Value::Null)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn volume_snapshot(volume: Option<&Value>) -> Value {
    let mut channels = Map::new();
    if let Some(values) = volume.and_then(Value::as_object) {
        for (channel, value) in values {
            let raw = value
                .get("value")
                .or_else(|| value.get("raw"))
                .cloned()
                .unwrap_or(Value::Null);
            let percent = value
                .get("value_percent")
                .or_else(|| value.get("percent"))
                .cloned()
                .unwrap_or(Value::Null);
            channels.insert(channel.clone(), json!({ "raw": raw, "percent": percent }));
        }
    }
    Value::Object(channels)
}

fn uniform_raw_volume(sink: &Value) -> Option<i64> {
    let channels = volume_snapshot(sink.get("volume"));
    let values: Vec<i64> = channels
        .as_object()?
        .values()
        .map(|channel| channel.get("raw").and_then(Value::as_i64))
        .collect::<Option<Vec<_>>>()?;
    let first = *values.first()?;
    values.iter().all(|value| *value == first).then_some(first)
}

fn summarize_port(port: &Value) -> Value {
    json!({
        "name": field(port, "name"),
        "description": field(port, "description"),
        "availability": field(port, "availability"),
    })
}

fn summarize_sink(sink: &Value) -> Value {
    let ports = sink
        .get("ports")
        .and_then(Value::as_array)
        .map(|ports| ports.iter().map(summarize_port).collect::<Vec<_>>())
        .unwrap_or_default();
    json!({
        "index": field(sink, "index"),
        "name": field(sink, "name"),
        "description": field(sink, "description"),
        "driver": field(sink, "driver"),
        "state": field(sink, "state"),
        "mute": field(sink, "mute"),
        "volume": volume_snapshot(sink.get("volume")),
        "sample_specification": field(sink, "sample_specification"),
        "channel_map": field(sink, "channel_map"),
        "active_port": field(sink, "active_port"),
        "ports": ports,
    })
}

fn summarize_source(source: &Value) -> Value {
    json!({
        "index": field(source, "index"),
        "name": field(source, "name"),
        "description": field(source, "description"),
        "driver": field(source, "driver"),
        "state": field(source, "state"),
        "mute": field(source, "mute"),
        "volume": volume_snapshot(source.get("volume")),
        "active_port": field(source, "active_port"),
    })
}

fn summarize_stream(stream: &Value) -> Value {
    let properties = stream.get("properties").unwrap_or(&Value::Null);
    json!({
        "index": field(stream, "index"),
        "sink": field(stream, "sink"),
        "application_name": field(properties, "application.name"),
        "media_name": field(properties, "media.name"),
        "media_role": field(properties, "media.role"),
        "mute": field(stream, "mute"),
        "volume": volume_snapshot(stream.get("volume")),
    })
}

fn capability(
    feature_id: &str,
    status: &str,
    summary: &str,
    remediation: Option<&str>,
    evidence: Value,
) -> Value {
    json!({
        "feature_id": feature_id,
        "status": status,
        "summary": summary,
        "selected_backend": "PulseAudio-compatible protocol via pactl",
        "alternatives_considered": [
            "wpctl (PipeWire control CLI)",
            "native PipeWire/WirePlumber adapter for advanced routing",
        ],
        "remediation": remediation,
        "evidence": evidence,
    })
}

fn value_array(value: Value, description: &str) -> Result<Vec<Value>, ProbeError> {
    value
        .as_array()
        .cloned()
        .ok_or_else(|| ProbeError::new("malformed_json", description))
}

fn discover(pactl: &Path) -> Result<Discovery, ProbeError> {
    let info = pactl_json(pactl, &["info"])?;
    let sinks = value_array(
        pactl_json(pactl, &["list", "sinks"])?,
        "sinks were not an array",
    )?;
    let sources = value_array(
        pactl_json(pactl, &["list", "sources"])?,
        "sources were not an array",
    )?;
    let sink_inputs = value_array(
        pactl_json(pactl, &["list", "sink-inputs"])?,
        "sink-inputs were not an array",
    )?;
    let default_sink_name = string_field(&info, "default_sink_name");
    let default_sink = default_sink_name.as_deref().and_then(|name| {
        sinks
            .iter()
            .find(|sink| string_field(sink, "name").as_deref() == Some(name))
            .cloned()
    });

    Ok(Discovery {
        server: json!({
            "server_name": field(&info, "server_name"),
            "server_version": field(&info, "server_version"),
            "server_protocol_version": field(&info, "server_protocol_version"),
            "library_protocol_version": field(&info, "library_protocol_version"),
            "is_local": field(&info, "is_local"),
            "default_sink_name": field(&info, "default_sink_name"),
            "default_source_name": field(&info, "default_source_name"),
        }),
        sinks,
        sources,
        sink_inputs,
        default_sink,
    })
}

fn find_sink<'a>(discovery: &'a Discovery, name: &str) -> Option<&'a Value> {
    discovery
        .sinks
        .iter()
        .find(|sink| string_field(sink, "name").as_deref() == Some(name))
}

fn choose_changed_volume(original: i64) -> i64 {
    let step = 655;
    if original < 65_536 {
        (original + step).min(65_536)
    } else {
        (original - step).max(0)
    }
}

fn mutation_report(pactl: &Path, discovery: &Discovery) -> Value {
    let Some(default_sink) = discovery.default_sink.as_ref() else {
        return json!({
            "mode": "skipped",
            "reason": "No default sink is available for a reversible mutation.",
            "restored": true,
        });
    };
    if !discovery.sink_inputs.is_empty() {
        return json!({
            "mode": "skipped",
            "reason": "Active application streams are present; mutation is skipped to avoid disrupting playback.",
            "restored": true,
        });
    }

    let Some(sink_name) = string_field(default_sink, "name") else {
        return json!({
            "mode": "skipped",
            "reason": "The default sink lacks a name required for restoration.",
            "restored": true,
        });
    };
    let Some(original_mute) = default_sink.get("mute").and_then(Value::as_bool) else {
        return json!({
            "mode": "skipped",
            "reason": "The default sink lacks a mute state required for restoration.",
            "restored": true,
        });
    };
    let Some(original_volume) = uniform_raw_volume(default_sink) else {
        return json!({
            "mode": "skipped",
            "reason": "The default sink has unequal or unreadable channel volumes, so exact restoration is unavailable.",
            "restored": true,
        });
    };

    let target_volume = choose_changed_volume(original_volume);
    let mut guard = RestoreGuard {
        pactl: pactl.to_path_buf(),
        sink: sink_name.clone(),
        mute: original_mute,
        volume: original_volume,
        restoration_attempted: false,
    };
    let mut actions = Vec::new();
    let mut error = None;

    let attempt = (|| -> Result<(), ProbeError> {
        pactl_command(pactl, &["set-sink-mute", &sink_name, "toggle"])?;
        let after_toggle = discover(pactl)?;
        let toggled_sink = find_sink(&after_toggle, &sink_name);
        let expected_mute = !original_mute;
        let observed_mute = toggled_sink
            .and_then(|sink| sink.get("mute"))
            .and_then(Value::as_bool);
        let verified = observed_mute == Some(expected_mute);
        actions.push(json!({
            "action": "toggle_mute",
            "expected_mute": expected_mute,
            "observed_mute": observed_mute,
            "verified": verified,
        }));
        if !verified {
            return Err(ProbeError::new(
                "mutation_unverified",
                "mute state did not change as requested",
            ));
        }

        pactl_command(
            pactl,
            &["set-sink-volume", &sink_name, &target_volume.to_string()],
        )?;
        let after_volume = discover(pactl)?;
        let observed_volume = find_sink(&after_volume, &sink_name).and_then(uniform_raw_volume);
        let verified = observed_volume == Some(target_volume);
        actions.push(json!({
            "action": "set_volume",
            "original_raw": original_volume,
            "target_raw": target_volume,
            "observed_raw": observed_volume,
            "verified": verified,
        }));
        if !verified {
            return Err(ProbeError::new(
                "mutation_unverified",
                "volume state did not change to the requested raw value",
            ));
        }
        Ok(())
    })();

    if let Err(attempt_error) = attempt {
        error = Some(attempt_error.detail);
    }

    let restore_errors = guard.restore();
    if !restore_errors.is_empty() {
        let detail = restore_errors.join("; ");
        error = Some(
            error
                .map(|current| format!("{current}; {detail}"))
                .unwrap_or(detail),
        );
    }

    let restored = discover(pactl)
        .ok()
        .and_then(|current| find_sink(&current, &sink_name).cloned())
        .is_some_and(|sink| {
            sink.get("mute").and_then(Value::as_bool) == Some(original_mute)
                && uniform_raw_volume(&sink) == Some(original_volume)
        });

    json!({
        "mode": "attempted",
        "sink": sink_name,
        "actions": actions,
        "restored": restored,
        "error": error,
    })
}

fn build_report(pactl: &Path, exercise_mutation: bool) -> Value {
    let discovery = match discover(pactl) {
        Ok(discovery) => discovery,
        Err(error) => {
            return json!({
                "probe": "audio",
                "phase": "0",
                "read_only": !exercise_mutation,
                "observed_at_unix_ms": now_ms(),
                "transport_error": { "category": error.category, "detail": error.detail },
                "capabilities": [capability(
                    "audio.server",
                    if error.category == "missing_command" { "MissingDependency" } else { "Unsupported" },
                    "PulseAudio-compatible audio server discovery failed.",
                    Some("Install or start PipeWire/PulseAudio and ensure pactl can reach the user-session socket."),
                    json!({ "error_category": error.category }),
                )],
            });
        }
    };

    let mutation_eligible = discovery
        .default_sink
        .as_ref()
        .is_some_and(|sink| discovery.sink_inputs.is_empty() && uniform_raw_volume(sink).is_some());
    let capabilities = vec![
        capability(
            "audio.server",
            "Supported",
            "A local PulseAudio-compatible server is reachable.",
            None,
            discovery.server.clone(),
        ),
        capability(
            "audio.outputs",
            if discovery.sinks.is_empty() {
                "Unsupported"
            } else {
                "Supported"
            },
            "Output-device discovery through the PulseAudio-compatible server.",
            discovery
                .sinks
                .is_empty()
                .then_some("Connect an output device or start the audio server."),
            json!({
                "sink_count": discovery.sinks.len(),
                "default_sink_name": field(&discovery.server, "default_sink_name"),
            }),
        ),
        capability(
            "audio.streams",
            if discovery.sink_inputs.is_empty() {
                "Limited"
            } else {
                "Supported"
            },
            "Per-application stream discovery through sink-inputs.",
            discovery
                .sink_inputs
                .is_empty()
                .then_some("Start playback in an application to validate per-stream controls."),
            json!({ "active_sink_input_count": discovery.sink_inputs.len() }),
        ),
        capability(
            "audio.output-mutation",
            if mutation_eligible { "Supported" } else { "Limited" },
            "Reversible default-sink mute and volume control.",
            (!mutation_eligible).then_some(
                "Stop active playback and use a sink with a single restorable channel volume before exercising mutation.",
            ),
            json!({
                "explicit_mutation_required": true,
                "active_sink_input_count": discovery.sink_inputs.len(),
                "default_sink_available": discovery.default_sink.is_some(),
            }),
        ),
    ];

    let mut report = json!({
        "probe": "audio",
        "phase": "0",
        "read_only": !exercise_mutation,
        "observed_at_unix_ms": now_ms(),
        "transport": {
            "command": pactl.display().to_string(),
            "protocol": "PulseAudio-compatible",
            "phase_0_only": true,
        },
        "server": discovery.server.clone(),
        "sinks": discovery.sinks.iter().map(summarize_sink).collect::<Vec<_>>(),
        "sources": discovery.sources.iter().map(summarize_source).collect::<Vec<_>>(),
        "sink_inputs": discovery.sink_inputs.iter().map(summarize_stream).collect::<Vec<_>>(),
        "capabilities": capabilities,
    });
    if exercise_mutation {
        report
            .as_object_mut()
            .expect("report is an object")
            .insert("mutation".to_string(), mutation_report(pactl, &discovery));
    }
    report
}

fn find_pactl() -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|path| path.join("pactl"))
            .find(|candidate| candidate.is_file())
    })
}

fn arguments() -> (bool, Option<PathBuf>) {
    let mut exercise_mutation = false;
    let mut pactl = None;
    let mut args = env::args_os().skip(1);

    while let Some(argument) = args.next() {
        if argument == "--exercise-mutation" {
            exercise_mutation = true;
        } else if argument == "--pactl" {
            pactl = args.next().map(PathBuf::from);
        } else {
            eprintln!("usage: kestrel-audio-spike [--exercise-mutation] [--pactl PATH]");
            std::process::exit(2);
        }
    }
    (exercise_mutation, pactl.or_else(find_pactl))
}

fn missing_pactl_report(exercise_mutation: bool) -> Value {
    json!({
        "probe": "audio",
        "phase": "0",
        "read_only": !exercise_mutation,
        "observed_at_unix_ms": now_ms(),
        "transport_error": {
            "category": "missing_command",
            "detail": "pactl was not found on PATH",
        },
        "capabilities": [capability(
            "audio.server",
            "MissingDependency",
            "pactl is unavailable.",
            Some("Install PulseAudio utilities or a PipeWire PulseAudio compatibility package."),
            json!({}),
        )],
    })
}

fn main() {
    let (exercise_mutation, pactl) = arguments();
    let report = pactl
        .as_deref()
        .map(|path| build_report(path, exercise_mutation))
        .unwrap_or_else(|| missing_pactl_report(exercise_mutation));
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report must serialize")
    );
}

#[cfg(test)]
mod tests {
    use super::{choose_changed_volume, uniform_raw_volume};
    use serde_json::json;

    #[test]
    fn chooses_a_small_different_volume() {
        assert_eq!(choose_changed_volume(0), 655);
        assert_eq!(choose_changed_volume(65_536), 64_881);
    }

    #[test]
    fn only_accepts_uniform_channel_volume() {
        let equal = json!({
            "volume": {
                "front-left": { "value": 65536 },
                "front-right": { "value": 65536 },
            }
        });
        let unequal = json!({
            "volume": {
                "front-left": { "value": 65536 },
                "front-right": { "value": 32768 },
            }
        });
        assert_eq!(uniform_raw_volume(&equal), Some(65536));
        assert_eq!(uniform_raw_volume(&unequal), None);
    }
}
