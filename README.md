# Kestrel

Kestrel is a local-first, capability-aware utility host for Linux desktops. It
is designed to bring monitoring, audio, clipboard, capture, and automation
workflows into one human-facing application while working alongside an existing
desktop environment.

Kestrel targets a Wayland-first core with X11 compatibility. Its integrations
are capability-driven: each feature reports whether the current desktop,
compositor, portal, service, dependency, hardware, and permission state can
support it.

## Status

Phase 0 capability validation is complete. Phase 1 now has a production
workspace, a versioned non-sensitive configuration boundary, and a normal
GTK/libadwaita command window. Concrete feature integrations are introduced
in their individual Phase 1 slices.

- [Requirements and support contract](docs/REQUIREMENTS.md)
- [Initial architecture](docs/ARCHITECTURE.md)

## Scope

Kestrel aims to provide:

- a unified command and status surface for local desktop utility workflows;
- transparent feature availability, limitations, and remediation steps;
- progressive enhancement through portals, user-session services, and narrow
  desktop or compositor adapters;
- local-first operation without required accounts or telemetry.

Kestrel is not a replacement desktop shell, panel, dock, launcher,
notification daemon, or compositor configuration. It also does not promise
uniform feature support across every Linux desktop; release claims will name
the tested desktop, session capability, and feature scope.

## Workspace

- `apps/kestrel`: GTK/libadwaita composition root, XDG configuration I/O, and
  normal command-surface window.
- `crates/kestrel-core`: UI-agnostic feature and capability model.
- `crates/kestrel-platform`: future platform-adapter boundary; no concrete OS
  adapter is committed yet.
- `crates/kestrel-services`: feature registry and lifecycle boundary; no
  concrete feature service is committed yet.
- `docs/REQUIREMENTS.md`: product boundary, support contract, security, packaging,
  and delivery requirements.
- `docs/ARCHITECTURE.md`: initial process model, crate boundaries, capability
  reporting, and Phase 0 validation strategy.

## Principles

- Local-first, without telemetry or required accounts.
- User-session operation without a root daemon.
- Feature modules that report supported, limited, permission-gated, dependency-gated,
  or unsupported status.
- Explicit tested support instead of claiming uniform behavior across Linux desktops.

## Configuration

Kestrel stores only versioned, non-sensitive preferences in
`$XDG_CONFIG_HOME/kestrel/config.toml` (or `~/.config/kestrel/config.toml`).
Per-feature enablement is keyed by stable feature IDs. Invalid feature settings
are ignored individually and reported in the normal window, so they do not
prevent other features or the command surface from starting.

## Development

### Prerequisites

Kestrel is a Rust workspace. Install a current stable Rust toolchain with `rustup`.
The application requires GTK4 and libadwaita development packages; future D-Bus
and PipeWire integrations will need their corresponding Linux development packages.

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
