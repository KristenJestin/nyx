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
    /// The MCP server itself, ONLY to read its bound port when opening a shell: a terminal this
    /// host spawns must export the port THIS instance listens on, and that is known solely at
    /// call time (the server binds after the dispatcher is installed).
    server: Arc<McpServer>,
}

impl SidecarTerminalHost {
    fn is_live(&self, terminal_id: &str) -> bool {
        self.live_records.lock().unwrap().contains_key(terminal_id)
    }
}

impl TerminalHost for SidecarTerminalHost {
    fn park_opening_command(&self, terminal_id: &str, command: &str) {
        // PARK the command against the record. The DB record was just created in nyx-core and has
        // no shell yet, so the command cannot run now; it waits in the PTY manager and the spawn
        // `open_terminal_shell` triggers right after types it in. (A terminal record carries no
        // command column: nothing else would ever run it, which is why an MCP
        // `create_terminal { command }` used to open a dead, silent shell.)
        self.pty.park_opening_command(terminal_id, command);
    }

    fn open_terminal_shell(&self, terminal_id: &str, cwd: &str) {
        // `"."` is the record's "no cwd was asked for" marker (what `create_terminal` stores when
        // the argument is absent); the PTY manager spells that same default as an empty cwd.
        let cwd = if cwd == "." { "" } else { cwd };
        // The bound MCP port, read LIVE so the shell exports the port this instance is really
        // listening on — an agent-opened terminal is exactly where a `claude` gets launched, and
        // its SessionStart hook has to reach THIS nyx.
        let mcp_port = self.server.bound_port();
        if let Err(e) = self
            .pty
            .spawn_headless(cwd, terminal_id, mcp_port, self.events.clone())
        {
            // The record stands (the tool has already answered with its id), so say why it has no
            // shell instead of leaving the caller to discover a mute terminal.
            self.events
                .log("warn", format!("terminal {terminal_id} got no shell: {e}"));
        }
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

    fn send_keys(&self, terminal_id: &str, chunks: &[Vec<u8>]) -> Result<bool, String> {
        // SAME liveness gate; the payload is the RAW pre-resolved bytes (NO appended newline)
        // the v1 host wrote verbatim.
        if !self.is_live(terminal_id) {
            return Ok(false);
        }
        // ONE PTY WRITE PER CHUNK, spaced by `KEY_CHUNK_PACING` — the whole point of the fix
        // (see `TerminalHost::send_keys`): a raw-mode TUI classifies each stdin chunk as a
        // whole, so `enter` must arrive as a lone `\r` rather than glued to the text before it.
        // `write_to_record` already does `write_all` + `flush` per call, so the segmentation is
        // exactly the loop; the pause is what stops the child's next `read()` from re-gluing
        // two flushes that landed in the same buffer.
        let mut all_written = true;
        for (i, chunk) in chunks.iter().enumerate() {
            if i > 0 {
                std::thread::sleep(mcp_tools_core::KEY_CHUNK_PACING);
            }
            if !self.pty.write_to_record(terminal_id, chunk) {
                // The PTY went away mid-sequence (it was live at the gate). Stop rather than
                // keep hammering a dead record, and report the failure as the caller's
                // `invalid_state` — the same answer it would have got one call earlier.
                all_written = false;
                break;
            }
        }
        Ok(all_written)
    }

    fn close_terminal_pty(&self, terminal_id: &str) {
        self.pty.close_record(terminal_id);
    }

    fn close_terminal_ptys(&self, terminal_ids: &[String]) {
        // The BULK close, overriding the trait's default loop: `close_records` raises every
        // terminal's `closing` flag BEFORE joining the first pump, so a workspace holding N
        // terminals pays ~one `PUMP_CLOSE_POLL` window instead of N. Same primitive, and the
        // same per-terminal ordering, as the UI bridge's `workspace_delete` route.
        self.pty.close_records(terminal_ids);
    }

    fn terminal_liveness(&self, terminal_id: &str) -> (bool, Option<bool>) {
        // `live` = a PTY is registered for this record. `busy` stays `None` (the OS
        // foreground bit is served from the DB exec-state by list_terminals), exactly as
        // in the v1 host.
        (self.is_live(terminal_id), None)
    }

    fn scrollback_is_behind(&self, terminal_id: &str) -> bool {
        // The sidecar OWNS the PTY, so it sees both halves of the question — the pump's output
        // and the renderer's `persist_scrollback` — and answers exactly, not by comparing clocks.
        self.pty.scrollback_is_behind(terminal_id)
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
            // The Système workspace is closed to Git, watch included: it is anchored on the HOME
            // directory, and a real-time watcher over a user's whole home is exactly the Git I/O
            // the scope promises never to do. Refused HERE, before the watch is registered — the
            // same `system_scope` table the core dispatchers read.
            if let Some(refusal) =
                nyx_core::mcp_context::refuse_system_scope(&self.db, name, args)
            {
                return Err(refusal);
            }
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
    /// THE SYSTÈME APPROVAL PUSH IS NO LONGER HERE, and its move is the protocol change (PDR-0030).
    ///
    /// It used to fire when a protected call RETURNED `approval_required` — which worked only
    /// because the call returned immediately. A protected call now BLOCKS on the human for up to
    /// forty-five seconds, so a push emitted on the way out would reach the decision surface long
    /// after the decision was needed. It is emitted by the ledger itself, at the instant the row
    /// appears (`system_approvals::set_pending_notifier`, installed in `Runtime::attach_events`),
    /// onto the same `system-approvals` topic the UI's grant/deny routes use.
    fn dispatch(
        &self,
        name: &str,
        arguments: &Value,
        caller_terminal_id: Option<&str>,
    ) -> Result<Value, RpcError> {
        self.dispatch_inner(name, arguments, caller_terminal_id)
    }

    /// The routing itself.
    fn dispatch_inner(
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
            return mcp_runtime::dispatch_command_tool(&self.db, &runner, name, arguments, caller_terminal_id)
                .expect("runtime command tool dispatch");
        }

        // 2) Workspace registration tools (pure pool writes). Both funnel through the SHARED
        //    `usecase::register_workspace_anchored` (PDR-0005), which they call DIRECTLY since
        //    v2.5 so they can carry their optional `spec_id`. That is a DISTINCT entry point
        //    from the UI bridge's `create_workspace` db-command, which reaches the same use
        //    case through the UNANCHORED `usecase::register_workspace` (`spec_id = None`); the
        //    two paths meet only INSIDE the anchored use case, never before it.
        //    The use case persists a `workspace.created` `domain_events` row IN its
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
        self.server.set_dispatcher(Arc::new(self.dispatcher()));
        match self.server.start() {
            Ok(port) => port,
            Err(e) => {
                self.events
                    .log("warn", format!("MCP server did not start: {e}"));
                0
            }
        }
    }

    /// The dispatcher over the shared services — everything in it is an `Arc`/handle, so building
    /// one is a handful of refcount bumps. Factored out because there are now TWO callers: the MCP
    /// server (an agent's `tools/call`) and the approval executor below (the HUMAN's grant), and
    /// they must resolve a tool through the SAME table or the operation the user approved and the
    /// operation nyx performs could drift apart.
    fn dispatcher(&self) -> SidecarDispatcher {
        SidecarDispatcher {
            db: Arc::clone(&self.db),
            runner: Arc::clone(&self.runner),
            terminal_host: SidecarTerminalHost {
                pty: Arc::clone(&self.pty),
                live_records: self.pty.live_records(),
                events: self.events.clone(),
                server: Arc::clone(&self.server),
            },
            activity: Arc::clone(&self.activity),
            events: self.events.clone(),
            git_watch: Arc::clone(&self.git_watch),
        }
    }

    /// RUN an operation a human has just approved (PDR-0030): dispatch `tool` with the frozen
    /// canonical `args` the ledger held, with the approval matrix suspended for this call.
    ///
    /// This is the whole reason the protocol could drop its token. The agent used to have to come
    /// back and replay, carrying a secret through a channel no MCP client reliably renders; now the
    /// decision itself is what performs the operation, so nothing secret ever has to travel.
    ///
    /// It goes through [`SidecarDispatcher::dispatch`], NOT through a private table of its own —
    /// same routing, same effects, same `changed` pushes as if the agent had been allowed to call
    /// it. `with_granted_authority` is what stops the matrix from classifying this very call as
    /// protected and asking the user a second time, forever.
    ///
    /// `caller_terminal_id` is `None`: the operation is performed by nyx on the user's behalf, not
    /// by a terminal-bound agent, and pretending otherwise would put a false actor on every record
    /// the tool writes.
    pub fn execute_granted(&self, tool: &str, args: &Value) -> Result<Value, String> {
        let dispatcher = self.dispatcher();
        nyx_core::system_approvals::with_granted_authority(|| {
            dispatcher
                .dispatch(tool, args, None)
                // The wire error's one-line rendering, which is what the ledger's outcome column
                // and the waiting agent both read.
                .map_err(|e| e.to_string())
        })
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
        dispatcher_with_pty(db, buf).0
    }

    /// Like [`dispatcher`] but ALSO hands back the `PtyManager` it is wired to, so a test can
    /// drive the spawn side (what the renderer does when it mounts a terminal) against the SAME
    /// manager the terminal tools reached.
    fn dispatcher_with_pty(
        db: Arc<Db>,
        buf: &Arc<Mutex<Vec<u8>>>,
    ) -> (SidecarDispatcher, Arc<PtyManager>) {
        let events = EventSink::new(Box::new(SharedBuf(buf.clone())));
        let pty = Arc::new(PtyManager::new(Arc::clone(&db)));
        let disp = SidecarDispatcher {
            db,
            runner: Arc::new(Mutex::new(None)),
            terminal_host: SidecarTerminalHost {
                pty: Arc::clone(&pty),
                live_records: pty.live_records(),
                events: events.clone(),
                // Never started, so `bound_port()` is 0 — a spawned shell exports no
                // `NYX_MCP_PORT`, exactly as it does when a terminal races the boot `McpStart`.
                server: Arc::new(McpServer::default()),
            },
            activity: Arc::new(AgentActivityStore::new()),
            events,
            // No real-time git-watch runtime in the unit harness (the `repo_watch` /
            // `repo_unwatch` tools report it as unavailable) — the extension-tool surface
            // under test here does not need one.
            git_watch: Arc::new(Mutex::new(None)),
        };
        (disp, pty)
    }

    /// DONE-CRITERIA (« MCP : découverte dédiée ») — through the REAL sidecar dispatcher, over a
    /// database bootstrapped by the real `Db::open`: an agent discovers the Système commands, acts
    /// on one straight from the response, and still cannot see a Système terminal.
    ///
    /// The nyx-core tests drive the same tools through `dispatch_extension_tool`; what this adds is
    /// the SIDECAR's own routing — `list_system_commands` reaching that dispatcher at all, and the
    /// terminal tools refusing on the same wiring the live server installs.
    #[test]
    fn an_agent_discovers_the_system_commands_and_never_the_system_terminals() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let db = Arc::new(temp_db());
        let scope = db
            .with_conn(nyx_core::system_scope::load)
            .expect("read the scope")
            .expect("Db::open bootstraps the Système scope");
        db.with_conn(|c| {
            db::create_template(
                c,
                &scope.project.id,
                "services",
                "echo hi",
                None,
                db::CommandSource::default(),
            )
        })
        .expect("a Système service");

        let (disp, pty) = dispatcher_with_pty(Arc::clone(&db), &buf);

        // DISCOVERY — the scope, and a launchable id.
        let out = disp
            .call("list_system_commands", &serde_json::json!({}))
            .expect("the sidecar dispatcher serves list_system_commands");
        assert_eq!(out["scope"]["project_id"], serde_json::json!(scope.project.id));
        assert_eq!(out["scope"]["home"], serde_json::json!(scope.home()));
        let command = &out["commands"].as_array().expect("commands")[0];
        assert_eq!(command["nature"], "system");
        assert_eq!(command["cwd"], serde_json::json!(scope.home()));
        let instance = command["instance_id"].as_str().expect("a launchable id");

        // …and it is immediately usable on the ORDINARY runtime surface: the id goes straight to
        // `get_command_output`, which the Système BOUNDARY lets through (it is allow-listed) and
        // which then fails only on this harness having no managed runner. The distinction is the
        // whole assertion: `mcp_unavailable` means the call was ROUTED, `system_scope_forbidden`
        // would mean the scope refused its own read. (The end-to-end read-and-start is pinned in
        // nyx-core by
        // `mcp_tools_core::tests::list_system_commands_discovers_the_scope_and_opens_nothing_else`,
        // which has a real runner.)
        let routed = disp
            .call(
                "get_command_output",
                &serde_json::json!({ "instance_id": instance }),
            )
            .expect_err("no managed runner in this harness");
        assert_eq!(
            routed.code.as_str(),
            "mcp_unavailable",
            "the Système scope's own read must be ROUTED, never refused: {routed:?}"
        );

        // THE TERMINALS ARE NOT PART OF IT. One exists (created through the human's own use case,
        // with no shell here), and the agent can neither list it nor read it.
        let sys_terminal = nyx_core::system_scope::create_system_terminal(
            &db,
            Some("shell".into()),
            &NoOpShell,
        )
        .expect("the human's Système terminal")
        .terminal
        .id;
        let listed = disp
            .call("list_terminals", &serde_json::json!({}))
            .expect("list_terminals");
        assert!(
            !listed["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["terminal_id"] == serde_json::json!(sys_terminal)),
            "a Système terminal is never offered to an agent: {listed}"
        );
        let err = disp
            .call(
                "read_terminal",
                &serde_json::json!({ "terminal_id": sys_terminal }),
            )
            .expect_err("nor readable, even with the id in hand");
        assert_eq!(
            err.code.as_str(),
            nyx_core::system_scope::SYSTEM_SCOPE_FORBIDDEN
        );
        drop(pty);
    }

    /// A shell opener that opens nothing — this harness has no pane and the terminal's PTY is not
    /// the subject of the test above.
    struct NoOpShell;
    impl nyx_core::system_scope::SystemTerminalShell for NoOpShell {
        fn open_at_home(&self, _terminal_id: &str, _home: &str) -> Result<(), String> {
            Ok(())
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

    /// done_criterion (review F3, 01KXZP1QDXCAQ54H1Z1QDGNY2C): a review-FINDING MUTATION via the MCP
    /// tools pushes BOTH `repos://changed` (the review viewer feed) AND `specs://changed` (the
    /// terminal-spec OVERLAY / Spec detail, where a finding shows as a `tasks kind=review` row that
    /// `useSpecDetail` re-pulls on `specs`, NOT `repos`). Before the fix `review_submit_feedback`
    /// named only `Repos`, so the overlay reflected a chat retour ONLY after a manual reload. Drives
    /// the FULL sidecar chain: `dispatch` → `review_submit_feedback` returns
    /// `ToolOutcome{ effects: [Repos, Specs] }` → `emit_effects` → two pushed invalidations.
    #[test]
    fn a_review_finding_mutation_pushes_repos_and_specs_changed_for_the_overlay() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let db = Arc::new(temp_db());
        // A project (→ its root workspace) + a Spec to hang the finding on.
        let (project, ws) = db
            .with_conn(|c| db::create_project(c, "Proj", "/tmp/mcp-review", None))
            .expect("project");
        let spec_id = db
            .with_conn(|c| {
                db::create_spec(c, &project.id, "Spec", Default::default()).map(|(s, _)| s.id)
            })
            .expect("spec");

        let disp = dispatcher(Arc::clone(&db), &buf);
        let out = disp
            .call(
                "review_submit_feedback",
                &serde_json::json!({
                    "workspace_id": ws.id,
                    "repository_id": "repo",
                    "spec_id": spec_id,
                    "body": "the empty state looks broken",
                }),
            )
            .expect("review_submit_feedback succeeds");
        assert_eq!(
            out["finding"]["grill_state"], "proposed",
            "the chat retour was grilled into a proposed finding"
        );

        let pushed = captured_events(&buf);
        let changed: Vec<&str> = pushed
            .iter()
            .filter_map(|e| match e {
                Event::Changed { topic } => Some(topic.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            changed.contains(&"repos"),
            "the review mutation pushed `repos://changed` for the viewer, got: {changed:?}"
        );
        assert!(
            changed.contains(&"specs"),
            "the review mutation pushed `specs://changed` for the OVERLAY (finding = tasks kind=review), got: {changed:?}"
        );
    }

    /// FINDING `01M0BN0R0Q0VXK9JM7TGXX9D7W` — A WORKSPACE OPENED BY THE GRAPH REACHES THE SIDEBAR,
    /// through the SHELL, without a relaunch. The sibling of the two tests above, on the road that
    /// had none.
    ///
    /// `create_workspace` the TOOL is reached at step 2 of `dispatch`, which injects a
    /// `PushEventSink`: the use case emits `workspace.created` after commit and the sink pushes
    /// `workspaces://changed`. `create_workspace` the NODE runs INSIDE a `nyx_next` tick, where no
    /// shell sink reaches — the call passes a `NullEventSink` — so its only road out is step 3's
    /// `emit_effects`, over the tick's own `ToolOutcome::effects`. Those effects named `Specs` and
    /// nothing else, and the sidebar re-pulls on `workspaces://changed` ALONE: on 2026-08-18 a `go`
    /// answered in `/nspec` created DEPOT-6, wrote the row, made the folder, anchored the Spec — and
    /// the sidebar stayed empty until the app was restarted, which is indistinguishable from a flow
    /// that failed.
    ///
    /// This drives the FULL sidecar chain the human's screen depends on: `dispatch("nyx_next")` →
    /// the tick runs the node's effect inline → `ToolOutcome::effects` → `emit_effects` → a PUSHED
    /// `Event::Changed { topic: "workspaces" }` on the pipe (which `main/sidecar.ts` maps to
    /// `workspaces://changed`, the topic `use-projects` re-lists on).
    #[test]
    fn a_workspace_opened_by_a_graph_tick_pushes_the_sidebar_invalidation() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let db = Arc::new(temp_db());
        // The folder the node opens the workspace UNDER — a fresh empty dir, never the project's.
        let roots = std::env::temp_dir().join(format!(
            "nyx-graph-ws-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&roots).unwrap();

        // A project, a Spec driven to `ready` (the anchor's precondition), and a one-node graph whose
        // only node is the `create_workspace` the `go` branch reaches.
        let (project, _root_ws) = db
            .with_conn(|c| db::create_project(c, "Proj", "/tmp/mcp-graph-ws", None))
            .expect("project");
        let spec_id = db
            .with_conn(|c| {
                db::create_spec(
                    c,
                    &project.id,
                    "Spec",
                    db::SpecSections {
                        problem: "the workspace never shows up".into(),
                        solution: "say the pool changed".into(),
                        ..Default::default()
                    },
                )
            })
            .expect("spec")
            .0
            .id;
        db.with_conn(|c| db::transition_spec(c, &spec_id, db::SpecTransition::Ready, true))
            .expect("the spec is ready — the anchor's precondition");
        let run_id = db
            .with_conn(|c| {
                let wf = db::create_workflow(c, None, "graph-ws", db::WORKFLOW_SCOPE_CUSTOM, None)?;
                db::create_node(
                    c,
                    &wf.id,
                    None,
                    0,
                    db::NODE_TYPE_CREATE_WORKSPACE,
                    "open the dev workspace",
                    "{}",
                )?;
                let board = serde_json::json!({ "worktrees_root": roots.to_string_lossy() });
                db::create_workflow_run(c, &wf.id, Some(&spec_id), None, &board.to_string())
            })
            .expect("run")
            .id;

        let disp = dispatcher(Arc::clone(&db), &buf);
        let out = disp
            .call("nyx_next", &serde_json::json!({ "run_id": run_id }))
            .expect("the tick runs the create_workspace node");
        assert_eq!(
            out["status"], "done",
            "the one-node graph ran to completion"
        );

        // The effect really landed — the topic below would otherwise be a claim about nothing.
        let ws = db
            .with_conn(|c| db::list_workspaces(c, &project.id))
            .expect("workspaces")
            .into_iter()
            .find(|w| w.root_path != nyx_core::pathnorm::normalize("/tmp/mcp-graph-ws"))
            .expect("the node opened the Spec's dev workspace");
        assert_eq!(
            db.with_conn(|c| db::get_spec(c, &spec_id))
                .unwrap()
                .unwrap()
                .workspace_id
                .as_deref(),
            Some(ws.id.as_str()),
            "and anchored it on the Spec"
        );

        let changed: Vec<String> = captured_events(&buf)
            .into_iter()
            .filter_map(|e| match e {
                Event::Changed { topic } => Some(topic),
                _ => None,
            })
            .collect();
        assert!(
            changed.iter().any(|t| t == "workspaces"),
            "the graph tick pushed `workspaces://changed` — the ONLY topic the sidebar re-pulls on, \
             and the difference between a workspace that appears and one the user reads as a \
             failure. Got: {changed:?}"
        );
        assert!(
            changed.iter().any(|t| t == "specs"),
            "and it still pushes `specs://changed` for the run cursor / the anchored Spec: \
             {changed:?}"
        );
        std::fs::remove_dir_all(&roots).ok();
    }

    /// H1 (with F8) — a terminal created over MCP gets a REAL shell and runs its opening command,
    /// with NOTHING mounted anywhere.
    ///
    /// The renderer mounts panes only for the project it is displaying, and that mount used to be
    /// the only thing that spawned a PTY. So a terminal an agent opened in any other project — or
    /// a loose one — never got a shell: the tool answered `has_command: true`, the record sat in
    /// the sidebar mute, `send_to_terminal` answered `invalid_state` forever, and the parked
    /// command waited for a mount that would only come if the human switched projects. An agent
    /// driving nyx has to open sessions where the work is, not where the eye is.
    ///
    /// Drives the real chain with NO spawn from the test: `create_terminal { command }` through the
    /// dispatcher → the `TerminalHost` park → the shell the tool opens itself → the command
    /// observed EXECUTED in that shell's own VT screen, and `send_to_terminal` accepted.
    ///
    /// The marker is ASSEMBLED by the shell (`printf 'H1_%s' RAN`), so seeing `H1_RAN` on the
    /// screen can only mean the line was EXECUTED — the terminal's echo of the typed line spells
    /// `printf 'H1_%s' RAN`, never the marker itself.
    #[cfg(unix)]
    #[test]
    fn a_terminal_created_over_mcp_gets_its_shell_with_nothing_on_screen() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let db = Arc::new(temp_db());
        let (disp, pty) = dispatcher_with_pty(Arc::clone(&db), &buf);

        let created = disp
            .call(
                "create_terminal",
                &serde_json::json!({ "command": "printf 'H1_%s' RAN", "label": "h1" }),
            )
            .expect("create_terminal succeeds");
        assert_eq!(
            created["has_command"],
            serde_json::json!(true),
            "the tool promises the command will run"
        );
        let record_id = created["terminal_id"].as_str().expect("a record id");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut ran = false;
        while std::time::Instant::now() < deadline && !ran {
            if let Some(s) = pty.snapshot_by_record(record_id) {
                ran = (0..s.main.rows).any(|r| s.main.row_text(r).contains("H1_RAN"));
            }
            if !ran {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
        }
        let screen = pty
            .snapshot_by_record(record_id)
            .map(|s| {
                (0..s.main.rows)
                    .map(|r| s.main.row_text(r))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // The shell is DRIVABLE, not just alive: the tool that used to answer `invalid_state`
        // forever now writes into it.
        let sent = disp.call(
            "send_to_terminal",
            &serde_json::json!({ "terminal_id": record_id, "command": "true" }),
        );
        pty.close_record(record_id);
        assert!(
            ran,
            "the opening command must RUN in the shell the tool opened; screen was: {screen:?}"
        );
        assert!(
            sent.is_ok(),
            "an agent must be able to type into the terminal it just opened, got: {sent:?}"
        );
    }

    /// F1 — the loop an agent actually needs, with NOTHING mounted anywhere: `create_terminal` →
    /// `send_to_terminal` → `read_terminal` gives back what the shell printed.
    ///
    /// `terminals.scrollback` used to be written ONLY by the renderer's `persist_scrollback`, and
    /// only while a pane was mounted. A terminal an agent opened off-screen therefore answered an
    /// empty, `stale: true` buffer for as long as no human switched to that project: the agent
    /// could drive a shell it could not read. What `read_terminal` means must not depend on whose
    /// screen is showing what — that is the ambiguity the `live`/`stale` fields exist to remove,
    /// not a second one to add. So while a terminal has NO view, the SIDECAR persists it.
    ///
    /// The test also pins the HAND-OVER in both directions, ownership being the whole design: once
    /// a pane attaches the renderer owns the buffer again (the sidecar writes no more — there is no
    /// renderer here, so the record simply stops moving), and what was accumulated before the mount
    /// is still there.
    ///
    /// The markers are ASSEMBLED by the shell (`printf 'F1_%s' READBACK`), so reading `F1_READBACK`
    /// can only mean the line was EXECUTED — the terminal's echo of the typed line spells
    /// `printf 'F1_%s' READBACK`, never the marker itself.
    #[cfg(unix)]
    #[test]
    fn an_agent_reads_back_the_terminal_it_drove_with_no_pane_anywhere() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let db = Arc::new(temp_db());
        let (disp, pty) = dispatcher_with_pty(Arc::clone(&db), &buf);

        // 1) OPEN — no pane exists, and none mounts until step 4.
        let record_id = disp
            .call("create_terminal", &serde_json::json!({ "label": "f1" }))
            .expect("create_terminal succeeds")["terminal_id"]
            .as_str()
            .expect("a record id")
            .to_string();

        // 2) DRIVE.
        disp.call(
            "send_to_terminal",
            &serde_json::json!({
                "terminal_id": record_id,
                "command": "printf 'F1_%s' READBACK",
            }),
        )
        .expect("an agent can type into the terminal it opened");

        // 3) READ BACK — the point of the whole loop.
        let read = |disp: &SidecarDispatcher| {
            disp.call(
                "read_terminal",
                &serde_json::json!({ "terminal_id": record_id }),
            )
            .expect("read_terminal succeeds")
        };
        let has_marker = |out: &serde_json::Value, marker: &str| {
            out["output"].as_str().unwrap_or_default().contains(marker)
        };
        // Poll for the SETTLED answer: the marker saved AND the buffer level with the shell. The
        // save is debounced (as the mounted renderer's is), so a read taken mid-burst legitimately
        // says `stale` — what must never happen is that it stays that way because nobody is
        // looking.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut out = read(&disp);
        while std::time::Instant::now() < deadline
            && !(has_marker(&out, "F1_READBACK") && out["stale"] == serde_json::json!(false))
        {
            std::thread::sleep(std::time::Duration::from_millis(30));
            out = read(&disp);
        }
        assert!(
            has_marker(&out, "F1_READBACK"),
            "an agent must read back what the terminal it drove printed, with no pane anywhere; \
             got: {out:?}"
        );
        assert_eq!(
            out["live"],
            serde_json::json!(true),
            "the shell is running behind the record"
        );
        assert_eq!(
            out["stale"], serde_json::json!(false),
            "the buffer IS the terminal's present state — a read must not answer differently for \
             want of someone looking at it"
        );

        // 4) A PANE ATTACHES: the renderer owns the buffer again, and the pre-mount history is
        //    still in the record. Nothing else writes it here, so anything the shell prints from
        //    now on must NOT appear — proving the sidecar really stopped (no double-write over the
        //    renderer's saves) rather than the two of them racing over the same column.
        let events = EventSink::new(Box::new(SharedBuf(Arc::new(Mutex::new(Vec::new())))));
        pty.spawn(80, 24, "", &record_id, 0, events)
            .expect("the mount takes over the shell");
        disp.call(
            "send_to_terminal",
            &serde_json::json!({
                "terminal_id": record_id,
                "command": "printf 'F1_%s' AFTERMOUNT",
            }),
        )
        .expect("send after the mount");
        // Wait until the shell HAS printed it (on its VT screen), so the assertion below is about
        // WHO saves, not about whether anything happened yet.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut printed = false;
        while std::time::Instant::now() < deadline && !printed {
            printed = pty
                .snapshot_by_record(&record_id)
                .map(|s| (0..s.main.rows).any(|r| s.main.row_text(r).contains("F1_AFTERMOUNT")))
                .unwrap_or(false);
            if !printed {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
        }
        let out = read(&disp);
        pty.close_record(&record_id);
        assert!(printed, "the shell must have printed the post-mount marker");
        assert!(
            has_marker(&out, "F1_READBACK"),
            "the hand-over must not lose the history accumulated before the pane existed; got: \
             {out:?}"
        );
        assert!(
            !has_marker(&out, "F1_AFTERMOUNT"),
            "once a pane is attached the RENDERER owns the scrollback; the sidecar must not keep \
             writing over its saves; got: {out:?}"
        );
    }

    /// F2 — `read_terminal` hands back the scrollback the FRONT last saved, and the front only
    /// saves it while the terminal is mounted on screen. So a terminal that runs off-view returns
    /// a frozen buffer; the text alone gives a caller no way to tell that apart from the present
    /// state. The answer must SAY it, and say it exactly.
    ///
    /// Drives the real chain against a real shell: nothing saved yet while the shell prints →
    /// `stale`; a save (what the mounted front does) → not `stale`; the shell prints again →
    /// `stale` again; no shell at all → `live:false` and `stale`, since the buffer is then only
    /// that terminal's final output.
    #[cfg(unix)]
    #[test]
    fn read_terminal_says_when_its_scrollback_is_not_the_terminals_present_state() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let db = Arc::new(temp_db());
        let (disp, pty) = dispatcher_with_pty(Arc::clone(&db), &buf);

        let record_id = disp
            .call("create_terminal", &serde_json::json!({ "label": "f2" }))
            .expect("create_terminal succeeds")["terminal_id"]
            .as_str()
            .expect("a record id")
            .to_string();
        let events = EventSink::new(Box::new(SharedBuf(Arc::new(Mutex::new(Vec::new())))));
        let pty_id = pty
            .spawn(80, 24, "", &record_id, 0, events)
            .expect("spawn the record's pty");

        let read = |disp: &SidecarDispatcher| {
            disp.call(
                "read_terminal",
                &serde_json::json!({ "terminal_id": record_id }),
            )
            .expect("read_terminal succeeds")
        };
        // Wait for the shell to have PRINTED something nobody saved — the state a terminal is in
        // as soon as it runs while its pane is not mounted.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline && !pty.scrollback_is_behind(&record_id) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let out = read(&disp);
        assert_eq!(
            out["live"],
            serde_json::json!(true),
            "the terminal's shell is running"
        );
        assert_eq!(
            out["stale"], serde_json::json!(true),
            "the shell printed output nyx has not saved — the returned buffer is NOT the terminal's present state"
        );

        // A save — exactly what the mounted front's debounced `persist_scrollback` does.
        pty.mark_scrollback_persisted(&record_id);
        let out = read(&disp);
        assert_eq!(
            out["stale"],
            serde_json::json!(false),
            "right after a save the buffer IS the terminal's present state"
        );

        // The shell prints again with nobody saving: behind once more.
        pty.write(pty_id, b"printf 'F2_%s' MORE\n").expect("write");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline && !pty.scrollback_is_behind(&record_id) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            read(&disp)["stale"],
            serde_json::json!(true),
            "new output with no save puts the buffer behind the shell again"
        );

        // No shell at all (closed, or its shell exited): the buffer is that terminal's
        // FINAL output, never its present state.
        pty.close(pty_id);
        let out = read(&disp);
        assert_eq!(
            out["live"],
            serde_json::json!(false),
            "no shell is running behind the record any more"
        );
        assert_eq!(
            out["stale"],
            serde_json::json!(true),
            "a terminal with no shell can only return a PAST buffer"
        );
    }
}
