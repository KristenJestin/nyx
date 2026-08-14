//! SPIKE — v2.8 Phase 1, PRD 01KY4DQS06B939Z1CM8CPE7TW0, task 01KY71HYTSHJCFHGSWGEE1FWR8.
//!
//! The *juge de paix* of the v2.8 engine PRD: PROVE, in ONE session, the core runtime loop
//!
//!     nyx_next  →  spawn(coder via NATIVE sub-agent)  →  gate(typecheck)
//!               →  branch(red = re-spawn / green = advance)  →  re-spawn …
//!
//! with **no external session spawn** (`claude -p` is proscribed by design — pricing), plus
//! **reliable detection of the native sub-agent's end** and **extraction of its result (a
//! diff)**. The loop must be **provider-agnostic** (enforcement lives in the verb handler,
//! not in a provider hook), so it is exercised through TWO sub-agent spawners: a native
//! `Task`-style one (Claude) and an inline fallback (a provider with no native sub-agents,
//! e.g. Codex/OpenCode) — the graph unrolls identically for both.
//!
//! This is a SPIKE: the behaviour-tree *interpreter* here is deliberately the minimal state
//! machine for the P1 loop, NOT the general node interpreter (that is Phase 2). What it
//! de-risks is REAL nyx-core capability, exercised through the exact primitives the engine
//! will use in production:
//!
//!   * **The gate is run by nyx itself, over the PTY** — [`nyx_core::command::CommandPty`]
//!     (`spawn` + blocking `wait()` → exit code). Exit 0 = green, non-zero = red. This is
//!     the "nyx a le pty, exécute le gate_check LUI-MÊME et bloque tant que rouge" property.
//!   * **The native sub-agent's end is detected by nyx's real activity machine** —
//!     [`nyx_core::agent_activity::AgentActivityStore`], fed the exact `SubagentStarted` /
//!     `SubagentFinished` events the Claude plugin hooks emit. While a native sub-agent is in
//!     flight the store reads `Working`; when it ends the store settles to `Idle`. This is
//!     the Claude-native *comfort* signal.
//!   * **The result is extracted as a real git diff** of the sandbox the sub-agent edited
//!     (`git -C <dir> diff`), via [`nyx_core::proc_util::command`].
//!
//! The UNIVERSAL end-detection — the one that makes the loop cross-provider *without any
//! hook* — is structural: the thin dispatcher's next `nyx_next(run, last_result)` call IS the
//! end signal, and `last_result` carries the extracted diff. The `AgentActivityStore` is an
//! extra, Claude-only convenience layered on top; the inline-fallback path proves the loop
//! completes with that convenience absent (the PRD's "un provider sans ces hooks perd juste
//! le confort, pas l'enforcement").

use std::path::{Path, PathBuf};

use nyx_core::agent_activity::{Activity, ActivityEvent, AgentActivityStore};
use nyx_core::command::CommandPty;
use portable_pty::PtySize;

/// The gate is `node --check <file>` — a real, fast parse gate over a CommonJS file (Node's
/// ESM-detection makes `--check` lenient on `export`, so the sandbox is CommonJS: a genuine
/// syntax error is reliably exit 1). These three source states drive the loop:
///   * [`SRC_BROKEN`]     — the initial red state (a syntax error).
///   * [`SRC_UNDER_FIX`]  — a coder attempt that is STILL broken (keeps the gate red → forces
///     a real re-spawn).
///   * [`SRC_GREEN`]      — the valid fix (gate green → advance).
const SRC_BROKEN: &str = "function add(a, b) {\n  return a + ;\n}\nmodule.exports = { add };\n";
const SRC_UNDER_FIX: &str =
    "function add(a, b) {\n  return a + // still broken\n}\nmodule.exports = { add };\n";
const SRC_GREEN: &str = "function add(a, b) {\n  return a + b;\n}\nmodule.exports = { add };\n";

// ===========================================================================================
// Test scaffolding — an on-disk git sandbox the "coder" edits, and the real PTY gate.
// ===========================================================================================

/// RAII temp dir removed on drop (same pattern as the crate's gate e2e tests).
struct TempDir(PathBuf);
impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "nyx-v2-8-spike-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// Run a plain (non-PTY) helper process to completion, returning `(exit_ok, stdout)`. Used
/// only for git sandbox plumbing — NOT the gate (the gate goes through the real PTY runner).
fn run(program: &str, args: &[&str], cwd: &Path) -> (bool, String) {
    let out = nyx_core::proc_util::command(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("spawn {program}: {e}"));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

/// A git sandbox holding one JS source file with an initial **syntax error** (the stand-in
/// for a red typecheck). Returns the temp dir guard + the source file path.
fn make_sandbox() -> (TempDir, PathBuf) {
    let tmp = temp_dir("sandbox");
    let dir = tmp.path().to_path_buf();
    run("git", &["init", "-q"], &dir);
    run("git", &["config", "user.email", "spike@nyx"], &dir);
    run("git", &["config", "user.name", "spike"], &dir);
    let src = dir.join("impl.js");
    // Deliberately BROKEN: `return a + ;` is a syntax error → `node --check` exits non-zero.
    std::fs::write(&src, SRC_BROKEN).unwrap();
    run("git", &["add", "-A"], &dir);
    run("git", &["commit", "-q", "-m", "initial (broken)"], &dir);
    (tmp, src)
}

/// THE GATE, run the way nyx runs it in production: over a real PTY via
/// [`CommandPty`], blocking on `wait()` for the exit code. `node --check <file>` is a real,
/// fast syntax/parse gate (a typecheck stand-in) — exit 0 = green, non-zero = red.
///
/// Returns `true` when GREEN (exit 0). This function is the whole of "nyx exécute le
/// gate_check LUI-MÊME (il a le pty)".
fn gate_is_green(src: &Path) -> bool {
    let cmdline = format!("node --check {}", src.display());
    let (mut pty, rx) = CommandPty::spawn(
        &cmdline,
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        src.parent().and_then(|p| p.to_str()),
        &[],
    )
    .expect("spawn gate PTY");
    // Drain output so the reader thread runs to EOF (channel is unbounded — no backpressure).
    while rx.recv().is_ok() {}
    pty.wait() == Some(0)
}

/// Extract the sub-agent's RESULT as a real diff of the sandbox working tree.
fn extract_diff(dir: &Path) -> String {
    run("git", &["--no-pager", "diff"], dir).1
}

// ===========================================================================================
// The provider seam — how a native sub-agent is spawned. Two flavours, ONE loop.
// ===========================================================================================

/// The abstract "spawn a sub-agent to do the coder's work" seam. In production this is the
/// per-provider adapter: Claude's native `Task`, or an inline fallback where the same session
/// does the work. Here each flavour APPLIES the scripted edit for the current attempt to the
/// sandbox (that is the coder's output), and the loop extracts the diff afterwards.
trait SubagentSpawner {
    /// Human label of the provider mechanism (for the trace + assertions).
    fn mechanism(&self) -> &'static str;

    /// Whether this provider emits the native activity hooks (Claude does; a bare inline
    /// provider does not). Drives whether the loop can rely on the `AgentActivityStore`
    /// *comfort* signal — but NEVER on the enforcement, which is hook-free.
    fn emits_activity_hooks(&self) -> bool;

    /// Run the coder sub-agent for `attempt` (0-based): apply that attempt's edit to `src`.
    /// Returns nothing — the loop reads the result back as a git diff (result extraction).
    fn run_coder(&self, attempt: usize, src: &Path);
}

/// Native `Task`-style spawner (Claude): a real native sub-agent that also drives nyx's
/// activity machine via the plugin hooks. `edits[attempt]` is the file content the coder
/// writes on that attempt.
struct NativeTaskSpawner {
    edits: Vec<&'static str>,
}
impl SubagentSpawner for NativeTaskSpawner {
    fn mechanism(&self) -> &'static str {
        "claude-native-task"
    }
    fn emits_activity_hooks(&self) -> bool {
        true
    }
    fn run_coder(&self, attempt: usize, src: &Path) {
        std::fs::write(src, self.edits[attempt]).unwrap();
    }
}

/// Inline-fallback spawner (a provider with NO native sub-agents — Codex/OpenCode degrade to
/// inline): the same session does the coder's edit, and it emits NO activity hooks. The loop
/// must still unroll to green.
struct InlineFallbackSpawner {
    edits: Vec<&'static str>,
}
impl SubagentSpawner for InlineFallbackSpawner {
    fn mechanism(&self) -> &'static str {
        "inline-fallback"
    }
    fn emits_activity_hooks(&self) -> bool {
        false
    }
    fn run_coder(&self, attempt: usize, src: &Path) {
        std::fs::write(src, self.edits[attempt]).unwrap();
    }
}

// ===========================================================================================
// The engine — the minimal `nyx_next` tick for the P1 loop.
//
//     loop { spawn_subagent(coder) → gate_check(typecheck) → selector(red↺ / green→) }
//     → transition(advance)
//
// The dispatcher is thin: it only ever calls `next()`, and if the action is `Spawn` it runs
// the native sub-agent and reports the extracted diff back on the NEXT `next()` call. nyx
// owns the state, runs the gate ITSELF inside `next()`, and refuses to emit `Advance` while
// the gate is red.
// ===========================================================================================

/// The atomic action `nyx_next` hands back to the thin dispatcher.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// Run the coder as a native sub-agent, then call `next(Some(diff))`.
    Spawn { agent: String, attempt: usize },
    /// The gate is green — the graph advanced past the loop to its transition.
    Advance { to: String },
}

/// The blackboard nyx keeps for the run (`workflow_runs.blackboard` in the real schema).
#[derive(Debug, Default, Clone)]
struct Blackboard {
    /// Number of coder sub-agents spawned so far (attempts).
    spawns: usize,
    /// Verdict of the last gate nyx ran (`None` before the first gate).
    last_gate_green: Option<bool>,
    /// The last extracted diff (the sub-agent's result).
    last_diff: String,
    /// Full ordered trace of actions nyx emitted (for the assertions).
    trace: Vec<Action>,
}

/// Where the tick currently sits in the P1 micro-workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cursor {
    /// At the `spawn_subagent(coder)` leaf — the next tick emits `Spawn`.
    Spawn,
    /// The dispatcher owes us a sub-agent result; the next tick runs the gate + branches.
    AwaitingResult,
    /// The loop is done (gate went green); the next tick emits `Advance`.
    Advanced,
}

/// The single-run engine. `next()` is the whole `nyx_next` verb handler for the P1 loop.
struct Engine {
    cursor: Cursor,
    board: Blackboard,
    /// Safety bound so a never-green gate cannot spin forever (a real loop node carries an
    /// `until`/max-iterations guard).
    max_spawns: usize,
    src: PathBuf,
}

impl Engine {
    fn new(src: PathBuf) -> Self {
        Engine {
            cursor: Cursor::Spawn,
            board: Blackboard::default(),
            max_spawns: 8,
            src,
        }
    }

    /// THE TICK. `last_diff` is the result the dispatcher extracted from the sub-agent it was
    /// told to spawn (None on the very first tick). Returns the next atomic action.
    ///
    /// ENFORCEMENT INVARIANT (asserted by the tests): when the gate nyx runs is RED, this
    /// never returns `Advance` — it returns `Spawn` (re-spawn). The branch is decided by the
    /// gate nyx ran ITSELF, inside this handler, over the PTY.
    fn next(&mut self, last_diff: Option<String>) -> Action {
        match self.cursor {
            Cursor::Spawn => {
                let action = Action::Spawn {
                    agent: "coder".into(),
                    attempt: self.board.spawns,
                };
                self.board.spawns += 1;
                self.cursor = Cursor::AwaitingResult;
                self.board.trace.push(action.clone());
                action
            }
            Cursor::AwaitingResult => {
                // The sub-agent finished (the dispatcher called us back). Record its result.
                self.board.last_diff = last_diff.unwrap_or_default();
                // nyx RUNS THE GATE ITSELF, over the PTY, and BLOCKS on the verdict.
                let green = gate_is_green(&self.src);
                self.board.last_gate_green = Some(green);
                if green {
                    // selector → green branch → advance past the loop to the transition.
                    self.cursor = Cursor::Advanced;
                    self.next(None)
                } else if self.board.spawns >= self.max_spawns {
                    panic!("gate never went green within {} spawns", self.max_spawns);
                } else {
                    // selector → red branch → loop back to spawn_subagent (RE-SPAWN). nyx
                    // does NOT advance while red — enforcement in the handler, no hook.
                    self.cursor = Cursor::Spawn;
                    self.next(None)
                }
            }
            Cursor::Advanced => {
                let action = Action::Advance {
                    to: "review".into(),
                };
                self.board.trace.push(action.clone());
                action
            }
        }
    }
}

/// Drive the full single-session loop with a given spawner, returning the finished engine.
///
/// This function IS the thin dispatcher: `next()` → (if `Spawn`) run the native sub-agent +
/// extract the diff → `next(diff)`; repeat until `Advance`. It NEVER spawns an external
/// session (`claude -p`); the sub-agent runs in-session via the spawner seam.
fn drive_loop(spawner: &dyn SubagentSpawner, src: &Path, activity: &AgentActivityStore) -> Engine {
    let dir = src.parent().unwrap().to_path_buf();
    // The dispatcher's terminal record id — the key the activity store uses in production.
    let term = format!("term-{}", spawner.mechanism());
    let mut engine = Engine::new(src.to_path_buf());
    let mut last: Option<String> = None;

    loop {
        match engine.next(last.take()) {
            Action::Spawn { agent, attempt } => {
                assert_eq!(agent, "coder", "the P1 loop only ever spawns the coder");

                // --- NATIVE SUB-AGENT START -------------------------------------------------
                // Claude drives nyx's real activity machine via the SubagentStart hook; a
                // bare inline provider emits nothing (no hook).
                if spawner.emits_activity_hooks() {
                    let snap = activity.apply(&term, ActivityEvent::SubagentStarted);
                    assert_eq!(
                        snap.activity,
                        Activity::Working,
                        "while a native sub-agent is in flight nyx reads Working"
                    );
                }

                // The sub-agent does the coder's work (its edit lands in the sandbox).
                spawner.run_coder(attempt, src);

                // --- RESULT EXTRACTION (diff) + NATIVE SUB-AGENT END ------------------------
                let diff = extract_diff(&dir);
                assert!(
                    !diff.trim().is_empty(),
                    "the coder sub-agent produced a non-empty diff (extractable result)"
                );

                if spawner.emits_activity_hooks() {
                    // The dispatcher's main turn tries to END while the native sub-agent's
                    // stop is still pending. nyx does NOT settle: it reliably knows the
                    // sub-agent is still in flight (0-stale — the crux of reliable
                    // end-detection). This is the exact deferral the v2.5.5 spike proved.
                    let pending = activity.apply(&term, ActivityEvent::TurnFinished);
                    assert_eq!(
                        pending.activity,
                        Activity::Working,
                        "nyx will not settle while the native sub-agent is still in flight (0-stale)"
                    );
                    // The native sub-agent ENDS (its `SubagentStop`) — only NOW does nyx's
                    // real machine settle to Idle. Reliable, Claude-native end-detection.
                    let settled = activity.apply(&term, ActivityEvent::SubagentFinished);
                    assert_eq!(
                        settled.activity,
                        Activity::Idle,
                        "when the native sub-agent ends, nyx reliably settles to Idle"
                    );
                }

                // Report the result back — the UNIVERSAL, hook-free end signal.
                last = Some(diff);
            }
            Action::Advance { to } => {
                assert_eq!(to, "review");
                return engine;
            }
        }
    }
}

// ===========================================================================================
// THE PROOFS.
// ===========================================================================================

/// VOLET 1 — the core loop, driven by a NATIVE `Task`-style sub-agent (Claude), in ONE
/// session: spawn → gate → branch(red → RE-SPAWN) → spawn → gate → branch(green → advance).
///
/// Proves, over REAL nyx-core primitives:
///   * the loop chains spawn(native) → gate → branch → re-spawn with NO external session;
///   * a RED gate makes `nyx_next` re-spawn and NEVER advance (enforcement in the handler);
///   * the native sub-agent's end is reliably detected (the real `AgentActivityStore` goes
///     Working → Idle) and its result is extracted as a real git diff.
#[test]
fn nyx_next_drives_native_spawn_gate_branch_respawn_in_one_session() {
    let (tmp, src) = make_sandbox();
    let _tmp = tmp; // keep the sandbox alive for the whole test
    let activity = AgentActivityStore::new();

    // Attempt 0 = an UNDER-FIX that is STILL a syntax error (gate stays red → forces a real
    // re-spawn). Attempt 1 = a valid fix (gate goes green → advance).
    let spawner = NativeTaskSpawner {
        edits: vec![SRC_UNDER_FIX, SRC_GREEN],
    };

    let engine = drive_loop(&spawner, &src, &activity);

    // Exactly TWO native sub-agents were spawned: the first (red) FORCED a re-spawn.
    assert_eq!(
        engine.board.spawns, 2,
        "the red gate forced a re-spawn — two coder sub-agents ran in the one session"
    );

    // The action trace is precisely: Spawn(0) → Spawn(1) → Advance. A red gate NEVER yielded
    // an Advance between the two spawns (enforcement).
    assert_eq!(
        engine.board.trace,
        vec![
            Action::Spawn {
                agent: "coder".into(),
                attempt: 0
            },
            Action::Spawn {
                agent: "coder".into(),
                attempt: 1
            },
            Action::Advance {
                to: "review".into()
            },
        ],
        "spawn → (red) re-spawn → (green) advance — nyx_next never advanced on red"
    );

    // The final gate nyx ran itself was GREEN, and it advanced only then.
    assert_eq!(engine.board.last_gate_green, Some(true));
    // The extracted result (the last diff) reflects the coder's fix.
    assert!(
        engine.board.last_diff.contains("return a + b;"),
        "the extracted diff carries the coder's green fix"
    );

    // After the whole loop, the native activity machine is back to Idle (no phantom Working).
    assert_eq!(
        activity
            .snapshot(&format!("term-{}", spawner.mechanism()))
            .map(|s| s.activity)
            .unwrap_or(Activity::Idle),
        Activity::Idle,
        "the run ends with nyx's activity machine settled (native sub-agent end detected)"
    );
}

/// VOLET 2 — provider-agnosticism: the SAME loop unrolls identically when the provider has NO
/// native sub-agents and NO activity hooks (Codex/OpenCode → inline fallback). The gate,
/// branch and re-spawn are decided entirely in nyx's handler over the PTY, so the graph does
/// not care which provider produced the diff. This is the "démontré sur un 2e provider"
/// axis at the MECHANISM level (the loop is hook-free); the live opencode evidence is in the
/// spike doc.
#[test]
fn same_loop_holds_on_a_provider_without_native_subagents() {
    let (tmp, src) = make_sandbox();
    let _tmp = tmp;
    let activity = AgentActivityStore::new();

    let spawner = InlineFallbackSpawner {
        edits: vec![SRC_UNDER_FIX, SRC_GREEN],
    };

    let engine = drive_loop(&spawner, &src, &activity);

    // IDENTICAL action shape to the native path — the loop is provider-agnostic.
    assert_eq!(
        engine.board.trace,
        vec![
            Action::Spawn {
                agent: "coder".into(),
                attempt: 0
            },
            Action::Spawn {
                agent: "coder".into(),
                attempt: 1
            },
            Action::Advance {
                to: "review".into()
            },
        ],
        "the inline-fallback provider unrolls the SAME spawn → re-spawn → advance loop"
    );
    assert_eq!(engine.board.spawns, 2);
    assert_eq!(engine.board.last_gate_green, Some(true));

    // No native hooks fired, so the activity machine stayed empty — the loop completed WITHOUT
    // the comfort signal. Enforcement held on the gate alone (hook-free), exactly the PRD's
    // "un provider sans ces hooks perd juste le confort, pas l'enforcement".
    assert!(
        activity
            .snapshot(&format!("term-{}", spawner.mechanism()))
            .is_none(),
        "the inline provider emitted no activity hooks, yet the loop still reached green"
    );
}

/// VOLET 3 — the isolated enforcement invariant, stated bare: at a gate that nyx runs and
/// finds RED, `nyx_next` returns `Spawn` (re-spawn), never `Advance`. Only when nyx's own
/// gate is green does it advance.
#[test]
fn a_red_gate_makes_nyx_next_respawn_and_never_advance() {
    let (tmp, src) = make_sandbox();
    let _tmp = tmp;

    let mut engine = Engine::new(src.clone());

    // Tick 1: at the spawn leaf → Spawn(0).
    assert!(matches!(
        engine.next(None),
        Action::Spawn { attempt: 0, .. }
    ));

    // The dispatcher runs a sub-agent that leaves the code STILL broken (red gate). Report a
    // (non-empty) diff back. Because nyx runs the gate and finds it red, the tick must
    // RE-SPAWN — it must NOT advance.
    std::fs::write(&src, SRC_UNDER_FIX).unwrap();
    let action = engine.next(Some("some red diff".into()));
    assert!(
        matches!(action, Action::Spawn { attempt: 1, .. }),
        "a red gate re-spawns (never advances): got {action:?}"
    );
    assert_eq!(
        engine.board.last_gate_green,
        Some(false),
        "nyx recorded the gate it ran as RED"
    );

    // Now the dispatcher's sub-agent fixes it → nyx's gate is green → and ONLY now advance.
    std::fs::write(&src, SRC_GREEN).unwrap();
    let action = engine.next(Some("green diff".into()));
    assert!(
        matches!(action, Action::Advance { .. }),
        "a green gate is the ONLY thing that advances: got {action:?}"
    );
}
