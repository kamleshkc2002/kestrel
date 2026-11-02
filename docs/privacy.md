# What's stored and sent

Files go in the standard XDG folders, under a `kestrel` folder;
debug builds use `kestrel-devel` instead.

| Folder | Default path | Holds |
|---|---|---|
| Config | `$XDG_CONFIG_HOME/kestrel` (usually `~/.config/kestrel`) | `config.toml` |
| Data | `$XDG_DATA_HOME/kestrel` (usually `~/.local/share/kestrel`) | captures, scratchpad, snippets, command ranking |
| State | `$XDG_STATE_HOME/kestrel` (usually `~/.local/state/kestrel`) | logs, and the `setup`, `last-version`, and `background-notice` files |
| Videos | `$XDG_VIDEOS_DIR/Kestrel` (usually `~/Videos/Kestrel`; `Kestrel (Development)` in debug builds) | recordings, and trim exports you save there |

The autostart entry is the one file outside these folders: it lives in
`$XDG_CONFIG_HOME/autostart`.

## Kept on disk

| Feature | File or folder | What is in it | Permissions | Limits and retention | What deletes it |
|---|---|---|---|---|---|
| Settings | `config.toml` | Features, window and feature settings, shortcuts, command-bar file roots, scripts and their arguments, device names | 0600 file, 0700 folder | No size limit; replaced atomically whenever a setting changes | Nothing automatic; `install-local.sh --uninstall` keeps it |
| Diagnostic logs | `logs/` | Warnings and errors, and features starting, stopping, or changing capability; paths in your home folder and network addresses are replaced before they are written | 0700 folder, 0600 files | 7 days and 20 MiB by default (`logging.retention_days`, `logging.max_total_megabytes`); a new file each day or at a quarter of the size limit | Files older than the retention period go at startup and every hour; the oldest go first over the size limit; Clear logs deletes them all |
| Screenshots | `captures/` | Original and edited PNG captures | 0700 folder, 0600 files | 30 files, 256 MB, and 168 hours by default (`capture.max_entries`, `capture.max_total_megabytes`, `capture.max_age_hours`) | Age and size limits apply at startup and after each capture; Delete and Clear all remove captures |
| Screenshots from the portal | The file the Screenshot portal returns (in `/tmp` or `$XDG_RUNTIME_DIR`) | The PNG the desktop captured | Set by the portal | None | Deleted right after it is read, when it is a temporary file |
| Screen recordings | `Videos/Kestrel/` | VP8/Opus WebM takes, and a hidden `.partial` while one records | The folder uses your default folder permissions (usually 0755); files are 0600 | No automatic retention; recording stops below 512 MiB of free space; the panel lists the newest 10 takes | Delete removes a take; a partial is removed when a recording is cancelled or fails, on quit, and at startup when it is older than 60 seconds — a crash can leave one behind |
| Trim exports | The folder you choose | The kept range as VP8/Opus WebM, written as `.<name>.partial` and renamed once complete | 0600 | None | Cancelling, failing, or quitting removes the partial; the finished export is yours |
| Scratchpad | `scratchpad.json` | Notes with their titles, text, and times, and the open tab | 0600 file, 0700 folder | 20 notes of up to 256 KiB each (`scratchpad.max_notes`, `scratchpad.max_note_bytes`); age removal happens only when `scratchpad.max_age_days` is above 0 | Closing a note, or confirming expired notes; turning the feature off or quitting keeps them |
| Scratchpad, unreadable copy | `scratchpad.json.corrupt-<time>` | A file that could not be read, kept instead of overwriting it | 0600 | None | Only you; it is never deleted automatically |
| Snippets | `snippets.toml` | Name, folder, trigger, and the text with its `{{…}}` variables | 0600 file, 0700 folder | 64 KiB per snippet by default, up to 1 MiB (`snippets.max_content_bytes`) | Deleting a snippet; turning the feature off keeps the file |
| Command ranking | `command_ranking.toml` | Command IDs, usage counts, and pins; never search text | 0600 file, 0700 folder | 200 commands | Nothing; it is rewritten as you use commands |
| Setup | `setup` | Whether setup was offered, and whether it finished or was skipped | 0600 | One line | Nothing |
| What's new | `last-version` | The version you last ran | 0600 | One line | Nothing |
| Background notice | `background-notice` | Whether the keep-running notice was shown | 0600 | One line | Nothing |
| Autostart | `$XDG_CONFIG_HOME/autostart/io.github.kamleshkc2002.Kestrel.desktop` (`…Kestrel.Devel.desktop` in debug builds) | `Type`, `Name`, and `Exec` | 0644 under your umask; the folder uses its default permissions | One entry | Turning Autostart off, or `install-local.sh --uninstall` |
| Capture export | The file you choose in Save as | A copy of the capture | Your umask | None | You |
| Note export | The file you choose in Export | The note's text | Your umask | None | You |
| Settings export | The file you choose in Settings → Export | Your settings as TOML, the same contents as `config.toml`, including any command-bar file roots and scripts | Your umask | None | You |

- Recordings have no automatic retention: they stay until you delete them.
- Files are replaced through a hidden temporary beside them and then renamed, so a crash can leave one behind. It is never read back: the scratchpad removes its own on the next load, screenshots remove theirs when the folder is opened, and the rest can be deleted by hand.
- Turning a feature off does not delete what it stored. Captures, recordings, notes, snippets, ranking, state files, and settings stay until you remove them.
- `install-local.sh --uninstall` removes the installed binary, desktop entry, icon, and autostart entry, but keeps settings and data. Full uninstall cleanup is tracked in [issue #178](https://github.com/kamleshkc2002/kestrel/issues/178).
- The recordings folder uses normal folder permissions, so other local users can see take names and sizes even though the files themselves are 0600.

## Settings changed on your desktop

- **Dark mode** writes `true` or `false` to `$XDG_CONFIG_HOME/cosmic/com.system76.CosmicTheme.Mode/v1/is_dark` on COSMIC, through a temporary file beside it that is then renamed. Everywhere else it sets the `color-scheme` setting (`prefer-dark` or `default`) through the desktop settings bus.
- **Hidden files** sets `show-hidden-files` for the file manager and `show-hidden` for GTK file choosers.
- **Desktop icons** sets `show-desktop-icons` for the desktop that provides it (GNOME, Nemo, or MATE).
- **Brightness and keyboard light** write a number to `/sys/class/backlight/*/brightness` and `/sys/class/leds/*::kbd_backlight/brightness`.
- **Empty Trash** permanently deletes the files and metadata under `$XDG_DATA_HOME/Trash`; there is no undo. Remote Trash folders are not included.

These are your desktop's settings, so the value persists after quitting, or after you turn the feature off.

## Kept in memory only

- **Clipboard history:** memory only, bounded by `clipboard.max_items` (default 100), `max_total_bytes` (default 16 MiB), and `max_age_hours` (default 24), with hard caps of 1,000 entries and 128 MiB. Locking the session, sleeping, quitting, or stopping the feature wipes it, and pinned entries survive Clear but not Wipe. While history runs, each new selection is also taken over so it can be offered back later; that keeps the entry in memory and makes the app the clipboard owner for other applications. If `clipboard.clear_seconds` is set, the live selection is also cleared after that many seconds.
- **OCR text and QR results:** held in memory until you dismiss the result or run a new job. They are not written to disk. Copy puts them on the clipboard, where clipboard history keeps them if it is on; opening a QR code's web address hands it to your browser.
- **A colour sample, the screen frame being selected, and the camera preview frame:** one of each at a time; dropped when you confirm, cancel, stop, or close the window.
- **Network addresses:** read only after you press Show and never stored, logged, or exported; dropped on the next refresh or when the feature stops.
- **The speed-test result:** the last measurement is held in memory and lost on quit.
- **The snippet `{{clipboard}}` value:** read once at insert time, clipped to `snippets.clipboard_variable_bytes`, and inserted; it is never written to the snippet file.

Only clipboard history is zeroized: its entries and the live selection it owns are overwritten when they are dropped. OCR and QR text, notes in use, snippet text, frames, and the other values listed here are ordinary memory, freed when they are dropped but not overwritten.

## What leaves this computer or goes to other apps

**Automatic** — no click needed:

| What | Passed along | To whom |
|---|---|---|
| Alert notifications, while monitoring runs and a rule fires | The rule's label and its reading, such as "CPU is at 95%" | Your desktop's notification service |
| Screenshot-saved notification, when `capture.after_screenshot` is `notify` (the default) | "Screenshot saved", and whether it was added to recent captures; Open, Edit, and Copy actions when the service supports actions | Your desktop's notification service |
| Keep awake ended and the keep-running notice | Fixed text only | Your desktop's notification service |
| Tray readouts, for each readout you configure | The label, value, and detail line, which can name a filesystem and mount point, a battery, a sensor, or a GPU | The tray host over the session bus |
| Clipboard re-own, while clipboard history runs | The selection just captured, written back unchanged | The clipboard, and so any application that reads it |
| Keyboard shortcut registration, when `global.shortcuts` starts | The app ID, and each command's ID, description, and preferred key combination | The desktop's GlobalShortcuts portal |
| Audio mixer and microphone controls, while they run | Requests that list and change devices, streams, levels, and defaults | Your sound server, through libpulse |

Nothing else leaves the app without a user action. The sound server connection
goes wherever libpulse is configured to connect: normally a local socket, or a
remote host when `PULSE_SERVER` or your PulseAudio configuration names one.
Apart from that, nothing contacts a network host without a user action. There
is no update check, telemetry, or crash reporting, and the speed test's `curl`
is the app's only network client of its own.

**User-triggered** — each starts from a button, menu, command, or shortcut:

- **Speed test:** `curl` makes HTTPS requests to `speed.cloudflare.com`, which sees your IP address. The latency phase and the download fetch data from `__down` and discard it; the upload posts the configured number of zero bytes to `__up`. `curl` runs with `--proto =https` and ignores `~/.curlrc`, but it inherits the app's environment, including any proxy variables.
- **Report a problem:** opens the GitHub issue form in your browser with the app's version, your desktop session (`XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`), and your distribution's name from `/etc/os-release` in the link. GitHub, and the browser, see them.
- **QR Open:** a QR result that is an `http://` or `https://` address with no whitespace opens in your browser, which then contacts that host.
- **Open ports Open:** a TCP listener opens in your browser at `http://127.0.0.1:<port>/` or `http://localhost:<port>/`; only the loopback address is contacted.
- **Command-bar URL and file results:** `xdg-open` is run with the URL or path as an argument; the handler sees it, a URL may leave the computer, and the argument appears in the process list.
- **Opening folders and files:** the log folder, the recordings folder, a take, or a trimmed copy is handed to your desktop as a `file://` address; Save as, Export, and the file choosers write to the path you pick.
- **Application launches and process Restart:** the desktop entry or action is launched by ID, with no URIs or app data passed.
- **Command-bar scripts:** receive the executable and arguments configured for them, inherit the app's environment, and get empty standard input; their bounded output is read back.
- **Snippet insertion:** the expanded snippet text, including any `{{clipboard}}` value, goes to `wtype`, `ydotool`, or `xdotool` on standard input.
- **OCR:** the selected image goes to the local `tesseract` executable on standard input, with the language and mode as arguments.
- **Screen capture on wlroots desktops:** `grim` receives the capture geometry and returns the image on standard output; `slurp` returns the area you drag.
- **Screenshots, recording, and colour picking:** requests go to the desktop's Screenshot, ScreenCast, or colour-picker portal. Recording tells the portal whether you chose a screen or a window and whether the cursor is included, asks it not to remember the choice, and receives the shared frames over PipeWire. Recorded audio is read directly from your sound server: the system and microphone tracks through `pulsesrc`, and a single application's audio through `pipewiresrc` attached to that application's playback streams.
- **Copy actions:** text, an image, or a file list goes to the clipboard, where any application or clipboard manager can read it.
- **The "What's stored and sent" link in Settings:** opens this page on GitHub in your browser.
- **System actions:** Keep awake holds a logind inhibit, quick toggles change Wi-Fi, Bluetooth, unmount or eject, volume, and the desktop settings above, process actions send signals, and Empty Trash deletes local Trash files. They act on the system and carry no app data.

## Logs

See [Diagnostic logs](features.md#diagnostic-logs) for what the log files contain, how
long they are kept, and how to clear them.
