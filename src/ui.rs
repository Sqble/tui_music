use crate::audio::AudioEngine;
use crate::core::BrowserEntryKind;
use crate::core::TuneCore;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::time::Duration;

pub struct ActionPanelView {
    pub title: String,
    pub hint: String,
    pub options: Vec<String>,
    pub selected: usize,
}

const BG: Color = Color::Rgb(10, 15, 24);
const PANEL_BG: Color = Color::Rgb(19, 29, 43);
const PANEL_ALT_BG: Color = Color::Rgb(24, 38, 58);
const BORDER: Color = Color::Rgb(69, 121, 176);
const TEXT: Color = Color::Rgb(214, 228, 248);
const MUTED: Color = Color::Rgb(149, 173, 204);
const ACCENT: Color = Color::Rgb(100, 203, 184);
const ALERT: Color = Color::Rgb(249, 174, 88);

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
    frame.render_widget(
        Block::default().style(Style::default().bg(BG)),
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

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "TuneTUI  ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("Tracks {}", core.tracks.len()),
            Style::default().fg(TEXT),
        ),
        Span::styled("  |  ", Style::default().fg(MUTED)),
        Span::styled(
            format!("Mode {:?}", core.playback_mode),
            Style::default().fg(ALERT),
        ),
    ]))
    .block(panel_block("Status", PANEL_BG));
    frame.render_widget(header, vertical[0]);

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
                BrowserEntryKind::Back => Style::default().fg(ALERT),
                BrowserEntryKind::Folder => Style::default().fg(ACCENT),
                BrowserEntryKind::Playlist => Style::default().fg(Color::Rgb(156, 186, 255)),
                BrowserEntryKind::AllSongs => Style::default().fg(Color::Rgb(214, 205, 133)),
                BrowserEntryKind::Track => Style::default().fg(TEXT),
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(MUTED)),
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
        .block(panel_block(&library_title, PANEL_BG))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(34, 55, 82))
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("-> ");
    frame.render_stateful_widget(list, body[0], &mut state);

    let now_playing = core.current_path();
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

    let queue_position = core
        .current_queue_index
        .map(|idx| format!("{}/{}", idx + 1, core.queue.len()))
        .unwrap_or_else(|| format!("-/{}", core.queue.len()));

    let info_text = vec![
        Line::from(vec![
            Span::styled(
                "Now",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", now_playing_title),
                Style::default().fg(TEXT),
            ),
        ]),
        Line::from(Span::styled(
            format!("Artist  {now_playing_artist}"),
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            format!("Album   {now_playing_album}"),
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            format!("Queue   {queue_position}"),
            Style::default().fg(ALERT),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Selected",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {selected_title}"), Style::default().fg(TEXT)),
        ]),
        Line::from(Span::styled(
            format!("Artist  {selected_artist}"),
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            format!("Album   {selected_album}"),
            Style::default().fg(MUTED),
        )),
    ];
    let info_block = Paragraph::new(info_text)
        .block(panel_block("Song Info", PANEL_ALT_BG))
        .wrap(Wrap { trim: true });
    frame.render_widget(info_block, body[1]);

    let timeline_text = timeline_line(audio, 26, 14);
    let timeline_block = Paragraph::new(Span::styled(timeline_text, Style::default().fg(TEXT)))
        .block(panel_block("Timeline", PANEL_BG))
        .wrap(Wrap { trim: true });
    frame.render_widget(timeline_block, vertical[2]);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            "Keys: Enter play, Backspace back, n next, b previous, m cycle mode, / actions, t tray, Ctrl+C quit",
            Style::default().fg(MUTED),
        ),
        Span::styled("  |  ", Style::default().fg(MUTED)),
        Span::styled(core.status.as_str(), Style::default().fg(TEXT)),
    ]))
    .block(panel_block("Message", PANEL_BG));
    frame.render_widget(footer, vertical[3]);

    if let Some(panel) = action_panel {
        draw_action_panel(frame, panel);
    }
}

fn panel_block(title: &str, bg: Color) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(bg))
}

fn draw_action_panel(frame: &mut Frame, panel: &ActionPanelView) {
    let popup = centered_rect(frame.area(), 62, 58);
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = panel
        .options
        .iter()
        .map(|item| ListItem::new(Span::styled(item, Style::default().fg(TEXT))))
        .collect();

    let mut state = ListState::default();
    if !panel.options.is_empty() {
        state.select(Some(panel.selected.min(panel.options.len() - 1)));
    }

    let list = List::new(items)
        .block(panel_block(&panel.title, Color::Rgb(22, 33, 51)))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(45, 70, 99))
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
            Style::default().fg(MUTED),
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
