//! MCP server exposed over napi (PRD-5 task #3). The nyx-core MCP server
//! ([`nyx_core::mcp::McpServer`]) runs UNDER Electron the same way it ran under
//! Tauri: a loopback `tiny_http` server on the fixed/configurable port, started once,
//! serving the `initialize` / `tools/list` handshake and `tools/call`. The decisive
//! parity point of this task is the FROZEN decision *MCP shares the same pool*: the
//! dispatcher installed here resolves its DB reads from the EXACT SAME `Arc<Db>` (r2d2
//! pool) the napi DB tasks use — there is no second connection authority and no global
//! `Mutex<SqliteConnection>`.
//!
//! ## Dispatcher scope — FULL advertised surface (PRD-5 review #68)
//!
//! The [`PoolBackedDispatcher`] serves EVERY tool advertised in `tools/list` at Tauri
//! parity, by routing through the SHARED, shell-agnostic dispatch lifted into nyx-core —
//! the SAME code the Tauri dispatcher now delegates to. There is no hand-written subset
//! anymore (the bug this review fixes), so NO advertised tool falls into the `unknown
//! tool` arm:
//!
//! - **Runtime command tools** (`start_command` / `stop_command` / `relaunch_command` /
//!   `get_command_output` / `wait_for_command`) → [`mcp_runtime::dispatch_command_tool`]
//!   onto the SHARED [`CommandRunner`] the core-host built.
//! - **Workspace registration** (`workspace_add` / `create_workspace`) →
//!   [`mcp_runtime::dispatch_workspace_tool`] off the pool.
//! - **Everything else** — the read tools (`probe` / `list_projects` / `list_workspaces`
//!   / `list_commands` / `list_importable_scripts`), the command-template CRUD
//!   (`add_command` / `update_command` / `import_commands` / `remove_command` /
//!   `remove_commands` / `remove_workspace` / `clear_command_output`), the agent-session
//!   channel (`agent_session_event`), and the interactive-terminal tools
//!   (`create_terminal` / `send_to_terminal` / `list_terminals` / `close_terminal` /
//!   `read_terminal`) → [`mcp_tools_core::dispatch_extension_tool`].
//!
//! The earlier bug — `list_commands` returning the TERMINALS table — is fixed by routing
//! it through the shared `list_commands`, which lists COMMANDS (filtered by
//! `workspace_id`/`project_id`).
//!
//! ## Event seam + live PTY (the core-host half)
//!
//! A mutating tool names the coarse `changed` topic the front must re-pull
//! ([`mcp_tools_core::ChangedTopic`]); the dispatcher pushes it to Node through a
//! fire-and-forget `ThreadsafeFunction` (`on_changed`) — the SAME `changed` invalidation
//! the host emits elsewhere, so a UI- and an MCP-driven mutation converge on one refresh
//! (e.g. a `create_terminal` fires `terminals` so the renderer mounts the xterm + spawns
//! the PTY, exactly as a UI-created terminal). The LIVE-PTY operations of the terminal
//! tools (the actual shell write of `send_to_terminal`, the PTY kill of `close_terminal`,
//! the parked opening command of `create_terminal`) are delegated to the host's Node PTY
//! manager through a second fire-and-forget `ThreadsafeFunction` (`on_terminal_op`).

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::JsFunction;
use napi_derive::napi;
use serde_json::Value;

use nyx_core::command::CommandRunner;
use nyx_core::db::Db;
use nyx_core::mcp::{McpServer, RpcCode, RpcError, ToolDispatcher};
use nyx_core::mcp_runtime;
use nyx_core::mcp_tools_core::{self, ChangedTopic, TerminalHost};

use crate::command::NodeRunnerSink;
use crate::core_db::NyxCore;

/// A coarse `changed` invalidation surfaced to Node so the host emits its
/// `*://changed` event and the renderer re-pulls (mirrors `nyx_core::frontier::ChangedTopic`).
#[napi(object)]
pub struct McpChangedEvent {
    /// `terminals` | `workspaces` | `commands` | `agent-sessions`.
    pub topic: String,
}

/// A LIVE-PTY terminal operation an MCP terminal tool needs the host's Node PTY manager
/// to perform (the half `nyx-core` cannot do — it owns the records, not the live PTY).
/// Delivered to Node fire-and-forget; the renderer-owned PTY reconciliation does the work.
#[napi(object)]
pub struct McpTerminalOp {
    /// `park` (a `create_terminal` opening command) | `send` (a `send_to_terminal` write)
    /// | `close` (kill the terminal's PTY).
    pub op: String,
    /// The terminal RECORD id the op targets.
    pub terminal_id: String,
    /// The command line for `park`/`send`; empty for `close`.
    pub command: String,
}

/// The [`TerminalHost`] backed by a Node callback: the LIVE-PTY half of the terminal
/// tools is dispatched to the host's PTY manager over a fire-and-forget
/// `ThreadsafeFunction`. The DB-record half ran in nyx-core BEFORE this is called.
///
/// The MCP server runs on its own (Rust) thread with no synchronous channel back to the
/// Node loop, so the MUTATING ops are QUEUED to Node (like Tauri's `pty_write`, which queues
/// the write). But LIVENESS is read SYNCHRONOUSLY from the `live_terminals` registry (the SAME
/// `TerminalPtyMap` role Tauri reads): `send_to_terminal` returns `invalid_state` when no PTY
/// is live for the record (parity — no mendacious `sent: true`), and `terminal_liveness`
/// reports the true `live` bit. `busy` stays `None` (the OS foreground bit lives on the Node-
/// owned `NyxPty`); the persisted `exec_state`/`exec_state_updated_at` (the agent's real
/// command-completion signal) are served from the DB by `list_terminals`.
struct NodeTerminalHost {
    /// The host's PTY-op callback. `None` for a bare/test start that did not wire it: the
    /// DB-record half still serves; the live-PTY half is dropped (a `send` then reports no
    /// live shell rather than queuing — handled by `send_to_terminal` below).
    op_tsfn: Option<ThreadsafeFunction<McpTerminalOp, ErrorStrategy::Fatal>>,
    /// The SYNCHRONOUS record↔live-PTY liveness registry (the SAME `Arc` `NyxCore` holds,
    /// fed by `register_terminal_pty` / PTY-exit cleanup). The MCP thread reads it WITHOUT a
    /// round-trip to the Node loop, so `send_to_terminal` can return `invalid_state` (Tauri
    /// parity) when no PTY is live BEFORE it queues the fire-and-forget write, and
    /// `terminal_liveness` reports the true `live` bit for `list_terminals`. A record is in
    /// the map IFF its shell currently has a live PTY.
    live_terminals: Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>>,
}

impl NodeTerminalHost {
    /// Whether a LIVE PTY is currently registered for this terminal record — the
    /// synchronous liveness check the Tauri `resolve_live_pty` does against `TerminalPtyMap`.
    fn is_live(&self, terminal_id: &str) -> bool {
        self.live_terminals
            .lock()
            .unwrap()
            .contains_key(terminal_id)
    }
}

impl NodeTerminalHost {
    fn dispatch(&self, op: &str, terminal_id: &str, command: &str) -> bool {
        let Some(tsfn) = self.op_tsfn.as_ref() else {
            return false;
        };
        tsfn.call(
            McpTerminalOp {
                op: op.to_string(),
                terminal_id: terminal_id.to_string(),
                command: command.to_string(),
            },
            ThreadsafeFunctionCallMode::NonBlocking,
        );
        true
    }
}

impl TerminalHost for NodeTerminalHost {
    fn park_opening_command(&self, terminal_id: &str, command: &str) {
        self.dispatch("park", terminal_id, command);
    }

    fn send_to_terminal(
        &self,
        terminal_id: &str,
        command: &str,
    ) -> std::result::Result<bool, String> {
        // LIVENESS GATE (PRD-5 review — send_to_terminal parity): the synchronous
        // record↔live-PTY registry decides the OUTCOME. No live PTY for this record → `false`
        // so the caller surfaces `invalid_state` ("no live shell yet") — the EXACT Tauri
        // behaviour, instead of a mendacious `sent: true` for a write that lands nowhere.
        // A live PTY → `true` AND we queue the fire-and-forget write to the Node PTY manager
        // (the alive-record gate already passed in nyx-core); the queued write is the
        // side-effect, the liveness is the contract (so a wired host always delivers, and the
        // `sent: true` the caller returns is now truthful).
        if !self.is_live(terminal_id) {
            return Ok(false);
        }
        self.dispatch("send", terminal_id, command);
        Ok(true)
    }

    fn close_terminal_pty(&self, terminal_id: &str) {
        self.dispatch("close", terminal_id, "");
    }

    fn terminal_liveness(&self, terminal_id: &str) -> (bool, Option<bool>) {
        // `live` = a PTY is registered for this record (the synchronous registry the MCP
        // thread CAN read). `busy` stays `None`: the OS foreground bit is on the live
        // `NyxPty` (Node loop) the MCP thread cannot read synchronously, and the persisted
        // `exec_state` (the real command-completion authority an agent polls) is served from
        // the DB by `list_terminals`. So `list_terminals` now reports the true `live` bit at
        // Tauri parity, with `busy` deferred to the exec-state fields.
        (self.is_live(terminal_id), None)
    }
}

/// A [`ToolDispatcher`] backed by the SHARED r2d2 pool, the managed runner, and the host's
/// Node PTY bridge — serving the FULL advertised surface at Tauri parity (PRD-5 review #68).
struct PoolBackedDispatcher {
    db: Arc<Db>,
    /// The shared cell holding the runner the host built (the SAME `NyxCore.runner` cell).
    runner: Arc<std::sync::Mutex<Option<Arc<CommandRunner<NodeRunnerSink>>>>>,
    /// Fire-and-forget `changed` invalidation to Node (the front re-pulls). `None` ⇒ no
    /// front to push to (e.g. a headless start); the mutation still commits.
    on_changed: Option<ThreadsafeFunction<McpChangedEvent, ErrorStrategy::Fatal>>,
    /// The host's Node PTY bridge for the live-PTY half of the terminal tools.
    terminal_host: NodeTerminalHost,
}

impl PoolBackedDispatcher {
    /// Push the `changed` topics a mutating tool produced to Node (fire-and-forget).
    fn emit_effects(&self, effects: &[ChangedTopic]) {
        let Some(tsfn) = self.on_changed.as_ref() else {
            return;
        };
        for topic in effects {
            tsfn.call(
                McpChangedEvent {
                    topic: topic.as_str().to_string(),
                },
                ThreadsafeFunctionCallMode::NonBlocking,
            );
        }
    }
}

impl ToolDispatcher for PoolBackedDispatcher {
    fn call(&self, name: &str, arguments: &Value) -> std::result::Result<Value, RpcError> {
        // 1) Runtime command tools (+ wait_for_command): route onto the shared runner.
        if matches!(
            name,
            "start_command"
                | "stop_command"
                | "relaunch_command"
                | "get_command_output"
                | "wait_for_command"
        ) {
            let runner = self.runner.lock().unwrap().clone();
            let Some(runner) = runner else {
                return Err(RpcError::new(
                    RpcCode::McpUnavailable,
                    format!(
                        "tool `{name}` needs the managed-command runtime, which is not started"
                    ),
                ));
            };
            // Returns `Some` for these names by construction.
            return mcp_runtime::dispatch_command_tool(&self.db, &runner, name, arguments)
                .expect("runtime command tool dispatch");
        }

        // 2) Workspace registration tools (pure pool writes).
        if let Some(res) = mcp_runtime::dispatch_workspace_tool(&self.db, name, arguments) {
            let result = res?;
            // The post-write `workspaces://changed` seam (the front re-pulls the sidebar).
            self.emit_effects(&[ChangedTopic::Workspaces]);
            return Ok(result);
        }

        // 3) Every other advertised tool (reads + CRUD + agent-session + terminals).
        // The command-template CRUD / clear_command_output read the runner's live state,
        // so they need it; the pure reads + the terminal/agent tools do not. We hand the
        // runner when present, else a fresh empty runner so a read still works (nothing is
        // running ⇒ the cold DB state is authoritative). Mutations that strictly need the
        // runner (update/remove guards) are correct either way: an empty runner reports
        // nothing running, which is true when the runner is not built yet.
        let runner = self.runner.lock().unwrap().clone();
        let outcome = match runner {
            Some(runner) => mcp_tools_core::dispatch_extension_tool(
                &self.db,
                &runner,
                &self.terminal_host,
                name,
                arguments,
            ),
            None => {
                // No managed runtime yet: build a throwaway runner over a no-op sink so the
                // reads/terminal tools still serve. The CRUD running-guards see "nothing
                // running" (true pre-runtime). This is the SAME degradation the runtime
                // command tools use (`mcp_unavailable`) — but the non-runtime tools have no
                // reason to fail, so they serve.
                let throwaway = CommandRunner::new(
                    NoopSink,
                    portable_pty::PtySize {
                        rows: 24,
                        cols: 80,
                        pixel_width: 0,
                        pixel_height: 0,
                    },
                );
                mcp_tools_core::dispatch_extension_tool(
                    &self.db,
                    &throwaway,
                    &self.terminal_host,
                    name,
                    arguments,
                )
            }
        };
        match outcome {
            Some(res) => {
                let out = res?;
                self.emit_effects(&out.effects);
                Ok(out.result)
            }
            // Genuinely unknown tool (not advertised). This is the ONLY path to
            // `method_not_found` now — no advertised tool reaches it.
            None => Err(RpcError::new(
                RpcCode::MethodNotFound,
                format!("unknown tool `{name}`"),
            )),
        }
    }
}

/// A no-op [`nyx_core::command::RunnerSink`] for the pre-runtime throwaway runner: nothing
/// is running, nothing to persist (the DB cold state is authoritative for the reads).
struct NoopSink;
impl nyx_core::command::RunnerSink for NoopSink {
    fn on_state(&self, _: &str, _: nyx_core::command::RunState, _: Option<i32>) {}
    fn on_acknowledge(&self, _: &str) {}
    fn on_output(&self, _: &str, _: &[u8]) {}
    fn persist_scrollback(&self, _: &str, _: &str) {}
    fn archive_previous_run(&self, _: &str) {}
    fn clear_output(&self, _: &str) {}
}

#[napi]
impl NyxCore {
    /// Start the MCP server (idempotent — start-once, ADR-0003 D3), installing the
    /// pool-backed dispatcher so `tools/call` resolves the FULL advertised surface from
    /// the SHARED pool + the managed runner + the host's Node PTY bridge. Returns the
    /// bound port. A bind failure (port taken) throws a readable error the host logs as a
    /// warning — it must NOT be a hard boot failure.
    ///
    /// `on_changed` (optional) receives a coarse `changed` invalidation after a mutating
    /// tool so the renderer re-pulls; `on_terminal_op` (optional) receives the live-PTY
    /// operations the terminal tools queue to the host's PTY manager. A bare/headless
    /// start may omit both (the mutations still commit; nothing is pushed to a front).
    #[napi]
    pub fn mcp_start(
        &self,
        #[napi(ts_arg_type = "(err: null | Error, ev: McpChangedEvent) => void")]
        on_changed: Option<JsFunction>,
        #[napi(ts_arg_type = "(err: null | Error, op: McpTerminalOp) => void")]
        on_terminal_op: Option<JsFunction>,
    ) -> Result<u16> {
        let mut guard = self.mcp.lock().unwrap();
        let server = guard.get_or_insert_with(|| Arc::new(McpServer::default()));

        let on_changed_tsfn = match on_changed {
            Some(cb) => Some(cb.create_threadsafe_function(0, |ctx| Ok(vec![ctx.value]))?),
            None => None,
        };
        let op_tsfn = match on_terminal_op {
            Some(cb) => Some(cb.create_threadsafe_function(0, |ctx| Ok(vec![ctx.value]))?),
            None => None,
        };

        server.set_dispatcher(Arc::new(PoolBackedDispatcher {
            db: Arc::clone(&self.db),
            runner: Arc::clone(&self.runner),
            on_changed: on_changed_tsfn,
            terminal_host: NodeTerminalHost {
                op_tsfn,
                // The SAME liveness registry `register_terminal_pty` feeds — so the MCP
                // dispatcher's send/list see live PTYs the moment the front registers them.
                live_terminals: Arc::clone(&self.live_terminals),
            },
        }));
        server
            .start()
            .map_err(|e| Error::from_reason(format!("MCP server did not start: {e}")))
    }

    /// The port the MCP server bound, or 0 if not started.
    #[napi]
    pub fn mcp_port(&self) -> u16 {
        self.mcp
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.bound_port())
            .unwrap_or(0)
    }

    /// Whether the MCP server has been started (latched).
    #[napi]
    pub fn mcp_is_started(&self) -> bool {
        self.mcp
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.is_started())
            .unwrap_or(false)
    }

    /// Boot reconciliation (PRD-5 task #4, parity with the Tauri `setup`): for every
    /// provider the user has explicitly installed, re-template/re-register its bundled
    /// plugin and propagate version bumps — but NEVER install on boot. Runs DETACHED on
    /// its own thread; best-effort. `data_dir` is the integrations-state dir;
    /// `resource_dir` (or `null`) lets a packaged build resolve the bundled plugin.
    #[napi]
    pub fn mcp_reconcile(&self, data_dir: String, resource_dir: Option<String>) {
        *self.resource_dir.lock().unwrap() = resource_dir.clone();
        let port = self
            .mcp
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.bound_port())
            .unwrap_or(0);
        if port == 0 {
            return;
        }
        std::thread::Builder::new()
            .name("nyx-mcp-reconcile".into())
            .spawn(move || {
                use nyx_core::{agent, onboarding};
                let state_path =
                    std::path::Path::new(&data_dir).join(onboarding::INTEGRATIONS_FILE);
                let app_data_dir = std::path::PathBuf::from(&data_dir);
                let resource_dir = resource_dir.map(std::path::PathBuf::from);
                let registry = agent::AgentRegistry::default();
                onboarding::reconcile_installed_providers(port, &state_path, |provider_key| {
                    let adapter = registry.get(provider_key)?;
                    let install =
                        adapter.plugin_install(resource_dir.as_deref(), Some(&app_data_dir))?;
                    let cli = adapter.plugin_cli()?;
                    Some((install, cli))
                });
            })
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_core::db::Db;
    use nyx_core::mcp::RpcCode;
    use nyx_core::mcp_tools_core::{self, TerminalHost};
    use serde_json::json;

    /// A fresh, isolated on-disk DB per test (nyx-core's `Db::in_memory` is `#[cfg(test)]`-only
    /// inside that crate, so a sibling crate opens its own file). A unique path per call keeps
    /// the tests independent (no shared-state flakiness), and the migrations run in `Db::open`.
    fn fresh_db() -> Db {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nyx-napi-mcp-test-{}-{}-{n}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Db::open(&path).expect("open temp test db")
    }

    /// Build the REAL host under review with NO op bridge (a `ThreadsafeFunction` needs a JS
    /// env, absent in a unit test). `op_tsfn: None` means the queued write is a no-op — but
    /// the LIVENESS GATE (the behaviour this finding fixes) is driven entirely by the
    /// `live_terminals` registry, so the `send_to_terminal` outcome is fully exercised.
    fn host_with_registry() -> (
        NodeTerminalHost,
        Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>>,
    ) {
        let live = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let host = NodeTerminalHost {
            op_tsfn: None,
            live_terminals: Arc::clone(&live),
        };
        (host, live)
    }

    /// Finding C: the REAL `NodeTerminalHost` (not `NoTerminalHost`) must return
    /// `invalid_state` for `send_to_terminal` on an alive record with NO live PTY — the exact
    /// Tauri parity — and must WRITE (truthful `sent: true`) once a PTY is registered live.
    #[test]
    fn send_to_terminal_on_node_host_gates_on_live_pty() {
        let db = fresh_db();
        let (host, live) = host_with_registry();

        // Create an alive terminal record (no live PTY registered yet).
        let created = mcp_tools_core::create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        // No live PTY for the record → invalid_state (NOT a mendacious sent:true), parity Tauri.
        let err = mcp_tools_core::send_to_terminal(
            &db,
            &host,
            &json!({ "terminal_id": tid, "command": "ls" }),
        )
        .unwrap_err();
        assert_eq!(
            err.code,
            RpcCode::InvalidState,
            "alive record with no live PTY must be invalid_state on the REAL host"
        );

        // Register a live PTY for the record (what `register_terminal_pty` does).
        live.lock().unwrap().insert(tid.clone(), 42);

        // Now the write is accepted (sent:true is truthful — a wired host delivers it).
        let ok = mcp_tools_core::send_to_terminal(
            &db,
            &host,
            &json!({ "terminal_id": tid, "command": "ls" }),
        )
        .unwrap();
        assert_eq!(
            ok.result["sent"],
            json!(true),
            "a live PTY → the write is accepted"
        );

        // Unregistering (PTY exit) returns it to invalid_state.
        live.lock().unwrap().remove(&tid);
        let err2 = mcp_tools_core::send_to_terminal(
            &db,
            &host,
            &json!({ "terminal_id": tid, "command": "ls" }),
        )
        .unwrap_err();
        assert_eq!(
            err2.code,
            RpcCode::InvalidState,
            "after PTY exit → invalid_state again"
        );
    }

    /// Finding A + C: `list_terminals` on the REAL host reports the true `live` bit from the
    /// liveness registry — `false` before `register_terminal_pty`, `true` after.
    #[test]
    fn list_terminals_on_node_host_reflects_liveness_registry() {
        let db = fresh_db();
        let (host, live) = host_with_registry();

        let created = mcp_tools_core::create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        // Before registration: live=false (the bug was a hard-coded false; now it is read).
        let before = mcp_tools_core::list_terminals(&db, &host, &json!({})).unwrap();
        let row = &before["terminals"].as_array().unwrap()[0];
        assert_eq!(row["terminal_id"], json!(tid));
        assert_eq!(row["live"], json!(false));

        // After registration: live=true, surfaced synchronously to the MCP dispatcher.
        live.lock().unwrap().insert(tid.clone(), 7);
        let after = mcp_tools_core::list_terminals(&db, &host, &json!({})).unwrap();
        let row = &after["terminals"].as_array().unwrap()[0];
        assert_eq!(
            row["live"],
            json!(true),
            "list_terminals reflects the live PTY"
        );
    }

    /// Direct unit of the synchronous liveness check the MCP thread relies on.
    #[test]
    fn terminal_liveness_reads_the_registry() {
        let (host, live) = host_with_registry();
        assert_eq!(host.terminal_liveness("t"), (false, None));
        live.lock().unwrap().insert("t".to_string(), 1);
        assert_eq!(host.terminal_liveness("t"), (true, None));
    }
}
