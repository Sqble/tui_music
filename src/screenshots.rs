use crate::audio::AudioEngine;
use crate::core::{HeaderSection, LyricsMode, StatsFilterFocus, TuneCore};
use crate::lyrics::{LyricLine, LyricsDocument, LyricsSource, LyricsTimingPrecision};
use crate::model::{PersistedState, Playlist, RepeatMode, Theme, Track};
use crate::online::{OnlineRoomMode, OnlineSession, Participant, QueueDelivery, SharedQueueItem};
use crate::stats::{ListenSessionRecord, StatsQuery, StatsRange, StatsSort, StatsStore};
use crate::ui::{ActionPanelView, OnlineRoomFieldView, OverlayViews};
use anyhow::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_SIZES: [(u16, u16); 2] = [(120, 36), (90, 30)];
const CELL_WIDTH: f32 = 10.0;
const CELL_HEIGHT: f32 = 20.0;
const FONT_SIZE: f32 = 15.0;
const DEFAULT_BG: (u8, u8, u8) = (10, 15, 24);
const DEFAULT_FG: (u8, u8, u8) = (214, 228, 248);

#[derive(Debug, Clone)]
pub struct ScreenshotOptions {
    pub pages: Vec<String>,
    pub sizes: Vec<(u16, u16)>,
    pub font_scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenshotPage {
    Library,
    Lyrics,
    Stats,
    Online,
    Actions,
}

impl ScreenshotPage {
    const ALL: [Self; 5] = [
        Self::Library,
        Self::Lyrics,
        Self::Stats,
        Self::Online,
        Self::Actions,
    ];

    fn slug(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Lyrics => "lyrics",
            Self::Stats => "stats",
            Self::Online => "online",
            Self::Actions => "actions",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Library => "README hero: library, queue, album art, playback controls",
            Self::Lyrics => "Docs lyrics page: synced lyrics and editor workflow",
            Self::Stats => "Docs stats page: listening history and trend graph",
            Self::Online => "Docs online page: room controls, peers, shared queue",
            Self::Actions => "Docs controls page: searchable command palette",
        }
    }
}

pub fn generate_to_executable_dir(options: ScreenshotOptions) -> Result<()> {
    let exe = std::env::current_exe().context("failed to locate current executable")?;
    let output_dir = exe.parent().with_context(|| {
        format!(
            "failed to resolve executable directory for {}",
            exe.display()
        )
    })?;
    generate(output_dir, options)
}

fn generate(output_dir: &Path, options: ScreenshotOptions) -> Result<()> {
    let pages = parse_pages(&options.pages)?;
    let sizes = if options.sizes.is_empty() {
        DEFAULT_SIZES.to_vec()
    } else {
        options.sizes
    };
    if !options.font_scale.is_finite() || options.font_scale <= 0.0 {
        anyhow::bail!("screenshot font scale must be positive");
    }

    let mut manifest = String::from("TuneTUI seeded screenshot outputs\n\n");
    manifest.push_str("Recommended use:\n");
    for page in ScreenshotPage::ALL {
        let _ = writeln!(manifest, "- {}: {}", page.slug(), page.description());
    }
    manifest.push_str("\nGenerated files:\n");

    for size in sizes {
        for page in &pages {
            let svg = render_page_svg(*page, size, options.font_scale)?;
            let scale_label = format!("{:03}", (options.font_scale * 100.0).round() as u16);
            let file_name = format!(
                "tunetui-{}-{}x{}-scale{}.svg",
                page.slug(),
                size.0,
                size.1,
                scale_label
            );
            let path = output_dir.join(&file_name);
            fs::write(&path, svg).with_context(|| format!("failed to write {}", path.display()))?;
            let _ = writeln!(manifest, "- {file_name}");
        }
    }

    let manifest_path = output_dir.join("tunetui-screenshots-manifest.txt");
    fs::write(&manifest_path, manifest)
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;

    Ok(())
}

fn parse_pages(raw_pages: &[String]) -> Result<Vec<ScreenshotPage>> {
    if raw_pages.is_empty()
        || raw_pages
            .iter()
            .any(|page| page.eq_ignore_ascii_case("all"))
    {
        return Ok(ScreenshotPage::ALL.to_vec());
    }

    raw_pages
        .iter()
        .map(|page| match page.trim().to_ascii_lowercase().as_str() {
            "library" => Ok(ScreenshotPage::Library),
            "lyrics" => Ok(ScreenshotPage::Lyrics),
            "stats" => Ok(ScreenshotPage::Stats),
            "online" => Ok(ScreenshotPage::Online),
            "actions" | "action" | "palette" => Ok(ScreenshotPage::Actions),
            other => anyhow::bail!("unknown screenshot page '{other}'"),
        })
        .collect()
}

fn render_page_svg(page: ScreenshotPage, size: (u16, u16), font_scale: f32) -> Result<String> {
    let mut terminal = Terminal::new(TestBackend::new(size.0, size.1))?;
    let core = seeded_core(page);
    let audio = ScreenshotAudio::new(
        active_track_path(),
        Duration::from_secs(96),
        Duration::from_secs(241),
    );
    let stats_store = seeded_stats_store(&core.tracks);
    let stats_snapshot = (page == ScreenshotPage::Stats).then(|| {
        stats_store.query(
            &StatsQuery {
                range: core.stats_range,
                sort: core.stats_sort,
                artist_filter: core.stats_artist_filter.clone(),
                album_filter: core.stats_album_filter.clone(),
                search: core.stats_search.clone(),
            },
            1_725_000_000,
        )
    });
    let action_panel = (page == ScreenshotPage::Actions).then(seeded_action_panel);
    let online_room_field = (page == ScreenshotPage::Online).then(|| OnlineRoomFieldView {
        label: String::from("Room name"),
        value: String::from("docs-listening-lounge"),
        secret: false,
    });

    terminal.draw(|frame| {
        crate::ui::draw(
            frame,
            &core,
            &audio,
            action_panel.as_ref(),
            stats_snapshot.as_ref(),
            OverlayViews {
                join_prompt_modal: None,
                room_directory_view: None,
                online_password_prompt: None,
                host_invite_modal: None,
                online_room_field: online_room_field.as_ref(),
                room_code_revealed: page == ScreenshotPage::Online,
            },
        );
    })?;

    Ok(buffer_to_svg(terminal.backend().buffer(), font_scale))
}

fn seeded_core(page: ScreenshotPage) -> TuneCore {
    let tracks = seeded_tracks();
    let mut playlists = HashMap::new();
    playlists.insert(
        String::from("Night Drive QA"),
        Playlist {
            tracks: vec![
                tracks[2].path.clone(),
                tracks[4].path.clone(),
                tracks[7].path.clone(),
            ],
        },
    );
    playlists.insert(
        String::from("Lossless Test Bench"),
        Playlist {
            tracks: vec![
                tracks[0].path.clone(),
                tracks[5].path.clone(),
                tracks[9].path.clone(),
            ],
        },
    );

    let state = PersistedState {
        folders: vec![PathBuf::from("D:/Music/TuneTUI Demo")],
        playlists,
        shuffle_enabled: true,
        repeat_mode: RepeatMode::All,
        loudness_normalization: true,
        crossfade_seconds: 8,
        scrub_seconds: 15,
        theme: Theme::Galaxy,
        stats_top_songs_count: 8,
        online_nickname: Some(String::from("sqble")),
        ..PersistedState::default()
    };
    let mut core = TuneCore::from_persisted_with_tracks(state, tracks);
    core.current_queue_index = Some(2);
    core.selected_track = 2;
    core.online_sync_correction_threshold_ms = 300;
    core.lyrics_preview_expanded = true;
    core.stats_range = StatsRange::Days30;
    core.stats_sort = StatsSort::ListenTime;
    core.stats_focus = StatsFilterFocus::Sort(1);
    core.status = String::from("Seeded docs screenshots: deterministic demo library");

    for (idx, track) in core.tracks.iter().enumerate() {
        core.cache_duration_seconds_for_path(&track.path, Some(184 + (idx as u32 * 17) % 180));
    }

    seed_lyrics(&mut core);
    seed_online(&mut core);
    set_browser_all_songs(&mut core);

    match page {
        ScreenshotPage::Library => {
            core.header_section = HeaderSection::Library;
            core.selected_browser =
                selected_browser_for_path(&core, &active_track_path()).unwrap_or(3);
        }
        ScreenshotPage::Lyrics => {
            core.header_section = HeaderSection::Lyrics;
            core.lyrics_mode = LyricsMode::Edit;
            core.lyrics_selected_line = 4;
        }
        ScreenshotPage::Stats => {
            core.header_section = HeaderSection::Stats;
            core.stats_artist_filter = String::from("Signal");
        }
        ScreenshotPage::Online => {
            core.header_section = HeaderSection::Online;
            core.status = String::from("Hosting room DOCS42 on tunetui.online");
        }
        ScreenshotPage::Actions => {
            core.header_section = HeaderSection::Library;
            core.selected_browser =
                selected_browser_for_path(&core, &active_track_path()).unwrap_or(3);
            core.status = String::from("Action search: audio quality, themes, queues");
        }
    }

    core
}

fn seeded_tracks() -> Vec<Track> {
    let rows = [
        (
            "D:/Music/TuneTUI Demo/01 Lossless Handshake.flac",
            "Lossless Handshake",
            "Signal Harbor",
            "Terminal Sessions",
        ),
        (
            "D:/Music/TuneTUI Demo/02 Async Moonrise.mp3",
            "Async Moonrise",
            "The Event Loop",
            "Terminal Sessions",
        ),
        (
            "D:/Music/TuneTUI Demo/03 Neon Packet.flac",
            "Neon Packet",
            "Signal Harbor",
            "Night Drive QA",
        ),
        (
            "D:/Music/TuneTUI Demo/04 Cache Warm.wav",
            "Cache Warm",
            "Index Bloom",
            "Warm Starts",
        ),
        (
            "D:/Music/TuneTUI Demo/05 Shared Queue.ogg",
            "Shared Queue",
            "Room Tone",
            "Listen Together",
        ),
        (
            "D:/Music/TuneTUI Demo/06 Drift Calibrator.flac",
            "Drift Calibrator",
            "Clock Sync",
            "Listen Together",
        ),
        (
            "D:/Music/TuneTUI Demo/07 Sidecar Lines.m4a",
            "Sidecar Lines",
            "Lyric Engine",
            "Timestamp Notes",
        ),
        (
            "D:/Music/TuneTUI Demo/08 Replay Gain.aac",
            "Replay Gain",
            "Meter Bridge",
            "Audio Quality",
        ),
        (
            "D:/Music/TuneTUI Demo/09 Spectral Bloom.wav",
            "Spectral Bloom",
            "Meter Bridge",
            "Audio Quality",
        ),
        (
            "D:/Music/TuneTUI Demo/10 SSH Palette.flac",
            "SSH Palette",
            "Terminal Paint",
            "Remote Shell",
        ),
    ];

    rows.into_iter()
        .map(|(path, title, artist, album)| Track {
            path: PathBuf::from(path),
            title: String::from(title),
            artist: Some(String::from(artist)),
            album: Some(String::from(album)),
        })
        .collect()
}

fn active_track_path() -> PathBuf {
    PathBuf::from("D:/Music/TuneTUI Demo/03 Neon Packet.flac")
}

fn set_browser_all_songs(core: &mut TuneCore) {
    core.browser_path = None;
    core.browser_playlist = None;
    core.browser_all_songs = true;
    core.browser_local_queue = false;
    core.browser_shared_queue = false;
    core.refresh_browser_view();
}

fn selected_browser_for_path(core: &TuneCore, path: &Path) -> Option<usize> {
    core.browser_entries
        .iter()
        .position(|entry| paths_equal(&entry.path, path))
}

fn seed_lyrics(core: &mut TuneCore) {
    core.lyrics_track_path = Some(active_track_path());
    core.lyrics = Some(LyricsDocument {
        source: LyricsSource::Sidecar,
        precision: LyricsTimingPrecision::Line,
        lines: vec![
            lyric(0, "Wake the cache, paint the frame"),
            lyric(18_000, "Every packet keeps the room in time"),
            lyric(36_000, "Sidecar words line up with the waveform"),
            lyric(54_000, "Shared queue waits for the downbeat"),
            lyric(72_000, "Neon packets over terminal skies"),
            lyric(96_000, "Lossless when the link can carry it"),
            lyric(114_000, "Balanced when the night gets thin"),
            lyric(132_000, "Press Ctrl+T and stamp the chorus"),
            lyric(150_000, "Save the LRC before the fade"),
        ],
    });
    core.lyrics_selected_line = 5;
}

fn lyric(timestamp_ms: u32, text: &str) -> LyricLine {
    LyricLine {
        timestamp_ms: Some(timestamp_ms),
        text: String::from(text),
    }
}

fn seed_online(core: &mut TuneCore) {
    let mut session = OnlineSession::host("sqble");
    session.room_code = String::from("DOCS42");
    session.mode = OnlineRoomMode::Collaborative;
    session.last_sync_drift_ms = -12;
    session.participants = vec![
        Participant {
            nickname: String::from("sqble"),
            is_local: true,
            is_host: true,
            ping_ms: 0,
            manual_extra_delay_ms: 0,
            auto_ping_delay: true,
        },
        Participant {
            nickname: String::from("mira"),
            is_local: false,
            is_host: false,
            ping_ms: 38,
            manual_extra_delay_ms: 12,
            auto_ping_delay: true,
        },
        Participant {
            nickname: String::from("loopback"),
            is_local: false,
            is_host: false,
            ping_ms: 72,
            manual_extra_delay_ms: 0,
            auto_ping_delay: true,
        },
    ];
    session.shared_queue = VecDeque::from(vec![
        shared_item(
            "D:/Music/TuneTUI Demo/05 Shared Queue.ogg",
            "Shared Queue",
            "mira",
        ),
        shared_item(
            "D:/Music/TuneTUI Demo/06 Drift Calibrator.flac",
            "Drift Calibrator",
            "sqble",
        ),
        shared_item(
            "D:/Music/TuneTUI Demo/10 SSH Palette.flac",
            "SSH Palette",
            "loopback",
        ),
    ]);
    core.online.session = Some(session);
}

fn shared_item(path: &str, title: &str, owner: &str) -> SharedQueueItem {
    SharedQueueItem {
        path: PathBuf::from(path),
        title: String::from(title),
        delivery: QueueDelivery::HostStreamOnly,
        owner_nickname: Some(String::from(owner)),
    }
}

fn seeded_stats_store(tracks: &[Track]) -> StatsStore {
    let mut store = StatsStore::default();
    let now = 1_725_000_000_i64;
    for day in 0..30_i64 {
        for (idx, track) in tracks.iter().enumerate() {
            if (idx as i64 + day).rem_euclid(3) == 0 {
                let listened_seconds = 140 + ((idx as u32 * 37 + day as u32 * 11) % 220);
                store.record_listen(ListenSessionRecord {
                    track_path: track.path.clone(),
                    title: track.title.clone(),
                    artist: track.artist.clone(),
                    album: track.album.clone(),
                    provider_track_id: None,
                    started_at_epoch_seconds: now - day * 86_400 - idx as i64 * 2_400,
                    listened_seconds,
                    completed: listened_seconds > 210,
                    duration_seconds: Some(240),
                    counted_play_override: Some(true),
                    allow_short_listen: true,
                });
            }
        }
    }
    store
}

fn seeded_action_panel() -> ActionPanelView {
    ActionPanelView {
        title: String::from("Actions"),
        hint: String::from("Type to filter. Enter runs the selected action."),
        search_query: Some(String::new()),
        selected: 7,
        options: vec![
            String::from("Recent"),
            String::from("  View audio quality + spectrograph"),
            String::from("  Theme"),
            String::from("Settings"),
            String::from("  Playback settings"),
            String::from("  Audio driver settings"),
            String::from("Queue"),
            String::from("  Move selected queue item to next"),
            String::from("  Remove selected queue item"),
            String::from("Library"),
            String::from("  Edit selected track metadata"),
            String::from("  View audio quality + spectrograph"),
            String::from("  Rescan library"),
            String::from("Appearance"),
            String::from("  Theme"),
            String::from("Stats"),
            String::from("  Clear listen history (backup)"),
            String::from("Lyrics"),
            String::from("  Import TXT to lyrics"),
            String::from("Actions"),
            String::from("  Close panel"),
        ],
    }
}

fn buffer_to_svg(buffer: &Buffer, font_scale: f32) -> String {
    let cell_width = CELL_WIDTH * font_scale;
    let cell_height = CELL_HEIGHT * font_scale;
    let font_size = FONT_SIZE * font_scale;
    let width_px = f32::from(buffer.area.width) * cell_width;
    let height_px = f32::from(buffer.area.height) * cell_height;
    let mut out = String::with_capacity(buffer.content.len().saturating_mul(72));

    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width_px:.0}\" height=\"{height_px:.0}\" viewBox=\"0 0 {width_px:.2} {height_px:.2}\" role=\"img\" aria-label=\"TuneTUI seeded terminal screenshot\">"
    );
    let _ = writeln!(
        out,
        "<rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
        color_to_hex(Color::Reset, true)
    );
    out.push_str("<g shape-rendering=\"crispEdges\">\n");
    for y in 0..buffer.area.height {
        let mut x = 0;
        while x < buffer.area.width {
            let bg = buffer[(x, y)].bg;
            let mut run_width = 1;
            while x + run_width < buffer.area.width && buffer[(x + run_width, y)].bg == bg {
                run_width += 1;
            }
            let _ = writeln!(
                out,
                "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"{}\"/>",
                f32::from(x) * cell_width,
                f32::from(y) * cell_height,
                f32::from(run_width) * cell_width,
                cell_height,
                color_to_hex(bg, true)
            );
            x += run_width;
        }
    }
    out.push_str("</g>\n");
    out.push_str("<g shape-rendering=\"crispEdges\">\n");
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            let symbol = cell.symbol();
            if symbol == " " || cell.modifier.contains(Modifier::HIDDEN) {
                continue;
            }
            let (fg, bg) = if cell.modifier.contains(Modifier::REVERSED) {
                (cell.bg, cell.fg)
            } else {
                (cell.fg, cell.bg)
            };
            let fill = color_to_hex(resolve_visible_fg(fg, bg), false);
            let _ = write_cell_vector(
                &mut out,
                symbol,
                CellMetrics {
                    x: f32::from(x) * cell_width,
                    y: f32::from(y) * cell_height,
                    width: cell_width,
                    height: cell_height,
                    font_scale,
                },
                &fill,
            );
        }
    }
    out.push_str("</g>\n");
    let _ = writeln!(
        out,
        "<g font-family=\"JetBrains Mono, Cascadia Mono, Consolas, monospace\" font-size=\"{font_size:.2}\" dominant-baseline=\"text-before-edge\">"
    );
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            let symbol = cell.symbol();
            if symbol == " " || cell.modifier.contains(Modifier::HIDDEN) {
                continue;
            }
            if can_render_as_vector(symbol) {
                continue;
            }

            let (fg, bg) = if cell.modifier.contains(Modifier::REVERSED) {
                (cell.bg, cell.fg)
            } else {
                (cell.fg, cell.bg)
            };
            let escaped = escape_xml(symbol);
            let weight = if cell.modifier.contains(Modifier::BOLD) {
                " font-weight=\"700\""
            } else {
                ""
            };
            let decoration = if cell.modifier.contains(Modifier::UNDERLINED) {
                " text-decoration=\"underline\""
            } else {
                ""
            };
            let opacity = if cell.modifier.contains(Modifier::DIM) {
                " opacity=\"0.72\""
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "<text x=\"{:.2}\" y=\"{:.2}\" fill=\"{}\"{}{}{}>{}</text>",
                f32::from(x) * cell_width,
                f32::from(y) * cell_height + (cell_height - font_size) * 0.45,
                color_to_hex(resolve_visible_fg(fg, bg), false),
                weight,
                decoration,
                opacity,
                escaped
            );
        }
    }
    out.push_str("</g>\n</svg>\n");
    out
}

fn can_render_as_vector(symbol: &str) -> bool {
    symbol
        .chars()
        .next()
        .is_some_and(|ch| symbol.len() == ch.len_utf8() && vector_glyph_kind(ch).is_some())
}

fn write_cell_vector(out: &mut String, symbol: &str, metrics: CellMetrics, fill: &str) -> bool {
    let Some(ch) = symbol.chars().next() else {
        return false;
    };
    if symbol.len() != ch.len_utf8() {
        return false;
    }

    match vector_glyph_kind(ch) {
        Some(VectorGlyph::FullBlock { opacity }) => {
            write_svg_rect(
                out,
                metrics.x,
                metrics.y,
                metrics.width,
                metrics.height,
                fill,
                opacity,
            );
            true
        }
        Some(VectorGlyph::HorizontalFraction(numer, denom)) => {
            let rect_width = metrics.width * f32::from(numer) / f32::from(denom);
            write_svg_rect(
                out,
                metrics.x,
                metrics.y,
                rect_width,
                metrics.height,
                fill,
                1.0,
            );
            true
        }
        Some(VectorGlyph::VerticalFraction(numer, denom)) => {
            let rect_height = metrics.height * f32::from(numer) / f32::from(denom);
            write_svg_rect(
                out,
                metrics.x,
                metrics.y + metrics.height - rect_height,
                metrics.width,
                rect_height,
                fill,
                1.0,
            );
            true
        }
        Some(VectorGlyph::TopHalf) => {
            write_svg_rect(
                out,
                metrics.x,
                metrics.y,
                metrics.width,
                metrics.height / 2.0,
                fill,
                1.0,
            );
            true
        }
        Some(VectorGlyph::BottomHalf) => {
            write_svg_rect(
                out,
                metrics.x,
                metrics.y + metrics.height / 2.0,
                metrics.width,
                metrics.height / 2.0,
                fill,
                1.0,
            );
            true
        }
        Some(VectorGlyph::LeftHalf) => {
            write_svg_rect(
                out,
                metrics.x,
                metrics.y,
                metrics.width / 2.0,
                metrics.height,
                fill,
                1.0,
            );
            true
        }
        Some(VectorGlyph::RightHalf) => {
            write_svg_rect(
                out,
                metrics.x + metrics.width / 2.0,
                metrics.y,
                metrics.width / 2.0,
                metrics.height,
                fill,
                1.0,
            );
            true
        }
        Some(VectorGlyph::Box {
            up,
            right,
            down,
            left,
        }) => {
            let thickness = (metrics.font_scale * 1.35).max(1.0);
            let cx = metrics.x + metrics.width / 2.0;
            let cy = metrics.y + metrics.height / 2.0;
            let half = thickness / 2.0;
            if left {
                write_svg_rect(
                    out,
                    metrics.x,
                    cy - half,
                    metrics.width / 2.0 + half,
                    thickness,
                    fill,
                    1.0,
                );
            }
            if right {
                write_svg_rect(
                    out,
                    cx - half,
                    cy - half,
                    metrics.width / 2.0 + half,
                    thickness,
                    fill,
                    1.0,
                );
            }
            if up {
                write_svg_rect(
                    out,
                    cx - half,
                    metrics.y,
                    thickness,
                    metrics.height / 2.0 + half,
                    fill,
                    1.0,
                );
            }
            if down {
                write_svg_rect(
                    out,
                    cx - half,
                    cy - half,
                    thickness,
                    metrics.height / 2.0 + half,
                    fill,
                    1.0,
                );
            }
            true
        }
        None => false,
    }
}

#[derive(Clone, Copy)]
struct CellMetrics {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    font_scale: f32,
}

fn write_svg_rect(
    out: &mut String,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    fill: &str,
    opacity: f32,
) {
    let opacity_attr = if opacity < 1.0 {
        format!(" opacity=\"{opacity:.2}\"")
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "<rect x=\"{x:.2}\" y=\"{y:.2}\" width=\"{width:.2}\" height=\"{height:.2}\" fill=\"{fill}\"{opacity_attr}/>"
    );
}

enum VectorGlyph {
    FullBlock {
        opacity: f32,
    },
    HorizontalFraction(u8, u8),
    VerticalFraction(u8, u8),
    TopHalf,
    BottomHalf,
    LeftHalf,
    RightHalf,
    Box {
        up: bool,
        right: bool,
        down: bool,
        left: bool,
    },
}

fn vector_glyph_kind(ch: char) -> Option<VectorGlyph> {
    match ch {
        '█' => Some(VectorGlyph::FullBlock { opacity: 1.0 }),
        '▓' => Some(VectorGlyph::FullBlock { opacity: 0.75 }),
        '▒' => Some(VectorGlyph::FullBlock { opacity: 0.50 }),
        '░' => Some(VectorGlyph::FullBlock { opacity: 0.28 }),
        '▀' => Some(VectorGlyph::TopHalf),
        '▄' => Some(VectorGlyph::BottomHalf),
        '▌' => Some(VectorGlyph::LeftHalf),
        '▐' => Some(VectorGlyph::RightHalf),
        '▏' => Some(VectorGlyph::HorizontalFraction(1, 8)),
        '▎' => Some(VectorGlyph::HorizontalFraction(2, 8)),
        '▍' => Some(VectorGlyph::HorizontalFraction(3, 8)),
        '▋' => Some(VectorGlyph::HorizontalFraction(5, 8)),
        '▊' => Some(VectorGlyph::HorizontalFraction(6, 8)),
        '▉' => Some(VectorGlyph::HorizontalFraction(7, 8)),
        '▁' => Some(VectorGlyph::VerticalFraction(1, 8)),
        '▂' => Some(VectorGlyph::VerticalFraction(2, 8)),
        '▃' => Some(VectorGlyph::VerticalFraction(3, 8)),
        '▅' => Some(VectorGlyph::VerticalFraction(5, 8)),
        '▆' => Some(VectorGlyph::VerticalFraction(6, 8)),
        '▇' => Some(VectorGlyph::VerticalFraction(7, 8)),
        '─' | '━' | '═' => Some(box_glyph(false, true, false, true)),
        '│' | '┃' | '║' => Some(box_glyph(true, false, true, false)),
        '┌' | '┏' | '╔' | '╭' => Some(box_glyph(false, true, true, false)),
        '┐' | '┓' | '╗' | '╮' => Some(box_glyph(false, false, true, true)),
        '└' | '┗' | '╚' | '╰' => Some(box_glyph(true, true, false, false)),
        '┘' | '┛' | '╝' | '╯' => Some(box_glyph(true, false, false, true)),
        '├' | '┣' | '╠' => Some(box_glyph(true, true, true, false)),
        '┤' | '┫' | '╣' => Some(box_glyph(true, false, true, true)),
        '┬' | '┳' | '╦' => Some(box_glyph(false, true, true, true)),
        '┴' | '┻' | '╩' => Some(box_glyph(true, true, false, true)),
        '┼' | '╋' | '╬' => Some(box_glyph(true, true, true, true)),
        _ => None,
    }
}

fn box_glyph(up: bool, right: bool, down: bool, left: bool) -> VectorGlyph {
    VectorGlyph::Box {
        up,
        right,
        down,
        left,
    }
}

fn resolve_visible_fg(fg: Color, bg: Color) -> Color {
    if fg == bg { Color::Reset } else { fg }
}

fn color_to_hex(color: Color, background: bool) -> String {
    let (r, g, b) = match color {
        Color::Reset => {
            if background {
                DEFAULT_BG
            } else {
                DEFAULT_FG
            }
        }
        Color::Black => (0, 0, 0),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 205),
        Color::Gray => (229, 229, 229),
        Color::DarkGray => (102, 102, 102),
        Color::LightRed => (241, 76, 76),
        Color::LightGreen => (35, 209, 139),
        Color::LightYellow => (245, 245, 67),
        Color::LightBlue => (59, 142, 234),
        Color::LightMagenta => (214, 112, 214),
        Color::LightCyan => (41, 184, 219),
        Color::White => (255, 255, 255),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(index) => indexed_color(index),
    };
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn indexed_color(index: u8) -> (u8, u8, u8) {
    const ANSI: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    if index < 16 {
        return ANSI[usize::from(index)];
    }
    if index < 232 {
        let idx = index - 16;
        let r = idx / 36;
        let g = (idx % 36) / 6;
        let b = idx % 6;
        return (cube_level(r), cube_level(g), cube_level(b));
    }
    let value = 8_u8.saturating_add((index - 232).saturating_mul(10));
    (value, value, value)
}

fn cube_level(value: u8) -> u8 {
    if value == 0 { 0 } else { 55 + value * 40 }
}

fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

struct ScreenshotAudio {
    current: PathBuf,
    position: Duration,
    duration: Duration,
    volume: f32,
}

impl ScreenshotAudio {
    fn new(current: PathBuf, position: Duration, duration: Duration) -> Self {
        Self {
            current,
            position,
            duration,
            volume: 0.82,
        }
    }
}

impl AudioEngine for ScreenshotAudio {
    fn play(&mut self, path: &Path) -> Result<()> {
        self.current = path.to_path_buf();
        self.position = Duration::ZERO;
        Ok(())
    }

    fn queue_crossfade(&mut self, path: &Path) -> Result<()> {
        self.play(path)
    }

    fn tick(&mut self) {}

    fn pause(&mut self) {}

    fn resume(&mut self) {}

    fn stop(&mut self) {}

    fn is_paused(&self) -> bool {
        false
    }

    fn current_track(&self) -> Option<&Path> {
        Some(self.current.as_path())
    }

    fn position(&self) -> Option<Duration> {
        Some(self.position)
    }

    fn duration(&self) -> Option<Duration> {
        Some(self.duration)
    }

    fn seek_to(&mut self, position: Duration) -> Result<()> {
        self.position = position.min(self.duration);
        Ok(())
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 2.5);
    }

    fn output_name(&self) -> Option<String> {
        Some(String::from("Seeded screenshot output"))
    }

    fn reload_driver(&mut self) -> Result<()> {
        Ok(())
    }

    fn available_outputs(&self) -> Vec<String> {
        vec![String::from("Seeded screenshot output")]
    }

    fn selected_output_device(&self) -> Option<String> {
        Some(String::from("Seeded screenshot output"))
    }

    fn set_output_device(&mut self, _output: Option<&str>) -> Result<()> {
        Ok(())
    }

    fn loudness_normalization(&self) -> bool {
        true
    }

    fn set_loudness_normalization(&mut self, _enabled: bool) {}

    fn crossfade_seconds(&self) -> u16 {
        8
    }

    fn set_crossfade_seconds(&mut self, _seconds: u16) {}

    fn crossfade_queued_track(&self) -> Option<&Path> {
        None
    }

    fn is_finished(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{ScreenshotPage, parse_pages};

    #[test]
    fn parse_pages_defaults_to_all() {
        assert_eq!(parse_pages(&[]).unwrap().len(), ScreenshotPage::ALL.len());
    }

    #[test]
    fn parse_pages_accepts_aliases() {
        assert_eq!(
            parse_pages(&[String::from("library"), String::from("palette")]).unwrap(),
            vec![ScreenshotPage::Library, ScreenshotPage::Actions]
        );
    }

    #[test]
    fn parse_pages_rejects_unknown_page() {
        assert!(parse_pages(&[String::from("missing")]).is_err());
    }
}
