mod bridge;
mod db;
#[cfg(target_os = "linux")]
mod proc;
mod pty;
mod schema;

use std::fs;
use std::path::PathBuf;

use tauri::Manager;

use db::Db;

/// File name of nyx's SQLite database inside `app_data_dir`.
const DB_FILE: &str = "nyx.db";

/// Resolve nyx's data directory. Honors the `NYX_DATA_DIR` env override, falling
/// back to Tauri's `app_data_dir`. The override lets the e2e suite pin a
/// deterministic, per-run DB location on EVERY platform: `app_data_dir` is
/// OS-specific and, on Windows, is NOT steerable via `XDG_DATA_HOME` (that only
/// affects the Linux path), so a portable knob is needed for the restore specs.
fn resolve_data_dir<R: tauri::Runtime>(app: &tauri::App<R>) -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("NYX_DATA_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    app.path()
        .app_data_dir()
        .map_err(|e| anyhow::anyhow!("could not resolve app_data_dir: {e}"))
}

/// Open the database under the resolved data dir, creating the directory if
/// needed, and register it as managed Tauri state. Migrations run in [`Db::open`].
fn setup_db<R: tauri::Runtime>(app: &tauri::App<R>) -> anyhow::Result<()> {
    let data_dir = resolve_data_dir(app)?;
    fs::create_dir_all(&data_dir)
        .map_err(|e| anyhow::anyhow!("could not create {}: {e}", data_dir.display()))?;
    let db = Db::open(&data_dir.join(DB_FILE))?;
    app.manage(db);
    Ok(())
}

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
        .setup(|app| {
            setup_db(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
