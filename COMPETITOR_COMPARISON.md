# TuneTUI Competitor Comparison

This document compares TuneTUI with terminal music players, Spotify clients, daemons, and adjacent tools listed below. The matrix is intentionally conservative: `Yes` means the feature is documented in the project's primary README or in TuneTUI's local docs as of 2026-05-02. `No` means it was not documented there, even if the project may support it elsewhere.

## Products Compared

| Product | Repository | Main Positioning |
|---|---|---|
| TuneTUI | This repository | Local terminal music player with lyrics, stats, quality tools, tray behavior, and listen-together rooms |
| spotify-tui | https://github.com/rigellute/spotify-tui | Spotify terminal controller using Spotify Web API |
| spotatui | https://github.com/LargeModGames/spotatui | Spotify terminal client with native Spotify streaming, lyrics, visualizer, and Discord RPC |
| spotify-player | https://github.com/aome510/spotify-player | Full-featured Spotify terminal player with native streaming, media control, images, daemon mode, and CLI |
| spotifyd | https://github.com/Spotifyd/spotifyd | Headless Spotify Connect daemon |
| ncspot | https://github.com/hrkfdn/ncspot | ncurses Spotify client using librespot |
| sprofile | https://github.com/goodboyneon/sprofile | TUI for viewing Spotify listening activity |
| termusic | https://github.com/tramhao/termusic | Local music and podcast TUI with online downloads and tag editing |
| kew | https://github.com/ravachol/kew | Local terminal music player focused on private, fast library playback |
| tuisic | https://github.com/Dark-Kernel/tuisic | Online music streaming TUI with downloads, MCP, visualizer, MPRIS, and Discord RPC |
| mocp | https://github.com/jonsafari/mocp | Classic console audio player with detached server and broad format support |
| opencubicplayer | https://github.com/mywave82/opencubicplayer | Demoscene/chiptune/module player with rich visualizers and specialty format support |
| musikcube | https://github.com/clangen/musikcube | Cross-platform terminal music player, library indexer, audio engine, and streaming server |

## Spotify-Focused Matrix

| Feature | TuneTUI | spotify-tui | spotatui | spotify-player | spotifyd | ncspot | sprofile |
|---|---|---|---|---|---|---|---|
| Terminal UI | Yes | Yes | Yes | Yes | No | Yes | Yes |
| Plays local music files | Yes | No | No | No | No | No | No |
| Recursive local library | Yes | No | No | No | No | No | No |
| Persistent local metadata/library cache | Yes | No | No | No | No | No | No |
| Local playlists/queue editing | Yes | No | No | No | No | No | No |
| Spotify catalog browsing/control | No | Yes | Yes | Yes | No | Yes | No |
| Native Spotify audio streaming | No | No | Yes | Yes | Yes | Yes | No |
| Works without Spotify Premium | Yes | No | No | No | No | No | Yes |
| Spotify playlists/library editing | No | Yes | Yes | Yes | No | Yes | No |
| CLI commands for playback/search | No | Yes | Yes | Yes | No | No | No |
| Lyrics display | Yes | No | Yes | Yes | No | No | No |
| Built-in lyrics editor | Yes | No | No | No | No | No | No |
| Metadata editor for local files | Yes | No | No | No | No | No | No |
| Album art in terminal | Yes | No | No | Yes | No | No | No |
| Audio visualizer or spectrograph | Yes | No | Yes | Yes | No | No | No |
| Audio quality rating/inspector | Yes | No | No | No | No | No | No |
| Listen history/activity views | Yes | No | Yes | Yes | No | No | Yes |
| Listen-together rooms/shared queue | Yes | No | No | No | No | No | No |
| Headless/daemon mode | Yes | No | No | Yes | Yes | No | No |
| MPRIS/media-key integration | No | No | Yes | Yes | No | No | No |
| Discord Rich Presence | No | No | Yes | No | No | No | No |
| Tray/minimize while playing | Yes | No | No | No | No | No | No |
| Output device selection/recovery | Yes | Yes | Yes | Yes | No | No | No |
| Crossfade/loudness settings | Yes | No | No | No | No | No | No |
| Online music sources outside Spotify | No | No | No | No | No | No | No |
| Downloads online songs | No | No | No | No | No | No | No |
| Podcasts/RSS | No | No | No | No | No | No | No |
| Mobile/remote streaming client | No | No | No | No | No | No | No |
| MCP/AI integration | No | No | No | No | No | No | No |

## Local And General Player Matrix

| Feature | TuneTUI | termusic | kew | tuisic | mocp | opencubicplayer | musikcube |
|---|---|---|---|---|---|---|---|
| Terminal UI | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Plays local music files | Yes | Yes | Yes | No | Yes | Yes | Yes |
| Recursive local library | Yes | Yes | Yes | No | Yes | Yes | Yes |
| Persistent local metadata/library cache | Yes | Yes | No | No | Yes | No | Yes |
| Local playlists/queue editing | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Spotify catalog browsing/control | No | No | No | No | No | No | No |
| Native Spotify audio streaming | No | No | No | No | No | No | No |
| Works without Spotify Premium | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Spotify playlists/library editing | No | No | No | No | No | No | No |
| CLI commands for playback/search | No | No | Yes | No | Yes | No | No |
| Lyrics display | Yes | Yes | Yes | Yes | No | No | No |
| Built-in lyrics editor | Yes | Yes | No | No | No | No | No |
| Metadata editor for local files | Yes | Yes | No | No | No | Yes | No |
| Album art in terminal | Yes | Yes | Yes | No | No | No | No |
| Audio visualizer or spectrograph | Yes | No | Yes | Yes | No | Yes | No |
| Audio quality rating/inspector | Yes | No | No | No | No | No | No |
| Listen history/activity views | Yes | No | No | No | No | No | No |
| Listen-together rooms/shared queue | Yes | No | No | No | No | No | No |
| Headless/daemon mode | Yes | No | No | Yes | Yes | No | Yes |
| MPRIS/media-key integration | No | Yes | Yes | Yes | No | No | No |
| Discord Rich Presence | No | No | Yes | Yes | No | No | No |
| Tray/minimize while playing | Yes | No | No | No | No | No | No |
| Output device selection/recovery | Yes | No | No | No | No | No | No |
| Crossfade/loudness settings | Yes | No | No | No | No | No | No |
| ReplayGain support | No | No | Yes | No | No | No | No |
| Equalizer | No | No | No | No | Yes | No | No |
| Online music sources outside Spotify | No | Yes | No | Yes | No | Yes | No |
| Downloads online songs | No | Yes | No | Yes | No | No | No |
| Podcasts/RSS | No | Yes | No | No | No | No | No |
| Mobile/remote streaming client | No | No | No | No | No | No | Yes |
| Chiptune/module specialty formats | No | No | No | No | Yes | Yes | Yes |
| MCP/AI integration | No | No | No | Yes | No | No | No |

## TuneTUI-Only Documented Differentiators

| TuneTUI Feature | Competitor Coverage | Why It Matters |
|---|---|---|
| Listen-together rooms with shared queue and encrypted invite codes | No listed competitor documents the same local-file listen-together room model | Gives TuneTUI a social/collaborative feature without depending on Spotify |
| Built-in audio quality inspector with static spectrograph and bitrate-based rating | Visualizers exist in spotatui, spotify-player, kew, tuisic, and opencubicplayer, but not a documented local-file quality rating workflow | Helps users inspect file quality directly inside the terminal player |
| Split-pane LRC lyrics editor with timestamp stamping | termusic documents lyric timestamp adjustment, but TuneTUI documents a dedicated split-pane editor and stamping workflow | Makes lyrics correction part of playback instead of a separate tool |
| Config-dir LRC sidecars that override embedded lyrics | Competitors document lyrics display, but not this exact precedence and sidecar workflow | Keeps lyric edits portable and non-destructive to source audio files |
| Combined local stats, ASCII charts, recent listen log, and metadata-keyed sidecar | sprofile covers Spotify activity and some Spotify clients show history, but local-file stats are not documented across the listed local players | Gives local-library users listening analytics without external services |
| Tray collapse with continued playback on Windows/Linux desktops | Not documented by the listed terminal competitors | Lets a terminal player behave more like a desktop app while preserving terminal-first UX |
| Persistent selected output speaker with fallback and driver recovery | Other players may select devices, but this persistence plus recovery workflow is a documented TuneTUI focus | Reduces playback breakage after device changes across launches |
| Background startup/add-folder/rescan with warm-start metadata cache | termusic and musikcube index libraries, but TuneTUI documents TUI startup without waiting for full tag parsing | Improves responsiveness for large local libraries |

## Competitor Features TuneTUI Does Not Currently Document

| Feature Missing From TuneTUI | Products That Document It | Practical Value |
|---|---|---|
| Spotify catalog browsing/control | spotify-tui, spotatui, spotify-player, ncspot | Access to Spotify libraries, search, playlists, and Connect ecosystem |
| Native Spotify streaming | spotatui, spotify-player, spotifyd, ncspot | Terminal playback of Spotify without the official client |
| MPRIS/media-key integration | spotatui, spotify-player, termusic, kew, tuisic | Desktop media keys, lock-screen controls, and playerctl integration |
| Discord Rich Presence | spotatui, kew, tuisic | Social presence/status integration |
| Online music sources outside Spotify | termusic, tuisic, opencubicplayer | YouTube/SoundCloud/JioSaavn-style search or direct public archive browsing |
| Downloading online songs | termusic, tuisic | Offline collection building from online sources |
| Podcasts/RSS | termusic | Non-music audio workflows inside the same TUI |
| ReplayGain | kew | Album/track loudness normalization from file metadata |
| Equalizer | mocp | User-controlled tonal shaping |
| Remote/mobile streaming client | musikcube | Remote playback/control from mobile clients |
| Broad demoscene/chiptune/module specialty playback | mocp, opencubicplayer, musikcube | MOD/S3M/IT/SID/MIDI/game-music formats beyond normal song files |
| MCP/AI integration | tuisic | Control/search through AI tools that support Model Context Protocol |
| Full Spotify CLI scripting | spotify-tui, spotatui, spotify-player | Shell automation around search, playback, playlists, and library operations |

## Product Notes

| Product | Notes |
|---|---|
| TuneTUI | Strongest documented advantage is being a local-library player with social rooms, lyrics editing, quality inspection, stats, tray behavior, and performance-oriented scanning in one app |
| spotify-tui | Mature Spotify Web API controller, but documented playback requires the official Spotify client or spotifyd |
| spotatui | A maintained spotify-tui fork with native Spotify streaming, synced lyrics, system-wide visualizer, MPRIS/macOS media integration, and Discord Rich Presence |
| spotify-player | Most complete Spotify terminal client in this list, with streaming, Spotify Connect, visualization, media control, image rendering, notifications, daemon mode, and CLI commands |
| spotifyd | Not a TUI; best viewed as a lightweight Spotify Connect backend/daemon |
| ncspot | Focused Spotify ncurses client with small footprint, broad platform support, Vim keybindings, and IPC remote control |
| sprofile | Not a player; useful for Spotify listening activity/profile visualization, but development is paused according to its README |
| termusic | Closest local-library overlap with TuneTUI because it has local playback, podcasts, online downloads, tag editing, cover support, and an indexed library database |
| kew | Strong local-player alternative with CLI-first search, gapless playback, covers, visualizer, lyrics, ReplayGain, MPRIS, and Discord status |
| tuisic | Online-source streaming app rather than local-library player; stands out for downloads, MCP support, MPRIS, Discord RPC, Cava visualizer, and beta lyrics |
| mocp | Classic robust local console player with detached server, gapless playback, broad formats, configurable keys, themes, tag cache, and an equalizer |
| opencubicplayer | Specialty demoscene/chiptune/module player with extensive format support and visual modes; less comparable to a mainstream library player |
| musikcube | Strong local library/indexer/audio-engine competitor with built-in streaming server and Android remote client support |

## Positioning Summary

TuneTUI is not trying to be the strongest Spotify client, online downloader, or chiptune specialist. Its current documented niche is a local-library terminal player that combines playback, lyrics editing, metadata editing, listening stats, audio quality inspection, tray persistence, and collaborative listen-together rooms.

The most obvious feature gaps, if TuneTUI wants to compete more broadly, are MPRIS/media-key support, Discord Rich Presence, ReplayGain, equalizer controls, online source/download support, podcasts, and remote/mobile clients. The most defensible differentiators to highlight today are the listen-together shared queue, local-file audio quality inspector, built-in LRC editor, local listening analytics, and desktop-like tray behavior.
