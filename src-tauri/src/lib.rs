mod bridge;
mod pty;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    // Single-instance: a second launch of nyx focuses the existing `main`
    // window instead of opening a duplicate. Desktop-only (the plugin does not
    // build on mobile targets).
    #[cfg(desktop)]
    {
        use tauri::Manager;
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }

    builder = builder.plugin(tauri_plugin_opener::init());
    builder = bridge::init(builder);

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
