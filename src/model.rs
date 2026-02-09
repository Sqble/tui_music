use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackMode {
    Normal,
    Shuffle,
    Loop,
    LoopOne,
}

impl PlaybackMode {
    pub fn next(self) -> Self {
        match self {
            Self::Normal => Self::Shuffle,
            Self::Shuffle => Self::Loop,
            Self::Loop => Self::LoopOne,
            Self::LoopOne => Self::Normal,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Playlist {
    pub tracks: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    pub folders: Vec<PathBuf>,
    pub playlists: HashMap<String, Playlist>,
    pub playback_mode: PlaybackMode,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            playlists: HashMap::new(),
            playback_mode: PlaybackMode::Normal,
        }
    }
}
