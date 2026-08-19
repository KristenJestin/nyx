//! nyx-sidecar — supervised Rust backend executable wrapping [`nyx_core`].
//!
//! In the V2 architecture (PDR-0006 / PDR-0007) the sidecar is a direct,
//! supervised CHILD of Electron main: it is not a daemon and does not outlive the
//! application. It speaks to the supervisor over `stdin`/`stdout` using the
//! length-prefixed MessagePack [`pipe`] transport; `stderr` carries only logs.
//!
//! Responsibilities wired here:
//!   * the supervised LIFECYCLE — handshake / ping / bounded shutdown, parent-death
//!     (Linux `PR_SET_PDEATHSIG` + portable stdin-EOF watch), descendant reaping
//!     (process group); see [`nyx_sidecar::lifecycle`].
//!   * the framed PIPE transport — u32-BE length-prefixed MessagePack frames,
//!     id-correlated request/response, unsolicited events, per-terminal
//!     backpressure, version-incompatibility detection at boot; see
//!     [`nyx_sidecar::pipe`].

use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use nyx_sidecar::lifecycle;
use nyx_sidecar::pipe::{self, codec::FrameDecoder, wire};
use nyx_sidecar::runtime::events::EventSink;
use nyx_sidecar::runtime::Runtime;

/// How long the bounded shutdown waits for the descendant group to drain on
/// `SIGTERM` before escalating to `SIGKILL`.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

fn main() {
    // Touch nyx-core so the link to the core is real and the build fails loudly if
    // the core ever stops compiling under the sidecar.
    let _ = nyx_core::pathnorm::normalize("/");

    // 1. Become a process-group leader so anything we later spawn shares our group
    //    and can be reaped as one unit.
    let pgid = lifecycle::become_group_leader();

    // 1b. Windows kill-with-parent: put ourselves in a Job Object with
    //     KILL_ON_JOB_CLOSE so every descendant we spawn is terminated with the sidecar
    //     (incl. abrupt parent-death). The Windows analog of the Unix process-group reaping;
    //     a no-op on Unix. Best-effort — the portable stdin-EOF watch is the baseline.
    if lifecycle::setup_windows_job_object() {
        eprintln!("[nyx-sidecar] Windows job object armed (kill-on-job-close)");
    }

    // 2. Install the SIGTERM reaper so that when parent-death fires (delivered as
    //    SIGTERM by the kernel) we tear down the descendant group instead of
    //    leaving orphans.
    lifecycle::install_term_reaper(pgid);

    // 3. Arm kernel parent-death (Linux). If the parent was already gone at arming
    //    time, exit immediately — we are an orphan and must not linger.
    if lifecycle::arm_parent_death() {
        eprintln!("[nyx-sidecar] parent already gone at startup; exiting");
        lifecycle::reap_group(pgid, SHUTDOWN_GRACE);
        std::process::exit(0);
    }

    eprintln!(
        "[nyx-sidecar] up (v{}, protocol v{}, pgid {})",
        env!("CARGO_PKG_VERSION"),
        wire::PROTOCOL_VERSION,
        pgid
    );

    // 4. Boot the RUNTIME (HHE): open + migrate the SQLite DB (PDR-0023). The runtime
    //    is the persistence + PTY + managed-command authority the sidecar now owns. A
    //    boot failure (bad data dir / migration error) is FATAL — we must not serve a
    //    broken schema — so we reap the group and exit non-zero with a readable log.
    let runtime = match Runtime::boot() {
        Ok(rt) => {
            eprintln!("[nyx-sidecar] runtime booted (SQLite migrated, runtime tables ready)");
            rt
        }
        Err(e) => {
            eprintln!("[nyx-sidecar] FATAL: runtime boot failed: {e}");
            lifecycle::reap_group(pgid, SHUTDOWN_GRACE);
            std::process::exit(1);
        }
    };

    let exit_code = run(pgid, runtime);
    std::process::exit(exit_code);
}

/// The framed control loop: read length-prefixed MessagePack frames from `stdin`,
/// dispatch requests, write framed responses to `stdout`. Returns the exit code.
///
/// * A `Shutdown` request -> framed ack, reap the group, exit 0 (clean shutdown).
/// * `stdin` EOF -> the supervisor closed the pipe (it died): reap and exit 0
///   (parent-death, portable path).
/// * A frame that fails to decode -> a structured `Error` response is framed back;
///   the loop continues (one bad frame does not kill the sidecar). A
///   version-incompatible frame yields a readable error at boot.
/// * An over-sized length prefix -> unrecoverable: log and exit non-zero.
fn run(pgid: i32, runtime: Runtime) -> i32 {
    let mut stdin = std::io::stdin();
    let mut decoder = FrameDecoder::new();
    let mut read_buf = [0u8; 64 * 1024];

    // ONE shared, serialized writer over stdout: the run loop sends responses through
    // it AND the runtime's event-emitting threads (PTY pumps, the command runner, the
    // MCP dispatcher) stream events through clones of it, so a response frame and a
    // concurrent binary output frame never tear on the wire.
    let events = EventSink::new(Box::new(std::io::stdout()));

    // Finish the event-driven boot (HHD): build the managed-command + MCP runtimes over
    // the shared pool + this sink, then run the boot restore (reprises).
    runtime.attach_events(&events);

    // WARM THE SHELL DISCOVERY (review `01M0BT262WB7VZQKHQZHEP281T`), before the run loop below
    // starts serving. `shell_list` is answered ON that loop, and the WSL half of the discovery is
    // the one genuinely slow question in it — `wsl.exe --list` starts LxssManager when the service
    // is cold, and can hang outright on a broken install. Asking it HERE, on `nyx_core::shells`'
    // own bounded worker, is what lets the command answer from a cache instead of freezing every
    // other request behind a probe: by the time a user can open Settings, the answer is already in.
    //
    // It lives in the BINARY's boot and not in `attach_events` on purpose: `attach_events` is also
    // how the unit tests build a runtime, and starting a `wsl.exe` per test harness is both waste
    // and a measured source of timing interference (it made the git-watch quiescence test ring).
    // The real boot happens once; that is where a one-shot warm-up belongs.
    //
    // Returns immediately (a no-op off Windows): nothing here waits on the probe.
    nyx_core::shells::warm_detection();

    // Watch the loop we are about to enter: it serves ONE request at a time, so it is the one
    // thread that cannot report its own freeze. Started before the first frame is read.
    spawn_stuck_request_watchdog();

    // Emit an UNSOLICITED `ready` event (sidecar -> UI) over the pipe before any
    // request arrives — demonstrating the events channel of the transport.
    events.log(
        "info",
        format!("sidecar ready (protocol v{})", wire::PROTOCOL_VERSION),
    );

    loop {
        let n = match stdin.read(&mut read_buf) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[nyx-sidecar] stdin read error: {e}; treating as parent gone");
                break;
            }
        };
        if n == 0 {
            eprintln!("[nyx-sidecar] stdin EOF (parent gone); shutting down");
            break;
        }
        decoder.push(&read_buf[..n]);

        loop {
            let payload = match decoder.next_frame() {
                Ok(Some(p)) => p,
                Ok(None) => break, // need more bytes
                Err(e) => {
                    eprintln!("[nyx-sidecar] fatal frame error: {e}; closing");
                    shutdown_and_reap(&runtime, pgid);
                    return 1;
                }
            };

            match dispatch(&payload, &runtime, &events) {
                Dispatch::Reply { frame, exit } => {
                    events.write_frame(&frame);
                    if exit {
                        eprintln!("[nyx-sidecar] shutdown requested; reaping group");
                        // Tear down every live PTY (kill its process group + join its
                        // pump) before reaping so nothing is orphaned past exit — inside a
                        // published watchdog window, so a freeze in it has a name.
                        shutdown_and_reap(&runtime, pgid);
                        return 0;
                    }
                }
                Dispatch::Ignore => {}
            }
        }
    }

    shutdown_and_reap(&runtime, pgid);
    0
}

enum Dispatch {
    Reply { frame: Vec<u8>, exit: bool },
    Ignore,
}

/// Whether EVERY request should be timed into the log (`NYX_SIDECAR_TRACE=1`), not just the
/// slow ones. Read once — the control loop asks per request and the env cannot change under
/// a running process.
fn trace_requests() -> bool {
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| {
        matches!(
            std::env::var("NYX_SIDECAR_TRACE").as_deref(),
            Ok("1") | Ok("true")
        )
    })
}

/// A request slower than this is LOGGED with its name and duration. The control loop serves
/// one request at a time, so anything that holds it this long is already delaying every later
/// request; the supervisor's own per-request deadline is 5 s, so a warning at 250 ms surfaces
/// the culprit well before it turns into a `request id=NNN timed out` with no name attached
/// (review `01M0A7QGBK43MZB10RF51QR6TS` — 46 consecutive anonymous timeouts).
const SLOW_REQUEST_LOG_THRESHOLD: Duration = Duration::from_millis(250);

/// How long a request may hold the control loop before the watchdog NAMES it from its own
/// thread, without waiting for it to come back.
///
/// The after-the-fact line above can only ever describe requests that RETURN, so on its own it
/// leaves the one case that matters most — a command that never hands the thread back — exactly
/// as anonymous as before (review `01M0AB3R53M22DJ2KJ3022DDG4`, finding
/// `01M0AB99QKRXA6AWYHKXRKGX92`). 2 s sits above any legitimately slow request the after-the-fact
/// line already covers and below the supervisor's own 5 s per-request deadline
/// (`apps/desktop/src/main/sidecar.ts`), so the culprit is named BEFORE the first
/// `request id=NNN timed out` reaches the log rather than after 46 of them.
const STUCK_REQUEST_REPORT_AFTER: Duration = Duration::from_secs(2);

/// How often the watchdog looks at the in-flight slot. Bounds how late a stuck request can be
/// named ([`STUCK_REQUEST_REPORT_AFTER`] + one poll) while costing an idle sidecar 4 wakeups per
/// second, each a mutex acquisition over an `Option`.
const STUCK_REQUEST_POLL: Duration = Duration::from_millis(250);

/// The request the control loop is serving RIGHT NOW — published BEFORE the call
/// ([`begin_request`]) and cleared after it ([`end_request`]), so a command that never returns
/// is still identifiable BY NAME while it blocks. `None` between requests.
static IN_FLIGHT: Mutex<Option<InFlight>> = Mutex::new(None);

/// The in-flight request's identity + how long it has held the loop.
struct InFlight {
    /// [`wire::Request::label`] of the request being served.
    label: String,
    /// Its correlation id — the SAME id the supervisor reports on a timeout, so the two logs
    /// can be lined up.
    id: u64,
    /// When the loop started serving it.
    since: Instant,
    /// Whether the watchdog has already named this one, so a request stuck for minutes is
    /// reported once rather than four times a second.
    named: bool,
}

/// Take the in-flight slot, IGNORING poisoning: a panic inside one request handler must not
/// silence the diagnostic for every later request (the lock only ever guards an `Option` that
/// the next `begin_request` overwrites wholesale, so there is no half-updated state to inherit).
fn in_flight_slot() -> std::sync::MutexGuard<'static, Option<InFlight>> {
    IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Publish the request the loop is about to serve.
fn begin_request(label: &str, id: u64) {
    *in_flight_slot() = Some(InFlight {
        label: label.to_string(),
        id,
        since: Instant::now(),
        named: false,
    });
}

/// Clear the in-flight slot — the loop is free again.
fn end_request() {
    *in_flight_slot() = None;
}

/// The pseudo-request labels the ordered TEARDOWN publishes into [`IN_FLIGHT`], so a freeze in
/// it is NAMED rather than anonymous. Two labels and not one, because the halves hang for
/// different reasons: [`Runtime::shutdown`] stops the managed commands and runs
/// `PtyManager::close_all` (the kill + pump-join sequence), while [`lifecycle::reap_group`]
/// signals and waits on the whole process group.
const SHUTDOWN_TEARDOWN_LABEL: &str = "shutdown-teardown";
const SHUTDOWN_REAP_LABEL: &str = "shutdown-reap-group";

/// The id published with a teardown pseudo-request. A real request carries the supervisor's id;
/// the teardown answers to nobody, so it uses a reserved `0` and is recognised by its LABEL.
const TEARDOWN_REQUEST_ID: u64 = 0;

/// Run `f` inside a PUBLISHED watchdog window, so a freeze inside it is named by
/// [`spawn_stuck_request_watchdog`] exactly as a stuck request would be.
fn under_watch<T>(label: &str, f: impl FnOnce() -> T) -> T {
    begin_request(label, TEARDOWN_REQUEST_ID);
    let out = f();
    end_request();
    out
}

/// The ordered TEARDOWN — run under the watchdog's eye.
///
/// [`dispatch`] clears the in-flight slot BEFORE returning `Dispatch::Reply { exit: true }`, so
/// the real work of a shutdown used to happen outside every published window: `Runtime::shutdown`
/// — which stops the managed commands and calls `PtyManager::close_all`, i.e. exactly the kill +
/// pump-join sequence this instrumentation was added for — and then `lifecycle::reap_group`. A
/// freeze there left [`IN_FLIGHT`] empty and the watchdog silent: an ANONYMOUS hang in the log,
/// the very symptom the watchdog exists to remove, in the one place it could not see (review
/// `01M0AEPH1N5SHVJWAG6HHJ3MCJ`, finding `01M0AEQYC5PEMH1EZ70W5DEJBT`).
///
/// Nothing about the shutdown's BEHAVIOUR changes — the supervisor still force-kills after its
/// own grace, so the app never stays wedged either way. What this buys is the diagnostic, which
/// is the entire deliverable of the mechanism.
fn shutdown_and_reap(runtime: &Runtime, pgid: i32) {
    under_watch(SHUTDOWN_TEARDOWN_LABEL, || runtime.shutdown());
    under_watch(SHUTDOWN_REAP_LABEL, || {
        lifecycle::reap_group(pgid, SHUTDOWN_GRACE)
    });
}

/// Whether an in-flight request should be NAMED now, latching `named` so a request stuck for
/// minutes is reported ONCE rather than on every poll. Split out of the watchdog thread so the
/// latch and the threshold can be exercised without waiting on a real clock.
fn take_stuck_report(f: &mut InFlight, elapsed: Duration) -> bool {
    if f.named || elapsed < STUCK_REQUEST_REPORT_AFTER {
        return false;
    }
    f.named = true;
    true
}

/// Start the watchdog that names a request STILL HOLDING the control loop past
/// [`STUCK_REQUEST_REPORT_AFTER`].
///
/// It runs on its OWN thread precisely because the loop it watches is single-threaded: a command
/// that froze it cannot log anything itself, which is why the dogfood session that motivated this
/// (46 consecutive `request id=NNN timed out`, sidecar still `ready`) had nothing in the sidecar's
/// log to read. Detached on purpose — it has no teardown state and the process exits under it.
///
/// COVERS THE ORDERED TEARDOWN TOO. Its window is not limited to requests: [`shutdown_and_reap`]
/// republishes one around `Runtime::shutdown` and `lifecycle::reap_group`, which run AFTER
/// [`dispatch`] has already freed the slot. Those two are the kill + pump-join sequence and the
/// group reap — a hang there is exactly the class this thread exists to name, and it would
/// otherwise be the one place it could not see.
fn spawn_stuck_request_watchdog() {
    let _ = std::thread::Builder::new()
        .name("stuck-request-watchdog".into())
        .spawn(|| loop {
            std::thread::sleep(STUCK_REQUEST_POLL);
            let mut slot = in_flight_slot();
            if let Some(f) = slot.as_mut() {
                if take_stuck_report(f, f.since.elapsed()) {
                    eprintln!(
                        "[nyx-sidecar] request {} id={} STILL RUNNING after {}ms — it is holding \
                         the control loop; every later request will time out until it returns",
                        f.label,
                        f.id,
                        f.since.elapsed().as_millis()
                    );
                }
            }
        });
}

/// Decode one frame payload, dispatch it (routing stateless lifecycle requests vs PTY
/// requests onto the runtime), and produce the framed reply (if any).
///
/// TIMING, in two halves — because a request that FREEZES the loop and one that is merely SLOW
/// need different instruments:
///   * BEFORE the call the request's name + id are published into [`IN_FLIGHT`], where the
///     [`spawn_stuck_request_watchdog`] thread reads them: a command that never returns is named
///     in the log within [`STUCK_REQUEST_REPORT_AFTER`], while it is still stuck. Under
///     `NYX_SIDECAR_TRACE=1` a `begin` line is written there too, so the trace shows entries with
///     no matching exit.
///   * AFTER it returns, a request past [`SLOW_REQUEST_LOG_THRESHOLD`] is logged as
///     `[nyx-sidecar] request <name> id=<id> took <ms>ms` (every request under the trace flag).
///
/// Together they are what turns the supervisor's `request id=NNN timed out` — which names no
/// command — into a named culprit.
fn dispatch(payload: &[u8], runtime: &Runtime, events: &EventSink) -> Dispatch {
    use nyx_sidecar::pipe::Handled;
    match wire::decode_message(payload) {
        Ok(wire::Message::Request(req)) => {
            let started = Instant::now();
            // PUBLISH FIRST: everything below this line may fail to hand the thread back, and
            // from here the watchdog can name it without the loop's cooperation.
            begin_request(req.label(), req.id());
            if trace_requests() {
                eprintln!(
                    "[nyx-sidecar] request {} id={} begin",
                    req.label(),
                    req.id()
                );
            }
            let (resp, exit) = match pipe::handle_request_stateless(&req) {
                Handled::Reply { response, exit } => (response, exit),
                // A PTY op: drive the runtime's live PTY manager. The spawned pump
                // streams binary output / cwd / exec-state / exit through `events`.
                Handled::NeedsRuntime => (runtime.dispatch_runtime(&req, events), false),
            };
            let elapsed = started.elapsed();
            end_request();
            if elapsed >= SLOW_REQUEST_LOG_THRESHOLD || trace_requests() {
                eprintln!(
                    "[nyx-sidecar] request {} id={} took {}ms",
                    req.label(),
                    req.id(),
                    elapsed.as_millis()
                );
            }
            let frame = wire::encode_message_frame(&wire::Message::Response(resp))
                .expect("response frame within size bound");
            Dispatch::Reply { frame, exit }
        }
        // Responses/events FROM the supervisor are not acted on in this phase.
        Ok(_) => Dispatch::Ignore,
        Err(wire::DecodeError::VersionMismatch { theirs, ours }) => {
            // Readable version-incompatibility error at boot (the supervisor's
            // first frame stamps an incompatible version).
            let msg = wire::Message::Response(wire::Response::Error {
                id: 0,
                code: wire::ErrorCode::VersionIncompatible,
                message: format!(
                    "protocol version mismatch: supervisor speaks v{theirs}, sidecar speaks v{ours}"
                ),
            });
            let frame = wire::encode_message_frame(&msg).expect("error frame within bound");
            Dispatch::Reply { frame, exit: false }
        }
        Err(e) => {
            let msg = wire::Message::Response(wire::Response::Error {
                id: 0,
                code: wire::ErrorCode::MalformedRequest,
                message: format!("could not decode message: {e}"),
            });
            let frame = wire::encode_message_frame(&msg).expect("error frame within bound");
            Dispatch::Reply { frame, exit: false }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The diagnostic the dogfood freeze lacked: the loop's CURRENT request is readable from
    /// another thread while it runs, and the watchdog names it exactly once, only past the
    /// threshold (review `01M0AB3R53M22DJ2KJ3022DDG4`, finding `01M0AB99QKRXA6AWYHKXRKGX92`) —
    /// and the ORDERED TEARDOWN, which runs after the request slot is freed, is inside a
    /// published window too (finding `01M0AEQYC5PEMH1EZ70W5DEJBT`).
    ///
    /// One test, not four: [`IN_FLIGHT`] is process-wide, so tests splitting this between them
    /// would race each other inside the same test binary.
    #[test]
    fn a_request_in_flight_is_named_while_it_runs_and_reported_once() {
        // While it is being served, the slot names it — this is what the watchdog reads, and
        // what nothing could answer while the log only spoke after the call returned.
        begin_request("pty_close", 4242);
        {
            let mut slot = in_flight_slot();
            let f = slot.as_mut().expect("a request is in flight");
            assert_eq!(f.label, "pty_close");
            assert_eq!(f.id, 4242);

            // Under the threshold nothing is said: a request that is merely slow is the
            // after-the-fact timing line's job.
            assert!(
                !take_stuck_report(f, STUCK_REQUEST_REPORT_AFTER - Duration::from_millis(1)),
                "a request under the threshold must not be reported as stuck"
            );
            // Past it, named — once, however long it goes on holding the loop.
            assert!(
                take_stuck_report(f, STUCK_REQUEST_REPORT_AFTER),
                "a request holding the loop past the threshold must be named"
            );
            assert!(
                !take_stuck_report(f, STUCK_REQUEST_REPORT_AFTER * 60),
                "the same stuck request must be named once, not on every poll"
            );
        }

        // …and the slot is free again once the request returns, so the next one is not blamed
        // for its predecessor.
        end_request();
        assert!(
            in_flight_slot().is_none(),
            "the in-flight slot must be cleared when the loop is free"
        );

        // THE TEARDOWN IS WATCHED TOO. `dispatch` frees the slot before the loop runs
        // `Runtime::shutdown` + `lifecycle::reap_group`, so a freeze in that kill + pump-join
        // sequence used to be anonymous. `under_watch` republishes a window around it: what is
        // asserted is that the label is readable FROM INSIDE the teardown — which is precisely
        // what the watchdog thread reads — and that the window closes when it returns.
        let seen = under_watch(SHUTDOWN_TEARDOWN_LABEL, || {
            in_flight_slot().as_ref().map(|f| (f.label.clone(), f.id))
        });
        assert_eq!(
            seen,
            Some((SHUTDOWN_TEARDOWN_LABEL.to_string(), TEARDOWN_REQUEST_ID)),
            "a freeze inside the shutdown teardown must be nameable — the window has to be \
             published AROUND it, not cleared before it"
        );
        assert!(
            in_flight_slot().is_none(),
            "the teardown window closes when the teardown returns"
        );
    }
}
