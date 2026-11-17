use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const POPUP_LABEL: &str = "strip-detail";
const DETAIL_EVENT: &str = "metrik://strip-detail";
const DETAIL_CLOSED_EVENT: &str = "metrik://strip-detail-closed";
const WARMUP_DELAY_MS: u64 = 120;
const WATCHER_POLL_MS: u64 = 80;
const CURSOR_NEAR_PAD_PX: i32 = 6;

#[derive(Default)]
struct Inner {
    generation: u64,
    visible: bool,
    payload: Option<serde_json::Value>,
    warmed: bool,
    watcher_running: bool,
}

#[derive(Default)]
pub struct StripDetailState(Mutex<Inner>);

fn apply_popup_window_styles(hwnd: isize) {
    use core::ffi::c_void;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, WS_EX_APPWINDOW,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    let handle = HWND(hwnd as *mut c_void);
    unsafe {
        let current = GetWindowLongPtrW(handle, GWL_EXSTYLE) as u32;
        let updated = (current
            | WS_EX_TOOLWINDOW.0
            | WS_EX_NOACTIVATE.0
            | WS_EX_TRANSPARENT.0
            | WS_EX_LAYERED.0)
            & !WS_EX_APPWINDOW.0;
        if updated != current {
            SetWindowLongPtrW(handle, GWL_EXSTYLE, updated as isize);
        }
        let _ = SetWindowPos(
            handle,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED
                | SWP_NOMOVE
                | SWP_NOSIZE
                | SWP_NOZORDER
                | SWP_NOOWNERZORDER
                | SWP_NOACTIVATE,
        );
    }
}

fn hide_popup_hwnd(app: &AppHandle) {
    use core::ffi::c_void;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
        SWP_NOZORDER,
    };

    let Some(popup) = app.get_webview_window(POPUP_LABEL) else {
        return;
    };
    let Ok(hwnd) = popup.hwnd() else {
        return;
    };
    let hwnd_val = hwnd.0 as isize;
    let _ = popup.run_on_main_thread(move || unsafe {
        let _ = SetWindowPos(
            HWND(hwnd_val as *mut c_void),
            None,
            0,
            0,
            0,
            0,
            SWP_HIDEWINDOW
                | SWP_NOACTIVATE
                | SWP_NOMOVE
                | SWP_NOSIZE
                | SWP_NOZORDER
                | SWP_NOOWNERZORDER,
        );
    });
}

fn cursor_near_main_window(app: &AppHandle, pad: i32) -> bool {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect, IsWindowVisible};

    let Some(main_window) = app.get_webview_window("main") else {
        return false;
    };
    let Ok(hwnd) = main_window.hwnd() else {
        return false;
    };
    let handle = HWND(hwnd.0);
    unsafe {
        if !IsWindowVisible(handle).as_bool() {
            return false;
        }
        let mut rect = RECT::default();
        if GetWindowRect(handle, &mut rect).is_err() {
            return false;
        }
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return false;
        }
        pt.x >= rect.left - pad
            && pt.x <= rect.right + pad
            && pt.y >= rect.top - pad
            && pt.y <= rect.bottom + pad
    }
}

pub fn setup(app: &tauri::App) -> tauri::Result<()> {
    if app.get_webview_window(POPUP_LABEL).is_some() {
        return Ok(());
    }

    let window = WebviewWindowBuilder::new(
        app,
        POPUP_LABEL,
        WebviewUrl::App("index.html?view=strip-detail".into()),
    )
    .title("Metrik Detail")
    .inner_size(260.0, 360.0)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focused(false)
    .visible(false)
    .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
    .build()?;

    let _ = window.set_ignore_cursor_events(true);
    if let Ok(hwnd) = window.hwnd() {
        let hwnd_val = hwnd.0 as isize;
        super::disable_system_corner_rounding(hwnd_val);
        apply_popup_window_styles(hwnd_val);
    }

    Ok(())
}

pub fn hide_internal(app: &AppHandle) {
    if let Some(state) = app.try_state::<StripDetailState>() {
        if let Ok(mut inner) = state.0.lock() {
            inner.generation += 1;
            inner.visible = false;
            inner.payload = None;
        }
    }
    hide_popup_hwnd(app);
    let _ = app.emit_to(POPUP_LABEL, DETAIL_EVENT, serde_json::Value::Null);
}

#[tauri::command]
pub fn get_strip_detail_state(
    state: tauri::State<'_, StripDetailState>,
) -> Result<Option<serde_json::Value>, String> {
    let inner = state.0.lock().map_err(|error| error.to_string())?;
    Ok(inner.payload.clone())
}

#[tauri::command]
pub fn warm_strip_detail(
    app: AppHandle,
    state: tauri::State<'_, StripDetailState>,
) -> Result<(), String> {
    use core::ffi::c_void;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    };

    {
        let mut inner = state.0.lock().map_err(|error| error.to_string())?;
        if inner.warmed {
            return Ok(());
        }
        inner.warmed = true;
        if inner.visible {
            return Ok(());
        }
    }

    if let Some(popup) = app.get_webview_window(POPUP_LABEL) {
        if let Ok(hwnd) = popup.hwnd() {
            let hwnd_val = hwnd.0 as isize;
            let _ = popup.run_on_main_thread(move || unsafe {
                let _ = SetWindowPos(
                    HWND(hwnd_val as *mut c_void),
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_SHOWWINDOW | SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER,
                );
            });
        }
    }

    let app_handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(std::time::Duration::from_millis(WARMUP_DELAY_MS));
        let should_hide = app_handle
            .try_state::<StripDetailState>()
            .and_then(|s| s.0.lock().ok().map(|inner| !inner.visible))
            .unwrap_or(true);
        if should_hide {
            hide_popup_hwnd(&app_handle);
        }
    });
    Ok(())
}

#[tauri::command]
pub fn request_strip_detail(
    app: AppHandle,
    state: tauri::State<'_, StripDetailState>,
    mut payload: serde_json::Value,
) -> Result<u64, String> {
    let generation = {
        let mut inner = state.0.lock().map_err(|error| error.to_string())?;
        inner.generation += 1;
        let generation = inner.generation;
        if let Some(map) = payload.as_object_mut() {
            map.insert("generation".to_string(), serde_json::json!(generation));
        }
        inner.payload = Some(payload.clone());
        generation
    };
    let _ = app.emit_to(POPUP_LABEL, DETAIL_EVENT, &payload);
    Ok(generation)
}

#[tauri::command]
pub fn present_strip_detail(
    app: AppHandle,
    state: tauri::State<'_, StripDetailState>,
    generation: u64,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    use core::ffi::c_void;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_SHOWWINDOW,
    };

    {
        let inner = state.0.lock().map_err(|error| error.to_string())?;
        if inner.generation != generation || inner.payload.is_none() {
            return Ok(());
        }
    }

    if !cursor_near_main_window(&app, CURSOR_NEAR_PAD_PX) {
        hide_internal(&app);
        let _ = app.emit_to("main", DETAIL_CLOSED_EVENT, ());
        return Ok(());
    }

    let spawn_watcher = {
        let mut inner = state.0.lock().map_err(|error| error.to_string())?;
        if inner.generation != generation || inner.payload.is_none() {
            return Ok(());
        }
        inner.visible = true;
        let should_spawn = !inner.watcher_running;
        if should_spawn {
            inner.watcher_running = true;
        }
        should_spawn
    };

    if let Some(popup) = app.get_webview_window(POPUP_LABEL) {
        if let Ok(hwnd) = popup.hwnd() {
            let hwnd_val = hwnd.0 as isize;
            let width_i32 = width.max(1) as i32;
            let height_i32 = height.max(1) as i32;
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            popup
                .run_on_main_thread(move || {
                    let result = unsafe {
                        SetWindowPos(
                            HWND(hwnd_val as *mut c_void),
                            Some(HWND_TOPMOST),
                            x,
                            y,
                            width_i32,
                            height_i32,
                            SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                        )
                    };
                    let _ = sender.send(result.map_err(|error| error.to_string()));
                })
                .map_err(|error| error.to_string())?;
            receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|error| error.to_string())??;
        }
    }

    if spawn_watcher {
        let app_handle = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let mut outside_ticks = 0u8;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(WATCHER_POLL_MS));
                let near = cursor_near_main_window(&app_handle, CURSOR_NEAR_PAD_PX);
                let Some(state) = app_handle.try_state::<StripDetailState>() else {
                    break;
                };
                let mut should_hide = false;
                {
                    let Ok(mut inner) = state.0.lock() else {
                        break;
                    };
                    if !inner.visible {
                        inner.watcher_running = false;
                        break;
                    }
                    if near {
                        outside_ticks = 0;
                        continue;
                    }
                    outside_ticks = outside_ticks.saturating_add(1);
                    if outside_ticks >= 2 {
                        inner.generation += 1;
                        inner.visible = false;
                        inner.payload = None;
                        inner.watcher_running = false;
                        should_hide = true;
                    }
                }
                if should_hide {
                    hide_popup_hwnd(&app_handle);
                    let _ = app_handle.emit_to(POPUP_LABEL, DETAIL_EVENT, serde_json::Value::Null);
                    let _ = app_handle.emit_to("main", DETAIL_CLOSED_EVENT, ());
                    break;
                }
            }
        });
    }
    Ok(())
}

#[tauri::command]
pub fn hide_strip_detail(app: AppHandle) -> Result<(), String> {
    hide_internal(&app);
    Ok(())
}
