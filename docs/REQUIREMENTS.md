# Kestrel Requirements & Architecture

Status: **Draft — for discussion**
Date: 2026-08-23
Scope: Requirements and architecture for Kestrel, a generic Linux utility suite.

> This document is Kestrel's requirements baseline. It defines the product boundary,
> support contract, architecture, security model, packaging modes, and delivery phases.

---

## 1. Summary

Kestrel is a new Linux-native utility suite. It is not a port of an existing desktop
application: it must be implemented against Linux, freedesktop, Wayland, X11, PipeWire,
and hardware interfaces. No single Linux API covers every desktop capability, so Kestrel
uses a common product layer with capability-driven platform backends.

The reusable requirement is the product model: one local-first utility suite with
independently enabled features, a unified command surface, transparent capability status,
and explicit resource ownership. The implementation must be new and Linux-native.

**Conclusion:** Kestrel is feasible as a new application. A large subset of the feature
set has a clean Linux implementation; the remainder (window snapping, app switcher with
previews, fan control, and some display/input features) is desktop-environment-,
compositor-, or hardware-dependent. Feature support must be reported per capability
rather than summarized as a single portability percentage.

---

## 2. Product boundary

- **Not** a desktop-environment replacement: Kestrel integrates with existing desktops,
  compositors, portals, and package managers.
- **Yes** a Linux-native utility suite: Kestrel provides a coherent local-first
  experience for monitoring, audio, clipboard, capture, automation, and optional
  desktop-specific integrations.
- **Independent identity:** Kestrel has its own name, logo, application ID, visual
  identity, repository, and release artifacts.
### 2.1 Positioning and differentiation

Kestrel is a **human-facing, local-first utility host for Linux desktops**. It gives
users a unified command surface and clear feature status while adapting to the
portals, services, hardware, and compositor APIs available in their session.

The comparison is about product boundaries, not feature-count parity. Kestrel will
not attempt to replace the depth of every specialist tool or the shell owned by a
desktop environment.

| Product category | Primary orientation | Typical desktop boundary | Kestrel distinction |
|---|---|---|---|
| Desktop-native utility suites | Cohesive utilities for one desktop environment | Own or closely couple with the desktop's settings, panel, notifications, and compositor APIs | Integrates with the user's existing desktop instead of replacing it; publishes support by tested desktop/session capability. |
| Specialist utilities | Deep workflow for one domain such as capture, clipboard, audio, or monitoring | One application and configuration model per workflow | Provides common entry points and cross-module workflows while keeping the first release deliberately narrower than specialist feature depth. |
| Wayland shells, bars, and docks | A unified visual shell for selected compositors | Often owns the panel, launcher, notification daemon, dock, or compositor configuration | Runs as a conventional application and optional command/status surface; it must not require a shell replacement or compositor configuration takeover. |
| Desktop automation platforms and hardware-abstraction layers | Programmatic control for agents, testing, or remote desktop workflows | Broad APIs, daemons, and sometimes input-injection privileges | Prioritizes human utility workflows, consent-aware actions, and narrow privileges; automation interfaces are not the product center. |
| **Kestrel** | Capability-aware, local-first desktop utility workflows | Existing desktop, portals, session services, and optional compositor adapters | Makes available, limited, permission-gated, missing-dependency, and unsupported states visible before the user relies on a feature. |

#### Marketing-safe differentiators

Marketing and product messaging must be backed by the release support matrix and
runtime evidence. Subject to that evidence, Kestrel's differentiators are:

- **Capability transparency:** every feature reports whether it is supported,
  limited, permission-gated, dependency-gated, or unavailable, with the selected
  backend and an actionable explanation.
- **Cross-desktop without shell replacement:** Kestrel aims to work alongside
  existing desktops, panels, launchers, notification services, and compositor
  settings rather than taking ownership of them.
- **One human-facing utility surface:** a shared command and status experience
  connects monitoring, audio, clipboard, capture, and automation-adjacent tasks.
- **Progressive enhancement:** a useful core remains available when optional
  compositor, portal, hardware, or package-manager integrations are absent.
- **Local-first privacy:** no required account or telemetry; clipboard, media, and
  diagnostics data follow explicit retention, exclusion, consent, and redaction
  policies.
- **Explicit support contract:** release claims name the tested desktop,
  compositor, portal backend, package format, and feature scope instead of
  claiming uniform compatibility across Linux.

Kestrel must not claim universal Linux support, feature parity with specialist
tools, or deep window/input control on environments where the relevant compositor
or portal contract is not verified.

---

## 3. Feature-by-feature feasibility

Difficulty legend: ✅ straightforward · 🟡 moderate/partial · 🔶 hard or DE-dependent ·
➖ not applicable.

| Kestrel feature area | Linux difficulty | Native mechanism | Existing Linux tool (reference) |
|---|---|---|---|
| Tray icon + panel | ✅ | StatusNotifierItem (SNI) / libappindicator | `waybar`, KDE tray |
| Per-app volume mixer | ✅ | PipeWire / PulseAudio (`pavucontrol` model) | `pavucontrol`, `qpwgraph` |
| System monitor (CPU/RAM/temps/net) | ✅ | `/proc`, `/sys/class/hwmon`, `/proc/net` | `btop`, `Mission Center` |
| Clipboard history | ✅ | X11 selections / `wl-clipboard`; polling vs. ownership | `CopyQ`, `cliphist` |
| Text snippets | ✅ | `xdotool` / `ydotool` / `wtype` for injection | `Espanso` |
| Screenshots | ✅ | `grim`+`slurp` (Wayland), X11 capture, portal | `flameshot`, `grim` |
| Screen recording | 🟡 | PipeWire + `org.freedesktop.portal.ScreenCast` | `OBS`, `Kooha` |
| Keep awake / battery alerts | ✅ | `systemd-inhibit`, UPower, `/sys/class/power_supply` | `caffeine`, GNOME power settings |
| Brightness toggles | ✅ | `/sys/class/backlight`, `brightnessctl` | `brightnessctl` |
| Bluetooth / WiFi toggles | ✅ | BlueZ D-Bus, NetworkManager / `rfkill` | DE quick settings |
| Command bar / launcher | ✅ | File search + command registry, D-Bus | `rofi`, `wofi`, `ulauncher` |
| Fan control | 🔶 | `/sys/class/hwmon`, `thinkfan`, `nbfc`; hardware-specific | `thinkfan`, `nbfc` |
| Window snapping / layout | 🟡/🔶 | Easy on X11 (EWMH); **restricted by design on Wayland** | WM built-ins, GNOME "Tactile" |
| App switcher + window previews | 🟡/🔶 | Easy on X11; compositor-specific on Wayland | `rofi` window mode, `sway`/`hyprland` IPC |
| Dock previews | ➖ | No Dock on Linux; closest is taskbar/overview | KDE/GNOME overview |
| Uninstaller / Homebrew handoff | ➖ | Different model: package managers, Flatpak | GNOME Software, KDE Discover |
| Radial menu | ✅ | Custom UI overlay | n/a |
| File shelf / drop zone | ✅ | GTK drag-and-drop | n/a |
| Clipboard auto-clear / paste as plain text | ✅/🟡 | Clipboard manager + timed ownership; text-only write | `CopyQ`, `wl-copy` |
| OCR / QR / color picker | ✅/🟡 | Portal or compositor capture + Tesseract/ZBar/pixel sampling | `ksnip`, `grim`, `slurp` |
| Camera preview | ✅ | V4L2/PipeWire | `v4l2`, PipeWire |
| Media conversion / editing | ✅ | FFmpeg/GStreamer | `ffmpeg`, `HandBrake` |
| URL tracking-parameter cleanup | ✅ | Local URL parser + configurable parameter list | browser extensions |
| App updates / package cleanup | 🟡 | APT, Flatpak, Snap and AppImage providers | COSMIC Store, `apt`, `flatpak` |
| Temporary sharing | 🟡 | Explicit upload provider, local-only by default | user-configured service |
| Display control / sleep Bluetooth | 🟡/🔶 | DRM/DDC/sysfs, BlueZ and power-management APIs | `brightnessctl`, `ddcutil` |
| Cleaning mode / scratchpad | ✅ | Fullscreen input-blocking overlay; local GTK storage | n/a |

Net: the high-value, low-risk core (monitor, audio, clipboard, toggles, screenshots,
snippets) is all clean. The advanced desktop features (window control via Accessibility,
per-app audio via CoreAudio HAL, hardware temps via SMC) map to Linux equivalents with
varying fidelity — good for audio and sensors, limited for window management.

---

## 4. Linux platform landscape

A Linux tray utility has to work across two display servers, several desktop
environments, and multiple distros. This section captures the constraints that shape
the architecture.

### 4.1 Display server: X11 vs Wayland

| Concern | X11 | Wayland |
|---|---|---|
| Window enumeration / moving / focusing | Fully scriptable via EWMH (`_NET_CLIENT_LIST`), `xdotool`, `wmctrl` | **Deliberately restricted** — an app cannot move/steal other apps' windows without compositor cooperation |
| Input injection (snippets) | `XTEST` (`xdotool`) | `ydotool` (uinput) or compositor-specific protocols; needs a virtual device |
| Clipboard | X selections (`PRIMARY`/`CLIPBOARD`), well understood | `wl-clipboard`; clipboard owned by focused app, loses content on app exit without a manager |
| Screenshots / capture | X11 grab or `ffmpeg x11grab` | PipeWire + XDG `ScreenCast` portal |
| Global shortcuts | X11 grab | Portal `GlobalShortcuts` (newer) or compositor config |

**Implication:** a fully feature-equivalent app is simpler to build for X11, but Wayland
is the present and future default (GNOME, KDE, Fedora, Ubuntu). The architecture must
treat Wayland as the primary target and degrade gracefully (hide/disable features) where
a compositor does not expose the needed protocol.

Wayland support must be capability-driven rather than inferred from the session type.
For example, `xdg-desktop-portal` may provide screen capture or global shortcuts while
the compositor still provides no window activation or movement API. The implementation
should probe portal interfaces and compositor protocols at startup, record the result,
and expose the reason for every unavailable feature. Potential window integrations
include compositor-specific IPC, KDE/GNOME extensions, and protocols such as
`wlr-foreign-toplevel-management` or `ext-foreign-toplevel-list`; none is a universal
Wayland contract.

### 4.2 Desktop environments and the tray

- **StatusNotifierItem (SNI)** is the modern cross-DE tray standard (D-Bus based).
- **KDE Plasma, XFCE, Cinnamon, MATE, Budgie, LXQt** support SNI (and legacy XEmbed) natively.
- **GNOME** does **not** show tray icons natively; users must install the
  "AppIndicator and KStatusNotifierItem Support" extension. This is a real UX/distribution
  risk and must be documented or mitigated (e.g., a GNOME-shell-free fallback).
- **Tiling WMs** (sway, hyprland, i3) rely on bars (`waybar`, `polybar`) which consume SNI.

### 4.3 Support contract

The project should publish support by desktop/session combination rather than claim
that every Linux desktop has identical behavior. A practical initial contract is:

| Environment | Release status | Expected baseline |
|---|---|---|
| GNOME Wayland | Release-blocking | Core panel, monitor, audio, clipboard, capture portals |
| KDE Plasma Wayland | Release-blocking | Core panel, monitor, audio, clipboard, capture portals |
| COSMIC Wayland | Best-effort initially | Core features; compositor-specific window features may be absent |
| One wlroots compositor (Sway or Hyprland) | Best-effort initially | Core features plus tested compositor adapters |
| X11 with KDE/XFCE/Cinnamon | Compatibility | Core features and X11 window/input fallbacks |
| Other desktops/compositors | Community-supported | Capability probing and documented limitations |

Every release should record the tested desktop, compositor, portal backend, PipeWire
version, GPU driver path, and package format. Hardware-specific behavior cannot be
fully covered by CI, so hardware control remains opt-in and must include a diagnostic
report and a safe read-only fallback.

### 4.4 Audio

- **PipeWire** is the default on Fedora, Ubuntu 22.10+, and most modern distros. It
  exposes the **PulseAudio** API for compatibility, so one PulseAudio backend covers both.
- Per-app volume, mute, and basic routing are available through the PulseAudio
  compatibility API (`libpulse`, `pactl`, `wpctl`). More advanced PipeWire node/link
  operations should use PipeWire/WirePlumber metadata rather than parsing command output.
- Recommendation: start with a PulseAudio-compatible backend for common controls, then
  add a native PipeWire backend for robust stream discovery, device routing, and
  application-specific metadata. Keep both behind the same service interface.

### 4.5 Sensors and hardware

- CPU/memory/process: `/proc` (self-maintained or via `sysinfo` crate).
- Temperatures/fans: `/sys/class/hwmon/*`, optionally `lm-sensors` for naming. Fan control
  is **not portable** — exposed only on some laptops; must be opt-in and hardware-guarded.
- Battery/AC: `UPower` (D-Bus) or `/sys/class/power_supply`.
- Brightness: `/sys/class/backlight/*` or `brightnessctl`.
- Bluetooth: BlueZ D-Bus (`org.bluez`). WiFi: NetworkManager or `iwd` D-Bus; low-level `rfkill`.

### 4.6 Permissions model

Linux has no macOS "Accessibility/Screen Recording" grant dialogs. Instead:

- **Screen capture** on Wayland → user consent via the XDG `ScreenCast` portal.
- **Input injection** (snippets) → `ydotool`/uinput or compositor-specific APIs. Avoid
  treating membership in the broad `input` group as the default; prefer narrowly scoped
  udev permissions or a small Polkit-mediated helper.
- **Hardware (fans, backlight)** → sysfs permissions, vendor services, or a small Polkit
  rule. Read-only monitoring should remain available when control is not safe.
- **No root daemon** should be required; use portals, polkit, and user session D-Bus only.

Linux does not provide a single permission dashboard equivalent to macOS. The
application should therefore show a capability status page listing portal consent,
missing groups or udev rules, unavailable D-Bus services, missing binaries, and
hardware limitations. Every privileged operation must be narrowly scoped and auditable.

### 4.7 Packaging and distribution

- **Flatpak** gives sandboxing + portals but requires careful device/portal permissions
  and complicates deep system access (sensors, backlight).
- **Native packages** (`.deb`, `.rpm`, AUR) give full access but more maintenance.
- **AppImage** is a good middle ground for early distribution, but it is not necessarily
  a truly static binary: GTK, GLib, graphics, PipeWire, and portal libraries are often
  dynamically linked.
- Recommendation: ship native packages or an AppImage bundle first, with a restricted
  Flatpak mode after portal paths are proven. Full hardware and input integration may
  remain native-only.

---

## 5. Hard problems and mitigations

| # | Problem | Impact | Mitigation |
|---|---|---|---|
| 1 | **Wayland window management** (no unified API to move/switch/focus other windows) | Blocks window snapping and a macOS-style switcher | X11 first; on Wayland support per-compositor (wlroots `wlr-foreign-toplevel`, KDE/GNOME extensions); degrade gracefully |
| 2 | **GNOME tray gap** | App invisible in default GNOME | SNI + document the extension; optionally offer a GNOME extension or a normal window fallback |
| 3 | **Fan control portability** | Broken/absent on many machines | Opt-in, hardware-probed (`/sys/class/hwmon`), never crash on missing sensors |
| 4 | **Screen capture consent UX** | Wayland requires portal dialogs | Use `ScreenCast` portal; treat capture features as "portal-gated" |
| 5 | **Clipboard lifetime on Wayland** | Clipboard dies with its owning app | Run as a persistent clipboard manager (re-own selection), like `cliphist`/`CopyQ` |
| 6 | **Multi-DE test matrix** | Regressions across GNOME/KDE/XFCE/sway | CI matrix + manual smoke checklist; abstract DE-specific bits behind a backend trait |
| 7 | **Input injection permissions** (snippets) | `ydotool`/uinput group friction | Detect capability at runtime; fall back to `xdotool` on X11; document setup |
| 8 | **Global shortcut availability** | Portal and compositor support varies | Provider chain: portal → compositor configuration → X11 grabs; explain setup in the UI |
| 9 | **Sensitive local data** (clipboard, recordings, files) | A local-first app can still retain secrets | Retention limits, ignore rules, clear-on-lock/sleep, size limits, and explicit upload consent |
| 10 | **Tray/popup behavior** | SNI exists but tray placement and popovers differ | Use a normal-window fallback and do not make the tray the only access path |

The single biggest strategic risk is **#1**: it is what makes the macOS "switcher +
window controls" feel hard to fully replicate on Wayland, and it should not block the MVP.

---

## 6. Proposed tech stack

### Recommendation: **Rust + GTK4 (+ libadwaita)**

| Criterion | Rationale |
|---|---|
| Native feel & DE integration | GTK4 renders natively on GNOME; libadwaita gives platform-consistent theming |
| System programming | Reading `/proc`, `/sys`, D-Bus, and audio is first-class in Rust; memory-safe |
| Distribution | Rust supports native packages and AppImage bundles; avoid promising a fully static GTK binary |
| Async/threads | `tokio` for D-Bus/portal async, fine-grained control for polling loops |
| Tray | `ksni` (SNI) or `tray-icon` crate; D-Bus via `zbus` |
| Longevity | Strong, well-maintained ecosystem for the exact APIs this app needs |

### Alternatives

| Stack | Pros | Cons | Verdict |
|---|---|---|---|
| **Go + gotk4/systray** | Fast dev, easy concurrency, single binary | GTK bindings less mature; weaker SNI/D-Bus ergonomics | Strong runner-up |
| **Python + PyGObject (GTK)** | Fastest prototyping, `psutil` for monitoring | Distribution (PyInstaller) is painful; runtime dependency | Good for a throwaway prototype |
| **C++ + Qt (QML)** | KDE-native (`QSystemTrayIcon`), mature | More boilerplate, larger code, C++ complexity | Best if KDE is the only target |
| **Swift** | Language continuity with this repo | **No native Linux GUI stack** (no SwiftUI/AppKit) | Reject for GUI; fine for CLI tools |

### Candidate crates (to validate during Phase 0)

- UI: `gtk4`, `libadwaita` (optional `relm4` for a reactive layer)
- Tray: `ksni` (SNI) or `tray-icon`
- D-Bus: `zbus` (BlueZ, UPower, NetworkManager, portals, SNI)
- Monitoring: `sysinfo` (or hand-rolled `/proc` + `/sys` readers)
- Audio: `libpulse-binding` (PulseAudio/PipeWire) or `pactl` via subprocess
- Clipboard: `arboard` (cross-platform clipboard)
- X11 windowing (Phase 3): `x11rb`
- Wayland (Phase 3): `smithay-client-toolkit` / compositor-specific IPC

> Exact crate selection is a Phase 0 task; the above is a well-trodden path, not a
> commitment.

---

## 7. Architecture

### 7.1 Design principles

1. **One user-session suite, no root daemon.** The first MVP may be one process, but the
   design should allow an unprivileged session daemon and a separate UI process. Privilege
   only via portals, Polkit, or narrowly scoped helpers where a feature truly needs it.
2. **Feature registry, not a monolith.** Each utility is a self-contained module that
   registers capabilities; the tray/panel renders whatever is present.
3. **Backend abstraction.** Anything DE/server-specific (X11 vs Wayland, PulseAudio vs
   PipeWire) sits behind a trait so features degrade gracefully instead of crashing.
4. **Local-only, private by default.** No telemetry, no accounts; mirror Kestrel's
   privacy stance.
5. **Graceful degradation.** If a capability is unavailable (no portal, no sensor, no
   tray), hide/disable that feature and keep the rest running.

### 7.2 Layered structure

```
┌─────────────────────────────────────────────────────┐
│                  Tray Icon (SNI)                     │
│   click → panel popover · scroll → quick action      │
└───────────────────────────┬─────────────────────────┘
                            │
┌───────────────────────────▼─────────────────────────┐
│                    UI Layer (GTK4)                   │
│  Panel popover · Settings window · feature sub-views │
│  (pure presentation, no business logic)              │
└───────────────────────────┬─────────────────────────┘
                            │
┌───────────────────────────▼─────────────────────────┐
│                 Application Core                     │
│  Feature registry · config store · event bus         │
│  scheduler (polling) · capability detection          │
└───────────────────────────┬─────────────────────────┘
                            │
┌───────────────────────────▼─────────────────────────┐
│                  Service Layer                       │
│  system-monitor │ audio │ clipboard │ snippets │ ...  │
│  (one module per feature; owns its own state)        │
└───────────────────────────┬─────────────────────────┘
                            │
┌───────────────────────────▼─────────────────────────┐
│              Platform Backends (OS)                  │
│  /proc /sys │ PipeWire/PulseAudio │ D-Bus (BlueZ,    │
│  UPower, NM) │ portals │ X11 / Wayland               │
└─────────────────────────────────────────────────────┘
```

### 7.3 Layer responsibilities

- **Tray Icon:** SNI registration, menu/popover trigger, scroll events, context menu.
- **UI Layer:** the popover panel (mirrors Kestrel's tabbed panel: System / Mixer /
  Controls / Utilities), settings, and per-feature dialogs. GTK widgets only.
- **Application Core:** startup, single-instance guard, feature discovery, config
  (key/value with schema), a lightweight event bus (`tokio` broadcast channels), and a
  scheduler that runs periodic polls (monitor sampling, battery checks).
- **Service Layer:** one module per feature implementing a common service interface
  (`id`, capability report, `start`, `stop`, commands, state and events). Services do not
  return GTK widgets; the UI renders their state through view models and dispatches
  commands back to them.
- **Platform Backends:** thin wrappers over the OS. Each backend exposes a trait so the
  service above it never branches on "X11 vs Wayland" directly.

### 7.4 The `Feature` trait (conceptual)

```rust
trait Feature {
    fn id(&self) -> &'static str;                 // stable id, e.g. "audio.mixer"
    fn label(&self) -> &'static str;              // localized
    fn capability(&self, caps: &Caps) -> CapabilityStatus;
    fn start(&mut self, bus: &Bus) -> Result<()>;
    fn stop(&mut self);
    fn state(&self) -> FeatureState;              // UI-agnostic state snapshot
    fn commands(&self) -> &[CommandSpec];         // UI-agnostic actions
}
```

This is the mechanism that makes the app a *registry* of independent utilities, exactly
matching Kestrel's "install only what you use" philosophy (disabled features are
simply not started).

### 7.5 Capability statuses

The feature registry should distinguish availability from user enablement. A feature
can be installed and enabled while still being limited by the current platform:

```rust
enum CapabilityStatus {
    Supported,
    Limited { reason: String },
    NeedsPermission { permission: Permission },
    MissingDependency { name: String },
    Unsupported { reason: String },
}
```

Capability probing should be cached for the session and refreshed when devices,
portals, or compositor services appear. The UI should show both the status and a
specific remediation rather than simply hiding a feature without explanation.

### 7.6 Config, IPC, and lifecycle

- **Config:** a versioned TOML/key-value file under `$XDG_CONFIG_HOME`, with schema
  migrations and an export/import format. Sensitive data should not be stored unless
  explicitly required by a feature.
- **IPC/single instance:** D-Bus name ownership (one instance) + D-Bus activation so a
  second launch forwards to the running instance (e.g., "open panel"). If the UI and
  session daemon are split, use a documented, versioned D-Bus API.
- **Autostart:** `.desktop` entry in `~/.config/autostart` (XDG autostart spec).
- **Lifecycle:** features start only when enabled and capable, stop when disabled, and
  release clipboard, audio, input, and portal resources deterministically.

---

## 8. Component breakdown (per feature)

| Feature | Module | Primary data source | DE / server notes |
|---|---|---|---|
| System monitor | `system-monitor` | `/proc`, `/sys/class/hwmon`, `/proc/net` | universal |
| Per-app volume | `audio` | PulseAudio compatibility API + PipeWire metadata | Native PipeWire backend for advanced routing |
| Clipboard history | `clipboard` | X11 selections / Wayland data-control / portal clipboard APIs | `wl-clipboard` alone is not a history manager; persist by re-owning selection |
| Text snippets | `snippets` | `xdotool`/`ydotool`/`wtype` injection | X11 vs uinput capability detection; app-specific Wayland behavior is limited |
| Screenshots | `screenshot` | `grim`+`slurp` (Wayland), X11 grab, portal | portal-gated on Wayland |
| Screen recording | `recorder` | PipeWire + `ScreenCast` portal | portal consent dialog |
| Keep awake / battery | `power` | `systemd-inhibit`, UPower, `/sys/class/power_supply` | universal |
| Brightness / BT / WiFi | `toggles` | `/sys/class/backlight`, BlueZ D-Bus, NetworkManager | hardware-probed |
| Launcher / command bar | `launcher` | file index + command registry + D-Bus | DE-independent |
| Fan control | `fan` | `/sys/class/hwmon`, `thinkfan`/`nbfc` | opt-in, hardware-guarded |
| Window snapping / switcher | `windows` | X11 EWMH; wlroots/`hyprland`/KDE/GNOME protocols | Phase 3, Wayland-limited |
| OCR / QR / color | `screen-tools` | Portal/compositor capture + Tesseract/ZBar/pixel sampling | one-shot tools should not require a permanent capture session |
| Camera preview | `camera` | V4L2/PipeWire | permission/camera availability is runtime-probed |
| Packages / cleanup | `packages` | APT, Flatpak, Snap, AppImage metadata | provider interface; never assume one package database |
| URL cleaner | `url-cleaner` | local URL parser and configurable rules | no network required |
| Scratchpad / cleaning mode | `utility-overlays` | GTK overlay and local state | compositor positioning may be limited |

---

## 9. Security & permissions

- **Least privilege:** run as a normal user; no root, no setuid binary, no root daemon.
- **Portals:** screen capture and recording go through the XDG `ScreenCast` portal (user
  consent, per-session, revocable).
- **polkit (only if needed):** e.g., suspend/lid behavior may need a polkit rule; prefer
  `systemd-inhibit` which is user-session.
- **Input injection:** snippets use `ydotool`/uinput or `xdotool` on X11; capability is
  detected at runtime and documented. Avoid granting unrestricted access to every input
  device when a narrower udev or helper configuration is possible.
- **Sandboxing:** a Flatpak build is the end state; it must declare portal, D-Bus, and
  device access explicitly. Some hardware/input features may intentionally remain native
  package features rather than weakening the sandbox.
- **Clipboard privacy:** provide retention limits, per-application ignore rules, maximum
  item sizes, clear-on-lock/sleep, and a way to remove all history immediately. Do not
  persist clipboard contents by default until the user enables history.
- **Media privacy:** screenshots, recordings, camera frames, OCR text, and QR results
  stay local by default. Upload or temporary sharing must be an explicit, separately
  enabled provider action with a reviewable destination and retention policy.
- **Diagnostics:** diagnostic bundles must redact clipboard content, file paths where
  possible, network identifiers, and tokens before export.
- **Privacy baseline:** no telemetry, no analytics, and no account are required, matching Kestrel's
  local-first stance.

---

## 10. Packaging & distribution

| Artifact | Pros | Cons | Priority |
|---|---|---|---|
| Native `.deb` / `.rpm` / AUR | Full integration, services and device access | Per-distro maintenance | **First for full mode** |
| AppImage bundle | Easy early distribution, one download | Dynamic library and desktop-integration caveats | **First for preview releases** |
| Flatpak restricted mode | Sandboxing, portals, cross-distro store | Hardware/input/package-manager friction | Third (prove portals first) |

- **Autostart** via XDG autostart `.desktop`.
- **Tray note:** document the GNOME AppIndicator extension requirement prominently.
- **Update note:** the suite should not silently replace system packages. Expose package
  providers explicitly and delegate updates to APT, Flatpak, Snap, or the configured
  application source.

---

## 11. MVP roadmap (phased)

### Phase 0 — Feasibility spikes (validate assumptions, throwaway code)
1. Show a tray icon on GNOME + KDE + a Wayland bar (SNI works; GNOME extension caveat).
2. Read CPU/RAM from `/proc` and temps from `/sys/class/hwmon` (hardware probe).
3. Enumerate PipeWire/PulseAudio streams and set per-app volume.
4. Capture and re-own the clipboard (X11 and Wayland).
5. Take a screenshot on Wayland via the portal.
6. Register a global shortcut through the portal where supported and document a
   compositor-configured fallback.
7. Verify a UI-only process can communicate with a user-session daemon over D-Bus.
- **Exit criteria:** each spike works on at least 2 DEs or has a documented capability
  limitation; decide final stack, crates, process boundary, and support contract.

### Phase 1 — MVP (the high-value, low-risk core)
- Tray icon + tabbed popover panel (System / Mixer / Controls / Utilities).
- System monitor: CPU, RAM, temps, network, battery + keep-awake toggle.
- Per-app volume mixer + output device switching.
- Clipboard history (persistent manager).
- Quick toggles: brightness, Bluetooth, WiFi, keep-awake, battery alerts.
- Settings window + config file + autostart.
- **Exit criteria:** installable native package and AppImage preview; runs on GNOME and
  KDE Wayland plus one X11 environment; no root daemon required.

### Phase 2 — Utility expansion
- Text snippets (X11 + Wayland injection).
- Command bar / launcher (file search, commands, math, unit conversion).
- Screenshots with annotation; screen recording via portal.
- OCR, QR recognition, color picker, camera preview, scratchpad, and paste-as-plain-text.

### Phase 3 — Hard / optional (DE-dependent)
- Window snapping (X11 first; per-compositor on Wayland).
- App switcher with window previews.
- Fan control (opt-in, hardware-guarded).
- Radial menu and file shelf (custom UI).
- Display/DDC controls, Bluetooth-on-sleep, URL cleanup, package-provider actions,
  media tools, temporary sharing, and cleaning mode.

### Explicitly out of scope (Linux has no equivalent)
- Dock previews (no Dock).
- A Finder-style Dock integration and macOS-specific disk-image installation workflow.
  Package removal and cleanup may still be offered through explicit APT/Flatpak/Snap
  providers, but they are not one universal uninstaller.

---

## 12. Risks & decisions

### Risks

| Risk | Likelihood | Severity | Mitigation |
|---|---|---|---|
| Wayland window-management APIs stay fragmented | High | Medium (Phase 3 only) | Defer to Phase 3; per-compositor backends |
| GNOME tray requires extension | High | Medium (adoption) | Document; offer fallback/extension |
| Fan/hwmon support varies by laptop | High | Low (optional feature) | Opt-in, probe-driven |
| Flatpak permissions hurt system features | Medium | Medium | Ship native binary first, Flatpak later |
| Maintenance across many DEs | Medium | High | Backend traits + CI matrix + smoke checklist |
| Feature status is unclear to users | Medium | High | Capability statuses with actionable explanations |
| Clipboard/input data is over-collected | Medium | High | Secure defaults, retention controls, redaction and narrow permissions |
| Native/AppImage/Flatpak behavior diverges | Medium | Medium | Define full mode versus restricted mode and test both |

### Decisions and validation status

Resolve the following as project decisions rather than leaving them as open-ended
questions. Each decision has a proposed default, a validation experiment, and a
condition for revisiting it.

#### D-001 — Support contract

- **Status:** Proposed.
- **Decision:** Release-blocking support for GNOME Wayland, KDE Plasma Wayland, and X11
  with at least one mainstream desktop. COSMIC, one wlroots compositor such as Sway or
  Hyprland, and other desktops are initially best-effort.
- **Rationale:** This provides a credible generic-Linux baseline without claiming that
  every compositor exposes identical window, input, or tray APIs.
- **Validation:** Run the Phase 0 tray, monitor, audio, clipboard, capture, and shortcut
  probes on each target environment.
- **Revisit when:** A target environment becomes common enough to justify release-blocking
  coverage, or a required MVP capability cannot be supported reliably.

#### D-002 — Display server priority

- **Status:** Proposed.
- **Decision:** Wayland-first with X11 compatibility. The portable core must work through
  Wayland/freedesktop interfaces; X11 provides compatibility for richer window and input
  operations where available.
- **Rationale:** Wayland is the current Linux direction, while X11 remains valuable for
  legacy desktops and capabilities intentionally restricted by Wayland.
- **Validation:** Verify monitoring, audio, clipboard, screenshots, portals, notifications,
  keep-awake, and global-shortcut registration on Wayland and X11.
- **Revisit when:** A required core feature cannot be made reliable through the selected
  Wayland baseline.

#### D-003 — Process model

- **Status:** Proposed.
- **Decision:** Begin with one user-session binary, but keep UI, application core, feature
  services, and platform backends as separate internal layers. Reserve a stable D-Bus
  boundary for a future UI/session-daemon split.
- **Rationale:** This keeps the MVP manageable while preserving a path to persistent
  clipboard state, crash isolation, headless operation, and independent UI restarts.
- **Validation:** Complete the Phase 0 D-Bus and lifecycle spike, including deterministic
  cleanup of clipboard, audio, input, and portal resources.
- **Revisit when:** persistent background state or UI isolation becomes a reliability
  requirement.

#### D-004 — Distribution modes

- **Status:** Proposed.
- **Decision:** Ship a native `.deb` package for full integration and an AppImage bundle
  for preview releases. Add Flatpak later as a restricted, portal-first mode.
- **Rationale:** Sensors, backlight, input/uinput, package managers, and system services
  are difficult to expose cleanly from a sandboxed Flatpak.
- **Validation:** Produce a native package and AppImage from the same release, then verify
  autostart, portal capture, update behavior, and capability reporting in both modes.
- **Revisit when:** Flatpak can provide the required core capabilities without broad host
  permissions or a confusing split between full and restricted modes.

#### D-005 — Identity

- **Status:** Proposed before public release.
- **Decision:** Use a new name, logo, application ID, and visual identity. An internal
  codename is acceptable during development.
- **Rationale:** Kestrel must maintain an independent name, logo, application ID, and visual identity.
- **Validation:** Complete a branding/trademark review before publishing packages,
  desktop files, or a public Linux repository.
- **Revisit when:** The project adopts a different approved public identity.

---

## 13. Existing Linux tools — build vs. reuse

Before building any module, consider whether an existing, maintained tool already solves
it well enough to *integrate* (shell out / D-Bus) rather than reinvent:

| Need | Existing tool | Reuse strategy |
|---|---|---|
| Per-app volume | `pavucontrol`, `qpwgraph` | Use PulseAudio/PipeWire API directly (same backend) |
| System monitor | `btop`, `Mission Center` | Reuse `/proc`+`/sys` approach, not the tools |
| Clipboard | `CopyQ`, `cliphist` | Could integrate or reimplement; re-owning selection is the key |
| Snippets | `Espanso`, `xremap` | Integrate with Espanso or provide a uinput-backed implementation; app-specific Wayland behavior is limited |
| Launcher | `rofi`, `wofi`, `ulauncher`, `albert` | Compete or embed; integrate via config/scripts |
| Screenshots | `flameshot`, `grim`+`slurp` | Reuse `grim`/portal; custom annotation UI if needed |
| Recording | `OBS`, `Kooha`, `wf-recorder` | Use portal directly or shell to `wf-recorder` |
| Command bar | `Vicinae` | Evaluate as an integration target and competitive reference |
| Per-app audio | `volctl`, EasyEffects | Integrate where it provides the desired UI/effects; retain PipeWire backend ownership |
| Input remapping | `Input Remapper`, `xremap` | Integrate or reuse their device/profile model; avoid duplicating low-level injection prematurely |
| Fan control | `CoolerControl`, `asusctl`, `thinkfan` | Detect and delegate supported hardware; never assume generic laptop control |
| Capture tools | GPU Screen Recorder, `ksnip`, Flameshot | Reuse mature capture/encoding paths before building a full editor |
| Shell/dock | Noctalia, Docking | Treat as optional ecosystem integrations; neither is a universal replacement for the suite |
| Cleanup | BleachBit | Integrate cautiously or provide focused cleanup rules; preview before deletion |

**Recommendation:** implement monitor, audio, clipboard, and toggles natively (they are
the product's core and cheap on Linux), but **integrate with `Espanso`** for snippets and
**shell out to `grim`/`wf-recorder`** for capture in Phase 2 rather than reimplementing.

---

## 14. Recommendation & next steps

1. **Proceed as a new Rust + GTK4 application**, with its own product design and implementation.
2. **Start with Phase 0 spikes** to validate tray, sensors, audio, clipboard, and portal
   capture before committing to a full build.
3. **Resolve the proposed decisions** in §12 and record any changes before writing
   production code.
4. **Gate Phase 1** on the Phase 0 exit criteria (works on ≥2 DEs, no root).

### Immediate next actions

- [ ] Review and accept or revise decisions D-001 through D-005 in §12.
- [ ] Create the Kestrel repository with its own application ID and visual identity.
- [ ] Run Phase 0 spike #1: SNI tray icon on GNOME + KDE.
- [ ] Define the initial capability-status schema and support matrix.
- [ ] Decide which features are native, integrated, shell-backed, or intentionally excluded.
