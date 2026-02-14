use crate::audio::{AudioEngine, NullAudioEngine, WasapiAudioEngine};
use crate::config;
use crate::core::{HeaderSection, LyricsMode, StatsFilterFocus, TuneCore};
use crate::model::{PlaybackMode, Theme};
use crate::online::{Participant, TransportCommand, TransportEnvelope};
use crate::online_net::{
    LocalAction as NetworkLocalAction, NetworkEvent, NetworkRole, OnlineNetwork, build_invite_code,
    decode_invite_code, resolve_advertise_addr,
};
use crate::stats::{self, ListenSessionRecord, StatsStore};
use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::collections::HashMap;
use std::io::stdout;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(windows)]
const APP_INSTANCE_MUTEX: &str = "TuneTui.SingleInstance";
#[cfg(windows)]
const APP_CONSOLE_TITLE: &str = "TuneTUI";
const MAX_VOLUME: f32 = 2.5;
const VOLUME_STEP_COARSE: f32 = 0.05;
const VOLUME_STEP_FINE: f32 = 0.01;
const SCRUB_SECONDS_OPTIONS: [u16; 5] = [5, 10, 15, 30, 60];
const PARTIAL_LISTEN_FLUSH_SECONDS: u32 = 10;
const ONLINE_DEFAULT_BIND_ADDR: &str = "0.0.0.0:7878";

struct OnlineRuntime {
    network: Option<OnlineNetwork>,
    local_nickname: String,
    last_transport_seq: u64,
    join_prompt_active: bool,
    join_code_input: String,
    streamed_track_cache: HashMap<PathBuf, PathBuf>,
    pending_stream_path: Option<PathBuf>,
    remote_logical_track: Option<PathBuf>,
    last_periodic_sync_at: Instant,
}

impl OnlineRuntime {
    fn shutdown(&mut self) {
        if let Some(network) = self.network.take() {
            network.shutdown();
        }
        self.pending_stream_path = None;
        self.remote_logical_track = None;
    }
}

#[derive(Debug, Clone)]
struct ActiveListenSession {
    track_path: PathBuf,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    started_at_epoch_seconds: i64,
    playing_started_at: Option<Instant>,
    listened: Duration,
    persisted_listened_seconds: u32,
    play_count_recorded: bool,
    last_position: Option<Duration>,
    duration: Option<Duration>,
}

#[derive(Debug, Default)]
struct ListenTracker {
    active: Option<ActiveListenSession>,
}

impl ListenTracker {
    fn reset(&mut self) {
        self.active = None;
    }

    fn tick(&mut self, core: &TuneCore, audio: &dyn AudioEngine, stats: &mut StatsStore) -> bool {
        let mut wrote_event = false;
        let current_track = audio.current_track().map(Path::to_path_buf);
        let paused = audio.is_paused();

        let should_finalize = self
            .active
            .as_ref()
            .is_some_and(|active| current_track.as_ref() != Some(&active.track_path));
        if should_finalize {
            wrote_event = self.finalize_active(stats) || wrote_event;
        }

        if current_track.is_none() {
            return wrote_event;
        }

        if self.active.is_none() {
            let path = current_track.expect("checked some");
            let now = Instant::now();
            self.active = Some(ActiveListenSession {
                title: core.title_for_path(&path).unwrap_or_else(|| {
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("-")
                        .to_string()
                }),
                artist: core.artist_for_path(&path).map(ToOwned::to_owned),
                album: core.album_for_path(&path).map(ToOwned::to_owned),
                track_path: path,
                started_at_epoch_seconds: stats::now_epoch_seconds(),
                playing_started_at: (!paused).then_some(now),
                listened: Duration::ZERO,
                persisted_listened_seconds: 0,
                play_count_recorded: false,
                last_position: audio.position(),
                duration: audio.duration(),
            });
            return wrote_event;
        }

        if let Some(active) = self.active.as_mut() {
            active.last_position = audio.position().or(active.last_position);
            active.duration = audio.duration().or(active.duration);
            if paused {
                if let Some(started) = active.playing_started_at.take() {
                    active.listened = active.listened.saturating_add(started.elapsed());
                }
            } else if active.playing_started_at.is_none() {
                active.playing_started_at = Some(Instant::now());
            }
        }

        wrote_event = self.flush_partial(stats) || wrote_event;

        wrote_event
    }

    fn finalize_active(&mut self, stats: &mut StatsStore) -> bool {
        let mut active = match self.active.take() {
            Some(active) => active,
            None => return false,
        };

        if let Some(started) = active.playing_started_at.take() {
            active.listened = active.listened.saturating_add(started.elapsed());
        }

        let total_listened_seconds = duration_to_recorded_seconds(active.listened);
        let listened_seconds =
            total_listened_seconds.saturating_sub(active.persisted_listened_seconds);
        let completed =
            active
                .duration
                .zip(active.last_position)
                .is_some_and(|(duration, position)| {
                    position >= duration
                        || duration.saturating_sub(position) <= Duration::from_secs(1)
                });
        let counted_play = crate::stats::should_count_as_play(
            total_listened_seconds,
            completed,
            active.duration.map(|duration| duration.as_secs() as u32),
        ) && !active.play_count_recorded;
        let allow_short_listen = active.persisted_listened_seconds > 0 || counted_play;

        stats.record_listen(ListenSessionRecord {
            track_path: active.track_path,
            title: active.title,
            artist: active.artist,
            album: active.album,
            started_at_epoch_seconds: active.started_at_epoch_seconds,
            listened_seconds,
            completed,
            duration_seconds: active.duration.map(|duration| duration.as_secs() as u32),
            counted_play_override: Some(counted_play),
            allow_short_listen,
        });
        listened_seconds >= PARTIAL_LISTEN_FLUSH_SECONDS || counted_play || allow_short_listen
    }

    fn flush_partial(&mut self, stats: &mut StatsStore) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };

        let mut listened = active.listened;
        if let Some(started) = active.playing_started_at {
            listened = listened.saturating_add(started.elapsed());
        }

        let total_seconds = duration_to_recorded_seconds(listened);
        let delta = total_seconds.saturating_sub(active.persisted_listened_seconds);
        let should_record_play = crate::stats::should_count_as_play(
            total_seconds,
            false,
            active.duration.map(|duration| duration.as_secs() as u32),
        ) && !active.play_count_recorded;

        if delta < PARTIAL_LISTEN_FLUSH_SECONDS && !should_record_play {
            return false;
        }

        stats.record_listen(ListenSessionRecord {
            track_path: active.track_path.clone(),
            title: active.title.clone(),
            artist: active.artist.clone(),
            album: active.album.clone(),
            started_at_epoch_seconds: active.started_at_epoch_seconds,
            listened_seconds: delta,
            completed: false,
            duration_seconds: active.duration.map(|duration| duration.as_secs() as u32),
            counted_play_override: Some(should_record_play),
            allow_short_listen: false,
        });
        active.persisted_listened_seconds = active.persisted_listened_seconds.saturating_add(delta);
        if should_record_play {
            active.play_count_recorded = true;
        }
        true
    }
}

fn inferred_tunetui_config_dir(
    userprofile: Option<&str>,
    home: Option<&str>,
    override_dir: Option<&str>,
) -> Option<PathBuf> {
    if override_dir.is_some_and(|value| !value.trim().is_empty()) {
        return None;
    }
    if userprofile.is_some_and(|value| !value.trim().is_empty()) {
        return None;
    }
    let home = home.filter(|value| !value.trim().is_empty())?;
    Some(PathBuf::from(home).join(".config").join("tunetui"))
}

fn should_set_ssh_term(
    ssh_tty: Option<&str>,
    ssh_connection: Option<&str>,
    ssh_client: Option<&str>,
    term: Option<&str>,
) -> bool {
    let over_ssh = [ssh_tty, ssh_connection, ssh_client]
        .into_iter()
        .flatten()
        .any(|value| !value.trim().is_empty());
    if !over_ssh {
        return false;
    }

    match term.map(str::trim) {
        None | Some("") => true,
        Some(value) => value.eq_ignore_ascii_case("dumb"),
    }
}

fn prepare_runtime_environment() {
    if let Some(config_dir) = inferred_tunetui_config_dir(
        std::env::var("USERPROFILE").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        std::env::var("TUNETUI_CONFIG_DIR").ok().as_deref(),
    ) {
        unsafe {
            std::env::set_var("TUNETUI_CONFIG_DIR", config_dir);
        }
    }

    if should_set_ssh_term(
        std::env::var("SSH_TTY").ok().as_deref(),
        std::env::var("SSH_CONNECTION").ok().as_deref(),
        std::env::var("SSH_CLIENT").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    ) {
        unsafe {
            std::env::set_var("TERM", "xterm-256color");
        }
    }
}

fn duration_to_recorded_seconds(duration: Duration) -> u32 {
    if duration.is_zero() {
        return 0;
    }
    let secs = duration.as_secs();
    let has_subsec = duration.subsec_nanos() > 0;
    let rounded = if has_subsec {
        secs.saturating_add(1)
    } else {
        secs
    };
    rounded.min(u64::from(u32::MAX)) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootActionId {
    AddDirectory,
    AddSelectedToPlaylist,
    AddNowPlayingToPlaylist,
    SetPlaybackMode,
    PlaybackSettings,
    PlayPlaylist,
    RemoveSelectedFromPlaylist,
    CreatePlaylist,
    RemovePlaylist,
    RemoveDirectory,
    RescanLibrary,
    AudioDriverSettings,
    Theme,
    SaveState,
    ClearListenHistory,
    MinimizeToTray,
    ImportTxtToLyrics,
    ClosePanel,
}

const ROOT_ACTIONS: [RootActionId; 18] = [
    RootActionId::AddDirectory,
    RootActionId::AddSelectedToPlaylist,
    RootActionId::AddNowPlayingToPlaylist,
    RootActionId::SetPlaybackMode,
    RootActionId::PlaybackSettings,
    RootActionId::PlayPlaylist,
    RootActionId::RemoveSelectedFromPlaylist,
    RootActionId::CreatePlaylist,
    RootActionId::RemovePlaylist,
    RootActionId::RemoveDirectory,
    RootActionId::RescanLibrary,
    RootActionId::AudioDriverSettings,
    RootActionId::Theme,
    RootActionId::SaveState,
    RootActionId::ClearListenHistory,
    RootActionId::MinimizeToTray,
    RootActionId::ImportTxtToLyrics,
    RootActionId::ClosePanel,
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootVisibleAction {
    action: RootActionId,
    label: String,
}

fn root_action_label(action: RootActionId) -> &'static str {
    match action {
        RootActionId::AddDirectory => "Add directory",
        RootActionId::AddSelectedToPlaylist => "Add selected item to playlist",
        RootActionId::AddNowPlayingToPlaylist => "Add now playing song to playlist",
        RootActionId::SetPlaybackMode => "Set playback mode",
        RootActionId::PlaybackSettings => "Playback settings",
        RootActionId::PlayPlaylist => "Play playlist",
        RootActionId::RemoveSelectedFromPlaylist => "Remove selected from playlist",
        RootActionId::CreatePlaylist => "Create playlist",
        RootActionId::RemovePlaylist => "Remove playlist",
        RootActionId::RemoveDirectory => "Remove directory",
        RootActionId::RescanLibrary => "Rescan library",
        RootActionId::AudioDriverSettings => "Audio driver settings",
        RootActionId::Theme => "Theme",
        RootActionId::SaveState => "Save state",
        RootActionId::ClearListenHistory => "Clear listen history (backup)",
        RootActionId::MinimizeToTray => "Minimize to tray",
        RootActionId::ImportTxtToLyrics => "Import TXT to lyrics",
        RootActionId::ClosePanel => "Close panel",
    }
}

fn root_action_index(action: RootActionId) -> usize {
    ROOT_ACTIONS
        .iter()
        .position(|entry| *entry == action)
        .unwrap_or(0)
}

fn root_action_matches_query(action: RootActionId, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    root_action_label(action)
        .to_ascii_lowercase()
        .contains(query_lower)
}

fn root_visible_actions(
    query: &str,
    recent_root_actions: &[RootActionId],
) -> Vec<RootVisibleAction> {
    let query_lower = query.trim().to_ascii_lowercase();
    let mut seen = [false; ROOT_ACTIONS.len()];
    let mut visible = Vec::with_capacity(ROOT_ACTIONS.len());

    for action in recent_root_actions.iter().copied() {
        let index = root_action_index(action);
        if seen[index] || !root_action_matches_query(action, &query_lower) {
            continue;
        }
        seen[index] = true;
        visible.push(RootVisibleAction {
            action,
            label: format!("Recent: {}", root_action_label(action)),
        });
    }

    for action in ROOT_ACTIONS {
        let index = root_action_index(action);
        if seen[index] || !root_action_matches_query(action, &query_lower) {
            continue;
        }
        seen[index] = true;
        visible.push(RootVisibleAction {
            action,
            label: String::from(root_action_label(action)),
        });
    }

    visible
}

fn root_selected_for_action(action: RootActionId, recent_root_actions: &[RootActionId]) -> usize {
    root_visible_actions("", recent_root_actions)
        .iter()
        .position(|entry| entry.action == action)
        .unwrap_or(0)
}

fn update_recent_root_actions(recent_root_actions: &mut Vec<RootActionId>, action: RootActionId) {
    if matches!(action, RootActionId::ClosePanel) {
        return;
    }
    if let Some(index) = recent_root_actions
        .iter()
        .position(|entry| *entry == action)
    {
        recent_root_actions.remove(index);
    }
    recent_root_actions.insert(0, action);
    recent_root_actions.truncate(3);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActionPanelState {
    Closed,
    Root {
        selected: usize,
        query: String,
    },
    Mode {
        selected: usize,
    },
    PlaylistPlay {
        selected: usize,
    },
    PlaylistAdd {
        selected: usize,
    },
    PlaylistAddNowPlaying {
        selected: usize,
    },
    PlaylistCreate {
        selected: usize,
        input: String,
    },
    PlaylistRemove {
        selected: usize,
    },
    AudioSettings {
        selected: usize,
    },
    AudioOutput {
        selected: usize,
    },
    PlaybackSettings {
        selected: usize,
    },
    ThemeSettings {
        selected: usize,
    },
    LyricsImportTxt {
        selected: usize,
        path_input: String,
        interval_input: String,
    },
    AddDirectory {
        selected: usize,
        input: String,
    },
    RemoveDirectory {
        selected: usize,
    },
}

impl ActionPanelState {
    fn open(&mut self) {
        *self = Self::Root {
            selected: 0,
            query: String::new(),
        };
    }

    fn close(&mut self) {
        *self = Self::Closed;
    }

    fn is_open(&self) -> bool {
        !matches!(self, Self::Closed)
    }

    fn to_view(
        &self,
        core: &TuneCore,
        audio: &dyn AudioEngine,
        recent_root_actions: &[RootActionId],
    ) -> Option<crate::ui::ActionPanelView> {
        match self {
            Self::Closed => None,
            Self::Root { selected, query } => {
                let visible_actions = root_visible_actions(query, recent_root_actions);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Actions"),
                    hint: String::from("Type search  Enter select  Esc close  Up/Down navigate"),
                    search_query: Some(query.clone()),
                    options: if visible_actions.is_empty() {
                        vec![String::from("(no matching actions)")]
                    } else {
                        visible_actions
                            .into_iter()
                            .map(|entry| entry.label)
                            .collect()
                    },
                    selected: *selected,
                })
            }
            Self::Mode { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Playback Mode"),
                hint: String::from("Enter apply  Backspace back"),
                search_query: None,
                options: vec![
                    String::from("Normal"),
                    String::from("Shuffle"),
                    String::from("Loop playlist"),
                    String::from("Loop single track"),
                ],
                selected: *selected,
            }),
            Self::PlaylistPlay { selected } => {
                let playlists = sorted_playlist_names(core);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Play Playlist"),
                    hint: String::from("Enter play  Backspace back"),
                    search_query: None,
                    options: if playlists.is_empty() {
                        vec![String::from("(no playlists)")]
                    } else {
                        playlists
                    },
                    selected: *selected,
                })
            }
            Self::PlaylistAdd { selected } => {
                let playlists = sorted_playlist_names(core);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Add To Playlist"),
                    hint: String::from("Enter add  Backspace back"),
                    search_query: None,
                    options: if playlists.is_empty() {
                        vec![String::from("(no playlists)")]
                    } else {
                        playlists
                    },
                    selected: *selected,
                })
            }
            Self::PlaylistAddNowPlaying { selected } => {
                let playlists = sorted_playlist_names(core);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Add Now Playing To Playlist"),
                    hint: String::from("Enter add  Backspace back"),
                    search_query: None,
                    options: if playlists.is_empty() {
                        vec![String::from("(no playlists)")]
                    } else {
                        playlists
                    },
                    selected: *selected,
                })
            }
            Self::PlaylistCreate { selected, input } => Some(crate::ui::ActionPanelView {
                title: String::from("Create Playlist"),
                hint: String::from("Type name + Enter  Backspace back"),
                search_query: None,
                options: vec![if input.is_empty() {
                    String::from("Name: ")
                } else {
                    format!("Name: {input}")
                }],
                selected: *selected,
            }),
            Self::PlaylistRemove { selected } => {
                let playlists = sorted_playlist_names(core);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Remove Playlist"),
                    hint: String::from("Enter remove  Backspace back"),
                    search_query: None,
                    options: if playlists.is_empty() {
                        vec![String::from("(no playlists)")]
                    } else {
                        playlists
                    },
                    selected: *selected,
                })
            }
            Self::AudioSettings { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Audio Driver Settings"),
                hint: String::from("Enter select  Backspace back"),
                search_query: None,
                options: vec![
                    String::from("Reload audio driver"),
                    String::from("Select output speaker"),
                    String::from("Back"),
                ],
                selected: *selected,
            }),
            Self::AudioOutput { selected } => {
                let options = audio_output_options(audio);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Output Speaker"),
                    hint: String::from("Enter apply  Backspace back"),
                    search_query: None,
                    options,
                    selected: *selected,
                })
            }
            Self::PlaybackSettings { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Playback Settings"),
                hint: String::from("Enter toggle/select  Backspace back"),
                search_query: None,
                options: playback_settings_options(core),
                selected: *selected,
            }),
            Self::ThemeSettings { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Theme"),
                hint: String::from("Enter apply  Backspace back"),
                search_query: None,
                options: theme_options(core.theme),
                selected: *selected,
            }),
            Self::LyricsImportTxt {
                selected,
                path_input,
                interval_input,
            } => Some(crate::ui::ActionPanelView {
                title: String::from("Import TXT To Lyrics"),
                hint: String::from("Type path/seconds then Enter on Import"),
                search_query: None,
                options: vec![
                    if path_input.is_empty() {
                        String::from("TXT path: ")
                    } else {
                        format!("TXT path: {path_input}")
                    },
                    if interval_input.is_empty() {
                        String::from("Seed interval seconds: 3")
                    } else {
                        format!("Seed interval seconds: {interval_input}")
                    },
                    String::from("Import and save sidecar"),
                ],
                selected: *selected,
            }),
            Self::AddDirectory { selected, input } => Some(crate::ui::ActionPanelView {
                title: String::from("Add Directory"),
                hint: String::from("Enter choose folder  Down type path"),
                search_query: None,
                options: vec![
                    String::from("Choose folder externally"),
                    if input.is_empty() {
                        String::from("Path: ")
                    } else {
                        format!("Path: {input}")
                    },
                ],
                selected: *selected,
            }),
            Self::RemoveDirectory { selected } => {
                let paths = sorted_folder_paths(core);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Remove Directory"),
                    hint: String::from("Enter remove  Backspace back"),
                    search_query: None,
                    options: if paths.is_empty() {
                        vec![String::from("(no folders)")]
                    } else {
                        paths
                            .iter()
                            .map(|path| {
                                crate::config::sanitize_display_text(&path.display().to_string())
                            })
                            .collect()
                    },
                    selected: *selected,
                })
            }
        }
    }
}

pub fn run() -> Result<()> {
    prepare_runtime_environment();

    #[cfg(windows)]
    let _single_instance = match ensure_single_instance() {
        Ok(Some(guard)) => guard,
        Ok(None) => return Ok(()),
        Err(err) => return Err(err),
    };

    let state = config::load_state()?;
    let preferred_output = state.selected_output_device.clone();
    let mut core = TuneCore::from_persisted(state);
    let mut stats_store = stats::load_stats().unwrap_or_default();
    let mut listen_tracker = ListenTracker::default();

    let mut audio: Box<dyn AudioEngine> = match WasapiAudioEngine::new() {
        Ok(engine) => Box::new(engine),
        Err(_) => Box::new(NullAudioEngine::new()),
    };

    apply_audio_preferences_from_core(&core, &mut *audio);
    apply_saved_audio_output(&mut core, &mut *audio, preferred_output);

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut action_panel = ActionPanelState::Closed;
    let mut recent_root_actions: Vec<RootActionId> = Vec::new();
    let mut last_tick = Instant::now();
    let mut library_rect = ratatui::prelude::Rect::default();
    let mut stats_enabled_last = core.stats_enabled;
    let mut online_runtime = OnlineRuntime {
        network: None,
        local_nickname: inferred_online_nickname(),
        last_transport_seq: 0,
        join_prompt_active: false,
        join_code_input: String::new(),
        streamed_track_cache: HashMap::new(),
        pending_stream_path: None,
        remote_logical_track: None,
        last_periodic_sync_at: Instant::now(),
    };

    let result: Result<()> = loop {
        pump_tray_events(&mut core);
        drain_online_network_events(&mut core, &mut *audio, &mut online_runtime);
        audio.tick();
        maybe_publish_online_playback_sync(&core, &*audio, &mut online_runtime);
        if core.stats_enabled && listen_tracker.tick(&core, &*audio, &mut stats_store) {
            let _ = stats::save_stats(&stats_store);
        }
        if stats_enabled_last
            && !core.stats_enabled
            && listen_tracker.finalize_active(&mut stats_store)
        {
            let _ = stats::save_stats(&stats_store);
        }
        if core.clear_stats_requested {
            listen_tracker.reset();
            stats_store.clear_history();
            if let Err(err) = stats::save_stats(&stats_store) {
                core.status = format!("Failed to clear listen history: {err}");
            } else {
                core.status = String::from("Listen history cleared (backup saved)");
            }
            core.clear_stats_requested = false;
            core.dirty = true;
        }
        stats_enabled_last = core.stats_enabled;
        maybe_auto_advance_track(&mut core, &mut *audio);
        let lyrics_track_path = audio
            .current_track()
            .map(Path::to_path_buf)
            .or_else(|| core.current_path().map(Path::to_path_buf));
        core.sync_lyrics_for_track(lyrics_track_path.as_deref());
        if core.header_section == HeaderSection::Lyrics && core.lyrics_mode == LyricsMode::View {
            core.sync_lyrics_highlight_to_position(audio.position());
        }

        if core.dirty || last_tick.elapsed() > Duration::from_millis(250) {
            terminal.draw(|frame| {
                library_rect = crate::ui::library_rect(frame.area());
                let panel_view = action_panel.to_view(&core, &*audio, &recent_root_actions);
                let stats_snapshot = (core.header_section == HeaderSection::Stats).then(|| {
                    stats_store.query(
                        &crate::stats::StatsQuery {
                            range: core.stats_range,
                            sort: core.stats_sort,
                            artist_filter: core.stats_artist_filter.clone(),
                            album_filter: core.stats_album_filter.clone(),
                            search: core.stats_search.clone(),
                        },
                        stats::now_epoch_seconds(),
                    )
                });
                crate::ui::draw(
                    frame,
                    &core,
                    &*audio,
                    panel_view.as_ref(),
                    stats_snapshot.as_ref(),
                )
            })?;
            core.dirty = false;
            last_tick = Instant::now();
        }

        if !event::poll(Duration::from_millis(33))? {
            continue;
        }

        let event = event::read()?;
        if let Event::Mouse(mouse) = event {
            handle_mouse(&mut core, mouse, library_rect);
            continue;
        }

        let Event::Key(key) = event else {
            continue;
        };

        if key.kind != KeyEventKind::Press {
            continue;
        }

        if action_panel.is_open() {
            handle_action_panel_input_with_recent(
                &mut core,
                &mut *audio,
                &mut action_panel,
                &mut recent_root_actions,
                key.code,
            );
            continue;
        }

        if handle_stats_inline_input(&mut core, key) {
            continue;
        }
        if handle_lyrics_inline_input(&mut core, &*audio, key) {
            continue;
        }
        if handle_online_inline_input(&mut core, &*audio, key, &mut online_runtime) {
            continue;
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break Ok(()),
            KeyCode::Down => core.select_next(),
            KeyCode::Up => core.select_prev(),
            KeyCode::Enter => {
                if let Some(path) = core.activate_selected() {
                    if let Err(err) = audio.play(&path) {
                        core.status = concise_audio_error(&err);
                    } else {
                        publish_current_playback_state(&core, &*audio, &online_runtime);
                    }
                }
            }
            KeyCode::Left | KeyCode::Backspace => core.navigate_back(),
            KeyCode::Char(' ') => {
                if audio.is_paused() {
                    audio.resume();
                    core.status = String::from("Resumed");
                } else {
                    audio.pause();
                    core.status = String::from("Paused");
                }
                publish_current_playback_state(&core, &*audio, &online_runtime);
                core.dirty = true;
            }
            KeyCode::Char('n') => {
                if let Some(path) = core.next_track_path() {
                    if let Err(err) = audio.play(&path) {
                        core.status = concise_audio_error(&err);
                        core.dirty = true;
                    } else {
                        publish_current_playback_state(&core, &*audio, &online_runtime);
                    }
                }
            }
            KeyCode::Char('b') => {
                if let Some(path) = core.prev_track_path() {
                    if let Err(err) = audio.play(&path) {
                        core.status = concise_audio_error(&err);
                        core.dirty = true;
                    } else {
                        publish_current_playback_state(&core, &*audio, &online_runtime);
                    }
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                if let Err(err) = scrub_current_track(&mut *audio, core.scrub_seconds, false) {
                    core.status = format!("Scrub failed: {err}");
                } else {
                    core.status = format!("Scrubbed back {}", scrub_label(core.scrub_seconds));
                    publish_current_playback_state(&core, &*audio, &online_runtime);
                }
                core.dirty = true;
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if let Err(err) = scrub_current_track(&mut *audio, core.scrub_seconds, true) {
                    core.status = format!("Scrub failed: {err}");
                } else {
                    core.status = format!("Scrubbed forward {}", scrub_label(core.scrub_seconds));
                    publish_current_playback_state(&core, &*audio, &online_runtime);
                }
                core.dirty = true;
            }
            KeyCode::Char('m') => {
                core.cycle_mode();
                auto_save_state(&mut core, &*audio);
            }
            KeyCode::Tab => core.cycle_header_section(),
            KeyCode::Char('t') => {
                #[cfg(windows)]
                {
                    minimize_to_tray();
                    core.status = String::from("Minimized to tray");
                }
                #[cfg(not(windows))]
                {
                    core.status = String::from("Tray minimize is only available on Windows");
                }
                core.dirty = true;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                let step = if key.code == KeyCode::Char('+')
                    || key.modifiers.contains(KeyModifiers::SHIFT)
                {
                    VOLUME_STEP_FINE
                } else {
                    VOLUME_STEP_COARSE
                };
                let next = (audio.volume() + step).clamp(0.0, MAX_VOLUME);
                audio.set_volume(next);
                core.status = format!("Volume: {}%", (next * 100.0).round() as u16);
                core.dirty = true;
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                let step = if key.code == KeyCode::Char('_')
                    || key.modifiers.contains(KeyModifiers::SHIFT)
                {
                    VOLUME_STEP_FINE
                } else {
                    VOLUME_STEP_COARSE
                };
                let next = (audio.volume() - step).clamp(0.0, MAX_VOLUME);
                audio.set_volume(next);
                core.status = format!("Volume: {}%", (next * 100.0).round() as u16);
                core.dirty = true;
            }
            KeyCode::Char('r') => core.rescan(),
            KeyCode::Char('s') => {
                if let Err(err) = save_state_with_audio(&mut core, &*audio) {
                    core.status = format!("save error: {err:#}");
                    core.dirty = true;
                }
            }
            KeyCode::Char('/') => {
                action_panel.open();
                core.dirty = true;
            }
            _ => {}
        }
    };

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    cleanup_tray();
    terminal.show_cursor()?;
    if listen_tracker.finalize_active(&mut stats_store) {
        let _ = stats::save_stats(&stats_store);
    }
    online_runtime.shutdown();
    let save_result = save_state_with_audio(&mut core, &*audio);
    result?;
    save_result?;
    Ok(())
}

#[cfg(windows)]
struct SingleInstanceGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn ensure_single_instance() -> anyhow::Result<Option<SingleInstanceGuard>> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let mutex_name = to_wide(APP_INSTANCE_MUTEX);
    let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 1, mutex_name.as_ptr()) };
    if handle.is_null() {
        return Err(anyhow::anyhow!(
            "Failed to initialize single-instance mutex"
        ));
    }

    let already_exists = unsafe { GetLastError() == ERROR_ALREADY_EXISTS };
    if already_exists {
        focus_existing_instance();
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        return Ok(None);
    }

    set_console_title(APP_CONSOLE_TITLE);
    Ok(Some(SingleInstanceGuard(handle)))
}

#[cfg(windows)]
fn set_console_title(title: &str) {
    use windows_sys::Win32::System::Console::SetConsoleTitleW;

    let title_wide = to_wide(title);
    unsafe {
        SetConsoleTitleW(title_wide.as_ptr());
    }
}

#[cfg(windows)]
fn focus_existing_instance() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };

    if request_tray_restore() {
        return;
    }

    let class_name = to_wide("ConsoleWindowClass");
    let title = to_wide(APP_CONSOLE_TITLE);
    let hwnd = unsafe { FindWindowW(class_name.as_ptr(), title.as_ptr()) };
    if !hwnd.is_null() {
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(windows)]
fn request_tray_restore() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};

    let class_name = to_wide("TuneTuiTrayWindow");
    let hwnd = unsafe { FindWindowW(class_name.as_ptr(), std::ptr::null()) };
    if hwnd.is_null() {
        return false;
    }

    unsafe { PostMessageW(hwnd, TRAY_RESTORE_MSG, 0, 0) != 0 }
}

fn maybe_auto_advance_track(core: &mut TuneCore, audio: &mut dyn AudioEngine) {
    if audio.current_track().is_none() || audio.is_paused() {
        return;
    }

    let crossfade_triggered = should_trigger_crossfade_advance(audio);
    if crossfade_triggered && audio.crossfade_queued_track().is_some() {
        return;
    }

    if !audio.is_finished() && !crossfade_triggered {
        return;
    }

    if let Some(path) = core.next_track_path() {
        let result = if crossfade_triggered {
            audio.queue_crossfade(&path)
        } else {
            audio.play(&path)
        };
        if let Err(err) = result {
            core.status = concise_audio_error(&err);
            core.dirty = true;
        }
    } else if audio.is_finished() {
        audio.stop();
        core.status = String::from("Reached end of queue");
        core.dirty = true;
    }
}

fn handle_stats_inline_input(core: &mut TuneCore, key: KeyEvent) -> bool {
    if core.header_section != HeaderSection::Stats {
        return false;
    }

    if key.code == KeyCode::Up && key.modifiers.contains(KeyModifiers::SHIFT) {
        core.stats_scroll = 0;
        core.stats_focus = StatsFilterFocus::Range(core_range_index(core.stats_range));
        core.status = String::from("Stats view reset to filters");
        core.dirty = true;
        return true;
    }

    match key.code {
        KeyCode::Left => {
            if core.stats_scroll > 0 && matches!(core.stats_focus, StatsFilterFocus::Search) {
                stats_scroll_up(core);
                return true;
            }
            move_stats_focus_or_value(core, false)
        }
        KeyCode::Right => {
            if matches!(core.stats_focus, StatsFilterFocus::Search) {
                stats_scroll_down(core);
                return true;
            }
            move_stats_focus_or_value(core, true)
        }
        KeyCode::Up => {
            if core.stats_scroll > 0 && matches!(core.stats_focus, StatsFilterFocus::Search) {
                stats_scroll_up(core);
                return true;
            }
            move_stats_row(core, false)
        }
        KeyCode::Down => {
            if matches!(core.stats_focus, StatsFilterFocus::Search) {
                stats_scroll_down(core);
                return true;
            }
            move_stats_row(core, true)
        }
        KeyCode::Enter => {
            match core.stats_focus {
                StatsFilterFocus::Range(index) => {
                    let next = (index + 1) % 4;
                    core.stats_focus = StatsFilterFocus::Range(next);
                    set_stats_range_by_index(core, next);
                }
                StatsFilterFocus::Sort(index) => {
                    let next = 1_u8.saturating_sub(index.min(1));
                    core.stats_focus = StatsFilterFocus::Sort(next);
                    set_stats_sort_by_index(core, next);
                }
                StatsFilterFocus::Artist | StatsFilterFocus::Album | StatsFilterFocus::Search => {}
            }
            true
        }
        KeyCode::Backspace => {
            let target = match core.stats_focus {
                StatsFilterFocus::Artist => Some(&mut core.stats_artist_filter),
                StatsFilterFocus::Album => Some(&mut core.stats_album_filter),
                StatsFilterFocus::Search => Some(&mut core.stats_search),
                StatsFilterFocus::Range(_) | StatsFilterFocus::Sort(_) => None,
            };

            if let Some(text) = target {
                if !text.is_empty() {
                    text.pop();
                    core.status = format!("{} filter updated", core.stats_focus.label());
                    core.dirty = true;
                }
                return true;
            }

            true
        }
        KeyCode::Char(ch) => {
            let target = match core.stats_focus {
                StatsFilterFocus::Artist => Some(&mut core.stats_artist_filter),
                StatsFilterFocus::Album => Some(&mut core.stats_album_filter),
                StatsFilterFocus::Search => Some(&mut core.stats_search),
                StatsFilterFocus::Range(_) | StatsFilterFocus::Sort(_) => None,
            };

            if let Some(text) = target {
                text.push(ch);
                core.status = format!("{} filter updated", core.stats_focus.label());
                core.dirty = true;
                return true;
            }

            false
        }
        KeyCode::Delete => {
            match core.stats_focus {
                StatsFilterFocus::Artist => core.stats_artist_filter.clear(),
                StatsFilterFocus::Album => core.stats_album_filter.clear(),
                StatsFilterFocus::Search => core.stats_search.clear(),
                StatsFilterFocus::Range(_) | StatsFilterFocus::Sort(_) => return false,
            }
            core.status = format!("{} filter cleared", core.stats_focus.label());
            core.dirty = true;
            true
        }
        _ => false,
    }
}

fn handle_lyrics_inline_input(core: &mut TuneCore, audio: &dyn AudioEngine, key: KeyEvent) -> bool {
    if core.header_section != HeaderSection::Lyrics {
        return false;
    }

    if key.code == KeyCode::Tab || key.code == KeyCode::Char('/') {
        return false;
    }

    if core.lyrics_missing_prompt {
        match key.code {
            KeyCode::Enter => {
                core.create_empty_lyrics_sidecar();
                true
            }
            KeyCode::Esc | KeyCode::Backspace => {
                core.decline_lyrics_creation();
                true
            }
            _ => true,
        }
    } else {
        if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
            core.toggle_lyrics_mode();
            return true;
        }

        match core.lyrics_mode {
            LyricsMode::View => match key.code {
                KeyCode::Up => {
                    core.lyrics_move_selection(false);
                    true
                }
                KeyCode::Down => {
                    core.lyrics_move_selection(true);
                    true
                }
                _ => false,
            },
            LyricsMode::Edit => match key.code {
                KeyCode::Up => {
                    core.lyrics_move_selection(false);
                    true
                }
                KeyCode::Down => {
                    core.lyrics_move_selection(true);
                    true
                }
                KeyCode::Backspace => {
                    core.lyrics_backspace();
                    true
                }
                KeyCode::Enter => {
                    core.lyrics_insert_line_after();
                    true
                }
                KeyCode::Delete => {
                    core.lyrics_delete_selected_line();
                    true
                }
                KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    core.lyrics_stamp_selected_line(audio.position());
                    true
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    core.lyrics_insert_char(ch);
                    true
                }
                _ => false,
            },
        }
    }
}

fn handle_online_inline_input(
    core: &mut TuneCore,
    audio: &dyn AudioEngine,
    key: KeyEvent,
    online_runtime: &mut OnlineRuntime,
) -> bool {
    if core.header_section != HeaderSection::Online {
        return false;
    }

    if key.code == KeyCode::Tab || key.code == KeyCode::Char('/') {
        return false;
    }

    if online_runtime.join_prompt_active {
        match key.code {
            KeyCode::Esc => {
                online_runtime.join_prompt_active = false;
                online_runtime.join_code_input.clear();
                core.status = String::from("Join cancelled");
                core.dirty = true;
                return true;
            }
            KeyCode::Backspace => {
                online_runtime.join_code_input.pop();
                core.status = format!("Enter invite code: {}", online_runtime.join_code_input);
                core.dirty = true;
                return true;
            }
            KeyCode::Enter => {
                if online_runtime.join_code_input.trim().is_empty() {
                    core.status = String::from("Enter invite code, then press Enter");
                    core.dirty = true;
                    return true;
                }
                let invite_code = online_runtime.join_code_input.trim().to_string();
                online_runtime.join_prompt_active = false;
                online_runtime.join_code_input.clear();
                join_from_invite_code(core, online_runtime, &invite_code);
                return true;
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !ch.is_ascii_whitespace() {
                    online_runtime.join_code_input.push(ch);
                    core.status = format!("Enter invite code: {}", online_runtime.join_code_input);
                    core.dirty = true;
                }
                return true;
            }
            _ => return true,
        }
    }

    match key.code {
        KeyCode::Char('c') => {
            online_runtime.shutdown();
            online_runtime.last_transport_seq = 0;
            core.online_host_room(&online_runtime.local_nickname);
            let bind_addr = std::env::var("TUNETUI_ONLINE_BIND_ADDR")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| String::from(ONLINE_DEFAULT_BIND_ADDR));
            let advertise_addr = std::env::var("TUNETUI_ONLINE_ADVERTISE_ADDR")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| resolve_advertise_addr(&bind_addr).ok())
                .unwrap_or_else(|| String::from("127.0.0.1:7878"));
            let password = optional_env("TUNETUI_ONLINE_PASSWORD");
            let include_password = env_bool("TUNETUI_ONLINE_INCLUDE_PASSWORD", true);
            let invite_code =
                match build_invite_code(&advertise_addr, password.as_deref(), include_password) {
                    Ok(code) => code,
                    Err(err) => {
                        core.online_leave_room();
                        core.status = format!("Invite code build failed: {err}");
                        core.dirty = true;
                        return true;
                    }
                };
            if let Some(active) = core.online.session.as_mut() {
                active.room_code = invite_code.clone();
            }
            let Some(session) = core.online.session.clone() else {
                core.status = String::from("Online room initialization failed");
                core.dirty = true;
                return true;
            };
            match OnlineNetwork::start_host(&bind_addr, session, password.clone()) {
                Ok(network) => {
                    online_runtime.network = Some(network);
                    core.status = format!(
                        "Hosting {bind_addr} invite {invite_code}{}",
                        if password.is_some() && !include_password {
                            " (password required separately)"
                        } else {
                            ""
                        }
                    );
                    core.dirty = true;
                }
                Err(err) => {
                    core.online_leave_room();
                    core.status = format!("Online host failed: {err}");
                    core.dirty = true;
                }
            }
            true
        }
        KeyCode::Char('j') => {
            if let Some(room_code_input) = optional_env("TUNETUI_ONLINE_ROOM_CODE") {
                join_from_invite_code(core, online_runtime, &room_code_input);
            } else {
                online_runtime.join_prompt_active = true;
                online_runtime.join_code_input.clear();
                core.status = String::from("Enter invite code: ");
                core.dirty = true;
            }
            true
        }
        KeyCode::Char('l') => {
            online_runtime.shutdown();
            online_runtime.last_transport_seq = 0;
            core.online_leave_room();
            true
        }
        KeyCode::Char('o') => {
            core.online_toggle_mode();
            if let (Some(network), Some(session)) =
                (&online_runtime.network, core.online.session.as_ref())
            {
                network.send_local_action(NetworkLocalAction::SetMode(session.mode));
            }
            true
        }
        KeyCode::Char('q') => {
            core.online_cycle_quality();
            if let (Some(network), Some(session)) =
                (&online_runtime.network, core.online.session.as_ref())
            {
                network.send_local_action(NetworkLocalAction::SetQuality(session.quality));
            }
            true
        }
        KeyCode::Char('p') => {
            core.online_add_simulated_peer();
            true
        }
        KeyCode::Char('x') => {
            core.online_remove_simulated_peer();
            true
        }
        KeyCode::Char('g') => {
            core.online_recalibrate_ping();
            true
        }
        KeyCode::Char('[') => {
            core.online_adjust_manual_delay(-10);
            if let (Some(network), Some(session)) =
                (&online_runtime.network, core.online.session.as_ref())
                && let Some(local) = session.local_participant()
            {
                network.send_local_action(NetworkLocalAction::DelayUpdate {
                    manual_extra_delay_ms: local.manual_extra_delay_ms,
                    auto_ping_delay: local.auto_ping_delay,
                });
            }
            true
        }
        KeyCode::Char(']') => {
            core.online_adjust_manual_delay(10);
            if let (Some(network), Some(session)) =
                (&online_runtime.network, core.online.session.as_ref())
                && let Some(local) = session.local_participant()
            {
                network.send_local_action(NetworkLocalAction::DelayUpdate {
                    manual_extra_delay_ms: local.manual_extra_delay_ms,
                    auto_ping_delay: local.auto_ping_delay,
                });
            }
            true
        }
        KeyCode::Char('a') => {
            core.online_toggle_auto_delay();
            if let (Some(network), Some(session)) =
                (&online_runtime.network, core.online.session.as_ref())
                && let Some(local) = session.local_participant()
            {
                network.send_local_action(NetworkLocalAction::DelayUpdate {
                    manual_extra_delay_ms: local.manual_extra_delay_ms,
                    auto_ping_delay: local.auto_ping_delay,
                });
            }
            true
        }
        KeyCode::Char('s') => {
            let queued_path = audio
                .current_track()
                .map(Path::to_path_buf)
                .or_else(|| core.current_path().map(Path::to_path_buf));
            core.online_queue_current_track(queued_path.as_deref());
            if let (Some(network), Some(session)) =
                (&online_runtime.network, core.online.session.as_ref())
                && let Some(last) = session.shared_queue.last()
            {
                network.send_local_action(NetworkLocalAction::QueueAdd(last.clone()));
            }
            true
        }
        _ => false,
    }
}

fn inferred_online_nickname() -> String {
    std::env::var("TUNETUI_ONLINE_NICKNAME")
        .ok()
        .or_else(|| std::env::var("USERNAME").ok())
        .or_else(|| std::env::var("USER").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| String::from("you"))
}

fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    let Some(value) = optional_env(key) else {
        return default;
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn join_from_invite_code(
    core: &mut TuneCore,
    online_runtime: &mut OnlineRuntime,
    invite_code: &str,
) {
    online_runtime.shutdown();
    online_runtime.last_transport_seq = 0;

    let decoded = match decode_invite_code(invite_code) {
        Ok(decoded) => decoded,
        Err(err) => {
            core.status = format!("Invalid invite code: {err}");
            core.dirty = true;
            return;
        }
    };

    let join_password = decoded
        .password
        .clone()
        .or_else(|| optional_env("TUNETUI_ONLINE_PASSWORD"));
    core.online_join_room(&decoded.room_code, &online_runtime.local_nickname);
    match OnlineNetwork::start_client(
        &decoded.server_addr,
        &decoded.room_code,
        &online_runtime.local_nickname,
        join_password,
    ) {
        Ok(network) => {
            online_runtime.network = Some(network);
            core.status = format!("Connected to {}", decoded.server_addr);
            core.dirty = true;
        }
        Err(err) => {
            core.online_leave_room();
            core.status = format!("Online join failed: {err}");
            core.dirty = true;
        }
    }
}

fn publish_transport_command(
    core: &TuneCore,
    online_runtime: &OnlineRuntime,
    command: TransportCommand,
) {
    let Some(network) = online_runtime.network.as_ref() else {
        return;
    };
    if core.online.session.is_none() {
        return;
    }
    network.send_local_action(NetworkLocalAction::Transport(TransportEnvelope {
        seq: 0,
        origin_nickname: online_runtime.local_nickname.clone(),
        command,
    }));
}

fn publish_current_playback_state(
    core: &TuneCore,
    audio: &dyn AudioEngine,
    online_runtime: &OnlineRuntime,
) {
    let Some(path) = audio
        .current_track()
        .map(Path::to_path_buf)
        .or_else(|| core.current_path().map(Path::to_path_buf))
    else {
        return;
    };
    let position_ms = audio
        .position()
        .map(|position| position.as_millis() as u64)
        .unwrap_or(0);
    publish_transport_command(
        core,
        online_runtime,
        TransportCommand::SetPlaybackState {
            path,
            position_ms,
            paused: audio.is_paused(),
        },
    );
}

fn maybe_publish_online_playback_sync(
    core: &TuneCore,
    audio: &dyn AudioEngine,
    online_runtime: &mut OnlineRuntime,
) {
    let Some(network) = online_runtime.network.as_ref() else {
        return;
    };
    if !matches!(network.role(), NetworkRole::Host) {
        return;
    }
    if online_runtime.last_periodic_sync_at.elapsed() < Duration::from_millis(950) {
        return;
    }

    online_runtime.last_periodic_sync_at = Instant::now();
    publish_current_playback_state(core, audio, online_runtime);
}

fn drain_online_network_events(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    online_runtime: &mut OnlineRuntime,
) {
    loop {
        let event = {
            let Some(network) = online_runtime.network.as_ref() else {
                return;
            };
            network.try_recv_event()
        };
        let Some(event) = event else {
            break;
        };

        match event {
            NetworkEvent::Status(message) => {
                core.status = message;
                core.dirty = true;
            }
            NetworkEvent::StreamTrackReady {
                requested_path,
                local_temp_path,
            } => {
                online_runtime
                    .streamed_track_cache
                    .insert(requested_path.clone(), local_temp_path.clone());
                if online_runtime.pending_stream_path.as_ref() == Some(&requested_path) {
                    match audio.play(&local_temp_path) {
                        Ok(()) => {
                            online_runtime.remote_logical_track = Some(requested_path.clone());
                            core.status = format!(
                                "Streaming from host: {}",
                                requested_path
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("track")
                            );
                            core.dirty = true;
                        }
                        Err(err) => {
                            core.status =
                                format!("Stream playback failed: {}", concise_audio_error(&err));
                            core.dirty = true;
                        }
                    }
                    online_runtime.pending_stream_path = None;
                }
            }
            NetworkEvent::SessionSync(mut session) => {
                let role = online_runtime
                    .network
                    .as_ref()
                    .map(OnlineNetwork::role)
                    .copied()
                    .unwrap_or(NetworkRole::Client);
                normalize_local_online_participant(
                    &mut session,
                    &online_runtime.local_nickname,
                    &role,
                );
                if let Some(last_transport) = session.last_transport.as_ref()
                    && last_transport.seq > online_runtime.last_transport_seq
                {
                    online_runtime.last_transport_seq = last_transport.seq;
                    if !last_transport
                        .origin_nickname
                        .eq_ignore_ascii_case(&online_runtime.local_nickname)
                    {
                        apply_remote_transport(
                            core,
                            audio,
                            online_runtime,
                            &last_transport.command,
                        );
                    }
                }
                core.online.session = Some(session);
                core.dirty = true;
            }
        }
    }
}

fn normalize_local_online_participant(
    session: &mut crate::online::OnlineSession,
    local_nickname: &str,
    role: &NetworkRole,
) {
    for participant in &mut session.participants {
        participant.is_local = false;
    }

    if let Some(participant) = session
        .participants
        .iter_mut()
        .find(|participant| participant.nickname.eq_ignore_ascii_case(local_nickname))
    {
        participant.is_local = true;
        if matches!(role, NetworkRole::Host) {
            participant.is_host = true;
        }
        return;
    }

    session.participants.push(Participant {
        nickname: local_nickname.to_string(),
        is_local: true,
        is_host: matches!(role, NetworkRole::Host),
        ping_ms: 30,
        manual_extra_delay_ms: 0,
        auto_ping_delay: true,
    });
}

fn apply_remote_transport(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    online_runtime: &mut OnlineRuntime,
    command: &TransportCommand,
) {
    match command {
        TransportCommand::SetPaused { paused } => {
            if *paused {
                audio.pause();
                core.status = String::from("Remote paused playback");
            } else {
                audio.resume();
                core.status = String::from("Remote resumed playback");
            }
            core.dirty = true;
        }
        TransportCommand::PlayTrack { path } => {
            if ensure_remote_track(core, audio, online_runtime, path) {
                core.current_queue_index = core.queue_position_for_path(path);
                core.status = String::from("Remote switched track");
            }
            core.dirty = true;
        }
        TransportCommand::SetPlaybackState {
            path,
            position_ms,
            paused,
        } => {
            if !ensure_remote_track(core, audio, online_runtime, path) {
                core.dirty = true;
                return;
            }

            let local_ms = audio
                .position()
                .map(|position| position.as_millis() as i64)
                .unwrap_or(0);
            let target_ms = *position_ms as i64;
            let drift_ms = (target_ms - local_ms).abs();
            let seek_threshold = if *paused { 80_i64 } else { 220_i64 };
            if drift_ms >= seek_threshold {
                let _ = audio.seek_to(Duration::from_millis(*position_ms));
            }

            if *paused {
                audio.pause();
            } else {
                audio.resume();
            }

            online_runtime.remote_logical_track = Some(path.clone());
            core.current_queue_index = core.queue_position_for_path(path);
            core.status = format!("Remote sync drift {}ms", drift_ms);
            core.dirty = true;
        }
    }
}

fn ensure_remote_track(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    online_runtime: &mut OnlineRuntime,
    path: &Path,
) -> bool {
    if online_runtime.remote_logical_track.as_ref() == Some(&path.to_path_buf())
        && audio.current_track().is_some()
    {
        return true;
    }

    if let Some(cached) = online_runtime.streamed_track_cache.get(path)
        && cached.exists()
        && audio.play(cached).is_ok()
    {
        online_runtime.remote_logical_track = Some(path.to_path_buf());
        return true;
    }

    match audio.play(path) {
        Ok(()) => {
            online_runtime.remote_logical_track = Some(path.to_path_buf());
            true
        }
        Err(err) => {
            if online_runtime.pending_stream_path.as_ref() != Some(&path.to_path_buf()) {
                if let Some(network) = online_runtime.network.as_ref() {
                    network.request_track_stream(path.to_path_buf());
                    online_runtime.pending_stream_path = Some(path.to_path_buf());
                    online_runtime.remote_logical_track = Some(path.to_path_buf());
                    core.status =
                        String::from("Remote track missing locally, requesting host stream...");
                } else {
                    core.status = concise_audio_error(&err);
                }
            }
            false
        }
    }
}

fn stats_scroll_down(core: &mut TuneCore) {
    core.stats_scroll = core.stats_scroll.saturating_add(1);
    core.dirty = true;
}

fn stats_scroll_up(core: &mut TuneCore) {
    core.stats_scroll = core.stats_scroll.saturating_sub(1);
    core.dirty = true;
}

fn move_stats_focus_or_value(core: &mut TuneCore, forward: bool) -> bool {
    match core.stats_focus {
        StatsFilterFocus::Range(index) => {
            let next = if forward {
                (index + 1) % 4
            } else {
                (index + 3) % 4
            };
            core.stats_focus = StatsFilterFocus::Range(next);
            set_stats_range_by_index(core, next);
            true
        }
        StatsFilterFocus::Sort(index) => {
            let next = 1_u8.saturating_sub(index.min(1));
            core.stats_focus = StatsFilterFocus::Sort(next);
            set_stats_sort_by_index(core, next);
            true
        }
        StatsFilterFocus::Artist | StatsFilterFocus::Album | StatsFilterFocus::Search => {
            move_stats_row(core, forward)
        }
    }
}

fn move_stats_row(core: &mut TuneCore, forward: bool) -> bool {
    core.stats_focus = match core.stats_focus {
        StatsFilterFocus::Range(_) => {
            if forward {
                StatsFilterFocus::Sort(core_sort_index(core.stats_sort))
            } else {
                StatsFilterFocus::Search
            }
        }
        StatsFilterFocus::Sort(_) => {
            if forward {
                StatsFilterFocus::Artist
            } else {
                StatsFilterFocus::Range(core_range_index(core.stats_range))
            }
        }
        StatsFilterFocus::Artist => {
            if forward {
                StatsFilterFocus::Album
            } else {
                StatsFilterFocus::Sort(core_sort_index(core.stats_sort))
            }
        }
        StatsFilterFocus::Album => {
            if forward {
                StatsFilterFocus::Search
            } else {
                StatsFilterFocus::Artist
            }
        }
        StatsFilterFocus::Search => {
            if forward {
                StatsFilterFocus::Range(core_range_index(core.stats_range))
            } else {
                StatsFilterFocus::Album
            }
        }
    };
    core.dirty = true;
    true
}

fn set_stats_range_by_index(core: &mut TuneCore, index: u8) {
    core.stats_range = match index {
        0 => crate::stats::StatsRange::Lifetime,
        1 => crate::stats::StatsRange::Today,
        2 => crate::stats::StatsRange::Days7,
        _ => crate::stats::StatsRange::Days30,
    };
    core.dirty = true;
}

fn set_stats_sort_by_index(core: &mut TuneCore, index: u8) {
    core.stats_sort = if index == 0 {
        crate::stats::StatsSort::ListenTime
    } else {
        crate::stats::StatsSort::Plays
    };
    core.dirty = true;
}

fn core_range_index(range: crate::stats::StatsRange) -> u8 {
    match range {
        crate::stats::StatsRange::Lifetime => 0,
        crate::stats::StatsRange::Today => 1,
        crate::stats::StatsRange::Days7 => 2,
        crate::stats::StatsRange::Days30 => 3,
    }
}

fn core_sort_index(sort: crate::stats::StatsSort) -> u8 {
    match sort {
        crate::stats::StatsSort::ListenTime => 0,
        crate::stats::StatsSort::Plays => 1,
    }
}

fn should_trigger_crossfade_advance(audio: &dyn AudioEngine) -> bool {
    let crossfade_seconds = audio.crossfade_seconds();
    if crossfade_seconds == 0 {
        return false;
    }

    let Some(position) = audio.position() else {
        return false;
    };
    let Some(duration) = audio.duration() else {
        return false;
    };
    if duration <= position {
        return false;
    }

    let remaining = duration.saturating_sub(position);
    remaining <= Duration::from_secs(u64::from(crossfade_seconds))
}

fn scrub_current_track(audio: &mut dyn AudioEngine, seconds: u16, forward: bool) -> Result<()> {
    let position = audio
        .position()
        .ok_or_else(|| anyhow::anyhow!("current backend does not expose position"))?;

    let mut target = if forward {
        position.saturating_add(Duration::from_secs(u64::from(seconds)))
    } else {
        position.saturating_sub(Duration::from_secs(u64::from(seconds)))
    };

    if let Some(duration) = audio.duration() {
        target = target.min(duration);
    }

    audio.seek_to(target)
}

fn concise_audio_error(err: &anyhow::Error) -> String {
    let message = err.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("device") && (lower.contains("no longer") || lower.contains("unavailable")) {
        return String::from("Audio device unavailable. Use / -> Audio driver settings -> Reload");
    }
    format!("Playback failed: {message}")
}

fn save_state_with_audio(core: &mut TuneCore, audio: &dyn AudioEngine) -> Result<()> {
    persist_state_with_audio(core, audio, true)
}

fn auto_save_state(core: &mut TuneCore, audio: &dyn AudioEngine) {
    let _ = persist_state_with_audio(core, audio, false);
}

fn persist_state_with_audio(
    core: &mut TuneCore,
    audio: &dyn AudioEngine,
    show_status: bool,
) -> Result<()> {
    let state = persisted_state_with_audio(core, audio);
    config::save_state(&state)?;
    if show_status {
        core.status = String::from("State saved");
        core.dirty = true;
    }
    Ok(())
}

fn persisted_state_with_audio(
    core: &TuneCore,
    audio: &dyn AudioEngine,
) -> crate::model::PersistedState {
    let mut state = core.persisted_state();
    state.selected_output_device = audio.selected_output_device();
    state
}

fn apply_saved_audio_output(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    preferred_output: Option<String>,
) {
    let Some(preferred_output) = preferred_output else {
        return;
    };

    if let Err(err) = audio.set_output_device(Some(preferred_output.as_str())) {
        core.status = format!(
            "Saved output '{}' unavailable. Using default. / -> Audio driver settings",
            preferred_output
        );
        core.dirty = true;

        if let Err(default_err) = audio.set_output_device(None) {
            core.status =
                format!("Audio init failed: {default_err}. / -> Audio driver settings -> Reload");
            core.dirty = true;
        } else {
            core.status = format!("{} ({})", core.status, concise_audio_error(&err));
        }
    }
}

fn handle_mouse(core: &mut TuneCore, mouse: MouseEvent, library_rect: ratatui::prelude::Rect) {
    let inside_library = point_in_rect(mouse.column, mouse.row, library_rect);
    match mouse.kind {
        MouseEventKind::ScrollDown if inside_library => core.select_next(),
        MouseEventKind::ScrollUp if inside_library => core.select_prev(),
        _ => {}
    }
}

fn point_in_rect(x: u16, y: u16, rect: ratatui::prelude::Rect) -> bool {
    if rect.width == 0 || rect.height == 0 {
        return false;
    }
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn sorted_playlist_names(core: &TuneCore) -> Vec<String> {
    let mut names: Vec<String> = core.playlists.keys().cloned().collect();
    names.sort_by_cached_key(|name| name.to_ascii_lowercase());
    names
}

fn sorted_folder_paths(core: &TuneCore) -> Vec<PathBuf> {
    let mut paths = core.folders.clone();
    paths.sort_by_cached_key(|path| path.to_string_lossy().to_ascii_lowercase());
    paths
}

fn audio_output_options(audio: &dyn AudioEngine) -> Vec<String> {
    let selected = audio.selected_output_device();
    let outputs = audio.available_outputs();
    let mut options = Vec::with_capacity(outputs.len().saturating_add(1));
    options.push(if selected.is_none() {
        String::from("* System default output")
    } else {
        String::from("System default output")
    });

    for output in outputs {
        let label = if selected.as_deref() == Some(output.as_str()) {
            format!("* {output}")
        } else {
            output
        };
        options.push(label);
    }

    options
}

fn playback_settings_options(core: &TuneCore) -> Vec<String> {
    vec![
        format!(
            "Loudness normalization: {}",
            if core.loudness_normalization {
                "On"
            } else {
                "Off"
            }
        ),
        format!(
            "Song crossfade: {}",
            crossfade_label(core.crossfade_seconds)
        ),
        format!("Scrub length: {}", scrub_label(core.scrub_seconds)),
        format!(
            "Stats tracking: {}",
            if core.stats_enabled { "On" } else { "Off" }
        ),
        String::from("Back"),
    ]
}

fn theme_options(theme: Theme) -> Vec<String> {
    [
        Theme::Dark,
        Theme::PitchBlack,
        Theme::Galaxy,
        Theme::Matrix,
        Theme::Demonic,
        Theme::CottonCandy,
    ]
    .into_iter()
    .map(|entry| {
        if entry == theme {
            format!("* {}", theme_label(entry))
        } else {
            theme_label(entry).to_string()
        }
    })
    .collect()
}

fn theme_label(theme: Theme) -> &'static str {
    match theme {
        Theme::Dark => "Dark",
        Theme::PitchBlack => "Pitch Black",
        Theme::Galaxy => "Galaxy",
        Theme::Matrix => "Matrix",
        Theme::Demonic => "Demonic",
        Theme::CottonCandy => "Cotton Candy",
        Theme::Ocean => "Ocean (legacy)",
        Theme::Forest => "Forest (legacy)",
        Theme::Sunset => "Sunset (legacy)",
    }
}

fn crossfade_label(seconds: u16) -> String {
    if seconds == 0 {
        String::from("Off")
    } else {
        format!("{seconds}s")
    }
}

fn next_crossfade_seconds(current: u16) -> u16 {
    match current {
        0 => 2,
        2 => 4,
        4 => 6,
        6 => 8,
        8 => 10,
        _ => 0,
    }
}

fn scrub_label(seconds: u16) -> String {
    if seconds == 60 {
        String::from("1m")
    } else {
        format!("{seconds}s")
    }
}

fn next_scrub_seconds(current: u16) -> u16 {
    let index = SCRUB_SECONDS_OPTIONS
        .iter()
        .position(|entry| *entry == current)
        .unwrap_or(0);
    SCRUB_SECONDS_OPTIONS[(index + 1) % SCRUB_SECONDS_OPTIONS.len()]
}

fn apply_audio_preferences_from_core(core: &TuneCore, audio: &mut dyn AudioEngine) {
    audio.set_loudness_normalization(core.loudness_normalization);
    audio.set_crossfade_seconds(core.crossfade_seconds);
}

fn update_panel_selection(panel: &mut ActionPanelState, option_count: usize, move_next: bool) {
    if option_count == 0 {
        return;
    }

    let advance = |selected: &mut usize| {
        if move_next {
            *selected = (*selected + 1) % option_count;
        } else {
            *selected = if *selected == 0 {
                option_count - 1
            } else {
                *selected - 1
            };
        }
    };

    match panel {
        ActionPanelState::Root { selected, .. }
        | ActionPanelState::Mode { selected }
        | ActionPanelState::PlaylistPlay { selected }
        | ActionPanelState::PlaylistAdd { selected }
        | ActionPanelState::PlaylistAddNowPlaying { selected }
        | ActionPanelState::PlaylistCreate { selected, .. }
        | ActionPanelState::PlaylistRemove { selected }
        | ActionPanelState::AudioSettings { selected }
        | ActionPanelState::AudioOutput { selected }
        | ActionPanelState::PlaybackSettings { selected }
        | ActionPanelState::ThemeSettings { selected }
        | ActionPanelState::LyricsImportTxt { selected, .. }
        | ActionPanelState::AddDirectory { selected, .. }
        | ActionPanelState::RemoveDirectory { selected } => advance(selected),
        ActionPanelState::Closed => {}
    }
}

#[cfg(test)]
fn handle_action_panel_input(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    panel: &mut ActionPanelState,
    key: KeyCode,
) {
    let mut recent_root_actions = Vec::new();
    handle_action_panel_input_with_recent(core, audio, panel, &mut recent_root_actions, key);
}

fn handle_action_panel_input_with_recent(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    panel: &mut ActionPanelState,
    recent_root_actions: &mut Vec<RootActionId>,
    key: KeyCode,
) {
    if let ActionPanelState::Root { selected, query } = panel {
        match key {
            KeyCode::Char(ch) => {
                query.push(ch);
                *selected = 0;
                core.dirty = true;
                return;
            }
            KeyCode::Backspace if !query.is_empty() => {
                query.pop();
                *selected = 0;
                core.dirty = true;
                return;
            }
            _ => {}
        }
    }

    if let ActionPanelState::AddDirectory { selected, input } = panel {
        match key {
            KeyCode::Char(ch) if *selected == 1 => {
                input.push(ch);
                core.dirty = true;
                return;
            }
            KeyCode::Backspace if *selected == 1 && !input.is_empty() => {
                input.pop();
                core.dirty = true;
                return;
            }
            _ => {}
        }
    }

    if let ActionPanelState::PlaylistCreate { selected, input } = panel {
        match key {
            KeyCode::Char(ch) if *selected == 0 => {
                input.push(ch);
                core.dirty = true;
                return;
            }
            KeyCode::Backspace if *selected == 0 && !input.is_empty() => {
                input.pop();
                core.dirty = true;
                return;
            }
            _ => {}
        }
    }

    if let ActionPanelState::LyricsImportTxt {
        selected,
        path_input,
        interval_input,
    } = panel
    {
        match key {
            KeyCode::Char(ch) if *selected == 0 => {
                path_input.push(ch);
                core.dirty = true;
                return;
            }
            KeyCode::Char(ch) if *selected == 1 && ch.is_ascii_digit() => {
                interval_input.push(ch);
                core.dirty = true;
                return;
            }
            KeyCode::Backspace if *selected == 0 && !path_input.is_empty() => {
                path_input.pop();
                core.dirty = true;
                return;
            }
            KeyCode::Backspace if *selected == 1 && !interval_input.is_empty() => {
                interval_input.pop();
                core.dirty = true;
                return;
            }
            _ => {}
        }
    }

    let option_count = match panel {
        ActionPanelState::Closed => 0,
        ActionPanelState::Root { query, .. } => {
            root_visible_actions(query, recent_root_actions).len()
        }
        ActionPanelState::Mode { .. } => 4,
        ActionPanelState::PlaylistPlay { .. }
        | ActionPanelState::PlaylistAdd { .. }
        | ActionPanelState::PlaylistAddNowPlaying { .. }
        | ActionPanelState::PlaylistRemove { .. } => sorted_playlist_names(core).len().max(1),
        ActionPanelState::PlaylistCreate { .. } => 1,
        ActionPanelState::AudioSettings { .. } => 3,
        ActionPanelState::AudioOutput { .. } => audio.available_outputs().len().saturating_add(1),
        ActionPanelState::PlaybackSettings { .. } => 5,
        ActionPanelState::ThemeSettings { .. } => 6,
        ActionPanelState::LyricsImportTxt { .. } => 3,
        ActionPanelState::AddDirectory { .. } => 2,
        ActionPanelState::RemoveDirectory { .. } => sorted_folder_paths(core).len().max(1),
    };

    if let ActionPanelState::Root { selected, .. } = panel {
        if option_count == 0 {
            *selected = 0;
        } else if *selected >= option_count {
            *selected = option_count - 1;
        }
    }

    match key {
        KeyCode::Esc => {
            panel.close();
            core.dirty = true;
        }
        KeyCode::Up => {
            update_panel_selection(panel, option_count, false);
            core.dirty = true;
        }
        KeyCode::Down => {
            update_panel_selection(panel, option_count, true);
            core.dirty = true;
        }
        KeyCode::Left | KeyCode::Backspace => {
            *panel = match panel {
                ActionPanelState::Mode { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::SetPlaybackMode,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::PlaylistPlay { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::PlayPlaylist,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::PlaylistAdd { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::AddSelectedToPlaylist,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::PlaylistAddNowPlaying { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::AddNowPlayingToPlaylist,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::PlaylistCreate { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::CreatePlaylist,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::PlaylistRemove { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::RemovePlaylist,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::AudioSettings { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::AudioDriverSettings,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::PlaybackSettings { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::PlaybackSettings,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::AddDirectory { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::AddDirectory,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::AudioOutput { .. } => {
                    ActionPanelState::AudioSettings { selected: 0 }
                }
                ActionPanelState::ThemeSettings { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(RootActionId::Theme, recent_root_actions),
                    query: String::new(),
                },
                ActionPanelState::LyricsImportTxt { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::ImportTxtToLyrics,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::RemoveDirectory { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::RemoveDirectory,
                        recent_root_actions,
                    ),
                    query: String::new(),
                },
                ActionPanelState::Root { .. } | ActionPanelState::Closed => {
                    ActionPanelState::Closed
                }
            };
            core.dirty = true;
        }
        KeyCode::Enter => match panel.clone() {
            ActionPanelState::Root { selected, query } => {
                let visible_actions = root_visible_actions(&query, recent_root_actions);
                let Some(selected_action) = visible_actions.get(selected).map(|entry| entry.action)
                else {
                    core.status = String::from("No matching actions");
                    core.dirty = true;
                    return;
                };

                update_recent_root_actions(recent_root_actions, selected_action);

                match selected_action {
                    RootActionId::AddDirectory => {
                        *panel = ActionPanelState::AddDirectory {
                            selected: 1,
                            input: String::new(),
                        };
                        core.dirty = true;
                    }
                    RootActionId::AddSelectedToPlaylist => {
                        if sorted_playlist_names(core).is_empty() {
                            core.status = String::from("No playlists available");
                            core.dirty = true;
                            panel.close();
                        } else {
                            *panel = ActionPanelState::PlaylistAdd { selected: 0 };
                            core.dirty = true;
                        }
                    }
                    RootActionId::AddNowPlayingToPlaylist => {
                        if sorted_playlist_names(core).is_empty() {
                            core.status = String::from("No playlists available");
                            core.dirty = true;
                            panel.close();
                        } else {
                            *panel = ActionPanelState::PlaylistAddNowPlaying { selected: 0 };
                            core.dirty = true;
                        }
                    }
                    RootActionId::SetPlaybackMode => {
                        *panel = ActionPanelState::Mode { selected: 0 };
                        core.dirty = true;
                    }
                    RootActionId::PlaybackSettings => {
                        *panel = ActionPanelState::PlaybackSettings { selected: 0 };
                        core.dirty = true;
                    }
                    RootActionId::PlayPlaylist => {
                        if sorted_playlist_names(core).is_empty() {
                            core.status = String::from("No playlists available");
                            core.dirty = true;
                            panel.close();
                        } else {
                            *panel = ActionPanelState::PlaylistPlay { selected: 0 };
                            core.dirty = true;
                        }
                    }
                    RootActionId::RemoveSelectedFromPlaylist => {
                        core.remove_selected_from_current_playlist();
                        auto_save_state(core, &*audio);
                        panel.close();
                    }
                    RootActionId::CreatePlaylist => {
                        *panel = ActionPanelState::PlaylistCreate {
                            selected: 0,
                            input: String::new(),
                        };
                        core.dirty = true;
                    }
                    RootActionId::RemovePlaylist => {
                        *panel = ActionPanelState::PlaylistRemove { selected: 0 };
                        core.dirty = true;
                    }
                    RootActionId::RemoveDirectory => {
                        *panel = ActionPanelState::RemoveDirectory { selected: 0 };
                        core.dirty = true;
                    }
                    RootActionId::RescanLibrary => {
                        core.rescan();
                        panel.close();
                    }
                    RootActionId::AudioDriverSettings => {
                        *panel = ActionPanelState::AudioSettings { selected: 0 };
                        core.dirty = true;
                    }
                    RootActionId::Theme => {
                        let selected = match core.theme {
                            Theme::Dark | Theme::Ocean => 0,
                            Theme::PitchBlack => 1,
                            Theme::Galaxy => 2,
                            Theme::Matrix | Theme::Forest => 3,
                            Theme::Demonic => 4,
                            Theme::CottonCandy | Theme::Sunset => 5,
                        };
                        *panel = ActionPanelState::ThemeSettings { selected };
                        core.dirty = true;
                    }
                    RootActionId::SaveState => {
                        if let Err(err) = save_state_with_audio(core, &*audio) {
                            core.status = format!("save error: {err:#}");
                            core.dirty = true;
                        }
                        panel.close();
                    }
                    RootActionId::ClearListenHistory => {
                        core.clear_stats_requested = true;
                        core.status = String::from("Clearing listen history...");
                        core.dirty = true;
                        panel.close();
                    }
                    RootActionId::MinimizeToTray => {
                        minimize_to_tray();
                        core.status = String::from("Minimized to tray");
                        core.dirty = true;
                        panel.close();
                    }
                    RootActionId::ImportTxtToLyrics => {
                        *panel = ActionPanelState::LyricsImportTxt {
                            selected: 0,
                            path_input: String::new(),
                            interval_input: String::from("3"),
                        };
                        core.dirty = true;
                    }
                    RootActionId::ClosePanel => {
                        panel.close();
                        core.dirty = true;
                    }
                }
            }
            ActionPanelState::Mode { selected } => {
                core.playback_mode = match selected {
                    0 => PlaybackMode::Normal,
                    1 => PlaybackMode::Shuffle,
                    2 => PlaybackMode::Loop,
                    _ => PlaybackMode::LoopOne,
                };
                core.status = String::from("Playback mode updated");
                core.dirty = true;
                auto_save_state(core, &*audio);
                panel.close();
            }
            ActionPanelState::PlaylistPlay { selected } => {
                let playlists = sorted_playlist_names(core);
                if let Some(name) = playlists.get(selected) {
                    core.load_playlist_queue(name);
                    if let Some(err) = core
                        .next_track_path()
                        .and_then(|path| audio.play(&path).err())
                    {
                        core.status = concise_audio_error(&err);
                        core.dirty = true;
                    }
                } else {
                    core.status = String::from("No playlists available");
                    core.dirty = true;
                }
                panel.close();
            }
            ActionPanelState::PlaylistAdd { selected } => {
                let playlists = sorted_playlist_names(core);
                if let Some(name) = playlists.get(selected) {
                    core.add_selected_to_playlist(name);
                    auto_save_state(core, &*audio);
                } else {
                    core.status = String::from("No playlists available");
                    core.dirty = true;
                }
                panel.close();
            }
            ActionPanelState::PlaylistAddNowPlaying { selected } => {
                let playlists = sorted_playlist_names(core);
                if let Some(name) = playlists.get(selected) {
                    if let Some(path) = audio.current_track() {
                        core.add_track_to_playlist(name, path);
                        auto_save_state(core, &*audio);
                    } else {
                        core.status = String::from("No track currently playing");
                        core.dirty = true;
                    }
                } else {
                    core.status = String::from("No playlists available");
                    core.dirty = true;
                }
                panel.close();
            }
            ActionPanelState::PlaylistCreate { input, .. } => {
                let name = input.trim();
                if name.is_empty() {
                    core.status = String::from("Enter a playlist name");
                    core.dirty = true;
                    return;
                }
                core.create_playlist(name);
                auto_save_state(core, &*audio);
                panel.close();
            }
            ActionPanelState::PlaylistRemove { selected } => {
                let playlists = sorted_playlist_names(core);
                if let Some(name) = playlists.get(selected) {
                    core.remove_playlist(name);
                    auto_save_state(core, &*audio);
                } else {
                    core.status = String::from("No playlists available");
                    core.dirty = true;
                }
                panel.close();
            }
            ActionPanelState::AudioSettings { selected } => match selected {
                0 => {
                    if let Err(err) = audio.reload_driver() {
                        core.status =
                            format!("Audio reset failed: {err}. Use / -> Audio driver settings");
                    } else {
                        core.status = format!(
                            "Audio reset on {}. Reload again if needed",
                            audio
                                .output_name()
                                .unwrap_or_else(|| String::from("unknown output"))
                        );
                    }
                    core.dirty = true;
                    panel.close();
                }
                1 => {
                    let selected = audio
                        .selected_output_device()
                        .and_then(|name| {
                            audio
                                .available_outputs()
                                .iter()
                                .position(|entry| entry == &name)
                        })
                        .map(|index| index.saturating_add(1))
                        .unwrap_or(0);
                    *panel = ActionPanelState::AudioOutput { selected };
                    core.dirty = true;
                }
                _ => {
                    *panel = ActionPanelState::Root {
                        selected: root_selected_for_action(
                            RootActionId::AudioDriverSettings,
                            recent_root_actions,
                        ),
                        query: String::new(),
                    };
                    core.dirty = true;
                }
            },
            ActionPanelState::AudioOutput { selected } => {
                let outputs = audio.available_outputs();
                let result = if selected == 0 {
                    audio.set_output_device(None)
                } else {
                    match outputs.get(selected - 1) {
                        Some(name) => audio.set_output_device(Some(name.as_str())),
                        None => Err(anyhow::anyhow!("selected audio output is unavailable")),
                    }
                };

                if let Err(err) = result {
                    core.status = format!("Output switch failed: {err}. Try Reload audio driver");
                } else {
                    core.status = format!(
                        "Output: {}",
                        audio
                            .output_name()
                            .unwrap_or_else(|| String::from("unknown output"))
                    );
                    auto_save_state(core, &*audio);
                }
                core.dirty = true;
                panel.close();
            }
            ActionPanelState::PlaybackSettings { selected } => match selected {
                0 => {
                    core.loudness_normalization = !core.loudness_normalization;
                    audio.set_loudness_normalization(core.loudness_normalization);
                    core.status = format!(
                        "Loudness normalization: {}",
                        if core.loudness_normalization {
                            "On"
                        } else {
                            "Off"
                        }
                    );
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                1 => {
                    core.crossfade_seconds = next_crossfade_seconds(core.crossfade_seconds);
                    audio.set_crossfade_seconds(core.crossfade_seconds);
                    core.status = format!("Crossfade: {}", crossfade_label(core.crossfade_seconds));
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                2 => {
                    core.scrub_seconds = next_scrub_seconds(core.scrub_seconds);
                    core.status = format!("Scrub length: {}", scrub_label(core.scrub_seconds));
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                3 => {
                    core.stats_enabled = !core.stats_enabled;
                    core.status = format!(
                        "Stats tracking: {}",
                        if core.stats_enabled { "On" } else { "Off" }
                    );
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                _ => {
                    *panel = ActionPanelState::Root {
                        selected: root_selected_for_action(
                            RootActionId::PlaybackSettings,
                            recent_root_actions,
                        ),
                        query: String::new(),
                    };
                    core.dirty = true;
                }
            },
            ActionPanelState::ThemeSettings { selected } => {
                core.theme = match selected {
                    0 => Theme::Dark,
                    1 => Theme::PitchBlack,
                    2 => Theme::Galaxy,
                    3 => Theme::Matrix,
                    4 => Theme::Demonic,
                    _ => Theme::CottonCandy,
                };
                core.status = format!("Theme: {}", theme_label(core.theme));
                core.dirty = true;
                auto_save_state(core, &*audio);
                panel.close();
            }
            ActionPanelState::LyricsImportTxt {
                selected,
                path_input,
                interval_input,
            } => {
                if selected < 2 {
                    return;
                }
                let trimmed_path = path_input.trim();
                if trimmed_path.is_empty() {
                    core.status = String::from("Provide TXT path to import");
                    core.dirty = true;
                    return;
                }
                let interval = interval_input.trim().parse::<u32>().unwrap_or(3).max(1);
                core.import_txt_to_lyrics(Path::new(trimmed_path), interval);
                panel.close();
            }
            ActionPanelState::AddDirectory { selected, input } => {
                if selected == 1 {
                    let trimmed = input.trim();
                    if trimmed.is_empty() {
                        core.status = String::from("Enter a folder path or choose externally");
                        core.dirty = true;
                        return;
                    }
                    core.add_folder(Path::new(trimmed));
                    auto_save_state(core, &*audio);
                    panel.close();
                } else {
                    match choose_folder_externally() {
                        Ok(Some(path)) => {
                            core.add_folder(&path);
                            auto_save_state(core, &*audio);
                            panel.close();
                        }
                        Ok(None) => {
                            core.status = String::from("Folder selection cancelled");
                            core.dirty = true;
                        }
                        Err(err) => {
                            core.status = format!("Folder picker failed: {err}");
                            core.dirty = true;
                        }
                    }
                }
            }
            ActionPanelState::RemoveDirectory { selected } => {
                let folders = sorted_folder_paths(core);
                if let Some(path) = folders.get(selected) {
                    core.remove_folder(path);
                    auto_save_state(core, &*audio);
                } else {
                    core.status = String::from("No folders available");
                    core.dirty = true;
                }
                panel.close();
            }
            ActionPanelState::Closed => {}
        },
        _ => {}
    }
}

#[cfg(windows)]
fn choose_folder_externally() -> Result<Option<PathBuf>> {
    let _ = disable_raw_mode();
    struct RawModeRestore;
    impl Drop for RawModeRestore {
        fn drop(&mut self) {
            let _ = enable_raw_mode();
        }
    }
    let _restore = RawModeRestore;

    let script = "Add-Type -AssemblyName System.Windows.Forms; $dlg = New-Object System.Windows.Forms.FolderBrowserDialog; $dlg.Description = 'Select music folder'; if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.WriteLine($dlg.SelectedPath) }";
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()?;

    if !output.status.success() {
        return Err(anyhow::anyhow!("powerShell folder picker failed"));
    }

    let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if selected.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(selected)))
    }
}

#[cfg(not(windows))]
fn choose_folder_externally() -> Result<Option<PathBuf>> {
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("/"));

    let attempts: [(&str, Vec<&str>); 2] = [
        (
            "zenity",
            vec![
                "--file-selection",
                "--directory",
                "--title=Select music folder",
            ],
        ),
        ("kdialog", vec!["--getexistingdirectory", home.as_str()]),
    ];

    let mut picker_available = false;
    for (command, args) in attempts {
        match try_external_folder_picker(command, &args)? {
            FolderPickerResult::Unavailable => continue,
            FolderPickerResult::Cancelled => {
                picker_available = true;
                break;
            }
            FolderPickerResult::Selected(path) => return Ok(Some(path)),
        }
    }

    if picker_available {
        Ok(None)
    } else {
        Err(anyhow::anyhow!(
            "No external folder picker found. Install zenity or kdialog, or type a path manually."
        ))
    }
}

#[cfg(not(windows))]
enum FolderPickerResult {
    Unavailable,
    Cancelled,
    Selected(PathBuf),
}

#[cfg(not(windows))]
fn try_external_folder_picker(command: &str, args: &[&str]) -> Result<FolderPickerResult> {
    let output = match std::process::Command::new(command).args(args).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FolderPickerResult::Unavailable);
        }
        Err(err) => {
            return Err(anyhow::anyhow!(
                "failed to launch folder picker {command}: {err}"
            ));
        }
    };

    if !output.status.success() {
        return Ok(FolderPickerResult::Cancelled);
    }

    match parse_folder_picker_selection(&output.stdout) {
        Some(path) => Ok(FolderPickerResult::Selected(path)),
        None => Ok(FolderPickerResult::Cancelled),
    }
}

#[cfg(not(windows))]
fn parse_folder_picker_selection(stdout: &[u8]) -> Option<PathBuf> {
    let selected = String::from_utf8_lossy(stdout).trim().to_string();
    if selected.is_empty() {
        return None;
    }
    let cleaned = selected.strip_prefix("file://").unwrap_or(&selected);
    Some(PathBuf::from(cleaned))
}

#[cfg(windows)]
const TRAY_CALLBACK_MSG: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1;
#[cfg(windows)]
const TRAY_RESTORE_MSG: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 2;
#[cfg(windows)]
const TRAY_ICON_ID: u32 = 1;

#[cfg(windows)]
static TRAY_RESTORE_REQUESTED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static TRAY_CONTROLLER: OnceLock<Mutex<TrayController>> = OnceLock::new();

#[cfg(windows)]
fn minimize_to_tray() {
    if let Some(mut controller) = tray_controller() {
        controller.minimize();
    }
}

#[cfg(not(windows))]
fn minimize_to_tray() {}

#[cfg(windows)]
fn pump_tray_events(core: &mut TuneCore) {
    if let Some(mut controller) = tray_controller() {
        controller.pump();
    }

    if TRAY_RESTORE_REQUESTED.swap(false, Ordering::SeqCst) {
        restore_from_tray();
        if let Some(mut controller) = tray_controller() {
            controller.hide_icon();
        }
        core.status = String::from("Restored from tray");
        core.dirty = true;
    }
}

#[cfg(not(windows))]
fn pump_tray_events(_core: &mut TuneCore) {}

#[cfg(windows)]
fn cleanup_tray() {
    if let Some(mut controller) = tray_controller() {
        controller.cleanup();
    }
}

#[cfg(not(windows))]
fn cleanup_tray() {}

#[cfg(windows)]
fn restore_from_tray() {
    if let Some(mut controller) = tray_controller() {
        controller.restore();
    }
}

#[cfg(windows)]
fn tray_controller() -> Option<std::sync::MutexGuard<'static, TrayController>> {
    let lock = TRAY_CONTROLLER.get_or_init(|| Mutex::new(TrayController::new()));
    lock.lock().ok()
}

#[cfg(windows)]
struct TrayController {
    window: isize,
    hidden_window: isize,
    icon_visible: bool,
}

#[cfg(windows)]
impl TrayController {
    fn new() -> Self {
        Self {
            window: 0,
            hidden_window: 0,
            icon_visible: false,
        }
    }

    fn minimize(&mut self) {
        use windows_sys::Win32::System::Console::GetConsoleWindow;

        unsafe {
            if self.ensure_window().is_none() {
                return;
            }
            if !self.icon_visible && !self.show_icon() {
                return;
            }
            let mut hidden = std::ptr::null_mut();
            let primary = tray_host_window();
            if hide_window(primary) {
                hidden = primary;
            } else {
                let console = GetConsoleWindow();
                if hide_window(console) {
                    hidden = console;
                }
            }

            if !hidden.is_null() {
                self.hidden_window = hidden as isize;
            }
        }
    }

    fn restore(&mut self) {
        use windows_sys::Win32::System::Console::GetConsoleWindow;
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_RESTORE, SetForegroundWindow};

        unsafe {
            let hwnd = if self.hidden_window != 0 {
                self.hidden_window as _
            } else {
                GetConsoleWindow()
            };
            if !hwnd.is_null() {
                show_window(hwnd);
                windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
            }
        }
        self.hidden_window = 0;
    }

    fn pump(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        };

        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn cleanup(&mut self) {
        unsafe {
            self.hide_icon();
            if self.window != 0 {
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(self.window as _);
                self.window = 0;
            }
        }
    }

    fn ensure_window(&mut self) -> Option<windows_sys::Win32::Foundation::HWND> {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, RegisterClassW, WNDCLASSW,
        };

        if self.window != 0 {
            return Some(self.window as _);
        }

        let class_name = to_wide("TuneTuiTrayWindow");
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };

        let mut wc: WNDCLASSW = unsafe { std::mem::zeroed() };
        wc.lpfnWndProc = Some(tray_wnd_proc);
        wc.hInstance = instance;
        wc.lpszClassName = class_name.as_ptr();
        unsafe {
            RegisterClassW(&wc);
        }

        self.window = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            ) as isize
        };

        (self.window != 0).then_some(self.window as _)
    }

    fn show_icon(&mut self) -> bool {
        use windows_sys::Win32::UI::Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFYICONDATAW, Shell_NotifyIconW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{IDI_APPLICATION, LoadIconW};

        let Some(hwnd) = self.ensure_window() else {
            return false;
        };

        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ICON_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = TRAY_CALLBACK_MSG;
        nid.hIcon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };

        let tip = to_wide("TuneTUI - click to restore");
        let max_len = nid.szTip.len().saturating_sub(1).min(tip.len());
        nid.szTip[..max_len].copy_from_slice(&tip[..max_len]);

        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) != 0 };
        if ok {
            self.icon_visible = true;
        }
        ok
    }

    fn hide_icon(&mut self) {
        use windows_sys::Win32::UI::Shell::{NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW};

        if !self.icon_visible || self.window == 0 {
            return;
        }

        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.window as _;
        nid.uID = TRAY_ICON_ID;
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        self.icon_visible = false;
    }
}

#[cfg(windows)]
fn tray_host_window() -> windows_sys::Win32::Foundation::HWND {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetForegroundWindow, IsWindowVisible,
    };

    unsafe {
        let console = GetConsoleWindow();
        if !console.is_null() {
            let console_root = GetAncestor(console, GA_ROOT);
            if !console_root.is_null() && IsWindowVisible(console_root) != 0 {
                return console_root;
            }
            if IsWindowVisible(console) != 0 {
                return console;
            }
        }

        let foreground = GetForegroundWindow();
        if !foreground.is_null() {
            let root = GetAncestor(foreground, GA_ROOT);
            if !root.is_null() {
                return root;
            }
            return foreground;
        }

        console
    }
}

#[cfg(windows)]
fn hide_window(hwnd: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsWindowVisible, SW_HIDE, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SetWindowPos, ShowWindow, ShowWindowAsync,
    };

    if hwnd.is_null() {
        return false;
    }

    unsafe {
        ShowWindow(hwnd, SW_HIDE);
        ShowWindowAsync(hwnd, SW_HIDE);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        IsWindowVisible(hwnd) == 0
    }
}

#[cfg(windows)]
fn show_window(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
        SetWindowPos, ShowWindow, ShowWindowAsync,
    };

    if hwnd.is_null() {
        return;
    }

    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_SHOWWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        ShowWindowAsync(hwnd, SW_SHOW);
        ShowWindow(hwnd, SW_SHOW);
    }
}

#[cfg(windows)]
unsafe extern "system" fn tray_wnd_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    };

    if msg == TRAY_RESTORE_MSG {
        TRAY_RESTORE_REQUESTED.store(true, Ordering::SeqCst);
        return 0;
    }

    if msg == TRAY_CALLBACK_MSG {
        let event = lparam as u32;
        if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
            TRAY_RESTORE_REQUESTED.store(true, Ordering::SeqCst);
        }
        return 0;
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioEngine;
    use crate::model::PersistedState;
    use crate::model::Track;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    struct TestAudioEngine {
        paused: bool,
        current: Option<PathBuf>,
        queued: Option<PathBuf>,
        finished: bool,
        position: Option<Duration>,
        duration: Option<Duration>,
        played: Vec<PathBuf>,
        stopped: bool,
        outputs: Vec<String>,
        selected_output: Option<String>,
        reload_calls: usize,
        loudness_normalization: bool,
        crossfade_seconds: u16,
    }

    impl TestAudioEngine {
        fn new() -> Self {
            Self {
                paused: false,
                current: None,
                queued: None,
                finished: false,
                position: None,
                duration: None,
                played: Vec::new(),
                stopped: false,
                outputs: vec![String::from("Headphones"), String::from("Speakers")],
                selected_output: None,
                reload_calls: 0,
                loudness_normalization: false,
                crossfade_seconds: 0,
            }
        }

        fn finished_with_current(path: &str) -> Self {
            Self {
                paused: false,
                current: Some(PathBuf::from(path)),
                queued: None,
                finished: true,
                position: None,
                duration: None,
                played: Vec::new(),
                stopped: false,
                outputs: vec![String::from("Headphones"), String::from("Speakers")],
                selected_output: None,
                reload_calls: 0,
                loudness_normalization: false,
                crossfade_seconds: 0,
            }
        }
    }

    impl AudioEngine for TestAudioEngine {
        fn play(&mut self, path: &Path) -> Result<()> {
            self.current = Some(path.to_path_buf());
            self.queued = None;
            self.finished = false;
            self.position = Some(Duration::from_secs(0));
            self.played.push(path.to_path_buf());
            Ok(())
        }

        fn queue_crossfade(&mut self, path: &Path) -> Result<()> {
            self.queued = Some(path.to_path_buf());
            Ok(())
        }

        fn tick(&mut self) {
            if !self.finished {
                return;
            }

            if let Some(path) = self.queued.take() {
                self.current = Some(path.clone());
                self.played.push(path);
                self.finished = false;
                self.position = Some(Duration::from_secs(u64::from(self.crossfade_seconds)));
            }
        }

        fn pause(&mut self) {
            self.paused = true;
        }

        fn resume(&mut self) {
            self.paused = false;
        }

        fn stop(&mut self) {
            self.stopped = true;
            self.current = None;
            self.finished = false;
            self.position = None;
        }

        fn is_paused(&self) -> bool {
            self.paused
        }

        fn current_track(&self) -> Option<&Path> {
            self.current.as_deref()
        }

        fn position(&self) -> Option<Duration> {
            self.position
        }

        fn duration(&self) -> Option<Duration> {
            self.duration
        }

        fn seek_to(&mut self, position: Duration) -> Result<()> {
            if self.current.is_none() {
                return Err(anyhow::anyhow!("no active track"));
            }

            let clamped = self
                .duration
                .map(|duration| position.min(duration))
                .unwrap_or(position);
            self.position = Some(clamped);
            self.queued = None;
            self.finished = false;
            Ok(())
        }

        fn volume(&self) -> f32 {
            1.0
        }

        fn set_volume(&mut self, _volume: f32) {}

        fn output_name(&self) -> Option<String> {
            Some(
                self.selected_output
                    .clone()
                    .unwrap_or_else(|| String::from("System default output (test)")),
            )
        }

        fn reload_driver(&mut self) -> Result<()> {
            self.reload_calls = self.reload_calls.saturating_add(1);
            Ok(())
        }

        fn available_outputs(&self) -> Vec<String> {
            self.outputs.clone()
        }

        fn selected_output_device(&self) -> Option<String> {
            self.selected_output.clone()
        }

        fn set_output_device(&mut self, output: Option<&str>) -> Result<()> {
            if let Some(name) =
                output.filter(|name| !self.outputs.iter().any(|entry| entry == *name))
            {
                return Err(anyhow::anyhow!("audio output device not found: {name}"));
            }
            self.selected_output = output.map(ToOwned::to_owned);
            Ok(())
        }

        fn loudness_normalization(&self) -> bool {
            self.loudness_normalization
        }

        fn set_loudness_normalization(&mut self, enabled: bool) {
            self.loudness_normalization = enabled;
        }

        fn crossfade_seconds(&self) -> u16 {
            self.crossfade_seconds
        }

        fn set_crossfade_seconds(&mut self, seconds: u16) {
            self.crossfade_seconds = seconds;
        }

        fn crossfade_queued_track(&self) -> Option<&Path> {
            self.queued.as_deref()
        }

        fn is_finished(&self) -> bool {
            self.finished
        }
    }

    #[test]
    fn action_panel_mode_selection_applies_mode() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 3,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::Mode { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(core.playback_mode, crate::model::PlaybackMode::Shuffle);
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn root_action_search_executes_selected_filtered_action() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 0,
            query: String::new(),
        };
        let mut recent_root_actions = Vec::new();

        for ch in "theme".chars() {
            handle_action_panel_input_with_recent(
                &mut core,
                &mut audio,
                &mut panel,
                &mut recent_root_actions,
                KeyCode::Char(ch),
            );
        }
        handle_action_panel_input_with_recent(
            &mut core,
            &mut audio,
            &mut panel,
            &mut recent_root_actions,
            KeyCode::Enter,
        );

        assert!(matches!(panel, ActionPanelState::ThemeSettings { .. }));
        assert_eq!(recent_root_actions, vec![RootActionId::Theme]);
    }

    #[test]
    fn recent_root_actions_are_unique_and_capped_at_three() {
        let mut recent = Vec::new();
        update_recent_root_actions(&mut recent, RootActionId::AddDirectory);
        update_recent_root_actions(&mut recent, RootActionId::Theme);
        update_recent_root_actions(&mut recent, RootActionId::SaveState);
        update_recent_root_actions(&mut recent, RootActionId::Theme);
        update_recent_root_actions(&mut recent, RootActionId::PlaybackSettings);

        assert_eq!(
            recent,
            vec![
                RootActionId::PlaybackSettings,
                RootActionId::Theme,
                RootActionId::SaveState,
            ]
        );
    }

    #[test]
    fn root_visible_actions_prioritize_recent_without_duplicates() {
        let visible = root_visible_actions("", &[RootActionId::Theme, RootActionId::AddDirectory]);

        assert_eq!(visible[0].action, RootActionId::Theme);
        assert_eq!(visible[1].action, RootActionId::AddDirectory);
        assert_eq!(
            visible
                .iter()
                .filter(|entry| entry.action == RootActionId::Theme)
                .count(),
            1
        );
    }

    #[test]
    fn action_panel_playlist_add_requires_playlist() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 1,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(core.status, "No playlists available");
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_now_playing_add_requires_playlist() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("now.mp3"));
        let mut panel = ActionPanelState::Root {
            selected: 2,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(core.status, "No playlists available");
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_adds_now_playing_track_to_playlist() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.create_playlist("mix");
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("now.mp3"));
        let mut panel = ActionPanelState::Root {
            selected: 2,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(
            panel,
            ActionPanelState::PlaylistAddNowPlaying { .. }
        ));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::Closed));

        let playlist = core.playlists.get("mix").expect("playlist exists");
        assert_eq!(playlist.tracks, vec![PathBuf::from("now.mp3")]);
    }

    #[test]
    fn action_panel_add_directory_from_typed_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("typed-folder");
        std::fs::create_dir_all(&dir).expect("create");

        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 0,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(
            panel,
            ActionPanelState::AddDirectory { selected: 1, .. }
        ));

        for ch in dir.to_string_lossy().chars() {
            handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Char(ch));
        }
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        let expected = crate::config::normalize_path(&dir);
        assert!(
            core.folders
                .iter()
                .any(|folder| crate::config::normalize_path(folder) == expected)
        );
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_remove_directory_from_list() {
        let mut state = PersistedState::default();
        state.folders.push(PathBuf::from(r"E:\LOCALMUSIC"));
        let mut core = TuneCore::from_persisted(state);
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 9,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::RemoveDirectory { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(core.folders.is_empty());
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_create_playlist_from_input() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 7,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(
            panel,
            ActionPanelState::PlaylistCreate { selected: 0, .. }
        ));

        for ch in "mix".chars() {
            handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Char(ch));
        }
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert!(core.playlists.contains_key("mix"));
        assert!(core.browser_entries.iter().any(|entry| {
            entry.kind == crate::core::BrowserEntryKind::Playlist && entry.label == "[PL] mix"
        }));
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_remove_playlist_from_list() {
        let mut state = PersistedState::default();
        state
            .playlists
            .insert(String::from("mix"), crate::model::Playlist::default());
        let mut core = TuneCore::from_persisted(state);
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 8,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::PlaylistRemove { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(!core.playlists.contains_key("mix"));
        assert_eq!(core.status, "Playlist removed");
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_audio_driver_reload_updates_status() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 11,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::AudioSettings { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(audio.reload_calls, 1);
        assert_eq!(
            core.status,
            "Audio reset on System default output (test). Reload again if needed"
        );
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_audio_output_selection_sets_device() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 11,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(
            panel,
            ActionPanelState::AudioOutput { selected: 0 }
        ));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(
            audio.selected_output_device(),
            Some(String::from("Speakers"))
        );
        assert_eq!(core.status, "Output: Speakers");
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn playback_settings_toggle_loudness_and_crossfade() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 4,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::PlaybackSettings { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(core.loudness_normalization);
        assert!(audio.loudness_normalization());

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert_eq!(core.crossfade_seconds, 2);
        assert_eq!(audio.crossfade_seconds(), 2);

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert_eq!(core.scrub_seconds, 10);

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(!core.stats_enabled);
    }

    #[test]
    fn stats_left_on_range_cycles_back() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = crate::core::HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Range(3);
        core.stats_range = crate::stats::StatsRange::Days30;

        assert!(handle_stats_inline_input(
            &mut core,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)
        ));
        assert_eq!(core.stats_range, crate::stats::StatsRange::Days7);
        assert!(matches!(
            core.stats_focus,
            crate::core::StatsFilterFocus::Range(2)
        ));
    }

    #[test]
    fn stats_left_from_search_scrolls_up_when_scrolled() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = crate::core::HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Search;
        core.stats_scroll = 3;

        assert!(handle_stats_inline_input(
            &mut core,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)
        ));
        assert_eq!(core.stats_scroll, 2);
        assert!(matches!(
            core.stats_focus,
            crate::core::StatsFilterFocus::Search
        ));
    }

    #[test]
    fn stats_shift_up_resets_scroll_to_top() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = crate::core::HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Search;
        core.stats_range = crate::stats::StatsRange::Days7;
        core.stats_scroll = 9;

        assert!(handle_stats_inline_input(
            &mut core,
            KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)
        ));
        assert_eq!(core.stats_scroll, 0);
        assert!(matches!(
            core.stats_focus,
            crate::core::StatsFilterFocus::Range(2)
        ));
    }

    #[test]
    fn theme_settings_updates_core() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 12,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(
            panel,
            ActionPanelState::ThemeSettings { selected: 0 }
        ));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert_eq!(core.theme, Theme::PitchBlack);
        assert_eq!(core.status, "Theme: Pitch Black");
    }

    #[test]
    fn persisted_state_contains_selected_audio_output() {
        let core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        audio
            .set_output_device(Some("Speakers"))
            .expect("select output");

        let state = persisted_state_with_audio(&core, &audio);
        assert_eq!(state.selected_output_device, Some(String::from("Speakers")));
    }

    #[test]
    fn persisted_state_contains_playback_settings() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.loudness_normalization = true;
        core.crossfade_seconds = 4;
        core.scrub_seconds = 30;
        core.theme = Theme::Galaxy;
        core.stats_enabled = false;
        let audio = TestAudioEngine::new();

        let state = persisted_state_with_audio(&core, &audio);
        assert!(state.loudness_normalization);
        assert_eq!(state.crossfade_seconds, 4);
        assert_eq!(state.scrub_seconds, 30);
        assert_eq!(state.theme, Theme::Galaxy);
        assert!(!state.stats_enabled);
    }

    #[test]
    fn invalid_saved_output_falls_back_to_default_with_actionable_status() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();

        apply_saved_audio_output(&mut core, &mut audio, Some(String::from("Missing Device")));

        assert_eq!(audio.selected_output_device(), None);
        assert!(
            core.status
                .contains("Saved output 'Missing Device' unavailable")
        );
        assert!(core.status.contains("Audio driver settings"));
    }

    #[test]
    fn listen_tracker_flushes_partial_session_while_playing() {
        let core = TuneCore::from_persisted(PersistedState::default());
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker::default();
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("a.mp3"));
        audio.duration = Some(Duration::from_secs(200));

        assert!(!tracker.tick(&core, &audio, &mut stats));
        let active = tracker.active.as_mut().expect("active session");
        active.playing_started_at = Instant::now().checked_sub(Duration::from_secs(12));

        assert!(tracker.tick(&core, &audio, &mut stats));
        let event = stats.events.last().expect("partial event");
        assert!(event.listened_seconds >= 10);
        assert!(!event.counted_play);
    }

    #[test]
    fn finalize_after_partial_flush_retains_total_listen_and_play_count() {
        let mut stats = StatsStore::default();
        stats.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("a.mp3"),
            title: String::from("a"),
            artist: None,
            album: None,
            started_at_epoch_seconds: 10,
            listened_seconds: 30,
            completed: false,
            duration_seconds: Some(200),
            counted_play_override: Some(false),
            allow_short_listen: true,
        });

        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                track_path: PathBuf::from("a.mp3"),
                title: String::from("a"),
                artist: None,
                album: None,
                started_at_epoch_seconds: 10,
                playing_started_at: None,
                listened: Duration::from_secs(35),
                persisted_listened_seconds: 30,
                play_count_recorded: false,
                last_position: Some(Duration::from_secs(200)),
                duration: Some(Duration::from_secs(200)),
            }),
        };

        assert!(tracker.finalize_active(&mut stats));
        let snapshot = stats.query(
            &crate::stats::StatsQuery {
                range: crate::stats::StatsRange::Lifetime,
                sort: crate::stats::StatsSort::ListenTime,
                artist_filter: String::new(),
                album_filter: String::new(),
                search: String::new(),
            },
            1_000,
        );
        assert_eq!(snapshot.total_listen_seconds, 35);
        assert_eq!(snapshot.total_plays, 1);
    }

    #[test]
    fn listen_tracker_records_play_during_partial_flush_once() {
        let core = TuneCore::from_persisted(PersistedState::default());
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker::default();
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("a.mp3"));
        audio.duration = Some(Duration::from_secs(200));

        assert!(!tracker.tick(&core, &audio, &mut stats));
        let active = tracker.active.as_mut().expect("active session");
        active.playing_started_at = Instant::now().checked_sub(Duration::from_secs(31));

        assert!(tracker.tick(&core, &audio, &mut stats));
        let first_counted = stats
            .events
            .iter()
            .filter(|event| event.counted_play)
            .count();
        assert_eq!(first_counted, 1);

        let active = tracker.active.as_mut().expect("active session");
        active.playing_started_at = Instant::now().checked_sub(Duration::from_secs(42));
        assert!(tracker.tick(&core, &audio, &mut stats));

        let total_counted = stats
            .events
            .iter()
            .filter(|event| event.counted_play)
            .count();
        assert_eq!(total_counted, 1);
    }

    #[test]
    fn finalize_keeps_short_tail_after_partial_flush() {
        let mut stats = StatsStore::default();
        stats.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("song.mp3"),
            title: String::from("song"),
            artist: None,
            album: None,
            started_at_epoch_seconds: 100,
            listened_seconds: 140,
            completed: false,
            duration_seconds: Some(153),
            counted_play_override: Some(true),
            allow_short_listen: true,
        });

        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                track_path: PathBuf::from("song.mp3"),
                title: String::from("song"),
                artist: None,
                album: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(153),
                persisted_listened_seconds: 140,
                play_count_recorded: true,
                last_position: Some(Duration::from_secs(153)),
                duration: Some(Duration::from_secs(153)),
            }),
        };

        assert!(tracker.finalize_active(&mut stats));
        let snapshot = stats.query(
            &crate::stats::StatsQuery {
                range: crate::stats::StatsRange::Lifetime,
                sort: crate::stats::StatsSort::ListenTime,
                artist_filter: String::new(),
                album_filter: String::new(),
                search: String::new(),
            },
            1_000,
        );

        assert_eq!(snapshot.total_listen_seconds, 153);
        assert_eq!(snapshot.total_plays, 1);
        assert_eq!(snapshot.recent.len(), 1);
        assert_eq!(snapshot.recent[0].listened_seconds, 153);
    }

    #[test]
    fn pause_resume_session_accumulates_and_counts_one_play() {
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                track_path: PathBuf::from("song.mp3"),
                title: String::from("song"),
                artist: None,
                album: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(40),
                persisted_listened_seconds: 0,
                play_count_recorded: false,
                last_position: Some(Duration::from_secs(40)),
                duration: Some(Duration::from_secs(153)),
            }),
        };

        assert!(tracker.finalize_active(&mut stats));
        let snapshot = stats.query(
            &crate::stats::StatsQuery {
                range: crate::stats::StatsRange::Lifetime,
                sort: crate::stats::StatsSort::ListenTime,
                artist_filter: String::new(),
                album_filter: String::new(),
                search: String::new(),
            },
            1_000,
        );

        assert_eq!(snapshot.total_listen_seconds, 40);
        assert_eq!(snapshot.total_plays, 1);
        assert_eq!(snapshot.recent.len(), 1);
        assert_eq!(snapshot.recent[0].listened_seconds, 40);
        assert!(snapshot.recent[0].counted_play);
    }

    #[test]
    fn scrubbing_session_can_exceed_track_duration_in_listen_time() {
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                track_path: PathBuf::from("song.mp3"),
                title: String::from("song"),
                artist: None,
                album: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(190),
                persisted_listened_seconds: 0,
                play_count_recorded: false,
                last_position: Some(Duration::from_secs(153)),
                duration: Some(Duration::from_secs(153)),
            }),
        };

        assert!(tracker.finalize_active(&mut stats));
        let snapshot = stats.query(
            &crate::stats::StatsQuery {
                range: crate::stats::StatsRange::Lifetime,
                sort: crate::stats::StatsSort::ListenTime,
                artist_filter: String::new(),
                album_filter: String::new(),
                search: String::new(),
            },
            1_000,
        );

        assert_eq!(snapshot.total_listen_seconds, 190);
        assert_eq!(snapshot.total_plays, 1);
    }

    #[test]
    fn short_skip_session_under_ten_seconds_is_not_logged() {
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                track_path: PathBuf::from("skip.mp3"),
                title: String::from("skip"),
                artist: None,
                album: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(2),
                persisted_listened_seconds: 0,
                play_count_recorded: false,
                last_position: Some(Duration::from_secs(2)),
                duration: Some(Duration::from_secs(153)),
            }),
        };

        assert!(!tracker.finalize_active(&mut stats));
        let snapshot = stats.query(
            &crate::stats::StatsQuery {
                range: crate::stats::StatsRange::Lifetime,
                sort: crate::stats::StatsSort::ListenTime,
                artist_filter: String::new(),
                album_filter: String::new(),
                search: String::new(),
            },
            1_000,
        );

        assert_eq!(snapshot.total_listen_seconds, 0);
        assert_eq!(snapshot.total_plays, 0);
        assert!(snapshot.recent.is_empty());
    }

    #[test]
    fn scrub_current_track_clamps_within_bounds() {
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("a.mp3"));
        audio.duration = Some(Duration::from_secs(100));
        audio.position = Some(Duration::from_secs(98));

        scrub_current_track(&mut audio, 10, true).expect("scrub forward");
        assert_eq!(audio.position, Some(Duration::from_secs(100)));

        scrub_current_track(&mut audio, 120, false).expect("scrub backward");
        assert_eq!(audio.position, Some(Duration::from_secs(0)));
    }

    #[test]
    fn scrub_current_track_clears_queued_crossfade() {
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("a.mp3"));
        audio.duration = Some(Duration::from_secs(120));
        audio.position = Some(Duration::from_secs(50));
        audio.queued = Some(PathBuf::from("b.mp3"));

        scrub_current_track(&mut audio, 15, true).expect("scrub");

        assert_eq!(audio.position, Some(Duration::from_secs(65)));
        assert_eq!(audio.crossfade_queued_track(), None);
    }

    #[test]
    fn auto_advance_plays_next_track_when_finished() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from("a.mp3"),
                title: String::from("a"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("b.mp3"),
                title: String::from("b"),
                artist: None,
                album: None,
            },
        ];
        core.queue = vec![0, 1];
        core.current_queue_index = Some(0);

        let mut audio = TestAudioEngine::finished_with_current("a.mp3");
        maybe_auto_advance_track(&mut core, &mut audio);

        assert_eq!(audio.played, vec![PathBuf::from("b.mp3")]);
        assert_eq!(core.current_queue_index, Some(1));
    }

    #[test]
    fn auto_advance_starts_next_track_within_crossfade_window() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from("a.mp3"),
                title: String::from("a"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("b.mp3"),
                title: String::from("b"),
                artist: None,
                album: None,
            },
        ];
        core.queue = vec![0, 1];
        core.current_queue_index = Some(0);

        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("a.mp3"));
        audio.duration = Some(Duration::from_secs(100));
        audio.position = Some(Duration::from_secs(95));
        audio.crossfade_seconds = 6;

        maybe_auto_advance_track(&mut core, &mut audio);

        assert_eq!(audio.played, Vec::<PathBuf>::new());
        assert_eq!(audio.crossfade_queued_track(), Some(Path::new("b.mp3")));
        assert_eq!(core.current_queue_index, Some(1));

        audio.finished = true;
        audio.tick();
        assert_eq!(audio.played, vec![PathBuf::from("b.mp3")]);
        assert_eq!(audio.position, Some(Duration::from_secs(6)));
    }

    #[test]
    fn auto_advance_stops_when_queue_ends() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![Track {
            path: PathBuf::from("a.mp3"),
            title: String::from("a"),
            artist: None,
            album: None,
        }];
        core.queue = vec![0];
        core.current_queue_index = Some(0);

        let mut audio = TestAudioEngine::finished_with_current("a.mp3");
        maybe_auto_advance_track(&mut core, &mut audio);

        assert!(audio.stopped);
        assert_eq!(core.status, "Reached end of queue");
    }

    #[test]
    fn inferred_tunetui_config_dir_uses_home_when_userprofile_missing() {
        let inferred = inferred_tunetui_config_dir(None, Some("/home/tune"), None);
        assert_eq!(inferred, Some(PathBuf::from("/home/tune/.config/tunetui")));
    }

    #[test]
    fn inferred_tunetui_config_dir_respects_existing_override() {
        let inferred =
            inferred_tunetui_config_dir(None, Some("/home/tune"), Some("/custom/tunetui-config"));
        assert_eq!(inferred, None);
    }

    #[test]
    fn should_set_ssh_term_when_over_ssh_and_term_is_missing() {
        assert!(should_set_ssh_term(Some("/dev/pts/0"), None, None, None));
    }

    #[test]
    fn should_not_set_ssh_term_when_terminal_is_already_set() {
        assert!(!should_set_ssh_term(
            Some("/dev/pts/0"),
            None,
            None,
            Some("xterm-256color")
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn parse_folder_picker_selection_handles_plain_path() {
        let parsed = parse_folder_picker_selection(b"/home/tune/music\n");
        assert_eq!(parsed, Some(PathBuf::from("/home/tune/music")));
    }

    #[cfg(not(windows))]
    #[test]
    fn parse_folder_picker_selection_strips_file_scheme() {
        let parsed = parse_folder_picker_selection(b"file:///home/tune/music\n");
        assert_eq!(parsed, Some(PathBuf::from("/home/tune/music")));
    }

    #[cfg(not(windows))]
    #[test]
    fn parse_folder_picker_selection_handles_empty_output() {
        let parsed = parse_folder_picker_selection(b"\n");
        assert_eq!(parsed, None);
    }
}
