//! Linux-specific test of the KERNEL parent-death path (`PR_SET_PDEATHSIG`),
//! distinct from the portable stdin-EOF path covered in `lifecycle.rs`.
//!
//! We need a REAL intermediate parent so that killing it leaves the sidecar an
//! orphan whose only death trigger is the kernel pdeathsig (we keep the sidecar's
//! stdin open the whole time, so the stdin-EOF detector is ruled out).
//!
//! Layout:
//!   test ── spawns ──▶ sh (intermediate parent)
//!                       └─ spawns ──▶ nyx-sidecar (grandchild of the test)
//!
//! The `sh` reports the sidecar's pid on its stdout (which the test reads), then
//! waits. The sidecar's stdin is a pipe whose write end the TEST holds open, so
//! stdin never EOFs. The test SIGKILLs `sh`. On Linux the kernel immediately
//! sends SIGTERM to the sidecar (PR_SET_PDEATHSIG armed at startup), so the
//! sidecar must die. We poll `/proc` to confirm.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn sidecar_bin() -> String {
    env!("CARGO_BIN_EXE_nyx-sidecar").to_string()
}

/// Is `pid` a live, non-zombie process?
fn pid_alive(pid: i32) -> bool {
    let status = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(_) => return false,
    };
    if let Some(close) = status.rfind(')') {
        if let Some(state) = status[close + 1..].split_whitespace().next() {
            return state != "Z";
        }
    }
    true
}

#[test]
fn sidecar_dies_when_intermediate_parent_is_killed() {
    // Pipe feeding the sidecar's stdin; the test keeps the WRITE end open so the
    // sidecar's stdin never reaches EOF (isolating the kernel pdeathsig path).
    let mut sc_stdin = [0i32; 2];
    assert_eq!(
        unsafe { libc::pipe(sc_stdin.as_mut_ptr()) },
        0,
        "pipe failed"
    );
    let (sc_stdin_r, sc_stdin_w) = (sc_stdin[0], sc_stdin[1]);

    // The intermediate `sh`:
    //   * starts the sidecar in the background with stdin from fd 0 (inherited
    //     from the pipe read end we attach),
    //   * prints the sidecar's pid (`$!`) to its stdout,
    //   * `wait`s so it stays alive as the sidecar's parent until we kill it.
    let read_dup = unsafe { libc::dup(sc_stdin_r) };
    assert!(read_dup >= 0, "dup failed");

    // `<&0` forces the BACKGROUNDED sidecar to inherit sh's stdin (our open pipe)
    // instead of the `/dev/null` POSIX sh otherwise redirects async jobs to — so
    // the sidecar's stdin stays open and only the kernel pdeathsig can kill it.
    let mut sh = Command::new("sh")
        .arg("-c")
        .arg(format!("{} <&0 & echo $!; wait", sidecar_bin()))
        .stdin(unsafe { Stdio::from(OwnedFd::from_raw_fd(read_dup)) })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn intermediate sh");

    // We don't need our copy of the read end anymore (sh dup'd it); keep the WRITE
    // end open to hold the sidecar's stdin open.
    unsafe { libc::close(sc_stdin_r) };

    // Read the sidecar pid that `sh` echoed.
    let mut sh_out = BufReader::new(sh.stdout.take().expect("sh stdout"));
    let mut line = String::new();
    sh_out.read_line(&mut line).expect("read sidecar pid");
    let sidecar_pid: i32 = line.trim().parse().expect("parse sidecar pid");

    // Let the sidecar arm PR_SET_PDEATHSIG.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        pid_alive(sidecar_pid),
        "sidecar (pid {sidecar_pid}) should be alive before we kill its parent"
    );

    // Kill the intermediate parent. The kernel pdeathsig must now fire on the
    // sidecar even though its stdin is still open (we still hold the write end).
    let sh_pid = sh.id() as i32;
    assert_eq!(unsafe { libc::kill(sh_pid, libc::SIGKILL) }, 0, "kill sh");
    // Reap sh so it doesn't linger as a zombie.
    let _ = sh.wait();

    // The sidecar must die from pdeathsig, NOT from stdin EOF (still open).
    let start = Instant::now();
    while pid_alive(sidecar_pid) {
        if start.elapsed() > Duration::from_secs(5) {
            unsafe { libc::kill(sidecar_pid, libc::SIGKILL) };
            unsafe { libc::close(sc_stdin_w) };
            panic!("sidecar survived its parent's death (pdeathsig did not fire)");
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    // Clean up the write end we held open.
    unsafe { libc::close(sc_stdin_w) };
}
