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
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

/// Env var carrying nyx's PERSISTENT terminal record id (`terminals.id`) into the
/// spawned shell and everything it launches (PRD-5 task #3). An agent integration
/// running INSIDE the shell (e.g. the Claude plugin's SessionStart hook) reads this
/// to correlate its session event back to the exact nyx terminal — unambiguously,
/// even when two sessions share a cwd. Injected on the production interactive-shell
/// spawn path ([`Pty::spawn`]) whenever the caller supplies the record id; it then
/// propagates to child processes via normal env inheritance on every OS. The value
/// is exactly the `terminals.id` the bridge passed (see `pty_spawn`).
pub const NYX_TERMINAL_ID_ENV: &str = "NYX_TERMINAL_ID";

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
/// `pub(crate)` so the managed-command runtime ([`crate::command`]) spawns its
/// read-only command PTYs under the SAME shell as the terminals (honors `$SHELL`
/// identically), instead of duplicating the resolution.
pub(crate) fn resolve_shell() -> String {
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
    /// Writer onto the PTY master (i.e. the child's stdin).
    writer: Box<dyn Write + Send>,
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
    /// Returns the [`Pty`] handle and a [`Receiver`] yielding chunks of output
    /// bytes as the reader thread reads them. The receiver completes (the
    /// sender is dropped) when the PTY reaches EOF — i.e. the child has exited.
    pub fn spawn(
        size: PtySize,
        cwd: Option<&str>,
        terminal_id: Option<&str>,
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
        Self::spawn_program_with_integration(&shell, size, cwd, &plan, terminal_id)
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
        // No terminal-record id on the program path: tests / the managed-command
        // runner spawn arbitrary non-interactive programs that carry no agent session.
        Self::spawn_program_with_integration(
            program,
            size,
            cwd,
            &crate::shellinteg::IntegrationPlan::default(),
            None,
        )
    }

    /// TEST-ONLY: spawn `program` with the given `args`, exporting `terminal_id` as
    /// [`NYX_TERMINAL_ID_ENV`], so the env-injection path (PRD-5 task #3) can be
    /// exercised on a controllable program per-OS (a POSIX `sh -c` on Unix, `cmd /C`
    /// on Windows) WITHOUT depending on which interactive shell `resolve_shell`
    /// picks. Production code uses [`Pty::spawn`], which carries the id from the
    /// bridge; this reuses the SAME single env-injection line via an
    /// [`crate::shellinteg::IntegrationPlan`] that only carries the args.
    #[cfg(test)]
    pub fn spawn_program_with_terminal_id(
        program: &str,
        args: &[&str],
        size: PtySize,
        cwd: Option<&str>,
        terminal_id: Option<&str>,
    ) -> anyhow::Result<(Self, Receiver<Vec<u8>>)> {
        let plan = crate::shellinteg::IntegrationPlan {
            args: args.iter().map(std::ffi::OsString::from).collect(),
            ..Default::default()
        };
        Self::spawn_program_with_integration(program, size, cwd, &plan, terminal_id)
    }

    /// Spawn `program`, applying a shell-integration [`IntegrationPlan`]'s extra
    /// args + env (empty plan = plain spawn). The single real spawn path the two
    /// public constructors share.
    ///
    /// `terminal_id`, when `Some`, is exported as [`NYX_TERMINAL_ID_ENV`] so the
    /// child shell (and everything it launches) carries nyx's persistent terminal
    /// record id for agent-session correlation (PRD-5 task #3).
    fn spawn_program_with_integration(
        program: &str,
        size: PtySize,
        cwd: Option<&str>,
        plan: &crate::shellinteg::IntegrationPlan,
        terminal_id: Option<&str>,
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

        let child = pair.slave.spawn_command(cmd)?;
        // The slave fd is no longer needed in this process once the child holds
        // it; dropping it ensures we see EOF on the master when the child exits.
        drop(pair.slave);

        // Capture the shell pid before moving `child` into the waiter thread.
        // This is the anchor for live cwd lookups via `/proc/<pid>/cwd`.
        let shell_pid = child.process_id();
        let killer = child.clone_killer();
        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;
        // Share the master with the waiter thread so it can close it on child
        // exit (the lever that unblocks the reader on Windows; see the field doc).
        let master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> =
            Arc::new(Mutex::new(Some(pair.master)));

        // Reader thread: pump raw bytes onto the channel until EOF.
        let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = mpsc::channel();
        let reader_handle = std::thread::Builder::new()
            .name("nyx-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
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
            })?;

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
        };
        Ok((pty, rx))
    }

    /// Opaque, process-unique id of this PTY (used by the bridge to key state).
    pub fn id(&self) -> u64 {
        self.id
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

    /// Write bytes to the PTY (the child's stdin). Flushes immediately so
    /// keystrokes are delivered without buffering latency.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
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
        // Ensure the child is dead so both helper threads can terminate:
        // the waiter unblocks from `wait`, and (on Unix) the reader sees EOF once
        // the master/child fds close.
        let _ = self.killer.kill();
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
        let (mut pty, rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn sh");
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
        )
        .expect("spawn nested sh");
        let out = read_until(&rx, "NYXTID=[", Duration::from_secs(5));
        assert!(
            out.contains(&format!("NYXTID=[{tid}]")),
            "the var must inherit into a child process of the shell; got: {out:?}"
        );
        let _ = pty.kill();
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
