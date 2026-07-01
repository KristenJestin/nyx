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
use std::time::Duration;

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
                    runtime.shutdown();
                    lifecycle::reap_group(pgid, SHUTDOWN_GRACE);
                    return 1;
                }
            };

            match dispatch(&payload, &runtime, &events) {
                Dispatch::Reply { frame, exit } => {
                    events.write_frame(&frame);
                    if exit {
                        eprintln!("[nyx-sidecar] shutdown requested; reaping group");
                        // Tear down every live PTY (kill its process group + join its
                        // pump) before reaping so nothing is orphaned past exit.
                        runtime.shutdown();
                        lifecycle::reap_group(pgid, SHUTDOWN_GRACE);
                        return 0;
                    }
                }
                Dispatch::Ignore => {}
            }
        }
    }

    runtime.shutdown();
    lifecycle::reap_group(pgid, SHUTDOWN_GRACE);
    0
}

enum Dispatch {
    Reply { frame: Vec<u8>, exit: bool },
    Ignore,
}

/// Decode one frame payload, dispatch it (routing stateless lifecycle requests vs PTY
/// requests onto the runtime), and produce the framed reply (if any).
fn dispatch(payload: &[u8], runtime: &Runtime, events: &EventSink) -> Dispatch {
    use nyx_sidecar::pipe::Handled;
    match wire::decode_message(payload) {
        Ok(wire::Message::Request(req)) => {
            let (resp, exit) = match pipe::handle_request_stateless(&req) {
                Handled::Reply { response, exit } => (response, exit),
                // A PTY op: drive the runtime's live PTY manager. The spawned pump
                // streams binary output / cwd / exec-state / exit through `events`.
                Handled::NeedsRuntime => (runtime.dispatch_runtime(&req, events), false),
            };
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
