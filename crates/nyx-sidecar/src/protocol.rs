//! Control protocol between Electron main (the supervisor) and the sidecar.
//!
//! Task 522 establishes the *supervised-lifecycle* control plane: a `handshake`
//! that exchanges versions + capabilities, a `ping` liveness probe, and a
//! bounded `shutdown`. The wire encoding here is deliberately minimal — one
//! newline-delimited JSON object per message over `stdin`/`stdout` — so the
//! lifecycle can be implemented and tested *before* task 523 lands the versioned,
//! length-prefixed MessagePack frame transport. Task 523 reframes these exact
//! request/response shapes over the binary pipe; the message *semantics* defined
//! here do not change.
//!
//! Requests flow supervisor -> sidecar; responses flow sidecar -> supervisor,
//! correlated by the request `id`.

use serde::{Deserialize, Serialize};

/// The control-protocol version the sidecar speaks. The supervisor announces the
/// version it expects in [`HandshakeRequest::protocol_version`]; a mismatch is a
/// hard, readable boot error (the full version-incompatibility detection over the
/// framed pipe is task 523, but the field is wired from 522 so it round-trips).
pub const PROTOCOL_VERSION: u32 = 1;

/// Capabilities this sidecar build advertises during the handshake. Task 522 ships
/// the supervised lifecycle only; PTY and command capabilities are added by their
/// dedicated tasks.
pub const CAPABILITIES: &[&str] = &["handshake", "ping", "shutdown"];

/// A request from the supervisor to the sidecar. `id` correlates the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// First message: exchange protocol version + capabilities.
    Handshake {
        id: u64,
        /// Protocol version the supervisor expects the sidecar to speak.
        protocol_version: u32,
    },
    /// Liveness probe; echoes `nonce` back in the response.
    Ping { id: u64, nonce: u64 },
    /// Ask the sidecar to shut down gracefully within its bounded deadline.
    Shutdown { id: u64 },
}

impl Request {
    /// The correlation id carried by every request variant. Part of the protocol
    /// contract surface task 523 correlates over the framed pipe; exercised by the
    /// round-trip tests here.
    #[allow(dead_code)]
    pub fn id(&self) -> u64 {
        match self {
            Request::Handshake { id, .. } | Request::Ping { id, .. } | Request::Shutdown { id } => {
                *id
            }
        }
    }
}

/// A response from the sidecar to the supervisor, correlated by `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Successful handshake: the sidecar's own protocol version + capabilities.
    HandshakeOk {
        id: u64,
        protocol_version: u32,
        capabilities: Vec<String>,
        /// Informational build version of the sidecar (`CARGO_PKG_VERSION`).
        sidecar_version: String,
    },
    /// Liveness reply echoing the probe nonce.
    Pong { id: u64, nonce: u64 },
    /// Acknowledges a shutdown request; the process exits right after sending it.
    ShutdownAck { id: u64 },
    /// A structured, stable error (e.g. version incompatibility at handshake).
    Error {
        id: u64,
        code: ErrorCode,
        message: String,
    },
}

impl Response {
    /// The correlation id carried by every response variant. Part of the protocol
    /// contract surface task 523 correlates over the framed pipe; exercised by the
    /// round-trip tests here.
    #[allow(dead_code)]
    pub fn id(&self) -> u64 {
        match self {
            Response::HandshakeOk { id, .. }
            | Response::Pong { id, .. }
            | Response::ShutdownAck { id }
            | Response::Error { id, .. } => *id,
        }
    }
}

/// Stable, machine-readable error codes. Kept small and additive so the TS side
/// can match on them; task 523 carries these unchanged over the framed pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The supervisor's expected protocol version differs from the sidecar's.
    VersionIncompatible,
    /// The request could not be decoded.
    MalformedRequest,
}

/// Handle a single decoded request and produce its response. Pure (no I/O), so it
/// is exhaustively unit-testable. `should_exit` is set when the response is a
/// shutdown acknowledgement, telling the caller to stop the loop and reap.
pub fn handle(req: &Request) -> (Response, bool) {
    match req {
        Request::Handshake {
            id,
            protocol_version,
        } => {
            if *protocol_version != PROTOCOL_VERSION {
                let resp = Response::Error {
                    id: *id,
                    code: ErrorCode::VersionIncompatible,
                    message: format!(
                        "protocol version mismatch: supervisor expects v{}, sidecar speaks v{}",
                        protocol_version, PROTOCOL_VERSION
                    ),
                };
                (resp, false)
            } else {
                let resp = Response::HandshakeOk {
                    id: *id,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: CAPABILITIES.iter().map(|s| s.to_string()).collect(),
                    sidecar_version: env!("CARGO_PKG_VERSION").to_string(),
                };
                (resp, false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_ok_advertises_version_and_capabilities() {
        let (resp, exit) = handle(&Request::Handshake {
            id: 7,
            protocol_version: PROTOCOL_VERSION,
        });
        assert!(!exit);
        match resp {
            Response::HandshakeOk {
                id,
                protocol_version,
                capabilities,
                ..
            } => {
                assert_eq!(id, 7);
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert!(capabilities.iter().any(|c| c == "ping"));
                assert!(capabilities.iter().any(|c| c == "shutdown"));
            }
            other => panic!("expected HandshakeOk, got {other:?}"),
        }
    }

    #[test]
    fn handshake_version_mismatch_is_readable_error() {
        let (resp, exit) = handle(&Request::Handshake {
            id: 1,
            protocol_version: PROTOCOL_VERSION + 99,
        });
        assert!(!exit);
        match resp {
            Response::Error { code, message, .. } => {
                assert_eq!(code, ErrorCode::VersionIncompatible);
                assert!(message.contains("protocol version mismatch"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn ping_echoes_nonce() {
        let (resp, exit) = handle(&Request::Ping { id: 2, nonce: 42 });
        assert!(!exit);
        assert!(matches!(resp, Response::Pong { id: 2, nonce: 42 }));
    }

    #[test]
    fn shutdown_acks_and_signals_exit() {
        let (resp, exit) = handle(&Request::Shutdown { id: 3 });
        assert!(exit);
        assert!(matches!(resp, Response::ShutdownAck { id: 3 }));
    }

    #[test]
    fn request_and_response_roundtrip_through_json() {
        let req = Request::Ping { id: 9, nonce: 5 };
        let line = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back.id(), 9);
        let (resp, _) = handle(&back);
        let rline = serde_json::to_string(&resp).unwrap();
        let rback: Response = serde_json::from_str(&rline).unwrap();
        assert_eq!(rback.id(), 9);
    }
}
