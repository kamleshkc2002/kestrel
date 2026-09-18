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
per-feature opt-in and a successful capability probe. Retained UTF-8 text is
memory-only and bounded to 100 items, 1 MiB per item, and 24 hours. Wipe,
lock, sleep, shutdown, and service stop zero and drop retained buffers; Kestrel
clears the live selection only when it still matches content Kestrel owns.

Wayland uses the data-control protocol when available, with X11 `CLIPBOARD` as
the compatibility path. No external clipboard-manager command is required.
Generic source-application exclusions are unavailable because these clipboard
interfaces do not provide verifiable source-application identity.

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
