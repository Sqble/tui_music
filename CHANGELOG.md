# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- Album covers and metadata edits are now saved as ID3v2.3 instead of ID3v2.4, so other apps and OS integrations recognize them ([#36](https://github.com/Sqble/tui_music/issues/36))
- Fixed a malformed APIC frame description terminator that corrupted cover art for strict tag readers ([#36](https://github.com/Sqble/tui_music/issues/36))

## [0.1.0] — beta 1 (unreleased)

First public beta.

### Added
- Terminal music player: library scanning, playback, shuffle/repeat, seek, persistent volume, crossfade, loudness normalization
- Playlists and local/shared queues
- Synced lyrics from embedded metadata or `.lrc` sidecars, with a split-pane lyrics editor
- ASCII album art, listen stats, and an audio quality spectrograph
- Online rooms: host or join, shared queue, password-protected invite codes
- Keyboard and mouse support, action search, themes, tray minimize on Linux
