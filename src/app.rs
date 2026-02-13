use crate::audio::{AudioEngine, NullAudioEngine, WasapiAudioEngine};
use crate::config;
use crate::core::TuneCore;
use crate::model::PlaybackMode;
use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::stdout;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionPanelState {
    Closed,
    Root { selected: usize },
    Mode { selected: usize },
    PlaylistPlay { selected: usize },
    PlaylistAdd { selected: usize },
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

    fn to_view(self, core: &TuneCore) -> Option<crate::ui::ActionPanelView> {
        match self {
            Self::Closed => None,
            Self::Root { selected } => Some(crate::ui::ActionPanelView {
                title: String::from("Actions"),
                hint: String::from("Enter select  Esc close  Up/Down navigate"),
                options: vec![
                    String::from("Load main library queue"),
                    String::from("Set playback mode"),
                    String::from("Play playlist"),
                    String::from("Add selected item to playlist"),
                    String::from("Remove selected from playlist"),
                    String::from("Rescan library"),
                    String::from("Save state"),
                    String::from("Minimize to tray"),
                    String::from("Close panel"),
                ],
                selected,
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
                selected,
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
                    selected,
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
                    selected,
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
    let mut core = TuneCore::from_persisted(state);

    let mut audio: Box<dyn AudioEngine> = match WasapiAudioEngine::new() {
        Ok(engine) => Box::new(engine),
        Err(_) => Box::new(NullAudioEngine::new()),
    };

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut action_panel = ActionPanelState::Closed;
    let mut last_tick = Instant::now();
    let mut library_rect = ratatui::prelude::Rect::default();

    let result: Result<()> = loop {
        pump_tray_events(&mut core);
        maybe_auto_advance_track(&mut core, &mut *audio);

        if core.dirty || last_tick.elapsed() > Duration::from_millis(250) {
            terminal.draw(|frame| {
                library_rect = crate::ui::library_rect(frame.area());
                let panel_view = action_panel.to_view(&core);
                crate::ui::draw(frame, &core, &*audio, panel_view.as_ref())
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

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break Ok(()),
            KeyCode::Down => core.select_next(),
            KeyCode::Up => core.select_prev(),
            KeyCode::Enter => {
                if let Some(err) = core
                    .activate_selected()
                    .and_then(|path| audio.play(&path).err())
                {
                    core.status = format!("playback error: {err:#}");
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
                    core.status = format!("playback error: {err:#}");
                    core.dirty = true;
                }
            }
            KeyCode::Char('b') => {
                if let Some(err) = core
                    .prev_track_path()
                    .and_then(|path| audio.play(&path).err())
                {
                    core.status = format!("playback error: {err:#}");
                    core.dirty = true;
                }
            }
            KeyCode::Char('m') => core.cycle_mode(),
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
                if let Err(err) = core.save() {
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
    let save_result = core.save();
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
    if audio.current_track().is_none() || audio.is_paused() || !audio.is_finished() {
        return;
    }

    if let Some(path) = core.next_track_path() {
        if let Err(err) = audio.play(&path) {
            core.status = format!("playback error: {err:#}");
            core.dirty = true;
        }
    } else {
        audio.stop();
        core.status = String::from("Reached end of queue");
        core.dirty = true;
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
        | ActionPanelState::PlaylistAdd { selected } => advance(selected),
        ActionPanelState::Closed => {}
    }
}

fn handle_action_panel_input(
    core: &mut TuneCore,
    audio: &mut dyn AudioEngine,
    panel: &mut ActionPanelState,
    key: KeyCode,
) {
    let option_count = match panel {
        ActionPanelState::Closed => 0,
        ActionPanelState::Root { .. } => 9,
        ActionPanelState::Mode { .. } => 4,
        ActionPanelState::PlaylistPlay { .. } | ActionPanelState::PlaylistAdd { .. } => {
            sorted_playlist_names(core).len().max(1)
        }
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
                ActionPanelState::Mode { .. }
                | ActionPanelState::PlaylistPlay { .. }
                | ActionPanelState::PlaylistAdd { .. } => ActionPanelState::Root { selected: 0 },
                ActionPanelState::Root { .. } | ActionPanelState::Closed => {
                    ActionPanelState::Closed
                }
            };
            core.dirty = true;
        }
        KeyCode::Enter => match *panel {
            ActionPanelState::Root { selected } => match selected {
                0 => {
                    core.reset_main_queue();
                    panel.close();
                }
                1 => {
                    *panel = ActionPanelState::Mode { selected: 0 };
                    core.dirty = true;
                }
                2 => {
                    if sorted_playlist_names(core).is_empty() {
                        core.status = String::from("No playlists available");
                        core.dirty = true;
                        panel.close();
                    } else {
                        *panel = ActionPanelState::PlaylistPlay { selected: 0 };
                        core.dirty = true;
                    }
                }
                3 => {
                    if sorted_playlist_names(core).is_empty() {
                        core.status = String::from("No playlists available");
                        core.dirty = true;
                        panel.close();
                    } else {
                        *panel = ActionPanelState::PlaylistAdd { selected: 0 };
                        core.dirty = true;
                    }
                }
                4 => {
                    core.remove_selected_from_current_playlist();
                    panel.close();
                }
                5 => {
                    core.rescan();
                    panel.close();
                }
                6 => {
                    if let Err(err) = core.save() {
                        core.status = format!("save error: {err:#}");
                        core.dirty = true;
                    }
                    panel.close();
                }
                7 => {
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
                        core.status = format!("playback error: {err:#}");
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
                } else {
                    core.status = String::from("No playlists available");
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
        finished: bool,
        played: Vec<PathBuf>,
        stopped: bool,
    }

    impl TestAudioEngine {
        fn finished_with_current(path: &str) -> Self {
            Self {
                paused: false,
                current: Some(PathBuf::from(path)),
                finished: true,
                played: Vec::new(),
                stopped: false,
            }
        }
    }

    impl AudioEngine for TestAudioEngine {
        fn play(&mut self, path: &Path) -> Result<()> {
            self.current = Some(path.to_path_buf());
            self.finished = false;
            self.played.push(path.to_path_buf());
            Ok(())
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
        }

        fn is_paused(&self) -> bool {
            self.paused
        }

        fn current_track(&self) -> Option<&Path> {
            self.current.as_deref()
        }

        fn position(&self) -> Option<Duration> {
            None
        }

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn volume(&self) -> f32 {
            1.0
        }

        fn set_volume(&mut self, _volume: f32) {}

        fn output_name(&self) -> Option<String> {
            Some(String::from("test"))
        }

        fn is_finished(&self) -> bool {
            self.finished
        }
    }

    #[test]
    fn action_panel_mode_selection_applies_mode() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        let mut panel = ActionPanelState::Root { selected: 1 };

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
        let mut panel = ActionPanelState::Root { selected: 3 };

        handle_action_panel_input(&mut core, &mut audio, &mut panel, KeyCode::Enter);

        assert_eq!(core.status, "No playlists available");
        assert!(matches!(panel, ActionPanelState::Closed));
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
