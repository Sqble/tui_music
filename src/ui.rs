use crate::audio::AudioEngine;
use crate::core::BrowserEntryKind;
use crate::core::HeaderSection;
use crate::core::LyricsMode;
use crate::core::StatsFilterFocus;
use crate::core::TuneCore;
use crate::model::{CoverArtTemplate, Theme};
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
}

pub struct JoinPromptModalView {
    pub invite_code: String,
    pub paste_selected: bool,
}

pub struct OnlineRoomDirectoryModalView {
    pub server_addr: String,
    pub search: String,
    pub selected: usize,
    pub rooms: Vec<String>,
}

pub struct OverlayViews<'a> {
    pub join_prompt_modal: Option<&'a JoinPromptModalView>,
    pub room_directory_modal: Option<&'a OnlineRoomDirectoryModalView>,
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
    switch_hint: Color,
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
            switch_hint: Color::Rgb(255, 122, 165),
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
            switch_hint: Color::Rgb(255, 133, 168),
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
            switch_hint: Color::Rgb(255, 140, 200),
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
            switch_hint: Color::Rgb(119, 255, 210),
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
            switch_hint: Color::Rgb(255, 109, 109),
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
            switch_hint: Color::Rgb(123, 248, 255),
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
        ])
        .split(frame.area());

    frame.render_widget(status_panel_block(&colors), vertical[0]);

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
        Span::styled(
            format!("Tracks {}", core.tracks.len()),
            Style::default().fg(colors.text),
        ),
        Span::styled("  |  ", Style::default().fg(colors.muted)),
        Span::styled(
            format!("Mode {:?}", core.playback_mode),
            Style::default().fg(colors.alert),
        ),
        Span::styled("  |  ", Style::default().fg(colors.muted)),
        Span::styled(
            if core.online.session.is_some() {
                "ONLINE"
            } else {
                "OFFLINE"
            },
            Style::default().fg(if core.online.session.is_some() {
                colors.accent
            } else {
                colors.muted
            }),
        ),
    ]));
    frame.render_widget(header_left, header_chunks[0]);

    let header_right =
        Paragraph::new(header_tabs_line(core.header_section, &colors)).alignment(Alignment::Right);
    frame.render_widget(header_right, header_chunks[1]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(66), Constraint::Percentage(34)])
        .split(vertical[1]);

    frame.render_widget(Clear, body[0]);
    frame.render_widget(Clear, body[1]);

    if core.header_section == HeaderSection::Library {
        let items: Vec<ListItem> = core
            .browser_entries
            .iter()
            .enumerate()
            .map(|entry| {
                let marker = if core.is_browser_entry_playing(entry.0) {
                    "  > "
                } else {
                    "    "
                };
                let entry = entry.1;
                let kind_style = match entry.kind {
                    BrowserEntryKind::Back => Style::default().fg(colors.alert),
                    BrowserEntryKind::Folder => Style::default().fg(colors.accent),
                    BrowserEntryKind::Playlist => Style::default().fg(colors.playlist),
                    BrowserEntryKind::AllSongs => Style::default().fg(colors.all_songs),
                    BrowserEntryKind::Track => Style::default().fg(colors.text),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(marker, Style::default().fg(colors.muted)),
                    Span::styled(entry.label.as_str(), kind_style),
                ]))
            })
            .collect();

        let mut state = ListState::default();
        state.select((!core.browser_entries.is_empty()).then_some(core.selected_browser));

        let library_title = if let Some(name) = &core.browser_playlist {
            format!("Library / Playlist / {name}")
        } else if core.browser_all_songs {
            String::from("Library / All Songs")
        } else if let Some(path) = &core.browser_path {
            format!("Library / {}", path.display())
        } else {
            String::from("Library")
        };

        let list = List::new(items)
            .block(panel_block(
                &library_title,
                colors.content_panel_bg,
                colors.text,
                colors.border,
            ))
            .highlight_style(
                Style::default()
                    .bg(colors.selected_bg)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("-> ");
        frame.render_stateful_widget(list, body[0], &mut state);

        let library_inner = body[0].inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        let library_viewport_lines = usize::from(library_inner.height);
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
            .and_then(|path| core.duration_seconds_for_path(path))
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
                draw_online_section(frame, &body, colors, core, overlays.room_code_revealed);
            }
        }
    }

    let timeline_text = timeline_line(audio, 42);
    let timeline_block = Paragraph::new(Span::styled(
        timeline_text,
        Style::default().fg(colors.text),
    ))
    .block(panel_block(
        "Timeline",
        colors.panel_bg,
        colors.text,
        colors.border,
    ))
    .wrap(Wrap { trim: true });
    frame.render_widget(timeline_block, vertical[2]);

    let control_text = control_line(audio, 16);
    let control_block =
        Paragraph::new(Span::styled(control_text, Style::default().fg(colors.text)))
            .block(panel_block(
                "Control",
                colors.panel_bg,
                colors.text,
                colors.border,
            ))
            .wrap(Wrap { trim: true });
    frame.render_widget(control_block, vertical[3]);

    let key_hint = if core.header_section == HeaderSection::Stats {
        "Keys: Left/Right focus, Enter cycle, type filters, Backspace edit, Shift+Up top, Tab tabs"
    } else if core.header_section == HeaderSection::Lyrics {
        "Keys: Ctrl+e edit/view, Up/Down line, Enter new line, Ctrl+t timestamp, / actions, Tab tabs"
    } else if core.header_section == HeaderSection::Online {
        "Keys: h host room, j join/browse rooms, l leave, o mode, q quality, t hide/show code, 2 copy code"
    } else {
        "Keys: Enter play, Backspace back, n next, b previous, a/d scrub, m cycle mode, / actions, t tray, Ctrl+C quit"
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
    frame.render_widget(footer, vertical[4]);

    if let Some(panel) = action_panel {
        draw_action_panel(frame, panel, &colors);
    }
    if let Some(join_prompt_modal) = overlays.join_prompt_modal {
        draw_join_prompt(frame, join_prompt_modal, &colors);
    }
    if let Some(room_directory_modal) = overlays.room_directory_modal {
        draw_room_directory_modal(frame, room_directory_modal, &colors);
    }
    if let Some(password_prompt) = overlays.online_password_prompt {
        draw_online_password_prompt(frame, password_prompt, &colors);
    }
    if let Some(host_invite_modal) = overlays.host_invite_modal {
        draw_host_invite_modal(frame, host_invite_modal, &colors);
    }
}

fn draw_room_directory_modal(
    frame: &mut Frame,
    modal: &OnlineRoomDirectoryModalView,
    colors: &ThemePalette,
) {
    let popup = centered_rect(frame.area(), 76, 58);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        panel_block(
            "Room Directory",
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
    let mut lines = vec![
        Line::from(Span::styled(
            format!("Server {}", modal.server_addr),
            Style::default().fg(colors.muted),
        )),
        Line::from(Span::styled(
            format!("Search: {}", modal.search),
            Style::default().fg(colors.accent),
        )),
        Line::from(""),
    ];
    if modal.rooms.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no rooms)",
            Style::default().fg(colors.muted),
        )));
    } else {
        for (index, room_line) in modal.rooms.iter().enumerate().take(14) {
            let style = if index == modal.selected {
                Style::default()
                    .fg(colors.text)
                    .bg(colors.popup_selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(colors.text)
            };
            lines.push(Line::from(Span::styled(room_line.as_str(), style)));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Type to search. Up/Down select. Enter join. Esc cancel.",
        Style::default().fg(colors.muted),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_join_prompt(frame: &mut Frame, modal: &JoinPromptModalView, colors: &ThemePalette) {
    let popup = centered_rect(frame.area(), 68, 34);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        panel_block(
            "Join Online Room",
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
            Span::styled("Server / Link", Style::default().fg(colors.muted)),
            Span::styled(": ", Style::default().fg(colors.muted)),
            Span::styled(modal.invite_code.as_str(), Style::default().fg(colors.text)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[ Join ]",
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
            "Type server address or room link. Tab/arrow selects button. Enter continues. Ctrl+V pastes.",
            Style::default()
                .fg(colors.accent)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_online_password_prompt(
    frame: &mut Frame,
    prompt: &OnlinePasswordPromptView,
    colors: &ThemePalette,
) {
    let popup = centered_rect(frame.area(), 58, 34);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        panel_block(&prompt.title, colors.popup_bg, colors.text, colors.border),
        popup,
    );
    let inner = popup.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let masked = if prompt.masked_input.is_empty() {
        String::from("(empty)")
    } else {
        prompt.masked_input.clone()
    };
    let lines = vec![
        Line::from(Span::styled(
            prompt.subtitle.as_str(),
            Style::default().fg(colors.muted),
        )),
        Line::from(""),
        Line::from(Span::styled(masked, Style::default().fg(colors.accent))),
        Line::from(""),
        Line::from(Span::styled(
            "Press Enter to continue, Esc to cancel.",
            Style::default().fg(colors.muted),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        inner,
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
            HeaderSection::Library => colors.accent,
            HeaderSection::Lyrics => Color::Rgb(231, 165, 255),
            HeaderSection::Stats => Color::Rgb(255, 200, 116),
            HeaderSection::Online => Color::Rgb(108, 221, 255),
        };
        let mut style = Style::default().fg(tab_color);
        if section == selected {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        spans.push(Span::styled(section.label(), style));
    }

    Line::from(spans)
}

fn header_tabs_width() -> u16 {
    let labels = [
        HeaderSection::Library.label(),
        HeaderSection::Lyrics.label(),
        HeaderSection::Stats.label(),
        HeaderSection::Online.label(),
    ];
    let labels_len: usize = labels.iter().map(|label| label.len()).sum();
    let separators_len = " -- ".len() * labels.len().saturating_sub(1);
    (labels_len + separators_len) as u16
}

fn header_switch_hint_line(colors: &ThemePalette) -> Line<'static> {
    Line::from(Span::styled(
        "Press Tab to switch",
        Style::default()
            .fg(colors.switch_hint)
            .add_modifier(Modifier::BOLD),
    ))
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
    room_code_revealed: bool,
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
        draw_placeholder_section(
            frame,
            body,
            colors,
            "Online",
            "No room connected. Press h to host or j to join.",
        );
        return;
    };

    let code_display = if room_code_revealed {
        session.room_code.clone()
    } else {
        String::from("[hidden]")
    };

    let mut left_lines = vec![Line::from(vec![
        Span::styled("Mode ", Style::default().fg(colors.muted)),
        Span::styled(session.mode.label(), Style::default().fg(colors.text)),
        Span::styled("  |  Stream ", Style::default().fg(colors.muted)),
        Span::styled(session.quality.label(), Style::default().fg(colors.alert)),
    ])];

    left_lines.push(Line::from(vec![
        Span::styled("Room code ", Style::default().fg(colors.muted)),
        Span::styled(
            code_display,
            Style::default()
                .fg(colors.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  [t] show/hide  [2] copy",
            Style::default().fg(colors.muted),
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

    if let Some(local) = session.local_participant() {
        left_lines.push(Line::from(Span::styled(
            format!(
                "You {}  ping {}ms  manual {}ms  effective {}ms  auto {}",
                local.nickname,
                local.ping_ms,
                local.manual_extra_delay_ms,
                local.effective_delay_ms(),
                if local.auto_ping_delay { "on" } else { "off" }
            ),
            Style::default().fg(colors.text),
        )));
    }

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

    let mut right_lines = vec![Line::from(Span::styled(
        "Shared Queue",
        Style::default()
            .fg(colors.text)
            .add_modifier(Modifier::BOLD),
    ))];
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
        "- {}{}  ping {}ms  delay {}ms",
        participant.nickname,
        tags,
        participant.ping_ms,
        participant.effective_delay_ms()
    )
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

    let label_width = 4_usize;
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

fn status_panel_block(colors: &ThemePalette) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Status ",
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(APP_VERSION, Style::default().fg(colors.muted)))
        .title_bottom(header_switch_hint_line(colors).alignment(Alignment::Right))
        .border_style(Style::default().fg(colors.border))
        .style(Style::default().bg(colors.panel_bg))
}

fn draw_action_panel(frame: &mut Frame, panel: &ActionPanelView, colors: &ThemePalette) {
    let popup = centered_rect(frame.area(), 62, 58);
    frame.render_widget(Clear, popup);

    let panel_block_widget = panel_block(&panel.title, colors.popup_bg, colors.text, colors.border);
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

    let items: Vec<ListItem> = panel
        .options
        .iter()
        .map(|item| ListItem::new(Span::styled(item, Style::default().fg(colors.text))))
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

fn control_line(audio: &dyn AudioEngine, volume_bar_width: usize) -> String {
    let volume_percent = (audio.volume() * 100.0).round() as u16;
    let volume_ratio = audio.volume().clamp(0.0, 1.0) as f64;

    format!(
        "Vol {} {:>3}%  +/- adjust  Shift fine  |  A/D scrub",
        progress_bar(Some(volume_ratio), volume_bar_width),
        volume_percent
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn action_panel_scrollbar_only_when_overflow_exists() {
        assert!(list_overflows(8, 5));
        assert!(!list_overflows(5, 5));
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
    fn control_line_shows_volume_and_scrub_hints() {
        let mut audio = crate::audio::NullAudioEngine::new();
        audio.set_volume(1.2);
        let line = control_line(&audio, 10);
        assert!(line.contains("Vol"));
        assert!(line.contains("A/D scrub"));
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
}
