---
layout: default
title: Development
---

# Development

## Workspace

- `apps/kestrel`: GTK/libadwaita application, configuration I/O, and window.
- `apps/kestrel-record`: GStreamer recorder helper the app starts on demand.
- `crates/kestrel-core`: UI-agnostic feature and capability model.
- `crates/kestrel-platform`: capability probes and OS/session adapters.
- `crates/kestrel-services`: feature registry and worker lifecycles.
- `docs/REQUIREMENTS.md`: product boundary, support, security, packaging, and delivery.
- `docs/ARCHITECTURE.md`: process model, crate boundaries, and capability reporting.

## Install from source

To use the app day to day from a `main` checkout:

```bash
bash scripts/install-local.sh            # build and install, or update
bash scripts/install-local.sh --restart  # update and restart a running copy
bash scripts/install-local.sh --uninstall
```

The script builds a release binary and installs it to `~/.local/bin/kestrel`, with a desktop entry and icon under `~/.local/share`. Settings and data stay in `~/.config/kestrel` and `~/.local/share/kestrel`.

Debug builds (`cargo run -p kestrel`) run as **Kestrel (Development)** with the ID `io.github.kamleshkc2002.Kestrel.Devel` and separate `kestrel-devel` config and data folders. They run next to the installed copy without sharing settings, captures, or the single-instance bus name. Release builds, including the AppImage, use the regular ID and folders. Screen recording in a debug run needs `cargo build -p kestrel-record` once, so the helper sits beside the binary; without it recording reports that the helper is missing.

## AppImage preview

Tagged versions are published as x86_64 AppImage previews on GitHub Releases.
The build runs in an Ubuntu 22.04 container, so the artifact needs glibc 2.35 or
newer and starts on Ubuntu 22.04, Linux Mint 21, Debian 12, and newer systems.
The container builds GTK 4.14, libadwaita 1.5, glib, wayland, and
gtk4-layer-shell from source with `scripts/build-gtk-stack.sh` and bundles
them; the matching `.sha256` file verifies downloads. The AppImage does not
bundle GStreamer: screen recording uses the host's GStreamer 1.x runtime, while
everything else starts without it and recording reports what to install.

The workflow also builds on `main` pushes. Those runs only fill the GTK stack
and Rust caches that pull requests restore; a `main` push uploads no release,
and only a release tag signs, publishes, and creates the release.

```bash
bash scripts/build-appimage.sh 0.1.0
```

Releases from 0.6.0-preview.6 on also carry a minisign signature
(`Kestrel-<version>-x86_64.AppImage.minisig`), made in the release workflow with
the `MINISIGN_SECRET_KEY` repository secret. The public key is
[`data/release-signing.pub`](https://github.com/kamleshkc2002/kestrel/blob/main/data/release-signing.pub); check a download with:

```bash
minisign -V -p data/release-signing.pub -m Kestrel-<version>-x86_64.AppImage
```

The `.sha256` file only detects a damaged download. The signature shows the
release was made with the project's key.

## Arch package

The project doesn't publish to the AUR; `packaging/aur` is a package that Arch
users build themselves with `makepkg -si`. It holds the `kestrel-desktop` package: a locked release build of
the tagged source, installed under `/usr` with the desktop entry, metainfo,
icon, and the `kestrel-record` helper beside `kestrel`. The AUR name `kestrel`
is an unrelated file-encryption package, so this one installs as
`kestrel-desktop` while the binaries keep their names, and it conflicts with
`kestrel` instead of providing it.

Only GTK4, libadwaita, and libpulse are required: the app starts without
GStreamer, and the helper that records is the only part that links it. The
GStreamer packages are optional dependencies, needed for screen recording.

The package does not self-update; a build that does not run from an
AppImage offers "View release" instead of installing the update.

The package uses the distribution's `rust`; with rustup, cargo reads
`rust-toolchain.toml` and builds with the pinned 1.98.0. To build the checkout
without a release tag:

```bash
cd packaging/aur
KESTREL_LOCAL_SOURCE="$(git rev-parse --show-toplevel)" makepkg -s --noconfirm
```

The AUR workflow uses this override on pull requests and tags, so a pull
request that changes the package does not need a tag to exist.

For a release:

1. Set `pkgver` in `packaging/aur/PKGBUILD` to the tag with the dash before
   `preview` replaced by a dot (`0.7.0-preview.1` becomes `0.7.0.preview.1`).
2. Download the tag archive and write its checksum into `sha256sums`:
   ```bash
   curl -L "https://github.com/kamleshkc2002/kestrel/archive/refs/tags/v0.7.0-preview.1.tar.gz" | sha256sum
   ```
3. Regenerate the metadata and check the package:
   ```bash
   cd packaging/aur
   makepkg --printsrcinfo >.SRCINFO
   bash -n PKGBUILD
   namcap PKGBUILD
   ```
4. Commit the updated `PKGBUILD` and `.SRCINFO` with the release's follow-up
   change, so `makepkg` in a fresh checkout builds the new tag.

## Flatpak package

`packaging/flatpak` holds a GNOME 50 manifest, `cargo-sources.json` for an
offline Cargo build, and a `gtk4-layer-shell` module. GNOME 50 is the runtime
the sandbox behaviour was checked on; the manifest lint reports when a newer
one exists, and moving to it needs a fresh sandbox pass. The Flatpak is a
restricted build: the sandbox blocks some features and the app marks them
Unavailable or Limited with the reason. [Privacy](privacy.md#flatpak-permissions)
lists each permission and the feature that needs it.

The committed manifest names the tagged release as its source, which is the
form Flathub builds. `scripts/flatpak-build.sh` replaces that one source with a
`dir` source from the checkout, so a local build and a pull request compile
their own tree; the rewritten manifest stays in `build/flatpak/`.

### Regenerate the Cargo sources

`packaging/flatpak/cargo-sources.json` vendors every crate in `Cargo.lock` for
the offline build. Regenerate it whenever `Cargo.lock` changes:

```bash
bash scripts/flatpak-sources.sh
```

The script downloads flatpak-builder-tools' cargo generator into a temporary
directory and needs `uv`, `git`, `curl`, and `sed`. CI runs the same script
with `--check` and fails when the committed file is stale.

### Build locally

Install the runtime, SDK, and Rust extension, then build and add the bundle to
your user installation:

```bash
flatpak install flathub org.gnome.Platform//50 org.gnome.Sdk//50 \
  org.freedesktop.Sdk.Extension.rust-stable//25.08
bash scripts/flatpak-build.sh --install
flatpak run io.github.kamleshkc2002.Kestrel --version
```

The script uses `flatpak-builder` when it is installed and the
`org.flatpak.Builder` application otherwise, and needs `jq`. The bundle is
written to `dist/`. Remove it with
`flatpak uninstall io.github.kamleshkc2002.Kestrel`.

`--system-talk-name=org.freedesktop.login1` covers keep awake, screen lock,
and the clipboard wipe; no portal provides those. The linter treats logind
access as an error that needs an exception, so
`packaging/flatpak/linter-exceptions.json` records it for the CI lint. Flathub
reads exceptions from the linter repository, which is the submission step
below.

### Release

The manifest names the release tag it is cut with. Set `tag` to the tag being
created in the release change; the tag is made on that change, so the manifest
inside the tag builds its own tree. Regenerate `cargo-sources.json` in the same
change if `Cargo.lock` moved, and bump the release version and the screenshot
in the metainfo.

The Flatpak workflow builds the bundle from the tagged checkout and uploads it
as an artifact; attach it to the release with
`gh release upload vX.Y.Z Kestrel-X.Y.Z-x86_64.flatpak`.

### Flathub submission

Publishing is a maintainer step; this repository's CI never pushes to Flathub.

1. Fork `flathub/flathub` and create a branch from `new-pr`, not `main`.
2. Add `io.github.kamleshkc2002.Kestrel.yml` at the repository root with the
   release tag source and `cargo-sources.json` next to it. The manifest
   identifies the release by tag, so the submitted copy needs no change.
3. Open a pull request against `flathub/flatpak-builder-lint` adding
   `finish-args-login1-system-talk-name` for the app id to
   `flatpak_builder_lint/staticfiles/exceptions.json`. The reason: keep awake,
   screen lock, and the clipboard wipe use logind, and no portal provides
   them.
4. Open a pull request against the `new-pr` branch and answer the build test
   the bot starts.

Reviewers will ask for:

- the release tag source and a `cargo-sources.json` generated from the same
  `Cargo.lock`;
- screenshots, a release entry with a date, a developer id, and a launchable
  entry in the metainfo;
- the desktop entry, icon, metainfo, and license installed under `/app`;
- the smallest `finish-args` with a reason for each, and no `dir` or local
  sources in the submitted manifest;
- a build that starts without the checkout, which the workflow's bundle run
  already checks by installing and running it.

Only the overview screenshot is referenced today; add feature screenshots
before submitting if the reviewers ask for more.

## Development

To propose a change, start with [`CONTRIBUTING.md`](https://github.com/kamleshkc2002/kestrel/blob/main/CONTRIBUTING.md).

Changing a feature ID, a `config.toml` key, or a `kestrel --command` ID means a
configuration migration plus a stability-list update; see the
[compatibility promise](stability.md).

### Prerequisites

The workspace is pinned to Rust 1.98.0 by `rust-toolchain.toml`; rustup selects
Rust, rustfmt, and Clippy. The application requires GTK4, libadwaita 1.5 or
newer, PulseAudio, D-Bus, PipeWire, and GStreamer development packages.

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
linked against it needs the library at run time. `scripts/install-local.sh`
also finds a copy installed under `~/.local` (Meson `--prefix ~/.local`) and
links it with an rpath, so the installed binary needs no extra setup.

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

Incremental compilation is off for the `dev` and `test` profiles to keep
`target/` small; set `CARGO_INCREMENTAL=1` to turn it on for one command.

Build scripts for the `-sys` crates re-run whenever `PKG_CONFIG_PATH` changes,
so keep it the same across builds, rust-analyzer included (`cargo.extraEnv` in
a local `rust-analyzer.toml`). Then `KESTREL_LAYER_SHELL=off` rebuilds only the
`kestrel` crate, and the build without gtk4-layer-shell shares the same
`target/`. Give each Git worktree its own `target/`: Cargo judges a workspace
crate fresh by file times, so a target shared between checkouts can reuse
artifacts built from another checkout's code.

With gtk4-layer-shell installed and Sway available, check that the command bar
becomes an overlay (`SWAY_FLAGS=--unsupported-gpu` on NVIDIA drivers):

```bash
bash scripts/layer-shell-smoke.sh target/debug/kestrel
```

### Continuous integration

Pull requests run the checks in parallel jobs instead of one serial job:

| Job | What it runs |
| --- | --- |
| `Lint` | `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets`, and the same clippy with `KESTREL_LAYER_SHELL=off` |
| `Quality and test` | `cargo build --workspace --all-targets` and `cargo test --workspace --all-targets` |
| `Idle cost` | builds the debug binary and runs `scripts/idle-measure.sh` against `scripts/idle-baseline.json` |
| `Recovery checks` | builds the debug binary and runs `scripts/recovery-checks.sh` |
| `Clipboard clean session` | `scripts/clipboard-clean-session.sh` |

`Quality and test` is the required build-and-test check. Each job installs only
the packages it needs; the shared Rust toolchain, GTK development packages, and
gtk4-layer-shell build live in `.github/actions/linux-gtk-deps`.

Jobs that compile Rust restore a dependency cache. Only runs on `main` save
it, so a pull request restores the cache that the merged change left behind
rather than rebuilding the dependencies; the first pull request after a
dependency change is still cold. The AppImage workflow also builds on `main`
pushes and saves its GTK stack and Rust caches there, and the Flatpak workflow
does the same for its builder state. Every pull-request workflow has a
concurrency group that cancels a superseded run; `main` and tag runs are never
cancelled.

### Recovery checks

`scripts/recovery-checks.sh` exercises what happens when the session changes
under a running instance. It uses a private headless Sway compositor, private
D-Bus session and system buses, private XDG directories, and a fake logind
under `scripts/fakes/`:

```bash
bash scripts/recovery-checks.sh target/debug/kestrel
```

It needs `sway`, `dbus-daemon`, `gdbus`, `python3-gi`, `wl-copy`, and
`wl-paste`; `pulseaudio` with `pactl` adds the audio scenario. Missing tools
skip the run, and `KESTREL_REQUIRE_RECOVERY_CHECKS=1` makes them fatal, which
is how CI runs it. `SWAY_FLAGS` passes options to Sway.

The checks read feature state through the hidden
`kestrel --command diagnostics.dump-state` query. It prints one tab-separated
line per feature with lifecycle, enablement, capability kind, and typed
counters; clipboard content, device names, and paths never appear. It is not
listed by `--list-commands` and cannot be bound to a shortcut.

Each scenario prints PASS or FAIL with its expected outcome:

1. **Suspend and resume.** The fake logind raises `PrepareForSleep(true)` and
   then `(false)`. Clipboard history is wiped, the live selection is released,
   and an active keep-awake inhibitor is still held after resume.
2. **SIGKILL and restart.** A kept-awake instance is killed with `SIGKILL` and
   restarted. The fake logind sees the inhibitor descriptor close, no
   temporary files remain under Kestrel's directories, no selection is still
   served, and a stale partial recording is removed without appearing as a
   completed take.
3. **Dependency appears and disappears.** `grim` and `slurp` are added to a
   directory on `PATH` after startup. Refresh reports the screenshot provider
   as limited, then supported; removing `grim` returns it to unsupported.
4. **Compositor restart.** Killing the private compositor ends Kestrel with
   status 0 or 1 and never by a signal. A fresh instance runs against a
   restarted compositor.
5. **Audio device disappears and returns.** A PulseAudio null sink is removed
   and another is loaded. Refresh reports the changed output count.
6. **Permission revoked.** The fake logind answers `Inhibit` with
   `AccessDenied`. Keep awake reports `error=permission-denied`, the process
   keeps running, and the action works once access returns.

Scenarios that need a real desktop session stay manual:

- Suspend the machine and resume; clipboard history is wiped and a keep-awake
  session is still held.
- Revoke Kestrel's Screen Capture, ScreenCast, or Global Shortcuts grant in
  the desktop's privacy settings while it runs, then Refresh: the feature
  reports the missing permission instead of stale state.
- Restart the real compositor or its portal service and confirm every open
  surface and capture flow still works.
- Unplug and replug a physical output, microphone, or camera and check the
  mixer, microphone, and recording panels.
- Revoke Logind, NetworkManager, or BlueZ access with polkit while the feature
  is open.
- Kill Kestrel during a real recording and restart it: the hidden `.partial`
  file is removed on a later start and no incomplete take is listed.

## Compatibility table

`docs/compatibility.md` records what has been tested or reported for each
desktop. Capability probes decide support at run time, so the table is
evidence, not a promise.

A feature ID registered in `apps/kestrel/src/runtime/mod.rs` gets a row, with
every desktop Untested until there is evidence.

To update the table from a report:

1. Read the desktop, desktop version, session type, feature ID, and result.
   The compatibility report form asks for all of them; the bug report form
   carries the desktop and session type in their own fields.
2. Open the Feature Hub on a session of that desktop and copy the feature's
   status line. An Unsupported cell uses that reason, such as no
   wlr-layer-shell, no GlobalShortcuts portal, or no data-control.
3. Change the cell:
   - **Tested** needs a version and a date from a maintainer run. A CI or
     QA-script run names the script instead, as the headless Sway cells do.
   - **Reported** needs a link to the issue or comment.
   - **Unsupported** needs a reason that the probe reports on that desktop
     every time. A dependency the user can install, such as Tesseract or a
     portal backend, stays Untested.
   - **Untested** covers everything else.
4. Keep one of those four forms in every cell, and do not infer support from
   a desktop name.
5. If a reason changes, update the matching entry under
   [Known limits](features.md#known-limits).

The Tested cells come from `scripts/clipboard-clean-session.sh`,
`scripts/layer-shell-smoke.sh`, and the workflows that run them. Extending
what CI exercises is the way to add a Tested cell without a person on that
desktop.
