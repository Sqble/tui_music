use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyPlaybackMode {
    Normal,
    Shuffle,
    Loop,
    LoopOne,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Dark,
    System,
    PitchBlack,
    Galaxy,
    Matrix,
    Demonic,
    CottonCandy,
    Ocean,
    Forest,
    Sunset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CoverArtTemplate {
    #[default]
    Aurora,
}

impl CoverArtTemplate {
    pub fn next(self) -> Self {
        Self::Aurora
    }
}

impl Serialize for CoverArtTemplate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str("Aurora")
    }
}

impl<'de> Deserialize<'de> for CoverArtTemplate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "Aurora" | "aurora" | "Mosaic" | "mosaic" | "Horizon" | "horizon" | "Ember"
            | "ember" | "Cat" | "cat" | "Mono" | "mono" => Ok(Self::Aurora),
            _ => Ok(Self::Aurora),
        }
    }
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::All => "Playlist",
            Self::One => "One",
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
    #[serde(default)]
    pub shuffle_enabled: bool,
    #[serde(default)]
    pub repeat_mode: RepeatMode,
    #[serde(default, skip_serializing)]
    pub playback_mode: Option<LegacyPlaybackMode>,
    #[serde(default)]
    pub loudness_normalization: bool,
    #[serde(default)]
    pub crossfade_seconds: u16,
    #[serde(default = "default_scrub_seconds")]
    pub scrub_seconds: u16,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub selected_output_device: Option<String>,
    #[serde(default = "default_saved_volume")]
    pub saved_volume: f32,
    #[serde(default = "default_stats_enabled")]
    pub stats_enabled: bool,
    #[serde(default = "default_online_sync_correction_threshold_ms")]
    pub online_sync_correction_threshold_ms: u16,
    #[serde(default = "default_stats_top_songs_count")]
    pub stats_top_songs_count: u8,
    #[serde(default)]
    pub fallback_cover_template: CoverArtTemplate,
    #[serde(default)]
    pub online_nickname: Option<String>,
}

fn default_stats_enabled() -> bool {
    true
}

fn default_saved_volume() -> f32 {
    1.0
}

fn default_scrub_seconds() -> u16 {
    5
}

fn default_online_sync_correction_threshold_ms() -> u16 {
    300
}

fn default_stats_top_songs_count() -> u8 {
    10
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            playlists: HashMap::new(),
            shuffle_enabled: false,
            repeat_mode: RepeatMode::Off,
            playback_mode: None,
            loudness_normalization: false,
            crossfade_seconds: 0,
            scrub_seconds: default_scrub_seconds(),
            theme: Theme::default(),
            selected_output_device: None,
            saved_volume: default_saved_volume(),
            stats_enabled: default_stats_enabled(),
            online_sync_correction_threshold_ms: default_online_sync_correction_threshold_ms(),
            stats_top_songs_count: default_stats_top_songs_count(),
            fallback_cover_template: CoverArtTemplate::default(),
            online_nickname: None,
        }
    }
}

impl PersistedState {
    pub fn migrate_legacy_playback_mode(&mut self) {
        let Some(mode) = self.playback_mode.take() else {
            return;
        };

        match mode {
            LegacyPlaybackMode::Normal => {
                self.shuffle_enabled = false;
                self.repeat_mode = RepeatMode::Off;
            }
            LegacyPlaybackMode::Shuffle => {
                self.shuffle_enabled = true;
                self.repeat_mode = RepeatMode::All;
            }
            LegacyPlaybackMode::Loop => {
                self.shuffle_enabled = false;
                self.repeat_mode = RepeatMode::All;
            }
            LegacyPlaybackMode::LoopOne => {
                self.shuffle_enabled = false;
                self.repeat_mode = RepeatMode::One;
            }
        }
    }
}
