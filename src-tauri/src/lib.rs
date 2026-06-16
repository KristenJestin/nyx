mod bridge;
mod command;
mod db;
mod osc7;
mod pathnorm;
mod pkgjson;
#[cfg(target_os = "linux")]
mod proc;
mod pty;
mod resolve;
mod schema;
mod subfolder;

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

/// Apply the WebKitGTK rendering workarounds nyx needs to paint reliably on the
/// Linux WebView (wry → WebKitGTK). On many Linux/WSLg stacks the default DMABUF
/// renderer and the accelerated compositing path produce a BLANK WebView (the GL
/// context never presents), so React mounts but nothing is shown and — under
/// `tauri-driver`/WebKitWebDriver — the page script never runs to completion,
/// leaving `window.__nyx` permanently null (the PRD-1 e2e 0/3 failure: the front
/// bootstrap never reaches `create_terminal`).
///
/// `WEBKIT_DISABLE_DMABUF_RENDERER=1` forces the portable (non-DMABUF) software
/// presentation path; `WEBKIT_DISABLE_COMPOSITING_MODE=1` disables accelerated
/// compositing. These are the WebKitGTK-documented escape hatches for exactly
/// this class of blank-render bug and are inert on stacks that didn't need them.
///
/// We set them ONLY when UNSET so a user/operator can still override per-launch
/// (e.g. force-enable DMABUF on a known-good GPU). Set BEFORE the WebView is
/// created — `WebKitWebProcess` reads them at WebView construction time — so the
/// very first window paints correctly. Linux-only: these vars don't exist on the
/// WebView2 (Windows) / WKWebView (macOS) backends.
#[cfg(target_os = "linux")]
fn apply_webkit_rendering_workarounds() {
    for (key, val) in [
        ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
        ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
    ] {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, val);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Linux WebKitGTK needs rendering workarounds to avoid a blank WebView; must
    // be set before the WebView is constructed (see the function docs).
    #[cfg(target_os = "linux")]
    apply_webkit_rendering_workarounds();

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
    // Native folder picker for the manual add-project / add-workspace flow.
    builder = builder.plugin(tauri_plugin_dialog::init());
    builder = bridge::init(builder);

    // Shutdown snapshot + reap: when the main window is closing, persist which
    // command instances are running so the boot flow can relaunch exactly those
    // (gated by each template's restart_on_startup toggle), THEN tree-kill the live
    // processes so managed commands are not orphaned past exit. Using
    // `on_window_event` for `CloseRequested`/`Destroyed` captures both the
    // user-close and app-quit paths (the reap is latched to run once).
    builder = builder.on_window_event(|window, event| {
        use tauri::WindowEvent;
        if matches!(
            event,
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
        ) {
            bridge::snapshot_commands_from_handle(&window.app_handle().clone());
        }
    });

    builder
        .setup(|app| {
            setup_db(app)?;
            // Register the managed-command runner (managed state for the lifecycle
            // commands) now that the AppHandle exists and the Db is managed.
            let handle = app.handle().clone();
            bridge::manage_command_runner(&handle);
            // Boot restoration: relaunch the instances the last shutdown snapshot
            // marked (restart_on_startup ON + was_running_on_shutdown), normalize
            // orphaned `running` to idle, and reset the snapshot.
            bridge::restore_commands_from_handle(&handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
