# Configuration

Preferences are stored in `$XDG_CONFIG_HOME/kestrel/config.toml` (or
`~/.config/kestrel/config.toml`), written atomically as a 0600 file in a 0700
folder. Feature enablement uses stable IDs. Invalid settings are ignored
individually and reported in the window. Export contains only typed
application-owned preferences; it excludes clipboard content, runtime
snapshots, credentials, and resolved executable paths. See
[What's stored and sent](privacy.md#whats-stored-and-sent).

Feature IDs, `config.toml` keys, and `kestrel --command` IDs are covered by the
[compatibility promise](stability.md): a rename or removal goes through a
schema migration and keeps files written by earlier releases readable.

`[ui] close_action` is `ask` (the default), `minimize`, or `quit`. With `ask`,
the first close asks whether to keep running in the background and saves that
answer as `minimize` or `quit`. Files written by older builds contain
`minimize` even when nobody chose it, so the question appears once after the
upgrade.

`[updates] check` is `unasked` (the default), `enabled`, or `disabled`. Overview
asks once and saves either answer, so `unasked` in an existing file shows the
question; setup's final page saves the switch's answer. While `enabled`, the
update check is due once a day from the last attempt and contacts
`api.github.com` for the release list. Settings → Updates shows the state, has
Check now, and can turn the check off.

`[clipboard] keep_pinned` is `false` by default. Setting it to `true` saves pinned **text**
entries across restarts in `$XDG_DATA_HOME/kestrel/clipboard-pins.bin`, encrypted with a key
held in the desktop Secret Service; images and file pins are never saved. `pinned_max_age_days`
is 1–365 (default 90) and unlimited retention is rejected. Without a usable keyring the pins
stay in memory and the Clipboard page shows the remedy. The switch, the saved count and bytes,
Delete saved pins, and Reset appear on the Clipboard page.

`[url_cleanup] automatic` is `false` by default and turns on automatic
cleaning of a single copied link. `extra_global` adds parameter names that
apply to every site, and each `[[url_cleanup.sites]]` entry has `host`,
`enabled`, and `params` as a TOML string list. Parameter names may end in `*`
for a prefix match, and a site host matches its subdomains.
Built-in rules cover common tracking parameters and sites; a configured site
with the same host replaces the built-in rule, so switching one off keeps its
parameter names. Invalid entries are reported and skipped. These rules are
also editable on the Clipboard page.

Essentials, Balanced, and Everything presets change feature enablement in one
reversible operation. The Feature Hub marks each feature as running, needing
attention (a missing dependency or permission, or a fix to apply), unavailable
here, or off, with one line saying its state or what it needs. Hovering a
feature's (i) icon shows how much power it uses in the background and how often
it wakes up, its backend, and its state. A feature that
cannot run here (unsupported, or missing a dependency or permission) can be
turned off but not on until it becomes available. Tools on the Capture and
Notes pages work the same way in place: a tool that is off says what it does
and has a Turn on button, and one that cannot run here gives its reason. The
hub lists only features that can be switched; the main window is always on and
is not listed.

The Overview page's Tools row has a one-click button for every feature that is
on and can run here (the capture tools, camera preview, scratchpad, snippets,
clipboard history, command bar, monitoring, a speed test, the audio mixer, and
processes), and a link to the Feature Hub for the rest. Each button takes you
to its section: the capture tools open their selection view, the speed test
starts on the Monitoring page where its progress shows, and the others open
their page scrolled to the feature.

The window lists its pages in a sidebar (Overview, Monitoring, Audio, Clipboard,
Shelf, Capture, Notes, Processes, Command bar, Shortcuts, Feature hub, and
Settings) and shows one at a time. Each feature's settings sit on its own page,
and the Settings page keeps app-wide choices. The sidebar search filters pages
and settings together. On narrow windows the sidebar and the page take turns.

`shelf` holds the shelf's bounds, whether its entries survive a restart, and
whether new screenshots join it: `keep_across_restarts` (default `false`),
`add_screenshots` (default `false`), `max_items` (1–200; default 50),
`max_text_megabytes` (1–4; default 1), `max_image_megabytes` (1–64; default 16),
and `max_age_hours` (0–720; default 0, never). The switches on the Shelf page
write `keep_across_restarts` and `add_screenshots`; the bounds are edited here.
See [Shelf](features.md#shelf--filesshelf).

- **First launch:** when no configuration file exists yet, setup offers a preset
  and explains what each feature it turns on uses and keeps; anything you
  switch off there is left off. Closing setup early turns nothing on and it
  opens again on the next launch, Skip setup records it as done without
  changing anything, and Settings → Run setup again reopens it. The preset
  change it applies is undone like any other.

## Flatpak

Inside the Flatpak, a command from a terminal or a desktop shortcut is

```bash
flatpak run io.github.kamleshkc2002.Kestrel --command <id>
```

where `<id>` is the same ID that `--list-commands` prints, for example
`flatpak run io.github.kamleshkc2002.Kestrel --command window.show`. The Flatpak
stores its configuration in `~/.var/app/io.github.kamleshkc2002.Kestrel/config/kestrel/config.toml`,
so edits to `~/.config/kestrel/config.toml` do not reach it. See
[Flatpak permissions](privacy.md#flatpak-permissions) and
[the feature limits](features.md#flatpak).
