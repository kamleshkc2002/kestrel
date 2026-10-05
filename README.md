# Kestrel

Kestrel is a local-first, capability-aware utility host for Linux desktops. It
combines monitoring, audio, clipboard, capture, and automation workflows in a
single application alongside an existing desktop environment. The core is
Wayland-first with X11 compatibility, and each integration reports its actual
session, dependency, hardware, and permission capabilities.

## Status

Phase 0 capability validation is complete. Phase 1 has a production workspace,
versioned non-sensitive configuration, and a GTK/libadwaita capability window;
optional StatusNotifierItem tray actions are available when a compatible host
exists.

- [Requirements and support contract](docs/REQUIREMENTS.md)
- [Initial architecture](docs/ARCHITECTURE.md)

## Principles

- Local-first, with no telemetry or accounts.
- Runs entirely as the unprivileged session user.
- Every feature reports supported, limited, permission-gated, dependency-gated, or unsupported status with remediation.
- Works alongside your existing shell, panel, launcher, and compositor; support claims name the tested desktop and session.

## Workspace

- `apps/kestrel`: GTK/libadwaita application, configuration I/O, and window.
- `crates/kestrel-core`: UI-agnostic feature and capability model.
- `crates/kestrel-platform`: capability probes and OS/session adapters.
- `crates/kestrel-services`: feature registry and worker lifecycles.
- `docs/REQUIREMENTS.md`: product boundary, support, security, packaging, and delivery.
- `docs/ARCHITECTURE.md`: process model, crate boundaries, and capability reporting.

## Configuration

Preferences are stored in `$XDG_CONFIG_HOME/kestrel/config.toml` (or
`~/.config/kestrel/config.toml`). Feature enablement uses stable IDs. Invalid
settings are ignored individually and reported in the window. Export contains
only typed application-owned preferences; it excludes clipboard content,
runtime snapshots, credentials, and resolved executable paths.

Essentials, Balanced, and Everything presets change feature enablement as one
reversible operation. The Feature Hub reports registered, enabled, available,
and running state separately.

## Features

### System monitoring — `system.monitor`

- **Default:** disabled; Essentials enables it.
- **Configuration:** `[monitoring] refresh_interval_millis` (250–60,000; default 1,000), `history_samples` (1–600; default 120), `readouts`, and `[monitoring.alerts]` rules (`threshold` >0 and ≤100% or ≤150 °C, `sustain_samples` 1–600, `cooldown_seconds` ≤86,400).
- Samples `/proc` and `/sys`; missing backends stay visible with their source reason. History is bounded and kept in memory; process IDs and command lines are excluded.

### Audio mixer — `audio.mixer`

- **Default:** disabled; Essentials enables it.
- **Configuration:** `[audio] boost_percent` (100–150; default 130), `output_switch` (`default_output` or `all_streams`), `disconnect_policy` (`preserve_volume` or `reset_volume`), `disconnect_volume_percent` (0–100; default 100), and `include_inactive_streams` (default false).
- Amplification is bounded by the configured ceiling and a 150% hard cap; process IDs and the Pulse authentication cookie stay private.

### Microphone — `audio.microphone`

- **Default:** disabled; Essentials enables it.
- **Configuration:** no feature-specific keys.
- Lists capture inputs (sink monitors excluded); mute state is re-read from the server, and capability evidence carries counts only.
- Mute toggle: the window switch, `Ctrl+Shift+M`, the command bar, or the tray menu.

### Network speed test — `network.speed_test`

- **Default:** disabled; runs only when you start it.
- **Configuration:** `[speed_test] download_megabytes` (1–90; default 25), `upload_megabytes` (0–25; default 10; 0 skips upload), `timeout_seconds` (5–120; default 30).
- Shows the Cloudflare host (which sees your IP), transfer limits, and phase times before sending data; uses HTTPS-only `curl` launched directly, ignores `~/.curlrc`, and supports cancellation.

### Text snippets — `snippets.text`

- **Default:** disabled.
- **Configuration:** `[snippets] max_content_bytes` (1 B–1 MiB; default 64 KiB), `clipboard_variable_bytes` (64 B–64 KiB; default 4 KiB), `insert_timeout_millis` (250–10,000; default 2,000), `preferred_provider` (`auto`, `wtype`, `ydotool`, or `xdotool`; default `auto`), and `expansion_timing` (`manual` or `delimiter`; default `manual`).
- Snippets are stored atomically in a mode-0600 file in a mode-0700 directory; files keep the literal `{{...}}` variables.

### Command bar — `commands.bar`

- **Default:** disabled.
- **Configuration:** `[command_bar] max_results` (5–50; default 12), `enable_applications` (default true), `enable_files` (default false), `enable_scripts` (default true), `enable_emoji` (default true), up to 8 absolute `file_roots`, and scripts with 250 ms–60 s (default 5 s) timeouts and 1 KiB–1 MiB (default 64 KiB) output bounds.
- Search is local and bounded to configured sources; learned ranking stores command IDs, counts, and pins.

### Clipboard history — `clipboard.history`

- **Default:** disabled and requires explicit opt-in.
- **Configuration:** `[clipboard] max_items` (1–1,000; default 100), `max_item_bytes` (1 KiB–16 MiB; default 1 MiB), `max_image_bytes` (1 KiB–64 MiB; default 4 MiB), `max_file_entries` (1–1,024; default 64), `max_total_bytes` (1 KiB–128 MiB; default 16 MiB), `max_age_hours` (1–720; default 24), `clear_seconds` (0–86,400; default 0), `filter_sensitive` (default false), and `paste_plain_text` (default true).
- History is memory-only and bounded; wipe, lock, sleep, shutdown, and service stop drop retained buffers, and diagnostics carry metadata only.

### Desktop integration

- The normal window is always available; the tray is optional.
- The StatusNotifierItem tray (open, refresh, toggle microphone mute, quit) registers when a compatible host exists, and the window works either way. KDE Plasma, XFCE, Cinnamon, MATE, Budgie, LXQt, and SNI-capable bars provide one; GNOME needs the AppIndicator extension.
- See [Keyboard shortcuts](#keyboard-shortcuts) for `global.shortcuts` and `kestrel --command`.

### Keyboard shortcuts

`kestrel --command <id>` sends one action to the running instance; if Kestrel is not running it starts, opens its window, and runs the action. Bind it in your desktop's custom-shortcut settings (GNOME Settings → Keyboard → Custom Shortcuts, KDE System Settings → Shortcuts → Add Command, or your compositor config):

```bash
kestrel --command microphone.toggle-mute
kestrel --command toggle.power.keep-awake
kestrel --list-commands   # every ID with a description
```

IDs cover the window, capability refresh, audio output cycling, microphone mute, speed test, clipboard wipe and clear, presets, and every quick toggle (`toggle.<feature-id>`). Unknown IDs exit with status 2.

#### Global shortcuts — `global.shortcuts`

- **Default:** disabled; enabling it in the Feature Hub registers the configured bindings.
- **Providers:** the `GlobalShortcuts` portal where the desktop exports it (KDE Plasma, GNOME 48+, Hyprland), which may ask you to confirm or change each key; `XGrabKey` on real X11 sessions, which reports a combination another client already holds. Other sessions show the feature as unsupported and point to `kestrel --command`.
- **Configuration:** up to 32 bindings; each `trigger` needs at least one modifier (`CTRL`, `ALT`, `SHIFT`, `LOGO`) plus an XKB key name. Defaults:

```toml
[[shortcuts.bindings]]
command = "window.show"
trigger = "LOGO+ALT+k"

[[shortcuts.bindings]]
command = "microphone.toggle-mute"
trigger = "LOGO+ALT+m"

[[shortcuts.bindings]]
command = "audio.output-next"
trigger = "LOGO+ALT+o"
```

- The window lists each binding with its state (active, waiting, in use, not registered). Disabling the feature releases the portal session or every grab.

## AppImage preview

Tagged versions are published as x86_64 AppImage previews on GitHub Releases;
Ubuntu 24.04 builds bundle GTK4/libadwaita, and the matching `.sha256` file
verifies downloads.

```bash
bash scripts/build-appimage.sh 0.1.0
```

## Development

### Prerequisites

The workspace is pinned to Rust 1.98.0 by `rust-toolchain.toml`; rustup selects
Rust, rustfmt, and Clippy. The application requires GTK4, libadwaita, PulseAudio,
D-Bus, and PipeWire development packages.

On Debian/Ubuntu-derived distributions:

```bash
sudo apt update
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libdbus-1-dev libpipewire-0.3-dev libpulse-dev
```

If Rust is unavailable:

```bash
sudo apt install rustup
rustup toolchain install 1.98.0 --profile minimal \
  --component rustfmt --component clippy
source "$HOME/.cargo/env"
```

Verify the tools:

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
