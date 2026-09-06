//! Rust side of the Rust <-> TS contract-parity fixtures (task 523).
//!
//! `<repo>/contract/fixtures.json` is the SINGLE SOURCE OF TRUTH for the canonical
//! wire bytes of each message kind. This test:
//!   * asserts the Rust wire codec ENCODES each fixture message to exactly the
//!     pinned `envelope_hex` and `frame_hex` bytes;
//!   * asserts the Rust codec DECODES those bytes back to the original message.
//!
//! The TS side (`contract/contract.test.ts`) reads the SAME fixtures and makes the
//! SAME assertions with @msgpack/msgpack, proving byte-for-byte parity.
//!
//! Regenerate the fixtures (after adding/altering a message kind) with:
//!   NYX_GEN_FIXTURES=1 cargo test -p nyx-sidecar --test contract -- --nocapture

use nyx_sidecar::pipe::codec::encode_frame;
use nyx_sidecar::pipe::wire::{
    self, decode_message, encode_message, ErrorCode, Event, Message, Request, Response,
    PROTOCOL_VERSION,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The canonical fixture set, named. The names must match `contract.test.ts`.
fn fixtures() -> Vec<(&'static str, Message)> {
    vec![
        (
            "req_handshake",
            Message::Request(Request::Handshake {
                id: 1,
                protocol_version: PROTOCOL_VERSION,
            }),
        ),
        (
            "req_ping",
            Message::Request(Request::Ping {
                id: 2,
                nonce: 0xDEAD,
            }),
        ),
        (
            "req_shutdown",
            Message::Request(Request::Shutdown { id: 3 }),
        ),
        (
            "res_handshake_ok",
            Message::Response(Response::HandshakeOk {
                id: 1,
                protocol_version: PROTOCOL_VERSION,
                capabilities: wire::CAPABILITIES.iter().map(|s| s.to_string()).collect(),
                max_frame_len: nyx_sidecar::pipe::codec::MAX_FRAME_LEN,
            }),
        ),
        (
            "res_pong",
            Message::Response(Response::Pong {
                id: 2,
                nonce: 0xDEAD,
            }),
        ),
        (
            "res_shutdown_ack",
            Message::Response(Response::ShutdownAck { id: 3 }),
        ),
        (
            "res_error_version",
            Message::Response(Response::Error {
                id: 4,
                code: ErrorCode::VersionIncompatible,
                message: "protocol version mismatch: supervisor speaks v2, sidecar speaks v1"
                    .into(),
            }),
        ),
        (
            "evt_terminal_output",
            Message::Event(Event::TerminalOutput {
                terminal_id: 7,
                seq: 42,
                // raw bytes incl. an ESC sequence + a non-UTF8 byte (0xff) to prove
                // binary frames, not JSON/base64.
                data: vec![0x1b, b'[', b'0', b'm', 0xff],
            }),
        ),
        (
            "evt_log",
            Message::Event(Event::Log {
                level: "info".into(),
                message: "ready".into(),
            }),
        ),
        // --- PTY surface (HHC) ---------------------------------------------------
        (
            "req_pty_spawn",
            Message::Request(Request::PtySpawn {
                id: 10,
                cols: 80,
                rows: 24,
                cwd: "/work".into(),
                record_id: "term-rec-1".into(),
            }),
        ),
        (
            "req_pty_write",
            Message::Request(Request::PtyWrite {
                id: 11,
                terminal_id: 7,
                // raw bytes incl. a control byte (0x03 = Ctrl-C) — binary, not base64.
                data: vec![b'l', b's', 0x0d, 0x03],
            }),
        ),
        (
            "req_pty_resize",
            Message::Request(Request::PtyResize {
                id: 12,
                terminal_id: 7,
                cols: 120,
                rows: 40,
            }),
        ),
        (
            "req_pty_close",
            Message::Request(Request::PtyClose {
                id: 13,
                terminal_id: 7,
            }),
        ),
        (
            "req_pty_ack",
            Message::Request(Request::PtyAck {
                id: 14,
                terminal_id: 7,
                bytes: 65536,
            }),
        ),
        (
            "res_pty_spawned",
            Message::Response(Response::PtySpawned {
                id: 10,
                terminal_id: 7,
            }),
        ),
        ("res_ok", Message::Response(Response::Ok { id: 11 })),
        (
            "evt_pty_exit",
            Message::Event(Event::PtyExit {
                terminal_id: 7,
                code: Some(0),
            }),
        ),
        (
            "evt_pty_exit_unknown",
            Message::Event(Event::PtyExit {
                terminal_id: 7,
                code: None,
            }),
        ),
        (
            "evt_pty_cwd",
            Message::Event(Event::PtyCwd {
                terminal_id: 7,
                cwd: "/work/sub".into(),
            }),
        ),
        (
            "evt_pty_exec_state",
            Message::Event(Event::PtyExecState {
                terminal_id: 7,
                state: "error".into(),
                exit_code: Some(1),
            }),
        ),
        // --- Managed-command + MCP surface (HHD) ---------------------------------
        (
            "req_command_start",
            Message::Request(Request::CommandStart {
                id: 30,
                instance_id: "inst-1".into(),
            }),
        ),
        (
            "req_command_stop",
            Message::Request(Request::CommandStop {
                id: 31,
                instance_id: "inst-1".into(),
            }),
        ),
        (
            "req_command_relaunch",
            Message::Request(Request::CommandRelaunch {
                id: 32,
                instance_id: "inst-1".into(),
            }),
        ),
        (
            "req_mcp_start",
            Message::Request(Request::McpStart { id: 33 }),
        ),
        (
            "res_command_status",
            Message::Response(Response::CommandStatus {
                id: 30,
                instance_id: "inst-1".into(),
                state: "running".into(),
                running: true,
                exit_code: None,
                unread: false,
                was_running: false,
                restarted: false,
            }),
        ),
        (
            "res_mcp_started",
            Message::Response(Response::McpStarted { id: 33, port: 8765 }),
        ),
        (
            "evt_command_state",
            Message::Event(Event::CommandState {
                event: "state".into(),
                instance_id: "inst-1".into(),
                state: "success".into(),
                exit_code: Some(0),
            }),
        ),
        (
            "evt_command_output",
            Message::Event(Event::CommandOutput {
                instance_id: "inst-1".into(),
                data: vec![b'o', b'k', 0x0a],
            }),
        ),
        (
            "evt_changed",
            Message::Event(Event::Changed {
                topic: "commands".into(),
            }),
        ),
        (
            "req_db_command",
            Message::Request(Request::DbCommand {
                id: 40,
                command: "list_terminals".into(),
                args_json: "{}".into(),
            }),
        ),
        // THE PROJECT-IDENTITY WRITES, on the wire. `DbCommand` carries its arguments as an
        // OPAQUE JSON string, so what these two cases pin is not a new message kind but the fact
        // that a payload with a NULL member and a payload with two string members survive the
        // MessagePack round trip byte-for-byte on both sides — `accent: null` is the CLEAR
        // gesture (the absence of an accent has one spelling, `NULL`), and it is exactly the
        // shape a naive encoder is most likely to drop.
        (
            "req_db_command_set_project_accent",
            Message::Request(Request::DbCommand {
                id: 41,
                command: "set_project_accent".into(),
                args_json: "{\"id\":\"proj-1\",\"accent\":\"identity-3\"}".into(),
            }),
        ),
        (
            "req_db_command_set_project_mark_clear",
            Message::Request(Request::DbCommand {
                id: 42,
                command: "set_project_mark".into(),
                args_json: "{\"id\":\"proj-1\",\"icon\":null}".into(),
            }),
        ),
        (
            "res_db_result",
            Message::Response(Response::DbResult {
                id: 40,
                result_json: "[]".into(),
            }),
        ),
    ]
}

fn fixtures_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("contract")
        .join("fixtures.json")
}

#[test]
fn rust_codec_matches_canonical_fixtures() {
    let cases = fixtures();

    if std::env::var("NYX_GEN_FIXTURES").is_ok() {
        let entries: Vec<String> = cases
            .iter()
            .map(|(name, msg)| {
                let env = encode_message(msg);
                let frame = encode_frame(&env).unwrap();
                format!(
                    "    {{ \"name\": \"{}\", \"envelope_hex\": \"{}\", \"frame_hex\": \"{}\" }}",
                    name,
                    hex(&env),
                    hex(&frame)
                )
            })
            .collect();
        let json = format!(
            "{{\n  \"_comment\": \"Canonical wire bytes shared by the Rust (tests/contract.rs) and TS (contract/contract.test.ts) contract tests, task 523. envelope_hex = MessagePack envelope; frame_hex = u32-BE length-prefixed frame. Single source of truth = the Rust codec; regenerate with NYX_GEN_FIXTURES=1 cargo test -p nyx-sidecar --test contract.\",\n  \"protocol_version\": {},\n  \"cases\": [\n{}\n  ]\n}}\n",
            PROTOCOL_VERSION,
            entries.join(",\n")
        );
        std::fs::write(fixtures_path(), json).expect("write fixtures.json");
        eprintln!("wrote {}", fixtures_path().display());
        return;
    }

    let raw = std::fs::read_to_string(fixtures_path())
        .expect("contract/fixtures.json missing — run NYX_GEN_FIXTURES=1 first");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("valid fixtures JSON");
    let arr = doc["cases"].as_array().expect("cases array");
    assert_eq!(arr.len(), cases.len(), "fixture count drift");

    for (name, msg) in &cases {
        let entry = arr
            .iter()
            .find(|e| e["name"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("fixture '{name}' missing from fixtures.json"));
        let want_env = entry["envelope_hex"].as_str().unwrap();
        let want_frame = entry["frame_hex"].as_str().unwrap();

        let env = encode_message(msg);
        let frame = encode_frame(&env).unwrap();
        assert_eq!(hex(&env), want_env, "envelope bytes drift for '{name}'");
        assert_eq!(hex(&frame), want_frame, "frame bytes drift for '{name}'");

        let back = decode_message(&env).unwrap();
        assert_eq!(&back, msg, "decode round-trip mismatch for '{name}'");
    }
}
