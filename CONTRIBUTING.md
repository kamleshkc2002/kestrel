# Contributing to Kestrel

Thanks for helping. This page is the short path; the linked documents hold the
details, so they stay the single source.

## Before you start

- **What Kestrel is and is not:** [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md)
  sets the product and security boundaries. Kestrel runs as an ordinary user,
  never elevates itself, and keeps unavailable features visible with the reason.
- **How the code is laid out:** [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md),
  and the layer and convention rules in [`AGENTS.md`](AGENTS.md) (core →
  platform → services → app).
- **Set up and build:** [docs/development.md → Development](docs/development.md)
  lists the toolchain, system packages, and the optional gtk4-layer-shell.

## Proposing a change

1. Open an issue describing the change as a story (who wants what, and why)
   with acceptance criteria, or comment on an existing one. Agree on the design
   there before building anything sizeable.
2. Branch from `main` and keep one story per pull request.
3. Open a pull request that says what changed and how you checked it. Changes
   you can see in the window come with a screenshot.

## Before you open the pull request

- Run the four checks from [docs/development.md → Validate the workspace](docs/development.md#validate-the-workspace);
  CI runs the same ones and the AppImage smoke test.
- **Tests:** cover new code by extending an existing test of the same behaviour
  where one fits, and add a test only when none does. Test what a user or
  caller can observe, never field copies or wording. See
  [AGENTS.md → Testing & QA](AGENTS.md#testing--qa).
- **Release notes:** a change users will notice adds one sentence to
  [`data/release-notes.toml`](data/release-notes.toml), tagged with its feature
  ID (or `kestrel` for app-wide changes). Kestrel shows these after an update.
- **Documentation:** update the feature's section in
  [`docs/features.md`](docs/features.md) when behaviour or settings change.
- **Wording in the repository:** describe changes in Kestrel's own terms;
  files, commits, and pull requests never name other products as references,
  and files never contain commit hashes or pull-request links.
- **Privacy:** never log or store clipboard, note, or snippet text, screen-reading
  results, or window titles; see the privacy rules in [AGENTS.md](AGENTS.md).

## License

Contributions are accepted under the project's [MIT license](LICENSE).
