use crate::config;
use crate::library;
use crate::model::{PersistedState, PlaybackMode, Playlist, Track};
use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserEntryKind {
    Back,
    Folder,
    Playlist,
    AllSongs,
    Track,
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
    pub browser_path: Option<PathBuf>,
    pub browser_playlist: Option<String>,
    pub browser_all_songs: bool,
    pub browser_entries: Vec<BrowserEntry>,
    pub selected_browser: usize,
    pub dirty: bool,
    pub status: String,
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
            browser_path: None,
            browser_playlist: None,
            browser_all_songs: false,
            browser_entries: Vec::new(),
            selected_browser: 0,
            dirty: true,
            status: String::from("Ready"),
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
        }
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        config::save_state(&self.persisted_state())?;
        self.set_status("State saved");
        Ok(())
    }

    pub fn add_folder(&mut self, input: &Path) {
        let normalized = config::normalize_path(input);
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
        self.set_status("Playlist created");
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

    pub fn current_path(&self) -> Option<&Path> {
        let queue_index = self.current_queue_index?;
        let track_index = *self.queue.get(queue_index)?;
        self.tracks
            .get(track_index)
            .map(|track| track.path.as_path())
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
                        label: track.title.clone(),
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
                    let file_name = entry.file_name().to_string_lossy().to_string();

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
                    .map(|name| name.to_string_lossy().to_string())
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
                    label: format!("[PL] {name}"),
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
            .map(|track| track.title.clone())
            .unwrap_or_else(|| {
                path.file_name()
                    .map(|file| file.to_string_lossy().to_string())
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
