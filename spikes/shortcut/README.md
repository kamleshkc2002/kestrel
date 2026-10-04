# Phase 0 spike: global-shortcut activation paths

Disposable read-only probe for Kestrel issue #7, outside the main workspace. It
never calls `BindShortcuts` or triggers a consent dialog.

```sh
bash spikes/shortcut/probe.sh > /tmp/kestrel-shortcut-probe.txt 2>&1
gcc -Wall -Wextra -O2 -o spikes/shortcut/xgrabkey_probe spikes/shortcut/xgrabkey_probe.c -lX11
spikes/shortcut/xgrabkey_probe
```

## Environment (validated live)

- Wayland COSMIC session (`XDG_SESSION_TYPE=wayland`, `WAYLAND_DISPLAY=wayland-1`).
- Xwayland at `DISPLAY=:1` (X.Org 24.1.13, 2560x1600, root 24bpp); WM is Smithay X WM.
- `xdg-desktop-portal 1.18.4` (`1.18.4-1ubuntu2.24.04.2`), COSMIC backend
  0.1.0, GTK backend 1.15.1; both backends are active on the session bus.

## Portal GlobalShortcuts (validated)

`org.freedesktop.portal.GlobalShortcuts` is not exported. Version and
`CreateSession` probes both returned `No such interface`; the frontend exports
24 interfaces. Neither backend exports
`org.freedesktop.impl.portal.GlobalShortcuts`, and both `.portal` files omit it:

- COSMIC: Access, FileChooser, RemoteDesktop, ScreenCast, Screenshot, Settings.
- GTK: Access, Account, AppChooser, DynamicLauncher, Email, FileChooser, Inhibit,
  Lockdown, Notification, Print, Settings.

The installed `xdg-desktop-portal` NEWS says the frontend introduced the portal
in 1.16.0. Version 1.18.4 contains the implementation strings but does not export
the interface without a backend. Published interface version 2 provides
`CreateSession`, `BindShortcuts`, `ListShortcuts`, `ConfigureShortcuts` and
`Activated`, `Deactivated`, `ShortcutsChanged` signals.

### Permission model (portal specification)

- `CreateSession(options)` returns a request handle; the response contains a
  `session_handle` object path (typed `s` for historical reasons).
- One `BindShortcuts(session_handle, shortcuts, parent_window, options)` call per
  session may show a user dialog. Entries contain `description` and optional
  `preferred_trigger`; the response returns only the bound subset and trigger
  descriptions, not the actual trigger.
- `ListShortcuts` returns active or previously bound shortcuts for that app;
  `ConfigureShortcuts` (v2) reopens configuration.
- Activation includes `session_handle`, shortcut ID, timestamp, and optional
  `activation_token`; `Deactivated` mirrors it.
- Sessions belong to the creating app and close through `Session.Close`; the
  backend/user resolves conflicts, and an app gets one bind attempt per session.

No GlobalShortcuts permission-store rows exist: each of
`shortcuts`, `global_shortcuts`, `global-shortcuts`, and `GlobalShortcuts` returned
`as 0`. The exact table name remains deferred without a binding-capable backend.

## COSMIC alternatives (validated)

`com.system76.CosmicSettingsDaemon` and `com.system76.CosmicComp` expose only
D-Bus Introspectable/Peer/Properties at their roots, not keybinding registration.
No shortcut/keybind config files exist under `~/.config/cosmic`; COSMIC keybindings
are configured in Settings, not through an application D-Bus API in this version.

Upstream/deferred evidence: KDE's portal backend implements GlobalShortcuts;
GNOME's backend adds it in 48.rc; Hyprland has
`src/portals/GlobalShortcuts.cpp`; no COSMIC implementation was found locally or
upstream during this spike.

## X11 XGrabKey fallback (validated)

Every tested grab on `DISPLAY=:1` returned `AlreadyGrabbed` (status 1), including
F12 and `a`, with no modifiers and Ctrl+Alt, plus space with no modifiers and
Ctrl+Alt. The probe result was `no_free_combo_found (XGrabKey surface is occupied)`.

XGrabKey is callable, but Smithay X WM owns the keyboard-grab surface on this
Wayland/Xwayland session, so the fallback is unsupported here. On real X11 it is
the expected fallback: report `AlreadyGrabbed`, ungrab with `XUngrabKey` on
cleanup, and validate that path separately (deferred).

## Normal-window policy and activation chain

`docs/ARCHITECTURE.md` requires a normal application window and command surface
that remain useful without a tray host. The current app is a capability-printer
scaffold; the GTK4/libadwaita window is a Phase 1 deliverable. Normal-window
activation is therefore **Supported by design** and the only guaranteed path.

1. **Always available:** normal window/command surface via app grid, autostart,
   or a second single-instance invocation over the session D-Bus name; no permission.
2. **Preferred:** `org.freedesktop.portal.GlobalShortcuts` when frontend and
   backend exist. Call `BindShortcuts` only after an explicit Settings action,
   never during startup probing.
3. **Compositor fallback:** document a user-configured COSMIC keybinding invoking
   `kestrel --command <id>` or a single-instance D-Bus method.
4. **X11 fallback:** use `XGrabKey` only on real X11; report conflicts and ungrab
   on disable/exit.
5. **Cleanup:** close portal sessions with `Session.Close`; no probe consents.

## Supported capability states (COSMIC Wayland)

| Path | State | Evidence / remediation |
|---|---|---|
| Normal window / command surface | Supported by design; UI deferred to Phase 1 | Implement GTK4/libadwaita surface. |
| Portal GlobalShortcuts | Unsupported | Frontend/backend absent; use KDE, GNOME 48+, or Hyprland backend, or await COSMIC support. |
| Portal GlobalShortcuts on a capable desktop | NeedsPermission (deferred) | Validate `CreateSession` + `BindShortcuts` with an explicit grant. |
| COSMIC compositor keybinding | Limited (user-configured; no app API) | Document a custom Settings shortcut and ship command IDs. |
| X11 XGrabKey on real X11 | Limited (deferred) | Validate success/cleanup; handle `AlreadyGrabbed`. |
| X11 XGrabKey on this Wayland/Xwayland session | Unsupported | All grabs are `AlreadyGrabbed`; do not attempt it here. |

## Validated vs deferred

Validated: session identity; portal frontend/backend absence; empty permission
store; COSMIC API/config absence; Xwayland reachability and XGrabKey refusal;
and normal-window policy. Deferred: a real portal bind/activation/cleanup cycle
on KDE, GNOME 48+, or Hyprland, which requires a backend and interactive grant;
and successful real-X11 XGrabKey cleanup.
