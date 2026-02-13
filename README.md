# TuneTUI

Performance-oriented terminal music player for Windows-first workflows.

## Features

- Minimal redraw strategy (renders only on dirty state or timed tick)
- Folder-based music library with recursive scan
- Playlist create/add/play flows
- Playback modes: normal, shuffle, loop playlist, loop single track
- Main library queue order uses metadata titles (not file names)
- Queue scope follows where you start playback (folder, playlist, or All Songs)
- Single-instance behavior on Windows (new launches focus/restore existing app)
- Automatic track advance when a song ends, including while minimized to tray
- Persistent state in `%USERPROFILE%\\.config\\tunetui\\state.json`
- Keyboard-driven TUI with actions panel

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
- `m`: cycle playback mode
- `=`/`-`: volume adjust
- `+`/`_` (Shift): higher precision volume adjust
- `r`: rescan folders
- `s`: save state
- `/`: actions panel
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
