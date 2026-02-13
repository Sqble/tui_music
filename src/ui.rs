use crate::audio::AudioEngine;
use crate::core::BrowserEntryKind;
use crate::core::HeaderSection;
use crate::core::StatsFilterFocus;
use crate::core::TuneCore;
use crate::model::Theme;
use crate::stats::{ListenEvent, StatsRange, StatsSnapshot, StatsSort, TrendSeries};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::sync::OnceLock;
use std::time::Duration;
use time::{OffsetDateTime, UtcOffset};

const APP_TITLE_WITH_VERSION: &str = "TuneTUI v1.0.0-alpha-2  ";

pub struct ActionPanelView {
    pub title: String,
    pub hint: String,
    pub options: Vec<String>,
    pub selected: usize,
}

#[derive(Clone, Copy)]
struct ThemePalette {
    bg: Color,
    panel_bg: Color,
    panel_alt_bg: Color,
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
            panel_bg: Color::Rgb(19, 29, 43),
            panel_alt_bg: Color::Rgb(24, 38, 58),
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
            panel_bg: Color::Rgb(8, 8, 8),
            panel_alt_bg: Color::Rgb(15, 15, 15),
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
            panel_bg: Color::Rgb(18, 16, 44),
            panel_alt_bg: Color::Rgb(27, 25, 61),
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
            panel_bg: Color::Rgb(8, 22, 8),
            panel_alt_bg: Color::Rgb(12, 30, 12),
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
            panel_bg: Color::Rgb(30, 6, 7),
            panel_alt_bg: Color::Rgb(44, 10, 11),
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
            panel_bg: Color::Rgb(51, 29, 68),
            panel_alt_bg: Color::Rgb(66, 38, 86),
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
        ])
        .split(frame.area());

    frame.render_widget(
        panel_block("Status", colors.panel_bg, colors.text, colors.border),
        vertical[0],
    );

    let header_inner = vertical[0].inner(Margin {
        vertical: 0,
        horizontal: 1,
    });
    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(header_inner);

    let header_left = Paragraph::new(Line::from(vec![
        Span::styled(
            APP_TITLE_WITH_VERSION,
            Style::default()
                .fg(colors.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("Tracks {}", core.tracks.len()),
            Style::default().fg(colors.text),
        ),
        Span::styled("  |  ", Style::default().fg(colors.muted)),
        Span::styled(
            format!("Mode {:?}", core.playback_mode),
            Style::default().fg(colors.alert),
        ),
    ]));
    frame.render_widget(header_left, header_chunks[0]);

    let header_right = Paragraph::new(header_section_line(core.header_section, &colors))
        .alignment(Alignment::Right);
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
                colors.panel_bg,
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
        let info_block = Paragraph::new(info_text)
            .block(panel_block(
                "Song Info",
                colors.panel_alt_bg,
                colors.text,
                colors.border,
            ))
            .wrap(Wrap { trim: true });
        frame.render_widget(info_block, body[1]);
    } else {
        match core.header_section {
            HeaderSection::Library => {}
            HeaderSection::Stats => {
                draw_stats_section(frame, &body, colors, core, stats_snapshot);
            }
            HeaderSection::Lyrics => {
                draw_placeholder_section(
                    frame,
                    &body,
                    colors,
                    "Lyrics",
                    "Lyrics view is not wired yet. Press Tab for Stats.",
                );
            }
            HeaderSection::Online => {
                draw_placeholder_section(
                    frame,
                    &body,
                    colors,
                    "Online",
                    "Online features are coming soon. Press Tab for Stats.",
                );
            }
        }
    }

    let timeline_text = timeline_line(audio, 26, 14);
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

    let key_hint = if core.header_section == HeaderSection::Stats {
        "Keys: Left/Right focus, Enter cycle, type filters, Backspace edit, Shift+Up top, Tab tabs"
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
    frame.render_widget(footer, vertical[3]);

    if let Some(panel) = action_panel {
        draw_action_panel(frame, panel, &colors);
    }
}

fn header_section_line(selected: HeaderSection, colors: &ThemePalette) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "Press Tab to switch",
        Style::default()
            .fg(colors.switch_hint)
            .add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::styled(" - ", Style::default().fg(colors.muted)));

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
                colors.panel_bg,
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
            colors.panel_alt_bg,
            colors.text,
            colors.border,
        )),
        body[1],
    );
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

    for (index, row) in snapshot.rows.iter().take(8).enumerate() {
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

    let left = Paragraph::new(left_lines)
        .block(panel_block(
            "Stats",
            colors.panel_bg,
            colors.text,
            colors.border,
        ))
        .scroll((core.stats_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(left, horizontal[0]);

    let mut recent_lines = vec![Line::from(Span::styled(
        "Recent plays",
        Style::default()
            .fg(colors.text)
            .add_modifier(Modifier::BOLD),
    ))];
    for event in &snapshot.recent {
        recent_lines.push(Line::from(Span::styled(
            format_recent_event(event, now),
            Style::default().fg(colors.muted),
        )));
    }
    if snapshot.recent.is_empty() {
        recent_lines.push(Line::from(Span::styled(
            "No recent listens yet.",
            Style::default().fg(colors.muted),
        )));
    }

    let right = Paragraph::new(recent_lines)
        .block(panel_block(
            "Recent Log",
            colors.panel_alt_bg,
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
        trend_label_at(trend, trend.start_epoch_seconds),
        trend_label_at(trend, trend.end_epoch_seconds),
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
        format_clock_label_local(epoch_seconds, span)
    } else {
        format_offset_label(
            epoch_seconds.saturating_sub(trend.start_epoch_seconds),
            trend.unit,
        )
    }
}

fn format_clock_label_local(epoch_seconds: i64, span_seconds: i64) -> String {
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

    if span_seconds > 86_400 {
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

fn local_utc_offset() -> UtcOffset {
    static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();
    *LOCAL_OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

fn short_metric_label(value: u64, sort: StatsSort) -> String {
    match sort {
        StatsSort::Plays => format!("{value}p"),
        StatsSort::ListenTime => {
            if value >= 60 {
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

fn draw_action_panel(frame: &mut Frame, panel: &ActionPanelView, colors: &ThemePalette) {
    let popup = centered_rect(frame.area(), 62, 58);
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = panel
        .options
        .iter()
        .map(|item| ListItem::new(Span::styled(item, Style::default().fg(colors.text))))
        .collect();

    let mut state = ListState::default();
    if !panel.options.is_empty() {
        state.select(Some(panel.selected.min(panel.options.len() - 1)));
    }

    let list = List::new(items)
        .block(panel_block(
            &panel.title,
            colors.popup_bg,
            colors.text,
            colors.border,
        ))
        .highlight_style(
            Style::default()
                .bg(colors.popup_selected_bg)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("-> ");
    frame.render_stateful_widget(list, popup, &mut state);

    let hint_area = Rect {
        x: popup.x.saturating_add(2),
        y: popup.y.saturating_add(popup.height.saturating_sub(2)),
        width: popup.width.saturating_sub(4),
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

fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
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

fn timeline_line(
    audio: &dyn AudioEngine,
    timeline_bar_width: usize,
    volume_bar_width: usize,
) -> String {
    let elapsed = audio.position().unwrap_or(Duration::from_secs(0));
    let total = audio.duration();
    let ratio = total.and_then(|duration| {
        let total_secs = duration.as_secs_f64();
        (total_secs > 0.0).then_some((elapsed.as_secs_f64() / total_secs).clamp(0.0, 1.0))
    });

    let volume_percent = (audio.volume() * 100.0).round() as u16;
    let volume_ratio = audio.volume().clamp(0.0, 1.0) as f64;

    format!(
        "{} / {} {}  |  Vol {} {:>3}%  +/- adjust  Shift fine  |  A/D scrub",
        format_duration(elapsed),
        total
            .map(format_duration)
            .unwrap_or_else(|| String::from("--:--")),
        progress_bar(ratio, timeline_bar_width),
        progress_bar(Some(volume_ratio), volume_bar_width),
        volume_percent
    )
}
