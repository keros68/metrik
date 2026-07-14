//! macOS 专属外壳：菜单栏 NSPanel + 独立的完整视图窗口。
//!
//! Windows 上 Metrik 是一个会变形的无边框窗口（小插件 ⇄ 完整视图），带自绘窗口按钮。
//! macOS 的原生形态不同：菜单栏图标点开一个不抢焦点的面板，完整视图是另一个标准窗口。
//! NSPanel 的样式掩码与可缩放标准窗口互斥，所以这里是两个窗口，而不是一个窗口变形。

use std::sync::Mutex;

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    ActivationPolicy, AppHandle, Manager, PhysicalPosition, Rect, WebviewUrl, WebviewWindowBuilder,
};
use tauri_nspanel::cocoa::appkit::{NSMainMenuWindowLevel, NSWindowCollectionBehavior};
use tauri_nspanel::{panel_delegate, ManagerExt, WebviewWindowExt};

/// NSWindowStyleMaskNonActivatingPanel：面板获得键盘焦点时不激活本 App，
/// 用户正在用的窗口不会失焦。这是菜单栏应用与普通窗口最本质的区别。
const NONACTIVATING_PANEL: i32 = 1 << 7;

/// 面板与菜单栏之间的呼吸缝（逻辑像素）。
const MENU_BAR_GAP: f64 = 6.0;
/// 面板贴近屏幕边缘时保留的余量（逻辑像素）。
const SCREEN_MARGIN: f64 = 8.0;

const PANEL_LABEL: &str = "main";
const EXPANDED_LABEL: &str = "expanded";

/// 托盘图标最后一次上报的屏幕矩形。菜单项里的"显示 / 隐藏"拿不到点击事件的 rect，
/// 用它把面板对齐到图标下方；托盘的任何一次事件（点击/移入/移动）都会刷新。
static TRAY_RECT: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);

pub fn setup(app: &mut tauri::App) -> tauri::Result<()> {
    // 只保留菜单栏图标，不占 Dock；打开完整视图时再临时切回 Regular。
    app.set_activation_policy(ActivationPolicy::Accessory);
    to_menubar_panel(app.app_handle());
    // 菜单栏应用启动时不弹面板，等用户点图标。
    hide_panel(app.app_handle());
    setup_tray(app)
}

/// 把 main 窗口换成菜单栏面板：不抢焦点、浮在全屏应用之上、失焦自动收起。
fn to_menubar_panel(app: &AppHandle) {
    let Some(window) = app.get_webview_window(PANEL_LABEL) else {
        return;
    };
    let panel = match window.to_panel() {
        Ok(panel) => panel,
        Err(error) => {
            // 面板不可用时窗口仍是普通窗口，功能不塌，只是不像菜单栏应用。
            eprintln!("Metrik could not turn its widget into a menu bar panel ({error:?})");
            return;
        }
    };

    panel.set_level(NSMainMenuWindowLevel + 1);
    panel.set_style_mask(NONACTIVATING_PANEL);
    panel.set_collection_behaviour(
        NSWindowCollectionBehavior::NSWindowCollectionBehaviorCanJoinAllSpaces
            | NSWindowCollectionBehavior::NSWindowCollectionBehaviorStationary
            | NSWindowCollectionBehavior::NSWindowCollectionBehaviorFullScreenAuxiliary,
    );

    let delegate = panel_delegate!(MetrikPanelDelegate {
        window_did_resign_key
    });
    let handle = app.clone();
    delegate.set_listener(Box::new(move |event: String| {
        if event == "window_did_resign_key" {
            hide_panel(&handle);
        }
    }));
    panel.set_delegate(delegate);
}

pub fn hide_panel(app: &AppHandle) {
    if let Ok(panel) = app.get_webview_panel(PANEL_LABEL) {
        panel.order_out(None);
    }
}

pub fn show_panel(app: &AppHandle) {
    let Ok(panel) = app.get_webview_panel(PANEL_LABEL) else {
        return;
    };
    position_panel(app);
    panel.show();
}

fn toggle_panel(app: &AppHandle) {
    let Ok(panel) = app.get_webview_panel(PANEL_LABEL) else {
        return;
    };
    if panel.is_visible() {
        panel.order_out(None);
        return;
    }
    position_panel(app);
    panel.show();
}

/// 把面板水平居中对齐到托盘图标、垂直贴在菜单栏下方；靠近屏幕右缘时向内收，不出屏。
fn position_panel(app: &AppHandle) {
    let Some(window) = app.get_webview_window(PANEL_LABEL) else {
        return;
    };
    let Some((icon_x, icon_y, icon_width, icon_height)) = *TRAY_RECT.lock().unwrap() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);

    let width = f64::from(size.width);
    let mut x = icon_x + icon_width / 2.0 - width / 2.0;
    let y = icon_y + icon_height + MENU_BAR_GAP * scale;

    // 图标所在的那块屏幕（多显示器下菜单栏可能不在主屏）。
    let screen = window
        .available_monitors()
        .unwrap_or_default()
        .into_iter()
        .find(|monitor| {
            let position = monitor.position();
            let monitor_size = monitor.size();
            icon_x >= f64::from(position.x)
                && icon_x < f64::from(position.x) + f64::from(monitor_size.width)
        })
        .or_else(|| window.primary_monitor().ok().flatten());

    if let Some(monitor) = screen {
        let margin = SCREEN_MARGIN * scale;
        let left = f64::from(monitor.position().x) + margin;
        let right =
            f64::from(monitor.position().x) + f64::from(monitor.size().width) - width - margin;
        if right >= left {
            x = x.clamp(left, right);
        }
    }

    let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

/// 完整视图是一个独立的标准窗口：原生红绿灯、可缩放、进 Dock 与 Cmd-Tab。
/// 面板（NSPanel）无法兼任这个角色，所以单开一个窗口。
pub fn open_expanded_window(app: AppHandle, nav: Option<String>) -> Result<(), String> {
    hide_panel(&app);

    if let Some(window) = app.get_webview_window(EXPANDED_LABEL) {
        let _ = app.set_activation_policy(ActivationPolicy::Regular);
        window.show().map_err(|error| error.to_string())?;
        window.unminimize().ok();
        window.set_focus().map_err(|error| error.to_string())?;
        return Ok(());
    }

    let mut url = String::from("index.html?view=expanded");
    if let Some(nav) = nav.as_deref() {
        url.push_str("&nav=");
        url.push_str(nav);
    }

    let window = WebviewWindowBuilder::new(&app, EXPANDED_LABEL, WebviewUrl::App(url.into()))
        .title("Metrik")
        .inner_size(1120.0, 760.0)
        .min_inner_size(960.0, 700.0)
        .resizable(true)
        .center()
        .build()
        .map_err(|error| error.to_string())?;

    // 完整视图开着时才进 Dock；它被关掉后回到纯菜单栏应用。
    let handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let _ = handle.set_activation_policy(ActivationPolicy::Accessory);
        }
    });

    let _ = app.set_activation_policy(ActivationPolicy::Regular);
    window.set_focus().map_err(|error| error.to_string())?;
    Ok(())
}

fn remember_tray_rect(window_scale: f64, rect: Rect) {
    let position = rect.position.to_physical::<f64>(window_scale);
    let size = rect.size.to_physical::<f64>(window_scale);
    *TRAY_RECT.lock().unwrap() = Some((position.x, position.y, size.width, size.height));
}

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    // 面板里没有窗口按钮（macOS 上那四个按钮不渲染），被移除的功能落在这个菜单里。
    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏", true, None::<&str>)?;
    let expanded = MenuItem::with_id(app, "expanded", "完整视图", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Metrik", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &expanded, &settings, &quit])?;

    // 菜单栏要的是单色字形，不是彩色 App 图标：template 图由系统按浅/深色菜单栏反色。
    let icon = Image::from_bytes(include_bytes!("../icons/tray-macos.png"))?;

    TrayIconBuilder::with_id("main")
        .tooltip("Metrik")
        .icon(icon)
        .icon_as_template(true)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => toggle_panel(app),
            "expanded" => {
                let _ = open_expanded_window(app.clone(), None);
            }
            "settings" => {
                let _ = open_expanded_window(app.clone(), Some("settings".into()));
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            let app = tray.app_handle();
            let scale = app
                .get_webview_window(PANEL_LABEL)
                .and_then(|window| window.scale_factor().ok())
                .unwrap_or(1.0);

            // 任何托盘事件都刷新图标位置，菜单里的"显示 / 隐藏"也就有了对齐依据。
            match &event {
                TrayIconEvent::Click { rect, .. }
                | TrayIconEvent::Enter { rect, .. }
                | TrayIconEvent::Move { rect, .. }
                | TrayIconEvent::Leave { rect, .. } => remember_tray_rect(scale, *rect),
                _ => {}
            }

            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_panel(app);
            }
        })
        .build(app)?;
    Ok(())
}
