---
layout: default
title: Kestrel
---

# Kestrel

![The Overview page: the sidebar of pages, one-click tools, and quick toggles such as Keep awake and Desktop dark mode](images/hero.png)

**The utilities a COSMIC, Sway, or Hyprland desktop leaves out: clipboard
history that pastes back into the application you came from, screenshots with
an editor and OCR, a floating shelf, a command bar, audio routing, and quiet
status readouts. One window, running beside your compositor without replacing
it.**

## Why Kestrel on your desktop

**COSMIC.** COSMIC ships a compositor and its own settings, without a
per-application audio mixer, clipboard history that pastes back, or a screenshot
editor with OCR. Those gaps are covered here; the window follows COSMIC's light
and dark mode, and the floating surfaces sit alongside COSMIC's own panels.

**Sway.** A keyboard-first tiling compositor leaves the mixer, clipboard
manager, and screenshot editor to you. Bind `kestrel --command <id>` to open any
tool; captures go through `grim` and `slurp` on wlroots, and the command bar and
status overlay are layer-shell surfaces inside your layout.

**Hyprland.** The same gaps, with extra integration: shortcuts register through
the GlobalShortcuts portal where Hyprland exports it, and the floating shelf,
command bar, and status overlay sit above tiled windows without taking a tile.

## What you get

- **Clipboard history:** opt-in, memory-only history with search; Shift+Return pastes an entry into the previous application. Pinned text can be kept across restarts, encrypted with a key in your desktop keyring.
- **Screenshots and screen tools:** capture a screen, window, or area, then crop, draw, redact, or copy its text with on-computer OCR; also read QR codes and pick colours.
- **Shelf:** a floating shelf you can pin open, drag files, text, and links into, and drag back out to another application.
- **Command bar:** a floating search over the app's tools, features, and your scripts, with answers for arithmetic, units, dates, and colours.
- **Audio mixer:** per-application volume, output and input priority, and a port picker on outputs with more than one.
- **Status overlay:** a small pill that appears only while something is active — recording, alerts, volume, microphone, or the Keep awake countdown.
- **Keep awake:** hold off idle and sleep for 15 minutes up to 24 hours, with the time left on Overview and in the tray.
- **Clean links:** see each tracking parameter removed from a link before it is copied, or clean a single copied link automatically.

## Screenshots

![Clipboard history with search and paste](images/clipboard.png)

![The screenshot editor with crop, annotations, and redaction](images/capture.png)

![The floating shelf holding files, text, and links](images/shelf.png)

![The command bar searching the app's tools](images/command-bar.png)

![The status overlay showing the Keep awake countdown](images/overlay.png)

## Install

Set `VERSION` to the latest preview on
[GitHub Releases](https://github.com/kamleshkc2002/kestrel/releases), then:

```bash
VERSION="0.9.0-preview.1"
FILE="Kestrel-$VERSION-x86_64.AppImage"
URL="https://github.com/kamleshkc2002/kestrel/releases/download/v$VERSION"
curl -LO "$URL/$FILE"
curl -LO "$URL/$FILE.sha256"
sha256sum -c "$FILE.sha256"
chmod +x "$FILE"
./"$FILE"
```

- **Requires:** a GTK4 desktop session. Screen recording is the only feature that uses GStreamer, which the AppImage does not bundle; without it, recording says which packages to install and everything else works.
- **Arch Linux:** the `kestrel-desktop` AUR package is not published yet; until it is, build [packaging/aur](https://github.com/kamleshkc2002/kestrel/tree/main/packaging/aur) with `makepkg`.
- If the AppImage does not mount, run it with `--appimage-extract-and-run`. No root is needed.
- **Flatpak:** each release from 0.9 on attaches a restricted `.flatpak` bundle; some features are unavailable in the sandbox and say why ([details](features.md#flatpak)). It is not on Flathub yet.
- To build from source, see [Development](development.md).

## Where it runs

- **Tested:** Pop!_OS 24.04 with COSMIC on Wayland, and the AppImage in CI on Ubuntu 22.04 under Xvfb and headless Sway.
- **Should run:** glibc 2.35 or newer: Ubuntu 22.04+, Linux Mint 21+, Debian 12+, Fedora 38+, and Arch (untested).
- **Won't start:** systems with glibc older than 2.35, such as Ubuntu 20.04 and Debian 11.

Which features work on your desktop is in the
[compatibility table](compatibility.md); known limits are in
[Features](features.md#known-limits).

## Privacy

Local-first. There is no telemetry, account, or crash reporting, and the update
check is opt-in; nothing is uploaded. Clipboard history stays in memory and is
wiped when you lock the session, sleep, or quit; only text pins you choose to
keep are written to disk, encrypted. Settings and data live in your
XDG folders, and Settings → Remove Kestrel's data… lists every file and removes
only what you choose. Full detail is in [Privacy](privacy.md).

## Documentation

[Features](features.md) · [Compatibility](compatibility.md) ·
[Privacy](privacy.md) · [Configuration](configuration.md) ·
[Stability](stability.md) · [Development](development.md) ·
[Requirements](REQUIREMENTS.md) · [Architecture](ARCHITECTURE.md)

Source, issues, and releases are on
[GitHub](https://github.com/kamleshkc2002/kestrel).

## License

MIT. See [`LICENSE`](https://github.com/kamleshkc2002/kestrel/blob/main/LICENSE).
