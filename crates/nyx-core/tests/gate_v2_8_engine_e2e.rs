//! GATE e2e (v2.8 — Harnais : moteur d'orchestration behavior-tree, PRD
//! `01KY4DQS06B939Z1CM8CPE7TW0`, Phase 7 task `01KY71J4T763KS5G72765YN3VZ`) — the FINAL
//! *juge de paix* that proves the whole P1-P6 assembly holds bout en bout, in ONE session,
//! DETERMINISTICALLY (sub-agent / judge results scripted à la P1 spike — no real LLM), with
//! ZERO external session spawn (`claude -p` is proscribed by design).
//!
//! It exercises the REAL production bricks, not stand-ins:
//!   * the SEEDED built-in ndev workflow ([`nyx_core::seed::seed_builtin_workflows`], run by
//!     `Db::open`, Phase 4) — the clé-en-main graph a fresh project runs;
//!   * the REAL `nyx_next` verb ([`nyx_core::mcp_engine::nyx_next`], Phase 3) — the single
//!     tick that advances the tree, runs the gate ITSELF over the PTY, imposes the transition
//!     through the guarded Spec authority, and hands the thin dispatcher one atomic action;
//!   * the REAL MCP authoring verbs ([`nyx_core::mcp_authoring::dispatch_authoring_tool`],
//!     Phase 4) — `workflow_create` + `node_add` build a custom graph from scratch;
//!   * the REAL multi-provider spawn adapter ([`nyx_core::engine::spawn`], Phase 3) — native
//!     `Task` for Claude, inline fallback otherwise;
//!   * the REAL managed-command PTY gate ([`nyx_core::command::CommandPty`], Phase 3);
//!   * the SAME `nyx_core::db` engine read accessors the Phase-6 sidecar read-commands
//!     (`engine_run_list` / `engine_workflow_graph`) serve — so the visualiser's read model
//!     is proven to reflect the live run FAITHFULLY.
//!
//! # The five clauses of the done-criterion (verbatim), each proven below
//!
//!   1. "ndev défaut se déroule single-session jusqu'à la review" —
//!      [`ndev_default_unrolls_single_session_to_review`].
//!   2. "un gate rouge bloque la progression" — [`a_red_blocking_gate_blocks_progression`].
//!   3. "un workflow customisé via MCP tourne" — [`a_custom_workflow_authored_via_mcp_runs_end_to_end`].
//!   4. "visualiser runtime fidèle" — [`the_engine_read_model_reflects_the_live_run`] (the Rust
//!      half: the read accessors mirror the live cursor + blackboard); the IPC-read-command half
//!      is in `nyx-sidecar` (`engine_run_list_reflects_a_live_nyx_next_run`) and the renderer
//!      highlight half is in `build-graph.test.ts`.
//!   5. "spawn natif Claude + fallback inline" —
//!      [`the_spawn_adapter_resolves_native_for_claude_and_inline_fallback_in_the_deroule`].
//!
//! Deterministic vs live: the P1 spike (`tests/nyx_next_loop_spike.rs`) already demonstrated the
//! loop LIVE (Claude native sub-agents + OpenCode) and captured it in
//! `docs/spikes/v2.8-nyx-next-single-session.md`. THIS gate proves the full ASSEMBLY
//! deterministically — the sub-agent/judge outcomes are scripted, exactly the P1 method — so no
//! real LLM or second live provider is needed here; the provider seam is exercised at the
//! MECHANISM level (native vs inline), which is what the assembly depends on.

use std::path::{Path, PathBuf};

use nyx_core::db::{self, CommandSource, Db, SpecSections, SpecTransition};
use nyx_core::mcp::RpcError;
use nyx_core::mcp_authoring::dispatch_authoring_tool;
use nyx_core::mcp_engine::{nyx_next, run_start, CallerSession, EngineRuntime};
use serde_json::{json, Value};

// ============================================================================
// Scaffolding — an on-disk temp nyx.db (Db::open SEEDS the built-ins), RAII-cleaned.
// ============================================================================

/// RAII temp dir removed on drop (same pattern as the crate's other gate e2e tests). The caller
/// binds it as `tmp` — declared BEFORE `db` — so cleanup survives a panicking assertion and the
/// dir is removed AFTER `db` closes its pool (locals drop in reverse declaration order).
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
        "nyx-gate-2-8-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// Open an on-disk nyx.db under `tmp` — `Db::open` runs the migrations AND seeds the three
/// built-in default workflows (ndev / nspec / ndoc), so the ndev default is present out of the
/// box (integration tests cannot use the `#[cfg(test)]` in-memory `Db`).
fn open_seeded_db(tmp: &TempDir) -> Db {
    Db::open(&tmp.path().join("nyx.db")).expect("open temp nyx.db (seeds built-ins)")
}

/// A live project whose ROOT workspace is the on-disk sandbox `dir` (so a gate command can run
/// there and a sentinel file lands in it). Returns `(project_id, workspace_id, workspace_root)`.
fn live_project(db: &Db, dir: &Path) -> (String, String, String) {
    db.with_conn(|c| {
        let (project, root) =
            db::create_project(c, "Engine", dir.to_str().unwrap(), None).expect("project");
        (project.id, root.id.clone(), root.root_path)
    })
}

/// A Spec driven to `in_progress` (anchored on `workspace_id`) so a `transition(review)` can fire.
fn spec_in_progress(db: &Db, project_id: &str, workspace_id: &str) -> String {
    db.with_conn(|c| {
        let (spec, _rev) = db::create_spec(
            c,
            project_id,
            "Engine spec",
            SpecSections {
                problem: "the assembly must hold".into(),
                solution: "walk the seeded ndev tree".into(),
                ..Default::default()
            },
        )
        .expect("spec");
        db::transition_spec(c, &spec.id, SpecTransition::Ready, true).expect("ready");
        db::transition_spec_with_workspace(
            c,
            &spec.id,
            SpecTransition::Start,
            true,
            Some(workspace_id),
        )
        .expect("start");
        spec.id
    })
}

/// Create a gate managed command (`cmdline`, run at the project's root workspace) and return the
/// materialized instance id — the `command_id` a `gate_check` node references.
///
/// The COMMAND LINE is never a shell built-in: see [`always_green_command`] and
/// [`sentinel_gate_command`], the only two callers, for why.
fn gate_command(db: &Db, project_id: &str, cmdline: &str) -> String {
    db.with_conn(|c| {
        let tpl = db::create_template(
            c,
            project_id,
            "gate",
            cmdline,
            None,
            CommandSource::default(),
        )
        .expect("gate template");
        db::instance_ids_for_template(c, &tpl.id)
            .expect("gate instances")
            .into_iter()
            .next()
            .expect("one gate instance materialized")
    })
}

// ----------------------------------------------------------------------------
// THE TWO CHECKS THIS FILE ARMS ITS GATES WITH — as SCRIPTS, never as shell built-ins.
//
// A gate spawns its command through [`nyx_core::pty::resolve_shell`], i.e. under WHATEVER shell the
// machine offers: `bash -c` on a Unix box or a Git Bash session, `powershell.exe -Command` on a
// plain Windows one. A literal command line therefore does not mean one thing — it means whatever
// the shell of the day makes of it. `true` and `test -f gate_ok` are POSIX BUILT-INS: under
// PowerShell `true` is not a command at all (non-zero exit → the always-green gate comes out RED,
// and the coder loop spins to its iteration cap), and `test -f gate_ok` never returns zero (→ the
// green half of every sentinel deroulé is unreachable). The file was consequently green only when
// launched from Git Bash — 0/7 under PowerShell — and, since `test:core` runs `--lib` and skips
// `tests/`, most often not launched at all.
//
// The fix is the one `gate_v2_9_spec_category_e2e.rs` already applied for the same reason: express
// the check as a `node` SCRIPT written into the workspace the gate runs in. `node <file>` is one
// program with one argument — the same sentence under `-c` and under `-Command` — and its exit code
// is what the shell reports either way. What each check MEANS is unchanged, so what the assertions
// prove is unchanged: the always-green check still always passes, and the sentinel check is still
// red until `gate_ok` lands in the gate's own cwd (`existsSync("gate_ok")` is `test -f gate_ok`,
// letter for letter).
//
// The helpers are DUPLICATED from `gate_v2_9_spec_category_e2e.rs` rather than shared: `tests/`
// carries no common module today, and introducing one would drag a file this finding does not name
// into the change. Two four-line scripts is the smaller debt.
// ----------------------------------------------------------------------------

/// The ALWAYS-GREEN check's script name and command line — the stand-in for a build control.
const ALWAYS_GREEN_SCRIPT: &str = "nyx-always-green.js";
const ALWAYS_GREEN_CMDLINE: &str = "node nyx-always-green.js";

/// The SENTINEL check's script name and command line. RED (exit 1) while `gate_ok` is absent from
/// the working directory, GREEN (exit 0) once the deroulé's fix spawn has created it.
const SENTINEL_SCRIPT: &str = "nyx-sentinel-check.js";
const SENTINEL_CMDLINE: &str = "node nyx-sentinel-check.js";

/// Write `body` as `name` in `dir` — the workspace the gate will run in, so the relative command
/// lines above resolve against the gate's own cwd and nothing else.
fn write_check_script(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("the check script");
}

/// An ALWAYS-GREEN check, armed on `project_id` and materialized in `dir`.
fn always_green_command(db: &Db, project_id: &str, dir: &Path) -> String {
    write_check_script(
        dir,
        ALWAYS_GREEN_SCRIPT,
        "// An always-green check — the portable spelling of `true`.\n\
         console.log('checks green');\n\
         process.exit(0);\n",
    );
    gate_command(db, project_id, ALWAYS_GREEN_CMDLINE)
}

/// The SENTINEL check, armed on `project_id` and materialized in `dir`: red until `gate_ok` exists
/// in the gate's cwd.
fn sentinel_gate_command(db: &Db, project_id: &str, dir: &Path) -> String {
    write_check_script(
        dir,
        SENTINEL_SCRIPT,
        // `existsSync("gate_ok")` is relative to the process's cwd — which is the gate's cwd, i.e.
        // exactly what `test -f gate_ok` used to read.
        "// The SENTINEL check: red until the sentinel `gate_ok` lands in the workspace.\n\
         const fs = require('node:fs');\n\
         const ok = fs.existsSync('gate_ok');\n\
         console.log(ok ? 'gate: the sentinel is there' : 'gate: no sentinel yet');\n\
         process.exit(ok ? 0 : 1);\n",
    );
    gate_command(db, project_id, SENTINEL_CMDLINE)
}

/// ARM the project's coder gate with a real, always-green check, walking the EXACT route nyx's own
/// refusal names — through the real MCP verbs, no DB shortcut. The seeded coder ships its `checks` gate
/// BLOCKING and declaring the `build_check` SLOT, and `nyx_next` refuses to advance past a gate that
/// arms nothing (a gate that verifies nothing cannot certify a build), so the route is: register the
/// command (`add_command`'s primitive here) and BIND it to this project's slot (`slot_bind`).
///
/// NOTHING IS FORKED, and that is the property this helper carries into every deroulé below: the flow
/// the runs resolve is the SHARED built-in, so the graph they exercise is the one nyx ships and keeps
/// upgrading. Arming used to mean forking that built-in — the only way to set commands on a node of an
/// immutable seeded tree — which froze the project's coder graph at the revision of the day it was
/// armed. Returns the BUILT-IN coder's workflow id: the flow this project now runs.
///
/// `dir` is the project's ROOT workspace — where the check script is materialized, since that is
/// the cwd the gate's command line resolves against.
fn arm_coder_gate(db: &Db, project_id: &str, dir: &Path) -> String {
    let command = always_green_command(db, project_id, dir);
    let (builtin_coder, _n) = builtin(db, "coder");
    author(
        db,
        "slot_bind",
        json!({
            "project_id": project_id,
            "slot": nyx_core::seed::BUILD_CHECK_SLOT,
            "command_ids": [command],
        }),
    );
    assert!(
        db.with_conn(|c| db::list_workflows(c, Some(project_id)))
            .expect("project workflows")
            .is_empty(),
        "arming the seeded gate must fork nothing — the project keeps running nyx's own graph",
    );
    builtin_coder.id
}

/// The `(workflow, nodes)` of the SEEDED built-in flow of `scope`.
fn builtin(db: &Db, scope: &str) -> (db::Workflow, Vec<db::Node>) {
    db.with_conn(|c| {
        let wf = db::list_workflows(c, None)
            .unwrap()
            .into_iter()
            .find(|w| w.scope == scope)
            .expect("built-in present (seeded by Db::open)");
        let nodes = db::list_nodes(c, &wf.id).unwrap();
        (wf, nodes)
    })
}

/// A READY Spec (draft → Ready) carrying `n` open `slice` tasks in ONE phase ("Build") — the state a
/// real ndev run starts from: its entry `start` transition (ready → in_progress) fires (v2.8 review
/// N12), and the open tasks are what its build gate (`spec_open_tasks`) routes on, giving the coder
/// sub-workflow child real work to drive (start → gate → done) per task (v2.8 lot 2).
fn spec_ready_with_tasks(db: &Db, project_id: &str, n: usize) -> String {
    db.with_conn(|c| {
        let (spec, _rev) = db::create_spec(
            c,
            project_id,
            "Engine spec",
            SpecSections {
                problem: "the assembly must hold".into(),
                solution: "walk the seeded ndev tree".into(),
                ..Default::default()
            },
        )
        .expect("spec");
        for i in 0..n {
            db::create_task(
                c,
                project_id,
                &format!("t{i}"),
                db::NewTaskInput {
                    spec_id: Some(spec.id.clone()),
                    phase_name: Some("Build".to_string()),
                    ..db::NewTaskInput::slice()
                },
            )
            .expect("task");
        }
        db::transition_spec(c, &spec.id, SpecTransition::Ready, true).expect("ready");
        spec.id
    })
}

/// FILE a finding of `severity` the way an auditor does — a `kind=review` task on the Spec, in the
/// build phase (so the coder child drains it like any other work). This is the ONLY trace an audit
/// leaves, and what the ndev graph DERIVES its `audit_verdict` from (`spec_open_findings`, which
/// counts only the severities that REOPEN the build). Returns the finding's task id.
fn file_finding_at(
    db: &Db,
    project_id: &str,
    spec_id: &str,
    title: &str,
    severity: &str,
) -> String {
    db.with_conn(|c| {
        db::create_task(
            c,
            project_id,
            title,
            db::NewTaskInput {
                spec_id: Some(spec_id.to_string()),
                phase_name: Some("Build".to_string()),
                ..db::NewTaskInput::review(severity)
            },
        )
    })
    .expect("finding")
    .id
}

/// [`file_finding_at`] at the severity that REOPENS a build — the nominal auditor gesture.
fn file_finding(db: &Db, project_id: &str, spec_id: &str, title: &str) -> String {
    file_finding_at(db, project_id, spec_id, title, db::TASK_SEVERITY_HIGH)
}

/// Drive a coder SUB-WORKFLOW child (from a surfaced `sub_workflow` action) to `done` under `provider`,
/// exactly like the real dispatcher: loop `nyx_next(child_run_id)`, performing each action until done.
/// The child owns the per-phase task loop, so it surfaces `spawn_subagent{coder}` + `note` + `done`.
/// Returns the number of coder spawns it performed (one per open task; 0 when the phase is drained). For
/// `claude`, asserts each coder spawn resolves to the NATIVE Task (the provider seam holds in the child).
fn drive_coder_child(db: &Db, rt: &EngineRuntime, child_run_id: &str, provider: &str) -> usize {
    let mut coder_spawns = 0;
    let mut last: Option<Value> = None;
    let mut steps = 0;
    loop {
        let out = tick(db, rt, child_run_id, provider, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            // The coder's CATEGORY PREFLIGHT (built-in revision 24). These Specs carry no category,
            // which is exactly the population the preflight exists for: it classifies EXPLICITLY
            // before the flow routes on the genre. `feature` — the branch that owes no reproduction,
            // which is the path this gate has always walked.
            "judge" => last = Some(json!({ "verdict": "feature" })),
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "coder", "the coder child spawns the coder");
                if provider == "claude" {
                    assert_eq!(out["action"]["mode"], "native_task", "native spawn holds in the child");
                }
                coder_spawns += 1;
                last = Some(json!({ "diff": "d" }));
            }
            other => panic!("unexpected coder child action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 60, "runaway coder child loop");
    }
    coder_spawns
}

/// OPEN a run through the REAL `run_start` verb (the /ndev entry point) — never a raw INSERT — for
/// an EXPLICIT `workflow_id`, optionally bound to `spec_id`. Clause 1 additionally proves the
/// by-SCOPE resolution (`run_start` with just a `spec_id` resolves the seeded built-in); the other
/// clauses drive a specific authored/seeded workflow, so they pin it by id here (review B2).
fn create_run(db: &Db, workflow_id: &str, spec_id: Option<&str>) -> String {
    let mut args = json!({ "workflow_id": workflow_id });
    if let Some(sid) = spec_id {
        args.as_object_mut()
            .unwrap()
            .insert("spec_id".into(), json!(sid));
    }
    run_start(db, &EngineRuntime::new(), &args).expect("run_start").result["run_id"]
        .as_str()
        .expect("run_id")
        .to_string()
}

/// One `nyx_next` call for `run_id` under `provider`, optionally carrying `last_result`. Returns
/// the raw handler `Result` so a test can assert either the returned action or a refusal.
fn try_tick(
    db: &Db,
    rt: &EngineRuntime,
    run_id: &str,
    provider: &str,
    last: Option<Value>,
) -> Result<Value, RpcError> {
    let mut args = json!({ "run_id": run_id, "provider": provider });
    if let Some(last) = last {
        args.as_object_mut()
            .unwrap()
            .insert("last_result".into(), last);
    }
    session(|caller| nyx_next(db, rt, caller, &args)).map(|o| o.result)
}

/// A `nyx_next` call that must succeed (panics on a handler error).
fn tick(db: &Db, rt: &EngineRuntime, run_id: &str, provider: &str, last: Option<Value>) -> Value {
    try_tick(db, rt, run_id, provider, last).expect("nyx_next")
}

fn spec_status(db: &Db, spec_id: &str) -> String {
    db.with_conn(|c| db::get_spec(c, spec_id))
        .unwrap()
        .unwrap()
        .status
}

fn task_status(db: &Db, task_id: &str) -> String {
    db.with_conn(|c| db::get_task(c, task_id))
        .unwrap()
        .unwrap()
        .status
}

fn run_row(db: &Db, run_id: &str) -> db::WorkflowRun {
    db.with_conn(|c| db::get_workflow_run(c, run_id))
        .unwrap()
        .unwrap()
}

thread_local! {
    /// The terminal every deroulé here ticks as. THREAD-LOCAL because Rust runs each test on its own
    /// thread and each opens its own DB, so the human turns one deroulé records can never satisfy
    /// another's consent check.
    static SESSION: String = "term-gate".to_string();
}

/// Run `f` with this test's [`SESSION`] as the tick's caller.
fn session<T>(f: impl FnOnce(CallerSession<'_>) -> T) -> T {
    SESSION.with(|terminal_id| {
        f(CallerSession {
            terminal_id: Some(terminal_id),
        })
    })
}

/// Answer the question the run just asked, and return the `last_result` the next tick carries. Every
/// answer travels the same `{"decision": …}` route; the one that COMMITS the Spec says so
/// (`human_turn_required`, H6), and the deroulé then does what a real session must — put the question
/// to the human and get a REPLY, which is what recording a human turn stands for here.
fn answer(db: &Db, out: &Value, decision: &str) -> Option<Value> {
    if out["action"]["human_turn_required"] == json!(true) {
        SESSION.with(|terminal_id| {
            db.with_conn(|c| db::record_human_turn(c, terminal_id, None, decision))
                .expect("record the human turn")
        });
    }
    Some(json!({ "decision": decision }))
}

/// Drive an authoring verb through the REAL MCP routing ([`dispatch_authoring_tool`]) — the same
/// entry point the sidecar dispatches. Panics on a mis-route or a tool error.
fn author(db: &Db, name: &str, args: Value) -> Value {
    dispatch_authoring_tool(db, name, &args)
        .unwrap_or_else(|| panic!("authoring tool `{name}` was not routed"))
        .unwrap_or_else(|e| {
            panic!(
                "authoring tool `{name}` failed: [{:?}] {}",
                e.code, e.message
            )
        })
        .result
}

// ============================================================================
// CLAUSE 1 — "ndev défaut se déroule single-session jusqu'à la review"
// ============================================================================

/// The SEEDED built-in ndev workflow, bound to a READY Spec, unrolls in ONE session through the REAL
/// `nyx_next` verb across the WHOLE lifecycle (v2.8 review N12): query → the entry `start`
/// (in_progress) → the build-REWORK loop (coder child → the AUDIT, one pass per Spec commitment; a
/// FILED finding makes the derived verdict non-zero, the human answers 'revise' and a FRESH coder child
/// re-runs) → in_review → the review park → the final `done`. The Spec ends `completed`, and
/// the invariant holds: it is `in_progress` throughout the build and enters `in_review` only at the
/// end, never mid-build. Deterministic (scripted sub-agent diffs + a real filed finding); no real LLM,
/// no external session spawn.
#[test]
fn ndev_default_unrolls_single_session_start_to_done() {
    let tmp = temp_dir("ndev");
    let db = open_seeded_db(&tmp);
    let (project_id, _workspace_id, _ws) = live_project(&db, tmp.path());
    // The coder child's per-task gate ships unarmed and REFUSES to advance — configure it first, the
    // way a project does before its first /ndev.
    arm_coder_gate(&db, &project_id, tmp.path());
    // A READY Spec with one build task, so the coder sub-workflow child has real work to drive.
    let spec_id = spec_ready_with_tasks(&db, &project_id, 1);

    let (wf, _nodes) = builtin(&db, "ndev");

    // START THE RUN THE WAY /ndev DOES (review B2): run_start resolves the SEEDED built-in ndev
    // workflow from the Spec's project (which owns no custom ndev → the built-in default) and binds
    // the run — the REAL entry point, not a raw `create_workflow_run` INSERT.
    let opened = run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id }))
        .expect("run_start")
        .result;
    assert_eq!(
        opened["workflow_id"], wf.id,
        "run_start resolved the seeded ndev built-in for the project"
    );
    assert_eq!(opened["scope"], "ndev");
    assert_eq!(opened["spec_id"], spec_id, "and bound the run to the Spec");
    let run_id = opened["run_id"].as_str().unwrap().to_string();

    // ONE session: ONE engine runtime + ONE Db across every tick.
    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut coder_spawns = 0;
    let mut coder_children = 0;
    let mut audit_subjects: Vec<String> = Vec::new();
    let mut audits = 0;
    let mut finding: Option<String> = None;
    let mut steps = 0;

    loop {
        let out = tick(&db, &rt, &run_id, "claude", last.take());
        match out["action"]["type"].as_str().unwrap() {
            // The build pass is DELEGATED to a coder CHILD: drive it to done (like the dispatcher),
            // accumulating the coder spawns it performs (each native_task under claude). Mid-build the
            // Spec has STARTED but NOT yet entered review.
            "sub_workflow" => {
                assert_eq!(out["status"], "running");
                assert_eq!(
                    spec_status(&db, &spec_id),
                    db::SPEC_STATUS_IN_PROGRESS,
                    "the Spec started and is mid-build (not yet in review)"
                );
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                coder_children += 1;
                coder_spawns += drive_coder_child(&db, &rt, &child, "claude");
                last = None;
            }
            "spawn_subagent" => {
                // ndev's own spawn is the AUDITOR — Claude's NATIVE Task, in-session (never `claude -p`).
                // Each pass is handed the Spec COMMITMENT it examines as its per-node subject.
                assert_eq!(out["action"]["agent_id"], "auditor", "ndev itself spawns only the auditor");
                assert_eq!(out["action"]["mode"], "native_task", "clause 5 within the deroulé");
                assert_eq!(out["action"]["mechanism"], "claude-native-task");
                let subject = out["action"]["subject"]
                    .as_str()
                    .expect("every audit pass names the commitment it examines")
                    .to_string();
                if subject == "user_stories" {
                    audits += 1;
                    // The FIRST audit FILES a finding (the auditor's one gesture — a kind=review task),
                    // which is what makes the DERIVED verdict non-zero and re-runs a fresh coder child;
                    // the second audit files nothing, so the derived verdict is 0 and the loop exits.
                    if audits == 1 {
                        finding = Some(file_finding(&db, &project_id, &spec_id, "story unbuilt"));
                    }
                }
                audit_subjects.push(subject);
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                if p.contains("in review") {
                    // The review PARK: the Spec is now in review (the transition fired at build's end).
                    assert_eq!(
                        spec_status(&db, &spec_id),
                        db::SPEC_STATUS_IN_REVIEW,
                        "review is entered only after the build, never mid-build"
                    );
                    last = answer(&db, &out, "approve");
                } else if p.contains("reworking") {
                    // N25 — the open finding asks "rework or abandon"; keep REWORKING. The prompt
                    // carries the count nyx DERIVED from the filed findings, not a claimed verdict.
                    assert!(
                        p.contains("1 open finding"),
                        "the ask names the derived finding count: {p}"
                    );
                    last = answer(&db, &out, "revise");
                } else {
                    last = answer(&db, &out, "yes"); // the confirm-done ask
                }
            }
            "done" => break,
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 80, "runaway ndev deroulé");
    }

    // The FILED finding re-opened a FRESH coder child → two children; the first drove the build task,
    // the second closed the finding (a finding IS a task) — and the auditor ran its four commitment
    // passes on each of the two build passes.
    assert_eq!(coder_children, 2, "the audit rework re-opened a fresh coder child");
    assert_eq!(
        coder_spawns, 2,
        "the first coder child drove the build task, the second closed the filed finding"
    );
    assert_eq!(
        audit_subjects,
        [
            "user_stories",
            "testing_decisions",
            "implementation_decisions",
            "out_of_scope",
            "user_stories",
            "testing_decisions",
            "implementation_decisions",
            "out_of_scope",
        ],
        "each build pass audits the FOUR commitments the Spec states, one spawn each",
    );
    assert_eq!(
        task_status(&db, finding.as_deref().expect("the audit filed a finding")),
        db::TASK_STATUS_DONE,
        "the rework CLOSED the filed finding — that is what made the second audit clean",
    );

    // The full lifecycle completed: the Spec is NOW completed, the run is done + un-cursored.
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_COMPLETED,
        "ndev drove the Spec all the way to completed (done, not in_review)"
    );
    let row = run_row(&db, &run_id);
    assert_eq!(row.status, db::RUN_STATUS_DONE);
    assert!(row.current_node_id.is_none(), "a done run parks nowhere");
}

/// THE SHAPE THAT COST 3 H 35, 109 TICKS AND $69.48 — a ONE-PHASE Spec whose audit files ONE HIGH
/// and a stream of LOWS. The live run's findings per round were 4 → 1 → 4 → 3 → 3 → 5 → 0: once the
/// single high-severity finding was handled, every later round only produced second-order lows, and
/// each low bought a whole coder sub-workflow plus four audit passes.
///
/// The deroulé here reproduces it and pins the decided behaviour end to end: ndev builds the HIGH,
/// then STOPS — no coder child is opened for a low — and the Spec reaches `completed` with the lows
/// STILL OPEN on it, carrying their severity, findable next cycle. Same real bricks as the clause-1
/// deroulé (seeded built-in, real `nyx_next`, real PTY gate in the child); only the auditor's filing
/// is scripted, and it is scripted to be RELENTLESS: every audit round files fresh lows, so nothing
/// but the deferral can bring this run to an end.
#[test]
fn a_build_stops_on_lows_and_the_spec_completes_carrying_them() {
    let tmp = temp_dir("lows");
    let db = open_seeded_db(&tmp);
    let (project_id, _workspace_id, _ws) = live_project(&db, tmp.path());
    arm_coder_gate(&db, &project_id, tmp.path());
    // ONE phase, one build task — the user's Spec.
    let spec_id = spec_ready_with_tasks(&db, &project_id, 1);

    let opened = run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id }))
        .expect("run_start")
        .result;
    let run_id = opened["run_id"].as_str().unwrap().to_string();

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut coder_children = 0;
    let mut coder_spawns = 0;
    let mut audit_rounds = 0;
    let mut high: Option<String> = None;
    let mut lows: Vec<String> = Vec::new();
    let mut rework_prompts: Vec<String> = Vec::new();
    let done_notes: Vec<String>;
    let mut steps = 0;

    loop {
        let out = tick(&db, &rt, &run_id, "claude", last.take());
        match out["action"]["type"].as_str().unwrap() {
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                coder_children += 1;
                coder_spawns += drive_coder_child(&db, &rt, &child, "claude");
                last = None;
            }
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "auditor");
                // One filing per ROUND, on the first of the four commitment passes. EVERY round files
                // lows — the second-order stream that made the live run unaffordable; only the FIRST
                // files the high.
                if out["action"]["subject"] == json!("user_stories") {
                    audit_rounds += 1;
                    if audit_rounds == 1 {
                        high = Some(file_finding(&db, &project_id, &spec_id, "story unbuilt"));
                    }
                    for i in 0..3 {
                        lows.push(file_finding_at(
                            &db,
                            &project_id,
                            &spec_id,
                            &format!("nit {audit_rounds}.{i}"),
                            db::TASK_SEVERITY_LOW,
                        ));
                    }
                }
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("").to_string();
                if p.contains("in review") {
                    last = answer(&db, &out, "approve");
                } else if p.contains("reworking") {
                    rework_prompts.push(p);
                    last = answer(&db, &out, "revise");
                } else {
                    last = answer(&db, &out, "yes");
                }
            }
            "done" => {
                done_notes = out["action"]["notes"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                break;
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 80, "runaway deroulé — a low re-opened the build");
    }

    // THE CENTRAL CLAIM. Two coder children: one for the build task, one for the HIGH. The six lows
    // filed across the two audit rounds bought NOTHING — a third child (and a third audit round) is
    // exactly the burn the decision removed.
    assert_eq!(
        coder_children, 2,
        "one child for the build task, one for the high — a low opens NO coder sub-workflow",
    );
    assert_eq!(coder_spawns, 2, "and each child drove exactly one piece of work");
    assert_eq!(audit_rounds, 2, "the second audit's lows did not buy a third round");

    // The human was asked to spend a build round ONCE — for the high, and the ask said so.
    assert_eq!(
        rework_prompts.len(),
        1,
        "the budget question is put once, for the high alone: {rework_prompts:?}",
    );
    assert!(
        rework_prompts[0].contains("1 open finding"),
        "the derived verdict counted the high alone, not the four findings filed: {}",
        rework_prompts[0],
    );

    // The HIGH was built; the SIX lows are untouched and still open, with their severity.
    assert_eq!(
        task_status(&db, high.as_deref().expect("the audit filed a high")),
        db::TASK_STATUS_DONE,
        "the high-severity finding WAS reworked — the deferral narrows the loop, it does not open it",
    );
    assert_eq!(lows.len(), 6, "both audit rounds filed their lows");
    for low in &lows {
        let task = db.with_conn(|c| db::get_task(c, low)).unwrap().unwrap();
        assert_eq!(
            task.status,
            db::TASK_STATUS_TODO,
            "a low is never worked in this cycle — it is left exactly as filed",
        );
        assert_eq!(
            task.severity.as_deref(),
            Some(db::TASK_SEVERITY_LOW),
            "…and keeps its severity, which is what makes it findable next cycle",
        );
    }

    // AND THE SPEC COMPLETES CARRYING THEM — the decided outcome, and the completion SAYS SO rather
    // than pretending the Spec is clean.
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_COMPLETED,
        "the Spec completes with its lows open — they do not block done",
    );
    assert!(
        done_notes.iter().any(|n| n.contains("6 open low-severity finding")),
        "the run's last word names what the Spec shipped with: {done_notes:?}",
    );
    let row = run_row(&db, &run_id);
    assert_eq!(row.status, db::RUN_STATUS_DONE);
}

// ============================================================================
// CLAUSE 2 — "un gate rouge bloque la progression"
// ============================================================================

/// A BLOCKING `gate_check` nyx runs RED over the real PTY makes `nyx_next` REFUSE to advance: the
/// verb errors in its handler (`invalid_state` / `gate_red`), the Spec is NEVER transitioned, and
/// the run is parked `blocked`. The GREEN counterpart (same graph, sentinel present) advances and
/// transitions — proving it is the gate, run by nyx itself, that gates. No provider hook anywhere.
#[test]
fn a_red_blocking_gate_blocks_progression() {
    let tmp = temp_dir("redgate");
    let db = open_seeded_db(&tmp);
    let (project_id, workspace_id, ws_path) = live_project(&db, tmp.path());

    // The gate is green ONLY once the sentinel `gate_ok` exists in the workspace.
    let gate = sentinel_gate_command(&db, &project_id, tmp.path());

    let build = |db: &Db, wf: &str| {
        let root = db.with_conn(|c| {
            db::create_node(c, wf, None, 0, db::NODE_TYPE_SEQUENCE, "root", "{}")
                .unwrap()
                .id
        });
        db.with_conn(|c| {
            db::create_node(
                c,
                wf,
                Some(&root),
                0,
                db::NODE_TYPE_GATE_CHECK,
                "checks",
                &json!({ "command_ids": [gate], "blocking": true }).to_string(),
            )
            .unwrap();
            db::create_node(
                c,
                wf,
                Some(&root),
                1,
                db::NODE_TYPE_TRANSITION,
                "→ review",
                &json!({ "target_state": "in_review" }).to_string(),
            )
            .unwrap();
            db::set_workflow_root(c, wf, Some(&root)).unwrap();
        });
    };

    // --- RED: the gate refuses, the Spec is untouched, the run is blocked ---------------------
    let spec_red = spec_in_progress(&db, &project_id, &workspace_id);
    let wf_red = db
        .with_conn(|c| db::create_workflow(c, None, "wf-red", db::WORKFLOW_SCOPE_CUSTOM, None))
        .unwrap()
        .id;
    build(&db, &wf_red);
    let run_red = create_run(&db, &wf_red, Some(&spec_red));

    let rt = EngineRuntime::new();
    let err = try_tick(&db, &rt, &run_red, "claude", None).unwrap_err();
    assert_eq!(
        err.code, "invalid_state",
        "a red blocking gate refuses the progression verb"
    );
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("gate_red"),
        "the refusal names the red gate"
    );
    // nyx NEVER returned "avance": the Spec stayed in_progress, the run is blocked.
    assert_eq!(
        spec_status(&db, &spec_red),
        db::SPEC_STATUS_IN_PROGRESS,
        "not advanced past the red gate"
    );
    assert_eq!(
        run_row(&db, &run_red).status,
        db::RUN_STATUS_BLOCKED,
        "the run is parked blocked"
    );

    // --- GREEN: the same graph, sentinel present → it advances and transitions -----------------
    std::fs::write(format!("{ws_path}/gate_ok"), "ok").unwrap();
    let spec_green = spec_in_progress(&db, &project_id, &workspace_id);
    let wf_green = db
        .with_conn(|c| db::create_workflow(c, None, "wf-green", db::WORKFLOW_SCOPE_CUSTOM, None))
        .unwrap()
        .id;
    build(&db, &wf_green);
    let run_green = create_run(&db, &wf_green, Some(&spec_green));

    let rt2 = EngineRuntime::new();
    let out = tick(&db, &rt2, &run_green, "claude", None);
    assert_eq!(
        out["action"]["type"], "done",
        "the green gate lets the run advance to done"
    );
    assert_eq!(
        spec_status(&db, &spec_green),
        db::SPEC_STATUS_IN_REVIEW,
        "only a green gate advances past to the transition"
    );
}

// ============================================================================
// CLAUSE 3 — "un workflow customisé via MCP tourne"
// ============================================================================

/// A workflow built FROM SCRATCH through the real MCP authoring verbs (`workflow_create` +
/// `node_add`) is then DRIVEN to termination by the real `nyx_next` verb — proving the whole
/// authoring → execution path end to end. The authored graph is a real coder→gate loop over a
/// real PTY gate that starts RED (sentinel absent) and goes GREEN after the second coder writes
/// the sentinel: a red gate forces a real RE-SPAWN, a green gate exits the loop, and the run
/// reaches done.
#[test]
fn a_custom_workflow_authored_via_mcp_runs_end_to_end() {
    let tmp = temp_dir("custom");
    let db = open_seeded_db(&tmp);
    let (project_id, _workspace_id, ws_path) = live_project(&db, tmp.path());
    let gate = sentinel_gate_command(&db, &project_id, tmp.path());

    // --- AUTHOR the graph via MCP -------------------------------------------------------------
    let wf = author(
        &db,
        "workflow_create",
        json!({ "name": "custom dev loop", "scope": "custom", "project_id": project_id }),
    );
    let wf_id = wf["workflow"]["id"].as_str().unwrap().to_string();

    // sequence root (the first parent-less node becomes the workflow root).
    let root = author(
        &db,
        "node_add",
        json!({ "workflow_id": wf_id, "type": "sequence", "label": "custom" }),
    );
    let root_id = root["node"]["id"].as_str().unwrap().to_string();

    // loop { spawn coder → gate } until gate_green == true.
    let loop_node = author(
        &db,
        "node_add",
        json!({
            "workflow_id": wf_id, "type": "loop", "label": "phases", "parent_id": root_id,
            "order_index": 0, "config": { "until": { "var": "gate_green", "eq": true } },
        }),
    );
    let loop_id = loop_node["node"]["id"].as_str().unwrap().to_string();
    author(
        &db,
        "node_add",
        json!({
            "workflow_id": wf_id, "type": "spawn_subagent", "label": "coder", "parent_id": loop_id,
            "order_index": 0, "config": { "agent_id": "coder" },
        }),
    );
    author(
        &db,
        "node_add",
        json!({
            "workflow_id": wf_id, "type": "gate_check", "label": "checks", "parent_id": loop_id,
            "order_index": 1, "config": { "command_ids": [gate], "blocking": true },
        }),
    );
    // A trailing note after the loop so the run terminates cleanly (no Spec binding needed).
    author(
        &db,
        "node_add",
        json!({
            "workflow_id": wf_id, "type": "note", "label": "wrap up", "parent_id": root_id,
            "order_index": 1, "config": { "text": "phase loop complete" },
        }),
    );

    // The authored tree is coherent + reconstructible (the authoring layer validated it on each
    // add; here we confirm the shape the interpreter will walk).
    let (nodes, root_ptr) = db.with_conn(|c| {
        let nodes = db::list_nodes(c, &wf_id).unwrap();
        let root_ptr = db::get_workflow(c, &wf_id).unwrap().unwrap().root_node_id;
        (nodes, root_ptr)
    });
    assert_eq!(
        root_ptr.as_deref(),
        Some(root_id.as_str()),
        "authoring wired the root pointer"
    );
    assert_eq!(nodes.len(), 5, "root + loop + coder + gate + note");

    // --- RUN the authored graph via nyx_next ---------------------------------------------------
    let run_id = create_run(&db, &wf_id, None);
    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut coder_spawns = 0;
    let mut surfaced: Vec<String> = Vec::new();
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, "claude", last.take());
        let ty = out["action"]["type"].as_str().unwrap().to_string();
        surfaced.push(ty.clone());
        match ty.as_str() {
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "coder");
                coder_spawns += 1;
                // Only the SECOND coder writes the sentinel — the first gate is RED and forces a
                // real re-spawn (the loop re-runs), which nyx pilots without any external session.
                if coder_spawns == 2 {
                    std::fs::write(format!("{ws_path}/gate_ok"), "ok").unwrap();
                }
                last = Some(json!({ "diff": format!("attempt {coder_spawns}") }));
            }
            "done" => {
                // G4 — the trailing note rides the end instead of costing a stop of its own.
                assert_eq!(out["action"]["notes"], json!(["phase loop complete"]));
                break;
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 30, "runaway custom deroulé: {surfaced:?}");
    }

    assert_eq!(
        coder_spawns, 2,
        "the red gate forced exactly one re-spawn in the authored loop"
    );
    assert_eq!(
        surfaced,
        vec!["spawn_subagent", "spawn_subagent", "done"],
        "authoring → execution: spawn → (red) re-spawn → (green) done, the note riding it"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}

// ============================================================================
// CLAUSE 4 (Rust half) — "visualiser runtime fidèle"
// ============================================================================

/// The engine READ MODEL the Phase-6 visualiser consumes reflects the LIVE run FAITHFULLY. After
/// each real `nyx_next` tick, the SAME `nyx_core::db` accessors the sidecar read-commands wrap —
/// `list_workflow_runs` (behind `engine_run_list`) and `get_workflow` + `list_nodes` (behind
/// `engine_workflow_graph`) — expose EXACTLY the run's persisted `current_node_id` + `blackboard`
/// and the node the cursor points at. So the renderer, fed those, highlights the true cursor. The
/// IPC-read-command half is proven in `nyx-sidecar` (`engine_run_list_reflects_a_live_nyx_next_run`);
/// the renderer-highlight half in `build-graph.test.ts`.
#[test]
fn the_engine_read_model_reflects_the_live_run() {
    let tmp = temp_dir("fidelity");
    let db = open_seeded_db(&tmp);
    let (project_id, _ws_id, _ws) = live_project(&db, tmp.path());
    arm_coder_gate(&db, &project_id, tmp.path());
    // A READY Spec with one open build task — the ndev build gate reads `spec_open_tasks`, so real work
    // is what routes the run into the coder sub-workflow this test parks on.
    let spec_id = spec_ready_with_tasks(&db, &project_id, 1);

    let (wf, _all_nodes) = builtin(&db, "ndev");
    let run_id = create_run(&db, &wf.id, Some(&spec_id));
    let rt = EngineRuntime::new();

    // The read-model accessors, exactly as the sidecar `engine_run_list` / `engine_workflow_graph`
    // read-commands call them (nyx-sidecar/src/runtime/db_command.rs `engine_run_list` /
    // `engine_workflow_graph`). Assert the run's live cursor + blackboard round-trip through them,
    // and the cursor points at a real node of the graph dump.
    // The read model = the PERSISTED (full) blackboard + cursor, exactly as the visualiser reads them
    // from the DB. N22 changed the TICK OUTPUT to a per-tick delta, but `persist` still writes the FULL
    // board, so the read model is unaffected — this asserts precisely that.
    let read_model = |cursor: &str| -> Value {
        let (runs, nodes) = db.with_conn(|c| {
            let runs = db::list_workflow_runs(c, &wf.id).unwrap();
            let nodes = db::list_nodes(c, &wf.id).unwrap();
            (runs, nodes)
        });
        assert_eq!(runs.len(), 1, "the workflow's single run is listed newest-first");
        assert_eq!(
            runs[0].current_node_id.as_deref(),
            Some(cursor),
            "engine_run_list reflects the live cursor nyx_next persisted"
        );
        assert!(
            nodes.iter().any(|n| n.id == cursor),
            "engine_workflow_graph carries the cursor node — the renderer can locate + highlight it"
        );
        serde_json::from_str(&runs[0].blackboard).unwrap()
    };

    // Tick 1 → the entry query + start fold INLINE (not surfaced), then the run OPENS the coder child
    // and parks on the sub_workflow node. The PERSISTED board already carries the queried spec_status.
    let out1 = tick(&db, &rt, &run_id, "claude", None);
    assert_eq!(out1["action"]["type"], "sub_workflow");
    let child_run_id = out1["action"]["child_run_id"].as_str().unwrap().to_string();
    let cursor1 = out1["current_node_id"].as_str().unwrap().to_string();
    let board1 = read_model(&cursor1);
    assert_eq!(
        board1["spec_status"],
        json!("ready"),
        "the persisted read model carries the queried status"
    );

    // Drive the coder child to done (it drives the phase's single task), then tick the PARENT: it folds
    // the child's VERIFIED result and parks on the AUDITOR spawn. The persisted board now carries sub_result.
    drive_coder_child(&db, &rt, &child_run_id, "claude");
    let out2 = tick(&db, &rt, &run_id, "claude", None);
    assert_eq!(out2["action"]["type"], "spawn_subagent");
    assert_eq!(out2["action"]["agent_id"], "auditor");
    let cursor2 = out2["current_node_id"].as_str().unwrap().to_string();
    assert_ne!(
        cursor2, cursor1,
        "the cursor MOVED — the read model tracks it, not a stale value"
    );
    let board2 = read_model(&cursor2);
    // The persisted read model exposes the true folded state — the child's VERIFIED sub_result.
    assert_eq!(
        board2["sub_result"]["status"],
        json!("done"),
        "the folded child result (its verified status) is persisted"
    );
    // N22 — the TICK delta carries ONLY what changed this tick: the newly-folded sub_result, but NOT
    // spec_status (set at tick 1, unchanged since — so it is not re-sent).
    assert_eq!(
        out2["blackboard_delta"]["sub_result"]["status"],
        json!("done"),
        "the delta carries the newly-folded child result"
    );
    assert!(
        out2["blackboard_delta"].get("spec_status").is_none(),
        "an unchanged key is NOT re-sent in the delta (N22)"
    );
}

// ============================================================================
// CLAUSE 5 — "spawn natif Claude + fallback inline"
// ============================================================================

/// The multi-provider spawn adapter holds WITHIN the e2e deroulé: driving the SAME seeded ndev
/// graph resolves every `spawn_subagent` to Claude's NATIVE `Task` under `provider=claude`, and to
/// the INLINE fallback under a provider with no native sub-agents (`opencode`) — and NEITHER mode
/// is an external session spawn (the `claude -p` proscription). The enforcement + flow are
/// provider-independent, so the graph unrolls to done identically for both.
#[test]
fn the_spawn_adapter_resolves_native_for_claude_and_inline_fallback_in_the_deroule() {
    let tmp = temp_dir("provider");
    let db = open_seeded_db(&tmp);
    let (project_id, _ws, _) = live_project(&db, tmp.path());
    arm_coder_gate(&db, &project_id, tmp.path());
    let (wf, _nodes) = builtin(&db, "ndev");

    // Drive the seeded ndev to done under `provider` (bound to a fresh READY Spec with one build task
    // so the coder child spawns the coder; the audit files nothing so it is clean first pass, the
    // review is approved), collecting the resolved (mode, mechanism) of every spawn action — the
    // coder's (INSIDE the child) AND the auditor's four commitment passes (in the parent).
    let deroule = |provider: &str| -> Vec<(String, String)> {
        let spec_id = spec_ready_with_tasks(&db, &project_id, 1);
        let run_id = create_run(&db, &wf.id, Some(&spec_id));
        let rt = EngineRuntime::new();
        let mut last: Option<Value> = None;
        let mut modes: Vec<(String, String)> = Vec::new();
        let mut steps = 0;
        // Record one spawn's resolved (mode, mechanism), asserting neither is an external session spawn.
        let mut record = |out: &Value| {
            let mode = out["action"]["mode"].as_str().unwrap().to_string();
            let mechanism = out["action"]["mechanism"].as_str().unwrap().to_string();
            assert!(
                mode == "native_task" || mode == "inline",
                "provider {provider}: {mode} must be an in-session mechanism (never `claude -p`)"
            );
            assert!(
                !mechanism.contains("session") && !mechanism.contains("-p"),
                "provider {provider}: mechanism {mechanism} must not name an external session"
            );
            modes.push((mode, mechanism));
        };
        loop {
            let out = tick(&db, &rt, &run_id, provider, last.take());
            match out["action"]["type"].as_str().unwrap() {
                // The build pass is a coder CHILD — drive it, recording ITS coder spawn modes.
                "sub_workflow" => {
                    let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                    let mut clast: Option<Value> = None;
                    let mut csteps = 0;
                    loop {
                        let cout = tick(&db, &rt, &child, provider, clast.take());
                        match cout["action"]["type"].as_str().unwrap() {
                            "done" => break,
                            // The category preflight (revision 24) — see `drive_coder_child`.
                            "judge" => clast = Some(json!({ "verdict": "feature" })),
                            "spawn_subagent" => {
                                record(&cout);
                                clast = Some(json!({ "diff": "d" }));
                            }
                            other => panic!("unexpected coder child action '{other}' ({provider})"),
                        }
                        csteps += 1;
                        assert!(csteps < 60, "runaway coder child ({provider})");
                    }
                    last = None;
                }
                "spawn_subagent" => {
                    record(&out);
                    last = Some(json!({ "diff": "d" }));
                }
                "ask_human" => {
                    let p = out["action"]["prompt"].as_str().unwrap_or("");
                    let ans = if p.contains("in review") { "approve" } else { "yes" };
                    last = answer(&db, &out, ans);
                }
                    "done" => break,
                other => panic!("unexpected action '{other}' for provider {provider}"),
            }
            steps += 1;
            assert!(steps < 60, "runaway {provider} deroulé");
        }
        // The graph unrolled to done regardless of provider.
        assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
        modes
    };

    // Claude → every spawn is the NATIVE Task (one coder in the child + the four audit passes in the
    // parent).
    let claude = deroule("claude");
    assert_eq!(
        claude.len(),
        5,
        "one coder (child) + one auditor per Spec commitment (parent) spawned"
    );
    for (mode, mechanism) in &claude {
        assert_eq!(mode, "native_task", "Claude resolves to the native Task");
        assert_eq!(mechanism, "claude-native-task");
    }

    // OpenCode (no native sub-agents) → every spawn degrades to the INLINE fallback, same graph.
    let opencode = deroule("opencode");
    assert_eq!(opencode.len(), 5, "the SAME graph unrolls the same spawns");
    for (mode, mechanism) in &opencode {
        assert_eq!(
            mode, "inline",
            "a provider without native sub-agents degrades to inline"
        );
        assert_eq!(mechanism, "inline-fallback");
    }
}

// ============================================================================
// Lot 2 — the coder sub-workflow built-in walks a task start → gate green → done
// ============================================================================

/// The SEEDED coder sub-workflow walks a phase task bout-en-bout, driven directly the way ndev's child
/// would be: it STARTS the task (transition_task, nyx-inline), makes it GREEN (spawn the coder → the
/// per-task gate nyx runs itself), and marks it DONE (transition_task) — the task ends `done` and the
/// run reaches done. Deterministic (scripted coder diff); the gate is nyx's own, and REAL: the flow is
/// the project's armed fork of the built-in, because the built-in's empty gate refuses to advance.
#[test]
fn the_coder_subworkflow_walks_a_task_start_gate_green_to_done() {
    let tmp = temp_dir("coder-walk");
    let db = open_seeded_db(&tmp);
    let (project_id, workspace_id, _ws) = live_project(&db, tmp.path());
    let coder_wf_id = arm_coder_gate(&db, &project_id, tmp.path());
    // An in_progress Spec with one build task (todo) in a phase — the coder's active phase. The plan
    // is POSED first: past `draft` a task may only name a phase the plan already holds, never mint
    // one (K2).
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    db.with_conn(|c| db::set_spec_phases(c, &spec_id, r#"[{"number":1,"name":"Build"}]"#))
        .expect("phase plan");
    let task_id = db
        .with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                "t",
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    phase_name: Some("Build".to_string()),
                    ..db::NewTaskInput::slice()
                },
            )
        })
        .expect("task")
        .id;

    // Drive the coder flow DIRECTLY (as ndev's sub_workflow child would drive it).
    let run_id = create_run(&db, &coder_wf_id, Some(&spec_id));

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut coder_spawns = 0;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, "claude", last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            // The category preflight (revision 24) — this Spec carries none, so the genre is
            // classified before the build routes on it. `feature`: no reproduction owed.
            "judge" => last = Some(json!({ "verdict": "feature" })),
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "coder", "the coder built-in spawns the coder");
                // The task was STARTED (transition_task start ran inline) before the coder implements it.
                assert_eq!(
                    task_status(&db, &task_id),
                    db::TASK_STATUS_IN_PROGRESS,
                    "the task was started (transition_task start) before the coder ran"
                );
                coder_spawns += 1;
                last = Some(json!({ "diff": "the fix" }));
            }
            other => panic!("unexpected coder action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway coder walk");
    }

    assert_eq!(coder_spawns, 1, "the coder ran once for the single phase task");
    assert_eq!(
        task_status(&db, &task_id),
        db::TASK_STATUS_DONE,
        "the coder walked the task start → (gate green) → done (transition_task done)"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}
