# Kestrel

Kestrel is a local-first, capability-aware utility host for Linux desktops. It
combines monitoring, audio, clipboard, capture, and automation workflows in one
application alongside an existing desktop. The core is Wayland-first with X11
compatibility; each integration reports its actual session, dependency,
hardware, and permission capabilities.

## Status

Phase 0 capability validation is complete. Phase 1 provides a production
workspace, versioned non-sensitive configuration, and a GTK/libadwaita
capability window; optional StatusNotifierItem tray actions are available when
a compatible host exists.

- [Requirements and support contract](docs/REQUIREMENTS.md)
- [Initial architecture](docs/ARCHITECTURE.md)

## Principles

- Local-first, with no telemetry or accounts.
- Runs entirely as the unprivileged session user.
- Every feature reports supported, limited, permission-gated, dependency-gated, or unsupported status and remediation.
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

Essentials, Balanced, and Everything presets change feature enablement in one
reversible operation. The Feature Hub reports registered, enabled, available,
and running states separately.

The window lists its pages in a sidebar (Overview, Monitoring, Audio, Clipboard,
Command bar, Capture, Notes, Processes, Shortcuts, Feature hub, and Settings)
and shows one at a time. Each feature's settings sit on its own page, and the
Settings page keeps app-wide choices. The sidebar search filters pages and
settings together. On narrow windows the sidebar and the page take turns.

## Features

### System monitoring — `system.monitor`

- **Default:** disabled; Essentials enables it.
- **Configuration:** `[monitoring] refresh_interval_millis` (250–60,000; default 1,000), `history_samples` (1–600; default 120), `readouts`, `network_rate_unit` (`bytes` or `bits`; default `bytes`), `per_core_cpu` (default false), `tray_readouts` (up to 3; default none), and `[monitoring.alerts]` rules (`threshold` >0 and ≤100% or ≤150 °C, `sustain_samples` 1–600, `cooldown_seconds` ≤86,400).
- Samples `/proc` and `/sys`; missing backends stay visible with their source reason. History is bounded and kept in memory; process IDs and command lines are excluded.
- Per-core CPU load is grouped into performance and efficiency cores on hybrid processors. Network shows the transfer since monitoring started, and the battery shows its power source, including a paused charge on external power.
- NVIDIA GPUs are read through NVML (`libnvidia-ml.so.1`) when the driver is loaded; a missing library or a driver/library mismatch is shown with its remedy. Accessory batteries (mice, keyboards, headsets, controllers) come from UPower; no serial numbers are read.
- Local addresses appear only after **Show** in the Monitoring panel and are never stored, logged, or exported.

### Audio mixer — `audio.mixer`

- **Default:** disabled; Essentials enables it.
- **Configuration:** `[audio] boost_percent` (100–150; default 130), `output_switch` (`default_output` or `all_streams`), `disconnect_policy` (`preserve_volume` or `reset_volume`), `disconnect_volume_percent` (0–100; default 100), `include_inactive_streams` (default false), `output_priority` (up to 16 device names), `follow_output_priority` (default false), and `mixer_favorites` (up to 32 application identities). The ranking and pins are edited in the panel.
- Amplification is bounded by the configured ceiling and a 150% hard cap; process IDs and the Pulse authentication cookie stay private.
- Output priority: while followed, the highest-ranked connected output becomes the default when outputs connect or disconnect, or when the ranking changes. A manual choice stands until then. Disconnected devices keep their rank. Only server device names are stored.
- Pinned applications are listed first in their pinned order, keyed by `application.id`, else the process binary, else the application name. Unpinned streams keep the server's order, and a pin applies again when the application returns.

### Microphone — `audio.microphone`

- **Default:** disabled; Essentials enables it.
- **Configuration:** `[audio] input_priority` (up to 16 device names) and `follow_input_priority` (default false), following the output ranking's rules.
- Lists capture inputs (sink monitors excluded); mute state and the default input's level (0–150%) are re-read from the server, and capability evidence carries counts only.
- Recording applications lists each capture stream with its level and mute; a stream reading a sink monitor is labelled as recording system audio. Only the application name and identity are kept.
- Mute toggle: the window switch, `Ctrl+Shift+M`, the command bar, or the tray menu.

### Network speed test — `network.speed_test`

- **Default:** disabled; runs only when you start it.
- **Configuration:** `[speed_test] download_megabytes` (1–90; default 25), `upload_megabytes` (0–25; default 10; 0 skips upload), `timeout_seconds` (5–120; default 30).
- Shows the Cloudflare host (which sees your IP), transfer limits, and phase times before sending data; uses HTTPS-only `curl` launched directly, ignores `~/.curlrc`, and supports cancellation.

### Text snippets — `snippets.text`

- **Default:** disabled.
- **Configuration:** `[snippets] max_content_bytes` (1 B–1 MiB; default 64 KiB), `clipboard_variable_bytes` (64 B–64 KiB; default 4 KiB), `insert_timeout_millis` (250–10,000; default 2,000), `preferred_provider` (`auto`, `wtype`, `ydotool`, or `xdotool`; default `auto`), and `expansion_timing` (`manual` or `delimiter`; default `manual`).
- Snippets are stored atomically in a mode-0600 file in a mode-0700 directory; files keep the literal `{{...}}` variables.
- Alt+Up and Alt+Down move the current line or selected lines in the snippet editor; Ctrl+Z undoes a move.

### Command bar — `commands.bar`

- **Default:** disabled.
- **Configuration:** `[command_bar] max_results` (5–50; default 12), `enable_applications` (default true), `enable_files` (default false), `enable_scripts` (default true), `enable_emoji` (default true), `enable_windows` (default true), `enable_clipboard` (default true), up to 8 absolute `file_roots`, and scripts with 250 ms–60 s (default 5 s) timeouts and 1 KiB–1 MiB (default 64 KiB) output bounds.
- Search is local and bounded to configured sources; learned ranking stores command IDs, counts, and pins, never queries, window titles, or clipboard entries.
- `kestrel --command command-bar.open` opens the bar as a floating surface without raising the main window, with the search field already focused. Up/Down or Ctrl+N/Ctrl+P select, Return runs the selection (or the first result) and shows the outcome in the bar, and Escape closes it and returns focus to the window you were in. While the feature is off, the bar shows how to turn it on.
- Results cover Kestrel actions, feature switches (searching a feature lists its switch first), snippets, applications and their desktop actions, open windows, clipboard history entries while clipboard history runs, scripts, files, and emoji. Running an application brings its window forward, hides it when it is already in front (where the window backend can minimize), and launches it otherwise.
- Open windows come from COSMIC toplevel management, wlroots foreign-toplevel management (Sway, Hyprland, labwc), the generic Wayland toplevel list (listing only), or EWMH on X11. GNOME exposes no window list to applications, so window results stay off there and the provider row says why.
- Computed answers: arithmetic (numbers may group thousands with a space or an apostrophe and use a comma or period decimal), unit conversion, dates and times, `feb 29` or `days until dec 25`, and colors in `#rrggbb`, `rgb()`, or `hsl()` notation with a swatch.
- The bar is a wlr-layer-shell overlay where the compositor offers the protocol and Kestrel was built with gtk4-layer-shell (the AppImage bundles it); elsewhere it is a normal undecorated window. The command bar panel shows which placement is in use and why.

### Quick panel

- **Configuration:** `[quick_panel] favorites`, up to 24 command IDs from `kestrel --list-commands`, also editable on the Command bar page.
- `kestrel --command quick-panel.open` opens a floating palette of your favorites in saved order. Tools whose feature is off or unavailable stay listed, greyed out, with the reason. Return runs the selected tool and closes the panel; Escape closes it.

### Clipboard history — `clipboard.history`

- **Default:** disabled and requires explicit opt-in.
- **Configuration:** `[clipboard] max_items` (1–1,000; default 100), `max_item_bytes` (1 KiB–16 MiB; default 1 MiB), `max_image_bytes` (1 KiB–64 MiB; default 4 MiB), `max_file_entries` (1–1,024; default 64), `max_total_bytes` (1 KiB–128 MiB; default 16 MiB), `max_age_hours` (1–720; default 24), `clear_seconds` (0–86,400; default 0), `filter_sensitive` (default false), `paste_plain_text` (default true), and `history_layout` (`list` or `cards`; default `list`), also editable on the Clipboard page.
- History is memory-only and bounded; wipe, lock, sleep, shutdown, and service stop drop retained buffers, and diagnostics carry metadata only.
- Copying an entry again moves it to the top; pinned entries keep their place. Wipe history and Clear unpinned ask first and state how many entries go; anything copied while the prompt is open is kept.
- Search highlights matches. Ctrl+N and Ctrl+P move through results and Return copies the selected entry, or the first one when none is selected.
- Previews lay JSON out one value per line. Image previews offer Copy text, which reads the image with the local `tesseract` executable; the text goes to the clipboard only and is neither retained nor logged. Without Tesseract the preview says what to install.
- Cards name the source app only when the clipboard backend reports a reliable owner; none does today, so cards say "Source app unknown".

### Screenshots — `capture.screenshot`

- **Default:** disabled; Balanced and Everything enable it. Nothing is captured until you press a capture button or run `kestrel --command capture.screenshot` / `capture.screen`.
- **Providers:** the Screenshot portal where the desktop exports it (GNOME, KDE Plasma, COSMIC): "Choose in dialog" opens the desktop's own capture UI, and "Screen" asks once for screenshot permission; `grim` with `slurp` for area selection on wlroots compositors such as Sway and Hyprland; core X11 for the focused window or the whole screen on real X11 sessions.
- **Configuration:** `[capture] max_entries` (1–200; default 30), `max_total_megabytes` (1–2,048; default 256), `max_age_hours` (1–720; default 168), also editable on the Capture page.
- Recent captures are PNG files in `$XDG_DATA_HOME/kestrel/captures` (folder 0700, files 0600); the oldest go first when a limit is reached, and Clear all removes every file at once. Temporary files a portal leaves behind are deleted after import.
- Each capture can be copied as an image, saved elsewhere, deleted, or edited. The editor crops, draws boxes, arrows, and highlights, and redacts regions; redaction overwrites the pixels with black in the saved copy, and the original capture stays until you delete it.

### Screen tools — `capture.text`, `capture.qr`, `capture.color`

- **Default:** disabled; Balanced and Everything enable all three. Each tool is its own feature and fails on its own: reading text needs Tesseract (`sudo apt install tesseract-ocr tesseract-ocr-eng`), and all three need a screenshot provider (see Screenshots).
- **Use:** the Screen tools buttons on the Capture page, or `kestrel --command capture.text` / `capture.qr` / `capture.color`. Kestrel takes one frame through the screenshot provider and opens a selection view with a magnifier: drag an area to read (or read the whole frame), or click a pixel for its color. Arrow keys move the cursor and Enter confirms. Where the Screenshot portal offers its color picker, colors come from the desktop's picker.
- **Configuration:** `[screen_tools] ocr_language` (Tesseract codes such as `eng` or `eng+deu`; default `eng`) and `ocr_timeout_seconds` (5–120; default 30), also editable on the Capture page.
- Everything runs on this computer: text through the `tesseract` executable, QR codes in-process. The frame stays in memory only while you select and is dropped once reading starts or you cancel; results stay until cleared and are never saved. Text and QR results copy to the clipboard, web addresses in QR codes open in your browser, and colors copy as HEX, RGB, or HSL.

### Screen recording — `capture.recording`

- **Default:** disabled; Balanced and Everything enable it. Nothing is shared until you press Screen, Window, or Area; the desktop's ScreenCast dialog asks every time, and Kestrel never asks it to remember the choice.
- **Needs:** a ScreenCast portal backend (GNOME, KDE Plasma, COSMIC, wlroots), PipeWire, and GStreamer with its PipeWire, Base, and Good plugins (`sudo apt install gstreamer1.0-pipewire gstreamer1.0-plugins-base gstreamer1.0-plugins-good`). The feature names whichever piece is missing.
- **Sources:** a screen or a window as the desktop offers; Area shares a screen, then shows its current frame so you can drag the region to keep.
- **Audio:** System audio (the default output's monitor) and Microphone (the default input) are separate opt-in switches, each recorded as its own Opus track. A track that fails or disconnects is marked in the panel and the rest of the recording continues.
- **Configuration:** `[recording] framerate` (10–60; default 30), `cursor` (default true), `system_audio`, `microphone` (default false), also editable in the panel.
- Recordings are VP8 WebM files in `$XDG_VIDEOS_DIR/Kestrel` (usually `~/Videos/Kestrel`), written as a hidden `.partial` file (0600) and renamed only once complete. Discard, failures, and leftovers from a crash remove the partial file; earlier recordings are never touched. Pause leaves the paused time out of the file.
- Stop, disabling the feature, and quitting save the take; the desktop ending the share saves what was recorded and says so. Recording stops and saves when less than 512 MiB of disk space remains.
- **Trim:** each recent recording has Trim, which opens its timeline with Start and End handles and a preview of the frame at either point. Export writes the kept range, re-encoded as VP8/Opus WebM with every track, to a file you choose, through a hidden partial file renamed once complete. Cancelling or a failed export removes that file; the recording itself is never changed. Disabling the feature or quitting cancels a running export.

### Scratchpad — `notes.scratchpad`

- **Default:** disabled; Balanced and Everything enable it. Open it with `kestrel --command scratchpad.open`.
- **Configuration:** `[scratchpad] max_notes` (1–50; default 20), `max_note_bytes` (1 KiB–1 MiB; default 256 KiB), and `max_age_days` (0–365; default 0, never), also editable on the Notes page.
- Tabbed notes autosave shortly after you stop typing to `$XDG_DATA_HOME/kestrel/scratchpad.json` (folder 0700, file 0600, replaced atomically). A failed save shows a warning in the panel; an unreadable file is kept aside as `scratchpad.json.corrupt-<time>`.
- Alt+Up and Alt+Down move the current line or selected lines; Ctrl+Z undoes a move and restores the selection.
- Preview renders the note's Markdown. Export writes the selected note to a file you choose. Notes past the age limit are listed and removed only after you confirm. Note text never enters the configuration file or its export.

### Camera preview — `camera.preview`

- **Default:** disabled; Balanced and Everything enable it. The camera stays off until you press Start.
- Lists cameras from sysfs without opening them, then opens the chosen one through V4L2 at 640 × 480 (MJPEG, else YUYV) only while the preview runs. Stop, closing the window, and disabling the feature turn the camera off.
- Distinct states for no camera, no permission (log in at the computer's own seat or join the `video` group), a camera another application holds, and a camera that disconnects mid-preview. Frames stay in memory and are never recorded or saved.

### Processes — `system.processes`

- **Default:** disabled; Balanced and Everything enable it. Open it with `kestrel --command processes.open`.
- **Configuration:** `[processes] refresh_millis` (1,000–10,000; default 2,000) and `max_rows` (rows shown, 10–500; default 100), also editable on the Processes page.
- Rows show the executable, PID, start time in clock ticks since boot, owner, CPU, and resident memory, busiest first; search covers every process by name, executable, owner, or PID. Command lines and environments are never read.
- End (SIGTERM, then a 3 s wait) and Kill (SIGKILL) apply only to your own processes. Kernel threads, other users' processes, PID 1, your systemd manager, and Kestrel itself show the reason instead. Each action asks for confirmation naming the executable, PID, owner, and signal.
- Kestrel opens a pidfd and checks the start time again before every signal, so a PID reused by another process is refused. Without pidfd support (Linux 5.3+), the list still works, End, Kill, Restart, and Stop owner are hidden, and the panel says why.
- Restart appears for systemd user services (`systemctl --user restart`) and apps the desktop launched into their own `app-…scope` (SIGTERM to every process in the scope, then a relaunch from the desktop entry). Force restart sends SIGKILL to anything still running after 5 s. Services in `session.slice` are never restarted.

### Listening ports — `network.listeners`

- **Default:** disabled; Balanced and Everything enable it.
- **Configuration:** no feature-specific keys; the list refreshes every 2 s while enabled and holds at most 1,024 sockets.
- Lists TCP and UDP listeners from `/proc/net` with the owning process when its sockets are visible to you. Each row copies the port, address, or PID. When a browser is installed, TCP listeners on a loopback address open at that address (for example `http://127.0.0.1:8765/`), and listeners on every address open at `http://localhost:<port>/`.
- Stop owner uses the same confirmed End flow; listeners owned by other users show the reason. Addresses stay in the window and are dropped when the feature is disabled.

### Desktop integration

- The normal window is always available; the tray is optional.
- The close button minimizes Kestrel to the taskbar by default, so monitoring, clipboard history, and recordings keep running; launching Kestrel again, the tray, or `window.show` brings the window back. Choose "Quit Kestrel" under Settings → Close button (`[ui] close_action = "quit"`) to make it quit instead. Ctrl+Q, the Quit button beside that setting, and the tray's Quit always quit. Minimizing turns off a running camera preview.
- The StatusNotifierItem tray (open, refresh, toggle microphone mute, quit) registers when a compatible host exists, and the window works either way. KDE Plasma, XFCE, Cinnamon, MATE, Budgie, LXQt, COSMIC, and SNI-capable bars provide one; GNOME needs the AppIndicator extension. Each readout marked **Tray** in Settings (up to three) gets its own tray icon showing its label and current value; clicking it opens Kestrel, and its menu shows the full reading. Hosts that support tooltips also show the reading on hover; COSMIC shows no tray tooltips.
- See [Keyboard shortcuts](#keyboard-shortcuts) for `global.shortcuts` and `kestrel --command`.

### Keyboard shortcuts

`kestrel --command <id>` sends one action to the running instance; if Kestrel is not running it starts, opens its window, and runs the action. Bind it in your desktop's custom-shortcut settings (GNOME Settings → Keyboard → Custom Shortcuts, KDE System Settings → Shortcuts → Add Command, or your compositor config):

```bash
kestrel --command command-bar.open        # floating command bar
kestrel --command quick-panel.open        # floating palette of favorite tools
kestrel --command clipboard.quick-paste   # window with clipboard history search focused
kestrel --command microphone.toggle-mute
kestrel --command audio.output-next
kestrel --command toggle.power.keep-awake
kestrel --list-commands                   # every ID with a description
```

IDs cover the window, command bar, quick panel (`quick-panel.open`), clipboard quick paste, scratchpad (`scratchpad.open`), capability refresh, audio output cycling, microphone mute, speed test, screenshots (`capture.screenshot`, `capture.screen`), screen tools (`capture.text`, `capture.qr`, `capture.color`), the process inspector (`processes.open`), clipboard wipe and clear, presets, and every quick toggle (`toggle.<feature-id>`). The exit status says what happened:

| Status | Meaning |
|---|---|
| 0 | Sent to the running instance |
| 2 | Unknown or malformed ID, or bad arguments |
| 3 | The action's feature is disabled; enable it in the Feature Hub |
| 4 | The action's feature is unavailable on this system; the window shows why |
| 5 | The action's feature is enabled but not running |

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

Tagged versions are published as x86_64 AppImage previews on GitHub Releases.
Ubuntu 24.04 builds bundle GTK4/libadwaita; the matching `.sha256` file verifies
downloads. The AppImage uses the system's GStreamer (present on mainstream
desktops), so screen recording uses the host's plugins.

```bash
bash scripts/build-appimage.sh 0.1.0
```

## Install from source

To use Kestrel day to day from a `main` checkout:

```bash
bash scripts/install-local.sh            # build and install, or update
bash scripts/install-local.sh --restart  # update and restart a running copy
bash scripts/install-local.sh --uninstall
```

The script builds a release binary and installs it to `~/.local/bin/kestrel`, with a desktop entry and icon under `~/.local/share`. Settings and data stay in `~/.config/kestrel` and `~/.local/share/kestrel`.

Debug builds (`cargo run -p kestrel`) run as **Kestrel (Development)** with the ID `io.github.kamleshkc2002.Kestrel.Devel` and separate `kestrel-devel` config and data folders. They run next to the installed copy without sharing settings, captures, or the single-instance bus name. Release builds, including the AppImage, use the regular ID and folders.

## Development

### Prerequisites

The workspace is pinned to Rust 1.98.0 by `rust-toolchain.toml`; rustup selects
Rust, rustfmt, and Clippy. The application requires GTK4, libadwaita, PulseAudio,
D-Bus, PipeWire, and GStreamer development packages.

On Debian/Ubuntu-derived distributions:

```bash
sudo apt update
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libdbus-1-dev libpipewire-0.3-dev libpulse-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
# Screen recording at run time, and the recorder tests:
sudo apt install gstreamer1.0-pipewire gstreamer1.0-plugins-base gstreamer1.0-plugins-good
```

Floating surfaces become layer-shell overlays only when the build finds
gtk4-layer-shell through pkg-config (`gtk4-layer-shell-0`). It is optional and
not packaged on Ubuntu 24.04; build it from source (v1.3.0, Meson, installed to
`/usr/local`) as the CI workflows do. `KESTREL_LAYER_SHELL=required` makes a
missing library a build error and `KESTREL_LAYER_SHELL=off` skips it; a binary
linked against it needs the library at run time.

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

With gtk4-layer-shell installed and Sway available, check that the command bar
becomes an overlay (`SWAY_FLAGS=--unsupported-gpu` on NVIDIA drivers):

```bash
bash scripts/layer-shell-smoke.sh target/debug/kestrel
```

## License

MIT. See [`LICENSE`](LICENSE).
