//! The MCP HTTP server in the sidecar (HHD — `01KWCJPFP72FP5CDVWSB8DTHHD`).
//!
//! Ports the v1 `nyx-napi` MCP wiring (which the TS core-host owned) into the
//! sidecar. The nyx-core MCP server ([`nyx_core::mcp::McpServer`]) runs the SAME way
//! it ran under Electron: a loopback `tiny_http` server on the fixed/configurable
//! port, started once, serving the `initialize` / `tools/list` handshake and
//! `tools/call`. The decisive parity point — *MCP shares the same pool* — holds: the
//! dispatcher installed here resolves its DB reads from the EXACT SAME `Arc<Db>` the
//! pipe dispatch + the managed-command runner use (no second connection authority).
//!
//! The dispatcher serves the FULL advertised v1 surface by routing through the
//! SHARED, shell-agnostic dispatch lifted into nyx-core:
//!   * runtime command tools -> [`mcp_runtime::dispatch_command_tool`] onto the
//!     SHARED [`CommandRunner`];
//!   * workspace registration -> [`mcp_runtime::dispatch_workspace_tool`];
//!   * everything else (reads, CRUD, agent-session, interactive-terminal tools) ->
//!     [`mcp_tools_core::dispatch_extension_tool`].
//!
//! The live-PTY half of the terminal tools + the `changed` invalidations go through a
//! [`SidecarTerminalHost`] bound to the sidecar's PTY manager + the pipe event sink —
//! the sidecar twin of the v1 Node bridge, but in-process (no Node round-trip).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nyx_core::agent_activity::AgentActivityStore;
use nyx_core::command::CommandRunner;
use nyx_core::db::Db;
use nyx_core::mcp::{McpServer, RpcCode, RpcError, ToolDispatcher};
use nyx_core::mcp_runtime;
use nyx_core::mcp_tools_core::{self, ChangedTopic, TerminalHost};
use serde_json::Value;

use super::command::SidecarRunnerSink;
use super::events::EventSink;
use super::pty::PtyManager;
use crate::pipe::wire::Event;

/// The [`TerminalHost`] backed by the sidecar's PTY manager + the pipe event sink — the
/// sidecar twin of the v1 `NodeTerminalHost`. The DB-record half of a terminal tool
/// runs in nyx-core BEFORE this is called; this is the LIVE-PTY half.
pub struct SidecarTerminalHost {
    pty: Arc<PtyManager>,
    /// The SAME record -> live-pty registry the PTY manager maintains, read
    /// SYNCHRONOUSLY so `send_to_terminal` returns `invalid_state` when no PTY is live
    /// (parity) and `terminal_liveness` reports the true `live` bit.
    live_records: Arc<Mutex<HashMap<String, u32>>>,
    events: EventSink,
}

impl SidecarTerminalHost {
    fn is_live(&self, terminal_id: &str) -> bool {
        self.live_records.lock().unwrap().contains_key(terminal_id)
    }
}

impl TerminalHost for SidecarTerminalHost {
    fn park_opening_command(&self, terminal_id: &str, command: &str) {
        // The renderer owns terminal mounting; surface the parked opening command as a
        // `changed` invalidation so the front re-pulls and mounts/opens the terminal.
        // (The DB record was already created in nyx-core.) The opening command is
        // carried in the record; the front reconciles it on the `terminals` refresh.
        let _ = (terminal_id, command);
        self.events.emit(Event::Changed {
            topic: ChangedTopic::Terminals.as_str().to_string(),
        });
    }

    fn send_to_terminal(&self, terminal_id: &str, command: &str) -> Result<bool, String> {
        // LIVENESS GATE (parity): no live PTY for this record -> Ok(false) so the caller
        // surfaces `invalid_state`, never a mendacious sent:true. A live PTY -> write the
        // command LINE + a carriage return (the v1 host appended `\r`).
        if !self.is_live(terminal_id) {
            return Ok(false);
        }
        let mut bytes = command.as_bytes().to_vec();
        bytes.push(b'\r');
        Ok(self.pty.write_to_record(terminal_id, &bytes))
    }

    fn send_keys(&self, terminal_id: &str, bytes: &[u8]) -> Result<bool, String> {
        // SAME liveness gate; the payload is the RAW pre-resolved bytes (NO appended
        // newline) the v1 host wrote verbatim.
        if !self.is_live(terminal_id) {
            return Ok(false);
        }
        Ok(self.pty.write_to_record(terminal_id, bytes))
    }

    fn close_terminal_pty(&self, terminal_id: &str) {
        self.pty.close_record(terminal_id);
    }

    fn terminal_liveness(&self, terminal_id: &str) -> (bool, Option<bool>) {
        // `live` = a PTY is registered for this record. `busy` stays `None` (the OS
        // foreground bit is served from the DB exec-state by list_terminals), exactly as
        // in the v1 host.
        (self.is_live(terminal_id), None)
    }
}

/// A [`ToolDispatcher`] backed by the SHARED pool + the managed runner + the sidecar's
/// PTY bridge — serving the FULL advertised v1 surface (parity with the v1
/// `PoolBackedDispatcher`).
pub struct SidecarDispatcher {
    db: Arc<Db>,
    /// The shared cell holding the runner the sidecar built. `None` before the runtime
    /// is up; the runtime command tools then degrade to `mcp_unavailable`.
    runner: Arc<Mutex<Option<Arc<CommandRunner<SidecarRunnerSink>>>>>,
    terminal_host: SidecarTerminalHost,
    /// The runtime agent-activity store the `agent_session_event` hooks write.
    activity: Arc<AgentActivityStore>,
    /// The pipe event sink, used to push the `changed` invalidations a mutating tool
    /// produces (the front re-pulls).
    events: EventSink,
}

impl SidecarDispatcher {
    fn emit_effects(&self, effects: &[ChangedTopic]) {
        for topic in effects {
            self.events.emit(Event::Changed {
                topic: topic.as_str().to_string(),
            });
        }
    }
}

impl ToolDispatcher for SidecarDispatcher {
    fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
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
            return mcp_runtime::dispatch_command_tool(&self.db, &runner, name, arguments)
                .expect("runtime command tool dispatch");
        }

        // 2) Workspace registration tools (pure pool writes).
        if let Some(res) = mcp_runtime::dispatch_workspace_tool(&self.db, name, arguments) {
            let result = res?;
            self.emit_effects(&[ChangedTopic::Workspaces]);
            return Ok(result);
        }

        // 3) Every other advertised tool (reads + CRUD + agent-session + terminals).
        let runner = self.runner.lock().unwrap().clone();
        let outcome = match runner {
            Some(runner) => mcp_tools_core::dispatch_extension_tool(
                &self.db,
                &runner,
                &self.terminal_host,
                &self.activity,
                name,
                arguments,
            ),
            None => {
                // No managed runtime yet: a throwaway runner over a no-op sink so the
                // reads / terminal tools still serve (the cold DB state is authoritative).
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
                    &self.activity,
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
            None => Err(RpcError::new(
                RpcCode::MethodNotFound,
                format!("unknown tool `{name}`"),
            )),
        }
    }
}

/// A no-op [`nyx_core::command::RunnerSink`] for the pre-runtime throwaway runner.
struct NoopSink;
impl nyx_core::command::RunnerSink for NoopSink {
    fn on_state(&self, _: &str, _: nyx_core::command::RunState, _: Option<i32>) {}
    fn on_acknowledge(&self, _: &str) {}
    fn on_output(&self, _: &str, _: &[u8]) {}
    fn persist_scrollback(&self, _: &str, _: &str) {}
    fn archive_previous_run(&self, _: &str) {}
    fn clear_output(&self, _: &str) {}
}

/// The MCP runtime the sidecar owns: the `tiny_http` server + the shared activity store
/// + the runner cell the dispatcher routes onto. Built once at boot.
pub struct McpRuntime {
    server: Arc<McpServer>,
    db: Arc<Db>,
    runner: Arc<Mutex<Option<Arc<CommandRunner<SidecarRunnerSink>>>>>,
    activity: Arc<AgentActivityStore>,
    pty: Arc<PtyManager>,
    events: EventSink,
}

impl McpRuntime {
    /// Build the MCP runtime over the shared services. The server is NOT started yet
    /// (start-once happens on the first [`start`](McpRuntime::start)). `activity` is the
    /// SHARED runtime agent-activity store — the SAME `Arc` the pipe `agent_activity_snapshot`
    /// / `agent_mark_ready_read` commands read/clear, so the live dot the MCP per-turn hooks
    /// write is the one the renderer pulls (no second store).
    pub fn new(
        db: Arc<Db>,
        runner: Arc<Mutex<Option<Arc<CommandRunner<SidecarRunnerSink>>>>>,
        pty: Arc<PtyManager>,
        activity: Arc<AgentActivityStore>,
        events: EventSink,
    ) -> Self {
        McpRuntime {
            server: Arc::new(McpServer::default()),
            db,
            runner,
            activity,
            pty,
            events,
        }
    }

    /// The SHARED runtime agent-activity store (the live "what is Claude doing" dot). The
    /// pipe `agent_activity_snapshot` reads it and `agent_mark_ready_read` clears it; the
    /// MCP `agent_session_event` hooks write it — the SAME `Arc`.
    pub fn activity(&self) -> Arc<AgentActivityStore> {
        Arc::clone(&self.activity)
    }

    /// Start the MCP HTTP server (idempotent — start-once), installing the dispatcher so
    /// `tools/call` resolves the FULL advertised surface from the SHARED pool + the
    /// runner + the sidecar's PTY bridge. Returns the bound loopback port. A bind failure
    /// (port taken) is a readable error the caller logs as a warning — NOT a hard boot
    /// failure (it returns `Ok(0)` so the rest of the sidecar serves).
    pub fn start(&self) -> u16 {
        self.server.set_dispatcher(Arc::new(SidecarDispatcher {
            db: Arc::clone(&self.db),
            runner: Arc::clone(&self.runner),
            terminal_host: SidecarTerminalHost {
                pty: Arc::clone(&self.pty),
                live_records: self.pty.live_records(),
                events: self.events.clone(),
            },
            activity: Arc::clone(&self.activity),
            events: self.events.clone(),
        }));
        match self.server.start() {
            Ok(port) => port,
            Err(e) => {
                self.events
                    .log("warn", format!("MCP server did not start: {e}"));
                0
            }
        }
    }

    /// The port the MCP server bound, or 0 if not started.
    pub fn bound_port(&self) -> u16 {
        self.server.bound_port()
    }

    /// Whether the MCP server has been started (latched).
    pub fn is_started(&self) -> bool {
        self.server.is_started()
    }
}
