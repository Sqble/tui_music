use crate::config;
use crate::library;
use crate::lyrics::{self, LyricLine, LyricsDocument, LyricsSource};
use crate::model::{PersistedState, PlaybackMode, Playlist, Theme, Track};
use crate::stats::{StatsRange, StatsSort};
use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserEntryKind {
    Back,
    Folder,
    Playlist,
    AllSongs,
    Track,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderSection {
    Library,
    Lyrics,
    Stats,
    Online,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsFilterFocus {
    Range(u8),
    Sort(u8),
    Artist,
    Album,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsMode {
    View,
    Edit,
}

impl StatsFilterFocus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Range(_) => "Range",
            Self::Sort(_) => "Sort",
            Self::Artist => "Artist",
            Self::Album => "Album",
            Self::Search => "Search",
        }
    }
}

impl HeaderSection {
    fn next(self) -> Self {
        match self {
            Self::Library => Self::Lyrics,
            Self::Lyrics => Self::Stats,
            Self::Stats => Self::Online,
            Self::Online => Self::Library,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Lyrics => "Lyrics",
            Self::Stats => "Stats",
            Self::Online => "Online",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrowserEntry {
    pub kind: BrowserEntryKind,
    pub path: PathBuf,
    pub label: String,
}

#[derive(Debug)]
pub struct TuneCore {
    pub folders: Vec<PathBuf>,
    pub tracks: Vec<Track>,
    track_lookup: HashMap<String, usize>,
    pub playlists: HashMap<String, Playlist>,
    pub queue: Vec<usize>,
    pub selected_track: usize,
    pub current_queue_index: Option<usize>,
    pub playback_mode: PlaybackMode,
    pub loudness_normalization: bool,
    pub crossfade_seconds: u16,
    pub scrub_seconds: u16,
    pub theme: Theme,
    pub header_section: HeaderSection,
    pub browser_path: Option<PathBuf>,
    pub browser_playlist: Option<String>,
    pub browser_all_songs: bool,
    pub browser_entries: Vec<BrowserEntry>,
    pub selected_browser: usize,
    pub dirty: bool,
    pub status: String,
    pub stats_enabled: bool,
    pub stats_range: StatsRange,
    pub stats_sort: StatsSort,
    pub stats_artist_filter: String,
    pub stats_album_filter: String,
    pub stats_search: String,
    pub stats_focus: StatsFilterFocus,
    pub stats_scroll: u16,
    pub clear_stats_requested: bool,
    pub lyrics: Option<LyricsDocument>,
    pub lyrics_track_path: Option<PathBuf>,
    pub lyrics_mode: LyricsMode,
    pub lyrics_selected_line: usize,
    pub lyrics_missing_prompt: bool,
    pub lyrics_creation_declined: bool,
    duration_lookup: RefCell<HashMap<String, Option<u32>>>,
    shuffle_order: Vec<usize>,
    shuffle_cursor: usize,
    shuffle_rng: SmallRng,
}

impl TuneCore {
    pub fn from_persisted(state: PersistedState) -> Self {
        let tracks = library::scan_many(&state.folders);
        let track_lookup = build_track_lookup(&tracks);
        let mut core = Self {
            folders: state.folders,
            tracks,
            track_lookup,
            playlists: state.playlists,
            queue: Vec::new(),
            selected_track: 0,
            current_queue_index: None,
            playback_mode: state.playback_mode,
            loudness_normalization: state.loudness_normalization,
            crossfade_seconds: state.crossfade_seconds,
            scrub_seconds: normalize_scrub_seconds(state.scrub_seconds),
            theme: state.theme,
            header_section: HeaderSection::Library,
            browser_path: None,
            browser_playlist: None,
            browser_all_songs: false,
            browser_entries: Vec::new(),
            selected_browser: 0,
            dirty: true,
            status: String::from("Ready"),
            stats_enabled: state.stats_enabled,
            stats_range: StatsRange::Lifetime,
            stats_sort: StatsSort::ListenTime,
            stats_artist_filter: String::new(),
            stats_album_filter: String::new(),
            stats_search: String::new(),
            stats_focus: StatsFilterFocus::Range(0),
            stats_scroll: 0,
            clear_stats_requested: false,
            lyrics: None,
            lyrics_track_path: None,
            lyrics_mode: LyricsMode::View,
            lyrics_selected_line: 0,
            lyrics_missing_prompt: false,
            lyrics_creation_declined: false,
            duration_lookup: RefCell::new(HashMap::new()),
            shuffle_order: Vec::new(),
            shuffle_cursor: 0,
            shuffle_rng: SmallRng::from_os_rng(),
        };
        core.rebuild_main_queue();
        core.refresh_browser_entries();
        core
    }

    pub fn persisted_state(&self) -> PersistedState {
        PersistedState {
            folders: self.folders.clone(),
            playlists: self.playlists.clone(),
            playback_mode: self.playback_mode,
            loudness_normalization: self.loudness_normalization,
            crossfade_seconds: self.crossfade_seconds,
            scrub_seconds: self.scrub_seconds,
            theme: self.theme,
            selected_output_device: None,
            stats_enabled: self.stats_enabled,
        }
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        config::save_state(&self.persisted_state())?;
        self.set_status("State saved");
        Ok(())
    }

    pub fn add_folder(&mut self, input: &Path) {
        let sanitized = config::sanitize_user_folder_path(input);
        if sanitized.as_os_str().is_empty() {
            self.set_status("Invalid folder path");
            return;
        }

        let resolved = if input.exists() {
            input.to_path_buf()
        } else {
            config::resolve_existing_path(input)
        };

        if !resolved.exists() {
            self.set_status("Folder not found");
            return;
        }

        if !resolved.is_dir() {
            self.set_status("Path is not a folder");
            return;
        }

        let normalized = config::normalize_path(&resolved);
        if self.folders.iter().any(|f| f == &normalized) {
            self.set_status("Folder already added");
            return;
        }

        self.folders.push(normalized.clone());
        let mut found = library::scan_folder(&normalized);
        let count = found.len();
        self.tracks.append(&mut found);
        self.tracks.sort_by(|a, b| a.path.cmp(&b.path));
        self.tracks.dedup_by(|a, b| a.path == b.path);
        self.rebuild_main_queue();
        self.refresh_browser_entries();
        self.set_status(&format!("Added folder with {count} tracks"));
    }

    pub fn remove_folder(&mut self, input: &Path) {
        if input.as_os_str().is_empty() {
            self.set_status("Invalid folder path");
            return;
        }

        let sanitized = config::sanitize_user_folder_path(input);
        let display_clean = PathBuf::from(config::sanitize_display_text(&input.to_string_lossy()));
        let recovered = config::resolve_existing_path(input);

        let mut candidates = vec![config::normalize_path(input)];
        if !sanitized.as_os_str().is_empty() {
            candidates.push(config::normalize_path(&sanitized));
        }
        if !display_clean.as_os_str().is_empty() {
            candidates.push(config::normalize_path(&display_clean));
        }
        if recovered.exists() {
            candidates.push(config::normalize_path(&recovered));
        }

        let before = self.folders.len();
        self.folders.retain(|folder| {
            !candidates
                .iter()
                .any(|candidate| path_eq(folder, candidate))
        });
        if self.folders.len() == before {
            self.set_status("Folder not found");
            return;
        }

        self.browser_path = None;
        self.browser_all_songs = false;
        self.selected_browser = 0;
        self.tracks = library::scan_many(&self.folders);
        self.rebuild_main_queue();
        self.refresh_browser_entries();
        self.set_status("Folder removed");
    }

    pub fn rescan(&mut self) {
        self.tracks = library::scan_many(&self.folders);
        self.rebuild_main_queue();
        self.refresh_browser_entries();
        self.set_status("Library rescanned");
    }

    pub fn create_playlist(&mut self, name: &str) {
        if self.playlists.contains_key(name) {
            self.set_status("Playlist already exists");
            return;
        }

        self.playlists.insert(name.to_string(), Playlist::default());
        self.refresh_browser_entries();
        self.set_status("Playlist created");
    }

    pub fn remove_playlist(&mut self, name: &str) {
        if self.playlists.remove(name).is_none() {
            self.set_status("Playlist not found");
            return;
        }

        if self.browser_playlist.as_deref() == Some(name) {
            self.browser_playlist = None;
            self.selected_browser = 0;
        }

        self.refresh_browser_entries();

        self.set_status("Playlist removed");
    }

    pub fn add_selected_to_playlist(&mut self, name: &str) {
        let paths = self.selected_paths_for_playlist_action();
        if paths.is_empty() {
            self.set_status("Nothing selectable to add");
            return;
        }

        let playlist = self.playlists.entry(name.to_string()).or_default();
        let added = paths.len();
        playlist.tracks.extend(paths);
        self.set_status(&format!("Added {added} track(s) to playlist"));
    }

    pub fn add_track_to_playlist(&mut self, name: &str, path: &Path) {
        let playlist = self.playlists.entry(name.to_string()).or_default();
        playlist.tracks.push(path.to_path_buf());
        self.set_status("Added now playing track to playlist");
    }

    pub fn remove_selected_from_current_playlist(&mut self) {
        let Some(name) = self.browser_playlist.clone() else {
            self.set_status("Open a playlist to remove tracks");
            return;
        };

        let Some(entry) = self.browser_entries.get(self.selected_browser).cloned() else {
            self.set_status("No selection");
            return;
        };

        if entry.kind != BrowserEntryKind::Track {
            self.set_status("Select a playlist track to remove");
            return;
        }

        let Some(playlist) = self.playlists.get_mut(&name) else {
            self.set_status("Playlist not found");
            return;
        };

        if let Some(pos) = playlist
            .tracks
            .iter()
            .position(|path| path_eq(path, &entry.path))
        {
            playlist.tracks.remove(pos);
            self.refresh_browser_entries();
            self.set_status("Removed track from playlist");
        } else {
            self.set_status("Track not found in playlist");
        }
    }

    pub fn load_playlist_queue(&mut self, name: &str) {
        let Some(tracks) = self
            .playlists
            .get(name)
            .map(|playlist| playlist.tracks.clone())
        else {
            self.set_status("Playlist not found");
            return;
        };

        self.queue = self.queue_from_paths(&tracks);
        self.current_queue_index = None;
        self.rebuild_shuffle_order();
        self.set_status(&format!("Loaded playlist: {name}"));
        self.dirty = true;
    }

    pub fn reset_main_queue(&mut self) {
        self.rebuild_main_queue();
        self.current_queue_index = None;
        self.set_status("Loaded main library queue");
    }

    pub fn select_next(&mut self) {
        if self.browser_entries.is_empty() {
            return;
        }
        self.selected_browser = (self.selected_browser + 1).min(self.browser_entries.len() - 1);
        self.dirty = true;
    }

    pub fn select_prev(&mut self) {
        self.selected_browser = self.selected_browser.saturating_sub(1);
        self.dirty = true;
    }

    pub fn activate_selected(&mut self) -> Option<PathBuf> {
        let Some(entry) = self.browser_entries.get(self.selected_browser).cloned() else {
            self.set_status("Nothing selected");
            return None;
        };

        match entry.kind {
            BrowserEntryKind::Back => {
                self.navigate_back();
                None
            }
            BrowserEntryKind::Folder => {
                self.browser_playlist = None;
                self.browser_all_songs = false;
                self.browser_path = Some(entry.path);
                self.selected_browser = 0;
                self.refresh_browser_entries();
                self.set_status("Opened folder");
                None
            }
            BrowserEntryKind::Playlist => {
                self.browser_path = None;
                self.browser_all_songs = false;
                self.browser_playlist = Some(entry.path.to_string_lossy().to_string());
                self.selected_browser = 0;
                self.refresh_browser_entries();
                self.set_status("Opened playlist");
                None
            }
            BrowserEntryKind::AllSongs => {
                self.browser_path = None;
                self.browser_playlist = None;
                self.browser_all_songs = true;
                self.selected_browser = 0;
                self.refresh_browser_entries();
                self.set_status("Opened all songs");
                None
            }
            BrowserEntryKind::Track => {
                if let Some(name) = &self.browser_playlist {
                    if let Some(tracks) = self
                        .playlists
                        .get(name)
                        .map(|playlist| playlist.tracks.clone())
                    {
                        self.queue = self.queue_from_paths(&tracks);
                    } else {
                        self.queue.clear();
                    }
                } else if self.browser_all_songs {
                    self.queue = self.metadata_sorted_library_queue();
                } else if self.browser_path.is_some() {
                    let tracks = self.browser_track_paths();
                    self.queue = self.queue_from_paths(&tracks);
                } else {
                    self.queue = self.metadata_sorted_library_queue();
                }
                self.rebuild_shuffle_order();
                self.current_queue_index = if self.browser_playlist.is_some()
                    || self.browser_all_songs
                    || self.browser_path.is_some()
                {
                    self.selected_track_position_in_browser()
                } else {
                    self.queue
                        .iter()
                        .position(|track_idx| path_eq(&self.tracks[*track_idx].path, &entry.path))
                };
                self.set_status("Playing selected track");
                Some(entry.path)
            }
        }
    }

    pub fn is_browser_entry_playing(&self, browser_index: usize) -> bool {
        let Some(current_queue_index) = self.current_queue_index else {
            return false;
        };
        let Some(current_track_index) = self.queue.get(current_queue_index).copied() else {
            return false;
        };
        let Some(entry) = self.browser_entries.get(browser_index) else {
            return false;
        };
        if entry.kind != BrowserEntryKind::Track {
            return false;
        }

        let Some(entry_track_index) = self.track_index(&entry.path) else {
            return false;
        };
        if entry_track_index != current_track_index {
            return false;
        }

        let queue_occurrence = self.queue[..=current_queue_index]
            .iter()
            .filter(|track_idx| **track_idx == current_track_index)
            .count();

        let entry_occurrence = self.browser_entries[..=browser_index]
            .iter()
            .filter(|browser_entry| {
                browser_entry.kind == BrowserEntryKind::Track
                    && self
                        .track_index(&browser_entry.path)
                        .map(|idx| idx == current_track_index)
                        .unwrap_or(false)
            })
            .count();

        queue_occurrence == entry_occurrence
    }

    pub fn navigate_back(&mut self) {
        if self.browser_playlist.take().is_some() {
            self.selected_browser = 0;
            self.refresh_browser_entries();
            self.set_status("Went back");
            return;
        }

        if self.browser_all_songs {
            self.browser_all_songs = false;
            self.selected_browser = 0;
            self.refresh_browser_entries();
            self.set_status("Went back");
            return;
        }

        match &self.browser_path {
            Some(current) => {
                if let Some(root) = self
                    .folders
                    .iter()
                    .filter(|root| path_is_within(current, root))
                    .max_by_key(|root| root.components().count())
                {
                    if path_eq(current, root) {
                        self.browser_path = None;
                    } else {
                        let parent = current.parent().map(PathBuf::from);
                        self.browser_path = parent.filter(|path| path_is_within(path, root));
                    }
                } else {
                    self.browser_path = None;
                }
            }
            None => return,
        }
        self.selected_browser = 0;
        self.refresh_browser_entries();
        self.set_status("Went back");
    }

    pub fn cycle_mode(&mut self) {
        self.playback_mode = self.playback_mode.next();
        self.set_status("Playback mode updated");
    }

    pub fn cycle_header_section(&mut self) {
        self.header_section = self.header_section.next();
        self.set_status(&format!("Section: {}", self.header_section.label()));
    }

    pub fn cycle_stats_range(&mut self) {
        self.stats_range = self.stats_range.next();
        self.set_status(&format!("Stats range: {}", self.stats_range.label()));
    }

    pub fn toggle_stats_sort(&mut self) {
        self.stats_sort = self.stats_sort.toggle();
        self.set_status(&format!("Stats sort: {}", self.stats_sort.label()));
    }

    pub fn clear_stats_filters(&mut self) {
        self.stats_artist_filter.clear();
        self.stats_album_filter.clear();
        self.stats_search.clear();
        self.set_status("Stats filters cleared");
    }

    pub fn sync_lyrics_for_track(&mut self, track: Option<&Path>) {
        let Some(path) = track else {
            self.lyrics = None;
            self.lyrics_track_path = None;
            self.lyrics_mode = LyricsMode::View;
            self.lyrics_selected_line = 0;
            self.lyrics_missing_prompt = false;
            self.lyrics_creation_declined = false;
            return;
        };

        if self
            .lyrics_track_path
            .as_ref()
            .is_some_and(|current| path_eq(current, path))
        {
            return;
        }

        self.lyrics_track_path = Some(path.to_path_buf());
        self.lyrics_mode = LyricsMode::View;
        self.lyrics_selected_line = 0;
        self.lyrics_creation_declined = false;
        match lyrics::load_for_track(path) {
            Ok(Some(doc)) => {
                self.lyrics = Some(doc);
                self.lyrics_missing_prompt = false;
            }
            Ok(None) => {
                self.lyrics = None;
                self.lyrics_missing_prompt = true;
            }
            Err(err) => {
                self.lyrics = None;
                self.lyrics_missing_prompt = false;
                self.set_status(&format!("Lyrics load failed: {err}"));
            }
        }
    }

    pub fn decline_lyrics_creation(&mut self) {
        self.lyrics_missing_prompt = false;
        self.lyrics_creation_declined = true;
        self.set_status("Lyrics creation skipped");
    }

    pub fn create_empty_lyrics_sidecar(&mut self) {
        let Some(path) = self.lyrics_track_path.clone() else {
            self.set_status("No active track for lyrics");
            return;
        };
        let doc = LyricsDocument {
            lines: vec![LyricLine {
                timestamp_ms: None,
                text: String::new(),
            }],
            source: LyricsSource::Created,
            precision: lyrics::LyricsTimingPrecision::None,
        };

        match lyrics::write_sidecar(&path, &doc) {
            Ok(saved) => {
                self.lyrics = Some(doc);
                self.lyrics_mode = LyricsMode::Edit;
                self.lyrics_selected_line = 0;
                self.lyrics_missing_prompt = false;
                self.lyrics_creation_declined = false;
                self.set_status(&format!("Created {}", saved.display()));
            }
            Err(err) => self.set_status(&format!("Lyrics create failed: {err}")),
        }
    }

    pub fn toggle_lyrics_mode(&mut self) {
        self.lyrics_mode = match self.lyrics_mode {
            LyricsMode::View => LyricsMode::Edit,
            LyricsMode::Edit => {
                self.save_lyrics_sidecar();
                LyricsMode::View
            }
        };
        self.dirty = true;
        self.status = format!("Lyrics mode: {:?}", self.lyrics_mode);
    }

    pub fn save_lyrics_sidecar(&mut self) {
        let Some(path) = self.lyrics_track_path.clone() else {
            self.set_status("No active track for lyrics");
            return;
        };
        let Some(doc) = self.lyrics.as_ref() else {
            self.set_status("No lyrics loaded");
            return;
        };
        match lyrics::write_sidecar(&path, doc) {
            Ok(saved) => self.set_status(&format!("Saved {}", saved.display())),
            Err(err) => self.set_status(&format!("Lyrics save failed: {err}")),
        }
    }

    pub fn import_txt_to_lyrics(&mut self, txt_path: &Path, interval_seconds: u32) {
        match lyrics::read_txt_for_import(txt_path) {
            Ok(lines) if lines.is_empty() => self.set_status("TXT import found no non-empty lines"),
            Ok(lines) => {
                self.lyrics = Some(lyrics::build_seeded_from_lines(lines, interval_seconds));
                self.lyrics_mode = LyricsMode::Edit;
                self.lyrics_selected_line = 0;
                self.lyrics_missing_prompt = false;
                self.lyrics_creation_declined = false;
                self.save_lyrics_sidecar();
                self.set_status("Imported TXT into seeded LRC");
            }
            Err(err) => self.set_status(&format!("TXT import failed: {err}")),
        }
    }

    pub fn active_lyric_line_for_position(&self, position: Option<Duration>) -> Option<usize> {
        let position_ms = position.map(|pos| pos.as_millis().min(u128::from(u32::MAX)) as u32)?;
        let doc = self.lyrics.as_ref()?;

        let mut current = None;
        for (idx, line) in doc.lines.iter().enumerate() {
            let Some(ts) = line.timestamp_ms else {
                continue;
            };
            if ts <= position_ms {
                current = Some(idx);
            } else {
                break;
            }
        }
        current
    }

    pub fn sync_lyrics_highlight_to_position(&mut self, position: Option<Duration>) {
        let Some(active_idx) = self.active_lyric_line_for_position(position) else {
            return;
        };
        if self.lyrics_selected_line != active_idx {
            self.lyrics_selected_line = active_idx;
            self.dirty = true;
        }
    }

    pub fn lyrics_move_selection(&mut self, down: bool) {
        let Some(doc) = self.lyrics.as_ref() else {
            return;
        };
        if doc.lines.is_empty() {
            self.lyrics_selected_line = 0;
            return;
        }
        if down {
            self.lyrics_selected_line = (self.lyrics_selected_line + 1).min(doc.lines.len() - 1);
        } else {
            self.lyrics_selected_line = self.lyrics_selected_line.saturating_sub(1);
        }
        self.dirty = true;
    }

    pub fn lyrics_insert_char(&mut self, ch: char) {
        let Some(doc) = self.lyrics.as_mut() else {
            return;
        };
        if doc.lines.is_empty() {
            doc.lines.push(LyricLine {
                timestamp_ms: None,
                text: String::new(),
            });
            self.lyrics_selected_line = 0;
        }
        if let Some(line) = doc.lines.get_mut(self.lyrics_selected_line) {
            line.text.push(ch);
            self.dirty = true;
        }
    }

    pub fn lyrics_backspace(&mut self) {
        let Some(doc) = self.lyrics.as_mut() else {
            return;
        };
        let Some(line) = doc.lines.get_mut(self.lyrics_selected_line) else {
            return;
        };
        if !line.text.is_empty() {
            line.text.pop();
            self.dirty = true;
        }
    }

    pub fn lyrics_insert_line_after(&mut self) {
        let Some(doc) = self.lyrics.as_mut() else {
            return;
        };
        let insert_at = self
            .lyrics_selected_line
            .saturating_add(1)
            .min(doc.lines.len());
        let timestamp = doc
            .lines
            .get(self.lyrics_selected_line)
            .and_then(|line| line.timestamp_ms);
        doc.lines.insert(
            insert_at,
            LyricLine {
                timestamp_ms: timestamp,
                text: String::new(),
            },
        );
        self.lyrics_selected_line = insert_at;
        self.dirty = true;
    }

    pub fn lyrics_delete_selected_line(&mut self) {
        let Some(doc) = self.lyrics.as_mut() else {
            return;
        };
        if doc.lines.is_empty() {
            return;
        }
        if self.lyrics_selected_line < doc.lines.len() {
            doc.lines.remove(self.lyrics_selected_line);
        }
        if doc.lines.is_empty() {
            self.lyrics_selected_line = 0;
        } else {
            self.lyrics_selected_line = self.lyrics_selected_line.min(doc.lines.len() - 1);
        }
        self.dirty = true;
    }

    pub fn lyrics_stamp_selected_line(&mut self, position: Option<Duration>) {
        let Some(position) = position else {
            self.set_status("Cannot stamp timestamp without playback position");
            return;
        };
        let Some(doc) = self.lyrics.as_mut() else {
            return;
        };
        let Some(line) = doc.lines.get_mut(self.lyrics_selected_line) else {
            return;
        };
        line.timestamp_ms = Some(position.as_millis().min(u128::from(u32::MAX)) as u32);
        doc.lines
            .sort_by_key(|entry| entry.timestamp_ms.unwrap_or(u32::MAX));
        self.lyrics_selected_line = self
            .active_lyric_line_for_position(Some(position))
            .unwrap_or(self.lyrics_selected_line);
        self.dirty = true;
    }

    pub fn current_path(&self) -> Option<&Path> {
        let queue_index = self.current_queue_index?;
        let track_index = *self.queue.get(queue_index)?;
        self.tracks
            .get(track_index)
            .map(|track| track.path.as_path())
    }

    pub fn queue_position_for_path(&self, path: &Path) -> Option<usize> {
        self.queue.iter().position(|idx| {
            self.tracks
                .get(*idx)
                .map(|track| path_eq(&track.path, path))
                .unwrap_or(false)
        })
    }

    pub fn title_for_path(&self, path: &Path) -> Option<String> {
        let idx = self.track_index(path)?;
        self.tracks.get(idx).map(|track| track.title.clone())
    }

    pub fn artist_for_path(&self, path: &Path) -> Option<&str> {
        let idx = self.track_index(path)?;
        self.tracks
            .get(idx)
            .and_then(|track| track.artist.as_deref())
    }

    pub fn album_for_path(&self, path: &Path) -> Option<&str> {
        let idx = self.track_index(path)?;
        self.tracks
            .get(idx)
            .and_then(|track| track.album.as_deref())
    }

    pub fn duration_seconds_for_path(&self, path: &Path) -> Option<u32> {
        let key = normalized_path_key(path);
        if let Some(cached) = self.duration_lookup.borrow().get(&key).copied() {
            return cached;
        }

        let idx = self.track_index(path)?;
        let duration = self
            .tracks
            .get(idx)
            .and_then(|track| library::duration_seconds(&track.path));
        self.duration_lookup.borrow_mut().insert(key, duration);
        duration
    }

    pub fn next_track_path(&mut self) -> Option<PathBuf> {
        if self.queue.is_empty() {
            self.set_status("Queue is empty");
            return None;
        }

        let idx = match self.current_queue_index {
            Some(current) => self.next_index(current),
            None => {
                if self.playback_mode == PlaybackMode::Shuffle {
                    if self.shuffle_order.len() != self.queue.len() {
                        self.rebuild_shuffle_order();
                    }
                    self.shuffle_order.first().copied()
                } else {
                    Some(0)
                }
            }
        }?;

        self.current_queue_index = Some(idx);
        self.dirty = true;
        self.queue
            .get(idx)
            .and_then(|track_idx| self.tracks.get(*track_idx))
            .map(|track| track.path.clone())
    }

    pub fn prev_track_path(&mut self) -> Option<PathBuf> {
        if self.queue.is_empty() {
            self.set_status("Queue is empty");
            return None;
        }

        let idx = match self.current_queue_index {
            Some(current) => self.prev_index(current),
            None => {
                if self.playback_mode == PlaybackMode::Shuffle {
                    if self.shuffle_order.len() != self.queue.len() {
                        self.rebuild_shuffle_order();
                    }
                    self.shuffle_order.last().copied()
                } else {
                    self.queue.len().checked_sub(1)
                }
            }
        }?;

        self.current_queue_index = Some(idx);
        self.dirty = true;
        self.queue
            .get(idx)
            .and_then(|track_idx| self.tracks.get(*track_idx))
            .map(|track| track.path.clone())
    }

    fn next_index(&mut self, current: usize) -> Option<usize> {
        match self.playback_mode {
            PlaybackMode::LoopOne => Some(current),
            PlaybackMode::Normal => {
                let next = current + 1;
                (next < self.queue.len()).then_some(next)
            }
            PlaybackMode::Loop => {
                if self.queue.is_empty() {
                    None
                } else {
                    Some((current + 1) % self.queue.len())
                }
            }
            PlaybackMode::Shuffle => {
                if self.shuffle_order.len() != self.queue.len() {
                    self.rebuild_shuffle_order();
                }

                if self.shuffle_order.is_empty() {
                    return None;
                }

                if let Some(pos) = self.shuffle_order.iter().position(|idx| *idx == current) {
                    self.shuffle_cursor = pos;
                }

                self.shuffle_cursor = (self.shuffle_cursor + 1) % self.shuffle_order.len();
                self.shuffle_order.get(self.shuffle_cursor).copied()
            }
        }
    }

    fn prev_index(&mut self, current: usize) -> Option<usize> {
        match self.playback_mode {
            PlaybackMode::LoopOne => Some(current),
            PlaybackMode::Normal => current.checked_sub(1),
            PlaybackMode::Loop => {
                if self.queue.is_empty() {
                    None
                } else if current == 0 {
                    Some(self.queue.len() - 1)
                } else {
                    Some(current - 1)
                }
            }
            PlaybackMode::Shuffle => {
                if self.shuffle_order.len() != self.queue.len() {
                    self.rebuild_shuffle_order();
                }

                if self.shuffle_order.is_empty() {
                    return None;
                }

                if let Some(pos) = self.shuffle_order.iter().position(|idx| *idx == current) {
                    self.shuffle_cursor = pos;
                }

                self.shuffle_cursor = if self.shuffle_cursor == 0 {
                    self.shuffle_order.len() - 1
                } else {
                    self.shuffle_cursor - 1
                };
                self.shuffle_order.get(self.shuffle_cursor).copied()
            }
        }
    }

    fn rebuild_main_queue(&mut self) {
        self.track_lookup = build_track_lookup(&self.tracks);
        self.queue = self.metadata_sorted_library_queue();
        self.rebuild_shuffle_order();
        self.dirty = true;
    }

    fn metadata_sorted_library_queue(&self) -> Vec<usize> {
        let mut queue: Vec<usize> = (0..self.tracks.len()).collect();
        queue.sort_by_cached_key(|idx| self.tracks[*idx].title.to_ascii_lowercase());
        queue
    }

    fn selected_paths_for_playlist_action(&self) -> Vec<PathBuf> {
        let Some(entry) = self.browser_entries.get(self.selected_browser) else {
            return self
                .tracks
                .get(self.selected_track)
                .map(|track| vec![track.path.clone()])
                .unwrap_or_default();
        };

        match entry.kind {
            BrowserEntryKind::Track => vec![entry.path.clone()],
            BrowserEntryKind::Folder => self
                .tracks
                .iter()
                .filter(|track| path_is_within(&track.path, &entry.path))
                .map(|track| track.path.clone())
                .collect(),
            BrowserEntryKind::Playlist => self
                .playlists
                .get(entry.path.to_string_lossy().as_ref())
                .map(|playlist| playlist.tracks.clone())
                .unwrap_or_default(),
            BrowserEntryKind::AllSongs => self
                .metadata_sorted_library_queue()
                .into_iter()
                .filter_map(|idx| self.tracks.get(idx).map(|track| track.path.clone()))
                .collect(),
            BrowserEntryKind::Back => Vec::new(),
        }
    }

    fn selected_track_position_in_browser(&self) -> Option<usize> {
        let entry = self.browser_entries.get(self.selected_browser)?;
        if entry.kind != BrowserEntryKind::Track {
            return None;
        }

        Some(
            self.browser_entries[..=self.selected_browser]
                .iter()
                .filter(|browser_entry| browser_entry.kind == BrowserEntryKind::Track)
                .count()
                .saturating_sub(1),
        )
    }

    fn browser_track_paths(&self) -> Vec<PathBuf> {
        self.browser_entries
            .iter()
            .filter(|entry| entry.kind == BrowserEntryKind::Track)
            .map(|entry| entry.path.clone())
            .collect()
    }

    fn refresh_browser_entries(&mut self) {
        let mut entries = Vec::new();

        if let Some(name) = &self.browser_playlist {
            entries.push(BrowserEntry {
                kind: BrowserEntryKind::Back,
                path: PathBuf::new(),
                label: String::from("[..] Back"),
            });

            if let Some(playlist) = self.playlists.get(name) {
                for track in &playlist.tracks {
                    let cleaned = config::strip_windows_verbatim_prefix(track);
                    entries.push(BrowserEntry {
                        kind: BrowserEntryKind::Track,
                        label: self.track_label_from_path(&cleaned),
                        path: cleaned,
                    });
                }
            }
        } else if self.browser_all_songs {
            entries.push(BrowserEntry {
                kind: BrowserEntryKind::Back,
                path: PathBuf::new(),
                label: String::from("[..] Back"),
            });

            let queue = self.metadata_sorted_library_queue();
            for idx in queue {
                if let Some(track) = self.tracks.get(idx) {
                    entries.push(BrowserEntry {
                        kind: BrowserEntryKind::Track,
                        label: config::sanitize_display_text(&track.title),
                        path: track.path.clone(),
                    });
                }
            }
        } else if let Some(current) = &self.browser_path {
            let cleaned_current = config::strip_windows_verbatim_prefix(current);
            entries.push(BrowserEntry {
                kind: BrowserEntryKind::Back,
                path: cleaned_current.clone(),
                label: String::from("[..] Back"),
            });

            if let Ok(read_dir) = fs::read_dir(current) {
                let mut folders = Vec::new();
                let mut files = Vec::new();

                for entry in read_dir.filter_map(Result::ok) {
                    let path = config::strip_windows_verbatim_prefix(&entry.path());
                    let file_name =
                        config::sanitize_display_text(&entry.file_name().to_string_lossy());

                    if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                        folders.push(BrowserEntry {
                            kind: BrowserEntryKind::Folder,
                            path,
                            label: format!("[DIR] {file_name}"),
                        });
                    } else if is_audio_file(&path) {
                        files.push(BrowserEntry {
                            kind: BrowserEntryKind::Track,
                            label: self.track_label_from_path(&path),
                            path,
                        });
                    }
                }

                folders.sort_by_cached_key(|entry| entry.label.to_ascii_lowercase());
                files.sort_by_cached_key(|entry| entry.label.to_ascii_lowercase());
                entries.extend(folders);
                entries.extend(files);
            }
        } else {
            for folder in &self.folders {
                let cleaned = config::strip_windows_verbatim_prefix(folder);
                let label = cleaned
                    .file_name()
                    .map(|name| config::sanitize_display_text(&name.to_string_lossy()))
                    .unwrap_or_else(|| cleaned.display().to_string());
                entries.push(BrowserEntry {
                    kind: BrowserEntryKind::Folder,
                    path: cleaned,
                    label: format!("[DIR] {label}"),
                });
            }

            entries.push(BrowserEntry {
                kind: BrowserEntryKind::AllSongs,
                path: PathBuf::new(),
                label: String::from("[ALL] All Songs"),
            });

            for name in self.playlists.keys() {
                entries.push(BrowserEntry {
                    kind: BrowserEntryKind::Playlist,
                    path: PathBuf::from(name),
                    label: format!("[PL] {}", config::sanitize_display_text(name)),
                });
            }

            entries.sort_by_cached_key(|entry| entry.label.to_ascii_lowercase());
        }

        self.browser_entries = entries;
        if self.browser_entries.is_empty() {
            self.selected_browser = 0;
        } else {
            self.selected_browser = self.selected_browser.min(self.browser_entries.len() - 1);
        }
        self.dirty = true;
    }

    fn track_label_from_path(&self, path: &Path) -> String {
        self.track_index(path)
            .and_then(|idx| self.tracks.get(idx))
            .map(|track| config::sanitize_display_text(&track.title))
            .unwrap_or_else(|| {
                path.file_name()
                    .map(|file| config::sanitize_display_text(&file.to_string_lossy()))
                    .unwrap_or_else(|| path.display().to_string())
            })
    }

    fn queue_from_paths(&mut self, paths: &[PathBuf]) -> Vec<usize> {
        let mut queue = Vec::with_capacity(paths.len());
        for path in paths {
            queue.push(self.ensure_track_for_path(path));
        }
        queue
    }

    fn track_index(&self, path: &Path) -> Option<usize> {
        let key = normalized_path_key(path);
        self.track_lookup.get(&key).copied().or_else(|| {
            self.tracks
                .iter()
                .position(|track| path_eq(&track.path, path))
        })
    }

    fn ensure_track_for_path(&mut self, path: &Path) -> usize {
        if let Some(idx) = self.track_index(path) {
            return idx;
        }

        let cleaned = config::strip_windows_verbatim_prefix(path);
        let title = cleaned
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or("unknown")
            .to_string();
        let idx = self.tracks.len();
        self.tracks.push(Track {
            path: cleaned,
            title,
            artist: None,
            album: None,
        });
        self.track_lookup = build_track_lookup(&self.tracks);
        idx
    }

    fn rebuild_shuffle_order(&mut self) {
        self.shuffle_order = (0..self.queue.len()).collect();
        self.shuffle_order.shuffle(&mut self.shuffle_rng);
        self.shuffle_cursor = 0;
    }

    fn set_status(&mut self, message: &str) {
        self.status = message.to_string();
        self.dirty = true;
    }
}

fn is_audio_file(path: &Path) -> bool {
    const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "ogg", "m4a", "aac", "opus"];
    let ext = path.extension().and_then(OsStr::to_str).unwrap_or_default();
    AUDIO_EXTENSIONS
        .iter()
        .any(|supported| ext.eq_ignore_ascii_case(supported))
}

fn path_eq(a: &Path, b: &Path) -> bool {
    let a = config::normalize_path(a);
    let b = config::normalize_path(b);
    let mut left = a.components();
    let mut right = b.components();

    loop {
        match (left.next(), right.next()) {
            (Some(l), Some(r)) if path_component_eq(l.as_os_str(), r.as_os_str()) => {}
            (Some(_), Some(_)) => return false,
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = config::normalize_path(path);
    let root = config::normalize_path(root);

    let mut path_components = path.components();
    for root_component in root.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };

        if !path_component_eq(path_component.as_os_str(), root_component.as_os_str()) {
            return false;
        }
    }

    true
}

fn path_component_eq(left: &OsStr, right: &OsStr) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(right.to_string_lossy().as_ref())
    } else {
        left == right
    }
}

fn normalized_path_key(path: &Path) -> String {
    let normalized = config::normalize_path(path);
    let value = normalized.to_string_lossy();
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value.to_string()
    }
}

fn build_track_lookup(tracks: &[Track]) -> HashMap<String, usize> {
    let mut map = HashMap::with_capacity(tracks.len());
    for (idx, track) in tracks.iter().enumerate() {
        map.insert(normalized_path_key(&track.path), idx);
    }
    map
}

fn normalize_scrub_seconds(seconds: u16) -> u16 {
    match seconds {
        5 | 10 | 15 | 30 | 60 => seconds,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Playlist;
    use proptest::prop_assert;

    #[test]
    fn loop_mode_wraps() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from("a"),
                title: String::from("a"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("b"),
                title: String::from("b"),
                artist: None,
                album: None,
            },
        ];
        core.track_lookup = build_track_lookup(&core.tracks);
        core.queue = vec![0, 1];
        core.playback_mode = PlaybackMode::Loop;
        core.current_queue_index = Some(1);

        let next = core.next_track_path().expect("next");
        assert_eq!(next, PathBuf::from("a"));
    }

    #[test]
    fn cycle_header_section_wraps_and_updates_status() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        assert_eq!(core.header_section, HeaderSection::Library);

        core.cycle_header_section();
        assert_eq!(core.header_section, HeaderSection::Lyrics);
        assert_eq!(core.status, "Section: Lyrics");

        core.cycle_header_section();
        core.cycle_header_section();
        core.cycle_header_section();
        assert_eq!(core.header_section, HeaderSection::Library);
    }

    #[test]
    fn root_browser_uses_folders() {
        let mut state = PersistedState::default();
        state.folders.push(PathBuf::from(r"E:\LOCALMUSIC"));
        let core = TuneCore::from_persisted(state);
        assert!(
            core.browser_entries
                .iter()
                .any(|entry| entry.kind == BrowserEntryKind::Folder)
        );
    }

    #[test]
    fn add_folder_sanitizes_leading_bullet_character() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real = temp.path().join("LOCALMUSIC");
        std::fs::create_dir_all(&real).expect("create");
        let copied = PathBuf::from(format!("• {}", real.display()));

        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.add_folder(&copied);

        assert!(core.folders.iter().any(|folder| path_eq(folder, &real)));
    }

    #[test]
    fn add_folder_sanitizes_leading_bullet_without_space() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real = temp.path().join("LOCALMUSIC");
        std::fs::create_dir_all(&real).expect("create");
        let copied = PathBuf::from(format!("•{}", real.display()));

        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.add_folder(&copied);

        assert!(core.folders.iter().any(|folder| path_eq(folder, &real)));
    }

    #[test]
    fn add_folder_sanitizes_bullet_inside_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("Albums");
        let real = parent.join("Live");
        std::fs::create_dir_all(&real).expect("create");

        let copied = PathBuf::from(
            real.to_string_lossy()
                .replace("Albums", "•Albums")
                .replace("Live", "▪Live"),
        );

        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.add_folder(&copied);

        assert!(core.folders.iter().any(|folder| path_eq(folder, &real)));
    }

    #[test]
    fn add_folder_preserves_existing_leading_dash_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real = temp.path().join("-mixes");
        std::fs::create_dir_all(&real).expect("create");

        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.add_folder(&real);

        assert!(core.folders.iter().any(|folder| path_eq(folder, &real)));
    }

    #[test]
    fn add_folder_recovers_from_control_character_copy_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real = temp.path().join("A B");
        std::fs::create_dir_all(&real).expect("create");
        let copied = PathBuf::from(real.to_string_lossy().replace("A B", "A\u{0007} B"));

        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.add_folder(&copied);

        assert!(core.folders.iter().any(|folder| path_eq(folder, &real)));
    }

    #[test]
    fn remove_folder_removes_matching_entry() {
        let mut state = PersistedState::default();
        state.folders.push(PathBuf::from(r"E:\LOCALMUSIC"));
        let mut core = TuneCore::from_persisted(state);

        core.remove_folder(Path::new(r"E:\LOCALMUSIC"));

        assert!(core.folders.is_empty());
        assert_eq!(core.status, "Folder removed");
    }

    #[test]
    fn root_browser_includes_all_songs_entry() {
        let core = TuneCore::from_persisted(PersistedState::default());
        assert!(
            core.browser_entries
                .iter()
                .any(|entry| entry.kind == BrowserEntryKind::AllSongs)
        );
    }

    #[test]
    fn root_browser_includes_playlists() {
        let mut state = PersistedState::default();
        state.playlists.insert(
            String::from("mix"),
            Playlist {
                tracks: vec![PathBuf::from("song.mp3")],
            },
        );
        let core = TuneCore::from_persisted(state);
        assert!(
            core.browser_entries
                .iter()
                .any(|entry| entry.kind == BrowserEntryKind::Playlist && entry.label == "[PL] mix")
        );
    }

    #[test]
    fn activating_playlist_uses_playlist_queue() {
        let mut state = PersistedState::default();
        state.playlists.insert(
            String::from("mix"),
            Playlist {
                tracks: vec![PathBuf::from("song.mp3")],
            },
        );
        let mut core = TuneCore::from_persisted(state);

        core.selected_browser = core
            .browser_entries
            .iter()
            .position(|entry| entry.kind == BrowserEntryKind::Playlist)
            .expect("playlist entry");

        core.activate_selected();
        assert_eq!(core.browser_playlist.as_deref(), Some("mix"));
        assert_eq!(core.browser_entries[0].kind, BrowserEntryKind::Back);
        assert_eq!(core.browser_entries[1].kind, BrowserEntryKind::Track);

        core.selected_browser = 1;
        let selected = core.activate_selected().expect("track selected");
        assert_eq!(selected, PathBuf::from("song.mp3"));
        assert_eq!(core.queue, vec![0]);
        assert_eq!(core.current_queue_index, Some(0));
    }

    #[test]
    fn activating_track_matches_queue_index_by_normalized_path() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![Track {
            path: PathBuf::from(r"\\?\E:\LOCALMUSIC\song.mp3"),
            title: String::from("song"),
            artist: Some(String::from("artist")),
            album: Some(String::from("album")),
        }];
        core.browser_entries = vec![BrowserEntry {
            kind: BrowserEntryKind::Track,
            path: PathBuf::from(r"E:\LOCALMUSIC\song.mp3"),
            label: String::from("song"),
        }];
        core.selected_browser = 0;

        let selected = core.activate_selected().expect("track selected");
        assert_eq!(selected, PathBuf::from(r"E:\LOCALMUSIC\song.mp3"));
        assert_eq!(core.current_queue_index, Some(0));
    }

    #[test]
    fn playlist_browser_prefers_track_title_labels() {
        let mut state = PersistedState::default();
        state.playlists.insert(
            String::from("mix"),
            Playlist {
                tracks: vec![PathBuf::from("song.mp3")],
            },
        );

        let mut core = TuneCore::from_persisted(state);
        core.tracks = vec![Track {
            path: PathBuf::from("song.mp3"),
            title: String::from("Metadata Title"),
            artist: Some(String::from("Metadata Artist")),
            album: None,
        }];
        core.browser_playlist = Some(String::from("mix"));
        core.refresh_browser_entries();

        assert_eq!(core.browser_entries[1].label, "Metadata Title");
    }

    #[test]
    fn navigate_back_stops_at_added_root() {
        let mut state = PersistedState::default();
        state.folders.push(PathBuf::from(r"E:\LOCALMUSIC"));
        let mut core = TuneCore::from_persisted(state);

        core.browser_path = Some(PathBuf::from(r"e:\localmusic"));
        core.navigate_back();

        assert_eq!(core.browser_path, None);
    }

    #[test]
    fn navigate_back_does_not_escape_added_root() {
        let mut state = PersistedState::default();
        state.folders.push(PathBuf::from(r"E:\LOCALMUSIC"));
        let mut core = TuneCore::from_persisted(state);

        core.browser_path = Some(PathBuf::from(r"E:\LOCALMUSIC\Albums"));
        core.navigate_back();
        assert_eq!(core.browser_path, Some(PathBuf::from(r"E:\LOCALMUSIC")));

        core.navigate_back();
        assert_eq!(core.browser_path, None);
    }

    #[test]
    fn shuffle_visits_each_track_before_repeat() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.queue = vec![0, 1, 2, 3];
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
            Track {
                path: PathBuf::from("c.mp3"),
                title: String::from("c"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("d.mp3"),
                title: String::from("d"),
                artist: None,
                album: None,
            },
        ];
        core.rebuild_shuffle_order();
        core.playback_mode = PlaybackMode::Shuffle;

        let mut seen = std::collections::HashSet::new();
        for _ in 0..4 {
            let path = core.next_track_path().expect("next");
            seen.insert(path);
        }

        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn main_queue_is_sorted_by_metadata_title() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from("a.mp3"),
                title: String::from("Zulu"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("b.mp3"),
                title: String::from("alpha"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("c.mp3"),
                title: String::from("Mike"),
                artist: None,
                album: None,
            },
        ];

        core.reset_main_queue();

        let queued_titles: Vec<&str> = core
            .queue
            .iter()
            .map(|idx| core.tracks[*idx].title.as_str())
            .collect();
        assert_eq!(queued_titles, vec!["alpha", "Mike", "Zulu"]);
    }

    #[test]
    fn activating_library_track_uses_metadata_sorted_queue() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from("a.mp3"),
                title: String::from("Zulu"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("b.mp3"),
                title: String::from("Alpha"),
                artist: None,
                album: None,
            },
        ];
        core.track_lookup = build_track_lookup(&core.tracks);
        core.browser_entries = vec![BrowserEntry {
            kind: BrowserEntryKind::Track,
            path: PathBuf::from("b.mp3"),
            label: String::from("Alpha"),
        }];
        core.selected_browser = 0;

        let selected = core.activate_selected().expect("track selected");

        assert_eq!(selected, PathBuf::from("b.mp3"));
        assert_eq!(core.queue, vec![1, 0]);
        assert_eq!(core.current_queue_index, Some(0));
    }

    #[test]
    fn activating_folder_track_uses_folder_local_queue() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from(r"music\folder\a.mp3"),
                title: String::from("a"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from(r"music\folder\b.mp3"),
                title: String::from("b"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from(r"music\other\c.mp3"),
                title: String::from("c"),
                artist: None,
                album: None,
            },
        ];
        core.track_lookup = build_track_lookup(&core.tracks);
        core.browser_path = Some(PathBuf::from(r"music\folder"));
        core.browser_entries = vec![
            BrowserEntry {
                kind: BrowserEntryKind::Back,
                path: PathBuf::from(r"music\folder"),
                label: String::from("[..] Back"),
            },
            BrowserEntry {
                kind: BrowserEntryKind::Track,
                path: PathBuf::from(r"music\folder\a.mp3"),
                label: String::from("a"),
            },
            BrowserEntry {
                kind: BrowserEntryKind::Track,
                path: PathBuf::from(r"music\folder\b.mp3"),
                label: String::from("b"),
            },
        ];
        core.selected_browser = 2;

        let selected = core.activate_selected().expect("track selected");

        assert_eq!(selected, PathBuf::from(r"music\folder\b.mp3"));
        assert_eq!(core.queue, vec![0, 1]);
        assert_eq!(core.current_queue_index, Some(1));
    }

    #[test]
    fn adding_folder_selection_to_playlist_adds_all_folder_tracks() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from(r"music\folder\a.mp3"),
                title: String::from("a"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from(r"music\folder\sub\b.mp3"),
                title: String::from("b"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from(r"music\other\c.mp3"),
                title: String::from("c"),
                artist: None,
                album: None,
            },
        ];
        core.track_lookup = build_track_lookup(&core.tracks);
        core.browser_entries = vec![BrowserEntry {
            kind: BrowserEntryKind::Folder,
            path: PathBuf::from(r"music\folder"),
            label: String::from("[DIR] folder"),
        }];

        core.add_selected_to_playlist("mix");

        let playlist = core.playlists.get("mix").expect("playlist exists");
        assert_eq!(playlist.tracks.len(), 2);
        assert!(
            playlist
                .tracks
                .iter()
                .any(|p| p == &PathBuf::from(r"music\folder\a.mp3"))
        );
        assert!(
            playlist
                .tracks
                .iter()
                .any(|p| p == &PathBuf::from(r"music\folder\sub\b.mp3"))
        );
    }

    #[test]
    fn adding_playlist_selection_to_playlist_copies_tracks() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.playlists.insert(
            String::from("source"),
            Playlist {
                tracks: vec![PathBuf::from("a.mp3"), PathBuf::from("b.mp3")],
            },
        );
        core.browser_entries = vec![BrowserEntry {
            kind: BrowserEntryKind::Playlist,
            path: PathBuf::from("source"),
            label: String::from("[PL] source"),
        }];

        core.add_selected_to_playlist("target");

        let playlist = core.playlists.get("target").expect("target exists");
        assert_eq!(
            playlist.tracks,
            vec![PathBuf::from("a.mp3"), PathBuf::from("b.mp3")]
        );
    }

    #[test]
    fn adding_all_songs_selection_to_playlist_adds_full_library() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from("z.mp3"),
                title: String::from("Zulu"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("a.mp3"),
                title: String::from("Alpha"),
                artist: None,
                album: None,
            },
        ];
        core.track_lookup = build_track_lookup(&core.tracks);
        core.browser_entries = vec![BrowserEntry {
            kind: BrowserEntryKind::AllSongs,
            path: PathBuf::new(),
            label: String::from("[ALL] All Songs"),
        }];

        core.add_selected_to_playlist("mix");

        let playlist = core.playlists.get("mix").expect("playlist exists");
        assert_eq!(
            playlist.tracks,
            vec![PathBuf::from("a.mp3"), PathBuf::from("z.mp3")]
        );
    }

    #[test]
    fn remove_selected_from_current_playlist_removes_track() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.playlists.insert(
            String::from("mix"),
            Playlist {
                tracks: vec![PathBuf::from("a.mp3"), PathBuf::from("b.mp3")],
            },
        );
        core.browser_playlist = Some(String::from("mix"));
        core.browser_entries = vec![
            BrowserEntry {
                kind: BrowserEntryKind::Back,
                path: PathBuf::new(),
                label: String::from("[..] Back"),
            },
            BrowserEntry {
                kind: BrowserEntryKind::Track,
                path: PathBuf::from("b.mp3"),
                label: String::from("b"),
            },
        ];
        core.selected_browser = 1;

        core.remove_selected_from_current_playlist();

        let playlist = core.playlists.get("mix").expect("playlist exists");
        assert_eq!(playlist.tracks, vec![PathBuf::from("a.mp3")]);
    }

    #[test]
    fn remove_playlist_refreshes_root_browser_entries() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.create_playlist("mix");
        assert!(
            core.browser_entries
                .iter()
                .any(|entry| entry.kind == BrowserEntryKind::Playlist && entry.label == "[PL] mix")
        );

        core.remove_playlist("mix");

        assert!(
            !core
                .browser_entries
                .iter()
                .any(|entry| entry.kind == BrowserEntryKind::Playlist && entry.label == "[PL] mix")
        );
    }

    #[test]
    fn prev_track_path_moves_back_in_queue() {
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
        core.track_lookup = build_track_lookup(&core.tracks);
        core.queue = vec![0, 1];
        core.current_queue_index = Some(1);
        core.playback_mode = PlaybackMode::Normal;

        let prev = core.prev_track_path().expect("prev track");

        assert_eq!(prev, PathBuf::from("a.mp3"));
        assert_eq!(core.current_queue_index, Some(0));
    }

    proptest::proptest! {
        #[test]
        fn next_index_is_in_bounds(len in 1usize..50, current in 0usize..50) {
            let mut core = TuneCore::from_persisted(PersistedState::default());
            core.tracks = (0..len)
                .map(|n| Track {
                    path: PathBuf::from(format!("{n}")),
                    title: format!("{n}"),
                    artist: None,
                    album: None,
                })
                .collect();
            core.track_lookup = build_track_lookup(&core.tracks);
            core.queue = (0..len).collect();
            core.rebuild_shuffle_order();
            core.current_queue_index = Some(current.min(len - 1));

            for mode in [PlaybackMode::Normal, PlaybackMode::Shuffle, PlaybackMode::Loop, PlaybackMode::LoopOne] {
                core.playback_mode = mode;
                if let Some(path) = core.next_track_path() {
                    assert!(core.tracks.iter().any(|track| track.path == path));
                }
            }
        }

        #[test]
        fn core_state_invariants_hold_after_random_ops(ops in proptest::collection::vec(0u8..8, 1..200)) {
            let mut core = TuneCore::from_persisted(PersistedState::default());
            core.tracks = (0..8)
                .map(|n| Track {
                    path: PathBuf::from(format!("song_{n}.mp3")),
                    title: format!("song_{n}"),
                    artist: None,
                    album: None,
                })
                .collect();
            core.track_lookup = build_track_lookup(&core.tracks);
            core.reset_main_queue();

            for op in ops {
                match op {
                    0 => core.select_next(),
                    1 => core.select_prev(),
                    2 => core.cycle_mode(),
                    3 => {
                        let _ = core.next_track_path();
                    }
                    4 => core.navigate_back(),
                    5 => core.reset_main_queue(),
                    6 => core.refresh_browser_entries(),
                    _ => {
                        if let Some(name) = core.playlists.keys().next().cloned() {
                            core.load_playlist_queue(&name);
                        }
                    }
                }

                if let Some(idx) = core.current_queue_index {
                    prop_assert!(idx < core.queue.len());
                }
                prop_assert!(core.queue.iter().all(|idx| *idx < core.tracks.len()));
                if !core.browser_entries.is_empty() {
                    prop_assert!(core.selected_browser < core.browser_entries.len());
                }
            }
        }
    }
}
