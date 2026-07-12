//! The versioned message catalogue carried over the pipe (PDR-0007).
//!
//! Three categories travel on the wire:
//!   * **Request**  (supervisor -> sidecar) — correlated by `id`.
//!   * **Response** (sidecar -> supervisor) — echoes the request `id`.
//!   * **Event**    (sidecar -> supervisor, UNSOLICITED) — no correlation id;
//!     e.g. terminal output, logs.
//!
//! ## Wire shape — positional arrays
//!
//! Every message encodes as a MessagePack **array** (never a struct-as-map), so
//! the bytes are identical across rmp-serde (Rust) and @msgpack/msgpack (TS) with
//! no key-ordering ambiguity. The fixed envelope is:
//!
//! ```text
//! [ protocol_version: u32, category: u8, kind: u8, body: <array> ]
//! ```
//!
//! * `protocol_version` is stamped on EVERY message so version incompatibility is
//!   detectable at the very first frame (the boot handshake), not just inferred.
//! * `category`: 0 = Request, 1 = Response, 2 = Event.
//! * `kind`: the message kind within its category (see the `*_KIND_*` consts).
//! * `body`: a kind-specific positional array.
//!
//! The TS mirror in `apps/desktop/src/shared/pipe-protocol.ts` encodes the exact
//! same shape; `contract/fixtures.json` pins the canonical bytes both sides must
//! reproduce.

use serde::{Deserialize, Serialize};

use super::codec::{encode_frame, FrameError};

/// Protocol version stamped on every message. Bump on any breaking wire change.
pub const PROTOCOL_VERSION: u32 = 1;

/// Capabilities advertised at handshake (mirrors the TS `CAPABILITIES`). The `pty`
/// capability (HHC) advertises the interactive-terminal surface; `commands` + `mcp`
/// (HHD) advertise the managed-command runtime + the in-sidecar MCP HTTP server.
pub const CAPABILITIES: &[&str] = &[
    "handshake",
    "ping",
    "shutdown",
    "events",
    "pty",
    "commands",
    "mcp",
];

// Envelope categories.
pub const CAT_REQUEST: u8 = 0;
pub const CAT_RESPONSE: u8 = 1;
pub const CAT_EVENT: u8 = 2;

// Request kinds.
pub const REQ_HANDSHAKE: u8 = 0;
pub const REQ_PING: u8 = 1;
pub const REQ_SHUTDOWN: u8 = 2;
// PTY request kinds (HHC) — the interactive-terminal surface over the pipe.
pub const REQ_PTY_SPAWN: u8 = 3;
pub const REQ_PTY_WRITE: u8 = 4;
pub const REQ_PTY_RESIZE: u8 = 5;
pub const REQ_PTY_CLOSE: u8 = 6;
pub const REQ_PTY_ACK: u8 = 7;
// Managed-command + MCP request kinds (HHD).
pub const REQ_COMMAND_START: u8 = 8;
pub const REQ_COMMAND_STOP: u8 = 9;
pub const REQ_COMMAND_RELAUNCH: u8 = 10;
pub const REQ_MCP_START: u8 = 11;
// The GENERIC, allowlisted non-PTY backend-command request (G0). The supervisor
// forwards every contract `BackendCommand` that has no dedicated typed kind above
// (the whole DB-backed + integrations + live-PTY-introspection long tail) as this one
// request: a command NAME + a JSON args STRING. The sidecar routes it to the right
// authority and replies [`RES_DB_RESULT`] with a JSON result string. This is the
// sidecar replacement for the v1 napi `db_command(command, argsJson)` the TS core-host
// owned (PDR-0007: N-API + core-host are transitional, removed once the sidecar has
// parity — Phase 3 reaches it here).
pub const REQ_DB_COMMAND: u8 = 12;

// Response kinds.
pub const RES_HANDSHAKE_OK: u8 = 0;
pub const RES_PONG: u8 = 1;
pub const RES_SHUTDOWN_ACK: u8 = 2;
pub const RES_ERROR: u8 = 3;
// PTY response kinds (HHC).
pub const RES_PTY_SPAWNED: u8 = 4;
pub const RES_OK: u8 = 5;
// Managed-command + MCP response kinds (HHD).
pub const RES_COMMAND_STATUS: u8 = 6;
pub const RES_MCP_STARTED: u8 = 7;
// The reply to a [`REQ_DB_COMMAND`] (G0): the command's result already serialized to a
// JSON string (`"null"` for the void commands), which the supervisor `JSON.parse`s back
// to the contract shape. A command FAILURE is a [`RES_ERROR`] (readable message), never a
// bogus result.
pub const RES_DB_RESULT: u8 = 8;

// Event kinds.
pub const EVT_TERMINAL_OUTPUT: u8 = 0;
pub const EVT_LOG: u8 = 1;
// PTY lifecycle/observation events (HHC).
pub const EVT_PTY_EXIT: u8 = 2;
pub const EVT_PTY_CWD: u8 = 3;
pub const EVT_PTY_EXEC_STATE: u8 = 4;
// Managed-command lifecycle events (HHD).
pub const EVT_COMMAND_STATE: u8 = 5;
pub const EVT_COMMAND_OUTPUT: u8 = 6;
pub const EVT_CHANGED: u8 = 7;
// OS busy-state transition for a terminal (v2.5.5 review T1). Keyed by the persistent
// RECORD id (NOT the live pty id) so the renderer folds it directly onto the record.
pub const EVT_PTY_BUSY_STATE: u8 = 8;

/// Stable, machine-matchable error codes (mirrored in TS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    VersionIncompatible,
    MalformedRequest,
    FrameTooLarge,
    QuotaExceeded,
}

impl ErrorCode {
    /// Stable string code used on the wire (the TS side matches on these).
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::VersionIncompatible => "version_incompatible",
            ErrorCode::MalformedRequest => "malformed_request",
            ErrorCode::FrameTooLarge => "frame_too_large",
            ErrorCode::QuotaExceeded => "quota_exceeded",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "version_incompatible" => Some(ErrorCode::VersionIncompatible),
            "malformed_request" => Some(ErrorCode::MalformedRequest),
            "frame_too_large" => Some(ErrorCode::FrameTooLarge),
            "quota_exceeded" => Some(ErrorCode::QuotaExceeded),
            _ => None,
        }
    }
}

/// A request (supervisor -> sidecar), correlated by `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Handshake {
        id: u64,
        protocol_version: u32,
    },
    Ping {
        id: u64,
        nonce: u64,
    },
    Shutdown {
        id: u64,
    },
    /// Spawn an interactive shell PTY. The sidecar allocates the live `terminal_id`
    /// (the `Pty::id()`), starts streaming binary [`Event::TerminalOutput`] for it,
    /// and replies [`Response::PtySpawned`]. `cwd`/`record_id` are optional.
    PtySpawn {
        id: u64,
        cols: u16,
        rows: u16,
        /// Working dir for the shell; empty string -> the sidecar's cwd.
        cwd: String,
        /// The persistent terminal RECORD id (`terminals.id`) exported as
        /// `NYX_TERMINAL_ID` for agent-session correlation; empty -> none.
        record_id: String,
    },
    /// Write raw bytes (keystrokes / paste) to a live PTY. `data` is BINARY (no
    /// base64) — the bytes land verbatim on the child's stdin.
    PtyWrite {
        id: u64,
        terminal_id: u32,
        data: Vec<u8>,
    },
    /// Resize a live PTY (delivers SIGWINCH to the child).
    PtyResize {
        id: u64,
        terminal_id: u32,
        cols: u16,
        rows: u16,
    },
    /// Close (kill the process group of) a live PTY.
    PtyClose {
        id: u64,
        terminal_id: u32,
    },
    /// FLOW-CONTROL ACK: the renderer consumed `bytes` of this terminal's output.
    /// The sidecar frees that much of the per-terminal quota and resumes the paused
    /// reader once the backlog drops below the resume threshold — the credit loop
    /// that bounds memory and keeps the binary stream lossless (PDR-0007 / annexe §E).
    PtyAck {
        id: u64,
        terminal_id: u32,
        bytes: u64,
    },
    /// Start a managed-command instance (idempotent on an already-running one). The
    /// sidecar resolves the command line + cwd from the DB and spawns the off-screen
    /// command PTY. Replies [`Response::CommandStatus`].
    CommandStart {
        id: u64,
        instance_id: String,
    },
    /// Stop a running managed-command instance (tree-kill of its process group).
    CommandStop {
        id: u64,
        instance_id: String,
    },
    /// Relaunch (the explicit restart: stop-then-start if running, else start).
    CommandRelaunch {
        id: u64,
        instance_id: String,
    },
    /// Start the in-sidecar MCP HTTP server (idempotent — start-once). Replies
    /// [`Response::McpStarted`] with the bound port.
    McpStart {
        id: u64,
    },
    /// The GENERIC non-PTY backend command (G0): a contract `BackendCommand` name + its
    /// args serialized to JSON (`"{}"` when none). The sidecar routes it to its authority
    /// (the DB CRUD, the integrations core, the live-PTY introspection, the activity
    /// store, the managed-command runner's output/acknowledge) and replies
    /// [`Response::DbResult`] with the JSON result string. The supervisor speaks the
    /// SAME `invoke(command, args)` it already spoke to the v1 core-host, so no call-site
    /// changes — only the transport underneath.
    DbCommand {
        id: u64,
        command: String,
        args_json: String,
    },
}

/// A response (sidecar -> supervisor), echoing the request `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    HandshakeOk {
        id: u64,
        protocol_version: u32,
        capabilities: Vec<String>,
        max_frame_len: u32,
    },
    Pong {
        id: u64,
        nonce: u64,
    },
    ShutdownAck {
        id: u64,
    },
    Error {
        id: u64,
        code: ErrorCode,
        message: String,
    },
    /// A PTY was spawned; `terminal_id` is the live id the renderer keys its xterm by
    /// and uses for subsequent write/resize/close/ack on this terminal.
    PtySpawned {
        id: u64,
        terminal_id: u32,
    },
    /// Generic acknowledgement for a void PTY op (write/resize/close/ack) that
    /// succeeded with no payload — echoes the request `id`.
    Ok {
        id: u64,
    },
    /// The status of a managed-command instance after a lifecycle call (the wire shape
    /// of the v1 `CommandStatus`). `running` is the live flag the dot reads;
    /// `state`/`exit_code` the factual outcome; `was_running`/`restarted` describe what
    /// the call did.
    CommandStatus {
        id: u64,
        instance_id: String,
        /// `idle|running|success|error`.
        state: String,
        running: bool,
        exit_code: Option<i32>,
        unread: bool,
        was_running: bool,
        restarted: bool,
    },
    /// The MCP HTTP server started (or was already started); `port` is the bound
    /// loopback port (0 if the bind failed — a non-fatal warning, never a hard error).
    McpStarted {
        id: u64,
        port: u16,
    },
    /// The result of a [`Request::DbCommand`] (G0): the command's value ALREADY serialized
    /// to a JSON string (`"null"` for the void commands), which the supervisor `JSON.parse`s
    /// back to the contract shape. A command failure is a [`Response::Error`] instead.
    DbResult {
        id: u64,
        result_json: String,
    },
}

/// An unsolicited event (sidecar -> supervisor). No correlation id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Bounded terminal output chunk. `data` is raw binary (MessagePack `bin`),
    /// NOT JSON/base64 per chunk (explicit non-goal). `seq` orders chunks for the
    /// terminal so the UI can detect gaps.
    TerminalOutput {
        terminal_id: u32,
        seq: u64,
        data: Vec<u8>,
    },
    /// A structured log line surfaced to the UI.
    Log { level: String, message: String },
    /// The interactive terminal `terminal_id` exited with `code` (`None` = unknown /
    /// signalled). After this event the renderer tears down the xterm.
    PtyExit {
        terminal_id: u32,
        /// Present exit code, or absent (`None`) when unknown. Encoded as MessagePack
        /// `nil` when absent so the TS side decodes it as `null`.
        code: Option<i32>,
    },
    /// A decoded OSC 7 working-directory report for a live PTY (portable cwd source;
    /// purely observational — the bytes still flow in [`Event::TerminalOutput`]).
    PtyCwd { terminal_id: u32, cwd: String },
    /// A decoded OSC 133 command-END exec-state transition for a live PTY: `state` is
    /// `"success"` or `"error"`; `exit_code` is the parsed code when present.
    PtyExecState {
        terminal_id: u32,
        state: String,
        exit_code: Option<i32>,
    },
    /// An OS BUSY-STATE transition for a terminal (v2.5.5 review T1): the foreground process
    /// group of the terminal RECORD `record_id` entered (`busy = true`) or left
    /// (`busy = false`) a running command. Emitted by the busy poll ([`crate::runtime::busy_poll`])
    /// ONLY on a transition, keyed by the persistent RECORD id (so the renderer folds it
    /// directly onto its record, and the runtime tree re-pulls the snapshot whose overlaid
    /// `busy` bit then lights the LINE's `running` dot). Independent of OSC 133 / exec-state.
    PtyBusyState { record_id: String, busy: bool },
    /// A managed-command run-state / ack / output-cleared transition (the wire shape of
    /// the v1 `command://state` / `command://ack` / `command://output-cleared`).
    CommandState {
        /// `"state"` | `"ack"` | `"output-cleared"`.
        event: String,
        instance_id: String,
        /// `idle|running|success|error` for a `"state"` event; empty otherwise.
        state: String,
        exit_code: Option<i32>,
    },
    /// A coalesced managed-command output chunk (BINARY, no base64 — the wire shape of
    /// the v1 `command://output`), keyed by `instance_id`.
    CommandOutput { instance_id: String, data: Vec<u8> },
    /// A coarse "X changed -> re-fetch" invalidation (the EventSink.changed frontier on
    /// the wire). `topic` is one of `terminals|workspaces|commands|agent-sessions`.
    Changed { topic: String },
}

/// A decoded message of any category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Request(Request),
    Response(Response),
    Event(Event),
}

/// Errors decoding a message from a frame payload.
#[derive(Debug)]
pub enum DecodeError {
    /// MessagePack could not be parsed.
    Msgpack(String),
    /// Envelope shape (version/category/kind/body) was invalid.
    BadEnvelope(String),
    /// The message's stamped protocol version differs from ours.
    VersionMismatch { theirs: u32, ours: u32 },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Msgpack(e) => write!(f, "messagepack decode error: {e}"),
            DecodeError::BadEnvelope(e) => write!(f, "bad message envelope: {e}"),
            DecodeError::VersionMismatch { theirs, ours } => write!(
                f,
                "protocol version mismatch: peer speaks v{theirs}, this build speaks v{ours}"
            ),
        }
    }
}

impl std::error::Error for DecodeError {}

// --- The on-wire envelope as a serde tuple (encodes as a MessagePack array) ---
//
// We serialize/deserialize through `rmpv::Value` for the polymorphic `body`,
// keeping the envelope itself a fixed 4-tuple so the bytes are deterministic.

type Envelope = (u32, u8, u8, rmpv::Value);

fn val_u64(v: &rmpv::Value) -> Option<u64> {
    v.as_u64()
}
fn val_u32(v: &rmpv::Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}
fn val_u16(v: &rmpv::Value) -> Option<u16> {
    v.as_u64().and_then(|n| u16::try_from(n).ok())
}
fn val_str(v: &rmpv::Value) -> Option<String> {
    v.as_str().map(|s| s.to_string())
}
fn val_bool(v: &rmpv::Value) -> Option<bool> {
    v.as_bool()
}
fn val_arr(v: &rmpv::Value) -> Option<&Vec<rmpv::Value>> {
    v.as_array()
}
/// Encode an `Option<i32>` as MessagePack: an integer when present, `nil` when absent
/// (so the TS side decodes it as `null`). Used for PTY exit codes.
fn opt_i32(v: Option<i32>) -> rmpv::Value {
    match v {
        Some(n) => (n as i64).into(),
        None => rmpv::Value::Nil,
    }
}
/// Decode a MessagePack value into an `Option<i32>`: `nil` -> `None`, an integer ->
/// `Some`. Any other type is treated as `None` (defensive).
fn val_opt_i32(v: &rmpv::Value) -> Option<i32> {
    v.as_i64().and_then(|n| i32::try_from(n).ok())
}

/// Encode a message to its MessagePack envelope bytes (no frame prefix).
pub fn encode_message(msg: &Message) -> Vec<u8> {
    let (category, kind, body): (u8, u8, rmpv::Value) = match msg {
        Message::Request(r) => match r {
            Request::Handshake {
                id,
                protocol_version,
            } => (
                CAT_REQUEST,
                REQ_HANDSHAKE,
                rmpv::Value::Array(vec![(*id).into(), (*protocol_version).into()]),
            ),
            Request::Ping { id, nonce } => (
                CAT_REQUEST,
                REQ_PING,
                rmpv::Value::Array(vec![(*id).into(), (*nonce).into()]),
            ),
            Request::Shutdown { id } => (
                CAT_REQUEST,
                REQ_SHUTDOWN,
                rmpv::Value::Array(vec![(*id).into()]),
            ),
            Request::PtySpawn {
                id,
                cols,
                rows,
                cwd,
                record_id,
            } => (
                CAT_REQUEST,
                REQ_PTY_SPAWN,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    (*cols as u64).into(),
                    (*rows as u64).into(),
                    rmpv::Value::String(cwd.clone().into()),
                    rmpv::Value::String(record_id.clone().into()),
                ]),
            ),
            Request::PtyWrite {
                id,
                terminal_id,
                data,
            } => (
                CAT_REQUEST,
                REQ_PTY_WRITE,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    (*terminal_id).into(),
                    rmpv::Value::Binary(data.clone()),
                ]),
            ),
            Request::PtyResize {
                id,
                terminal_id,
                cols,
                rows,
            } => (
                CAT_REQUEST,
                REQ_PTY_RESIZE,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    (*terminal_id).into(),
                    (*cols as u64).into(),
                    (*rows as u64).into(),
                ]),
            ),
            Request::PtyClose { id, terminal_id } => (
                CAT_REQUEST,
                REQ_PTY_CLOSE,
                rmpv::Value::Array(vec![(*id).into(), (*terminal_id).into()]),
            ),
            Request::PtyAck {
                id,
                terminal_id,
                bytes,
            } => (
                CAT_REQUEST,
                REQ_PTY_ACK,
                rmpv::Value::Array(vec![(*id).into(), (*terminal_id).into(), (*bytes).into()]),
            ),
            Request::CommandStart { id, instance_id } => (
                CAT_REQUEST,
                REQ_COMMAND_START,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    rmpv::Value::String(instance_id.clone().into()),
                ]),
            ),
            Request::CommandStop { id, instance_id } => (
                CAT_REQUEST,
                REQ_COMMAND_STOP,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    rmpv::Value::String(instance_id.clone().into()),
                ]),
            ),
            Request::CommandRelaunch { id, instance_id } => (
                CAT_REQUEST,
                REQ_COMMAND_RELAUNCH,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    rmpv::Value::String(instance_id.clone().into()),
                ]),
            ),
            Request::McpStart { id } => (
                CAT_REQUEST,
                REQ_MCP_START,
                rmpv::Value::Array(vec![(*id).into()]),
            ),
            Request::DbCommand {
                id,
                command,
                args_json,
            } => (
                CAT_REQUEST,
                REQ_DB_COMMAND,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    rmpv::Value::String(command.clone().into()),
                    rmpv::Value::String(args_json.clone().into()),
                ]),
            ),
        },
        Message::Response(r) => match r {
            Response::HandshakeOk {
                id,
                protocol_version,
                capabilities,
                max_frame_len,
            } => (
                CAT_RESPONSE,
                RES_HANDSHAKE_OK,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    (*protocol_version).into(),
                    rmpv::Value::Array(
                        capabilities
                            .iter()
                            .map(|c| rmpv::Value::String(c.clone().into()))
                            .collect(),
                    ),
                    (*max_frame_len).into(),
                ]),
            ),
            Response::Pong { id, nonce } => (
                CAT_RESPONSE,
                RES_PONG,
                rmpv::Value::Array(vec![(*id).into(), (*nonce).into()]),
            ),
            Response::ShutdownAck { id } => (
                CAT_RESPONSE,
                RES_SHUTDOWN_ACK,
                rmpv::Value::Array(vec![(*id).into()]),
            ),
            Response::Error { id, code, message } => (
                CAT_RESPONSE,
                RES_ERROR,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    rmpv::Value::String(code.as_str().into()),
                    rmpv::Value::String(message.clone().into()),
                ]),
            ),
            Response::PtySpawned { id, terminal_id } => (
                CAT_RESPONSE,
                RES_PTY_SPAWNED,
                rmpv::Value::Array(vec![(*id).into(), (*terminal_id).into()]),
            ),
            Response::Ok { id } => (CAT_RESPONSE, RES_OK, rmpv::Value::Array(vec![(*id).into()])),
            Response::CommandStatus {
                id,
                instance_id,
                state,
                running,
                exit_code,
                unread,
                was_running,
                restarted,
            } => (
                CAT_RESPONSE,
                RES_COMMAND_STATUS,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    rmpv::Value::String(instance_id.clone().into()),
                    rmpv::Value::String(state.clone().into()),
                    rmpv::Value::Boolean(*running),
                    opt_i32(*exit_code),
                    rmpv::Value::Boolean(*unread),
                    rmpv::Value::Boolean(*was_running),
                    rmpv::Value::Boolean(*restarted),
                ]),
            ),
            Response::McpStarted { id, port } => (
                CAT_RESPONSE,
                RES_MCP_STARTED,
                rmpv::Value::Array(vec![(*id).into(), (*port as u64).into()]),
            ),
            Response::DbResult { id, result_json } => (
                CAT_RESPONSE,
                RES_DB_RESULT,
                rmpv::Value::Array(vec![
                    (*id).into(),
                    rmpv::Value::String(result_json.clone().into()),
                ]),
            ),
        },
        Message::Event(e) => match e {
            Event::TerminalOutput {
                terminal_id,
                seq,
                data,
            } => (
                CAT_EVENT,
                EVT_TERMINAL_OUTPUT,
                rmpv::Value::Array(vec![
                    (*terminal_id).into(),
                    (*seq).into(),
                    rmpv::Value::Binary(data.clone()),
                ]),
            ),
            Event::Log { level, message } => (
                CAT_EVENT,
                EVT_LOG,
                rmpv::Value::Array(vec![
                    rmpv::Value::String(level.clone().into()),
                    rmpv::Value::String(message.clone().into()),
                ]),
            ),
            Event::PtyExit { terminal_id, code } => (
                CAT_EVENT,
                EVT_PTY_EXIT,
                rmpv::Value::Array(vec![(*terminal_id).into(), opt_i32(*code)]),
            ),
            Event::PtyCwd { terminal_id, cwd } => (
                CAT_EVENT,
                EVT_PTY_CWD,
                rmpv::Value::Array(vec![
                    (*terminal_id).into(),
                    rmpv::Value::String(cwd.clone().into()),
                ]),
            ),
            Event::PtyExecState {
                terminal_id,
                state,
                exit_code,
            } => (
                CAT_EVENT,
                EVT_PTY_EXEC_STATE,
                rmpv::Value::Array(vec![
                    (*terminal_id).into(),
                    rmpv::Value::String(state.clone().into()),
                    opt_i32(*exit_code),
                ]),
            ),
            Event::PtyBusyState { record_id, busy } => (
                CAT_EVENT,
                EVT_PTY_BUSY_STATE,
                rmpv::Value::Array(vec![
                    rmpv::Value::String(record_id.clone().into()),
                    rmpv::Value::Boolean(*busy),
                ]),
            ),
            Event::CommandState {
                event,
                instance_id,
                state,
                exit_code,
            } => (
                CAT_EVENT,
                EVT_COMMAND_STATE,
                rmpv::Value::Array(vec![
                    rmpv::Value::String(event.clone().into()),
                    rmpv::Value::String(instance_id.clone().into()),
                    rmpv::Value::String(state.clone().into()),
                    opt_i32(*exit_code),
                ]),
            ),
            Event::CommandOutput { instance_id, data } => (
                CAT_EVENT,
                EVT_COMMAND_OUTPUT,
                rmpv::Value::Array(vec![
                    rmpv::Value::String(instance_id.clone().into()),
                    rmpv::Value::Binary(data.clone()),
                ]),
            ),
            Event::Changed { topic } => (
                CAT_EVENT,
                EVT_CHANGED,
                rmpv::Value::Array(vec![rmpv::Value::String(topic.clone().into())]),
            ),
        },
    };
    let env: Envelope = (PROTOCOL_VERSION, category, kind, body);
    rmp_serde::to_vec(&env).expect("envelope serialization is infallible")
}

/// Encode a message straight into a length-prefixed frame ready for the pipe.
pub fn encode_message_frame(msg: &Message) -> Result<Vec<u8>, FrameError> {
    encode_frame(&encode_message(msg))
}

/// Decode a MessagePack envelope (no frame prefix) into a [`Message`], enforcing
/// the protocol version stamped on the envelope.
pub fn decode_message(bytes: &[u8]) -> Result<Message, DecodeError> {
    let env: Envelope =
        rmp_serde::from_slice(bytes).map_err(|e| DecodeError::Msgpack(e.to_string()))?;
    let (version, category, kind, body) = env;
    if version != PROTOCOL_VERSION {
        return Err(DecodeError::VersionMismatch {
            theirs: version,
            ours: PROTOCOL_VERSION,
        });
    }
    let fields = val_arr(&body)
        .ok_or_else(|| DecodeError::BadEnvelope("body is not an array".into()))?
        .clone();
    let bad = |m: &str| DecodeError::BadEnvelope(m.to_string());

    let msg = match (category, kind) {
        (CAT_REQUEST, REQ_HANDSHAKE) => Message::Request(Request::Handshake {
            id: val_u64(&fields[0]).ok_or_else(|| bad("handshake.id"))?,
            protocol_version: val_u32(fields.get(1).ok_or_else(|| bad("handshake.pv"))?)
                .ok_or_else(|| bad("handshake.pv"))?,
        }),
        (CAT_REQUEST, REQ_PING) => Message::Request(Request::Ping {
            id: val_u64(&fields[0]).ok_or_else(|| bad("ping.id"))?,
            nonce: val_u64(fields.get(1).ok_or_else(|| bad("ping.nonce"))?)
                .ok_or_else(|| bad("ping.nonce"))?,
        }),
        (CAT_REQUEST, REQ_SHUTDOWN) => Message::Request(Request::Shutdown {
            id: val_u64(&fields[0]).ok_or_else(|| bad("shutdown.id"))?,
        }),
        (CAT_REQUEST, REQ_PTY_SPAWN) => Message::Request(Request::PtySpawn {
            id: val_u64(&fields[0]).ok_or_else(|| bad("pty_spawn.id"))?,
            cols: val_u16(fields.get(1).ok_or_else(|| bad("pty_spawn.cols"))?)
                .ok_or_else(|| bad("pty_spawn.cols"))?,
            rows: val_u16(fields.get(2).ok_or_else(|| bad("pty_spawn.rows"))?)
                .ok_or_else(|| bad("pty_spawn.rows"))?,
            cwd: val_str(fields.get(3).ok_or_else(|| bad("pty_spawn.cwd"))?)
                .ok_or_else(|| bad("pty_spawn.cwd"))?,
            record_id: val_str(fields.get(4).ok_or_else(|| bad("pty_spawn.record_id"))?)
                .ok_or_else(|| bad("pty_spawn.record_id"))?,
        }),
        (CAT_REQUEST, REQ_PTY_WRITE) => Message::Request(Request::PtyWrite {
            id: val_u64(&fields[0]).ok_or_else(|| bad("pty_write.id"))?,
            terminal_id: val_u32(fields.get(1).ok_or_else(|| bad("pty_write.term"))?)
                .ok_or_else(|| bad("pty_write.term"))?,
            data: fields
                .get(2)
                .and_then(|v| v.as_slice())
                .map(|s| s.to_vec())
                .ok_or_else(|| bad("pty_write.data not bin"))?,
        }),
        (CAT_REQUEST, REQ_PTY_RESIZE) => Message::Request(Request::PtyResize {
            id: val_u64(&fields[0]).ok_or_else(|| bad("pty_resize.id"))?,
            terminal_id: val_u32(fields.get(1).ok_or_else(|| bad("pty_resize.term"))?)
                .ok_or_else(|| bad("pty_resize.term"))?,
            cols: val_u16(fields.get(2).ok_or_else(|| bad("pty_resize.cols"))?)
                .ok_or_else(|| bad("pty_resize.cols"))?,
            rows: val_u16(fields.get(3).ok_or_else(|| bad("pty_resize.rows"))?)
                .ok_or_else(|| bad("pty_resize.rows"))?,
        }),
        (CAT_REQUEST, REQ_PTY_CLOSE) => Message::Request(Request::PtyClose {
            id: val_u64(&fields[0]).ok_or_else(|| bad("pty_close.id"))?,
            terminal_id: val_u32(fields.get(1).ok_or_else(|| bad("pty_close.term"))?)
                .ok_or_else(|| bad("pty_close.term"))?,
        }),
        (CAT_REQUEST, REQ_PTY_ACK) => Message::Request(Request::PtyAck {
            id: val_u64(&fields[0]).ok_or_else(|| bad("pty_ack.id"))?,
            terminal_id: val_u32(fields.get(1).ok_or_else(|| bad("pty_ack.term"))?)
                .ok_or_else(|| bad("pty_ack.term"))?,
            bytes: val_u64(fields.get(2).ok_or_else(|| bad("pty_ack.bytes"))?)
                .ok_or_else(|| bad("pty_ack.bytes"))?,
        }),
        (CAT_REQUEST, REQ_COMMAND_START) => Message::Request(Request::CommandStart {
            id: val_u64(&fields[0]).ok_or_else(|| bad("command_start.id"))?,
            instance_id: val_str(fields.get(1).ok_or_else(|| bad("command_start.iid"))?)
                .ok_or_else(|| bad("command_start.iid"))?,
        }),
        (CAT_REQUEST, REQ_COMMAND_STOP) => Message::Request(Request::CommandStop {
            id: val_u64(&fields[0]).ok_or_else(|| bad("command_stop.id"))?,
            instance_id: val_str(fields.get(1).ok_or_else(|| bad("command_stop.iid"))?)
                .ok_or_else(|| bad("command_stop.iid"))?,
        }),
        (CAT_REQUEST, REQ_COMMAND_RELAUNCH) => Message::Request(Request::CommandRelaunch {
            id: val_u64(&fields[0]).ok_or_else(|| bad("command_relaunch.id"))?,
            instance_id: val_str(fields.get(1).ok_or_else(|| bad("command_relaunch.iid"))?)
                .ok_or_else(|| bad("command_relaunch.iid"))?,
        }),
        (CAT_REQUEST, REQ_MCP_START) => Message::Request(Request::McpStart {
            id: val_u64(&fields[0]).ok_or_else(|| bad("mcp_start.id"))?,
        }),
        (CAT_REQUEST, REQ_DB_COMMAND) => Message::Request(Request::DbCommand {
            id: val_u64(&fields[0]).ok_or_else(|| bad("db_command.id"))?,
            command: val_str(fields.get(1).ok_or_else(|| bad("db_command.command"))?)
                .ok_or_else(|| bad("db_command.command"))?,
            args_json: val_str(fields.get(2).ok_or_else(|| bad("db_command.args"))?)
                .ok_or_else(|| bad("db_command.args"))?,
        }),
        (CAT_RESPONSE, RES_HANDSHAKE_OK) => Message::Response(Response::HandshakeOk {
            id: val_u64(&fields[0]).ok_or_else(|| bad("handshake_ok.id"))?,
            protocol_version: val_u32(&fields[1]).ok_or_else(|| bad("handshake_ok.pv"))?,
            capabilities: val_arr(&fields[2])
                .ok_or_else(|| bad("handshake_ok.caps"))?
                .iter()
                .filter_map(val_str)
                .collect(),
            max_frame_len: val_u32(&fields[3]).ok_or_else(|| bad("handshake_ok.max"))?,
        }),
        (CAT_RESPONSE, RES_PONG) => Message::Response(Response::Pong {
            id: val_u64(&fields[0]).ok_or_else(|| bad("pong.id"))?,
            nonce: val_u64(&fields[1]).ok_or_else(|| bad("pong.nonce"))?,
        }),
        (CAT_RESPONSE, RES_SHUTDOWN_ACK) => Message::Response(Response::ShutdownAck {
            id: val_u64(&fields[0]).ok_or_else(|| bad("shutdown_ack.id"))?,
        }),
        (CAT_RESPONSE, RES_ERROR) => Message::Response(Response::Error {
            id: val_u64(&fields[0]).ok_or_else(|| bad("error.id"))?,
            code: val_str(&fields[1])
                .as_deref()
                .and_then(ErrorCode::from_str)
                .ok_or_else(|| bad("error.code"))?,
            message: val_str(&fields[2]).ok_or_else(|| bad("error.message"))?,
        }),
        (CAT_RESPONSE, RES_PTY_SPAWNED) => Message::Response(Response::PtySpawned {
            id: val_u64(&fields[0]).ok_or_else(|| bad("pty_spawned.id"))?,
            terminal_id: val_u32(fields.get(1).ok_or_else(|| bad("pty_spawned.term"))?)
                .ok_or_else(|| bad("pty_spawned.term"))?,
        }),
        (CAT_RESPONSE, RES_OK) => Message::Response(Response::Ok {
            id: val_u64(&fields[0]).ok_or_else(|| bad("ok.id"))?,
        }),
        (CAT_RESPONSE, RES_COMMAND_STATUS) => Message::Response(Response::CommandStatus {
            id: val_u64(&fields[0]).ok_or_else(|| bad("cmd_status.id"))?,
            instance_id: val_str(fields.get(1).ok_or_else(|| bad("cmd_status.iid"))?)
                .ok_or_else(|| bad("cmd_status.iid"))?,
            state: val_str(fields.get(2).ok_or_else(|| bad("cmd_status.state"))?)
                .ok_or_else(|| bad("cmd_status.state"))?,
            running: val_bool(fields.get(3).ok_or_else(|| bad("cmd_status.running"))?)
                .ok_or_else(|| bad("cmd_status.running"))?,
            exit_code: fields.get(4).and_then(val_opt_i32),
            unread: val_bool(fields.get(5).ok_or_else(|| bad("cmd_status.unread"))?)
                .ok_or_else(|| bad("cmd_status.unread"))?,
            was_running: val_bool(fields.get(6).ok_or_else(|| bad("cmd_status.was_running"))?)
                .ok_or_else(|| bad("cmd_status.was_running"))?,
            restarted: val_bool(fields.get(7).ok_or_else(|| bad("cmd_status.restarted"))?)
                .ok_or_else(|| bad("cmd_status.restarted"))?,
        }),
        (CAT_RESPONSE, RES_MCP_STARTED) => Message::Response(Response::McpStarted {
            id: val_u64(&fields[0]).ok_or_else(|| bad("mcp_started.id"))?,
            port: val_u16(fields.get(1).ok_or_else(|| bad("mcp_started.port"))?)
                .ok_or_else(|| bad("mcp_started.port"))?,
        }),
        (CAT_RESPONSE, RES_DB_RESULT) => Message::Response(Response::DbResult {
            id: val_u64(&fields[0]).ok_or_else(|| bad("db_result.id"))?,
            result_json: val_str(fields.get(1).ok_or_else(|| bad("db_result.result"))?)
                .ok_or_else(|| bad("db_result.result"))?,
        }),
        (CAT_EVENT, EVT_TERMINAL_OUTPUT) => Message::Event(Event::TerminalOutput {
            terminal_id: val_u32(&fields[0]).ok_or_else(|| bad("term.id"))?,
            seq: val_u64(&fields[1]).ok_or_else(|| bad("term.seq"))?,
            data: fields[2]
                .as_slice()
                .map(|s| s.to_vec())
                .ok_or_else(|| bad("term.data not bin"))?,
        }),
        (CAT_EVENT, EVT_LOG) => Message::Event(Event::Log {
            level: val_str(&fields[0]).ok_or_else(|| bad("log.level"))?,
            message: val_str(&fields[1]).ok_or_else(|| bad("log.message"))?,
        }),
        (CAT_EVENT, EVT_PTY_EXIT) => Message::Event(Event::PtyExit {
            terminal_id: val_u32(&fields[0]).ok_or_else(|| bad("pty_exit.term"))?,
            code: fields.get(1).and_then(val_opt_i32),
        }),
        (CAT_EVENT, EVT_PTY_CWD) => Message::Event(Event::PtyCwd {
            terminal_id: val_u32(&fields[0]).ok_or_else(|| bad("pty_cwd.term"))?,
            cwd: val_str(fields.get(1).ok_or_else(|| bad("pty_cwd.cwd"))?)
                .ok_or_else(|| bad("pty_cwd.cwd"))?,
        }),
        (CAT_EVENT, EVT_PTY_EXEC_STATE) => Message::Event(Event::PtyExecState {
            terminal_id: val_u32(&fields[0]).ok_or_else(|| bad("pty_exec.term"))?,
            state: val_str(fields.get(1).ok_or_else(|| bad("pty_exec.state"))?)
                .ok_or_else(|| bad("pty_exec.state"))?,
            exit_code: fields.get(2).and_then(val_opt_i32),
        }),
        (CAT_EVENT, EVT_PTY_BUSY_STATE) => Message::Event(Event::PtyBusyState {
            record_id: val_str(&fields[0]).ok_or_else(|| bad("pty_busy.record"))?,
            busy: val_bool(fields.get(1).ok_or_else(|| bad("pty_busy.busy"))?)
                .ok_or_else(|| bad("pty_busy.busy"))?,
        }),
        (CAT_EVENT, EVT_COMMAND_STATE) => Message::Event(Event::CommandState {
            event: val_str(&fields[0]).ok_or_else(|| bad("cmd_state.event"))?,
            instance_id: val_str(fields.get(1).ok_or_else(|| bad("cmd_state.iid"))?)
                .ok_or_else(|| bad("cmd_state.iid"))?,
            state: val_str(fields.get(2).ok_or_else(|| bad("cmd_state.state"))?)
                .ok_or_else(|| bad("cmd_state.state"))?,
            exit_code: fields.get(3).and_then(val_opt_i32),
        }),
        (CAT_EVENT, EVT_COMMAND_OUTPUT) => Message::Event(Event::CommandOutput {
            instance_id: val_str(&fields[0]).ok_or_else(|| bad("cmd_output.iid"))?,
            data: fields
                .get(1)
                .and_then(|v| v.as_slice())
                .map(|s| s.to_vec())
                .ok_or_else(|| bad("cmd_output.data not bin"))?,
        }),
        (CAT_EVENT, EVT_CHANGED) => Message::Event(Event::Changed {
            topic: val_str(&fields[0]).ok_or_else(|| bad("changed.topic"))?,
        }),
        (c, k) => return Err(bad(&format!("unknown category/kind {c}/{k}"))),
    };
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: Message) {
        let bytes = encode_message(&msg);
        let back = decode_message(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn request_response_event_roundtrip() {
        roundtrip(Message::Request(Request::Handshake {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
        }));
        roundtrip(Message::Request(Request::Ping { id: 2, nonce: 99 }));
        roundtrip(Message::Request(Request::Shutdown { id: 3 }));
        roundtrip(Message::Response(Response::HandshakeOk {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
            capabilities: CAPABILITIES.iter().map(|s| s.to_string()).collect(),
            max_frame_len: super::super::codec::MAX_FRAME_LEN,
        }));
        roundtrip(Message::Response(Response::Pong { id: 2, nonce: 99 }));
        roundtrip(Message::Response(Response::ShutdownAck { id: 3 }));
        roundtrip(Message::Response(Response::Error {
            id: 4,
            code: ErrorCode::VersionIncompatible,
            message: "boom".into(),
        }));
        roundtrip(Message::Event(Event::TerminalOutput {
            terminal_id: 7,
            seq: 42,
            data: vec![0x1b, b'[', b'0', b'm'],
        }));
        roundtrip(Message::Event(Event::Log {
            level: "info".into(),
            message: "ready".into(),
        }));
        // v2.5.5 review T1: the OS busy-state transition, keyed by the record id.
        roundtrip(Message::Event(Event::PtyBusyState {
            record_id: "term-abc".into(),
            busy: true,
        }));
        roundtrip(Message::Event(Event::PtyBusyState {
            record_id: "term-xyz".into(),
            busy: false,
        }));
    }

    #[test]
    fn version_mismatch_detected_on_decode() {
        // Hand-build an envelope stamped with a wrong version.
        let env: Envelope = (
            PROTOCOL_VERSION + 100,
            CAT_REQUEST,
            REQ_PING,
            rmpv::Value::Array(vec![1u64.into(), 2u64.into()]),
        );
        let bytes = rmp_serde::to_vec(&env).unwrap();
        match decode_message(&bytes) {
            Err(DecodeError::VersionMismatch { theirs, ours }) => {
                assert_eq!(theirs, PROTOCOL_VERSION + 100);
                assert_eq!(ours, PROTOCOL_VERSION);
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
    }
}
