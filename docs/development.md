# Development

## Workspace

- `apps/kestrel`: GTK/libadwaita application, configuration I/O, and window.
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

Debug builds (`cargo run -p kestrel`) run as **Kestrel (Development)** with the ID `io.github.kamleshkc2002.Kestrel.Devel` and separate `kestrel-devel` config and data folders. They run next to the installed copy without sharing settings, captures, or the single-instance bus name. Release builds, including the AppImage, use the regular ID and folders.

## AppImage preview

Tagged versions are published as x86_64 AppImage previews on GitHub Releases.
Ubuntu 24.04 builds bundle GTK4/libadwaita; the matching `.sha256` file verifies
downloads. The AppImage uses the system's GStreamer (present on mainstream
desktops), so screen recording uses the host's plugins.

```bash
bash scripts/build-appimage.sh 0.1.0
```

## Development

To propose a change, start with [`CONTRIBUTING.md`](../CONTRIBUTING.md).

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

With gtk4-layer-shell installed and Sway available, check that the command bar
becomes an overlay (`SWAY_FLAGS=--unsupported-gpu` on NVIDIA drivers):

```bash
bash scripts/layer-shell-smoke.sh target/debug/kestrel
```
