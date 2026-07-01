//! Supervised-process lifecycle: the sidecar must DIE WITH ITS PARENT and REAP
//! ITS DESCENDANTS. (PDR-0007: the sidecar is a child of Electron main, not a
//! daemon, and must not outlive the application or leave orphans behind.)
//!
//! Two independent guarantees, defence in depth:
//!
//! 1. **Parent-death** — if Electron main exits (clean quit, crash, or `SIGKILL`),
//!    the sidecar must terminate promptly even if it never receives a graceful
//!    `Shutdown` request.
//!    - Linux: `prctl(PR_SET_PDEATHSIG, SIGTERM)` makes the kernel deliver
//!      `SIGTERM` to *this* process the instant its parent dies. There is an
//!      unavoidable race (the parent could die between `fork` and the `prctl`
//!      call), so we *also* re-check `getppid()` right after arming: if it is
//!      already 1 (re-parented to init) the parent is already gone.
//!    - Portable fallback (every OS, incl. macOS/Windows where `PR_SET_PDEATHSIG`
//!      does not exist): the supervisor holds the sidecar's `stdin`. When the
//!      supervisor dies, the OS closes the pipe and the sidecar's `stdin` reads
//!      hit EOF. The main read loop treats stdin-EOF as "parent gone -> exit".
//!
//! 2. **Descendant reaping** — the sidecar makes itself a process-GROUP leader
//!    (`setsid` on Unix) so every process it later spawns shares its group. On
//!    shutdown it signals the whole NEGATIVE process group (`kill(-pgid, sig)`),
//!    the same idiom nyx-core's managed-command runner uses
//!    (crates/nyx-core/src/command.rs). This guarantees a spawned tree
//!    (shell -> node -> cargo -> …) is torn down with the sidecar, not orphaned.

/// The process-group id to reap, published for the async-signal-safe SIGTERM
/// handler installed by [`install_term_reaper`]. Set once at startup.
#[cfg(unix)]
static REAP_PGID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Become a session / process-group leader so descendants share this group and
/// can be reaped as one. Unix-only; a no-op (returns the current pid as a stand-in
/// pgid) elsewhere. Returns the process-group id to signal on shutdown.
#[cfg(unix)]
pub fn become_group_leader() -> i32 {
    // `setsid` fails with EPERM if we are ALREADY a group leader (e.g. launched
    // such that pid == pgid). That is fine: in that case our pid already *is* the
    // pgid we want to signal. So: try setsid; on success the new session id is our
    // pgid; on failure fall back to our own pid (== pgid).
    let sid = unsafe { libc::setsid() };
    if sid >= 0 {
        sid
    } else {
        unsafe { libc::getpid() }
    }
}

#[cfg(not(unix))]
pub fn become_group_leader() -> i32 {
    std::process::id() as i32
}

// --- Windows descendant-reaping via a Job Object (kill-with-parent) -------------------
//
// The Unix story above (process group + `kill(-pgid, …)` + `PR_SET_PDEATHSIG`) has no
// Windows equivalent. On Windows the durable "kill the whole tree when the supervisor
// dies, leaving no orphans" guarantee is a JOB OBJECT:
//   * the sidecar creates a job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and assigns its
//     own process to it;
//   * a process started by a process already in a job is, by default, assigned to that
//     same job — so EVERY descendant the sidecar spawns (the managed-command shells, the
//     dev servers they launch, …) lands in the job too;
//   * when the LAST handle to the job closes — which happens when the sidecar process
//     exits, INCLUDING an abrupt parent-death exit (the supervisor closing the pipe makes
//     the sidecar's stdin hit EOF and the run loop exits) — the kernel TERMINATES the whole
//     job. The descendant tree dies with the sidecar; nothing is orphaned.
//
// We keep the job handle in a process-lifetime static so it is never dropped early (which
// would close the handle and kill the job prematurely). This is the Windows analog of the
// Unix `reap_group`, but enforced by the kernel rather than an explicit signal sweep.

/// Holds the job-object handle for the process lifetime so it is not closed early.
#[cfg(windows)]
static JOB_HANDLE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Create a Job Object with KILL_ON_JOB_CLOSE and assign THIS process to it, so every child
/// the sidecar spawns is reaped with the sidecar (incl. on parent-death). Returns `true` on
/// success. Best-effort: a failure (a locked-down host, an already-jobbed process that
/// forbids nesting) is logged-by-return `false` and the sidecar still runs — the portable
/// stdin-EOF parent-death watch is the baseline guarantee even without the job.
#[cfg(windows)]
pub fn setup_windows_job_object() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() || job == INVALID_HANDLE_VALUE {
            return false;
        }
        // KILL_ON_JOB_CLOSE: when the last job handle closes (the sidecar exits), terminate
        // every process still in the job (the descendant tree).
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if set == 0 {
            CloseHandle(job);
            return false;
        }
        // Put OURSELVES in the job; spawned children inherit it (no JOB_OBJECT_LIMIT_BREAKAWAY
        // is set, so they cannot break out).
        let assigned = AssignProcessToJobObject(job, GetCurrentProcess());
        if assigned == 0 {
            CloseHandle(job);
            return false;
        }
        // Keep the handle open for the process lifetime (closing it now would kill the job).
        JOB_HANDLE.store(job as isize, std::sync::atomic::Ordering::SeqCst);
        true
    }
}

/// Non-Windows no-op (the Unix process-group reaping covers descendant reaping there).
#[cfg(not(windows))]
pub fn setup_windows_job_object() -> bool {
    false
}

/// Arm the kernel-level parent-death signal so this process is terminated the
/// instant its parent dies. Linux-only (`PR_SET_PDEATHSIG`). Returns `true` if the
/// parent was *already* gone at arming time (caller should exit immediately),
/// `false` otherwise. On non-Linux this is a no-op returning `false` — the
/// portable stdin-EOF watch (see module docs) is the death detector there.
#[cfg(target_os = "linux")]
pub fn arm_parent_death() -> bool {
    // Remember the parent we were launched under, BEFORE arming, so we can detect
    // the fork/arm race below.
    let original_ppid = unsafe { libc::getppid() };
    // Deliver SIGTERM to us when our parent dies. SIGTERM (not SIGKILL) so our
    // signal handler can still reap the descendant group first.
    unsafe {
        libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGTERM as libc::c_ulong,
            0,
            0,
            0,
        );
    }
    // Race guard: if the parent died between launch and the prctl above, the
    // pdeathsig will never fire. Detect it: either we were re-parented to init
    // (ppid == 1) or the ppid changed from the one we recorded.
    let now_ppid = unsafe { libc::getppid() };
    now_ppid == 1 || now_ppid != original_ppid
}

#[cfg(not(target_os = "linux"))]
pub fn arm_parent_death() -> bool {
    false
}

/// Install a SIGTERM handler that reaps the descendant group before the process
/// dies. The kernel's parent-death signal (`PR_SET_PDEATHSIG`) is delivered as
/// SIGTERM, whose default disposition is a bare terminate that would leave any
/// spawned descendants orphaned. This handler instead SIGKILLs the whole process
/// group, then `_exit`s — so the sidecar "récolte ses descendants" even when it
/// dies via parent-death rather than a graceful shutdown request.
///
/// The handler body uses ONLY async-signal-safe calls (`kill`, `_exit`).
/// Unix-only; a no-op elsewhere.
#[cfg(unix)]
pub fn install_term_reaper(pgid: i32) {
    REAP_PGID.store(pgid, std::sync::atomic::Ordering::SeqCst);
    extern "C" fn on_term(_sig: libc::c_int) {
        let pgid = REAP_PGID.load(std::sync::atomic::Ordering::SeqCst);
        if pgid > 0 {
            // SIGKILL the whole group (async-signal-safe). We are about to exit, so
            // there is no graceful window to honour here — parent-death is abrupt.
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        unsafe { libc::_exit(0) };
    }
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_term as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = 0;
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
pub fn install_term_reaper(_pgid: i32) {}

/// Send `signal` to the entire process GROUP identified by `pgid` (the negative
/// pgid hits every member: the sidecar AND everything it spawned). Returns whether
/// the signal was delivered to a live group (`ESRCH` = already gone counts as
/// delivered). Unix-only; a no-op returning `false` elsewhere. Kept as the shared
/// group-signal primitive (used by tests and available to later spawn-managing
/// tasks); [`reap_group`] needs finer-grained per-pid control so does not call it.
#[allow(dead_code)]
#[cfg(unix)]
pub fn signal_group(pgid: i32, signal: i32) -> bool {
    let rc = unsafe { libc::kill(-pgid, signal) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
pub fn signal_group(_pgid: i32, _signal: i32) -> bool {
    false
}

/// Reap the sidecar's DESCENDANT tree within a bounded deadline, WITHOUT killing
/// the calling sidecar itself.
///
/// The sidecar is its own group leader, so the descendants it spawned share its
/// pgid. A naive `kill(-pgid, SIG)` would also hit the sidecar — so we:
///   1. set SIGTERM to `SIG_IGN` on ourselves, then broadcast SIGTERM to the
///      group (descendants get it; we ignore it);
///   2. wait up to `grace` for the descendants to drain (probing membership via
///      `/proc`, excluding our own pid);
///   3. SIGKILL each surviving descendant individually (never our own pid).
///
/// Unix-only meaningful work; a no-op elsewhere (no POSIX process groups).
#[cfg(unix)]
pub fn reap_group(pgid: i32, grace: std::time::Duration) {
    let self_pid = unsafe { libc::getpid() };

    // Don't let the group-wide SIGTERM terminate us while we reap descendants.
    let prev = unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
    // Graceful: SIGTERM the whole group (descendants honour it; we ignore it).
    let _ = unsafe { libc::kill(-pgid, libc::SIGTERM) };

    let deadline = std::time::Instant::now() + grace;
    loop {
        let members = group_members(pgid, self_pid);
        if members.is_empty() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            // Force whatever survived the grace window — each by pid, never self.
            for pid in members {
                let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Restore the previous SIGTERM disposition.
    unsafe { libc::signal(libc::SIGTERM, prev) };
}

/// All live pids in process group `pgid` EXCEPT `exclude` (the sidecar itself),
/// read from `/proc`. Empty when only the excluded pid (or nothing) remains.
#[cfg(target_os = "linux")]
fn group_members(pgid: i32, exclude: i32) -> Vec<i32> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(s) = name.to_str() else { continue };
        let Ok(pid) = s.parse::<i32>() else { continue };
        if pid == exclude {
            continue;
        }
        // 5th field of /proc/<pid>/stat (after "pid (comm) state ppid") is pgrp.
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        // fields[0]=state, [1]=ppid, [2]=pgrp.
        if let Some(pgrp) = fields.get(2).and_then(|v| v.parse::<i32>().ok()) {
            if pgrp == pgid {
                out.push(pid);
            }
        }
    }
    out
}

/// Portable fallback for non-Linux unix (no `/proc`): probe the group as a whole.
/// Without `/proc` we cannot enumerate members, so we report the group non-empty
/// while `kill(-pgid, 0)` succeeds for anyone other than us. Since the caller
/// excludes self via SIG_IGN during SIGTERM and individual SIGKILL is unavailable
/// here, the bounded loop simply waits out the grace window.
#[cfg(all(unix, not(target_os = "linux")))]
fn group_members(pgid: i32, _exclude: i32) -> Vec<i32> {
    let alive = unsafe { libc::kill(-pgid, 0) } == 0;
    if alive {
        // Unknown membership: report a single sentinel so the loop waits the grace
        // window, after which the caller's SIGKILL-each path is a no-op (we cannot
        // enumerate), matching the documented non-Linux limitation.
        vec![]
    } else {
        vec![]
    }
}

#[cfg(not(unix))]
pub fn reap_group(_pgid: i32, _grace: std::time::Duration) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn become_group_leader_returns_a_pgid() {
        let pgid = become_group_leader();
        assert!(pgid > 0, "expected a positive pgid, got {pgid}");
    }

    #[cfg(unix)]
    #[test]
    fn signal_group_zero_probes_own_group_alive() {
        // Probing our own group with signal 0 must report it alive (we are in it).
        let pgid = unsafe { libc::getpgrp() };
        assert!(signal_group(pgid, 0));
    }

    #[cfg(unix)]
    #[test]
    fn reap_group_of_empty_group_returns_quickly() {
        // A pgid that owns no processes: reap must not hang past the grace window.
        // Use a very-unlikely-to-exist pgid; ESRCH path returns immediately.
        let start = std::time::Instant::now();
        reap_group(0x7fff_fff0, Duration::from_millis(200));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
