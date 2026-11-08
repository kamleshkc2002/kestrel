# What's stored and sent

Files go in the standard XDG folders, under a `kestrel` folder;
debug builds use `kestrel-devel` instead.

| Folder | Default path | Holds |
|---|---|---|
| Config | `$XDG_CONFIG_HOME/kestrel` (usually `~/.config/kestrel`) | `config.toml` |
| Data | `$XDG_DATA_HOME/kestrel` (usually `~/.local/share/kestrel`) | captures, scratchpad, shelf, snippets, command ranking |
| State | `$XDG_STATE_HOME/kestrel` (usually `~/.local/state/kestrel`) | logs, and the `setup`, `update-check`, and `last-version` files |
| Videos | `$XDG_VIDEOS_DIR/Kestrel` (usually `~/Videos/Kestrel`; `Kestrel (Development)` in debug builds) | recordings, and trim exports you save there |

The autostart entry is the one file outside these folders: it lives in
`$XDG_CONFIG_HOME/autostart`.

## Kept on disk

| Feature | File or folder | What is in it | Permissions | Limits and retention | What deletes it |
|---|---|---|---|---|---|
| Settings | `config.toml` | Features, window and feature settings, shortcuts, command-bar file roots, scripts and their arguments, link cleanup rules and the automatic preference, device names | 0600 file, 0700 folder | No size limit; replaced atomically whenever a setting changes | Nothing automatic; `install-local.sh --uninstall` keeps it |
| Diagnostic logs | `logs/` | Warnings and errors, and features starting, stopping, or changing capability; paths in your home folder and network addresses are replaced before they are written | 0700 folder, 0600 files | 7 days and 20 MiB by default (`logging.retention_days`, `logging.max_total_megabytes`); a new file each day or at a quarter of the size limit | Files older than the retention period go at startup and every hour; the oldest go first over the size limit; Clear logs deletes them all |
| Screenshots | `captures/` | Original and edited PNG captures | 0700 folder, 0600 files | 30 files, 256 MB, and 168 hours by default (`capture.max_entries`, `capture.max_total_megabytes`, `capture.max_age_hours`) | Age and size limits apply at startup and after each capture; Delete and Clear all remove captures |
| Screenshots from the portal | The file the Screenshot portal returns (in `/tmp` or `$XDG_RUNTIME_DIR`) | The PNG the desktop captured | Set by the portal | None | Deleted right after it is read, when it is a temporary file |
| Screen recordings | `Videos/Kestrel/` | VP8/Opus WebM takes, and a hidden `.partial` while one records | The folder uses your default folder permissions (usually 0755); files are 0600 | No automatic retention; recording stops below 512 MiB of free space; the panel lists the newest 10 takes | Delete removes a take; a partial is removed when a recording is cancelled or fails, on quit, and at startup when it is older than 60 seconds — a crash can leave one behind |
| Trim exports | The folder you choose | The kept range as VP8/Opus WebM, written as `.<name>.partial` and renamed once complete | 0600 | None | Cancelling, failing, or quitting removes the partial; the finished export is yours |
| Scratchpad | `scratchpad.json` | Notes with their titles, text, and times, and the open tab | 0600 file, 0700 folder | 20 notes of up to 256 KiB each (`scratchpad.max_notes`, `scratchpad.max_note_bytes`); age removal happens only when `scratchpad.max_age_days` is above 0 | Closing a note, or confirming expired notes; turning the feature off or quitting keeps them |
| Scratchpad, unreadable copy | `scratchpad.json.corrupt-<time>` | A file that could not be read, kept instead of overwriting it | 0600 | None | Only you; it is never deleted automatically |
| Shelf | `shelf.toml` | Links, text, and file locations with their pins and times; never images or file contents. Written only while "Keep across restarts" is on | 0600 file, 0700 folder | 50 entries of up to 1 MiB of text and 16 MiB of images by default (`shelf.max_items`, `shelf.max_text_megabytes`, `shelf.max_image_megabytes`); nothing is written while the switch is off | Turning the switch off deletes it; entry clears, expiry, and removal rewrite it |
| Shelf, unreadable copy | `shelf.toml.corrupt-<time>` | A file that could not be read, kept instead of overwriting it | 0600 | None | Only you; it is never deleted automatically |
| Snippets | `snippets.toml` | Name, folder, trigger, and the text with its `{{…}}` variables | 0600 file, 0700 folder | 64 KiB per snippet by default, up to 1 MiB (`snippets.max_content_bytes`) | Deleting a snippet; turning the feature off keeps the file |
| Command ranking | `command_ranking.toml` | Command IDs, usage counts, and pins; never search text | 0600 file, 0700 folder | 200 commands | Nothing; it is rewritten as you use commands |
| Setup | `setup` | Whether setup was offered, and whether it finished or was skipped | 0600 | One line | Nothing |
| What's new | `last-version` | The version you last ran | 0600 | One line | Nothing |
| Update check | `update-check` | The last attempt and successful-check times, the offered version and its release page, and the version you dismissed | 0600 | A few lines | Nothing |
| Update download | The AppImage's folder | `.kestrel-update-<pid>-<n>-appimage.part` and `.kestrel-update-<pid>-<n>-signature.part`: the AppImage being downloaded and its release signature | 0600 | Until the step ends | Removed when the download is cancelled or fails, and after an install; never read back |
| Previous version | Beside the AppImage: `<AppImage>.previous` | The AppImage the last update replaced | A copy is 0600; a hard link keeps the old file's mode | One previous version, about the AppImage's size | Replaced by the next update, or renamed over the AppImage by Go back |
| Autostart | `$XDG_CONFIG_HOME/autostart/io.github.kamleshkc2002.Kestrel.desktop` (`…Kestrel.Devel.desktop` in debug builds) | `Type`, `Name`, and `Exec` | 0644 under your umask; the folder uses its default permissions | One entry | Turning Autostart off, or `install-local.sh --uninstall` |
| Capture export | The file you choose in Save as | A copy of the capture | Your umask | None | You |
| Clipboard image export | The file you choose in Save as | The retained image, as a PNG, written through a hidden temporary beside it and renamed once complete | Your umask | The clipboard image limit (`clipboard.max_image_bytes`, 64 MiB at most) | You |
| Saved clipboard pins | `clipboard-pins.bin` in the data folder | Pinned **text** entries, encrypted with XChaCha20-Poly1305; the 256-bit key is a Secret Service item in your default collection with `application` and `purpose=clipboard-store` attributes, and the file is a magic and version header, a fresh nonce per write, and the ciphertext of length-prefixed records (pin time, byte length, text). No plaintext and no key file | 0600 file, 0700 folder, replaced atomically | At most the global clipboard count, per-entry, and total-byte bounds, and `clipboard.pinned_max_age_days` (1–365, default 90); unlimited retention is rejected | Delete saved pins, turning the switch off and choosing delete, or a history wipe |
| Note export | The file you choose in Export | The note's text | Your umask | None | You |
| Settings export | The file you choose in Settings → Export | Your settings as TOML, the same contents as `config.toml`, including any command-bar file roots and scripts | Your umask | None | You |

- Older builds may have left a `background-notice` file in the state folder. It is no longer read or written, and it can be deleted.
- Recordings have no automatic retention: they stay until you delete them.
- Files are replaced through a hidden temporary beside them and then renamed, so a crash can leave one behind. It is never read back: the scratchpad removes its own on the next load, screenshots remove theirs when the folder is opened, and the rest can be deleted by hand.
- Turning a feature off does not delete what it stored. Captures, recordings, notes, snippets, ranking, state files, and settings stay until you remove them.
- `install-local.sh --uninstall` removes the installed binary, desktop entry, icon, and autostart entry, but keeps settings and data. Use Settings → Remove Kestrel's data… (or `kestrel --uninstall-preview`) to review and remove them too.
- The recordings folder uses normal folder permissions, so other local users can see take names and sizes even though the files themselves are 0600.

## Flatpak permissions

The Flatpak build asks for one permission per shipped feature and nothing else. The rest of the session stays outside the sandbox, which is why some features report a limit (see [the feature list](features.md#flatpak)).

| Permission | Why it is there |
|---|---|
| `--socket=wayland` | Drawing and input on a Wayland session. |
| `--socket=fallback-x11` | The same on an X11 session. |
| `--share=ipc` | Shared memory the X11 path needs. |
| `--device=dri` | GTK's GPU rendering. |
| `--socket=pulseaudio` | The audio mixer, the microphone controls, and recorded audio tracks. |
| `--share=network` | The opt-in update check and the speed test you start. |
| `--talk-name=org.freedesktop.Notifications` | Desktop alerts, such as a saved screenshot or a finished keep-awake session. |
| `--talk-name=org.freedesktop.FileManager1` | **Show in** from the shelf. |
| `--talk-name=org.kde.StatusNotifierWatcher` | The optional tray entry, registered under the app's own bus name. |
| `--system-talk-name=org.freedesktop.login1` | Keep awake, screen lock, and the clipboard wipe on lock or sleep. |
| `--system-talk-name=org.freedesktop.NetworkManager` | The Wi-Fi quick toggle. |
| `--system-talk-name=org.bluez` | The Bluetooth quick toggle. |
| `--system-talk-name=org.freedesktop.UDisks2` | Eject. |
| `--system-talk-name=org.freedesktop.UPower` | Accessory batteries, and the overlay's battery module. |
| `--filesystem=xdg-videos:create` | Recordings and trim exports in `~/Videos/Kestrel`. |

The build does not ask for home access, writable `/sys`, dconf, the host autostart folder, full device access, or the Flatpak socket that launches host programs. Those would each replace the sandbox for one feature.

Inside the Flatpak, the folders in the table above move under `~/.var/app/io.github.kamleshkc2002.Kestrel/`: settings go in `config/kestrel`, data in `data/kestrel`, state in `.local/state/kestrel`, and the cache in `cache`. Recordings stay in `~/Videos/Kestrel`, which is the one folder the sandbox shares with the host. The app's autostart entry is managed by the desktop's Background portal, not written by the sandbox.

## Removing Kestrel

Settings → Remove Kestrel's data… lists every file and folder Kestrel keeps, with its size, before anything is deleted: the configuration, data, state and cache folders, recordings, the autostart entry, the desktop entry and icon, launcher scripts, and the binary. Kestrel's settings and metadata start selected; your recordings and the binary do not, and a previous AppImage beside it is its own choice too.

Choosing Remove stops monitoring, clipboard ownership, shortcut registration, recording, and other running features first, then deletes only the selected paths and reports each one that could not be removed, with the path to remove by hand. Cancelling changes nothing, and the flow quits after the report. No step asks for root: a package-managed install is left in place and the dialog says to remove the `kestrel` package with your package manager.

`kestrel --uninstall-preview` prints the same list and deletes nothing, including when no window can open.

Files you saved elsewhere through Save as or Export are yours: they are not listed and are not deleted. The recordings folder is deleted as a whole when selected, so move any take you want to keep before selecting it.

## Settings changed on your desktop

- **Dark mode** writes `true` or `false` to `$XDG_CONFIG_HOME/cosmic/com.system76.CosmicTheme.Mode/v1/is_dark` on COSMIC, through a temporary file beside it that is then renamed. Everywhere else it sets the `color-scheme` setting (`prefer-dark` or `default`) through the desktop settings bus.
- **Hidden files** sets `show-hidden-files` for the file manager and `show-hidden` for GTK file choosers.
- **Desktop icons** sets `show-desktop-icons` for the desktop that provides it (GNOME, Nemo, or MATE).
- **Brightness and keyboard light** write a number to `/sys/class/backlight/*/brightness` and `/sys/class/leds/*::kbd_backlight/brightness`.
- **Empty Trash** permanently deletes the files and metadata under `$XDG_DATA_HOME/Trash`; there is no undo. Remote Trash folders are not included.

These are your desktop's settings, so the value persists after quitting, or after you turn the feature off.

## Saved clipboard entries: threat model and decision

**Status: implemented, opt-in.** Memory-only history stays the default. A second switch,
"Keep pinned entries across restarts" (`[clipboard] keep_pinned`), is separate from enabling
history; only pinned entries are saved without it. Turning it off asks whether to delete the
saved pins, and deleting is the default.

**Data classes for 1.0: text only.** Image and file-list pins stay in memory as before and say
"Not saved: only text pins are kept". Saving images and files is deferred as part of the
decision.

**Encryption.** Each write uses XChaCha20-Poly1305 from the RustCrypto implementation with a
fresh random 24-byte nonce drawn from the operating system. The 256-bit key is created on first
save and kept as an item in the desktop Secret Service (`org.freedesktop.secrets` over the
session bus), in the default collection, with attributes
`application=io.github.kamleshkc2002.Kestrel` and `purpose=clipboard-store`. There is no key
file and no plaintext fallback. Plaintext buffers are zeroized after use. If the Secret Service
is absent or locked, saving is unavailable with the remedy "Unlock or install a keyring
(Secret Service) to keep pinned entries"; history keeps working in memory.

**Storage and bounds.** The file is `$XDG_DATA_HOME/kestrel/clipboard-pins.bin`
(`kestrel-devel` for debug builds), written through the app's private atomic writer (0600 file,
0700 folder). Saved pins obey the global history count, per-entry byte, and total-byte limits,
plus `clipboard.pinned_max_age_days` (1–365, default 90), enforced before writing. Unlimited
retention is rejected by validation.

**Recovery.** A truncated or corrupt file, a wrong key or key loss, and an unknown format
version mean the pins are not loaded, history keeps running in memory, and the Clipboard page
shows "Saved pins could not be read" with **Reset** (delete the file and the key) and **Keep**
(leave the file to try later). Partial content is never exposed.

**Wipe.** **Delete saved pins** shows the pinned count and a byte estimate, asks for
confirmation, deletes the file and the Secret Service item (crypto-shredding), verifies the file
is absent, and reports. A history wipe removes the saved copy of every removed pin through the
same path. Turning `keep_pinned` off asks about deleting the saved pins.

**Session events.** Lock and sleep wipe in-memory buffers, pins included; service stop wipes
memory too. Saved pins stay closed while the session is locked or asleep and reload only after
the matching unlock or resume, so a session that starts locked decrypts nothing until it is
unlocked. Deleting the key while held still works.

**Documented limits.** Zeroization covers memory, not swap or core dumps, so encrypt swap if
that matters to you. Saving and loading size every buffer that holds pin text exactly, so none
is reallocated and each is wiped when dropped. A backup of the data folder contains only ciphertext; the key is not in the
backup unless your keyring is. `unlink` on SSD and copy-on-write filesystems cannot guarantee
that blocks are erased, which is why deleting the key is part of the wipe.
Diagnostics and logs record counts and states only, never clipboard content, paths, or key
material.

## Kept in memory only

- **Clipboard history:** memory only, bounded by `clipboard.max_items` (default 100), `max_total_bytes` (default 16 MiB), and `max_age_hours` (default 24), with hard caps of 1,000 entries and 128 MiB. Locking the session, sleeping, quitting, or stopping the feature wipes it, and pinned entries survive Clear but not Wipe. While history runs, each new selection is also taken over so it can be offered back later; that keeps the entry in memory and makes the app the clipboard owner for other applications. If `clipboard.clear_seconds` is set, the live selection is also cleared after that many seconds. A write an application marks as temporary (`x-kde-passwordManagerHint` set to `secret`, or `application/x-kde-onlyReplaceEmpty`) is skipped without reading its text, image, or file data, and the selection it replaced returns as the entry already retained.
- **Link cleanup:** a link typed into the preview is parsed on this computer and nothing is sent anywhere; an automatic clean rewrites the copied link on the clipboard. The local rules and the automatic preference are stored in `config.toml`, and no link is written to disk or to a log.
- **OCR text and QR results:** held in memory until you dismiss the result or run a new job. They are not written to disk. Text read from a recent capture or the screenshot editor is held only while it is copied and is never added to capture history, configuration, or diagnostics. Copy puts them on the clipboard, where clipboard history keeps them if it is on; opening a QR code's web address hands it to your browser.
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
| Keep awake ended | Fixed text only | Your desktop's notification service |
| Tray readouts, for each readout you configure | The label, value, and detail line, which can name a filesystem and mount point, a battery, a sensor, or a GPU | The tray host over the session bus |
| Clipboard re-own, while clipboard history runs | The selection just captured, written back unchanged | The clipboard, and so any application that reads it |
| Keyboard shortcut registration, when `global.shortcuts` starts | The app ID, and each command's ID, description, and preferred key combination | The desktop's GlobalShortcuts portal |
| Audio mixer and microphone controls, while they run | Requests that list and change devices, streams, levels, and defaults | Your sound server, through libpulse |
| Update check, after you turn it on | Once a day, an HTTPS request for the release list; GitHub sees your IP address and curl's user agent | api.github.com |

Nothing else leaves the app without a user action. The sound server connection
goes wherever libpulse is configured to connect: normally a local socket, or a
remote host when `PULSE_SERVER` or your PulseAudio configuration names one.
Apart from that connection and the update check once you turn it on, nothing
contacts a network host without a user action. There is no telemetry or crash
reporting, and `curl` is used only by the update check and the speed test.

**User-triggered** — each starts from a button, menu, command, or shortcut:

- **Get update:** downloads the AppImage and its signature over HTTPS from github.com (redirected to GitHub's file servers), which see your IP address.
- **Speed test:** `curl` makes HTTPS requests to `speed.cloudflare.com`, which sees your IP address. The latency phase and the download fetch data from `__down` and discard it; the upload posts the configured number of zero bytes to `__up`. `curl` runs with `--proto =https` and ignores `~/.curlrc`, but it inherits the app's environment, including any proxy variables.
- **Report a problem:** opens the GitHub issue form in your browser with the app's version, your desktop session (`XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`), and your distribution's name from `/etc/os-release` in the link. GitHub, and the browser, see them.
- **QR Open:** a QR result that is an `http://` or `https://` address with no whitespace opens in your browser, which then contacts that host.
- **Open ports Open:** a TCP listener opens in your browser at `http://127.0.0.1:<port>/` or `http://localhost:<port>/`; only the loopback address is contacted.
- **Command-bar URL and file results:** `xdg-open` is run with the URL or path as an argument; the handler sees it, a URL may leave the computer, and the argument appears in the process list.
- **Opening folders and files:** the log folder, the recordings folder, a take, or a trimmed copy is handed to your desktop as a `file://` address; Save as, Export, and the file choosers write to the path you pick. **Show in** on a shelf file hands the file's `file://` address to your file manager through the session bus; when the desktop's file manager interface is missing, the file's folder address goes to your default folder app instead. No contents are passed.
- **Install a package or open a disk image:** the chosen file's path goes to the default app for its type, which reads it; the file is not copied, changed, or deleted.
- **Shelf file actions:** Move to and Rename change or copy the file itself, using your own permissions; no contents go to another app, and nothing is logged.
- **Application launches and process Restart:** the desktop entry or action is launched by ID, with no URIs or app data passed.
- **Command-bar scripts:** receive the executable and arguments configured for them, inherit the app's environment, and get empty standard input; their bounded output is read back.
- **Snippet insertion:** the expanded snippet text, including any `{{clipboard}}` value, goes to `wtype`, `ydotool`, or `xdotool` on standard input.
- **Paste into the previous application:** the same insertion provider receives only the configured key combination (`ctrl_v`, `ctrl_shift_v`, or `shift_insert`); the entry stays on the clipboard and never reaches the provider's arguments, standard input, or a log.
- **OCR:** the selected image, or a saved capture you copy text from, goes to the local `tesseract` executable on standard input, with the language and mode as arguments.
- **Screen capture on wlroots desktops:** `grim` receives the capture geometry and returns the image on standard output; `slurp` returns the area you drag.
- **Screenshots, recording, and colour picking:** requests go to the desktop's Screenshot, ScreenCast, or colour-picker portal. Recording tells the portal whether you chose a screen or a window and whether the cursor is included, asks it not to remember the choice, and receives the shared frames over PipeWire. Recorded audio is read directly from your sound server: the system and microphone tracks through `pulsesrc`, and a single application's audio through `pipewiresrc` attached to that application's playback streams.
- **Copy actions:** text, an image, or a file list goes to the clipboard, where any application or clipboard manager can read it.
- **The "What's stored and sent" link in Settings:** opens this page on GitHub in your browser.
- **System actions:** Keep awake holds a logind inhibit, quick toggles change Wi-Fi, Bluetooth, unmount or eject, volume, and the desktop settings above, process actions send signals, and Empty Trash deletes local Trash files. They act on the system and carry no app data.

## Logs

See [Diagnostic logs](features.md#diagnostic-logs) for what the log files contain, how
long they are kept, and how to clear them.
