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

/// Monotonic source of PTY ids. Forward-compatible with PRD 1 multi-terminal:
/// the bridge keys its managed state by this id.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a fresh, process-unique PTY id.
fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Resolve the shell to spawn: `$SHELL`, then `bash`, then `sh`.
///
/// We don't probe the filesystem; we hand a program name to the PTY layer and
/// let `execvp` resolve it via `PATH`. `$SHELL` is honored when set and
/// non-empty (the user's real login shell); otherwise we fall back to `bash`,
/// then `sh` (POSIX-guaranteed). The chosen name is returned so callers/tests
/// can assert on it.
fn resolve_shell() -> String {
    match std::env::var("SHELL") {
        Ok(s) if !s.is_empty() => s,
        _ => {
            // `bash` is the pragmatic default; `sh` is the last-resort POSIX
            // shell. We can't cheaply verify existence here without spawning,
            // so prefer `bash` and rely on the spawn to surface a hard failure.
            if which_exists("bash") {
                "bash".to_string()
            } else {
                "sh".to_string()
            }
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
    /// Master side of the PTY. Held for the lifetime of the `Pty` so that the
    /// writer and `resize` keep working; dropping it closes the master fd.
    master: Box<dyn MasterPty + Send>,
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
}

impl Pty {
    /// Spawn the default shell inside a fresh PTY of the given size.
    ///
    /// `cwd` sets the child's working directory when `Some`; otherwise the
    /// child inherits nyx's cwd. The environment is inherited from the current
    /// process (`CommandBuilder` copies the live env by default).
    ///
    /// Returns the [`Pty`] handle and a [`Receiver`] yielding chunks of output
    /// bytes as the reader thread reads them. The receiver completes (the
    /// sender is dropped) when the PTY reaches EOF — i.e. the child has exited.
    pub fn spawn(size: PtySize, cwd: Option<&str>) -> anyhow::Result<(Self, Receiver<Vec<u8>>)> {
        Self::spawn_program(&resolve_shell(), size, cwd)
    }

    /// Spawn an arbitrary program (used by tests; production uses [`Pty::spawn`]
    /// which always launches the default shell).
    pub fn spawn_program(
        program: &str,
        size: PtySize,
        cwd: Option<&str>,
    ) -> anyhow::Result<(Self, Receiver<Vec<u8>>)> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size)?;

        let mut cmd = CommandBuilder::new(program);
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        // Make $TERM sane for full-screen TUIs (vim/htop) in the absence of one.
        if std::env::var_os("TERM").is_none() {
            cmd.env("TERM", "xterm-256color");
        }

        let child = pair.slave.spawn_command(cmd)?;
        // The slave fd is no longer needed in this process once the child holds
        // it; dropping it ensures we see EOF on the master when the child exits.
        drop(pair.slave);

        let killer = child.clone_killer();
        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;

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

        // Waiter thread: block until the child exits, record its exit code.
        let exit_code = Arc::new(Mutex::new(None::<i32>));
        let waiter_handle = {
            let exit_code = Arc::clone(&exit_code);
            let mut child: Box<dyn Child + Send + Sync> = child;
            std::thread::Builder::new()
                .name("nyx-pty-waiter".into())
                .spawn(move || {
                    if let Ok(status) = child.wait() {
                        *exit_code.lock().unwrap() = Some(status.exit_code() as i32);
                    }
                })?
        };

        let pty = Pty {
            id: next_id(),
            master: pair.master,
            writer,
            killer,
            reader_handle: Some(reader_handle),
            waiter_handle: Some(waiter_handle),
            exit_code,
        };
        Ok((pty, rx))
    }

    /// Opaque, process-unique id of this PTY (used by the bridge to key state).
    pub fn id(&self) -> u64 {
        self.id
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
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width,
            pixel_height,
        })?;
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
        // the waiter unblocks from `wait`, and the reader sees EOF once the
        // master/child fds close.
        let _ = self.killer.kill();
        if let Some(handle) = self.waiter_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.reader_handle.take() {
            let _ = handle.join();
        }
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
        // Spawn, then drop; Drop must kill the child and JOIN both helper
        // threads. If a thread leaked (failed to terminate), the joins in Drop
        // would block forever and this test would hang (caught as a timeout).
        // We also assert the id allocator advances exactly once per spawn (no
        // double-spawn / no stray PTY) without assuming an absolute id, since
        // tests run in parallel against the shared global counter.
        let (held, _rx) = Pty::spawn_program("sh", small_size(), None).expect("spawn baseline sh");
        let baseline = held.id();
        let dropped_id = {
            let (pty, _rx) =
                Pty::spawn_program("sh", small_size(), None).expect("spawn sh to drop");
            pty.id()
            // Drop here: kill + join(reader) + join(waiter). Must return promptly.
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
}
