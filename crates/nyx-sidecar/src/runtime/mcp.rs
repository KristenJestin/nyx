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
use serde_json::{json, Value};

use super::command::SidecarRunnerSink;
use super::events::{EventSink, PushEventSink};
use super::git_watch::GitWatchRuntime;
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
    /// The shared cell holding the v2.4 real-time git-watch runtime (Q-015). Lets an MCP
    /// client register/unregister a `(workspace, repository)` review watch via
    /// `repo_watch` / `repo_unwatch` — the SAME watch the UI db-command drives (PDR-0005),
    /// so a change pushes `repos://changed` to BOTH surfaces. `None` before the runtime is up.
    git_watch: Arc<Mutex<Option<Arc<GitWatchRuntime>>>>,
}

impl SidecarDispatcher {
    fn emit_effects(&self, effects: &[ChangedTopic]) {
        for topic in effects {
            self.events.emit(Event::Changed {
                topic: topic.as_str().to_string(),
            });
        }
    }

    /// Handle the v2.4 `repo_watch` / `repo_unwatch` MCP tools (Q-015): register / release
    /// a real-time watch of a `(workspace, repository)` review. Resolves the repo's absolute
    /// path from the SHARED pool (identity isolation), then drives the SAME git-watch runtime
    /// the UI `repo_watch` db-command drives. Returns `Some(result)` when `name` is one of
    /// these tools, `None` otherwise (so the dispatcher falls through to the rest of the surface).
    fn dispatch_repo_watch(&self, name: &str, args: &Value) -> Option<Result<Value, RpcError>> {
        let unwatch = match name {
            "repo_watch" => false,
            "repo_unwatch" => true,
            _ => return None,
        };
        Some((|| {
            let workspace_id = require_str(args, "workspace_id")?;
            let repository_id = require_str(args, "repository_id")?;
            let git_watch = self.git_watch.lock().unwrap().clone().ok_or_else(|| {
                RpcError::new(
                    RpcCode::McpUnavailable,
                    "the real-time git-watch runtime is not started".to_string(),
                )
            })?;
            if unwatch {
                git_watch.unwatch(workspace_id, repository_id);
                return Ok(json!({ "watching": false }));
            }
            let base = args.get("base").and_then(Value::as_str).map(str::to_string);
            let target = args
                .get("target")
                .and_then(Value::as_str)
                .map(str::to_string);
            let absolute_path = self
                .db
                .with_conn(|c| {
                    nyx_core::repository::member_absolute_path(c, workspace_id, repository_id)
                })
                .map_err(|e| RpcError::new(RpcCode::Internal, e.to_string()))?
                .ok_or_else(|| {
                    RpcError::new(
                        RpcCode::InvalidId,
                        format!("no repository {repository_id} in workspace {workspace_id}"),
                    )
                })?;
            git_watch.watch(workspace_id, repository_id, &absolute_path, base, target);
            Ok(json!({ "watching": true }))
        })())
    }
}

/// Require a non-empty string arg from an MCP tool's JSON object (the sidecar-side twin of
/// the core `require_str`, kept local so this file does not reach into `mcp_tools_core`'s
/// private helpers).
fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                format!("missing or empty string arg `{key}`"),
            )
        })
}

impl ToolDispatcher for SidecarDispatcher {
    fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
        self.dispatch(name, arguments, None)
    }

    /// The MCP-server entry point: threads the CALLER TERMINAL (the `X-Nyx-Terminal-Id`
    /// header nyx-core resolved) into the dispatch so a spec-scoped mutation is auto-mapped
    /// onto that terminal's live session (v2.5.10 Phase 1).
    fn call_with_caller(
        &self,
        name: &str,
        arguments: &Value,
        caller_terminal_id: Option<&str>,
    ) -> Result<Value, RpcError> {
        self.dispatch(name, arguments, caller_terminal_id)
    }
}

impl SidecarDispatcher {
    /// The shared dispatch body (parity with the v1 `PoolBackedDispatcher`). `caller_terminal_id`
    /// is the terminal the MCP connection is bound to (`Some` on the live MCP path via
    /// `call_with_caller`, `None` for a caller-agnostic invocation / a unit test) — passed
    /// through to `dispatch_extension_tool` for the auto-map.
    fn dispatch(
        &self,
        name: &str,
        arguments: &Value,
        caller_terminal_id: Option<&str>,
    ) -> Result<Value, RpcError> {
        // 0) The v2.4 real-time review WATCH tools (Q-015): `repo_watch` / `repo_unwatch`
        //    register/release a `(workspace, repository)` on the git-watch runtime — a
        //    sidecar-owned effect, so handled here (not in the shell-agnostic core) before
        //    the rest of the surface. A change to a watched repo then pushes `repos://changed`
        //    to BOTH the UI and MCP (the same channel the UI `repo_watch` db-command drives).
        if let Some(res) = self.dispatch_repo_watch(name, arguments) {
            return res;
        }

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

        // 2) Workspace registration tools (pure pool writes). Both funnel through the
        //    SHARED `usecase::register_workspace` (PDR-0005, the same use case the UI bridge
        //    drives). The use case persists a `workspace.created` `domain_events` row IN its
        //    transaction (task #9) and then emits the DOMAIN event through the injected sink
        //    AFTER commit — here the real-time [`PushEventSink`] (task #10), which pushes the
        //    v2.0 unsolicited `workspaces://changed` so the renderer re-pulls WITHOUT a reload
        //    or a poll. The push is now driven by the committed DOMAIN event itself (not a
        //    separate `emit_effects` string-match), so it covers this and every future
        //    identity mutation uniformly.
        let push = PushEventSink::new(self.events.clone());
        if let Some(res) = mcp_runtime::dispatch_workspace_tool(&self.db, &push, name, arguments) {
            return res;
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
                caller_terminal_id,
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
                    caller_terminal_id,
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
    /// The shared cell holding the v2.4 real-time git-watch runtime (Q-015), so the MCP
    /// `repo_watch` / `repo_unwatch` tools drive the SAME watcher the UI does.
    git_watch: Arc<Mutex<Option<Arc<GitWatchRuntime>>>>,
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
        git_watch: Arc<Mutex<Option<Arc<GitWatchRuntime>>>>,
    ) -> Self {
        McpRuntime {
            server: Arc::new(McpServer::default()),
            db,
            runner,
            activity,
            pty,
            events,
            git_watch,
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
            git_watch: Arc::clone(&self.git_watch),
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

    /// STOP the MCP HTTP listener (close the loopback socket + join its accept thread).
    ///
    /// Called FIRST in [`Runtime::shutdown`](super::Runtime::shutdown), BEFORE the PTYs are
    /// closed. That ordering is the whole fix: when the sidecar kills the interactive Claude
    /// PTYs, each dying Claude fires a `SessionEnd(reason=other)` hook that `POST`s
    /// `agent_session_event` to this server. In v1 the in-process MCP died WITH the host, so
    /// that POST lost the race and the session stayed `active` (and was resumed next boot). In
    /// v2 the sidecar outlives the PTYs, so the POST would land and `mark_session_ended` —
    /// killing the resume forever. Closing the listener before `close_all` restores the v1 /
    /// crash-reaper behaviour: the late POST hits a refused socket, the session stays `active`,
    /// and lazy-resume reclaims it. Bounded + idempotent (delegates to [`McpServer::stop`]).
    pub fn stop(&self) {
        self.server.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::codec::FrameDecoder;
    use crate::pipe::wire::{decode_message, Message};
    use nyx_core::db;

    /// A `Write` that appends to a shared buffer, so a test reads back exactly the frames the
    /// sink pushed onto the pipe (twin of the `events.rs` test helper).
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Decode every complete `Event` frame the shared buffer captured.
    fn captured_events(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<Event> {
        let bytes = buf.lock().unwrap().clone();
        let mut dec = FrameDecoder::new();
        dec.push(&bytes);
        let mut events = Vec::new();
        while let Some(payload) = dec.next_frame().expect("decode frame") {
            if let Ok(Message::Event(e)) = decode_message(&payload) {
                events.push(e);
            }
        }
        events
    }

    /// A fresh on-disk migrated `Db` under a unique temp dir (the sidecar cannot use
    /// nyx-core's `#[cfg(test)]` `Db::in_memory`).
    fn temp_db() -> Db {
        let dir = std::env::temp_dir().join(format!(
            "nyx-mcp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("nyx.db")).expect("open db")
    }

    /// Build a `SidecarDispatcher` over the given `Db` + a pipe sink writing to `buf`, with NO
    /// managed runner (the throwaway-runner path serves the extension tools). The SAME wiring
    /// [`McpRuntime::start`] installs, minus the live-HTTP server.
    fn dispatcher(db: Arc<Db>, buf: &Arc<Mutex<Vec<u8>>>) -> SidecarDispatcher {
        let events = EventSink::new(Box::new(SharedBuf(buf.clone())));
        let pty = Arc::new(PtyManager::new());
        SidecarDispatcher {
            db,
            runner: Arc::new(Mutex::new(None)),
            terminal_host: SidecarTerminalHost {
                pty: Arc::clone(&pty),
                live_records: pty.live_records(),
                events: events.clone(),
            },
            activity: Arc::new(AgentActivityStore::new()),
            events,
            // No real-time git-watch runtime in the unit harness (the `repo_watch` /
            // `repo_unwatch` tools report it as unavailable) — the extension-tool surface
            // under test here does not need one.
            git_watch: Arc::new(Mutex::new(None)),
        }
    }

    /// done_criterion (T9) — a Spec MUTATION via the MCP tools pushes a `specs://changed`
    /// invalidation on the pipe. The FULL sidecar chain: `dispatch` → `spec_create` returns
    /// `ToolOutcome{ effects: [ChangedTopic::Specs] }` → `emit_effects` → a PUSHED unsolicited
    /// `Event::Changed { topic: "specs" }` (which the main process maps to `specs://changed`).
    #[test]
    fn a_spec_mutation_pushes_a_specs_changed_invalidation() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let db = Arc::new(temp_db());
        let project_id = db
            .with_conn(|c| db::create_project(c, "Proj", "/tmp/mcp-p", None))
            .expect("project")
            .0
            .id;

        let disp = dispatcher(Arc::clone(&db), &buf);
        let out = disp
            .call(
                "spec_create",
                &serde_json::json!({ "project_id": project_id, "title": "Live spec" }),
            )
            .expect("spec_create succeeds");
        assert_eq!(out["spec"]["status"], "draft", "the spec was created");

        let pushed = captured_events(&buf);
        assert!(
            pushed
                .iter()
                .any(|e| matches!(e, Event::Changed { topic } if topic == "specs")),
            "the spec mutation pushed a `specs://changed` invalidation, got: {pushed:?}"
        );
        assert!(
            pushed
                .iter()
                .filter(|e| matches!(e, Event::Changed { .. }))
                .all(|e| matches!(e, Event::Changed { topic } if topic == "specs")),
            "the only `changed` invalidation is on the `specs` topic"
        );
    }
}
