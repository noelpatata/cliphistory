# cliphistory

A lightweight, **modular clipboard history manager** for Linux.

`cliphistory` records everything you copy into a local history database.
Bind a key to `cliphistory show` to browse texts and images you copied earlier
and push any of them back onto the clipboard.

```
┌────────────┐   events    ┌──────────────────────────────────────────┐
│ clipboard-*   │────────────▶│                 core                     │
│ (wayland)  │             │  engine · storage · discovery · plugins  │◀── CLI
│ (x11)      │◀────────────│                                          │
└────────────┘ write-back  └─────────────▲────────────────────────────┘
                                          │ entries
┌────────────┐   selection  ┌─────────────┴───────────┐
│ frontend-* │◀─────────────│      unix socket        │
│ rofi/wofi/ │─────────────▶│                         │
│ dmenu      │              └─────────────────────────┘
└────────────┘
```

## Design goals

- **Lightweight** – single Rust binary, no runtime, SQLite storage, threads
  instead of an async runtime.
- **Modular** – only two things vary between systems: *reading* the clipboard
  and *showing* the history. Both are separate executables ("modules") that
  speak a small JSON protocol (`crates/proto`). The core contains no
  Wayland/X11/menu-toolkit code at all.
- **Self-configuring** – on start, discovery detects your session
  (`XDG_SESSION_TYPE`, `$WAYLAND_DISPLAY`, `$DISPLAY`), probes external tools,
  ranks candidate modules (config preferences win) and downloads exactly the
  ones it needs from GitHub Releases — never a Wayland clipboard module on an X11 box.
- **Configurable** – every behaviour-affecting knob lives in
  `~/.config/cliphistory/config.toml`; true constants live in one Rust file.

## Install / build

Requires a Rust toolchain.

```sh
git clone <this repo> && cd cliphistory
cargo build --release
install -Dm755 target/release/cliphistory ~/.local/bin/cliphistory
```

Modules are fetched automatically from GitHub Releases when the daemon starts.
For development against locally built modules set `modules.local_dir` in the
config (see below).

## Usage

> **Full CLI reference: [docs/cli.md](docs/cli.md)** — every subcommand,
> flags, exit codes and scripting notes.

```sh
cliphistory serve          # run the daemon (use a service manager to background it)
cliphistory show           # open the picker; copies the selected entry
cliphistory history -n 20  # list recent entries
cliphistory copy <id>      # re-copy a specific entry
cliphistory remove <id>    # delete an entry
cliphistory clear          # clear history (pinned survive)
cliphistory pin <id>       # protect from pruning (cliphistory unpin <id> to undo)
cliphistory status         # what's running where
cliphistory discover       # what would discovery pick right now?
cliphistory doctor         # full diagnostic report
cliphistory modules list | install [ids…] | update | remove <id>
cliphistory config init    # write the documented default config
```

### Hyprland example

```ini
# ~/.config/hypr/hyprland.conf
bind = SUPER, V, exec, cliphistory show
exec-once = cliphistory serve
```

### systemd user service

```ini
# ~/.config/systemd/user/cliphistory.service
[Unit]
Description=cliphistory clipboard history daemon

[Service]
ExecStart=%h/.local/bin/cliphistory serve
Restart=on-failure

[Install]
WantedBy=default.target
```

## Configuration

`cliphistory config init` writes `~/.config/cliphistory/config.toml`.

> **Full configuration reference: [docs/configuration.md](docs/configuration.md)** —
> every key, default, discovery semantics, pins/channels, offline mirrors and
> troubleshooting.

```toml
[general]
log_level = "info"            # RUST_LOG overrides

[storage]
max_entries = 500
max_item_size = 5242880       # bytes; larger payloads are ignored
max_age_days = 0              # 0 = keep forever

[discovery]
# preferred_clipboard = "clipboard-wayland"
# preferred_frontend = "frontend-rofi"
strict = false                # fail instead of falling back when tools miss

[modules]
source_url = "https://github.com/noelpatata/cliphistory"  # file:///path works too
channel = "stable"            # latest published release
auto_update = false
# local_dir = "../target/debug"     # dev mode: skip downloads entirely
# platform_override = "x86_64-unknown-linux-musl"

[modules.pins]
# clipboard-wayland = "v0.1.0"   # pin individual modules to tags

[frontend]
extra_args = []               # passed to every frontend invocation
```

## Images & previews

Copying an image stores the actual PNG bytes (never html markup) plus a
cached preview — longest edge configurable via `[storage].thumbnail_size`
(default 256 px) — under `<db dir>/thumbs/<hash>.png`. Image-capable
frontends declare `features = ["images"]` in their manifest and receive the
preview path per entry; `frontend-wofi` renders them natively via
`--allow-images`.

## Auto-paste

With `[general].auto_paste = true` (default), selecting an entry replays
Ctrl+V into whatever window has focus ~150 ms later. Injection is attempted
through, in order:

1. `[general].paste_command` — expert override, run verbatim via `sh`
2. a native paster module:
   - `paster-uinput` — kernel-level virtual keyboard; works on Wayland,
     X11 and TTY. Needs write access to `/dev/uinput` (logind grants it to
     the active seat) and the udev rule from `dist/udev/` so the compositor
     may read the injected device.
   - `paster-wayland` — `zwp_virtual_keyboard_v1`, no special permissions.
3. an external tool on PATH for the detected session (`wtype`, `ydotool`,
   `dotool`, `xdotool`)

Set `auto_paste = false` for copy-only behavior. Run
`cliphistory doctor` to see which mechanism is active and why.

### The paste chord

Paster modules replay a single fixed chord: **Shift+Insert**. It works in
GUI apps (GTK hardcodes it, Chromium/Google Docs honor it via its editor
pipeline) *and* in terminals — where it traditionally pastes the PRIMARY
selection, which is why `clipboard-wayland` claims **both** CLIPBOARD and
PRIMARY on every copy.

One caveat: some terminals bind Shift+Insert to PRIMARY-only paste or not
at all. If auto-paste does nothing in your terminal, point the binding at
the clipboard (alacritty example):

```toml
[[keyboard.bindings]]
key = "Insert"
mods = "Shift"
action = "Paste"
```

Apps needing a different chord are covered by `general.paste_command`.

## Module development guide

A module is any executable answering two commands:

| Invocation        | Behaviour                                                                 |
|-------------------|---------------------------------------------------------------------------|
| `--manifest`      | print one `ModuleManifest` JSON document                                  |
| `run …`           | clipboards: NDJSON frames on stdout, control frames on stdin; frontends: one `ShowRequest` JSON on stdin, one `ShowResponse` JSON on stdout |

Reader frames (`crates/proto/src/lib.rs`):

```
stdout:  {"type":"ready","protocol_version":2}
         {"type":"event","content":{"kind":"text","text":"…"}}
         {"type":"event","content":{"kind":"image","mime":"image/png","data":"<base64>","width":null,"height":null}}
         {"type":"pong"}   {"type":"error","message":"…"}
stdin:   {"type":"ping"}   {"type":"stop"}
         {"type":"set_clipboard","content":{…}}     # write-back capability
```

Manifests declare `capabilities` (`read`/`write`), `features` (e.g.
`images` for frontends that render thumbnails) and `requires` (external
tools probed on PATH during discovery). Module kinds: `clipboard`,
`frontend`, `paster`. Modules keep all their own constants internally; the
core knows nothing about how they work.

Release manifests published by CI look like:

```json
{
  "release": "v0.1.0",
  "protocol_version": 1,
  "targets": {
    "x86_64-unknown-linux-gnu": {
      "core": {"path": "…", "sha256": "…"},
      "modules": [{"id": "clipboard-wayland", "kind": "clipboard", "requires": [],
                   "file": {"path": "…", "sha256": "…" }}]
    }
  }
}
```

The downloader verifies sha256 checksums, installs atomically under
`~/.local/share/cliphistory/modules/<id>/<tag>/` and flips a `current` symlink.
`file://` sources and `modules.local_dir` make offline/dev installs trivial.

## Development

### Toolchain & build

Any recent stable Rust (`rustup` or your distro's `rust` package). The
workspace is plain Cargo — no extra system dependencies to compile; the only
C code is SQLite, bundled via the `rusqlite` `bundled` feature.

```sh
cargo build                        # fast debug build (what you iterate with)
cargo build --release              # optimised binaries in target/release/
cargo build -p cliphistory-core       # just the daemon/CLI
cargo build -p cliphistory-clipboard-wayland -p cliphistory-frontend-rofi   # select modules
```

Binaries produced: `cliphistory` plus one per module
(`cliphistory-clipboard-wayland`, `cliphistory-clipboard-x11`, `cliphistory-frontend-{rofi,wofi,dmenu}`).

### Tests

```sh
cargo test --workspace             # everything: unit + integration tests
cargo test --workspace -- --nocapture          # see log output
cargo test -p cliphistory-core        # core unit + plugin-lifecycle integration tests
cargo test -p cliphistory-proto       # wire-protocol roundtrips
```

What lives where:

| Suite | Location | Covers |
|-------|----------|--------|
| proto roundtrips | `crates/proto/src/lib.rs` | JSON envelopes, base64 images, previews |
| storage | `crates/core/src/storage.rs` | dedup/promotion, pruning vs pins, search |
| discovery | `crates/core/src/discovery.rs` | session detection, ranking order |
| config | `crates/core/src/config.rs` | template parsing, tilde expansion |
| plugins | `crates/core/tests/plugin_lifecycle.rs` | **end-to-end**: fake module served over `file://`, download → sha256 verify → atomic install → spawn → ping/pong → stop → uninstall; tampered-download rejection |

Integration tests need no compositor: they use a shell-script fake module and
a `file://` release source inside a tempdir.

### Lint & format (CI enforces both)

```sh
cargo fmt --all                    # format
cargo fmt --all -- --check         # what CI checks
cargo clippy --workspace --all-targets -- -D warnings
```

### Run against your working tree

Sandboxed end-to-end run without touching your real config/history:

```sh
export XDG_CONFIG_HOME=/tmp/cw/cfg XDG_DATA_HOME=/tmp/cw/data XDG_RUNTIME_DIR=/tmp/cw/run
mkdir -p /tmp/cw/{cfg,data,run}

target/debug/cliphistory discover     # what would be picked on this machine?
RUST_LOG=debug target/debug/cliphistory serve &
target/debug/cliphistory history
```

To iterate on modules without downloads, point the config at your build dir:

```toml
[modules]
local_dir = "~/projects/cliphistory/target/debug"
```

Then restart the daemon after each rebuild — modules are spawned fresh at
startup. You can also exercise any module binary directly:

```sh
./target/debug/cliphistory-clipboard-wayland --manifest      # self-description JSON
./target/debug/cliphistory-frontend-rofi run <<< '{"entries":[{"id":1,"kind":"text","mime":"text/plain","preview":"hello","size_bytes":5,"created_at":0,"use_count":0,"pinned":false}]}'
```

### Before opening a PR

PRs target `dev`; CI runs exactly:

```sh
cargo fmt --all -- --check && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo test --workspace && \
cargo build --workspace
```

## Repository layout

```
crates/
├── proto/                    # wire types + PROTOCOL_VERSION (the contract)
├── core/                     # `cliphistory` binary + library
│   └── src/{constants,config,storage,discovery,plugins,engine,ipc,cli}.rs
└── modules/
    ├── module-common/           # shared NDJSON/chord plumbing for modules
    ├── clipboard-wayland/       # wl-clipboard-rs, no external deps
    ├── clipboard-x11/           # xclip polling
    ├── frontend-common/      # shared menu plumbing
    ├── frontend-rofi/ wofi/ dmenu/
```

## Branching & releases

```
feature/* ──▶ dev ──▶ release/vX.Y ──▶ main   (merge = publish)
```

- PRs target `dev`; CI runs fmt/clippy/tests.
- Stabilisation happens on `release/vX.Y` branches cut from `dev`.
- **Merging `release/vX.Y` into `main` triggers the release workflow**: it
  builds all targets with `cross`, extracts each module's metadata by running
  its native `--manifest`, generates `manifest.json` with sha256 checksums,
  and publishes the GitHub Release tagged from `Cargo.toml`.

## Roadmap ideas

- GTK/TUI frontend modules, image thumbnails in rofi themes
- primary-selection support, entry expiry policies per MIME type
- PKGBUILD / packaging for common distros

License: MIT
