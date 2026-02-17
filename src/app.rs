use crate::audio::{AudioEngine, NullAudioEngine, WasapiAudioEngine};
use crate::config;
use crate::core::{BrowserEntryKind, HeaderSection, LyricsMode, StatsFilterFocus, TuneCore};
use crate::library::{self, MetadataEdit};
use crate::model::{CoverArtTemplate, PlaybackMode, Theme};
use crate::online::{OnlineSession, Participant, TransportCommand, TransportEnvelope};
use crate::online_net::{
    LocalAction as NetworkLocalAction, NetworkEvent, NetworkRole, OnlineNetwork, build_invite_code,
    decode_invite_code, resolve_advertise_addr,
};
use crate::stats::{self, ListenSessionRecord, StatsStore};
use anyhow::{Context, Result};
use arboard::Clipboard;
use base64::Engine;
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
use std::io::{Write, stdout};
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
const STATS_TOP_SONGS_COUNT_OPTIONS: [u8; 5] = [5, 8, 10, 12, 15];
const PARTIAL_LISTEN_FLUSH_SECONDS: u32 = 10;
const ONLINE_DEFAULT_BIND_ADDR: &str = "0.0.0.0:7878";
const LOOP_RESTART_END_WINDOW_SECONDS: u64 = 2;
const LOOP_RESTART_START_WINDOW_SECONDS: u64 = 5;
const LOOP_RESTART_FALLBACK_MIN_PREVIOUS_SECONDS: u64 = 20;
const ONLINE_SYNC_CORRECTION_THRESHOLD_PAUSED_MS: i64 = 100;
const ONLINE_SYNC_CORRECTION_THRESHOLD_OPTIONS_MS: [u16; 8] =
    [100, 150, 200, 300, 400, 500, 750, 1000];
const MAX_ONLINE_EVENTS_PER_TICK: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnlinePlaybackSource {
    LocalQueue,
    SharedQueue,
}

struct OnlineRuntime {
    network: Option<OnlineNetwork>,
    local_nickname: String,
    last_transport_seq: u64,
    join_prompt_active: bool,
    join_code_input: String,
    join_prompt_button: JoinPromptButton,
    password_prompt_active: bool,
    password_prompt_mode: OnlinePasswordPromptMode,
    password_input: String,
    pending_join_invite_code: String,
    room_code_revealed: bool,
    host_invite_modal_active: bool,
    host_invite_code: String,
    host_invite_button: HostInviteModalButton,
    streamed_track_cache: HashMap<PathBuf, PathBuf>,
    pending_stream_path: Option<PathBuf>,
    remote_logical_track: Option<PathBuf>,
    remote_track_title: Option<String>,
    remote_track_artist: Option<String>,
    remote_track_album: Option<String>,
    remote_provider_track_id: Option<String>,
    last_remote_transport_origin: Option<String>,
    last_periodic_sync_at: Instant,
    online_playback_source: OnlinePlaybackSource,
}

impl OnlineRuntime {
    fn shutdown(&mut self) {
        if let Some(network) = self.network.take() {
            network.shutdown();
        }
        self.pending_stream_path = None;
        self.remote_logical_track = None;
        self.remote_track_title = None;
        self.remote_track_artist = None;
        self.remote_track_album = None;
        self.remote_provider_track_id = None;
        self.last_remote_transport_origin = None;
        self.password_prompt_active = false;
        self.password_prompt_mode = OnlinePasswordPromptMode::Host;
        self.password_input.clear();
        self.pending_join_invite_code.clear();
        self.join_prompt_button = JoinPromptButton::Join;
        self.room_code_revealed = false;
        self.host_invite_modal_active = false;
        self.host_invite_code.clear();
        self.host_invite_button = HostInviteModalButton::Copy;
        self.online_playback_source = OnlinePlaybackSource::LocalQueue;
    }

    fn host_invite_modal_view(&self) -> Option<crate::ui::HostInviteModalView> {
        if !self.host_invite_modal_active {
            return None;
        }
        Some(crate::ui::HostInviteModalView {
            invite_code: self.host_invite_code.clone(),
            copy_selected: matches!(self.host_invite_button, HostInviteModalButton::Copy),
        })
    }

    fn join_prompt_view(&self) -> Option<crate::ui::JoinPromptModalView> {
        if !self.join_prompt_active {
            return None;
        }
        Some(crate::ui::JoinPromptModalView {
            invite_code: self.join_code_input.clone(),
            paste_selected: matches!(self.join_prompt_button, JoinPromptButton::Paste),
        })
    }

    fn password_prompt_view(&self) -> Option<crate::ui::OnlinePasswordPromptView> {
        if !self.password_prompt_active {
            return None;
        }
        let (title, subtitle) = match self.password_prompt_mode {
            OnlinePasswordPromptMode::Host => (
                "Set Room Password",
                "This password encrypts the invite code and protects joins.",
            ),
            OnlinePasswordPromptMode::Join => (
                "Enter Room Password",
                "Needed to decrypt invite code and verify checksum.",
            ),
        };
        Some(crate::ui::OnlinePasswordPromptView {
            title: String::from(title),
            subtitle: String::from(subtitle),
            masked_input: "*".repeat(self.password_input.chars().count()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnlinePasswordPromptMode {
    Host,
    Join,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostInviteModalButton {
    Copy,
    Ok,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinPromptButton {
    Join,
    Paste,
}

impl JoinPromptButton {
    fn toggle(self) -> Self {
        match self {
            Self::Join => Self::Paste,
            Self::Paste => Self::Join,
        }
    }
}

impl HostInviteModalButton {
    fn toggle(self) -> Self {
        match self {
            Self::Copy => Self::Ok,
            Self::Ok => Self::Copy,
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveListenSession {
    playback_path: PathBuf,
    track_path: PathBuf,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    provider_track_id: Option<String>,
    started_at_epoch_seconds: i64,
    playing_started_at: Option<Instant>,
    listened: Duration,
    persisted_listened_seconds: u32,
    play_count_recorded: bool,
    pending_same_track_restart: bool,
    last_position: Option<Duration>,
    duration: Option<Duration>,
}

#[derive(Debug, Default)]
struct ListenTracker {
    active: Option<ActiveListenSession>,
}

#[derive(Debug, Clone)]
struct StatsIdentityHint {
    logical_path: PathBuf,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    provider_track_id: Option<String>,
}

impl ListenTracker {
    fn reset(&mut self) {
        self.active = None;
    }

    fn tick(
        &mut self,
        core: &TuneCore,
        audio: &dyn AudioEngine,
        stats: &mut StatsStore,
        identity_hint: Option<&StatsIdentityHint>,
    ) -> bool {
        let mut wrote_event = false;
        let current_track = audio.current_track().map(Path::to_path_buf);
        let current_position = audio.position();
        let crossfade_seconds = audio.crossfade_seconds();
        let paused = audio.is_paused();

        let finished = audio.is_finished();
        let mut force_completed = finished;
        let should_finalize = self.active.as_ref().is_some_and(|active| {
            let track_changed = current_track.as_ref() != Some(&active.playback_path);
            let restarted_same_track = !track_changed
                && !finished
                && same_track_restarted(active, current_position, paused, crossfade_seconds);
            if restarted_same_track {
                force_completed = true;
            }
            track_changed || finished || restarted_same_track
        });
        if should_finalize {
            wrote_event = self.finalize_active(stats, force_completed) || wrote_event;
        }

        if current_track.is_none() || finished {
            return wrote_event;
        }

        if self.active.is_none() {
            let path = current_track.expect("checked some");
            let (logical_path, provider_track_id, hint_title, hint_artist, hint_album) =
                if let Some(hint) = identity_hint {
                    (
                        hint.logical_path.clone(),
                        hint.provider_track_id.clone(),
                        hint.title.clone(),
                        hint.artist.clone(),
                        hint.album.clone(),
                    )
                } else {
                    (path.clone(), None, None, None, None)
                };
            let now = Instant::now();
            self.active = Some(ActiveListenSession {
                title: hint_title
                    .or_else(|| core.title_for_path(&logical_path))
                    .unwrap_or_else(|| {
                        logical_path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("-")
                            .to_string()
                    }),
                artist: hint_artist
                    .or_else(|| core.artist_for_path(&logical_path).map(ToOwned::to_owned)),
                album: hint_album
                    .or_else(|| core.album_for_path(&logical_path).map(ToOwned::to_owned)),
                playback_path: path,
                track_path: logical_path,
                provider_track_id,
                started_at_epoch_seconds: stats::now_epoch_seconds(),
                playing_started_at: (!paused).then_some(now),
                listened: Duration::ZERO,
                persisted_listened_seconds: 0,
                play_count_recorded: false,
                pending_same_track_restart: false,
                last_position: current_position,
                duration: audio.duration(),
            });
            return wrote_event;
        }

        if let Some(active) = self.active.as_mut() {
            let queued_same_track = audio
                .crossfade_queued_track()
                .is_some_and(|queued| queued == active.playback_path.as_path());
            active.pending_same_track_restart |= queued_same_track;
            active.last_position = current_position.or(active.last_position);
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

    fn finalize_active(&mut self, stats: &mut StatsStore, force_completed: bool) -> bool {
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
        let completed = force_completed
            || active
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
            provider_track_id: active.provider_track_id,
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
            provider_track_id: active.provider_track_id.clone(),
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

fn same_track_restarted(
    active: &ActiveListenSession,
    current_position: Option<Duration>,
    paused: bool,
    crossfade_seconds: u16,
) -> bool {
    if paused || !active.pending_same_track_restart {
        return false;
    }

    let Some(current) = current_position else {
        return false;
    };
    let Some(previous) = active.last_position else {
        return false;
    };
    if current >= previous {
        return false;
    }

    let start_window = Duration::from_secs(
        LOOP_RESTART_START_WINDOW_SECONDS.max(u64::from(crossfade_seconds).saturating_add(2)),
    );
    let was_near_end = active.duration.is_some_and(|duration| {
        let end_window = Duration::from_secs(LOOP_RESTART_END_WINDOW_SECONDS);
        previous >= duration.saturating_sub(end_window)
    }) || previous
        >= Duration::from_secs(LOOP_RESTART_FALLBACK_MIN_PREVIOUS_SECONDS);
    let now_near_start = current <= start_window;
    was_near_end && now_near_start
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
    MetadataEditor,
    MinimizeToTray,
    ImportTxtToLyrics,
    ClosePanel,
}

const ROOT_ACTIONS: [RootActionId; 19] = [
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
    RootActionId::MetadataEditor,
    RootActionId::MinimizeToTray,
    RootActionId::ImportTxtToLyrics,
    RootActionId::ClosePanel,
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootVisibleAction {
    action: RootActionId,
    label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetadataEditorState {
    selected_track_path: Option<PathBuf>,
    copy_target_label: String,
    copy_target_paths: Vec<PathBuf>,
    title_input: String,
    artist_input: String,
    album_input: String,
    confirm_all_songs_cover_copy: bool,
}

impl MetadataEditorState {
    fn options(&self) -> Vec<String> {
        if self.selected_track_path.is_some() {
            vec![
                format!("Title: {}", self.title_input),
                format!("Artist: {}", self.artist_input),
                format!("Album: {}", self.album_input),
                String::from("Save embedded tags"),
                String::from("Clear title/artist/album tags"),
                format!("Copy now playing cover art to {}", self.copy_target_label),
                String::from("Back"),
            ]
        } else {
            vec![
                if self.confirm_all_songs_cover_copy {
                    format!(
                        "Confirm: copy now playing cover art to {}",
                        self.copy_target_label
                    )
                } else {
                    format!("Copy now playing cover art to {}", self.copy_target_label)
                },
                String::from("Back"),
            ]
        }
    }

    fn metadata_edit(&self) -> MetadataEdit {
        MetadataEdit {
            title: Some(self.title_input.clone()),
            artist: Some(self.artist_input.clone()),
            album: Some(self.album_input.clone()),
        }
    }
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
        RootActionId::MetadataEditor => "Edit selected track metadata",
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
    OnlineDelaySettings {
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
    MetadataEditor {
        selected: usize,
        state: MetadataEditorState,
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
            Self::OnlineDelaySettings { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Online Delay Settings"),
                hint: String::from("Enter apply  Backspace back"),
                search_query: None,
                options: online_delay_settings_options(core),
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
            Self::MetadataEditor { selected, state } => Some(crate::ui::ActionPanelView {
                title: String::from("Edit Metadata"),
                hint: String::from("Type fields  Enter save/select  Backspace back"),
                search_query: None,
                options: state.options(),
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
    let saved_volume = state.saved_volume;
    let mut core = TuneCore::from_persisted(state);
    let mut stats_store = stats::load_stats().unwrap_or_default();
    let mut listen_tracker = ListenTracker::default();

    let mut audio: Box<dyn AudioEngine> = match WasapiAudioEngine::new() {
        Ok(engine) => Box::new(engine),
        Err(_) => Box::new(NullAudioEngine::new()),
    };

    apply_audio_preferences_from_core(&core, &mut *audio);
    apply_saved_volume(&mut *audio, saved_volume);
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
        join_prompt_button: JoinPromptButton::Join,
        password_prompt_active: false,
        password_prompt_mode: OnlinePasswordPromptMode::Host,
        password_input: String::new(),
        pending_join_invite_code: String::new(),
        room_code_revealed: false,
        host_invite_modal_active: false,
        host_invite_code: String::new(),
        host_invite_button: HostInviteModalButton::Copy,
        streamed_track_cache: HashMap::new(),
        pending_stream_path: None,
        remote_logical_track: None,
        remote_track_title: None,
        remote_track_artist: None,
        remote_track_album: None,
        remote_provider_track_id: None,
        last_remote_transport_origin: None,
        last_periodic_sync_at: Instant::now(),
        online_playback_source: OnlinePlaybackSource::LocalQueue,
    };

    let result: Result<()> = loop {
        pump_tray_events(&mut core);
        drain_online_network_events(&mut core, &mut *audio, &mut online_runtime);
        audio.tick();
        maybe_publish_online_playback_sync(&core, &*audio, &mut online_runtime);
        let stats_identity_hint = online_streaming_stats_identity(&online_runtime, &*audio);
        if core.stats_enabled
            && listen_tracker.tick(
                &core,
                &*audio,
                &mut stats_store,
                stats_identity_hint.as_ref(),
            )
        {
            let _ = stats::save_stats(&stats_store);
        }
        if stats_enabled_last
            && !core.stats_enabled
            && listen_tracker.finalize_active(&mut stats_store, false)
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
        maybe_start_online_shared_queue_if_idle(&mut core, &mut *audio, &mut online_runtime);
        maybe_auto_advance_track(&mut core, &mut *audio, &mut online_runtime);
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
                let join_prompt_modal = online_runtime.join_prompt_view();
                let host_invite_modal = online_runtime.host_invite_modal_view();
                let password_prompt_modal = online_runtime.password_prompt_view();
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
                    crate::ui::OverlayViews {
                        join_prompt_modal: join_prompt_modal.as_ref(),
                        online_password_prompt: password_prompt_modal.as_ref(),
                        host_invite_modal: host_invite_modal.as_ref(),
                        room_code_revealed: online_runtime.room_code_revealed,
                    },
                )
            })?;
            core.dirty = false;
            last_tick = Instant::now();
        }

        if !event::poll(Duration::from_millis(33))? {
            continue;
        }

        let event = event::read()?;
        if let Event::Paste(text) = &event
            && core.header_section == HeaderSection::Online
            && online_runtime.password_prompt_active
        {
            append_password_input(&mut online_runtime, text);
            core.status = format!(
                "Password length: {}",
                online_runtime.password_input.chars().count()
            );
            core.dirty = true;
            continue;
        }
        if let Event::Paste(text) = &event
            && core.header_section == HeaderSection::Online
            && online_runtime.join_prompt_active
        {
            append_invite_input(&mut online_runtime, text);
            online_runtime.join_prompt_button = JoinPromptButton::Join;
            core.status = format!("Enter invite code: {}", online_runtime.join_code_input);
            core.dirty = true;
            continue;
        }
        if let Event::Mouse(mouse) = event {
            handle_mouse_with_panel(
                &mut core,
                &mut *audio,
                &mut action_panel,
                &mut recent_root_actions,
                &online_runtime,
                mouse,
                library_rect,
            );
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
                Some(&online_runtime),
                key.code,
            );
            continue;
        }

        if handle_online_password_prompt_input(&mut core, key, &mut online_runtime) {
            continue;
        }

        if handle_host_invite_modal_input(&mut core, key, &mut online_runtime) {
            continue;
        }

        if handle_online_inline_input(&mut core, &*audio, key, &mut online_runtime) {
            continue;
        }
        if handle_stats_inline_input(&mut core, key) {
            continue;
        }
        if handle_lyrics_inline_input(&mut core, &*audio, key) {
            continue;
        }

        match key.code {
            KeyCode::Char(ch)
                if (key.modifiers.contains(KeyModifiers::CONTROL)
                    && ch.eq_ignore_ascii_case(&'c'))
                    || ch == '\u{3}' =>
            {
                break Ok(());
            }
            KeyCode::Char(_)
                if key_event_matches_ctrl_char(&key, 's')
                    && core.header_section == HeaderSection::Library =>
            {
                let selected_paths = core.selected_paths_for_online_queue_action();
                let added = core.online_queue_paths(&selected_paths);
                if let Some(network) = online_runtime.network.as_ref() {
                    for item in added {
                        network.send_local_action(NetworkLocalAction::QueueAdd(item));
                    }
                }
            }
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
            KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'n') => {
                if let Some(path) = core.next_track_path() {
                    if let Err(err) = audio.play(&path) {
                        core.status = concise_audio_error(&err);
                        core.dirty = true;
                    } else {
                        publish_current_playback_state(&core, &*audio, &online_runtime);
                    }
                }
            }
            KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'b') => {
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
            KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'m') => {
                core.cycle_mode();
                auto_save_state(&mut core, &*audio);
            }
            KeyCode::Tab => core.cycle_header_section(),
            KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'t') => {
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
            KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'r') => core.rescan(),
            KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'s') => {
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
    if listen_tracker.finalize_active(&mut stats_store, false) {
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

fn maybe_auto_advance_track(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    online_runtime: &mut OnlineRuntime,
) {
    if audio.current_track().is_none() || audio.is_paused() {
        return;
    }

    if core.online.session.is_some() {
        maybe_auto_advance_online_track(core, audio, online_runtime);
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

fn maybe_auto_advance_online_track(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    online_runtime: &mut OnlineRuntime,
) {
    if !audio.is_finished() {
        return;
    }

    let local_is_authority = core
        .online
        .session
        .as_ref()
        .and_then(online_authority_nickname)
        .is_some_and(|authority| authority.eq_ignore_ascii_case(&online_runtime.local_nickname));
    if !local_is_authority {
        return;
    }

    let next_shared = core
        .online
        .session
        .as_ref()
        .and_then(|session| session.shared_queue.first().cloned());
    if let Some(shared_item) = next_shared {
        if online_runtime.pending_stream_path.as_ref() == Some(&shared_item.path) {
            return;
        }

        let switched = ensure_remote_track(core, audio, online_runtime, &shared_item.path);
        let stream_pending = online_runtime.pending_stream_path.as_ref() == Some(&shared_item.path);
        if switched || stream_pending {
            consume_shared_queue_item(core, online_runtime, Some(shared_item.path.clone()));
            online_runtime.online_playback_source = OnlinePlaybackSource::SharedQueue;
            if switched {
                publish_current_playback_state(core, audio, online_runtime);
            }
        }
        return;
    }

    if online_runtime.online_playback_source == OnlinePlaybackSource::SharedQueue {
        audio.stop();
        online_runtime.online_playback_source = OnlinePlaybackSource::LocalQueue;
        core.status = String::from("Reached end of shared queue");
        core.dirty = true;
        publish_transport_command(core, online_runtime, TransportCommand::StopPlayback);
        return;
    }

    if let Some(path) = core.next_track_path() {
        match audio.play(&path) {
            Ok(()) => {
                online_runtime.online_playback_source = OnlinePlaybackSource::LocalQueue;
                publish_current_playback_state(core, audio, online_runtime);
            }
            Err(err) => {
                core.status = concise_audio_error(&err);
                core.dirty = true;
            }
        }
    } else {
        audio.stop();
        online_runtime.online_playback_source = OnlinePlaybackSource::LocalQueue;
        core.status = String::from("Reached end of queue");
        core.dirty = true;
        publish_transport_command(core, online_runtime, TransportCommand::StopPlayback);
    }
}

fn maybe_start_online_shared_queue_if_idle(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    online_runtime: &mut OnlineRuntime,
) {
    if audio.current_track().is_some() || online_runtime.pending_stream_path.is_some() {
        return;
    }

    let local_is_authority = core
        .online
        .session
        .as_ref()
        .and_then(online_authority_nickname)
        .is_some_and(|authority| authority.eq_ignore_ascii_case(&online_runtime.local_nickname));
    if !local_is_authority {
        return;
    }

    let next_shared = core
        .online
        .session
        .as_ref()
        .and_then(|session| session.shared_queue.first().cloned());
    let Some(shared_item) = next_shared else {
        return;
    };

    let switched = ensure_remote_track(core, audio, online_runtime, &shared_item.path);
    let stream_pending = online_runtime.pending_stream_path.as_ref() == Some(&shared_item.path);
    if switched || stream_pending {
        consume_shared_queue_item(core, online_runtime, Some(shared_item.path.clone()));
        online_runtime.online_playback_source = OnlinePlaybackSource::SharedQueue;
        if switched {
            audio.resume();
            publish_current_playback_state(core, audio, online_runtime);
        }
    }
}

fn online_authority_nickname(session: &OnlineSession) -> Option<&str> {
    if let Some(last_transport) = session.last_transport.as_ref()
        && session.participants.iter().any(|participant| {
            participant
                .nickname
                .eq_ignore_ascii_case(&last_transport.origin_nickname)
        })
    {
        return Some(last_transport.origin_nickname.as_str());
    }
    session
        .participants
        .iter()
        .find(|participant| participant.is_host)
        .map(|participant| participant.nickname.as_str())
}

fn consume_shared_queue_item(
    core: &mut TuneCore,
    online_runtime: &OnlineRuntime,
    expected_path: Option<PathBuf>,
) {
    if let Some(network) = online_runtime.network.as_ref() {
        network.send_local_action(NetworkLocalAction::QueueConsume { expected_path });
        return;
    }
    if let Some(session) = core.online.session.as_mut() {
        let can_consume = match (session.shared_queue.first(), expected_path.as_ref()) {
            (Some(_), None) => true,
            (Some(next), Some(expected)) => next.path == *expected,
            _ => false,
        };
        if can_consume {
            session.shared_queue.remove(0);
        }
    }
}

fn key_code_matches_char(code: KeyCode, expected: char) -> bool {
    matches!(code, KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&expected))
}

fn key_event_matches_ctrl_char(key: &KeyEvent, expected: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key_code_matches_char(key.code, expected)
}

fn online_tab_allows_global_shortcut(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('-') | KeyCode::Char('_')
    )
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
        KeyCode::Char(ch)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                && !ch.is_control() =>
        {
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
        KeyCode::Char(_) => false,
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
        if key_event_matches_ctrl_char(&key, 'e') {
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
                KeyCode::Char(ch)
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && ch.eq_ignore_ascii_case(&'t') =>
                {
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
    _audio: &dyn AudioEngine,
    key: KeyEvent,
    online_runtime: &mut OnlineRuntime,
) -> bool {
    if core.header_section != HeaderSection::Online {
        return false;
    }

    if key_event_matches_ctrl_char(&key, 'c') {
        return false;
    }

    if online_runtime.join_prompt_active {
        match key.code {
            KeyCode::Esc => {
                online_runtime.join_prompt_active = false;
                online_runtime.join_code_input.clear();
                online_runtime.join_prompt_button = JoinPromptButton::Join;
                core.status = String::from("Join cancelled");
                core.dirty = true;
                return true;
            }
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down => {
                online_runtime.join_prompt_button = online_runtime.join_prompt_button.toggle();
                core.dirty = true;
                return true;
            }
            KeyCode::Backspace => {
                online_runtime.join_code_input.pop();
                online_runtime.join_prompt_button = JoinPromptButton::Join;
                core.status = format!("Enter invite code: {}", online_runtime.join_code_input);
                core.dirty = true;
                return true;
            }
            KeyCode::Char(ch)
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && ch.eq_ignore_ascii_case(&'v') =>
            {
                match paste_invite_from_clipboard(online_runtime) {
                    Ok(()) => {
                        core.status =
                            format!("Pasted invite code: {}", online_runtime.join_code_input);
                    }
                    Err(err) => {
                        core.status = format!("Clipboard paste failed: {err}");
                    }
                }
                online_runtime.join_prompt_button = JoinPromptButton::Join;
                core.dirty = true;
                return true;
            }
            KeyCode::Enter => {
                if matches!(online_runtime.join_prompt_button, JoinPromptButton::Paste) {
                    match paste_invite_from_clipboard(online_runtime) {
                        Ok(()) => {
                            core.status =
                                format!("Pasted invite code: {}", online_runtime.join_code_input);
                        }
                        Err(err) => {
                            core.status = format!("Clipboard paste failed: {err}");
                        }
                    }
                    online_runtime.join_prompt_button = JoinPromptButton::Join;
                    core.dirty = true;
                    return true;
                }
                if online_runtime.join_code_input.trim().is_empty() {
                    core.status = String::from("Enter invite code, then press Enter");
                    core.dirty = true;
                    return true;
                }
                online_runtime.pending_join_invite_code =
                    online_runtime.join_code_input.trim().to_string();
                online_runtime.join_prompt_active = false;
                online_runtime.join_code_input.clear();
                online_runtime.join_prompt_button = JoinPromptButton::Join;
                online_runtime.password_prompt_active = true;
                online_runtime.password_prompt_mode = OnlinePasswordPromptMode::Join;
                online_runtime.password_input.clear();
                core.status = String::from("Enter room password, then press Enter");
                core.dirty = true;
                return true;
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                append_invite_char(online_runtime, ch);
                online_runtime.join_prompt_button = JoinPromptButton::Join;
                core.status = format!("Enter invite code: {}", online_runtime.join_code_input);
                core.dirty = true;
                return true;
            }
            _ => return true,
        }
    }

    if key.code == KeyCode::Tab || key.code == KeyCode::Char('/') {
        return false;
    }

    match key.code {
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'h') => {
            if core.online.session.is_some() {
                core.status = String::from("Already in online room. Press l to leave first");
                core.dirty = true;
                return true;
            }
            online_runtime.password_prompt_active = true;
            online_runtime.password_prompt_mode = OnlinePasswordPromptMode::Host;
            online_runtime.password_input.clear();
            core.status = String::from("Set room password, then press Enter");
            core.dirty = true;
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'j') => {
            if core.online.session.is_some() {
                core.status = String::from("Already in online room. Press l to leave first");
                core.dirty = true;
                return true;
            }
            if let Some(room_code_input) = optional_env("TUNETUI_ONLINE_ROOM_CODE") {
                online_runtime.pending_join_invite_code = room_code_input;
                online_runtime.join_prompt_button = JoinPromptButton::Join;
                online_runtime.password_prompt_active = true;
                online_runtime.password_prompt_mode = OnlinePasswordPromptMode::Join;
                online_runtime.password_input.clear();
                core.status = String::from("Enter room password, then press Enter");
                core.dirty = true;
            } else {
                online_runtime.join_prompt_active = true;
                online_runtime.join_code_input.clear();
                online_runtime.join_prompt_button = JoinPromptButton::Join;
                core.status = String::from("Enter invite code: ");
                core.dirty = true;
            }
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'l') => {
            online_runtime.shutdown();
            online_runtime.last_transport_seq = 0;
            core.online_leave_room();
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'o') => {
            core.online_toggle_mode();
            if let (Some(network), Some(session)) =
                (&online_runtime.network, core.online.session.as_ref())
            {
                network.send_local_action(NetworkLocalAction::SetMode(session.mode));
            }
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'q') => {
            core.online_cycle_quality();
            if let (Some(network), Some(session)) =
                (&online_runtime.network, core.online.session.as_ref())
            {
                network.send_local_action(NetworkLocalAction::SetQuality(session.quality));
            }
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'t') => {
            online_runtime.room_code_revealed = !online_runtime.room_code_revealed;
            core.status = if online_runtime.room_code_revealed {
                String::from("Room code shown")
            } else {
                String::from("Room code hidden")
            };
            core.dirty = true;
            true
        }
        KeyCode::Char('2') => {
            if let Some(session) = core.online.session.as_ref() {
                match copy_invite_to_clipboard(&session.room_code) {
                    Ok(()) => {
                        core.status = String::from("Copied room code");
                    }
                    Err(err) => {
                        core.status = format!("Clipboard copy failed: {err}");
                    }
                }
                core.dirty = true;
            }
            true
        }
        _ => !online_tab_allows_global_shortcut(key.code),
    }
}

fn handle_online_password_prompt_input(
    core: &mut TuneCore,
    key: KeyEvent,
    online_runtime: &mut OnlineRuntime,
) -> bool {
    if core.header_section != HeaderSection::Online || !online_runtime.password_prompt_active {
        return false;
    }
    if key_event_matches_ctrl_char(&key, 'c') {
        return false;
    }

    match key.code {
        KeyCode::Esc => {
            online_runtime.password_prompt_active = false;
            online_runtime.password_input.clear();
            online_runtime.pending_join_invite_code.clear();
            core.status = String::from("Password entry cancelled");
            core.dirty = true;
            true
        }
        KeyCode::Backspace => {
            online_runtime.password_input.pop();
            core.status = format!(
                "Password length: {}",
                online_runtime.password_input.chars().count()
            );
            core.dirty = true;
            true
        }
        KeyCode::Enter => {
            let password = online_runtime.password_input.trim().to_string();
            if password.is_empty() {
                core.status = String::from("Password is required");
                core.dirty = true;
                return true;
            }
            match online_runtime.password_prompt_mode {
                OnlinePasswordPromptMode::Host => {
                    online_runtime.password_prompt_active = false;
                    online_runtime.password_input.clear();
                    start_host_with_password(core, online_runtime, &password);
                }
                OnlinePasswordPromptMode::Join => {
                    let invite_code = online_runtime.pending_join_invite_code.trim().to_string();
                    if invite_code.is_empty() {
                        core.status = String::from("Invite code missing");
                        core.dirty = true;
                        return true;
                    }
                    online_runtime.password_prompt_active = false;
                    online_runtime.password_input.clear();
                    online_runtime.pending_join_invite_code.clear();
                    join_from_invite_code(core, online_runtime, &invite_code, &password);
                }
            }
            true
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            append_password_char(online_runtime, ch);
            core.status = format!(
                "Password length: {}",
                online_runtime.password_input.chars().count()
            );
            core.dirty = true;
            true
        }
        _ => true,
    }
}

fn start_host_with_password(
    core: &mut TuneCore,
    online_runtime: &mut OnlineRuntime,
    password: &str,
) {
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
    let invite_code = match build_invite_code(&advertise_addr, password) {
        Ok(code) => code,
        Err(err) => {
            core.online_leave_room();
            core.status = format!("Invite code build failed: {err}");
            core.dirty = true;
            return;
        }
    };

    if let Some(active) = core.online.session.as_mut() {
        active.room_code = invite_code.clone();
    }
    let Some(session) = core.online.session.clone() else {
        core.status = String::from("Online room initialization failed");
        core.dirty = true;
        return;
    };

    match OnlineNetwork::start_host(&bind_addr, session, Some(password.to_string())) {
        Ok(network) => {
            online_runtime.network = Some(network);
            online_runtime.host_invite_modal_active = true;
            online_runtime.host_invite_code = invite_code.clone();
            online_runtime.host_invite_button = HostInviteModalButton::Copy;
            core.status = format!("Hosting {bind_addr} invite {invite_code}");
            core.dirty = true;
        }
        Err(err) => {
            core.online_leave_room();
            core.status = format!("Online host failed: {err}");
            core.dirty = true;
        }
    }
}

fn handle_host_invite_modal_input(
    core: &mut TuneCore,
    key: KeyEvent,
    online_runtime: &mut OnlineRuntime,
) -> bool {
    if !online_runtime.host_invite_modal_active {
        return false;
    }
    if key_event_matches_ctrl_char(&key, 'c') {
        return false;
    }

    match key.code {
        KeyCode::Esc => {
            online_runtime.host_invite_modal_active = false;
            core.status = String::from("Invite dialog closed");
            core.dirty = true;
            true
        }
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Tab
        | KeyCode::BackTab => {
            online_runtime.host_invite_button = online_runtime.host_invite_button.toggle();
            core.dirty = true;
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'c') => {
            match copy_invite_to_clipboard(&online_runtime.host_invite_code) {
                Ok(()) => {
                    core.status =
                        format!("Copied invite code: {}", online_runtime.host_invite_code);
                }
                Err(err) => {
                    core.status = format!("Clipboard copy failed: {err}");
                }
            }
            core.dirty = true;
            true
        }
        KeyCode::Enter => {
            match online_runtime.host_invite_button {
                HostInviteModalButton::Copy => {
                    match copy_invite_to_clipboard(&online_runtime.host_invite_code) {
                        Ok(()) => {
                            core.status =
                                format!("Copied invite code: {}", online_runtime.host_invite_code);
                        }
                        Err(err) => {
                            core.status = format!("Clipboard copy failed: {err}");
                        }
                    }
                }
                HostInviteModalButton::Ok => {
                    online_runtime.host_invite_modal_active = false;
                    core.status = String::from("Invite dialog closed");
                }
            }
            core.dirty = true;
            true
        }
        _ => true,
    }
}

fn append_invite_char(online_runtime: &mut OnlineRuntime, ch: char) {
    if ch.is_ascii_whitespace() {
        return;
    }
    if online_runtime.join_code_input.len() >= 96 {
        return;
    }
    online_runtime.join_code_input.push(ch.to_ascii_uppercase());
}

fn append_invite_input(online_runtime: &mut OnlineRuntime, value: &str) {
    for ch in value.chars() {
        append_invite_char(online_runtime, ch);
    }
}

fn append_password_char(online_runtime: &mut OnlineRuntime, ch: char) {
    if ch.is_control() {
        return;
    }
    if online_runtime.password_input.len() >= 64 {
        return;
    }
    online_runtime.password_input.push(ch);
}

fn append_password_input(online_runtime: &mut OnlineRuntime, value: &str) {
    for ch in value.chars() {
        append_password_char(online_runtime, ch);
    }
}

fn paste_invite_from_clipboard(online_runtime: &mut OnlineRuntime) -> anyhow::Result<()> {
    let mut clipboard = Clipboard::new().context("clipboard unavailable")?;
    let value = clipboard.get_text().context("clipboard text unavailable")?;
    append_invite_input(online_runtime, &value);
    Ok(())
}

fn copy_invite_to_clipboard(invite_code: &str) -> anyhow::Result<()> {
    if let Ok(mut clipboard) = Clipboard::new()
        && clipboard.set_text(invite_code.to_string()).is_ok()
    {
        return Ok(());
    }

    copy_invite_via_osc52(invite_code)
}

fn copy_invite_via_osc52(invite_code: &str) -> anyhow::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(invite_code.as_bytes());
    let mut out = stdout();
    if std::env::var_os("TMUX").is_some() {
        write!(out, "\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")
            .context("tmux osc52 write failed")?;
    } else if std::env::var_os("STY").is_some() {
        write!(out, "\x1bP\x1b]52;c;{encoded}\x07\x1b\\").context("screen osc52 write failed")?;
    } else {
        write!(out, "\x1b]52;c;{encoded}\x07\x1b]52;c;{encoded}\x1b\\")
            .context("osc52 write failed")?;
    }
    std::io::Write::flush(&mut out).context("stdout flush failed")
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

fn join_from_invite_code(
    core: &mut TuneCore,
    online_runtime: &mut OnlineRuntime,
    invite_code: &str,
    password: &str,
) {
    online_runtime.shutdown();
    online_runtime.last_transport_seq = 0;

    let decoded = match decode_invite_code(invite_code, password) {
        Ok(decoded) => decoded,
        Err(err) => {
            core.status = format!("Invite decryption failed: {err}");
            core.dirty = true;
            return;
        }
    };

    core.online_join_room(&decoded.room_code, &online_runtime.local_nickname);
    match OnlineNetwork::start_client(
        &decoded.server_addr,
        &decoded.room_code,
        &online_runtime.local_nickname,
        Some(password.to_string()),
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
    let title = core.title_for_path(&path).or_else(|| {
        path.file_stem()
            .and_then(|name| name.to_str())
            .map(str::to_string)
    });
    let artist = core.artist_for_path(&path).map(str::to_string);
    let album = core.album_for_path(&path).map(str::to_string);
    let provider_track_id = provider_track_id_for_path(&path);
    publish_transport_command(
        core,
        online_runtime,
        TransportCommand::SetPlaybackState {
            path,
            title,
            artist,
            album,
            provider_track_id: Some(provider_track_id),
            position_ms,
            paused: audio.is_paused(),
        },
    );
}

fn provider_track_id_for_path(path: &Path) -> String {
    crate::config::normalize_path(path)
        .to_string_lossy()
        .to_ascii_lowercase()
}

fn publish_online_delay_update(core: &TuneCore, online_runtime: Option<&OnlineRuntime>) {
    let Some(runtime) = online_runtime else {
        return;
    };
    if let (Some(network), Some(session)) = (&runtime.network, core.online.session.as_ref())
        && let Some(local) = session.local_participant()
    {
        network.send_local_action(NetworkLocalAction::DelayUpdate {
            manual_extra_delay_ms: local.manual_extra_delay_ms,
            auto_ping_delay: local.auto_ping_delay,
        });
    }
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

fn online_streaming_stats_identity(
    online_runtime: &OnlineRuntime,
    audio: &dyn AudioEngine,
) -> Option<StatsIdentityHint> {
    let logical_path = online_runtime.remote_logical_track.clone()?;
    let current_playback_path = audio.current_track()?;
    let streamed_path = online_runtime.streamed_track_cache.get(&logical_path)?;
    if current_playback_path != streamed_path {
        return None;
    }

    Some(StatsIdentityHint {
        logical_path: logical_path.clone(),
        title: online_runtime.remote_track_title.clone(),
        artist: online_runtime.remote_track_artist.clone(),
        album: online_runtime.remote_track_album.clone(),
        provider_track_id: online_runtime
            .remote_provider_track_id
            .clone()
            .or_else(|| Some(provider_track_id_for_path(&logical_path))),
    })
}

fn drain_online_network_events(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    online_runtime: &mut OnlineRuntime,
) {
    let mut processed = 0_usize;
    loop {
        if processed >= MAX_ONLINE_EVENTS_PER_TICK {
            core.dirty = true;
            break;
        }
        let event = {
            let Some(network) = online_runtime.network.as_ref() else {
                return;
            };
            network.try_recv_event()
        };
        let Some(event) = event else {
            break;
        };
        processed = processed.saturating_add(1);

        match event {
            NetworkEvent::Status(message) => {
                let disconnected = is_online_disconnect_status(&message);
                core.status = message;
                if disconnected {
                    online_runtime.shutdown();
                    online_runtime.last_transport_seq = 0;
                    core.online_leave_room();
                }
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
                                "Streaming fallback active: {}",
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
                        online_runtime.last_remote_transport_origin =
                            Some(last_transport.origin_nickname.clone());
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

fn is_online_disconnect_status(message: &str) -> bool {
    message == "Disconnected from online host"
        || message.contains("Online socket read error")
        || message.contains("Host ended session")
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
        TransportCommand::StopPlayback => {
            audio.stop();
            online_runtime.pending_stream_path = None;
            online_runtime.remote_logical_track = None;
            online_runtime.online_playback_source = OnlinePlaybackSource::LocalQueue;
            core.status = String::from("Remote stopped playback");
            core.dirty = true;
        }
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
        TransportCommand::PlayTrack {
            path,
            title,
            artist,
            album,
            provider_track_id,
        } => {
            if ensure_remote_track(core, audio, online_runtime, path) {
                online_runtime.online_playback_source = OnlinePlaybackSource::LocalQueue;
                online_runtime.remote_track_title = title.clone();
                online_runtime.remote_track_artist = artist.clone();
                online_runtime.remote_track_album = album.clone();
                online_runtime.remote_provider_track_id = provider_track_id
                    .clone()
                    .or_else(|| Some(provider_track_id_for_path(path)));
                core.current_queue_index = core.queue_position_for_path(path);
                core.status = String::from("Remote switched track");
            }
            core.dirty = true;
        }
        TransportCommand::SetPlaybackState {
            path,
            title,
            artist,
            album,
            provider_track_id,
            position_ms,
            paused,
        } => {
            if !ensure_remote_track(core, audio, online_runtime, path) {
                core.dirty = true;
                return;
            }
            online_runtime.online_playback_source = OnlinePlaybackSource::LocalQueue;

            let local_ms = audio
                .position()
                .map(|position| position.as_millis() as i64)
                .unwrap_or(0);
            let remote_delay_ms = if *paused {
                0_i64
            } else {
                core.online
                    .session
                    .as_ref()
                    .and_then(|session| session.local_participant())
                    .map(|participant| i64::from(participant.effective_delay_ms()))
                    .unwrap_or(0)
            };
            let target_ms = (*position_ms as i64).saturating_add(remote_delay_ms);
            let drift_ms = (target_ms - local_ms).abs();
            let seek_threshold = if *paused {
                ONLINE_SYNC_CORRECTION_THRESHOLD_PAUSED_MS
            } else {
                i64::from(core.online_sync_correction_threshold_ms)
            };
            if drift_ms >= seek_threshold {
                let _ = audio.seek_to(Duration::from_millis(target_ms as u64));
            }

            if *paused {
                audio.pause();
            } else {
                audio.resume();
            }

            online_runtime.remote_logical_track = Some(path.clone());
            online_runtime.remote_track_title = title.clone();
            online_runtime.remote_track_artist = artist.clone();
            online_runtime.remote_track_album = album.clone();
            online_runtime.remote_provider_track_id = provider_track_id
                .clone()
                .or_else(|| Some(provider_track_id_for_path(path)));
            core.current_queue_index = core.queue_position_for_path(path);
            if let Some(session) = core.online.session.as_mut() {
                session.last_sync_drift_ms = drift_ms.min(i64::from(i32::MAX)) as i32;
            }
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
                    let source_nickname =
                        preferred_stream_source(core, online_runtime, network.role(), path);
                    network.request_track_stream(path.to_path_buf(), source_nickname.clone());
                    online_runtime.pending_stream_path = Some(path.to_path_buf());
                    online_runtime.remote_logical_track = Some(path.to_path_buf());
                    core.status = if let Some(source) = source_nickname {
                        format!("Remote track missing locally, requesting stream from {source}...")
                    } else {
                        String::from("Remote track missing locally, requesting stream...")
                    };
                } else {
                    core.status = concise_audio_error(&err);
                }
            }
            false
        }
    }
}

fn preferred_stream_source(
    core: &TuneCore,
    online_runtime: &OnlineRuntime,
    role: &NetworkRole,
    path: &Path,
) -> Option<String> {
    if !matches!(role, NetworkRole::Host) {
        return None;
    }
    let session = core.online.session.as_ref()?;
    let local_nickname = session
        .local_participant()
        .map(|entry| entry.nickname.as_str());
    let queue_owner = session
        .shared_queue
        .iter()
        .rev()
        .find(|item| {
            item.path == path
                && item.owner_nickname.as_deref().is_some_and(|owner| {
                    local_nickname
                        .map(|local| !owner.eq_ignore_ascii_case(local))
                        .unwrap_or(true)
                })
        })
        .and_then(|item| item.owner_nickname.clone());
    if queue_owner.is_some() {
        return queue_owner;
    }
    online_runtime
        .last_remote_transport_origin
        .as_deref()
        .filter(|origin| {
            local_nickname
                .map(|local| !origin.eq_ignore_ascii_case(local))
                .unwrap_or(true)
        })
        .map(str::to_string)
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
    state.saved_volume = audio.volume().clamp(0.0, MAX_VOLUME);
    state
}

fn apply_saved_volume(audio: &mut dyn AudioEngine, saved_volume: f32) {
    audio.set_volume(saved_volume.clamp(0.0, MAX_VOLUME));
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
        MouseEventKind::ScrollDown if inside_library => match core.header_section {
            HeaderSection::Library => core.select_next(),
            HeaderSection::Stats => {
                stats_scroll_down(core);
                core.stats_focus = StatsFilterFocus::Search;
            }
            HeaderSection::Lyrics | HeaderSection::Online => {}
        },
        MouseEventKind::ScrollUp if inside_library => match core.header_section {
            HeaderSection::Library => core.select_prev(),
            HeaderSection::Stats => {
                stats_scroll_up(core);
                core.stats_focus = StatsFilterFocus::Search;
            }
            HeaderSection::Lyrics | HeaderSection::Online => {}
        },
        _ => {}
    }
}

fn handle_mouse_with_panel(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    panel: &mut ActionPanelState,
    recent_root_actions: &mut Vec<RootActionId>,
    online_runtime: &OnlineRuntime,
    mouse: MouseEvent,
    library_rect: ratatui::prelude::Rect,
) {
    if panel.is_open() {
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                handle_action_panel_input_with_recent(
                    core,
                    audio,
                    panel,
                    recent_root_actions,
                    Some(online_runtime),
                    KeyCode::Down,
                );
                return;
            }
            MouseEventKind::ScrollUp => {
                handle_action_panel_input_with_recent(
                    core,
                    audio,
                    panel,
                    recent_root_actions,
                    Some(online_runtime),
                    KeyCode::Up,
                );
                return;
            }
            _ => {}
        }
    }

    handle_mouse(core, mouse, library_rect);
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

fn metadata_editor_state_for_selection(core: &TuneCore) -> Option<MetadataEditorState> {
    let entry = core.selected_browser_entry()?;
    let target_paths = core.selected_paths_for_browser_selection();
    if target_paths.is_empty() {
        return None;
    }

    match entry.kind {
        BrowserEntryKind::Track => {
            let path = entry.path;
            let metadata = library::metadata_snapshot_for_path(&path);
            Some(MetadataEditorState {
                selected_track_path: Some(path),
                copy_target_label: String::from("selected track"),
                copy_target_paths: target_paths,
                title_input: metadata.title.unwrap_or_default(),
                artist_input: metadata.artist.unwrap_or_default(),
                album_input: metadata.album.unwrap_or_default(),
                confirm_all_songs_cover_copy: false,
            })
        }
        BrowserEntryKind::Folder => Some(MetadataEditorState {
            selected_track_path: None,
            copy_target_label: String::from("current folder"),
            copy_target_paths: target_paths,
            title_input: String::new(),
            artist_input: String::new(),
            album_input: String::new(),
            confirm_all_songs_cover_copy: false,
        }),
        BrowserEntryKind::Playlist => Some(MetadataEditorState {
            selected_track_path: None,
            copy_target_label: String::from("current playlist"),
            copy_target_paths: target_paths,
            title_input: String::new(),
            artist_input: String::new(),
            album_input: String::new(),
            confirm_all_songs_cover_copy: false,
        }),
        BrowserEntryKind::AllSongs => Some(MetadataEditorState {
            selected_track_path: None,
            copy_target_label: String::from("all songs"),
            copy_target_paths: target_paths,
            title_input: String::new(),
            artist_input: String::new(),
            album_input: String::new(),
            confirm_all_songs_cover_copy: true,
        }),
        BrowserEntryKind::Back => None,
    }
}

fn now_playing_cover_source_path(core: &TuneCore, audio: &dyn AudioEngine) -> Option<PathBuf> {
    audio
        .current_track()
        .map(Path::to_path_buf)
        .or_else(|| core.current_path().map(Path::to_path_buf))
}

fn copy_now_playing_cover_to_paths(
    core: &mut TuneCore,
    source_path: &Path,
    targets: &[PathBuf],
    target_label: &str,
) {
    let Some(image_data) = library::embedded_cover_art(source_path) else {
        core.status = String::from("Now playing track has no embedded cover art");
        core.dirty = true;
        return;
    };

    let mut copied = 0usize;
    let mut failed = 0usize;
    let mut first_error = None;
    for target in targets {
        match library::write_embedded_cover_art(target, &image_data) {
            Ok(()) => {
                core.reload_track_metadata(target);
                copied += 1;
            }
            Err(err) => {
                failed += 1;
                if first_error.is_none() {
                    first_error = Some(err.to_string());
                }
            }
        }
    }

    core.status = if failed == 0 {
        format!("Copied cover art to {copied} {target_label}")
    } else {
        format!(
            "Copied cover art to {copied} {target_label} ({failed} failed: {})",
            first_error.unwrap_or_else(|| String::from("unknown error"))
        )
    };
    core.dirty = true;
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
        format!("Stats top songs rows: {}", core.stats_top_songs_count),
        format!(
            "Missing cover fallback: {}",
            cover_template_label(core.fallback_cover_template)
        ),
        String::from("Online sync delay settings"),
        String::from("Back"),
    ]
}

fn cover_template_label(_template: CoverArtTemplate) -> &'static str {
    "Music Note"
}

fn online_delay_settings_options(core: &TuneCore) -> Vec<String> {
    let detail = core
        .online
        .session
        .as_ref()
        .and_then(|session| session.local_participant())
        .map(|local| {
            format!(
                "Manual {}ms  Effective {}ms  Auto {}",
                local.manual_extra_delay_ms,
                local.effective_delay_ms(),
                if local.auto_ping_delay { "On" } else { "Off" }
            )
        })
        .unwrap_or_else(|| String::from("Join or host a room first"));

    vec![
        String::from("Manual delay -10ms"),
        String::from("Manual delay +10ms"),
        String::from("Toggle auto-ping delay"),
        String::from("Refresh ping calibration"),
        format!(
            "Sync correction threshold: {}ms",
            core.online_sync_correction_threshold_ms
        ),
        format!("Back ({detail})"),
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

fn next_stats_top_songs_count(current: u8) -> u8 {
    let index = STATS_TOP_SONGS_COUNT_OPTIONS
        .iter()
        .position(|entry| *entry == current)
        .unwrap_or_else(|| {
            STATS_TOP_SONGS_COUNT_OPTIONS
                .iter()
                .position(|entry| *entry >= current)
                .unwrap_or(0)
        });
    STATS_TOP_SONGS_COUNT_OPTIONS[(index + 1) % STATS_TOP_SONGS_COUNT_OPTIONS.len()]
}

fn next_online_sync_correction_threshold_ms(current: u16) -> u16 {
    let index = ONLINE_SYNC_CORRECTION_THRESHOLD_OPTIONS_MS
        .iter()
        .position(|entry| *entry == current)
        .unwrap_or_else(|| {
            ONLINE_SYNC_CORRECTION_THRESHOLD_OPTIONS_MS
                .iter()
                .position(|entry| *entry >= current)
                .unwrap_or(0)
        });
    ONLINE_SYNC_CORRECTION_THRESHOLD_OPTIONS_MS
        [(index + 1) % ONLINE_SYNC_CORRECTION_THRESHOLD_OPTIONS_MS.len()]
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
        | ActionPanelState::OnlineDelaySettings { selected }
        | ActionPanelState::ThemeSettings { selected }
        | ActionPanelState::LyricsImportTxt { selected, .. }
        | ActionPanelState::MetadataEditor { selected, .. }
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
    handle_action_panel_input_with_recent(core, audio, panel, &mut recent_root_actions, None, key);
}

fn handle_action_panel_input_with_recent(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    panel: &mut ActionPanelState,
    recent_root_actions: &mut Vec<RootActionId>,
    online_runtime: Option<&OnlineRuntime>,
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

    if let ActionPanelState::MetadataEditor { selected, state } = panel
        && state.selected_track_path.is_some()
    {
        let target = match *selected {
            0 => Some(&mut state.title_input),
            1 => Some(&mut state.artist_input),
            2 => Some(&mut state.album_input),
            _ => None,
        };
        if let Some(target) = target {
            match key {
                KeyCode::Char(ch) => {
                    target.push(ch);
                    core.dirty = true;
                    return;
                }
                KeyCode::Backspace if !target.is_empty() => {
                    target.pop();
                    core.dirty = true;
                    return;
                }
                _ => {}
            }
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
        ActionPanelState::PlaybackSettings { .. } => 8,
        ActionPanelState::OnlineDelaySettings { .. } => 6,
        ActionPanelState::ThemeSettings { .. } => 6,
        ActionPanelState::LyricsImportTxt { .. } => 3,
        ActionPanelState::MetadataEditor { state, .. } => state.options().len(),
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
                ActionPanelState::OnlineDelaySettings { .. } => {
                    ActionPanelState::PlaybackSettings { selected: 6 }
                }
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
                ActionPanelState::MetadataEditor { .. } => ActionPanelState::Root {
                    selected: root_selected_for_action(
                        RootActionId::MetadataEditor,
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
                    RootActionId::MetadataEditor => {
                        let Some(state) = metadata_editor_state_for_selection(core) else {
                            core.status = String::from(
                                "Select a track, folder, playlist, or [ALL] entry first",
                            );
                            core.dirty = true;
                            panel.close();
                            return;
                        };
                        *panel = ActionPanelState::MetadataEditor { selected: 0, state };
                        core.dirty = true;
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
                4 => {
                    core.stats_top_songs_count =
                        next_stats_top_songs_count(core.stats_top_songs_count);
                    core.status = format!("Stats top songs rows: {}", core.stats_top_songs_count);
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                5 => {
                    core.fallback_cover_template = core.fallback_cover_template.next();
                    core.status = format!(
                        "Missing cover fallback: {}",
                        cover_template_label(core.fallback_cover_template)
                    );
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                6 => {
                    *panel = ActionPanelState::OnlineDelaySettings { selected: 0 };
                    core.dirty = true;
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
            ActionPanelState::OnlineDelaySettings { selected } => match selected {
                0 => {
                    core.online_adjust_manual_delay(-10);
                    publish_online_delay_update(core, online_runtime);
                }
                1 => {
                    core.online_adjust_manual_delay(10);
                    publish_online_delay_update(core, online_runtime);
                }
                2 => {
                    core.online_toggle_auto_delay();
                    publish_online_delay_update(core, online_runtime);
                }
                3 => {
                    core.online_recalibrate_ping();
                    publish_online_delay_update(core, online_runtime);
                }
                4 => {
                    core.online_sync_correction_threshold_ms =
                        next_online_sync_correction_threshold_ms(
                            core.online_sync_correction_threshold_ms,
                        );
                    core.status = format!(
                        "Online sync correction threshold: {}ms",
                        core.online_sync_correction_threshold_ms
                    );
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                _ => {
                    *panel = ActionPanelState::PlaybackSettings { selected: 6 };
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
            ActionPanelState::MetadataEditor { selected, state } => match selected {
                0 if state.selected_track_path.is_none() => {
                    if state.confirm_all_songs_cover_copy {
                        let mut next_state = state.clone();
                        next_state.confirm_all_songs_cover_copy = false;
                        *panel = ActionPanelState::MetadataEditor {
                            selected: 0,
                            state: next_state,
                        };
                        core.status = String::from(
                            "Press Enter again to confirm copying cover art to all songs",
                        );
                        core.dirty = true;
                        return;
                    }

                    let Some(source_path) = now_playing_cover_source_path(core, &*audio) else {
                        core.status = String::from("No track is currently playing");
                        core.dirty = true;
                        return;
                    };

                    copy_now_playing_cover_to_paths(
                        core,
                        &source_path,
                        &state.copy_target_paths,
                        &state.copy_target_label,
                    );
                    panel.close();
                }
                1 if state.selected_track_path.is_none() => {
                    *panel = ActionPanelState::Root {
                        selected: root_selected_for_action(
                            RootActionId::MetadataEditor,
                            recent_root_actions,
                        ),
                        query: String::new(),
                    };
                    core.dirty = true;
                }
                3 => {
                    let Some(path) = state.selected_track_path.as_ref() else {
                        return;
                    };
                    match library::write_embedded_metadata(path, &state.metadata_edit()) {
                        Ok(()) => {
                            core.reload_track_metadata(path);
                            core.status = String::from("Metadata saved");
                            core.dirty = true;
                        }
                        Err(err) => {
                            core.status = format!("Metadata save failed: {err:#}");
                            core.dirty = true;
                            return;
                        }
                    }
                    panel.close();
                }
                4 => {
                    let Some(path) = state.selected_track_path.as_ref() else {
                        return;
                    };
                    match library::clear_embedded_metadata(path) {
                        Ok(()) => {
                            core.reload_track_metadata(path);
                            core.status = String::from("Metadata cleared");
                            core.dirty = true;
                        }
                        Err(err) => {
                            core.status = format!("Metadata clear failed: {err:#}");
                            core.dirty = true;
                            return;
                        }
                    }
                    panel.close();
                }
                5 => {
                    let Some(source_path) = now_playing_cover_source_path(core, &*audio) else {
                        core.status = String::from("No track is currently playing");
                        core.dirty = true;
                        return;
                    };

                    copy_now_playing_cover_to_paths(
                        core,
                        &source_path,
                        &state.copy_target_paths,
                        &state.copy_target_label,
                    );
                    panel.close();
                }
                6 => {
                    *panel = ActionPanelState::Root {
                        selected: root_selected_for_action(
                            RootActionId::MetadataEditor,
                            recent_root_actions,
                        ),
                        query: String::new(),
                    };
                    core.dirty = true;
                }
                _ => {}
            },
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
        volume: f32,
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
                volume: 1.0,
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
                volume: 1.0,
            }
        }
    }

    fn test_online_runtime() -> OnlineRuntime {
        OnlineRuntime {
            network: None,
            local_nickname: String::from("listener"),
            last_transport_seq: 0,
            join_prompt_active: false,
            join_code_input: String::new(),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: false,
            host_invite_code: String::new(),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
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
            self.volume
        }

        fn set_volume(&mut self, volume: f32) {
            self.volume = volume.clamp(0.0, MAX_VOLUME);
        }

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
                None,
                KeyCode::Char(ch),
            );
        }
        handle_action_panel_input_with_recent(
            &mut core,
            &mut audio,
            &mut panel,
            &mut recent_root_actions,
            None,
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
    fn metadata_editor_action_requires_selectable_entry() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.browser_entries = vec![crate::core::BrowserEntry {
            kind: crate::core::BrowserEntryKind::Back,
            path: PathBuf::new(),
            label: String::from("[..] Back"),
        }];
        core.selected_browser = 0;
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 15,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(
            core.status,
            "Select a track, folder, playlist, or [ALL] entry first"
        );
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn metadata_editor_action_opens_for_selected_track() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.browser_entries = vec![crate::core::BrowserEntry {
            kind: crate::core::BrowserEntryKind::Track,
            path: PathBuf::from("song.mp3"),
            label: String::from("song"),
        }];
        core.selected_browser = 0;
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 15,
            query: String::new(),
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        match panel {
            ActionPanelState::MetadataEditor {
                selected: 0,
                ref state,
            } => {
                let options = state.options();
                assert_eq!(options.len(), 7);
                assert_eq!(options[5], "Copy now playing cover art to selected track");
            }
            _ => panic!("expected metadata editor"),
        }
    }

    #[test]
    fn metadata_editor_all_songs_copy_requires_confirmation() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::MetadataEditor {
            selected: 0,
            state: MetadataEditorState {
                selected_track_path: None,
                copy_target_label: String::from("all songs"),
                copy_target_paths: vec![PathBuf::from("song.mp3")],
                title_input: String::new(),
                artist_input: String::new(),
                album_input: String::new(),
                confirm_all_songs_cover_copy: true,
            },
        };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(
            core.status,
            "Press Enter again to confirm copying cover art to all songs"
        );
        assert!(matches!(
            panel,
            ActionPanelState::MetadataEditor {
                selected: 0,
                state: MetadataEditorState {
                    confirm_all_songs_cover_copy: false,
                    ..
                }
            }
        ));
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

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert_eq!(core.stats_top_songs_count, 12);

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert_eq!(core.fallback_cover_template, CoverArtTemplate::Aurora);
    }

    #[test]
    fn online_delay_settings_cycles_sync_correction_threshold() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::OnlineDelaySettings { selected: 4 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(core.online_sync_correction_threshold_ms, 400);
        assert_eq!(core.status, "Online sync correction threshold: 400ms");
    }

    #[test]
    fn playback_settings_cover_template_cycle_stays_music_note() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.fallback_cover_template = CoverArtTemplate::Aurora;
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::PlaybackSettings { selected: 5 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(core.fallback_cover_template, CoverArtTemplate::Aurora);
        assert_eq!(core.status, "Missing cover fallback: Music Note");
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
    fn stats_mouse_scroll_down_moves_scroll_and_focuses_search() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = crate::core::HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Range(0);
        core.stats_scroll = 0;

        handle_mouse(
            &mut core,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 2,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            ratatui::prelude::Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 20,
            },
        );

        assert_eq!(core.stats_scroll, 1);
        assert!(matches!(
            core.stats_focus,
            crate::core::StatsFilterFocus::Search
        ));
    }

    #[test]
    fn stats_mouse_scroll_up_moves_scroll_and_focuses_search() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = crate::core::HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Range(3);
        core.stats_scroll = 3;

        handle_mouse(
            &mut core,
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 2,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            ratatui::prelude::Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 20,
            },
        );

        assert_eq!(core.stats_scroll, 2);
        assert!(matches!(
            core.stats_focus,
            crate::core::StatsFilterFocus::Search
        ));
    }

    #[test]
    fn mouse_scroll_targets_action_panel_when_open() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::Root {
            selected: 0,
            query: String::new(),
        };
        let mut recent_root_actions = Vec::new();
        let online_runtime = test_online_runtime();

        handle_mouse_with_panel(
            &mut core,
            &mut audio,
            &mut panel,
            &mut recent_root_actions,
            &online_runtime,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 2,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            ratatui::prelude::Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 20,
            },
        );

        assert!(matches!(
            panel,
            ActionPanelState::Root {
                selected: 1,
                query: _
            }
        ));
    }

    #[test]
    fn stats_tab_does_not_consume_ctrl_c() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Search;

        assert!(!handle_stats_inline_input(
            &mut core,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ));
    }

    #[test]
    fn stats_tab_does_not_consume_etx_char_or_mutate_filters() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Artist;

        assert!(!handle_stats_inline_input(
            &mut core,
            KeyEvent::new(KeyCode::Char('\u{3}'), KeyModifiers::NONE)
        ));
        assert!(core.stats_artist_filter.is_empty());
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
        core.online_sync_correction_threshold_ms = 500;
        core.theme = Theme::Galaxy;
        core.stats_enabled = false;
        core.stats_top_songs_count = 15;
        core.fallback_cover_template = CoverArtTemplate::Aurora;
        let mut audio = TestAudioEngine::new();
        audio.set_volume(1.75);

        let state = persisted_state_with_audio(&core, &audio);
        assert!(state.loudness_normalization);
        assert_eq!(state.crossfade_seconds, 4);
        assert_eq!(state.scrub_seconds, 30);
        assert_eq!(state.online_sync_correction_threshold_ms, 500);
        assert_eq!(state.theme, Theme::Galaxy);
        assert!(!state.stats_enabled);
        assert_eq!(state.stats_top_songs_count, 15);
        assert_eq!(state.fallback_cover_template, CoverArtTemplate::Aurora);
        assert!((state.saved_volume - 1.75).abs() < f32::EPSILON);
    }

    #[test]
    fn apply_saved_volume_clamps_into_supported_range() {
        let mut audio = TestAudioEngine::new();

        apply_saved_volume(&mut audio, 3.25);
        assert!((audio.volume() - MAX_VOLUME).abs() < f32::EPSILON);

        apply_saved_volume(&mut audio, -0.5);
        assert!((audio.volume() - 0.0).abs() < f32::EPSILON);
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
    fn remote_sync_applies_effective_delay_to_seek_target() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.online.session = Some(crate::online::OnlineSession::join("ROOM22", "listener"));
        let local = core
            .online
            .session
            .as_mut()
            .and_then(|session| session.local_participant_mut())
            .expect("local participant");
        local.ping_ms = 80;
        local.manual_extra_delay_ms = 40;
        local.auto_ping_delay = true;

        let mut runtime = test_online_runtime();
        let mut audio = TestAudioEngine::new();
        let path = PathBuf::from("song.mp3");
        audio.current = Some(path.clone());
        audio.position = Some(Duration::from_millis(1_000));
        runtime.remote_logical_track = Some(path.clone());

        apply_remote_transport(
            &mut core,
            &mut audio,
            &mut runtime,
            &TransportCommand::SetPlaybackState {
                path,
                title: None,
                artist: None,
                album: None,
                provider_track_id: None,
                position_ms: 1_200,
                paused: false,
            },
        );

        assert_eq!(audio.position, Some(Duration::from_millis(1_320)));
        assert_eq!(
            core.online
                .session
                .as_ref()
                .map(|session| session.last_sync_drift_ms),
            Some(320)
        );
    }

    #[test]
    fn remote_sync_ignores_small_playing_drift_to_reduce_micro_skips() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.online.session = Some(crate::online::OnlineSession::join("ROOM22", "listener"));
        let mut runtime = test_online_runtime();
        let mut audio = TestAudioEngine::new();
        let path = PathBuf::from("song.mp3");
        audio.current = Some(path.clone());
        audio.position = Some(Duration::from_millis(1_000));
        runtime.remote_logical_track = Some(path.clone());

        apply_remote_transport(
            &mut core,
            &mut audio,
            &mut runtime,
            &TransportCommand::SetPlaybackState {
                path,
                title: None,
                artist: None,
                album: None,
                provider_track_id: None,
                position_ms: 1_250,
                paused: false,
            },
        );

        assert_eq!(audio.position, Some(Duration::from_millis(1_000)));
        assert_eq!(
            core.online
                .session
                .as_ref()
                .map(|session| session.last_sync_drift_ms),
            Some(250)
        );
    }

    #[test]
    fn remote_stop_transport_stops_playback() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut runtime = test_online_runtime();
        runtime.remote_logical_track = Some(PathBuf::from("song.mp3"));
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("song.mp3"));

        apply_remote_transport(
            &mut core,
            &mut audio,
            &mut runtime,
            &TransportCommand::StopPlayback,
        );

        assert!(audio.stopped);
        assert_eq!(runtime.remote_logical_track, None);
        assert_eq!(core.status, "Remote stopped playback");
    }

    #[test]
    fn streaming_stats_identity_uses_remote_metadata_over_temp_path() {
        let mut runtime = test_online_runtime();
        let mut audio = TestAudioEngine::new();
        let logical = PathBuf::from("host/library/song.flac");
        let temp = PathBuf::from("C:/tmp/tunetui_stream_1.flac");
        audio.current = Some(temp.clone());
        runtime.remote_logical_track = Some(logical.clone());
        runtime.streamed_track_cache.insert(logical.clone(), temp);
        runtime.remote_track_title = Some(String::from("Song"));
        runtime.remote_track_artist = Some(String::from("Artist"));
        runtime.remote_provider_track_id = Some(String::from("provider:host:42"));

        let hint = online_streaming_stats_identity(&runtime, &audio).expect("identity hint");
        assert_eq!(hint.logical_path, logical);
        assert_eq!(hint.title.as_deref(), Some("Song"));
        assert_eq!(hint.artist.as_deref(), Some("Artist"));
        assert_eq!(hint.provider_track_id.as_deref(), Some("provider:host:42"));
    }

    #[test]
    fn online_disconnect_status_detector_matches_disconnect_messages() {
        assert!(is_online_disconnect_status("Disconnected from online host"));
        assert!(is_online_disconnect_status("Host ended session"));
        assert!(is_online_disconnect_status(
            "Online socket read error: connection reset"
        ));
        assert!(!is_online_disconnect_status("Remote sync drift 120ms"));
    }

    #[test]
    fn listen_tracker_flushes_partial_session_while_playing() {
        let core = TuneCore::from_persisted(PersistedState::default());
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker::default();
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("a.mp3"));
        audio.duration = Some(Duration::from_secs(200));

        assert!(!tracker.tick(&core, &audio, &mut stats, None));
        let active = tracker.active.as_mut().expect("active session");
        active.playing_started_at = Instant::now().checked_sub(Duration::from_secs(12));

        assert!(tracker.tick(&core, &audio, &mut stats, None));
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
            provider_track_id: None,
            started_at_epoch_seconds: 10,
            listened_seconds: 30,
            completed: false,
            duration_seconds: Some(200),
            counted_play_override: Some(false),
            allow_short_listen: true,
        });

        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                playback_path: PathBuf::from("a.mp3"),
                track_path: PathBuf::from("a.mp3"),
                title: String::from("a"),
                artist: None,
                album: None,
                provider_track_id: None,
                started_at_epoch_seconds: 10,
                playing_started_at: None,
                listened: Duration::from_secs(35),
                persisted_listened_seconds: 30,
                play_count_recorded: false,
                pending_same_track_restart: false,
                last_position: Some(Duration::from_secs(200)),
                duration: Some(Duration::from_secs(200)),
            }),
        };

        assert!(tracker.finalize_active(&mut stats, false));
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

        assert!(!tracker.tick(&core, &audio, &mut stats, None));
        let active = tracker.active.as_mut().expect("active session");
        active.playing_started_at = Instant::now().checked_sub(Duration::from_secs(31));

        assert!(tracker.tick(&core, &audio, &mut stats, None));
        let first_counted = stats
            .events
            .iter()
            .filter(|event| event.counted_play)
            .count();
        assert_eq!(first_counted, 1);

        let active = tracker.active.as_mut().expect("active session");
        active.playing_started_at = Instant::now().checked_sub(Duration::from_secs(42));
        assert!(tracker.tick(&core, &audio, &mut stats, None));

        let total_counted = stats
            .events
            .iter()
            .filter(|event| event.counted_play)
            .count();
        assert_eq!(total_counted, 1);
    }

    #[test]
    fn looped_same_track_counts_new_play_per_natural_restart() {
        let core = TuneCore::from_persisted(PersistedState::default());
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker::default();
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("loop.mp3"));
        audio.duration = Some(Duration::from_secs(180));

        assert!(!tracker.tick(&core, &audio, &mut stats, None));
        let active = tracker.active.as_mut().expect("active session");
        active.playing_started_at = Instant::now().checked_sub(Duration::from_secs(45));

        audio.finished = true;
        assert!(tracker.tick(&core, &audio, &mut stats, None));
        assert!(tracker.active.is_none());

        audio.play(Path::new("loop.mp3")).expect("restart loop");
        audio.duration = Some(Duration::from_secs(180));
        assert!(!tracker.tick(&core, &audio, &mut stats, None));
        let active = tracker
            .active
            .as_mut()
            .expect("active session after restart");
        active.playing_started_at = Instant::now().checked_sub(Duration::from_secs(37));

        audio.finished = true;
        assert!(tracker.tick(&core, &audio, &mut stats, None));

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
        assert_eq!(snapshot.total_plays, 2);
    }

    #[test]
    fn natural_finish_marks_short_track_complete_without_position_sample() {
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                playback_path: PathBuf::from("short.mp3"),
                track_path: PathBuf::from("short.mp3"),
                title: String::from("short"),
                artist: None,
                album: None,
                provider_track_id: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(20),
                persisted_listened_seconds: 0,
                play_count_recorded: false,
                pending_same_track_restart: false,
                last_position: None,
                duration: Some(Duration::from_secs(20)),
            }),
        };

        assert!(tracker.finalize_active(&mut stats, true));
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

        assert_eq!(snapshot.total_plays, 1);
        assert_eq!(snapshot.total_listen_seconds, 20);
    }

    #[test]
    fn same_track_restart_near_boundary_counts_new_play_without_finished_flag() {
        let core = TuneCore::from_persisted(PersistedState::default());
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                playback_path: PathBuf::from("loop.mp3"),
                track_path: PathBuf::from("loop.mp3"),
                title: String::from("loop"),
                artist: None,
                album: None,
                provider_track_id: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(186),
                persisted_listened_seconds: 186,
                play_count_recorded: false,
                pending_same_track_restart: true,
                last_position: Some(Duration::from_secs(179)),
                duration: Some(Duration::from_secs(180)),
            }),
        };
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("loop.mp3"));
        audio.duration = Some(Duration::from_secs(180));
        audio.position = Some(Duration::from_secs(1));
        audio.finished = false;

        assert!(tracker.tick(&core, &audio, &mut stats, None));
        assert!(tracker.active.is_some());

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
        assert_eq!(snapshot.total_plays, 1);
    }

    #[test]
    fn same_track_seek_to_start_without_near_end_does_not_split_session() {
        let core = TuneCore::from_persisted(PersistedState::default());
        let mut stats = StatsStore::default();
        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                playback_path: PathBuf::from("loop.mp3"),
                track_path: PathBuf::from("loop.mp3"),
                title: String::from("loop"),
                artist: None,
                album: None,
                provider_track_id: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(42),
                persisted_listened_seconds: 42,
                play_count_recorded: true,
                pending_same_track_restart: false,
                last_position: Some(Duration::from_secs(40)),
                duration: Some(Duration::from_secs(180)),
            }),
        };
        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("loop.mp3"));
        audio.duration = Some(Duration::from_secs(180));
        audio.position = Some(Duration::from_secs(2));
        audio.finished = false;

        assert!(!tracker.tick(&core, &audio, &mut stats, None));
        assert!(stats.events.is_empty());
    }

    #[test]
    fn finalize_keeps_short_tail_after_partial_flush() {
        let mut stats = StatsStore::default();
        stats.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("song.mp3"),
            title: String::from("song"),
            artist: None,
            album: None,
            provider_track_id: None,
            started_at_epoch_seconds: 100,
            listened_seconds: 140,
            completed: false,
            duration_seconds: Some(153),
            counted_play_override: Some(true),
            allow_short_listen: true,
        });

        let mut tracker = ListenTracker {
            active: Some(ActiveListenSession {
                playback_path: PathBuf::from("song.mp3"),
                track_path: PathBuf::from("song.mp3"),
                title: String::from("song"),
                artist: None,
                album: None,
                provider_track_id: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(153),
                persisted_listened_seconds: 140,
                play_count_recorded: true,
                pending_same_track_restart: false,
                last_position: Some(Duration::from_secs(153)),
                duration: Some(Duration::from_secs(153)),
            }),
        };

        assert!(tracker.finalize_active(&mut stats, false));
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
                playback_path: PathBuf::from("song.mp3"),
                track_path: PathBuf::from("song.mp3"),
                title: String::from("song"),
                artist: None,
                album: None,
                provider_track_id: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(40),
                persisted_listened_seconds: 0,
                play_count_recorded: false,
                pending_same_track_restart: false,
                last_position: Some(Duration::from_secs(40)),
                duration: Some(Duration::from_secs(153)),
            }),
        };

        assert!(tracker.finalize_active(&mut stats, false));
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
                playback_path: PathBuf::from("song.mp3"),
                track_path: PathBuf::from("song.mp3"),
                title: String::from("song"),
                artist: None,
                album: None,
                provider_track_id: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(190),
                persisted_listened_seconds: 0,
                play_count_recorded: false,
                pending_same_track_restart: false,
                last_position: Some(Duration::from_secs(153)),
                duration: Some(Duration::from_secs(153)),
            }),
        };

        assert!(tracker.finalize_active(&mut stats, false));
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
                playback_path: PathBuf::from("skip.mp3"),
                track_path: PathBuf::from("skip.mp3"),
                title: String::from("skip"),
                artist: None,
                album: None,
                provider_track_id: None,
                started_at_epoch_seconds: 100,
                playing_started_at: None,
                listened: Duration::from_secs(2),
                persisted_listened_seconds: 0,
                play_count_recorded: false,
                pending_same_track_restart: false,
                last_position: Some(Duration::from_secs(2)),
                duration: Some(Duration::from_secs(153)),
            }),
        };

        assert!(!tracker.finalize_active(&mut stats, false));
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

        let mut runtime = test_online_runtime();
        let mut audio = TestAudioEngine::finished_with_current("a.mp3");
        maybe_auto_advance_track(&mut core, &mut audio, &mut runtime);

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

        let mut runtime = test_online_runtime();
        maybe_auto_advance_track(&mut core, &mut audio, &mut runtime);

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

        let mut runtime = test_online_runtime();
        let mut audio = TestAudioEngine::finished_with_current("a.mp3");
        maybe_auto_advance_track(&mut core, &mut audio, &mut runtime);

        assert!(audio.stopped);
        assert_eq!(core.status, "Reached end of queue");
    }

    #[test]
    fn online_auto_advance_skips_non_authority_peer() {
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
        core.online.session = Some(crate::online::OnlineSession::join("ROOM22", "listener"));
        if let Some(session) = core.online.session.as_mut() {
            session.participants.push(crate::online::Participant {
                nickname: String::from("host"),
                is_local: false,
                is_host: true,
                ping_ms: 0,
                manual_extra_delay_ms: 0,
                auto_ping_delay: true,
            });
            session.last_transport = Some(TransportEnvelope {
                seq: 1,
                origin_nickname: String::from("host"),
                command: TransportCommand::SetPaused { paused: false },
            });
        }

        let mut runtime = test_online_runtime();
        let mut audio = TestAudioEngine::finished_with_current("a.mp3");
        maybe_auto_advance_track(&mut core, &mut audio, &mut runtime);

        assert!(audio.played.is_empty());
        assert!(!audio.stopped);
        assert_eq!(core.current_queue_index, Some(0));
    }

    #[test]
    fn online_auto_advance_consumes_shared_queue_fifo() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.online.session = Some(crate::online::OnlineSession::host("host"));
        if let Some(session) = core.online.session.as_mut() {
            session.push_shared_track(
                Path::new("shared.mp3"),
                String::from("shared"),
                Some(String::from("listener")),
            );
        }

        let mut runtime = test_online_runtime();
        runtime.local_nickname = String::from("host");
        let mut audio = TestAudioEngine::finished_with_current("a.mp3");
        maybe_auto_advance_track(&mut core, &mut audio, &mut runtime);

        assert_eq!(audio.played, vec![PathBuf::from("shared.mp3")]);
        assert_eq!(
            runtime.online_playback_source,
            OnlinePlaybackSource::SharedQueue
        );
        assert_eq!(
            core.online
                .session
                .as_ref()
                .map(|session| session.shared_queue.len()),
            Some(0)
        );
    }

    #[test]
    fn online_auto_advance_stops_when_shared_queue_finishes() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.online.session = Some(crate::online::OnlineSession::host("host"));

        let mut runtime = test_online_runtime();
        runtime.local_nickname = String::from("host");
        runtime.online_playback_source = OnlinePlaybackSource::SharedQueue;
        let mut audio = TestAudioEngine::finished_with_current("shared.mp3");
        maybe_auto_advance_track(&mut core, &mut audio, &mut runtime);

        assert!(audio.stopped);
        assert_eq!(core.status, "Reached end of shared queue");
        assert_eq!(
            runtime.online_playback_source,
            OnlinePlaybackSource::LocalQueue
        );
    }

    #[test]
    fn online_auto_advance_falls_back_to_host_when_last_origin_missing() {
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
        core.online.session = Some(crate::online::OnlineSession::host("host"));
        if let Some(session) = core.online.session.as_mut() {
            session.last_transport = Some(TransportEnvelope {
                seq: 2,
                origin_nickname: String::from("gone"),
                command: TransportCommand::SetPaused { paused: false },
            });
        }

        let mut runtime = test_online_runtime();
        runtime.local_nickname = String::from("host");
        let mut audio = TestAudioEngine::finished_with_current("a.mp3");
        maybe_auto_advance_track(&mut core, &mut audio, &mut runtime);

        assert_eq!(audio.played, vec![PathBuf::from("b.mp3")]);
        assert_eq!(core.current_queue_index, Some(1));
    }

    #[test]
    fn online_shared_queue_starts_immediately_when_idle_for_authority() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.online.session = Some(crate::online::OnlineSession::host("host"));
        if let Some(session) = core.online.session.as_mut() {
            session.push_shared_track(
                Path::new("shared.mp3"),
                String::from("shared"),
                Some(String::from("listener")),
            );
        }

        let mut runtime = test_online_runtime();
        runtime.local_nickname = String::from("host");
        let mut audio = TestAudioEngine::new();

        maybe_start_online_shared_queue_if_idle(&mut core, &mut audio, &mut runtime);

        assert_eq!(audio.played, vec![PathBuf::from("shared.mp3")]);
        assert_eq!(
            runtime.online_playback_source,
            OnlinePlaybackSource::SharedQueue
        );
        assert_eq!(
            core.online
                .session
                .as_ref()
                .map(|session| session.shared_queue.len()),
            Some(0)
        );
    }

    #[test]
    fn online_shared_queue_does_not_start_immediately_when_idle_for_non_authority() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.online.session = Some(crate::online::OnlineSession::join("ROOM22", "listener"));
        if let Some(session) = core.online.session.as_mut() {
            session.participants.push(crate::online::Participant {
                nickname: String::from("host"),
                is_local: false,
                is_host: true,
                ping_ms: 0,
                manual_extra_delay_ms: 0,
                auto_ping_delay: true,
            });
            session.push_shared_track(
                Path::new("shared.mp3"),
                String::from("shared"),
                Some(String::from("listener")),
            );
        }

        let mut runtime = test_online_runtime();
        runtime.local_nickname = String::from("listener");
        let mut audio = TestAudioEngine::new();

        maybe_start_online_shared_queue_if_idle(&mut core, &mut audio, &mut runtime);

        assert!(audio.played.is_empty());
        assert_eq!(
            core.online
                .session
                .as_ref()
                .map(|session| session.shared_queue.len()),
            Some(1)
        );
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

    #[test]
    fn host_invite_modal_tab_toggles_and_ok_closes_dialog() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut runtime = OnlineRuntime {
            network: None,
            local_nickname: String::from("tester"),
            last_transport_seq: 0,
            join_prompt_active: false,
            join_code_input: String::new(),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: true,
            host_invite_code: String::from("T1ABCDE"),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
        };

        assert!(handle_host_invite_modal_input(
            &mut core,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut runtime,
        ));
        assert_eq!(runtime.host_invite_button, HostInviteModalButton::Ok);

        assert!(handle_host_invite_modal_input(
            &mut core,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut runtime,
        ));
        assert!(!runtime.host_invite_modal_active);
    }

    #[test]
    fn host_invite_modal_escape_closes_dialog() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut runtime = OnlineRuntime {
            network: None,
            local_nickname: String::from("tester"),
            last_transport_seq: 0,
            join_prompt_active: false,
            join_code_input: String::new(),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: true,
            host_invite_code: String::from("T1ABCDE"),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
        };

        assert!(handle_host_invite_modal_input(
            &mut core,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut runtime,
        ));
        assert!(!runtime.host_invite_modal_active);
    }

    #[test]
    fn online_tab_consumes_library_navigation_keys() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = HeaderSection::Online;
        let audio = NullAudioEngine::new();
        let mut runtime = OnlineRuntime {
            network: None,
            local_nickname: String::from("tester"),
            last_transport_seq: 0,
            join_prompt_active: false,
            join_code_input: String::new(),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: false,
            host_invite_code: String::new(),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
        };

        assert!(handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut runtime,
        ));
        assert!(handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut runtime,
        ));
    }

    #[test]
    fn online_tab_does_not_consume_ctrl_c() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = HeaderSection::Online;
        let audio = NullAudioEngine::new();
        let mut runtime = OnlineRuntime {
            network: None,
            local_nickname: String::from("tester"),
            last_transport_seq: 0,
            join_prompt_active: false,
            join_code_input: String::new(),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: false,
            host_invite_code: String::new(),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
        };

        assert!(!handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut runtime,
        ));
    }

    #[test]
    fn online_tab_consumes_ctrl_s() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = HeaderSection::Online;
        let audio = NullAudioEngine::new();
        let mut runtime = OnlineRuntime {
            network: None,
            local_nickname: String::from("tester"),
            last_transport_seq: 0,
            join_prompt_active: false,
            join_code_input: String::new(),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: false,
            host_invite_code: String::new(),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
        };

        assert!(handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &mut runtime,
        ));
    }

    #[test]
    fn online_tab_does_not_consume_volume_keys() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = HeaderSection::Online;
        let audio = NullAudioEngine::new();
        let mut runtime = OnlineRuntime {
            network: None,
            local_nickname: String::from("tester"),
            last_transport_seq: 0,
            join_prompt_active: false,
            join_code_input: String::new(),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: false,
            host_invite_code: String::new(),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
        };

        assert!(!handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Char('='), KeyModifiers::NONE),
            &mut runtime,
        ));
        assert!(!handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE),
            &mut runtime,
        ));
    }

    #[test]
    fn join_prompt_allows_typing_v_without_triggering_paste() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = HeaderSection::Online;
        let audio = NullAudioEngine::new();
        let mut runtime = OnlineRuntime {
            network: None,
            local_nickname: String::from("tester"),
            last_transport_seq: 0,
            join_prompt_active: true,
            join_code_input: String::from("AB"),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: false,
            host_invite_code: String::new(),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
        };

        assert!(handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            &mut runtime,
        ));
        assert_eq!(runtime.join_code_input, "ABV");
    }

    #[test]
    fn online_tab_uppercase_commands_work_with_caps_lock() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = HeaderSection::Online;
        let audio = NullAudioEngine::new();
        let mut runtime = OnlineRuntime {
            network: None,
            local_nickname: String::from("tester"),
            last_transport_seq: 0,
            join_prompt_active: false,
            join_code_input: String::new(),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: false,
            host_invite_code: String::new(),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
        };

        assert!(handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Char('H'), KeyModifiers::NONE),
            &mut runtime,
        ));
        assert!(runtime.password_prompt_active);

        runtime.password_prompt_active = false;
        core.online_host_room("tester");
        assert!(handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
            &mut runtime,
        ));
        assert!(core.online.session.is_none());
    }

    #[test]
    fn online_tab_does_not_apply_manual_delay_shortcuts_directly() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = HeaderSection::Online;
        core.online_host_room("tester");
        let audio = NullAudioEngine::new();
        let mut runtime = OnlineRuntime {
            network: None,
            local_nickname: String::from("tester"),
            last_transport_seq: 0,
            join_prompt_active: false,
            join_code_input: String::new(),
            join_prompt_button: JoinPromptButton::Join,
            password_prompt_active: false,
            password_prompt_mode: OnlinePasswordPromptMode::Host,
            password_input: String::new(),
            pending_join_invite_code: String::new(),
            room_code_revealed: false,
            host_invite_modal_active: false,
            host_invite_code: String::new(),
            host_invite_button: HostInviteModalButton::Copy,
            streamed_track_cache: HashMap::new(),
            pending_stream_path: None,
            remote_logical_track: None,
            remote_track_title: None,
            remote_track_artist: None,
            remote_track_album: None,
            remote_provider_track_id: None,
            last_remote_transport_origin: None,
            last_periodic_sync_at: Instant::now(),
            online_playback_source: OnlinePlaybackSource::LocalQueue,
        };

        assert!(handle_online_inline_input(
            &mut core,
            &audio,
            KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
            &mut runtime,
        ));
        let manual = core
            .online
            .session
            .as_ref()
            .and_then(|session| session.local_participant())
            .map(|local| local.manual_extra_delay_ms)
            .unwrap_or_default();
        assert_eq!(manual, 0);
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
