use crate::config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

const MAX_EVENTS: usize = 20_000;
const MIN_TRACKED_LISTEN_SECONDS: u32 = 10;
const MINUTE_TREND_END_ADVANCE_SECONDS: i64 = 20;
const STATS_SCHEMA_VERSION: u32 = 2;

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
    pub provider_track_id: Option<String>,
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
    #[serde(default)]
    pub provider_track_id: Option<String>,
    pub started_at_epoch_seconds: i64,
    pub listened_seconds: u32,
    pub counted_play: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrackTotals {
    pub play_count: u64,
    pub listen_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsStore {
    #[serde(default = "default_stats_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub provider_track_key_map: HashMap<String, String>,
    #[serde(default)]
    pub track_totals: HashMap<String, TrackTotals>,
    #[serde(default)]
    pub events: Vec<ListenEvent>,
}

impl Default for StatsStore {
    fn default() -> Self {
        Self {
            schema_version: STATS_SCHEMA_VERSION,
            provider_track_key_map: HashMap::new(),
            track_totals: HashMap::new(),
            events: Vec::new(),
        }
    }
}

fn default_stats_schema_version() -> u32 {
    STATS_SCHEMA_VERSION
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
    let mut store: StatsStore = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    migrate_store(&mut store);
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

        let normalized_provider = normalize_provider_track_id(record.provider_track_id.as_deref());
        if let (Some(provider), Some(metadata_key)) = (
            normalized_provider.as_deref(),
            metadata_track_key(record.artist.as_deref(), &record.title),
        ) {
            self.provider_track_key_map
                .entry(provider.to_string())
                .or_insert(metadata_key);
        }

        let key = self.resolve_track_key(
            &record.title,
            record.artist.as_deref(),
            &record.track_path,
            normalized_provider.as_deref(),
        );

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
            provider_track_id: normalized_provider,
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

            let key = self.resolve_track_key(
                &event.title,
                event.artist.as_deref(),
                &event.track_path,
                event.provider_track_id.as_deref(),
            );
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

    fn resolve_track_key(
        &self,
        title: &str,
        artist: Option<&str>,
        track_path: &Path,
        provider_track_id: Option<&str>,
    ) -> String {
        let normalized_provider = normalize_provider_track_id(provider_track_id);
        if let Some(provider) = normalized_provider.as_deref()
            && let Some(mapped) = self.provider_track_key_map.get(provider)
        {
            return mapped.clone();
        }

        if let Some(metadata_key) = metadata_track_key(artist, title) {
            return metadata_key;
        }

        if let Some(provider) = normalized_provider {
            return format!("provider:{provider}");
        }

        legacy_path_key(track_path)
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

    let bucket_len = ((span / step_seconds).saturating_add(1)).clamp(2, 512) as usize;
    let mut buckets = vec![0_u64; bucket_len];
    for event in events {
        match sort {
            StatsSort::Plays => {
                let index = ((event.started_at_epoch_seconds.saturating_sub(start)) / step_seconds)
                    .clamp(0, (bucket_len as i64) - 1) as usize;
                buckets[index] = buckets[index].saturating_add(u64::from(event.counted_play));
            }
            StatsSort::ListenTime => {
                add_listen_time_to_buckets(
                    &mut buckets,
                    start,
                    step_seconds,
                    event.started_at_epoch_seconds,
                    u64::from(event.listened_seconds),
                );
            }
        }
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

fn add_listen_time_to_buckets(
    buckets: &mut [u64],
    start_epoch_seconds: i64,
    step_seconds: i64,
    event_start_epoch_seconds: i64,
    listened_seconds: u64,
) {
    if buckets.is_empty() || listened_seconds == 0 {
        return;
    }

    let bucket_len = buckets.len();
    let series_end = start_epoch_seconds
        .saturating_add(step_seconds.saturating_mul(bucket_len.saturating_sub(1) as i64));
    let event_start = event_start_epoch_seconds.max(start_epoch_seconds);
    let event_end = event_start.saturating_add(listened_seconds as i64);
    let mut allocated = 0_u64;

    for (idx, bucket) in buckets.iter_mut().enumerate() {
        let bucket_start =
            start_epoch_seconds.saturating_add(step_seconds.saturating_mul(idx as i64));
        let bucket_end = bucket_start.saturating_add(step_seconds);
        let overlap_start = event_start.max(bucket_start);
        let overlap_end = event_end.min(bucket_end);
        if overlap_end <= overlap_start {
            continue;
        }
        let slice = (overlap_end.saturating_sub(overlap_start)) as u64;
        *bucket = bucket.saturating_add(slice);
        allocated = allocated.saturating_add(slice);
    }

    if event_end > series_end
        && allocated < listened_seconds
        && let Some(last) = buckets.last_mut()
    {
        *last = last.saturating_add(listened_seconds.saturating_sub(allocated));
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

fn legacy_path_key(path: &Path) -> String {
    let normalized = config::normalize_path(path);
    normalized.to_string_lossy().to_ascii_lowercase()
}

fn normalize_provider_track_id(value: Option<&str>) -> Option<String> {
    let trimmed = value.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

fn metadata_track_key(artist: Option<&str>, title: &str) -> Option<String> {
    let normalized_artist = normalize_artist_for_match(artist?);
    let normalized_title = normalize_text_for_match(title);
    if normalized_artist.is_empty() || normalized_title.is_empty() {
        return None;
    }
    Some(format!("meta:{normalized_artist}|{normalized_title}"))
}

fn normalize_artist_for_match(value: &str) -> String {
    let without_featured = strip_featured_artists(value);
    normalize_text_for_match(without_featured.trim())
}

fn strip_featured_artists(value: &str) -> &str {
    let lower = value.to_ascii_lowercase();
    let mut cut = value.len();
    for marker in [" feat.", " feat ", " ft.", " ft ", " featuring "] {
        if let Some(index) = lower.find(marker) {
            cut = cut.min(index);
        }
    }
    &value[..cut]
}

fn normalize_text_for_match(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_space = false;
    for ch in value.nfkc().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
            in_space = false;
        } else if (ch.is_whitespace() || ch.is_ascii_punctuation()) && !out.is_empty() && !in_space
        {
            out.push(' ');
            in_space = true;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

fn migrate_store(store: &mut StatsStore) {
    let mut migrated_totals = HashMap::with_capacity(store.track_totals.len());
    let event_path_metadata = event_metadata_by_path(&store.events);
    for (legacy_key, totals) in std::mem::take(&mut store.track_totals) {
        let target_key = event_path_metadata
            .get(&legacy_key)
            .and_then(|(artist, title)| metadata_track_key(artist.as_deref(), title))
            .unwrap_or(legacy_key);
        let bucket = migrated_totals
            .entry(target_key)
            .or_insert_with(TrackTotals::default);
        bucket.play_count = bucket.play_count.saturating_add(totals.play_count);
        bucket.listen_seconds = bucket.listen_seconds.saturating_add(totals.listen_seconds);
    }
    store.track_totals = migrated_totals;

    for event in &mut store.events {
        event.provider_track_id = normalize_provider_track_id(event.provider_track_id.as_deref());
    }

    let mut migrated_provider_map = HashMap::with_capacity(store.provider_track_key_map.len());
    for (provider, key) in std::mem::take(&mut store.provider_track_key_map) {
        let normalized_provider = normalize_provider_track_id(Some(&provider));
        let normalized_key = key.trim();
        if let (Some(provider), false) = (normalized_provider, normalized_key.is_empty()) {
            migrated_provider_map.insert(provider, normalized_key.to_string());
        }
    }
    for event in &store.events {
        if let (Some(provider), Some(key)) = (
            event.provider_track_id.as_deref(),
            metadata_track_key(event.artist.as_deref(), &event.title),
        ) {
            migrated_provider_map
                .entry(provider.to_string())
                .or_insert(key);
        }
    }
    store.provider_track_key_map = migrated_provider_map;
    store.schema_version = STATS_SCHEMA_VERSION;
}

fn event_metadata_by_path(events: &[ListenEvent]) -> HashMap<String, (Option<String>, String)> {
    let mut by_path: HashMap<String, (i64, Option<String>, String)> = HashMap::new();
    for event in events {
        let path_key = legacy_path_key(&event.track_path);
        let title = event.title.trim();
        if title.is_empty() {
            continue;
        }
        let artist = event
            .artist
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let entry = by_path.entry(path_key).or_insert((
            event.started_at_epoch_seconds,
            None,
            String::new(),
        ));
        if event.started_at_epoch_seconds >= entry.0 {
            *entry = (
                event.started_at_epoch_seconds,
                artist.map(str::to_string),
                title.to_string(),
            );
        }
    }

    by_path
        .into_iter()
        .map(|(path, (_, artist, title))| (path, (artist, title)))
        .collect()
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
            provider_track_id: None,
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
            provider_track_id: None,
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
            provider_track_id: None,
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
            provider_track_id: None,
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
            provider_track_id: None,
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
            provider_track_id: None,
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
            provider_track_id: None,
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
            provider_track_id: None,
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
            provider_track_id: None,
            started_at_epoch_seconds: 0,
            listened_seconds: 30,
            counted_play: true,
        }];

        let trend = build_trend_series(StatsRange::Today, StatsSort::ListenTime, 70, &events);

        assert_eq!(trend.unit, TrendUnit::Minutes);
        assert_eq!(trend.end_epoch_seconds, 60);
    }

    #[test]
    fn minute_trend_distributes_single_long_session_across_buckets() {
        let events = vec![ListenEvent {
            track_path: PathBuf::from("C:/music/A.mp3"),
            title: "A".to_string(),
            artist: None,
            album: None,
            provider_track_id: None,
            started_at_epoch_seconds: 0,
            listened_seconds: 4_740,
            counted_play: true,
        }];

        let trend = build_trend_series(StatsRange::Today, StatsSort::ListenTime, 4_740, &events);

        assert_eq!(trend.unit, TrendUnit::Minutes);
        assert_eq!(trend.buckets.iter().copied().max().unwrap_or(0), 60);
        assert_eq!(trend.buckets.iter().sum::<u64>(), 4_740);
    }

    #[test]
    fn minute_trend_does_not_stack_tail_into_last_bucket_near_six_hours() {
        let mut events = Vec::new();
        for index in 0..20 {
            events.push(ListenEvent {
                track_path: PathBuf::from(format!("C:/music/{index}.mp3")),
                title: format!("{index}"),
                artist: None,
                album: None,
                provider_track_id: None,
                started_at_epoch_seconds: 16_200 + (index as i64) * 180,
                listened_seconds: 180,
                counted_play: true,
            });
        }

        let trend = build_trend_series(StatsRange::Today, StatsSort::ListenTime, 21_000, &events);

        assert_eq!(trend.unit, TrendUnit::Minutes);
        assert!(trend.buckets.iter().copied().max().unwrap_or(0) <= 60);
    }

    #[test]
    fn metadata_key_merges_same_song_across_different_paths() {
        let mut store = StatsStore::default();
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("C:/music/A/song.mp3"),
            title: "Song".to_string(),
            artist: Some("Artist".to_string()),
            album: Some("One".to_string()),
            provider_track_id: None,
            started_at_epoch_seconds: 10,
            listened_seconds: 40,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("D:/backup/song.mp3"),
            title: "Song".to_string(),
            artist: Some("Artist".to_string()),
            album: Some("Two".to_string()),
            provider_track_id: None,
            started_at_epoch_seconds: 20,
            listened_seconds: 45,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });

        let snapshot = store.query(&StatsQuery::default(), 100);
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.total_plays, 2);
    }

    #[test]
    fn featured_artists_are_ignored_in_match_key() {
        let mut store = StatsStore::default();
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("a.mp3"),
            title: "Song".to_string(),
            artist: Some("Artist feat. Guest".to_string()),
            album: None,
            provider_track_id: None,
            started_at_epoch_seconds: 10,
            listened_seconds: 40,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("b.mp3"),
            title: "Song".to_string(),
            artist: Some("Artist".to_string()),
            album: None,
            provider_track_id: None,
            started_at_epoch_seconds: 20,
            listened_seconds: 40,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });

        let snapshot = store.query(&StatsQuery::default(), 100);
        assert_eq!(snapshot.rows.len(), 1);
    }

    #[test]
    fn provider_id_maps_to_metadata_and_pins_future_events() {
        let mut store = StatsStore::default();
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("stream-temp-a.mp3"),
            title: "Song".to_string(),
            artist: Some("Artist".to_string()),
            album: None,
            provider_track_id: Some("provider:123".to_string()),
            started_at_epoch_seconds: 10,
            listened_seconds: 30,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });
        store.record_listen(ListenSessionRecord {
            track_path: PathBuf::from("stream-temp-b.mp3"),
            title: "Different title".to_string(),
            artist: Some("Different artist".to_string()),
            album: None,
            provider_track_id: Some("provider:123".to_string()),
            started_at_epoch_seconds: 20,
            listened_seconds: 30,
            completed: false,
            duration_seconds: Some(180),
            counted_play_override: None,
            allow_short_listen: false,
        });

        let snapshot = store.query(&StatsQuery::default(), 100);
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.total_plays, 2);
    }
}
