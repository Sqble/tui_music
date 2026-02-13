use crate::audio::{AudioEngine, NullAudioEngine, WasapiAudioEngine};
use crate::config;
use crate::core::{HeaderSection, StatsFilterFocus, TuneCore};
use crate::model::{PlaybackMode, Theme};
use crate::stats::{self, ListenSessionRecord, StatsStore};
use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(windows)]
const APP_INSTANCE_MUTEX: &str = "TuneTui.SingleInstance";
#[cfg(windows)]
const APP_CONSOLE_TITLE: &str = "TuneTUI";
const MAX_VOLUME: f32 = 2.5;
const VOLUME_STEP_COARSE: f32 = 0.05;
const VOLUME_STEP_FINE: f32 = 0.01;

#[derive(Debug, Clone)]
struct ActiveListenSession {
    track_path: PathBuf,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    started_at_epoch_seconds: i64,
    playing_started_at: Option<Instant>,
    listened: Duration,
    last_position: Option<Duration>,
    duration: Option<Duration>,
}

#[derive(Debug, Default)]
struct ListenTracker {
    active: Option<ActiveListenSession>,
}

impl ListenTracker {
    fn reset(&mut self) {
        self.active = None;
    }

    fn tick(&mut self, core: &TuneCore, audio: &dyn AudioEngine, stats: &mut StatsStore) -> bool {
        let mut wrote_event = false;
        let current_track = audio.current_track().map(Path::to_path_buf);
        let paused = audio.is_paused();

        let should_finalize = self
            .active
            .as_ref()
            .is_some_and(|active| current_track.as_ref() != Some(&active.track_path));
        if should_finalize {
            wrote_event = self.finalize_active(stats) || wrote_event;
        }

        if current_track.is_none() {
            return wrote_event;
        }

        if self.active.is_none() {
            let path = current_track.expect("checked some");
            let now = Instant::now();
            self.active = Some(ActiveListenSession {
                title: core.title_for_path(&path).unwrap_or_else(|| {
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("-")
                        .to_string()
                }),
                artist: core.artist_for_path(&path).map(ToOwned::to_owned),
                album: core.album_for_path(&path).map(ToOwned::to_owned),
                track_path: path,
                started_at_epoch_seconds: stats::now_epoch_seconds(),
                playing_started_at: (!paused).then_some(now),
                listened: Duration::ZERO,
                last_position: audio.position(),
                duration: audio.duration(),
            });
            return wrote_event;
        }

        if let Some(active) = self.active.as_mut() {
            active.last_position = audio.position().or(active.last_position);
            active.duration = audio.duration().or(active.duration);
            if paused {
                if let Some(started) = active.playing_started_at.take() {
                    active.listened = active.listened.saturating_add(started.elapsed());
                }
            } else if active.playing_started_at.is_none() {
                active.playing_started_at = Some(Instant::now());
            }
        }

        wrote_event
    }

    fn finalize_active(&mut self, stats: &mut StatsStore) -> bool {
        let mut active = match self.active.take() {
            Some(active) => active,
            None => return false,
        };

        if let Some(started) = active.playing_started_at.take() {
            active.listened = active.listened.saturating_add(started.elapsed());
        }

        let listened_seconds = duration_to_recorded_seconds(active.listened);
        let completed =
            active
                .duration
                .zip(active.last_position)
                .is_some_and(|(duration, position)| {
                    position >= duration
                        || duration.saturating_sub(position) <= Duration::from_secs(1)
                });

        stats.record_listen(ListenSessionRecord {
            track_path: active.track_path,
            title: active.title,
            artist: active.artist,
            album: active.album,
            started_at_epoch_seconds: active.started_at_epoch_seconds,
            listened_seconds,
            completed,
            duration_seconds: active.duration.map(|duration| duration.as_secs() as u32),
        });
        listened_seconds > 0
    }
}

fn duration_to_recorded_seconds(duration: Duration) -> u32 {
    if duration.is_zero() {
        return 0;
    }
    let secs = duration.as_secs();
    let has_subsec = duration.subsec_nanos() > 0;
    let rounded = if has_subsec {
        secs.saturating_add(1)
    } else {
        secs
    };
    rounded.min(u64::from(u32::MAX)) as u32
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActionPanelState {
    Closed,
    Root { selected: usize },
    Mode { selected: usize },
    PlaylistPlay { selected: usize },
    PlaylistAdd { selected: usize },
    PlaylistCreate { selected: usize, input: String },
    PlaylistRemove { selected: usize },
    AudioSettings { selected: usize },
    AudioOutput { selected: usize },
    PlaybackSettings { selected: usize },
    ThemeSettings { selected: usize },
    AddDirectory { selected: usize, input: String },
    RemoveDirectory { selected: usize },
}

impl ActionPanelState {
    fn open(&mut self) {
        *self = Self::Root { selected: 0 };
    }

    fn close(&mut self) {
        *self = Self::Closed;
    }

    fn is_open(&self) -> bool {
        !matches!(self, Self::Closed)
    }

    fn to_view(
        &self,
        core: &TuneCore,
        audio: &dyn AudioEngine,
    ) -> Option<crate::ui::ActionPanelView> {
        match self {
            Self::Closed => None,
            Self::Root { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Actions"),
                hint: String::from("Enter select  Esc close  Up/Down navigate"),
                options: vec![
                    String::from("Add directory"),
                    String::from("Add selected item to playlist"),
                    String::from("Set playback mode"),
                    String::from("Playback settings"),
                    String::from("Play playlist"),
                    String::from("Remove selected from playlist"),
                    String::from("Create playlist"),
                    String::from("Remove playlist"),
                    String::from("Remove directory"),
                    String::from("Rescan library"),
                    String::from("Audio driver settings"),
                    String::from("Theme"),
                    String::from("Save state"),
                    String::from("Clear listen history (backup)"),
                    String::from("Minimize to tray"),
                    String::from("Close panel"),
                ],
                selected: *selected,
            }),
            Self::Mode { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Playback Mode"),
                hint: String::from("Enter apply  Backspace back"),
                options: vec![
                    String::from("Normal"),
                    String::from("Shuffle"),
                    String::from("Loop playlist"),
                    String::from("Loop single track"),
                ],
                selected: *selected,
            }),
            Self::PlaylistPlay { selected } => {
                let playlists = sorted_playlist_names(core);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Play Playlist"),
                    hint: String::from("Enter play  Backspace back"),
                    options: if playlists.is_empty() {
                        vec![String::from("(no playlists)")]
                    } else {
                        playlists
                    },
                    selected: *selected,
                })
            }
            Self::PlaylistAdd { selected } => {
                let playlists = sorted_playlist_names(core);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Add To Playlist"),
                    hint: String::from("Enter add  Backspace back"),
                    options: if playlists.is_empty() {
                        vec![String::from("(no playlists)")]
                    } else {
                        playlists
                    },
                    selected: *selected,
                })
            }
            Self::PlaylistCreate { selected, input } => Some(crate::ui::ActionPanelView {
                title: String::from("Create Playlist"),
                hint: String::from("Type name + Enter  Backspace back"),
                options: vec![if input.is_empty() {
                    String::from("Name: ")
                } else {
                    format!("Name: {input}")
                }],
                selected: *selected,
            }),
            Self::PlaylistRemove { selected } => {
                let playlists = sorted_playlist_names(core);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Remove Playlist"),
                    hint: String::from("Enter remove  Backspace back"),
                    options: if playlists.is_empty() {
                        vec![String::from("(no playlists)")]
                    } else {
                        playlists
                    },
                    selected: *selected,
                })
            }
            Self::AudioSettings { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Audio Driver Settings"),
                hint: String::from("Enter select  Backspace back"),
                options: vec![
                    String::from("Reload audio driver"),
                    String::from("Select output speaker"),
                    String::from("Back"),
                ],
                selected: *selected,
            }),
            Self::AudioOutput { selected } => {
                let options = audio_output_options(audio);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Output Speaker"),
                    hint: String::from("Enter apply  Backspace back"),
                    options,
                    selected: *selected,
                })
            }
            Self::PlaybackSettings { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Playback Settings"),
                hint: String::from("Enter toggle/select  Backspace back"),
                options: playback_settings_options(core),
                selected: *selected,
            }),
            Self::ThemeSettings { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Theme"),
                hint: String::from("Enter apply  Backspace back"),
                options: theme_options(core.theme),
                selected: *selected,
            }),
            Self::AddDirectory { selected, input } => Some(crate::ui::ActionPanelView {
                title: String::from("Add Directory"),
                hint: String::from("Enter choose folder  Down type path"),
                options: vec![
                    String::from("Choose folder externally"),
                    if input.is_empty() {
                        String::from("Path: ")
                    } else {
                        format!("Path: {input}")
                    },
                ],
                selected: *selected,
            }),
            Self::RemoveDirectory { selected } => {
                let paths = sorted_folder_paths(core);
                Some(crate::ui::ActionPanelView {
                    title: String::from("Remove Directory"),
                    hint: String::from("Enter remove  Backspace back"),
                    options: if paths.is_empty() {
                        vec![String::from("(no folders)")]
                    } else {
                        paths
                            .iter()
                            .map(|path| {
                                crate::config::sanitize_display_text(&path.display().to_string())
                            })
                            .collect()
                    },
                    selected: *selected,
                })
            }
        }
    }
}

pub fn run() -> Result<()> {
    #[cfg(windows)]
    let _single_instance = match ensure_single_instance() {
        Ok(Some(guard)) => guard,
        Ok(None) => return Ok(()),
        Err(err) => return Err(err),
    };

    let state = config::load_state()?;
    let preferred_output = state.selected_output_device.clone();
    let mut core = TuneCore::from_persisted(state);
    let mut stats_store = stats::load_stats().unwrap_or_default();
    let mut listen_tracker = ListenTracker::default();

    let mut audio: Box<dyn AudioEngine> = match WasapiAudioEngine::new() {
        Ok(engine) => Box::new(engine),
        Err(_) => Box::new(NullAudioEngine::new()),
    };

    apply_audio_preferences_from_core(&core, &mut *audio);
    apply_saved_audio_output(&mut core, &mut *audio, preferred_output);

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut action_panel = ActionPanelState::Closed;
    let mut last_tick = Instant::now();
    let mut library_rect = ratatui::prelude::Rect::default();
    let mut stats_enabled_last = core.stats_enabled;

    let result: Result<()> = loop {
        pump_tray_events(&mut core);
        audio.tick();
        if core.stats_enabled && listen_tracker.tick(&core, &*audio, &mut stats_store) {
            let _ = stats::save_stats(&stats_store);
        }
        if stats_enabled_last
            && !core.stats_enabled
            && listen_tracker.finalize_active(&mut stats_store)
        {
            let _ = stats::save_stats(&stats_store);
        }
        if core.clear_stats_requested {
            listen_tracker.reset();
            stats_store.clear_history();
            if let Err(err) = stats::save_stats(&stats_store) {
                core.status = format!("Failed to clear listen history: {err}");
            } else {
                core.status = String::from("Listen history cleared (backup saved)");
            }
            core.clear_stats_requested = false;
            core.dirty = true;
        }
        stats_enabled_last = core.stats_enabled;
        maybe_auto_advance_track(&mut core, &mut *audio);

        if core.dirty || last_tick.elapsed() > Duration::from_millis(250) {
            terminal.draw(|frame| {
                library_rect = crate::ui::library_rect(frame.area());
                let panel_view = action_panel.to_view(&core, &*audio);
                let stats_snapshot = (core.header_section == HeaderSection::Stats).then(|| {
                    stats_store.query(
                        &crate::stats::StatsQuery {
                            range: core.stats_range,
                            sort: core.stats_sort,
                            artist_filter: core.stats_artist_filter.clone(),
                            album_filter: core.stats_album_filter.clone(),
                            search: core.stats_search.clone(),
                        },
                        stats::now_epoch_seconds(),
                    )
                });
                crate::ui::draw(
                    frame,
                    &core,
                    &*audio,
                    panel_view.as_ref(),
                    stats_snapshot.as_ref(),
                )
            })?;
            core.dirty = false;
            last_tick = Instant::now();
        }

        if !event::poll(Duration::from_millis(33))? {
            continue;
        }

        let event = event::read()?;
        if let Event::Mouse(mouse) = event {
            handle_mouse(&mut core, mouse, library_rect);
            continue;
        }

        let Event::Key(key) = event else {
            continue;
        };

        if key.kind != KeyEventKind::Press {
            continue;
        }

        if action_panel.is_open() {
            handle_action_panel_input(&mut core, &mut *audio, &mut action_panel, key.code);
            continue;
        }

        if handle_stats_inline_input(&mut core, key) {
            continue;
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break Ok(()),
            KeyCode::Down => core.select_next(),
            KeyCode::Up => core.select_prev(),
            KeyCode::Enter => {
                if let Some(err) = core
                    .activate_selected()
                    .and_then(|path| audio.play(&path).err())
                {
                    core.status = concise_audio_error(&err);
                }
            }
            KeyCode::Left | KeyCode::Backspace => core.navigate_back(),
            KeyCode::Char(' ') => {
                if audio.is_paused() {
                    audio.resume();
                    core.status = String::from("Resumed");
                } else {
                    audio.pause();
                    core.status = String::from("Paused");
                }
                core.dirty = true;
            }
            KeyCode::Char('n') => {
                if let Some(err) = core
                    .next_track_path()
                    .and_then(|path| audio.play(&path).err())
                {
                    core.status = concise_audio_error(&err);
                    core.dirty = true;
                }
            }
            KeyCode::Char('b') => {
                if let Some(err) = core
                    .prev_track_path()
                    .and_then(|path| audio.play(&path).err())
                {
                    core.status = concise_audio_error(&err);
                    core.dirty = true;
                }
            }
            KeyCode::Char('m') => {
                core.cycle_mode();
                auto_save_state(&mut core, &*audio);
            }
            KeyCode::Tab => core.cycle_header_section(),
            KeyCode::Char('t') => {
                minimize_to_tray();
                core.status = String::from("Minimized to tray");
                core.dirty = true;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                let step = if key.code == KeyCode::Char('+')
                    || key.modifiers.contains(KeyModifiers::SHIFT)
                {
                    VOLUME_STEP_FINE
                } else {
                    VOLUME_STEP_COARSE
                };
                let next = (audio.volume() + step).clamp(0.0, MAX_VOLUME);
                audio.set_volume(next);
                core.status = format!("Volume: {}%", (next * 100.0).round() as u16);
                core.dirty = true;
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                let step = if key.code == KeyCode::Char('_')
                    || key.modifiers.contains(KeyModifiers::SHIFT)
                {
                    VOLUME_STEP_FINE
                } else {
                    VOLUME_STEP_COARSE
                };
                let next = (audio.volume() - step).clamp(0.0, MAX_VOLUME);
                audio.set_volume(next);
                core.status = format!("Volume: {}%", (next * 100.0).round() as u16);
                core.dirty = true;
            }
            KeyCode::Char('r') => core.rescan(),
            KeyCode::Char('s') => {
                if let Err(err) = save_state_with_audio(&mut core, &*audio) {
                    core.status = format!("save error: {err:#}");
                    core.dirty = true;
                }
            }
            KeyCode::Char('/') => {
                action_panel.open();
                core.dirty = true;
            }
            _ => {}
        }
    };

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    cleanup_tray();
    terminal.show_cursor()?;
    if listen_tracker.finalize_active(&mut stats_store) {
        let _ = stats::save_stats(&stats_store);
    }
    let save_result = save_state_with_audio(&mut core, &*audio);
    result?;
    save_result?;
    Ok(())
}

#[cfg(windows)]
struct SingleInstanceGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn ensure_single_instance() -> anyhow::Result<Option<SingleInstanceGuard>> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let mutex_name = to_wide(APP_INSTANCE_MUTEX);
    let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 1, mutex_name.as_ptr()) };
    if handle.is_null() {
        return Err(anyhow::anyhow!(
            "Failed to initialize single-instance mutex"
        ));
    }

    let already_exists = unsafe { GetLastError() == ERROR_ALREADY_EXISTS };
    if already_exists {
        focus_existing_instance();
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        return Ok(None);
    }

    set_console_title(APP_CONSOLE_TITLE);
    Ok(Some(SingleInstanceGuard(handle)))
}

#[cfg(windows)]
fn set_console_title(title: &str) {
    use windows_sys::Win32::System::Console::SetConsoleTitleW;

    let title_wide = to_wide(title);
    unsafe {
        SetConsoleTitleW(title_wide.as_ptr());
    }
}

#[cfg(windows)]
fn focus_existing_instance() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };

    let class_name = to_wide("ConsoleWindowClass");
    let title = to_wide(APP_CONSOLE_TITLE);
    let hwnd = unsafe { FindWindowW(class_name.as_ptr(), title.as_ptr()) };
    if !hwnd.is_null() {
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
    }
}

fn maybe_auto_advance_track(core: &mut TuneCore, audio: &mut dyn AudioEngine) {
    if audio.current_track().is_none() || audio.is_paused() {
        return;
    }

    let crossfade_triggered = should_trigger_crossfade_advance(audio);
    if crossfade_triggered && audio.crossfade_queued_track().is_some() {
        return;
    }

    if !audio.is_finished() && !crossfade_triggered {
        return;
    }

    if let Some(path) = core.next_track_path() {
        let result = if crossfade_triggered {
            audio.queue_crossfade(&path)
        } else {
            audio.play(&path)
        };
        if let Err(err) = result {
            core.status = concise_audio_error(&err);
            core.dirty = true;
        }
    } else if audio.is_finished() {
        audio.stop();
        core.status = String::from("Reached end of queue");
        core.dirty = true;
    }
}

fn handle_stats_inline_input(core: &mut TuneCore, key: KeyEvent) -> bool {
    if core.header_section != HeaderSection::Stats {
        return false;
    }

    if key.code == KeyCode::Up && key.modifiers.contains(KeyModifiers::SHIFT) {
        core.stats_scroll = 0;
        core.stats_focus = StatsFilterFocus::Range(core_range_index(core.stats_range));
        core.status = String::from("Stats view reset to filters");
        core.dirty = true;
        return true;
    }

    match key.code {
        KeyCode::Left => {
            if core.stats_scroll > 0 && matches!(core.stats_focus, StatsFilterFocus::Search) {
                stats_scroll_up(core);
                return true;
            }
            move_stats_focus_or_value(core, false)
        }
        KeyCode::Right => {
            if matches!(core.stats_focus, StatsFilterFocus::Search) {
                stats_scroll_down(core);
                return true;
            }
            move_stats_focus_or_value(core, true)
        }
        KeyCode::Up => {
            if core.stats_scroll > 0 && matches!(core.stats_focus, StatsFilterFocus::Search) {
                stats_scroll_up(core);
                return true;
            }
            move_stats_row(core, false)
        }
        KeyCode::Down => {
            if matches!(core.stats_focus, StatsFilterFocus::Search) {
                stats_scroll_down(core);
                return true;
            }
            move_stats_row(core, true)
        }
        KeyCode::Enter => {
            match core.stats_focus {
                StatsFilterFocus::Range(index) => {
                    let next = (index + 1) % 4;
                    core.stats_focus = StatsFilterFocus::Range(next);
                    set_stats_range_by_index(core, next);
                }
                StatsFilterFocus::Sort(index) => {
                    let next = 1_u8.saturating_sub(index.min(1));
                    core.stats_focus = StatsFilterFocus::Sort(next);
                    set_stats_sort_by_index(core, next);
                }
                StatsFilterFocus::Artist | StatsFilterFocus::Album | StatsFilterFocus::Search => {}
            }
            true
        }
        KeyCode::Backspace => {
            let target = match core.stats_focus {
                StatsFilterFocus::Artist => Some(&mut core.stats_artist_filter),
                StatsFilterFocus::Album => Some(&mut core.stats_album_filter),
                StatsFilterFocus::Search => Some(&mut core.stats_search),
                StatsFilterFocus::Range(_) | StatsFilterFocus::Sort(_) => None,
            };

            if let Some(text) = target {
                if !text.is_empty() {
                    text.pop();
                    core.status = format!("{} filter updated", core.stats_focus.label());
                    core.dirty = true;
                }
                return true;
            }

            true
        }
        KeyCode::Char(ch) => {
            let target = match core.stats_focus {
                StatsFilterFocus::Artist => Some(&mut core.stats_artist_filter),
                StatsFilterFocus::Album => Some(&mut core.stats_album_filter),
                StatsFilterFocus::Search => Some(&mut core.stats_search),
                StatsFilterFocus::Range(_) | StatsFilterFocus::Sort(_) => None,
            };

            if let Some(text) = target {
                text.push(ch);
                core.status = format!("{} filter updated", core.stats_focus.label());
                core.dirty = true;
                return true;
            }

            false
        }
        KeyCode::Delete => {
            match core.stats_focus {
                StatsFilterFocus::Artist => core.stats_artist_filter.clear(),
                StatsFilterFocus::Album => core.stats_album_filter.clear(),
                StatsFilterFocus::Search => core.stats_search.clear(),
                StatsFilterFocus::Range(_) | StatsFilterFocus::Sort(_) => return false,
            }
            core.status = format!("{} filter cleared", core.stats_focus.label());
            core.dirty = true;
            true
        }
        _ => false,
    }
}

fn stats_scroll_down(core: &mut TuneCore) {
    core.stats_scroll = core.stats_scroll.saturating_add(1);
    core.dirty = true;
}

fn stats_scroll_up(core: &mut TuneCore) {
    core.stats_scroll = core.stats_scroll.saturating_sub(1);
    core.dirty = true;
}

fn move_stats_focus_or_value(core: &mut TuneCore, forward: bool) -> bool {
    match core.stats_focus {
        StatsFilterFocus::Range(index) => {
            let next = if forward {
                (index + 1) % 4
            } else {
                (index + 3) % 4
            };
            core.stats_focus = StatsFilterFocus::Range(next);
            set_stats_range_by_index(core, next);
            true
        }
        StatsFilterFocus::Sort(index) => {
            let next = 1_u8.saturating_sub(index.min(1));
            core.stats_focus = StatsFilterFocus::Sort(next);
            set_stats_sort_by_index(core, next);
            true
        }
        StatsFilterFocus::Artist | StatsFilterFocus::Album | StatsFilterFocus::Search => {
            move_stats_row(core, forward)
        }
    }
}

fn move_stats_row(core: &mut TuneCore, forward: bool) -> bool {
    core.stats_focus = match core.stats_focus {
        StatsFilterFocus::Range(_) => {
            if forward {
                StatsFilterFocus::Sort(core_sort_index(core.stats_sort))
            } else {
                StatsFilterFocus::Search
            }
        }
        StatsFilterFocus::Sort(_) => {
            if forward {
                StatsFilterFocus::Artist
            } else {
                StatsFilterFocus::Range(core_range_index(core.stats_range))
            }
        }
        StatsFilterFocus::Artist => {
            if forward {
                StatsFilterFocus::Album
            } else {
                StatsFilterFocus::Sort(core_sort_index(core.stats_sort))
            }
        }
        StatsFilterFocus::Album => {
            if forward {
                StatsFilterFocus::Search
            } else {
                StatsFilterFocus::Artist
            }
        }
        StatsFilterFocus::Search => {
            if forward {
                StatsFilterFocus::Range(core_range_index(core.stats_range))
            } else {
                StatsFilterFocus::Album
            }
        }
    };
    core.dirty = true;
    true
}

fn set_stats_range_by_index(core: &mut TuneCore, index: u8) {
    core.stats_range = match index {
        0 => crate::stats::StatsRange::Today,
        1 => crate::stats::StatsRange::Days7,
        2 => crate::stats::StatsRange::Days30,
        _ => crate::stats::StatsRange::Lifetime,
    };
    core.dirty = true;
}

fn set_stats_sort_by_index(core: &mut TuneCore, index: u8) {
    core.stats_sort = if index == 0 {
        crate::stats::StatsSort::Plays
    } else {
        crate::stats::StatsSort::ListenTime
    };
    core.dirty = true;
}

fn core_range_index(range: crate::stats::StatsRange) -> u8 {
    match range {
        crate::stats::StatsRange::Today => 0,
        crate::stats::StatsRange::Days7 => 1,
        crate::stats::StatsRange::Days30 => 2,
        crate::stats::StatsRange::Lifetime => 3,
    }
}

fn core_sort_index(sort: crate::stats::StatsSort) -> u8 {
    match sort {
        crate::stats::StatsSort::Plays => 0,
        crate::stats::StatsSort::ListenTime => 1,
    }
}

fn should_trigger_crossfade_advance(audio: &dyn AudioEngine) -> bool {
    let crossfade_seconds = audio.crossfade_seconds();
    if crossfade_seconds == 0 {
        return false;
    }

    let Some(position) = audio.position() else {
        return false;
    };
    let Some(duration) = audio.duration() else {
        return false;
    };
    if duration <= position {
        return false;
    }

    let remaining = duration.saturating_sub(position);
    remaining <= Duration::from_secs(u64::from(crossfade_seconds))
}

fn concise_audio_error(err: &anyhow::Error) -> String {
    let message = err.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("device") && (lower.contains("no longer") || lower.contains("unavailable")) {
        return String::from("Audio device unavailable. Use / -> Audio driver settings -> Reload");
    }
    format!("Playback failed: {message}")
}

fn save_state_with_audio(core: &mut TuneCore, audio: &dyn AudioEngine) -> Result<()> {
    persist_state_with_audio(core, audio, true)
}

fn auto_save_state(core: &mut TuneCore, audio: &dyn AudioEngine) {
    let _ = persist_state_with_audio(core, audio, false);
}

fn persist_state_with_audio(
    core: &mut TuneCore,
    audio: &dyn AudioEngine,
    show_status: bool,
) -> Result<()> {
    let state = persisted_state_with_audio(core, audio);
    config::save_state(&state)?;
    if show_status {
        core.status = String::from("State saved");
        core.dirty = true;
    }
    Ok(())
}

fn persisted_state_with_audio(
    core: &TuneCore,
    audio: &dyn AudioEngine,
) -> crate::model::PersistedState {
    let mut state = core.persisted_state();
    state.selected_output_device = audio.selected_output_device();
    state
}

fn apply_saved_audio_output(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    preferred_output: Option<String>,
) {
    let Some(preferred_output) = preferred_output else {
        return;
    };

    if let Err(err) = audio.set_output_device(Some(preferred_output.as_str())) {
        core.status = format!(
            "Saved output '{}' unavailable. Using default. / -> Audio driver settings",
            preferred_output
        );
        core.dirty = true;

        if let Err(default_err) = audio.set_output_device(None) {
            core.status =
                format!("Audio init failed: {default_err}. / -> Audio driver settings -> Reload");
            core.dirty = true;
        } else {
            core.status = format!("{} ({})", core.status, concise_audio_error(&err));
        }
    }
}

fn handle_mouse(core: &mut TuneCore, mouse: MouseEvent, library_rect: ratatui::prelude::Rect) {
    let inside_library = point_in_rect(mouse.column, mouse.row, library_rect);
    match mouse.kind {
        MouseEventKind::ScrollDown if inside_library => core.select_next(),
        MouseEventKind::ScrollUp if inside_library => core.select_prev(),
        _ => {}
    }
}

fn point_in_rect(x: u16, y: u16, rect: ratatui::prelude::Rect) -> bool {
    if rect.width == 0 || rect.height == 0 {
        return false;
    }
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn sorted_playlist_names(core: &TuneCore) -> Vec<String> {
    let mut names: Vec<String> = core.playlists.keys().cloned().collect();
    names.sort_by_cached_key(|name| name.to_ascii_lowercase());
    names
}

fn sorted_folder_paths(core: &TuneCore) -> Vec<PathBuf> {
    let mut paths = core.folders.clone();
    paths.sort_by_cached_key(|path| path.to_string_lossy().to_ascii_lowercase());
    paths
}

fn audio_output_options(audio: &dyn AudioEngine) -> Vec<String> {
    let selected = audio.selected_output_device();
    let outputs = audio.available_outputs();
    let mut options = Vec::with_capacity(outputs.len().saturating_add(1));
    options.push(if selected.is_none() {
        String::from("* System default output")
    } else {
        String::from("System default output")
    });

    for output in outputs {
        let label = if selected.as_deref() == Some(output.as_str()) {
            format!("* {output}")
        } else {
            output
        };
        options.push(label);
    }

    options
}

fn playback_settings_options(core: &TuneCore) -> Vec<String> {
    vec![
        format!(
            "Loudness normalization: {}",
            if core.loudness_normalization {
                "On"
            } else {
                "Off"
            }
        ),
        format!(
            "Song crossfade: {}",
            crossfade_label(core.crossfade_seconds)
        ),
        format!(
            "Stats tracking: {}",
            if core.stats_enabled { "On" } else { "Off" }
        ),
        String::from("Back"),
    ]
}

fn theme_options(theme: Theme) -> Vec<String> {
    [
        Theme::Dark,
        Theme::PitchBlack,
        Theme::Galaxy,
        Theme::Matrix,
        Theme::Demonic,
        Theme::CottonCandy,
    ]
    .into_iter()
    .map(|entry| {
        if entry == theme {
            format!("* {}", theme_label(entry))
        } else {
            theme_label(entry).to_string()
        }
    })
    .collect()
}

fn theme_label(theme: Theme) -> &'static str {
    match theme {
        Theme::Dark => "Dark",
        Theme::PitchBlack => "Pitch Black",
        Theme::Galaxy => "Galaxy",
        Theme::Matrix => "Matrix",
        Theme::Demonic => "Demonic",
        Theme::CottonCandy => "Cotton Candy",
        Theme::Ocean => "Ocean (legacy)",
        Theme::Forest => "Forest (legacy)",
        Theme::Sunset => "Sunset (legacy)",
    }
}

fn crossfade_label(seconds: u16) -> String {
    if seconds == 0 {
        String::from("Off")
    } else {
        format!("{seconds}s")
    }
}

fn next_crossfade_seconds(current: u16) -> u16 {
    match current {
        0 => 2,
        2 => 4,
        4 => 6,
        6 => 8,
        8 => 10,
        _ => 0,
    }
}

fn apply_audio_preferences_from_core(core: &TuneCore, audio: &mut dyn AudioEngine) {
    audio.set_loudness_normalization(core.loudness_normalization);
    audio.set_crossfade_seconds(core.crossfade_seconds);
}

fn update_panel_selection(panel: &mut ActionPanelState, option_count: usize, move_next: bool) {
    if option_count == 0 {
        return;
    }

    let advance = |selected: &mut usize| {
        if move_next {
            *selected = (*selected + 1) % option_count;
        } else {
            *selected = if *selected == 0 {
                option_count - 1
            } else {
                *selected - 1
            };
        }
    };

    match panel {
        ActionPanelState::Root { selected }
        | ActionPanelState::Mode { selected }
        | ActionPanelState::PlaylistPlay { selected }
        | ActionPanelState::PlaylistAdd { selected }
        | ActionPanelState::PlaylistCreate { selected, .. }
        | ActionPanelState::PlaylistRemove { selected }
        | ActionPanelState::AudioSettings { selected }
        | ActionPanelState::AudioOutput { selected }
        | ActionPanelState::PlaybackSettings { selected }
        | ActionPanelState::ThemeSettings { selected }
        | ActionPanelState::AddDirectory { selected, .. }
        | ActionPanelState::RemoveDirectory { selected } => advance(selected),
        ActionPanelState::Closed => {}
    }
}

fn handle_action_panel_input(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    panel: &mut ActionPanelState,
    key: KeyCode,
) {
    if let ActionPanelState::AddDirectory { selected, input } = panel {
        match key {
            KeyCode::Char(ch) if *selected == 1 => {
                input.push(ch);
                core.dirty = true;
                return;
            }
            KeyCode::Backspace if *selected == 1 && !input.is_empty() => {
                input.pop();
                core.dirty = true;
                return;
            }
            _ => {}
        }
    }

    if let ActionPanelState::PlaylistCreate { selected, input } = panel {
        match key {
            KeyCode::Char(ch) if *selected == 0 => {
                input.push(ch);
                core.dirty = true;
                return;
            }
            KeyCode::Backspace if *selected == 0 && !input.is_empty() => {
                input.pop();
                core.dirty = true;
                return;
            }
            _ => {}
        }
    }

    let option_count = match panel {
        ActionPanelState::Closed => 0,
        ActionPanelState::Root { .. } => 16,
        ActionPanelState::Mode { .. } => 4,
        ActionPanelState::PlaylistPlay { .. }
        | ActionPanelState::PlaylistAdd { .. }
        | ActionPanelState::PlaylistRemove { .. } => sorted_playlist_names(core).len().max(1),
        ActionPanelState::PlaylistCreate { .. } => 1,
        ActionPanelState::AudioSettings { .. } => 3,
        ActionPanelState::AudioOutput { .. } => audio.available_outputs().len().saturating_add(1),
        ActionPanelState::PlaybackSettings { .. } => 4,
        ActionPanelState::ThemeSettings { .. } => 6,
        ActionPanelState::AddDirectory { .. } => 2,
        ActionPanelState::RemoveDirectory { .. } => sorted_folder_paths(core).len().max(1),
    };

    match key {
        KeyCode::Esc => {
            panel.close();
            core.dirty = true;
        }
        KeyCode::Up => {
            update_panel_selection(panel, option_count, false);
            core.dirty = true;
        }
        KeyCode::Down => {
            update_panel_selection(panel, option_count, true);
            core.dirty = true;
        }
        KeyCode::Left | KeyCode::Backspace => {
            *panel = match panel {
                ActionPanelState::Mode { .. } => ActionPanelState::Root { selected: 2 },
                ActionPanelState::PlaylistPlay { .. } => ActionPanelState::Root { selected: 4 },
                ActionPanelState::PlaylistAdd { .. } => ActionPanelState::Root { selected: 1 },
                ActionPanelState::PlaylistCreate { .. } => ActionPanelState::Root { selected: 6 },
                ActionPanelState::PlaylistRemove { .. } => ActionPanelState::Root { selected: 7 },
                ActionPanelState::AudioSettings { .. } => ActionPanelState::Root { selected: 10 },
                ActionPanelState::PlaybackSettings { .. } => ActionPanelState::Root { selected: 3 },
                ActionPanelState::AddDirectory { .. } => ActionPanelState::Root { selected: 0 },
                ActionPanelState::AudioOutput { .. } => {
                    ActionPanelState::AudioSettings { selected: 0 }
                }
                ActionPanelState::ThemeSettings { .. } => ActionPanelState::Root { selected: 11 },
                ActionPanelState::RemoveDirectory { .. } => ActionPanelState::Root { selected: 8 },
                ActionPanelState::Root { .. } | ActionPanelState::Closed => {
                    ActionPanelState::Closed
                }
            };
            core.dirty = true;
        }
        KeyCode::Enter => match panel.clone() {
            ActionPanelState::Root { selected } => match selected {
                0 => {
                    *panel = ActionPanelState::AddDirectory {
                        selected: 1,
                        input: String::new(),
                    };
                    core.dirty = true;
                }
                1 => {
                    if sorted_playlist_names(core).is_empty() {
                        core.status = String::from("No playlists available");
                        core.dirty = true;
                        panel.close();
                    } else {
                        *panel = ActionPanelState::PlaylistAdd { selected: 0 };
                        core.dirty = true;
                    }
                }
                2 => {
                    *panel = ActionPanelState::Mode { selected: 0 };
                    core.dirty = true;
                }
                3 => {
                    *panel = ActionPanelState::PlaybackSettings { selected: 0 };
                    core.dirty = true;
                }
                4 => {
                    if sorted_playlist_names(core).is_empty() {
                        core.status = String::from("No playlists available");
                        core.dirty = true;
                        panel.close();
                    } else {
                        *panel = ActionPanelState::PlaylistPlay { selected: 0 };
                        core.dirty = true;
                    }
                }
                5 => {
                    core.remove_selected_from_current_playlist();
                    auto_save_state(core, &*audio);
                    panel.close();
                }
                6 => {
                    *panel = ActionPanelState::PlaylistCreate {
                        selected: 0,
                        input: String::new(),
                    };
                    core.dirty = true;
                }
                7 => {
                    *panel = ActionPanelState::PlaylistRemove { selected: 0 };
                    core.dirty = true;
                }
                8 => {
                    *panel = ActionPanelState::RemoveDirectory { selected: 0 };
                    core.dirty = true;
                }
                9 => {
                    core.rescan();
                    panel.close();
                }
                10 => {
                    *panel = ActionPanelState::AudioSettings { selected: 0 };
                    core.dirty = true;
                }
                11 => {
                    let selected = match core.theme {
                        Theme::Dark | Theme::Ocean => 0,
                        Theme::PitchBlack => 1,
                        Theme::Galaxy => 2,
                        Theme::Matrix | Theme::Forest => 3,
                        Theme::Demonic => 4,
                        Theme::CottonCandy | Theme::Sunset => 5,
                    };
                    *panel = ActionPanelState::ThemeSettings { selected };
                    core.dirty = true;
                }
                12 => {
                    if let Err(err) = save_state_with_audio(core, &*audio) {
                        core.status = format!("save error: {err:#}");
                        core.dirty = true;
                    }
                    panel.close();
                }
                13 => {
                    core.clear_stats_requested = true;
                    core.status = String::from("Clearing listen history...");
                    core.dirty = true;
                    panel.close();
                }
                14 => {
                    minimize_to_tray();
                    core.status = String::from("Minimized to tray");
                    core.dirty = true;
                    panel.close();
                }
                _ => {
                    panel.close();
                    core.dirty = true;
                }
            },
            ActionPanelState::Mode { selected } => {
                core.playback_mode = match selected {
                    0 => PlaybackMode::Normal,
                    1 => PlaybackMode::Shuffle,
                    2 => PlaybackMode::Loop,
                    _ => PlaybackMode::LoopOne,
                };
                core.status = String::from("Playback mode updated");
                core.dirty = true;
                auto_save_state(core, &*audio);
                panel.close();
            }
            ActionPanelState::PlaylistPlay { selected } => {
                let playlists = sorted_playlist_names(core);
                if let Some(name) = playlists.get(selected) {
                    core.load_playlist_queue(name);
                    if let Some(err) = core
                        .next_track_path()
                        .and_then(|path| audio.play(&path).err())
                    {
                        core.status = concise_audio_error(&err);
                        core.dirty = true;
                    }
                } else {
                    core.status = String::from("No playlists available");
                    core.dirty = true;
                }
                panel.close();
            }
            ActionPanelState::PlaylistAdd { selected } => {
                let playlists = sorted_playlist_names(core);
                if let Some(name) = playlists.get(selected) {
                    core.add_selected_to_playlist(name);
                    auto_save_state(core, &*audio);
                } else {
                    core.status = String::from("No playlists available");
                    core.dirty = true;
                }
                panel.close();
            }
            ActionPanelState::PlaylistCreate { input, .. } => {
                let name = input.trim();
                if name.is_empty() {
                    core.status = String::from("Enter a playlist name");
                    core.dirty = true;
                    return;
                }
                core.create_playlist(name);
                auto_save_state(core, &*audio);
                panel.close();
            }
            ActionPanelState::PlaylistRemove { selected } => {
                let playlists = sorted_playlist_names(core);
                if let Some(name) = playlists.get(selected) {
                    core.remove_playlist(name);
                    auto_save_state(core, &*audio);
                } else {
                    core.status = String::from("No playlists available");
                    core.dirty = true;
                }
                panel.close();
            }
            ActionPanelState::AudioSettings { selected } => match selected {
                0 => {
                    if let Err(err) = audio.reload_driver() {
                        core.status =
                            format!("Audio reset failed: {err}. Use / -> Audio driver settings");
                    } else {
                        core.status = format!(
                            "Audio reset on {}. Reload again if needed",
                            audio
                                .output_name()
                                .unwrap_or_else(|| String::from("unknown output"))
                        );
                    }
                    core.dirty = true;
                    panel.close();
                }
                1 => {
                    let selected = audio
                        .selected_output_device()
                        .and_then(|name| {
                            audio
                                .available_outputs()
                                .iter()
                                .position(|entry| entry == &name)
                        })
                        .map(|index| index.saturating_add(1))
                        .unwrap_or(0);
                    *panel = ActionPanelState::AudioOutput { selected };
                    core.dirty = true;
                }
                _ => {
                    *panel = ActionPanelState::Root { selected: 10 };
                    core.dirty = true;
                }
            },
            ActionPanelState::AudioOutput { selected } => {
                let outputs = audio.available_outputs();
                let result = if selected == 0 {
                    audio.set_output_device(None)
                } else {
                    match outputs.get(selected - 1) {
                        Some(name) => audio.set_output_device(Some(name.as_str())),
                        None => Err(anyhow::anyhow!("selected audio output is unavailable")),
                    }
                };

                if let Err(err) = result {
                    core.status = format!("Output switch failed: {err}. Try Reload audio driver");
                } else {
                    core.status = format!(
                        "Output: {}",
                        audio
                            .output_name()
                            .unwrap_or_else(|| String::from("unknown output"))
                    );
                    auto_save_state(core, &*audio);
                }
                core.dirty = true;
                panel.close();
            }
            ActionPanelState::PlaybackSettings { selected } => match selected {
                0 => {
                    core.loudness_normalization = !core.loudness_normalization;
                    audio.set_loudness_normalization(core.loudness_normalization);
                    core.status = format!(
                        "Loudness normalization: {}",
                        if core.loudness_normalization {
                            "On"
                        } else {
                            "Off"
                        }
                    );
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                1 => {
                    core.crossfade_seconds = next_crossfade_seconds(core.crossfade_seconds);
                    audio.set_crossfade_seconds(core.crossfade_seconds);
                    core.status = format!("Crossfade: {}", crossfade_label(core.crossfade_seconds));
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                2 => {
                    core.stats_enabled = !core.stats_enabled;
                    core.status = format!(
                        "Stats tracking: {}",
                        if core.stats_enabled { "On" } else { "Off" }
                    );
                    core.dirty = true;
                    auto_save_state(core, &*audio);
                }
                _ => {
                    *panel = ActionPanelState::Root { selected: 3 };
                    core.dirty = true;
                }
            },
            ActionPanelState::ThemeSettings { selected } => {
                core.theme = match selected {
                    0 => Theme::Dark,
                    1 => Theme::PitchBlack,
                    2 => Theme::Galaxy,
                    3 => Theme::Matrix,
                    4 => Theme::Demonic,
                    _ => Theme::CottonCandy,
                };
                core.status = format!("Theme: {}", theme_label(core.theme));
                core.dirty = true;
                auto_save_state(core, &*audio);
                panel.close();
            }
            ActionPanelState::AddDirectory { selected, input } => {
                if selected == 1 {
                    let trimmed = input.trim();
                    if trimmed.is_empty() {
                        core.status = String::from("Enter a folder path or choose externally");
                        core.dirty = true;
                        return;
                    }
                    core.add_folder(Path::new(trimmed));
                    auto_save_state(core, &*audio);
                    panel.close();
                } else {
                    match choose_folder_externally() {
                        Ok(Some(path)) => {
                            core.add_folder(&path);
                            auto_save_state(core, &*audio);
                            panel.close();
                        }
                        Ok(None) => {
                            core.status = String::from("Folder selection cancelled");
                            core.dirty = true;
                        }
                        Err(err) => {
                            core.status = format!("Folder picker failed: {err}");
                            core.dirty = true;
                        }
                    }
                }
            }
            ActionPanelState::RemoveDirectory { selected } => {
                let folders = sorted_folder_paths(core);
                if let Some(path) = folders.get(selected) {
                    core.remove_folder(path);
                    auto_save_state(core, &*audio);
                } else {
                    core.status = String::from("No folders available");
                    core.dirty = true;
                }
                panel.close();
            }
            ActionPanelState::Closed => {}
        },
        _ => {}
    }
}

#[cfg(windows)]
fn choose_folder_externally() -> Result<Option<PathBuf>> {
    let _ = disable_raw_mode();
    struct RawModeRestore;
    impl Drop for RawModeRestore {
        fn drop(&mut self) {
            let _ = enable_raw_mode();
        }
    }
    let _restore = RawModeRestore;

    let script = "Add-Type -AssemblyName System.Windows.Forms; $dlg = New-Object System.Windows.Forms.FolderBrowserDialog; $dlg.Description = 'Select music folder'; if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.WriteLine($dlg.SelectedPath) }";
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()?;

    if !output.status.success() {
        return Err(anyhow::anyhow!("powerShell folder picker failed"));
    }

    let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if selected.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(selected)))
    }
}

#[cfg(not(windows))]
fn choose_folder_externally() -> Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(windows)]
const TRAY_CALLBACK_MSG: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1;
#[cfg(windows)]
const TRAY_ICON_ID: u32 = 1;

#[cfg(windows)]
static TRAY_RESTORE_REQUESTED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static TRAY_CONTROLLER: OnceLock<Mutex<TrayController>> = OnceLock::new();

#[cfg(windows)]
fn minimize_to_tray() {
    if let Some(mut controller) = tray_controller() {
        controller.minimize();
    }
}

#[cfg(not(windows))]
fn minimize_to_tray() {}

#[cfg(windows)]
fn pump_tray_events(core: &mut TuneCore) {
    if let Some(mut controller) = tray_controller() {
        controller.pump();
    }

    if TRAY_RESTORE_REQUESTED.swap(false, Ordering::SeqCst) {
        restore_from_tray();
        if let Some(mut controller) = tray_controller() {
            controller.hide_icon();
        }
        core.status = String::from("Restored from tray");
        core.dirty = true;
    }
}

#[cfg(not(windows))]
fn pump_tray_events(_core: &mut TuneCore) {}

#[cfg(windows)]
fn cleanup_tray() {
    if let Some(mut controller) = tray_controller() {
        controller.cleanup();
    }
}

#[cfg(not(windows))]
fn cleanup_tray() {}

#[cfg(windows)]
fn restore_from_tray() {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };

    unsafe {
        let hwnd = GetConsoleWindow();
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(windows)]
fn tray_controller() -> Option<std::sync::MutexGuard<'static, TrayController>> {
    let lock = TRAY_CONTROLLER.get_or_init(|| Mutex::new(TrayController::new()));
    lock.lock().ok()
}

#[cfg(windows)]
struct TrayController {
    window: isize,
    icon_visible: bool,
}

#[cfg(windows)]
impl TrayController {
    fn new() -> Self {
        Self {
            window: 0,
            icon_visible: false,
        }
    }

    fn minimize(&mut self) {
        use windows_sys::Win32::System::Console::GetConsoleWindow;
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

        unsafe {
            if self.ensure_window().is_none() {
                return;
            }
            if !self.icon_visible && !self.show_icon() {
                return;
            }
            let hwnd = GetConsoleWindow();
            if !hwnd.is_null() {
                ShowWindow(hwnd, SW_HIDE);
            }
        }
    }

    fn pump(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        };

        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn cleanup(&mut self) {
        unsafe {
            self.hide_icon();
            if self.window != 0 {
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(self.window as _);
                self.window = 0;
            }
        }
    }

    fn ensure_window(&mut self) -> Option<windows_sys::Win32::Foundation::HWND> {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, RegisterClassW, WNDCLASSW,
        };

        if self.window != 0 {
            return Some(self.window as _);
        }

        let class_name = to_wide("TuneTuiTrayWindow");
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };

        let mut wc: WNDCLASSW = unsafe { std::mem::zeroed() };
        wc.lpfnWndProc = Some(tray_wnd_proc);
        wc.hInstance = instance;
        wc.lpszClassName = class_name.as_ptr();
        unsafe {
            RegisterClassW(&wc);
        }

        self.window = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            ) as isize
        };

        (self.window != 0).then_some(self.window as _)
    }

    fn show_icon(&mut self) -> bool {
        use windows_sys::Win32::UI::Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFYICONDATAW, Shell_NotifyIconW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{IDI_APPLICATION, LoadIconW};

        let Some(hwnd) = self.ensure_window() else {
            return false;
        };

        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ICON_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = TRAY_CALLBACK_MSG;
        nid.hIcon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };

        let tip = to_wide("TuneTUI - click to restore");
        let max_len = nid.szTip.len().saturating_sub(1).min(tip.len());
        nid.szTip[..max_len].copy_from_slice(&tip[..max_len]);

        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) != 0 };
        if ok {
            self.icon_visible = true;
        }
        ok
    }

    fn hide_icon(&mut self) {
        use windows_sys::Win32::UI::Shell::{NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW};

        if !self.icon_visible || self.window == 0 {
            return;
        }

        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.window as _;
        nid.uID = TRAY_ICON_ID;
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        self.icon_visible = false;
    }
}

#[cfg(windows)]
unsafe extern "system" fn tray_wnd_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    };

    if msg == TRAY_CALLBACK_MSG {
        let event = lparam as u32;
        if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
            TRAY_RESTORE_REQUESTED.store(true, Ordering::SeqCst);
        }
        return 0;
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioEngine;
    use crate::model::PersistedState;
    use crate::model::Track;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    struct TestAudioEngine {
        paused: bool,
        current: Option<PathBuf>,
        queued: Option<PathBuf>,
        finished: bool,
        position: Option<Duration>,
        duration: Option<Duration>,
        played: Vec<PathBuf>,
        stopped: bool,
        outputs: Vec<String>,
        selected_output: Option<String>,
        reload_calls: usize,
        loudness_normalization: bool,
        crossfade_seconds: u16,
    }

    impl TestAudioEngine {
        fn new() -> Self {
            Self {
                paused: false,
                current: None,
                queued: None,
                finished: false,
                position: None,
                duration: None,
                played: Vec::new(),
                stopped: false,
                outputs: vec![String::from("Headphones"), String::from("Speakers")],
                selected_output: None,
                reload_calls: 0,
                loudness_normalization: false,
                crossfade_seconds: 0,
            }
        }

        fn finished_with_current(path: &str) -> Self {
            Self {
                paused: false,
                current: Some(PathBuf::from(path)),
                queued: None,
                finished: true,
                position: None,
                duration: None,
                played: Vec::new(),
                stopped: false,
                outputs: vec![String::from("Headphones"), String::from("Speakers")],
                selected_output: None,
                reload_calls: 0,
                loudness_normalization: false,
                crossfade_seconds: 0,
            }
        }
    }

    impl AudioEngine for TestAudioEngine {
        fn play(&mut self, path: &Path) -> Result<()> {
            self.current = Some(path.to_path_buf());
            self.queued = None;
            self.finished = false;
            self.position = Some(Duration::from_secs(0));
            self.played.push(path.to_path_buf());
            Ok(())
        }

        fn queue_crossfade(&mut self, path: &Path) -> Result<()> {
            self.queued = Some(path.to_path_buf());
            Ok(())
        }

        fn tick(&mut self) {
            if !self.finished {
                return;
            }

            if let Some(path) = self.queued.take() {
                self.current = Some(path.clone());
                self.played.push(path);
                self.finished = false;
                self.position = Some(Duration::from_secs(u64::from(self.crossfade_seconds)));
            }
        }

        fn pause(&mut self) {
            self.paused = true;
        }

        fn resume(&mut self) {
            self.paused = false;
        }

        fn stop(&mut self) {
            self.stopped = true;
            self.current = None;
            self.finished = false;
            self.position = None;
        }

        fn is_paused(&self) -> bool {
            self.paused
        }

        fn current_track(&self) -> Option<&Path> {
            self.current.as_deref()
        }

        fn position(&self) -> Option<Duration> {
            self.position
        }

        fn duration(&self) -> Option<Duration> {
            self.duration
        }

        fn volume(&self) -> f32 {
            1.0
        }

        fn set_volume(&mut self, _volume: f32) {}

        fn output_name(&self) -> Option<String> {
            Some(
                self.selected_output
                    .clone()
                    .unwrap_or_else(|| String::from("System default output (test)")),
            )
        }

        fn reload_driver(&mut self) -> Result<()> {
            self.reload_calls = self.reload_calls.saturating_add(1);
            Ok(())
        }

        fn available_outputs(&self) -> Vec<String> {
            self.outputs.clone()
        }

        fn selected_output_device(&self) -> Option<String> {
            self.selected_output.clone()
        }

        fn set_output_device(&mut self, output: Option<&str>) -> Result<()> {
            if let Some(name) =
                output.filter(|name| !self.outputs.iter().any(|entry| entry == *name))
            {
                return Err(anyhow::anyhow!("audio output device not found: {name}"));
            }
            self.selected_output = output.map(ToOwned::to_owned);
            Ok(())
        }

        fn loudness_normalization(&self) -> bool {
            self.loudness_normalization
        }

        fn set_loudness_normalization(&mut self, enabled: bool) {
            self.loudness_normalization = enabled;
        }

        fn crossfade_seconds(&self) -> u16 {
            self.crossfade_seconds
        }

        fn set_crossfade_seconds(&mut self, seconds: u16) {
            self.crossfade_seconds = seconds;
        }

        fn crossfade_queued_track(&self) -> Option<&Path> {
            self.queued.as_deref()
        }

        fn is_finished(&self) -> bool {
            self.finished
        }
    }

    #[test]
    fn action_panel_mode_selection_applies_mode() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 2 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::Mode { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(core.playback_mode, crate::model::PlaybackMode::Shuffle);
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_playlist_add_requires_playlist() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 1 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(core.status, "No playlists available");
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_add_directory_from_typed_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("typed-folder");
        std::fs::create_dir_all(&dir).expect("create");

        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 0 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(
            panel,
            ActionPanelState::AddDirectory { selected: 1, .. }
        ));

        for ch in dir.to_string_lossy().chars() {
            handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Char(ch));
        }
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        let expected = crate::config::normalize_path(&dir);
        assert!(
            core.folders
                .iter()
                .any(|folder| crate::config::normalize_path(folder) == expected)
        );
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_remove_directory_from_list() {
        let mut state = PersistedState::default();
        state.folders.push(PathBuf::from(r"E:\LOCALMUSIC"));
        let mut core = TuneCore::from_persisted(state);
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 8 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::RemoveDirectory { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(core.folders.is_empty());
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_create_playlist_from_input() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 6 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(
            panel,
            ActionPanelState::PlaylistCreate { selected: 0, .. }
        ));

        for ch in "mix".chars() {
            handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Char(ch));
        }
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert!(core.playlists.contains_key("mix"));
        assert!(core.browser_entries.iter().any(|entry| {
            entry.kind == crate::core::BrowserEntryKind::Playlist && entry.label == "[PL] mix"
        }));
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_remove_playlist_from_list() {
        let mut state = PersistedState::default();
        state
            .playlists
            .insert(String::from("mix"), crate::model::Playlist::default());
        let mut core = TuneCore::from_persisted(state);
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 7 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::PlaylistRemove { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(!core.playlists.contains_key("mix"));
        assert_eq!(core.status, "Playlist removed");
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_audio_driver_reload_updates_status() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 10 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::AudioSettings { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(audio.reload_calls, 1);
        assert_eq!(
            core.status,
            "Audio reset on System default output (test). Reload again if needed"
        );
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn action_panel_audio_output_selection_sets_device() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 10 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(
            panel,
            ActionPanelState::AudioOutput { selected: 0 }
        ));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(
            audio.selected_output_device(),
            Some(String::from("Speakers"))
        );
        assert_eq!(core.status, "Output: Speakers");
        assert!(matches!(panel, ActionPanelState::Closed));
    }

    #[test]
    fn playback_settings_toggle_loudness_and_crossfade() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 3 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(panel, ActionPanelState::PlaybackSettings { .. }));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(core.loudness_normalization);
        assert!(audio.loudness_normalization());

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert_eq!(core.crossfade_seconds, 2);
        assert_eq!(audio.crossfade_seconds(), 2);

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(!core.stats_enabled);
    }

    #[test]
    fn stats_left_on_range_cycles_back() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = crate::core::HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Range(2);
        core.stats_range = crate::stats::StatsRange::Days30;

        assert!(handle_stats_inline_input(
            &mut core,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)
        ));
        assert_eq!(core.stats_range, crate::stats::StatsRange::Days7);
        assert!(matches!(
            core.stats_focus,
            crate::core::StatsFilterFocus::Range(1)
        ));
    }

    #[test]
    fn stats_left_from_search_scrolls_up_when_scrolled() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = crate::core::HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Search;
        core.stats_scroll = 3;

        assert!(handle_stats_inline_input(
            &mut core,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)
        ));
        assert_eq!(core.stats_scroll, 2);
        assert!(matches!(
            core.stats_focus,
            crate::core::StatsFilterFocus::Search
        ));
    }

    #[test]
    fn stats_shift_up_resets_scroll_to_top() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.header_section = crate::core::HeaderSection::Stats;
        core.stats_focus = crate::core::StatsFilterFocus::Search;
        core.stats_range = crate::stats::StatsRange::Days7;
        core.stats_scroll = 9;

        assert!(handle_stats_inline_input(
            &mut core,
            KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)
        ));
        assert_eq!(core.stats_scroll, 0);
        assert!(matches!(
            core.stats_focus,
            crate::core::StatsFilterFocus::Range(1)
        ));
    }

    #[test]
    fn theme_settings_updates_core() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 11 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert!(matches!(
            panel,
            ActionPanelState::ThemeSettings { selected: 0 }
        ));

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Down);
        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);
        assert_eq!(core.theme, Theme::PitchBlack);
        assert_eq!(core.status, "Theme: Pitch Black");
    }

    #[test]
    fn persisted_state_contains_selected_audio_output() {
        let core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();
        audio
            .set_output_device(Some("Speakers"))
            .expect("select output");

        let state = persisted_state_with_audio(&core, &audio);
        assert_eq!(state.selected_output_device, Some(String::from("Speakers")));
    }

    #[test]
    fn persisted_state_contains_playback_settings() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.loudness_normalization = true;
        core.crossfade_seconds = 4;
        core.theme = Theme::Galaxy;
        core.stats_enabled = false;
        let audio = TestAudioEngine::new();

        let state = persisted_state_with_audio(&core, &audio);
        assert!(state.loudness_normalization);
        assert_eq!(state.crossfade_seconds, 4);
        assert_eq!(state.theme, Theme::Galaxy);
        assert!(!state.stats_enabled);
    }

    #[test]
    fn invalid_saved_output_falls_back_to_default_with_actionable_status() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = TestAudioEngine::new();

        apply_saved_audio_output(&mut core, &mut audio, Some(String::from("Missing Device")));

        assert_eq!(audio.selected_output_device(), None);
        assert!(
            core.status
                .contains("Saved output 'Missing Device' unavailable")
        );
        assert!(core.status.contains("Audio driver settings"));
    }

    #[test]
    fn auto_advance_plays_next_track_when_finished() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from("a.mp3"),
                title: String::from("a"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("b.mp3"),
                title: String::from("b"),
                artist: None,
                album: None,
            },
        ];
        core.queue = vec![0, 1];
        core.current_queue_index = Some(0);

        let mut audio = TestAudioEngine::finished_with_current("a.mp3");
        maybe_auto_advance_track(&mut core, &mut audio);

        assert_eq!(audio.played, vec![PathBuf::from("b.mp3")]);
        assert_eq!(core.current_queue_index, Some(1));
    }

    #[test]
    fn auto_advance_starts_next_track_within_crossfade_window() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![
            Track {
                path: PathBuf::from("a.mp3"),
                title: String::from("a"),
                artist: None,
                album: None,
            },
            Track {
                path: PathBuf::from("b.mp3"),
                title: String::from("b"),
                artist: None,
                album: None,
            },
        ];
        core.queue = vec![0, 1];
        core.current_queue_index = Some(0);

        let mut audio = TestAudioEngine::new();
        audio.current = Some(PathBuf::from("a.mp3"));
        audio.duration = Some(Duration::from_secs(100));
        audio.position = Some(Duration::from_secs(95));
        audio.crossfade_seconds = 6;

        maybe_auto_advance_track(&mut core, &mut audio);

        assert_eq!(audio.played, Vec::<PathBuf>::new());
        assert_eq!(audio.crossfade_queued_track(), Some(Path::new("b.mp3")));
        assert_eq!(core.current_queue_index, Some(1));

        audio.finished = true;
        audio.tick();
        assert_eq!(audio.played, vec![PathBuf::from("b.mp3")]);
        assert_eq!(audio.position, Some(Duration::from_secs(6)));
    }

    #[test]
    fn auto_advance_stops_when_queue_ends() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        core.tracks = vec![Track {
            path: PathBuf::from("a.mp3"),
            title: String::from("a"),
            artist: None,
            album: None,
        }];
        core.queue = vec![0];
        core.current_queue_index = Some(0);

        let mut audio = TestAudioEngine::finished_with_current("a.mp3");
        maybe_auto_advance_track(&mut core, &mut audio);

        assert!(audio.stopped);
        assert_eq!(core.status, "Reached end of queue");
    }
}
