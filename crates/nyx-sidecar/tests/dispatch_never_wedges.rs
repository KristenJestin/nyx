//! REGRESSION guard for the sidecar's single request-serving thread (review
//! `01M0A7QGBK43MZB10RF51QR6TS`, finding `01M0A7S2DJQ9020C9JM74SEBTT`).
//!
//! ## What went wrong
//!
//! The sidecar answers ONE request at a time on its framed control loop, so a single command
//! that does not hand the thread back freezes the whole pipe. A dogfood session hit exactly
//! that: the sidecar reached `ready`, served 224 requests, then 46 CONSECUTIVE requests died
//! on the supervisor's 5 s deadline (`request id=NNN timed out`) — with the sidecar still
//! `ready`, no crash, no restart and nothing in its own log.
//!
//! MEASURED here through the real binary: the blocking command was **`pty_close`**. Its
//! `PtyManager::close` joined the output pump, and the pump's only exit condition was the
//! core reader dropping its `Sender` — i.e. the reader thread returning from `read()` on the
//! PTY master. On Windows that read never returns (the reader owns a CLONE of the ConPTY
//! master and `ClosePseudoConsole` does not reliably unblock it — the hazard
//! `nyx_core::pty::Pty`'s `Drop` already sidesteps by DETACHING that reader). So a plain
//! `pty_close` on a bare `cmd.exe` never answered (> 30 s) and killed the control loop with
//! it: the follow-up `ping` never answered either.
//!
//! ## What these tests hold
//!
//! Both drive the REAL `nyx-sidecar` binary over the framed pipe and give every request the
//! supervisor's OWN deadline. Nothing here asserts on a longer timeout than production uses,
//! so the guard cannot be satisfied by relaxing a delay — only by the loop staying free.
//!
//! Their SETUP is waited on the same way: each step is held until the terminal PRINTS the proof
//! it happened ([`Harness::await_output`]), never for a fixed duration. A guard whose setup is
//! timed rather than observed degrades in silence — on a loaded machine the sleep runs out
//! early, the hard shape is never reached, and the test keeps reporting green on the easy case
//! it was written to avoid (review `01M0AB3R53M22DJ2KJ3022DDG4`, finding
//! `01M0ABBQ7GVDWFTNDTZZZKA16E`).
//!
//!   * [`pty_close_answers_and_leaves_the_loop_serving`] — the narrow measurement: close a
//!     terminal whose shell left a LIVE grandchild attached to the PTY, then prove the loop
//!     still serves.
//!   * [`a_workspace_rename_delete_session_never_times_out`] — the finding's own acceptance
//!     shape: a full rename/delete session on a workspace that owns a live terminal, every
//!     request inside the deadline, the loop still serving afterwards.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    decode_message, encode_message, Event, Message, Request, Response, PROTOCOL_VERSION,
};

/// The supervisor's OWN per-request deadline (`Sidecar.requestRaw`'s default `timeoutMs`).
/// Every assertion below uses THIS budget, so a fix that merely bought more time on the
/// renderer side would leave these tests red.
const SUPERVISOR_DEADLINE: Duration = Duration::from_secs(5);

/// Boot is allowed more than a request: it opens + migrates SQLite on a cold temp dir.
const BOOT_DEADLINE: Duration = Duration::from_secs(30);

/// How long a marker printed by the shell (or by its grandchild) may take to come back as
/// terminal output. NOT a request budget — it covers a real shell starting on a loaded machine,
/// and nothing is asserted about the sidecar's responsiveness on it. It is generous on purpose:
/// exceeding it FAILS the test loudly rather than letting it fall through to the easy case.
const OUTPUT_DEADLINE: Duration = Duration::from_secs(20);

/// How long [`Harness::drop`] gives a graceful `Shutdown` before killing.
const TEARDOWN_DEADLINE: Duration = Duration::from_secs(5);

/// The shell each spawned terminal opens on. Pinned so the test does not inherit whatever
/// the developer's `$SHELL` happens to be.
#[cfg(windows)]
const SHELL: &str = "cmd.exe";
#[cfg(not(windows))]
const SHELL: &str = "/bin/sh";

// The two lines typed into the terminal, each paired with the marker its EXECUTION prints.
//
// Each marker is ASSEMBLED by the GRANDCHILD it proves is running — `^^` survives the typing
// shell's parse and is folded by the child's (`cmd`), `%s` is folded by `printf` (`sh`). So the
// literal appears in NOTHING the terminal echoes back on its own: not in the typed line, and not
// in the OSC window title `cmd` sets from the child's command line. A marker spelled out in the
// command would be "observed" the instant it was written, before the shell had run anything — an
// observable wait that observes nothing.

/// Typed first: its output is the proof the shell is UP and executing, which is what the write
/// below needs before it can mean anything.
#[cfg(windows)]
const SHELL_UP_LINE: &[u8] = b"cmd /c echo SHELL^^UP\r\n";
#[cfg(not(windows))]
const SHELL_UP_LINE: &[u8] = b"sh -c 'printf \"SHELL%s\\n\" UP'\n";
const SHELL_UP_MARKER: &[u8] = b"SHELLUP";

/// Typed next: it leaves a LONG-LIVED grandchild of the shell attached to the PTY — the case
/// where killing the session-leader shell alone cannot make the master EOF — and the grandchild
/// ANNOUNCES itself before parking, so the close below is known to run against a live one.
#[cfg(windows)]
const GRANDCHILD_LINE: &[u8] = b"cmd /c echo GRAND^^CHILD ^& ping -n 600 127.0.0.1\r\n";
#[cfg(not(windows))]
const GRANDCHILD_LINE: &[u8] = b"sh -c 'printf \"GRAND%s\\n\" CHILD; sleep 600'\n";
const GRANDCHILD_MARKER: &[u8] = b"GRANDCHILD";

/// The cursor-position REQUEST (`DSR-CPR`) ConPTY writes as it starts, and the answer it waits
/// for before handing the console to the shell.
///
/// A real pane answers this without being asked — it is an xterm.js instance, and replying to
/// `ESC[6n` is part of being a terminal. This harness is not one, so it has to answer by hand:
/// MEASURED, without the reply the shell's banner, its prompt and every command's output stay
/// behind the unanswered query and the terminal produces nothing but the query itself. Nothing
/// sends it where there is no ConPTY, so the answer is only written if the query is seen.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";
const CURSOR_ANSWER: &[u8] = b"\x1b[1;1R";

fn sidecar_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

fn temp_data_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-sidecar-wedge-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Harness over the real binary. A background thread drains stdout into a channel of decoded
/// messages, so every receive here is BOUNDED — a wedged sidecar shows up as an expired
/// deadline, never as a hung test.
struct Harness {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Message>,
    /// Everything the terminals have PRINTED, accumulated by the reader thread itself.
    ///
    /// It lives here rather than being read off `rx` because [`Harness::await_response`] drops
    /// every event it walks past: a marker that landed while a request was in flight would be
    /// gone by the time anyone looked for it. Accumulating also means a marker split across two
    /// coalesced output frames is still found.
    output: Arc<Mutex<Vec<u8>>>,
    data_dir: std::path::PathBuf,
    next_id: u64,
}

impl Harness {
    fn boot(tag: &str) -> Self {
        let data_dir = temp_data_dir(tag);
        let mut child = Command::new(sidecar_bin())
            .env("NYX_DATA_DIR", &data_dir)
            .env("SHELL", SHELL)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn nyx-sidecar");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let reader_output = Arc::clone(&output);
        std::thread::Builder::new()
            .name("wedge-test-reader".into())
            .spawn(move || {
                let mut dec = FrameDecoder::new();
                let mut buf = [0u8; 64 * 1024];
                loop {
                    loop {
                        match dec.next_frame() {
                            Ok(Some(payload)) => {
                                if let Ok(m) = decode_message(&payload) {
                                    if let Message::Event(Event::TerminalOutput { data, .. }) = &m {
                                        reader_output.lock().unwrap().extend_from_slice(data);
                                    }
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
                        Ok(0) | Err(_) => return,
                        Ok(n) => dec.push(&buf[..n]),
                    }
                }
            })
            .unwrap();

        let mut h = Self {
            child,
            stdin,
            rx,
            output,
            data_dir,
            next_id: 1,
        };
        let id = h.send(|id| Request::Handshake {
            id,
            protocol_version: PROTOCOL_VERSION,
        });
        match h.await_response(id, BOOT_DEADLINE, "handshake") {
            Response::HandshakeOk { .. } => h,
            other => panic!("handshake failed: {other:?}"),
        }
    }

    /// Frame + write one request, returning the correlation id it was given.
    fn send(&mut self, build: impl FnOnce(u64) -> Request) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        assert!(self.try_write(build(id)), "write request id={id}");
        id
    }

    /// Frame + write one request, reporting failure instead of panicking — the shape
    /// [`Harness::drop`] needs, since a panic there during an unwind aborts the process and the
    /// assertion message with it.
    fn try_write(&mut self, req: Request) -> bool {
        let bytes = encode_message(&Message::Request(req));
        let Ok(frame) = encode_frame(&bytes) else {
            return false;
        };
        self.stdin.write_all(&frame).is_ok() && self.stdin.flush().is_ok()
    }

    /// Await the response correlated to `id`, FAILING (never hanging) past `deadline`. The
    /// elapsed time is reported on failure so a regression names its own duration.
    fn await_response(&self, id: u64, deadline: Duration, what: &str) -> Response {
        let started = Instant::now();
        let until = started + deadline;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                panic!(
                    "`{what}` (id={id}) never answered within {deadline:?} — the sidecar's \
                     request loop is wedged (waited {:?})",
                    started.elapsed()
                );
            }
            match self.rx.recv_timeout(left) {
                Ok(Message::Response(r)) if response_id(&r) == id => return r,
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => panic!(
                    "`{what}` (id={id}) never answered within {deadline:?} — the sidecar's \
                     request loop is wedged (waited {:?})",
                    started.elapsed()
                ),
            }
        }
    }

    /// Send a request and await its reply within the SUPERVISOR's deadline.
    fn request(&mut self, what: &str, build: impl FnOnce(u64) -> Request) -> Response {
        let id = self.send(build);
        self.await_response(id, SUPERVISOR_DEADLINE, what)
    }

    /// Write `data` to a terminal (no output expectation).
    fn type_line(&mut self, terminal_id: u32, data: &[u8]) {
        self.request("pty_write", |id| Request::PtyWrite {
            id,
            terminal_id,
            data: data.to_vec(),
        });
    }

    /// WAIT until the terminals have printed `marker`, FAILING past [`OUTPUT_DEADLINE`].
    ///
    /// This is what a fixed `sleep` cannot do: a sleep that turns out to be too short on a loaded
    /// machine leaves the test GREEN while it silently stops covering the shape it names (a shell
    /// that is really up; a grandchild that is really running), whereas a missed marker here is a
    /// failure that says so.
    fn await_output(&self, marker: &[u8], what: &str) {
        let started = Instant::now();
        while !contains(&self.output.lock().unwrap(), marker) {
            assert!(
                started.elapsed() < OUTPUT_DEADLINE,
                "`{what}` never printed {:?} within {OUTPUT_DEADLINE:?} — the terminal is not in \
                 the state this test claims to close",
                String::from_utf8_lossy(marker)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Type `line` into the terminal and wait for the marker its EXECUTION prints.
    fn type_and_await(&mut self, terminal_id: u32, line: &[u8], marker: &[u8], what: &str) {
        self.type_line(terminal_id, line);
        self.await_output(marker, what);
    }

    /// Answer ConPTY's startup cursor query so the shell can start printing at all — see
    /// [`CURSOR_QUERY`]. Nothing to answer where there is no ConPTY.
    fn answer_cursor_query(&mut self, terminal_id: u32) {
        if cfg!(not(windows)) {
            return;
        }
        self.await_output(CURSOR_QUERY, "ConPTY");
        self.type_line(terminal_id, CURSOR_ANSWER);
    }

    /// Run a db-command and return its parsed JSON result.
    fn db(&mut self, command: &str, args: serde_json::Value) -> serde_json::Value {
        let res = self.request(command, |id| Request::DbCommand {
            id,
            command: command.to_string(),
            args_json: args.to_string(),
        });
        match res {
            Response::DbResult { result_json, .. } => {
                serde_json::from_str(&result_json).expect("db result is json")
            }
            other => panic!("`{command}` failed: {other:?}"),
        }
    }

    /// The control-loop LIVENESS probe: a `ping` still answered means the loop was handed
    /// back. This is the assertion the wedge broke — after the blocking `pty_close`, not one
    /// further request was ever served.
    fn assert_loop_still_serving(&mut self) {
        let res = self.request("ping", |id| Request::Ping { id, nonce: 0xB0_0B });
        assert!(
            matches!(res, Response::Pong { nonce: 0xB0_0B, .. }),
            "the control loop must still serve after the teardown, got {res:?}"
        );
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // GRACEFUL FIRST, bounded, and never panicking — the teardown convention of the sidecar
        // harnesses next door (pty_flow.rs, command_mcp.rs, lifecycle.rs).
        //
        // It matters most on the path this file exists for. A hard `kill` denies the sidecar the
        // `close_all` it runs on the way out, and the shells it spawned via `setsid` are in no
        // group anything would reap afterwards: off Windows — where the job object's
        // kill-on-job-close covers it — the `sleep 600` grandchild these tests start would
        // outlive the run by ten minutes. A `Shutdown` lets the sidecar tear its own PTYs down.
        //
        // Every step is best-effort. `Drop` also runs while a failed assertion is unwinding, and
        // a second panic during an unwind ABORTS the process — which would replace the failure
        // message this file works hard to produce with nothing at all.
        let id = self.next_id;
        self.next_id += 1;
        if self.try_write(Request::Shutdown { id }) {
            // Wait on the sidecar's EXIT rather than on the `ShutdownAck` frame: the exit is the
            // stronger evidence (it implies the ack and the `close_all` behind it), and a wedged
            // sidecar — the very state a failing test here leaves behind — simply never gets
            // there, so the kill below is reached on the bound instead of hanging the teardown.
            let until = Instant::now() + TEARDOWN_DEADLINE;
            while Instant::now() < until {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        // The fallback, not the plan: a sidecar that never shut down (wedged, or already dead).
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::fs::remove_dir_all(&self.data_dir).ok();
    }
}

/// Whether `haystack` carries `needle` anywhere.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn response_id(r: &Response) -> u64 {
    match r {
        Response::HandshakeOk { id, .. }
        | Response::Pong { id, .. }
        | Response::ShutdownAck { id }
        | Response::Error { id, .. }
        | Response::PtySpawned { id, .. }
        | Response::Ok { id }
        | Response::DbResult { id, .. }
        | Response::CommandStatus { id, .. }
        | Response::McpStarted { id, .. } => *id,
    }
}

fn spawn_terminal_pty(h: &mut Harness, record_id: &str) -> u32 {
    let res = h.request("pty_spawn", |id| Request::PtySpawn {
        id,
        cols: 80,
        rows: 24,
        cwd: String::new(),
        record_id: record_id.to_string(),
    });
    let terminal_id = match res {
        Response::PtySpawned { terminal_id, .. } => terminal_id,
        other => panic!("pty_spawn failed: {other:?}"),
    };
    h.answer_cursor_query(terminal_id);
    terminal_id
}

/// THE MEASUREMENT, as a guard: closing a terminal must answer inside the supervisor's own
/// deadline AND leave the control loop serving. Driven on the hardest shape — a shell that
/// left a live grandchild attached to the PTY, so killing the session leader alone cannot
/// make the master EOF.
#[test]
fn pty_close_answers_and_leaves_the_loop_serving() {
    let mut h = Harness::boot("close");
    let terminal_id = spawn_terminal_pty(&mut h, "rec-close");
    // Both waits are on OUTPUT, not on a duration: the shell has answered a command, then the
    // grandchild has announced itself. Reaching the close below therefore MEANS the hard shape is
    // set up — where a fixed sleep would have let a loaded machine quietly downgrade this to the
    // easy case (a shell with no live descendant) while still reporting green.
    h.type_and_await(terminal_id, SHELL_UP_LINE, SHELL_UP_MARKER, "the shell");
    h.type_and_await(
        terminal_id,
        GRANDCHILD_LINE,
        GRANDCHILD_MARKER,
        "the grandchild",
    );

    let started = Instant::now();
    let res = h.request("pty_close", |id| Request::PtyClose { id, terminal_id });
    let elapsed = started.elapsed();
    assert!(
        matches!(res, Response::Ok { .. }),
        "pty_close must succeed, got {res:?}"
    );
    eprintln!("pty_close (live grandchild) answered in {elapsed:?}");
    h.assert_loop_still_serving();
}

/// The finding's ACCEPTANCE shape: "une session complète de manipulation rename/delete de
/// workspace ne produit plus aucun timeout". A workspace that owns a LIVE terminal is renamed
/// and then deleted — `workspace_delete` stops the workspace's command instances and CLOSES
/// every one of its terminal PTYs before delegating, which is where the wedge lived — and
/// every request in the session must land inside the supervisor's deadline, with the loop
/// still serving at the end.
#[test]
fn a_workspace_rename_delete_session_never_times_out() {
    let mut h = Harness::boot("session");
    let root = h.data_dir.to_string_lossy().to_string();

    let created = h.db(
        "create_project",
        serde_json::json!({ "name": "Wedge", "rootPath": root }),
    );
    let project_id = created["project"]["id"]
        .as_str()
        .expect("project id")
        .to_string();

    // Its OWN root path (a workspace's `root_path` is unique per project).
    let wt_path = h.data_dir.join("wt");
    std::fs::create_dir_all(&wt_path).expect("worktree dir");
    let wt_path = wt_path.to_string_lossy().to_string();
    let workspace = h.db(
        "create_workspace",
        serde_json::json!({ "projectId": project_id, "name": "wt", "path": wt_path }),
    );
    let workspace_id = workspace["id"].as_str().expect("workspace id").to_string();

    // A terminal RECORD attached to that workspace, backed by a LIVE PTY — so the delete has
    // real PTYs to tear down (`terminal_ids_for_workspace` → `close_record`).
    let terminal = h.db("create_terminal", serde_json::json!({ "cwd": root }));
    let record_id = terminal["id"].as_str().expect("terminal id").to_string();
    h.db(
        "attach_terminal",
        serde_json::json!({ "terminalId": record_id, "workspaceId": workspace_id, "mode": "manual" }),
    );
    let terminal_id = spawn_terminal_pty(&mut h, &record_id);
    // Same rule as above: the delete must land on a terminal whose shell is REALLY up, and the
    // proof is a marker it printed — not a delay it was given.
    h.type_and_await(terminal_id, SHELL_UP_LINE, SHELL_UP_MARKER, "the shell");

    // The session the user ran: rename, then remove.
    h.db(
        "rename_workspace",
        serde_json::json!({ "id": workspace_id, "name": "wt-renamed" }),
    );
    let started = Instant::now();
    h.db(
        "workspace_delete",
        serde_json::json!({ "id": workspace_id }),
    );
    eprintln!("workspace_delete answered in {:?}", started.elapsed());

    // The workspace is gone AND the loop is still answering — the two halves the wedge broke.
    let list = h.db(
        "list_workspaces",
        serde_json::json!({ "projectId": project_id }),
    );
    assert!(
        !list
            .as_array()
            .expect("workspace list")
            .iter()
            .any(|w| w["id"].as_str() == Some(workspace_id.as_str())),
        "the workspace must be gone after workspace_delete"
    );
    h.assert_loop_still_serving();
}
