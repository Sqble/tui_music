# TuneTUI

[![ci](https://img.shields.io/github/actions/workflow/status/Sqble/tui_music/ci.yml?branch=master&label=ci&logo=githubactions)](https://github.com/Sqble/tui_music/actions/workflows/ci.yml)
[![deploy](https://img.shields.io/github/actions/workflow/status/Sqble/tui_music/deploy.yml?branch=master&label=deploy&logo=githubactions)](https://github.com/Sqble/tui_music/actions/workflows/deploy.yml)
[![docs](https://img.shields.io/website?url=https%3A%2F%2Ftunetui.online&label=docs)](https://tunetui.online)
![rust](https://img.shields.io/badge/rust-1.93.1-orange)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

TuneTUI is a fast terminal music player for people who want a real desktop music workflow without leaving the terminal. Point it at your music folder, build playlists, follow synced lyrics, inspect audio quality, and even listen together with friends from the Online tab.

visitor count:

![visitor count](https://count.getloli.com/@:tunetui)

## Documentation

Full documentation is available at **https://tunetui.online**.

## Why TuneTUI?

- **Built for local libraries:** recursively scan folders, cache metadata for fast startup, search across your library, and keep queue order based on track metadata instead of raw file names.
- **Comfortable playback controls:** shuffle, repeat, seek, persistent volume, automatic track advance, output device selection, crossfade, and loudness normalization.
- **Playlists and queues:** create playlists, add tracks quickly, queue items next or at the end, and manage local or shared queues from the Library page.
- **Lyrics:** use embedded lyrics or `.lrc` sidecars, edit timestamps in a split-pane lyrics editor, and import plain text lyrics into timestamped files.
- **Useful listening context:** view listen stats, recent plays, time listening, now-playing metadata, ascii album art, and an audio quality spectrograph.
- **Listen together:** host or join rooms, use a shared queue, share password-protected invite codes, and stream through a public or self-hosted server.
- **Terminal-first polish:** keyboard and mouse support, categorized action search, direct page shortcuts, multiple themes, SSH compatibility, and tray minimize support on desktop environments with a tray host.

## Quick Start

Download `tune.exe` from releases and run it. No installer is required.

You can also build and run from source:

```bash
cargo run --release
```

After installing locally, the binary is named `tune`:

```bash
cargo install --path .
tune
```

## Add Music

1. Press `h` to open the Library page.
2. Select `[+] Add Directory`.
3. Choose your music folder or type its path.

TuneTUI scans in the background, so the interface opens quickly while metadata continues loading. The library cache is reused on later launches.

## Everyday Controls

| Key | Action |
|-----|--------|
| `h` `j` `k` `l` | Switch pages: Library, Lyrics, Stats, Online |
| `↑` `↓` | Navigate |
| `Enter` | Open or play the selected item |
| `Space` | Pause or resume |
| `n` / `b` | Next or previous track |
| `d` / `a` | Seek forward or backward |
| `m` | Cycle repeat mode |
| `v` | Toggle shuffle |
| `r` | Rescan library |
| `=` `+` / `-` `_` | Volume up or down |
| `/` | Open the actions panel |
| `Ctrl+f` | Focus Library search |
| `Esc` | Clear Library search |
| `t` | Minimize or collapse to tray |
| `Ctrl+c` | Quit |

Playlist and queue shortcuts:

| Key | Action |
|-----|--------|
| `Ctrl+p` | Add selected item to a playlist |
| `Ctrl+o` | Add now playing song to a playlist |
| `Ctrl+u` | Add selection to queue end |
| `Ctrl+y` | Add selection to queue next |
| `Ctrl+s` | Add selection to the Online shared queue |

Queue views appear in the Library root as `[QUEUE] Local Queue` and, when online, `[QUEUE] Shared Queue`. The actions panel also includes queue remove/move tools and the audio quality spectrograph action.

## Listen Together

A public server is available at **tunetui.online**. You can use it to host or join rooms without running your own server.

To host a room:

1. Press `l` to open the Online page.
2. Set a nickname if prompted.
3. Show public servers or enter a homeserver/link, then select `[+] Create Room`.
4. Enter a room name and optional password.
5. Share the invite code.

To join a room:

1. Press `l` to open the Online page.
2. Set a nickname if prompted.
3. Press Enter on `[ Show Public Servers ]`, or select `Server / Link` to type a custom server or invite link.
4. Select a room from the directory.
5. Enter the room password if needed.

Online quick controls:

| Key | Action |
|-----|--------|
| `Ctrl+n` | Start shared queue playback or jump to the next shared item |
| `Ctrl+l` | Leave the room |
| `o` | Toggle room mode |
| `q` | Cycle stream quality |
| `t` | Show or hide room codes |
| `2` | Copy the active room link/code |

Remote users can stream to each other through the room host connection; only the host server ports need to be exposed.

## Lyrics

TuneTUI reads synced lyrics from `.lrc` sidecars or embedded metadata. Sidecar lyrics are stored in the config directory under `lyrics/` and take precedence over embedded lyrics.

| Key | Action |
|-----|--------|
| `Ctrl+e` | Toggle the split-pane lyrics editor |
| `Ctrl+t` | Stamp the selected line with the current playback time |

Plain `.txt` lyrics can be imported into `.lrc` with fixed-interval timestamp seeding, giving you a quick starting point for synced lyrics.

## Configuration

Config directory:

| Platform | Default path |
|----------|--------------|
| Linux | `$XDG_CONFIG_HOME/tunetui/`, or `~/.config/tunetui/` |
| Windows | `%USERPROFILE%\.config\tunetui\` |

Set `TUNETUI_CONFIG_DIR` to override the config directory.

Important files:

| File | Purpose |
|------|---------|
| `state.json` | Playback state, library roots, and playlists |
| `library_index.json` | Cached metadata and fingerprints for warm startup |
| `stats.json` | Listen history and aggregate statistics |
| `lyrics/` | LRC sidecar files |

Themes are available from the actions panel: Dark, System / Terminal, Pitch Black, Galaxy, Matrix, Demonic, and Cotton Candy. The System / Terminal theme uses terminal ANSI/default colors, so themed terminal palettes can make TuneTUI follow your desktop theme.

On SSH sessions, TuneTUI auto-sets `TERM=xterm-256color` when `TERM` is missing or `dumb`.

## Host Your Own Server

Run a headless home server:

```bash
tune --host --host-ip 0.0.0.0
```

Defaults:

| Setting | Value |
|---------|-------|
| Home server port | `7878` |
| Room port range | `9000-9100` |

Run the server and app in one process:

```bash
tune --host --app --host-ip 0.0.0.0
```

Use a custom bind port or room range:

```bash
tune --host --host-ip 0.0.0.0:9000 --room-port-range 9000-9100
```

Connect directly to a server:

```bash
tune --ip 192.168.1.100
tune --ip 192.168.1.100:9000
```

Headless `--host` writes timestamped server logs to stderr for startup, room creation/cleanup, joins, disconnects, rejected requests, queue/control actions, and stream requests. `--host --app` keeps the TUI path quiet.

## Audio And Format Notes

TuneTUI uses Symphonia with support for AAC, ADPCM, FLAC, MP3, Ogg/Vorbis, PCM, WAV, and MP4/ISOBMFF audio. On Linux, it uses a larger output buffer when the device exposes a safe range and suppresses runtime backend stderr while the TUI is active so ALSA underrun recovery messages do not draw over the screen.

## Fuzzing

```bash
cargo install cargo-fuzz
cargo fuzz run playback_commands
```

## Contributor Workflow

- Agent/developer contract: `AGENTS.md`
- Contribution checklist: `CONTRIBUTING.md`
- One-command local verification on Linux/macOS: `bash scripts/verify.sh`
- One-command local verification on Windows: `powershell -ExecutionPolicy Bypass -File scripts/verify.ps1`

## License

This project is licensed under the MIT License. See `LICENSE`.
