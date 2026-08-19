//! The DEFAULT-SHELL preference — detection, persistence, and the resolution order a spawn reads.
//!
//! ## Why this module exists
//!
//! Which program a nyx terminal opens on was, until now, entirely implicit: [`crate::pty::spawn`]
//! asked [`crate::pty::resolve_shell`], which honours `$SHELL` and otherwise picks a per-OS default
//! ([`crate::pty::resolve_shell`]'s `TODO(idea)` is exactly this module). Implicit is fine as a
//! DEFAULT and unacceptable as the only answer: on Windows `$SHELL` is normally unset, so the user
//! got PowerShell whether or not that is the shell they work in, and nothing in the app could say
//! otherwise. This module gives that decision a name, a place to live and a way to be changed.
//!
//! ## The three concerns, in order
//!
//! 1. **Detection** ([`detect`]) — WHICH shells this machine actually has. The reference is Windows
//!    Terminal's profile menu: a list DISCOVERED from the machine, not a hard-coded vocabulary the
//!    user must match. The rule that makes such a list trustworthy is that every entry is real —
//!    [`detect`] never returns a shell whose binary is absent from disk, so nothing offered in the
//!    UI can fail at spawn.
//! 2. **Persistence** ([`preference`] / [`set_preference`]) — the chosen shell, stored as the
//!    `default_shell` key of the app-level preference store (`app_settings`). The ABSENCE of
//!    the key is meaningful: it is "the user has expressed nothing", which is what keeps the
//!    historical behaviour intact for every install that never opens the setting.
//! 3. **Resolution** ([`resolve`]) — what a spawn actually runs, in ONE order, defined once:
//!
//!    ```text
//!    stored preference (while it still names a shell this machine can open)  >  $SHELL  >  per-OS default
//!    ```
//!
//!    The order is chosen so that WITHOUT a preference the resolution is EXACTLY the historical
//!    one (`$SHELL`, then the per-OS default of [`crate::pty::resolve_shell`]) — same code path,
//!    same result — and so that a preference that has since stopped naming an openable shell
//!    degrades to that same historical chain instead of failing the spawn. A terminal that will not
//!    open is a far worse outcome than a terminal that opens on the wrong shell.
//!
//!    "Still names a shell this machine can open" is the WHOLE pair, program and arguments: the
//!    binary on disk ([`program_exists`]) and, for a WSL choice, the distribution still registered.
//!    Checking the path alone was a hole with a name — `wsl --unregister <distro>` leaves `wsl.exe`
//!    in place, so the preference kept winning and the terminal died at once (review
//!    `01M0BT2SST6ZMG84SW1CW9BSWH`).
//!
//! ## Why a shell is a program AND its arguments
//!
//! A [`ShellChoice`] carries `args`, not just a path, because one of the shells a Windows machine
//! really offers cannot be named by a path: a WSL distribution is `wsl.exe -d <distro>`. Without
//! args, "the shells on this machine" would silently mean "the shells reachable as a bare
//! executable", and every WSL user would find their shell missing from a list that claims to be
//! discovered from their machine. The args are FIXED per detected entry — they come from detection,
//! never from user input — so this is not the per-profile argument editing Windows Terminal offers
//! (explicitly out of scope); it is what makes a detected entry launchable at all.
//!
//! ## What this module does NOT do
//!
//! - It does not touch LIVE terminals. The preference is read at spawn, so it applies to the NEXT
//!   terminal and never relaunches one that is already running.
//! - It does not cover managed commands ([`crate::command`]), which keep spawning under
//!   [`crate::pty::resolve_shell`]: a command's shell is an execution detail of a project's script,
//!   not the interactive shell the user chose to work in.
//! - It carries no per-project or per-workspace override, and no notion of a "profile" beyond the
//!   program to run (no icon, no colours, no dedicated shortcut).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use diesel::result::QueryResult;
use diesel::SqliteConnection;
use serde::{Deserialize, Serialize};

use crate::db;

/// WHAT a spawn runs: a program plus the fixed arguments it needs to be that shell.
///
/// `args` is empty for every shell that is a plain executable (PowerShell, cmd, bash, zsh…) and
/// non-empty only where the shell has no executable of its own — a WSL distribution, which is
/// `wsl.exe` pointed at a distro name. Serialized camelCase: this is the SHAPE the renderer sends
/// back when the user picks a shell, and the shape stored under
/// [`db::SETTING_DEFAULT_SHELL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellChoice {
    /// Absolute path to the program to spawn (a bare name only for the historical `$SHELL` /
    /// per-OS fallbacks, which are resolved through `PATH` by the OS at spawn).
    pub path: String,
    /// The fixed arguments that turn `path` into this shell. Empty for a plain shell executable.
    #[serde(default)]
    pub args: Vec<String>,
}

impl ShellChoice {
    /// A plain shell executable with no arguments — the shape of every fallback and of every
    /// detected entry except WSL.
    pub fn program(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            args: Vec::new(),
        }
    }
}

/// One entry of the detected-shell list the Settings section renders.
///
/// `id` is the STABLE identity the UI keys its options on (and the value its `<select>` carries);
/// `path` + `args` are what the renderer sends back on a pick — the UI never re-derives a command
/// line, it hands back the entry detection produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedShell {
    /// Stable slug for this entry (`windows-powershell`, `pwsh`, `cmd`, `git-bash`, `wsl:Ubuntu`,
    /// or the absolute path for a `/etc/shells` entry).
    pub id: String,
    /// Human-readable name, in the vocabulary of the platform the user knows (the Windows Terminal
    /// profile names on Windows, the shell's own name on Unix).
    pub label: String,
    pub path: String,
    pub args: Vec<String>,
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// The shells REALLY installed on this machine, in the order the UI lists them.
///
/// Every returned entry has been verified against the disk: a candidate whose binary is missing is
/// dropped, never listed as unavailable. That is the whole contract of this list — the user can
/// pick anything in it and the terminal will open.
///
/// Duplicates are collapsed on the resolved program path, so a shell reachable by two routes
/// (`pwsh.exe` on `PATH` and its install directory) appears once.
pub fn detect() -> Vec<DetectedShell> {
    let mut out: Vec<DetectedShell> = Vec::new();
    for shell in detect_candidates() {
        if out
            .iter()
            .any(|kept| kept.path.eq_ignore_ascii_case(&shell.path) && kept.args == shell.args)
        {
            continue;
        }
        out.push(shell);
    }
    out
}

/// The per-OS candidate list, already filtered to what exists on disk (the de-duplication is
/// [`detect`]'s). Split out so each platform's discovery reads on its own.
#[cfg(windows)]
fn detect_candidates() -> Vec<DetectedShell> {
    let mut out = Vec::new();

    // Windows PowerShell — the one shell that ships with the OS, at a FIXED location under
    // System32. Looked up by path rather than through `PATH` because that is where it is, and a
    // `PATH` lookup would additionally depend on the user's `PATH` being intact.
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let win_ps = PathBuf::from(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if win_ps.is_file() {
        out.push(DetectedShell {
            id: "windows-powershell".to_string(),
            label: "Windows PowerShell".to_string(),
            path: win_ps.to_string_lossy().into_owned(),
            args: Vec::new(),
        });
    }

    // PowerShell 7+ — installed separately, so it is found through `PATH` (which is how its
    // installer makes it reachable) and listed with its resolved absolute path.
    if let Some(pwsh) = on_path("pwsh.exe") {
        out.push(DetectedShell {
            id: "pwsh".to_string(),
            label: "PowerShell".to_string(),
            path: pwsh.to_string_lossy().into_owned(),
            args: Vec::new(),
        });
    }

    // Command Prompt — `%ComSpec%` is the OS's own answer to "where is cmd"; System32 is the
    // fallback for the (effectively impossible) case where it is unset.
    let comspec = std::env::var("ComSpec").unwrap_or_else(|_| {
        PathBuf::from(&system_root)
            .join("System32")
            .join("cmd.exe")
            .to_string_lossy()
            .into_owned()
    });
    if PathBuf::from(&comspec).is_file() {
        out.push(DetectedShell {
            id: "cmd".to_string(),
            label: "Command Prompt".to_string(),
            path: comspec,
            args: Vec::new(),
        });
    }

    // Git Bash — the POSIX shell most Windows developers actually work in. The candidate paths are
    // the ones the e2e harness already probes (`e2e/wdio.conf.cjs`), reused rather than reinvented
    // so the app and the suite agree on where Git Bash lives.
    for candidate in git_bash_candidates() {
        if candidate.is_file() {
            out.push(DetectedShell {
                id: "git-bash".to_string(),
                label: "Git Bash".to_string(),
                path: candidate.to_string_lossy().into_owned(),
                args: Vec::new(),
            });
            break;
        }
    }

    // WSL distributions — one entry per INSTALLED distro, minus Docker Desktop's service VMs (see
    // `WSL_SERVICE_DISTRIBUTIONS`). The binary is `wsl.exe` (System32, so it is present whenever
    // WSL is), and the distro is named by an argument: this is the one shell family that cannot be
    // expressed by a path alone, which is why `ShellChoice` carries args.
    if let Some(wsl) = on_path("wsl.exe") {
        let wsl = wsl.to_string_lossy().into_owned();
        for distro in wsl_distributions() {
            out.push(DetectedShell {
                id: format!("wsl:{distro}"),
                label: format!("{distro} (WSL)"),
                path: wsl.clone(),
                args: vec!["-d".to_string(), distro],
            });
        }
    }

    out
}

/// The Git Bash install locations to probe, in the order `e2e/wdio.conf.cjs` probes them.
#[cfg(windows)]
fn git_bash_candidates() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(p) = std::env::var("ProgramFiles") {
        roots.push(PathBuf::from(p).join("Git"));
    } else {
        roots.push(PathBuf::from("C:\\Program Files\\Git"));
    }
    if let Ok(p) = std::env::var("ProgramFiles(x86)") {
        roots.push(PathBuf::from(p).join("Git"));
    }
    if let Ok(p) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(p).join("Programs").join("Git"));
    }
    roots
        .into_iter()
        .map(|root| root.join("bin").join("bash.exe"))
        .collect()
}

// ---------------------------------------------------------------------------
// The WSL probe — OFF the dispatch thread (review `01M0BT262WB7VZQKHQZHEP281T`)
// ---------------------------------------------------------------------------

/// A value READ instantly and REFRESHED on someone else's thread.
///
/// ## Why this exists
///
/// [`detect`] is answered INLINE on the sidecar's SINGLE command-dispatch thread (`nyx-sidecar`'s
/// framed control loop serves one request at a time), so anything it waits on, EVERY other backend
/// command waits on too — a keystroke's `pty_write`, a terminal spawn, every DB read of every
/// surface, and 3 s out of the 5 s supervisor deadline of a request already in flight. Bounding
/// the wait (the previous fix) capped the damage without removing it: opening Settings on a machine
/// whose LxssManager is cold still froze the whole backend for up to three seconds.
///
/// So the wait is not bounded here, it is MOVED. A read answers with what is already known — never
/// waiting, not even on a cold cache — and hands the slow question to a worker.
///
/// ## The single-flight latch
///
/// `refreshing` is the other half, and it is what keeps a SLOW probe from becoming a LEAK: the
/// previous shape spawned one worker (and one `wsl.exe`) per call and abandoned both on timeout, so
/// every Settings open accumulated a thread and a hung process. At most one refresh is ever in
/// flight here, so N reads cost ONE worker, and a worker that overruns is killed and reaped by
/// [`bounded_output`] rather than left behind.
#[cfg_attr(not(windows), allow(dead_code))]
struct BackgroundCache<T> {
    /// The last answer the probe produced, or `None` while it has never produced one.
    value: Mutex<Option<T>>,
    /// Whether a refresh worker is in flight. Set BEFORE the thread is spawned, cleared by the
    /// worker itself, so a concurrent reader can never slip a second probe in between.
    refreshing: AtomicBool,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl<T: Clone + Send + 'static> BackgroundCache<T> {
    const fn new() -> Self {
        Self {
            value: Mutex::new(None),
            refreshing: AtomicBool::new(false),
        }
    }

    /// What is known NOW (`None` = nothing yet), and a refresh started in the background. Returns
    /// without ever waiting on the probe — that is the whole point.
    fn read_and_refresh(&'static self, probe: fn() -> T) -> Option<T> {
        let _started = self.refresh(probe);
        self.read()
    }

    /// What is known now, with no refresh — the pure read.
    fn read(&'static self) -> Option<T> {
        // A poisoned lock is a worker that panicked; the value behind it is still a perfectly good
        // (if stale) list, and refusing to answer a shell list over it would be strictly worse.
        self.value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Start a refresh unless one is already running. Returns immediately either way, and says
    /// whether IT is the call that started a worker — the single-flight decision, made observable
    /// so a test can assert "twenty reads, one probe" without timing a thread.
    fn refresh(&'static self, probe: fn() -> T) -> bool {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return false; // Already probing — one worker at a time, never one per call.
        }
        let spawned = std::thread::Builder::new()
            .name("nyx-shell-probe".into())
            .spawn(move || {
                // The probe is TOTAL: it owns what its own failures mean and always states an
                // answer, so the cache never has to guess whether a stale value is still true.
                let fresh = probe();
                *self
                    .value
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fresh);
                self.refreshing.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            // The thread never started, so nothing will ever clear the latch — release it here or
            // the cache would be frozen for the process's lifetime.
            self.refreshing.store(false, Ordering::SeqCst);
        }
        spawned.is_ok()
    }
}

/// How long the WSL enumeration may run before it is KILLED. Not a caller's wait any more (see
/// [`BackgroundCache`]) — it is the worker's own leash, so a `wsl.exe` hung on a broken install
/// cannot outlive it.
#[cfg_attr(not(windows), allow(dead_code))]
const WSL_ENUMERATION_TIMEOUT: Duration = Duration::from_secs(3);

/// How often [`bounded_output`] looks at a child it is waiting on. Short enough that the kill lands
/// promptly after the deadline, long enough that the worker is not a spin loop.
#[cfg_attr(not(windows), allow(dead_code))]
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The cached answer of the WSL enumeration. See [`wsl_distributions`].
#[cfg(windows)]
static WSL_DISTRIBUTIONS: BackgroundCache<Vec<String>> = BackgroundCache::new();

/// The names of the INSTALLED WSL distributions, or an empty list while nothing has been probed yet
/// (and when WSL reports none, answers in a way we cannot read, or has to be killed for overrunning
/// [`WSL_ENUMERATION_TIMEOUT`]).
///
/// `wsl.exe --list --quiet` is the only enumeration WSL offers; it prints UTF-16LE, which is why
/// the output goes through [`decode_console_output`] rather than `String::from_utf8`. Failure of
/// any kind yields an empty list: a shell list that is short is a far better outcome than a
/// Settings section that errors.
///
/// IT NEVER WAITS. The probe is not cheap — `wsl.exe --list` starts LxssManager when the service is
/// cold and is known to hang outright on a broken install — and this is called on the sidecar's
/// single dispatch thread, so it answers from [`WSL_DISTRIBUTIONS`] and leaves the probing to a
/// worker ([`BackgroundCache`] carries the full argument). [`warm_detection`] runs the first probe
/// at boot, off that thread, so the list is already true by the time a user can open Settings.
///
/// The parsing (and the service-distribution filter) is [`parse_wsl_distributions`]'s, so it can be
/// held to a captured `wsl --list` output without a WSL install.
#[cfg(windows)]
fn wsl_distributions() -> Vec<String> {
    WSL_DISTRIBUTIONS
        .read_and_refresh(probe_wsl_distributions)
        .unwrap_or_default()
}

/// The worker's half of [`wsl_distributions`]: run the enumeration and parse it.
///
/// TOTAL by design — a failure of any kind (WSL absent, no distribution, an output we cannot
/// decode, an overrun that got the process killed) is stated as the EMPTY list, the same
/// degradation this probe has always had. It is deliberately NOT reported as "unknown, keep the
/// last list": the menu's contract is that everything in it is real, and a list of distributions
/// that could not be re-verified is exactly what would put an unregistered distro back on screen.
#[cfg(windows)]
fn probe_wsl_distributions() -> Vec<String> {
    wsl_list_output()
        .map(|raw| parse_wsl_distributions(&raw))
        .unwrap_or_default()
}

/// START the machine's shell discovery, off whatever thread will later ASK for it.
///
/// The sidecar binary calls this ONCE at boot, before its run loop starts serving. Without it the
/// first `shell_list` — the one a user's first Settings open issues — would answer from a cache
/// nothing had filled yet and omit every WSL distribution. With it, the slow question is asked at a
/// moment when no request is waiting on the answer, and the first read is already correct.
///
/// Starting `wsl.exe --list` at launch is the same thing Windows Terminal does for its dynamic WSL
/// profiles, which is the reference [`detect`] is built against; it enumerates the service's
/// registry, it does not boot a distribution.
///
/// A no-op on every other OS: `/etc/shells` is a file read, with nothing slow to move anywhere.
pub fn warm_detection() {
    #[cfg(windows)]
    WSL_DISTRIBUTIONS.refresh(probe_wsl_distributions);
}

/// The raw (decoded) `wsl.exe --list --quiet` output, or `None` when WSL is absent, refuses, or
/// cannot be read. Split out of [`probe_wsl_distributions`] so the latter is purely the parse.
#[cfg(windows)]
fn wsl_list_output() -> Option<String> {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW` — the sidecar is a background process; enumerating distributions must
    /// never flash a console window on the user's screen.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut child = std::process::Command::new("wsl.exe")
        .args(["--list", "--quiet"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let raw = bounded_output(&mut child, WSL_ENUMERATION_TIMEOUT)?;
    Some(decode_console_output(&raw))
}

/// A child's stdout, given AT MOST `timeout` to arrive — and the child KILLED AND REAPED if it does
/// not. `None` on an overrun, on a non-zero exit, or when the pipe cannot be read.
///
/// This is the anti-leak half of the previous shape's fix. `Command::output()` waits on the child's
/// pipes with no bound at all, so the old code could only abandon the worker on timeout: a `wsl.exe`
/// stuck on a broken install stayed stuck, holding a thread and a process, ONE PER SETTINGS OPEN.
/// Polling `try_wait` against a deadline and calling `kill` + `wait` on overrun ends both — the
/// process is terminated and reaped, and the worker returns instead of being orphaned.
///
/// Reading stdout only AFTER the child has exited is safe for the one caller: `wsl --list --quiet`
/// prints a handful of distribution names, far under the pipe buffer, so the child can never block
/// on a full pipe while we are polling. It is not a general-purpose runner and must not become one
/// for an output that could be large.
#[cfg_attr(not(windows), allow(dead_code))]
fn bounded_output(child: &mut std::process::Child, timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut raw = Vec::new();
                child.stdout.take()?.read_to_end(&mut raw).ok()?;
                return Some(raw);
            }
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            // OVERRUN: end it here rather than leaving it to finish on its own. `wait` after `kill`
            // is what actually reaps the process — without it the child would linger as a zombie on
            // Unix and its handle would stay open on Windows.
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(CHILD_POLL_INTERVAL);
    }
}

/// The SERVICE distributions Docker Desktop installs for its own engine — never a shell anyone
/// works in.
///
/// They are excluded for PARITY WITH WINDOWS TERMINAL, which is the reference this list is built
/// against ([`detect`]): Windows Terminal's dynamic WSL profile generator hides exactly these, so a
/// user who knows their machine through that menu would read a `docker-desktop` entry here as nyx
/// offering something Windows does not. `docker-desktop-data` cannot even be entered (it is a data
/// volume distribution with no init), and `docker-desktop` drops into the engine's own utility VM —
/// so listing them is not just noise, it is offering two entries out of a menu whose entire
/// contract is that everything in it opens.
///
/// This is a NAMED exclusion, not a general "hide what looks like infrastructure" heuristic: any
/// other distribution the machine reports is a shell the user installed on purpose and is listed.
#[cfg_attr(not(windows), allow(dead_code))]
const WSL_SERVICE_DISTRIBUTIONS: [&str; 2] = ["docker-desktop", "docker-desktop-data"];

/// Read `wsl.exe --list --quiet`'s (already decoded) output into distribution names.
///
/// Each line is one distribution, and `line.trim()` is all the cleanup a line needs: [`str::lines`]
/// already drops the `\r` of a CRLF ending, and `trim` removes any other padding WSL leaves around
/// a name (`\r` included — it is Unicode whitespace).
///
/// The BOM is the one thing `trim` canNOT do, which is why it is handled explicitly first: `U+FEFF`
/// is not `char::is_whitespace`, so a byte-order mark surviving the UTF-16 decode
/// ([`decode_console_output`]) would stay glued to the FIRST name and make it miss the
/// [`WSL_SERVICE_DISTRIBUTIONS`] comparison below — the one case where a Docker Desktop service VM
/// would reach the menu. Those service distributions are dropped here, at the detection site, so
/// nothing downstream has to know they exist.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_wsl_distributions(raw: &str) -> Vec<String> {
    raw.trim_start_matches('\u{feff}')
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .filter(|name| {
            !WSL_SERVICE_DISTRIBUTIONS
                .iter()
                .any(|service| name.eq_ignore_ascii_case(service))
        })
        .collect()
}

/// Decode a Windows console program's stdout, which may be UTF-16LE (WSL's is) or UTF-8.
///
/// The discriminator is the NUL bytes UTF-16LE puts between the bytes of every ASCII character:
/// ASCII text encoded as UTF-16LE is half zeros, while UTF-8 text carries none at all. Anything we
/// cannot decode comes back as an empty string, which the caller reads as "no distributions".
#[cfg(windows)]
fn decode_console_output(bytes: &[u8]) -> String {
    let zeros = bytes.iter().filter(|b| **b == 0).count();
    if bytes.len() >= 2 && zeros * 4 >= bytes.len() {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// The per-OS candidate list on Unix: the system's own registry of login shells, `/etc/shells`.
///
/// That file IS the platform's answer to "which shells does this machine offer" (it is what `chsh`
/// validates against), so nyx reads it rather than probing a hard-coded list. Comments and blank
/// lines are skipped, and every remaining entry is still checked against the disk — `/etc/shells`
/// routinely outlives the package it names.
#[cfg(not(windows))]
fn detect_candidates() -> Vec<DetectedShell> {
    let Ok(contents) = std::fs::read_to_string("/etc/shells") else {
        return Vec::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| std::path::Path::new(line).is_file())
        .map(|line| {
            let label = std::path::Path::new(line)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| line.to_string());
            DetectedShell {
                id: line.to_string(),
                label,
                path: line.to_string(),
                args: Vec::new(),
            }
        })
        .collect()
}

/// Resolve a bare program name against `PATH`, returning its absolute path.
///
/// Shared with [`crate::pty::resolve_shell`]'s fallback probe so "is this program installed" has a
/// single implementation: detection and the spawn-time fallback must agree, or the list could offer
/// a shell the spawn then refuses to find.
pub fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Whether `program` can be spawned: an absolute/relative path must BE a file, a bare name must be
/// findable on `PATH`.
///
/// This is the check that keeps a stale preference from breaking a terminal: the shell the user
/// picked months ago may have been uninstalled since, and the answer to that must be a fallback,
/// not a failed spawn.
pub fn program_exists(program: &str) -> bool {
    if program.is_empty() {
        return false;
    }
    let path = std::path::Path::new(program);
    if path.is_absolute() || program.contains('/') || program.contains('\\') {
        return path.is_file();
    }
    on_path(program).is_some()
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// READ the stored default-shell preference, or `None` when the user has expressed none.
///
/// A stored value that no longer parses (a hand-edited DB, a format that predates a change) is read
/// as `None` — the same verdict as "nothing chosen", so the resolution falls back instead of
/// propagating a parse error into a terminal spawn.
pub fn preference(conn: &mut SqliteConnection) -> QueryResult<Option<ShellChoice>> {
    Ok(db::get_app_setting(conn, db::SETTING_DEFAULT_SHELL)?
        .and_then(|raw| serde_json::from_str::<ShellChoice>(&raw).ok())
        .filter(|choice| !choice.path.is_empty()))
}

/// WRITE the default-shell preference, or CLEAR it (`None` = "Auto", back to the historical
/// resolution). Stored as JSON so the program and its arguments travel together.
pub fn set_preference(
    conn: &mut SqliteConnection,
    choice: Option<&ShellChoice>,
) -> QueryResult<()> {
    let encoded = choice.map(|c| serde_json::to_string(c).unwrap_or_default());
    db::set_app_setting(conn, db::SETTING_DEFAULT_SHELL, encoded.as_deref())
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// The shell a NEW terminal opens on, given the stored `preference`.
///
/// The single definition of the order documented at the top of this module: an INTACT preference
/// wins; anything else — no preference, or a preference that no longer names a shell this machine
/// can open — falls through to [`crate::pty::resolve_shell`], i.e. `$SHELL` then the per-OS
/// default. Total: it always returns something to spawn.
pub fn resolve(preference: Option<&ShellChoice>) -> ShellChoice {
    // The machine's distribution list is consulted ONLY for a preference that names one, so a
    // user who picked a plain shell never starts a `wsl.exe` from the spawn path.
    let known = preference
        .filter(|choice| wsl_target(choice).is_some())
        .and_then(|_| live_distributions());
    resolve_against(preference, known.as_deref())
}

/// [`resolve`] against an EXPLICIT view of the machine's WSL distributions (`None` = "not probed,
/// no verdict"). Split out so the rule can be held to a stated list instead of to whatever the
/// test machine happens to have registered.
fn resolve_against(preference: Option<&ShellChoice>, known: Option<&[String]>) -> ShellChoice {
    match preference {
        Some(choice) if choice_is_intact(choice, known) => choice.clone(),
        _ => ShellChoice::program(crate::pty::resolve_shell()),
    }
}

/// Whether a stored choice still names a shell this machine can OPEN — the program AND what its
/// arguments point at.
///
/// `program_exists` alone was the whole check, and it is only half the pair (review
/// `01M0BT2SST6ZMG84SW1CW9BSWH`). A WSL preference is stored as `wsl.exe` + `-d <distro>`, and
/// `wsl --unregister <distro>` leaves `wsl.exe` exactly where it was: the path check passed, the
/// preference won, and `wsl.exe -d <distro>` answered "There is no distribution with the supplied
/// name" and exited — a terminal that died the instant it opened. The args are precisely why
/// [`ShellChoice`] carries a pair, and they were the half nothing verified.
fn choice_is_intact(choice: &ShellChoice, known: Option<&[String]>) -> bool {
    program_exists(&choice.path) && target_is_present(choice, known)
}

/// Whether what a choice's ARGUMENTS point at still exists.
///
/// `true` for every plain executable — a shell with no arguments has no target beyond its path,
/// which [`program_exists`] already covers. Only a WSL invocation has one, and only then is
/// `known` consulted.
///
/// `known = None` is "nothing has been probed yet", and it yields NO VETO: the check may discard a
/// preference the machine has DENIED, never one it has simply not been asked about. Silently
/// dropping a user's chosen shell because an answer had not landed yet would trade a rare bug for
/// a common one.
fn target_is_present(choice: &ShellChoice, known: Option<&[String]>) -> bool {
    let Some(distro) = wsl_target(choice) else {
        return true;
    };
    let Some(known) = known else {
        return true;
    };
    known.iter().any(|name| name.eq_ignore_ascii_case(distro))
}

/// The WSL DISTRIBUTION a choice names, or `None` when the choice is a plain executable.
///
/// Recognised on the program being `wsl` (with or without `.exe`, at any path) plus the flag
/// [`detect`] writes, `-d` — `--distribution` is accepted too, since it is the same flag spelled
/// long and a stored preference outlives the code that wrote it.
fn wsl_target(choice: &ShellChoice) -> Option<&str> {
    let program = std::path::Path::new(&choice.path)
        .file_stem()
        .and_then(|stem| stem.to_str())?;
    if !program.eq_ignore_ascii_case("wsl") {
        return None;
    }
    let mut args = choice.args.iter();
    while let Some(arg) = args.next() {
        if arg.eq_ignore_ascii_case("-d") || arg.eq_ignore_ascii_case("--distribution") {
            return args.next().map(String::as_str);
        }
    }
    None
}

/// What this machine last reported as its WSL distributions, or `None` when nothing has been
/// probed yet (and always, off Windows — there is no WSL to contradict a preference).
///
/// READS the cache and kicks a refresh, exactly as [`detect`] does, so a distribution registered
/// or unregistered since the last probe is reflected from the next resolution on. It NEVER waits:
/// this runs on the spawn path, which is the sidecar's dispatch thread (the criterion of review
/// `01M0BT262WB7VZQKHQZHEP281T`), so a stale answer is the price of not freezing a terminal open.
/// The refresh is only ever kicked for a preference that IS a WSL pair — a user who picked a plain
/// shell never starts a `wsl.exe` from here.
#[cfg(windows)]
fn live_distributions() -> Option<Vec<String>> {
    WSL_DISTRIBUTIONS.read_and_refresh(probe_wsl_distributions)
}

#[cfg(not(windows))]
fn live_distributions() -> Option<Vec<String>> {
    None
}

/// The spawn-path entry point: read the preference from `conn` and resolve it.
///
/// Takes the DB error the same way it takes a missing preference — as "nothing chosen" — because
/// the caller is about to open a terminal and no database problem is a reason to refuse one.
pub fn resolve_for_spawn(conn: &mut SqliteConnection) -> ShellChoice {
    let stored = preference(conn).ok().flatten();
    resolve(stored.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every DETECTED shell is spawnable: its program exists on disk (or on `PATH`), and its
    /// identity/label are non-empty so the UI can key and render it. This is the list's whole
    /// contract — "no absent binary is ever offered" — asserted on the REAL machine the tests run
    /// on, whichever OS that is.
    #[test]
    fn detection_only_returns_shells_that_exist() {
        for shell in detect() {
            assert!(
                program_exists(&shell.path),
                "detected shell {shell:?} must exist on disk"
            );
            assert!(!shell.id.is_empty(), "detected shell {shell:?} needs an id");
            assert!(
                !shell.label.is_empty(),
                "detected shell {shell:?} needs a label"
            );
        }
    }

    /// The detected list carries no duplicate identity, so the UI's `<select>` has one option per
    /// shell (a `PATH` that lists a directory twice must not double an entry).
    #[test]
    fn detection_has_no_duplicate_ids() {
        let shells = detect();
        let mut ids: Vec<&str> = shells.iter().map(|s| s.id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate ids in {shells:?}");
    }

    /// Per-OS shape of the detection, asserted on what the platform GUARANTEES rather than on what
    /// the CI image happens to install: Windows always has a Command Prompt, and any Unix entry
    /// comes from `/etc/shells` (so it is an absolute path). Anything beyond that is machine
    /// -dependent and is covered by the "everything listed exists" test above.
    #[test]
    fn detection_matches_the_platform() {
        let shells = detect();
        if cfg!(windows) {
            assert!(
                shells.iter().any(|s| s.id == "cmd"),
                "Windows always offers a Command Prompt: {shells:?}"
            );
        } else {
            for shell in &shells {
                assert!(
                    shell.path.starts_with('/'),
                    "a /etc/shells entry is an absolute path: {shell:?}"
                );
                assert!(
                    shell.args.is_empty(),
                    "a Unix shell is a plain executable: {shell:?}"
                );
            }
        }
    }

    /// The WSL enumeration drops Docker Desktop's SERVICE distributions and keeps every real one —
    /// parity with Windows Terminal's profile menu, which hides exactly these (review finding
    /// `01M083PEKSTN0JJ7M8E0KN8GEF`). Held to a CAPTURED `wsl.exe --list --quiet` output — the one
    /// this machine really prints, `\r`-terminated lines and all — so the filter is covered without
    /// a WSL install and on any OS.
    #[test]
    fn wsl_enumeration_drops_docker_desktop_service_distributions() {
        let captured = "docker-desktop\r\nUbuntu\r\ndocker-desktop-data\r\n";
        assert_eq!(
            parse_wsl_distributions(captured),
            vec!["Ubuntu".to_string()],
            "docker-desktop / docker-desktop-data are service VMs, not shells"
        );

        // A machine WITHOUT Docker Desktop is untouched, and the order WSL reports is preserved.
        assert_eq!(
            parse_wsl_distributions("Ubuntu\r\nDebian\r\n"),
            vec!["Ubuntu".to_string(), "Debian".to_string()]
        );

        // The filter is on the NAME, not on a prefix: a distribution the user named after Docker
        // is theirs and stays listed.
        assert_eq!(
            parse_wsl_distributions("docker-desktop-clone\r\n"),
            vec!["docker-desktop-clone".to_string()]
        );

        // Blank lines (WSL pads its output) are not distributions.
        assert!(parse_wsl_distributions("\r\n\r\n").is_empty());

        // A byte-order mark surviving the UTF-16 decode is NOT part of the first name. It is the
        // only padding `trim` cannot remove (`U+FEFF` is not whitespace), and left in place it
        // would carry a service VM straight past the name comparison and into the menu.
        assert_eq!(
            parse_wsl_distributions("\u{feff}docker-desktop\r\nUbuntu\r\n"),
            vec!["Ubuntu".to_string()],
            "a BOM must not smuggle a service distribution past the filter"
        );
        assert_eq!(
            parse_wsl_distributions("\u{feff}Ubuntu\r\n"),
            vec!["Ubuntu".to_string()],
            "a BOM is not part of the distribution's name"
        );
    }

    /// The list the UI renders carries no Docker Desktop service VM either — the filter is at the
    /// detection site, so it holds through `detect()` on a machine that has them.
    #[test]
    fn detection_never_offers_a_docker_desktop_service_vm() {
        for shell in detect() {
            for service in WSL_SERVICE_DISTRIBUTIONS {
                assert_ne!(
                    shell.id,
                    format!("wsl:{service}"),
                    "a Docker Desktop service VM reached the shell list: {shell:?}"
                );
            }
        }
    }

    /// NO preference → the historical resolution, unchanged: exactly what
    /// [`crate::pty::resolve_shell`] returns (`$SHELL`, then the per-OS default), with no
    /// arguments. This is the guarantee that an install which never opens the setting behaves as it
    /// did before the setting existed.
    #[test]
    fn resolution_without_preference_is_the_historical_default() {
        let resolved = resolve(None);
        assert_eq!(resolved.path, crate::pty::resolve_shell());
        assert!(resolved.args.is_empty());
    }

    /// A preference whose binary EXISTS wins over `$SHELL` and the per-OS default — the point of
    /// the setting. The preferred program is a real file by construction (the test's own
    /// executable), so the assertion does not depend on which shells the machine has.
    #[test]
    fn resolution_prefers_an_existing_preference() {
        let real = std::env::current_exe().expect("the test binary is a real file");
        let choice = ShellChoice {
            path: real.to_string_lossy().into_owned(),
            args: vec!["-d".to_string(), "Ubuntu".to_string()],
        };
        let resolved = resolve(Some(&choice));
        assert_eq!(resolved, choice, "an existing preference is spawned as-is");
    }

    /// Review `01M0BT2SST6ZMG84SW1CW9BSWH` — THE ARGS ARE CHECKED, NOT JUST THE PATH.
    ///
    /// A WSL preference is `wsl.exe` + `-d <distro>`, and `wsl --unregister <distro>` leaves
    /// `wsl.exe` exactly where it was. `program_exists(path)` alone therefore still said yes, the
    /// preference still won, and the spawn ran a `wsl.exe` that printed "There is no distribution
    /// with the supplied name" and exited — a terminal dead the instant it opened. The pair is the
    /// unit that has to be valid, which is the whole reason [`ShellChoice`] carries one.
    ///
    /// Held to the pure rule with a STATED distribution list, so every branch is asserted on every
    /// OS instead of on whatever the test machine happens to have registered.
    #[test]
    fn a_wsl_pair_is_checked_against_the_machines_distributions_not_only_its_path() {
        let ubuntu = ShellChoice {
            path: "C:\\WINDOWS\\system32\\wsl.exe".to_string(),
            args: vec!["-d".to_string(), "Ubuntu".to_string()],
        };
        let registered = ["Ubuntu".to_string(), "Debian".to_string()];

        assert!(
            target_is_present(&ubuntu, Some(&registered)),
            "a registered distribution is a shell the machine can open"
        );
        assert!(
            !target_is_present(&ubuntu, Some(&["Debian".to_string()])),
            "after `wsl --unregister Ubuntu` the pair is dead, even though wsl.exe is not"
        );
        assert!(
            !target_is_present(&ubuntu, Some(&[])),
            "a machine with no distribution at all cannot open this preference either"
        );
        // NOT PROBED is not a verdict: the check may discard a preference the machine DENIED,
        // never one it was simply never asked about.
        assert!(
            target_is_present(&ubuntu, None),
            "nothing probed yet must not silently drop the user's chosen shell"
        );

        // WSL names are case-insensitive, and so is the flag's long spelling — a stored
        // preference outlives the code that wrote it.
        assert!(target_is_present(&ubuntu, Some(&["ubuntu".to_string()])));
        let long = ShellChoice {
            path: "wsl".to_string(),
            args: vec!["--distribution".to_string(), "Ubuntu".to_string()],
        };
        assert!(target_is_present(&long, Some(&registered)));
        assert!(!target_is_present(&long, Some(&["Debian".to_string()])));

        // A PLAIN shell has no target beyond its path: `program_exists` is the whole check, and
        // an empty distribution list must never veto it.
        assert!(target_is_present(&ShellChoice::program("bash"), Some(&[])));
        assert!(target_is_present(
            &ShellChoice::program("C:\\WINDOWS\\system32\\wsl.exe"),
            Some(&[])
        ));
        // …and neither does a `wsl.exe` invoked without `-d`, or with a `-d` naming nothing.
        assert!(target_is_present(
            &ShellChoice {
                path: "wsl.exe".to_string(),
                args: vec!["-d".to_string()],
            },
            Some(&[])
        ));
    }

    /// The same review, END TO END through [`resolve`]'s own rule: a preference whose distribution
    /// is gone loses to the historical chain, exactly as a preference whose binary is gone does.
    /// The path is a REAL `wsl.exe` where the machine has one, so `program_exists` genuinely
    /// passes and the fallback can only be the args' doing.
    #[test]
    fn resolution_falls_back_when_the_preferred_wsl_distribution_is_gone() {
        let Some(wsl) = on_path("wsl.exe") else {
            return; // No WSL here: `program_exists` would carry the fallback, proving nothing.
        };
        let gone = ShellChoice {
            path: wsl.to_string_lossy().into_owned(),
            args: vec!["-d".to_string(), "nyx-unregistered-distro".to_string()],
        };
        let registered = ["Ubuntu".to_string()];
        assert!(
            program_exists(&gone.path),
            "the PROGRAM is still installed — that is the trap"
        );
        assert_eq!(
            resolve_against(Some(&gone), Some(&registered)).path,
            crate::pty::resolve_shell(),
            "a dead pair falls back to the historical chain instead of failing the spawn"
        );

        // And the same pair against a machine that DOES have the distribution still wins.
        let alive = ShellChoice {
            path: gone.path.clone(),
            args: vec!["-d".to_string(), "Ubuntu".to_string()],
        };
        assert_eq!(
            resolve_against(Some(&alive), Some(&registered)),
            alive,
            "a live WSL preference is still spawned as-is"
        );
    }

    /// A preference pointing at a binary that is GONE falls back to the historical chain instead of
    /// being spawned — the difference between a terminal that opens on the wrong shell and one that
    /// does not open at all.
    #[test]
    fn resolution_falls_back_when_the_preferred_binary_is_missing() {
        let choice = ShellChoice::program(if cfg!(windows) {
            "C:\\nyx-does-not-exist\\ghost-shell.exe"
        } else {
            "/nyx-does-not-exist/ghost-shell"
        });
        let resolved = resolve(Some(&choice));
        assert_eq!(resolved.path, crate::pty::resolve_shell());
    }

    /// Review `01M0BT262WB7VZQKHQZHEP281T` — A SLOW PROBE NEVER HOLDS THE READER, AND NEVER STACKS.
    ///
    /// The two halves of the finding, measured on the exact machinery `shell_list` reads through
    /// (a `BackgroundCache` with a probe that is deliberately stuck):
    ///
    /// 1. **The reader is not blocked.** Twenty reads issued while the probe is wedged all answer
    ///    at once — the cost of a Settings open no longer lands on the dispatch thread that is also
    ///    serving every `pty_write`. The 250 ms ceiling is two orders of magnitude under the 3 s
    ///    the old inline `recv_timeout` charged, so this cannot pass by accident on a slow box.
    /// 2. **Nothing accumulates.** Those twenty reads spawned ONE worker between them, not twenty:
    ///    the old shape started a thread and a `wsl.exe` per call and abandoned both on timeout.
    ///
    /// And the refreshed value does land: once the probe is released, a subsequent read serves it.
    #[test]
    fn a_slow_probe_never_holds_the_reader_and_never_stacks_up() {
        use std::sync::atomic::AtomicUsize;
        use std::time::Instant;

        static CACHE: BackgroundCache<Vec<String>> = BackgroundCache::new();
        static ENTERED: AtomicUsize = AtomicUsize::new(0);
        static RELEASED: AtomicBool = AtomicBool::new(false);

        /// A probe that WEDGES until the test lets it go — standing in for `wsl.exe --list` on a
        /// machine whose LxssManager is cold, which is the case the finding is about.
        fn wedged_probe() -> Vec<String> {
            ENTERED.fetch_add(1, Ordering::SeqCst);
            while !RELEASED.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            vec!["Ubuntu".to_string()]
        }

        // A first read starts the probe and answers "nothing known yet" without waiting for it.
        assert_eq!(
            CACHE.read_and_refresh(wedged_probe),
            None,
            "a cold cache answers immediately; it does not wait for the probe"
        );
        // Wait for the worker to actually be INSIDE the probe, so the reads below are issued
        // against a genuinely in-flight, genuinely stuck refresh.
        let deadline = Instant::now() + Duration::from_secs(5);
        while ENTERED.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(ENTERED.load(Ordering::SeqCst), 1, "the probe is running");

        // Twenty more reads while it is wedged. Both halves are asserted on a DECISION, not on a
        // thread's timing: `refresh` says whether IT started a worker, so "one probe for twenty
        // reads" holds without waiting to see whether a nineteenth thread shows up late.
        let started = Instant::now();
        for _ in 0..20 {
            // (2) Nothing accumulates: not one of them starts a second probe (nor a second
            // `wsl.exe`), which is exactly what the old shape did once per Settings open.
            assert!(
                !CACHE.refresh(wedged_probe),
                "one refresh at a time: a read must not start a second probe"
            );
            // (1) The reader is served from the cache, never from the probe.
            assert_eq!(CACHE.read(), None);
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(250),
            "reads must not wait on the probe, took {elapsed:?}"
        );
        assert_eq!(
            ENTERED.load(Ordering::SeqCst),
            1,
            "and no second worker ever entered the probe"
        );

        // The answer still lands: released, the probe's result becomes the cache's.
        RELEASED.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = None;
        while Instant::now() < deadline {
            seen = CACHE.read();
            if seen.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            seen,
            Some(vec!["Ubuntu".to_string()]),
            "a completed refresh is what the next read serves"
        );
    }

    /// The same review's LEAK half, at the process level: a probe that overruns its deadline is
    /// KILLED AND REAPED, not abandoned.
    ///
    /// `Command::output()` — what the enumeration used — waits on the child's pipes with no bound,
    /// so the only thing the old code could do on timeout was walk away, leaving the `wsl.exe` and
    /// its worker thread behind ONE PER SETTINGS OPEN. Held here to a real long-running child:
    /// [`bounded_output`] must return `None` at the deadline (not minutes later), and `try_wait`
    /// must then report an exit status — which is only possible if the child was terminated AND
    /// reaped by the call.
    #[test]
    fn an_overrunning_probe_is_killed_and_reaped_not_abandoned() {
        use std::time::Instant;

        // A child that outlives any deadline this test would accept, spelled with a program the
        // platform itself guarantees (no `sh` on PATH assumption on Windows).
        let mut child = if cfg!(windows) {
            std::process::Command::new("ping")
                .args(["-n", "60", "127.0.0.1"])
                .stdout(std::process::Stdio::piped())
                .spawn()
        } else {
            std::process::Command::new("sleep")
                .arg("60")
                .stdout(std::process::Stdio::piped())
                .spawn()
        }
        .expect("the platform's own sleeper must be spawnable");

        let started = Instant::now();
        let out = bounded_output(&mut child, Duration::from_millis(200));
        let elapsed = started.elapsed();

        assert_eq!(
            out, None,
            "an overrun yields no output, like WSL being absent"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "the call must end AT the deadline, not with the child: {elapsed:?}"
        );
        assert!(
            matches!(child.try_wait(), Ok(Some(_))),
            "the overrun child must be dead and reaped when the call returns, not left running"
        );
    }

    /// THE LIST STAYS CORRECT through the asynchronous path — the third half of review
    /// `01M0BT262WB7VZQKHQZHEP281T`, asserted against the REAL `wsl.exe` of the machine the test
    /// runs on rather than a captured output (which
    /// [`wsl_enumeration_drops_docker_desktop_service_distributions`] already covers).
    ///
    /// The point is the SEAM, not the parser: moving the probe onto a worker must still land its
    /// answer in what [`detect`] returns, still filtered of Docker Desktop's service VMs. A machine
    /// with no `wsl.exe` has nothing to assert and says so by returning early; a machine where the
    /// enumeration fails or is killed reports an empty list, which the loops below read as
    /// "nothing to check" — so this is a real assertion where it can be one and never a flake
    /// where it cannot.
    #[test]
    #[cfg(windows)]
    fn the_warmed_cache_lands_this_machines_real_distributions_in_the_list() {
        use std::time::Instant;

        if on_path("wsl.exe").is_none() {
            return; // No WSL on this machine: nothing this test can hold anything to.
        }
        warm_detection();
        // The probe is asynchronous by construction; wait for it to LAND, never for a fixed delay.
        // Its own leash is WSL_ENUMERATION_TIMEOUT, so this deadline only has to outlast that.
        let deadline = Instant::now() + WSL_ENUMERATION_TIMEOUT + Duration::from_secs(7);
        while WSL_DISTRIBUTIONS.read().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let probed = WSL_DISTRIBUTIONS
            .read()
            .expect("the warm-up probe must land: it is bounded and killed at its own deadline");

        for service in WSL_SERVICE_DISTRIBUTIONS {
            assert!(
                !probed.iter().any(|name| name.eq_ignore_ascii_case(service)),
                "a Docker Desktop service VM survived the real enumeration: {probed:?}"
            );
        }
        let listed = detect();
        for distro in &probed {
            assert!(
                listed
                    .iter()
                    .any(|shell| shell.id == format!("wsl:{distro}")),
                "the warmed answer must reach the list: {distro} missing from {listed:?}"
            );
        }
    }

    /// A preference round-trips through the store, and clearing it returns the reader to "nothing
    /// chosen" — which [`resolve_for_spawn`] turns back into the historical default.
    #[test]
    fn preference_round_trips_and_clears() {
        let mut conn = db::open_in_memory();
        assert_eq!(preference(&mut conn).unwrap(), None, "nothing chosen yet");

        let choice = ShellChoice {
            path: "/usr/bin/zsh".to_string(),
            args: vec!["-l".to_string()],
        };
        set_preference(&mut conn, Some(&choice)).unwrap();
        assert_eq!(preference(&mut conn).unwrap(), Some(choice));

        set_preference(&mut conn, None).unwrap();
        assert_eq!(preference(&mut conn).unwrap(), None, "cleared = Auto");
        assert_eq!(
            resolve_for_spawn(&mut conn).path,
            crate::pty::resolve_shell(),
            "a cleared preference resolves to the historical default"
        );
    }

    /// A stored value that does not parse is read as "nothing chosen" rather than propagating an
    /// error into a spawn — the corrupt-DB half of the never-block-a-terminal rule.
    #[test]
    fn an_unparseable_stored_preference_reads_as_none() {
        let mut conn = db::open_in_memory();
        db::set_app_setting(&mut conn, db::SETTING_DEFAULT_SHELL, Some("not json")).unwrap();
        assert_eq!(preference(&mut conn).unwrap(), None);
        assert_eq!(
            resolve_for_spawn(&mut conn).path,
            crate::pty::resolve_shell()
        );
    }
}
