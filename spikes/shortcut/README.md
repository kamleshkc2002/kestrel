# Phase 0 spike: global-shortcut activation paths

Disposable probe for Kestrel Issue #7. It is intentionally outside the main
workspace (`spikes/shortcut/` is not a workspace member). The probe is
read-only: it never calls `BindShortcuts` and never triggers a consent dialog.

```sh
bash spikes/shortcut/probe.sh > /tmp/kestrel-shortcut-probe.txt 2>&1
gcc -Wall -Wextra -O2 -o spikes/shortcut/xgrabkey_probe spikes/shortcut/xgrabkey_probe.c -lX11
spikes/shortcut/xgrabkey_probe
```

## Environment (validated live)

- Session: `XDG_SESSION_TYPE=wayland`, `XDG_CURRENT_DESKTOP=COSMIC`,
  `XDG_SESSION_DESKTOP=COSMIC`, `WAYLAND_DISPLAY=wayland-1`.
- Xwayland reachable at `DISPLAY=:1` (X.Org 24.1.13, 2560x1600, root 24bpp).
- `xdg-desktop-portal 1.18.4` (`1.18.4-1ubuntu2.24.04.2`), plus
  `xdg-desktop-portal-cosmic 0.1.0` and `xdg-desktop-portal-gtk 1.15.1`.
- Portal backends active on the session bus: COSMIC and GTK/GNOME.

## 1. Portal GlobalShortcuts availability (validated)

`org.freedesktop.portal.GlobalShortcuts` is **not exported** on this session:

```text
frontend_GlobalShortcuts_version_probe:
  Failed to get property version on interface org.freedesktop.portal.GlobalShortcuts:
  No such interface "org.freedesktop.portal.GlobalShortcuts"

frontend_GlobalShortcuts_method_probe(CreateSession):
  Call failed: No such interface "org.freedesktop.portal.GlobalShortcuts" on
  object at path /org/freedesktop/portal/desktop
```

The portal frontend exports 24 portal interfaces; `GlobalShortcuts` is absent.
Neither active backend exports `org.freedesktop.impl.portal.GlobalShortcuts`:

- COSMIC backend interfaces: Access, FileChooser, RemoteDesktop, ScreenCast,
  Screenshot, Settings (no GlobalShortcuts).
- GTK backend interfaces: Access, Account, AppChooser, DynamicLauncher, Email,
  FileChooser, Inhibit, Lockdown, Notification, Print, Settings (no
  GlobalShortcuts).

Both backend `.portal` files omit `org.freedesktop.impl.portal.GlobalShortcuts`
from their `Interfaces=` list (`cosmic.portal` and `gtk.portal`).

Upstream attribution (deferred, not executable here):

- The portal frontend introduced the Global Shortcuts portal in
  `xdg-desktop-portal 1.16.0` ("Introduce the Global Shortcuts portal ..."),
  per the installed `NEWS.gz`. The frontend binary 1.18.4 contains
  `XdpDbusGlobalShortcuts`/`org.freedesktop.portal.GlobalShortcuts` strings, so
  the code is compiled in but is not exported because no selected backend
  implements the backend interface.
- Current published frontend/backend interface version is **2**
  (`CreateSession`, `BindShortcuts`, `ListShortcuts`, `ConfigureShortcuts`;
  signals `Activated`, `Deactivated`, `ShortcutsChanged`).

### Permission model (from the portal spec)

- `CreateSession(options) -> request handle`; response returns a
  `session_handle` object path (typed `s` for historical reasons).
- `BindShortcuts(session_handle, shortcuts, parent_window, options)` is called
  once per session. Each shortcut is `(id, vardict)` with keys
  `description (s)` and optional `preferred_trigger (s)` (XDG shortcuts spec).
  The portal may show a dialog letting the user accept/reconfigure triggers.
  The response returns the bound subset as `(id, {description,
  trigger_description})`; the actual bound trigger is intentionally not
  exposed to the app.
- `ListShortcuts(session_handle)` returns active shortcuts for a session, or
  shortcuts successfully bound in a previous session by the same app
  (persistence across sessions).
- `ConfigureShortcuts` (version 2) reopens the configuration UI.
- Activation arrives as `Activated(session_handle, shortcut_id, timestamp,
  options)` with an `activation_token` option for focus/activation handoff;
  `Deactivated` mirrors it.
- Sessions are bound to the creating app; cleanup is the normal portal
  `Session.Close` path. Conflicts are resolved by the backend/user in the
  configuration dialog; an app can only attempt `BindShortcuts` once per
  session.

No GlobalShortcuts permission-store rows exist on this host
(`List('shortcuts' | 'global_shortcuts' | 'global-shortcuts' |
'GlobalShortcuts')` all return `as 0`). The exact store table name is not
observable without a binding-capable backend, so it is deferred.

## 2. Compositor-specific alternatives (validated for COSMIC)

- `com.system76.CosmicSettingsDaemon` and `com.system76.CosmicComp` are
  present, but neither exposes a keybinding/shortcut registration interface;
  introspection shows only DBus Introspectable/Peer/Properties at their roots.
- No COSMIC config files for shortcuts/keybinds exist under
  `~/.config/cosmic` (`*shortcut*`, `*keybind*`, `*key_bind*`, `*binding*`
  matches return empty). COSMIC's keybinding surface is user-configured in the
  Settings UI, not an application-facing D-Bus API in this version.

Cross-desktop (deferred, upstream evidence):

- **KDE Plasma**: `xdg-desktop-portal-kde` implements GlobalShortcuts
  (`data/kde.portal` lists `org.freedesktop.impl.portal.GlobalShortcuts`;
  `src/globalshortcuts.cpp` + `GlobalShortcutsDialog.qml` present).
- **GNOME**: `xdg-desktop-portal-gnome` NEWS says "Add global shortcuts portal
  backend" in **48.rc** (GNOME 48); later releases carry improvements.
- **Hyprland**: `xdg-desktop-portal-hyprland` has
  `src/portals/GlobalShortcuts.cpp`.
- **COSMIC**: no backend implementation found locally or upstream at the time
  of this spike.

## 3. X11 XGrabKey fallback (validated)

Xwayland is reachable, and the X11 WM is `Smithay X WM` (COSMIC's Xwayland
window manager). Every `XGrabKey` attempt on this display returns
`AlreadyGrabbed` (status 1), even for unused combinations, plain keys with no
modifiers, and grabs on a fresh application window:

```text
display=:1
scan F12 keycode=96 modifiers=0x0 (none) root=AlreadyGrabbed child=AlreadyGrabbed
scan F12 keycode=96 modifiers=0xc (Ctrl+Alt) root=AlreadyGrabbed child=AlreadyGrabbed
scan a keycode=38 modifiers=0x0 (none) root=AlreadyGrabbed child=AlreadyGrabbed
scan a keycode=38 modifiers=0xc (Ctrl+Alt) root=AlreadyGrabbed child=AlreadyGrabbed
scan space keycode=65 modifiers=0x0 (none) root=AlreadyGrabbed child=AlreadyGrabbed
scan space keycode=65 modifiers=0xc (Ctrl+Alt) root=AlreadyGrabbed child=AlreadyGrabbed
RESULT no_free_combo_found (XGrabKey surface is occupied)
```

Interpretation: `XGrabKey` is present and callable, but on this Wayland/Xwayland
session the keyboard-grab surface is occupied by the compositor's X WM, so the
X11 fallback is **not usable** here. On a real X11 session `XGrabKey` is the
expected fallback and must handle `AlreadyGrabbed` conflicts plus `XUngrabKey`
cleanup on disable/exit; that path is deferred (no real X11 session available
to validate).

## 4. Normal-window / command-surface fallback (validated as policy)

`docs/ARCHITECTURE.md` requires: "The process must remain useful without a tray
host. A normal application window and command surface are always valid entry
points." The current `apps/kestrel` is only the scaffold capability printer;
the GTK4/libadwaita command-surface window is a Phase 1 deliverable. Therefore
normal-window activation is **by-design Supported** and is the only
guaranteed path; the window implementation itself is deferred.

## Command-surface activation policy (exit criterion)

Kestrel activates the command surface through a fixed fallback chain, and
never depends on a global shortcut as the sole entry point:

1. **Always available**: normal window + command surface, launched from the
   desktop/app grid, autostart, or a second single-instance invocation over the
   session D-Bus name. No permission. This is the baseline and must ship even
   when every global-shortcut path is Unsupported.
2. **Preferred global path**: XDG `org.freedesktop.portal.GlobalShortcuts`
   when both the frontend interface and a backend implementation are present.
   Registration is **permission-gated**: `BindShortcuts` may present an
   interactive user dialog, so Kestrel must only call it from an explicit user
   action in Settings, never from a startup probe.
3. **Compositor fallback**: when the portal is absent, document a
   user-configured compositor keybinding that invokes a Kestrel command
   (e.g. `kestrel --command <id>` or a single-instance D-Bus method). Kestrel
   supplies command IDs and documentation; it does not require a compositor
   registration API.
4. **X11 fallback**: `XGrabKey` only on real X11 sessions. Handle
   `AlreadyGrabbed` conflicts by reporting the collision, and always
   `XUngrabKey`/close sessions on disable or exit.
5. **Cleanup**: portal sessions are closed via `Session.Close`; X11 grabs via
   `XUngrabKey`; no probe triggers a consent dialog.

## Supported capability states (this session, COSMIC Wayland)

| Path | State | Evidence | Remediation |
|---|---|---|---|
| Normal window / command surface | Supported (by design; UI deferred to Phase 1) | ARCHITECTURE.md process rules; scaffold only today | Implement GTK4/libadwaita command surface in Phase 1 |
| Portal GlobalShortcuts | Unsupported | Frontend does not export interface; neither backend implements it | Install/upgrade a GlobalShortcuts-capable backend (KDE, GNOME 48+, Hyprland); on COSMIC wait for backend support or upgrade portal stack |
| Portal GlobalShortcuts (capable desktop) | NeedsPermission (deferred) | Portal spec requires user dialog on `BindShortcuts` | Validate `CreateSession`+`BindShortcuts` on GNOME/KDE/Hyprland with an explicit user grant |
| COSMIC compositor keybinding | Limited (user-configured; no app API) | `CosmicSettingsDaemon`/`CosmicComp` expose no shortcut registration | Document "run Kestrel command" custom shortcut in COSMIC Settings; ship command IDs |
| X11 XGrabKey (real X11 session) | Limited (deferred) | API present and callable; conflict/cleanup semantics defined | Validate on an X11 session; handle `AlreadyGrabbed`, ungrab on exit |
| X11 XGrabKey (this Wayland/Xwayland session) | Unsupported | All grabs return `AlreadyGrabbed`; `Smithay X WM` owns keyboard grabs | Do not attempt XGrabKey under Wayland/Xwayland |

## Validated vs deferred

- **Validated on this host**: session identity; portal frontend/backend absence
  of GlobalShortcuts; permission-store emptiness; COSMIC keybinding surface
  absence; Xwayland reachability and `XGrabKey` refusal; normal-window policy
  in the architecture doc.
- **Deferred**: a real portal `BindShortcuts` requires an interactive user
  grant and a GlobalShortcuts-capable backend (KDE Plasma, GNOME 48+,
  Hyprland) — not available in this COSMIC session, so the successful bind,
  `Activated` delivery, and portal cleanup were not exercised here.
- **Deferred**: real-X11 `XGrabKey` success/cleanup (only Xwayland is
  available here).
