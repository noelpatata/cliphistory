# cliphistory CLI reference

Every subcommand except [`serve`](#cliphistory-serve) is a one-shot client that
talks to a running daemon over a Unix socket
(`$XDG_RUNTIME_DIR/cliphistory/cliphistory.sock`), prints the answer and exits.
For what the values mean see [configuration.md](configuration.md); for the big
picture see the [README](../README.md).

- [Exit codes](#exit-codes)
- [Command reference](#command-reference)
- [Scripting notes](#scripting-notes)

---

## Exit codes

| Code | Meaning |
|------|---------|
| `0`  | Success |
| `1`  | The daemon reported an error (`error: …`) or the command failed |
| `2`  | No running daemon could be reached (or bad usage) |

---

## Command reference

### `cliphistory serve`

Run the daemon in the foreground: resolves/installs modules, starts the
clipboard module, binds the socket. Let a service manager background it
(`exec-once` in Hyprland, systemd user unit in the README). Starting twice is
refused while an instance is alive.

```sh
cliphistory serve
RUST_LOG=debug cliphistory serve   # verbose troubleshooting run
```

### `cliphistory show`

Open the configured frontend picker over the last 100 entries. Selecting one
pushes it back onto the clipboard (via the clipboard module's write-back) and bumps its
usage counter. Dismissing is never an error.

This is the command to bind a key to:

```ini
bind = SUPER, V, exec, cliphistory show
```

### `cliphistory history [-n LIMIT] [-q QUERY]`

List entries, pinned first, then newest-first.

| Flag | Description |
|------|-------------|
| `-n`, `--limit <N>` | Max entries to print (default: 20) |
| `-q`, `--query <TEXT>` | Substring filter on previews |

```sh
cliphistory history                 # latest 20
cliphistory history -n 100          # more
cliphistory history -q screenshot   # only entries matching "screenshot"
```

Output format: `<pin><id>  <preview>` — `*` marks pinned entries:

```
*    42  [image image/png 412.3KB] 1920x1080
     41  git commit -m "engine: dedupe by sha256"
```

### `cliphistory copy <ID>`

Push a stored entry onto the clipboard without opening a menu.

### `cliphistory remove <ID>`

Delete one entry. Pinned entries can be removed explicitly.

### `cliphistory clear`

Delete all **unpinned** entries. Pins survive on purpose.

### `cliphistory pin <ID>` / `cliphistory unpin <ID>`

Protect an entry from all pruning (`pin`) or release it again (`unpin`).
Pinned entries sort to the top of listings and are marked `*`.

### `cliphistory status`

Quick health summary of the running daemon:

```
pid:       51234
session:   wayland
clipboard module:    clipboard-wayland
frontend:  frontend-generic
entries:   317
db:        /home/you/.local/share/cliphistory/history.db (204800 bytes)
```

### `cliphistory doctor`

Deep diagnostic report: session detection, distro + package-manager hint,
per-tool probe results, module rankings, active modules, storage state and
every installed module with version/capabilities. Run this first when
anything misbehaves.

### `cliphistory discover`

The same discovery ranking as the daemon would perform, but standalone —
no daemon required. Prints which clipboard module/frontend would be picked right now.
Useful before installing anything.

### `cliphistory stop`

Ask the daemon to shut down cleanly (clipboard module stopped, socket removed).

### `cliphistory config init | path | print`

| Subcommand | Behaviour |
|------------|-----------|
| `init`     | Write the fully commented template config. Refuses to overwrite. |
| `path`     | Print the config file location this build uses. |
| `print`    | Dump the *effective* configuration (defaults merged with your file) as JSON. |

### `cliphistory modules list | install | update | remove`

Module management without editing config files:

```sh
cliphistory modules list                          # installed versions + requirements
cliphistory modules install                       # whatever discovery wants
cliphistory modules install clipboard-x11 frontend-generic paster-uinput
cliphistory modules install --force frontend-generic # reinstall even if current
cliphistory modules update                        # chase channel/pins for everything installed
cliphistory modules remove frontend-generic       # also works in dev mode (deletes the local_dir binary)
```

Module kinds: `clipboard-*` (read the system clipboard), `frontend-*`
(render the picker), `paster-*` (replay the paste chord — `paster-uinput`
is kernel-level and covers Wayland, X11 and TTY alike).

Change detection differs by platform: on **Wayland**,
`clipboard-wayland` subscribes to the compositor's data-control events and
uses no CPU while the clipboard is idle — strictly event-driven, no
polling. On **X11**, change events do not exist in the protocol, so
`clipboard-x11` necessarily polls (`POLL_INTERVAL_MS`) — that is an X11
limitation, not wasted work.

`frontend-generic` is the only frontend you need: a **built-in egui window**
(X11 + Wayland, nothing to install). Click an entry, or type in the filter
box and use ↑/↓ + Enter; <kbd>Delete</kbd> removes the focused entry from
history while the picker stays open; <kbd>Ctrl</kbd>+<kbd>P</kbd> toggles
its pin (pinned entries float to the top). Every row also carries pin 📌 /
delete 🗑 buttons at its right edge — press <kbd>→</kbd> to move keyboard
focus onto them (<kbd>←</kbd> steps back) and <kbd>Enter</kbd> to activate
the focused one. The list refreshes live: entries you copy while the picker
is open appear on their own.
<kbd>Ctrl</kbd>+<kbd>Delete</kbd> (or the 🗑 clear-all button, pressed twice
to confirm) clears every **unpinned** entry; Esc dismisses.

Notes:

- `install`/`update` need network unless `[modules].source_url` is a local
  path or `local_dir` dev mode is active.
- After changing modules, restart the daemon to apply:
  `cliphistory stop && cliphistory serve`.
- Downloads are checksum-verified against the release's `manifest.json`;
  failures leave the previous installation untouched.

---

## Scripting notes

- Output is line-oriented and stable; `history` lines always start with
  `<pin><id>  `, so `awk '{print $1}'` yields ids (`*` included — strip with
  `tr -d '*'` when piping).
- Machine-readable effective configuration: `cliphistory config print`.
- The socket speaks newline-delimited JSON (`IpcRequest`/`IpcResponse` in
  `crates/core/src/ipc.rs`) if you want to drive the daemon directly.
- All commands are safe to run concurrently; the daemon serialises access to
  storage.
