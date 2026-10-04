# Phase 0 spike: clipboard ownership and privacy lifecycle

Disposable Rust probe for Kestrel issue #5, outside the main workspace. It
probes Wayland data-control through `wl-clipboard-rs` and X11 selection
ownership through `x11rb`; it runs independently of `wl-copy`, `wl-paste`,
`xclip`, and `xsel`.

```sh
cargo run --quiet --manifest-path spikes/clipboard/Cargo.toml > /tmp/kestrel-clipboard-capabilities.json
cargo run --quiet --manifest-path spikes/clipboard/Cargo.toml -- --exercise-lifecycle all > /tmp/kestrel-clipboard-lifecycle.json
```

## Read-only default

The default report covers Wayland `ext-data-control`/`wlr-data-control`
selection reachability, empty/non-empty state, MIME count, X11 `CLIPBOARD`
reachability and owner state, missing-protocol/seat/display/tool remediation,
and the Phase 0 privacy boundary. Clipboard text, images, file lists, MIME
names, owner IDs, socket paths, and application metadata stay private.

## Explicit lifecycle test

`--exercise-lifecycle wayland|x11|all` is separate from discovery. For each
backend it:

1. confirms the standard selection is empty using metadata only;
2. starts a backend-restricted child owning a generated marker briefly;
3. checks marker availability while the owner lives and after it exits;
4. clears only if the current value still matches the marker;
5. reports booleans and lifecycle states while keeping the marker and clipboard
   content private.

An occupied selection skips the test. If another application replaces it,
cleanup preserves the newer value. Only the generated marker is compared in
memory; pre-existing data stays private.

## Clean-session validation

`run-clean-session-lifecycle.sh` provides non-interactive evidence in an
isolated environment: X11 uses a fresh Xvfb display, Wayland uses headless Sway
with a private `XDG_RUNTIME_DIR`, and neither starts a clipboard manager.

```sh
bash spikes/clipboard/run-clean-session-lifecycle.sh
```

Both runs require the marker while its owner lives, disappearance after owner
exit, and absence at the end. Disappearance is the expected no-manager result:
An opt-in history service must hold data and selection ownership for its
lifetime. If the selection is already empty after exit, cleanup is
`not_required_selection_already_empty`.


## Initial lifecycle and storage contract

The future `clipboard` service must:

- remain disabled by default and avoid persistent storage before opt-in;
- keep ownership independent of UI lifetime;
- process data in memory until explicit history opt-in;
- require item-size, item-count/time, immediate-wipe, and lock/sleep-clear bounds;
- release ownership and zero/drop in-memory history when disabled, stopped, or wiped.

Wayland data-control and X11 provide no reliable standard source-application
identity. Generic per-application or history-manager exclusions therefore cannot
be promised; desktop-specific exclusions need verified identity and enforcement.

## Phase 0 outcome

Wayland requires a compositor advertising `ext-data-control` or
`wlr-data-control`; a Wayland session alone is insufficient. X11 is separate
and may be available through XWayland. A persistent manager may retain a
selection after its owner exits; that is runtime evidence only.
