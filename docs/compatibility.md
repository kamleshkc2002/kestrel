# Compatibility

Capability probes in the running app decide whether a feature works in a session. This table records the evidence behind that decision: what the project has tested, what people have reported, and where a probe reports a feature unsupported. A cell that says Untested can still work on your desktop.

The desktop columns assume a Wayland session. The X11 column is a session type, not a desktop.

Keyboard shortcuts depend on the session: a real X11 session uses key grabs, and a Wayland session needs a GlobalShortcuts portal from the installed portal backend, so their cells stay Untested until a run records the result.

## System baseline

The AppImage bundles GTK 4.14, libadwaita 1.5, and the rest of the desktop stack from source, so a desktop does not need its own GTK version. That leaves glibc: the artifact needs glibc 2.35 or newer, which covers Ubuntu 22.04+, Linux Mint 21+, Debian 12+, Fedora 38+ (glibc 2.37), and Arch. Older systems such as Ubuntu 20.04 and Debian 11 cannot start it.

CI builds the AppImage and runs both startup smoke tests plus the layer-shell check in an Ubuntu 22.04 container, so a build cannot quietly raise that floor. The workflow also inspects every bundled ELF object with `objdump -T` and fails if any file needs a GLIBC_ version above 2.35.

## Cell values

- **Tested** — observed by the project on that desktop, with the version and date. A CI or QA-script run names the run instead.
- **Reported** — a person reported it working, named in the cell or linked to the report.
- **Untested** — no evidence either way.
- **Unsupported** — the capability probe reports the feature unavailable on that desktop, with the probe's reason, such as no wlr-layer-shell or no data-control.

The Tested cells come from CI: the clipboard clean-session run, which uses a private Xvfb session and headless Sway, and the layer-shell smoke check on headless Sway. CI installs Xvfb and Sway in the environment where it runs those checks, so those cells name the session and CI instead of a version. The Reported cells come from the maintainer's day-to-day use of Pop!_OS 24.04 with COSMIC in November 2026, and from a COSMIC tester report about output port switching in [issue #331](https://github.com/kamleshkc2002/kestrel/issues/331).

## How to report

Use the [compatibility report form](https://github.com/kamleshkc2002/kestrel/issues/new?template=compatibility_report.yml) to say whether one feature works on your desktop. For a defect, use the [bug report form](https://github.com/kamleshkc2002/kestrel/issues/new?template=bug_report.yml). General preview feedback goes to [issue #331](https://github.com/kamleshkc2002/kestrel/issues/331).

Include the desktop and its version, the session type, the feature ID, and the status line the Feature Hub shows for that feature. Those details are what updates this table.

## Window and desktop integration

| Feature | COSMIC | Sway | Hyprland | KDE Plasma | GNOME | X11 |
|---|---|---|---|---|---|---|
| Window and close button (`app.command-surface`) | Reported (maintainer, Pop!_OS 24.04 COSMIC) | Untested | Untested | Untested | Untested | Untested |
| Tray icon (`app.status-notifier`) | Untested | Untested | Untested | Untested | Unsupported (no StatusNotifierItem tray host without the AppIndicator extension) | Untested |
| Command bar (`commands.bar`) | Untested | Tested (headless Sway, CI) | Untested | Untested | Untested | Untested |
| Status bar overlay (`overlay.status`) | Untested | Tested (headless Sway, CI) | Untested | Untested | Unsupported (no wlr-layer-shell) | Unsupported (layer-shell overlays need a Wayland session) |
| Keyboard shortcuts (`global.shortcuts`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Quick panel | Untested | Untested | Untested | Untested | Untested | Untested |

## System and audio

| Feature | COSMIC | Sway | Hyprland | KDE Plasma | GNOME | X11 |
|---|---|---|---|---|---|---|
| Monitoring (`system.monitor`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Audio mixer (`audio.mixer`) | Reported (maintainer, Pop!_OS 24.04 COSMIC; [#331](https://github.com/kamleshkc2002/kestrel/issues/331)) | Untested | Untested | Untested | Untested | Untested |
| Microphone controls (`audio.microphone`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Speed test (`network.speed_test`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Processes (`system.processes`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Open ports (`network.listeners`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Camera preview (`camera.preview`) | Untested | Untested | Untested | Untested | Untested | Untested |

## Capture

| Feature | COSMIC | Sway | Hyprland | KDE Plasma | GNOME | X11 |
|---|---|---|---|---|---|---|
| Screenshots (`capture.screenshot`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Read text (`capture.text`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Read QR code (`capture.qr`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Pick color (`capture.color`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Screen recording (`capture.recording`) | Untested | Untested | Untested | Untested | Untested | Untested |

## Clipboard, text, and files

| Feature | COSMIC | Sway | Hyprland | KDE Plasma | GNOME | X11 |
|---|---|---|---|---|---|---|
| Clipboard history (`clipboard.history`) | Untested | Tested (headless Sway, CI) | Untested | Untested | Unsupported (no wlr-data-control, not an X11 session) | Tested (Xvfb, CI) |
| Clean links (Clipboard page) | Untested | Untested | Untested | Untested | Untested | Untested |
| Snippets (`snippets.text`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Scratchpad (`notes.scratchpad`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Shelf (`files.shelf`) | Untested | Untested | Untested | Untested | Untested | Untested |

## Quick toggles

| Feature | COSMIC | Sway | Hyprland | KDE Plasma | GNOME | X11 |
|---|---|---|---|---|---|---|
| Keep awake (`power.keep-awake`) | Reported (maintainer, Pop!_OS 24.04 COSMIC) | Untested | Untested | Untested | Untested | Untested |
| Desktop dark mode (`desktop.appearance`) | Reported (maintainer, Pop!_OS 24.04 COSMIC) | Untested | Untested | Untested | Untested | Untested |
| Screen brightness (`display.brightness`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Keyboard light (`keyboard.backlight`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Bluetooth (`radio.bluetooth`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Wi-Fi (`radio.wifi`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Empty Trash (`files.empty-trash`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Eject drives (`storage.eject`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Show hidden files (`files.show-hidden`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Desktop icons (`desktop.icons`) | Untested | Untested | Untested | Untested | Untested | Untested |
| Lock screen (`session.lock`) | Untested | Untested | Untested | Untested | Untested | Untested |

## App-wide

| Feature | COSMIC | Sway | Hyprland | KDE Plasma | GNOME | X11 |
|---|---|---|---|---|---|---|
| Update check and install | Reported (maintainer, Pop!_OS 24.04 COSMIC) | Untested | Untested | Untested | Untested | Untested |
| What's new | Untested | Untested | Untested | Untested | Untested | Untested |
| Diagnostic logs | Untested | Untested | Untested | Untested | Untested | Untested |
| Launcher script commands | Untested | Untested | Untested | Untested | Untested | Untested |
