# TuneTUI

Performance-oriented terminal music player for desktop terminal workflows.

## Features

- Minimal redraw strategy (renders only on dirty state or timed tick)
- Folder-based music library with recursive scan
- Playlist create/add/play flows
- Playback modes: normal, shuffle, loop playlist, loop single track
- Main library queue order uses metadata titles (not file names)
- Queue scope follows where you start playback (folder, playlist, or All Songs)
- Single-instance behavior on Windows (new launches focus/restore existing app)
- Automatic track advance when a song ends, including while minimized to tray (Windows)
- Persistent state in config dir (`$XDG_CONFIG_HOME/tunetui/state.json` on Linux, `%USERPROFILE%\\.config\\tunetui\\state.json` on Windows)
- Stats sidecar in config dir (`$XDG_CONFIG_HOME/tunetui/stats.json` on Linux, `%USERPROFILE%\\.config\\tunetui\\stats.json` on Windows) with metadata-keyed listen events/aggregates (normalized title-first merge across local/online sources), automatic migration from legacy path-keyed totals, and provider-ID pinning for stable online mapping
- Keyboard-driven TUI with actions panel search, recent actions (session-local, last 3), and overflow scrollbar
- Right-aligned status tabs with `Tab` cycling (Library, Lyrics, Stats, Online)
- Song Info panel renders now-playing embedded album art as a cached Unicode color raster, with configurable built-in fallback templates for tracks missing embedded art
- Stats tab with totals, ASCII charts, configurable top songs rows (default 10 via Playback settings), and range-filtered recent listen log sized to panel space (looped track replays count as separate plays)
- Lyrics tab with live line sync from `.lrc` sidecars or embedded lyric metadata
- Lyrics sidecars are stored in the config dir lyrics folder (`$XDG_CONFIG_HOME/tunetui/lyrics/` on Linux, `%USERPROFILE%\.config\tunetui\lyrics\` on Windows)
- Split-pane lyrics editor in TUI (`Ctrl+e` toggle in Lyrics tab) with per-line timestamp stamping
- `.txt` to `.lrc` import with fixed-interval timestamp seeding from actions panel
- Metadata editor in actions panel for selected track embedded tags (title/artist/album) with save/clear and cover-art copy flows (selected track, current folder, current playlist, or all songs with confirmation)
- If no lyrics exist, Lyrics tab prompts before creating a new sidecar `.lrc`
- Sidecar-first source precedence (`.lrc` wins over embedded tags when both exist)
- Audio driver recovery and output speaker selection from actions panel
- Selected output speaker persists across launches with fallback to default when unavailable
- Volume level persists across launches in saved state
- Playback settings in actions panel: loudness normalization, crossfade, scrub length cycle (5s/10s/15s/30s/1m), stats tracking toggle, top songs rows, missing-cover fallback template (Music Note), and themes (Dark, Pitch Black, Galaxy, Matrix, Demonic, Cotton Candy)
- Middle status area uses separate `Timeline` and `Control` panels (`Control` shows volume and scrub/adjust hints)
- Actions panel includes "Clear listen history (backup)" to reset stats while preserving a `.bak` snapshot
- Add directory from actions panel via typed path or external folder picker (PowerShell on Windows, zenity/kdialog on Linux)
- Remove directory from actions panel
- Online tab direct TCP host/client room sync: room-code handshake, host-only vs collaborative mode, shared queue updates (global FIFO consume), shared-queue auto-start when idle, last-player-or-host authority for end-of-song auto-advance, shared queue priority when local queue songs end, stop-at-end when shared queue is exhausted, sub-second periodic playback-state sync (track/position/pause plus metadata/provider ID for stats identity, including ping-compensated target position and a small drift deadzone to avoid micro-seeks, plus null-audio host fallback), periodic measured ping RTT updates, ping-timeout peer cleanup for abrupt disconnects, and lossless bidirectional file streaming fallback (host->listener and host<-listener over the same session socket)
- Invite code is password-encrypted with checksum validation (secure `T2` format); host sets password first, joiner enters invite then password
- Auto-save on state-changing actions (folders, playlists, playback settings, theme, mode, output)

## Run

```bash
cargo run --release
```

On SSH sessions, TuneTUI auto-sets `TERM=xterm-256color` when `TERM` is missing/`dumb`.
If `TUNETUI_CONFIG_DIR` is not set and `USERPROFILE` is unavailable, TuneTUI auto-falls back to `$HOME/.config/tunetui`.

## Controls

- `Up/Down`: select track
- `Enter`: play selected track
- `Backspace`: go back in library navigation
- `Space`: pause/resume
- `n`: next track
- `b`: previous track
- `a` / `d`: scrub backward/forward by configured scrub length
- `m`: cycle playback mode
- `=`/`-`: volume adjust
- `+`/`_` (Shift): higher precision volume adjust
- `r`: rescan folders
- `s`: save state
- `/`: actions panel
- `Type/Backspace` (actions panel): filter actions by search
- `/` -> `Edit selected track metadata`: edit selected track title/artist/album tags and copy now-playing cover art to selected/folder/playlist/all songs
- `Tab`: cycle header sections (Library/Lyrics/Stats/Online)
- `Left/Right` (Stats tab): move filter focus
- `Enter` (Stats tab): cycle focused range/sort filter
- `Type/Backspace` (Stats tab): live edit artist/album/search filters
- `Shift+Up` (Stats tab): jump back to top filters
- `Ctrl+e` (Lyrics tab): toggle playback view <-> split editor
- `Up/Down` (Lyrics tab): move selected lyric line
- `Enter` (Lyrics edit mode): insert line after selection
- `Ctrl+t` (Lyrics edit mode): stamp selected line with current playback time
- `h` / `j` / `l` (Online tab): host room / join room or browse room directory / leave room
- `o` / `q` (Online tab): toggle room mode / cycle stream quality profile
- `Ctrl+s` (Library tab, while in online room): add selected item to shared queue (track, folder, playlist, or all songs in selection order)
- Join prompt modal: type home-server link/address (supports bare `127.0.0.1:7878/room/name` and `http(s)://...`), `V` paste clipboard, `Enter` continue, `Esc` cancel
- Host flow: `h` asks for room link (`<server>/room/<name>`) and optional `?max=2..32`, then optional password
- Join flow: if link includes room, optional password prompt appears before connect; if link has only server, a searchable room directory modal opens (lock/open + current/max)
- Online delay tuning moved to actions panel: `/` -> `Playback settings` -> `Online sync delay settings` (manual delay, auto-ping, recalibrate, sync correction threshold)
- Clipboard copy falls back to terminal OSC 52 when native clipboard access is unavailable (useful over SSH, including tmux/screen passthrough; terminal/tmux must allow clipboard escape sequences)
- `Ctrl+C`: quit

### Online Networking Defaults

- Home server bind (CLI): `tune --host --ip 0.0.0.0:7878`
- Headless home server: `tune --host --ip 0.0.0.0:7878`
- Home server + app in one process: `tune --host --app --ip 0.0.0.0:7878`
- App-only targeting a home server: `tune --ip 127.0.0.1:7878`
- Password for host/join: optional in TUI (lock icon in room directory indicates password required)
- Nickname: `TUNETUI_ONLINE_NICKNAME` (fallback `USERNAME`/`USER`/`you`)
- Reverse stream safety: peer uploads are only served for shared-queue items owned by that peer and are capped at 1 GiB per file

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
