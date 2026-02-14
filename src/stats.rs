use crate::config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_EVENTS: usize = 20_000;
const MIN_TRACKED_LISTEN_SECONDS: u32 = 10;
const MINUTE_TREND_END_ADVANCE_SECONDS: i64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsRange {
    Today,
    Days7,
    Days30,
    Lifetime,
}

impl StatsRange {
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
            Self::Lifetime => "All",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Today => Self::Days7,
            Self::Days7 => Self::Days30,
            Self::Days30 => Self::Lifetime,
            Self::Lifetime => Self::Today,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsSort {
    Plays,
    ListenTime,
}

impl StatsSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Plays => "plays",
            Self::ListenTime => "listen",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Plays => Self::ListenTime,
            Self::ListenTime => Self::Plays,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatsQuery {
    pub range: StatsRange,
    pub sort: StatsSort,
    pub artist_filter: String,
    pub album_filter: String,
    pub search: String,
}

impl Default for StatsQuery {
    fn default() -> Self {
        Self {
            range: StatsRange::Lifetime,
            sort: StatsSort::ListenTime,
            artist_filter: String::new(),
            album_filter: String::new(),
            search: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListenSessionRecord {
    pub track_path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub started_at_epoch_seconds: i64,
    pub listened_seconds: u32,
    pub completed: bool,
    pub duration_seconds: Option<u32>,
    pub counted_play_override: Option<bool>,
    pub allow_short_listen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenEvent {
    pub track_path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub started_at_epoch_seconds: i64,
    pub listened_seconds: u32,
    pub counted_play: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrackTotals {
    pub play_count: u64,
    pub listen_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatsStore {
    pub track_totals: HashMap<String, TrackTotals>,
    pub events: Vec<ListenEvent>,
}

#[derive(Debug, Clone)]
pub struct TrackStatsRow {
    pub track_path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub play_count: u64,
    pub listen_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct StatsSnapshot {
    pub total_plays: u64,
    pub total_listen_seconds: u64,
    pub rows: Vec<TrackStatsRow>,
    pub recent: Vec<ListenEvent>,
    pub trend: TrendSeries,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrendUnit {
    Minutes,
    Hours,
    Days,
    Weeks,
    AllTime,
}

impl TrendUnit {
    pub fn label(self) -> &'static str {
        match self {
            Self::Minutes => "minutes",
            Self::Hours => "hours",
            Self::Days => "days",
            Self::Weeks => "weeks",
            Self::AllTime => "all-time",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrendSeries {
    pub unit: TrendUnit,
    pub start_epoch_seconds: i64,
    pub end_epoch_seconds: i64,
    pub buckets: Vec<u64>,
    pub show_clock_time_labels: bool,
}

pub fn now_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn load_stats() -> Result<StatsStore> {
    let path = config::stats_path()?;
    load_stats_from_path(&path)
}

pub fn save_stats(store: &StatsStore) -> Result<()> {
    config::ensure_config_dir()?;
    let path = config::stats_path()?;
    save_stats_to_path(&path, store)
}

fn load_stats_from_path(path: &Path) -> Result<StatsStore> {
    if !path.exists() {
        return Ok(StatsStore::default());
    }

    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let store: StatsStore = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(store)
}

fn save_stats_to_path(path: &Path, store: &StatsStore) -> Result<()> {
    if path.exists() {
        let backup = path.with_extension("json.bak");
        let _ = fs::copy(path, &backup);
    }
    let json = serde_json::to_string_pretty(store)?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

impl StatsStore {
    pub fn clear_history(&mut self) {
        self.track_totals.clear();
        self.events.clear();
    }

    pub fn record_listen(&mut self, record: ListenSessionRecord) {
        let counted_play = record.counted_play_override.unwrap_or_else(|| {
            should_count_as_play(
                record.listened_seconds,
                record.completed,
                record.duration_seconds,
            )
        });
        if record.listened_seconds < MIN_TRACKED_LISTEN_SECONDS
            && !counted_play
            && !record.allow_short_listen
        {
            return;
        }

        let key = track_key(&record.track_path);

        let totals = self.track_totals.entry(key).or_default();
        totals.listen_seconds = totals
            .listen_seconds
            .saturating_add(u64::from(record.listened_seconds));
        if counted_play {
            totals.play_count = totals.play_count.saturating_add(1);
        }

        self.events.push(ListenEvent {
            track_path: record.track_path,
            title: record.title,
            artist: record.artist,
            album: record.album,
            started_at_epoch_seconds: record.started_at_epoch_seconds,
            listened_seconds: record.listened_seconds,
            counted_play,
        });

        if self.events.len() > MAX_EVENTS {
            let drop_count = self.events.len().saturating_sub(MAX_EVENTS);
            self.events.drain(0..drop_count);
        }
    }

    pub fn query(&self, query: &StatsQuery, now_epoch_seconds: i64) -> StatsSnapshot {
        let range_start = range_start_epoch(query.range, now_epoch_seconds);
        let artist_filter = query.artist_filter.trim().to_ascii_lowercase();
        let album_filter = query.album_filter.trim().to_ascii_lowercase();
        let search_tokens: Vec<String> = query
            .search
            .split_whitespace()
            .map(|token| token.to_ascii_lowercase())
            .collect();

        let mut by_track: HashMap<String, TrackStatsRow> = HashMap::new();
        let mut total_plays = 0_u64;
        let mut total_listen_seconds = 0_u64;
        let mut recent: HashMap<String, ListenEvent> = HashMap::new();

        for event in &self.events {
            if matches!(
                range_start,
                Some(start) if event.started_at_epoch_seconds < start
            ) {
                continue;
            }

            let artist_text = event
                .artist
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let album_text = event
                .album
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !artist_filter.is_empty() && !artist_text.contains(&artist_filter) {
                continue;
            }
            if !album_filter.is_empty() && !album_text.contains(&album_filter) {
                continue;
            }

            if !search_tokens.is_empty() {
                let haystack = format!(
                    "{} {} {}",
                    event.title,
                    event.artist.as_deref().unwrap_or_default(),
                    event.album.as_deref().unwrap_or_default()
                )
                .to_ascii_lowercase();
                if !search_tokens
                    .iter()
                    .all(|token| fuzzy_match(&haystack, token))
                {
                    continue;
                }
            }

            let key = track_key(&event.track_path);
            let row = by_track
                .entry(key.clone())
                .or_insert_with(|| TrackStatsRow {
                    track_path: event.track_path.clone(),
                    title: event.title.clone(),
                    artist: event.artist.clone(),
                    album: event.album.clone(),
                    play_count: 0,
                    listen_seconds: 0,
                });
            row.listen_seconds = row
                .listen_seconds
                .saturating_add(u64::from(event.listened_seconds));
            if event.counted_play {
                row.play_count = row.play_count.saturating_add(1);
                total_plays = total_plays.saturating_add(1);
            }
            total_listen_seconds =
                total_listen_seconds.saturating_add(u64::from(event.listened_seconds));

            let recent_key = format!("{}|{}", key, event.started_at_epoch_seconds);
            match recent.get_mut(&recent_key) {
                Some(aggregate) => {
                    aggregate.listened_seconds = aggregate
                        .listened_seconds
                        .saturating_add(event.listened_seconds);
                    aggregate.counted_play |= event.counted_play;
                }
                None => {
                    recent.insert(recent_key, event.clone());
                }
            }
        }

        let mut rows: Vec<TrackStatsRow> = by_track.into_values().collect();
        rows.sort_by(|a, b| compare_rows(a, b, query.sort));

        let mut recent: Vec<ListenEvent> = recent.into_values().collect();
        recent.sort_by(|a, b| b.started_at_epoch_seconds.cmp(&a.started_at_epoch_seconds));
        let trend = build_trend_series(query.range, query.sort, now_epoch_seconds, &recent);
        recent.truncate(12);

        StatsSnapshot {
            total_plays,
            total_listen_seconds,
            rows,
            recent,
            trend,
        }
    }
}

fn build_trend_series(
    range: StatsRange,
    sort: StatsSort,
    now_epoch_seconds: i64,
    events: &[ListenEvent],
) -> TrendSeries {
    let day = 86_400_i64;
    let week = day * 7;
    let default_start = range_start_epoch(range, now_epoch_seconds)
        .unwrap_or(now_epoch_seconds.saturating_sub(week * 12));
    let default_end = now_epoch_seconds.max(default_start + 1);

    let start = events
        .iter()
        .map(|event| event.started_at_epoch_seconds)
        .min()
        .unwrap_or(default_start);
    let mut end = events
        .iter()
        .map(|event| event.started_at_epoch_seconds)
        .max()
        .unwrap_or(default_end);
    if matches!(
        range,
        StatsRange::Lifetime | StatsRange::Today | StatsRange::Days7 | StatsRange::Days30
    ) {
        end = now_epoch_seconds.max(start.saturating_add(1));
    }
    if end <= start {
        end = start.saturating_add(day);
    }

    let span = end.saturating_sub(start).max(1);
    let (unit, step_seconds) = if span <= 6 * 3600 {
        (TrendUnit::Minutes, 60_i64)
    } else if span <= day * 3 {
        (TrendUnit::Hours, 3600_i64)
    } else if span <= day * 120 {
        (TrendUnit::Days, day)
    } else if span <= day * 365 * 2 {
        (TrendUnit::Weeks, week)
    } else {
        let step = (span / 40).max(day);
        (TrendUnit::AllTime, step)
    };

    let bucket_len = ((span / step_seconds).saturating_add(1)).clamp(2, 256) as usize;
    let mut buckets = vec![0_u64; bucket_len];
    for event in events {
        let index = ((event.started_at_epoch_seconds.saturating_sub(start)) / step_seconds)
            .clamp(0, (bucket_len as i64) - 1) as usize;
        let bucket_value = match sort {
            StatsSort::Plays => u64::from(event.counted_play),
            StatsSort::ListenTime => u64::from(event.listened_seconds),
        };
        buckets[index] = buckets[index].saturating_add(bucket_value);
    }

    let mut end_epoch_seconds =
        start.saturating_add(step_seconds.saturating_mul((bucket_len as i64) - 1));
    if unit == TrendUnit::Minutes {
        let lag = end.saturating_sub(end_epoch_seconds);
        if lag >= MINUTE_TREND_END_ADVANCE_SECONDS {
            end_epoch_seconds = end;
        }
    }
    TrendSeries {
        unit,
        start_epoch_seconds: start,
        end_epoch_seconds,
        buckets,
        show_clock_time_labels: true,
    }
}

fn compare_rows(a: &TrackStatsRow, b: &TrackStatsRow, sort: StatsSort) -> Ordering {
    let primary = match sort {
        StatsSort::Plays => b.play_count.cmp(&a.play_count),
        StatsSort::ListenTime => b.listen_seconds.cmp(&a.listen_seconds),
    };
    if primary != Ordering::Equal {
        return primary;
    }

    b.listen_seconds
        .cmp(&a.listen_seconds)
        .then(b.play_count.cmp(&a.play_count))
        .then_with(|| {
            a.title
                .to_ascii_lowercase()
                .cmp(&b.title.to_ascii_lowercase())
        })
}

fn range_start_epoch(range: StatsRange, now_epoch_seconds: i64) -> Option<i64> {
    let day = 86_400_i64;
    match range {
        StatsRange::Today => Some(now_epoch_seconds.saturating_sub(day)),
        StatsRange::Days7 => Some(now_epoch_seconds.saturating_sub(day * 7)),
        StatsRange::Days30 => Some(now_epoch_seconds.saturating_sub(day * 30)),
        StatsRange::Lifetime => None,
    }
}

fn track_key(path: &Path) -> String {
    let normalized = config::normalize_path(path);
    normalized.to_string_lossy().to_ascii_lowercase()
}

fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.contains(needle) {
        return true;
    }

    let mut chars = needle.chars();
    let mut target = chars.next();
    for ch in haystack.chars() {
        if let Some(needed) = target {
            if ch == needed {
                target = chars.next();
            }
        } else {
            return true;
        }
    }
    target.is_none()
}

pub(crate) fn should_count_as_play(
    listened_seconds: u32,
    completed: bool,
    duration_seconds: Option<u32>,
) -> bool {
    if duration_seconds.is_some_and(|duration| duration < 30) {
        return completed;
    }
    listened_seconds >= 30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_track_counts_only_on_complete() {
        assert!(!should_count_as_play(29, false, Some(20)));
        assert!(should_count_as_play(20, true, Some(20)));
    }

    #[test]
    fn long_track_counts_after_30_seconds() {
        assert!(!should_count_as_play(29, true, Some(240)));
        assert!(should_count_as_play(30, false, Some(240)));
    }

    #[test]
    fn ignores_very_short_listens() {
        let mut store = StatsStore::default();
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("C:/music/Short.mp3"),
            title: "Short".to_string(),
            artist: None,
            album: None,
            started_at_epoch_seconds: 10,
            listened_seconds: 9,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });

        assert!(store.events.is_empty());
        assert!(store.track_totals.is_empty());
    }

    #[test]
    fn query_applies_search_filters_and_sort() {
        let mut store = StatsStore::default();
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("C:/music/A.mp3"),
            title: "Night Drive".to_string(),
            artist: Some("Neon".to_string()),
            album: Some("Skyline".to_string()),
            started_at_epoch_seconds: 1_000,
            listened_seconds: 40,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("C:/music/B.mp3"),
            title: "Ocean Room".to_string(),
            artist: Some("Blue".to_string()),
            album: Some("Harbor".to_string()),
            started_at_epoch_seconds: 1_200,
            listened_seconds: 80,
            completed: false,
            duration_seconds: Some(220),
            counted_play_override: None,
            allow_short_listen: false,
        });

        let snapshot = store.query(
            &StatsQuery {
                range: StatsRange::Lifetime,
                sort: StatsSort::ListenTime,
                artist_filter: String::from("bl"),
                album_filter: String::new(),
                search: String::from("or"),
            },
            2_000,
        );

        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.rows[0].title, "Ocean Room");
        assert_eq!(snapshot.total_plays, 1);
    }

    #[test]
    fn trend_metric_tracks_selected_sort_mode() {
        let mut store = StatsStore::default();
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("C:/music/A.mp3"),
            title: "A".to_string(),
            artist: None,
            album: None,
            started_at_epoch_seconds: 10,
            listened_seconds: 45,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("C:/music/B.mp3"),
            title: "B".to_string(),
            artist: None,
            album: None,
            started_at_epoch_seconds: 20,
            listened_seconds: 15,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });

        let by_plays = store.query(
            &StatsQuery {
                range: StatsRange::Lifetime,
                sort: StatsSort::Plays,
                artist_filter: String::new(),
                album_filter: String::new(),
                search: String::new(),
            },
            100,
        );
        let by_listen = store.query(
            &StatsQuery {
                range: StatsRange::Lifetime,
                sort: StatsSort::ListenTime,
                artist_filter: String::new(),
                album_filter: String::new(),
                search: String::new(),
            },
            100,
        );

        assert_eq!(
            by_plays.trend.buckets.iter().sum::<u64>(),
            by_plays.total_plays
        );
        assert_eq!(
            by_listen.trend.buckets.iter().sum::<u64>(),
            by_listen.total_listen_seconds
        );
    }

    #[test]
    fn recent_log_collapses_partial_events_for_same_session() {
        let mut store = StatsStore::default();
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("C:/music/A.mp3"),
            title: "A".to_string(),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            started_at_epoch_seconds: 1_000,
            listened_seconds: 10,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: Some(false),
            allow_short_listen: true,
        });
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("C:/music/A.mp3"),
            title: "A".to_string(),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            started_at_epoch_seconds: 1_000,
            listened_seconds: 12,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: Some(false),
            allow_short_listen: true,
        });

        let snapshot = store.query(
            &StatsQuery {
                range: StatsRange::Lifetime,
                sort: StatsSort::ListenTime,
                artist_filter: String::new(),
                album_filter: String::new(),
                search: String::new(),
            },
            2_000,
        );

        assert_eq!(snapshot.recent.len(), 1);
        assert_eq!(snapshot.recent[0].listened_seconds, 22);
    }

    #[test]
    fn minute_trend_advances_end_to_now_after_reasonable_lag() {
        let events = vec![ListenEvent {
            track_path: PathBuf::from("C:/music/A.mp3"),
            title: "A".to_string(),
            artist: None,
            album: None,
            started_at_epoch_seconds: 0,
            listened_seconds: 30,
            counted_play: true,
        }];

        let trend = build_trend_series(StatsRange::Today, StatsSort::ListenTime, 95, &events);

        assert_eq!(trend.unit, TrendUnit::Minutes);
        assert_eq!(trend.end_epoch_seconds, 95);
    }

    #[test]
    fn minute_trend_keeps_bucket_aligned_end_for_small_lag() {
        let events = vec![ListenEvent {
            track_path: PathBuf::from("C:/music/A.mp3"),
            title: "A".to_string(),
            artist: None,
            album: None,
            started_at_epoch_seconds: 0,
            listened_seconds: 30,
            counted_play: true,
        }];

        let trend = build_trend_series(StatsRange::Today, StatsSort::ListenTime, 70, &events);

        assert_eq!(trend.unit, TrendUnit::Minutes);
        assert_eq!(trend.end_epoch_seconds, 60);
    }
}
