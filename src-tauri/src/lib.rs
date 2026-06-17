mod bridge;
mod command;
mod db;
mod mcp;
mod mcp_tools;
mod onboarding;
// OSC 133 shell-integration parser + the exec-state gate decision (ADR-0002,
// PRD 2.1 task #1). Pure parser; wired into the bridge output pump in phase 2.
mod osc133;
mod osc7;
mod pathnorm;
mod pkgjson;
mod portless;
#[cfg(target_os = "linux")]
mod proc;
mod pty;
mod resolve;
mod schema;
// Shell-integration injection (OSC 133 emit hooks for bash/zsh/PowerShell;
// PRD-2.1 task #5). Pure classification + snippet generation; applied in `pty`.
mod shellinteg;
mod subfolder;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use tauri::Manager;

use db::Db;
use mcp::McpServer;

/// File name of nyx's SQLite database inside `app_data_dir`.
const DB_FILE: &str = "nyx.db";

/// Resolve nyx's data directory. Honors the `NYX_DATA_DIR` env override, falling
/// back to Tauri's `app_data_dir`. The override lets the e2e suite pin a
/// deterministic, per-run DB location on EVERY platform: `app_data_dir` is
/// OS-specific and, on Windows, is NOT steerable via `XDG_DATA_HOME` (that only
/// affects the Linux path), so a portable knob is needed for the restore specs.
/// Generic over any `Manager` (both `App` and `AppHandle` implement it), so the
/// setup hook (`&App`) and the runtime bridge commands (`AppHandle`) resolve the
/// SAME data dir through one helper.
pub(crate) fn resolve_data_dir<R, M>(app: &M) -> anyhow::Result<PathBuf>
where
    R: tauri::Runtime,
    M: tauri::Manager<R>,
{
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
///
/// D1 (fail-fast): [`Db::open`] calls `db::run_migrations` which propagates any
/// migration failure as an `Err`. This function propagates that `Err` to the Tauri
/// `setup` closure, which then surfaces it via `.expect("error while running tauri
/// application")` — nyx refuses to start rather than serving a broken schema.
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
            // Resolve the data dir once so later steps in setup can reference it
            // (e.g. the MCP reconcile needs `integrations.json` from the same dir).
            let data_dir = resolve_data_dir(app)?;
            // Register the managed-command runner (managed state for the lifecycle
            // commands) now that the AppHandle exists and the Db is managed.
            let handle = app.handle().clone();
            bridge::manage_command_runner(&handle);
            // Boot restoration: relaunch the instances the last shutdown snapshot
            // marked (restart_on_startup ON + was_running_on_shutdown), normalize
            // orphaned `running` to idle, and reset the snapshot.
            bridge::restore_commands_from_handle(&handle);
            // Local MCP HTTP server (PRD-4, ADR-0003): one loopback server on the
            // fixed/configurable port, owned by this single live nyx process. A
            // second nyx focuses the existing window (single-instance plugin) and
            // never reaches this setup, so there is at most one server (D3). Bind
            // failures (port already taken) are surfaced as a warning, not a hard
            // boot failure — the UI must still come up.
            let server = Arc::new(McpServer::default());
            // Phase-2 (PRD-4 #3/#4): install the PRD-2/PRD-3-backed tool dispatcher
            // BEFORE the listener accepts requests, so `tools/call` routes to the
            // SAME Db + ManagedCommandRunner the UI drives (ADR-0003 D6) instead of
            // the phase-1 "not yet available" stub. The Db and runner are already
            // managed above, so the dispatcher's managed-state lookups resolve.
            server.set_dispatcher(Arc::new(mcp_tools::NyxToolDispatcher::new(handle.clone())));
            match server.start() {
                Ok(port) => {
                    eprintln!("nyx MCP server listening on http://127.0.0.1:{port}/mcp");
                    // Boot reconciliation (PRD-4 task #1, ADR-0003 D10/D11): update the
                    // `nyx` MCP entry in every provider config the user has explicitly
                    // installed via Settings → Integrations. Only updates already-present
                    // entries (url/port) — never creates a new entry silently. Best-effort:
                    // a failure (no $HOME, unwritable file) is a warning, never a boot
                    // failure — the UI must still come up.
                    let state_path = data_dir.join(onboarding::INTEGRATIONS_FILE);
                    onboarding::reconcile_installed_providers(port, &state_path);
                }
                Err(e) => eprintln!("nyx MCP server did not start: {e}"),
            }
            app.manage(server);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
