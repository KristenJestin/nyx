//! The internal main <-> sidecar transport (PDR-0007): length-prefixed
//! MessagePack frames, id-correlated request/response, unsolicited events,
//! per-terminal backpressure, stable structured errors, and boot-time version
//! incompatibility detection.
//!
//! Submodules:
//!   * [`codec`] — u32-BE length-prefixed framing + incremental decode + max size.
//!   * [`wire`]  — the versioned, positional-array MessagePack message catalogue.
//!   * [`flow`]  — per-terminal quota / backpressure.
//!
//! The TS counterpart lives in `apps/desktop/src/shared/pipe-protocol.ts`; the
//! canonical wire bytes are pinned in `contract/fixtures.json` and verified GREEN
//! on both sides (`cargo test -p nyx-sidecar --test contract` and
//! `bun test contract/contract.test.ts`).

pub mod codec;
pub mod flow;
pub mod wire;

use wire::{ErrorCode, Message, Request, Response};

/// Outcome of handling a stateless lifecycle request. PTY requests (HHC) are NOT
/// stateless — they drive the runtime's live PTY manager — so they are not handled
/// here; the binary's run loop routes them to [`crate::runtime`] instead.
pub enum Handled {
    /// A stateless reply was produced; `exit` is set for a shutdown ack.
    Reply { response: Response, exit: bool },
    /// This request targets the runtime (a PTY op); the caller must dispatch it there.
    NeedsRuntime,
}

/// Handle one decoded STATELESS lifecycle request (handshake / ping / shutdown).
/// Returns [`Handled::NeedsRuntime`] for any request that drives runtime state (the
/// PTY ops), which the binary routes to [`crate::runtime`].
pub fn handle_request_stateless(req: &Request) -> Handled {
    match req {
        // PTY ops (HHC) + managed-command/MCP ops (HHD) all drive runtime state.
        Request::PtySpawn { .. }
        | Request::PtyWrite { .. }
        | Request::PtyResize { .. }
        | Request::PtyClose { .. }
        | Request::PtyAck { .. }
        | Request::CommandStart { .. }
        | Request::CommandStop { .. }
        | Request::CommandRelaunch { .. }
        | Request::McpStart { .. }
        | Request::DbCommand { .. } => Handled::NeedsRuntime,
        _ => {
            let (response, exit) = handle_request(req);
            Handled::Reply { response, exit }
        }
    }
}

/// Handle one decoded stateless request and produce its response (pure; no I/O).
/// Returns `(response, should_exit)` — `should_exit` is set for a shutdown ack.
///
/// This is the framed-transport equivalent of the line-based control handler from
/// task 522; the lifecycle semantics are identical, now carried over the pipe.
///
/// Only the stateless lifecycle requests reach this function; PTY requests are routed
/// to the runtime by [`handle_request_stateless`]. The PTY arms are `unreachable!`
/// here so the match stays exhaustive without duplicating the routing decision.
pub fn handle_request(req: &Request) -> (Response, bool) {
    match req {
        Request::PtySpawn { .. }
        | Request::PtyWrite { .. }
        | Request::PtyResize { .. }
        | Request::PtyClose { .. }
        | Request::PtyAck { .. }
        | Request::CommandStart { .. }
        | Request::CommandStop { .. }
        | Request::CommandRelaunch { .. }
        | Request::McpStart { .. }
        | Request::DbCommand { .. } => {
            unreachable!("runtime requests are routed to the runtime, not handle_request")
        }
        Request::Handshake {
            id,
            protocol_version,
        } => {
            if *protocol_version != wire::PROTOCOL_VERSION {
                (
                    Response::Error {
                        id: *id,
                        code: ErrorCode::VersionIncompatible,
                        message: format!(
                            "protocol version mismatch: supervisor expects v{}, sidecar speaks v{}",
                            protocol_version,
                            wire::PROTOCOL_VERSION
                        ),
                    },
                    false,
                )
            } else {
                (
                    Response::HandshakeOk {
                        id: *id,
                        protocol_version: wire::PROTOCOL_VERSION,
                        capabilities: wire::CAPABILITIES.iter().map(|s| s.to_string()).collect(),
                        max_frame_len: codec::MAX_FRAME_LEN,
                    },
                    false,
                )
            }
        }
        Request::Ping { id, nonce } => (
            Response::Pong {
                id: *id,
                nonce: *nonce,
            },
            false,
        ),
        Request::Shutdown { id } => (Response::ShutdownAck { id: *id }, true),
    }
}

/// Convenience: decode a frame payload, dispatch if it is a request, and return
/// the framed response bytes. Used by the round-trip test and the binary's loop.
pub fn handle_message(bytes: &[u8]) -> Result<Option<(Message, bool)>, wire::DecodeError> {
    match wire::decode_message(bytes)? {
        Message::Request(req) => {
            let (resp, exit) = handle_request(&req);
            Ok(Some((Message::Response(resp), exit)))
        }
        // The sidecar does not act on responses/events it receives in this phase.
        other => Ok(Some((other, false))),
    }
}
