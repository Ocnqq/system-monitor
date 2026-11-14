use std::{
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject, EndPaint, InvalidateRect,
            PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
        },
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoSetProxyBlanket, CoUninitialize, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
            },
            LibraryLoader::GetModuleHandleW,
            Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE},
            Variant::{VARIANT, VT_I4, VT_UI4, VariantClear},
            Wmi::{
                IWbemContext, IWbemLocator, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
                WbemLocator,
            },
        },
        UI::WindowsAndMessaging::{
            CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DispatchMessageW,
            GWLP_USERDATA, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, HTCLIENT,
            HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, KillTimer, LWA_COLORKEY, LoadCursorW,
            MB_ICONERROR, MB_OK, MSG, MessageBoxW, PostQuitMessage, RegisterClassW, SM_CXSCREEN,
            SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetLayeredWindowAttributes, SetTimer,
            SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WM_DESTROY,
            WM_LBUTTONUP, WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
        },
    },
    core::{BSTR, Error, w},
};

use super::{
    CpuSampler, GAME_CHECK_INTERVAL, GpuTelemetry, MediaInfo, REFRESH_INTERVAL, ShortcutAction,
    automatic_visibility, control_player, start_global_shortcuts, start_windows_gpu_monitor,
    start_windows_media_monitor, steam_game_running,
};

const WINDOW_WIDTH: i32 = 350;
const WINDOW_HEIGHT: i32 = 50;
const BUTTONS_X: i32 = 253;
const BUTTONS_Y: i32 = 25;
const BUTTONS_WIDTH: i32 = 94;
const BUTTONS_HEIGHT: i32 = 25;
const TIMER_ID: usize = 1;
const PREVIOUS_BUTTON: i32 = 0;
const PAUSE_BUTTON: i32 = 32;
const NEXT_BUTTON: i32 = 64;
const BUTTON_WIDTH: i32 = 28;
const TEMPERATURE_CHECK_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
struct SensorTemperatures {
    cpu: Option<i32>,
}

struct AppState {
    cpu: CpuSampler,
    cpu_usage: Option<u8>,
    cpu_temperature: Option<i32>,
    gpu: GpuTelemetry,
    media: Option<MediaInfo>,
    media_error: Option<String>,
    shortcut_error: Option<String>,
    gpu_receiver: Receiver<Result<Option<GpuTelemetry>, String>>,
    media_receiver: Receiver<Result<Option<MediaInfo>, String>>,
    shortcut_receiver: Receiver<ShortcutAction>,
    temperature_receiver: Receiver<Result<SensorTemperatures, String>>,
    main_window: Option<HWND>,
    media_window: Option<HWND>,
    manual_visibility: Option<bool>,
    visible: bool,
    controls_visible: bool,
    previous_auto_visibility: bool,
    game_running: bool,
    game_refresh_elapsed: Duration,
}

impl AppState {
    fn refresh(&mut self, window: HWND) {
        self.cpu_usage = self.cpu.sample().ok().flatten();
        while let Ok(update) = self.gpu_receiver.try_recv() {
            match update {
                Ok(Some(telemetry)) => self.gpu = telemetry,
                Ok(None) => self.gpu = GpuTelemetry::default(),
                Err(error) => {
                    self.gpu = GpuTelemetry::default();
                    eprintln!("{error}");
                }
            }
        }
        while let Ok(update) = self.media_receiver.try_recv() {
            match update {
                Ok(media) => {
                    self.media = media;
                    self.media_error = None;
                }
                Err(error) => {
                    self.media_error = Some(error.clone());
                    eprintln!("{error}");
                }
            }
        }
        while let Ok(update) = self.temperature_receiver.try_recv() {
            match update {
                Ok(temperatures) => {
                    self.cpu_temperature = temperatures.cpu;
                }
                Err(error) => eprintln!("{error}"),
            }
        }
        while let Ok(action) = self.shortcut_receiver.try_recv() {
            match action {
                ShortcutAction::ToggleOverlay => {
                    let visible = self.manual_visibility.unwrap_or(self.visible);
                    self.manual_visibility = Some(!visible);
                }
                ShortcutAction::SkipTrack => {
                    if let Err(error) = super::skip_track() {
                        eprintln!("{error}");
                        self.shortcut_error = Some(error);
                    } else {
                        self.shortcut_error = None;
                    }
                }
                ShortcutAction::Error(error) => {
                    eprintln!("{error}");
                    self.shortcut_error = Some(error);
                }
            }
        }

        self.game_refresh_elapsed += REFRESH_INTERVAL;
        if self.game_refresh_elapsed >= GAME_CHECK_INTERVAL {
            self.game_refresh_elapsed = Duration::ZERO;
            self.game_running = steam_game_running();
        }

        let auto_visibility = automatic_visibility(self.game_running, self.media.is_some(), false);
        if auto_visibility != self.previous_auto_visibility {
            self.manual_visibility = None;
            self.previous_auto_visibility = auto_visibility;
        }
        let visible = self.manual_visibility.unwrap_or(auto_visibility);
        if visible != self.visible {
            self.visible = visible;
            unsafe {
                let _ = ShowWindow(window, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
        }
        let controls_visible = visible && self.media_controls_available();
        if controls_visible != self.controls_visible {
            self.controls_visible = controls_visible;
            if let Some(media_window) = self.media_window {
                unsafe {
                    let _ = ShowWindow(
                        media_window,
                        if controls_visible {
                            SW_SHOWNOACTIVATE
                        } else {
                            SW_HIDE
                        },
                    );
                }
            }
        }
        if !unsafe { InvalidateRect(Some(window), None, false) }.as_bool() {
            eprintln!("Could not refresh the Dave overlay window");
        }
        if let Some(media_window) = self.media_window
            && !unsafe { InvalidateRect(Some(media_window), None, false) }.as_bool()
        {
            eprintln!("Could not refresh the Dave media controls");
        }
    }

    fn media_title(&self) -> String {
        if let Some(error) = &self.shortcut_error {
            return truncate_chars(error, 30);
        }
        if let Some(error) = &self.media_error {
            return truncate_chars(error, 30);
        }
        self.media
            .as_ref()
            .map(|media| truncate_chars(&media.title, 30))
            .unwrap_or_else(|| "No media playing".to_owned())
    }

    fn metric_line(&self) -> String {
        let cpu = self
            .cpu_usage
            .map(|usage| format!("{usage:>3}%"))
            .unwrap_or_else(|| " --%".to_owned());
        let cpu_temp = self
            .cpu_temperature
            .map(|temp| format!("{temp}°C"))
            .unwrap_or_else(|| "--°C".to_owned());
        let gpu = self
            .gpu
            .utilization
            .map(|usage| format!("{usage:>3}%"))
            .unwrap_or_else(|| " --%".to_owned());
        let gpu_temp = self
            .gpu
            .temperature
            .map(|temp| format!("{temp}°C"))
            .unwrap_or_else(|| "--°C".to_owned());
        format!("CPU {cpu} {cpu_temp}     GPU {gpu} {gpu_temp}")
    }

    fn media_controls_available(&self) -> bool {
        self.media.is_some()
    }
}

pub fn run(background_mode: bool) -> Result<(), String> {
    let (gpu_sender, gpu_receiver) = mpsc::sync_channel(1);
    start_windows_gpu_monitor(gpu_sender);
    let (media_sender, media_receiver) = mpsc::sync_channel(1);
    start_windows_media_monitor(media_sender);
    let (shortcut_sender, shortcut_receiver) = mpsc::channel();
    start_global_shortcuts(shortcut_sender);
    let (temperature_sender, temperature_receiver) = mpsc::sync_channel(1);
    start_temperature_monitor(temperature_sender);

    let mut state = Box::new(AppState {
        cpu: CpuSampler::default(),
        cpu_usage: None,
        cpu_temperature: None,
        gpu: GpuTelemetry::default(),
        media: None,
        media_error: None,
        shortcut_error: None,
        gpu_receiver,
        media_receiver,
        shortcut_receiver,
        temperature_receiver,
        main_window: None,
        media_window: None,
        manual_visibility: (!background_mode).then_some(true),
        visible: false,
        controls_visible: false,
        previous_auto_visibility: false,
        game_running: false,
        game_refresh_elapsed: Duration::ZERO,
    });

    unsafe {
        let instance = GetModuleHandleW(None)
            .map_err(|error| format!("Could not get the Windows application handle: {error}"))?;
        let cursor = LoadCursorW(None, IDC_ARROW)
            .map_err(|error| format!("Could not load the Windows pointer cursor: {error}"))?;
        let class_name = w!("DaveNativeOverlay");
        let window_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: cursor,
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&window_class) == 0 {
            return Err(format!(
                "Could not register the Dave window class: {}",
                Error::from_win32()
            ));
        }

        let state_ptr = (&mut *state as *mut AppState).cast();
        let window = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name,
            w!("Dave"),
            WS_POPUP,
            0,
            0,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            None,
            None,
            Some(instance.into()),
            Some(state_ptr),
        )
        .map_err(|error| format!("Could not create the Dave overlay window: {error}"))?;
        state.main_window = Some(window);

        SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_COLORKEY)
            .map_err(|error| format!("Could not configure the transparent overlay: {error}"))?;

        let x = GetSystemMetrics(SM_CXSCREEN) - WINDOW_WIDTH;
        SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            x,
            0,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            SWP_NOACTIVATE,
        )
        .map_err(|error| format!("Could not position the Dave overlay: {error}"))?;

        let media_window = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name,
            w!("Dave media controls"),
            WS_POPUP,
            x + BUTTONS_X,
            BUTTONS_Y,
            BUTTONS_WIDTH,
            BUTTONS_HEIGHT,
            None,
            None,
            Some(instance.into()),
            Some(state_ptr),
        )
        .map_err(|error| format!("Could not create the Dave media controls: {error}"))?;
        state.media_window = Some(media_window);
        SetLayeredWindowAttributes(media_window, COLORREF(0), 255, LWA_COLORKEY)
            .map_err(|error| format!("Could not configure the media controls: {error}"))?;
        SetWindowPos(
            media_window,
            Some(HWND_TOPMOST),
            x + BUTTONS_X,
            BUTTONS_Y,
            BUTTONS_WIDTH,
            BUTTONS_HEIGHT,
            SWP_NOACTIVATE,
        )
        .map_err(|error| format!("Could not position the Dave media controls: {error}"))?;

        let timer = SetTimer(
            Some(window),
            TIMER_ID,
            REFRESH_INTERVAL.as_millis() as u32,
            None,
        );
        if timer == 0 {
            return Err(format!(
                "Could not start the Dave refresh timer: {}",
                Error::from_win32()
            ));
        }

        state.refresh(window);
        if !state.visible {
            let _ = ShowWindow(window, SW_HIDE);
        }
        if !state.controls_visible {
            let _ = ShowWindow(media_window, SW_HIDE);
        }

        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result == 0 {
                break;
            }
            if result < 0 {
                return Err(format!(
                    "The Windows message loop failed: {}",
                    Error::from_win32()
                ));
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        KillTimer(Some(window), TIMER_ID)
            .map_err(|error| format!("Could not stop the Dave refresh timer: {error}"))?;
    }
    Ok(())
}

pub fn show_error(message: &str) {
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            windows::core::PCWSTR(message.as_ptr()),
            w!("Dave"),
            MB_OK | MB_ICONERROR,
        );
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = if message == WM_NCCREATE {
        let create = unsafe {
            &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW)
        };
        unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize) };
        create.lpCreateParams as *mut AppState
    } else {
        unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *mut AppState }
    };

    if message == WM_NCCREATE {
        return LRESULT(1);
    }
    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    if !matches!(message, WM_PAINT | WM_TIMER | WM_NCHITTEST | WM_LBUTTONUP) {
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }
    if state_ptr.is_null() {
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }

    let state = unsafe { &mut *state_ptr };
    match message {
        WM_PAINT => {
            unsafe { paint(window, state) };
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_ID {
                state.refresh(window);
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            if state.media_window == Some(window) {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTTRANSPARENT as isize)
            }
        }
        WM_LBUTTONUP => {
            if state.media_window == Some(window)
                && let Some(button) = hit_test_media_button(
                    (lparam.0 as u16) as i32,
                    ((lparam.0 as u32 >> 16) as u16) as i32,
                )
            {
                let action = match button {
                    MediaButton::Previous => "previous",
                    MediaButton::Pause => "play-pause",
                    MediaButton::Next => "next",
                };
                if let Err(error) = control_player("windows-media-keys", action) {
                    eprintln!("{error}");
                    state.shortcut_error = Some(error);
                } else {
                    state.shortcut_error = None;
                }
                let main_window = state.main_window.unwrap_or(window);
                if !unsafe { InvalidateRect(Some(main_window), None, false) }.as_bool() {
                    eprintln!("Could not refresh the Dave overlay window");
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

unsafe fn paint(window: HWND, state: &AppState) {
    let mut paint = PAINTSTRUCT::default();
    let dc = unsafe { BeginPaint(window, &mut paint) };
    let brush = unsafe { CreateSolidBrush(COLORREF(0)) };
    unsafe {
        windows::Win32::Graphics::Gdi::FillRect(dc, &paint.rcPaint, brush);
        if !DeleteObject(brush.into()).as_bool() {
            eprintln!("Could not release the Dave overlay background brush");
        }
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00f5_f7fc));
        let font = windows::Win32::Graphics::Gdi::GetStockObject(DEFAULT_GUI_FONT);
        let old_font = SelectObject(dc, font);

        if state.media_window == Some(window) {
            draw_text(dc, PREVIOUS_BUTTON + 7, 4, "<");
            draw_text(
                dc,
                PAUSE_BUTTON + 7,
                4,
                if state.media.as_ref().is_some_and(|media| media.playing) {
                    "||"
                } else {
                    ">"
                },
            );
            draw_text(dc, NEXT_BUTTON + 7, 4, ">");
        } else {
            draw_text(dc, 7, 5, &state.metric_line());
            draw_text(dc, 7, 29, &state.media_title());
        }

        SelectObject(dc, old_font);
        if !EndPaint(window, &paint).as_bool() {
            eprintln!("Could not complete drawing the Dave overlay");
        }
    }
}

unsafe fn draw_text(dc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        if !TextOutW(dc, x, y, &wide).as_bool() {
            eprintln!("Could not draw text in the Dave overlay");
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MediaButton {
    Previous,
    Pause,
    Next,
}

fn hit_test_media_button(x: i32, y: i32) -> Option<MediaButton> {
    if !(0..BUTTONS_HEIGHT).contains(&y) {
        return None;
    }
    if (PREVIOUS_BUTTON..PREVIOUS_BUTTON + BUTTON_WIDTH).contains(&x) {
        Some(MediaButton::Previous)
    } else if (PAUSE_BUTTON..PAUSE_BUTTON + BUTTON_WIDTH).contains(&x) {
        Some(MediaButton::Pause)
    } else if (NEXT_BUTTON..NEXT_BUTTON + BUTTON_WIDTH).contains(&x) {
        Some(MediaButton::Next)
    } else {
        None
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut shortened = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    shortened.push('…');
    shortened
}

fn start_temperature_monitor(sender: mpsc::SyncSender<Result<SensorTemperatures, String>>) {
    thread::spawn(move || {
        let mut last_cpu_error = None::<String>;
        loop {
            let cpu = match query_wmi_temperature("ROOT\\WMI", "MSAcpi_ThermalZoneTemperature") {
                Ok(temperature) => {
                    last_cpu_error = None;
                    temperature
                }
                Err(error) => {
                    if last_cpu_error.as_ref() != Some(&error) {
                        eprintln!("Windows CPU temperature sensor unavailable: {error}");
                    }
                    last_cpu_error = Some(error);
                    None
                }
            };
            match sender.try_send(Ok(SensorTemperatures { cpu })) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
            thread::sleep(TEMPERATURE_CHECK_INTERVAL);
        }
    });
}

fn query_wmi_temperature(namespace: &str, class: &str) -> Result<Option<i32>, String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|error| format!("Could not initialize WMI COM: {error}"))?;
    }
    struct ComUninitializer;
    impl Drop for ComUninitializer {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let _uninitializer = ComUninitializer;

    let locator: IWbemLocator = unsafe {
        CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| format!("Could not create the WMI locator: {error}"))?
    };
    let namespace = BSTR::from(namespace);
    let empty = BSTR::new();
    let services = unsafe {
        locator
            .ConnectServer(
                &namespace,
                &empty,
                &empty,
                &empty,
                0,
                &empty,
                None::<&IWbemContext>,
            )
            .map_err(|error| format!("Could not connect to WMI namespace: {error}"))?
    };
    unsafe {
        CoSetProxyBlanket(
            &services,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            None::<&windows::core::PCWSTR>,
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
        .map_err(|error| format!("Could not configure WMI access: {error}"))?;
    }

    let query = BSTR::from(format!("SELECT CurrentTemperature FROM {class}"));
    let language = BSTR::from("WQL");
    let enumerator = unsafe {
        services
            .ExecQuery(
                &language,
                &query,
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None::<&IWbemContext>,
            )
            .map_err(|error| format!("Could not query the Windows temperature sensor: {error}"))?
    };
    let mut objects = [None];
    let mut fetched = 0u32;
    let status = unsafe { enumerator.Next(500, &mut objects, &mut fetched) };
    if status.0 < 0 {
        return Err(format!(
            "Could not read the Windows temperature sensor: {status}"
        ));
    }
    if fetched == 0 {
        return Ok(None);
    }
    let Some(object) = objects.into_iter().next().flatten() else {
        return Ok(None);
    };

    let mut value = VARIANT::default();
    let get_result = unsafe { object.Get(w!("CurrentTemperature"), 0, &mut value, None, None) };
    if let Err(error) = get_result {
        return Err(format!("Could not read the temperature value: {error}"));
    }
    let raw = unsafe {
        let value = &value.Anonymous.Anonymous;
        match value.vt {
            VT_I4 => Some(i64::from(value.Anonymous.lVal)),
            VT_UI4 => Some(i64::from(value.Anonymous.ulVal)),
            _ => None,
        }
    };
    unsafe {
        VariantClear(&mut value)
            .map_err(|error| format!("Could not release the WMI temperature value: {error}"))?;
    }
    Ok(raw.and_then(wmi_temperature_to_celsius))
}

fn wmi_temperature_to_celsius(tenths_kelvin: i64) -> Option<i32> {
    let celsius = tenths_kelvin as f64 / 10.0 - 273.15;
    (celsius.is_finite() && (0.0..=200.0).contains(&celsius)).then(|| celsius.round() as i32)
}

#[cfg(test)]
mod tests {
    use super::{MediaButton, hit_test_media_button, truncate_chars, wmi_temperature_to_celsius};

    #[test]
    fn media_controls_are_the_only_clickable_regions() {
        assert_eq!(hit_test_media_button(2, 12), Some(MediaButton::Previous));
        assert_eq!(hit_test_media_button(34, 12), Some(MediaButton::Pause));
        assert_eq!(hit_test_media_button(66, 12), Some(MediaButton::Next));
        assert_eq!(hit_test_media_button(95, 12), None);
        assert_eq!(hit_test_media_button(2, 25), None);
    }

    #[test]
    fn truncates_track_titles_without_splitting_unicode_characters() {
        assert_eq!(truncate_chars("short", 8), "short");
        assert_eq!(
            truncate_chars("A very long 🎵 song title", 12),
            "A very long…"
        );
    }

    #[test]
    fn converts_supported_acpi_temperatures_and_rejects_sentinels() {
        assert_eq!(wmi_temperature_to_celsius(3000), Some(27));
        assert_eq!(wmi_temperature_to_celsius(0), None);
        assert_eq!(wmi_temperature_to_celsius(5000), None);
    }
}
