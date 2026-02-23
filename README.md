# TuneTUI

Performance-oriented terminal music player for desktop terminal workflows.

## Documentation

Full documentation available at **https://tunetui.online**

## Features

- Minimal redraw strategy (renders only on dirty state or timed tick)
- Folder-based music library with recursive scan
- Playlist create/add/play flows
- Playback modes: normal, shuffle, loop playlist, loop single track
- Main library queue order uses metadata titles (not file names)
- Queue scope follows where you start playback (folder, playlist, or All Songs)
- Single-instance behavior on Windows (new launches focus/restore existing app)
- Automatic track advance when a song ends, including while minimized to tray (Windows)
- Persistent state in config dir (`$XDG_CONFIG_HOME/tunetui/state.json` on Linux, `%USERPROFILE%\.config\tunetui\state.json` on Windows)
- Stats sidecar in config dir with metadata-keyed listen events/aggregates
- Keyboard-driven TUI with actions panel search, recent actions, and overflow scrollbar
- Right-aligned status tabs with `Tab` cycling (Library, Lyrics, Stats, Online)
- Song Info panel renders now-playing embedded album art as cached Unicode color raster
- Stats tab with totals, ASCII charts, and range-filtered recent listen log
- Lyrics tab with live line sync from `.lrc` sidecars or embedded metadata
- Lyrics sidecars stored in config dir (`lyrics/`)
- Split-pane lyrics editor (`Ctrl+e` toggle) with per-line timestamp stamping
- `.txt` to `.lrc` import with fixed-interval timestamp seeding
- Metadata editor for title/artist/album with cover-art copy flows
- Audio driver recovery and output speaker selection
- Selected output speaker persists across launches with fallback to default
- Volume level persists across launches
- Playback settings: loudness normalization, crossfade, scrub length, stats tracking, top songs rows, missing-cover fallback template, themes
- Online tab: TCP host/client room sync, room-code handshake, collaborative/host-only modes, shared queue, password-encrypted invite codes
- Clipboard fallback to OSC 52 for SSH
- Auto-save on state-changing actions

## Quick Start

Download `tune.exe` from releases and run. No installation required.

### Adding Music

1. Press `Tab` to switch to Library tab
2. Press `/` to open actions panel
3. Select "Add directory"
4. Choose your music folder

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
| `m` | Cycle playback mode |
| `=` `+` | Volume up |
| `-` `_` | Volume down |
| `/` | Open actions panel |
| `Tab` | Cycle tabs (Library/Lyrics/Stats/Online) |
| `Ctrl+c` | Quit |

### Online / Listen Together

A public server is available at **tunetui.online** — anyone can use it to host or join rooms.

**Host a room:**
1. `Tab` to Online tab
2. `h` to host
3. Enter room name (optional password)
4. Share invite code

**Join a room:**
1. `Tab` to Online tab  
2. `j` to join
3. Enter server (`tunetui.online`) or room link
4. Enter invite code (and password if needed)

**Online quick control:**
- `Ctrl+n` starts shared queue playback immediately (or jumps to the next shared item)

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

## Hosting Your Own Server

**Headless server:**
```bash
tune --host --ip 0.0.0.0
```
- Default port: **7878**
- Room port range: **9000-9100** (default)

**Server + app in one process:**
```bash
tune --host --app --ip 0.0.0.0
```

**Custom port/range:**
```bash
tune --host --ip 0.0.0.0:9000 --room-port-range 9000-9100
```

**Connect to a server:**
```bash
tune --ip 192.168.1.100   # defaults to port 7878
tune --ip 192.168.1.100:9000
```

## Configuration

Config directory:
- **Linux:** `$XDG_CONFIG_HOME/tunetui/` (default `~/.config/tunetui/`)
- **Windows:** `%USERPROFILE%\.config\tunetui\`
- Override: `TUNETUI_CONFIG_DIR` env var

Files:
- `state.json` — Playback state, library, playlists
- `stats.json` — Listen history and statistics
- `lyrics/` — LRC sidecar files

Themes (actions panel → Playback settings): Dark, Pitch Black, Galaxy, Matrix, Demonic, Cotton Candy

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
