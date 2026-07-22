//! v2.6 VALIDATION GATE — end-to-end smoke of the DURABLE-MEMORY parcours
//! (task `01KXZ9K5PN6N6T60VZ3DCXJ9SY`, PRD `01KX8X0F7SFA9C4AHEVNX0CNPV`, Phase 5).
//!
//! This is the executable half of the gate: it proves nyx's native durable memory works
//! END TO END — "mémoire durable nyx sans second système" — by driving the ONE cohesive
//! real-life scenario the PRD's done-criteria call out through the SAME surfaces the agents
//! and the UI use: the **MCP `tools/call` HTTP server** (the agent surface — the exact path a
//! Claude plugin hook reaches) and the **UI-bridge `db_command`** over the framed pipe (the
//! renderer surface). Both resolve from the sidecar's ONE shared `nyx.db` pool (PDR-0005), so
//! this run certifies that the wired stack — not just the DB layer — carries the whole story.
//!
//! It walks, in ONE run, the six gate steps of the done-criteria
//! ("capture idée → Spec liée en dev (planned) → complétion (implemented) ; dédup ;
//! idea_search dans /nspec remonte l'existant ; annexe dédupliquée ; dépendance Spec affichée"):
//!
//!   1. **CAPTURE** — `idea_add` (MCP) captures a follow-up as an `open` idea.
//!   2. **LIFECYCLE piloté par la Spec liée (kind=addresses)** — `spec_create` + `idea_link`
//!      (addresses), then `spec_transition start` flips the idea `open → planned`, and
//!      `spec_transition done` closes it `implemented` with the motif "implémentée via Spec #N";
//!      a bonus `cancel` leg reopens a `planned` idea back to `open` with the fermeture trail
//!      cleared. All observed through `idea_get` (MCP) — the propagation is driven by the REAL
//!      transition tool, never a direct DB poke.
//!   3. **DÉDUP / FUSION** — two near-duplicate ideas, `idea_merge` (MCP): one survives `open`,
//!      the bodies (points) are preserved, the source's Spec link is carried over, the source is
//!      retired `dropped`.
//!   4. **/nspec SURFAÇAGE** — `idea_search` (MCP) for the topic returns the surviving idea, AND
//!      the `/nspec` role render `agent_context(role=spec)` surfaces it in the "search the backlog
//!      before you spec" preview — proving the author would FIND the tracked need instead of
//!      re-creating a Spec. Cross-checked over the UI `db_command idea_search` (same authority).
//!   5. **ANNEXE DÉDUPLIQUÉE** — the SAME content attached twice to a Spec (`annexe_add`, MCP)
//!      yields two attachment rows sharing ONE `content_hash` (surface proof of content-hash
//!      dedup), and — re-opening the persisted `nyx.db` after a clean shutdown — exactly ONE
//!      physical blob (the durable single-copy clincher, à la `gate_v2_1_identity_e2e`).
//!   6. **DÉPENDANCE Spec AFFICHÉE** — `spec_link` (MCP) then `spec_deps` (MCP) and the UI
//!      `spec_dependency_view` (db_command) both report the "bloquée par N" / "débloque M" badge
//!      counts of the linked pair.
//!
//! WHY THE REAL MCP PIPE+HTTP (not the in-process tool fns): the knowledge write tools
//! (`idea_add`/`idea_merge`/`idea_link`/`annexe_add`/`spec_link`/`spec_transition`) are the AGENT
//! surface, reached over the sidecar's loopback MCP HTTP server (`tools/call`); the read models
//! the UI paints (`idea_search`/`idea_themes`/`annexe_list`/`spec_dependency_view`) are the
//! db_command surface. Driving BOTH here — and asserting they AGREE on the same rows — is the
//! "UI et MCP sur la même autorité, no second system" proof the gate exists for: a broken
//! dispatch arm (the phase-4 transport-wiring bug class) fails this gate.
//!
//! The single db re-open at the end (the blob-count clincher of step 5) mirrors
//! `gate_v2_1_identity_e2e`'s re-open-to-read: the content-hash DEDUP is proven through the
//! surface (`annexe_list` → two rows, one hash); the single PHYSICAL blob is a durable-state
//! read of the persisted file, the one place no MCP/db_command surface exposes.
//!
//! Gated `#![cfg(not(windows))]` for parity with the sibling gate harnesses (it spawns the real
//! sidecar binary + drives its loopback HTTP); GREEN on Linux.
#![cfg(not(windows))]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    decode_message, encode_message, Event, Message, Request, Response, PROTOCOL_VERSION,
};
use serde_json::{json, Value};

fn sidecar_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

/// A fresh, unique temp dir (each run isolates its own `nyx.db`).
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-gate26-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Pick a likely-free fixed port for the in-test MCP server. The nyx MCP server rejects the
/// ephemeral port 0 (it must advertise a stable port), so we bind an OS-assigned free port on a
/// throwaway listener and release it immediately so the sidecar can re-claim it.
fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// A pipe+HTTP harness over the REAL sidecar binary: a background reader thread drains the framed
/// stdout into a channel of decoded `Message`s (for `db_command`/`McpStart`), and the MCP tools
/// are driven over the loopback HTTP server the sidecar starts — the EXACT agent + UI surfaces.
struct Harness {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Message>,
    next_id: u64,
    /// The loopback port the in-sidecar MCP HTTP server bound (0 until [`Self::start_mcp`]).
    mcp_port: u16,
}

impl Harness {
    fn spawn(data_dir: PathBuf, mcp_port: u16) -> Self {
        let mut cmd = Command::new(sidecar_bin());
        cmd.env("NYX_DATA_DIR", &data_dir)
            .env("SHELL", "/bin/sh")
            .env("NYX_MCP_PORT", mcp_port.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn().expect("spawn nyx-sidecar");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
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
        });

        let mut h = Harness {
            child,
            stdin,
            rx,
            next_id: 100,
            mcp_port: 0,
        };
        // Drain the unsolicited `ready` Log event — the sidecar has booted past the single-file
        // DB open + migrate and is serving the pipe.
        match h.recv(Duration::from_secs(10)) {
            Some(Message::Event(Event::Log { .. })) => {}
            other => panic!("expected ready event, got {other:?}"),
        }
        h.handshake();
        h
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn send(&mut self, req: Request) {
        let frame = encode_frame(&encode_message(&Message::Request(req))).unwrap();
        self.stdin.write_all(&frame).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self, timeout: Duration) -> Option<Message> {
        self.rx.recv_timeout(timeout).ok()
    }

    fn recv_until<T>(&mut self, deadline: Duration, mut f: impl FnMut(&Message) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            let remaining = deadline
                .checked_sub(start.elapsed())
                .expect("timed out waiting for a matching message");
            match self.recv(remaining) {
                Some(msg) => {
                    if let Some(v) = f(&msg) {
                        return v;
                    }
                }
                None => panic!("timed out waiting for a matching message"),
            }
        }
    }

    fn handshake(&mut self) -> Vec<String> {
        let id = self.id();
        self.send(Request::Handshake {
            id,
            protocol_version: PROTOCOL_VERSION,
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::HandshakeOk { capabilities, .. }) => {
                Some(capabilities.clone())
            }
            _ => None,
        })
    }

    /// Start the in-sidecar MCP HTTP server over the pipe; latch the bound loopback port.
    fn start_mcp(&mut self) {
        let id = self.id();
        self.send(Request::McpStart { id });
        let port = self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::McpStarted { port, .. }) => Some(*port),
            _ => None,
        });
        assert_ne!(
            port, 0,
            "the MCP server bound a real port (0 ⇒ bind failed)"
        );
        self.mcp_port = port;
    }

    /// Drive a UI-bridge `db_command` over the pipe and return its parsed JSON result — the EXACT
    /// path the renderer's `nyxBridge.invoke(...)` reaches. Panics on a `Response::Error`.
    fn db_command(&mut self, command: &str, args: Value) -> Value {
        let id = self.id();
        self.send(Request::DbCommand {
            id,
            command: command.to_string(),
            args_json: args.to_string(),
        });
        self.recv_until(Duration::from_secs(10), |m| match m {
            Message::Response(Response::DbResult {
                id: rid,
                result_json,
            }) if *rid == id => Some(serde_json::from_str(result_json).unwrap()),
            Message::Response(Response::Error {
                id: rid, message, ..
            }) if *rid == id => panic!("db_command {command} errored over the pipe: {message}"),
            _ => None,
        })
    }

    /// Call an MCP tool over the REAL loopback HTTP server exactly as an agent's plugin does — a
    /// JSON-RPC `tools/call` POSTed to `/mcp` — and return the tool's `structuredContent` (the JSON
    /// the tool produced). Panics on a JSON-RPC error or an `isError` result.
    fn mcp_call(&mut self, name: &str, args: Value) -> Value {
        let id = self.id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        });
        let v = self.mcp_post(&req.to_string());
        if let Some(err) = v.get("error") {
            panic!("mcp tools/call {name} errored: {err}");
        }
        assert_eq!(
            v["result"]["isError"],
            json!(false),
            "mcp tools/call {name} isError: {v}"
        );
        v["result"]["structuredContent"].clone()
    }

    /// Call an MCP tool expecting a JSON-RPC ERROR, and return its `error.data.code` string (the
    /// ADR-0003 D8 machine code, e.g. `invalid_state` / `invalid_id`). Panics if the call SUCCEEDS.
    fn mcp_call_err(&mut self, name: &str, args: Value) -> String {
        let id = self.id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        });
        let v = self.mcp_post(&req.to_string());
        v["error"]["data"]["code"]
            .as_str()
            .unwrap_or_else(|| panic!("mcp tools/call {name} expected an error, got: {v}"))
            .to_string()
    }

    /// POST a JSON-RPC body to the sidecar's `/mcp` over a raw loopback socket (no http-client dep),
    /// parse the response body as JSON. `Response::from_string` (tiny_http) sets Content-Length, so
    /// the body is the tail after the `\r\n\r\n` header terminator.
    fn mcp_post(&self, body: &str) -> Value {
        assert_ne!(self.mcp_port, 0, "start_mcp() before mcp_post()");
        let addr = format!("127.0.0.1:{}", self.mcp_port);
        let mut stream = TcpStream::connect(&addr).expect("connect mcp");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        let _ = stream.read_to_string(&mut raw);
        let json_body = raw
            .split_once("\r\n\r\n")
            .map(|(_, b)| b)
            .unwrap_or(&raw)
            .trim();
        serde_json::from_str(json_body)
            .unwrap_or_else(|e| panic!("mcp response body is not JSON ({e}); raw: {raw:?}"))
    }

    fn shutdown(&mut self) {
        let id = self.id();
        self.send(Request::Shutdown { id });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::ShutdownAck { .. }) => Some(()),
            _ => None,
        });
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                assert!(status.success(), "clean shutdown exits 0, got {status:?}");
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                let _ = self.child.kill();
                panic!("sidecar did not exit after shutdown");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Sensitive Spec transitions (ready/start/done/cancel) demand explicit human confirmation — the
/// gate confirms as the user would. Small helper so the scenario reads as the intent, not the flag.
fn confirmed(mut args: Value) -> Value {
    args["user_confirmed"] = json!(true);
    args
}

/// THE durable-memory parcours smoke: capture → Spec-driven lifecycle → dedup → /nspec surfacing →
/// annexe dedup → Spec dependency, all over the REAL MCP + db_command surfaces on the spawned
/// sidecar, then a re-open of the SAME `nyx.db` for the single-blob clincher.
#[test]
fn durable_memory_parcours_end_to_end_over_the_real_sidecar() {
    let data_dir = temp_dir("data");
    let root = temp_dir("root");
    let mcp_port = pick_free_port();

    let mut h = Harness::spawn(data_dir.clone(), mcp_port);
    // The MCP + commands capabilities are the two surfaces this gate exercises.
    let caps = h.handshake();
    assert!(caps.iter().any(|c| c == "mcp"), "mcp capability advertised");

    // A project (+ its root workspace) over the UI bridge — the same `create_project` use case the
    // MCP surface funnels into. The root workspace is the dev anchor the `start` transition poses.
    let created = h.db_command(
        "create_project",
        json!({ "name": "GateKnowledge", "rootPath": root.to_str().unwrap() }),
    );
    let project_id = created["project"]["id"].as_str().unwrap().to_string();
    let root_ws_id = created["root"]["id"].as_str().unwrap().to_string();

    // Start the in-sidecar MCP HTTP server (the agent surface for the knowledge write tools).
    h.start_mcp();

    // ================= STEP 1 — CAPTURE (idea_add over MCP) =========================
    let captured = h.mcp_call(
        "idea_add",
        json!({
            "project_id": project_id,
            "title": "Rework the empty-password login branch",
            "body": "the empty-password branch is fragile and untested",
            "theme": "auth",
        }),
    );
    let idea_login = captured["idea"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        captured["idea"]["status"], "open",
        "STEP 1: a captured idea enters the backlog `open`"
    );

    // ================= STEP 2 — LIFECYCLE piloté par la Spec liée ===================
    // Author Spec A (problem+solution so it can pass the ready gate) and link the idea as
    // `addresses` — the kind the auto lifecycle keys on.
    let spec_a = h.mcp_call(
        "spec_create",
        json!({
            "project_id": project_id,
            "title": "Login hardening",
            "problem": "the empty-password branch is fragile",
            "solution": "reject empty passwords + add coverage",
        }),
    );
    let spec_a_id = spec_a["spec"]["id"].as_str().unwrap().to_string();
    let spec_a_number = spec_a["spec"]["number"].as_i64().unwrap();
    h.mcp_call(
        "idea_link",
        json!({ "idea_id": idea_login, "spec_id": spec_a_id, "kind": "addresses" }),
    );

    // ready → start (in_progress, anchored on the dev workspace): the idea leaves the open backlog
    // as `planned` — driven purely by the REAL `spec_transition` tool.
    h.mcp_call(
        "spec_transition",
        confirmed(json!({ "spec_id": spec_a_id, "transition": "ready" })),
    );
    let started = h.mcp_call(
        "spec_transition",
        confirmed(
            json!({ "spec_id": spec_a_id, "transition": "start", "workspace_id": root_ws_id }),
        ),
    );
    assert_eq!(
        started["spec"]["status"], "in_progress",
        "STEP 2: the Spec is in dev"
    );
    let planned = h.mcp_call("idea_get", json!({ "id": idea_login }));
    assert_eq!(
        planned["idea"]["status"], "planned",
        "STEP 2: a linked Spec going in_progress moves its addressed idea `open → planned`"
    );
    assert!(
        planned["idea"]["closed_at"].is_null() && planned["idea"]["closed_reason"].is_null(),
        "STEP 2: `planned` is a LIVE state — no fermeture trail stamped"
    );

    // review → done (completed): the idea is closed `implemented` with the closure motif.
    h.mcp_call(
        "spec_transition",
        json!({ "spec_id": spec_a_id, "transition": "review" }),
    );
    let done = h.mcp_call(
        "spec_transition",
        confirmed(json!({ "spec_id": spec_a_id, "transition": "done" })),
    );
    assert_eq!(
        done["spec"]["status"], "completed",
        "STEP 2: the Spec completed"
    );
    let implemented = h.mcp_call("idea_get", json!({ "id": idea_login }));
    assert_eq!(
        implemented["idea"]["status"], "implemented",
        "STEP 2: the completed Spec closes its addressed idea `implemented`"
    );
    assert_eq!(
        implemented["idea"]["closed_reason"],
        json!(format!("implémentée via Spec #{spec_a_number}")),
        "STEP 2: the closure motif names the resolving Spec"
    );

    // BONUS — cancel leg: a `planned` idea reopens to `open` (not dropped), trail cleared.
    let cancel_idea = h.mcp_call(
        "idea_add",
        json!({ "project_id": project_id, "title": "Maybe throttle logins", "theme": "auth" }),
    );
    let cancel_idea_id = cancel_idea["idea"]["id"].as_str().unwrap().to_string();
    let spec_cancel = h.mcp_call(
        "spec_create",
        json!({
            "project_id": project_id,
            "title": "Login throttling (maybe)",
            "problem": "brute force is possible",
            "solution": "rate-limit auth attempts",
        }),
    );
    let spec_cancel_id = spec_cancel["spec"]["id"].as_str().unwrap().to_string();
    h.mcp_call(
        "idea_link",
        json!({ "idea_id": cancel_idea_id, "spec_id": spec_cancel_id, "kind": "addresses" }),
    );
    h.mcp_call(
        "spec_transition",
        confirmed(json!({ "spec_id": spec_cancel_id, "transition": "ready" })),
    );
    h.mcp_call(
        "spec_transition",
        confirmed(
            json!({ "spec_id": spec_cancel_id, "transition": "start", "workspace_id": root_ws_id }),
        ),
    );
    h.mcp_call(
        "spec_transition",
        confirmed(json!({ "spec_id": spec_cancel_id, "transition": "cancel" })),
    );
    let reopened = h.mcp_call("idea_get", json!({ "id": cancel_idea_id }));
    assert_eq!(
        reopened["idea"]["status"], "open",
        "STEP 2 (bonus): cancelling the linked Spec reopens the idea to `open`, not `dropped`"
    );
    assert!(
        reopened["idea"]["closed_at"].is_null() && reopened["idea"]["closed_reason"].is_null(),
        "STEP 2 (bonus): the reopen path clears the fermeture trail"
    );

    // ================= STEP 2b — MANUAL close vs AUTO implement + guards + reconcile ==========
    // A MANUAL fermeture-par-constat lands the DISTINCT `closed` status (v29), NOT `implemented`.
    let constat = h.mcp_call(
        "idea_add",
        json!({ "project_id": project_id, "title": "Dup of the login rework", "theme": "auth" }),
    );
    let constat_id = constat["idea"]["id"].as_str().unwrap().to_string();
    let closed = h.mcp_call(
        "idea_close",
        json!({ "id": constat_id, "reason": "déjà couvert par la Spec login" }),
    );
    assert_eq!(
        closed["idea"]["status"], "closed",
        "STEP 2b: manual close → the DISTINCT `closed`, not `implemented`"
    );
    assert_eq!(
        closed["idea"]["closed_reason"], "déjà couvert par la Spec login",
        "STEP 2b: the motif is recorded"
    );
    // TERMINAL GUARD: re-implementing a closed idea is refused (invalid_state) — no resurrection,
    // and the recorded motif is preserved.
    assert_eq!(
        h.mcp_call_err("idea_implement", json!({ "id": constat_id })),
        "invalid_state",
        "STEP 2b: a terminal idea rejects a further transition"
    );
    let still_closed = h.mcp_call("idea_get", json!({ "id": constat_id }));
    assert_eq!(still_closed["idea"]["status"], "closed");
    assert_eq!(
        still_closed["idea"]["closed_reason"], "déjà couvert par la Spec login",
        "STEP 2b: the guard preserved the close motif (never overwritten)"
    );
    // idea_reopen is the ONE explicit way back to the backlog — trail cleared.
    let reborn = h.mcp_call("idea_reopen", json!({ "id": constat_id }));
    assert_eq!(reborn["idea"]["status"], "open", "STEP 2b: reopen → open");
    assert!(
        reborn["idea"]["closed_at"].is_null() && reborn["idea"]["closed_reason"].is_null(),
        "STEP 2b: reopen clears the fermeture trail"
    );
    // RECONCILE-ON-LINK: linking `addresses` to the ALREADY-completed Spec A reconciles the fresh
    // idea straight to `implemented` (+motif) at LINK time — `planned` is reachable the same way.
    let late = h.mcp_call(
        "idea_add",
        json!({ "project_id": project_id, "title": "Late follow-up on login", "theme": "auth" }),
    );
    let late_id = late["idea"]["id"].as_str().unwrap().to_string();
    h.mcp_call(
        "idea_link",
        json!({ "idea_id": late_id, "spec_id": spec_a_id, "kind": "addresses" }),
    );
    let reconciled = h.mcp_call("idea_get", json!({ "id": late_id }));
    assert_eq!(
        reconciled["idea"]["status"], "implemented",
        "STEP 2b: linking addresses to an already-completed Spec reconciles →implemented at link time"
    );
    assert_eq!(
        reconciled["idea"]["closed_reason"],
        json!(format!("implémentée via Spec #{spec_a_number}")),
        "STEP 2b: the reconcile stamps the resolving-Spec motif"
    );

    // ================= STEP 3 — DÉDUP / FUSION (idea_merge over MCP) ================
    let target = h.mcp_call(
        "idea_add",
        json!({
            "project_id": project_id,
            "title": "Speed up idea search",
            "body": "point ONE: index the title",
            "theme": "search",
        }),
    );
    let target_id = target["idea"]["id"].as_str().unwrap().to_string();
    let source = h.mcp_call(
        "idea_add",
        json!({
            "project_id": project_id,
            "title": "Faster idea search",
            "body": "point TWO: index the body too",
            "theme": "search",
        }),
    );
    let source_id = source["idea"]["id"].as_str().unwrap().to_string();
    // Give the SOURCE a Spec link so the merge must carry it over onto the survivor.
    h.mcp_call(
        "idea_link",
        json!({ "idea_id": source_id, "spec_id": spec_a_id, "kind": "related" }),
    );
    let merged = h.mcp_call(
        "idea_merge",
        json!({ "source_ids": [source_id], "target_id": target_id }),
    );
    assert_eq!(
        merged["idea"]["status"], "open",
        "STEP 3: the surviving idea stays `open` in the backlog"
    );
    let survivor_body = merged["idea"]["body"].as_str().unwrap();
    assert!(
        survivor_body.contains("point ONE") && survivor_body.contains("point TWO"),
        "STEP 3: the merge preserves the points of BOTH ideas: {survivor_body:?}"
    );
    let carried: Vec<&str> = merged["linked_specs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["spec_id"].as_str().unwrap())
        .collect();
    assert!(
        carried.contains(&spec_a_id.as_str()),
        "STEP 3: the source's Spec link is carried onto the survivor: {carried:?}"
    );
    let dropped = h.mcp_call("idea_get", json!({ "id": source_id }));
    assert_eq!(
        dropped["idea"]["status"], "dropped",
        "STEP 3: the merged-away source is retired as a `dropped` tombstone"
    );

    // ================= STEP 4 — /nspec SURFAÇAGE ===================================
    // idea_search (MCP) returns the surviving idea for the topic — the agent FINDS it.
    let search = h.mcp_call(
        "idea_search",
        json!({ "project_id": project_id, "query": "idea search speed" }),
    );
    let hits: Vec<&str> = search["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert!(
        hits.contains(&target_id.as_str()),
        "STEP 4: idea_search surfaces the existing idea for the topic: {hits:?}"
    );
    let top = search["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(target_id))
        .unwrap();
    assert!(
        top["score"].as_f64().unwrap() > 0.0 && !top["extract"].as_str().unwrap().is_empty(),
        "STEP 4: the hit carries a score + a match extract (a scored surfacing, not a flat dump)"
    );
    // The /nspec role render (agent_context role=spec) surfaces the backlog + the idea_search
    // instruction — so the author sees the tracked need up front and does NOT re-create a Spec.
    let nspec = h.mcp_call(
        "agent_context",
        json!({ "role": "spec", "project_id": project_id }),
    );
    let context = nspec["context"].as_str().unwrap();
    assert!(
        context.contains("idea_search") && context.contains("before you spec"),
        "STEP 4: /nspec instructs the author to idea_search the backlog before spec'ing"
    );
    assert!(
        context.contains("Speed up idea search"),
        "STEP 4: /nspec surfaces the tracked idea in its anti-duplication backlog preview"
    );
    // Same authority over the UI surface: the db_command idea_search returns the SAME survivor.
    let ui_search = h.db_command(
        "idea_search",
        json!({ "projectId": project_id, "query": "idea search speed" }),
    );
    let ui_hits: Vec<&str> = ui_search
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert!(
        ui_hits.contains(&target_id.as_str()),
        "STEP 4: the UI db_command idea_search sees the SAME idea (UI + MCP, one authority)"
    );

    // ================= STEP 5 — ANNEXE DÉDUPLIQUÉE (annexe_add over MCP) ============
    const ANNEXE_CONTENT: &str = "the shared prototype HTML bytes — identical on purpose";
    let a1 = h.mcp_call(
        "annexe_add",
        json!({ "spec_id": spec_a_id, "kind": "prototype", "title": "L1 prototype", "content": ANNEXE_CONTENT }),
    );
    let a2 = h.mcp_call(
        "annexe_add",
        json!({ "spec_id": spec_a_id, "kind": "doc", "title": "same bytes as a doc", "content": ANNEXE_CONTENT }),
    );
    assert_eq!(
        a1["annexe"]["content_hash"], a2["annexe"]["content_hash"],
        "STEP 5: identical content ⇒ identical content_hash (content-addressed dedup)"
    );
    // Both surfaces list the two attachment rows sharing the one hash.
    let mcp_annexes = h.mcp_call("annexe_list", json!({ "spec_id": spec_a_id }));
    assert_eq!(
        mcp_annexes["annexes"].as_array().unwrap().len(),
        2,
        "STEP 5: two attachment rows on the Spec (MCP annexe_list)"
    );
    let ui_annexes = h.db_command("annexe_list", json!({ "specId": spec_a_id }));
    let ui_rows = ui_annexes.as_array().unwrap();
    assert_eq!(
        ui_rows.len(),
        2,
        "STEP 5: the UI db_command annexe_list sees the SAME two rows (UI + MCP, one authority)"
    );
    assert_eq!(
        ui_rows[0]["content_hash"], ui_rows[1]["content_hash"],
        "STEP 5: both attachments share ONE content_hash on the UI surface too"
    );

    // ================= STEP 6 — DÉPENDANCE Spec AFFICHÉE (spec_link over MCP) =======
    // Spec B, then link A --blocks--> B (A depends on / comes after B). So A is "bloquée par" B and
    // B "débloque" A — the exact badges the board + Spec-detail render.
    let spec_b = h.mcp_call(
        "spec_create",
        json!({
            "project_id": project_id,
            "title": "Session store refactor",
            "problem": "sessions are in-memory",
            "solution": "move to a durable store",
        }),
    );
    let spec_b_id = spec_b["spec"]["id"].as_str().unwrap().to_string();
    h.mcp_call(
        "spec_link",
        json!({ "from_spec_id": spec_a_id, "to_spec_id": spec_b_id, "kind": "blocks", "note": "A ships on B" }),
    );
    // spec_deps (MCP) for A: blocked_by B (count 1), unblocks nothing.
    let deps_a = h.mcp_call("spec_deps", json!({ "spec_id": spec_a_id }));
    assert_eq!(
        deps_a["blocked_by_count"], 1,
        "STEP 6: Spec A is `bloquée par` 1 (Spec B)"
    );
    assert_eq!(deps_a["unblocks_count"], 0, "STEP 6: Spec A blocks nothing");
    assert_eq!(
        deps_a["blocked_by"][0]["spec_id"], spec_b_id,
        "STEP 6: A's blocker is Spec B"
    );
    // spec_deps (MCP) for B: unblocks A (count 1), blocked by nothing.
    let deps_b = h.mcp_call("spec_deps", json!({ "spec_id": spec_b_id }));
    assert_eq!(
        deps_b["unblocks_count"], 1,
        "STEP 6: Spec B `débloque` 1 (Spec A)"
    );
    assert_eq!(
        deps_b["blocked_by_count"], 0,
        "STEP 6: Spec B has no blocker"
    );
    // Same authority over the UI surface: the db_command dependency view matches the badges.
    let ui_view_a = h.db_command("spec_dependency_view", json!({ "specId": spec_a_id }));
    assert_eq!(
        ui_view_a["blocked_by_count"], 1,
        "STEP 6: the UI db_command spec_dependency_view shows the SAME `bloquée par` badge (UI + MCP, one authority)"
    );
    assert_eq!(
        ui_view_a["blocked_by"][0]["spec_id"], spec_b_id,
        "STEP 6: the UI view names Spec B as A's blocker"
    );

    // Clean shutdown BEFORE the durable re-open (the persisted state is the assertion).
    h.shutdown();

    // ---- STEP 5 CLINCHER: re-open the SAME nyx.db and prove ONE physical blob for the two
    //      identical attachments (the content-addressed store never stores a second copy). This is
    //      a durable-state read of the persisted file — the one fact no MCP/db_command surface
    //      exposes — à la `gate_v2_1_identity_e2e`'s re-open-to-read.
    let db_file = data_dir.join("nyx.db");
    assert!(
        db_file.is_file(),
        "exactly one SQLite authority file at <data_dir>/nyx.db"
    );
    let db = nyx_core::db::Db::open(&db_file).expect("re-open the single nyx.db");
    // The content-addressed store lives in `annex_blobs` (one row per distinct content). Counting it
    // over a raw SQL read of the persisted file is the durable single-copy proof (`annex_blob_count`
    // is a `#[cfg(test)]` helper in nyx-core, not linked into this integration crate).
    #[derive(diesel::QueryableByName)]
    struct BlobCount {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        count: i64,
    }
    let blob_count = db
        .with_conn(|c| {
            use diesel::RunQueryDsl;
            diesel::sql_query("SELECT COUNT(*) AS count FROM annex_blobs")
                .get_result::<BlobCount>(c)
                .map(|r| r.count)
        })
        .expect("count annexe blobs");
    assert_eq!(
        blob_count, 1,
        "STEP 5: two identical attachments persist as ONE physical blob (content-hash dedup)"
    );

    std::fs::remove_dir_all(&data_dir).ok();
    std::fs::remove_dir_all(&root).ok();
}
