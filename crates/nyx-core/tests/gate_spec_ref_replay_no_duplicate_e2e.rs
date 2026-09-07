//! GATE e2e — « rejouer `/nspec 3` sur une Spec qui existe : zéro doublon »
//! (PRD `01M08Y8AW9A9F68SXB4MC03W3C`, phase 4, task `01M0901Z4YZ1XK174KDVAS9SV9`).
//!
//! THE INCIDENT, REPLAYED. A project holding a Spec `#3`, a human typing `/nspec 3 …` to pick that
//! work back up, and the reference reaching nobody: the hook recognised only the canonical id, so it
//! forwarded the `cwd` alone; the `cwd` anchored no Spec; the /nspec context came back with no Spec
//! in it; and the session dutifully authored a DUPLICATE of the very Spec the human had named
//! (session `01a011d0`).
//!
//! THE DOOR CHANGED, THE TRAP DID NOT (PDR-0031). The chain used to end at `run_start`, which opened
//! a run bound to the Spec the reference designated and refused the references it could not resolve.
//! The behaviour-tree engine is withdrawn; the resolution AND the refusal now belong to
//! [`nyx_core::mcp_context::agent_context`], the call that renders the command's manual. The trap is
//! word for word the one `/nspec`'s manual warns about — *never author a new one to get past a
//! refused reference* — so this gate keeps its subject and changes its bricks:
//!
//!   * the REAL hook — `resources/claude-plugin/hooks/nyx-session-hook.cjs`, spawned under `node`
//!     with the `UserPromptSubmit` payload Claude Code actually sends (the prompt the human TYPED,
//!     never an expanded command body);
//!   * the REAL [`nyx_core::mcp_context::agent_context`], over an ON-DISK `Db::open` — served to the
//!     hook over loopback HTTP/JSON-RPC by [`RealSidecar`], so the two halves meet on the wire
//!     exactly as they do in the product;
//!   * the REAL surviving Spec verbs — `spec_list` (the ORACLE reads the project through the tool an
//!     agent reads it through), `spec_create` (the duplicate, when it is authored, is minted by the
//!     verb a session would actually call) and `idea_add` (the backlog the manual surfaces).
//!
//! Its two harnesses are COPIED from the ones this crate already has, not shared with them: the
//! HTTP / `node` plumbing is `hook_push_injection_e2e.rs`'s (a mock sidecar there, the real verbs
//! here) and the DB scaffolding — an RAII temp dir, an on-disk `Db`, tools driven through their real
//! entry points — is `gate_spec_anchor_resolution_e2e.rs`'s, and both were re-typed here. Each file
//! under `tests/` is its OWN crate, so sharing would mean a `tests/common/` module this crate does
//! not have. The cost is stated rather than dressed up as reuse: a fix to either original does not
//! reach this copy.
//!
//! # THE ORACLE, AND WHAT MAKES IT BITE
//!
//! The assertion that carries this whole PRD is one line: THE PROJECT HOLDS THE SAME SPECS BEFORE
//! AND AFTER. An oracle nothing can turn red proves nothing, though, and nyx authors no Spec by
//! itself — the SESSION does, on the branch the injected text sends it down. So the gate replays
//! that ONE decision, and only it, in
//! [`an_nspec_session_authors_when_the_manual_says_there_is_no_spec`]: when what nyx showed the
//! session is a /nspec manual whose Spec block reads « none yet — you are authoring a NEW Spec »,
//! the modelled session authors. With it in place, ANY break in the chain — the hook dropping the
//! typed token, `agent_context` failing to resolve `3`, the render falling back on the Spec-less
//! `cwd` — lands a second Spec in the project and the oracle names it.
//!
//! It bites on the REFUSAL leg too, which is why the refused-reference block is not merely tolerated
//! there but REQUIRED: a `/nspec 99` that was quietly rendered off the `cwd` instead of being
//! refused would show the session that same "author a new one" manual, and the duplicate would land.
//!
//! # WHAT THIS GATE DELIBERATELY DOES NOT DO (the task's non-goals)
//!
//! It depends on NO Claude binary and NO LLM — nothing here reaches a model. And it freezes NO
//! wording of nyx's own: a refusal is asserted by its typed [`RpcCode`] and by the VALUES it hands
//! back (the accepted forms, read from the grammar's own constant; the id of the nearest candidate
//! Spec), never by a sentence — and the block the hook injects in a refused manual's place by what it
//! CARRIES (the server's own sentence, relayed) rather than by its own prose.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use nyx_core::db::{self, Db};
use nyx_core::mcp::{RpcCode, RpcError};
use nyx_core::mcp_runtime::dispatch_workspace_tool;
use nyx_core::usecase::NullEventSink;
use serde_json::{json, Value};

// ============================================================================
// Scaffolding — an on-disk temp nyx.db, RAII-cleaned.
// ============================================================================

/// RAII temp dir removed on drop — the pattern `gate_spec_anchor_resolution_e2e.rs` uses, with one
/// addition this gate needs: the dir holds a LIVE SQLite file (plus its `-wal` / `-shm`), and on
/// Windows a removal racing the pool's last handle close fails outright. So the removal RETRIES
/// briefly instead of giving up on the first error — leaving temp dirs behind is a debt several
/// tests of this crate already carry and this one does not add to it.
///
/// That the cleanup actually happens is not left to this comment:
/// [`the_harness_removes_its_temp_dir_when_the_scene_drops`] drops a whole scene and asserts the
/// directory is gone.
struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }

    /// A child path under this sandbox, as the `String` the MCP tools take.
    fn child(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        for attempt in 0..20 {
            if !self.0.exists() || std::fs::remove_dir_all(&self.0).is_ok() {
                return;
            }
            // The pool's connections close as `Db` drops, which happens just before this; a
            // handle can still be settling on Windows.
            thread::sleep(Duration::from_millis(25 * (attempt + 1)));
        }
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "nyx-gate-spec-ref-replay-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// Path to the bundled hook script under test — the SHIPPED file, never a copy.
fn hook_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources/claude-plugin/hooks/nyx-session-hook.cjs")
}

/// `true` when `node` is runnable — otherwise the tests SKIP, exactly as
/// `hook_push_injection_e2e.rs` does (node-agnostic by contract, though node IS the hook's runtime).
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ============================================================================
// The sidecar — the REAL verbs, on a real socket.
// ============================================================================

/// The tools this gate's chain actually exercises. `agent_session_event` is NOT one of them: the
/// hook POSTs it fire-and-forget to report the turn (and, on a failed injection, to FILE the reason
/// in nyx), so it is RECORDED here and answered with a bare ok rather than dispatched. The manual
/// fetch goes to the production function.
const TOOL_AGENT_CONTEXT: &str = "agent_context";
const TOOL_SESSION_EVENT: &str = "agent_session_event";

/// THE MARKER OF A /nspec MANUAL THAT ANCHORS NO SPEC — `mcp_context::no_spec_block`'s own heading,
/// the first line of the `{{spec}}` block on the authoring render. It is what the session reads
/// right before the instruction to `spec_create` a new one, so it is the exact fact the modelled
/// session below branches on (see [`an_nspec_session_authors_when_the_manual_says_there_is_no_spec`]).
const NO_SPEC_YET: &str = "## Spec — none yet";

/// ONE call the sidecar served: the tool name, the arguments it received, and the RESULT the
/// production function returned — kept as the typed `Result` so an assertion reads the real
/// [`RpcCode`] and the real message rather than re-parsing the wire.
struct ServedCall {
    tool: String,
    arguments: Value,
    outcome: Result<Value, RpcError>,
}

/// A loopback HTTP/JSON-RPC endpoint that serves the hook's `tools/call` POSTs with the REAL nyx
/// verbs, over the REAL on-disk `Db`.
///
/// This is `hook_push_injection_e2e.rs`'s mock with its canned answers replaced by the production
/// function — same socket plumbing, same `Content-Length` reader, same stop/join lifecycle. What it
/// buys over that mock is the whole point of this gate: the hook's `agent_context` POST really
/// resolves the token against the database, so the anti-duplicate oracle observes what a session
/// would actually have been shown.
///
/// THE ENVELOPE IS REBUILT, not borrowed: `mcp.rs`'s `rpc_result_envelope` / `rpc_error_envelope`
/// (and `RpcError::rpc_code`) are private. It is faithful in the three fields THIS chain reads —
/// `result.structuredContent` (the hook's `manualFromResponse`), `error.message` (its trace and its
/// filing) and `error.data.code` (the ADR-0003 D8 string code). The JSON-RPC NUMBER is not one of
/// them, so it is fixed rather than mirrored, and no assertion below reads it: refusals are asserted
/// on the recorded [`ServedCall::outcome`], typed code included.
struct RealSidecar {
    port: u16,
    stop: Arc<AtomicBool>,
    served: Arc<Mutex<Vec<ServedCall>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl RealSidecar {
    /// Bind an ephemeral port and serve `db` until [`RealSidecar::stop`].
    fn start(db: Arc<Db>) -> RealSidecar {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the gate's sidecar");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let served: Arc<Mutex<Vec<ServedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let stop_t = stop.clone();
        let served_t = served.clone();
        let handle = thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        stream
                            .set_read_timeout(Some(Duration::from_millis(5_000)))
                            .ok();
                        handle_conn(&mut stream, &db, &served_t);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        RealSidecar {
            port,
            stop,
            served,
            handle: Some(handle),
        }
    }

    /// The tool names the hook POSTed, in order.
    fn tool_names(&self) -> Vec<String> {
        self.served
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.tool.clone())
            .collect()
    }

    /// The arguments of the FIRST call to `tool` — `None` when the hook never made it.
    fn arguments_for(&self, tool: &str) -> Option<Value> {
        self.served
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.tool == tool)
            .map(|c| c.arguments.clone())
    }

    /// The RESULT the production function returned for the FIRST call to `tool`.
    fn outcome_for(&self, tool: &str) -> Option<Result<Value, RpcError>> {
        self.served
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.tool == tool)
            .map(|c| c.outcome.clone())
    }

    /// The reasons the hook FILED in nyx through `agent_session_event` — one per injection that
    /// produced no manual. The channel by which a refusal reaches nyx rather than only stderr.
    fn filed_injection_failures(&self) -> Vec<String> {
        self.served
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.tool == TOOL_SESSION_EVENT)
            .filter_map(|c| {
                c.arguments["nyx_injection_failed"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect()
    }

    fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

impl Drop for RealSidecar {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Read one HTTP request off `stream`, DISPATCH it to the production verb, record what was asked and
/// what came back, and answer in the sidecar's envelope shape.
fn handle_conn(stream: &mut TcpStream, db: &Db, served: &Arc<Mutex<Vec<ServedCall>>>) {
    let Some(body) = read_http_body(stream) else {
        return;
    };
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = parsed["id"].clone();
    let tool = parsed["params"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let arguments = parsed["params"]["arguments"].clone();

    let outcome = match tool.as_str() {
        TOOL_AGENT_CONTEXT => {
            nyx_core::mcp_context::agent_context(db, &arguments).map(|o| o.result)
        }
        // Fire-and-forget telemetry, out of the chain under test: recorded, answered, not dispatched.
        _ => Ok(json!({})),
    };

    if !tool.is_empty() {
        served.lock().unwrap().push(ServedCall {
            tool,
            arguments,
            outcome: outcome.clone(),
        });
    }

    let payload = match &outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": {
            "content": [{ "type": "text", "text": result.to_string() }],
            "structuredContent": result,
            "isError": false,
        }}),
        Err(e) => {
            let mut data = match e.data.as_ref().and_then(Value::as_object) {
                Some(obj) => Value::Object(obj.clone()),
                None => json!({}),
            };
            data["code"] = json!(e.code.as_str());
            json!({ "jsonrpc": "2.0", "id": id, "error": {
                // See [`RealSidecar`]: the transport NUMBER is not a field this chain reads, and
                // the mapping that produces it is private. The typed code rides `data.code`, as it
                // does on the wire.
                "code": -32000,
                "message": e.message,
                "data": data,
            }})
        }
    }
    .to_string();

    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Read an HTTP request's body honoring `Content-Length` (the hook always sets it). `None` if the
/// socket closes before a full request arrives.
fn read_http_body(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            let content_length = content_length(&buf[..pos]);
            let body_start = pos + 4;
            if buf.len() - body_start >= content_length {
                return Some(buf[body_start..body_start + content_length].to_vec());
            }
        }
        match stream.read(&mut tmp) {
            Ok(0) => return None,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => return None,
        }
    }
}

/// Case-insensitive `Content-Length` from a raw header block (0 when absent).
fn content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    for line in text.split("\r\n") {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                return v.trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

/// First index of `needle` in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ============================================================================
// The hook — spawned for real, under node, on the payload Claude Code sends.
// ============================================================================

/// The hook's observable output: its exit code, its stdout (the injection) and its stderr (the trace
/// saying why there was none).
struct HookRun {
    code: i32,
    stdout: String,
    stderr: String,
}

impl HookRun {
    /// `hookSpecificOutput.additionalContext` from stdout, or `None` when the hook injected nothing.
    fn injected(&self) -> Option<String> {
        let trimmed = self.stdout.trim();
        if trimmed.is_empty() {
            return None;
        }
        let v: Value = serde_json::from_str(trimmed).ok()?;
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            json!("UserPromptSubmit"),
            "the injected block is tagged UserPromptSubmit",
        );
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .map(str::to_string)
    }
}

/// Run the REAL hook as `node nyx-session-hook.cjs UserPromptSubmit`, piping in the payload shape
/// Claude Code sends — `prompt` is the text the human TYPED, which is all a `UserPromptSubmit` hook
/// ever receives.
fn run_hook(prompt: &str, cwd: &str, port: u16) -> HookRun {
    let input = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-spec-ref-replay",
        "cwd": cwd,
        "prompt": prompt,
    })
    .to_string();

    let mut child = Command::new("node")
        .arg(hook_path())
        .arg("UserPromptSubmit")
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env("NYX_MCP_PORT", port.to_string())
        .env("NYX_TERMINAL_ID", TERMINAL_ID)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn node hook");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .expect("write hook stdin");

    let out = child.wait_with_output().expect("wait hook");
    HookRun {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

// ============================================================================
// The scene — the exact state of the incident.
// ============================================================================

/// The terminal the replayed session runs from — the hook's `NYX_TERMINAL_ID`.
const TERMINAL_ID: &str = "term-spec-ref-replay";

/// The state session `01a011d0` was in when `/nspec 3` was typed.
///
/// FIELD ORDER IS THE DROP ORDER (fields drop in declaration order): `db` first — the pool's
/// connections close — then `tmp`, which removes the directory those connections held open.
struct Scene {
    db: Arc<Db>,
    project_id: String,
    /// The Spec the human names as `3` — a `draft` mid-grill, the work they are coming back to.
    spec3_id: String,
    /// The folder Claude Code hands the hook as `cwd` — a dev workspace of the project, anchored on
    /// NO Spec (a `draft` cannot be anchored), so the typed reference is the ONLY thing that can
    /// bind the render. That is what made the incident possible and what this gate re-creates.
    cwd: String,
    tmp: TempDir,
}

impl Scene {
    fn build(tag: &str) -> Scene {
        let tmp = temp_dir(tag);
        let db = Arc::new(Db::open(&tmp.path().join("nyx.db")).expect("open temp nyx.db"));

        let project_id = db
            .with_conn(|c| db::create_project(c, "Nyx", &tmp.child("project-root"), None))
            .expect("project")
            .0
            .id;

        // #1 and #2 exist so `3` is a real per-project number and not the only row in the table —
        // and so the oracle below counts a project, not a singleton.
        draft_spec(&db, &project_id, "porter le contexte du run");
        draft_spec(&db, &project_id, "afficher l'id à côté du #N");
        let spec3_id = draft_spec(&db, &project_id, "ajouter la contre-pression");
        assert_eq!(
            spec_row(&db, &spec3_id).number,
            3,
            "the subject Spec is the project's #3 — the reference the human types",
        );
        assert_eq!(
            spec_row(&db, &spec3_id).status,
            db::SPEC_STATUS_DRAFT,
            "the subject Spec is a draft, as it was in the incident",
        );

        // An OPEN idea on the same topic: the backlog `/nspec`'s manual tells the author to search
        // before authoring. It is the SECOND defence against the duplicate this gate is about, and
        // the render is asserted to surface it.
        idea_tool(
            &db,
            "idea_add",
            json!({
                "project_id": project_id,
                "title": "la contre-pression manque au flux d'événements",
                "body": "les événements s'accumulent sans borne",
            }),
        );

        let cwd = register_workspace(&db, &project_id, &tmp.child("dev-workspace"));

        Scene {
            db,
            project_id,
            spec3_id,
            cwd,
            tmp,
        }
    }

    /// The project's Specs as `(number, id)`, read through `spec_list` — the tool an agent reads the
    /// project through, so the ORACLE observes exactly what a session would see.
    fn specs(&self) -> Vec<(i64, String)> {
        spec_tool(
            &self.db,
            "spec_list",
            json!({ "project_id": self.project_id }),
        )["specs"]
            .as_array()
            .expect("spec_list answers a specs array")
            .iter()
            .map(|s| {
                (
                    s["number"].as_i64().expect("a per-project number"),
                    s["id"].as_str().expect("an id").to_string(),
                )
            })
            .collect()
    }

    /// AUTHOR a Spec the way a session would — through the `spec_create` verb, never a DB poke.
    fn spec_create(&self, title: &str) {
        spec_tool(
            &self.db,
            "spec_create",
            json!({
                "project_id": self.project_id,
                "title": title,
                "problem": "the reference the human typed reached nobody",
            }),
        );
    }

    fn sidecar(&self) -> RealSidecar {
        RealSidecar::start(self.db.clone())
    }
}

/// Drive a Spec tool through the REAL MCP routing (`dispatch_spec_tool`) — the entry the sidecar's
/// dispatcher calls. Panics on a mis-route or a refusal.
fn spec_tool(db: &Db, name: &str, args: Value) -> Value {
    nyx_core::mcp_tools_spec::dispatch_spec_tool(db, name, &args)
        .unwrap_or_else(|| panic!("`{name}` was not routed"))
        .unwrap_or_else(|e| panic!("`{name}` failed: [{}] {}", e.code, e.message))
        .result
}

/// The same, for the idea tools.
fn idea_tool(db: &Db, name: &str, args: Value) -> Value {
    nyx_core::mcp_tools_idea::dispatch_idea_tool(db, name, &args)
        .unwrap_or_else(|| panic!("`{name}` was not routed"))
        .unwrap_or_else(|e| panic!("`{name}` failed: [{}] {}", e.code, e.message))
        .result
}

/// A `draft` Spec of `project_id` with a problem but NO solution — a draft mid-grill, which is what
/// a `/nspec` session comes back to. Created through the `spec_create` TOOL, which mints the
/// per-project `number` the human then types.
fn draft_spec(db: &Db, project_id: &str, title: &str) -> String {
    spec_tool(
        db,
        "spec_create",
        json!({
            "project_id": project_id,
            "title": title,
            "problem": "the reference the human typed reaches nobody",
        }),
    )["spec"]["id"]
        .as_str()
        .expect("the created Spec's id")
        .to_string()
}

fn spec_row(db: &Db, spec_id: &str) -> db::Spec {
    db.with_conn(|c| db::get_spec(c, spec_id))
        .expect("read the Spec")
        .expect("the Spec exists")
}

/// Register the dev workspace the human types from, through the REAL MCP tool and with NO
/// `spec_id`: the folder anchors no Spec. Returns the STORED root path (`db::create_workspace`
/// normalizes it), which is what a `cwd` argument has to look like on this platform.
fn register_workspace(db: &Db, project_id: &str, path: &str) -> String {
    std::fs::create_dir_all(path).expect("the folder workspace_add registers must exist");
    let res = dispatch_workspace_tool(
        db,
        &NullEventSink,
        "workspace_add",
        &json!({ "project_id": project_id, "name": "dev", "path": path }),
    )
    .expect("`workspace_add` is routed")
    .expect("`workspace_add` registers the dev workspace");
    res["workspace"]["path"]
        .as_str()
        .expect("the registered workspace path")
        .to_string()
}

/// WHAT AN `/nspec` SESSION DOES WITH WHAT NYX SHOWED IT — the damage function, and the only reason
/// the Spec-count oracle can ever go red.
///
/// nyx authors no Spec: the SESSION does, and which branch it takes is decided by the ONE thing it
/// can read — the block the hook injected. When that block is a /nspec manual whose `{{spec}}`
/// section says [`NO_SPEC_YET`], the text the session reads next is nyx's own instruction: « You are
/// authoring a NEW Spec in this project. Create it with spec_create(…) ». That branch IS the
/// incident: `/nspec 3` resolved nothing, the manual came back Spec-less, and a duplicate of Spec #3
/// was authored.
///
/// So this replays THAT ONE DECISION and nothing else. It reaches no model and reads no table — the
/// injected text is the whole of its input, which is also the whole of a session's.
///
/// Called AFTER the oracle's "before" snapshot and BEFORE its comparison, so that any break in the
/// chain upstream — the hook dropping the typed token, `agent_context` failing to resolve `3`, a
/// refusal that did not hold — shows up as a second Spec in the project rather than as nothing at
/// all.
fn an_nspec_session_authors_when_the_manual_says_there_is_no_spec(
    scene: &Scene,
    injected: Option<&str>,
) {
    if injected.is_some_and(|text| text.contains(NO_SPEC_YET)) {
        scene.spec_create("ajouter la contre-pression (authored by a session shown no Spec)");
    }
}

// ============================================================================
// THE GATE.
// ============================================================================

/// THE REPLAY. `/nspec 3` typed on a project that holds a Spec #3: the reference travels the real
/// hook, the real `agent_context` resolves it, the session is shown a manual BOUND to that Spec, and
/// the project ends the turn holding EXACTLY the Specs it started with.
///
/// The facts asserted, in this order: the same Specs before and after (the ORACLE), the token
/// reached the server RAW beside the cwd, the render resolved it to Spec #3, and the text the
/// session actually received names that Spec — and carries the anti-duplication lists (the sibling
/// Specs and the open backlog) that are the manual's own second defence against authoring twice.
#[test]
fn replaying_nspec_3_on_an_existing_spec_authors_no_second_spec() {
    if !node_available() {
        eprintln!("node unavailable: skipped the /nspec replay gate (node-agnostic by contract)");
        return;
    }
    let scene = Scene::build("replay");
    let sidecar = scene.sidecar();

    // The state of the world the instant before the human presses Enter.
    let specs_before = scene.specs();
    assert_eq!(
        specs_before.len(),
        3,
        "the scene starts on three Specs: {specs_before:?}",
    );

    // `/nspec 3 …` — the prompt the human TYPED, through the shipped hook, under node.
    let hook = run_hook("/nspec 3 reprends la spec", &scene.cwd, sidecar.port);
    assert_eq!(hook.code, 0, "the hook always exits 0");
    let injected = hook.injected().unwrap_or_else(|| {
        panic!(
            "/nspec 3 must inject a manual: stdout {:?} stderr {:?}",
            hook.stdout, hook.stderr
        )
    });

    // The session's own turn, on the text it was handed (see the function's doc: this is the damage
    // function the oracle needs).
    an_nspec_session_authors_when_the_manual_says_there_is_no_spec(&scene, Some(&injected));

    // ------------------------------------------------------------------
    // THE ORACLE — what this whole PRD exists for, and therefore the FIRST thing asserted once the
    // turn is played: every assertion below it diagnoses a break the oracle has already caught.
    // ------------------------------------------------------------------
    assert_eq!(
        scene.specs(),
        specs_before,
        "ZERO DUPLICATE: the project must hold the SAME Specs after `/nspec 3` as before it. A row \
         appearing here is the incident itself — the reference designated Spec #3, the manual came \
         back carrying no Spec, and the session authored a second one",
    );

    // WHERE the chain would have broken, had it broken: the token reached `agent_context` RAW,
    // beside the cwd (the hook resolves nothing).
    assert_eq!(
        sidecar.tool_names(),
        vec![
            TOOL_SESSION_EVENT.to_string(),
            TOOL_AGENT_CONTEXT.to_string(),
        ],
        "the hook reported the turn, then fetched the manual — and, the manual having arrived, \
         filed no failure",
    );
    let manual_args = sidecar
        .arguments_for(TOOL_AGENT_CONTEXT)
        .expect("agent_context was called");
    assert_eq!(
        manual_args["spec_id"],
        json!("3"),
        "the hook forwarded the typed reference BYTE FOR BYTE, resolving nothing: {manual_args}",
    );
    assert_eq!(
        manual_args["cwd"],
        json!(scene.cwd),
        "…beside the cwd, which is what resolves the project a bare number is counted in: \
         {manual_args}",
    );
    assert_eq!(
        manual_args["role"],
        json!("spec"),
        "…on the role `/nspec` reads, which is what makes it the RIGHT manual: {manual_args}",
    );

    // WHAT THE SERVER ANSWERED: the reference resolved to Spec #3, with no diagnosis attached.
    let rendered = sidecar
        .outcome_for(TOOL_AGENT_CONTEXT)
        .expect("agent_context was called")
        .unwrap_or_else(|e| panic!("agent_context must not refuse: [{}] {}", e.code, e.message));
    assert_eq!(
        rendered["spec_id"],
        json!(scene.spec3_id),
        "`3` designated Spec #3 and nyx resolved it to its canonical id: {rendered}",
    );
    assert_eq!(
        rendered["warnings"],
        json!([]),
        "a render that resolved carries no case-0 diagnosis: {rendered}",
    );

    // AND THE SESSION WAS SHOWN IT. The payload is nyx's; the injected text is the session's, and
    // they must agree — a manual that resolved a Spec the injected block never names is a session
    // still free to author a duplicate.
    assert!(
        injected.contains(&scene.spec3_id),
        "the injected manual names the Spec it is bound to, by id: {injected}",
    );
    assert!(
        !injected.contains(NO_SPEC_YET),
        "…and it is NOT the authoring manual: the session was never told to create a new Spec: \
         {injected}",
    );

    // THE MANUAL'S OWN SECOND DEFENCE — the anti-duplication lists. Even bound, the /nspec manual
    // shows what the project already tracks, which is what stops an author re-creating a need that
    // is already written down somewhere.
    assert!(
        injected.contains("porter le contexte du run"),
        "the render lists the project's other Specs: {injected}",
    );
    assert!(
        injected.contains("la contre-pression manque au flux d'événements")
            && injected.contains("idea_search"),
        "…and its open idea backlog, with the instruction to search it before authoring: \
         {injected}",
    );

    sidecar.stop();
}

/// THE OTHER HALF OF THE SAME TURN: a reference that DESIGNATES NOTHING. `/nspec 99` must leave the
/// project exactly as it found it — no Spec authored — and must hand back a refusal a caller can
/// ACT on.
///
/// "Actionable" is asserted as FACTS, never as a sentence: the typed code says the id is unknown,
/// the accepted forms come from the grammar's own constant, and the nearest candidate is named BY
/// ITS ID — the value the next call is formed from. That the refusal then TRAVELS is asserted
/// against the message the verb itself produced (so a re-wording follows rather than breaks), on
/// the THREE channels it has to reach: the block the hook injects (the only one the MODEL is ever
/// shown), the hook's stderr trace, and the `agent_session_event` filing that puts it in nyx.
///
/// THAT FIRST CHANNEL IS REQUIRED, NOT MERELY TOLERATED. Claude Code never feeds stderr to the
/// model, so swallowing the refusal is nyx saying nothing at all about a command it has just
/// refused, leaving the session to improvise at the exact spot where improvising authored the
/// duplicate. `refusedReferenceBlock` relays it instead — and it carries the instruction that IS
/// this gate's subject: never author a new Spec to get past a refused reference.
#[test]
fn nspec_99_designates_nothing_so_it_authors_no_spec_and_names_what_to_send() {
    if !node_available() {
        eprintln!(
            "node unavailable: skipped `/nspec 99` — the unknown-reference half of the replay gate \
             (node-agnostic by contract)"
        );
        return;
    }
    let scene = Scene::build("unknown");
    let sidecar = scene.sidecar();

    let specs_before = scene.specs();

    let hook = run_hook("/nspec 99 reprends la spec", &scene.cwd, sidecar.port);
    assert_eq!(hook.code, 0, "the hook always exits 0");

    // The session takes its turn on whatever it was shown. When the refusal held, that is the
    // refused-reference block, which names no Spec to author and says so; when it did NOT hold,
    // what reaches the session is the Spec-less authoring manual, and the modelled session authors.
    // That is what makes the Spec assertion below a lock on THIS leg too.
    let injected = hook.injected();
    an_nspec_session_authors_when_the_manual_says_there_is_no_spec(&scene, injected.as_deref());

    assert_eq!(
        scene.specs(),
        specs_before,
        "`/nspec 99` authored no Spec: the project holds the same rows",
    );

    // The refusal itself, read off the verb that produced it.
    let refusal = sidecar
        .outcome_for(TOOL_AGENT_CONTEXT)
        .expect("agent_context was called")
        .expect_err("`99` designates no Spec of this project, so agent_context refuses");
    assert_eq!(
        refusal.code,
        RpcCode::InvalidId,
        "the refusal says the REFERENCE is unknown: {}",
        refusal.message,
    );
    assert!(
        refusal.message.contains(nyx_core::spec_ref::ACCEPTED_FORMS),
        "the refusal names the forms nyx accepts, read from the grammar's own constant: {}",
        refusal.message,
    );
    assert!(
        refusal.message.contains(&scene.spec3_id),
        "…and names the project's nearest candidate BY ID, so the next call can be formed from the \
         answer: {}",
        refusal.message,
    );

    // NO MANUAL IS NOT NOTHING. The refusal reaches the three channels it has to, VERBATIM — the
    // server's own sentence, whatever it says, not a wording restated by the hook. Asserted against
    // the message the verb ACTUALLY produced above, so re-wording the refusal re-words these too.
    let injected = injected
        .expect("a refused reference is RELAYED to the session — the model is never shown stderr");
    assert!(
        injected.contains(&refusal.message),
        "the injected block carries the SERVER's sentence, not a hook-side paraphrase: {injected}",
    );
    assert!(
        !injected.contains(NO_SPEC_YET),
        "…and it is NOT a manual: a refused reference must never degrade into the authoring \
         branch, which is the incident itself: {injected}",
    );
    assert!(
        injected.contains("never author a new one"),
        "…and it carries the instruction this whole gate is about — stop, ask, do not author: \
         {injected}",
    );
    assert!(
        hook.stderr.contains(&refusal.message),
        "the trace carries the SERVER's sentence, not a hook-side paraphrase: {:?}",
        hook.stderr,
    );
    let filed = sidecar.filed_injection_failures();
    assert!(
        filed.iter().any(|r| r.contains(&refusal.message)),
        "the failed injection is FILED in nyx carrying that same sentence — stderr alone is read by \
         nobody, which is how a dead injection once survived a release. Got {filed:?}",
    );

    sidecar.stop();
}

/// THE ORACLE CAN GO RED — the control both legs above depend on and neither can provide.
///
/// A test whose damage function never fires proves nothing: "no duplicate was authored" and "this
/// session authors nothing, ever" look identical from the outside. So the same scene, the same
/// hook, the same modelled session are driven on the ONE prompt where authoring is LEGITIMATE —
/// `/nspec` with no reference at all, from a folder whose workspace anchors no Spec, which is the
/// nominal entry of a project about to write its next one. nyx renders the authoring manual, says
/// WHY it anchored nothing, and the session authors: the project gains a Spec.
///
/// That is not a duplicate and nothing here calls it one. It is the proof that the two legs above
/// are locks and not tautologies.
#[test]
fn with_no_reference_at_all_the_author_is_told_to_create_one_and_the_oracle_moves() {
    if !node_available() {
        eprintln!("node unavailable: skipped the oracle's own control (node-agnostic by contract)");
        return;
    }
    let scene = Scene::build("authoring");
    let sidecar = scene.sidecar();

    let specs_before = scene.specs();
    let hook = run_hook("/nspec ajoute la contre-pression", &scene.cwd, sidecar.port);
    assert_eq!(hook.code, 0, "the hook always exits 0");
    let injected = hook.injected().unwrap_or_else(|| {
        panic!(
            "an unreferenced /nspec still gets its manual: stdout {:?} stderr {:?}",
            hook.stdout, hook.stderr
        )
    });

    // No reference typed ⇒ none forwarded: the render has only the cwd to go on.
    let manual_args = sidecar
        .arguments_for(TOOL_AGENT_CONTEXT)
        .expect("agent_context was called");
    assert!(
        manual_args.get("spec_id").is_none(),
        "the hook forwards a reference only when the human typed one: {manual_args}",
    );

    let rendered = sidecar
        .outcome_for(TOOL_AGENT_CONTEXT)
        .expect("agent_context was called")
        .unwrap_or_else(|e| panic!("the AUTHOR is never refused for lack of a Spec: {}", e.message));
    assert_eq!(
        rendered["spec_id"],
        Value::Null,
        "nothing was anchored, and nyx says so instead of picking one: {rendered}",
    );
    assert_eq!(
        rendered["warnings"][0]["code"],
        json!("spec_none_on_location"),
        "…naming the case: the location IS a real workspace, it simply carries no Spec: {rendered}",
    );
    assert!(
        injected.contains(NO_SPEC_YET),
        "the manual the session was shown is the AUTHORING one: {injected}",
    );

    an_nspec_session_authors_when_the_manual_says_there_is_no_spec(&scene, Some(&injected));

    let after = scene.specs();
    assert_eq!(
        after.len(),
        specs_before.len() + 1,
        "THE ORACLE MOVES: shown the authoring manual, the modelled session authors — which is what \
         makes its silence on the other two legs a fact and not a tautology. Before {specs_before:?} \
         after {after:?}",
    );

    sidecar.stop();
}

/// THE HARNESS CLEANS UP AFTER ITSELF. Several tests of this crate leave their temp directories
/// behind; this one may not, and saying so in a comment is not saying it at all — so the property is
/// a test: build a whole scene (temp dir, on-disk SQLite with its `-wal`/`-shm`), drop it, and
/// assert the directory is gone.
#[test]
fn the_harness_removes_its_temp_dir_when_the_scene_drops() {
    // NO `node_available()` guard: this one spawns no hook and reaches no `node`. It builds a scene
    // and drops it, which is pure Rust and pure filesystem, so it runs everywhere.
    let sandbox = {
        let scene = Scene::build("cleanup");
        let sandbox = scene.tmp.path().to_path_buf();
        assert!(
            sandbox.join("nyx.db").exists(),
            "the scene really put a database in {sandbox:?}",
        );
        sandbox
    };
    assert!(
        !sandbox.exists(),
        "the scene's temp dir must be gone once it drops, {sandbox:?} is still there",
    );
}
