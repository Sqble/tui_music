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
- Stats sidecar in config dir (`$XDG_CONFIG_HOME/tunetui/stats.json` on Linux, `%USERPROFILE%\\.config\\tunetui\\stats.json` on Windows) with listen events and aggregates
- Keyboard-driven TUI with actions panel
- Right-aligned status tabs with `E` cycling (Library, Lyrics, Stats, Online)
- Stats tab with totals, ASCII charts, top songs, and recent listen log
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
- Auto-save on state-changing actions (folders, playlists, playback settings, theme, mode, output)

## Run

```bash
cargo run --release
```

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
- `Tab`: cycle header sections (Library/Lyrics/Stats/Online)
- `Left/Right` (Stats tab): move filter focus
- `Enter` (Stats tab): cycle focused range/sort filter
- `Type/Backspace` (Stats tab): live edit artist/album/search filters
- `Shift+Up` (Stats tab): jump back to top filters
- `Ctrl+e` (Lyrics tab): toggle playback view <-> split editor
- `Up/Down` (Lyrics tab): move selected lyric line
- `Enter` (Lyrics edit mode): insert line after selection
- `Ctrl+t` (Lyrics edit mode): stamp selected line with current playback time
- `Ctrl+C`: quit

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
