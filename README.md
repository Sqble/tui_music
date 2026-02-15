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
- Stats sidecar in config dir (`$XDG_CONFIG_HOME/tunetui/stats.json` on Linux, `%USERPROFILE%\\.config\\tunetui\\stats.json` on Windows) with metadata-keyed listen events/aggregates (canonical `Artist+Title`), automatic migration from legacy path-keyed totals, and provider-ID pinning for stable online mapping
- Keyboard-driven TUI with actions panel search, recent actions (session-local, last 3), and overflow scrollbar
- Right-aligned status tabs with `Tab` cycling (Library, Lyrics, Stats, Online)
- Stats tab with totals, ASCII charts, top songs, and recent listen log (looped track replays count as separate plays)
- Lyrics tab with live line sync from `.lrc` sidecars or embedded lyric metadata
- Lyrics sidecars are stored in the config dir lyrics folder (`$XDG_CONFIG_HOME/tunetui/lyrics/` on Linux, `%USERPROFILE%\.config\tunetui\lyrics\` on Windows)
- Split-pane lyrics editor in TUI (`Ctrl+e` toggle in Lyrics tab) with per-line timestamp stamping
- `.txt` to `.lrc` import with fixed-interval timestamp seeding from actions panel
- If no lyrics exist, Lyrics tab prompts before creating a new sidecar `.lrc`
- Sidecar-first source precedence (`.lrc` wins over embedded tags when both exist)
- Audio driver recovery and output speaker selection from actions panel
- Selected output speaker persists across launches with fallback to default when unavailable
- Playback settings in actions panel: loudness normalization, crossfade, scrub length cycle (5s/10s/15s/30s/1m), stats tracking toggle, and themes (Dark, Pitch Black, Galaxy, Matrix, Demonic, Cotton Candy)
- Actions panel includes "Clear listen history (backup)" to reset stats while preserving a `.bak` snapshot
- Add directory from actions panel via typed path or external folder picker (PowerShell on Windows, zenity/kdialog on Linux)
- Remove directory from actions panel
- Online tab direct TCP host/client room sync: room-code handshake, host-only vs collaborative mode, shared queue updates, sub-second periodic playback-state sync (track/position/pause plus metadata/provider ID for stats identity, including ping-compensated target position and a small drift deadzone to avoid micro-seeks, plus null-audio host fallback), periodic measured ping RTT updates, ping-timeout peer cleanup for abrupt disconnects, and lossless bidirectional file streaming fallback (host->listener and host<-listener over the same session socket)
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
- `Tab`: cycle header sections (Library/Lyrics/Stats/Online)
- `Left/Right` (Stats tab): move filter focus
- `Enter` (Stats tab): cycle focused range/sort filter
- `Type/Backspace` (Stats tab): live edit artist/album/search filters
- `Shift+Up` (Stats tab): jump back to top filters
- `Ctrl+e` (Lyrics tab): toggle playback view <-> split editor
- `Up/Down` (Lyrics tab): move selected lyric line
- `Enter` (Lyrics edit mode): insert line after selection
- `Ctrl+t` (Lyrics edit mode): stamp selected line with current playback time
- `h` / `j` / `l` (Online tab): host room / join demo room / leave room
- `o` / `q` (Online tab): toggle room mode / cycle stream quality profile
- `s` (Online tab): add current track to shared queue
- Join prompt modal: type invite, `V` paste clipboard, `Enter` join, `Esc` cancel (persists across tabs)
- Host flow: `h` prompts for room password first, then opens invite modal with code centered and `Copy to clipboard` / `OK` buttons (`Tab`/arrow to select, `Enter` activate, `C` quick-copy)
- Join flow: after invite entry, password prompt appears before connection/decryption
- Online delay tuning moved to actions panel: `/` -> `Playback settings` -> `Online sync delay settings`
- Clipboard copy falls back to terminal OSC 52 when native clipboard access is unavailable (useful over SSH, including tmux/screen passthrough; terminal/tmux must allow clipboard escape sequences)
- `Ctrl+C`: quit

### Online Networking Defaults

- Host bind: `TUNETUI_ONLINE_BIND_ADDR` (default `0.0.0.0:7878`)
- Host advertise address for invite generation: `TUNETUI_ONLINE_ADVERTISE_ADDR` (optional override; auto-detected from bind when omitted)
- Auto-detect prefers public NAT address via STUN (Google STUN) and falls back to local adapter IP when STUN is unavailable
- Room code for join: `TUNETUI_ONLINE_ROOM_CODE` (required; use host invite code)
- Password for host/join: prompted interactively in TUI (required for secure invite decryption/handshake)
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
