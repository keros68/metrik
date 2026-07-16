//! macOS 专属外壳：菜单栏 NSPanel + 独立的完整视图窗口。
//!
//! tauri-nspanel 仍绑在已弃用的 cocoa/objc 上（上游未迁 objc2），它的 panel_delegate!
//! 宏展开里还带着过时的 cfg(cargo-clippy)。两个 lint 只在本文件关掉，不影响其余代码。
//!
//! Windows 上 Metrik 是一个会变形的无边框窗口（小插件 ⇄ 完整视图），带自绘窗口按钮。
//! macOS 的原生形态不同：菜单栏图标点开一个不抢焦点的面板，完整视图是另一个标准窗口。
//! NSPanel 的样式掩码与可缩放标准窗口互斥，所以这里是两个窗口，而不是一个窗口变形。

#![allow(deprecated)]
#![allow(unexpected_cfgs)]

use std::sync::Mutex;

use image::imageops::FilterType;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    ActivationPolicy, AppHandle, LogicalSize, Manager, PhysicalPosition, Rect, WebviewUrl,
    WebviewWindowBuilder,
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
const STATUS_ICON_SIZE: u32 = 44;
const PROVIDER_MARK_SIZE: u32 = 32;

const CHATGPT_MARK: &[u8] = include_bytes!("../../src/assets/chatgpt-app-icon.png");
const CLAUDE_MARK: &[u8] = include_bytes!("../../src/assets/claude-app-icon.jpg");
const ZCODE_MARK: &[u8] = include_bytes!("../../src/assets/zcode-app-icon.png");
const OPENCODE_MARK: &[u8] = include_bytes!("../../src/assets/opencode-app-icon.png");
const KIMI_MARK: &[u8] = include_bytes!("../../src/assets/kimi-app-icon.png");
const ANTIGRAVITY_MARK: &[u8] = include_bytes!("../../src/assets/antigravity-app-icon.png");

#[derive(Clone, Copy)]
struct StatusItemSpec {
    id: &'static str,
    name: &'static str,
    icon: &'static [u8],
}

const STATUS_ITEMS: [StatusItemSpec; 6] = [
    StatusItemSpec {
        id: "codex",
        name: "ChatGPT",
        icon: CHATGPT_MARK,
    },
    StatusItemSpec {
        id: "claude",
        name: "Claude",
        icon: CLAUDE_MARK,
    },
    StatusItemSpec {
        id: "zcode",
        name: "ZCode / GLM",
        icon: ZCODE_MARK,
    },
    StatusItemSpec {
        id: "opencode",
        name: "OpenCode",
        icon: OPENCODE_MARK,
    },
    StatusItemSpec {
        id: "kimi",
        name: "Kimi",
        icon: KIMI_MARK,
    },
    StatusItemSpec {
        id: "antigravity",
        name: "Antigravity",
        icon: ANTIGRAVITY_MARK,
    },
];

/// 托盘图标最后一次上报的屏幕矩形。菜单项里的"显示 / 隐藏"拿不到点击事件的 rect，
/// 用它把面板对齐到图标下方；托盘的任何一次事件（点击/移入/移动）都会刷新。
static TRAY_RECT: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);

fn normalized_percent(value: Option<f64>) -> Option<u8> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 100.0).round().clamp(0.0, 100.0) as u8)
}

/// 把应用里已有的官方品牌图标转成 macOS template image。背景由四角颜色推断并
/// 去除，保留原始品牌轮廓；系统负责按浅/深菜单栏自动反色。
fn provider_status_icon(source: &[u8]) -> Result<Image<'static>, String> {
    let decoded = image::load_from_memory(source)
        .map_err(|error| format!("菜单栏品牌图标无法解码：{error}"))?
        .to_rgba8();
    let resized = image::imageops::resize(
        &decoded,
        PROVIDER_MARK_SIZE,
        PROVIDER_MARK_SIZE,
        FilterType::Lanczos3,
    );
    let corners = [
        resized.get_pixel(0, 0),
        resized.get_pixel(PROVIDER_MARK_SIZE - 1, 0),
        resized.get_pixel(0, PROVIDER_MARK_SIZE - 1),
        resized.get_pixel(PROVIDER_MARK_SIZE - 1, PROVIDER_MARK_SIZE - 1),
    ];
    let background = [0, 1, 2].map(|channel| {
        (corners
            .iter()
            .map(|pixel| u32::from(pixel[channel]))
            .sum::<u32>()
            / corners.len() as u32) as u8
    });

    let mut rgba = vec![0; (STATUS_ICON_SIZE * STATUS_ICON_SIZE * 4) as usize];
    let offset = (STATUS_ICON_SIZE - PROVIDER_MARK_SIZE) / 2;
    for (x, y, pixel) in resized.enumerate_pixels() {
        let distance = [0, 1, 2]
            .map(|channel| pixel[channel].abs_diff(background[channel]))
            .into_iter()
            .max()
            .unwrap_or_default();
        // JPEG 背景会有少量压缩噪点，8 以下视为背景；150 的色差即完全不透明。
        let alpha = u16::from(distance.saturating_sub(8)) * 255 / 142;
        let alpha = alpha.min(255) as u8;
        let alpha = (u16::from(alpha) * u16::from(pixel[3]) / 255) as u8;
        let output_x = x + offset;
        let output_y = y + offset;
        let index = ((output_y * STATUS_ICON_SIZE + output_x) * 4) as usize;
        rgba[index..index + 3].fill(255);
        rgba[index + 3] = alpha;
    }

    Ok(Image::new_owned(rgba, STATUS_ICON_SIZE, STATUS_ICON_SIZE))
}

fn status_item_title(remaining: Option<f64>, stale: bool) -> String {
    match normalized_percent(remaining) {
        Some(percent) if stale => format!("~{percent}%"),
        Some(percent) => format!("{percent}%"),
        None => "--".into(),
    }
}

pub fn update_status_items(
    app: &AppHandle,
    agents: &[String],
    remaining: &[Option<f64>],
    stale: &[bool],
) -> Result<(), String> {
    if agents.len() != remaining.len() || agents.len() != stale.len() {
        return Err("macOS 菜单栏状态项参数长度不一致".into());
    }

    for spec in STATUS_ITEMS {
        let Some(tray) = app.tray_by_id(spec.id) else {
            return Err(format!("macOS {} 菜单栏状态项不存在", spec.name));
        };
        let selected_index = agents.iter().position(|agent| agent == spec.id);
        tray.set_visible(selected_index.is_some())
            .map_err(|error| error.to_string())?;
        let Some(index) = selected_index else {
            continue;
        };
        let item_remaining = remaining[index];
        let item_stale = stale[index];
        tray.set_title(Some(status_item_title(item_remaining, item_stale)))
            .map_err(|error| error.to_string())?;
        let label = match normalized_percent(item_remaining) {
            Some(percent) => format!("{} {percent}% 剩余", spec.name),
            None => format!("{} 配额不可用", spec.name),
        };
        let suffix = if item_stale {
            " · 数据可能已过期"
        } else {
            ""
        };
        tray.set_tooltip(Some(format!("Metrik · {label}{suffix}")))
            .map_err(|error| error.to_string())?;
    }

    Ok(())
}

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
    // 跟随 macOS 当前系统外观，不把 vibrancy 锁死为 dark。内容层会分别为
    // light/dark 材质保证对比度。
    let _ = window.set_theme(None);
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

/// 在紧凑卡片与胶囊条之间变形，并按新尺寸重新对齐菜单栏图标。
/// 尺寸范围在命令入口校验；这里保持原生 NSPanel 的层级和行为不变。
pub fn resize_panel(app: &AppHandle, width: f64, height: f64) -> Result<(), String> {
    let Some(window) = app.get_webview_window(PANEL_LABEL) else {
        return Err("macOS 菜单栏面板不存在".into());
    };
    window
        .set_size(LogicalSize::new(width, height))
        .map_err(|error| error.to_string())?;
    // 不依赖 resize 事件是否已回写 outer_size；直接用目标逻辑宽度计算锚点，
    // 避免卡片/胶囊切换的一帧里仍按旧宽度定位。
    position_panel_with_width(app, Some(width));
    Ok(())
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
    position_panel_with_width(app, None);
}

fn position_panel_with_width(app: &AppHandle, logical_width: Option<f64>) {
    let Some(window) = app.get_webview_window(PANEL_LABEL) else {
        return;
    };
    let Some((icon_x, icon_y, icon_width, icon_height)) = *TRAY_RECT.lock().unwrap() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let width = match logical_width {
        Some(width) => width * scale,
        None => match window.outer_size() {
            Ok(size) => f64::from(size.width),
            Err(_) => return,
        },
    };
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

fn build_status_item(
    app: &mut tauri::App,
    spec: StatusItemSpec,
    icon: Image<'static>,
    visible: bool,
) -> tauri::Result<()> {
    // 每个 Agent 状态项都能独立打开同一个面板，右键菜单也保持一致。
    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏", true, None::<&str>)?;
    let expanded = MenuItem::with_id(app, "expanded", "完整视图", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Metrik", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &expanded, &settings, &quit])?;

    let tray = TrayIconBuilder::with_id(spec.id)
        .tooltip(format!("Metrik · {}", spec.name))
        .title(status_item_title(None, false))
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
    tray.set_visible(visible)?;
    Ok(())
}

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    // Metrik 自己的状态栏语法：每个已选 Agent 一个品牌状态项，只带额度数字。
    // 不复制第三方工具的多账户、重置倒计时或菜单布局。
    // macOS 会把后创建的状态项放在左侧，所以反向创建，最终视觉顺序与设置列表一致。
    for spec in STATUS_ITEMS.into_iter().rev() {
        let icon = provider_status_icon(spec.icon)
            .map_err(|error| tauri::Error::Anyhow(anyhow::Error::msg(error)))?;
        let visible = matches!(spec.id, "codex" | "claude");
        build_status_item(app, spec, icon, visible)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_title_clamps_percentages_and_marks_stale_data() {
        assert_eq!(normalized_percent(Some(-5.0)), Some(0));
        assert_eq!(normalized_percent(Some(120.0)), Some(100));
        assert_eq!(normalized_percent(Some(f64::NAN)), None);
        assert_eq!(status_item_title(Some(94.0), false), "94%");
        assert_eq!(status_item_title(Some(94.0), true), "~94%");
        assert_eq!(status_item_title(None, false), "--");
    }

    #[test]
    fn provider_status_icons_use_real_brand_assets_as_template_images() {
        for spec in STATUS_ITEMS {
            let icon = provider_status_icon(spec.icon).expect("provider mark should decode");
            assert_eq!(icon.width(), STATUS_ICON_SIZE);
            assert_eq!(icon.height(), STATUS_ICON_SIZE);
            assert!(icon.rgba().chunks_exact(4).any(|pixel| pixel[3] > 200));
            assert!(icon.rgba().chunks_exact(4).any(|pixel| pixel[3] == 0));
        }
    }
}
