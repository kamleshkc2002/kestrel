# Kestrel

Kestrel is a local-first, capability-aware utility host for Linux desktops. It
is designed to bring monitoring, audio, clipboard, capture, and automation
workflows into one human-facing application while working alongside an existing
desktop environment.

Kestrel targets a Wayland-first core with X11 compatibility. Its integrations
are capability-driven: each feature reports whether the current desktop,
compositor, portal, service, dependency, hardware, and permission state can
support it.

## Status

Phase 0 capability validation is complete. Phase 1 has a production workspace,
versioned non-sensitive configuration, and a responsive GTK/libadwaita capability
window. Optional StatusNotifierItem integration adds tray activation and actions
when the desktop provides a compatible host; the normal window remains independent.

- [Requirements and support contract](docs/REQUIREMENTS.md)
- [Initial architecture](docs/ARCHITECTURE.md)

## Scope

Kestrel aims to provide:

- a unified command and status surface for local desktop utility workflows;
- transparent feature availability, limitations, and remediation steps;
- progressive enhancement through portals, user-session services, and narrow
  desktop or compositor adapters;
- local-first operation without required accounts or telemetry.

Kestrel is not a replacement desktop shell, panel, dock, launcher,
notification daemon, or compositor configuration. It also does not promise
uniform feature support across every Linux desktop; release claims will name
the tested desktop, session capability, and feature scope.

## Workspace

- `apps/kestrel`: GTK/libadwaita composition root, XDG configuration I/O, and
  normal command-surface window.
- `crates/kestrel-core`: UI-agnostic feature and capability model.
- `crates/kestrel-platform`: capability probes and narrow OS/session adapters.
- `crates/kestrel-services`: feature registry and bounded worker lifecycles.
- `docs/REQUIREMENTS.md`: product boundary, support contract, security, packaging,
  and delivery requirements.
- `docs/ARCHITECTURE.md`: initial process model, crate boundaries, capability
  reporting, and Phase 0 validation strategy.

## Principles

- Local-first, without telemetry or required accounts.
- User-session operation without a root daemon.
- Feature modules that report supported, limited, permission-gated, dependency-gated,
  or unsupported status.
- Explicit tested support instead of claiming uniform behavior across Linux desktops.

## Configuration

Kestrel stores only versioned, non-sensitive preferences in
`$XDG_CONFIG_HOME/kestrel/config.toml` (or `~/.config/kestrel/config.toml`).
Per-feature enablement is keyed by stable feature IDs. Invalid feature settings
are ignored individually and reported in the normal window, so they do not
prevent other features or the command surface from starting.

The Feature Hub shows registered, enabled, available, and running state
separately, together with capability remediation and conservative idle,
interaction, and polling costs. Essentials, Balanced, and Everything presets
change feature enablement as one reversible operation without discarding
pre-existing per-feature choices.

Settings search covers both controls and features. Quick Controls, the
Feature Hub, and Monitoring can be hidden or reordered independently;
appearance and XDG autostart are separate preferences. Import validates each
portable setting independently, while export contains only typed
application-owned preferences, never clipboard content, runtime snapshots,
credentials, or resolved executable paths.

### System monitoring

`system.monitor` samples `/proc` and `/sys` at a configurable interval
(default 1 s, bounded to 0.25-60 s). The Monitoring panel section renders the
readouts the user selected and ordered: CPU, memory, swap, disk, network,
temperature, battery, and GPU. Every readout stays visible when its backend is
absent and reports the source reason instead of disappearing.

Sampled values are kept in a bounded in-memory history (default 120 samples,
maximum 600) that drops the oldest entry first and never touches disk. History
feeds the per-readout maximum shown in the window; nothing else is persisted.
Sampling runs on a worker thread and coalesces misses, so a slow sample never
blocks the interface.

Alerts are configured per kind in the `[monitoring.alerts]` table. Each rule
declares a threshold, the number of consecutive samples that must cross it
before the first notification, and a cooldown that rate-limits repeats while
the condition persists. A rule re-arms only after the value recovers past a
hysteresis margin, so a value hovering at the threshold cannot spam
notifications. Rules are evaluated independently: an unreadable temperature
sensor or a missing disk adapter never suppresses CPU, memory, or battery
alerts, and a notification failure is reported per kind in the window. Battery
alerts additionally require the `power.battery-alerts` quick toggle to be on.

Temperature thresholds use degrees Celsius; every other kind uses a percentage
of capacity or usage. GPU readings come from the read-only
`gpu_busy_percent` attribute that AMD and several integrated drivers expose;
NVIDIA GPUs need an NVML adapter that is not registered yet, which is reported
as an explicit unavailable state. Per-process metrics are not collected at all,
so snapshots never contain process command lines or process identifiers.

### Audio mixer

`audio.mixer` is enabled by default and controls the PulseAudio-compatible
server (PipeWire's PulseAudio service or PulseAudio itself) through `libpulse`.
No production path parses `pactl` output. The mixer exposes the master output,
every discovered output device grouped by its owning card, and each playing
stream with its own volume, mute, and routing selector. Outputs can be cycled
without stopping playback.

Amplification is bounded, not silent. `[audio] boost_percent` sets the ceiling
the UI and service may request; it must stay between 100 and the hard cap of
150, and the default is 130. A request above the ceiling is rejected with the
effective maximum, and any server value above the ceiling is still reported
truthfully rather than shown as a clamped 100%. Volume percentages round-trip
exactly at every step.

`[audio] output_switch` decides whether switching the default output leaves
playing streams on their current device (`default_output`) or moves them with
it (`all_streams`). `[audio] disconnect_policy` decides what happens when a
stream's output device disappears: `preserve_volume` keeps whatever volume the
stream had, while `reset_volume` reapplies `disconnect_volume_percent` (0–100).
Device loss is reconciled on the next refresh by re-homing affected streams to
the current default output and re-reading the graph, so stale routing is never
displayed and a vanished device is reported as a repair rather than a failure.
`[audio] include_inactive_streams` lists corked or idle streams next to playing
ones; they stay controllable by identifier while hidden.

### Microphone

`audio.microphone` is disabled by default and is part of the Essentials preset.
It reads the same PulseAudio-compatible server through `libpulse` and lists
capture inputs only; sink monitor sources are never offered as microphones.

The mute switch mutes or unmutes every input, so changing the default input
cannot silently reopen capture. Its state is always the server's reading:
every command re-reads the server before acting and again afterwards, a mixed
reading is reported as "n of m inputs muted" rather than as muted or live, and a
failed read shows the state as unknown instead of the last answer. The running
control re-reads the server every two seconds, so mute changes made by hardware
keys or another mixer appear without a manual refresh. The default-input
selector sets the server default; when the default device disconnects, Kestrel
says so and claims no default until the server names one.

The mute toggle is also available as `Ctrl+Shift+M` in the window, as
**Toggle microphone mute** in the command bar, and from the tray menu.
Capability evidence carries input counts only, never device names. System-wide
global shortcuts remain unavailable until a portable adapter is registered.

### Network speed test

`network.speed_test` is disabled by default and only runs when you press
**Start** (or choose **Run network speed test** in the command bar). The panel
shows, before anything is sent, which host is contacted
(`speed.cloudflare.com`, operated by Cloudflare, which sees your IP address),
how much data each direction may transfer, and how long each phase may take.

A run measures latency (median of three requests on one connection), download,
and upload through the system `curl`, resolved from `PATH` and started without
a shell, with `~/.curlrc` ignored, HTTPS only, and redirects not followed.
Kestrel counts the downloaded bytes itself and stops the transfer at the
configured size; the upload is exactly the configured number of bytes. Cancel
stops the run immediately and ends curl's whole process group, and disabling
the feature cancels a run in progress. Failures are reported by kind (name
resolution, connection, TLS, HTTP status, timeout) without curl's own messages,
which can contain addresses.

```toml
[speed_test]
download_megabytes = 25 # 1–90
upload_megabytes = 10   # 0–25; 0 skips the upload phase
timeout_seconds = 30    # 5–120, applied to each phase
```

### Text snippets

`snippets.text` is disabled by default. Once enabled, the library is searchable
and each snippet is inserted into the focused window through a verified
provider; the library itself works without one.

Snippets are stored in `$XDG_DATA_HOME/kestrel/snippets.toml` (falling back to
`~/.local/share`), created with `0600` in a `0700` directory and written
atomically through a temporary file plus rename. The file holds the literal
`{{...}}` tokens, never a resolved value, so snippet storage cannot retain
clipboard-derived text; the portable configuration export carries only the
library bounds. A malformed entry is skipped with a per-entry warning instead of
dropping the file.

Variables are rendered locally at insert time: `{{date}}`, `{{time}}`,
`{{datetime}}`, `{{timezone}}`, and `{{utc_offset}}` come from the session clock,
and `{{clipboard}}` inserts the live selection clipped to
`[snippets] clipboard_variable_bytes` (64 B–64 KiB, default 4 KiB). Previews
render every variable as a placeholder, so a preview never depends on the clock
or reads the clipboard. An unknown variable, a whitespace trigger, a trigger that
does not start with a delimiter, or a name or trigger that collides with another
snippet is rejected with a specific message.

Insertion needs a verified provider. Automatic selection tries `wtype`
(Wayland virtual-keyboard protocol) and then `xdotool` (X11); `ydotool` is never
selected automatically because it requires input-device access, and it is only
used when `[snippets] preferred_provider = "ydotool"` is set deliberately. When
no provider is verified, the Feature Hub reports the missing dependency with its
remediation and every insertion action stays disabled. Provider runs use a
resolved executable path, no shell, a bounded timeout (`[snippets]
insert_timeout_millis`), discarded output, and structured errors.

Trigger expansion timing is stored (`manual` or `delimiter`) and reported
honestly: delimiter expansion needs a key-capture provider, which this release
does not have, so manual insertion is what inserts text today.

### Command bar

`commands.bar` is disabled by default. When enabled it ranks one keyboard-first
surface over Kestrel commands, snippets, applications, computed values, and
configured scripts. Providers that need no desktop integration — Kestrel
actions, snippets, math, units, dates, links, and emoji — always answer, so a
missing integration never leaves the bar empty.

Ranking is bounded and local: fuzzy matching prefers prefixes, contiguous runs,
and shorter labels; pins and learned use counts decide ties. The learned state is
a private file (mode `600`, written atomically) that maps command identifiers to
counts and pins — it has no field for query text, so nothing a user types is
retained. The window shows those identifiers and counts and can reset them, and
`kestrel:reset-command-ranking` is available as a command.

Computed providers are deterministic and offline: arithmetic is evaluated by a
small built-in parser, units convert inside one family (temperature is affine),
dates render from the local clock, and links are only accepted when they look
like a host. Emoji come from a built-in table.

Search never indexes the filesystem. File results appear only inside the
`[command_bar] file_roots` a user configures, walked with a depth, entry, and
match budget that also skips hidden and symlinked directories; with no roots the
provider stays off. Applications come from the standard XDG application
directories, and launching, opening a link, or opening a file requires a
resolved `xdg-open`.

Script actions are explicit configuration: each entry declares a resolved
executable or a `PATH` name, its arguments, a timeout (250 ms–60 s), and an
output bound (1 KiB–1 MiB). They run without a shell, arguments are passed
verbatim, output is read on capped reader threads, and a timeout, a missing
executable, or truncated output is reported as a structured outcome rather than
a silent truncation. A non-zero exit status is data, not a transport failure.

### Desktop integration

Kestrel always provides a normal application window. Its optional
StatusNotifierItem (SNI) registers only when the user session has a compatible
tray host; registration failure is shown as a capability limitation and never
prevents the window from opening. Tray activation opens the same window, and the
tray menu forwards refresh and quit actions to the application.

KDE Plasma, XFCE, Cinnamon, MATE, Budgie, LXQt, and SNI-capable bars commonly
provide a host. GNOME does not display SNI items by default; it requires an
extension such as **AppIndicator and KStatusNotifierItem Support**. Kestrel does
not treat that extension or any tray icon as its sole entry point.

### Clipboard history

`clipboard.history` is disabled by default and starts only after explicit
per-feature opt-in and a successful capability probe. History is memory-only and
bounded by `[clipboard]`: at most 100 entries (configurable to 1000), 1 MiB per
text entry, 4 MiB per PNG image, 64 file paths per list, 16 MiB in total, and 24
hours of age. Wipe, lock, sleep, shutdown, and service stop zero and drop
retained buffers regardless of those values, and Kestrel releases the live
selection only while it still owns it.

The window lists retained entries with a bounded preview, and supports search,
pins, multi-select deletion, explicit preview with text editing, quick copy, and
paste-as-plain-text. Plain-text copying strips ANSI escape sequences and
trailing whitespace, and file entries copy as their paths. Pins survive age
pruning and eviction but never bypass the count or total-byte bounds.

Entry kinds are per capability. Wayland data-control carries text, PNG images,
and `text/uri-list` file lists; the X11 compatibility path captures text only
and reports that reduction with remediation. Images and file lists are moved as
byte-exact payloads — Kestrel never decodes or re-encodes them — so an image
entry is the original PNG and its dimensions come from the PNG header.

`[clipboard] clear_seconds` clears the live selection a fixed time after Kestrel
takes it, without deleting saved entries; `0` disables it. `[clipboard]
filter_sensitive` skips capturing text that matches a documented pattern set (a
PEM private-key block, an `AKIA` access key, a `Bearer` token, a `password`
assignment, or a mixed-case base64/hex run of 48 or more characters). Those
heuristics produce documented false positives — a 64-character hex digest, a
base64 blob in prose, or a code sample mentioning `password=` — so the filter is
off by default and every skip is counted in the window.

Snapshots and diagnostics never carry clipboard content: rows and previews come
only from an explicit, bounded search or preview request. No external
clipboard-manager command is required. Generic source-application exclusions are
unavailable because these clipboard interfaces do not provide verifiable
source-application identity.

## AppImage preview

Tagged versions are published as x86_64 AppImage previews on the repository's
GitHub Releases page. AppImages are built on Ubuntu 24.04 and bundle the
GTK4/libadwaita runtime; the corresponding `.sha256` file verifies the download.

Build the artifact in a compatible environment with:

```bash
bash scripts/build-appimage.sh 0.1.0
```

## Development

### Prerequisites

Kestrel is a Rust workspace pinned to Rust 1.98.0 by `rust-toolchain.toml`.
Rustup selects the pinned compiler, rustfmt, and Clippy components automatically
inside the repository. The application requires GTK4, libadwaita, and PulseAudio
development packages; future D-Bus and native PipeWire integrations will need
their corresponding Linux development packages.

On Debian/Ubuntu-derived distributions:

```bash
sudo apt update
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libdbus-1-dev libpipewire-0.3-dev libpulse-dev
```


Install Rust if `cargo --version` is unavailable:

```bash
sudo apt install rustup
rustup toolchain install 1.98.0 --profile minimal \
  --component rustfmt --component clippy
```
If `cargo` is unavailable in a shell after installation, load Rustup's
environment before running the commands:

```bash
source "$HOME/.cargo/env"
```

Open a new shell after the Rust setup completes, then verify the toolchain:

```bash
rustc --version
cargo --version
rustfmt --version
cargo clippy --version
```

### Validate the workspace

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace --all-targets
```

## License

MIT. See [`LICENSE`](LICENSE).
