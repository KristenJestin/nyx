//! GATE e2e — « rejouer `/nspec 3` sur une Spec au run vivant : zéro doublon »
//! (PRD `01M08Y8AW9A9F68SXB4MC03W3C`, phase 4, task `01M0901Z4YZ1XK174KDVAS9SV9`).
//!
//! THE INCIDENT, REPLAYED. A project holding a Spec `#3` in `draft`, an `nspec` run ALREADY live on
//! it and parked mid-flow, and a human typing `/nspec 3 …` to pick that work back up. What happened
//! (session `01a011d0`) is that the reference `3` reached nobody: the hook recognised only the
//! canonical id, so it POSTed `run_start` with the `cwd` alone; the `cwd` anchored no Spec, so the
//! run opened UNBOUND; the flow's entry gate read `spec_status` off nothing and fell through to its
//! "No Spec yet — author a NEW one from scratch" branch; and the session dutifully authored a
//! DUPLICATE of the very Spec the human had named.
//!
//! Every other task of this PRD fixed one link. THIS one proves the chain holds, over the REAL
//! bricks and end to end:
//!
//!   * the REAL hook — `resources/claude-plugin/hooks/nyx-session-hook.cjs`, spawned under `node`
//!     with the `UserPromptSubmit` payload Claude Code actually sends (the prompt the human TYPED,
//!     never an expanded command body);
//!   * the REAL [`nyx_core::mcp_engine::run_start`] and [`nyx_core::mcp_context::agent_context`],
//!     over an ON-DISK `Db::open` that has SEEDED the built-in `nspec` flow — served to the hook
//!     over loopback HTTP/JSON-RPC by [`RealSidecar`], so the two halves meet on the wire exactly as
//!     they do in the product;
//!   * the REAL [`nyx_core::mcp_engine::nyx_next`], used ONCE in the setup to park the live run on a
//!     node with the persisted traversal a resume needs.
//!
//! Its two harnesses are COPIED from the ones this crate already has, not shared with them: the
//! HTTP / `node` plumbing is `hook_push_injection_e2e.rs`'s (a mock sidecar there, the real verbs
//! here) and the DB scaffolding — an RAII temp dir, an on-disk seeded `Db`, tools driven through
//! their real entry points — is `gate_spec_anchor_resolution_e2e.rs`'s, and both were re-typed here.
//! Each file under `tests/` is its OWN crate, so sharing would mean a `tests/common/` module this
//! crate does not have; introducing one to de-duplicate three benches is a refactor of the whole
//! test tree and out of this task's scope. The cost is stated rather than dressed up as reuse: a fix
//! to either original does not reach this copy.
//!
//! # THE ORACLE, AND WHAT MAKES IT BITE
//!
//! The assertion that carries this whole PRD is one line: THE PROJECT HOLDS THE SAME SPECS BEFORE
//! AND AFTER. An oracle nothing can turn red proves nothing, though, and `run_start` never authors a
//! Spec by itself — nyx does not: the SESSION does, on the branch the flow's entry gate sends it
//! down. So the gate replays that ONE decision, and only it, in
//! [`an_nspec_session_authors_when_its_run_carries_no_spec`]. With it in place, ANY break in the
//! chain — the hook dropping the typed token, `run_start` failing to resolve `3`, the run coming
//! back unbound for any other reason — lands a second Spec in the project and the oracle names it.
//!
//! # WHAT THIS GATE DELIBERATELY DOES NOT DO (the task's non-goals)
//!
//! It does not WALK the `nspec` graph (untouched by this PRD): the one node-level fact it uses is
//! the entry gate's own branch condition, read as the fact it is (`the run carries no Spec`) rather
//! than by ticking. It depends on NO Claude binary and NO LLM — nothing here reaches a model. And it
//! freezes NO wording: a resume is asserted by its machine code and its `outcome` tag, a refusal by
//! its typed [`RpcCode`] and by the VALUES it hands back (the accepted forms, read from the
//! grammar's own constant; the id of the nearest candidate Spec), never by a sentence — and the
//! block the hook injects in a refused manual's place by what it CARRIES (the server's own sentence,
//! relayed; no `run_id`), never by its own prose.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use nyx_core::db::{self, Db, SpecSections};
use nyx_core::mcp::{RpcCode, RpcError};
use nyx_core::mcp_engine::{nyx_next, run_start, CallerSession, EngineRuntime};
use nyx_core::mcp_runtime::dispatch_workspace_tool;
use nyx_core::usecase::NullEventSink;
use serde_json::{json, Value};

// ============================================================================
// Scaffolding — an on-disk temp nyx.db (Db::open SEEDS the built-ins), RAII-cleaned.
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
/// in nyx), so it is RECORDED here and answered with a bare ok rather than dispatched. Everything
/// else the hook calls goes to the production function.
const TOOL_RUN_START: &str = "run_start";
const TOOL_AGENT_CONTEXT: &str = "agent_context";
const TOOL_SESSION_EVENT: &str = "agent_session_event";

/// The `run_start` WARNING code for a run RESUMED rather than opened
/// (`mcp_engine::RUN_RESUMED_WARNING_CODE`, `pub(crate)` and therefore unreachable from an
/// integration test). A STABLE MACHINE code — which is precisely why the gate keys on it: the
/// task's non-goals forbid freezing the sentence beside it, and a code is the half that is meant to
/// be depended on.
const RESUMED_WARNING_CODE: &str = "run_resumed";

/// The `run_start` `outcome` tag for a run handed back as it was (`RUN_START_OUTCOME_RESUMED`, same
/// visibility, same reason).
const OUTCOME_RESUMED: &str = "resumed";

/// The `run_start` `outcome` tag for a run that did not exist before (`RUN_START_OUTCOME_CREATED`).
const OUTCOME_CREATED: &str = "created";

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
/// functions — same socket plumbing, same `Content-Length` reader, same stop/join lifecycle. What it
/// buys over that mock is the whole point of this gate: the hook's `run_start` POST really opens (or
/// resumes) a row, so the anti-duplicate oracle observes the DATABASE and not a fixture.
///
/// THE ENVELOPE IS REBUILT, not borrowed: `mcp.rs`'s `rpc_result_envelope` / `rpc_error_envelope`
/// (and `RpcError::rpc_code`) are private. It is faithful in the three fields THIS chain reads —
/// `result.structuredContent` (the hook's `runFromResponse` / `manualFromResponse`), `error.message`
/// (its trace and its filing) and `error.data.code` (the ADR-0003 D8 string code). The JSON-RPC
/// NUMBER is not one of them, so it is fixed rather than mirrored, and no assertion below reads it:
/// refusals are asserted on the recorded [`ServedCall::outcome`], typed code included.
struct RealSidecar {
    port: u16,
    stop: Arc<AtomicBool>,
    served: Arc<Mutex<Vec<ServedCall>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl RealSidecar {
    /// Bind an ephemeral port and serve `db` through `runtime` until [`RealSidecar::stop`].
    ///
    /// `runtime` is the caller's — ONE nyx, ONE [`EngineRuntime`]: the setup's `nyx_next` tick and
    /// the hook's `run_start` must share the engine registry, as they do inside the sidecar process.
    fn start(db: Arc<Db>, runtime: Arc<EngineRuntime>) -> RealSidecar {
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
                        handle_conn(&mut stream, &db, &runtime, &served_t);
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
    /// produced nothing. The channel by which a refusal reaches nyx rather than only stderr.
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
fn handle_conn(
    stream: &mut TcpStream,
    db: &Db,
    runtime: &EngineRuntime,
    served: &Arc<Mutex<Vec<ServedCall>>>,
) {
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
        TOOL_RUN_START => run_start(db, runtime, &arguments).map(|o| o.result),
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

/// The terminal the replayed session runs from. Threaded into the setup tick's [`CallerSession`] and
/// into the hook's `NYX_TERMINAL_ID` so both halves speak for the same session.
const TERMINAL_ID: &str = "term-spec-ref-replay";

/// The state session `01a011d0` was in when `/nspec 3` was typed.
///
/// FIELD ORDER IS THE DROP ORDER (fields drop in declaration order): `db` first — the pool's
/// connections close — then `tmp`, which removes the directory those connections held open.
struct Scene {
    db: Arc<Db>,
    runtime: Arc<EngineRuntime>,
    project_id: String,
    /// The Spec the human names as `3` — `draft`, and the subject of the live run.
    spec3_id: String,
    /// The built-in `nspec` workflow the run walks.
    nspec_workflow_id: String,
    /// The folder Claude Code hands the hook as `cwd` — a dev workspace of the project, anchored on
    /// NO Spec (a `draft` cannot be anchored), so the typed reference is the ONLY thing that can
    /// bind the run. That is what made the incident possible and what this gate re-creates.
    cwd: String,
    /// The `nspec` run ALREADY live on Spec #3, parked on a node.
    live_run_id: String,
    tmp: TempDir,
}

impl Scene {
    fn build(tag: &str) -> Scene {
        let tmp = temp_dir(tag);
        let db = Arc::new(
            Db::open(&tmp.path().join("nyx.db")).expect("open temp nyx.db (seeds the built-ins)"),
        );

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

        let cwd = register_workspace(&db, &project_id, &tmp.child("dev-workspace"));

        let runtime = Arc::new(EngineRuntime::new());
        let (live_run_id, nspec_workflow_id) =
            open_and_park_the_live_run(&db, &runtime, &cwd, &spec3_id);

        Scene {
            db,
            runtime,
            project_id,
            spec3_id,
            nspec_workflow_id,
            cwd,
            live_run_id,
            tmp,
        }
    }

    /// The project's Specs as `(number, id)`, in the project's own order — the value the ORACLE
    /// compares before and after.
    fn specs(&self) -> Vec<(i32, String)> {
        self.db
            .with_conn(|c| db::list_specs(c, Some(&self.project_id)))
            .expect("list the project's Specs")
            .into_iter()
            .map(|s| (s.number, s.id))
            .collect()
    }

    /// Every run of the `(nspec workflow, Spec #3)` pair — the pair N20 converges to at most one.
    fn runs_on_spec3(&self) -> Vec<String> {
        self.db
            .with_conn(|c| db::find_runs_for_spec(c, &self.nspec_workflow_id, Some(&self.spec3_id)))
            .expect("the runs of the (nspec, #3) pair")
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    /// Every run of the built-in `nspec` workflow, whatever it is bound to — what catches a SECOND,
    /// UNBOUND run being opened beside the live one (the shape the incident actually took).
    fn all_nspec_runs(&self) -> Vec<String> {
        self.db
            .with_conn(|c| db::list_workflow_runs(c, &self.nspec_workflow_id))
            .expect("the nspec workflow's runs")
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    fn run_row(&self, run_id: &str) -> db::WorkflowRun {
        self.db
            .with_conn(|c| db::get_workflow_run(c, run_id))
            .expect("read the run")
            .unwrap_or_else(|| panic!("run '{run_id}' exists"))
    }

    fn sidecar(&self) -> RealSidecar {
        RealSidecar::start(self.db.clone(), self.runtime.clone())
    }
}

/// A `draft` Spec of `project_id` with a problem but NO solution — a draft mid-grill, which is what
/// a live `/nspec` run is parked on. Created through `db::create_spec`, which mints the per-project
/// `number` the human then types.
fn draft_spec(db: &Db, project_id: &str, title: &str) -> String {
    db.with_conn(|c| {
        db::create_spec(
            c,
            project_id,
            title,
            SpecSections {
                problem: "the reference the human typed reaches nobody".into(),
                ..SpecSections::empty()
            },
        )
    })
    .expect("create the Spec")
    .0
    .id
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

/// Open the `nspec` run on Spec #3 through the REAL `run_start`, then TICK it ONCE so it is PARKED
/// ON A NODE. Returns `(run_id, workflow_id)`.
///
/// THE TICK IS NOT DECORATION. `build_engine` REFUSES to resume a run that carries a cursor with no
/// persisted traversal, so a run parked by hand would be REPLACED by the second `run_start` instead
/// of resumed — the gate would then assert the wrong thing about a state no real session reaches.
/// One real tick is what produces a real parking. It is SETUP: nothing about the walk is asserted
/// beyond the run having a cursor and still being live.
fn open_and_park_the_live_run(
    db: &Db,
    runtime: &EngineRuntime,
    cwd: &str,
    spec3_id: &str,
) -> (String, String) {
    let opened = run_start(
        db,
        runtime,
        &json!({ "scope": "nspec", "cwd": cwd, "spec_id": spec3_id }),
    )
    .expect("the live run opens")
    .result;
    assert_eq!(
        opened["outcome"], OUTCOME_CREATED,
        "the scene starts with a run nobody had opened before: {opened}",
    );
    let run_id = opened["run_id"].as_str().expect("run_id").to_string();
    let workflow_id = opened["workflow_id"]
        .as_str()
        .expect("workflow_id")
        .to_string();

    nyx_next(
        db,
        runtime,
        CallerSession {
            terminal_id: Some(TERMINAL_ID),
        },
        &json!({ "run_id": run_id }),
    )
    .unwrap_or_else(|e| {
        panic!(
            "the live run must tick once to park: [{}] {}",
            e.code, e.message
        )
    });

    let parked = db
        .with_conn(|c| db::get_workflow_run(c, &run_id))
        .expect("read the parked run")
        .expect("the run exists");
    assert!(
        parked.current_node_id.is_some(),
        "the live run is PARKED ON A NODE (the incident's state), got status={} cursor={:?}",
        parked.status,
        parked.current_node_id,
    );
    assert!(
        !db::run_status_is_terminal(&parked.status),
        "the live run is still LIVE, got status={}",
        parked.status,
    );
    assert!(
        parked.traversal.is_some(),
        "the parked run carries the persisted traversal a resume needs, got none",
    );
    assert_eq!(
        parked.spec_id.as_deref(),
        Some(spec3_id),
        "the live run is bound to Spec #3 — it is THAT Spec's session the human comes back to",
    );
    (run_id, workflow_id)
}

/// WHAT AN `nspec` SESSION DOES WITH THE RUN THE HOOK HANDED IT — the damage function, and the only
/// reason the Spec-count oracle can ever go red.
///
/// nyx authors no Spec: the SESSION does, and which branch it takes is decided by the flow's ENTRY
/// GATE — `seed.rs`'s `build_nspec` reads `spec_status` off the run's Spec and, when the run carries
/// none, falls through to its child 2, the note "No Spec yet — author a NEW one from scratch."
/// That branch IS the incident: `/nspec 3` opened an unbound run, the gate read nothing, and a
/// duplicate of Spec #3 was authored.
///
/// So this replays THAT ONE DECISION and nothing else of the graph. It does not tick, it walks no
/// node (a non-goal of this gate: the graph is untouched by this PRD) and it reaches no model —
/// `run.spec_id.is_none()` is the same fact the `query spec_status` node reads, taken from the same
/// row.
///
/// Called AFTER the oracle's "before" snapshot and BEFORE its comparison, so that any break in the
/// chain upstream — the hook dropping the typed token, `run_start` failing to resolve `3` — shows up
/// as a second Spec in the project rather than as nothing at all.
fn an_nspec_session_authors_when_its_run_carries_no_spec(scene: &Scene, run_id: &str) {
    if scene.run_row(run_id).spec_id.is_none() {
        draft_spec(
            &scene.db,
            &scene.project_id,
            "ajouter la contre-pression (authored by a session whose run carried no Spec)",
        );
    }
}

// ============================================================================
// THE GATE.
// ============================================================================

/// THE REPLAY. `/nspec 3` typed on a project whose Spec #3 already carries a live, parked `nspec`
/// run: the reference travels the real hook, the real `run_start` RESUMES that very run, and the
/// project ends the turn holding EXACTLY the Specs it started with.
///
/// The five facts the done-criterion names, each asserted below in this order: the same Specs before
/// and after (the ORACLE), the run handed back is the one that existed (same id), it is anchored on
/// Spec #3, no second run was opened for the pair — nor anywhere on the `nspec` flow — and the
/// RESUME was announced in the `warnings`, all the way into the text the session receives.
#[test]
fn replaying_nspec_3_on_a_spec_with_a_live_run_authors_no_second_spec_and_opens_no_second_run() {
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
    let nspec_runs_before = scene.all_nspec_runs();
    assert_eq!(
        nspec_runs_before,
        vec![scene.live_run_id.clone()],
        "the scene starts on exactly ONE nspec run — the live one on Spec #3",
    );

    // `/nspec 3 …` — the prompt the human TYPED, through the shipped hook, under node.
    let hook = run_hook("/nspec 3 reprends la spec", &scene.cwd, sidecar.port);
    assert_eq!(hook.code, 0, "the hook always exits 0");
    let injected = hook.injected().unwrap_or_else(|| {
        panic!(
            "/nspec 3 must inject a run block: stdout {:?} stderr {:?}",
            hook.stdout, hook.stderr
        )
    });

    // The session's own turn, on the run it was handed (see the function's doc: this is the damage
    // function the oracle needs, not a walk of the graph).
    let injected_run_id = run_id_of(&injected);
    an_nspec_session_authors_when_its_run_carries_no_spec(&scene, &injected_run_id);

    // ------------------------------------------------------------------
    // THE ORACLE — what this whole PRD exists for, and therefore the FIRST thing asserted once the
    // turn is played: every assertion below it diagnoses a break the oracle has already caught.
    // ------------------------------------------------------------------
    assert_eq!(
        scene.specs(),
        specs_before,
        "ZERO DUPLICATE: the project must hold the SAME Specs after `/nspec 3` as before it. A row \
         appearing here is the incident itself — the reference designated Spec #3, the run came \
         back carrying no Spec, and the session authored a second one",
    );

    // WHERE the chain would have broken, had it broken: the token reached `run_start` RAW, beside
    // the cwd (the hook resolves nothing).
    assert_eq!(
        sidecar.tool_names(),
        vec![
            TOOL_SESSION_EVENT.to_string(),
            TOOL_RUN_START.to_string(),
            TOOL_AGENT_CONTEXT.to_string(),
        ],
        "the hook reported the turn, opened the run, then fetched a manual",
    );
    // FOR THAT RUN, and the order alone does not say so: the third call has to NAME the run the
    // second one handed back. A hook that fetched a manual with no `run_id` (or with another one)
    // would leave the session ticking something else, and the sequence above would not move.
    let manual_args = sidecar
        .arguments_for(TOOL_AGENT_CONTEXT)
        .expect("agent_context was called");
    assert_eq!(
        manual_args["run_id"],
        json!(scene.live_run_id),
        "the manual was fetched FOR the run `run_start` resumed: {manual_args}",
    );
    assert_eq!(
        manual_args["scope"],
        json!("nspec"),
        "…on the flow `/nspec` opens, which is what makes it the RIGHT manual: {manual_args}",
    );
    let run_args = sidecar
        .arguments_for(TOOL_RUN_START)
        .expect("run_start was called");
    assert_eq!(
        run_args["spec_id"],
        json!("3"),
        "the hook forwarded the typed reference BYTE FOR BYTE, resolving nothing: {run_args}",
    );
    assert_eq!(
        run_args["cwd"],
        json!(scene.cwd),
        "…beside the cwd, which still anchors the run's workspace: {run_args}",
    );

    // The run the session was handed is THE ONE THAT EXISTED, and it is anchored on Spec #3.
    assert_eq!(
        injected_run_id, scene.live_run_id,
        "the injected run is the one that was already live, not a fresh one: {injected}",
    );
    assert_eq!(
        scene.run_row(&injected_run_id).spec_id.as_deref(),
        Some(scene.spec3_id.as_str()),
        "the run is anchored on Spec #3 — the Spec the human named",
    );

    // NO SECOND RUN — neither for the pair (N20's convergence), nor anywhere on the nspec flow (the
    // incident's second run was UNBOUND, so it belonged to no pair at all).
    assert_eq!(
        scene.runs_on_spec3(),
        vec![scene.live_run_id.clone()],
        "the (nspec, #3) pair still holds exactly ONE run",
    );
    assert_eq!(
        scene.all_nspec_runs(),
        nspec_runs_before,
        "no second nspec run was opened — not even an unbound one beside the live one",
    );

    // THE RESUME WAS ANNOUNCED — by machine code and by outcome tag, never by wording; and it
    // reached the text the session actually reads, not just the API's return value.
    let started = sidecar
        .outcome_for(TOOL_RUN_START)
        .expect("run_start was called")
        .unwrap_or_else(|e| panic!("run_start must not refuse: [{}] {}", e.code, e.message));
    assert_eq!(
        started["outcome"], OUTCOME_RESUMED,
        "run_start published WHICH of the three things it did: {started}",
    );
    assert!(
        warning_codes(&started)
            .iter()
            .any(|c| c == RESUMED_WARNING_CODE),
        "the resume is announced in the warnings, got {:?}",
        warning_codes(&started),
    );
    let resumed = warning_with_code(&started, RESUMED_WARNING_CODE);
    // The landing node, asserted as a NAME and not merely as an equality: `current_node_id` is an
    // `Option`, so `null == null` would pass this on a run parked nowhere — which is precisely the
    // case where the announcement says nothing useful and the reader most needs it to.
    let parked_on = scene.run_row(&scene.live_run_id).current_node_id;
    let parked_on =
        parked_on.expect("the scene parks the live run ON a node — the resume needs one");
    assert_eq!(
        resumed["node_id"],
        json!(parked_on),
        "the announcement names the node the run is parked on: {resumed}",
    );
    assert!(
        injected.contains(&format!("[{RESUMED_WARNING_CODE}]")),
        "the announcement is RELAYED to the session, not swallowed by the hook: {injected}",
    );

    sidecar.stop();
}

/// THE OTHER HALF OF THE SAME TURN: a reference that DESIGNATES NOTHING. `/nspec 99` must leave the
/// project exactly as it found it — no Spec authored, no run opened, not even an unbound one — and
/// must hand back a refusal a caller can ACT on.
///
/// "Actionable" is asserted as FACTS, never as a sentence: the typed code says the id is unknown,
/// the accepted forms come from the grammar's own constant, and the nearest candidate is named BY
/// ITS ID — the value the next call is formed from. That the refusal then TRAVELS is asserted
/// against the message the verb itself produced (so a re-wording follows rather than breaks), on
/// the THREE channels it has to reach: the block the hook injects (the only one the MODEL is ever
/// shown), the hook's stderr trace, and the `agent_session_event` filing that puts it in nyx.
///
/// THAT FIRST CHANNEL IS A REVERSAL OF WHAT THIS GATE ORIGINALLY FROZE. It asserted that a refused
/// `run_start` injects NOTHING — true of the hook the day this file was written, and wrong as a
/// requirement: Claude Code never feeds stderr to the model, so swallowing the refusal is nyx
/// saying nothing at all about a command it has just refused, leaving the session to improvise at
/// the exact spot where improvising authored the duplicate. `refusedReferenceBlock` relays it
/// instead. What must not be injected is a MANUAL (asserted here through `agent_context` never
/// being called) or a header naming a run that does not exist — and THAT is the half of the old
/// assertion that was load-bearing, kept below as the `run_id` guard.
#[test]
fn nspec_99_designates_nothing_so_it_opens_no_run_authors_no_spec_and_names_what_to_send() {
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
    let nspec_runs_before = scene.all_nspec_runs();
    let live_before = scene.run_row(&scene.live_run_id);

    let hook = run_hook("/nspec 99 reprends la spec", &scene.cwd, sidecar.port);
    assert_eq!(hook.code, 0, "the hook always exits 0");

    // The session takes its turn on whatever RUN it was handed — none, when the refusal held: the
    // block injected in that case names no run (asserted below), so there is nothing to take a turn
    // on. When the refusal did NOT hold, what reaches the session is a run carrying no Spec, and the
    // modelled session authors: that is what makes the Spec assertion below a lock on this leg too,
    // and not merely a restatement of "run_start does not create Specs".
    let injected = hook.injected();
    if let Some(run_id) = injected.as_deref().and_then(run_id_in) {
        an_nspec_session_authors_when_its_run_carries_no_spec(&scene, &run_id);
    }

    // Nothing was created, on either axis.
    assert_eq!(
        scene.specs(),
        specs_before,
        "`/nspec 99` authored no Spec: the project holds the same rows",
    );
    assert_eq!(
        scene.all_nspec_runs(),
        nspec_runs_before,
        "`/nspec 99` opened NO run — the refusal precedes every write",
    );
    assert_eq!(
        scene.run_row(&scene.live_run_id),
        live_before,
        "…and it left the run that was already live UNTOUCHED — every column of it, cursor and \
         updated_at included",
    );

    // The refusal itself, read off the verb that produced it.
    let refusal = sidecar
        .outcome_for(TOOL_RUN_START)
        .expect("run_start was called")
        .expect_err("`99` designates no Spec of this project, so run_start refuses");
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

    // `agent_context` is never even reached: no run, no manual (the hook must not degrade into
    // injecting a manual with no run to tick).
    assert!(
        !sidecar
            .tool_names()
            .contains(&TOOL_AGENT_CONTEXT.to_string()),
        "no run ⇒ no manual: {:?}",
        sidecar.tool_names(),
    );

    // NO MANUAL IS NOT NOTHING. The refusal reaches the three channels it has to, VERBATIM — the
    // server's own sentence, whatever it says, not a wording restated by the hook. Asserted against
    // the message the verb ACTUALLY produced above, so re-wording the refusal re-words these too.
    //
    // FIRST, THE ONLY ONE THE MODEL IS SHOWN: a block IS injected, and it is REQUIRED, not merely
    // tolerated (see this test's doc for the reversal). Two facts lock it — it relays the server's
    // sentence, and it names no run: `run_id_in` is the very reader a session uses to learn the id
    // it ticks, so a `None` here is that session finding nothing to hunt for, and the literal guard
    // beside it forbids even an empty header field.
    let injected = injected
        .expect("a refused reference is RELAYED to the session — the model is never shown stderr");
    assert!(
        injected.contains(&refusal.message),
        "the injected block carries the SERVER's sentence, not a hook-side paraphrase: {injected}",
    );
    assert!(
        run_id_in(&injected).is_none() && !injected.contains("run_id"),
        "…and names NO run: there is no run to tick, and an id-less header is exactly what sends a \
         session hunting for one: {injected}",
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

/// THE HARNESS CLEANS UP AFTER ITSELF. Several tests of this crate leave their temp directories
/// behind; this one may not, and saying so in a comment is not saying it at all — so the property is
/// a test: build a whole scene (temp dir, on-disk SQLite with its `-wal`/`-shm`, a live run), drop
/// it, and assert the directory is gone.
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

// ============================================================================
// Reading what came back.
// ============================================================================

/// The `run_id` out of the block the hook injected — read from the line the hook writes it on, which
/// is the only place the session ever learns it. `None` when the block names no run at all, which is
/// a REAL shape and not a parse failure: a refused `run_start` gets a block relaying the refusal, and
/// it deliberately carries no id (`refusedReferenceBlock`).
fn run_id_in(injected: &str) -> Option<String> {
    injected
        .lines()
        .find_map(|l| l.trim().strip_prefix("run_id:"))
        .map(|v| v.trim().to_string())
}

/// [`run_id_in`] where a run is the POINT of the injection — the replay leg, whose block is a run
/// header by definition, so an absent id is a failure of the thing under test and not a shape.
fn run_id_of(injected: &str) -> String {
    run_id_in(injected)
        .unwrap_or_else(|| panic!("the injected block names the run to tick: {injected}"))
}

/// The `code` of every entry in a `run_start` result's `warnings`.
fn warning_codes(started: &Value) -> Vec<String> {
    started["warnings"]
        .as_array()
        .map(|ws| {
            ws.iter()
                .filter_map(|w| w["code"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The single `warnings` entry carrying `code`.
fn warning_with_code(started: &Value, code: &str) -> Value {
    started["warnings"]
        .as_array()
        .and_then(|ws| ws.iter().find(|w| w["code"] == json!(code)).cloned())
        .unwrap_or_else(|| panic!("a '{code}' warning is present: {started}"))
}
