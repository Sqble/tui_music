use crate::audio::AudioEngine;
use crate::core::TuneCore;
use crate::core::{BrowserEntry, BrowserEntryKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use std::time::Duration;

pub fn library_rect(area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(vertical[1]);

    body[0]
}

pub fn draw(
    frame: &mut Frame,
    core: &TuneCore,
    audio: &dyn AudioEngine,
    command_buffer: &str,
    command_mode: bool,
) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let header = Paragraph::new(format!(
        "TuneTUI | Tracks: {} | Mode: {:?}",
        core.tracks.len(),
        core.playback_mode
    ))
    .block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(header, vertical[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(vertical[1]);

    let items: Vec<ListItem> = core
        .browser_entries
        .iter()
        .map(|entry| {
            let marker = if is_currently_playing(entry, core.current_path()) {
                ">>"
            } else {
                "  "
            };
            ListItem::new(format!("{marker} {}", entry.label))
        })
        .collect();

    let mut state = ListState::default();
    state.select((!core.browser_entries.is_empty()).then_some(core.selected_browser));

    let library_title = if let Some(name) = &core.browser_playlist {
        format!("Library: Playlist/{name}")
    } else if let Some(path) = &core.browser_path {
        format!("Library: {}", path.display())
    } else {
        String::from("Library")
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(library_title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
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
        .filter(|entry| entry.kind == crate::core::BrowserEntryKind::Track)
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
    let selected_file = selected_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "-".to_string());

    let queue_position = core
        .current_queue_index
        .map(|idx| format!("{}/{}", idx + 1, core.queue.len()))
        .unwrap_or_else(|| format!("-/{}", core.queue.len()));

    let info_text = format!(
        "Now Playing\nTitle: {now_playing_title}\nArtist: {now_playing_artist}\nAlbum: {now_playing_album}\nQueue: {queue_position}\nMode: {:?}\n\nSelected\nTitle: {selected_title}\nArtist: {selected_artist}\nAlbum: {selected_album}\nFile: {selected_file}",
        core.playback_mode
    );
    let info_block = Paragraph::new(info_text)
        .block(Block::default().borders(Borders::ALL).title("Song Info"))
        .wrap(Wrap { trim: true });
    frame.render_widget(info_block, body[1]);

    let timeline_text = timeline_line(audio, 30, 16);
    let timeline_block = Paragraph::new(timeline_text)
        .block(Block::default().borders(Borders::ALL).title("Timeline"))
        .wrap(Wrap { trim: true });
    frame.render_widget(timeline_block, vertical[2]);

    let command_title = if command_mode {
        "Command (:help)"
    } else {
        "Keys: Enter open/play, Backspace back, n next, m mode, t minimize, +/- volume, : command, Ctrl+C quit"
    };

    let command = Paragraph::new(if command_mode {
        format!(":{command_buffer}")
    } else {
        String::from("Press ':' to enter command mode")
    })
    .block(Block::default().borders(Borders::ALL).title(command_title))
    .wrap(Wrap { trim: true });
    frame.render_widget(command, vertical[3]);

    let footer = Paragraph::new(core.status.as_str())
        .block(Block::default().borders(Borders::ALL).title("Message"));
    frame.render_widget(footer, vertical[4]);
}

fn is_currently_playing(entry: &BrowserEntry, current_path: Option<&std::path::Path>) -> bool {
    entry.kind == BrowserEntryKind::Track
        && current_path
            .map(|path| path == entry.path.as_path())
            .unwrap_or(false)
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
        "{} / {} {}  |  Vol {} {:>3}%",
        format_duration(elapsed),
        total
            .map(format_duration)
            .unwrap_or_else(|| String::from("--:--")),
        progress_bar(ratio, timeline_bar_width),
        progress_bar(Some(volume_ratio), volume_bar_width),
        volume_percent
    )
}
