# Kestrel

Kestrel is a local-first Linux utility suite for monitoring, audio controls,
clipboard workflows, capture tools, automation, and optional desktop-specific
integrations.

It targets a portable Wayland-first core with X11 compatibility and
capability-driven integrations for individual desktops, compositors, and hardware.

## Status

Early architecture scaffold. The requirements baseline and support contract are in
[`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md).

## Workspace

- `apps/kestrel`: application entry point.
- `crates/kestrel-core`: UI-agnostic feature and capability model.
- `docs/REQUIREMENTS.md`: product, architecture, security, packaging, and delivery
  requirements.

## Principles

- Local-first, without telemetry or required accounts.
- User-session operation without a root daemon.
- Feature modules that report supported, limited, permission-gated, dependency-gated,
  or unsupported status.
- Explicit platform support instead of claiming uniform behavior across Linux desktops.

## Development

### Prerequisites

Kestrel is a Rust workspace. Install a current stable Rust toolchain with `rustup`.
The initial scaffold has no native dependencies; the planned GTK4/libadwaita, D-Bus, and
PipeWire integrations will need the corresponding Linux development packages.

On Debian/Ubuntu-derived distributions:

```bash
sudo apt update
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libdbus-1-dev libpipewire-0.3-dev
```


Install Rust if `cargo --version` is unavailable:

```bash
sudo apt install rustup
source "$HOME/.cargo/env"
rustup default stable
rustup component add rustfmt clippy
```
If `cargo` is unavailable in a shell after installation, load Rustup's
environment before running the commands:

```bash
source "$HOME/.cargo/env"
```

Open a new shell after the Rust setup completes, then verify the toolchain:

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

## License

MIT. See [`LICENSE`](LICENSE).
