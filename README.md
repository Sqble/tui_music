# TuneTUI

Performance-oriented terminal music player for Windows-first workflows.

## Features

- Minimal redraw strategy (renders only on dirty state or timed tick)
- Folder-based music library with recursive scan
- Playlist create/add/play flows
- Playback modes: normal, shuffle, loop playlist, loop single track
- Main library queue order uses metadata titles (not file names)
- Automatic track advance when a song ends, including while minimized to tray
- Persistent state in `%USERPROFILE%\\.config\\tunetui\\state.json`
- Keyboard-driven TUI and command mode

## Run

```bash
cargo run --release
```

## Controls

- `Up/Down`: select track
- `Enter`: play selected track
- `Space`: pause/resume
- `n`: next track
- `m`: cycle playback mode
- `r`: rescan folders
- `s`: save state
- `:`: command mode
- `Ctrl+C`: quit

## Command Mode

- `add <path>`
- `playlist new <name>`
- `playlist add <name>`
- `playlist play <name>`
- `library` (return queue to full library)
- `mode <normal|shuffle|loop|single>`
- `save`

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
