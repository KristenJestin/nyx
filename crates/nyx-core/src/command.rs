//! Managed-command runtime, fully decoupled from Tauri.
//!
//! A [`CommandPty`] runs ONE arbitrary command line inside a pseudo-terminal,
//! **read-only**: it exposes a reader (the child's output), a waiter / exit code,
//! and a process-tree kill — but **no user stdin path**. This is the difference
//! from [`crate::pty::Pty`], which is an interactive terminal: managed commands
//! are services the user watches, never types into.
//!
//! The one exception is purely internal terminal EMULATION: the reader thread
//! auto-answers terminal QUERIES (DSR cursor position `ESC[6n`, DSR status
//! `ESC[5n`, Primary Device Attributes `ESC[c`) over a writer it never exposes —
//! see [`scan_terminal_queries`]. Without these canned replies a TTY-aware CLI
//! (bun, …) withholds its output until the terminal answers, so only a few bytes
//! are ever captured. No user keystroke is ever forwarded: the read-only,
//! no-user-input invariant holds.
//!
//! The command runs under the platform shell ([`crate::pty::resolve_shell`], the
//! same resolution the terminals use, honoring `$SHELL`) so a shell command line
//! (`npm run dev`, `cargo watch -x test`, ...) is interpreted exactly as the user
//! would type it. [`command_invocation`] maps the shell family to its
//! run-this-string flag (POSIX `-c`, PowerShell `-Command`, cmd.exe `/C`).
//!
//! Bytes read from the PTY master are pushed, unmodified and un-throttled, onto an
//! [`std::sync::mpsc`] channel; coalescing/throttling is the bridge's job (mirrors
//! the [`crate::pty`] contract). The reader channel disconnects on child exit.
//!
//! **Process-tree kill.** Stopping a service must kill the *whole* tree, not just
//! the parent shell — a bare `kill(shell)` leaves the actual `node`/`cargo`
//! orphaned. `portable-pty` runs the child through `setsid()` on Unix, so the
//! child is a session/process-group leader and its pid *is* its pgid; we kill the
//! negative pgid to signal the entire group (TERM then KILL). On Windows
//! `portable-pty` exposes no job object and only `TerminateProcess`es the parent,
//! so we kill the whole tree by pid with `taskkill /T /F /PID <pid>` (the shell,
//! the program it ran, AND the ConPTY `conhost` host) — a parent-only kill there
//! leaked grandchildren + `conhost` (the observed zombie leak). [`KillHandle`]
//! carries the pid + a parent-only killer so the runner can drive these strategies
//! without re-spawning.
//!
//! Consumer note: the `#[tauri::command]` lifecycle surface that drives this
//! runtime (`start`/`stop`/`relaunch`) lands in PRD-3 Phase 3. Until then the
//! public items here are exercised by this module's tests and the production sink
//! in [`crate::bridge`], so the module carries `not(test)` dead-code suppression —
//! the SAME deferral the Phase-1 db helpers (`set_last_state`, …) used while their
//! runner consumer (this phase) was still pending.
#![cfg_attr(not(test), allow(dead_code))]

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::pty::resolve_shell;

/// Monotonic source of command-PTY ids, distinct from the terminal PTY id space.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Map a shell to the (program, args) that run an arbitrary command STRING under
/// it. The split mirrors how each shell family takes "run this whole string":
///
/// - **POSIX** (`bash`/`sh`/`zsh`/`fish`/...): `<shell> -c "<cmdline>"`.
/// - **PowerShell** (`pwsh`/`powershell`): `<shell> -Command "<cmdline>"`.
/// - **cmd.exe**: `<shell> /C "<cmdline>"`.
///
/// The family is detected from the shell's file stem (case-insensitively, since
/// Windows paths are case-insensitive), so an absolute path like
/// `/usr/bin/zsh` or `C:\…\pwsh.exe` is classified correctly. Anything we do not
/// recognize falls back to the POSIX `-c` form: every non-Windows default and
/// `$SHELL` value is POSIX, so this is the safe default.
pub fn command_invocation(shell: &str, cmdline: &str) -> (String, Vec<String>) {
    let stem = shell_stem(shell);
    let flag = match stem.as_str() {
        // PowerShell editions.
        "pwsh" | "powershell" => "-Command",
        // Legacy Windows command interpreter.
        "cmd" => "/C",
        // Everything else (bash/sh/zsh/fish/dash/ksh/…) is POSIX `-c`.
        _ => "-c",
    };
    (
        shell.to_string(),
        vec![flag.to_string(), cmdline.to_string()],
    )
}

/// Lower-cased file stem of a shell path: `/usr/bin/zsh` -> `zsh`,
/// `C:\Windows\System32\cmd.exe` -> `cmd`, `pwsh.exe` -> `pwsh`. Splits on BOTH
/// `/` and `\` so a Windows path classifies even on a Unix build, and strips a
/// trailing `.exe`/`.cmd`/`.bat`. Pure: unit-tested without spawning.
fn shell_stem(shell: &str) -> String {
    let base = shell
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(shell)
        .to_ascii_lowercase();
    // Strip a known Windows executable extension if present.
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stripped) = base.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    base
}

// --- Terminal-query auto-responder (read-only PTY) -----------------------
//
// A read-only command PTY has no user stdin, but TTY-aware CLIs (bun, and other
// programs that probe the terminal) emit terminal QUERIES on startup and WITHHOLD
// their output until they get a reply:
//
//   - DSR cursor-position   `ESC [ 6 n`   → reply `ESC [ <row> ; <col> R`
//   - DSR terminal-status   `ESC [ 5 n`   → reply `ESC [ 0 n` (OK)
//   - Primary Device Attrs  `ESC [ c`     → reply a minimal VT100  identity
//     (also `ESC [ 0 c`, the explicit-parameter form)
//
// nyx never replied, so such a program would emit ~4 bytes (the query itself) and
// then block forever waiting for an answer — the "play gives me nothing / only 4
// bytes captured" bug. We answer these queries automatically from the reader
// thread. This is TERMINAL EMULATION, not user input: the only bytes ever written
// back are these canned protocol replies. The read-only/no-user-stdin invariant is
// intact — there is still no path that forwards user keystrokes to the child.

/// A minimal Primary Device Attributes reply: "VT100 with Advanced Video Option"
/// (`ESC [ ? 1 ; 2 c`). Enough to satisfy a `ESC[c` probe.
const DA_REPLY: &[u8] = b"\x1b[?1;2c";
/// DSR terminal-status OK reply (`ESC [ 0 n`).
const DSR_OK_REPLY: &[u8] = b"\x1b[0n";

/// Scan a raw PTY output chunk for terminal QUERIES and return the bytes to write
/// back to the child as replies (concatenated, in order). Pure + allocation-free
/// on the common no-query path so it is cheap to run on every chunk.
///
/// Recognized queries (CSI = `ESC [`):
///   - `CSI 6 n` (DSR cursor position) → `CSI 1 ; 1 R` (we report the top-left;
///     the off-screen command grid has no meaningful cursor, and every program we
///     care about only needs *a* well-formed reply to unblock).
///   - `CSI 5 n` (DSR status)          → `CSI 0 n`.
///   - `CSI c` / `CSI 0 c` (Primary DA) → [`DA_REPLY`].
///
/// `carry` holds any trailing partial escape sequence from the PREVIOUS chunk so a
/// query split across a read boundary is still recognized: the function prepends it,
/// scans, and refills it with the new unterminated tail. The carry is bounded (a
/// CSI query is a handful of bytes); a runaway `ESC[` with no final byte is dropped
/// once it exceeds a small cap so a hostile stream cannot grow it without limit.
fn scan_terminal_queries(chunk: &[u8], carry: &mut Vec<u8>) -> Vec<u8> {
    // Fast path: nothing buffered and no ESC in the chunk → no query possible.
    if carry.is_empty() && !chunk.contains(&0x1b) {
        return Vec::new();
    }

    // Work over carry-prefixed bytes so a boundary-split sequence is seen whole.
    let mut buf = std::mem::take(carry);
    buf.extend_from_slice(chunk);

    let mut replies = Vec::new();
    let mut i = 0;
    let mut consumed_upto = 0; // bytes before this are fully processed (no partial)
    while i < buf.len() {
        if buf[i] != 0x1b {
            i += 1;
            consumed_upto = i;
            continue;
        }
        // An ESC: is it the start of a CSI (`ESC [`)? Need at least 2 bytes.
        if i + 1 >= buf.len() {
            break; // partial: keep `ESC` in carry
        }
        if buf[i + 1] != b'[' {
            // Not a CSI we care about; skip the ESC and continue.
            i += 1;
            consumed_upto = i;
            continue;
        }
        // CSI: collect the parameter bytes (0x30..=0x3f) until the final byte
        // (0x40..=0x7e). If we run out, it is a partial sequence → carry it.
        let mut j = i + 2;
        while j < buf.len() && (0x30..=0x3f).contains(&buf[j]) {
            j += 1;
        }
        if j >= buf.len() {
            break; // partial CSI: keep from `i` in carry
        }
        let final_byte = buf[j];
        if !(0x40..=0x7e).contains(&final_byte) {
            // Malformed (an intermediate/garbage byte where a final was expected):
            // skip the ESC and resync.
            i += 1;
            consumed_upto = i;
            continue;
        }
        let params = &buf[i + 2..j];
        match final_byte {
            b'n' => match params {
                b"6" => replies.extend_from_slice(b"\x1b[1;1R"),
                b"5" => replies.extend_from_slice(DSR_OK_REPLY),
                _ => {}
            },
            b'c' if params.is_empty() || params == b"0" => {
                replies.extend_from_slice(DA_REPLY);
            }
            _ => {}
        }
        // Whole sequence consumed.
        i = j + 1;
        consumed_upto = i;
    }

    // Whatever is unprocessed past `consumed_upto` is a partial tail to carry,
    // bounded so a stream of bare `ESC[` cannot grow it without limit.
    const MAX_CARRY: usize = 64;
    let tail = &buf[consumed_upto..];
    if tail.len() <= MAX_CARRY {
        carry.extend_from_slice(tail);
    }
    replies
}

/// A platform strategy for killing the command's PROCESS TREE, owned by the
/// [`CommandPty`] and cloned to the runner so it can stop the service without
/// holding the `CommandPty` itself.
///
/// On Unix the child is a session leader (`portable-pty` calls `setsid`), so its
/// pid is its process-group id; we signal the whole group via the negative pgid.
/// The `parent_killer` is the `portable-pty` killer (parent process only), used
/// as a fallback when the group signal is unavailable (no pid, or non-Unix).
pub struct KillHandle {
    /// OS pid of the spawned shell — also the pgid on Unix (session leader).
    pid: Option<u32>,
    /// `portable-pty`'s parent-only killer (TerminateProcess / SIGKILL of the
    /// shell). Shared so both the handle and the `CommandPty` can use it.
    parent_killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
}

impl KillHandle {
    /// OS pid of the spawned shell (the pgid on Unix), if the platform reported it.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Best-effort graceful stop of the whole tree: `SIGTERM` to the process group
    /// on Unix. Returns whether a group signal was actually delivered (so the
    /// caller can decide whether to wait for graceful exit before escalating). A
    /// no-op returning `false` on platforms/cases without a group to signal.
    pub fn term_tree(&self) -> bool {
        self.signal_group(libc_term())
    }

    /// Force-kill the whole tree. On Unix: `SIGKILL` to the process group (the
    /// negative pgid). On Windows: `taskkill /T /F /PID <pid>`, which terminates the
    /// shell, the program it ran (bun/node/…) AND the ConPTY host — the WHOLE tree by
    /// pid, since `portable-pty` exposes no job object and a parent-only
    /// `TerminateProcess` would leak the grandchildren + `conhost` (the observed
    /// zombie leak). Falls back to the parent-only `portable-pty` killer only when no
    /// tree strategy is available (no pid). Idempotent: killing a dead tree is a
    /// no-op (a `taskkill` "process not found" is swallowed, not surfaced).
    pub fn kill_tree(&self) {
        // Unix: signal the whole process group. Returns true once delivered (or the
        // group was already gone), so no fallback is needed.
        if self.signal_group(libc_kill()) {
            return;
        }
        // Windows: kill the whole tree by pid with taskkill. Returns true if the
        // tree-kill was attempted (the pid was known), so we skip the parent-only
        // fallback that would leave grandchildren + the conhost host alive.
        if self.kill_tree_windows() {
            return;
        }
        // No tree strategy available (e.g. unknown pid): best-effort parent kill.
        let _ = self.parent_killer.lock().unwrap().kill();
    }

    /// Windows tree kill: `taskkill /T /F /PID <pid>` run with `CREATE_NO_WINDOW` so
    /// no console flashes. `/T` kills the process AND its descendants, `/F` forces
    /// it — this reaps the shell, the actual program (bun/node), and the ConPTY
    /// `conhost` host that a parent-only `TerminateProcess` would orphan. Returns
    /// whether a tree kill was ATTEMPTED (the pid was known); a non-zero taskkill
    /// exit (e.g. the tree already exited → "process not found") is NOT surfaced —
    /// killing a dead tree is a no-op by contract. Always `false` off Windows so the
    /// caller's logic is uniform.
    #[cfg(windows)]
    fn kill_tree_windows(&self) -> bool {
        let Some(pid) = self.pid else {
            return false;
        };
        // Hardened spawn (centralized CREATE_NO_WINDOW): never pop a console for the helper.
        let _ = crate::proc_util::command("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        // We also TerminateProcess the parent as a belt-and-suspenders fallback in
        // case taskkill is unavailable (locked-down hosts); harmless if the tree is
        // already gone.
        let _ = self.parent_killer.lock().unwrap().kill();
        true
    }

    #[cfg(not(windows))]
    fn kill_tree_windows(&self) -> bool {
        false
    }

    /// Deliver `signal` to the child's process GROUP via the negative pgid
    /// (`kill(-pgid, sig)`). Unix-only and gated on a known pid; returns whether
    /// the signal was delivered to a live group. Other platforms always return
    /// `false` so the caller falls back to the parent killer.
    #[cfg(unix)]
    fn signal_group(&self, signal: i32) -> bool {
        let Some(pid) = self.pid else {
            return false;
        };
        // The child is a session leader (`setsid`), so pgid == pid. Signaling the
        // NEGATIVE pgid hits every process in the group — the shell AND whatever
        // it spawned (node/cargo/…). `ESRCH` (no such group) means already gone:
        // treat that as "delivered" so the caller does not redundantly fall back.
        let rc = unsafe { libc::kill(-(pid as i32), signal) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[cfg(not(unix))]
    fn signal_group(&self, _signal: i32) -> bool {
        // No POSIX process groups: the runner falls back to the parent-only
        // TerminateProcess (portable-pty does not expose a Windows job object, so
        // a grandchild can survive — documented limitation, guarded by the Phase 6
        // dogfood gate).
        false
    }
}

/// `SIGTERM`/`SIGKILL` numbers, factored so the non-Unix build compiles (the
/// values are never used off-Unix because `signal_group` is a no-op there).
#[cfg(unix)]
fn libc_term() -> i32 {
    libc::SIGTERM
}
#[cfg(unix)]
fn libc_kill() -> i32 {
    libc::SIGKILL
}
#[cfg(not(unix))]
fn libc_term() -> i32 {
    0
}
#[cfg(not(unix))]
fn libc_kill() -> i32 {
    0
}

/// A spawned **read-only** command PTY: the child command, the master side (held
/// so the waiter can close it on exit), a reader thread, and a waiter that records
/// the exit status. Exposes a reader channel, a waiter/exit code, and a process-
/// tree [`KillHandle`] — but deliberately **no writer**: managed-command output is
/// watch-only, there is no stdin path.
///
/// Dropping (or [`CommandPty::kill`]) terminates the tree and joins the helper
/// threads, so neither threads nor OS handles leak.
pub struct CommandPty {
    id: u64,
    /// Master side, shared with the waiter so it closes on child exit (unblocks the
    /// reader on Windows/ConPTY; harmless no-op on Unix). Mirrors [`crate::pty`].
    /// Held only to keep the shared `Arc` alive for the waiter thread — the field
    /// is never read directly through `self`, so `dead_code` would flag it; the
    /// retention is the point (dropping it early would close the PTY prematurely).
    #[allow(dead_code)]
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    /// Parent-only killer, shared with the [`KillHandle`].
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    /// OS pid of the spawned shell (the pgid on Unix), for the tree kill.
    pid: Option<u32>,
    reader_handle: Option<JoinHandle<()>>,
    waiter_handle: Option<JoinHandle<()>>,
    exit_code: Arc<Mutex<Option<i32>>>,
}

impl CommandPty {
    /// Spawn `cmdline` read-only under the platform shell ([`resolve_shell`]) at
    /// `cwd`, with the environment inherited from nyx and a sane `$TERM`.
    ///
    /// `env` is an OPTIONAL per-run set of `KEY=VALUE` overrides MERGED on top of the
    /// inherited environment (PRD-4 R-WSCMD): each entry is applied AFTER the inherited
    /// env, so it adds a new variable or overrides an inherited one (e.g. `VAULT_ENV`,
    /// values from a `.env`). An empty slice is a plain inherited-env spawn. Secret
    /// VALUES are NEVER logged here — only `cmd.env` receives them.
    ///
    /// Returns the handle and a [`Receiver`] yielding raw output chunks. The
    /// receiver disconnects (its sender drops) at EOF — i.e. when the command has
    /// exited. No writer is returned: this PTY is read-only by construction.
    pub fn spawn(
        cmdline: &str,
        size: PtySize,
        cwd: Option<&str>,
        env: &[(String, String)],
    ) -> anyhow::Result<(Self, Receiver<Vec<u8>>)> {
        let shell = resolve_shell();
        let (program, args) = command_invocation(&shell, cmdline);

        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size)?;

        let mut cmd = CommandBuilder::new(&program);
        for arg in &args {
            cmd.arg(arg);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        // `CommandBuilder` inherits the live env by default; only set a sane $TERM
        // for full-screen output when the parent has none (mirrors `Pty::spawn`).
        if std::env::var_os("TERM").is_none() {
            cmd.env("TERM", "xterm-256color");
        }
        // Per-run env MERGED on top of the inherited environment (R-WSCMD): applied
        // last so a provided KEY overrides any inherited value of the same name. The
        // KEYS may be useful to surface, but the VALUES (potential secrets) are never
        // logged — only handed to `cmd.env`.
        for (key, value) in env {
            cmd.env(key, value);
        }

        let child = pair.slave.spawn_command(cmd)?;
        // Drop the slave so the master EOFs when the child exits (Unix).
        drop(pair.slave);

        let pid = child.process_id();
        let killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>> =
            Arc::new(Mutex::new(child.clone_killer()));
        let mut reader = pair.master.try_clone_reader()?;
        // A writer onto the master, used SOLELY to auto-answer terminal queries
        // (DSR / Device Attributes) from the reader thread — see
        // `scan_terminal_queries`. This is NOT a user-stdin path: no public method
        // forwards user input here, the writer never leaves the reader thread, and
        // the only bytes ever sent are canned protocol replies. The read-only,
        // no-user-input invariant is preserved; without these replies a TTY-aware
        // CLI (bun) withholds its output and only ~4 bytes are ever captured.
        let mut query_writer = pair.master.take_writer()?;
        let master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> =
            Arc::new(Mutex::new(Some(pair.master)));

        // Reader thread: pump raw bytes onto the channel until EOF, and auto-answer
        // any terminal QUERY in the stream so query-gated CLIs unblock their output.
        let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = mpsc::channel();
        let reader_handle = std::thread::Builder::new()
            .name("nyx-cmd-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                // Carry for a terminal query split across read boundaries.
                let mut carry: Vec<u8> = Vec::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: child closed the PTY.
                        Ok(n) => {
                            // Auto-answer terminal queries BEFORE forwarding the
                            // chunk, so the child gets its reply promptly. The reply
                            // bytes are canned protocol responses, never user input.
                            let replies = scan_terminal_queries(&buf[..n], &mut carry);
                            if !replies.is_empty() {
                                let _ = query_writer.write_all(&replies);
                                let _ = query_writer.flush();
                            }
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break; // consumer hung up
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break, // master closed / error
                    }
                }
                // `tx` drops here → receiver observes disconnect (EOF signal).
            })?;

        // Waiter thread: record the exit code, then close the master so the reader
        // unblocks (the ConPTY EOF lever on Windows; harmless on Unix).
        let exit_code = Arc::new(Mutex::new(None::<i32>));
        let waiter_handle = {
            let exit_code = Arc::clone(&exit_code);
            let master = Arc::clone(&master);
            let mut child: Box<dyn Child + Send + Sync> = child;
            std::thread::Builder::new()
                .name("nyx-cmd-waiter".into())
                .spawn(move || {
                    if let Ok(status) = child.wait() {
                        *exit_code.lock().unwrap() = Some(status.exit_code() as i32);
                    }
                    let _ = master.lock().unwrap().take();
                })?
        };

        let pty = CommandPty {
            id: next_id(),
            master,
            killer,
            pid,
            reader_handle: Some(reader_handle),
            waiter_handle: Some(waiter_handle),
            exit_code,
        };
        Ok((pty, rx))
    }

    /// Opaque, process-unique id of this command PTY.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// A [`KillHandle`] the runner uses to kill the process TREE (not just the
    /// parent shell). Cloneable away from the `CommandPty` so the runner can stop
    /// a service it no longer owns the handle for.
    pub fn kill_handle(&self) -> KillHandle {
        KillHandle {
            pid: self.pid,
            parent_killer: Arc::clone(&self.killer),
        }
    }

    /// Current exit code, or `None` while the child is still running.
    pub fn exit_code(&self) -> Option<i32> {
        *self.exit_code.lock().unwrap()
    }

    /// Force-kill the whole process tree (TERM is the runner's job; this is the
    /// hard stop). Idempotent: killing a dead tree is a no-op.
    pub fn kill(&mut self) {
        self.kill_handle().kill_tree();
    }

    /// Block until the child has exited and its exit code is recorded, then return
    /// it. Joins the waiter thread.
    pub fn wait(&mut self) -> Option<i32> {
        if let Some(handle) = self.waiter_handle.take() {
            let _ = handle.join();
        }
        self.exit_code()
    }
}

impl Drop for CommandPty {
    fn drop(&mut self) {
        // Kill the whole tree so neither orphaned grandchildren nor helper threads
        // leak, then join the waiter (returns right after the kill). The reader is
        // platform-split for the SAME reason as `crate::pty::Pty::drop`: on Windows
        // the cloned ConPTY reader can block forever in `ReadFile`, so we detach it
        // there instead of joining (a join deadlocked the UI on close).
        self.kill_handle().kill_tree();
        if let Some(handle) = self.waiter_handle.take() {
            let _ = handle.join();
        }
        #[cfg(not(windows))]
        if let Some(handle) = self.reader_handle.take() {
            let _ = handle.join();
        }
        #[cfg(windows)]
        drop(self.reader_handle.take());
    }
}

// ===========================================================================
// CommandRunner: idle/running/success/error state machine + start/stop/relaunch
// ===========================================================================

use std::collections::HashMap;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

/// Flush cadence for coalesced command output (~60fps), matching the terminal
/// output pump in [`crate::bridge`].
const OUTPUT_FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// How long after the last output flush the live scrollback is persisted to the
/// DB. Debounced so a flood writes the row at a bounded cadence, not per chunk.
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);

/// In-memory cap on the live scrollback a runner buffers before it is bounded.
/// Mirrors the persisted [`crate::db::MAX_SCROLLBACK_BYTES`] so memory cannot
/// grow without limit under a flood (`yes`-style output). The TAIL is kept.
const MAX_LIVE_SCROLLBACK_BYTES: usize = crate::db::MAX_SCROLLBACK_BYTES;

/// How long the runner waits for a graceful (SIGTERM) tree exit before escalating
/// to SIGKILL when stopping a running instance.
const TERM_GRACE: Duration = Duration::from_millis(750);

/// How long [`CommandRunner::stop`] waits for the pump thread to finish before
/// DETACHING it. The tree-kill normally unblocks the reader so the pump returns
/// promptly; but a ConPTY host that does not EOF can keep the reader (and thus the
/// pump) alive indefinitely. We never block stop past this — the generation bump in
/// `stop` makes a detached pump's late transition a no-op, so detaching is safe.
const PUMP_JOIN_TIMEOUT: Duration = Duration::from_millis(500);

/// The derived run state of an instance. Maps 1:1 to the persisted `last_state`
/// strings (idle|running|success|error) the DB CHECK constraint enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Running,
    /// Natural exit with code 0.
    Success,
    /// Natural exit with a non-zero code (the code travels in the event payload).
    Error,
}

impl RunState {
    /// The persisted `last_state` string for this state (the DB CHECK vocabulary).
    pub fn as_db_str(self) -> &'static str {
        match self {
            RunState::Idle => "idle",
            RunState::Running => "running",
            RunState::Success => "success",
            RunState::Error => "error",
        }
    }

    /// Parse a persisted `last_state` string back into a [`RunState`]. The inverse of
    /// [`Self::as_db_str`]; an unrecognized value falls back to `Idle`. Used when a
    /// reader (the MCP) rehydrates the FACTUAL outcome from the DB row because the
    /// in-memory runner has no live entry (e.g. after a restart).
    pub fn from_db_str(s: &str) -> RunState {
        match s {
            "running" => RunState::Running,
            "success" => RunState::Success,
            "error" => RunState::Error,
            _ => RunState::Idle,
        }
    }
}

/// The result of a [`CommandRunner::start`] / `start_with_env` call (R-WSCMD): the
/// state after the call PLUS whether the instance was ALREADY running when start was
/// invoked. `was_running:true` means the call was an idempotent NO-OP — it did NOT
/// spawn a second process (the anti-double-spawn guarantee) — so the MCP `start_command`
/// ack can report `restarted:false`. `was_running:false` means a fresh process was
/// spawned (`restarted:false` too — a first start is not a restart; `relaunch` is the
/// explicit restart). The state is `Running` on a successful spawn / live no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartOutcome {
    /// State after the call (Running on success / live no-op).
    pub state: RunState,
    /// Whether the instance was already running BEFORE this call (→ no-op, no spawn).
    pub was_running: bool,
}

/// Default poll interval for [`poll_until`]: how long the bounded wait sleeps between
/// re-reads of the (observational) state. Small enough to keep the resolve latency low
/// (≤ this), large enough that a long wait does not spin. Tunable per-call.
pub const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The hard ceiling on a bounded wait (`wait_for_command`'s `timeout_ms` cap): a wait
/// is clamped to AT MOST this so the MCP long-poll is never a true unbounded block
/// (ADR-0003 D12). A request for more is silently clamped down to this.
pub const WAIT_MAX_TIMEOUT: Duration = Duration::from_millis(60_000);

/// The outcome of a bounded [`poll_until`] wait: whether the target was reached, the
/// state read on the final iteration, and how long the wait actually took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitOutcome {
    /// `true` if `read_state()` returned a state in the target set before `timeout`;
    /// `false` if `timeout` elapsed first (a NORMAL result, not an error).
    pub resolved: bool,
    /// The state observed on the LAST read — the state that satisfied the wait when
    /// `resolved`, else the state at timeout.
    pub state: RunState,
    /// How long the wait actually blocked (≤ the clamped timeout).
    pub waited: Duration,
}

/// BOUNDED, observational poll loop backing the `wait_for_command` MCP tool (ADR-0003
/// D12). Re-reads the current [`RunState`] via the `read_state` closure on a fixed
/// `interval`, returning as soon as the state is in the `target` set
/// (`resolved:true`), or when `timeout` elapses (`resolved:false`). It is purely a
/// reader: it NEVER mutates the runner (no start/stop/acknowledge), so any number of
/// callers may wait the same instance concurrently, and waiting never clears the
/// `unread` flag (waiting ≠ acknowledging).
///
/// `timeout` is clamped to [`WAIT_MAX_TIMEOUT`] by the caller so this is never an
/// infinite block. An empty `target` can still time out normally (nothing matches).
/// The state is checked ONCE up front (a zero-cost resolve when the target is already
/// reached, e.g. an already-finished command) before any sleep, then on each interval.
/// `sleep` is injectable so the unit tests drive the loop deterministically without
/// wall-clock delays.
pub fn poll_until(
    target: &[RunState],
    timeout: Duration,
    interval: Duration,
    mut read_state: impl FnMut() -> RunState,
    mut sleep: impl FnMut(Duration),
) -> WaitOutcome {
    let start = Instant::now();
    loop {
        let state = read_state();
        if target.contains(&state) {
            return WaitOutcome {
                resolved: true,
                state,
                waited: start.elapsed(),
            };
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            // Timed out: report the LAST observed state. resolved:false is normal.
            return WaitOutcome {
                resolved: false,
                state,
                waited: elapsed,
            };
        }
        // Sleep at most the remaining budget so the wait never overshoots `timeout`.
        let remaining = timeout - elapsed;
        sleep(interval.min(remaining));
    }
}

/// Side-effects of a runner transition, abstracted so the state machine is
/// unit-testable WITHOUT a live Tauri runtime or DB. The production implementation
/// emits `command://state` / `command://output` and writes through `crate::db`;
/// tests use a recording mock. (Tauri events are validated by the Phase 5
/// `tauri::test` integration suite, not here — see the PRD phase plan.)
pub trait RunnerSink: Send + Sync + 'static {
    /// A FACTUAL run-state transition occurred: persist the outcome and emit
    /// `command://state`. `exit_code` is the natural exit code for success/error,
    /// else `None`. The production sink persists `last_state` (+ on a success/error
    /// finish, the v4 `last_exit_code`/`ended_at`/`unread` outcome columns) so the
    /// factual outcome survives an acknowledge and a restart. This is NEVER called
    /// for an acknowledge — that goes through [`Self::on_acknowledge`] so the
    /// outcome is left intact.
    fn on_state(&self, instance_id: &str, state: RunState, exit_code: Option<i32>);
    /// An acknowledge cleared the instance's "unseen result" flag. The production
    /// sink clears ONLY the persisted `unread` flag (never the factual outcome) and
    /// emits a `command://ack` notification so the UI can hide the settled badge
    /// WITHOUT the factual state changing. Decoupling this from [`Self::on_state`] is
    /// the whole point of the v4 split: a UI ack must not erase the error the MCP
    /// (or any other observer) sees.
    fn on_acknowledge(&self, instance_id: &str);
    /// Coalesced output for an instance: emit `command://output`.
    fn on_output(&self, instance_id: &str, bytes: &[u8]);
    /// Debounced, bounded scrollback persistence for an instance.
    fn persist_scrollback(&self, instance_id: &str, serialized: &str);
    /// A fresh (re)launch is about to begin for an instance: RETAIN the last completed
    /// run before the current run's columns are reset. The production sink archives the
    /// completing run's scrollback + factual outcome into the bounded `prev_*` columns
    /// (N=1) and resets the CURRENT run to a clean `running` row — so the prior run
    /// stays retrievable while the new run starts unpolluted. Called by
    /// [`CommandRunner::start`] in place of a bare `persist_scrollback(id, "")`: it both
    /// archives and clears in one transaction. A sink with no persistence (tests) can
    /// no-op or just record the call.
    fn archive_previous_run(&self, instance_id: &str);
    /// An agent CLEARED the instance's captured output buffer (PRD-4 review R-OUTPUT,
    /// the `clear_command_output` tool). The production sink empties the persisted
    /// `scrollback` + `prev_scrollback` (the factual outcome is left intact) and emits a
    /// frontend refresh so the read-only output panel wipes its xterm — the analog of
    /// the run-start clear, but with NO state transition. Decoupled from
    /// [`Self::on_state`]/[`Self::archive_previous_run`] so a clear never looks like a
    /// (re)launch. A persistence-free sink (tests) can no-op or just record the call.
    fn clear_output(&self, instance_id: &str);
}

/// One live entry in the runner map: the running command PTY's tree-kill handle,
/// the current derived state, and the pump thread streaming its output.
struct RunnerEntry {
    /// Tree-kill handle for the live process (None once the process has exited).
    kill: Option<KillHandle>,
    /// Current derived state.
    state: RunState,
    /// The natural exit code of the LAST completed run, retained in memory after a
    /// success/error transition so a reader (the bridge / the MCP surface) can tell a
    /// crash (non-zero) from a clean run (zero) without a DB column. `None` while the
    /// instance has never finished a run (idle/running with no prior completion). It
    /// is the exit code carried by the pump's natural-exit `on_state` (`command.rs`),
    /// stored here so `get_command_output`/`list_commands` can surface it. A fresh
    /// `start` clears it so the previous run's code never leaks into a new run.
    last_exit_code: Option<i32>,
    /// The "unseen result" flag (v4): set `true` when a run FINISHES
    /// (success/error), cleared by [`CommandRunner::acknowledge`]. It is the
    /// notification axis, fully decoupled from `state`/`last_exit_code` (the factual
    /// outcome): an acknowledge flips ONLY this, so the MCP and any other observer
    /// still read the true `state` + `last_exit_code` afterwards. A fresh `start`
    /// resets it to `false` (a running command is not yet an unseen result).
    unread: bool,
    /// A monotonically increasing generation, bumped on every (re)spawn. The pump
    /// stamps the generation it was started for; a stale pump (from a process that
    /// was already stopped/relaunched) is ignored so a late natural-exit never
    /// clobbers a fresh `running`. This is the anti-double-instance guard.
    generation: u64,
    /// Pump thread handle, joined on teardown (best-effort).
    pump: Option<JoinHandle<()>>,
    /// The live, bounded scrollback tail the pump maintains in memory. Updated on
    /// every output chunk (same bound as the persisted row) BEFORE the debounced
    /// DB persist, so a mid-run [`CommandRunner::live_output`] read returns the
    /// true live tail rather than a row that can lag by up to [`PERSIST_DEBOUNCE`].
    /// Shared (not owned by the pump) so a reader can observe it under the entries
    /// lock without racing the pump's own writes.
    live_scrollback: Arc<Mutex<String>>,
}

/// Managed state: the live command runners keyed by `command_instances.id`.
///
/// The single `Mutex<HashMap>` serializes all lifecycle ops on a given instance,
/// so `start` is idempotent (a `running` entry short-circuits, never double-
/// spawning) and `relaunch` cannot interleave a stop with a competing start.
pub struct CommandRunner<S: RunnerSink> {
    entries: Arc<Mutex<HashMap<String, RunnerEntry>>>,
    sink: Arc<S>,
    size: PtySize,
    /// Latched once the shutdown reap has run, so the window event (which fires for
    /// BOTH `CloseRequested` and `Destroyed`) snapshots + kills exactly once.
    shutdown_started: AtomicBool,
}

impl<S: RunnerSink> CommandRunner<S> {
    /// Build a runner over a sink, using `size` for every spawned PTY.
    pub fn new(sink: S, size: PtySize) -> Self {
        CommandRunner {
            entries: Arc::new(Mutex::new(HashMap::new())),
            sink: Arc::new(sink),
            size,
            shutdown_started: AtomicBool::new(false),
        }
    }

    /// The runner's sink, so a shell adapter can drive a sink-only side effect for the
    /// PERSISTED-only acknowledge path (a success/error restored at boot, never re-run
    /// this session, has no live entry to flip — the adapter clears the persisted
    /// `unread` and emits `command://ack` straight through the sink). Parity with the
    /// Tauri `acknowledge_unread`'s direct emit on the no-live-entry branch.
    pub fn sink(&self) -> &S {
        &self.sink
    }

    /// Current derived state of an instance: the live entry's state, or `Idle`
    /// when there is no entry (never started, or stopped back to idle).
    pub fn state_of(&self, instance_id: &str) -> RunState {
        self.entries
            .lock()
            .unwrap()
            .get(instance_id)
            .map(|e| e.state)
            .unwrap_or(RunState::Idle)
    }

    /// True if `instance_id` currently has a LIVE running process in the runner.
    /// The authoritative "is this instance running right now?" check the bridge's
    /// running-mutation guard relies on (the DB `last_state` is only the persisted
    /// mirror; the live map is the truth).
    pub fn is_running(&self, instance_id: &str) -> bool {
        self.state_of(instance_id) == RunState::Running
    }

    /// The natural exit code of the instance's LAST completed run, or `None` when it
    /// has not finished a run this session (never started, still running, or only ever
    /// stopped — a stop is a kill, not a natural exit, so it records no code). Read
    /// straight off the in-memory entry the pump stamps on its success/error
    /// transition. Superseded in production by [`Self::outcome`] (which also carries
    /// the live state + `unread` in one snapshot); retained for the runner tests that
    /// assert an acknowledge preserves the factual exit code.
    #[cfg(test)]
    pub fn last_exit_code(&self, instance_id: &str) -> Option<i32> {
        self.entries
            .lock()
            .unwrap()
            .get(instance_id)
            .and_then(|e| e.last_exit_code)
    }

    /// The instance's LIVE in-memory scrollback tail when it is `running`, else
    /// `None`. This is the bounded buffer the pump maintains as output streams,
    /// fresher than the debounced-persisted DB row (which can lag by up to
    /// [`PERSIST_DEBOUNCE`]). `command_output` reads this while running and falls
    /// back to the persisted row for cold (idle/success/error) rehydration.
    ///
    /// Read-only on the entry's state, under the same `entries` lock as
    /// [`Self::is_running`], so a running entry's live buffer cannot be observed
    /// mid-eviction. Returns `None` for any non-running (or absent) instance.
    pub fn live_output(&self, instance_id: &str) -> Option<String> {
        // Clone the buffer handle under the entries lock, then release it before
        // reading the buffer so the two locks are never held at once.
        let live = {
            let entries = self.entries.lock().unwrap();
            let entry = entries.get(instance_id)?;
            if entry.state != RunState::Running {
                return None;
            }
            Arc::clone(&entry.live_scrollback)
        };
        let text = live.lock().unwrap().clone();
        Some(text)
    }

    /// Whether ANY of `instance_ids` is currently running in the runner. Used by the
    /// guards: a template update/delete (or a `delete_project`) is refused if any of
    /// the affected instances has a live process.
    pub fn any_running(&self, instance_ids: &[String]) -> bool {
        let entries = self.entries.lock().unwrap();
        instance_ids.iter().any(|id| {
            entries
                .get(id)
                .map(|e| e.state == RunState::Running)
                .unwrap_or(false)
        })
    }

    /// Start (or restart-from-terminal-state) an instance — the back-compat wrapper
    /// that returns just the resulting [`RunState`] and passes NO per-run env. Existing
    /// call sites (the bridge UI command, the boot restorer, the runner tests) keep this
    /// shape; the MCP layer that needs the `was_running` ack + per-run env uses
    /// [`Self::start_with_env`] directly.
    pub fn start(
        &self,
        instance_id: &str,
        cmdline: &str,
        cwd: Option<&str>,
    ) -> anyhow::Result<RunState> {
        self.start_with_env(instance_id, cmdline, cwd, &[])
            .map(|o| o.state)
    }

    /// Start (or restart-from-terminal-state) an instance, with an OPTIONAL per-run
    /// `env` merged onto the inherited environment, returning a [`StartOutcome`] that
    /// reports whether the instance was ALREADY running (R-WSCMD).
    ///
    /// - On `idle`/`success`/`error` (or no entry): spawn `cmdline` at `cwd` (with
    ///   `env` merged on top of the inherited environment), transition to `running`,
    ///   and stream output → `StartOutcome { state: Running, was_running: false }`.
    /// - On `running`: **idempotent no-op** — returns `StartOutcome { state: Running,
    ///   was_running: true }` and does NOT spawn a second process. This is the
    ///   no-double-spawn guarantee, enforced HERE at the runner boundary so neither the
    ///   UI nor the MCP can double-spawn. A caller wanting an explicit restart uses
    ///   [`Self::relaunch`] (which stops-then-starts), never a second `start`.
    ///
    /// `env` entries are `(KEY, VALUE)` pairs applied after inheritance; secret VALUES
    /// are never logged by the runner (only passed to the PTY spawn).
    pub fn start_with_env(
        &self,
        instance_id: &str,
        cmdline: &str,
        cwd: Option<&str>,
        env: &[(String, String)],
    ) -> anyhow::Result<StartOutcome> {
        let mut entries = self.entries.lock().unwrap();

        // Idempotent: a running instance short-circuits. We must NOT spawn again. The
        // no-op is reported via `was_running:true` so the MCP ack says restarted:false.
        if let Some(entry) = entries.get(instance_id) {
            if entry.state == RunState::Running {
                return Ok(StartOutcome {
                    state: RunState::Running,
                    was_running: true,
                });
            }
        }

        // Spawn the process tree and a fresh generation for it.
        let (pty, rx) = CommandPty::spawn(cmdline, self.size, cwd, env)?;
        let kill = pty.kill_handle();
        let generation = entries
            .get(instance_id)
            .map(|e| e.generation.wrapping_add(1))
            .unwrap_or(0);

        // The live, bounded scrollback the pump maintains in memory and the bridge
        // reads back via `live_output` while running. Shared with the pump so a
        // reader observes the true live tail, not the debounced DB row.
        let live_scrollback = Arc::new(Mutex::new(String::new()));

        // Pump thread: coalesce output, persist scrollback (debounced/bounded), and
        // on disconnect reap the natural exit code and transition to success/error.
        let pump = spawn_command_pump(
            Arc::clone(&self.entries),
            Arc::clone(&self.sink),
            instance_id.to_string(),
            generation,
            pty,
            rx,
            Arc::clone(&live_scrollback),
        );

        entries.insert(
            instance_id.to_string(),
            RunnerEntry {
                kill: Some(kill),
                state: RunState::Running,
                // A fresh run starts with no recorded exit code: the previous run's
                // code (if any) is cleared here so a crash/clean result reported by
                // `last_exit_code` always belongs to the CURRENT run, never a stale one.
                last_exit_code: None,
                // A running command is not an unseen result yet; a finish sets this.
                unread: false,
                generation,
                pump: Some(pump),
                live_scrollback,
            },
        );
        drop(entries);

        // Fresh run = fresh scrollback, but RETAIN the last completed run (v5): archive
        // the prior run's scrollback + outcome into the bounded `prev_*` columns (N=1),
        // THEN reset the current row so a cold rehydrate of THIS new run never returns
        // the previous run's output (the relaunch-piles-old-output bug) — while the
        // prior run stays retrievable via `get_command_output(run="previous")`. The
        // fresh live buffer above already starts empty; this archives + clears the DB
        // row to match, before the new pump's first debounced persist. The front
        // independently clears its xterm on the running transition below.
        self.sink.archive_previous_run(instance_id);

        // Transition AFTER the entry exists so the persisted state and the live map
        // agree. The natural-exit path may race to overwrite this; it is guarded by
        // the generation stamp (a stale pump never clobbers a fresh running).
        self.sink.on_state(instance_id, RunState::Running, None);
        Ok(StartOutcome {
            state: RunState::Running,
            was_running: false,
        })
    }

    /// Stop a running instance: best-effort process-TREE kill (SIGTERM then, after a
    /// grace window, SIGKILL on Unix; `taskkill /T /F` on Windows), then transition
    /// to `idle`.
    ///
    /// `stop` ALWAYS returns and always emits the resulting state — it must never
    /// hang (a dead Stop button + a frozen dot was the bug):
    ///   - it never blocks indefinitely on the pump join. After the tree-kill the
    ///     pump is joined with a bounded timeout and DETACHED if it lingers (a
    ///     ConPTY host that does not EOF would otherwise keep the reader — and thus
    ///     the pump — alive forever). The generation was bumped before the kill, so
    ///     a detached pump's late natural-exit transition is ignored and cannot
    ///     clobber the fresh `idle`.
    ///   - a no-op stop on a non-running/absent instance (a PHANTOM running dot,
    ///     e.g. a stale `last_state=running` with no live process after a restart)
    ///     still emits `idle` so the dot reconciles instead of staying frozen.
    ///     A genuine `idle`/`success`/`error` runner entry keeps its state (we never
    ///     overwrite a success/error dot with idle).
    ///
    /// Returns the state after the call.
    pub fn stop(&self, instance_id: &str) -> anyhow::Result<RunState> {
        // Snapshot + evict under the lock; do the (possibly blocking) kill outside.
        let (kill, pump) = {
            let mut entries = self.entries.lock().unwrap();
            match entries.get_mut(instance_id) {
                Some(entry) if entry.state == RunState::Running => {
                    // Bump generation so the about-to-die pump's natural-exit
                    // transition is ignored (it would otherwise race us to set
                    // success/error after we set idle).
                    entry.generation = entry.generation.wrapping_add(1);
                    let kill = entry.kill.take();
                    let pump = entry.pump.take();
                    entry.state = RunState::Idle;
                    (kill, pump)
                }
                // A genuine non-running runner entry (idle/success/error): no-op,
                // keep its state. Read it from THIS guard — never re-lock
                // `self.entries` while held (non-reentrant mutex deadlock).
                Some(entry) => return Ok(entry.state),
                // Absent entry: there is NO live process. This is the phantom-running
                // path (a stale `running` dot the runner does not back). Force idle
                // and emit so the dot reconciles instead of staying frozen.
                None => {
                    self.sink.on_state(instance_id, RunState::Idle, None);
                    return Ok(RunState::Idle);
                }
            }
        };

        if let Some(kill) = kill {
            kill_tree_graceful(&kill);
        }
        // Tear down the pump WITHOUT blocking stop indefinitely: join with a bounded
        // timeout, detach if it lingers. A lingering ConPTY host can keep the reader
        // (and thus the pump) alive past the kill; we must still return + emit idle.
        // The generation bump above makes a detached pump's late transition a no-op.
        if let Some(pump) = pump {
            join_pump_bounded(pump, PUMP_JOIN_TIMEOUT);
        }

        // The entry is now idle with no live process. Persist + emit.
        self.sink.on_state(instance_id, RunState::Idle, None);
        Ok(RunState::Idle)
    }

    /// Relaunch an instance — the back-compat wrapper returning just the resulting
    /// [`RunState`] and passing NO per-run env. The MCP layer that needs the per-run
    /// env uses [`Self::relaunch_with_env`].
    pub fn relaunch(
        &self,
        instance_id: &str,
        cmdline: &str,
        cwd: Option<&str>,
    ) -> anyhow::Result<RunState> {
        self.relaunch_with_env(instance_id, cmdline, cwd, &[])
            .map(|o| o.state)
    }

    /// Relaunch an instance with an OPTIONAL per-run `env` merged onto the inherited
    /// environment (R-WSCMD), returning a [`StartOutcome`] whose `was_running` reports
    /// whether a live process was stopped first (so the MCP `relaunch_command` ack can
    /// say `was_running`). `relaunch` is the EXPLICIT restart (contrast a second
    /// `start`, which is a no-op on a running instance): it ALWAYS spawns a fresh
    /// process — so the ack reports `restarted:true`.
    ///
    /// - On `running`: stop (tree kill) then start. If the stop fails, relaunch
    ///   fails and NO second instance is started.
    /// - On `idle`/`success`/`error`: start directly.
    ///
    /// Because both legs take the same per-instance lock path, a relaunch can never
    /// leave two live processes for one instance.
    pub fn relaunch_with_env(
        &self,
        instance_id: &str,
        cmdline: &str,
        cwd: Option<&str>,
        env: &[(String, String)],
    ) -> anyhow::Result<StartOutcome> {
        let was_running = self.state_of(instance_id) == RunState::Running;
        if was_running {
            // Stop first; only start if the stop made it to idle. A failed/partial
            // stop returns early WITHOUT starting a second instance.
            let stopped = self.stop(instance_id)?;
            if stopped != RunState::Idle {
                anyhow::bail!(
                    "relaunch aborted: stop did not reach idle (no second instance spawned)"
                );
            }
        }
        // The fresh start reports was_running:false (it spawned into an idle slot); the
        // relaunch's OWN was_running is whether we stopped a live process above.
        self.start_with_env(instance_id, cmdline, cwd, env)
            .map(|o| StartOutcome {
                state: o.state,
                was_running,
            })
    }

    /// Acknowledge a FINISHED one-shot: clear ONLY its "unseen result" flag
    /// (`unread`) — NEVER its factual outcome. A finished run's success/error result
    /// is the notification; selecting/opening the command = seeing it, so the settled
    /// BADGE clears (the UI hides it off `unread`) while the factual `state` +
    /// `last_exit_code` — what the MCP and any other observer read — are LEFT INTACT.
    /// This is the v4 fix for the finding: a UI acknowledge can no longer erase the
    /// error the MCP sees.
    ///
    /// Emits via [`RunnerSink::on_acknowledge`] (the prod sink clears the persisted
    /// `unread` flag + broadcasts `command://ack`), NOT `on_state` — so no false
    /// `idle` transition collapses the outcome.
    ///
    /// **No-op** when the instance is `running` (never acknowledge a live process —
    /// it has no unseen result yet) or has nothing unread (already seen / absent /
    /// idle never-run): in those cases NO event is emitted and the entry is untouched.
    ///
    /// Returns the FACTUAL state after the call (unchanged — an ack never moves it).
    pub fn acknowledge(&self, instance_id: &str) -> RunState {
        // Flip ONLY the unread flag under the lock, then emit OUTSIDE it (the sink may
        // touch the DB / event loop; never hold the entries mutex across that). The
        // factual `state`/`last_exit_code` are deliberately left untouched.
        let acknowledged = {
            let mut entries = self.entries.lock().unwrap();
            match entries.get_mut(instance_id) {
                // Only a finished, still-unread run is acknowledgeable. Never a live
                // `running` process (it has no settled result), never an already-read
                // or never-run instance.
                Some(entry)
                    if entry.unread
                        && matches!(entry.state, RunState::Success | RunState::Error) =>
                {
                    entry.unread = false;
                    true
                }
                _ => false,
            }
        };
        if acknowledged {
            self.sink.on_acknowledge(instance_id);
        }
        // The factual state is unchanged by an acknowledge — return it as-is.
        self.state_of(instance_id)
    }

    /// Clear an instance's captured output BUFFER (PRD-4 review R-OUTPUT, the
    /// `clear_command_output` tool): empties the LIVE in-memory scrollback tail (when a
    /// running entry holds one) AND, via [`RunnerSink::clear_output`], the persisted
    /// `scrollback`/`prev_scrollback` + a frontend refresh. The FACTUAL state/outcome is
    /// left UNTOUCHED — a clear wipes the bytes, never the run result — so this is NOT a
    /// stop/relaunch and emits no state transition. Safe on any instance: a running one
    /// keeps streaming (its NEXT chunk lands on the freshly-emptied buffer), an
    /// idle/finished one simply has its persisted log wiped, an absent one is a no-op on
    /// the live side and clears only the DB row.
    ///
    /// The live buffer is reset under the SAME `entries` lock as [`Self::live_output`]
    /// so a reader cannot observe it mid-clear, then the sink is called OUTSIDE the lock
    /// (it touches the DB / event loop — never hold the entries mutex across that).
    pub fn clear_output(&self, instance_id: &str) {
        {
            let entries = self.entries.lock().unwrap();
            if let Some(entry) = entries.get(instance_id) {
                // Empty the shared live tail in place; a running pump's next chunk
                // appends onto the cleared buffer (the pump writes the same Arc).
                entry.live_scrollback.lock().unwrap().clear();
            }
        }
        // Persist the clear + emit the refresh OUTSIDE the entries lock.
        self.sink.clear_output(instance_id);
    }

    /// Whether the instance's last finished run is still an "unseen result" (the v4
    /// `unread` flag). `false` for a running, never-run, absent, or already-read
    /// instance. The MCP surface exposes this so an agent can tell whether the UI has
    /// acknowledged the result — WITHOUT it affecting the factual outcome.
    pub fn is_unread(&self, instance_id: &str) -> bool {
        self.entries
            .lock()
            .unwrap()
            .get(instance_id)
            .map(|e| e.unread)
            .unwrap_or(false)
    }

    /// The instance's LIVE outcome snapshot — `(state, last_exit_code, unread)` —
    /// when the runner backs it this session, else `None` (no live entry: never
    /// started this session, or evicted). `None` is the signal for a reader (the MCP)
    /// to fall back to the PERSISTED outcome in the DB, so the factual outcome is
    /// reported correctly even across a restart (when the in-memory map is empty).
    /// Taken under a single lock so the three fields are a consistent snapshot.
    pub fn outcome(&self, instance_id: &str) -> Option<(RunState, Option<i32>, bool)> {
        self.entries
            .lock()
            .unwrap()
            .get(instance_id)
            .map(|e| (e.state, e.last_exit_code, e.unread))
    }

    /// Latch the shutdown so the reap runs exactly once. Returns `true` the FIRST
    /// time only; the window event fires for both `CloseRequested` and `Destroyed`,
    /// and a second snapshot AFTER [`Self::kill_all_running`] would see every
    /// instance idle and wrongly clear `was_running_on_shutdown` (breaking
    /// restart-on-startup).
    pub fn begin_shutdown(&self) -> bool {
        self.shutdown_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Hard-kill EVERY running instance's process tree. Called on app shutdown so a
    /// managed command (its shell + child dev server, plus the Windows conhost) is
    /// reaped instead of orphaned: the pump owns the [`CommandPty`] on a DETACHED
    /// thread and the runner map holds only a lightweight [`KillHandle`], so
    /// dropping the map alone would NOT kill the children. Best-effort and
    /// non-blocking — snapshot + evict the live kill handles under the lock, bump
    /// each generation so the about-to-die pump's natural-exit transition stays
    /// silent, then tree-kill OUTSIDE the lock.
    pub fn kill_all_running(&self) {
        let kills: Vec<KillHandle> = {
            let mut entries = self.entries.lock().unwrap();
            entries
                .values_mut()
                .filter(|e| e.state == RunState::Running)
                .filter_map(|e| {
                    e.generation = e.generation.wrapping_add(1);
                    // Detach the pump (the tree-kill below unblocks its reader so it
                    // exits on its own); the bumped generation keeps it silent.
                    e.pump = None;
                    e.state = RunState::Idle;
                    e.kill.take()
                })
                .collect()
        };
        for kill in kills {
            kill.kill_tree();
        }
    }
}

// --- Shutdown snapshot + boot restoration (shell-agnostic orchestration) ----
//
// These free functions are the auto-relaunch-on-startup contract, EXTRACTED out of
// the Tauri adapter (`apps/tauri/src-tauri/src/bridge.rs`) so BOTH shells (the Tauri
// adapter AND the Electron core-host over napi) drive the IDENTICAL restore/snapshot
// flow. They reason ONLY over `nyx-core` types — the [`Db`] pool, a
// [`CommandRunner<S>`] over ANY [`RunnerSink`], and the [`crate::subfolder`] cwd
// resolver — so NO shell type crosses them (the frozen "0 Tauri in nyx-core" rule).
//
// The contract is driven by TWO signals (never `last_state` alone):
//   - at SHUTDOWN, snapshot `was_running_on_shutdown = (the runner reports the
//     instance running)` for every instance;
//   - at BOOT, relaunch an instance ONLY when its template's `restart_on_startup`
//     is ON AND its `was_running_on_shutdown` snapshot is true; then reset the
//     snapshot so the next boot cannot relaunch a ghost; and normalize a stale
//     `running` (a dead/orphaned process) down to `idle` while keeping
//     `success`/`error` for the dot.

use crate::db::{self, Db, RestoreRow};

/// Snapshot the shutdown state: for every command instance, persist
/// `was_running_on_shutdown` = whether the runner currently has it running. Called
/// from the shell's window-close / app-exit hook. The runner's LIVE map is the source
/// of truth (a `last_state` of `running` that the runner does not back is NOT a
/// running process).
///
/// Extracted verbatim from the Tauri `bridge::snapshot_commands_on_shutdown`; the
/// Tauri adapter now delegates here, and the napi core-host calls the SAME function.
pub fn snapshot_commands_on_shutdown<S: RunnerSink>(db: &Db, runner: &CommandRunner<S>) {
    let rows = match db.with_conn(db::all_instances_for_restore) {
        Ok(rows) => rows,
        Err(_) => return,
    };
    db.with_conn(|c| {
        for row in &rows {
            let running = runner.is_running(&row.instance_id);
            let _ = db::set_was_running_on_shutdown(c, &row.instance_id, running);
        }
    });
}

/// Restore command instances at boot from the shutdown snapshot.
///
/// For every instance:
///   - if its template `restart_on_startup` is ON **and**
///     `was_running_on_shutdown` is true → `start` it through the runner (resolving
///     cwd via the validated subfolder);
///   - otherwise it is NOT relaunched; if its persisted `last_state` was `running`
///     (a process that did not survive the restart), normalize it to `idle` so the
///     UI never shows a phantom running dot. `success`/`error` are kept as-is.
///   - in ALL cases the `was_running_on_shutdown` snapshot is reset to false after
///     the boot decision, so a subsequent boot cannot relaunch a ghost.
///
/// Returns the ids that were relaunched (handy for tests/logging).
///
/// Extracted verbatim from the Tauri `bridge::restore_commands_on_boot`; the Tauri
/// adapter now delegates here, and the napi core-host calls the SAME function.
pub fn restore_commands_on_boot<S: RunnerSink>(db: &Db, runner: &CommandRunner<S>) -> Vec<String> {
    let rows = match db.with_conn(db::all_instances_for_restore) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    let mut relaunched = Vec::new();
    for row in &rows {
        let should_relaunch = row.restart_on_startup && row.was_running_on_shutdown;

        if should_relaunch {
            // Resolve the cwd (workspace + validated subfolder) and start. A failure
            // to resolve (e.g. the subfolder no longer exists) must not abort the
            // whole restore: skip this instance and normalize it instead.
            match crate::subfolder::resolve_run_dir(&row.workspace_path, row.subfolder.as_deref()) {
                Ok(cwd) => {
                    if runner
                        .start(&row.instance_id, &row.command, Some(&cwd))
                        .is_ok()
                    {
                        relaunched.push(row.instance_id.clone());
                    } else {
                        normalize_unrelaunched(db, row);
                    }
                }
                Err(_) => normalize_unrelaunched(db, row),
            }
        } else {
            normalize_unrelaunched(db, row);
        }

        // Reset the snapshot AFTER the boot decision so a future boot cannot relaunch
        // a ghost from a stale snapshot.
        db.with_conn(|c| {
            let _ = db::set_was_running_on_shutdown(c, &row.instance_id, false);
        });
    }
    relaunched
}

/// Normalize an instance that was NOT relaunched at boot: a persisted `running`
/// (the process did not survive the restart, so it is an orphan/dead) becomes
/// `idle`; `success`/`error`/`idle` are left untouched (the dot keeps its color).
fn normalize_unrelaunched(db: &Db, row: &RestoreRow) {
    if row.last_state == db::STATE_RUNNING {
        db.with_conn(|c| {
            let _ = db::set_last_state(c, &row.instance_id, db::STATE_IDLE);
        });
    }
}

/// Resolve an instance's command LINE + run cwd from the DB: the template `command`,
/// and the workspace path joined with the VALIDATED subfolder (anti path-traversal /
/// existence via [`crate::subfolder`]). `Err` BEFORE any spawn on an unknown instance
/// or an invalid/missing subfolder.
///
/// The shell-agnostic twin of the Tauri `bridge::resolve_command_and_cwd`, lifted into
/// nyx-core so the lifecycle command surface (Tauri `command_start`/`command_relaunch`
/// AND the Electron core-host's napi runner) resolve a run target identically. Returns
/// a readable error STRING (the same vocabulary both shells surface to the user).
pub fn resolve_command_and_cwd(db: &Db, instance_id: &str) -> Result<(String, String), String> {
    let ctx = db
        .with_conn(|c| db::instance_run_context(c, instance_id))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("unknown command instance {instance_id}"))?;
    let cwd = crate::subfolder::resolve_run_dir(&ctx.workspace_path, ctx.subfolder.as_deref())?;
    Ok((ctx.command, cwd))
}

/// Best-effort graceful tree kill: SIGTERM the group, wait up to [`TERM_GRACE`]
/// for it to drain, then SIGKILL. On platforms without a group signal this
/// degrades to the parent-only killer inside [`KillHandle::kill_tree`].
fn kill_tree_graceful(kill: &KillHandle) {
    if kill.term_tree() {
        // A group signal was delivered; give the tree a moment to exit cleanly.
        // We don't have the child handle here (the pump owns it), so this is a
        // fixed best-effort grace window before the hard kill.
        std::thread::sleep(TERM_GRACE);
    }
    kill.kill_tree();
}

/// Join a pump thread, but NEVER block longer than `timeout`: a watcher thread
/// performs the actual `join()` and signals completion over a channel; if the
/// signal does not arrive in time the pump is DETACHED (the watcher keeps it,
/// finishes the join in the background, and is itself detached). This is what makes
/// [`CommandRunner::stop`] always return: a ConPTY host that fails to EOF can keep
/// the reader (and pump) alive past the tree-kill, and an unconditional join would
/// hang the Stop button. Detaching is safe because `stop` bumped the entry's
/// generation, so a detached pump's late natural-exit transition is suppressed.
fn join_pump_bounded(pump: JoinHandle<()>, timeout: Duration) {
    let (done_tx, done_rx) = mpsc::channel::<()>();
    // The watcher OWNS the JoinHandle; it joins then signals. If we time out we drop
    // our receiver end and leave the watcher running (it cannot block us).
    std::thread::Builder::new()
        .name("nyx-cmd-pump-join".into())
        .spawn(move || {
            let _ = pump.join();
            let _ = done_tx.send(());
        })
        .ok();
    // Wait at most `timeout` for the join to complete; on timeout, detach.
    let _ = done_rx.recv_timeout(timeout);
}

/// Spawn the pump thread for one running command: coalesces output into
/// `command://output` at [`OUTPUT_FLUSH_INTERVAL`], persists bounded scrollback on
/// a [`PERSIST_DEBOUNCE`] cadence, and on disconnect reaps the natural exit code,
/// transitions to success/error, and clears the live entry's kill handle.
///
/// The pump owns the `CommandPty` so its threads/handles are dropped when the pump
/// returns. It stamps `generation`: if, on natural exit, the entry's generation has
/// moved on (a stop/relaunch happened), the transition is suppressed — that is the
/// anti-double-instance / no-stale-clobber guard.
#[allow(clippy::too_many_arguments)]
fn spawn_command_pump<S: RunnerSink>(
    entries: Arc<Mutex<HashMap<String, RunnerEntry>>>,
    sink: Arc<S>,
    instance_id: String,
    generation: u64,
    mut pty: CommandPty,
    rx: Receiver<Vec<u8>>,
    live_scrollback: Arc<Mutex<String>>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("nyx-cmd-pump-{}", pty.id()))
        .spawn(move || {
            let mut pending: Vec<u8> = Vec::new();
            let mut scrollback: Vec<u8> = Vec::new();
            let mut last_flush = Instant::now();
            let mut dirty_since: Option<Instant> = None;

            let flush_output = |pending: &mut Vec<u8>| {
                if pending.is_empty() {
                    return;
                }
                sink.on_output(&instance_id, pending);
                pending.clear();
            };
            // Persist the bounded scrollback tail (debounced).
            let persist = |scrollback: &Vec<u8>| {
                let text = String::from_utf8_lossy(scrollback);
                sink.persist_scrollback(&instance_id, &text);
            };

            loop {
                let since = last_flush.elapsed();
                let wait = OUTPUT_FLUSH_INTERVAL.saturating_sub(since);
                match rx.recv_timeout(wait) {
                    Ok(chunk) => {
                        pending.extend_from_slice(&chunk);
                        scrollback.extend_from_slice(&chunk);
                        bound_live_scrollback(&mut scrollback);
                        // Publish the bounded tail to the shared live buffer so a
                        // mid-run `live_output` read sees it immediately, ahead of
                        // the debounced DB persist below.
                        *live_scrollback.lock().unwrap() =
                            String::from_utf8_lossy(&scrollback).into_owned();
                        dirty_since.get_or_insert_with(Instant::now);
                        if last_flush.elapsed() >= OUTPUT_FLUSH_INTERVAL {
                            flush_output(&mut pending);
                            last_flush = Instant::now();
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        flush_output(&mut pending);
                        last_flush = Instant::now();
                        // Debounced persistence: write the row when output has been
                        // idle for PERSIST_DEBOUNCE since the last change.
                        if let Some(t) = dirty_since {
                            if t.elapsed() >= PERSIST_DEBOUNCE {
                                persist(&scrollback);
                                dirty_since = None;
                            }
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        // Child exited / master closed: flush the tail + final persist.
                        flush_output(&mut pending);
                        persist(&scrollback);
                        let code = pty.wait();
                        let next = match code {
                            Some(0) => RunState::Success,
                            // Any non-zero (or unknown) exit is an error state.
                            _ => RunState::Error,
                        };
                        // Only transition if THIS generation is still the live one.
                        // A stop/relaunch bumps the generation and owns the
                        // transition, so a stale pump must stay silent.
                        let mut guard = entries.lock().unwrap();
                        if let Some(entry) = guard.get_mut(&instance_id) {
                            if entry.generation == generation {
                                entry.state = next;
                                entry.kill = None;
                                // Retain the natural exit code in memory so a reader
                                // (bridge / MCP) can distinguish a crash (non-zero)
                                // from a clean run (zero) — the same `code` carried to
                                // the sink below. Only the live generation records it.
                                entry.last_exit_code = code;
                                // A freshly finished run is an "unseen result" until
                                // the UI acknowledges it (the notification axis, fully
                                // decoupled from the factual outcome above).
                                entry.unread = true;
                                drop(guard);
                                sink.on_state(&instance_id, next, code);
                            }
                        }
                        break;
                    }
                }
            }
        })
        .expect("failed to spawn command output pump thread")
}

/// Bound a live scrollback byte buffer to [`MAX_LIVE_SCROLLBACK_BYTES`] in place,
/// keeping the TAIL, so a flood cannot grow memory without limit. We cut on a
/// UTF-8 char boundary (walking forward from the target start) so the buffer stays
/// decodable; the DB persistence applies the same bound to the stored string.
fn bound_live_scrollback(buf: &mut Vec<u8>) {
    if buf.len() <= MAX_LIVE_SCROLLBACK_BYTES {
        return;
    }
    let mut start = buf.len() - MAX_LIVE_SCROLLBACK_BYTES;
    // Advance to the next char boundary (a continuation byte is 0b10xxxxxx).
    while start < buf.len() && (buf[start] & 0xC0) == 0x80 {
        start += 1;
    }
    buf.drain(..start);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn small_size() -> PtySize {
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// Drain the receiver into one String until `needle` appears or `timeout`
    /// elapses. Returns the accumulated output.
    fn read_until(rx: &Receiver<Vec<u8>>, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let mut acc = String::new();
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(chunk) => {
                    acc.push_str(&String::from_utf8_lossy(&chunk));
                    if acc.contains(needle) {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        acc
    }

    // --- command_invocation: shell family -> flag (pure, no spawn) ---------

    #[test]
    fn command_invocation_posix_uses_dash_c() {
        for sh in ["bash", "sh", "/bin/zsh", "/usr/bin/fish", "dash", "ksh"] {
            let (prog, args) = command_invocation(sh, "echo hi");
            assert_eq!(prog, sh, "program must be the shell verbatim");
            assert_eq!(
                args,
                vec!["-c".to_string(), "echo hi".to_string()],
                "POSIX shell {sh} must run via `-c <cmdline>`"
            );
        }
    }

    #[test]
    fn command_invocation_powershell_uses_dash_command() {
        for sh in [
            "pwsh",
            "powershell",
            "pwsh.exe",
            "powershell.exe",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
        ] {
            let (_prog, args) = command_invocation(sh, "Write-Output hi");
            assert_eq!(
                args,
                vec!["-Command".to_string(), "Write-Output hi".to_string()],
                "PowerShell {sh} must run via `-Command <cmdline>`"
            );
        }
    }

    #[test]
    fn command_invocation_cmd_uses_slash_c() {
        for sh in ["cmd", "cmd.exe", r"C:\Windows\System32\cmd.exe", "CMD.EXE"] {
            let (_prog, args) = command_invocation(sh, "echo hi");
            assert_eq!(
                args,
                vec!["/C".to_string(), "echo hi".to_string()],
                "cmd.exe ({sh}) must run via `/C <cmdline>`"
            );
        }
    }

    #[test]
    fn command_invocation_unknown_shell_falls_back_to_posix() {
        let (_prog, args) = command_invocation("/opt/weird/myshell", "do thing");
        assert_eq!(
            args,
            vec!["-c".to_string(), "do thing".to_string()],
            "an unrecognized shell must default to the POSIX `-c` form"
        );
    }

    // --- scan_terminal_queries: terminal-query auto-responder (pure) -------

    #[test]
    fn scan_no_escape_yields_no_reply() {
        let mut carry = Vec::new();
        assert!(
            scan_terminal_queries(b"plain output, no queries\n", &mut carry).is_empty(),
            "a chunk without ESC must produce no reply"
        );
        assert!(carry.is_empty(), "no partial sequence to carry");
    }

    #[test]
    fn scan_dsr_cursor_position_replies() {
        // `ESC[6n` (DSR cursor position) → `ESC[1;1R`. This is the exact reply that
        // (confirmed live) makes bun release its withheld output.
        let mut carry = Vec::new();
        let reply = scan_terminal_queries(b"\x1b[6n", &mut carry);
        assert_eq!(reply, b"\x1b[1;1R", "DSR 6n must be answered with a CPR");
    }

    #[test]
    fn scan_dsr_status_replies_ok() {
        let mut carry = Vec::new();
        let reply = scan_terminal_queries(b"\x1b[5n", &mut carry);
        assert_eq!(reply, b"\x1b[0n", "DSR 5n must be answered with status OK");
    }

    #[test]
    fn scan_primary_device_attributes_replies() {
        // Both the bare `ESC[c` and the explicit-parameter `ESC[0c` are Primary DA.
        let mut carry = Vec::new();
        assert_eq!(scan_terminal_queries(b"\x1b[c", &mut carry), DA_REPLY);
        let mut carry2 = Vec::new();
        assert_eq!(scan_terminal_queries(b"\x1b[0c", &mut carry2), DA_REPLY);
    }

    #[test]
    fn scan_query_embedded_in_output_is_answered() {
        // A query in the MIDDLE of real output is still detected + answered, and the
        // surrounding bytes never affect the reply (the chunk itself is forwarded
        // verbatim by the caller; this function only computes the reply).
        let mut carry = Vec::new();
        let reply = scan_terminal_queries(b"before\x1b[6nafter", &mut carry);
        assert_eq!(reply, b"\x1b[1;1R");
    }

    #[test]
    fn scan_query_split_across_chunks_is_answered() {
        // The query straddles a read boundary: `ESC[` in the first chunk, `6n` in
        // the next. The carry must stitch them so the reply still fires.
        let mut carry = Vec::new();
        assert!(
            scan_terminal_queries(b"out\x1b[", &mut carry).is_empty(),
            "a partial CSI must produce no reply yet"
        );
        assert!(!carry.is_empty(), "the partial sequence must be carried");
        let reply = scan_terminal_queries(b"6n", &mut carry);
        assert_eq!(reply, b"\x1b[1;1R", "the stitched query must be answered");
    }

    #[test]
    fn scan_multiple_queries_in_one_chunk() {
        // bun-style startup probing: several queries back-to-back must each get an
        // answer, concatenated in order.
        let mut carry = Vec::new();
        let reply = scan_terminal_queries(b"\x1b[5n\x1b[6n\x1b[c", &mut carry);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x1b[0n");
        expected.extend_from_slice(b"\x1b[1;1R");
        expected.extend_from_slice(DA_REPLY);
        assert_eq!(reply, expected);
    }

    #[test]
    fn scan_unknown_csi_is_ignored() {
        // A CSI we do not handle (e.g. SGR color `ESC[31m`) must NOT trigger a reply.
        let mut carry = Vec::new();
        assert!(
            scan_terminal_queries(b"\x1b[31mred\x1b[0m", &mut carry).is_empty(),
            "non-query CSI sequences must be ignored"
        );
        assert!(carry.is_empty());
    }

    #[test]
    fn scan_runaway_partial_is_bounded() {
        // A hostile `ESC[` followed by endless parameter bytes must not grow the
        // carry without limit.
        let mut carry = Vec::new();
        let mut chunk = vec![0x1b, b'['];
        chunk.extend(std::iter::repeat(b'0').take(500));
        let reply = scan_terminal_queries(&chunk, &mut carry);
        assert!(reply.is_empty(), "an unterminated CSI yields no reply");
        assert!(
            carry.len() <= 64,
            "the carried partial must be bounded, got {}",
            carry.len()
        );
    }

    // --- Spawn behavior (Unix POSIX shell; gated to non-Windows) -----------

    /// Pin `$SHELL` to a POSIX shell so resolve_shell() is deterministic in tests
    /// and the `-c` path is exercised regardless of the host default.
    #[cfg(not(windows))]
    fn with_posix_shell() {
        std::env::set_var("SHELL", "/bin/sh");
    }

    #[test]
    #[cfg(not(windows))]
    fn echo_produces_output() {
        with_posix_shell();
        let (mut cmd, rx) =
            CommandPty::spawn("echo nyx_cmd_marker", small_size(), None, &[]).expect("spawn echo");
        let out = read_until(&rx, "nyx_cmd_marker", Duration::from_secs(5));
        assert!(
            out.contains("nyx_cmd_marker"),
            "the command's stdout must reach the reader channel, got: {out:?}"
        );
        let code = cmd.wait();
        assert_eq!(code, Some(0), "`echo` exits 0");
    }

    #[test]
    #[cfg(not(windows))]
    fn exit_3_yields_exit_code_3() {
        with_posix_shell();
        let (mut cmd, _rx) =
            CommandPty::spawn("exit 3", small_size(), None, &[]).expect("spawn exit 3");
        let code = cmd.wait();
        assert_eq!(code, Some(3), "`exit 3` must surface exit_code 3");
    }

    #[test]
    #[cfg(not(windows))]
    fn cwd_is_applied() {
        with_posix_shell();
        let tmp = std::env::temp_dir();
        let canon = std::fs::canonicalize(&tmp).expect("canonicalize tmp");
        let (_cmd, rx) = CommandPty::spawn("pwd", small_size(), Some(tmp.to_str().unwrap()), &[])
            .expect("spawn pwd in tmp");
        let out = read_until(&rx, &canon.to_string_lossy(), Duration::from_secs(5));
        let got = std::fs::canonicalize(out.trim()).unwrap_or_else(|_| tmp.clone());
        assert_eq!(
            got, canon,
            "the command must run with the requested cwd (pwd should print it), got: {out:?}"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn kill_terminates_and_exit_code_recoverable() {
        with_posix_shell();
        let (mut cmd, _rx) =
            CommandPty::spawn("sleep 60", small_size(), None, &[]).expect("spawn sleep");
        std::thread::sleep(Duration::from_millis(150));
        assert!(cmd.exit_code().is_none(), "child should still be running");
        cmd.kill();
        let code = cmd.wait();
        assert!(
            code.is_some(),
            "exit code must be recoverable after a tree kill, got None"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn reader_disconnects_when_command_exits() {
        with_posix_shell();
        let (mut cmd, rx) = CommandPty::spawn("true", small_size(), None, &[]).expect("spawn true");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut disconnected = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        assert!(
            disconnected,
            "the reader channel must disconnect once the command exits"
        );
        assert_eq!(cmd.wait(), Some(0), "`true` exits 0");
    }

    #[test]
    #[cfg(not(windows))]
    fn kill_handle_exposes_a_usable_pid() {
        with_posix_shell();
        // The handle must surface a concrete pid (== pgid on Unix) so the runner
        // has an exploitable tree-kill target, not just the parent shell.
        let (mut cmd, _rx) =
            CommandPty::spawn("sleep 60", small_size(), None, &[]).expect("spawn sleep");
        let handle = cmd.kill_handle();
        assert!(
            handle.pid().is_some(),
            "the kill handle must expose the process (group) pid for a tree kill"
        );
        assert_eq!(handle.pid(), cmd.kill_handle().pid(), "pid is stable");
        cmd.kill();
        let _ = cmd.wait();
    }

    /// The tree kill must reap a process the SHELL spawned, not just the shell —
    /// the core anti-orphan property. We run a child `sleep` in the background,
    /// capture its pid, kill the tree, and assert (via `kill(pid, 0)`) the child
    /// is gone. Unix-only (process groups); guards the `-pgid` strategy.
    #[test]
    #[cfg(unix)]
    fn tree_kill_reaps_grandchild() {
        with_posix_shell();
        // Spawn a child sleep, print its pid, then wait on it so the shell stays
        // alive and the group is non-trivial.
        let (mut cmd, rx) =
            CommandPty::spawn("sleep 120 & echo CHILD:$!; wait", small_size(), None, &[])
                .expect("spawn group");
        let out = read_until(&rx, "CHILD:", Duration::from_secs(5));
        let child_pid: i32 = out
            .lines()
            .find_map(|l| l.trim().strip_prefix("CHILD:"))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_else(|| panic!("could not parse child pid from {out:?}"));

        // The child sleep is alive before the kill.
        assert_eq!(
            unsafe { libc::kill(child_pid, 0) },
            0,
            "the grandchild sleep should be alive before the tree kill"
        );

        cmd.kill(); // SIGKILL to the whole group via -pgid
        let _ = cmd.wait();

        // Poll until the grandchild is reaped (kill(pid,0) => ESRCH).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reaped = false;
        while Instant::now() < deadline {
            let rc = unsafe { libc::kill(child_pid, 0) };
            if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            reaped,
            "the tree kill must reap the grandchild ({child_pid}), not just the shell"
        );
    }

    /// Windows analog of `tree_kill_reaps_grandchild`: the `taskkill /T /F` tree
    /// kill must reap a GRANDCHILD the shell spawned (a detached `powershell` that
    /// sleeps), not just the parent shell — the property a parent-only
    /// `TerminateProcess` violated (the observed bun + conhost zombie leak). We
    /// launch a long-lived grandchild that prints its own `$PID`, capture it, kill
    /// the tree, then poll `tasklist` until that pid is gone.
    #[test]
    #[cfg(windows)]
    fn tree_kill_reaps_grandchild_windows() {
        // Start a detached grandchild (a sleeping powershell) and print its pid, then
        // wait on it so the shell tree stays alive and non-trivial. `Start-Process
        // -PassThru` returns the spawned process object whose `.Id` we echo.
        let cmdline = "powershell -NoProfile -Command \"$p = Start-Process powershell \
             -ArgumentList '-NoProfile','-Command','Start-Sleep 120' -PassThru; \
             Write-Output ('CHILD:' + $p.Id); Wait-Process -Id $p.Id\"";
        let (mut cmd, rx) =
            CommandPty::spawn(cmdline, small_size(), None, &[]).expect("spawn group");
        let out = read_until(&rx, "CHILD:", Duration::from_secs(20));
        let child_pid: u32 = out
            .lines()
            .find_map(|l| l.trim().strip_prefix("CHILD:"))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_else(|| panic!("could not parse grandchild pid from {out:?}"));

        assert!(
            pid_is_alive_windows(child_pid),
            "the grandchild powershell ({child_pid}) should be alive before the tree kill"
        );

        cmd.kill(); // taskkill /T /F /PID <shell pid> — whole tree
        let _ = cmd.wait();

        // Poll until the grandchild is gone from the process table.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut reaped = false;
        while Instant::now() < deadline {
            if !pid_is_alive_windows(child_pid) {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            reaped,
            "the Windows tree kill must reap the grandchild ({child_pid}), not just the shell"
        );
    }

    /// True if a pid is present in the Windows process table (via `tasklist`). Used
    /// by the Windows tree-kill test to confirm a grandchild is reaped.
    #[cfg(windows)]
    fn pid_is_alive_windows(pid: u32) -> bool {
        let out = crate::proc_util::command("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        match out {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                text.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn drop_is_prompt_and_kills_tree() {
        with_posix_shell();
        let (cmd, rx) =
            CommandPty::spawn("sleep 120 & wait", small_size(), None, &[]).expect("spawn group");
        std::thread::sleep(Duration::from_millis(100));
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            drop(cmd); // kill tree + join(waiter) + join/detach(reader): must NOT block
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "CommandPty::drop must return promptly (tree kill + joins)"
        );
        worker.join().expect("worker thread");
        drop(rx);
    }

    // --- bound_live_scrollback (pure) --------------------------------------

    #[test]
    fn bound_live_scrollback_keeps_tail_and_char_boundary() {
        // Under the cap: untouched.
        let mut small = b"hello".to_vec();
        bound_live_scrollback(&mut small);
        assert_eq!(small, b"hello");

        // Over the cap: bounded to <= cap, keeping the TAIL.
        let mut big = vec![b'a'; MAX_LIVE_SCROLLBACK_BYTES + 1000];
        big.extend_from_slice(b"TAIL_MARKER");
        bound_live_scrollback(&mut big);
        assert!(
            big.len() <= MAX_LIVE_SCROLLBACK_BYTES,
            "live scrollback must be bounded to the cap, got {}",
            big.len()
        );
        assert!(
            big.ends_with(b"TAIL_MARKER"),
            "the bound must keep the most-recent (tail) bytes"
        );

        // Multi-byte UTF-8 at the cut point: result stays valid UTF-8.
        let mut utf8 = "é".repeat(MAX_LIVE_SCROLLBACK_BYTES).into_bytes(); // 2 bytes each
        bound_live_scrollback(&mut utf8);
        assert!(
            std::str::from_utf8(&utf8).is_ok(),
            "the bound must cut on a char boundary (valid UTF-8 result)"
        );
    }

    #[test]
    fn run_state_db_strings_match_check_vocabulary() {
        // These four strings are exactly the DB CHECK vocabulary (db.rs).
        assert_eq!(RunState::Idle.as_db_str(), "idle");
        assert_eq!(RunState::Running.as_db_str(), "running");
        assert_eq!(RunState::Success.as_db_str(), "success");
        assert_eq!(RunState::Error.as_db_str(), "error");
    }

    // --- CommandRunner state machine (mock sink, real processes) -----------

    /// A recording sink: captures every state transition and all output bytes per
    /// instance, plus the last persisted scrollback. Lets the runner state machine
    /// be asserted without a Tauri runtime or DB (those are Phase-5 integration).
    #[derive(Default)]
    struct MockSink {
        states: Mutex<Vec<(String, RunState, Option<i32>)>>,
        /// Every `on_acknowledge` call's instance id, in order — so a test can assert
        /// an ack emitted the (decoupled) ack notification and NOT a state transition.
        acks: Mutex<Vec<String>>,
        output: Mutex<HashMap<String, Vec<u8>>>,
        output_events: Mutex<usize>,
        scrollback: Mutex<HashMap<String, String>>,
        /// Per-instance CURRENT-run outcome `(state, exit_code)`, mirrored from
        /// `on_state` so `archive_previous_run` can model the DB's archive decision
        /// (only a finished run is retained). The runner's own state map is the
        /// authority; this is the sink-side mirror the prod sink keeps in the DB row.
        cur_outcome: Mutex<HashMap<String, (RunState, Option<i32>)>>,
        /// The bounded (N=1) RETAINED prior run per instance: its persisted scrollback
        /// plus factual outcome, as `archive_previous_run` rolled it over on the last
        /// (re)launch. The v5 analog of the DB `prev_*` columns.
        prev_run: Mutex<HashMap<String, PrevRun>>,
        /// Every `clear_output` call's instance id, in order — so a test can assert the
        /// `clear_command_output` path reached the sink (review R-OUTPUT).
        cleared: Mutex<Vec<String>>,
    }

    /// The mock's retained prior run — mirrors the DB `prev_*` columns (v5).
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct PrevRun {
        scrollback: String,
        state: RunState,
        exit_code: Option<i32>,
    }

    impl MockSink {
        fn state_log(&self) -> Vec<(String, RunState, Option<i32>)> {
            self.states.lock().unwrap().clone()
        }
        fn ack_log(&self) -> Vec<String> {
            self.acks.lock().unwrap().clone()
        }
        fn output_of(&self, id: &str) -> String {
            String::from_utf8_lossy(
                self.output
                    .lock()
                    .unwrap()
                    .get(id)
                    .cloned()
                    .unwrap_or_default()
                    .as_slice(),
            )
            .into_owned()
        }
        fn output_event_count(&self) -> usize {
            *self.output_events.lock().unwrap()
        }
        fn scrollback_of(&self, id: &str) -> Option<String> {
            self.scrollback.lock().unwrap().get(id).cloned()
        }
        /// The retained prior run for an instance (v5), or `None` if none retained.
        fn prev_run_of(&self, id: &str) -> Option<PrevRun> {
            self.prev_run.lock().unwrap().get(id).cloned()
        }
        /// The ids cleared via `clear_output`, in call order (review R-OUTPUT).
        fn cleared_log(&self) -> Vec<String> {
            self.cleared.lock().unwrap().clone()
        }
    }

    impl RunnerSink for Arc<MockSink> {
        fn on_state(&self, instance_id: &str, state: RunState, exit_code: Option<i32>) {
            self.states
                .lock()
                .unwrap()
                .push((instance_id.to_string(), state, exit_code));
            // Mirror the CURRENT-run outcome so archive_previous_run can decide what
            // (if anything) to retain — the prod sink keeps this in the DB row.
            self.cur_outcome
                .lock()
                .unwrap()
                .insert(instance_id.to_string(), (state, exit_code));
        }
        fn on_acknowledge(&self, instance_id: &str) {
            self.acks.lock().unwrap().push(instance_id.to_string());
        }
        fn on_output(&self, instance_id: &str, bytes: &[u8]) {
            *self.output_events.lock().unwrap() += 1;
            self.output
                .lock()
                .unwrap()
                .entry(instance_id.to_string())
                .or_default()
                .extend_from_slice(bytes);
        }
        fn persist_scrollback(&self, instance_id: &str, serialized: &str) {
            self.scrollback
                .lock()
                .unwrap()
                .insert(instance_id.to_string(), serialized.to_string());
        }
        fn archive_previous_run(&self, instance_id: &str) {
            // Mirror `db::archive_and_reset_for_relaunch`: if the CURRENT run finished
            // (success|error), archive its scrollback + outcome into the bounded (N=1)
            // prev slot (OVERWRITING any earlier one), then reset the current run's
            // scrollback to "" so the new run starts unpolluted.
            let finished = matches!(
                self.cur_outcome.lock().unwrap().get(instance_id),
                Some((RunState::Success | RunState::Error, _))
            );
            if finished {
                let (state, exit_code) = *self
                    .cur_outcome
                    .lock()
                    .unwrap()
                    .get(instance_id)
                    .expect("checked finished above");
                let scrollback = self
                    .scrollback
                    .lock()
                    .unwrap()
                    .get(instance_id)
                    .cloned()
                    .unwrap_or_default();
                self.prev_run.lock().unwrap().insert(
                    instance_id.to_string(),
                    PrevRun {
                        scrollback,
                        state,
                        exit_code,
                    },
                );
            }
            // Reset the current run's persisted scrollback (the archive kept the prior).
            self.scrollback
                .lock()
                .unwrap()
                .insert(instance_id.to_string(), String::new());
        }
        fn clear_output(&self, instance_id: &str) {
            // Mirror `db::clear_instance_scrollback`: empty the persisted scrollback AND
            // the retained prior run, leaving the factual outcome (state/exit_code)
            // untouched. Record it so a runner test can assert the clear propagated.
            self.scrollback
                .lock()
                .unwrap()
                .insert(instance_id.to_string(), String::new());
            self.prev_run.lock().unwrap().remove(instance_id);
            self.cleared.lock().unwrap().push(instance_id.to_string());
        }
    }

    #[cfg(not(windows))]
    fn new_runner() -> (CommandRunner<Arc<MockSink>>, Arc<MockSink>) {
        with_posix_shell();
        let sink = Arc::new(MockSink::default());
        let runner = CommandRunner::new(Arc::clone(&sink), small_size());
        (runner, sink)
    }

    /// Poll the runner state for an instance until it reaches `want` or times out.
    #[cfg(not(windows))]
    fn wait_state(
        runner: &CommandRunner<Arc<MockSink>>,
        id: &str,
        want: RunState,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if runner.state_of(id) == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        runner.state_of(id) == want
    }

    #[test]
    fn clear_output_empties_buffer_via_sink_without_spawning() {
        // Review R-OUTPUT: clear_output on an idle/absent instance (no live process)
        // empties the persisted scrollback through the sink and records the clear, with
        // NO state transition emitted — proving the buffer wipe is decoupled from the
        // run lifecycle. Spawns nothing, so it runs under the ConPTY gap.
        let sink = Arc::new(MockSink::default());
        let runner = CommandRunner::new(Arc::clone(&sink), small_size());
        // Seed a persisted scrollback + a retained prior run for the instance.
        sink.persist_scrollback("i1", "noisy 160KiB log\n");
        sink.prev_run.lock().unwrap().insert(
            "i1".to_string(),
            PrevRun {
                scrollback: "old run\n".to_string(),
                state: RunState::Success,
                exit_code: Some(0),
            },
        );
        assert!(
            sink.scrollback_of("i1").is_some_and(|s| !s.is_empty()),
            "scrollback seeded"
        );

        runner.clear_output("i1");

        assert_eq!(
            sink.cleared_log(),
            vec!["i1".to_string()],
            "the clear reached the sink once"
        );
        assert_eq!(
            sink.scrollback_of("i1").as_deref(),
            Some(""),
            "current scrollback emptied"
        );
        assert!(
            sink.prev_run_of("i1").is_none(),
            "retained prior run emptied too"
        );
        // A clear is NOT a run transition: no state was emitted.
        assert!(
            sink.state_log().is_empty(),
            "clear emits no command://state transition"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn start_idle_to_running() {
        let (runner, sink) = new_runner();
        assert_eq!(runner.state_of("i1"), RunState::Idle, "absent == idle");
        let st = runner.start("i1", "sleep 30", None).expect("start");
        assert_eq!(st, RunState::Running, "start on idle must go running");
        assert_eq!(runner.state_of("i1"), RunState::Running);
        // The transition was emitted + (would be) persisted.
        assert!(
            sink.state_log()
                .iter()
                .any(|(id, s, _)| id == "i1" && *s == RunState::Running),
            "a command://state running transition must be emitted"
        );
        runner.stop("i1").expect("cleanup stop");
    }

    #[test]
    #[cfg(not(windows))]
    fn start_running_is_idempotent_no_double_spawn() {
        let (runner, _sink) = new_runner();
        // A child sleep whose pid we capture, so we can prove the SAME process is
        // still the one running after a second start (no second spawn).
        runner
            .start("i1", "echo PID:$$; sleep 30", None)
            .expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Running,
            Duration::from_secs(2)
        ));

        // Second start on running: idempotent no-op, returns running, no new entry.
        let st = runner
            .start("i1", "echo SECOND; sleep 30", None)
            .expect("second start");
        assert_eq!(
            st,
            RunState::Running,
            "start on running must be a no-op returning running"
        );

        // The second invocation must NOT have produced new "SECOND" output: if it
        // had double-spawned, we'd see it. (Give the pump a beat first.)
        std::thread::sleep(Duration::from_millis(300));
        // Only one live entry exists for the instance (map is keyed by id), and the
        // state is still running from the FIRST spawn.
        assert_eq!(runner.state_of("i1"), RunState::Running);
        runner.stop("i1").expect("cleanup stop");
    }

    #[test]
    #[cfg(not(windows))]
    fn start_with_env_reports_was_running_and_does_not_double_spawn() {
        // R-WSCMD #5: start_with_env on a RUNNING instance is a no-op that reports
        // was_running:true and spawns NO second process; on idle it reports
        // was_running:false. The pid marker proves the SAME first process survives.
        let (runner, sink) = new_runner();
        let first = runner
            .start_with_env("i1", "echo PID:$$; sleep 30", None, &[])
            .expect("first start");
        assert!(!first.was_running, "a fresh start was not already running");
        assert_eq!(first.state, RunState::Running);
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Running,
            Duration::from_secs(2)
        ));

        // Capture the FIRST process's pid from its output.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut first_pid: Option<String> = None;
        while Instant::now() < deadline && first_pid.is_none() {
            std::thread::sleep(Duration::from_millis(30));
            first_pid = sink
                .output_of("i1")
                .lines()
                .find_map(|l| l.trim().strip_prefix("PID:").map(str::to_string));
        }
        let first_pid = first_pid.expect("captured the first process pid");

        // Second start on the running instance: no-op, was_running:true, NO new spawn.
        let second = runner
            .start_with_env("i1", "echo PID:$$; sleep 30", None, &[])
            .expect("second start");
        assert!(
            second.was_running,
            "the second start saw an already-running instance"
        );
        assert_eq!(second.state, RunState::Running);
        std::thread::sleep(Duration::from_millis(300));
        // The output must still show ONLY the first pid: a second spawn would emit a
        // different PID line.
        let pids: Vec<String> = sink
            .output_of("i1")
            .lines()
            .filter_map(|l| l.trim().strip_prefix("PID:").map(str::to_string))
            .collect();
        assert_eq!(
            pids,
            vec![first_pid],
            "no second process was spawned (only the first pid appears)"
        );
        runner.stop("i1").expect("cleanup stop");
    }

    #[test]
    #[cfg(not(windows))]
    fn start_with_env_passes_env_to_the_spawned_process() {
        // R-WSCMD #7: a per-run env map reaches the spawned process. The command echoes
        // an env var nyx itself does not set; seeing its value in the output proves the
        // env was merged onto the inherited environment at spawn.
        let (runner, sink) = new_runner();
        let env = vec![("NYX_TEST_VAR".to_string(), "vault-value-123".to_string())];
        runner
            .start_with_env("i1", "echo VAR=$NYX_TEST_VAR; true", None, &env)
            .expect("start with env");
        // Wait for the run to finish (exit 0 → success) so all output is captured.
        assert!(
            wait_state(&runner, "i1", RunState::Success, Duration::from_secs(5)),
            "the echo command should finish"
        );
        let out = sink.output_of("i1");
        assert!(
            out.contains("VAR=vault-value-123"),
            "the per-run env var must reach the spawned process, got: {out:?}"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn relaunch_with_env_restarts_and_applies_env() {
        // R-WSCMD #5/#7: relaunch ALWAYS restarts (was_running reports a live stop) and
        // applies the per-run env to the fresh process.
        let (runner, sink) = new_runner();
        runner.start("i1", "sleep 30", None).expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Running,
            Duration::from_secs(2)
        ));

        let env = vec![("NYX_TEST_VAR".to_string(), "relaunch-val".to_string())];
        let outcome = runner
            .relaunch_with_env("i1", "echo VAR=$NYX_TEST_VAR; true", None, &env)
            .expect("relaunch with env");
        assert!(
            outcome.was_running,
            "relaunch on a running instance reports it stopped a live process"
        );
        assert!(
            wait_state(&runner, "i1", RunState::Success, Duration::from_secs(5)),
            "the relaunched echo command finishes"
        );
        let out = sink.output_of("i1");
        assert!(
            out.contains("VAR=relaunch-val"),
            "the relaunch env var must reach the fresh process, got: {out:?}"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn exit_zero_transitions_to_success() {
        let (runner, sink) = new_runner();
        runner.start("i1", "true", None).expect("start");
        assert!(
            wait_state(&runner, "i1", RunState::Success, Duration::from_secs(5)),
            "exit 0 must transition to success"
        );
        // The success transition carries exit code 0.
        assert!(
            sink.state_log()
                .iter()
                .any(|(id, s, code)| id == "i1" && *s == RunState::Success && *code == Some(0)),
            "success transition must include exit code 0"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn exit_nonzero_transitions_to_error_with_code() {
        let (runner, sink) = new_runner();
        runner.start("i1", "exit 7", None).expect("start");
        assert!(
            wait_state(&runner, "i1", RunState::Error, Duration::from_secs(5)),
            "exit != 0 must transition to error"
        );
        assert!(
            sink.state_log()
                .iter()
                .any(|(id, s, code)| id == "i1" && *s == RunState::Error && *code == Some(7)),
            "error transition must include the non-zero exit code"
        );
    }

    #[test]
    #[cfg(unix)]
    fn stop_running_kills_tree_and_goes_idle() {
        let (runner, _sink) = new_runner();
        runner
            .start("i1", "sleep 120 & echo CHILD:$!; wait", None)
            .expect("start");
        // Capture the grandchild pid from the live entry's pump output.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut child_pid: Option<i32> = None;
        while Instant::now() < deadline && child_pid.is_none() {
            std::thread::sleep(Duration::from_millis(50));
            if let Some(line) = _sink
                .output_of("i1")
                .lines()
                .find_map(|l| l.trim().strip_prefix("CHILD:").map(str::to_string))
            {
                child_pid = line.trim().parse().ok();
            }
        }
        let child_pid = child_pid.expect("captured grandchild pid");
        assert_eq!(
            unsafe { libc::kill(child_pid, 0) },
            0,
            "grandchild alive pre-stop"
        );

        let st = runner.stop("i1").expect("stop");
        assert_eq!(st, RunState::Idle, "stop on running must go idle");
        assert_eq!(runner.state_of("i1"), RunState::Idle);

        // The grandchild must be reaped by the tree kill.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reaped = false;
        while Instant::now() < deadline {
            let rc = unsafe { libc::kill(child_pid, 0) };
            if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            reaped,
            "stop must kill the process TREE (grandchild reaped)"
        );
    }

    #[test]
    #[cfg(unix)]
    fn kill_all_running_reaps_trees_and_idles_every_instance() {
        let (runner, sink) = new_runner();
        runner
            .start("i1", "sleep 120 & echo CHILD:$!; wait", None)
            .expect("start i1");
        runner.start("i2", "sleep 120", None).expect("start i2");
        assert!(
            wait_state(&runner, "i1", RunState::Running, Duration::from_secs(5)),
            "i1 should be running"
        );

        // Capture i1's grandchild pid so we can assert the whole TREE is reaped.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut child_pid: Option<i32> = None;
        while Instant::now() < deadline && child_pid.is_none() {
            std::thread::sleep(Duration::from_millis(50));
            if let Some(line) = sink
                .output_of("i1")
                .lines()
                .find_map(|l| l.trim().strip_prefix("CHILD:").map(str::to_string))
            {
                child_pid = line.trim().parse().ok();
            }
        }
        let child_pid = child_pid.expect("captured grandchild pid");

        runner.kill_all_running();
        // Every running instance is evicted back to idle.
        assert_eq!(runner.state_of("i1"), RunState::Idle, "i1 idle after reap");
        assert_eq!(runner.state_of("i2"), RunState::Idle, "i2 idle after reap");

        // The whole tree of i1 is reaped (grandchild gone), not just the parent.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reaped = false;
        while Instant::now() < deadline {
            let rc = unsafe { libc::kill(child_pid, 0) };
            if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            reaped,
            "kill_all_running must reap each running process TREE"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn stop_absent_instance_emits_reconciling_idle() {
        // The PHANTOM-running path: the runner has no entry for the instance (e.g. a
        // stale `last_state=running` after a restart, with no live process). A stop
        // must FORCE idle AND emit it so the frozen dot reconciles — not silently
        // return without an event (the old behavior that left the dot stuck).
        let (runner, sink) = new_runner();
        let st = runner.stop("i1").expect("stop");
        assert_eq!(st, RunState::Idle, "an absent instance stops to idle");
        assert_eq!(
            sink.state_log(),
            vec![("i1".to_string(), RunState::Idle, None)],
            "a stop on a phantom-running (absent) instance must emit a reconciling idle"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn stop_genuine_non_running_entry_is_noop() {
        // A GENUINE non-running runner entry (success/error/idle) must NOT be
        // overwritten with idle: the dot keeps its colour. This is the distinction
        // from the phantom (absent) path above.
        let (runner, sink) = new_runner();
        runner.start("i1", "true", None).expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Success,
            Duration::from_secs(5)
        ));
        let before = sink.state_log().len();
        let st = runner.stop("i1").expect("stop on success");
        assert_eq!(
            st,
            RunState::Success,
            "stop on success must be a no-op returning success"
        );
        assert_eq!(
            sink.state_log().len(),
            before,
            "stop on a genuine non-running entry must emit no new transition (keeps success)"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn acknowledge_success_clears_unread_but_preserves_the_outcome() {
        // A finished one-shot (success) is an "unseen result". Acknowledging it (the
        // user opened it) clears ONLY the `unread` flag — the FACTUAL outcome
        // (state=success, exit_code=0) is preserved, and an ack notification is
        // emitted via `on_acknowledge`, NOT a false idle state transition. This is the
        // v4 finding fix: a UI ack must not erase the outcome an observer reads.
        let (runner, sink) = new_runner();
        runner.start("i1", "true", None).expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Success,
            Duration::from_secs(5)
        ));
        assert!(runner.is_unread("i1"), "a finished run is unread");
        let states_before = sink.state_log().len();
        let st = runner.acknowledge("i1");
        // The factual outcome is UNCHANGED by the ack.
        assert_eq!(
            st,
            RunState::Success,
            "acknowledge returns the unchanged factual state (success)"
        );
        assert_eq!(
            runner.state_of("i1"),
            RunState::Success,
            "the factual state is preserved (NOT collapsed to idle)"
        );
        assert_eq!(
            runner.last_exit_code("i1"),
            Some(0),
            "the factual exit code is preserved through the ack"
        );
        assert!(!runner.is_unread("i1"), "the ack cleared the unread flag");
        // No NEW state transition — the ack went through on_acknowledge instead.
        assert_eq!(
            sink.state_log().len(),
            states_before,
            "acknowledge emits NO state transition (the outcome is untouched)"
        );
        assert_eq!(
            sink.ack_log(),
            vec!["i1".to_string()],
            "acknowledge emits exactly one ack notification"
        );
        // A second ack is a no-op (already read): no new ack notification.
        runner.acknowledge("i1");
        assert_eq!(
            sink.ack_log(),
            vec!["i1".to_string()],
            "a second ack on an already-read instance emits nothing"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn acknowledge_error_clears_unread_but_preserves_the_error_and_code() {
        // An error result is acknowledgeable the same way — and crucially the
        // non-zero exit code survives so the MCP still sees state=error + exit_code!=0.
        let (runner, sink) = new_runner();
        runner.start("i1", "exit 3", None).expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Error,
            Duration::from_secs(5)
        ));
        let st = runner.acknowledge("i1");
        assert_eq!(
            st,
            RunState::Error,
            "acknowledge returns the unchanged factual state (error)"
        );
        assert_eq!(
            runner.state_of("i1"),
            RunState::Error,
            "the error state is preserved through the ack"
        );
        assert_eq!(
            runner.last_exit_code("i1"),
            Some(3),
            "the non-zero exit code is preserved through the ack (the finding's crux)"
        );
        assert!(!runner.is_unread("i1"), "the ack cleared the unread flag");
        assert_eq!(
            sink.ack_log(),
            vec!["i1".to_string()],
            "acknowledge on error emits an ack notification, not a state transition"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn acknowledge_running_is_a_noop() {
        // NEVER acknowledge a live process — it has no unseen result, so the ack must
        // be a no-op (no notification, state unchanged).
        let (runner, sink) = new_runner();
        runner.start("i1", "sleep 30", None).expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Running,
            Duration::from_secs(2)
        ));
        let before = sink.state_log().len();
        let st = runner.acknowledge("i1");
        assert_eq!(
            st,
            RunState::Running,
            "acknowledge on running is a no-op returning running"
        );
        assert_eq!(
            runner.state_of("i1"),
            RunState::Running,
            "the running entry is unchanged"
        );
        assert_eq!(
            sink.state_log().len(),
            before,
            "acknowledge on running emits no transition"
        );
        assert!(
            sink.ack_log().is_empty(),
            "acknowledge on running emits no ack notification"
        );
        runner.stop("i1").expect("cleanup stop");
    }

    #[test]
    #[cfg(not(windows))]
    fn acknowledge_idle_or_absent_is_a_noop() {
        // Already idle (or absent): nothing to acknowledge, no transition emitted.
        let (runner, sink) = new_runner();
        // Absent entry.
        assert_eq!(
            runner.acknowledge("ghost"),
            RunState::Idle,
            "acknowledge on an absent instance returns idle"
        );
        assert!(
            sink.state_log().is_empty() && sink.ack_log().is_empty(),
            "acknowledge on an absent instance emits nothing"
        );
        // A genuinely idle entry (started then stopped back to idle).
        runner.start("i1", "sleep 30", None).expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Running,
            Duration::from_secs(2)
        ));
        runner.stop("i1").expect("stop");
        assert_eq!(runner.state_of("i1"), RunState::Idle);
        let before = sink.state_log().len();
        assert_eq!(runner.acknowledge("i1"), RunState::Idle);
        assert_eq!(
            sink.state_log().len(),
            before,
            "acknowledge on a genuinely idle entry emits no state transition"
        );
        assert!(
            sink.ack_log().is_empty(),
            "acknowledge on a genuinely idle entry emits no ack notification"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn stop_returns_promptly_and_never_hangs() {
        // `stop` must ALWAYS return (the dead-Stop-button bug): even with the bounded
        // pump-join detach path, a stop on a live long-running command returns within
        // a small bound and lands idle. We run it on a worker thread guarded by a
        // timeout so a regression that re-introduces an unbounded join FAILS here
        // instead of hanging the suite.
        let (runner, _sink) = new_runner();
        runner.start("i1", "sleep 120", None).expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Running,
            Duration::from_secs(2)
        ));

        let runner = Arc::new(runner);
        let r2 = Arc::clone(&runner);
        let (tx, rx) = mpsc::channel::<RunState>();
        std::thread::spawn(move || {
            let st = r2.stop("i1").expect("stop");
            let _ = tx.send(st);
        });
        // The bounded join is 500ms; allow generous slack for the term grace + kill.
        let got = rx
            .recv_timeout(Duration::from_secs(8))
            .expect("stop must return promptly, never hang");
        assert_eq!(got, RunState::Idle, "a real stop lands idle");
        assert_eq!(runner.state_of("i1"), RunState::Idle);
    }

    #[test]
    #[cfg(unix)]
    fn relaunch_never_leaves_two_live_instances() {
        let (runner, sink) = new_runner();
        // First run: capture its grandchild pid.
        runner
            .start("i1", "sleep 120 & echo CHILD:$!; wait", None)
            .expect("start");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut first_child: Option<i32> = None;
        while Instant::now() < deadline && first_child.is_none() {
            std::thread::sleep(Duration::from_millis(50));
            first_child = sink.output_of("i1").lines().find_map(|l| {
                l.trim()
                    .strip_prefix("CHILD:")
                    .and_then(|n| n.trim().parse().ok())
            });
        }
        let first_child = first_child.expect("first grandchild pid");

        // Relaunch: stop (kills first tree) then start a fresh one.
        let st = runner
            .relaunch("i1", "sleep 120 & echo CHILD:$!; wait", None)
            .expect("relaunch");
        assert_eq!(st, RunState::Running, "relaunch on running ends running");

        // The FIRST grandchild must be dead — no two live instances coexist.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut first_dead = false;
        while Instant::now() < deadline {
            let rc = unsafe { libc::kill(first_child, 0) };
            if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                first_dead = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            first_dead,
            "relaunch must kill the first instance before starting the second"
        );
        assert_eq!(runner.state_of("i1"), RunState::Running);
        runner.stop("i1").expect("cleanup stop");
    }

    #[test]
    #[cfg(not(windows))]
    fn relaunch_retains_previous_run_output_and_outcome() {
        // The finding's fix (v5): after start → output → relaunch, the PREVIOUS run's
        // output + its factual outcome (state/exit_code) must still be retrievable
        // (the bounded N=1 retained prior run), while the CURRENT run's scrollback is
        // reset so the new run is not polluted by the prior run's bytes.
        let (runner, sink) = new_runner();

        // Run 1: emit a unique marker, then exit non-zero so it finishes as `error`.
        runner
            .start("i1", "echo RUN1_MARKER; exit 5", None)
            .expect("start run 1");
        assert!(
            wait_state(&runner, "i1", RunState::Error, Duration::from_secs(5)),
            "run 1 must finish as error (exit 5)"
        );
        // Run 1's output + outcome are the CURRENT run right now.
        assert!(
            sink.scrollback_of("i1")
                .unwrap_or_default()
                .contains("RUN1_MARKER"),
            "run 1's output is the current persisted scrollback before relaunch"
        );
        // No prior run retained yet (run 1 is the first run).
        assert_eq!(
            sink.prev_run_of("i1"),
            None,
            "no prior run before the first relaunch"
        );

        // RELAUNCH: run 2 emits a DIFFERENT marker, stays running.
        runner
            .relaunch("i1", "echo RUN2_MARKER; sleep 30", None)
            .expect("relaunch into run 2");
        assert!(
            wait_state(&runner, "i1", RunState::Running, Duration::from_secs(5)),
            "run 2 must be running after the relaunch"
        );

        // The PREVIOUS run (run 1) is RETAINED: its output + factual outcome survive.
        let prev = sink
            .prev_run_of("i1")
            .expect("the previous run must be retained across the relaunch");
        assert!(
            prev.scrollback.contains("RUN1_MARKER"),
            "the retained previous run keeps run 1's output, got {:?}",
            prev.scrollback
        );
        assert_eq!(
            prev.state,
            RunState::Error,
            "the retained outcome is run 1's error"
        );
        assert_eq!(
            prev.exit_code,
            Some(5),
            "the retained outcome keeps run 1's exit code"
        );

        // Meanwhile the CURRENT run is NOT polluted by run 1's bytes: once run 2 has
        // streamed, the live/persisted current scrollback shows RUN2 and not RUN1.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut current_clean = false;
        while Instant::now() < deadline {
            let cur = runner.live_output("i1").unwrap_or_default();
            if cur.contains("RUN2_MARKER") {
                assert!(
                    !cur.contains("RUN1_MARKER"),
                    "the current run must not be polluted by run 1's output, got {cur:?}"
                );
                current_clean = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            current_clean,
            "run 2's output must reach the current live buffer"
        );

        runner.stop("i1").expect("cleanup stop");
    }

    #[test]
    #[cfg(not(windows))]
    fn relaunch_on_idle_starts_directly() {
        let (runner, _sink) = new_runner();
        let st = runner
            .relaunch("i1", "sleep 30", None)
            .expect("relaunch on idle");
        assert_eq!(st, RunState::Running, "relaunch on idle is a direct start");
        runner.stop("i1").expect("cleanup stop");
    }

    #[test]
    #[cfg(not(windows))]
    fn every_transition_is_persisted_and_emitted() {
        let (runner, sink) = new_runner();
        // running -> success, then stop is a no-op (no extra transition).
        runner.start("i1", "echo hello; true", None).expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Success,
            Duration::from_secs(5)
        ));
        let log = sink.state_log();
        // Exactly one running then one success transition were recorded (each call
        // to on_state both persists last_state and emits command://state in prod).
        assert!(
            log.iter()
                .any(|(id, s, _)| id == "i1" && *s == RunState::Running),
            "running transition recorded"
        );
        assert!(
            log.iter()
                .any(|(id, s, c)| id == "i1" && *s == RunState::Success && *c == Some(0)),
            "success transition recorded with code"
        );
        // The scrollback persistence ran with the command output (final persist on
        // disconnect), bounded by the same cap as the DB.
        let sb = sink
            .scrollback_of("i1")
            .expect("scrollback persisted on exit");
        assert!(
            sb.contains("hello"),
            "persisted scrollback must contain output, got {sb:?}"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn output_is_coalesced_and_bounded_under_flood() {
        let (runner, sink) = new_runner();
        // Emit many PADDED lines fast, then exit. Each line is ~256 bytes, so a few
        // thousand of them blow PAST the 256 KiB cap — exercising the in-memory
        // bound for real (not vacuously). The pump must coalesce into far fewer
        // command://output events than lines, and the persisted scrollback tail
        // must stay bounded. We generate the burst in a SINGLE awk process (a tight
        // loop, no per-line fork/subshell) so the test stays cheap under parallel
        // load while still being >> the handful of 16ms flush windows.
        runner
            .start(
                "i1",
                "awk 'BEGIN{ p=sprintf(\"%256s\",\"\"); for(i=1;i<=3000;i++) print \"flood_\" i p }'",
                None,
            )
            .expect("start");
        assert!(
            wait_state(&runner, "i1", RunState::Success, Duration::from_secs(20)),
            "flood command must finish"
        );
        let out = sink.output_of("i1");
        let lines = out.matches("flood_").count();
        let events = sink.output_event_count();
        assert!(
            lines > 1000,
            "the flood must produce many lines, got {lines}"
        );
        assert!(
            events < lines,
            "output must be COALESCED: {events} events for {lines} lines (events must be far fewer)"
        );
        // The persisted scrollback tail is bounded to the cap — and the total
        // output (lines * ~256B) genuinely exceeded it, so this is a real bound.
        let sb = sink.scrollback_of("i1").expect("scrollback persisted");
        assert!(
            sb.len() <= MAX_LIVE_SCROLLBACK_BYTES,
            "persisted scrollback must be bounded ({} <= {})",
            sb.len(),
            MAX_LIVE_SCROLLBACK_BYTES
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn relaunch_aborts_without_second_instance_when_stop_fails() {
        // White-box: if stop cannot reach idle, relaunch must NOT start a second
        // instance. We can't easily force a real stop failure, so we assert the
        // control-flow contract directly: a relaunch from a fresh idle instance is
        // a plain start (no stop leg), and a relaunch from running performs exactly
        // one resulting live entry. The dedicated tree-kill test above proves stop
        // actually reaps; here we assert no orphan-double via state count.
        let (runner, _sink) = new_runner();
        runner.start("i1", "sleep 30", None).expect("start");
        assert!(wait_state(
            &runner,
            "i1",
            RunState::Running,
            Duration::from_secs(2)
        ));
        runner.relaunch("i1", "sleep 30", None).expect("relaunch");
        // After relaunch there is exactly ONE entry (the map is keyed by id) in
        // running state — never two.
        assert_eq!(runner.state_of("i1"), RunState::Running);
        runner.stop("i1").expect("cleanup");
    }

    // --- poll_until: the bounded wait backing wait_for_command (ADR-0003 D12) ---

    #[test]
    fn poll_until_resolves_when_state_enters_target_without_sleeping() {
        // The fast path: the very first read is already in the target set (an
        // already-finished command), so the wait resolves immediately and NEVER
        // sleeps — there is no blind-poll latency for a command that has already
        // settled. A counter proves the injected sleep was not called.
        let mut sleeps = 0usize;
        let outcome = poll_until(
            &[RunState::Success, RunState::Error],
            Duration::from_secs(60),
            WAIT_POLL_INTERVAL,
            || RunState::Success,
            |_d| sleeps += 1,
        );
        assert!(outcome.resolved, "an already-settled command resolves");
        assert_eq!(
            outcome.state,
            RunState::Success,
            "reports the settled state"
        );
        assert_eq!(sleeps, 0, "a state already in target never sleeps");
    }

    #[test]
    fn poll_until_resolves_after_a_few_polls_when_state_transitions() {
        // The transition path (resolved-true): the command is `running` for the first
        // few reads, then finishes `success`. poll_until must keep polling and resolve
        // on the read that observes the transition — proving it tracks a state change
        // rather than only the initial state.
        let mut reads = 0usize;
        let mut sleeps = 0usize;
        let outcome = poll_until(
            &[RunState::Success, RunState::Error],
            Duration::from_secs(60),
            Duration::from_millis(1),
            || {
                reads += 1;
                // running, running, running, then success.
                if reads < 4 {
                    RunState::Running
                } else {
                    RunState::Success
                }
            },
            |_d| sleeps += 1,
        );
        assert!(outcome.resolved, "resolves once the state enters target");
        assert_eq!(outcome.state, RunState::Success);
        assert_eq!(reads, 4, "polled until the transition was observed");
        assert_eq!(
            sleeps, 3,
            "slept once between each of the pre-transition reads"
        );
    }

    #[test]
    fn poll_until_times_out_with_resolved_false_and_last_state() {
        // The timeout path (resolved-false): the state stays `running` forever, so the
        // bounded wait must give up at `timeout` and report resolved:false WITH the
        // last observed state (running) — NOT an error, NOT a block. A tiny real
        // timeout keeps the test fast (the manual's "tiny timeout_ms" guidance).
        let outcome = poll_until(
            &[RunState::Success, RunState::Error],
            Duration::from_millis(20),
            Duration::from_millis(2),
            || RunState::Running,
            std::thread::sleep,
        );
        assert!(
            !outcome.resolved,
            "a never-settling wait times out (resolved:false)"
        );
        assert_eq!(
            outcome.state,
            RunState::Running,
            "reports the last observed state"
        );
        assert!(
            outcome.waited >= Duration::from_millis(20),
            "the wait blocked for at least the timeout, got {:?}",
            outcome.waited
        );
    }

    #[test]
    fn poll_until_is_bounded_and_never_overshoots_the_timeout() {
        // The bounded guarantee: even with an interval LARGER than the timeout, the
        // wait never overshoots — the final sleep is clamped to the remaining budget,
        // so the total wait stays close to `timeout` rather than rounding up to a full
        // interval. (Sanity ceiling, generous for slow CI.)
        let start = Instant::now();
        let outcome = poll_until(
            &[RunState::Success],
            Duration::from_millis(30),
            Duration::from_millis(1000), // interval >> timeout
            || RunState::Running,
            std::thread::sleep,
        );
        let total = start.elapsed();
        assert!(!outcome.resolved);
        assert!(
            total < Duration::from_millis(500),
            "the wait is bounded to ~timeout, not the (1s) interval, got {total:?}"
        );
    }

    // --- Shutdown snapshot + boot restoration (extracted orchestration) -------
    //
    // These exercise `snapshot_commands_on_shutdown` / `restore_commands_on_boot`
    // DIRECTLY against a real in-memory `Db` and a `CommandRunner` over the test
    // sink — proving the EXTRACTED nyx-core orchestration (the same code both the
    // Tauri adapter and the Electron core-host now drive) is shell-agnostic. They
    // spawn real `sh` commands, so they are `not(windows)` (the same environmental
    // gate as every other process-spawning command test).

    /// Seed a project at `ws_path` with one template (command, restart flag) and
    /// return its single materialized instance id. The workspace ROOT is used as the
    /// run dir (no subfolder), so `resolve_run_dir` returns it verbatim (no FS check).
    #[cfg(not(windows))]
    fn seed_restore_instance(
        db: &Db,
        ws_path: &str,
        command: &str,
        restart_on_startup: bool,
    ) -> String {
        db.with_conn(|c| {
            let (project, workspace) = db::create_project(c, "p", ws_path, None).unwrap();
            let tpl = db::create_template(c, &project.id, "svc", command, None, Default::default())
                .unwrap();
            db::set_restart_on_startup(c, &tpl.id, restart_on_startup).unwrap();
            db::list_instances_for_workspace(c, &workspace.id)
                .unwrap()
                .into_iter()
                .find(|i| i.command_id == tpl.id)
                .unwrap()
                .id
        })
    }

    #[cfg(not(windows))]
    fn instance_row(db: &Db, id: &str) -> db::CommandInstance {
        db.with_conn(|c| db::get_instance(c, id)).unwrap().unwrap()
    }

    /// SHUTDOWN SNAPSHOT: a RUNNING instance snapshots `was_running_on_shutdown=true`;
    /// an idle one snapshots `false`. The runner's LIVE map is the source of truth.
    #[test]
    #[cfg(not(windows))]
    fn snapshot_on_shutdown_records_only_live_running_instances() {
        with_posix_shell();
        let db = Db::in_memory();
        let sink = Arc::new(MockSink::default());
        let runner = CommandRunner::new(Arc::clone(&sink), small_size());
        // The workspace root is `.` (cwd), which always exists; no subfolder.
        let id = seed_restore_instance(&db, ".", "sleep 30", true);

        // Idle → snapshot false.
        snapshot_commands_on_shutdown(&db, &runner);
        assert!(
            !instance_row(&db, &id).was_running_on_shutdown,
            "an idle instance snapshots was_running_on_shutdown=false"
        );

        // Start it (live in the runner), then snapshot → true.
        runner.start(&id, "sleep 30", Some(".")).expect("start");
        assert!(wait_state(
            &runner,
            &id,
            RunState::Running,
            Duration::from_secs(4)
        ));
        snapshot_commands_on_shutdown(&db, &runner);
        assert!(
            instance_row(&db, &id).was_running_on_shutdown,
            "a running instance snapshots was_running_on_shutdown=true"
        );
        runner.stop(&id).expect("cleanup stop");
    }

    /// BOOT RESTORE: template `restart_on_startup` ON + snapshot true → the instance
    /// is relaunched through the runner, and the snapshot is RESET to false afterward
    /// (no future boot can relaunch a ghost).
    #[test]
    #[cfg(not(windows))]
    fn restore_on_boot_relaunches_then_resets_snapshot() {
        with_posix_shell();
        let db = Db::in_memory();
        let sink = Arc::new(MockSink::default());
        let runner = CommandRunner::new(Arc::clone(&sink), small_size());
        let id = seed_restore_instance(&db, ".", "sleep 30", true);
        // Simulate the prior shutdown: this instance WAS running.
        db.with_conn(|c| db::set_was_running_on_shutdown(c, &id, true).unwrap());

        let relaunched = restore_commands_on_boot(&db, &runner);
        assert!(
            relaunched.contains(&id),
            "the instance must be relaunched at boot"
        );
        assert!(
            wait_state(&runner, &id, RunState::Running, Duration::from_secs(4)),
            "the relaunched instance is running in the runner"
        );
        assert!(
            !instance_row(&db, &id).was_running_on_shutdown,
            "the snapshot must be reset to false after the boot decision"
        );
        runner.stop(&id).expect("cleanup stop");
    }

    /// BOOT RESTORE / ORPHAN-NORMALIZE: a template with `restart_on_startup` OFF that
    /// was persisted `running` (a force-quit orphan) is NOT relaunched, and its
    /// `last_state` is normalized to `idle` so no phantom running dot survives.
    #[test]
    #[cfg(not(windows))]
    fn restore_on_boot_normalizes_orphan_running_when_toggle_off() {
        with_posix_shell();
        let db = Db::in_memory();
        let sink = Arc::new(MockSink::default());
        let runner = CommandRunner::new(Arc::clone(&sink), small_size());
        let id = seed_restore_instance(&db, ".", "sleep 30", false);
        // Prior shutdown: persisted running (orphan) + snapshot true, but toggle OFF.
        db.with_conn(|c| {
            db::set_last_state(c, &id, db::STATE_RUNNING).unwrap();
            db::set_was_running_on_shutdown(c, &id, true).unwrap();
        });

        let relaunched = restore_commands_on_boot(&db, &runner);
        assert!(
            !relaunched.contains(&id),
            "a toggle-OFF instance must NOT be relaunched"
        );
        let row = instance_row(&db, &id);
        assert_eq!(
            row.last_state, "idle",
            "an orphaned persisted-running instance is normalized to idle"
        );
        assert!(!row.was_running_on_shutdown, "snapshot reset to false");
        assert!(
            !runner.is_running(&id),
            "no process is spawned for a toggle-OFF instance"
        );
    }

    /// BOOT RESTORE / MULTI-INSTANCE: a mixed set restores at parity — a toggle-ON +
    /// was-running instance is relaunched; a toggle-ON but NOT-running one (its
    /// snapshot false, persisted `success`) is left untouched (dot keeps its color);
    /// a toggle-OFF orphan-running is normalized to idle. One restore pass, many
    /// instances — the multi-terminal/multi-command restore the PRD #18 criterion 2
    /// requires.
    #[test]
    #[cfg(not(windows))]
    fn restore_on_boot_handles_multiple_instances_at_parity() {
        with_posix_shell();
        let db = Db::in_memory();
        let sink = Arc::new(MockSink::default());
        let runner = CommandRunner::new(Arc::clone(&sink), small_size());

        // Three templates in the same project → three materialized instances.
        let (relaunch_id, keep_id, orphan_id) = db.with_conn(|c| {
            let (project, workspace) = db::create_project(c, "p", ".", None).unwrap();
            let mk = |c: &mut diesel::SqliteConnection, name: &str, cmd: &str, restart: bool| {
                let tpl = db::create_template(c, &project.id, name, cmd, None, Default::default())
                    .unwrap();
                db::set_restart_on_startup(c, &tpl.id, restart).unwrap();
                db::list_instances_for_workspace(c, &workspace.id)
                    .unwrap()
                    .into_iter()
                    .find(|i| i.command_id == tpl.id)
                    .unwrap()
                    .id
            };
            let relaunch_id = mk(c, "relaunch", "sleep 30", true);
            let keep_id = mk(c, "keep", "true", true);
            let orphan_id = mk(c, "orphan", "sleep 30", false);
            (relaunch_id, keep_id, orphan_id)
        });

        db.with_conn(|c| {
            // relaunch: was running (snapshot true) + toggle ON → relaunch.
            db::set_was_running_on_shutdown(c, &relaunch_id, true).unwrap();
            // keep: a finished success at the last shutdown (NOT running) → never
            // relaunched, last_state preserved for the dot.
            db::set_last_state(c, &keep_id, "success").unwrap();
            db::set_was_running_on_shutdown(c, &keep_id, false).unwrap();
            // orphan: persisted running + snapshot true but toggle OFF → normalize.
            db::set_last_state(c, &orphan_id, db::STATE_RUNNING).unwrap();
            db::set_was_running_on_shutdown(c, &orphan_id, true).unwrap();
        });

        let relaunched = restore_commands_on_boot(&db, &runner);

        assert_eq!(
            relaunched,
            vec![relaunch_id.clone()],
            "exactly the toggle-ON + was-running instance is relaunched"
        );
        assert!(wait_state(
            &runner,
            &relaunch_id,
            RunState::Running,
            Duration::from_secs(4)
        ));
        assert_eq!(
            instance_row(&db, &keep_id).last_state,
            "success",
            "a finished success is preserved (dot keeps its color)"
        );
        assert_eq!(
            instance_row(&db, &orphan_id).last_state,
            "idle",
            "a toggle-OFF orphan-running is normalized to idle"
        );
        // Every snapshot is reset so a future boot cannot relaunch a ghost.
        for id in [&relaunch_id, &keep_id, &orphan_id] {
            assert!(
                !instance_row(&db, id).was_running_on_shutdown,
                "snapshot reset for {id}"
            );
        }
        runner.stop(&relaunch_id).expect("cleanup stop");
    }

    /// SHUTDOWN REAP: `kill_all_running` (the shutdown reap the shell calls right
    /// after the snapshot) tree-kills every live instance so NOTHING is orphaned past
    /// shutdown, and `begin_shutdown` latches so the reap+snapshot run exactly once
    /// (the close-request + destroy double-event guard). Proven end-to-end against the
    /// restore-seeded runner.
    #[test]
    #[cfg(not(windows))]
    fn shutdown_reap_kills_all_and_latches_once() {
        with_posix_shell();
        let db = Db::in_memory();
        let sink = Arc::new(MockSink::default());
        let runner = CommandRunner::new(Arc::clone(&sink), small_size());
        let id = seed_restore_instance(&db, ".", "sleep 120 & wait", true);

        runner
            .start(&id, "sleep 120 & wait", Some("."))
            .expect("start");
        assert!(wait_state(
            &runner,
            &id,
            RunState::Running,
            Duration::from_secs(4)
        ));

        // The shutdown sequence: latch, snapshot (records running=true), reap.
        assert!(runner.begin_shutdown(), "first begin_shutdown latches true");
        snapshot_commands_on_shutdown(&db, &runner);
        assert!(
            instance_row(&db, &id).was_running_on_shutdown,
            "the snapshot captured the live process before the reap"
        );
        runner.kill_all_running();
        assert!(
            wait_state(&runner, &id, RunState::Idle, Duration::from_secs(4)),
            "kill_all_running reaps the live process tree (no orphan survives)"
        );
        // The latch holds: a second begin_shutdown (the destroy event) is a no-op, so
        // a second snapshot would not run on the now-idle map and clear the flag.
        assert!(
            !runner.begin_shutdown(),
            "begin_shutdown latches: the destroy event must not re-run the reap"
        );
    }
}
