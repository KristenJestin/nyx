//! PTY backend, fully decoupled from Tauri.
//!
//! A [`Pty`] owns a child process running inside a pseudo-terminal plus the
//! threads that read its output and wait for it to exit. It is the core of the
//! nyx terminal backend: the Tauri bridge (separate module) is only a thin
//! layer on top of this.
//!
//! Bytes read from the PTY master are pushed, unmodified and un-throttled, onto
//! an [`std::sync::mpsc`] channel; coalescing/throttling is the bridge's job.
//! Each [`Pty`] carries an opaque `id` so the bridge can manage several PTYs in
//! PRD 1 (multi-terminal) without changing this module.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

/// Lossless flow-control gate for one PTY's reader thread (PRD phase-3 / annexe §E).
///
/// The reader checks this gate *before* every `read()` of the PTY master. While
/// paused it BLOCKS on the condvar instead of reading; the kernel's PTY output
/// buffer then fills and the child (e.g. a `yes` flood) blocks on its own `write`
/// — real OS backpressure, so **nothing is ever dropped or reordered**. Resuming
/// wakes the reader and it continues from exactly where the stream stood.
///
/// The bridge (the napi pump) drives this from its own backlog: it pauses at the
/// high-water mark and resumes under the low-water mark (512 KiB / 128 KiB in the
/// napi layer). The gate itself carries no policy — only the paused bit + the
/// wakeup primitive — so the core stays agnostic of the bridge's thresholds.
#[derive(Clone, Default)]
pub struct PauseGate {
    inner: Arc<(Mutex<GateState>, Condvar)>,
}

/// The gate's two bits, under one mutex.
///
/// `parked` is not policy — it is the gate's own OBSERVABLE state: whether the reader
/// is currently blocked *at* the gate rather than inside a `read()`. The distinction is
/// the whole contract: the gate is checked BEFORE each read, so pausing can never
/// retract a `read()` already in flight — that pending read still delivers its chunk.
/// "Not one byte flows" only becomes true once the reader has come back round and
/// parked. Anything asserting that (see `pause_gate_is_lossless_and_ordered`) has to
/// wait for the park, not for a delay.
#[derive(Default)]
struct GateState {
    /// The bridge's pause bit.
    paused: bool,
    /// The reader is blocked at the gate right now (no read in flight).
    parked: bool,
}

impl PauseGate {
    /// Pause (`true`) or resume (`false`) the reader. Resuming wakes a reader that
    /// is blocked at the gate. Idempotent: setting the same state twice is a no-op
    /// beyond a redundant notify.
    pub fn set_paused(&self, paused: bool) {
        let (lock, cvar) = &*self.inner;
        let mut state = lock.lock().unwrap();
        state.paused = paused;
        if !paused {
            // Wake the reader so it loops back into `read()`.
            cvar.notify_all();
        }
    }

    /// Whether the gate is currently paused.
    pub fn is_paused(&self) -> bool {
        let (lock, _) = &*self.inner;
        lock.lock().unwrap().paused
    }

    /// Block the calling (reader) thread while paused. Returns as soon as the gate
    /// is not paused. Called by the reader thread just before each `read()`.
    fn wait_while_paused(&self) {
        let (lock, cvar) = &*self.inner;
        let mut state = lock.lock().unwrap();
        while state.paused {
            // Publish the park BEFORE blocking, and wake anyone waiting to observe it.
            state.parked = true;
            cvar.notify_all();
            state = cvar.wait(state).unwrap();
        }
        state.parked = false;
    }

    /// TEST-ONLY: block until the reader is parked AT the gate, or `timeout` elapses;
    /// returns whether it is parked.
    ///
    /// This is the observable condition a pause assertion needs. A reader that was
    /// already inside `read()` when the pause landed parks only after that one pending
    /// read completes and its chunk is sent — so once this returns `true`, everything
    /// that could escape the gate is already on the channel and nothing further can
    /// flow until [`Self::set_paused(false)`].
    #[cfg(test)]
    fn wait_until_parked(&self, timeout: std::time::Duration) -> bool {
        let (lock, cvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let (state, _) = cvar
            .wait_timeout_while(state, timeout, |s| !s.parked)
            .unwrap();
        state.parked
    }
}

/// How long a shell is given to honour the `SIGHUP` on its own before
/// [`Pty::kill_and_reap`] escalates to `SIGKILL`. Long enough for a shell to run its exit
/// path, short enough that a close never feels stuck — and it only ever ELAPSES for a shell
/// that ignored the hangup, which is precisely the case that used to block forever.
pub const SHELL_KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Env var carrying nyx's PERSISTENT terminal record id (`terminals.id`) into the
/// spawned shell and everything it launches (PRD-5 task #3). An agent integration
/// running INSIDE the shell (e.g. the Claude plugin's SessionStart hook) reads this
/// to correlate its session event back to the exact nyx terminal — unambiguously,
/// even when two sessions share a cwd. Injected on the production interactive-shell
/// spawn path ([`Pty::spawn`]) whenever the caller supplies the record id; it then
/// propagates to child processes via normal env inheritance on every OS. The value
/// is exactly the `terminals.id` the bridge passed (see `pty_spawn`).
pub const NYX_TERMINAL_ID_ENV: &str = "NYX_TERMINAL_ID";

/// Env var carrying the MCP HTTP port THIS nyx instance's sidecar actually bound, into the
/// spawned shell and everything it launches (review `01KXAXQN32GASMEB05PDW5BZSR`). The Claude
/// plugin's `SessionStart` hook (`nyx-session-hook.cjs`) POSTs `agent_session_event` to
/// `http://127.0.0.1:${NYX_MCP_PORT||8765}/mcp`; without this export the hook falls back to the
/// default 8765, which is the v1 cockpit whenever v1 is up (v2 then binds 8766) — so a v2
/// terminal's session events were silently delivered to v1, leaving v2's `agent_sessions` empty
/// and the agent icon dark. Injected on the production interactive-shell spawn path
/// ([`Pty::spawn`]) whenever the caller supplies the REAL bound port (`> 0`); it then propagates
/// to child processes via normal env inheritance on every OS. A port of `0` means "not bound yet"
/// (the MCP server has not started) — nothing is exported then, so the hook keeps its own fallback
/// rather than being pointed at a misleading value. NEVER a hard-coded 8765/8766: the injected
/// value is always the dynamically bound port, correct even under a v1/v2 port collision.
pub const NYX_MCP_PORT_ENV: &str = "NYX_MCP_PORT";

/// Monotonic source of PTY ids. Forward-compatible with PRD 1 multi-terminal:
/// the bridge keys its managed state by this id.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a fresh, process-unique PTY id.
fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Resolve the shell to spawn.
///
/// `$SHELL` is honored first on every platform when set and non-empty (the
/// user's chosen shell; also how the e2e suite forces a POSIX bash). Otherwise
/// the default is per-OS:
/// - **Unix:** `bash`, then `sh` (POSIX-guaranteed).
/// - **Windows:** `pwsh.exe`, then `powershell.exe` (the Windows Terminal
///   default), then `cmd.exe` via `%ComSpec%`. We deliberately do NOT fall back
///   to `bash`/`sh` there: neither is on `PATH` (a bare `bash` resolves to the
///   WSL launcher, which needs an installed distro), so doing so would spawn a
///   missing program and the terminal would come up blank.
///
/// TODO(idea): make the default shell user-selectable in settings (see AGENTS.md
/// §9 "Différé v1.1+"). The chosen name is returned so callers/tests can assert.
///
/// `pub` so the managed-command runtime ([`crate::command`]) spawns its
/// read-only command PTYs under the SAME shell as the terminals (honors `$SHELL`
/// identically), instead of duplicating the resolution.
pub fn resolve_shell() -> String {
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return s;
        }
    }

    #[cfg(windows)]
    {
        // Prefer PowerShell (pwsh 7+, then Windows PowerShell), else cmd.exe.
        if which_exists("pwsh.exe") {
            return "pwsh.exe".to_string();
        }
        if which_exists("powershell.exe") {
            return "powershell.exe".to_string();
        }
        // `%ComSpec%` is effectively always set on Windows; hard-code cmd.exe as
        // a last resort in case it somehow is not.
        return std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
    }

    #[cfg(not(windows))]
    {
        // `bash` is the pragmatic default; `sh` is the last-resort POSIX shell.
        if which_exists("bash") {
            "bash".to_string()
        } else {
            "sh".to_string()
        }
    }
}

/// Cheap PATH lookup for a bare program name (used only by the shell fallback).
fn which_exists(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file()
    })
}

/// A spawned PTY: child process, master side (for write/resize), reader thread,
/// and a background waiter that records the exit status.
///
/// Dropping a `Pty` (or calling [`Pty::kill`]) terminates the child and joins
/// the helper threads, so neither threads nor OS handles leak.
pub struct Pty {
    id: u64,
    /// Master side of the PTY, shared with the waiter thread. Held while the
    /// child is alive so `resize`/`foreground_pgid` keep working. The waiter
    /// `take()`s (drops) it the instant the child exits: on Windows/ConPTY the
    /// reader does NOT EOF on child exit, so closing the master (ClosePseudoConsole)
    /// is what unblocks the reader and disconnects the output channel that drives
    /// `pty://exit`.
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    /// Writer onto the PTY master (i.e. the child's stdin). Behind its OWN `Arc<Mutex>`
    /// (not the `PtyManager` registry lock) so a caller can clone the handle, release the
    /// registry lock, and only THEN do the potentially-blocking write — a stuck child
    /// then stalls writes to its own pty, never every pty (see `PtyManager::write_to`).
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Independent killer cloned from the child, usable while the waiter thread
    /// is blocked in `wait`.
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Handle of the thread reading the PTY output.
    reader_handle: Option<JoinHandle<()>>,
    /// Handle of the thread blocked on `child.wait()`.
    waiter_handle: Option<JoinHandle<()>>,
    /// Exit code of the child once it has terminated (`None` while running).
    exit_code: Arc<Mutex<Option<i32>>>,
    /// OS pid of the spawned shell. Used to read live cwd from
    /// `/proc/<pid>/cwd` (Linux); `None` if the platform/pty can't report it.
    shell_pid: Option<u32>,
    /// Lossless flow-control gate the reader thread checks before each `read()`.
    /// The bridge (napi pump) toggles it to apply OS-level backpressure under
    /// flood without dropping bytes (PRD phase-3 / annexe §E).
    pause_gate: PauseGate,
}

impl Pty {
    /// Spawn the default shell inside a fresh PTY of the given size.
    ///
    /// `cwd` sets the child's working directory when `Some`; otherwise the
    /// child inherits nyx's cwd. The environment is inherited from the current
    /// process (`CommandBuilder` copies the live env by default).
    ///
    /// `terminal_id` is nyx's PERSISTENT terminal record id (`terminals.id`). When
    /// supplied it is exported into the shell as [`NYX_TERMINAL_ID_ENV`] so an agent
    /// integration running inside (and any child the shell spawns) can correlate its
    /// session events back to THIS terminal (PRD-5 task #3). `None` (the socle / a
    /// record-less spawn) exports nothing.
    ///
    /// `mcp_port` is the MCP HTTP port THIS nyx instance's sidecar actually bound. When
    /// `> 0` it is exported as [`NYX_MCP_PORT_ENV`] so the Claude plugin's `SessionStart`
    /// hook POSTs `agent_session_event` to the RIGHT instance instead of the 8765 default
    /// (the v1 cockpit under a port collision) — review `01KXAXQN32GASMEB05PDW5BZSR`. `0`
    /// (the MCP server has not bound yet) exports nothing, so the hook keeps its own fallback.
    ///
    /// Returns the [`Pty`] handle and a [`Receiver`] yielding chunks of output
    /// bytes as the reader thread reads them. The receiver completes (the
    /// sender is dropped) when the PTY reaches EOF — i.e. the child has exited.
    pub fn spawn(
        size: PtySize,
        cwd: Option<&str>,
        terminal_id: Option<&str>,
        mcp_port: u16,
    ) -> anyhow::Result<(Self, Receiver<Vec<u8>>)> {
        // Resolve the shell, then build its OSC 133 shell-integration plan
        // (PRD-2.1 task #5): a NON-DESTRUCTIVE per-spawn snippet that makes the
        // shell emit `133;C`/`133;D` so the bridge can derive exec-state. An
        // Unsupported shell (sh/cmd/fish/…) or a disabled/failed injection yields an
        // EMPTY plan → the shell is spawned plain (idle-only degradation; no false
        // running/success/error). Injection applies ONLY here, on the production
        // interactive-shell path — never to `spawn_program` (tests / the managed
        // command runner, which spawn arbitrary non-interactive programs).
        let shell = resolve_shell();
        let plan = crate::shellinteg::build(&shell);
        Self::spawn_program_with_integration(&shell, size, cwd, &plan, terminal_id, mcp_port)
    }

    /// Spawn an arbitrary program with NO shell integration (used by tests and the
    /// managed-command runner; production interactive terminals use [`Pty::spawn`]
    /// which always launches the default shell WITH integration). `allow(dead_code)`
    /// in a non-test build: every in-crate caller is `#[cfg(test)]` now that
    /// `spawn` injects via `spawn_program_with_integration` instead.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn spawn_program(
        program: &str,
        size: PtySize,
        cwd: Option<&str>,
    ) -> anyhow::Result<(Self, Receiver<Vec<u8>>)> {
        // No terminal-record id nor MCP port on the program path: tests / the managed-command
        // runner spawn arbitrary non-interactive programs that carry no agent session.
        Self::spawn_program_with_integration(
            program,
            size,
            cwd,
            &crate::shellinteg::IntegrationPlan::default(),
            None,
            0,
        )
    }

    /// TEST-ONLY: spawn `program` with the given `args`, exporting `terminal_id` as
    /// [`NYX_TERMINAL_ID_ENV`] and (when `mcp_port > 0`) the MCP port as [`NYX_MCP_PORT_ENV`],
    /// so both env-injection paths (PRD-5 task #3 + review `01KXAXQN32GASMEB05PDW5BZSR`) can be
    /// exercised on a controllable program per-OS (a POSIX `sh -c` on Unix, `cmd /C`
    /// on Windows) WITHOUT depending on which interactive shell `resolve_shell`
    /// picks. Production code uses [`Pty::spawn`], which carries the id + bound port from the
    /// bridge; this reuses the SAME single env-injection line via an
    /// [`crate::shellinteg::IntegrationPlan`] that only carries the args.
    #[cfg(test)]
    pub fn spawn_program_with_terminal_id(
        program: &str,
        args: &[&str],
        size: PtySize,
        cwd: Option<&str>,
        terminal_id: Option<&str>,
        mcp_port: u16,
    ) -> anyhow::Result<(Self, Receiver<Vec<u8>>)> {
        let plan = crate::shellinteg::IntegrationPlan {
            args: args.iter().map(std::ffi::OsString::from).collect(),
            ..Default::default()
        };
        Self::spawn_program_with_integration(program, size, cwd, &plan, terminal_id, mcp_port)
    }

    /// Spawn `program`, applying a shell-integration [`IntegrationPlan`]'s extra
    /// args + env (empty plan = plain spawn). The single real spawn path the two
    /// public constructors share.
    ///
    /// `terminal_id`, when `Some`, is exported as [`NYX_TERMINAL_ID_ENV`] so the
    /// child shell (and everything it launches) carries nyx's persistent terminal
    /// record id for agent-session correlation (PRD-5 task #3). `mcp_port`, when `> 0`,
    /// is exported as [`NYX_MCP_PORT_ENV`] so the Claude plugin's hook reaches THIS
    /// instance's MCP server (review `01KXAXQN32GASMEB05PDW5BZSR`).
    fn spawn_program_with_integration(
        program: &str,
        size: PtySize,
        cwd: Option<&str>,
        plan: &crate::shellinteg::IntegrationPlan,
        terminal_id: Option<&str>,
        mcp_port: u16,
    ) -> anyhow::Result<(Self, Receiver<Vec<u8>>)> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size)?;

        let mut cmd = CommandBuilder::new(program);
        // Shell-integration args (e.g. bash `--rcfile <tmp> -i`, pwsh `-NoExit
        // -Command ". '<tmp>'"`) come right after the program name.
        for arg in &plan.args {
            cmd.arg(arg);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        // Make $TERM sane for full-screen TUIs (vim/htop) in the absence of one.
        if std::env::var_os("TERM").is_none() {
            cmd.env("TERM", "xterm-256color");
        }
        // Shell-integration env (e.g. zsh `ZDOTDIR`/`NYX_REAL_ZDOTDIR`).
        for (k, v) in &plan.env {
            cmd.env(k, v);
        }
        // Agent-session correlation (PRD-5 task #3): export nyx's persistent terminal
        // record id so an agent integration inside the shell — and any child it
        // spawns — can attribute its session events to THIS terminal unambiguously.
        // `CommandBuilder::env` is OS-portable (it populates the child's environment
        // block on Windows and the env array on Unix), so the var reaches bash/zsh on
        // Unix and pwsh/powershell/cmd on Windows identically. Set last so nothing in
        // the integration plan can shadow it.
        if let Some(tid) = terminal_id {
            cmd.env(NYX_TERMINAL_ID_ENV, tid);
        }
        // Agent-session ROUTING (review `01KXAXQN32GASMEB05PDW5BZSR`): export the MCP port THIS
        // instance's sidecar actually bound so the Claude plugin's SessionStart hook POSTs
        // `agent_session_event` to the RIGHT server. Without it the hook defaults to 8765 — the v1
        // cockpit whenever v1 holds it and v2 fell back to 8766 — so v2's terminal session events
        // landed in v1 and v2's `agent_sessions` stayed empty (no agent icon). Always the DYNAMIC
        // bound port, never a hard-coded 8765/8766, so it is correct even under a v1/v2 collision.
        //
        // FIRST scrub any `NYX_MCP_PORT` this process INHERITED (`CommandBuilder::new` seeds every
        // child from the sidecar's own `env::vars_os()`): the injected bound port below is the SINGLE
        // source of truth, so a STALE inherited value must never leak to the child. This is a no-op in
        // the nominal case (the sidecar's env carries none); when it does carry one (a user
        // `NYX_MCP_PORT` override) the scrub+inject net-nets to the same authoritative value the bind
        // produced (the inject already overrides). It also makes the port-0 contract hold
        // UNCONDITIONALLY — no injection AND no inherited leak, so the hook keeps its own fallback —
        // and it makes the `spawned_terminal_*` integration test a true guard: the child's ONLY source
        // of the port is the injection line below, so deleting that line turns the assert red.
        cmd.env_remove(NYX_MCP_PORT_ENV);
        // A port of `0` means the MCP server has not bound yet; we inject nothing then (never a
        // misleading default). Same OS-portable `CommandBuilder::env` as the terminal id, and set
        // alongside it so both reach every shell.
        if mcp_port != 0 {
            cmd.env(NYX_MCP_PORT_ENV, mcp_port.to_string());
        }

        let child = pair.slave.spawn_command(cmd)?;
        // The slave fd is no longer needed in this process once the child holds
        // it; dropping it ensures we see EOF on the master when the child exits.
        drop(pair.slave);

        // Capture the shell pid before moving `child` into the waiter thread.
        // This is the anchor for live cwd lookups via `/proc/<pid>/cwd`.
        let shell_pid = child.process_id();
        let killer = child.clone_killer();
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer()?));
        let mut reader = pair.master.try_clone_reader()?;
        // Share the master with the waiter thread so it can close it on child
        // exit (the lever that unblocks the reader on Windows; see the field doc).
        let master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> =
            Arc::new(Mutex::new(Some(pair.master)));

        // Reader thread: pump raw bytes onto the channel until EOF. Before each
        // read it waits at the pause gate, so a paused bridge applies real OS
        // backpressure (the kernel PTY buffer fills, the child blocks on write) —
        // lossless flow control, nothing dropped (PRD phase-3 / annexe §E).
        let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = mpsc::channel();
        let pause_gate = PauseGate::default();
        let reader_handle = {
            let pause_gate = pause_gate.clone();
            std::thread::Builder::new()
                .name("nyx-pty-reader".into())
                .spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        // Lossless backpressure: block here while the bridge has us
                        // paused, BEFORE touching the master. Nothing is read (so
                        // nothing can be dropped) until the bridge resumes us.
                        pause_gate.wait_while_paused();
                        match reader.read(&mut buf) {
                            Ok(0) => break, // EOF: child closed the PTY
                            Ok(n) => {
                                // If the consumer hung up, stop reading.
                                if tx.send(buf[..n].to_vec()).is_err() {
                                    break;
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(_) => break, // master closed / error
                        }
                    }
                    // `tx` drops here → receiver observes disconnect (EOF signal).
                })?
        };

        // Waiter thread: block until the child exits, record its exit code, then
        // close the master so the reader unblocks.
        let exit_code = Arc::new(Mutex::new(None::<i32>));
        let waiter_handle = {
            let exit_code = Arc::clone(&exit_code);
            let master = Arc::clone(&master);
            let mut child: Box<dyn Child + Send + Sync> = child;
            std::thread::Builder::new()
                .name("nyx-pty-waiter".into())
                .spawn(move || {
                    if let Ok(status) = child.wait() {
                        *exit_code.lock().unwrap() = Some(status.exit_code() as i32);
                    }
                    // The child is gone; close the master. On Windows the ConPTY
                    // read side only EOFs once the pseudoconsole is closed (the
                    // child exiting is not enough), so without this the reader
                    // blocks forever and `pty://exit` never fires. Dropping the
                    // master closes it; on Unix the reader has already EOF'd via
                    // the slave drop, so this is a harmless no-op.
                    let _ = master.lock().unwrap().take();
                })?
        };

        let pty = Pty {
            id: next_id(),
            master,
            writer,
            killer,
            reader_handle: Some(reader_handle),
            waiter_handle: Some(waiter_handle),
            exit_code,
            shell_pid,
            pause_gate,
        };
        Ok((pty, rx))
    }

    /// Opaque, process-unique id of this PTY (used by the bridge to key state).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// A clone-able handle to this PTY's lossless flow-control gate. The bridge
    /// (the napi pump) pauses/resumes the reader through it to apply OS-level
    /// backpressure under flood without dropping bytes (PRD phase-3 / annexe §E).
    pub fn pause_gate(&self) -> PauseGate {
        self.pause_gate.clone()
    }

    /// OS pid of the spawned shell, if the platform reported it. Anchor for the
    /// live cwd lookup (`/proc/<pid>/cwd`).
    pub fn shell_pid(&self) -> Option<u32> {
        self.shell_pid
    }

    /// The foreground process group leader of this PTY — i.e. `tcgetpgrp` on the
    /// master fd, surfaced by `portable-pty` as `process_group_leader`. When a
    /// full-screen program (htop, vim) runs in the foreground this is that
    /// program's pgid; at the shell prompt it is the shell's own pgid. Anchor
    /// for reading the foreground program name from `/proc/<pgid>/comm`.
    #[cfg(unix)]
    pub fn foreground_pgid(&self) -> Option<i32> {
        self.master.lock().unwrap().as_ref()?.process_group_leader()
    }

    /// Whether this terminal is BUSY — i.e. a command is running in the
    /// foreground, as opposed to sitting idle at the shell prompt (PRD
    /// "busy/idle from the PTY foreground process group", task #1).
    ///
    /// This is the OS-authoritative `busy` signal that REPLACES OSC-133-derived
    /// `running` as the source of truth for the UI dot: it is the live kernel
    /// state, impossible to leave "stuck" and independent of any shell
    /// integration. The derivation is exactly `foreground_pgid(PTY) != pgid of
    /// the shell`:
    ///  - at an idle prompt the PTY's foreground process group IS the shell's own
    ///    group (the shell is a session/group leader, so its pgid == its pid), so
    ///    `foreground_pgid == shell_pid` → `false` (idle);
    ///  - while a command runs in the foreground the shell put that command in its
    ///    own process group and handed it the terminal, so `foreground_pgid !=
    ///    shell_pid` → `true` (busy).
    ///
    /// Returns `None` when the signal cannot be derived — the shell pid is unknown
    /// or `tcgetpgrp` failed (e.g. the master was already closed on exit). The
    /// caller treats `None` as "not busy" for the dot (a terminal with no live
    /// foreground group is not running anything), but keeping it an `Option`
    /// lets the bridge distinguish "unknown" from a hard `false`.
    ///
    /// `pull-only`: there is no kernel notification for a `tcgetpgrp` change, so
    /// the bridge polls this on a bounded cadence (see the backend busy-state
    /// loop) rather than reacting to an event.
    #[cfg(unix)]
    pub fn is_busy(&self) -> Option<bool> {
        let shell_pid = self.shell_pid? as i32;
        let fg = self.foreground_pgid()?;
        Some(fg != shell_pid)
    }

    /// Non-Unix builds have no PTY foreground process group (ConPTY exposes no
    /// `tcgetpgrp`), so the OS busy signal is unavailable. Windows busy/idle is
    /// explicitly out of scope (a separate parked design); the dot simply never
    /// flips from this source there.
    #[cfg(not(unix))]
    pub fn is_busy(&self) -> Option<bool> {
        None
    }

    /// Write bytes to the PTY (the child's stdin). Flushes immediately so
    /// keystrokes are delivered without buffering latency. Takes `&self` (the writer is
    /// `Arc<Mutex>`-guarded), so it never requires the registry lock.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes)?;
        w.flush()
    }

    /// A clone-able handle to this PTY's writer, so a caller can drop the `PtyManager`
    /// registry lock BEFORE the (potentially blocking) write — see
    /// [`PtyManager::write_to`]. Writes through the handle serialize only on THIS pty.
    pub fn writer_handle(&self) -> Arc<Mutex<Box<dyn Write + Send>>> {
        Arc::clone(&self.writer)
    }

    /// Resize the PTY window. Informs the kernel (which delivers SIGWINCH to the
    /// child); pixel dimensions are best-effort and ignored on most systems.
    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> anyhow::Result<()> {
        // `None` once the child has exited and the waiter closed the master; a
        // resize then is simply a no-op.
        if let Some(master) = self.master.lock().unwrap().as_ref() {
            master.resize(PtySize {
                rows,
                cols,
                pixel_width,
                pixel_height,
            })?;
        }
        Ok(())
    }

    /// Terminate the child process. Idempotent: killing an already-dead child is
    /// a no-op error that we swallow. After this the waiter thread unblocks and
    /// records the exit code.
    pub fn kill(&mut self) -> std::io::Result<()> {
        // `kill` returns an error if the process is already gone; that's fine.
        let _ = self.killer.kill();
        Ok(())
    }

    /// Current exit code, or `None` if the child is still running.
    pub fn exit_code(&self) -> Option<i32> {
        *self.exit_code.lock().unwrap()
    }

    /// Terminate the child AND make the kill STICK — the teardown any caller that then JOINS a
    /// thread depending on the child's death must use, instead of the bare [`kill`](Self::kill).
    ///
    /// `kill` alone is a single `SIGHUP` an interactive shell may simply not honour (see
    /// [`ensure_child_dead`](Self::ensure_child_dead)); the shell then lives on, the master never
    /// EOFs, the reader never disconnects, and the join blocks FOREVER. `Drop` has always done the
    /// full sequence, but a caller holding the `Pty` alive (an `Arc` shared with a pump thread it
    /// is about to join) never reaches `Drop` — that is exactly the deadlock cycle. So the whole
    /// sequence lives HERE and `Drop` reuses it: kill, resume the pause-gate (a reader parked on
    /// the condvar would never reach `read()` to see EOF), then escalate to `SIGKILL` after
    /// `grace`.
    pub fn kill_and_reap(&mut self, grace: std::time::Duration) {
        let _ = self.killer.kill();
        self.pause_gate.set_paused(false);
        #[cfg(unix)]
        self.ensure_child_dead(grace);
        #[cfg(not(unix))]
        let _ = grace;
    }

    /// Make sure the child is really gone, escalating past a signal it can survive.
    ///
    /// `portable_pty`'s Unix killer sends exactly ONE `SIGHUP`. An interactive shell
    /// CATCHES that signal, so honouring it is its own decision, taken at a point it
    /// controls — and a child that never gets there (a reader that has already left on a
    /// hung-up consumer stops draining the PTY, and the shell's exit path then blocks on
    /// a buffer nobody empties) simply stays alive. The waiter's `waitpid` then never
    /// returns, and every `join()` below it blocks FOREVER: on Unix that froze whole test
    /// runs (`resize_…`, `busy_…`, `no_thread_or_handle_leak_…` each hung ~17% of the
    /// time when the pty module was looped), and in the app it would freeze whichever
    /// thread closed the terminal — the same class of freeze the Windows note below
    /// describes.
    ///
    /// So: wait a short grace for the child to honour the `SIGHUP` on its own — polling
    /// the exit code the waiter records, i.e. the real condition — then send `SIGKILL`,
    /// which no process can catch, block or defer.
    #[cfg(unix)]
    fn ensure_child_dead(&self, grace: std::time::Duration) {
        let Some(pid) = self.shell_pid else { return };
        let deadline = std::time::Instant::now() + grace;
        loop {
            if self.exit_code().is_some() {
                return;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // SAFETY: `kill(2)` with a pid we own; a dead/reaped pid just yields ESRCH.
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }

    /// Block until the child has exited and its exit code is recorded, then
    /// return it. Joins the waiter thread.
    pub fn wait(&mut self) -> Option<i32> {
        if let Some(handle) = self.waiter_handle.take() {
            let _ = handle.join();
        }
        self.exit_code()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Ensure the child is dead so both helper threads can terminate: the waiter unblocks
        // from `wait`, and (on Unix) the reader sees EOF once the master/child fds close. The
        // grace is what a shell needs to honour the SIGHUP itself; nothing waits on it once the
        // child is gone.
        self.kill_and_reap(SHELL_KILL_GRACE);
        if let Some(handle) = self.waiter_handle.take() {
            // Fast + reliable: `child.wait()` returns right after the kill, then
            // the waiter closes the master.
            let _ = handle.join();
        }
        // The reader thread is platform-split ON PURPOSE:
        // - On Unix, closing the master EOFs the reader's `read()`, so the join is
        //   prompt; we join to avoid leaking the thread.
        // - On Windows, the reader was cloned from the ConPTY master
        //   (`try_clone_reader`) and blocks in `ReadFile` on its OWN handle.
        //   `ClosePseudoConsole` on the master does NOT reliably unblock that
        //   clone, so `join()` can block FOREVER — and because `pty_close` runs on
        //   the MAIN thread, that froze the whole UI ("Not Responding") on close.
        //   (Diagnosed with per-stage logging: every freeze was stuck precisely on
        //   the reader `join()`.) So on Windows we DETACH the reader (drop the
        //   handle without joining): it terminates on its own once the OS tears
        //   the pipe down, and the teardown never blocks the caller.
        #[cfg(not(windows))]
        if let Some(handle) = self.reader_handle.take() {
            let _ = handle.join();
        }
        #[cfg(windows)]
        drop(self.reader_handle.take());
    }
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

    /// Drain the receiver into a single String until either we observe `needle`
    /// or `timeout` elapses. Returns the accumulated output.
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

    #[test]
    fn spawn_write_read_roundtrip() {
        let (mut pty, rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
        // Send a command; expect its output back through the PTY.
        pty.write(b"echo nyx_marker_123\n").expect("write");
        let out = read_until(&rx, "nyx_marker_123", Duration::from_secs(5));
        assert!(
            out.contains("nyx_marker_123"),
            "expected command output in PTY stream, got: {out:?}"
        );
        let _ = pty.kill();
    }

    #[test]
    fn resize_does_not_panic_and_is_reflected() {
        let (mut pty, rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
        // Apply a distinctive size, then ask the shell to report it.
        pty.resize(132, 50, 0, 0).expect("resize");
        // Give the kernel a moment to apply the winsize before querying.
        std::thread::sleep(Duration::from_millis(100));
        pty.write(b"stty size\n").expect("write");
        let out = read_until(&rx, "50 132", Duration::from_secs(5));
        assert!(
            out.contains("50 132"),
            "expected `stty size` to report rows cols `50 132`, got: {out:?}"
        );
        let _ = pty.kill();
    }

    #[test]
    fn kill_terminates_and_exit_code_recoverable() {
        // A shell that just sleeps for a long time; we kill it.
        let (mut pty, _rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
        pty.write(b"sleep 60\n").expect("write");
        std::thread::sleep(Duration::from_millis(150));
        assert!(pty.exit_code().is_none(), "child should still be running");
        pty.kill().expect("kill");
        // After kill, wait() must return a recoverable exit code.
        let code = pty.wait();
        assert!(
            code.is_some(),
            "exit code must be recoverable after kill, got None"
        );
    }

    #[test]
    fn no_thread_or_handle_leak_after_drop() {
        // Spawn, then drop; Drop must kill the child and return PROMPTLY. On Unix
        // it joins both helper threads (which terminate once the master closes);
        // on Windows it joins the waiter and DETACHES the reader (see `Pty::drop`).
        // Either way Drop must not block — if it did, this test would hang.
        // We also assert the id allocator advances exactly once per spawn (no
        // double-spawn / no stray PTY) without assuming an absolute id, since
        // tests run in parallel against the shared global counter.
        let (held, _rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn baseline sh");
        let baseline = held.id();
        let dropped_id = {
            let (pty, _rx) =
                Pty::spawn_program("sh", small_size(), None).expect("spawn sh to drop");
            pty.id()
            // Drop here: kill + join(waiter) + join/detach(reader). Must return
            // promptly (never blocks the caller).
        };
        assert!(
            dropped_id > baseline,
            "each spawn must allocate a distinct, increasing id ({dropped_id} > {baseline})"
        );
        drop(held);
    }

    #[test]
    fn reader_channel_closes_when_child_exits() {
        // `exit 0` makes the shell terminate on its own; the reader thread must
        // observe EOF and drop the sender, disconnecting the receiver.
        let (mut pty, rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
        pty.write(b"exit 0\n").expect("write");
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
            "reader channel must disconnect after child exits"
        );
        let code = pty.wait();
        assert_eq!(code, Some(0), "clean exit code should be 0");
    }

    #[test]
    fn drop_is_prompt_when_reader_is_blocked() {
        // REGRESSION (Windows "Not Responding" on close). `Pty::drop` used to
        // `join()` the reader thread unconditionally. On Windows that reader is
        // cloned from the ConPTY master and blocks in `ReadFile` on its OWN handle;
        // `ClosePseudoConsole` on the master does not reliably unblock it, so the
        // join could block FOREVER — and `pty_close` runs on the MAIN thread, so
        // the whole UI froze. An idle shell parks the reader in a blocking read:
        // the exact condition. We run the drop on a worker thread and FAIL (not
        // hang) if it does not return promptly.
        let (pty, rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
        std::thread::sleep(Duration::from_millis(100)); // let the reader settle in read()

        let (done_tx, done_rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            drop(pty); // kill + join(waiter) + join/detach(reader): must NOT block
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "Pty::drop did not return within 10s — the close-hang regressed (reader join deadlock)"
        );
        worker.join().expect("worker thread");
        drop(rx);
    }

    #[test]
    fn drop_is_prompt_with_active_output() {
        // Same invariant, but with the reader ACTIVELY cycling through reads (the
        // shell emits a line every ~20ms): teardown must still return promptly
        // with output in flight, not only when the reader sits idle.
        let (pty, rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
        pty.write(b"i=0; while :; do echo line $i; i=$((i+1)); sleep 0.02; done\n")
            .expect("write");
        assert!(
            read_until(&rx, "line ", Duration::from_secs(5)).contains("line "),
            "shell should be emitting output before we close it"
        );

        let (done_tx, done_rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            drop(pty);
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "Pty::drop deadlocked with output in flight (>10s)"
        );
        worker.join().expect("worker thread");
        drop(rx);
    }

    // --- NYX_TERMINAL_ID injection (PRD-5 task #3) -----------------------

    /// Unix: a shell spawned WITH a terminal record id sees `NYX_TERMINAL_ID` set to
    /// EXACTLY that id. We run `sh -c 'echo $NYX_TERMINAL_ID'` so the value the child
    /// prints is the env the parent injected — proving the var reaches the shell and
    /// matches the record id the bridge passed.
    #[cfg(not(windows))]
    #[test]
    fn nyx_terminal_id_is_exported_to_unix_shell() {
        let tid = "term-abc-123";
        let (mut pty, rx) = Pty::spawn_program_with_terminal_id(
            "sh",
            &["-c", "echo NYXTID=[$NYX_TERMINAL_ID]"],
            small_size(),
            None,
            Some(tid),
            0, // no MCP port for the NYX_TERMINAL_ID-focused assertion
        )
        .expect("spawn sh -c");
        let out = read_until(&rx, "NYXTID=[", Duration::from_secs(5));
        assert!(
            out.contains(&format!("NYXTID=[{tid}]")),
            "child shell must see NYX_TERMINAL_ID == the record id; got: {out:?}"
        );
        let _ = pty.kill();
    }

    /// Windows: same invariant via `cmd /C echo`. `%NYX_TERMINAL_ID%` expands to the
    /// injected value in the child's environment block.
    #[cfg(windows)]
    #[test]
    fn nyx_terminal_id_is_exported_to_windows_shell() {
        let tid = "term-win-456";
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
        let (mut pty, rx) = Pty::spawn_program_with_terminal_id(
            &comspec,
            &["/C", "echo NYXTID=[%NYX_TERMINAL_ID%]"],
            small_size(),
            None,
            Some(tid),
            0, // no MCP port for the NYX_TERMINAL_ID-focused assertion
        )
        .expect("spawn cmd /C");
        let out = read_until(&rx, "NYXTID=[", Duration::from_secs(10));
        assert!(
            out.contains(&format!("NYXTID=[{tid}]")),
            "child cmd must see %NYX_TERMINAL_ID% == the record id; got: {out:?}"
        );
        let _ = pty.kill();
    }

    /// A record-less spawn (`terminal_id = None`, the socle path) injects NOTHING:
    /// the var is UNSET in the child, so it expands to empty. This proves the
    /// injection is conditional on the bridge supplying an id, never a stray export.
    #[cfg(not(windows))]
    #[test]
    fn no_nyx_terminal_id_when_record_less_unix() {
        // Ensure the parent process itself doesn't carry the var (it must come ONLY
        // from injection), so a None spawn genuinely shows it empty.
        std::env::remove_var(NYX_TERMINAL_ID_ENV);
        let (mut pty, rx) = Pty::spawn_program_with_terminal_id(
            "sh",
            &["-c", "echo NYXTID=[${NYX_TERMINAL_ID:-UNSET}]"],
            small_size(),
            None,
            None,
            0,
        )
        .expect("spawn sh -c");
        let out = read_until(&rx, "NYXTID=[", Duration::from_secs(5));
        assert!(
            out.contains("NYXTID=[UNSET]"),
            "a record-less spawn must NOT export NYX_TERMINAL_ID; got: {out:?}"
        );
        let _ = pty.kill();
    }

    /// The injection reaches a CHILD of the shell, not just the shell itself — the
    /// real correlation path (an agent CLI launched inside the terminal reads the
    /// var). We start a subshell that runs another `sh -c` reading the var; it must
    /// still see the inherited value.
    #[cfg(not(windows))]
    #[test]
    fn nyx_terminal_id_propagates_to_grandchild_unix() {
        let tid = "term-inherit-789";
        let (mut pty, rx) = Pty::spawn_program_with_terminal_id(
            "sh",
            &["-c", "sh -c 'echo NYXTID=[$NYX_TERMINAL_ID]'"],
            small_size(),
            None,
            Some(tid),
            0, // no MCP port for the NYX_TERMINAL_ID inheritance assertion
        )
        .expect("spawn nested sh");
        let out = read_until(&rx, "NYXTID=[", Duration::from_secs(5));
        assert!(
            out.contains(&format!("NYXTID=[{tid}]")),
            "the var must inherit into a child process of the shell; got: {out:?}"
        );
        let _ = pty.kill();
    }

    // --- NYX_MCP_PORT injection (review 01KXAXQN32GASMEB05PDW5BZSR) -----------

    /// done_criterion 1: a shell spawned with a KNOWN bound MCP port sees `NYX_MCP_PORT` set to
    /// EXACTLY that port. Modelled on `nyx_terminal_id_is_exported_to_unix_shell`: we run
    /// `sh -c 'echo $NYX_MCP_PORT'` so the value the child prints is the env the parent injected
    /// — proving the port reaches the shell (and thus the Claude plugin's SessionStart hook, so it
    /// POSTs `agent_session_event` to THIS instance rather than the 8765 default). The port here is
    /// a non-default value (49234), so the assertion also proves it is the DYNAMIC bound port, not
    /// a hard-coded 8765/8766 (done_criterion 2).
    #[cfg(not(windows))]
    #[test]
    fn nyx_mcp_port_is_exported_to_unix_shell() {
        let port: u16 = 49234;
        let (mut pty, rx) = Pty::spawn_program_with_terminal_id(
            "sh",
            &["-c", "echo NYXPORT=[$NYX_MCP_PORT]"],
            small_size(),
            None,
            Some("term-mcp-1"),
            port,
        )
        .expect("spawn sh -c");
        let out = read_until(&rx, "NYXPORT=[", Duration::from_secs(5));
        assert!(
            out.contains(&format!("NYXPORT=[{port}]")),
            "child shell must see NYX_MCP_PORT == the bound port; got: {out:?}"
        );
        let _ = pty.kill();
    }

    /// A spawn whose MCP port is UNKNOWN (`0` — the MCP server has not bound yet) injects NOTHING:
    /// `NYX_MCP_PORT` is UNSET in the child, so the Claude plugin's hook keeps its own fallback
    /// rather than being pointed at a misleading value. This is the "port pas encore bindé au
    /// spawn" case the fix must handle without corrupting routing. We remove the var from the
    /// PARENT first so a `0` spawn genuinely shows it unset (the value can come ONLY from injection).
    #[cfg(not(windows))]
    #[test]
    fn no_nyx_mcp_port_when_port_is_zero_unix() {
        std::env::remove_var(NYX_MCP_PORT_ENV);
        let (mut pty, rx) = Pty::spawn_program_with_terminal_id(
            "sh",
            &["-c", "echo NYXPORT=[${NYX_MCP_PORT:-UNSET}]"],
            small_size(),
            None,
            Some("term-mcp-0"),
            0,
        )
        .expect("spawn sh -c");
        let out = read_until(&rx, "NYXPORT=[", Duration::from_secs(5));
        assert!(
            out.contains("NYXPORT=[UNSET]"),
            "a port-0 (not-yet-bound) spawn must NOT export NYX_MCP_PORT; got: {out:?}"
        );
        let _ = pty.kill();
    }

    // --- busy/idle from the PTY foreground process group (PRD task #1) --------

    /// Poll `is_busy()` until it equals `want` or we time out, returning the last
    /// observed value. Used to assert the busy/idle TRANSITIONS without racing the
    /// shell's job-control hand-off (which lands a few ms after the keystroke).
    #[cfg(unix)]
    fn busy_settles_to(pty: &Pty, want: bool, timeout: Duration) -> Option<bool> {
        let deadline = Instant::now() + timeout;
        let mut last = pty.is_busy();
        while Instant::now() < deadline {
            last = pty.is_busy();
            if last == Some(want) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        last
    }

    /// CORE derivation (task #1 done-criterion): an interactive shell sitting at
    /// its prompt is IDLE (`foreground_pgid == shell pgid`); a command running in
    /// the foreground makes it BUSY (`foreground_pgid != shell pgid`); when the
    /// command finishes it returns to IDLE. Proven against the LIVE kernel state
    /// (`tcgetpgrp`), with NO dependency on OSC 133 / shell integration.
    ///
    /// We spawn an INTERACTIVE shell so job control is on (the shell hands the
    /// terminal to the foreground command's process group — exactly what flips
    /// `tcgetpgrp`). A non-interactive `sh -c` would never reparent the tty, so it
    /// could not exercise the transition.
    #[cfg(unix)]
    #[test]
    fn busy_tracks_foreground_process_group() {
        // Force an interactive bash with job control; skip cleanly if bash is
        // absent (the derivation itself is shell-agnostic — it only needs a shell
        // that uses job control, which every interactive shell does).
        if !which_exists("bash") {
            eprintln!("skipping busy_tracks_foreground_process_group: bash not found");
            return;
        }
        let (mut pty, _rx) = Pty::spawn_program_with_terminal_id(
            "bash",
            &["--norc", "--noprofile", "-i"],
            small_size(),
            None,
            None,
            0,
        )
        .expect("spawn interactive bash");

        // At the prompt: idle. The foreground process group is the shell's own.
        assert_eq!(
            busy_settles_to(&pty, false, Duration::from_secs(5)),
            Some(false),
            "an interactive shell at its prompt must be IDLE (fg pgid == shell pgid)"
        );

        // Run a long foreground command → busy (fg pgid != shell pgid).
        pty.write(b"sleep 30\n").expect("write sleep");
        assert_eq!(
            busy_settles_to(&pty, true, Duration::from_secs(5)),
            Some(true),
            "a foreground command must make the terminal BUSY (fg pgid != shell pgid)"
        );

        // End the command (Ctrl-C) → back to idle.
        pty.write(&[0x03]).expect("write ctrl-c");
        assert_eq!(
            busy_settles_to(&pty, false, Duration::from_secs(5)),
            Some(false),
            "finishing the foreground command must return the terminal to IDLE"
        );

        let _ = pty.kill();
    }

    /// `is_busy()` derivation is purely `foreground_pgid != shell_pid` and does NOT
    /// consult OSC 133 / any shell integration: an idle prompt is idle even though
    /// the shell never emitted a `133;D` end. (The transition coverage is in
    /// `busy_tracks_foreground_process_group`; this guards the idle baseline +
    /// the None-when-unknown contract.)
    #[cfg(unix)]
    #[test]
    fn busy_is_idle_at_prompt_without_osc133() {
        let (mut pty, _rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
        // No command issued, no OSC 133 in play: the only foreground group is the
        // shell's, so busy is false.
        assert_eq!(
            busy_settles_to(&pty, false, Duration::from_secs(5)),
            Some(false),
            "a bare shell at its prompt is idle from the OS signal alone (no OSC 133)"
        );
        let _ = pty.kill();
    }

    /// LOSSLESS FLOW CONTROL (PRD phase-3 / annexe §E). Pausing the reader gate
    /// stops bytes flowing to the channel WITHOUT dropping anything: while paused
    /// the OS PTY buffer holds the child's output; on resume it all arrives, in
    /// order. We drive a finite, deterministic stream (a fixed count of numbered
    /// lines) and assert every line is present and ordered after a pause/resume
    /// cycle — proving the backpressure is lossless, not lossy throttling.
    #[test]
    fn pause_gate_is_lossless_and_ordered() {
        let (mut pty, rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
        let gate = pty.pause_gate();

        // Pause BEFORE issuing work, so the stream is gated from the start.
        gate.set_paused(true);
        assert!(gate.is_paused());

        // Emit a finite, numbered, ordered stream. `seq` is POSIX; the markers let
        // us assert exact count + order. End with a sentinel so we know we're done.
        // The sentinel is assembled from `$d` so the literal `NYX_DONE` cannot appear
        // in the tty's ECHO of this very line — only in the shell's real output.
        pty.write(b"d=DONE; for i in $(seq 1 200); do echo NYXLINE_$i; done; echo NYX_$d\n")
            .expect("write");

        // WAIT FOR THE READER TO REACH THE GATE — the observable condition, never a delay.
        // The gate is checked before each `read()`, so a pause cannot retract a read
        // already in flight: the reader thread starts with the `Pty` and may already be
        // inside `read()` when we pause, in which case it still delivers ONE chunk (in
        // practice the shell's own banner + prompt, `ESC[?2004h` + `sh-5.3$ `). Sleeping
        // 300 ms and asserting the channel was empty asserted the opposite of the
        // contract, and lost the race whenever the reader thread won the start.
        assert!(
            gate.wait_until_parked(Duration::from_secs(10)),
            "the reader never reached the pause gate"
        );

        // Everything that escaped is on the channel already (the reader sends BEFORE it
        // parks), so this drains it without racing. It is part of the stream and is kept
        // for the losslessness check below — nothing is discarded.
        let mut gated = String::new();
        while let Ok(chunk) = rx.try_recv() {
            gated.push_str(&String::from_utf8_lossy(&chunk));
        }
        // …and it is only the banner: the gate really did hold the stream back.
        assert!(
            !gated.contains("NYXLINE_200"),
            "the gate let the whole stream through before parking: {gated:?}"
        );

        // PARKED ⇒ not one further byte flows, for a real interval. This is now a
        // property of the reader's state, not a bet on scheduling.
        match rx.recv_timeout(Duration::from_millis(300)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {} // expected: nothing flowed
            Ok(chunk) => panic!(
                "a reader parked at the gate must not deliver output, got {:?}",
                String::from_utf8_lossy(&chunk)
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("unexpected early disconnect"),
        }

        // Resume: the buffered stream must now drain in full, in order.
        gate.set_paused(false);
        let out = format!(
            "{gated}{}",
            read_until(&rx, "NYX_DONE", Duration::from_secs(5))
        );

        // Every line present, none dropped.
        for i in 1..=200 {
            assert!(
                out.contains(&format!("NYXLINE_{i}")),
                "line {i} missing after pause/resume — flow control dropped output"
            );
        }
        // Ordering preserved: line 1 appears before line 200.
        let p1 = out.find("NYXLINE_1\r").or_else(|| out.find("NYXLINE_1\n"));
        let p200 = out.find("NYXLINE_200");
        assert!(
            matches!((p1, p200), (Some(a), Some(b)) if a < b),
            "lines must arrive in order after resume"
        );
        let _ = pty.kill();
    }

    /// Dropping a PTY that was left PAUSED must not deadlock: `Drop` resumes the
    /// gate so the reader can reach EOF and the (Unix) join returns promptly.
    #[test]
    fn drop_is_prompt_when_paused() {
        let (pty, rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
        pty.pause_gate().set_paused(true);
        std::thread::sleep(Duration::from_millis(100)); // let the reader park at the gate

        let (done_tx, done_rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            drop(pty); // must resume the gate, kill, and join without hanging
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "Pty::drop deadlocked while the reader was paused at the gate (>10s)"
        );
        worker.join().expect("worker thread");
        drop(rx);
    }

    #[test]
    fn dropping_many_ptys_is_prompt() {
        // Mirrors the real repro (closing dozens of terminals back-to-back): every
        // teardown must return promptly. The whole batch runs under ONE timeout so
        // a single deadlock surfaces as a FAILURE, not a hung test run.
        let mut ptys = Vec::new();
        for _ in 0..8 {
            ptys.push(Pty::spawn_program("sh", small_size(), None).expect("spawn sh"));
        }
        std::thread::sleep(Duration::from_millis(100));

        let (done_tx, done_rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            for (pty, rx) in ptys {
                drop(pty);
                drop(rx);
            }
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(20)).is_ok(),
            "dropping 8 PTYs did not finish within 20s — a teardown deadlocked"
        );
        worker.join().expect("worker thread");
    }
}
