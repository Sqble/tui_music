use crate::audio::AudioEngine;
use crate::core::BrowserEntryKind;
use crate::core::HeaderSection;
use crate::core::TuneCore;
use crate::model::Theme;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::time::Duration;

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

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            "Keys: Enter play, Backspace back, n next, b previous, m cycle mode, / actions, t tray, Ctrl+C quit",
            Style::default().fg(colors.muted),
        ),
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
        "Press E to switch",
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
    bar.push_str(&"#".repeat(filled));
    bar.push_str(&"-".repeat(width.saturating_sub(filled)));
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
        "{} / {} {}  |  Vol {} {:>3}%  +/- adjust  Shift fine",
        format_duration(elapsed),
        total
            .map(format_duration)
            .unwrap_or_else(|| String::from("--:--")),
        progress_bar(ratio, timeline_bar_width),
        progress_bar(Some(volume_ratio), volume_bar_width),
        volume_percent
    )
}
