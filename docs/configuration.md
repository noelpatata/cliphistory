# cliphistory configuration reference

This is the full reference for `config.toml`. For an overview of what
cliphistory is, how it works and how to build it, read the
[README](../README.md) first.

- [File location](#file-location)
- [Loading behaviour](#loading-behaviour)
- [`[general]`](#general)
- [`[storage]`](#storage)
- [`[discovery]`](#discovery)
- [`[modules]`](#modules)
- [`[frontend]`](#frontend)
- [Complete examples](#complete-examples)
- [Troubleshooting](#troubleshooting)

---

## File location

```
$XDG_CONFIG_HOME/cliphistory/config.toml      # usually ~/.config/cliphistory/config.toml
```

Create a fully commented template with:

```sh
cliphistory config init       # refuses to overwrite an existing file
cliphistory config path       # prints where the file lives
cliphistory config print      # dump the *effective* configuration as JSON
```

## Loading behaviour

- **Missing file** – not an error; every key falls back to its built-in
  default (documented below).
- **Invalid TOML or unknown syntax errors** – the CLI exits with an error
  naming the offending line; fix or delete the file to recover.
- **Unknown keys are ignored**, so configs written for newer versions keep
  older daemons working.
- **Tilde expansion** – any path value accepts `~` and `~/…`, expanded against
  your home directory.
- **Environment overrides** – only logging has an env override (`RUST_LOG`,
  see `[general]`). Everything else comes exclusively from this file.

Changes take effect on daemon restart:

```sh
cliphistory stop && cliphistory serve     # or: systemctl --user restart cliphistory
```

---

## `[general]`

| Key             | Type   | Default | Description |
|-----------------|--------|---------|-------------|
| `log_level`     | string | `"info"` | Daemon log verbosity: `trace`, `debug`, `info`, `warn` or `error`. |
| `auto_paste`    | bool   | `true`  | Replay the paste chord (**Shift+Insert**) into the focused window right after a selection is written back, via the discovered paster module (`paster-uinput` preferred; `paster-wayland` as Wayland-native fallback). |
| `paste_delay_ms`| integer| `150`   | Grace period between clipboard ownership and the injected paste keystroke. |
| `paste_command` | string | *(unset)* | Expert override: run this shell command instead of the paster module. |

The `RUST_LOG` environment variable always wins over this key, e.g.
`RUST_LOG=debug cliphistory serve`. Client commands stay quiet regardless;
logging applies to the daemon process.

---

## `[storage]`

History lives in a single SQLite database. Pinned entries (`cliphistory pin <id>`)
are exempt from **all** pruning.

| Key             | Type    | Default                              | Description |
|-----------------|---------|--------------------------------------|-------------|
| `db_path`       | path    | `$XDG_DATA_HOME/cliphistory/history.db` | Database location. Empty/unset uses the XDG default. |
| `max_entries`   | integer | `500`                                | Keep at most this many entries. Oldest unpinned entries are pruned after each insert. |
| `max_item_size` | integer | `5242880` (5 MiB)                    | Payloads larger than this many bytes are silently skipped (logged at `info`). |
| `max_age_days`  | integer | `0`                                  | Delete unpinned entries older than N days. `0` disables age-based pruning. |
| `thumbnail_size`| integer | `256`                                | Longest edge (px) of cached image previews shown in image-capable frontends. `0` disables thumbnails. Previews live in a `thumbs/` directory next to the database and are pruned together with their entries. |

Notes:

- Duplicate content never creates a second row — re-copying something already
  in history promotes the existing entry to the top instead.
- Images are stored as blobs up to `max_item_size`; raise it if you copy large
  screenshots and want them in history.
- Pruning runs on insert and once at daemon start.

---

## `[discovery]`

Discovery decides *which* clipboard module and frontend modules run on this machine.
It inspects `$XDG_SESSION_TYPE` (falling back to `$WAYLAND_DISPLAY` /
`$DISPLAY`), probes external tools declared by each module manifest, ranks
candidates and picks the winner. See the result any time with:

```sh
cliphistory discover   # without the daemon, dry-run
cliphistory doctor     # full report incl. tool probe results
```

| Key                  | Type   | Default | Description |
|----------------------|--------|---------|-------------|
| `preferred_clipboard`   | string | *(unset)* | Force a clipboard module module id (e.g. `"clipboard-x11"`). It wins over ranking and is downloaded automatically if missing. |
| `preferred_frontend` | string | *(unset)* | Force a frontend module id (e.g. `"frontend-wofi"`). Same semantics. |
| `strict`             | bool   | `false` | When `true`, a candidate whose required tools are missing is skipped entirely instead of being used as a fallback. |

Ranking rules, in order:

1. Configured preference beats everything.
2. Candidates whose requirements are satisfied beat candidates with missing
   tools.
3. Static priority order per kind:
   clipboard modules — `clipboard-wayland` > `clipboard-x11` (session-filtered: a Wayland
   session never considers `clipboard-x11`); frontends —
   `frontend-rofi` > `frontend-wofi` > `frontend-dmenu`.

---

## `[modules]`

How modules are sourced, updated and where they live.

| Key                 | Type     | Default                                        | Description |
|---------------------|----------|------------------------------------------------|-------------|
| `source_url`        | URL/path | `https://github.com/noelpatata/cliphistory`         | Release source queried by the downloader. Accepts any GitHub repository URL, a mirror with identical release layout, or a local directory (`file:///path` or `/path`) containing `manifest.json` plus artifacts. |
| `channel`           | string   | `"stable"`                                     | Which release to track. `"stable"` resolves to the latest published release. Pins override the channel per module. |
| `auto_update`       | bool     | `false`                                        | On daemon start, refresh installed modules when the tracked release differs from what's installed. |
| `install_dir`       | path     | `$XDG_DATA_HOME/cliphistory/modules`              | Installation root. Layout: `<dir>/<module-id>/<tag>/` with a `current` symlink. Ignored when `local_dir` is set. |
| `local_dir`         | path     | *(unset)*                                      | **Dev mode**: use module binaries straight from this directory (e.g. `../target/debug`) and disable all downloading/updating. |
| `platform_override` | string   | *(auto-detected)*                              | Force the release-manifest target triple, e.g. `"x86_64-unknown-linux-musl"` to prefer musl builds on glibc systems. Auto-detection tries `<arch>-unknown-linux-gnu` then `<arch>-unknown-linux-musl`. |
| `[modules.pins]`    | table    | *(empty)*                                      | Pin individual modules to tags: `clipboard-wayland = "v0.1.0"`. A pin is enforced whenever the daemon starts or you run `cliphistory modules update` — a mismatching installed version is replaced. |

Download safety model: artifacts are fetched over HTTPS, verified against the
sha256 checksum recorded in the release's `manifest.json`, staged in a temp
directory, then activated atomically by swapping the `current` symlink. A
failed or tampered download leaves the previous version untouched.

Manual control without editing config:

```sh
cliphistory modules install                       # whatever discovery wants
cliphistory modules install clipboard-wayland frontend-rofi
cliphistory modules install --force frontend-rofi # reinstall current version
cliphistory modules update                        # chase the channel/pins
cliphistory modules remove frontend-dmenu
```

### Using a fork or offline mirror

```toml
[modules]
source_url = "https://github.com/you/cliphistory"        # GitHub layout
source_url = "file:///srv/cliphistory-releases"          # plain directory
```

A local source needs one `manifest.json` (same schema as CI publishes) next
to the artifact files; tags go into subdirectories named after the tag when
you pin them.

### Development loop

```sh
cargo build -p cliphistory-clipboard-wayland -p cliphistory-frontend-rofi
```

```toml
[modules]
local_dir = "~/projects/cliphistory/target/debug"
```

With `local_dir` set, the daemon spawns those binaries directly, prints their
versions as `local`, and never touches the network.

---

## `[frontend]`

| Key           | Type            | Default | Description |
|---------------|-----------------|---------|-------------|
| `extra_args`  | array of strings| `[]`    | Arguments appended to **every** frontend invocation, e.g. rofi themes. They arrive after the literal `run` argument, so frontends treat them as passthrough options. |

```toml
[frontend]
extra_args = ["-theme", "~/.config/rofi/cliphistory.rasi"]
```

Per-invocation extras also work without touching config: anything after
`run` on a frontend binary's command line is forwarded to the menu program.

Module-specific knobs deliberately live inside each module — e.g.
`CLIPWELL_DMENU_BIN` lets `frontend-dmenu` use `bemenu` instead of `dmenu`.
The core knows nothing about module internals; that separation is what keeps
the system modular.

---

## Complete examples

### Arch Linux + Hyprland + rofi (default-ish)

```toml
[general]
log_level = "info"

[storage]
max_entries = 1000

[discovery]
preferred_frontend = "frontend-rofi"

[frontend]
extra_args = ["-theme", "~/.config/rofi/cliphistory.rasi"]
```

Everything else auto-discovers: `clipboard-wayland` is chosen because the session
is Wayland, and both modules are pulled from GitHub Releases on first start.

### X11 desktop, minimal menu

```toml
[discovery]
preferred_clipboard = "clipboard-x11"
preferred_frontend = "frontend-dmenu"
strict = true
```

### Privacy-hardened

```toml
[storage]
max_entries = 50
max_age_days = 7
max_item_size = 1048576
```

### Offline / self-hosted

```toml
[modules]
source_url = "file:///srv/cliphistory-releases"
channel = "stable"
auto_update = false

[modules.pins]
clipboard-wayland = "v0.1.0"
frontend-rofi = "v0.1.0"
```

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `no usable clipboard module module` on start | Run `cliphistory doctor`. Usually missing tools (`xclip`) or no graphical session. Install the tool using the printed distro hint. |
| Wrong module picked | Set `preferred_clipboard` / `preferred_frontend`, then `cliphistory stop && cliphistory serve`. |
| Downloads fail behind a proxy/fork | Point `source_url` at your mirror, or pre-install with `file://` source. |
| musl/glibc mismatch error mentioning targets | Set `modules.platform_override` to the triple that exists in the release manifest. |
| Config change did nothing | The daemon caches config at startup: `cliphistory stop && cliphistory serve`. Verify effective values with `cliphistory config print`. |
| Suspect a broken module binary | `~/.local/share/cliphistory/modules/<id>/<tag>/cliphistory-<id> --manifest` must print valid JSON; reinstall with `cliphistory modules install --force <id>`. |
