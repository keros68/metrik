use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::WebviewWindow;

#[cfg(target_os = "linux")]
const HOVER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);

/// One long-lived GTK-main-context hover timer per application process.
/// Reconfiguring pinning only changes atomics, so every GDK operation remains
/// serialized on GTK's owning thread.
#[derive(Default)]
pub struct Monitor {
    enabled: AtomicBool,
    target_opacity_bits: AtomicU64,
    started: AtomicBool,
    available: AtomicBool,
}

pub fn configure(
    window: WebviewWindow,
    monitor: Arc<Monitor>,
    enabled: bool,
    target_opacity: f64,
) -> bool {
    #[cfg(target_os = "linux")]
    {
        if !super::linux_supports_global_window_coordinates() {
            monitor.enabled.store(false, Ordering::Release);
            schedule_native_hover_state(&window, false, 1.0);
            return false;
        }
        monitor
            .target_opacity_bits
            .store(target_opacity.clamp(0.0, 1.0).to_bits(), Ordering::Release);

        if !enabled {
            monitor.enabled.store(false, Ordering::Release);
            schedule_native_hover_state(&window, false, 1.0);
            return false;
        }
        if !ensure_monitor(window.clone(), Arc::clone(&monitor)) {
            monitor.enabled.store(false, Ordering::Release);
            schedule_native_hover_state(&window, false, 1.0);
            return false;
        }
        // Repeated startup assertions are intentionally idempotent: do not
        // force opacity back to 1 between two identical pinned=true calls.
        monitor.enabled.store(true, Ordering::Release);
        monitor.available.load(Ordering::Acquire)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, monitor, enabled, target_opacity);
        false
    }
}

#[cfg(target_os = "linux")]
fn ensure_monitor(window: WebviewWindow, monitor: Arc<Monitor>) -> bool {
    if monitor.available.load(Ordering::Acquire) {
        return true;
    }
    if monitor
        .started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return monitor.available.load(Ordering::Acquire);
    }

    let dispatcher = window.clone();
    let target = window.clone();
    let timer_monitor = Arc::clone(&monitor);
    if dispatcher
        .run_on_main_thread(move || install_hover_timer(target, timer_monitor))
        .is_err()
    {
        monitor.started.store(false, Ordering::Release);
        return false;
    }
    monitor.available.store(true, Ordering::Release);
    true
}

#[cfg(target_os = "linux")]
fn install_hover_timer(window: WebviewWindow, monitor: Arc<Monitor>) {
    use gtk::prelude::*;

    let Ok(gtk_window) = window.gtk_window() else {
        monitor.available.store(false, Ordering::Release);
        monitor.started.store(false, Ordering::Release);
        return;
    };
    let Some(pointer) = X11Pointer::connect() else {
        monitor.available.store(false, Ordering::Release);
        monitor.started.store(false, Ordering::Release);
        return;
    };

    let mut last_inside = false;
    let mut last_target_bits = 1.0_f64.to_bits();
    gtk::glib::timeout_add_local(HOVER_POLL_INTERVAL, move || {
        if !monitor.enabled.load(Ordering::Acquire) {
            if last_inside {
                apply_gtk_hover_state(&gtk_window, false, 1.0);
                last_inside = false;
            }
            return gtk::glib::ControlFlow::Continue;
        }

        if !gtk_window.is_visible() {
            if last_inside {
                apply_gtk_hover_state(&gtk_window, false, 1.0);
                last_inside = false;
            }
            return gtk::glib::ControlFlow::Continue;
        }

        let target_bits = monitor.target_opacity_bits.load(Ordering::Acquire);
        let inside = gtk_pointer_inside_window(&gtk_window, &pointer).unwrap_or(last_inside);

        if inside != last_inside || (inside && target_bits != last_target_bits) {
            apply_gtk_hover_state(&gtk_window, inside, f64::from_bits(target_bits));
            last_inside = inside;
            last_target_bits = target_bits;
        }
        gtk::glib::ControlFlow::Continue
    });
}

#[cfg(target_os = "linux")]
fn hover_state(inside: bool, target_opacity: f64) -> (f64, bool) {
    let opacity = if inside {
        target_opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    // Opacity only controls painting: both fade and complete-hide are passive
    // presentation modes and must let clicks reach the desktop underneath.
    // The independent XQueryPointer connection still knows when the global
    // pointer leaves the unchanged bounds and restores hit-testing immediately.
    (opacity, inside)
}

#[cfg(target_os = "linux")]
fn apply_gtk_hover_state(gtk_window: &gtk::ApplicationWindow, inside: bool, target_opacity: f64) {
    use gtk::prelude::*;

    let (opacity, ignore_cursor_events) = hover_state(inside, target_opacity);
    let Some(gdk_window) = gtk_window.window() else {
        return;
    };

    if ignore_cursor_events {
        // Hide first, then expose the desktop underneath to input. On restore,
        // reverse the order so a visible widget never remains click-through.
        gtk_window.set_opacity(opacity);
        let empty_region = gtk::cairo::Region::create();
        gdk_window.input_shape_combine_region(&empty_region, 0, 0);
    } else {
        gtk_window.input_shape_combine_region(None);
        gtk_window.set_opacity(opacity);
    }
}

#[cfg(target_os = "linux")]
fn point_inside_window(pointer_x: i32, pointer_y: i32, width: u32, height: u32) -> bool {
    pointer_x >= 0
        && pointer_y >= 0
        && i64::from(pointer_x) < i64::from(width)
        && i64::from(pointer_y) < i64::from(height)
}

#[cfg(target_os = "linux")]
fn gtk_pointer_inside_window(
    window: &gtk::ApplicationWindow,
    pointer: &X11Pointer,
) -> Option<bool> {
    use gtk::prelude::*;

    let gdk_window = window.window()?;
    let x11_window = gdk_window.downcast::<gdkx11::X11Window>().ok()?;
    pointer.inside_window(x11_window.xid())
}

#[cfg(target_os = "linux")]
struct X11Pointer {
    display: *mut xlib::Display,
}

#[cfg(target_os = "linux")]
impl X11Pointer {
    fn connect() -> Option<Self> {
        let display = unsafe { xlib::XOpenDisplay(std::ptr::null()) };
        (!display.is_null()).then_some(Self { display })
    }

    fn inside_window(&self, window: xlib::Window) -> Option<bool> {
        let mut root_return = 0;
        let mut child_return = 0;
        let mut root_x = 0;
        let mut root_y = 0;
        let mut window_x = 0;
        let mut window_y = 0;
        let mut mask = 0;
        let success = unsafe {
            xlib::XQueryPointer(
                self.display,
                window,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut window_x,
                &mut window_y,
                &mut mask,
            )
        };
        if success == 0 {
            return None;
        }

        let mut geometry_root = 0;
        let mut x = 0;
        let mut y = 0;
        let mut width = 0;
        let mut height = 0;
        let mut border_width = 0;
        let mut depth = 0;
        let geometry_success = unsafe {
            xlib::XGetGeometry(
                self.display,
                window,
                &mut geometry_root,
                &mut x,
                &mut y,
                &mut width,
                &mut height,
                &mut border_width,
                &mut depth,
            )
        };
        (geometry_success != 0).then_some(point_inside_window(window_x, window_y, width, height))
    }
}

#[cfg(target_os = "linux")]
impl Drop for X11Pointer {
    fn drop(&mut self) {
        unsafe {
            xlib::XCloseDisplay(self.display);
        }
    }
}

#[cfg(target_os = "linux")]
mod xlib {
    use std::ffi::{c_char, c_int, c_uint, c_ulong};

    #[repr(C)]
    pub struct Display {
        _private: [u8; 0],
    }

    pub type Window = c_ulong;

    #[link(name = "X11")]
    extern "C" {
        pub fn XOpenDisplay(display_name: *const c_char) -> *mut Display;
        pub fn XQueryPointer(
            display: *mut Display,
            window: Window,
            root_return: *mut Window,
            child_return: *mut Window,
            root_x_return: *mut c_int,
            root_y_return: *mut c_int,
            window_x_return: *mut c_int,
            window_y_return: *mut c_int,
            mask_return: *mut c_uint,
        ) -> c_int;
        pub fn XGetGeometry(
            display: *mut Display,
            drawable: Window,
            root_return: *mut Window,
            x_return: *mut c_int,
            y_return: *mut c_int,
            width_return: *mut c_uint,
            height_return: *mut c_uint,
            border_width_return: *mut c_uint,
            depth_return: *mut c_uint,
        ) -> c_int;
        pub fn XCloseDisplay(display: *mut Display) -> c_int;
    }
}

#[cfg(target_os = "linux")]
fn schedule_native_hover_state(window: &WebviewWindow, inside: bool, opacity: f64) {
    let dispatcher = window.clone();
    let target = window.clone();
    let _ = dispatcher.run_on_main_thread(move || {
        if let Ok(gtk_window) = target.gtk_window() {
            apply_gtk_hover_state(&gtk_window, inside, opacity);
        }
    });
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{hover_state, point_inside_window};

    #[test]
    fn window_hit_test_uses_half_open_relative_bounds() {
        assert!(point_inside_window(0, 0, 320, 28));
        assert!(point_inside_window(319, 27, 320, 28));
        assert!(!point_inside_window(320, 27, 320, 28));
        assert!(!point_inside_window(319, 28, 320, 28));
    }

    #[test]
    fn negative_relative_coordinates_are_outside() {
        assert!(!point_inside_window(-1, 0, 320, 320));
        assert!(!point_inside_window(0, -1, 320, 320));
    }

    #[test]
    fn complete_hide_disables_native_cursor_hit_testing() {
        assert_eq!(hover_state(true, 0.0), (0.0, true));
    }

    #[test]
    fn fade_disables_native_cursor_hit_testing() {
        assert_eq!(hover_state(true, 0.35), (0.35, true));
    }

    #[test]
    fn pointer_exit_restores_visibility_and_hit_testing() {
        assert_eq!(hover_state(false, 0.0), (1.0, false));
    }
}
