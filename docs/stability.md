# Compatibility promise

From 1.0, the items below stay stable for every 1.x release. Updating within
1.x keeps settings, shortcuts, and scripted commands working.

## What stays stable

- **Feature IDs** such as `audio.mixer` and `clipboard.history`. They appear in
  `config.toml`, presets, shortcuts, logs, and release notes.
- **`config.toml` keys and their meaning.** A value that worked in one 1.x
  release keeps its meaning in the next. The schema version and any change to a
  key are handled by `apps/kestrel/src/config/migrate.rs`.
- **`kestrel --command` IDs** such as `window.show` and
  `keep-awake.start-30m`. `kestrel --list-commands` prints the current set for
  keybinding settings.
- **The on-disk formats written by the app**, which keep their shape across 1.x
  releases:
  - `config.toml`, including the exported copy
  - `shelf.toml` and `snippets.toml`
  - `scratchpad.json` and `command_ranking.toml`
  - capture PNGs and recording WebM files
  - the `setup`, `last-version`, and `update-check` state files
  - the autostart desktop entry

  [What's stored and sent](privacy.md#kept-on-disk) lists every file, its
  contents, and its limits.

## How a change happens

- **Additions** are compatible. A new feature ID, command ID, configuration
  key, or on-disk field does not change what an existing one means, and a
  missing value keeps its default.
- **A renamed or removed configuration key** raises the schema version in
  `apps/kestrel/src/config/migrate.rs` and adds a migration that carries the old
  value to the new key. A file from any released schema loads with its settings
  kept; a valid old key produces no warning.
- **A removed or renamed feature ID or command ID** also needs a migration for
  the places that store it: shortcuts, quick-panel favorites, command ranking,
  and the feature table in `config.toml`.
- **A deprecation** is announced in the release notes one minor release before
  removal. The item keeps working during that release, so there is a full minor
  cycle to move off it.

## Before 1.0

The preview releases are covered from the point the configuration schema was
versioned. A `config.toml` written by any 0.x preview since then loads in 1.0
with its settings kept, covered by the fixtures described below.

## Enforcement

`apps/kestrel/src/stability.rs` snapshots the released feature IDs, command
IDs, and configuration key paths. A test fails when a released entry disappears
and prints any new entry that must be appended to the checked-in list.

`apps/kestrel/tests/fixtures/stability/` holds one `config.toml` per released
schema version. A test loads each file and checks that its settings survive the
migration without warnings.

Changing an ID or key therefore means adding the migration, updating the
stability list, and noting the change in the release notes.
