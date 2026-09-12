# Repository Guidelines

## Project Overview

Kestrel is a local-first, capability-aware Linux desktop utility host written in Rust with GTK4/libadwaita. It provides one window for system monitoring, audio, clipboard, and desktop integrations while preserving the existing desktop environment.

- Wayland is primary; X11 is a compatibility path.
- The normal application window is always available. Tray/SNI and global shortcuts are optional enhancements, never the only entry point.
- Support is per capability, not a blanket Linux claim. Keep unavailable features visible with status, evidence, and remediation.
- Run as an unprivileged graphical-session user. Do not add root daemons, setuid helpers, silent privilege escalation, or broad input-device access.
- `docs/REQUIREMENTS.md` defines product/security boundaries; `docs/ARCHITECTURE.md` defines implementation boundaries. Spike findings are evidence, not production dependencies.

## Architecture & Data Flow

The workspace has four production packages with one-way dependency flow:

```text
apps/kestrel -> kestrel-services -> kestrel-platform -> kestrel-core
       |               |                   |                |
   GTK shell      policy/state       Linux adapters    domain contracts
```

The app and services also depend directly on lower layers where needed. Preserve these responsibilities:

- `crates/kestrel-core`: portable models and validation. No GTK, D-Bus, Linux I/O, or runtime policy.
- `crates/kestrel-platform`: read-only capability probes and narrow Linux adapters for PulseAudio, clipboard/logind, `/proc`, and `/sys`. No widgets or product enablement policy.
- `crates/kestrel-services`: feature lifecycle, commands, bounded state, sampling/retention policy, and backend-generic orchestration.
- `apps/kestrel`: composition root, XDG configuration, GTK event loop, view models, window rendering, and optional SNI integration.

Startup flow:

1. `main.rs` loads `$XDG_CONFIG_HOME/kestrel/config.toml` (or `$HOME/.config/kestrel/config.toml).
2. `ApplicationRuntime` registers stable feature descriptors and runs non-interactive probes.
3. `FeatureRegistry` retains every feature and starts only entries that are enabled and `Supported`/`Limited`.
4. Services publish owned snapshots; `view_model.rs` converts registry/config state into presentation data; `window.rs` renders it.

Command flow is typed UI/tray command -> enablement/capability validation -> service -> platform adapter -> structured result/snapshot. Permission denial and missing dependencies are normal states, not process failures.

Stable namespaced feature IDs such as `audio.mixer`, `clipboard.history`, and `system.monitor` join configuration, probes, registry state, services, and UI. Changing an ID requires a configuration migration. A probe report whose ID differs from its `FeatureSpec` is a hard registry error.

Service lifecycle is `register -> probe -> configure -> start -> publish/refresh -> stop`. Stop must deterministically release threads, subscriptions, clipboard ownership, and platform resources. Registry `running` indicates lifecycle gating; use the service snapshot/error for actual resource state.

## Key Directories

- `apps/kestrel/src/`: executable shell and app composition. New end-to-end features usually touch `runtime.rs`, `view_model.rs`, and `window.rs`.
- `crates/kestrel-core/src/`: cross-layer vocabulary, capability reports, feature specs, configuration models, and non-I/O validation.
- `crates/kestrel-platform/src/`: concrete Linux probes/adapters, one module per feature.
- `crates/kestrel-services/src/`: feature policy, state, commands, snapshots, and lifecycle, one module per feature.
- `crates/kestrel-services/tests/`: production-backend integration tests; currently clipboard lifecycle only.
- `spikes/`: disposable feasibility work outside the root Cargo workspace. Do not import spike implementations into production without an explicit adapter decision.
- `scripts/`: packaging automation; currently the AppImage builder.
- `data/`: desktop metadata and icons used by packaging.
- `docs/`: product requirements and architecture contracts.

## Development Commands

Run from the repository root:

```bash
cargo run -p kestrel
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace --all-targets
```

Focused examples:

```bash
cargo test -p kestrel-services
cargo test -p kestrel-services clipboard::tests::stop_releases_owned_selection_and_joins_worker -- --exact
cargo test --manifest-path spikes/audio/Cargo.toml
cargo test --manifest-path spikes/clipboard/Cargo.toml
```

AppImage preview:

```bash
bash scripts/build-appimage.sh 0.1.0
ARCH=aarch64 bash scripts/build-appimage.sh 0.1.0
```

The packaging script performs a locked release build and writes `dist/Kestrel-<version>-<arch>.AppImage` plus its SHA-256 file.

## Code Conventions & Common Patterns

- Rust 2021 syntax; `rustfmt.toml` uses the 2024 style edition, Unix newlines, and default small-item heuristics.
- `snake_case` modules/functions, `UpperCamelCase` types/traits/enums, and exported `FEATURE_ID` constants for stable namespaced IDs.
- Use typed command/result/error enums, not string dispatch or formatted-error matching. Recoverable platform absence belongs in `CapabilityReport`, `Metric::Unavailable`, warnings, or typed errors—not panics.
- Reserve `expect` for asserted built-in invariants. Convert user, configuration, and platform failures with `Result`, `Option`, `map_err`, or feature-level diagnostics.
- Validate policy at constructors/boundaries. Use checked or saturating arithmetic for kernel counters and rates.
- Keep probes read-only and non-interactive. Separate discovery from actions; probes must not open consent UI, mutate configuration, or acquire long-lived resources.
- Dependency injection uses narrow traits: `CapabilityProbe`, `AudioBackend`, `SystemMonitorSource`, `ClipboardBackend`, and `PrivacyEventSource`. Tests use small deterministic fakes, not a mocking framework.
- There is no Tokio/async-std runtime. GTK uses GLib local futures; blocking platform work uses named OS threads and channels. GTK-only state uses `Rc`/`RefCell`/`Cell`; cross-thread runtime state uses `Arc<Mutex<_>>`.
- Pass owned snapshots/view models across boundaries. Keep live backend handles private to their service/runtime owner; widgets must not own service resources.
- Configuration failures are isolated: a malformed feature setting emits a warning and disables/ignores only that feature rather than blocking unrelated startup.
- Privacy is structural: capability evidence is sanitized; clipboard content is bounded, memory-only, zeroized, and never exposed in public snapshots. Preserve lock/sleep/stop wipe behavior and `clear_if_matches` ownership checks.

## Important Files

- `apps/kestrel/src/main.rs`: executable entry, GTK controller, action/channel dispatch.
- `apps/kestrel/src/runtime.rs`: authoritative feature composition, concrete backends, enablement, and runtime operations.
- `apps/kestrel/src/config.rs`: XDG path resolution, schema migration, per-feature warning isolation.
- `apps/kestrel/src/view_model.rs`: domain/service state to owned presentation state.
- `apps/kestrel/src/window.rs`: GTK rendering; no service ownership.
- `apps/kestrel/src/status_notifier.rs`: optional SNI adapter and fallback capability report.
- `crates/kestrel-core/src/lib.rs`: shared capability/configuration contracts and feature-ID validation.
- `crates/kestrel-services/src/lib.rs`: `FeatureRegistry`, registration invariants, lifecycle transitions.
- `crates/kestrel-platform/src/lib.rs`: platform boundary and `CapabilityProbe` seam.
- `Cargo.toml`: root workspace membership.
- `rust-toolchain.toml`, `rustfmt.toml`: pinned compiler/components and formatting policy.
- `.github/workflows/ci.yml`: canonical quality, build, test, spike, and clean-session checks.
- `.github/workflows/appimage.yml`: desktop metadata validation, packaging inspection, and Xvfb startup smoke test.
- `scripts/build-appimage.sh`: release artifact construction.

## Runtime/Tooling Preferences

- Required Rust toolchain: **1.98.0**, minimal profile, with `rustfmt` and `clippy`. Use Cargo; there is no Node/Bun package manager.
- Native compile/link prerequisites include GTK4, libadwaita, and libpulse development packages. CI installs `libadwaita-1-dev libgtk-4-dev libpulse-dev`.
- Prefer standard portals/documented D-Bus APIs, then supported native user-session APIs, then bounded executable adapters with resolved paths, timeouts, bounded output, and structured errors.
- Never infer support solely from desktop/compositor/session names. Report observed backend evidence and remediation.
- Prefer event signals for D-Bus/PipeWire services and bounded configurable polling for `/proc`, `/sys`, and hardware.
- AppImage CI runs on Ubuntu 24.04, validates `data/io.github.kamleshkc2002.Kestrel.desktop`, inspects bundled libraries/assets, verifies checksums, and smoke-starts the artifact under Xvfb.

## Testing & QA

- Tests use Rust's built-in `#[test]` harness. Most live beside code in `#[cfg(test)]` modules; use snake-case behavioral names such as `counter_resets_produce_missing_rates_instead_of_underflow`.
- Assert observable contracts: capability status, lifecycle transitions, snapshots, boundaries, privacy/redaction, cleanup, and real errors. Avoid tests of field forwarding, mock plumbing, or incidental text.
- Use trait fakes, queued deterministic samples, and `tempfile::TempDir` for `/proc`/`/sys` fixtures. Explicitly stop threaded services so workers join.
- Ordinary tests must tolerate unavailable desktop, audio, clipboard, and logind services. Do not assume a particular session backend.
- `cargo test --workspace --all-targets` does not prove production clipboard ownership lifecycle; that integration test is environment-gated.
- Run isolated clipboard QA with:

```bash
KESTREL_REQUIRE_CLEAN_SESSION=1 bash spikes/clipboard/run-clean-session-lifecycle.sh
```

This requires Xvfb, Sway, and `jq`, uses fixed display `:99`, and must not run concurrently. It creates fresh X11 and headless Wayland sessions and never touches the user's live clipboard.
- Phase 0 spike crates are separate workspaces and require explicit `--manifest-path` formatting, clippy, and test commands.
- GUI/package changes require the actual AppImage Xvfb startup scenario from `.github/workflows/appimage.yml`, not only unit tests.
