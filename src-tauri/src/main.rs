#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let command = std::env::args_os().nth(1);
    if command.as_deref() == Some(std::ffi::OsStr::new("--statusline")) {
        metrik_lib::run_statusline();
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("--publish-widget-snapshot")) {
        let Some(database_path) = std::env::args_os().nth(2).map(std::path::PathBuf::from) else {
            eprintln!("--publish-widget-snapshot requires a database path");
            std::process::exit(2);
        };
        match metrik_lib::publish_widget_snapshot_from_database(&database_path) {
            Ok(path) => println!("{}", path.display()),
            Err(error) => {
                eprintln!("could not publish WidgetKit snapshot: {error:#}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Ubuntu 24.04's WebKitGTK can select its DMA-BUF renderer before Tauri
    // creates the first window.  With NVIDIA's proprietary driver that path
    // may fail every GBM allocation, leaving the tray alive while the WebView
    // never paints.  Set the documented WebKitGTK compatibility switch here,
    // before any GTK/WebKit initialization.  An explicit caller-provided value
    // still wins so developers can retest the renderer as drivers evolve.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    metrik_lib::run();
}
