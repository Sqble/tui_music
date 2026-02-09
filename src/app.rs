use crate::audio::{AudioEngine, NullAudioEngine, WasapiAudioEngine};
use crate::config;
use crate::core::TuneCore;
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
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub fn run() -> Result<()> {
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

    let mut command_mode = false;
    let mut command_buffer = String::new();
    let mut last_tick = Instant::now();
    let mut library_rect = ratatui::prelude::Rect::default();

    let result: Result<()> = loop {
        pump_tray_events(&mut core);
        maybe_auto_advance_track(&mut core, &mut *audio);

        if core.dirty || last_tick.elapsed() > Duration::from_millis(250) {
            terminal.draw(|frame| {
                library_rect = crate::ui::library_rect(frame.area());
                crate::ui::draw(frame, &core, &*audio, &command_buffer, command_mode)
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

        if command_mode {
            match key.code {
                KeyCode::Esc => {
                    command_mode = false;
                    command_buffer.clear();
                    core.dirty = true;
                }
                KeyCode::Enter => {
                    run_command(&mut core, &mut *audio, &command_buffer);
                    command_mode = false;
                    command_buffer.clear();
                }
                KeyCode::Backspace => {
                    command_buffer.pop();
                    core.dirty = true;
                }
                KeyCode::Char(ch) => {
                    command_buffer.push(ch);
                    core.dirty = true;
                }
                _ => {}
            }
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
            KeyCode::Char('m') => core.cycle_mode(),
            KeyCode::Char('t') => {
                minimize_to_tray();
                core.status = String::from("Minimized to tray");
                core.dirty = true;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                let next = (audio.volume() + 0.05).clamp(0.0, 2.0);
                audio.set_volume(next);
                core.status = format!("Volume: {}%", (next * 100.0).round() as u16);
                core.dirty = true;
            }
            KeyCode::Char('-') => {
                let next = (audio.volume() - 0.05).clamp(0.0, 2.0);
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
            KeyCode::Char(':') => {
                command_mode = true;
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

fn run_command(core: &mut TuneCore, audio: &mut dyn AudioEngine, raw: &str) {
    let input = raw.trim();
    if input.is_empty() {
        core.status = String::from("No command");
        core.dirty = true;
        return;
    }

    let mut command_split = input.splitn(2, char::is_whitespace);
    let command = command_split.next().unwrap_or_default();
    let rest = command_split.next().unwrap_or("").trim();

    match command {
        "help" => {
            core.status = String::from(
                "Commands: add <path> | playlist new <name> | playlist add <name> | playlist play <name> | library | mode <normal|shuffle|loop|single> | minimize | save",
            );
            core.dirty = true;
        }
        "add" => {
            if rest.is_empty() {
                core.status = String::from("Usage: add <path>");
                core.dirty = true;
            } else {
                core.add_folder(&PathBuf::from(rest));
            }
        }
        "playlist" => {
            let mut playlist_split = rest.splitn(2, char::is_whitespace);
            let action = playlist_split.next().unwrap_or_default();
            let name = playlist_split.next().unwrap_or("").trim();

            if action.is_empty() || name.is_empty() {
                core.status = String::from("Usage: playlist <new|add|play> <name>");
                core.dirty = true;
                return;
            }

            match action {
                "new" => core.create_playlist(name),
                "add" => core.add_selected_to_playlist(name),
                "play" => {
                    core.load_playlist_queue(name);
                    if let Some(err) = core
                        .next_track_path()
                        .and_then(|path| audio.play(&path).err())
                    {
                        core.status = format!("playback error: {err:#}");
                        core.dirty = true;
                    }
                }
                _ => {
                    core.status = String::from("Usage: playlist <new|add|play> <name>");
                    core.dirty = true;
                }
            }
        }
        "library" => core.reset_main_queue(),
        "minimize" => {
            minimize_to_tray();
            core.status = String::from("Minimized to tray");
            core.dirty = true;
        }
        "mode" => {
            if rest.is_empty() {
                core.status = String::from("Usage: mode <normal|shuffle|loop|single>");
                core.dirty = true;
                return;
            }
            core.playback_mode = match rest {
                "normal" => crate::model::PlaybackMode::Normal,
                "shuffle" => crate::model::PlaybackMode::Shuffle,
                "loop" => crate::model::PlaybackMode::Loop,
                "single" => crate::model::PlaybackMode::LoopOne,
                _ => {
                    core.status = String::from("Unknown mode");
                    core.dirty = true;
                    return;
                }
            };
            core.status = format!("Playback mode: {:?}", core.playback_mode);
            core.dirty = true;
        }
        "save" => {
            if let Err(err) = core.save() {
                core.status = format!("save error: {err:#}");
                core.dirty = true;
            }
        }
        _ => {
            core.status = String::from("Unknown command. Use :help");
            core.dirty = true;
        }
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
    fn unknown_command_is_reported() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();
        run_command(&mut core, &mut audio, "wat");
        assert!(core.status.contains("Unknown command"));
    }

    #[test]
    fn add_command_accepts_paths_with_spaces() {
        let mut core = TuneCore::from_persisted(PersistedState::default());
        let mut audio = NullAudioEngine::new();

        run_command(&mut core, &mut audio, "add C:\\Music Folder");

        assert!(core.folders.iter().any(|path| {
            path.to_string_lossy()
                .to_ascii_lowercase()
                .contains("music folder")
        }));
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
