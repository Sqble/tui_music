use crate::audio::AudioEngine;
use crate::core::BrowserEntryKind;
use crate::core::HeaderSection;
use crate::core::LyricsMode;
use crate::core::StatsFilterFocus;
use crate::core::TuneCore;
use crate::model::{CoverArtTemplate, RepeatMode, Theme};
use crate::online::OnlineSession;
use crate::stats::{ListenEvent, StatsRange, StatsSnapshot, StatsSort, TrendSeries};
use image::imageops::FilterType;
use image::{ImageBuffer, ImageFormat, Rgba};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use time::{OffsetDateTime, UtcOffset};

const APP_TITLE: &str = "TuneTUI";
const APP_VERSION: &str = "v1.0.0-alpha-3";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    Tab(HeaderSection),
    ToggleShuffle,
    CycleRepeat,
    OpenOnline,
    QuickAddSelectedToPlaylist,
    QuickAddNowPlayingToPlaylist,
    QuickAddSelectedToQueueEnd,
    QuickAddSelectedToQueueNext,
    LibrarySearchBar,
    LibraryRow(usize),
    Prev,
    Next,
    ScrubBack,
    ScrubFwd,
    VolumeDown,
    VolumeUp,
    VolumeBar { x: u16, width: u16 },
    TimelineBar { x: u16, width: u16 },
    ActionRow(usize),
    ActionPanelBackground,
    ActionPanelInside,
    // Stats
    StatsRange(usize),
    StatsSort(usize),
    StatsArtistFilter,
    StatsAlbumFilter,
    StatsSearchFilter,
    // Online inline / popup
    JoinPromptInput,
    JoinPromptPrimary,
    JoinPromptPaste,
    RoomDirectorySearch,
    RoomDirectoryRoom(usize),
    PasswordPromptInput,
    PasswordPromptContinue,
    HostInviteCopy,
    HostInviteOk,
    // Online session controls
    ToggleOnlineMode,
    CycleStreamQuality,
    ToggleRoomCodeReveal,
    CopyRoomCode,
}

#[derive(Debug, Default, Clone)]
pub struct HitMap {
    entries: Vec<(Rect, HitTarget)>,
}

impl HitMap {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn push(&mut self, rect: Rect, target: HitTarget) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        self.entries.push((rect, target));
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<HitTarget> {
        // Iterate in reverse so later registrations (e.g. modals/overlays) win
        // over earlier ones drawn underneath.
        self.entries.iter().rev().find_map(|(rect, target)| {
            if x >= rect.x
                && x < rect.x.saturating_add(rect.width)
                && y >= rect.y
                && y < rect.y.saturating_add(rect.height)
            {
                Some(*target)
            } else {
                None
            }
        })
    }

    pub fn entries(&self) -> &[(Rect, HitTarget)] {
        &self.entries
    }
}

static HIT_MAP: OnceLock<Mutex<HitMap>> = OnceLock::new();

fn hit_map_cell() -> &'static Mutex<HitMap> {
    HIT_MAP.get_or_init(|| Mutex::new(HitMap::default()))
}

pub fn take_hit_map() -> HitMap {
    let cell = hit_map_cell();
    let mut guard = cell.lock().expect("hit map mutex poisoned");
    std::mem::take(&mut *guard)
}

fn hit_map_clear() {
    let cell = hit_map_cell();
    let mut guard = cell.lock().expect("hit map mutex poisoned");
    guard.clear();
}

fn hit_map_push(rect: Rect, target: HitTarget) {
    let cell = hit_map_cell();
    let mut guard = cell.lock().expect("hit map mutex poisoned");
    guard.push(rect, target);
}

pub struct ActionPanelView {
    pub title: String,
    pub hint: String,
    pub search_query: Option<String>,
    pub options: Vec<String>,
    pub selected: usize,
}

pub struct HostInviteModalView {
    pub invite_code: String,
    pub copy_selected: bool,
}

pub struct OnlinePasswordPromptView {
    pub title: String,
    pub subtitle: String,
    pub masked_input: String,
    pub continue_selected: bool,
}

pub struct JoinPromptModalView {
    pub invite_code: String,
    pub input_selected: bool,
    pub primary_selected: bool,
    pub paste_selected: bool,
    pub room_name_mode: bool,
    pub nickname_mode: bool,
    pub connect_mode: bool,
}

pub struct OnlineRoomDirectoryModalView {
    pub server_addr: String,
    pub search: String,
    pub search_selected: bool,
    pub selected: usize,
    pub rooms: Vec<String>,
}

pub struct OverlayViews<'a> {
    pub join_prompt_modal: Option<&'a JoinPromptModalView>,
    pub room_directory_view: Option<&'a OnlineRoomDirectoryModalView>,
    pub online_password_prompt: Option<&'a OnlinePasswordPromptView>,
    pub host_invite_modal: Option<&'a HostInviteModalView>,
    pub room_code_revealed: bool,
}

#[derive(Clone, Copy)]
struct ThemePalette {
    bg: Color,
    panel_bg: Color,
    content_panel_bg: Color,
    content_panel_alt_bg: Color,
    border: Color,
    text: Color,
    muted: Color,
    accent: Color,
    alert: Color,
    playlist: Color,
    all_songs: Color,
    selected_bg: Color,
    popup_bg: Color,
    popup_selected_bg: Color,
}

fn palette(theme: Theme) -> ThemePalette {
    match theme {
        Theme::Dark => ThemePalette {
            bg: Color::Rgb(10, 15, 24),
            panel_bg: Color::Rgb(29, 47, 72),
            content_panel_bg: Color::Rgb(19, 29, 43),
            content_panel_alt_bg: Color::Rgb(22, 36, 56),
            border: Color::Rgb(69, 121, 176),
            text: Color::Rgb(214, 228, 248),
            muted: Color::Rgb(149, 173, 204),
            accent: Color::Rgb(100, 203, 184),
            alert: Color::Rgb(249, 174, 88),
            playlist: Color::Rgb(156, 186, 255),
            all_songs: Color::Rgb(214, 205, 133),
            selected_bg: Color::Rgb(34, 55, 82),
            popup_bg: Color::Rgb(22, 33, 51),
            popup_selected_bg: Color::Rgb(45, 70, 99),
        },
        Theme::System => ThemePalette {
            bg: Color::Reset,
            panel_bg: Color::Reset,
            content_panel_bg: Color::Reset,
            content_panel_alt_bg: Color::Reset,
            border: Color::Blue,
            text: Color::Reset,
            muted: Color::DarkGray,
            accent: Color::Cyan,
            alert: Color::Yellow,
            playlist: Color::Blue,
            all_songs: Color::Yellow,
            selected_bg: Color::DarkGray,
            popup_bg: Color::Reset,
            popup_selected_bg: Color::DarkGray,
        },
        Theme::PitchBlack => ThemePalette {
            bg: Color::Rgb(0, 0, 0),
            panel_bg: Color::Rgb(20, 20, 20),
            content_panel_bg: Color::Rgb(8, 8, 8),
            content_panel_alt_bg: Color::Rgb(13, 13, 13),
            border: Color::Rgb(74, 74, 74),
            text: Color::Rgb(242, 242, 242),
            muted: Color::Rgb(150, 150, 150),
            accent: Color::Rgb(212, 212, 212),
            alert: Color::Rgb(235, 176, 97),
            playlist: Color::Rgb(178, 195, 220),
            all_songs: Color::Rgb(222, 206, 135),
            selected_bg: Color::Rgb(26, 26, 26),
            popup_bg: Color::Rgb(10, 10, 10),
            popup_selected_bg: Color::Rgb(34, 34, 34),
        },
        Theme::Galaxy => ThemePalette {
            bg: Color::Rgb(7, 8, 23),
            panel_bg: Color::Rgb(36, 33, 82),
            content_panel_bg: Color::Rgb(18, 16, 44),
            content_panel_alt_bg: Color::Rgb(26, 24, 63),
            border: Color::Rgb(108, 107, 205),
            text: Color::Rgb(227, 225, 252),
            muted: Color::Rgb(167, 165, 210),
            accent: Color::Rgb(141, 204, 255),
            alert: Color::Rgb(255, 189, 121),
            playlist: Color::Rgb(188, 164, 255),
            all_songs: Color::Rgb(237, 215, 145),
            selected_bg: Color::Rgb(40, 37, 86),
            popup_bg: Color::Rgb(23, 21, 56),
            popup_selected_bg: Color::Rgb(58, 55, 110),
        },
        Theme::Matrix => ThemePalette {
            bg: Color::Rgb(4, 12, 4),
            panel_bg: Color::Rgb(16, 40, 16),
            content_panel_bg: Color::Rgb(8, 22, 8),
            content_panel_alt_bg: Color::Rgb(11, 28, 11),
            border: Color::Rgb(39, 143, 62),
            text: Color::Rgb(180, 255, 185),
            muted: Color::Rgb(102, 177, 115),
            accent: Color::Rgb(95, 255, 122),
            alert: Color::Rgb(219, 234, 114),
            playlist: Color::Rgb(142, 244, 152),
            all_songs: Color::Rgb(192, 227, 131),
            selected_bg: Color::Rgb(18, 43, 20),
            popup_bg: Color::Rgb(10, 26, 11),
            popup_selected_bg: Color::Rgb(24, 57, 26),
        },
        Theme::Demonic => ThemePalette {
            bg: Color::Rgb(16, 2, 2),
            panel_bg: Color::Rgb(56, 13, 14),
            content_panel_bg: Color::Rgb(30, 6, 7),
            content_panel_alt_bg: Color::Rgb(41, 9, 10),
            border: Color::Rgb(176, 38, 38),
            text: Color::Rgb(245, 214, 214),
            muted: Color::Rgb(188, 133, 133),
            accent: Color::Rgb(255, 92, 92),
            alert: Color::Rgb(255, 171, 83),
            playlist: Color::Rgb(255, 135, 135),
            all_songs: Color::Rgb(255, 207, 123),
            selected_bg: Color::Rgb(72, 17, 19),
            popup_bg: Color::Rgb(36, 8, 9),
            popup_selected_bg: Color::Rgb(88, 20, 22),
        },
        Theme::CottonCandy => ThemePalette {
            bg: Color::Rgb(34, 21, 44),
            panel_bg: Color::Rgb(80, 46, 105),
            content_panel_bg: Color::Rgb(51, 29, 68),
            content_panel_alt_bg: Color::Rgb(61, 35, 81),
            border: Color::Rgb(245, 146, 208),
            text: Color::Rgb(255, 233, 250),
            muted: Color::Rgb(224, 173, 219),
            accent: Color::Rgb(124, 225, 255),
            alert: Color::Rgb(255, 199, 150),
            playlist: Color::Rgb(172, 202, 255),
            all_songs: Color::Rgb(255, 227, 176),
            selected_bg: Color::Rgb(90, 49, 114),
            popup_bg: Color::Rgb(60, 34, 80),
            popup_selected_bg: Color::Rgb(110, 61, 139),
        },
        Theme::Ocean => palette(Theme::Dark),
        Theme::Forest => palette(Theme::Matrix),
        Theme::Sunset => palette(Theme::CottonCandy),
    }
}

pub fn library_rect(area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(66), Constraint::Percentage(34)])
        .split(vertical[1]);

    body[0]
}

pub fn draw(
    frame: &mut Frame,
    core: &TuneCore,
    audio: &dyn AudioEngine,
    action_panel: Option<&ActionPanelView>,
    stats_snapshot: Option<&StatsSnapshot>,
    overlays: OverlayViews<'_>,
) {
    hit_map_clear();
    let colors = palette(core.theme);
    frame.render_widget(
        Block::default().style(Style::default().bg(colors.bg)),
        frame.area(),
    );

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(frame.area());

    frame.render_widget(status_panel_block(core, &colors), vertical[0]);
    register_status_pill_hits(vertical[0], core);

    let header_inner = vertical[0].inner(Margin {
        vertical: 0,
        horizontal: 1,
    });
    let tabs_width = header_tabs_width().min(header_inner.width.saturating_sub(1));
    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(tabs_width)])
        .split(header_inner);

    let header_left = Paragraph::new(Line::from(vec![
        Span::styled(
            APP_TITLE,
            Style::default()
                .fg(colors.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default().fg(colors.muted)),
        Span::styled(APP_VERSION, Style::default().fg(colors.muted)),
    ]));
    frame.render_widget(header_left, header_chunks[0]);

    let header_right =
        Paragraph::new(header_tabs_line(core.header_section, &colors)).alignment(Alignment::Right);
    frame.render_widget(header_right, header_chunks[1]);
    register_header_tab_hits(header_chunks[1]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(66), Constraint::Percentage(34)])
        .split(vertical[1]);

    frame.render_widget(Clear, body[0]);
    frame.render_widget(Clear, body[1]);

    if core.header_section == HeaderSection::Library {
        let list_items: Vec<ListItem> = core
            .browser_entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let marker = if core.is_browser_entry_playing(i) {
                    "  > "
                } else {
                    "    "
                };
                let kind_style = match entry.kind {
                    BrowserEntryKind::Back => Style::default().fg(colors.alert),
                    BrowserEntryKind::AddDirectory | BrowserEntryKind::CreatePlaylist => {
                        Style::default()
                            .fg(colors.accent)
                            .add_modifier(Modifier::BOLD)
                    }
                    BrowserEntryKind::Folder => Style::default().fg(colors.accent),
                    BrowserEntryKind::Playlist => Style::default().fg(colors.playlist),
                    BrowserEntryKind::AllSongs => Style::default().fg(colors.all_songs),
                    BrowserEntryKind::QueueLocal | BrowserEntryKind::QueueShared => {
                        Style::default().fg(colors.accent)
                    }
                    BrowserEntryKind::Track => Style::default().fg(colors.text),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(marker, Style::default().fg(colors.muted)),
                    Span::styled(entry.label.as_str(), kind_style),
                ]))
            })
            .collect();

        let library_title = if !core.library_search_query.is_empty() {
            String::from("Library / Search")
        } else if let Some(name) = &core.browser_playlist {
            format!("Library / Playlist / {name}")
        } else if core.browser_all_songs {
            String::from("Library / All Songs")
        } else if core.browser_local_queue {
            String::from("Library / Local Queue")
        } else if core.browser_shared_queue {
            String::from("Library / Shared Queue")
        } else if let Some(path) = &core.browser_path {
            format!("Library / {}", path.display())
        } else {
            String::from("Library")
        };

        let block = panel_block(
            &library_title,
            colors.content_panel_bg,
            colors.text,
            colors.border,
        );
        frame.render_widget(block, body[0]);

        let library_inner = body[0].inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(library_inner);

        let search_text = if core.library_search_query.is_empty() {
            String::from("Search")
        } else {
            format!("Search: {}", core.library_search_query)
        };
        let search_style = if core.library_search_focused {
            Style::default()
                .fg(colors.text)
                .bg(colors.selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(colors.muted)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(search_text, search_style))),
            chunks[0],
        );
        hit_map_push(chunks[0], HitTarget::LibrarySearchBar);

        let list_area = chunks[1];
        let mut state = ListState::default();
        if !core.browser_entries.is_empty() && !core.library_search_focused {
            state.select(Some(core.selected_browser));
        }

        let list = List::new(list_items)
            .highlight_style(
                Style::default()
                    .bg(colors.selected_bg)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("-> ");
        frame.render_stateful_widget(list, list_area, &mut state);

        let visible_rows = usize::from(list_area.height);
        let offset = state.offset();
        for visible_idx in 0..visible_rows {
            let entry_idx = offset + visible_idx;
            if entry_idx >= core.browser_entries.len() {
                break;
            }
            hit_map_push(
                Rect {
                    x: list_area.x,
                    y: list_area.y + visible_idx as u16,
                    width: list_area.width,
                    height: 1,
                },
                HitTarget::LibraryRow(entry_idx),
            );
        }

        let library_viewport_lines = usize::from(list_area.height);
        let total_library_rows = core.browser_entries.len();
        if library_viewport_lines > 0 && list_overflows(total_library_rows, library_viewport_lines)
        {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_style(Style::default().fg(colors.border))
                .thumb_style(Style::default().fg(colors.accent));
            let mut scrollbar_state = ScrollbarState::new(total_library_rows)
                .position(state.offset())
                .viewport_content_length(library_viewport_lines);
            frame.render_stateful_widget(scrollbar, body[0], &mut scrollbar_state);
        }

        let now_playing = audio.current_track().or_else(|| core.current_path());
        let now_playing_title = now_playing
            .and_then(|path| core.title_for_path(path))
            .unwrap_or_else(|| "-".to_string());
        let now_playing_artist = now_playing
            .and_then(|path| core.artist_for_path(path))
            .unwrap_or("-");
        let now_playing_album = now_playing
            .and_then(|path| core.album_for_path(path))
            .unwrap_or("-");

        let selected_path = core
            .browser_entries
            .get(core.selected_browser)
            .filter(|entry| entry.kind == BrowserEntryKind::Track)
            .map(|entry| entry.path.clone());
        let selected_title = selected_path
            .as_ref()
            .and_then(|path| core.title_for_path(path))
            .unwrap_or_else(|| "-".to_string());
        let selected_artist = selected_path
            .as_ref()
            .and_then(|path| core.artist_for_path(path))
            .unwrap_or("-");
        let selected_album = selected_path
            .as_ref()
            .and_then(|path| core.album_for_path(path))
            .unwrap_or("-");
        let selected_length = selected_path
            .as_ref()
            .and_then(|path| core.cached_duration_seconds_for_path(path))
            .map(|seconds| format_duration(Duration::from_secs(u64::from(seconds))))
            .unwrap_or_else(|| String::from("--:--"));

        let queue_position = now_playing
            .and_then(|path| core.queue_position_for_path(path))
            .map(|idx| format!("{}/{}", idx + 1, core.queue.len()))
            .unwrap_or_else(|| format!("-/{}", core.queue.len()));

        let info_text = vec![
            Line::from(vec![
                Span::styled(
                    "Now",
                    Style::default()
                        .fg(colors.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", now_playing_title),
                    Style::default().fg(colors.text),
                ),
            ]),
            Line::from(Span::styled(
                format!("Artist  {now_playing_artist}"),
                Style::default().fg(colors.muted),
            )),
            Line::from(Span::styled(
                format!("Album   {now_playing_album}"),
                Style::default().fg(colors.muted),
            )),
            Line::from(Span::styled(
                format!("Queue   {queue_position}"),
                Style::default().fg(colors.alert),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "Selected",
                    Style::default()
                        .fg(colors.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {selected_title}"),
                    Style::default().fg(colors.text),
                ),
            ]),
            Line::from(Span::styled(
                format!("Artist  {selected_artist}"),
                Style::default().fg(colors.muted),
            )),
            Line::from(Span::styled(
                format!("Album   {selected_album}"),
                Style::default().fg(colors.muted),
            )),
            Line::from(Span::styled(
                format!("Length  {selected_length}"),
                Style::default().fg(colors.muted),
            )),
        ];

        frame.render_widget(
            panel_block(
                "Song Info",
                colors.content_panel_alt_bg,
                colors.text,
                colors.border,
            ),
            body[1],
        );

        let info_inner = body[1].inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        if info_inner.width > 0 && info_inner.height > 0 {
            let details_height = info_inner.height.min(9);
            let cover_height = info_inner.height.saturating_sub(details_height);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(cover_height),
                    Constraint::Length(details_height),
                ])
                .split(info_inner);

            if chunks[0].height > 0 {
                let cover_lines = now_playing
                    .and_then(|path| {
                        cover_art_lines_for_path(path, core, chunks[0].width, chunks[0].height)
                    })
                    .unwrap_or_else(|| cover_placeholder_lines(chunks[0].width, chunks[0].height));
                frame.render_widget(
                    Paragraph::new(cover_lines).style(Style::default().fg(colors.muted)),
                    chunks[0],
                );
            }

            if chunks[1].height > 0 {
                frame.render_widget(
                    Paragraph::new(info_text).wrap(Wrap { trim: true }),
                    chunks[1],
                );
            }
        }
    } else {
        match core.header_section {
            HeaderSection::Library => {}
            HeaderSection::Stats => {
                draw_stats_section(frame, &body, colors, core, stats_snapshot);
            }
            HeaderSection::Lyrics => {
                draw_lyrics_section(frame, &body, colors, core, audio);
            }
            HeaderSection::Online => {
                draw_online_section(frame, &body, colors, core, &overlays);
            }
        }
    }

    draw_timeline_panel(frame, vertical[2], core, audio, &colors);

    let control_block = Paragraph::new(control_line(audio, 16, &colors))
        .block(panel_block(
            "Control",
            colors.panel_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(control_block, vertical[3]);
    register_control_line_hits(vertical[3], 16);

    let selection_block = Paragraph::new(selection_actions_line(&colors))
        .block(panel_block(
            "Selection",
            colors.panel_bg,
            colors.text,
            colors.border,
        ))
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: true });
    frame.render_widget(selection_block, vertical[4]);
    register_selection_action_hits(vertical[4]);

    let key_hint = if core.header_section == HeaderSection::Stats {
        "Keys: Left/Right Focus, Enter Cycle, Type filters, Backspace Edit, Shift+Up Top"
    } else if core.header_section == HeaderSection::Lyrics {
        "Keys: Ctrl+E Edit/view, Up/Down Line, Enter New line, Ctrl+T Timestamp, / Actions"
    } else if core.header_section == HeaderSection::Online {
        "Keys: Enter Select/join, Ctrl+N Shared now, Ctrl+L Leave room"
    } else {
        "Keys: Enter Play, Backspace Back, Ctrl+F Search, / Actions, T Tray, Ctrl+C Quit"
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(key_hint, Style::default().fg(colors.muted)),
        Span::styled("  |  ", Style::default().fg(colors.muted)),
        Span::styled(core.status.as_str(), Style::default().fg(colors.text)),
    ]))
    .block(panel_block(
        "Message",
        colors.panel_bg,
        colors.text,
        colors.border,
    ));
    frame.render_widget(footer, vertical[5]);

    if let Some(panel) = action_panel {
        draw_action_panel(frame, panel, &colors);
    }
    if let Some(join_prompt_modal) = overlays.join_prompt_modal
        && !join_prompt_modal.connect_mode
        && !join_prompt_modal.room_name_mode
    {
        draw_join_prompt(frame, join_prompt_modal, &colors);
    }
    if let Some(host_invite_modal) = overlays.host_invite_modal {
        draw_host_invite_modal(frame, host_invite_modal, &colors);
    }
}

fn draw_room_directory_inline(
    frame: &mut Frame,
    horizontal: &[Rect],
    colors: ThemePalette,
    dir: &OnlineRoomDirectoryModalView,
) {
    let mut left_lines = vec![
        Line::from(Span::styled(
            "Room Directory",
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("Server: {}", dir.server_addr),
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            "Esc - Leave home server",
            Style::default().fg(colors.accent),
        )),
        Line::from(Span::styled(
            format!("Search: {}", dir.search),
            if dir.search_selected {
                Style::default()
                    .fg(colors.text)
                    .bg(colors.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(colors.accent)
            },
        )),
        Line::from(""),
    ];

    let max_rooms = horizontal[0].height.saturating_sub(8) as usize;
    for (index, room_line) in dir.rooms.iter().enumerate().take(max_rooms) {
        let style = if index == dir.selected && !dir.search_selected {
            Style::default()
                .fg(colors.text)
                .bg(colors.selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(colors.text)
        };
        left_lines.push(room_directory_line(room_line, style, &colors));
    }

    if dir.rooms.is_empty() {
        left_lines.push(Line::from(Span::styled(
            "(no rooms)",
            Style::default().fg(colors.muted),
        )));
    }

    let left = Paragraph::new(left_lines)
        .block(panel_block(
            "Online",
            colors.content_panel_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(left, horizontal[0]);

    let inner = horizontal[0].inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    // Search line is at inner.y + 3.
    hit_map_push(
        Rect {
            x: inner.x,
            y: inner.y + 3,
            width: inner.width,
            height: 1,
        },
        HitTarget::RoomDirectorySearch,
    );
    // Room lines start at inner.y + 5.
    for (index, _) in dir.rooms.iter().enumerate().take(max_rooms) {
        hit_map_push(
            Rect {
                x: inner.x,
                y: inner.y + 5 + index as u16,
                width: inner.width,
                height: 1,
            },
            HitTarget::RoomDirectoryRoom(index),
        );
    }

    let right_lines = vec![
        Line::from(Span::styled(
            "Controls",
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "[+] Create Room",
            Style::default().fg(colors.accent),
        )),
        Line::from(Span::styled(
            "    Start a new room for others to join",
            Style::default().fg(colors.muted),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "[open] Unlocked room",
            Style::default().fg(colors.text),
        )),
        Line::from(Span::styled(
            "[lock] Password protected",
            Style::default().fg(colors.alert),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Up/Down - Select search/rooms",
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            "Enter - Join/Create",
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            "Esc - Leave home server",
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            "Tab/Left/Right - Toggle search/list",
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            "Type - Search only when search selected",
            Style::default().fg(colors.muted),
        )),
    ];
    let right = Paragraph::new(right_lines)
        .block(panel_block(
            "Online",
            colors.content_panel_alt_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(right, horizontal[1]);
}

fn draw_online_password_prompt_inline(
    frame: &mut Frame,
    horizontal: &[Rect],
    colors: ThemePalette,
    prompt: &OnlinePasswordPromptView,
) {
    let masked = if prompt.masked_input.is_empty() {
        String::from("(empty)")
    } else {
        prompt.masked_input.clone()
    };
    let input_style = if prompt.continue_selected {
        Style::default().fg(colors.text)
    } else {
        Style::default()
            .fg(colors.text)
            .bg(colors.selected_bg)
            .add_modifier(Modifier::BOLD)
    };
    let continue_style = if prompt.continue_selected {
        Style::default()
            .fg(colors.text)
            .bg(colors.selected_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(colors.muted)
    };
    let left_lines = vec![
        Line::from(Span::styled(
            prompt.title.as_str(),
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        )),
        if prompt.title == "Set Room Password" {
            Line::from(Span::styled(
                "Esc - Back to room directory",
                Style::default().fg(colors.accent),
            ))
        } else {
            Line::from("")
        },
        Line::from(""),
        Line::from(Span::styled(
            prompt.subtitle.as_str(),
            Style::default().fg(colors.muted),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Password: ", Style::default().fg(colors.muted)),
            Span::styled(masked, input_style),
        ]),
        Line::from(""),
        Line::from(Span::styled("[ Continue ]", continue_style)),
    ];
    let left = Paragraph::new(left_lines)
        .block(panel_block(
            "Online",
            colors.content_panel_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(left, horizontal[0]);

    let inner = horizontal[0].inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    // Password line is at inner.y + 5.
    hit_map_push(
        Rect {
            x: inner.x,
            y: inner.y + 5,
            width: inner.width,
            height: 1,
        },
        HitTarget::PasswordPromptInput,
    );
    // Continue button is at inner.y + 7.
    let continue_w = "[ Continue ]".len() as u16;
    hit_map_push(
        Rect {
            x: inner.x,
            y: inner.y + 7,
            width: continue_w,
            height: 1,
        },
        HitTarget::PasswordPromptContinue,
    );

    let right_lines = vec![
        Line::from(Span::styled(
            "Help",
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            if prompt.title == "Set Room Password" {
                "Enter continues. Esc goes back to room directory."
            } else {
                "Enter continues. Esc cancels password entry."
            },
            Style::default().fg(colors.accent),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Tab/Up/Down - Toggle focus",
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            "Enter - Continue",
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            if prompt.title == "Set Room Password" {
                "Esc - Back to room directory"
            } else {
                "Esc - Cancel"
            },
            Style::default().fg(colors.muted),
        )),
    ];
    let right = Paragraph::new(right_lines)
        .block(panel_block(
            "Info",
            colors.content_panel_alt_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(right, horizontal[1]);
}

fn join_prompt_title(modal: &JoinPromptModalView) -> &'static str {
    if modal.nickname_mode {
        "Set Online Nickname"
    } else if modal.room_name_mode {
        "Host Online Room"
    } else {
        "Connect to Homeserver"
    }
}

fn join_prompt_input_label(modal: &JoinPromptModalView) -> &'static str {
    if modal.nickname_mode {
        "Nickname"
    } else if modal.room_name_mode {
        "Room name"
    } else {
        "Server / Link"
    }
}

fn join_prompt_help_line(modal: &JoinPromptModalView) -> &'static str {
    if modal.nickname_mode {
        "Pick a nickname for online rooms. Enter saves and continues."
    } else if modal.room_name_mode {
        "Type room name. Enter continues. Esc goes back to room directory."
    } else {
        "Show public servers, or select Server / Link to type a homeserver or room link."
    }
}

fn join_prompt_primary_label(modal: &JoinPromptModalView) -> &'static str {
    if modal.connect_mode {
        "[ Show Public Servers ]"
    } else {
        "[ Continue ]"
    }
}

fn draw_join_prompt_inline(
    frame: &mut Frame,
    horizontal: &[Rect],
    colors: ThemePalette,
    modal: &JoinPromptModalView,
) {
    let left_lines = vec![
        Line::from(Span::styled(
            join_prompt_title(modal),
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        )),
        if modal.room_name_mode {
            Line::from(Span::styled(
                "Esc - Back to room directory",
                Style::default().fg(colors.accent),
            ))
        } else {
            Line::from("")
        },
        Line::from(""),
        Line::from(vec![
            Span::styled(
                join_prompt_input_label(modal),
                if modal.input_selected {
                    Style::default()
                        .fg(colors.text)
                        .bg(colors.selected_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(colors.muted)
                },
            ),
            Span::styled(
                ": ",
                if modal.input_selected {
                    Style::default().fg(colors.text).bg(colors.selected_bg)
                } else {
                    Style::default().fg(colors.muted)
                },
            ),
            Span::styled(
                modal.invite_code.as_str(),
                if modal.input_selected {
                    Style::default().fg(colors.text).bg(colors.selected_bg)
                } else {
                    Style::default().fg(colors.text)
                },
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                join_prompt_primary_label(modal),
                if modal.primary_selected {
                    Style::default()
                        .fg(colors.text)
                        .bg(colors.selected_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(colors.muted)
                },
            ),
            Span::raw("   "),
            Span::styled(
                "[ Paste clipboard ]",
                if modal.paste_selected {
                    Style::default()
                        .fg(colors.text)
                        .bg(colors.selected_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(colors.muted)
                },
            ),
        ]),
    ];
    let left = Paragraph::new(left_lines)
        .block(panel_block(
            "Online",
            colors.content_panel_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(left, horizontal[0]);

    let inner = horizontal[0].inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    // Input line is at inner.y + 3.
    hit_map_push(
        Rect {
            x: inner.x,
            y: inner.y + 3,
            width: inner.width,
            height: 1,
        },
        HitTarget::JoinPromptInput,
    );
    // Buttons on inner.y + 5.
    let primary_label = join_prompt_primary_label(modal);
    let primary_w = primary_label.len() as u16;
    hit_map_push(
        Rect {
            x: inner.x,
            y: inner.y + 5,
            width: primary_w,
            height: 1,
        },
        HitTarget::JoinPromptPrimary,
    );
    let paste_w = "[ Paste clipboard ]".len() as u16;
    let paste_x = inner.x + primary_w + 3;
    hit_map_push(
        Rect {
            x: paste_x,
            y: inner.y + 5,
            width: paste_w,
            height: 1,
        },
        HitTarget::JoinPromptPaste,
    );

    let right_lines = vec![
        Line::from(Span::styled(
            "Help",
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            join_prompt_help_line(modal),
            Style::default().fg(colors.accent),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Enter - Activate selected",
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            "Left/Right/Up/Down/Tab - Select field/button",
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            "Type - Edit selected text field",
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            "Ctrl+V - Paste clipboard",
            Style::default().fg(colors.muted),
        )),
    ];
    let right = Paragraph::new(right_lines)
        .block(panel_block(
            "Info",
            colors.content_panel_alt_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(right, horizontal[1]);
}

fn room_directory_line(room_line: &str, base_style: Style, colors: &ThemePalette) -> Line<'static> {
    if let Some(rest) = room_line.strip_prefix("[lock]") {
        return Line::from(vec![
            Span::styled("[lock]", base_style.fg(colors.alert)),
            Span::styled(rest.to_string(), base_style),
        ]);
    }
    if let Some(rest) = room_line.strip_prefix("[open]") {
        return Line::from(vec![
            Span::styled("[open]", base_style.fg(colors.text)),
            Span::styled(rest.to_string(), base_style),
        ]);
    }
    if let Some(rest) = room_line.strip_prefix("[+]") {
        return Line::from(vec![
            Span::styled("[+]", base_style.fg(colors.accent)),
            Span::styled(rest.to_string(), base_style),
        ]);
    }
    Line::from(Span::styled(room_line.to_string(), base_style))
}

fn draw_join_prompt(frame: &mut Frame, modal: &JoinPromptModalView, colors: &ThemePalette) {
    let popup = centered_rect(frame.area(), 68, 34);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        panel_block(
            join_prompt_title(modal),
            colors.popup_bg,
            colors.text,
            colors.border,
        ),
        popup,
    );

    let inner = popup.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let lines = vec![
        Line::from(vec![
            Span::styled(
                join_prompt_input_label(modal),
                Style::default().fg(colors.muted),
            ),
            Span::styled(": ", Style::default().fg(colors.muted)),
            Span::styled(modal.invite_code.as_str(), Style::default().fg(colors.text)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[ Continue ]",
                if modal.paste_selected {
                    Style::default().fg(colors.muted)
                } else {
                    Style::default()
                        .fg(colors.text)
                        .bg(colors.popup_selected_bg)
                        .add_modifier(Modifier::BOLD)
                },
            ),
            Span::raw("   "),
            Span::styled(
                "[ Paste clipboard ]",
                if modal.paste_selected {
                    Style::default()
                        .fg(colors.text)
                        .bg(colors.popup_selected_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(colors.muted)
                },
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            join_prompt_help_line(modal),
            Style::default()
                .fg(colors.accent)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);

    // Input line at inner.y + 0.
    hit_map_push(
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
        HitTarget::JoinPromptInput,
    );
    // Buttons at inner.y + 2.
    let primary_w = "[ Continue ]".len() as u16;
    hit_map_push(
        Rect {
            x: inner.x,
            y: inner.y + 2,
            width: primary_w,
            height: 1,
        },
        HitTarget::JoinPromptPrimary,
    );
    let paste_w = "[ Paste clipboard ]".len() as u16;
    let paste_x = inner.x + primary_w + 3;
    hit_map_push(
        Rect {
            x: paste_x,
            y: inner.y + 2,
            width: paste_w,
            height: 1,
        },
        HitTarget::JoinPromptPaste,
    );
}

fn draw_host_invite_modal(frame: &mut Frame, modal: &HostInviteModalView, colors: &ThemePalette) {
    let popup = centered_rect(frame.area(), 54, 36);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        panel_block("Room Ready", colors.popup_bg, colors.text, colors.border),
        popup,
    );

    let inner = popup.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let copy_style = if modal.copy_selected {
        Style::default()
            .fg(colors.text)
            .bg(colors.popup_selected_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(colors.muted)
    };
    let ok_style = if modal.copy_selected {
        Style::default().fg(colors.muted)
    } else {
        Style::default()
            .fg(colors.text)
            .bg(colors.popup_selected_bg)
            .add_modifier(Modifier::BOLD)
    };

    let lines = vec![
        Line::from(Span::styled(
            "Share this invite code",
            Style::default().fg(colors.muted),
        )),
        Line::from(""),
        Line::from(Span::styled(
            modal.invite_code.as_str(),
            Style::default()
                .fg(colors.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled("[ Copy to clipboard ]", copy_style)),
        Line::from(""),
        Line::from(Span::styled("[ OK ]", ok_style)),
        Line::from(""),
        Line::from(Span::styled(
            "Use Up/Down or Tab. Enter activates selected button.",
            Style::default().fg(colors.muted),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        inner,
    );

    // Copy button at inner.y + 4.
    hit_map_push(
        Rect {
            x: inner.x,
            y: inner.y + 4,
            width: inner.width,
            height: 1,
        },
        HitTarget::HostInviteCopy,
    );
    // OK button at inner.y + 6.
    hit_map_push(
        Rect {
            x: inner.x,
            y: inner.y + 6,
            width: inner.width,
            height: 1,
        },
        HitTarget::HostInviteOk,
    );
}

fn header_tabs_line(selected: HeaderSection, colors: &ThemePalette) -> Line<'static> {
    let mut spans = Vec::new();

    for (idx, section) in [
        HeaderSection::Library,
        HeaderSection::Lyrics,
        HeaderSection::Stats,
        HeaderSection::Online,
    ]
    .into_iter()
    .enumerate()
    {
        if idx > 0 {
            spans.push(Span::styled(" -- ", Style::default().fg(colors.muted)));
        }

        let tab_color = match section {
            HeaderSection::Library => Color::Rgb(190, 164, 255),
            HeaderSection::Lyrics => Color::Rgb(139, 220, 255),
            HeaderSection::Stats => Color::Rgb(255, 204, 128),
            HeaderSection::Online => Color::Rgb(134, 255, 190),
        };
        let mut style = Style::default().fg(tab_color);
        if section == selected {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        spans.push(Span::styled(
            format!(
                "{} {}",
                section.shortcut().to_ascii_uppercase(),
                section.label()
            ),
            style,
        ));
    }

    Line::from(spans)
}

fn header_tabs_width() -> u16 {
    let labels = [
        HeaderSection::Library,
        HeaderSection::Lyrics,
        HeaderSection::Stats,
        HeaderSection::Online,
    ];
    let labels_len: usize = labels
        .iter()
        .map(|section| section.label().len() + section.shortcut().len_utf8() + 1)
        .sum();
    let separators_len = " -- ".len() * labels.len().saturating_sub(1);
    (labels_len + separators_len) as u16
}

fn register_header_tab_hits(area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let total = header_tabs_width();
    if total > area.width {
        return;
    }
    // Right-aligned: tabs start at area.x + (area.width - total).
    let mut x = area.x + (area.width - total);
    let sections = [
        HeaderSection::Library,
        HeaderSection::Lyrics,
        HeaderSection::Stats,
        HeaderSection::Online,
    ];
    for (idx, section) in sections.into_iter().enumerate() {
        if idx > 0 {
            // " -- " separator (4 cells) is not clickable.
            x = x.saturating_add(4);
        }
        let label_len = (section.label().len() + section.shortcut().len_utf8() + 1) as u16;
        let rect = Rect {
            x,
            y: area.y,
            width: label_len,
            height: 1,
        };
        hit_map_push(rect, HitTarget::Tab(section));
        x = x.saturating_add(label_len);
    }
}

fn draw_placeholder_section(
    frame: &mut Frame,
    body: &[Rect],
    colors: ThemePalette,
    title: &str,
    message: &str,
) {
    frame.render_widget(
        Paragraph::new(Span::styled(message, Style::default().fg(colors.muted)))
            .block(panel_block(
                title,
                colors.content_panel_bg,
                colors.text,
                colors.border,
            ))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        body[0],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Use / to open actions and configure features.",
            Style::default().fg(colors.muted),
        ))
        .block(panel_block(
            "Info",
            colors.content_panel_alt_bg,
            colors.text,
            colors.border,
        )),
        body[1],
    );
}

fn draw_online_section(
    frame: &mut Frame,
    body: &[Rect],
    colors: ThemePalette,
    core: &TuneCore,
    overlays: &OverlayViews<'_>,
) {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(Rect {
            x: body[0].x,
            y: body[0].y,
            width: body[0].width.saturating_add(body[1].width),
            height: body[0].height.max(body[1].height),
        });

    let Some(session) = core.online.session.as_ref() else {
        if let Some(prompt) = overlays.online_password_prompt {
            draw_online_password_prompt_inline(frame, &horizontal, colors, prompt);
        } else if let Some(dir) = overlays.room_directory_view {
            draw_room_directory_inline(frame, &horizontal, colors, dir);
        } else if let Some(prompt) = overlays
            .join_prompt_modal
            .filter(|prompt| prompt.connect_mode || prompt.room_name_mode)
        {
            draw_join_prompt_inline(frame, &horizontal, colors, prompt);
        } else {
            draw_placeholder_section(frame, body, colors, "Online", "No room connected.");
        }
        return;
    };

    let code_display = if overlays.room_code_revealed {
        session.room_code.clone()
    } else {
        String::from("[hidden]")
    };
    let code_width = code_display.chars().count() as u16;

    let mode_bg = Color::Rgb(95, 71, 138);
    let quality_bg = Color::Rgb(43, 94, 122);
    let toggle_bg = Color::Rgb(105, 76, 37);
    let copy_bg = Color::Rgb(37, 105, 75);

    let mode_badge = format!(" O Mode: {} ", session.mode.label());
    let quality_badge = format!(" Q Stream Quality: {} ", session.quality.label());
    let toggle_badge = if overlays.room_code_revealed {
        " T Hide ".to_string()
    } else {
        " T Show ".to_string()
    };
    let copy_badge = " 2 Copy ".to_string();

    let mut left_lines = vec![Line::from(vec![
        Span::styled(
            mode_badge.clone(),
            Style::default().fg(colors.text).bg(mode_bg),
        ),
        Span::styled("  ", Style::default().fg(colors.muted)),
        Span::styled(
            quality_badge.clone(),
            Style::default().fg(colors.text).bg(quality_bg),
        ),
    ])];

    left_lines.push(Line::from(vec![
        Span::styled("Room code ", Style::default().fg(colors.muted)),
        Span::styled(
            code_display,
            Style::default()
                .fg(colors.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default().fg(colors.muted)),
        Span::styled(
            toggle_badge.clone(),
            Style::default().fg(colors.text).bg(toggle_bg),
        ),
        Span::styled("  ", Style::default().fg(colors.muted)),
        Span::styled(
            copy_badge.clone(),
            Style::default().fg(colors.text).bg(copy_bg),
        ),
    ]));

    left_lines.push(Line::from(Span::styled(
        format!(
            "Peers {}  Shared queue {}  Drift {}ms",
            session.participants.len(),
            session.shared_queue.len(),
            session.last_sync_drift_ms
        ),
        Style::default().fg(colors.muted),
    )));

    left_lines.push(Line::from(""));
    left_lines.push(Line::from(Span::styled(
        "Participants",
        Style::default()
            .fg(colors.text)
            .add_modifier(Modifier::BOLD),
    )));
    for participant in &session.participants {
        left_lines.push(Line::from(Span::styled(
            participant_line(participant, session),
            Style::default().fg(colors.text),
        )));
    }

    let left = Paragraph::new(left_lines)
        .block(panel_block(
            "Online Session",
            colors.content_panel_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(left, horizontal[0]);

    // Register mouse hit targets for online session controls.
    let inner_x = horizontal[0].x.saturating_add(1);
    let inner_y = horizontal[0].y.saturating_add(1);
    let inner_width = horizontal[0].width.saturating_sub(2);

    let mode_badge_width = mode_badge.chars().count() as u16;
    let sep_width = 5u16; // "  |  "
    let quality_badge_width = quality_badge.chars().count() as u16;
    let line0_total = mode_badge_width + sep_width + quality_badge_width;
    if line0_total <= inner_width {
        let mut x = inner_x;
        hit_map_push(
            Rect {
                x,
                y: inner_y,
                width: mode_badge_width,
                height: 1,
            },
            HitTarget::ToggleOnlineMode,
        );
        x = x.saturating_add(mode_badge_width);
        x = x.saturating_add(sep_width);
        hit_map_push(
            Rect {
                x,
                y: inner_y,
                width: quality_badge_width,
                height: 1,
            },
            HitTarget::CycleStreamQuality,
        );
    }

    let label_width = 10u16; // "Room code "
    let toggle_width = toggle_badge.chars().count() as u16;
    let copy_width = copy_badge.chars().count() as u16;
    let line1_total = label_width
        + code_width
        + 2 // "  "
        + toggle_width
        + 2 // "  "
        + copy_width;
    if line1_total <= inner_width {
        let mut x = inner_x;
        x = x.saturating_add(label_width);
        hit_map_push(
            Rect {
                x,
                y: inner_y.saturating_add(1),
                width: code_width,
                height: 1,
            },
            HitTarget::ToggleRoomCodeReveal,
        );
        x = x.saturating_add(code_width);
        x = x.saturating_add(2);
        hit_map_push(
            Rect {
                x,
                y: inner_y.saturating_add(1),
                width: toggle_width,
                height: 1,
            },
            HitTarget::ToggleRoomCodeReveal,
        );
        x = x.saturating_add(toggle_width);
        x = x.saturating_add(2);
        hit_map_push(
            Rect {
                x,
                y: inner_y.saturating_add(1),
                width: copy_width,
                height: 1,
            },
            HitTarget::CopyRoomCode,
        );
    }

    let mut right_lines = Vec::new();
    if let Some(now_playing_line) = online_now_playing_line(session) {
        right_lines.push(Line::from(Span::styled(
            "Now Playing",
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        )));
        right_lines.push(Line::from(Span::styled(
            now_playing_line,
            Style::default().fg(colors.muted),
        )));
        right_lines.push(Line::from(""));
    }

    if let Some(waiting_message) = shared_queue_waiting_message(session) {
        right_lines.push(Line::from(Span::styled(
            waiting_message,
            Style::default().fg(colors.alert),
        )));
        right_lines.push(Line::from(Span::styled(
            "Shared queue starts when current song ends.",
            Style::default().fg(colors.muted),
        )));
        right_lines.push(Line::from(""));
    }

    right_lines.push(Line::from(Span::styled(
        "Shared Queue",
        Style::default()
            .fg(colors.text)
            .add_modifier(Modifier::BOLD),
    )));
    for (index, item) in session.shared_queue.iter().rev().take(10).enumerate() {
        let owner_suffix = item
            .owner_nickname
            .as_deref()
            .filter(|owner| !owner.is_empty())
            .map(|owner| format!(" @{}", truncate_for_line(owner, 12)))
            .unwrap_or_default();
        right_lines.push(Line::from(Span::styled(
            format!(
                "{:>2}. {}{} [{}]",
                index + 1,
                truncate_for_line(&item.title, 28),
                owner_suffix,
                item.delivery.label()
            ),
            Style::default().fg(colors.muted),
        )));
    }
    if session.shared_queue.is_empty() {
        right_lines.push(Line::from(Span::styled(
            "Queue empty. Press Ctrl+s in Library to add selected.",
            Style::default().fg(colors.muted),
        )));
    }
    right_lines.push(Line::from(Span::styled(
        "Ctrl+n: play shared now / next shared",
        Style::default().fg(colors.muted),
    )));
    right_lines.push(Line::from(""));
    right_lines.push(Line::from(Span::styled(
        "Networking",
        Style::default()
            .fg(colors.text)
            .add_modifier(Modifier::BOLD),
    )));
    right_lines.push(Line::from(Span::styled(
        "Direct TCP peer session active (host/client with room code handshake).",
        Style::default().fg(colors.muted),
    )));
    right_lines.push(Line::from(Span::styled(
        "Stream fallback works both directions over the existing room socket.",
        Style::default().fg(colors.muted),
    )));

    let right = Paragraph::new(right_lines)
        .block(panel_block(
            "Room Data",
            colors.content_panel_alt_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(right, horizontal[1]);
}

fn participant_line(participant: &crate::online::Participant, session: &OnlineSession) -> String {
    let mut parts = Vec::with_capacity(5);
    if participant.is_local {
        parts.push(String::from("you"));
    }
    if participant.is_host {
        parts.push(String::from("host"));
    }
    if session.mode == crate::online::OnlineRoomMode::HostOnly && !participant.is_host {
        parts.push(String::from("listen-only"));
    }
    let tags = if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    };
    format!(
        "- {}{}  ping {}ms",
        participant.nickname, tags, participant.ping_ms
    )
}

fn shared_queue_waiting_message(session: &OnlineSession) -> Option<String> {
    let next_shared_path = session
        .shared_queue
        .front()
        .map(|item| item.path.as_path())?;
    let last_transport = session.last_transport.as_ref()?;
    let current_path = match &last_transport.command {
        crate::online::TransportCommand::PlayTrack { path, .. }
        | crate::online::TransportCommand::SetPlaybackState { path, .. } => path.as_path(),
        crate::online::TransportCommand::StopPlayback
        | crate::online::TransportCommand::SetPaused { .. } => return None,
    };

    if current_path == next_shared_path {
        return None;
    }

    Some(format!(
        "Now playing @{} local queue.",
        truncate_for_line(&last_transport.origin_nickname, 14)
    ))
}

fn online_now_playing_line(session: &OnlineSession) -> Option<String> {
    let last_transport = session.last_transport.as_ref()?;
    let path = match &last_transport.command {
        crate::online::TransportCommand::PlayTrack { path, .. }
        | crate::online::TransportCommand::SetPlaybackState { path, .. } => path,
        crate::online::TransportCommand::StopPlayback
        | crate::online::TransportCommand::SetPaused { .. } => return None,
    };
    let track_label = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(|name| truncate_for_line(name, 28))
        .unwrap_or_else(|| truncate_for_line(&path.display().to_string(), 28));
    Some(format!(
        "{} @{}",
        track_label,
        truncate_for_line(&last_transport.origin_nickname, 14)
    ))
}

fn draw_lyrics_section(
    frame: &mut Frame,
    body: &[Rect],
    colors: ThemePalette,
    core: &TuneCore,
    audio: &dyn AudioEngine,
) {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(Rect {
            x: body[0].x,
            y: body[0].y,
            width: body[0].width.saturating_add(body[1].width),
            height: body[0].height.max(body[1].height),
        });

    let Some(doc) = core.lyrics.as_ref() else {
        let message = if core.lyrics_missing_prompt {
            "No lyrics found for this track. Enter creates an empty .lrc, Backspace skips."
        } else {
            "No lyrics loaded. Play a track, import TXT via /, or create a sidecar in this tab."
        };
        draw_placeholder_section(frame, body, colors, "Lyrics", message);
        return;
    };

    let focused = core
        .lyrics_selected_line
        .min(doc.lines.len().saturating_sub(1));
    let mut playback_lines = Vec::new();
    for idx in 0..doc.lines.len() {
        let line = &doc.lines[idx];
        let mut style = Style::default().fg(colors.muted);
        if idx == focused {
            style = Style::default()
                .fg(colors.accent)
                .add_modifier(Modifier::BOLD);
        }

        let stamp = line
            .timestamp_ms
            .map(format_lrc_time)
            .unwrap_or_else(|| "[--:--.--]".to_string());
        playback_lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", if idx == focused { ">" } else { " " }),
                Style::default().fg(colors.muted),
            ),
            Span::styled(stamp, Style::default().fg(colors.alert)),
            Span::styled(" ", Style::default().fg(colors.muted)),
            Span::styled(line.text.as_str(), style),
        ]));
    }

    let left_viewport_height = horizontal[0].height.saturating_sub(2) as usize;
    let left_scroll_top = centered_scroll_top(focused, left_viewport_height);

    let left = Paragraph::new(playback_lines)
        .block(panel_block(
            "Lyrics Playback",
            colors.content_panel_bg,
            colors.text,
            colors.border,
        ))
        .scroll((left_scroll_top, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(left, horizontal[0]);

    let mut right_lines = vec![Line::from(Span::styled(
        format!(
            "Mode {}  Source {:?}  Timing {:?}",
            match core.lyrics_mode {
                LyricsMode::View => "View",
                LyricsMode::Edit => "Edit",
            },
            doc.source,
            doc.precision
        ),
        Style::default().fg(colors.muted),
    ))];
    right_lines.push(Line::from(""));

    match core.lyrics_mode {
        LyricsMode::View => {
            right_lines.push(Line::from(Span::styled(
                "Press Ctrl+e to edit. Scroll follows line changes only.",
                Style::default().fg(colors.text),
            )));
            right_lines.push(Line::from(Span::styled(
                "Use / for TXT import.",
                Style::default().fg(colors.muted),
            )));
            if let Some(position) = audio.position() {
                right_lines.push(Line::from(""));
                right_lines.push(Line::from(Span::styled(
                    format!("Playhead {}", format_duration(position)),
                    Style::default().fg(colors.alert),
                )));
            }
        }
        LyricsMode::Edit => {
            right_lines.push(Line::from(Span::styled(
                "Editor",
                Style::default()
                    .fg(colors.text)
                    .add_modifier(Modifier::BOLD),
            )));
            right_lines.push(Line::from(Span::styled(
                "Type text, Enter new line, Backspace delete char, Delete remove line, Ctrl+t stamp",
                Style::default().fg(colors.muted),
            )));
            right_lines.push(Line::from(""));

            for idx in 0..doc.lines.len() {
                let line = &doc.lines[idx];
                let stamp = line
                    .timestamp_ms
                    .map(format_lrc_time)
                    .unwrap_or_else(|| "[--:--.--]".to_string());
                let style = if idx == focused {
                    Style::default()
                        .fg(colors.accent)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    Style::default().fg(colors.text)
                };
                right_lines.push(Line::from(Span::styled(
                    format!("{:>3} {} {}", idx + 1, stamp, line.text),
                    style,
                )));
            }

            let right_viewport_height = horizontal[1].height.saturating_sub(2) as usize;
            let scroll_top = editor_scroll_top(focused, right_viewport_height, 5);

            let right = Paragraph::new(right_lines)
                .block(panel_block(
                    "Lyrics Editor",
                    colors.content_panel_alt_bg,
                    colors.text,
                    colors.border,
                ))
                .scroll((scroll_top, 0))
                .wrap(Wrap { trim: true });
            frame.render_widget(right, horizontal[1]);
            return;
        }
    }

    let right = Paragraph::new(right_lines)
        .block(panel_block(
            "Lyrics Editor",
            colors.content_panel_alt_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(right, horizontal[1]);
}

fn centered_scroll_top(focused: usize, viewport_height: usize) -> u16 {
    let top = focused.saturating_sub(viewport_height.saturating_div(2));
    top.min(u16::MAX as usize) as u16
}

fn editor_scroll_top(focused: usize, viewport_height: usize, header_lines: usize) -> u16 {
    let top =
        header_lines.saturating_add(focused.saturating_sub(viewport_height.saturating_div(2)));
    top.min(u16::MAX as usize) as u16
}

fn draw_stats_section(
    frame: &mut Frame,
    body: &[Rect],
    colors: ThemePalette,
    core: &TuneCore,
    stats_snapshot: Option<&StatsSnapshot>,
) {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(Rect {
            x: body[0].x,
            y: body[0].y,
            width: body[0].width.saturating_add(body[1].width),
            height: body[0].height.max(body[1].height),
        });

    let Some(snapshot) = stats_snapshot else {
        draw_placeholder_section(frame, body, colors, "Stats", "Stats loading...");
        return;
    };

    let now = crate::stats::now_epoch_seconds();

    let mut left_lines = vec![Line::from(vec![
        Span::styled("Range ", Style::default().fg(colors.muted)),
        stats_choice_box(
            "All",
            core.stats_range == StatsRange::Lifetime,
            matches!(core.stats_focus, StatsFilterFocus::Range(0)),
            &colors,
        ),
        Span::raw(" "),
        stats_choice_box(
            "Today",
            core.stats_range == StatsRange::Today,
            matches!(core.stats_focus, StatsFilterFocus::Range(1)),
            &colors,
        ),
        Span::raw(" "),
        stats_choice_box(
            "7d",
            core.stats_range == StatsRange::Days7,
            matches!(core.stats_focus, StatsFilterFocus::Range(2)),
            &colors,
        ),
        Span::raw(" "),
        stats_choice_box(
            "30d",
            core.stats_range == StatsRange::Days30,
            matches!(core.stats_focus, StatsFilterFocus::Range(3)),
            &colors,
        ),
    ])];

    left_lines.push(Line::from(vec![
        Span::styled("Sort  ", Style::default().fg(colors.muted)),
        stats_choice_box(
            "Listen",
            core.stats_sort == StatsSort::ListenTime,
            matches!(core.stats_focus, StatsFilterFocus::Sort(0)),
            &colors,
        ),
        Span::raw(" "),
        stats_choice_box(
            "Plays",
            core.stats_sort == StatsSort::Plays,
            matches!(core.stats_focus, StatsFilterFocus::Sort(1)),
            &colors,
        ),
    ]));

    left_lines.push(Line::from(vec![
        stats_text_box(
            "Artist",
            &core.stats_artist_filter,
            matches!(core.stats_focus, StatsFilterFocus::Artist),
            &colors,
        ),
        Span::raw("  "),
        stats_text_box(
            "Album",
            &core.stats_album_filter,
            matches!(core.stats_focus, StatsFilterFocus::Album),
            &colors,
        ),
        Span::raw("  "),
        stats_text_box(
            "Search",
            &core.stats_search,
            matches!(core.stats_focus, StatsFilterFocus::Search),
            &colors,
        ),
    ]));

    left_lines.push(Line::from(Span::styled(
        format!(
            "Total plays {}  Total listen {}",
            snapshot.total_plays,
            format_seconds(snapshot.total_listen_seconds)
        ),
        Style::default()
            .fg(colors.accent)
            .add_modifier(Modifier::BOLD),
    )));
    left_lines.push(Line::from(""));

    left_lines.push(Line::from(Span::styled(
        format!("Trend by {}", snapshot.trend.unit.label()),
        Style::default()
            .fg(colors.text)
            .add_modifier(Modifier::BOLD),
    )));
    let graph_width = horizontal[0].width.saturating_sub(10).clamp(16, 48) as usize;
    for line in render_square_trend_graph(&snapshot.trend, core.stats_sort, graph_width, 10) {
        left_lines.push(Line::from(Span::styled(
            line,
            Style::default().fg(colors.text),
        )));
    }
    left_lines.push(Line::from(""));

    let metric_label = match core.stats_sort {
        StatsSort::Plays => "plays",
        StatsSort::ListenTime => "listen",
    };
    left_lines.push(Line::from(Span::styled(
        format!("Top songs by {metric_label}"),
        Style::default()
            .fg(colors.text)
            .add_modifier(Modifier::BOLD),
    )));

    let top_songs_limit = usize::from(core.stats_top_songs_count.max(1));
    for (index, row) in snapshot.rows.iter().take(top_songs_limit).enumerate() {
        let value = match core.stats_sort {
            StatsSort::Plays => row.play_count,
            StatsSort::ListenTime => row.listen_seconds,
        };
        let top_value = snapshot
            .rows
            .first()
            .map(|first| match core.stats_sort {
                StatsSort::Plays => first.play_count,
                StatsSort::ListenTime => first.listen_seconds,
            })
            .unwrap_or(0)
            .max(1);
        let title = truncate_for_line(&row.title, 22);
        let bar = unicode_bar(value, top_value, 14);
        let details = format!("{}P {}", row.play_count, format_seconds(row.listen_seconds));
        left_lines.push(Line::from(Span::styled(
            format!("{:>2}. {:<22} {} {}", index + 1, title, bar, details),
            Style::default().fg(colors.text),
        )));
    }

    if snapshot.rows.is_empty() {
        left_lines.push(Line::from(Span::styled(
            "No stats for current filters.",
            Style::default().fg(colors.muted),
        )));
    }

    let total_left_lines = left_lines.len();
    let left = Paragraph::new(left_lines)
        .block(panel_block(
            "Stats",
            colors.content_panel_bg,
            colors.text,
            colors.border,
        ))
        .scroll((core.stats_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(left, horizontal[0]);

    // Register mouse hit targets for stats filters.
    let stats_inner = horizontal[0].inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let scroll = core.stats_scroll as usize;
    let inner_y = stats_inner.y;
    let inner_x = stats_inner.x;

    // Line 0: Range filters.
    let line0 = 0usize;
    if line0 >= scroll {
        let y = inner_y + (line0 - scroll) as u16;
        let mut x = inner_x + 6; // "Range "
        for (idx, label) in [(0, "All"), (1, "Today"), (2, "7d"), (3, "30d")] {
            let w = stats_choice_width(label) as u16;
            hit_map_push(
                Rect {
                    x,
                    y,
                    width: w,
                    height: 1,
                },
                HitTarget::StatsRange(idx),
            );
            x = x.saturating_add(w + 1);
        }
    }

    // Line 1: Sort filters.
    let line1 = 1usize;
    if line1 >= scroll {
        let y = inner_y + (line1 - scroll) as u16;
        let mut x = inner_x + 6; // "Sort  "
        for (idx, label) in [(0, "Listen"), (1, "Plays")] {
            let w = stats_choice_width(label) as u16;
            hit_map_push(
                Rect {
                    x,
                    y,
                    width: w,
                    height: 1,
                },
                HitTarget::StatsSort(idx),
            );
            x = x.saturating_add(w + 1);
        }
    }

    // Line 2: Text filters.
    let line2 = 2usize;
    if line2 >= scroll {
        let y = inner_y + (line2 - scroll) as u16;
        let mut x = inner_x;
        let w = stats_text_box_width("Artist", &core.stats_artist_filter) as u16;
        hit_map_push(
            Rect {
                x,
                y,
                width: w,
                height: 1,
            },
            HitTarget::StatsArtistFilter,
        );
        x = x.saturating_add(w + 2); // "  "
        let w = stats_text_box_width("Album", &core.stats_album_filter) as u16;
        hit_map_push(
            Rect {
                x,
                y,
                width: w,
                height: 1,
            },
            HitTarget::StatsAlbumFilter,
        );
        x = x.saturating_add(w + 2);
        let w = stats_text_box_width("Search", &core.stats_search) as u16;
        hit_map_push(
            Rect {
                x,
                y,
                width: w,
                height: 1,
            },
            HitTarget::StatsSearchFilter,
        );
    }

    let stats_inner = horizontal[0].inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let stats_viewport_lines = usize::from(stats_inner.height);
    if stats_viewport_lines > 0 && list_overflows(total_left_lines, stats_viewport_lines) {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(Style::default().fg(colors.border))
            .thumb_style(Style::default().fg(colors.accent));
        let mut scrollbar_state = ScrollbarState::new(total_left_lines)
            .position(usize::from(core.stats_scroll))
            .viewport_content_length(stats_viewport_lines);
        frame.render_stateful_widget(scrollbar, horizontal[0], &mut scrollbar_state);
    }

    let mut recent_lines = vec![Line::from(Span::styled(
        "Recent plays",
        Style::default()
            .fg(colors.text)
            .add_modifier(Modifier::BOLD),
    ))];
    let recent_inner = horizontal[1].inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let max_recent_rows = usize::from(recent_inner.height.saturating_sub(1));
    for event in snapshot.recent.iter().take(max_recent_rows) {
        recent_lines.push(Line::from(Span::styled(
            format_recent_event(event, now),
            Style::default().fg(colors.muted),
        )));
    }
    if snapshot.recent.is_empty() && max_recent_rows > 0 {
        recent_lines.push(Line::from(Span::styled(
            "No recent listens yet.",
            Style::default().fg(colors.muted),
        )));
    }

    let right = Paragraph::new(recent_lines)
        .block(panel_block(
            "Recent Log",
            colors.content_panel_alt_bg,
            colors.text,
            colors.border,
        ))
        .wrap(Wrap { trim: true });
    frame.render_widget(right, horizontal[1]);
}

fn stats_choice_width(label: &str) -> usize {
    label.len() + 2
}

fn stats_text_box_width(label: &str, value: &str) -> usize {
    if value.is_empty() {
        label.len() + 5
    } else {
        label.len() + 4 + value.len()
    }
}

fn stats_choice_box<'a>(
    label: &'a str,
    selected: bool,
    focused: bool,
    colors: &ThemePalette,
) -> Span<'a> {
    let text = if selected {
        format!("[{label}]")
    } else {
        format!(" {label} ")
    };
    let mut style = if selected {
        Style::default()
            .fg(colors.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(colors.muted)
    };
    if focused {
        style = style
            .bg(colors.selected_bg)
            .add_modifier(Modifier::UNDERLINED);
    }
    Span::styled(text, style)
}

fn stats_text_box(label: &str, value: &str, focused: bool, colors: &ThemePalette) -> Span<'static> {
    let text = if value.is_empty() {
        format!("{label}: [ ]")
    } else {
        format!("{label}: [{value}]")
    };
    let mut style = if value.is_empty() {
        Style::default().fg(colors.muted)
    } else {
        Style::default().fg(colors.text)
    };
    if focused {
        style = style
            .bg(colors.selected_bg)
            .add_modifier(Modifier::UNDERLINED);
    }
    Span::styled(text, style)
}

fn unicode_bar(value: u64, max_value: u64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let filled = (((value as f64) / (max_value.max(1) as f64)) * width as f64).round() as usize;
    let mut out = String::with_capacity(width + 2);
    out.push('[');
    out.push_str(&"█".repeat(filled.min(width)));
    out.push_str(&" ".repeat(width.saturating_sub(filled.min(width))));
    out.push(']');
    out
}

fn render_square_trend_graph(
    trend: &TrendSeries,
    sort: StatsSort,
    width: usize,
    height: usize,
) -> Vec<String> {
    if width < 8 || height < 4 {
        return vec![String::from("(graph unavailable)")];
    }

    let series = plot_samples(&trend.buckets, width);
    let max_value = series.iter().copied().max().unwrap_or(1).max(1);

    let mut grid = vec![vec![' '; width]; height];
    let points: Vec<(usize, usize)> = series
        .iter()
        .enumerate()
        .map(|(x, value)| {
            let max_height = height.saturating_sub(1) as u64;
            let scaled = if *value == 0 {
                0
            } else {
                value
                    .saturating_mul(max_height)
                    .saturating_add(max_value.saturating_sub(1))
                    / max_value
            }
            .max(u64::from(*value > 0));
            let y = height
                .saturating_sub(1)
                .saturating_sub((scaled as usize).min(height.saturating_sub(1)));
            (x, y.min(height.saturating_sub(1)))
        })
        .collect();

    for window in points.windows(2) {
        if let [start, end] = window {
            draw_block_line(&mut grid, *start, *end);
        }
    }
    if let Some((x, y)) = points.last().copied() {
        grid[y][x] = '█';
    }

    let label_width = (0..height)
        .step_by(2)
        .map(|row_index| {
            let row_value = ((height.saturating_sub(1).saturating_sub(row_index)) as f64
                / (height.saturating_sub(1).max(1) as f64)
                * (max_value as f64)) as u64;
            short_metric_label(row_value, sort).len()
        })
        .max()
        .unwrap_or(4);
    let mut lines = Vec::with_capacity(height + 4);
    lines.push(format!(
        "{} +{}+",
        " ".repeat(label_width),
        "-".repeat(width)
    ));
    for (row_index, row) in grid.iter().enumerate() {
        let row_value = ((height.saturating_sub(1).saturating_sub(row_index)) as f64
            / (height.saturating_sub(1).max(1) as f64)
            * (max_value as f64)) as u64;
        let label = if row_index % 2 == 0 {
            short_metric_label(row_value, sort)
        } else {
            String::new()
        };
        lines.push(format!(
            "{:>label_width$} |{}|",
            label,
            row.iter().collect::<String>(),
            label_width = label_width
        ));
    }
    lines.push(format!(
        "{} +{}+",
        " ".repeat(label_width),
        "-".repeat(width)
    ));
    lines.push(format!(
        "{:>label_width$}  {}",
        "",
        trend_axis_labels(trend, width),
        label_width = label_width
    ));
    vec![format!(
        "span {} -> {}  max {}",
        trend_span_label_at(trend, trend.start_epoch_seconds),
        trend_span_label_at(trend, trend.end_epoch_seconds),
        format_seconds(max_value)
    )]
    .into_iter()
    .fold(lines, |mut acc, line| {
        acc.push(line);
        acc
    })
}

fn draw_block_line(grid: &mut [Vec<char>], start: (usize, usize), end: (usize, usize)) {
    let (x0, y0) = (start.0 as i32, start.1 as i32);
    let (x1, y1) = (end.0 as i32, end.1 as i32);
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut x = x0;
    let mut y = y0;

    loop {
        if y >= 0 && (y as usize) < grid.len() && x >= 0 && (x as usize) < grid[y as usize].len() {
            grid[y as usize][x as usize] = '█';
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

fn plot_samples(input: &[u64], width: usize) -> Vec<u64> {
    if input.is_empty() {
        return vec![0; width.max(2)];
    }
    if width <= 1 {
        return vec![input[0]];
    }
    if input.len() <= width {
        return (0..width)
            .map(|index| {
                let pos = (index as f64) * ((input.len().saturating_sub(1)) as f64)
                    / ((width.saturating_sub(1)) as f64);
                input[pos.round() as usize]
            })
            .collect();
    }

    (0..width)
        .map(|index| {
            let start = (index * input.len()) / width;
            let mut end = ((index + 1) * input.len()) / width;
            if end <= start {
                end = (start + 1).min(input.len());
            }
            input[start..end].iter().copied().max().unwrap_or(0)
        })
        .collect()
}

fn trend_axis_labels(trend: &TrendSeries, width: usize) -> String {
    let mut chars = vec![' '; width];
    let positions: Vec<usize> = if width >= 32 {
        vec![
            0usize,
            width / 4,
            width / 2,
            (width * 3) / 4,
            width.saturating_sub(1),
        ]
    } else {
        vec![0usize, width / 2, width.saturating_sub(1)]
    };
    let span = trend
        .end_epoch_seconds
        .saturating_sub(trend.start_epoch_seconds)
        .max(1);
    for position in positions {
        let ratio = (position as f64) / (width.saturating_sub(1).max(1) as f64);
        let offset_seconds = (ratio * (span as f64)).round() as i64;
        let epoch = trend.start_epoch_seconds.saturating_add(offset_seconds);
        let label = trend_label_at(trend, epoch);
        stamp_label(&mut chars, position, &label);
    }
    chars.iter().collect()
}

fn trend_label_at(trend: &TrendSeries, epoch_seconds: i64) -> String {
    if trend.show_clock_time_labels {
        let span = trend
            .end_epoch_seconds
            .saturating_sub(trend.start_epoch_seconds)
            .max(1);
        format_clock_label_local(epoch_seconds, span, trend.unit)
    } else {
        format_offset_label(
            epoch_seconds.saturating_sub(trend.start_epoch_seconds),
            trend.unit,
        )
    }
}

fn trend_span_label_at(trend: &TrendSeries, epoch_seconds: i64) -> String {
    if trend.show_clock_time_labels {
        format_clock_span_label_local(epoch_seconds)
    } else {
        format_offset_label(
            epoch_seconds.saturating_sub(trend.start_epoch_seconds),
            trend.unit,
        )
    }
}

fn format_clock_label_local(
    epoch_seconds: i64,
    span_seconds: i64,
    unit: crate::stats::TrendUnit,
) -> String {
    let offset = local_utc_offset();
    let dt = OffsetDateTime::from_unix_timestamp(epoch_seconds)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(offset);

    let hour24 = dt.hour();
    let minute = dt.minute();
    let am_pm = if hour24 < 12 { "AM" } else { "PM" };
    let hour12 = match hour24 % 12 {
        0 => 12,
        value => value,
    };

    if matches!(
        unit,
        crate::stats::TrendUnit::Days
            | crate::stats::TrendUnit::Weeks
            | crate::stats::TrendUnit::AllTime
    ) {
        format!("{}/{}", dt.month() as u8, dt.day())
    } else if span_seconds > 86_400 && !matches!(unit, crate::stats::TrendUnit::Hours) {
        format!(
            "{}/{} {}:{:02}{}",
            dt.month() as u8,
            dt.day(),
            hour12,
            minute,
            am_pm
        )
    } else {
        format!("{}:{:02}{}", hour12, minute, am_pm)
    }
}

fn format_clock_span_label_local(epoch_seconds: i64) -> String {
    let offset = local_utc_offset();
    let dt = OffsetDateTime::from_unix_timestamp(epoch_seconds)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(offset);

    let hour24 = dt.hour();
    let minute = dt.minute();
    let am_pm = if hour24 < 12 { "AM" } else { "PM" };
    let hour12 = match hour24 % 12 {
        0 => 12,
        value => value,
    };

    format!(
        "{} {}/{} {}:{:02}{}",
        weekday_short(dt.weekday()),
        dt.month() as u8,
        dt.day(),
        hour12,
        minute,
        am_pm
    )
}

fn weekday_short(day: time::Weekday) -> &'static str {
    match day {
        time::Weekday::Monday => "Mon",
        time::Weekday::Tuesday => "Tue",
        time::Weekday::Wednesday => "Wed",
        time::Weekday::Thursday => "Thu",
        time::Weekday::Friday => "Fri",
        time::Weekday::Saturday => "Sat",
        time::Weekday::Sunday => "Sun",
    }
}

fn local_utc_offset() -> UtcOffset {
    static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();
    *LOCAL_OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

fn short_metric_label(value: u64, sort: StatsSort) -> String {
    match sort {
        StatsSort::Plays => format!("{value}p"),
        StatsSort::ListenTime => {
            if value >= 7_200 {
                format!("{:.1}h", value as f64 / 3_600.0)
            } else if value >= 60 {
                format!("{}m", value / 60)
            } else {
                format!("{}s", value)
            }
        }
    }
}

fn format_offset_label(seconds: i64, unit: crate::stats::TrendUnit) -> String {
    match unit {
        crate::stats::TrendUnit::Minutes => format!("{}m", (seconds / 60).max(0)),
        crate::stats::TrendUnit::Hours => format!("{}h", (seconds / 3600).max(0)),
        crate::stats::TrendUnit::Days => format!("{}d", (seconds / 86_400).max(0)),
        crate::stats::TrendUnit::Weeks => format!("{}w", (seconds / (86_400 * 7)).max(0)),
        crate::stats::TrendUnit::AllTime => {
            let days = (seconds / 86_400).max(0);
            if days >= 365 {
                format!("{}y", days / 365)
            } else if days >= 30 {
                format!("{}mo", days / 30)
            } else {
                format!("{}d", days)
            }
        }
    }
}

fn stamp_label(buffer: &mut [char], center: usize, label: &str) {
    if buffer.is_empty() || label.is_empty() {
        return;
    }
    let len = label.chars().count();
    let mut start = center.saturating_sub(len / 2);
    if start + len > buffer.len() {
        start = buffer.len().saturating_sub(len);
    }
    for (idx, ch) in label.chars().enumerate() {
        if let Some(slot) = buffer.get_mut(start + idx) {
            *slot = ch;
        }
    }
}

fn truncate_for_line(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out = input
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('~');
    out
}

fn format_seconds(seconds: u64) -> String {
    let hours = seconds / 3600;
    let mins = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins:02}m {secs:02}s")
    }
}

fn format_recent_event(event: &ListenEvent, now_epoch_seconds: i64) -> String {
    let age_seconds = now_epoch_seconds
        .saturating_sub(event.started_at_epoch_seconds)
        .max(0) as u64;
    let age = if age_seconds < 60 {
        format!("{}s ago", age_seconds)
    } else if age_seconds < 3600 {
        format!("{}m ago", age_seconds / 60)
    } else {
        format!("{}h ago", age_seconds / 3600)
    };
    format!(
        "{}  {:>7}  {}",
        age,
        format_seconds(u64::from(event.listened_seconds)),
        truncate_for_line(&event.title, 24)
    )
}

fn panel_block(title: &str, bg: Color, text: Color, border: Color) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(text).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg))
}

fn draw_timeline_panel(
    frame: &mut Frame,
    area: Rect,
    core: &TuneCore,
    audio: &dyn AudioEngine,
    colors: &ThemePalette,
) {
    frame.render_widget(
        panel_block("Timeline", colors.panel_bg, colors.text, colors.border),
        area,
    );

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let controls = timeline_controls_line(core, colors);
    let controls_width = (controls.width() as u16).min(inner.width);
    let gap_width = u16::from(controls_width > 0 && controls_width < inner.width);
    let timeline_width = inner.width.saturating_sub(controls_width + gap_width);

    if timeline_width > 0 {
        let timeline_bar_width = usize::from(timeline_width.saturating_sub(18)).clamp(8, 42);
        let timeline_area = Rect {
            x: inner.x,
            y: inner.y,
            width: timeline_width,
            height: inner.height,
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                timeline_line(audio, timeline_bar_width),
                Style::default().fg(colors.text),
            )),
            timeline_area,
        );
    }

    if controls_width > 0 {
        let controls_area = Rect {
            x: inner.x + inner.width.saturating_sub(controls_width),
            y: inner.y,
            width: controls_width,
            height: inner.height,
        };
        frame.render_widget(
            Paragraph::new(controls).alignment(Alignment::Right),
            controls_area,
        );
        register_timeline_control_hits(controls_area, core);
    }

    if timeline_width > 0 {
        let timeline_bar_width = usize::from(timeline_width.saturating_sub(18)).clamp(8, 42) as u16;
        // Timeline text is "MM:SS / MM:SS [bar]" — bar starts after `time / time `
        // which is 15 cells (5 + 3 + 5 + 2 spaces). Approximate hit zone: the entire timeline_area
        // minus the leading 15 cells.
        let bar_x = inner.x.saturating_add(15);
        if inner.x.saturating_add(inner.width) > bar_x {
            let max_bar_width = inner.x.saturating_add(timeline_width).saturating_sub(bar_x);
            let bar_width = timeline_bar_width.min(max_bar_width);
            if bar_width > 0 {
                hit_map_push(
                    Rect {
                        x: bar_x,
                        y: inner.y,
                        width: bar_width,
                        height: 1,
                    },
                    HitTarget::TimelineBar {
                        x: bar_x,
                        width: bar_width,
                    },
                );
            }
        }
    }
}

fn register_timeline_control_hits(area: Rect, core: &TuneCore) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let scrub = timeline_scrub_label(core.scrub_seconds);
    let badges: [(u16, HitTarget); 4] = [
        (key_badge_width("B", "Previous"), HitTarget::Prev),
        (key_badge_width("N", "Next"), HitTarget::Next),
        (
            key_badge_width("A", &format!("-{}", scrub)),
            HitTarget::ScrubBack,
        ),
        (
            key_badge_width("D", &format!("+{}", scrub)),
            HitTarget::ScrubFwd,
        ),
    ];
    let total: u16 = badges.iter().map(|(w, _)| *w).sum::<u16>() + 3; // 3 spaces between 4 badges
    if total > area.width {
        return;
    }
    let mut x = area.x + (area.width - total);
    for (idx, (width, target)) in badges.iter().enumerate() {
        if idx > 0 {
            x = x.saturating_add(1);
        }
        hit_map_push(
            Rect {
                x,
                y: area.y,
                width: *width,
                height: 1,
            },
            *target,
        );
        x = x.saturating_add(*width);
    }
}

fn register_selection_action_hits(area: Rect) {
    if area.width == 0 || area.height < 3 {
        return;
    }

    let badges = selection_action_button_specs();
    let total: u16 = badges
        .iter()
        .map(|spec| key_badge_width(spec.key, spec.label))
        .sum::<u16>()
        + badges.len().saturating_sub(1) as u16;
    if total > area.width.saturating_sub(2) {
        return;
    }

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let y = inner.y;
    let mut x = inner.x;
    for (idx, spec) in badges.iter().enumerate() {
        if idx > 0 {
            x = x.saturating_add(1);
        }
        let width = key_badge_width(spec.key, spec.label);
        hit_map_push(
            Rect {
                x,
                y,
                width,
                height: 1,
            },
            spec.target,
        );
        x = x.saturating_add(width);
    }
}

fn key_badge_width(key: &str, label: &str) -> u16 {
    // "[" (1) + " " + key + " " + label + " " + "]" (1) = 5 + key + label
    (5 + key.chars().count() + label.chars().count()) as u16
}

fn timeline_controls_line(core: &TuneCore, colors: &ThemePalette) -> Line<'static> {
    let scrub = timeline_scrub_label(core.scrub_seconds);
    let mut spans = Vec::with_capacity(19);
    append_key_badge(
        &mut spans,
        "B",
        "Previous",
        Color::Rgb(95, 71, 138),
        Color::Rgb(190, 164, 255),
        colors.text,
    );
    spans.push(Span::raw(" "));
    append_key_badge(
        &mut spans,
        "N",
        "Next",
        Color::Rgb(43, 94, 122),
        Color::Rgb(139, 220, 255),
        colors.text,
    );
    spans.push(Span::raw(" "));
    append_key_badge(
        &mut spans,
        "A",
        &format!("-{}", scrub),
        Color::Rgb(105, 76, 37),
        Color::Rgb(255, 204, 128),
        colors.text,
    );
    spans.push(Span::raw(" "));
    append_key_badge(
        &mut spans,
        "D",
        &format!("+{}", scrub),
        Color::Rgb(37, 105, 75),
        Color::Rgb(134, 255, 190),
        colors.text,
    );
    Line::from(spans)
}

#[derive(Debug, Clone, Copy)]
struct SelectionActionButtonSpec {
    key: &'static str,
    label: &'static str,
    bg: Color,
    border: Color,
    target: HitTarget,
}

fn selection_action_button_specs() -> [SelectionActionButtonSpec; 4] {
    [
        SelectionActionButtonSpec {
            key: "Ctrl+P",
            label: "Sel->PL",
            bg: Color::Rgb(95, 71, 138),
            border: Color::Rgb(190, 164, 255),
            target: HitTarget::QuickAddSelectedToPlaylist,
        },
        SelectionActionButtonSpec {
            key: "Ctrl+O",
            label: "Now->PL",
            bg: Color::Rgb(43, 94, 122),
            border: Color::Rgb(139, 220, 255),
            target: HitTarget::QuickAddNowPlayingToPlaylist,
        },
        SelectionActionButtonSpec {
            key: "Ctrl+U",
            label: "Queue End",
            bg: Color::Rgb(105, 76, 37),
            border: Color::Rgb(255, 204, 128),
            target: HitTarget::QuickAddSelectedToQueueEnd,
        },
        SelectionActionButtonSpec {
            key: "Ctrl+Y",
            label: "Queue Next",
            bg: Color::Rgb(37, 105, 75),
            border: Color::Rgb(134, 255, 190),
            target: HitTarget::QuickAddSelectedToQueueNext,
        },
    ]
}

fn selection_actions_line(colors: &ThemePalette) -> Line<'static> {
    let specs = selection_action_button_specs();
    let mut spans = Vec::with_capacity(specs.len().saturating_mul(4).saturating_sub(1));
    for (idx, spec) in specs.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        append_key_badge(
            &mut spans,
            spec.key,
            spec.label,
            spec.bg,
            spec.border,
            colors.text,
        );
    }
    Line::from(spans)
}

fn append_key_badge(
    spans: &mut Vec<Span<'static>>,
    key: &str,
    label: &str,
    bg: Color,
    border: Color,
    text: Color,
) {
    spans.push(Span::styled(
        "[",
        Style::default()
            .fg(border)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!(" {key} {label} "),
        Style::default().fg(text).bg(bg),
    ));
    spans.push(Span::styled(
        "]",
        Style::default()
            .fg(border)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    ));
}

fn timeline_scrub_label(seconds: u16) -> String {
    if seconds == 60 {
        String::from("1m")
    } else {
        format!("{seconds}s")
    }
}

fn header_status_line(core: &TuneCore, colors: &ThemePalette) -> Line<'static> {
    let tracks_bg = Color::Rgb(95, 71, 138);
    let shuffle_bg = Color::Rgb(43, 94, 122);
    let repeat_bg = Color::Rgb(105, 76, 37);
    let online_bg = Color::Rgb(37, 105, 75);
    let shuffle_style = if core.shuffle_enabled {
        Style::default()
            .fg(colors.accent)
            .bg(shuffle_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(colors.muted).bg(shuffle_bg)
    };
    let repeat_style = match core.repeat_mode {
        RepeatMode::Off => Style::default().fg(colors.muted).bg(repeat_bg),
        RepeatMode::All => Style::default()
            .fg(colors.text)
            .bg(repeat_bg)
            .add_modifier(Modifier::BOLD),
        RepeatMode::One => Style::default()
            .fg(colors.alert)
            .bg(repeat_bg)
            .add_modifier(Modifier::BOLD),
    };
    let online_style = if core.online.session.is_some() {
        Style::default()
            .fg(colors.accent)
            .bg(online_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(colors.muted).bg(online_bg)
    };

    Line::from(vec![
        Span::styled(
            format!(" Tracks {} ", core.tracks.len()),
            Style::default().fg(colors.text).bg(tracks_bg),
        ),
        Span::raw(" "),
        Span::styled(
            format!(
                " V Shuffle {} ",
                if core.shuffle_enabled { "On" } else { "Off" }
            ),
            shuffle_style,
        ),
        Span::raw(" "),
        Span::styled(
            format!(" M Repeat {} ", core.repeat_mode.label()),
            repeat_style,
        ),
        Span::raw(" "),
        Span::styled(
            if core.online.session.is_some() {
                " ONLINE "
            } else {
                " OFFLINE "
            },
            online_style,
        ),
    ])
}

fn register_status_pill_hits(area: Rect, core: &TuneCore) {
    if area.width == 0 || area.height < 2 {
        return;
    }
    // Title-bottom of the status block sits on the bottom border row.
    let y = area.y + area.height - 1;
    let tracks_label = format!(" Tracks {} ", core.tracks.len());
    let shuffle_label = format!(
        " V Shuffle {} ",
        if core.shuffle_enabled { "On" } else { "Off" }
    );
    let repeat_label = format!(" M Repeat {} ", core.repeat_mode.label());
    let online_label = if core.online.session.is_some() {
        " ONLINE "
    } else {
        " OFFLINE "
    };

    let widths = [
        tracks_label.chars().count() as u16,
        1,
        shuffle_label.chars().count() as u16,
        1,
        repeat_label.chars().count() as u16,
        1,
        online_label.chars().count() as u16,
    ];
    let total: u16 = widths.iter().sum();
    if total == 0 || total > area.width {
        return;
    }
    let mut x = area.x + (area.width - total) / 2;

    // Tracks pill (no action - skip but advance cursor).
    x = x.saturating_add(widths[0]);
    x = x.saturating_add(widths[1]); // separator
    hit_map_push(
        Rect {
            x,
            y,
            width: widths[2],
            height: 1,
        },
        HitTarget::ToggleShuffle,
    );
    x = x.saturating_add(widths[2]);
    x = x.saturating_add(widths[3]);
    hit_map_push(
        Rect {
            x,
            y,
            width: widths[4],
            height: 1,
        },
        HitTarget::CycleRepeat,
    );
    x = x.saturating_add(widths[4]);
    x = x.saturating_add(widths[5]);
    hit_map_push(
        Rect {
            x,
            y,
            width: widths[6],
            height: 1,
        },
        HitTarget::OpenOnline,
    );
}

fn status_panel_block(core: &TuneCore, colors: &ThemePalette) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Status ",
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(header_status_line(core, colors).alignment(Alignment::Center))
        .border_style(Style::default().fg(colors.border))
        .style(Style::default().bg(colors.panel_bg))
}

fn draw_action_panel(frame: &mut Frame, panel: &ActionPanelView, colors: &ThemePalette) {
    // Cover the whole frame with a "background" hit so clicks outside the popup
    // close the panel. The popup itself overrides this with later registrations.
    hit_map_push(frame.area(), HitTarget::ActionPanelBackground);

    let popup = centered_rect(frame.area(), 62, 58);
    frame.render_widget(Clear, popup);
    hit_map_push(popup, HitTarget::ActionPanelInside);

    let mut panel_block_widget =
        panel_block(&panel.title, colors.popup_bg, colors.text, colors.border);
    if panel.title == "Actions" {
        panel_block_widget = panel_block_widget.title_alignment(Alignment::Center);
    }
    frame.render_widget(panel_block_widget, popup);

    let inner = popup.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let has_search = panel.search_query.is_some();
    let search_height = u16::from(has_search);
    let hint_height = 1;
    if inner.height <= search_height.saturating_add(hint_height) {
        return;
    }

    let list_height = inner.height.saturating_sub(search_height + hint_height);
    let list_height_usize = usize::from(list_height);

    if let Some(query) = &panel.search_query {
        let search_line = format!("Search: {query}");
        let search_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                search_line,
                Style::default().fg(colors.accent),
            )),
            search_area,
        );
    }

    let list_y = inner.y.saturating_add(search_height);
    let show_scrollbar = list_overflows(panel.options.len(), list_height_usize) && inner.width > 1;
    let list_width = if show_scrollbar {
        inner.width.saturating_sub(1)
    } else {
        inner.width
    };
    let list_area = Rect {
        x: inner.x,
        y: list_y,
        width: list_width,
        height: list_height,
    };

    let selected = if panel.options.is_empty() {
        None
    } else {
        Some(panel.selected.min(panel.options.len() - 1))
    };
    let scroll_top = selected
        .map(|focused| centered_scroll_top(focused, list_height_usize))
        .unwrap_or(0);

    let mut current_action_section = None;
    let items: Vec<ListItem> = panel
        .options
        .iter()
        .map(|item| action_panel_option_item(panel, item, colors, &mut current_action_section))
        .collect();

    let mut state = ListState::default()
        .with_selected(selected)
        .with_offset(usize::from(scroll_top));

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(colors.popup_selected_bg)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("-> ");
    frame.render_stateful_widget(list, list_area, &mut state);

    let panel_offset = state.offset();
    for visible_idx in 0..usize::from(list_area.height) {
        let option_idx = panel_offset + visible_idx;
        if option_idx >= panel.options.len() {
            break;
        }
        hit_map_push(
            Rect {
                x: list_area.x,
                y: list_area.y + visible_idx as u16,
                width: list_area.width,
                height: 1,
            },
            HitTarget::ActionRow(option_idx),
        );
    }

    if show_scrollbar {
        let scrollbar_area = Rect {
            x: list_area.x.saturating_add(list_area.width),
            y: list_area.y,
            width: 1,
            height: list_area.height,
        };
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(Style::default().fg(colors.border))
            .thumb_style(Style::default().fg(colors.accent));
        let mut scrollbar_state = ScrollbarState::new(panel.options.len())
            .position(usize::from(scroll_top))
            .viewport_content_length(list_height_usize);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    let hint_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(inner.height.saturating_sub(1)),
        width: inner.width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            panel.hint.as_str(),
            Style::default().fg(colors.muted),
        )),
        hint_area,
    );
}

fn action_panel_option_item(
    panel: &ActionPanelView,
    item: &str,
    colors: &ThemePalette,
    current_action_section: &mut Option<&'static str>,
) -> ListItem<'static> {
    let line = action_panel_option_line(item, colors);
    if panel.title != "Actions" {
        return ListItem::new(line);
    }

    if let Some(section) = action_panel_section_name(item) {
        *current_action_section = Some(section);
        let Some(bg) = action_panel_section_bg(section) else {
            return ListItem::new(line);
        };
        return ListItem::new(Line::from(Span::styled(
            item.to_string(),
            Style::default()
                .fg(colors.text)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(bg));
    }

    let Some(section) = *current_action_section else {
        return ListItem::new(line);
    };
    let Some(bg) = action_panel_section_bg(section) else {
        return ListItem::new(line);
    };
    ListItem::new(line).style(Style::default().bg(bg))
}

fn action_panel_section_name(item: &str) -> Option<&'static str> {
    match item {
        "Recent" => Some("Recent"),
        "Settings" => Some("Settings"),
        "Playlist" => Some("Playlist"),
        "Queue" => Some("Queue"),
        "Library" => Some("Library"),
        "Appearance" => Some("Appearance"),
        "Stats" => Some("Stats"),
        "Window" => Some("Window"),
        "Lyrics" => Some("Lyrics"),
        "Actions" => Some("Actions"),
        _ => None,
    }
}

fn action_panel_section_bg(section: &str) -> Option<Color> {
    match section {
        "Recent" => Some(Color::Rgb(74, 52, 100)),
        "Settings" => Some(Color::Rgb(65, 67, 108)),
        "Playlist" => Some(Color::Rgb(43, 88, 63)),
        "Queue" => Some(Color::Rgb(43, 94, 122)),
        "Library" => Some(Color::Rgb(45, 72, 108)),
        "Appearance" => Some(Color::Rgb(95, 71, 138)),
        "Stats" => Some(Color::Rgb(105, 76, 37)),
        "Window" => Some(Color::Rgb(76, 69, 58)),
        "Lyrics" => Some(Color::Rgb(90, 55, 55)),
        "Actions" => Some(Color::Rgb(80, 60, 112)),
        _ => None,
    }
}

fn action_panel_option_line(item: &str, colors: &ThemePalette) -> Line<'static> {
    let Some((prefix, body, suffix)) = parse_spectro_line(item) else {
        return Line::from(Span::styled(
            item.to_string(),
            Style::default().fg(colors.text),
        ));
    };

    let mut spans = Vec::with_capacity(
        prefix
            .len()
            .saturating_add(body.len())
            .saturating_add(suffix.len()),
    );
    spans.push(Span::styled(
        prefix.to_string(),
        Style::default().fg(colors.muted),
    ));
    for ch in body.chars() {
        spans.push(Span::styled(
            ch.to_string(),
            spectro_style_for_char(ch, colors),
        ));
    }
    spans.push(Span::styled(
        suffix.to_string(),
        Style::default().fg(colors.muted),
    ));
    Line::from(spans)
}

fn parse_spectro_line(item: &str) -> Option<(&str, &str, &str)> {
    let left = item.find('|')?;
    let right = item.rfind('|')?;
    if right <= left {
        return None;
    }
    let body = item.get(left + 1..right)?;
    if body.is_empty() {
        return None;
    }
    if !body.chars().all(|ch| {
        matches!(
            ch,
            ' ' | '.' | ':' | '-' | '=' | '+' | '*' | '#' | '%' | '@'
        )
    }) {
        return None;
    }

    let prefix = item.get(..=left)?;
    let suffix = item.get(right..)?;
    Some((prefix, body, suffix))
}

fn spectro_style_for_char(ch: char, colors: &ThemePalette) -> Style {
    let level = match ch {
        ' ' => 0,
        '.' => 1,
        ':' => 2,
        '-' => 3,
        '=' => 4,
        '+' => 5,
        '*' => 6,
        '#' => 7,
        '%' => 8,
        '@' => 9,
        _ => 0,
    };

    let palette = [
        Color::Rgb(14, 24, 52),
        Color::Rgb(18, 39, 92),
        Color::Rgb(24, 64, 141),
        Color::Rgb(31, 94, 181),
        Color::Rgb(24, 134, 170),
        Color::Rgb(42, 164, 110),
        Color::Rgb(117, 180, 58),
        Color::Rgb(205, 181, 49),
        Color::Rgb(219, 128, 42),
        Color::Rgb(206, 61, 34),
    ];
    let fg = palette[level.min(palette.len() - 1)];

    let mut style = Style::default().fg(fg);
    if level >= 7 {
        style = style.add_modifier(Modifier::BOLD);
    }
    if level == 0 {
        style = style.fg(colors.muted);
    }
    style
}

fn list_overflows(total_items: usize, viewport_height: usize) -> bool {
    total_items > viewport_height
}

fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);

    horizontal[1]
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct CoverRasterCacheKey {
    source_key: String,
    width: u16,
    height: u16,
}

fn cover_art_lines_for_path(
    path: &Path,
    core: &TuneCore,
    width: u16,
    height: u16,
) -> Option<Vec<Line<'static>>> {
    if width == 0 || height == 0 {
        return None;
    }

    let (source_key, art_bytes) = if let Some(embedded_art) = core.cover_art_for_path(path) {
        (path.to_string_lossy().into_owned(), embedded_art)
    } else {
        let fallback = fallback_cover_template_bytes(core.fallback_cover_template)?;
        (
            format!(
                "fallback:{}",
                fallback_cover_template_id(core.fallback_cover_template)
            ),
            fallback,
        )
    };

    let key = CoverRasterCacheKey {
        source_key,
        width,
        height,
    };

    if let Ok(cache) = cover_raster_cache().lock()
        && let Some(cached) = cache.get(&key)
    {
        return Some(cached.clone());
    }

    let rasterized = rasterize_cover_art(&art_bytes, width, height)?;

    if let Ok(mut cache) = cover_raster_cache().lock() {
        cache.insert(key, rasterized.clone());
    }

    Some(rasterized)
}

fn fallback_cover_template_cache() -> &'static Mutex<HashMap<CoverArtTemplate, Arc<[u8]>>> {
    static FALLBACK_COVER_TEMPLATE_CACHE: OnceLock<Mutex<HashMap<CoverArtTemplate, Arc<[u8]>>>> =
        OnceLock::new();
    FALLBACK_COVER_TEMPLATE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fallback_cover_template_bytes(template: CoverArtTemplate) -> Option<Arc<[u8]>> {
    if let Ok(cache) = fallback_cover_template_cache().lock()
        && let Some(bytes) = cache.get(&template)
    {
        return Some(bytes.clone());
    }

    let generated = generate_fallback_cover_template_png(template)?;
    let generated = Arc::<[u8]>::from(generated);
    if let Ok(mut cache) = fallback_cover_template_cache().lock() {
        cache.insert(template, generated.clone());
    }
    Some(generated)
}

fn fallback_cover_template_id(_template: CoverArtTemplate) -> &'static str {
    "music-note"
}

fn generate_fallback_cover_template_png(template: CoverArtTemplate) -> Option<Vec<u8>> {
    const WIDTH: u32 = 160;
    const HEIGHT: u32 = 160;
    let image = ImageBuffer::from_fn(WIDTH, HEIGHT, |x, y| {
        let pixel = fallback_cover_template_pixel(template, x, y, WIDTH, HEIGHT);
        Rgba([pixel.0, pixel.1, pixel.2, 255])
    });

    let mut bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut bytes);
    image
        .write_to(&mut cursor, ImageFormat::Png)
        .ok()
        .map(|_| bytes)
}

fn fallback_cover_template_pixel(
    _template: CoverArtTemplate,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> (u8, u8, u8) {
    let x = x.min(width.saturating_sub(1));
    let y = y.min(height.saturating_sub(1));
    let x_ratio = (x.saturating_mul(255) / width.max(1)) as u8;
    let y_ratio = (y.saturating_mul(255) / height.max(1)) as u8;

    let mut r = 10u8.saturating_add(x_ratio / 7);
    let mut g = 24u8.saturating_add(y_ratio / 5);
    let mut b = 48u8.saturating_add(x_ratio / 3).saturating_add(y_ratio / 6);

    let xi = x as i32;
    let yi = y as i32;
    let wi = width.max(1) as i32;
    let hi = height.max(1) as i32;

    let stem_left = wi * 9 / 16;
    let stem_right = stem_left + (wi / 16).max(3);
    let stem_top = hi * 3 / 16;
    let stem_bottom = hi * 11 / 16;
    let on_stem = xi >= stem_left && xi <= stem_right && yi >= stem_top && yi <= stem_bottom;

    let head_cx = wi * 7 / 16;
    let head_cy = hi * 11 / 16;
    let head_rx = (wi / 7).max(5);
    let head_ry = (hi / 9).max(4);
    let dx = xi - head_cx;
    let dy = yi - head_cy;
    let in_head = (dx * dx * head_ry * head_ry) + (dy * dy * head_rx * head_rx)
        <= (head_rx * head_rx * head_ry * head_ry);

    let flag_top = hi * 3 / 16;
    let flag_height = (hi / 5).max(6);
    let flag_width = (wi / 4).max(10);
    let rel_x = xi - stem_left;
    let rel_y = yi - flag_top;
    let in_flag = rel_x >= 0
        && rel_x <= flag_width
        && rel_y >= 0
        && rel_y <= flag_height
        && (rel_y * flag_width) <= (flag_height * rel_x + flag_width / 2);

    let note_color = (230u8, 236u8, 247u8);
    if on_stem || in_head || in_flag {
        return note_color;
    }

    let dist_stem = if xi < stem_left {
        stem_left - xi
    } else if xi > stem_right {
        xi - stem_right
    } else {
        0
    } + if yi < stem_top {
        stem_top - yi
    } else if yi > stem_bottom {
        yi - stem_bottom
    } else {
        0
    };

    let head_distance = ((dx * dx + dy * dy) as f32).sqrt() as i32 - head_rx.max(head_ry);
    let glow_dist = dist_stem.min(head_distance.max(0));
    if glow_dist <= 4 {
        let glow = (5 - glow_dist).max(1) as u8 * 10;
        r = r.saturating_add(glow / 2);
        g = g.saturating_add(glow / 2);
        b = b.saturating_add(glow);
    }

    (r, g, b)
}

fn cover_raster_cache() -> &'static Mutex<HashMap<CoverRasterCacheKey, Vec<Line<'static>>>> {
    static COVER_RASTER_CACHE: OnceLock<Mutex<HashMap<CoverRasterCacheKey, Vec<Line<'static>>>>> =
        OnceLock::new();
    COVER_RASTER_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn rasterize_cover_art(bytes: &[u8], width: u16, height: u16) -> Option<Vec<Line<'static>>> {
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    let target_width_pixels = width as u32;
    let target_height_pixels = (height * 2) as u32;

    let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
    let source_width = decoded.width();
    let source_height = decoded.height();
    if source_width == 0 || source_height == 0 {
        return None;
    }

    let (scaled_width, scaled_height, x_offset, y_offset) = fit_image_box(
        source_width,
        source_height,
        target_width_pixels,
        target_height_pixels,
    );
    let resized =
        image::imageops::resize(&decoded, scaled_width, scaled_height, FilterType::Triangle);

    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let top_y = (row * 2) as u32;
        let bottom_y = (top_y + 1).min(target_height_pixels.saturating_sub(1));
        let mut spans = Vec::with_capacity(width);

        for column in 0..width {
            let x = column as u32;
            if x < x_offset || x >= x_offset + scaled_width {
                spans.push(Span::raw(" "));
                continue;
            }

            if top_y < y_offset || top_y >= y_offset + scaled_height {
                spans.push(Span::raw(" "));
                continue;
            }

            let local_x = x - x_offset;
            let top_local_y = top_y - y_offset;
            let bottom_local_y = bottom_y.saturating_sub(y_offset).min(scaled_height - 1);
            let top = resized.get_pixel(local_x, top_local_y).0;
            let bottom = resized.get_pixel(local_x, bottom_local_y).0;
            spans.push(Span::styled(
                "▀",
                Style::default()
                    .fg(rgba_to_color(top))
                    .bg(rgba_to_color(bottom)),
            ));
        }

        lines.push(Line::from(spans));
    }

    Some(lines)
}

fn fit_image_box(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> (u32, u32, u32, u32) {
    let width_limited =
        target_width.saturating_mul(source_height) <= target_height.saturating_mul(source_width);

    let (scaled_width, scaled_height) = if width_limited {
        let scaled_height = (source_height.saturating_mul(target_width) / source_width).max(1);
        (target_width.max(1), scaled_height)
    } else {
        let scaled_width = (source_width.saturating_mul(target_height) / source_height).max(1);
        (scaled_width, target_height.max(1))
    };

    let x_offset = target_width.saturating_sub(scaled_width) / 2;
    let y_offset = target_height.saturating_sub(scaled_height) / 2;
    (scaled_width, scaled_height, x_offset, y_offset)
}

fn rgba_to_color(pixel: [u8; 4]) -> Color {
    let alpha = pixel[3];
    if alpha == 255 {
        return Color::Rgb(pixel[0], pixel[1], pixel[2]);
    }

    let blend = |channel: u8| ((u16::from(channel) * u16::from(alpha)) / 255) as u8;
    Color::Rgb(blend(pixel[0]), blend(pixel[1]), blend(pixel[2]))
}

fn cover_placeholder_lines(width: u16, height: u16) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    let mut lines = vec![Line::from(" ".repeat(width)); height];

    let label = "No cover art";
    let row = height / 2;
    let available = width.min(label.len());
    let start = width.saturating_sub(available) / 2;
    let end = start + available;

    let mut content = " ".repeat(width);
    content.replace_range(start..end, &label[..available]);
    lines[row] = Line::from(content);
    lines
}

fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn format_lrc_time(ms: u32) -> String {
    let minutes = ms / 60_000;
    let seconds = (ms % 60_000) / 1000;
    let hundredths = (ms % 1000) / 10;
    format!("[{minutes:02}:{seconds:02}.{hundredths:02}]")
}

fn progress_bar(ratio: Option<f64>, width: usize) -> String {
    let clamped = ratio.unwrap_or(0.0).clamp(0.0, 1.0);
    let filled = (clamped * width as f64).round() as usize;
    let mut bar = String::with_capacity(width + 2);
    bar.push('[');
    bar.push_str(&"█".repeat(filled));
    bar.push_str(&"░".repeat(width.saturating_sub(filled)));
    bar.push(']');
    bar
}

fn timeline_line(audio: &dyn AudioEngine, timeline_bar_width: usize) -> String {
    let elapsed = audio.position().unwrap_or(Duration::from_secs(0));
    let total = audio.duration();
    let ratio = total.and_then(|duration| {
        let total_secs = duration.as_secs_f64();
        (total_secs > 0.0).then_some((elapsed.as_secs_f64() / total_secs).clamp(0.0, 1.0))
    });

    format!(
        "{} / {} {}",
        format_duration(elapsed),
        total
            .map(format_duration)
            .unwrap_or_else(|| String::from("--:--")),
        progress_bar(ratio, timeline_bar_width),
    )
}

fn register_control_line_hits(area: Rect, volume_bar_width: u16) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let inner_x = area.x + 1;
    let inner_y = area.y + 1;
    let inner_width = area.width.saturating_sub(2);

    let bar_x = inner_x.saturating_add(5);
    let bar_width = volume_bar_width.min(inner_width.saturating_sub(5));
    if bar_width > 0 {
        hit_map_push(
            Rect {
                x: bar_x,
                y: inner_y,
                width: bar_width,
                height: 1,
            },
            HitTarget::VolumeBar {
                x: bar_x,
                width: bar_width,
            },
        );
    }

    // Layout reproduced from control_line: "Vol [bar] {pct:>3}%  [ - Lower ] [ + Raise ]  Shift fine"
    let lower_badge_w = key_badge_width("-", "Lower");
    let raise_badge_w = key_badge_width("+", "Raise");
    // 4 (Vol ) + 1 ([) + bar + 1 (]) + 1 ( ) + 3 (pct) + 1 (%) + 2 (  ) = bar + 13
    let lower_x = inner_x.saturating_add(volume_bar_width).saturating_add(13);
    let lower_end = lower_x.saturating_add(lower_badge_w);
    if lower_end <= inner_x.saturating_add(inner_width) {
        hit_map_push(
            Rect {
                x: lower_x,
                y: inner_y,
                width: lower_badge_w,
                height: 1,
            },
            HitTarget::VolumeDown,
        );
        let raise_x = lower_end.saturating_add(1);
        let raise_end = raise_x.saturating_add(raise_badge_w);
        if raise_end <= inner_x.saturating_add(inner_width) {
            hit_map_push(
                Rect {
                    x: raise_x,
                    y: inner_y,
                    width: raise_badge_w,
                    height: 1,
                },
                HitTarget::VolumeUp,
            );
        }
    }
}

fn control_line(
    audio: &dyn AudioEngine,
    volume_bar_width: usize,
    colors: &ThemePalette,
) -> Line<'static> {
    let volume_percent = (audio.volume() * 100.0).round() as u16;
    let volume_ratio = audio.volume().clamp(0.0, 1.0) as f64;
    let mut spans = Vec::with_capacity(10);

    spans.push(Span::styled(
        format!(
            "Vol {} {:>3}%  ",
            progress_bar(Some(volume_ratio), volume_bar_width),
            volume_percent
        ),
        Style::default().fg(colors.text),
    ));
    append_key_badge(
        &mut spans,
        "-",
        "Lower",
        Color::Rgb(90, 55, 55),
        Color::Rgb(255, 151, 151),
        colors.text,
    );
    spans.push(Span::raw(" "));
    append_key_badge(
        &mut spans,
        "+",
        "Raise",
        Color::Rgb(43, 88, 63),
        Color::Rgb(136, 255, 184),
        colors.text,
    );
    spans.push(Span::styled(
        "  Shift fine",
        Style::default().fg(colors.muted),
    ));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::online::{OnlineSession, TransportCommand, TransportEnvelope};
    use image::{ImageBuffer, ImageFormat, Rgba};
    use std::io::Cursor;

    #[test]
    fn centered_scroll_top_centers_focus_when_possible() {
        assert_eq!(centered_scroll_top(30, 10), 25);
    }

    #[test]
    fn centered_scroll_top_handles_small_focus() {
        assert_eq!(centered_scroll_top(2, 12), 0);
    }

    #[test]
    fn editor_scroll_top_includes_header_offset() {
        assert_eq!(editor_scroll_top(30, 10, 5), 30);
    }

    #[test]
    fn action_panel_scroll_top_uses_centered_strategy() {
        assert_eq!(centered_scroll_top(15, 6), 12);
    }

    #[test]
    fn hit_map_returns_topmost_target() {
        let mut map = HitMap::default();
        map.push(
            Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            HitTarget::ActionPanelBackground,
        );
        map.push(
            Rect {
                x: 2,
                y: 2,
                width: 4,
                height: 4,
            },
            HitTarget::ActionPanelInside,
        );
        // Inside the popup -> popup wins (registered later).
        assert_eq!(map.hit(3, 3), Some(HitTarget::ActionPanelInside));
        // Outside the popup -> background.
        assert_eq!(map.hit(8, 8), Some(HitTarget::ActionPanelBackground));
    }

    #[test]
    fn hit_map_misses_outside_all_rects() {
        let mut map = HitMap::default();
        map.push(
            Rect {
                x: 5,
                y: 5,
                width: 3,
                height: 3,
            },
            HitTarget::Tab(HeaderSection::Library),
        );
        assert!(map.hit(0, 0).is_none());
        assert!(map.hit(8, 8).is_none()); // x is at rect.x + width, exclusive
    }

    #[test]
    fn hit_map_skips_zero_sized_rects() {
        let mut map = HitMap::default();
        map.push(
            Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            HitTarget::Prev,
        );
        assert_eq!(map.entries().len(), 0);
    }

    #[test]
    fn key_badge_width_matches_rendered_text() {
        // "[ B Previous ]" = 14 cells.
        assert_eq!(key_badge_width("B", "Previous"), 14);
        assert_eq!(key_badge_width("-", "Lower"), 11);
        // "[ D +30s ]" = 10 cells.
        assert_eq!(key_badge_width("D", "+30s"), 10);
    }

    #[test]
    fn header_tab_hits_register_for_each_section() {
        // Make sure register_header_tab_hits pushes 4 entries with HitTarget::Tab(*).
        let cell = hit_map_cell();
        cell.lock().unwrap().clear();
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 1,
        };
        register_header_tab_hits(area);
        let entries: Vec<_> = cell
            .lock()
            .unwrap()
            .entries()
            .iter()
            .map(|(_, t)| *t)
            .collect();
        assert_eq!(
            entries,
            vec![
                HitTarget::Tab(HeaderSection::Library),
                HitTarget::Tab(HeaderSection::Lyrics),
                HitTarget::Tab(HeaderSection::Stats),
                HitTarget::Tab(HeaderSection::Online),
            ]
        );
    }

    #[test]
    fn timeline_control_hits_cover_four_badges() {
        let mut core = TuneCore::from_persisted(crate::model::PersistedState::default());
        core.scrub_seconds = 30;
        let cell = hit_map_cell();
        cell.lock().unwrap().clear();
        register_timeline_control_hits(
            Rect {
                x: 10,
                y: 5,
                width: 60,
                height: 1,
            },
            &core,
        );
        let entries: Vec<_> = cell
            .lock()
            .unwrap()
            .entries()
            .iter()
            .map(|(_, t)| *t)
            .collect();
        assert_eq!(
            entries,
            vec![
                HitTarget::Prev,
                HitTarget::Next,
                HitTarget::ScrubBack,
                HitTarget::ScrubFwd
            ]
        );
    }

    #[test]
    fn selection_action_hits_cover_four_badges() {
        let cell = hit_map_cell();
        cell.lock().unwrap().clear();
        register_selection_action_hits(Rect {
            x: 0,
            y: 10,
            width: 90,
            height: 3,
        });
        let entries: Vec<_> = cell
            .lock()
            .unwrap()
            .entries()
            .iter()
            .map(|(_, t)| *t)
            .collect();
        assert_eq!(
            entries,
            vec![
                HitTarget::QuickAddSelectedToPlaylist,
                HitTarget::QuickAddNowPlayingToPlaylist,
                HitTarget::QuickAddSelectedToQueueEnd,
                HitTarget::QuickAddSelectedToQueueNext,
            ]
        );
    }

    #[test]
    fn selection_actions_line_shows_ctrl_keys() {
        let colors = palette(Theme::Dark);
        let text = selection_actions_line(&colors)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(text.contains("Ctrl+P"));
        assert!(text.contains("Ctrl+O"));
        assert!(text.contains("Ctrl+U"));
        assert!(text.contains("Ctrl+Y"));
    }

    #[test]
    fn action_panel_scrollbar_only_when_overflow_exists() {
        assert!(list_overflows(8, 5));
        assert!(!list_overflows(5, 5));
    }

    #[test]
    fn action_panel_sections_have_distinct_backgrounds() {
        let sections = [
            "Recent",
            "Settings",
            "Playlist",
            "Queue",
            "Library",
            "Appearance",
            "Stats",
            "Window",
            "Lyrics",
            "Actions",
        ];
        let mut backgrounds = Vec::with_capacity(sections.len());

        for section in sections {
            let bg = action_panel_section_bg(section).expect("section background");
            assert!(!backgrounds.contains(&bg));
            backgrounds.push(bg);
        }
    }

    #[test]
    fn action_panel_section_detection_only_matches_headers() {
        assert_eq!(action_panel_section_name("Playlist"), Some("Playlist"));
        assert_eq!(action_panel_section_name("  Remove playlist"), None);
        assert_eq!(action_panel_section_name("(no matching actions)"), None);
    }

    #[test]
    fn timeline_line_only_shows_timeline_data() {
        let mut audio = crate::audio::NullAudioEngine::new();
        audio.set_volume(1.4);
        let line = timeline_line(&audio, 10);
        assert!(line.contains('/'));
        assert!(!line.contains("Vol"));
    }

    #[test]
    fn control_line_shows_volume_hint_without_scrub() {
        let mut audio = crate::audio::NullAudioEngine::new();
        audio.set_volume(1.2);
        let colors = palette(Theme::Dark);
        let line = control_line(&audio, 10, &colors);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(text.contains("Vol"));
        assert!(text.contains("[ - Lower ] [ + Raise ]  Shift fine"));
        assert!(!text.contains("scrub"));
        assert_eq!(line.spans[1].style.bg, Some(Color::Rgb(90, 55, 55)));
        assert_eq!(line.spans[5].style.bg, Some(Color::Rgb(43, 88, 63)));
    }

    #[test]
    fn timeline_controls_line_shows_colored_key_badges() {
        let mut core = TuneCore::from_persisted(crate::model::PersistedState::default());
        core.scrub_seconds = 30;
        let colors = palette(Theme::Dark);
        let line = timeline_controls_line(&core, &colors);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(text, "[ B Previous ] [ N Next ] [ A -30s ] [ D +30s ]");
        assert_eq!(line.spans[0].style.bg, Some(Color::Rgb(95, 71, 138)));
        assert_eq!(line.spans[4].style.bg, Some(Color::Rgb(43, 94, 122)));
        assert_eq!(line.spans[8].style.bg, Some(Color::Rgb(105, 76, 37)));
        assert_eq!(line.spans[12].style.bg, Some(Color::Rgb(37, 105, 75)));
    }

    #[test]
    fn header_status_text_shows_playback_state() {
        let core = TuneCore::from_persisted(crate::model::PersistedState::default());
        let colors = palette(Theme::Dark);
        let line = header_status_line(&core, &colors);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(text, " Tracks 0   V Shuffle Off   M Repeat Off   OFFLINE ");
        assert_eq!(line.spans[0].style.bg, Some(Color::Rgb(95, 71, 138)));
        assert_eq!(line.spans[2].style.bg, Some(Color::Rgb(43, 94, 122)));
        assert_eq!(line.spans[4].style.bg, Some(Color::Rgb(105, 76, 37)));
        assert_eq!(line.spans[6].style.bg, Some(Color::Rgb(37, 105, 75)));
    }

    #[test]
    fn hour_clock_labels_omit_date_even_when_span_crosses_days() {
        let label =
            format_clock_label_local(1_700_000_000, 200_000, crate::stats::TrendUnit::Hours);
        assert!(!label.contains('/'));
    }

    #[test]
    fn day_clock_labels_include_date_for_multi_day_span() {
        let label = format_clock_label_local(1_700_000_000, 200_000, crate::stats::TrendUnit::Days);
        assert!(label.contains('/'));
        assert!(!label.contains(':'));
    }

    #[test]
    fn listen_metric_label_uses_decimal_hours_when_over_two_hours() {
        assert_eq!(short_metric_label(43_080, StatsSort::ListenTime), "12.0h");
    }

    #[test]
    fn listen_metric_label_stays_in_minutes_under_two_hours() {
        assert_eq!(short_metric_label(7_140, StatsSort::ListenTime), "119m");
    }

    #[test]
    fn span_labels_include_weekday_and_date() {
        let label = format_clock_span_label_local(1_700_000_000);
        assert!(label.contains('/'));
        assert!(
            label.starts_with("Mon")
                || label.starts_with("Tue")
                || label.starts_with("Wed")
                || label.starts_with("Thu")
                || label.starts_with("Fri")
                || label.starts_with("Sat")
                || label.starts_with("Sun")
        );
    }

    #[test]
    fn rasterize_cover_art_outputs_requested_dimensions() {
        let image = ImageBuffer::from_fn(2, 2, |x, y| {
            if (x + y) % 2 == 0 {
                Rgba([255u8, 40u8, 40u8, 255u8])
            } else {
                Rgba([40u8, 180u8, 255u8, 255u8])
            }
        });
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("encode png");

        let lines = rasterize_cover_art(&bytes, 6, 4).expect("rasterized lines");
        assert_eq!(lines.len(), 4);
        assert!(lines.iter().all(|line| line.spans.len() == 6));
    }

    #[test]
    fn cover_placeholder_contains_label() {
        let lines = cover_placeholder_lines(16, 5);
        assert_eq!(lines.len(), 5);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("No cover art"))
        );
    }

    #[test]
    fn fit_image_box_letterboxes_wide_image() {
        let (scaled_w, scaled_h, x_off, y_off) = fit_image_box(1000, 500, 20, 24);
        assert_eq!(scaled_w, 20);
        assert_eq!(scaled_h, 10);
        assert_eq!(x_off, 0);
        assert_eq!(y_off, 7);
    }

    #[test]
    fn fit_image_box_letterboxes_tall_image() {
        let (scaled_w, scaled_h, x_off, y_off) = fit_image_box(500, 1000, 20, 24);
        assert_eq!(scaled_h, 24);
        assert_eq!(scaled_w, 12);
        assert_eq!(x_off, 4);
        assert_eq!(y_off, 0);
    }

    #[test]
    fn fallback_template_png_generation_works_for_all_templates() {
        let bytes =
            generate_fallback_cover_template_png(CoverArtTemplate::Aurora).expect("template png");
        let lines = rasterize_cover_art(&bytes, 8, 4).expect("rasterized lines");
        assert_eq!(lines.len(), 4);
        assert!(lines.iter().all(|line| line.spans.len() == 8));
    }

    #[test]
    fn fallback_template_bytes_are_cached_by_template() {
        let first = fallback_cover_template_bytes(CoverArtTemplate::Aurora).expect("first");
        let second = fallback_cover_template_bytes(CoverArtTemplate::Aurora).expect("second");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn shared_queue_waiting_message_shows_for_local_queue_playback() {
        let mut session = OnlineSession::host("host");
        session.push_shared_track(
            Path::new("shared.mp3"),
            String::from("shared"),
            Some(String::from("guest")),
        );
        session.last_transport = Some(TransportEnvelope {
            seq: 2,
            origin_nickname: String::from("host"),
            command: TransportCommand::SetPlaybackState {
                path: Path::new("local.mp3").to_path_buf(),
                title: None,
                artist: None,
                album: None,
                provider_track_id: None,
                position_ms: 5_000,
                paused: false,
            },
        });

        let message = shared_queue_waiting_message(&session);
        assert_eq!(message.as_deref(), Some("Now playing @host local queue."));
    }

    #[test]
    fn shared_queue_waiting_message_hidden_when_current_is_shared_head() {
        let mut session = OnlineSession::host("host");
        session.push_shared_track(
            Path::new("shared.mp3"),
            String::from("shared"),
            Some(String::from("guest")),
        );
        session.last_transport = Some(TransportEnvelope {
            seq: 2,
            origin_nickname: String::from("host"),
            command: TransportCommand::PlayTrack {
                path: Path::new("shared.mp3").to_path_buf(),
                title: None,
                artist: None,
                album: None,
                provider_track_id: None,
            },
        });

        assert_eq!(shared_queue_waiting_message(&session), None);
    }

    #[test]
    fn room_directory_lock_prefix_uses_alert_color() {
        let colors = palette(Theme::Dark);
        let line = room_directory_line("[lock] demo 1/8", Style::default(), &colors);
        assert_eq!(line.spans[0].content.as_ref(), "[lock]");
        assert_eq!(line.spans[0].style.fg, Some(colors.alert));
    }

    #[test]
    fn system_palette_uses_terminal_colors() {
        let colors = palette(Theme::System);

        assert_eq!(colors.bg, Color::Reset);
        assert_eq!(colors.text, Color::Reset);
        assert_eq!(colors.accent, Color::Cyan);
        assert_eq!(colors.border, Color::Blue);
    }

    #[test]
    fn online_now_playing_line_shows_transport_track_and_origin() {
        let mut session = OnlineSession::host("host");
        session.last_transport = Some(TransportEnvelope {
            seq: 3,
            origin_nickname: String::from("host"),
            command: TransportCommand::SetPlaybackState {
                path: Path::new("folder/live.mp3").to_path_buf(),
                title: None,
                artist: None,
                album: None,
                provider_track_id: None,
                position_ms: 1_000,
                paused: false,
            },
        });

        let line = online_now_playing_line(&session).expect("now playing line");
        assert!(line.contains("live.mp3"));
        assert!(line.contains("@host"));
    }

    #[test]
    fn online_now_playing_line_hidden_for_pause_and_stop_commands() {
        let mut session = OnlineSession::host("host");
        session.last_transport = Some(TransportEnvelope {
            seq: 1,
            origin_nickname: String::from("host"),
            command: TransportCommand::SetPaused { paused: true },
        });
        assert_eq!(online_now_playing_line(&session), None);

        session.last_transport = Some(TransportEnvelope {
            seq: 2,
            origin_nickname: String::from("host"),
            command: TransportCommand::StopPlayback,
        });
        assert_eq!(online_now_playing_line(&session), None);
    }
}
