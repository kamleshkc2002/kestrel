# Configuration

Preferences are stored in `$XDG_CONFIG_HOME/kestrel/config.toml` (or
`~/.config/kestrel/config.toml`), written atomically as a 0600 file in a 0700
folder. Feature enablement uses stable IDs. Invalid settings are ignored
individually and reported in the window. Export contains only typed
application-owned preferences; it excludes clipboard content, runtime
snapshots, credentials, and resolved executable paths. See
[What's stored and sent](privacy.md#whats-stored-and-sent).

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
Capture, Notes, Processes, Command bar, Shortcuts, Feature hub, and Settings)
and shows one at a time. Each feature's settings sit on its own page, and the
Settings page keeps app-wide choices. The sidebar search filters pages and
settings together. On narrow windows the sidebar and the page take turns.

- **First launch:** when no configuration file exists yet, setup offers a preset
  and explains what each feature it turns on uses and keeps; anything you
  switch off there is left off. Closing setup early turns nothing on and it
  opens again on the next launch, Skip setup records it as done without
  changing anything, and Settings → Run setup again reopens it. The preset
  change it applies is undone like any other.
