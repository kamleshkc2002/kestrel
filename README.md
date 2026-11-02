# Kestrel

Kestrel is the set of utilities your Linux desktop leaves out: system
monitoring with sustained alerts and tray readouts, audio routing with device
priority, private clipboard history, snippets, screenshots with OCR, QR and
color picking, screen recording with trim, a camera check, and process and
port tools, all in one window alongside your existing desktop.

Every feature says whether it works on your session, what it needs, and what
to install or allow when it doesn't; everything runs as your normal user, with
no root helpers, accounts, or telemetry.

## What's in it

- [Monitoring and alerts](docs/features.md#monitoring--systemmonitor): CPU, memory, disk, network, temperature, battery, GPU and accessory readings, with sustained alert rules and tray readouts; history stays in memory.
- [Audio mixer](docs/features.md#audio-mixer--audiomixer) and [microphone controls](docs/features.md#microphone-controls--audiomicrophone): per-application volume, output and input priority, mute, and following an output as devices connect.
- [Clipboard history](docs/features.md#clipboard-history--clipboardhistory), [snippets](docs/features.md#snippets--snippetstext) and [scratchpad](docs/features.md#scratchpad--notesscratchpad): memory-only history, snippet insertion through `wtype` or `xdotool`, tabbed notes.
- [Screenshots](docs/features.md#screenshots--capturescreenshot) and [screen tools](docs/features.md#screen-tools--capturetext-captureqr-capturecolor): crop, annotate, redact, OCR, QR codes, colour picking, and pasted images or extra canvas space; recent captures are kept under a limit.
- [Screen recording](docs/features.md#screen-recording--capturerecording): a screen, window or area, with system and microphone audio, saved as VP8 WebM, and trim; the desktop asks each time.
- [Camera preview](docs/features.md#camera-preview--camerapreview): check a webcam before a call, with distinct states for no camera, no permission, and a camera in use; frames stay in memory and the camera stays off until you press Start.
- [Processes](docs/features.md#processes--systemprocesses) and [open ports](docs/features.md#open-ports--networklisteners): find your own processes and end, kill, restart, or stop the owner; see TCP and UDP listeners.
- [Quick toggles](docs/features.md#quick-toggles): desktop dark mode, Keep awake from 15 minutes up to 24 hours, screen brightness, keyboard light, Wi-Fi, Bluetooth, lock screen, eject, Empty Trash, hidden files and desktop icons.
- [Command bar](docs/features.md#command-bar--commandsbar) and [keyboard shortcuts](docs/features.md#keyboard-shortcuts): search the app's actions, with computed answers for arithmetic, units, dates and colours, or bind actions to keys and `kestrel --command`.
- [Status bar overlay](docs/features.md#status-bar-overlay--overlaystatus): recording, alerts, volume and Keep awake in a small pill that appears only while a module has something active.

## Status

In preview: an x86_64 AppImage tagged `x.y.z-preview.N` on GitHub Releases,
with a SHA-256 file. GNOME, KDE Plasma, Hyprland, and X11 are supported by
design with little real use so far, so reports are welcome.

**Where it runs**

- **Tested:** Pop!_OS 24.04 with COSMIC on Wayland, and Ubuntu 24.04 in CI under Xvfb.
- **Should run:** glibc 2.39+ distributions such as Fedora 40+, Debian 13, and Arch (untested).
- **Won't start:** Ubuntu 22.04, Debian 12, and older.

Known limits are listed in [docs/features.md](docs/features.md#known-limits).

## Install

Download the AppImage and its `.sha256` file from
[GitHub Releases](https://github.com/kamleshkc2002/kestrel/releases), then:

```bash
curl -LO https://github.com/kamleshkc2002/kestrel/releases/download/<tag>/Kestrel-<version>-x86_64.AppImage
curl -LO https://github.com/kamleshkc2002/kestrel/releases/download/<tag>/Kestrel-<version>-x86_64.AppImage.sha256
sha256sum -c Kestrel-<version>-x86_64.AppImage.sha256
chmod +x Kestrel-<version>-x86_64.AppImage
./Kestrel-<version>-x86_64.AppImage
```

If it does not mount, run it with `--appimage-extract-and-run`. No root is
needed. Screen recording uses your system's GStreamer with its PipeWire, Base,
and Good plugins. To build from source, see [docs/development.md](docs/development.md).

## Privacy

- No telemetry or accounts, and no update check or crash reporting.
- Nothing contacts a network host without your action, except the sound-server connection, which goes where libpulse is configured to connect.
- Clipboard history is memory only; locking the session, sleeping, or quitting wipes it.
- Settings and data stay in your XDG folders, under a `kestrel` folder.

Full detail is in [docs/privacy.md](docs/privacy.md).

## Feedback

Feedback on the preview is welcome in
[issue #331](https://github.com/kamleshkc2002/kestrel/issues/331); for a bug,
use the [bug report form](https://github.com/kamleshkc2002/kestrel/issues/new?template=bug_report.yml).
Settings → Diagnostic logs → Report a problem opens the form with your
version, session, and distribution filled in; include the feature's status
line and logs.

## Documentation

[Features](docs/features.md) · [Privacy](docs/privacy.md) ·
[Configuration](docs/configuration.md) · [Development](docs/development.md) ·
[Requirements](docs/REQUIREMENTS.md) · [Architecture](docs/ARCHITECTURE.md)

## License

MIT. See [`LICENSE`](LICENSE).
