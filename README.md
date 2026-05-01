# TuneTUI

page visits count:

![visitor count](https://count.getloli.com/@:tunetui)

Performance-oriented terminal music player for desktop terminal workflows.

## Documentation

Full documentation available at **https://tunetui.online**

## Features

- Minimal redraw strategy (renders only on dirty state or timed tick)
- Folder-based music library with recursive scan
- Background library scan on startup/add-folder/rescan so the TUI opens without waiting for full tag parsing
- Persistent library index cache in config dir (`library_index.json`) for warm-start metadata reuse
- Playlist create/add/play flows
- Independent shuffle and repeat controls (repeat off, playlist, or single track)
- Main library queue order uses metadata titles (not file names)
- Queue scope follows where you start playback (folder, playlist, or All Songs)
- Library root shortcuts for `[+] Add Directory` and `[+] New Playlist`
- Queue tools in Library/actions: local/shared queue entries in the Library root plus add-to-end/next, remove item, and move item to next actions
- Single-instance behavior on Windows (new launches focus/restore existing app)
- Automatic track advance when a song ends, including while minimized to tray (Windows)
- Persistent state in config dir (`$XDG_CONFIG_HOME/tunetui/state.json` on Linux, `%USERPROFILE%\.config\tunetui\state.json` on Windows)
- Stats sidecar in config dir with metadata-keyed listen events/aggregates
- Keyboard-driven TUI with color-coded categorized actions panel search, recent actions, and overflow scrollbar
- Right-aligned status tabs with direct page keys (`h` Library, `j` Lyrics, `k` Stats, `l` Online)
- Song Info panel renders now-playing embedded album art as cached Unicode color raster
- Stats tab with totals, ASCII charts, and range-filtered recent listen log
- Lyrics tab with live line sync from `.lrc` sidecars or embedded metadata
- Lyrics sidecars stored in config dir (`lyrics/`)
- Split-pane lyrics editor (`Ctrl+e` toggle) with per-line timestamp stamping
- `.txt` to `.lrc` import with fixed-interval timestamp seeding
- Metadata editor for title/artist/album with cover-art copy flows
- Audio quality inspector action with static spectrograph and bitrate-based rating (Unavailable/Red/Yellow/Green/Gold*)
- Audio driver recovery and output speaker selection
- Selected output speaker persists across launches with fallback to default
- Linux TUI sessions suppress backend stderr splash and bias output buffering toward underrun-resistant playback
- Volume level persists across launches
- Playback settings: loudness normalization, crossfade, scrub length, stats tracking, top songs rows, missing-cover fallback template, themes including terminal/system colors on Linux
- Online tab: TCP host/client room sync, room-code handshake, collaborative/host-only modes, shared queue, password-encrypted invite codes
- Clipboard fallback to OSC 52 for SSH
- Auto-save on state-changing actions

## Quick Start

Download `tune.exe` from releases and run. No installation required.

### Adding Music

1. Press `h` to switch to Library page
2. Select `[+] Add Directory`
3. Choose your music folder or type its path

### Basic Controls

| Key | Action |
|-----|--------|
| `↑` `↓` | Navigate tracks |
| `Enter` | Play selected track |
| `Space` | Pause/Resume |
| `n` | Next track |
| `b` | Previous track |
| `d` | Seek forward |
| `a` | Seek backward |
| `m` | Cycle repeat mode |
| `v` | Toggle shuffle |
| `=` `+` | Volume up |
| `-` `_` | Volume down |
| `/` | Open actions panel |
| `h` `j` `k` `l` | Switch pages (Library/Lyrics/Stats/Online) |
| `Ctrl+f` | In Library, focus the search bar |
| type | When search is focused, type to filter tracks globally |
| `Esc` / `Backspace` | Clear search filter (or navigate back when empty) |
| `Ctrl+n` | In Online tab, start shared queue playback / next shared item |
| `Ctrl+l` | In Online tab, leave room |
| `Ctrl+c` | Quit |

Queue views are available as `[QUEUE] Local Queue` and `[QUEUE] Shared Queue` (when online) in the Library root. Queue edit tools remain in the categorized actions panel (`/`).
Use the actions panel entry "View audio quality + spectrograph" to run one-time analysis for the selected track (or now playing).

### Online / Listen Together

A public server is available at **tunetui.online** — anyone can use it to host or join rooms.

**Host a room:**
1. Press `l` for Online page
2. Select `[+] Create Room`
3. Enter room name and optional password
4. Share invite code

**Join a room:**
1. Press `l` for Online page  
2. Press Enter on `[ Show Public Servers ]`, or select `Server / Link` to type a custom server/link
3. Select a room from the directory
4. Enter invite code (and password if needed)

Homeserver, room creation, and password prompts stay embedded in the Online page; use `h`/`j`/`k`/`l` to switch pages.

**Online quick control:**
- In Online tab, `Ctrl+n` starts shared queue playback immediately (or jumps to the next shared item)

Remote users can stream to each other through the room host connection; only the host server ports need to be exposed.

### Lyrics

- `.lrc` sidecar files in config `lyrics/` folder take precedence over embedded
- `Ctrl+e` toggles split-pane editor
- `Ctrl+t` stamps selected line with current playback time

## Run

```bash
cargo run --release
```

Or run the built binary:
```bash
./tune
```

On SSH sessions, TuneTUI auto-sets `TERM=xterm-256color` when `TERM` is missing/`dumb`.
If `TUNETUI_CONFIG_DIR` is not set and `USERPROFILE` is unavailable, TuneTUI auto-falls back to `$HOME/.config/tunetui`.
On Linux, TuneTUI uses a larger output buffer when the device exposes a safe range and suppresses runtime backend stderr while the TUI is active so ALSA underrun recovery messages do not draw over the screen.

## Hosting Your Own Server

**Headless server:**
```bash
tune --host --host-ip 0.0.0.0
```
- Default port: **7878**
- Room port range: **9000-9100** (default)
- `--host-ip` is the address the server binds to. Use `0.0.0.0` to listen on all IPv4 interfaces.
- Headless `--host` writes timestamped server logs to stderr for startup, room creation/cleanup, joins, disconnects, rejected requests, queue/control actions, and stream requests. `--host --app` keeps the TUI path quiet.

**Server + app in one process:**
```bash
tune --host --app --host-ip 0.0.0.0
```

**Custom port/range:**
```bash
tune --host --host-ip 0.0.0.0:9000 --room-port-range 9000-9100
```

**Connect to a server:**
```bash
tune --ip 192.168.1.100   # defaults to port 7878
tune --ip 192.168.1.100:9000
```
- `--ip` is the server address the app connects to.

## Configuration

Config directory:
- **Linux:** `$XDG_CONFIG_HOME/tunetui/` (default `~/.config/tunetui/`)
- **Windows:** `%USERPROFILE%\.config\tunetui\`
- Override: `TUNETUI_CONFIG_DIR` env var

Files:
- `state.json` — Playback state, library, playlists
- `library_index.json` — Cached library metadata/fingerprints used to warm-start the library
- `stats.json` — Listen history and statistics
- `lyrics/` — LRC sidecar files

Themes (actions panel → Theme): Dark, System / Terminal, Pitch Black, Galaxy, Matrix, Demonic, Cotton Candy

The System / Terminal theme uses terminal ANSI/default colors, so Linux desktops that theme the terminal palette, including Omarchy/Hyprland setups, can make TuneTUI follow the active desktop theme without TuneTUI parsing desktop-specific files.

## Fuzzing

```bash
cargo install cargo-fuzz
cargo fuzz run playback_commands
```

## Binary Name

The crate binary is `tune`, so after installing:

```bash
cargo install --path .
tune
```

## Contributor Workflow

- Agent/developer contract: `AGENTS.md`
- Contribution checklist: `CONTRIBUTING.md`
- One-command local verification (Windows):

```powershell
powershell -ExecutionPolicy Bypass -File scripts/verify.ps1
```

- One-command local verification (Linux/macOS):

```bash
bash scripts/verify.sh
```

## License

This project is licensed under the MIT License. See `LICENSE`.
