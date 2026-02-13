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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Dark,
    PitchBlack,
    Galaxy,
    Matrix,
    Demonic,
    CottonCandy,
    Ocean,
    Forest,
    Sunset,
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
    #[serde(default)]
    pub loudness_normalization: bool,
    #[serde(default)]
    pub crossfade_seconds: u16,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub selected_output_device: Option<String>,
    #[serde(default = "default_stats_enabled")]
    pub stats_enabled: bool,
}

fn default_stats_enabled() -> bool {
    true
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            playlists: HashMap::new(),
            playback_mode: PlaybackMode::Normal,
            loudness_normalization: false,
            crossfade_seconds: 0,
            theme: Theme::default(),
            selected_output_device: None,
            stats_enabled: default_stats_enabled(),
        }
    }
}
