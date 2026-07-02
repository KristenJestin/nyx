//! Boot-reconcile integration test (finding #44/#45/#47): the REAL `nyx-sidecar` binary,
//! on `McpStart`, RE-TEMPLATES the installed Claude plugin's `.mcp.json` to the live MCP port
//! and re-registers it — the fix for "plugin enabled but its MCP never connects".
//!
//! The regression this guards: `reconcile_installed_providers` (which re-templates the port +
//! re-registers the plugin) was defined but NEVER wired at boot, so the installed
//! `<data_dir>/claude-plugin/.mcp.json` kept a stale / placeholder port and Claude Code did
//! not connect the nyx MCP. This test drives the SAME path production reaches
//! (`sidecar.start()` → `sidecar.startMcp()` == the `McpStart` request) and asserts the target
//! `.mcp.json` ends up carrying the REAL bound port with no `__NYX_MCP_PORT__` placeholder.
//!
//! Everything is injected via env so NOTHING real is touched:
//!   * `NYX_RECONCILE_CLAUDE_PLUGIN=1` — opt IN the boot reconcile (production sets it too),
//!   * `NYX_MCP_PORT` — a free port so the MCP server binds deterministically,
//!   * `NYX_CLAUDE_PLUGIN_DIR` — the bundled plugin SOURCE (the source tree),
//!   * `NYX_CLAUDE_STABLE_PLUGIN_DIR` — a temp INSTALL dir (the reconcile target),
//!   * `NYX_CLAUDE_BIN` — a fake `claude` whose `marketplace list --json` reports the nyx-v2
//!     marketplace PRESENT (so the conservative reconcile refreshes instead of no-op'ing),
//!   * `NYX_CLAUDE_SETTINGS` — a temp settings file.
//!
//! Gated `#[cfg(unix)]` (the fake CLI is a `/bin/sh` script).
#![cfg(unix)]

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    decode_message, encode_message, Event, Message, Request, Response, PROTOCOL_VERSION,
};

fn sidecar_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

/// A minimal `GET` against the loopback MCP server, returning the raw HTTP response text.
/// Enough to assert the server ANSWERS on the templated port (`/health` → 200 `status: ok`).
fn http_get(port: u16, path: &str) -> String {
    use std::net::TcpStream;
    let addr = format!("127.0.0.1:{port}");
    let mut stream = TcpStream::connect(&addr).expect("connect mcp");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out);
    out
}

/// The bundled plugin SOURCE (the source tree next to nyx-core): the `.mcp.json` carrying the
/// `__NYX_MCP_PORT__` placeholder + the session hooks the reconcile copies + templates.
fn bundled_plugin_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("nyx-core")
        .join("resources")
        .join("claude-plugin")
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-sidecar-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Pick a free loopback TCP port by binding :0 and reading the assigned port, then dropping the
/// listener so the sidecar's MCP server can bind it. (Same seam as `command_mcp.rs`.)
fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Write a fake `claude` shell script whose `plugin marketplace list --json` reports the nyx-v2
/// marketplace PRESENT (registered at the stable INSTALL dir), so the conservative reconcile
/// takes the "present → refresh" branch instead of the "absent → no-op" one. Every other
/// subcommand is a no-op success. Returns the script path.
fn write_fake_claude(dir: &std::path::Path, install_dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("fake-claude.sh");
    // `plugin marketplace list --json` -> a one-entry array naming `nyx-v2` at the install dir
    // (the shape `marketplace_list` parses: name + path). Everything else exits 0.
    let body = format!(
        r#"#!/bin/sh
case "$1 $2 $3" in
  "plugin marketplace list")
    printf '[{{"name":"nyx-v2","path":"{install}"}}]'
    exit 0 ;;
esac
exit 0
"#,
        install = install_dir.display(),
    );
    std::fs::write(&script, body).unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

struct Harness {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Message>,
    next_id: u64,
    data_dir: std::path::PathBuf,
    seam_dir: std::path::PathBuf,
    install_dir: std::path::PathBuf,
    mcp_port: u16,
}

impl Harness {
    fn spawn() -> Self {
        let data_dir = temp_dir("recon-data");
        let seam_dir = temp_dir("recon-seam");
        let install_dir = seam_dir.join("stable-plugin");
        let settings = seam_dir.join("claude-settings.json");
        let fake_claude = write_fake_claude(&seam_dir, &install_dir);
        let mcp_port = pick_free_port();

        let mut child = Command::new(sidecar_bin())
            .env("NYX_DATA_DIR", &data_dir)
            .env("SHELL", "/bin/sh")
            // Opt IN the boot reconcile (production sets this too).
            .env("NYX_RECONCILE_CLAUDE_PLUGIN", "1")
            // A deterministic free MCP port so the bound port is known + assertable.
            .env("NYX_MCP_PORT", mcp_port.to_string())
            // Plugin SOURCE + INSTALL dir + fake CLI + settings seams (no real ~/.claude touched).
            .env("NYX_CLAUDE_PLUGIN_DIR", bundled_plugin_dir())
            .env("NYX_CLAUDE_STABLE_PLUGIN_DIR", &install_dir)
            .env("NYX_CLAUDE_BIN", &fake_claude)
            .env("NYX_CLAUDE_SETTINGS", &settings)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn nyx-sidecar");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("recon-reader".into())
            .spawn(move || {
                let mut dec = FrameDecoder::new();
                let mut buf = [0u8; 64 * 1024];
                loop {
                    loop {
                        match dec.next_frame() {
                            Ok(Some(p)) => {
                                if let Ok(m) = decode_message(&p) {
                                    if tx.send(m).is_err() {
                                        return;
                                    }
                                }
                            }
                            Ok(None) => break,
                            Err(_) => return,
                        }
                    }
                    match stdout.read(&mut buf) {
                        Ok(0) => return,
                        Ok(n) => dec.push(&buf[..n]),
                        Err(_) => return,
                    }
                }
            })
            .unwrap();
        let mut h = Harness {
            child,
            stdin,
            rx,
            next_id: 100,
            data_dir,
            seam_dir,
            install_dir,
            mcp_port,
        };
        match h.recv(Duration::from_secs(5)) {
            Some(Message::Event(Event::Log { .. })) => {}
            other => panic!("expected ready event, got {other:?}"),
        }
        h.handshake();
        h
    }

    fn send(&mut self, req: Request) {
        let frame = encode_frame(&encode_message(&Message::Request(req))).unwrap();
        self.stdin.write_all(&frame).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self, timeout: Duration) -> Option<Message> {
        match self.rx.recv_timeout(timeout) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn recv_until<T>(&mut self, deadline: Duration, mut f: impl FnMut(&Message) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            let remaining = match deadline.checked_sub(start.elapsed()) {
                Some(r) => r,
                None => panic!("timed out"),
            };
            match self.recv(remaining) {
                Some(m) => {
                    if let Some(v) = f(&m) {
                        return v;
                    }
                }
                None => panic!("timed out waiting for a matching message"),
            }
        }
    }

    fn handshake(&mut self) {
        self.send(Request::Handshake {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
        });
        self.recv_until(Duration::from_secs(5), |m| {
            matches!(m, Message::Response(Response::HandshakeOk { .. })).then_some(())
        });
    }

    fn start_mcp(&mut self) -> u16 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(Request::McpStart { id });
        self.recv_until(Duration::from_secs(10), |m| match m {
            Message::Response(Response::McpStarted { id: rid, port }) if *rid == id => Some(*port),
            _ => None,
        })
    }

    fn shutdown(mut self) {
        self.send(Request::Shutdown { id: 9999 });
        self.recv_until(Duration::from_secs(5), |m| {
            matches!(m, Message::Response(Response::ShutdownAck { .. })).then_some(())
        });
        let start = Instant::now();
        loop {
            if self.child.try_wait().expect("try_wait").is_some() {
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                let _ = self.child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir_all(&self.data_dir).ok();
        std::fs::remove_dir_all(&self.seam_dir).ok();
    }
}

/// On `McpStart` (the request production sends via `sidecar.startMcp()`), the sidecar binds the
/// MCP server AND reconciles the (already-registered) Claude plugin: it copies the bundled
/// plugin into the stable install dir and re-templates `.mcp.json` to the REAL bound port —
/// proving the fix for "plugin enabled but MCP not connected".
#[test]
fn mcp_start_reconciles_and_templates_the_installed_mcp_port() {
    let mut h = Harness::spawn();

    // Sanity: the reconcile has not run yet (no McpStart), so nothing is copied.
    assert!(
        !h.install_dir.join(".mcp.json").exists(),
        "the stable install dir is empty before McpStart (no premature reconcile)"
    );

    // START the MCP (the exact request `sidecar.startMcp()` sends) → binds the port AND fires
    // the boot reconcile against the fake registry (nyx-v2 present → refresh).
    let bound = h.start_mcp();
    assert_eq!(
        bound, h.mcp_port,
        "the MCP server bound the requested NYX_MCP_PORT"
    );

    // The reconcile copied the bundled plugin into the stable install dir and TEMPLATED the
    // live port into the copied `.mcp.json` (the finding-#44 substitution the plugin registers).
    let mcp_json = h.install_dir.join(".mcp.json");
    assert!(
        mcp_json.exists(),
        "the reconcile copied the plugin's .mcp.json into the stable install dir"
    );
    let templated = std::fs::read_to_string(&mcp_json).unwrap();
    assert!(
        templated.contains(&format!("127.0.0.1:{}/mcp", h.mcp_port)),
        "the installed .mcp.json points at the REAL bound MCP port: {templated}"
    );
    assert!(
        !templated.contains("__NYX_MCP_PORT__"),
        "the placeholder is fully substituted (no stale token): {templated}"
    );

    // The hooks were copied too (the plugin bundles MCP + hooks as one unit).
    assert!(
        h.install_dir.join("hooks").join("hooks.json").exists(),
        "the session hooks were copied alongside the MCP descriptor"
    );

    // END-TO-END PROOF: the MCP server actually ANSWERS on the port the reconcile wrote into
    // `.mcp.json` — so a client reading that descriptor's `http://127.0.0.1:<port>/mcp` reaches
    // a live server (the whole point: the plugin's declared MCP now connects).
    let health = http_get(h.mcp_port, "/health");
    assert!(
        health.starts_with("HTTP/1.1 200"),
        "the MCP /health endpoint answers on the templated port with 200: {health}"
    );
    assert!(
        health.contains("\"status\":\"ok\""),
        "the MCP /health body reports ok on the templated port: {health}"
    );

    // A SECOND McpStart is start-once (same bound port) and must not re-drive the reconcile into
    // a broken state — the templated port stays correct.
    let bound_again = h.start_mcp();
    assert_eq!(bound_again, h.mcp_port, "start-once returns the same port");
    let still = std::fs::read_to_string(&mcp_json).unwrap();
    assert!(
        still.contains(&format!("127.0.0.1:{}/mcp", h.mcp_port)),
        "the installed port is stable across a repeated McpStart"
    );

    h.shutdown();
}
