use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::WebviewWindow;

#[cfg(target_os = "linux")]
const HOVER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);

/// One long-lived hover worker per application process. Reconfiguring pinning
/// only changes atomics; it never creates competing X11 clients or lets a stale
/// watcher overwrite a newer setting.
#[derive(Default)]
pub struct Monitor {
    enabled: AtomicBool,
    target_opacity_bits: AtomicU64,
    window_id: AtomicU64,
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
            set_native_opacity(&window, 1.0);
            return false;
        }
        let Some(window_id) = x11_window_id(&window) else {
            monitor.enabled.store(false, Ordering::Release);
            set_native_opacity(&window, 1.0);
            return false;
        };
        monitor.window_id.store(window_id, Ordering::Release);
        monitor
            .target_opacity_bits
            .store(target_opacity.clamp(0.0, 1.0).to_bits(), Ordering::Release);

        if !enabled {
            monitor.enabled.store(false, Ordering::Release);
            set_native_opacity(&window, 1.0);
            return false;
        }
        if !ensure_worker(window.clone(), Arc::clone(&monitor)) {
            monitor.enabled.store(false, Ordering::Release);
            set_native_opacity(&window, 1.0);
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
fn ensure_worker(window: WebviewWindow, monitor: Arc<Monitor>) -> bool {
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

    let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
    let worker_monitor = Arc::clone(&monitor);
    let spawn_result = std::thread::Builder::new()
        .name("metrik-pinned-hover".into())
        .spawn(move || {
            let Some(pointer) = X11Pointer::connect() else {
                worker_monitor.started.store(false, Ordering::Release);
                let _ = ready_sender.send(false);
                return;
            };
            worker_monitor.available.store(true, Ordering::Release);
            let _ = ready_sender.send(true);
            watch_x11_pointer(window, Arc::clone(&worker_monitor), pointer);
            worker_monitor.available.store(false, Ordering::Release);
            worker_monitor.started.store(false, Ordering::Release);
        });
    if spawn_result.is_err() {
        monitor.started.store(false, Ordering::Release);
        return false;
    }
    ready_receiver
        .recv_timeout(std::time::Duration::from_millis(250))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn watch_x11_pointer(window: WebviewWindow, monitor: Arc<Monitor>, pointer: X11Pointer) {
    let mut last_inside = false;
    let mut last_window_id = 0;
    let mut last_target_bits = 1.0_f64.to_bits();
    loop {
        let visible = match window.is_visible() {
            Ok(visible) => visible,
            Err(_) => break,
        };
        let enabled = monitor.enabled.load(Ordering::Acquire) && visible;
        let window_id = monitor.window_id.load(Ordering::Acquire);
        let target_bits = monitor.target_opacity_bits.load(Ordering::Acquire);
        let inside = if enabled {
            pointer
                .inside_window(window_id)
                // A transient X11 query failure keeps the previous state so
                // the widget never flashes back to full opacity for one poll.
                .unwrap_or(last_inside)
        } else {
            false
        };

        if inside != last_inside
            || window_id != last_window_id
            || (inside && target_bits != last_target_bits)
        {
            let opacity = if inside {
                f64::from_bits(target_bits).clamp(0.0, 1.0)
            } else {
                1.0
            };
            set_native_opacity(&window, opacity);
            last_inside = inside;
            last_window_id = window_id;
            last_target_bits = target_bits;
        }
        std::thread::sleep(HOVER_POLL_INTERVAL);
    }
    set_native_opacity(&window, 1.0);
}

#[cfg(target_os = "linux")]
fn point_inside_window(pointer_x: i32, pointer_y: i32, width: u32, height: u32) -> bool {
    pointer_x >= 0
        && pointer_y >= 0
        && i64::from(pointer_x) < i64::from(width)
        && i64::from(pointer_y) < i64::from(height)
}

#[cfg(target_os = "linux")]
fn x11_window_id(window: &WebviewWindow) -> Option<u64> {
    use gtk::prelude::*;

    let gtk_window = window.gtk_window().ok()?;
    let gdk_window = gtk_window.window()?;
    let x11_window = gdk_window.downcast::<gdkx11::X11Window>().ok()?;
    Some(x11_window.xid())
}

#[cfg(target_os = "linux")]
fn set_native_opacity(window: &WebviewWindow, opacity: f64) {
    use gtk::prelude::*;

    let dispatcher = window.clone();
    let target = window.clone();
    let _ = dispatcher.run_on_main_thread(move || {
        if let Ok(gtk_window) = target.gtk_window() {
            gtk_window.set_opacity(opacity);
        }
    });
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

/// The X11 connection is opened, queried and eventually closed by the same
/// worker thread. GTK owns its own display connection on the main thread.
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::point_inside_window;

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
}
