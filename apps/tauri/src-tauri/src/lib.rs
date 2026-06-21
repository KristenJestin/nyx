// The shell-agnostic core. Every business module (agent, agent_resume, command, db,
// mcp, onboarding, osc7, osc133, pathnorm, pkgjson, plugin, portless, proc, pty,
// resolve, schema, shellinteg, subfolder) now lives in `nyx-core`. They are re-exported
// at this crate's root so the Tauri adapter modules (`bridge`, `mcp_tools`) keep
// referring to them as `crate::db`, `crate::command`, … unchanged. The frontiers the
// adapter implements (EventSink via `TauriRunnerSink`/`app.emit`, AppPaths via
// `app.path()`, the service container via `app.manage`/`tauri::State`, the
// boot/shutdown lifecycle in this file's `run`) are defined Tauri-free in
// `nyx_core::frontier`.
pub use nyx_core::{
    agent, agent_resume, command, db, frontier, mcp, onboarding, osc133, osc7, pathnorm,
    pkgjson, plugin, portless, pty, resolve, schema, shellinteg, subfolder,
};
#[cfg(target_os = "linux")]
pub use nyx_core::proc;

mod bridge;
mod mcp_tools;

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
            // Terminal boot normalization (PRD task #2): settle any terminal left at
            // a persisted `exec_state = running` (a force-quit/restore artefact) to
            // idle, so no phantom running badge survives a restart. Busy/idle is
            // derived live from the OS now, so this is defensive — the dot reads the
            // busy-state signal, never the persisted field — but it also clears the
            // stored value so the dogfood symptom (terminals stuck running in the DB)
            // disappears entirely.
            bridge::normalize_terminals_from_handle(&handle);
            // Boot agent-session resume scan (PRD-5 #5): sweep stale active sessions to
            // `unknown`, then PARK a `claude --resume <id>` for every alive terminal
            // whose project opts in and whose session is resumable — injected into the
            // respawned shell when the front mounts each restored terminal's PTY.
            bridge::restore_agent_sessions_from_handle(&handle);
            // Busy/idle authority loop (PRD task #1, decision 1-B): poll the
            // foreground process group of every open PTY ~300ms and emit
            // `terminal://busy-state` on TRANSITION only. This is the OS-derived
            // running-dot signal that replaces OSC 133; a restored terminal with no
            // foreground command samples idle by construction (task #2 — no phantom
            // running survives a restart).
            bridge::start_busy_state_loop(&handle);
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
                    // Boot reconciliation (PRD-4 task #1 / PRD-5 #24, ADR-0003 D10/D11):
                    // update the `nyx` MCP entry AND the session-capture plugin in every
                    // provider the user has explicitly installed via Settings →
                    // Integrations. Only updates already-present installs (MCP url/port,
                    // plugin source path) — NEVER installs silently on boot. Best-effort:
                    // a failure (no $HOME, unwritable file) is a warning, never a boot
                    // failure — the UI must still come up. The plugin descriptor per
                    // provider is built by its agent adapter, so no agent specifics leak
                    // into the generic reconcile (finding #25); the resource dir lets a
                    // packaged build resolve the bundled plugin (finding #26).
                    let state_path = data_dir.join(onboarding::INTEGRATIONS_FILE);
                    let resource_dir = app.path().resource_dir().ok();
                    let app_data_dir = data_dir.clone();
                    // Run OFF the setup/main thread: the reconcile shells out to the agent's
                    // `claude` CLI (a child-process round-trip — `marketplace list`, and on a
                    // drift `marketplace/plugin update`), which would otherwise BLOCK the
                    // first window paint on every boot. Detached + best-effort; nothing in
                    // setup depends on its result.
                    std::thread::spawn(move || {
                        let registry = agent::AgentRegistry::default();
                        onboarding::reconcile_installed_providers(port, &state_path, |provider_key| {
                            let adapter = registry.get(provider_key)?;
                            let install =
                                adapter.plugin_install(resource_dir.as_deref(), Some(&app_data_dir))?;
                            let cli = adapter.plugin_cli()?;
                            Some((install, cli))
                        });
                    });
                }
                Err(e) => eprintln!("nyx MCP server did not start: {e}"),
            }
            app.manage(server);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod bundle_tests {
    //! Tauri-shell packaging guard (no Tauri runtime needed → runs on every host).
    use std::path::PathBuf;

    /// GUARD (finding #51): every file the bundled Claude plugin needs must be listed in
    /// THIS shell's `tauri.conf.json` `bundle.resources`, mapped from nyx-core's source
    /// tree, or the packaged Tauri build copies an incomplete plugin into
    /// `resources/claude-plugin/`. Asserts each of the four files is bundled to its
    /// expected destination AND that the nyx-core source path it maps from really exists.
    #[test]
    fn tauri_conf_bundles_every_plugin_file() {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let conf: serde_json::Value =
            serde_json::from_slice(&std::fs::read(crate_root.join("tauri.conf.json")).unwrap())
                .expect("parse tauri.conf.json");
        let resources = conf["bundle"]["resources"]
            .as_object()
            .expect("bundle.resources is a {source: dest} map");

        // dest (inside the packaged resource dir) -> the files of a loadable plugin. The
        // node session hook (`nyx-session-hook.cjs`) is REQUIRED: hooks.json invokes it, and
        // without it on Windows the SessionStart/SessionEnd channel (and the provider icon)
        // is dead — the whole point of the cross-platform node rewrite.
        let required_dests = [
            "resources/claude-plugin/.claude-plugin/marketplace.json",
            "resources/claude-plugin/.claude-plugin/plugin.json",
            "resources/claude-plugin/hooks/hooks.json",
            "resources/claude-plugin/hooks/nyx-session-hook.cjs",
            "resources/claude-plugin/.mcp.json",
        ];
        let dests: Vec<&str> = resources.values().filter_map(|v| v.as_str()).collect();
        for dest in required_dests {
            assert!(
                dests.contains(&dest),
                "tauri.conf.json bundle.resources does not ship `{dest}` — the packaged \
                 build would load an incomplete plugin. Mapped dests: {dests:?}"
            );
        }
        // Every mapped SOURCE path (relative to this crate, i.e. ../../../crates/nyx-core/…)
        // must exist, so the manifest never references a file that never ships.
        for src in resources.keys() {
            assert!(
                crate_root.join(src).exists(),
                "bundle.resources maps from `{src}` but it does not exist in the source tree"
            );
        }
    }
}
