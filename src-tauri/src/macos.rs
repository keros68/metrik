//! macOS 专属外壳：菜单栏 NSPanel + 桌面组件 + 独立的完整视图窗口。
//!
//! tauri-nspanel 仍绑在已弃用的 cocoa/objc 上（上游未迁 objc2），它的 panel_delegate!
//! 宏展开里还带着过时的 cfg(cargo-clippy)。两个 lint 只在本文件关掉，不影响其余代码。
//!
//! Windows 上 Metrik 是一个会变形的无边框窗口（小插件 ⇄ 完整视图），带自绘窗口按钮。
//! macOS 的原生形态不同：菜单栏图标点开一个不抢焦点的面板；可选桌面组件待在
//! 普通窗口下方；完整视图是另一个标准窗口。三种原生语义不能由一个窗口变形兼任。

#![allow(deprecated)]
#![allow(unexpected_cfgs)]

use std::io::Cursor;
use std::sync::Mutex;

use image::imageops::FilterType;
use objc2::{msg_send, AnyThread, ClassType, MainThreadMarker};
use objc2_app_kit::{
    NSAttributedStringAttachmentConveniences, NSColor, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSImage, NSTextAttachment,
};
use objc2_foundation::{
    NSAttributedString, NSData, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize,
    NSString,
};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    ActivationPolicy, AppHandle, LogicalSize, Manager, PhysicalPosition, Rect, Runtime, WebviewUrl,
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
const DESKTOP_WIDGET_LABEL: &str = "desktop-widget";
const EXPANDED_LABEL: &str = "expanded";
const STATUS_ITEM_ID: &str = "metrik-status";
const STATUS_ITEM_AUTOSAVE_NAME: &str = "app.metrik.desktop.status";
const DESKTOP_WIDGET_WIDTH: f64 = 320.0;
const DESKTOP_WIDGET_HEIGHT: f64 = 312.0;
const STATUS_ICON_SIZE: u32 = 44;
const PROVIDER_MARK_SIZE: u32 = 32;
const MENU_BAR_ICON_SIZE: f64 = 16.0;

/// 本地视觉验收开关。只在 debug 构建生效，避免为了看桌面层级去改用户持久设置；
/// release 构建仍严格遵守“默认关闭”。
#[cfg(debug_assertions)]
fn desktop_widget_preview_forced() -> bool {
    std::env::var("METRIK_DESKTOP_WIDGET_PREVIEW")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

#[cfg(not(debug_assertions))]
fn desktop_widget_preview_forced() -> bool {
    false
}

const CHATGPT_MARK: &[u8] = include_bytes!("../../src/assets/chatgpt-app-icon.png");
const CLAUDE_MARK: &[u8] = include_bytes!("../../src/assets/claude-app-icon.jpg");
const ZCODE_MARK: &[u8] = include_bytes!("../../src/assets/zcode-app-icon.png");
const OPENCODE_MARK: &[u8] = include_bytes!("../../src/assets/opencode-app-icon.png");
const KIMI_MARK: &[u8] = include_bytes!("../../src/assets/kimi-app-icon.png");
const ANTIGRAVITY_MARK: &[u8] = include_bytes!("../../src/assets/antigravity-app-icon.png");
const WORKBUDDY_MARK: &[u8] = include_bytes!("../../src/assets/workbuddy-app-icon.png");
const QODER_MARK: &[u8] = include_bytes!("../../src/assets/qoder-app-icon.png");
const GROK_MARK: &[u8] = include_bytes!("../../src/assets/grok-app-icon.png");
const PI_MARK: &[u8] = include_bytes!("../../src/assets/pi-app-icon.png");
const QWEN_MARK: &[u8] = include_bytes!("../../src/assets/qwen-app-icon.png");
const HERMES_MARK: &[u8] = include_bytes!("../../src/assets/hermes-app-icon.png");

#[derive(Clone, Copy)]
struct StatusItemSpec {
    id: &'static str,
    name: &'static str,
    icon: &'static [u8],
}

const STATUS_ITEMS: [StatusItemSpec; 12] = [
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
        name: "GLM",
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
    StatusItemSpec {
        id: "workbuddy",
        name: "WorkBuddy",
        icon: WORKBUDDY_MARK,
    },
    StatusItemSpec {
        id: "qoder",
        name: "Qoder",
        icon: QODER_MARK,
    },
    StatusItemSpec {
        id: "grok",
        name: "Grok",
        icon: GROK_MARK,
    },
    StatusItemSpec {
        id: "pi",
        name: "Pi",
        icon: PI_MARK,
    },
    StatusItemSpec {
        id: "qwen",
        name: "Qwen",
        icon: QWEN_MARK,
    },
    StatusItemSpec {
        id: "hermes",
        name: "Hermes",
        icon: HERMES_MARK,
    },
];

/// 托盘图标最后一次上报的屏幕矩形。菜单项里的"显示 / 隐藏"拿不到点击事件的 rect，
/// 用它把面板对齐到图标下方；托盘的任何一次事件（点击/移入/移动）都会刷新。
static TRAY_RECT: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);

/// macOS 26 由 ControlCenter 托管每个 NSStatusItem 的外层槽。即使 AppKit 侧把
/// length 设为 0，多个空 item 仍会在系统菜单栏留下间距。因此 Metrik 整个会话
/// 只持有这一个原生 item；Agent 只是它内部 attributedTitle 的内容片段。
fn initialize_native_status_item<R: Runtime>(
    tray: &tauri::tray::TrayIcon<R>,
) -> Result<(), String> {
    tray.with_inner_tray_icon(move |inner| {
        let status_item = inner
            .ns_status_item()
            .ok_or_else(|| "macOS NSStatusItem 不可用".to_owned())?;
        let autosave_name = NSString::from_str(STATUS_ITEM_AUTOSAVE_NAME);
        status_item.setAutosaveName(Some(&autosave_name));
        // autosaveName 会恢复上次持久化的可见性；Metrik 至少保留一个 Agent，
        // 这个唯一状态项应始终存在且可见。首轮额度到达前暂时折叠内容。
        status_item.setVisible(true);
        status_item.setLength(0.0);
        Ok::<(), String>(())
    })
    .map_err(|error| error.to_string())?
}

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

struct StatusItemSegment {
    icon: &'static [u8],
    title: String,
    accessibility_label: String,
}

fn status_item_segments(
    agents: &[String],
    remaining: &[Option<f64>],
    stale: &[bool],
) -> Result<Vec<StatusItemSegment>, String> {
    agents
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let spec = STATUS_ITEMS
                .iter()
                .find(|spec| spec.id == agent)
                .ok_or_else(|| format!("Metrik 未知 Agent {agent}"))?;
            let title = status_item_title(remaining[index], stale[index]);
            let availability = match normalized_percent(remaining[index]) {
                Some(percent) => format!("{percent}% 剩余"),
                None => "配额不可用".to_owned(),
            };
            let freshness = if stale[index] {
                "，数据可能已过期"
            } else {
                ""
            };
            Ok(StatusItemSegment {
                icon: spec.icon,
                title,
                accessibility_label: format!("{} {availability}{freshness}", spec.name),
            })
        })
        .collect()
}

fn status_item_accessibility_label(segments: &[StatusItemSegment]) -> String {
    format!(
        "Metrik：{}",
        segments
            .iter()
            .map(|segment| segment.accessibility_label.as_str())
            .collect::<Vec<_>>()
            .join("；")
    )
}

fn provider_status_ns_image(source: &[u8]) -> Result<objc2::rc::Retained<NSImage>, String> {
    let icon = provider_status_icon(source)?;
    let rgba = image::RgbaImage::from_raw(icon.width(), icon.height(), icon.rgba().to_vec())
        .ok_or_else(|| "菜单栏品牌图标像素尺寸不一致".to_owned())?;
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|error| format!("菜单栏品牌图标无法编码：{error}"))?;
    let encoded = encoded.into_inner();
    // dataWithBytes 会立即复制像素，返回后 Vec 可以安全释放。
    let data = unsafe { NSData::dataWithBytes_length(encoded.as_ptr().cast(), encoded.len()) };
    let image = NSImage::initWithData(NSImage::alloc(), &data)
        .ok_or_else(|| "菜单栏品牌图标无法创建 NSImage".to_owned())?;
    image.setSize(NSSize::new(MENU_BAR_ICON_SIZE, MENU_BAR_ICON_SIZE));
    image.setTemplate(true);
    Ok(image)
}

fn append_status_text(
    output: &NSMutableAttributedString,
    text: &str,
    font: &NSFont,
    color: &NSColor,
) {
    let piece = NSMutableAttributedString::from_nsstring(&NSString::from_str(text));
    let range = NSRange::new(0, piece.length());
    // NSFont / NSColor 都是合法的 Objective-C attribute value；两个对象在调用
    // 期间存活，并会由 attributed string retain。
    unsafe {
        piece.addAttribute_value_range(NSFontAttributeName, font.as_super(), range);
        piece.addAttribute_value_range(NSForegroundColorAttributeName, color.as_super(), range);
    }
    output.appendAttributedString(&piece);
}

fn set_native_status_item_content<R: Runtime>(
    tray: &tauri::tray::TrayIcon<R>,
    segments: Vec<StatusItemSegment>,
) -> Result<(), String> {
    let accessibility_label = status_item_accessibility_label(&segments);
    tray.with_inner_tray_icon(move |inner| {
        let status_item = inner
            .ns_status_item()
            .ok_or_else(|| "macOS NSStatusItem 不可用".to_owned())?;
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "macOS 菜单栏内容更新未运行在主线程".to_owned())?;
        let button = status_item
            .button(mtm)
            .ok_or_else(|| "macOS NSStatusBarButton 不可用".to_owned())?;
        let attributed = NSMutableAttributedString::new();
        let font = NSFont::menuBarFontOfSize(0.0);
        let color = NSColor::labelColor();

        for (index, segment) in segments.iter().enumerate() {
            if index > 0 {
                append_status_text(&attributed, "   ", &font, &color);
            }
            let attachment = NSTextAttachment::new();
            let image = provider_status_ns_image(segment.icon)?;
            attachment.setImage(Some(&image));
            // AppKit 的文字基线比 16pt 图标底部高约 2pt；负偏移让图标与数字居中。
            attachment.setBounds(NSRect::new(
                NSPoint::new(0.0, -2.0),
                NSSize::new(MENU_BAR_ICON_SIZE, MENU_BAR_ICON_SIZE),
            ));
            let icon = NSAttributedString::attributedStringWithAttachment(&attachment);
            attributed.appendAttributedString(&icon);
            append_status_text(&attributed, &format!(" {}", segment.title), &font, &color);
        }

        // 先清掉 tray-icon 可能留下的普通 image/title，再一次性写入完整富文本。
        // attributedTitle 让 ControlCenter 只托管一个 item，同时保留每个 Agent 的
        // 品牌图标和额度，不需要隐藏原生占位槽。
        button.setImage(None);
        button.setTitle(&NSString::from_str(""));
        button.setAttributedTitle(&attributed);
        let accessibility_label = NSString::from_str(&accessibility_label);
        unsafe {
            let _: () = msg_send![&*button, setAccessibilityLabel: Some(&*accessibility_label)];
        }
        status_item.setLength(-1.0);
        status_item.setVisible(true);
        Ok::<(), String>(())
    })
    .map_err(|error| error.to_string())?
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

    eprintln!("Metrik status item requested: {}", agents.join(","));
    let segments = status_item_segments(agents, remaining, stale)?;
    let tooltip = status_item_accessibility_label(&segments);
    let tray = app
        .tray_by_id(STATUS_ITEM_ID)
        .ok_or_else(|| "Metrik 菜单栏状态项不存在".to_owned())?;
    tray.set_tooltip(Some(tooltip))
        .map_err(|error| error.to_string())?;
    set_native_status_item_content(&tray, segments)?;
    eprintln!("Metrik status item updated");

    Ok(())
}

pub fn setup(app: &mut tauri::App) -> tauri::Result<()> {
    // 只保留菜单栏图标，不占 Dock；打开完整视图时再临时切回 Regular。
    app.set_activation_policy(ActivationPolicy::Accessory);
    to_menubar_panel(app.app_handle());
    // 菜单栏应用启动时不弹面板，等用户点图标。
    hide_panel(app.app_handle());
    setup_tray(app)?;
    if desktop_widget_preview_forced() {
        if let Err(error) = set_desktop_widget_visible(app.app_handle(), true) {
            eprintln!("Metrik could not open the desktop-widget preview ({error})");
        }
    }
    Ok(())
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

/// 可选桌面组件是独立窗口：位于普通应用窗口下方、出现在所有桌面空间，
/// 但仍可在桌面上点击和拖动。关闭设置时只隐藏而不销毁，避免反复创建 WebView。
pub fn set_desktop_widget_visible(app: &AppHandle, visible: bool) -> Result<(), String> {
    // debug 验收模式强制保持可见；正常构建与正常启动完全由设置开关决定。
    let visible = visible || desktop_widget_preview_forced();
    if let Some(window) = app.get_webview_window(DESKTOP_WIDGET_LABEL) {
        if visible {
            window
                .set_always_on_bottom(true)
                .map_err(|error| error.to_string())?;
            window
                .set_visible_on_all_workspaces(true)
                .map_err(|error| error.to_string())?;
            // borderless 透明 NSWindow 的系统阴影仍按矩形外框投射，会在 CSS
            // 圆角外露出第二层方角。材质自身的单层高光足够，不要这道影子。
            window
                .set_shadow(false)
                .map_err(|error| error.to_string())?;
            window.show().map_err(|error| error.to_string())?;
        } else {
            window.hide().map_err(|error| error.to_string())?;
        }
        return Ok(());
    }

    if !visible {
        return Ok(());
    }

    let window = WebviewWindowBuilder::new(
        app,
        DESKTOP_WIDGET_LABEL,
        WebviewUrl::App("index.html?view=desktop-widget".into()),
    )
    .title("Metrik Desktop Widget")
    .inner_size(DESKTOP_WIDGET_WIDTH, DESKTOP_WIDGET_HEIGHT)
    .min_inner_size(DESKTOP_WIDGET_WIDTH, DESKTOP_WIDGET_HEIGHT)
    .max_inner_size(DESKTOP_WIDGET_WIDTH, DESKTOP_WIDGET_HEIGHT)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_bottom(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .focused(false)
    .center()
    .prevent_overflow()
    .build()
    .map_err(|error| error.to_string())?;

    // 和菜单栏面板一样跟随系统 appearance；具体玻璃密度由 WebView 的 scrim 调整。
    let _ = window.set_theme(None);
    window
        .set_always_on_bottom(true)
        .map_err(|error| error.to_string())?;
    window
        .set_shadow(false)
        .map_err(|error| error.to_string())?;
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

/// Tauri 给每个 macOS 窗口默认打开 FullSizeContentView（tauri#3914 的变通），
/// 但在 macOS 26 上它会让手动指定的深色外观进不了标题栏——深色内容上面总顶着
/// 一条白色标题栏。完整视图是唯一带原生标题栏的窗口，创建后清掉这个标志，
/// 标题栏就能正常跟随窗口外观明暗。
fn strip_fullsize_content_view(window: &tauri::WebviewWindow) {
    use objc::{msg_send, sel, sel_impl};
    use tauri_nspanel::cocoa::base::id;

    const FULLSIZE_CONTENT_VIEW: usize = 1 << 15;

    let Ok(ns_window) = window.ns_window() else {
        return;
    };
    let ns_window = ns_window as usize;
    let _ = window.app_handle().run_on_main_thread(move || unsafe {
        let ns_window = ns_window as id;
        let mask: usize = msg_send![ns_window, styleMask];
        let _: () = msg_send![ns_window, setStyleMask: mask & !FULLSIZE_CONTENT_VIEW];
    });
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
    strip_fullsize_content_view(&window);

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

fn build_status_item(app: &AppHandle) -> tauri::Result<()> {
    // 一个原生状态项承载全部 Agent；内部片段变化不会让 ControlCenter 增删宿主。
    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏", true, None::<&str>)?;
    let expanded = MenuItem::with_id(app, "expanded", "完整视图", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Metrik", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &expanded, &settings, &quit])?;

    let tray = TrayIconBuilder::with_id(STATUS_ITEM_ID)
        .tooltip("Metrik")
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
    initialize_native_status_item(&tray)
        .map_err(|error| tauri::Error::Anyhow(anyhow::Error::msg(error)))?;
    Ok(())
}

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    // Metrik 自己的状态栏语法仍是每个 Agent 一个品牌图标 + 额度数字，但它们
    // 组合在同一个长期存活的 NSStatusItem 内。未选 Agent 没有任何原生占位对象。
    build_status_item(app.app_handle())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_visible_agent_has_a_menu_bar_status_item() {
        let status_ids = STATUS_ITEMS.map(|item| item.id);
        assert_eq!(status_ids, crate::domain::AGENT_IDS);
    }

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

    #[test]
    fn menu_bar_uses_one_stable_native_status_item() {
        assert_eq!(STATUS_ITEM_ID, "metrik-status");
        assert_eq!(STATUS_ITEM_AUTOSAVE_NAME, "app.metrik.desktop.status");
    }

    #[test]
    fn status_segments_preserve_selection_order_and_accessibility_state() {
        let agents = vec!["kimi".to_owned(), "codex".to_owned(), "zcode".to_owned()];
        let segments = status_item_segments(
            &agents,
            &[Some(80.0), Some(77.0), Some(98.0)],
            &[true, false, false],
        )
        .unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.title.as_str())
                .collect::<Vec<_>>(),
            vec!["~80%", "77%", "98%"]
        );
        assert_eq!(
            status_item_accessibility_label(&segments),
            "Metrik：Kimi 80% 剩余，数据可能已过期；ChatGPT 77% 剩余；GLM 98% 剩余"
        );
    }
}
