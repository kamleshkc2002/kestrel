# Phase 0 spike: clipboard ownership and privacy lifecycle

Disposable Rust probe for Kestrel Issue #5. It is intentionally outside the
main workspace. The package directly probes Wayland data-control through
`wl-clipboard-rs` and X11 selection ownership through `x11rb`; it does not
require `wl-copy`, `wl-paste`, `xclip`, or `xsel`.

```sh
cargo run --quiet --manifest-path spikes/clipboard/Cargo.toml > /tmp/kestrel-clipboard-capabilities.json
cargo run --quiet --manifest-path spikes/clipboard/Cargo.toml -- --exercise-lifecycle all > /tmp/kestrel-clipboard-lifecycle.json
```

## Read-only default

The default command reports only:

- whether Wayland's `ext-data-control` or `wlr-data-control` path can inspect
  the regular selection, its empty/non-empty state, and MIME-type count;
- whether the X11 `CLIPBOARD` selection is reachable and has an owner;
- session-safe remediation for missing protocols, seats, displays, or tools;
- the Phase 0 privacy boundary.

It never fetches or emits clipboard text, images, file lists, MIME type names,
selection owner IDs, socket paths, or application metadata.

## Explicit lifecycle test

`--exercise-lifecycle wayland|x11|all` is deliberately separate from the
read-only probe. For each requested backend it:

1. confirms the standard selection is empty using only selection metadata;
2. starts a child restricted to that backend and makes it own a generated
   marker for a bounded lifetime;
3. checks marker availability while the owner is alive;
4. checks whether a clipboard manager preserved the marker after the owner
   exits;
5. clears only if the current value still matches the generated marker;
6. reports booleans and lifecycle states, never emits the marker or clipboard
   content.

If the selection already has an owner/content, the test is skipped. If another
application replaces the selection while testing, cleanup preserves that newer
selection instead of overwriting it. This keeps the test reversible without
copying user clipboard contents into a report or a file.

An eligible lifecycle test compares only the probe's generated marker in memory
to protect against that race. It reports this fact separately and does not emit,
persist, or inspect pre-existing clipboard data.

## Clean-session validation

`run-clean-session-lifecycle.sh` provides repeatable, non-interactive lifecycle
evidence without accessing a user's desktop session:

- X11 runs on a fresh Xvfb display;
- Wayland runs on headless Sway with a private `XDG_RUNTIME_DIR`;
- neither environment starts a clipboard manager.

Both runs require the generated marker to be available while its owner lives,
unavailable after that owner exits, and absent at the end. A marker that
disappears after owner exit is the expected no-manager result. It establishes
that Kestrel cannot rely on a third-party clipboard manager for persistence:
an opt-in history service must hold the data and selection ownership for its
own lifetime. When the selection is already empty after exit, the report
records cleanup as `not_required_selection_already_empty`.

Run locally only where `Xvfb` and `sway` are installed:

```sh
bash spikes/clipboard/run-clean-session-lifecycle.sh
```

## Initial lifecycle and storage contract

The future `clipboard` service must:

- keep history **disabled by default** and avoid persistent clipboard storage
  until a user explicitly enables it;
- keep an enabled history service alive while it owns or serves clipboard
  data; UI lifetime must not determine clipboard ownership lifetime;
- process captured data in memory only until explicit history opt-in;
- require a maximum item byte size, item-count or time-retention bound,
  immediate wipe command, and clear-on-lock/sleep behavior before any
  persistence is implemented;
- release ownership and zero/drop in-memory history deterministically when
  disabled, stopped, or wiped.

Wayland data-control and X11 selections do not offer a reliable standard
source-application identity. Kestrel must therefore not promise generic
per-application exclusion or history-manager exclusion enforcement. It may
offer a desktop-specific exclusion only after that source identity and its
enforcement have been verified.

## Phase 0 outcome

Wayland capability depends on a compositor advertising `ext-data-control` or
`wlr-data-control`; a Wayland session alone is insufficient. X11 is a separate
compatibility path and may be available through XWayland even when the native
Wayland protocol is absent. A persistent clipboard manager can retain a
selection after the original owner exits, but that behavior is runtime evidence
rather than a Kestrel guarantee.
