//! Tests for the built-in default workflows (task `01KY71J32MDD2HF07QT2WMJYKZ`): the seed is
//! idempotent and creates the three flows; each is a COMPLETE, authoring-VALID graph; the ndev
//! default transitions to review ONLY at the end (and offers an abort on a failed audit); nspec
//! drives to `ready` on approval; ndoc's `query` guard routes on the real Spec status.

use std::collections::HashSet;

use serde_json::{json, Value};

use super::*;
use crate::db::{self, Db, SpecSections};
use crate::engine::Tree;
use crate::mcp_engine::{nyx_next, CallerSession, EngineRuntime};

/// A memory DB with the built-ins seeded.
fn seeded_db() -> Db {
    let db = Db::in_memory();
    db.with_conn(seed_builtin_workflows).unwrap();
    db
}

/// The `(workflow, nodes)` of the built-in flow of `scope`.
fn builtin(db: &Db, scope: &str) -> (db::Workflow, Vec<db::Node>) {
    db.with_conn(|c| {
        let wf = db::list_workflows(c, None)
            .unwrap()
            .into_iter()
            .find(|w| w.scope == scope)
            .expect("built-in present");
        let nodes = db::list_nodes(c, &wf.id).unwrap();
        (wf, nodes)
    })
}

/// The `(node_id, agent_id)` of a thin flow's single `spawn_subagent` node — the value finding #34
/// is about (the effector role the flow spawns).
fn thin_spawn(db: &Db, scope: &str) -> (String, String) {
    let (_, nodes) = builtin(db, scope);
    let spawn = nodes
        .iter()
        .find(|n| n.type_ == db::NODE_TYPE_SPAWN_SUBAGENT)
        .expect("the thin flow has a spawn node");
    let config: Value = serde_json::from_str(&spawn.config).expect("spawn config is JSON");
    let agent_id = config["agent_id"]
        .as_str()
        .expect("a spawn carries an agent_id")
        .to_string();
    (spawn.id.clone(), agent_id)
}

// --- seed presence + idempotency -------------------------------------------

/// DONE-CRITERION ("nspec/ndoc présents ; un projet neuf a le ndev défaut" + lot 2: the two
/// sub-workflow built-ins): the seed creates the FIVE built-in (NULL-project) flows — the three
/// triggerable ones (ndev/nspec/ndoc) plus the sub-workflow-only coder/prototyper — each rooted, and
/// re-running it is a no-op (no duplicates).
#[test]
fn seed_creates_five_rooted_builtins_idempotently() {
    let db = Db::in_memory();
    db.with_conn(seed_builtin_workflows).unwrap();
    db.with_conn(seed_builtin_workflows).unwrap(); // idempotent second boot

    let wfs = db.with_conn(|c| db::list_workflows(c, None)).unwrap();
    assert_eq!(wfs.len(), 5, "exactly the five built-ins, no duplicates");
    let scopes: HashSet<&str> = wfs.iter().map(|w| w.scope.as_str()).collect();
    assert_eq!(
        scopes,
        HashSet::from(["ndev", "nspec", "ndoc", "coder", "prototyper"]),
        "one of each scope (incl. the sub-workflow-only coder + prototyper)"
    );
    for w in &wfs {
        assert!(w.project_id.is_none(), "a built-in default owns no project");
        assert!(w.root_node_id.is_some(), "each built-in is rooted");
    }
}

// --- ndev: the full Spec lifecycle (start → build-rework → review → done) --

/// N12 + lot 2 — ndev drives a READY Spec through the WHOLE lifecycle under nyx_next: query → start
/// (in_progress) → the build (DELEGATED to a coder sub-workflow child that drives each task) → the AUDIT
/// (one pass per Spec commitment, filing nothing here) → in_review → review (approve) → done. The Spec
/// ends `completed`. The coder runs INSIDE the child (ndev's own spawns are only the auditor). query /
/// gate / transition are nyx-INLINE, and NO `judge` is surfaced at all — the audit verdict is derived
/// from the findings, never claimed.
#[test]
fn ndev_drives_a_ready_spec_through_to_completed() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (_project, spec_id) = ready_spec_with_tasks(&db, 2);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut coder_spawns = 0;
    let mut audit_subjects: Vec<String> = Vec::new();
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            // The build pass is a coder CHILD — drive it to done (like the dispatcher), accumulating the
            // coder spawns it performs, then re-tick the parent (which re-checks the child status).
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                coder_spawns += drive_coder_child(&db, &rt, &child);
                last = None;
            }
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "auditor", "ndev itself spawns only the auditor");
                audit_subjects.push(
                    out["action"]["subject"]
                        .as_str()
                        .expect("every audit pass is handed the commitment it examines")
                        .to_string(),
                );
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                // The review ask parks for the human ('approve'); the confirm-done ask commits ('yes').
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("in review") {
                    // The end-of-build `in_review` transition FIRED (the nominal in_progress path):
                    // the Spec is in review by the time the human is asked to review it.
                    assert_eq!(
                        spec_status(&db, &spec_id),
                        db::SPEC_STATUS_IN_REVIEW,
                        "the guarded end-of-build transition sent the built Spec to review"
                    );
                    "approve"
                } else {
                    "yes"
                };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 60, "runaway nyx_next loop");
    }
    assert_eq!(coder_spawns, 2, "the coder child drove both phase tasks (one coder spawn each)");
    assert_eq!(
        audit_subjects,
        [
            "user_stories",
            "testing_decisions",
            "implementation_decisions",
            "out_of_scope"
        ],
        "ONE audit pass per commitment the Spec itself states — each spawn told WHICH one it examines",
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_COMPLETED,
        "ndev drove the Spec all the way to completed (done, not in_review)"
    );
}

/// N12(c) + lot 2 — a FILED FINDING (not an agent's word) re-opens a FRESH coder sub-workflow child;
/// it does NOT propose a cancel. The audit's first pass files ONE finding — a `kind=review` task, the
/// only thing an auditor can leave behind — so the DERIVED verdict is non-zero and the human is asked
/// to rework or abandon; the second pass files none, so the derived verdict is 0 and the build loop
/// exits. TWO distinct coder children are opened (the per-node child id clears on fold), the rework
/// CLOSED the finding, the run completes via the review-approve path, and no ask mentions cancelling.
#[test]
fn ndev_reworks_the_coder_when_the_audit_files_a_finding() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (project_id, spec_id) = ready_spec_with_tasks(&db, 1);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut children: HashSet<String> = HashSet::new();
    let mut audit_passes = 0;
    let mut finding: Option<String> = None;
    let mut saw_cancel_prompt = false;
    let mut saw_derived_count = false;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                children.insert(child.clone());
                drive_coder_child(&db, &rt, &child);
                last = None;
            }
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "auditor");
                // The first pass of the audit sequence opens on the user stories; the FIRST audit
                // FILES a finding there (the auditor's one gesture), the second finds nothing to file.
                if out["action"]["subject"] == "user_stories" {
                    audit_passes += 1;
                    if audit_passes == 1 {
                        finding = Some(file_finding(&db, &project_id, &spec_id, "story 2 unbuilt"));
                    }
                }
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                // The confirm-CANCEL ask is the only one that mentions cancel WITHOUT being the review
                // ask (whose prompt merely lists 'cancel' as an option). Reaching it = the cancel
                // branch was taken.
                if !p.contains("in review") && p.to_lowercase().contains("cancel") {
                    saw_cancel_prompt = true;
                }
                // N25 — open findings ask "rework or abandon"; keep REWORKING (the rework path). The
                // prompt carries the DERIVED count, so the human arbitrates on the evidence.
                let ans = if p.contains("in review") {
                    "approve"
                } else if p.contains("reworking") {
                    saw_derived_count |= p.contains("1 open finding");
                    "revise"
                } else {
                    "yes"
                };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 60, "runaway nyx_next loop");
    }
    assert_eq!(
        children.len(),
        2,
        "the FILED finding re-opened a FRESH coder child (rework), not a cancel"
    );
    assert_eq!(audit_passes, 2, "the audit ran once per build pass");
    assert!(
        saw_derived_count,
        "the rework ask names the finding count it derived, not a claimed verdict"
    );
    assert_eq!(
        task_status(&db, finding.as_deref().expect("the audit filed a finding")),
        db::TASK_STATUS_DONE,
        "the rework pass CLOSED the finding — that is what made the second audit clean",
    );
    assert!(
        !saw_cancel_prompt,
        "an audit with findings never proposes a cancel — it reworks"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}

/// A BUILD THAT NEVER CONVERGES STOPS AT A BOUND SOMEBODY CHOSE, AND SAYS WHY. The inner build loop's
/// body files its own next iteration — each audit round turns what it finds into review TASKS, which
/// is more work to build — so nothing guarantees it terminates; the measured real run converged in 4
/// rounds, but only by luck of the code under it. Here EVERY audit files a fresh finding, so the
/// verdict is truthy forever and the human keeps answering 'revise'. The loop must die at
/// [`super::BUILD_REWORK_ROUNDS`] rounds — not the engine's unchosen 25 — and its message must
/// DIAGNOSE (H3): WHICH condition never held, and what the board last carried for it.
#[test]
fn a_build_that_never_converges_dies_at_the_round_nyx_chose_and_names_the_condition() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (project_id, spec_id) = ready_spec_with_tasks(&db, 1);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut children = 0;
    let mut rounds = 0;
    let mut steps = 0;
    let err = loop {
        let mut args = json!({ "run_id": run_id });
        if let Some(l) = last.take() {
            args.as_object_mut().unwrap().insert("last_result".into(), l);
        }
        let out = match session(|caller| nyx_next(&db, &rt, caller, &args)) {
            Ok(outcome) => outcome.result,
            // The ONLY exit of this deroulé: the loop's own bound.
            Err(e) => break e,
        };
        match out["action"]["type"].as_str().unwrap() {
            "done" => panic!("a build whose audit never comes back clean must not complete"),
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                children += 1;
                drive_coder_child(&db, &rt, &child);
                last = None;
            }
            "spawn_subagent" => {
                // Each round files ONE NEW finding — the shape the bound exists for: the audit's own
                // output is the work that re-opens the build.
                if out["action"]["subject"] == "user_stories" {
                    rounds += 1;
                    file_finding(&db, &project_id, &spec_id, &format!("finding {rounds}"));
                }
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                assert!(
                    p.contains("reworking"),
                    "the only ask reachable while the audit keeps filing is rework-or-abandon: {p}"
                );
                last = answer(&db, &out, "revise");
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 300, "the build loop never stopped — it has no bound");
    };

    assert_eq!(
        children,
        super::BUILD_REWORK_ROUNDS as usize,
        "the build reworked exactly the number of rounds nyx chose, then stopped paying"
    );
    assert_eq!(rounds, children, "each round audited what its coder child built");
    let message = err.message;
    assert!(
        message.contains(&format!("exceeded {} iterations", super::BUILD_REWORK_ROUNDS)),
        "the failure names the bound it hit: {message}"
    );
    assert!(
        message.contains(r#"audit_verdict != "revise""#),
        "…WHICH condition never held (H3): {message}"
    );
    assert!(
        message.contains(r#"audit_verdict = "revise""#),
        "…and what it last observed for it (H3): {message}"
    );
    assert_eq!(
        run_row(&db, &run_id).status,
        db::RUN_STATUS_FAILED,
        "a build that never converges fails — it does not quietly go to review"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_IN_PROGRESS,
        "and it never sent the Spec to review on work the audit kept rejecting"
    );
}

/// N25 — an audit that leaves findings lets the human ABANDON the build instead of burning coders to
/// max_iterations: answering 'abandon' to "rework or abandon" ends the run CLEANLY (done, not failed)
/// WITHOUT sending the Spec to review. The Spec is left `in_progress` (never `in_review`/`completed`),
/// the coder ran ONCE — the abandon short-circuited any re-spawn — and the finding stays OPEN (an
/// abandoned build resolves nothing).
#[test]
fn ndev_abandons_the_build_on_human_request_without_freezing() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (project_id, spec_id) = ready_spec_with_tasks(&db, 1);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut children = 0;
    let mut finding: Option<String> = None;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                children += 1;
                drive_coder_child(&db, &rt, &child);
                last = None;
            }
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "auditor");
                // The audit finds something real the coder cannot fix here (an unworkable env).
                if out["action"]["subject"] == "out_of_scope" && finding.is_none() {
                    finding = Some(file_finding(&db, &project_id, &spec_id, "scope overrun"));
                }
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                // The only ask reachable on the abandon path is "rework or abandon"; ABANDON it.
                assert!(
                    p.contains("reworking"),
                    "the only ask on the abandon path is rework-or-abandon, got: {p}"
                );
                last = answer(&db, &out, "abandon");
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(
            steps < 60,
            "an abandon must end the run cleanly, never loop to failure (N25)"
        );
    }
    assert_eq!(
        children, 1,
        "one coder child ran — abandon short-circuited any re-open"
    );
    assert_eq!(
        task_status(&db, finding.as_deref().expect("the audit filed a finding")),
        db::TASK_STATUS_TODO,
        "the abandoned build left the finding open — nothing was resolved by abandoning",
    );
    assert_eq!(
        run_row(&db, &run_id).status,
        db::RUN_STATUS_DONE,
        "an abandoned build ends the run cleanly (done, not failed)"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_IN_PROGRESS,
        "an abandoned build leaves the Spec in progress — NEVER in_review / completed (N25)"
    );
}

// --- the three graphs are COMPLETE + authoring-valid ------------------------

/// One `nyx_next` call for `run_id`, optionally carrying a `last_result`.
fn tick(db: &Db, rt: &EngineRuntime, run_id: &str, last: Option<Value>) -> Value {
    let mut args = json!({ "run_id": run_id });
    if let Some(l) = last {
        args.as_object_mut().unwrap().insert("last_result".into(), l);
    }
    session(|caller| nyx_next(db, rt, caller, &args))
        .expect("nyx_next")
        .result
}

thread_local! {
    /// The terminal every deroulé in this module ticks as. THREAD-LOCAL because Rust runs each test
    /// on its own thread and each test opens its own DB, so the human turns one deroulé records can
    /// never satisfy another's consent check, without threading a session through fifteen loops.
    static SESSION: String = "term-seed".to_string();
}

/// Run `f` with this test's [`SESSION`] as the tick's caller.
fn session<T>(f: impl FnOnce(CallerSession<'_>) -> T) -> T {
    SESSION.with(|terminal_id| {
        f(CallerSession {
            terminal_id: Some(terminal_id),
        })
    })
}

/// Answer the question the run just asked, and return the `last_result` the next tick carries.
///
/// Every answer travels the same way — `{"decision": …}` on the next tick. The one the graph makes
/// COMMIT the Spec says so (`human_turn_required`, H6), and for it the deroulé does what a real
/// session must: it puts the question to the human and the human REPLIES, which is what
/// `record_human_turn` stands for here. Without that turn nyx refuses the very same call — which is
/// exactly what [`crate::mcp_engine`]'s own tests drive.
fn answer(db: &Db, out: &Value, decision: &str) -> Option<Value> {
    if out["action"]["human_turn_required"] == json!(true) {
        SESSION.with(|terminal_id| {
            db.with_conn(|c| db::record_human_turn(c, terminal_id, None, decision))
                .expect("record the human turn")
        });
    }
    Some(json!({ "decision": decision }))
}

/// A project + a fresh DRAFT Spec on it, returning `(project_id, spec_id)`.
fn draft_spec(db: &Db) -> (String, String) {
    db.with_conn(|c| {
        let (project, _root) = db::create_project(c, "P", "/tmp/seed-spec", None).unwrap();
        let (spec, _rev) = db::create_spec(
            c,
            &project.id,
            "S",
            SpecSections {
                problem: "p".into(),
                solution: "s".into(),
                ..Default::default()
            },
        )
        .unwrap();
        (project.id, spec.id)
    })
}

/// A COMPLETED Spec (draft → ready → in_progress → in_review → completed) — the one state ndoc's
/// `query` guard lets the doc sync run from. Returns the spec id.
fn completed_spec(db: &Db) -> String {
    let (_project_id, spec_id) = draft_spec(db);
    for t in [
        db::SpecTransition::Ready,
        db::SpecTransition::Start,
        db::SpecTransition::Review,
        db::SpecTransition::Done,
    ] {
        db.with_conn(|c| db::transition_spec(c, &spec_id, t, true))
            .unwrap();
    }
    spec_id
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

/// FILE a finding the way the AUDITOR effector actually does — `task_add` with `kind=review` and NO
/// `phase`: a finding is not part of the plan, so it carries no phase number. Returns its task id.
fn file_unphased_finding(db: &Db, project_id: &str, spec_id: &str, title: &str) -> String {
    db.with_conn(|c| {
        db::create_task(
            c,
            project_id,
            title,
            db::NewTaskInput {
                spec_id: Some(spec_id.to_string()),
                ..db::NewTaskInput::review("high")
            },
        )
    })
    .expect("finding")
    .id
}

/// FILE a finding the way an auditor does: a `kind=review` task on the Spec, in the build phase (so
/// the coder child drains it like any other work). This is the ONLY trace an audit leaves — and what
/// the graph's `spec_open_findings` verdict is derived from. Returns the finding's task id.
fn file_finding(db: &Db, project_id: &str, spec_id: &str, title: &str) -> String {
    db.with_conn(|c| {
        db::create_task(
            c,
            project_id,
            title,
            db::NewTaskInput {
                spec_id: Some(spec_id.to_string()),
                phase_name: Some("Build".to_string()),
                ..db::NewTaskInput::review("high")
            },
        )
    })
    .expect("finding")
    .id
}

/// A project + a READY Spec on it (draft → Ready), returning `(project_id, spec_id)` — the state a
/// real ndev run starts from.
fn ready_spec(db: &Db) -> (String, String) {
    let (project_id, spec_id) = draft_spec(db);
    db.with_conn(|c| db::transition_spec(c, &spec_id, db::SpecTransition::Ready, true))
        .unwrap();
    (project_id, spec_id)
}

/// A project + a READY Spec carrying `n` open `slice` tasks in ONE phase ("Build") — so the coder
/// sub-workflow child has real work to drive (start → gate → done) per task. The coder's gate is ARMED
/// with a real command ([`arm_coder_gate`]): the built-in ships it blocking and slot-declaring, which
/// the runtime refuses until bound, so "real work to drive" now includes a real check to pass. Returns
/// `(project_id, spec_id)`.
fn ready_spec_with_tasks(db: &Db, n: usize) -> (String, String) {
    let (project_id, spec_id) = draft_spec(db);
    for i in 0..n {
        db.with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                &format!("t{i}"),
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    phase_name: Some("Build".to_string()),
                    ..db::NewTaskInput::slice()
                },
            )
        })
        .expect("task");
    }
    db.with_conn(|c| db::transition_spec(c, &spec_id, db::SpecTransition::Ready, true))
        .unwrap();
    arm_coder_gate(db, &project_id);
    (project_id, spec_id)
}

/// ARM the project's coder gate with a REAL, always-green check — what a project must do before its
/// first `/ndev`, and by the route nyx's own refusal names: the built-in ships its `checks` gate
/// BLOCKING and declaring the [`BUILD_CHECK_SLOT`] slot (which `nyx_next` refuses to advance past
/// while nothing answers it), so the project registers its command and BINDS it. The seeded coder
/// itself is untouched — it stays the flow this project's runs resolve, and it stays upgradable. The
/// command runs at the project's root workspace, so that directory must exist for the PTY to have a cwd.
///
/// "Always green" was spelled `true` — a POSIX PROGRAM NAME, and nyx hands the line to the
/// platform shell, which on Windows is PowerShell and has no such program. The gate never went
/// green there and the ndev loop spun to its cap, which is what made nine of these tests red. It
/// is now [`crate::test_cmdline::green`]: the same verdict, written for the shell that reads it.
fn arm_coder_gate(db: &Db, project_id: &str) {
    let cwd = db.with_conn(|c| {
        db::list_workspaces(c, project_id)
            .unwrap()
            .into_iter()
            .next()
            .expect("the project's root workspace")
            .root_path
    });
    std::fs::create_dir_all(&cwd).expect("the gate needs a real cwd to run in");
    let instance_id = db.with_conn(|c| {
        let tpl = db::create_template(
            c,
            project_id,
            "gate",
            &crate::test_cmdline::green(),
            None,
            Default::default(),
        )
        .expect("gate template");
        db::instance_ids_for_template(c, &tpl.id)
            .expect("gate instances")
            .into_iter()
            .next()
            .expect("one gate instance materialized")
    });
    let authoring = |name: &str, args: Value| {
        crate::mcp_authoring::dispatch_authoring_tool(db, name, &args)
            .unwrap_or_else(|| panic!("`{name}` was not routed"))
            .unwrap_or_else(|e| panic!("`{name}` failed: {}", e.message))
            .result
    };
    authoring(
        "slot_bind",
        json!({
            "project_id": project_id,
            "slot": BUILD_CHECK_SLOT,
            "command_ids": [instance_id],
        }),
    );
}

/// Drive a coder SUB-WORKFLOW child (from a surfaced `sub_workflow` action) to `done`, exactly like the
/// real dispatcher: loop `nyx_next(child_run_id)` performing each action until it returns done. The
/// child owns the per-phase task loop (spawn coder → the gate nyx runs itself → transition_task done),
/// so it surfaces only `spawn_subagent{coder}` + `note` + `done`. Returns the number of coder spawns it
/// performed (one per open task in the phase; 0 when the phase is already drained).
fn drive_coder_child(db: &Db, rt: &EngineRuntime, child_run_id: &str) -> usize {
    let mut coder_spawns = 0;
    let mut last: Option<Value> = None;
    let mut steps = 0;
    loop {
        let out = tick(db, rt, child_run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "coder", "the coder child spawns the coder");
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

/// A project + a READY Spec carrying ONE `kind=human` task in the "Build" phase — the task nspec
/// creates spontaneously (provisioning, access) and that the coder must never be handed. Returns
/// `(project_id, spec_id, task_id)`.
fn ready_spec_with_a_human_task(db: &Db) -> (String, String, String) {
    let (project_id, spec_id) = draft_spec(db);
    let task_id = db
        .with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                "Provision PostgreSQL and expose DATABASE_URL",
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    phase_name: Some("Build".to_string()),
                    ..db::NewTaskInput::human("psql $DATABASE_URL -c 'select 1'")
                },
            )
        })
        .expect("the human task")
        .id;
    db.with_conn(|c| db::transition_spec(c, &spec_id, db::SpecTransition::Ready, true))
        .unwrap();
    arm_coder_gate(db, &project_id);
    (project_id, spec_id, task_id)
}

/// Drive a coder SUB-WORKFLOW child to `done` like the real dispatcher, but ANSWERING the questions it
/// parks on — the bubbled `ask_human` reaches the top-level human, whose answer is routed right back to
/// `nyx_next(child_run_id)`, which is exactly what this loop does. `on_action` sees every surfaced
/// action (so a test can act as the sub-agent: block a task, correct a Spec section, …) and returns the
/// `last_result` to report. Returns the `(agent_id, label)` of every spawn the child performed.
fn drive_coder_child_answering(
    db: &Db,
    rt: &EngineRuntime,
    child_run_id: &str,
    mut on_action: impl FnMut(&Value) -> Value,
) -> Vec<(String, String)> {
    let mut spawns = Vec::new();
    let mut last: Option<Value> = None;
    let mut steps = 0;
    loop {
        let out = tick(db, rt, child_run_id, last.take());
        let action = out["action"].clone();
        match action["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => {
                spawns.push((
                    action["agent_id"].as_str().unwrap_or("").to_string(),
                    action["label"].as_str().unwrap_or("").to_string(),
                ));
                last = Some(on_action(&action));
            }
            "ask_human" => last = Some(on_action(&action)),
            other => panic!("unexpected coder child action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 60, "runaway coder child loop");
    }
    spawns
}

/// A project + an IN_REVIEW Spec (draft → Ready → Start → Review) — the state a re-launched /ndev
/// resumes at (its N17c entry door).
fn spec_in_review(db: &Db) -> (String, String) {
    let (project_id, spec_id) = ready_spec(db);
    db.with_conn(|c| {
        db::transition_spec(c, &spec_id, db::SpecTransition::Start, true).unwrap();
        db::transition_spec(c, &spec_id, db::SpecTransition::Review, false).unwrap();
    });
    (project_id, spec_id)
}

/// A project + an IN_REVIEW Spec that STILL carries `n` open tasks — the STEPPED case (a per-phase
/// review: the phases built so far are in review, the rest are still `todo`). The state the
/// NAMED-FACT build gate exists for.
fn spec_in_review_with_tasks(db: &Db, n: usize) -> (String, String) {
    let (project_id, spec_id) = ready_spec_with_tasks(db, n);
    db.with_conn(|c| {
        db::transition_spec(c, &spec_id, db::SpecTransition::Start, true).unwrap();
        db::transition_spec(c, &spec_id, db::SpecTransition::Review, false).unwrap();
    });
    (project_id, spec_id)
}

fn run_row(db: &Db, run_id: &str) -> db::WorkflowRun {
    db.with_conn(|c| db::get_workflow_run(c, run_id))
        .unwrap()
        .unwrap()
}

/// The Spec's current (active) revision number — a fork bumps it.
fn spec_rev(db: &Db, spec_id: &str) -> i32 {
    db.with_conn(|c| db::get_spec(c, spec_id))
        .unwrap()
        .unwrap()
        .current_revision_number
}

/// The set of `target_state`s of a tree's `transition` nodes.
fn transition_targets(nodes: &[db::Node]) -> HashSet<String> {
    nodes
        .iter()
        .filter(|n| n.type_ == db::NODE_TYPE_TRANSITION)
        .map(|n| {
            serde_json::from_str::<Value>(&n.config).unwrap()["target_state"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

/// DONE-CRITERION (v2.8 review — "rien de mince") : each built-in is a COMPLETE graph (a backtrack
/// loop + a selector fork) that the interpreter rebuilds AND the authoring validator accepts (its
/// engaging transitions are ask_human-gated — B3). Plus the flow-specific vocabulary: nspec spawns
/// INLINE and commits to ready/cancel; ndoc guards on a `query` and never transitions the Spec.
#[test]
fn all_three_builtins_are_complete_and_authoring_valid() {
    let db = seeded_db();
    // The three triggerable flows PLUS the two sub-workflow-only built-ins (lot 2): each rebuilds,
    // passes the authoring validator, and is a complete graph (a loop + a selector fork).
    for scope in ["ndev", "nspec", "ndoc", "coder", "prototyper"] {
        let (wf, nodes) = builtin(&db, scope);
        // (a) The interpreter rebuilds it (structural validity).
        Tree::from_rows(&nodes, wf.root_node_id.as_deref())
            .unwrap_or_else(|e| panic!("{scope} rebuilds: {e}"));
        // (b) It PASSES the authoring validator (engaging transitions ask_human-gated, no orphans).
        crate::mcp_authoring::validate_tree(&nodes, wf.root_node_id.as_deref())
            .unwrap_or_else(|e| panic!("{scope} must be authoring-valid: {e}"));
        // (c) A complete graph — a backtrack loop + a selector fork (no thin flow left).
        let types: HashSet<&str> = nodes.iter().map(|n| n.type_.as_str()).collect();
        assert!(types.contains(db::NODE_TYPE_LOOP), "{scope} has a loop");
        assert!(types.contains(db::NODE_TYPE_SELECTOR), "{scope} has a selector fork");
    }

    let (_w, nspec) = builtin(&db, "nspec");
    let inline_spawn = nspec.iter().any(|n| {
        n.type_ == db::NODE_TYPE_SPAWN_SUBAGENT
            && serde_json::from_str::<Value>(&n.config).unwrap()["inline"] == json!(true)
    });
    assert!(inline_spawn, "nspec's spec author runs inline (N2)");
    assert!(
        nspec.iter().any(|n| n.type_ == db::NODE_TYPE_QUERY),
        "nspec branches on a query — create / draft / fork (N12)"
    );
    assert_eq!(
        transition_targets(&nspec),
        HashSet::from(["ready".to_string()]),
        "nspec commits a draft to ready — its default branch is NOT a cancel (N12(d))"
    );

    let (_w, ndoc) = builtin(&db, "ndoc");
    assert!(
        ndoc.iter().any(|n| n.type_ == db::NODE_TYPE_QUERY),
        "ndoc guards on a query (B2)"
    );
    assert!(
        transition_targets(&ndoc).is_empty(),
        "ndoc does not transition the Spec (docs are orthogonal to its lifecycle)"
    );

    let (_w, ndev) = builtin(&db, "ndev");
    assert!(
        ndev.iter().any(|n| n.type_ == db::NODE_TYPE_QUERY),
        "ndev queries the Spec status to guard its entry start (N12)"
    );
    assert_eq!(
        transition_targets(&ndev),
        HashSet::from([
            "in_progress".to_string(),
            "in_review".to_string(),
            "done".to_string(),
            "cancel".to_string(),
        ]),
        "ndev drives the full lifecycle: start → review → done (or an explicit cancel)"
    );
}

/// N12 — nspec branches on the REAL status, runs the ENGINE-driven grill loop, and commits a DRAFT
/// to `ready`: query → the 'draft' route note → the grill loop (NYX's judge ends it, after a 'more'
/// round) → the final query → confirm ready → the Spec is `ready`. The author spawns TWICE (the grill
/// iterated), proving the completion decision lives in nyx's judge, not the effector's own prompt.
#[test]
fn nspec_grills_a_draft_then_commits_it_to_ready() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "nspec");
    let (_project, spec_id) = draft_spec(&db);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut author_spawns = 0;
    let mut grills = 0;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "spec", "nspec spawns the spec author");
                author_spawns += 1;
                last = Some(json!({ "diff": "spec text" }));
            }
            "judge" => {
                grills += 1;
                // NYX's judge ends the grill: one more round, then complete.
                last = Some(json!({ "verdict": if grills < 2 { "more" } else { "complete" } }));
            }
            "ask_human" => {
                // The draft is ready-able, so the entry offers "grill or finalize" — choose to KEEP
                // grilling; the grill 'steer' takes any answer; the commit 'confirm ready' takes 'yes'.
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("finalize") {
                    "grill"
                } else if p.contains("author's question") {
                    "an answer"
                } else {
                    "yes"
                };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway nyx_next loop");
    }
    assert_eq!(
        author_spawns, 2,
        "the grill loop iterated ('more' → 'complete') — nyx drove the completion"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_READY,
        "the draft was committed to ready"
    );
}

/// A fresh, EMPTY folder under the OS temp dir — the stand-in for a real worktrees root.
fn temp_root(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("nyx-seed-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The workspace name the `create_workspace` effect DERIVES for `spec_id` — the Spec's human key,
/// `<project key>-<number>`. Read back from the DB rather than spelled out, so a test asserting
/// "under the root, in the Spec's own subfolder" cannot pass on a name that merely happens to match.
fn derived_name(db: &Db, spec_id: &str) -> String {
    db.with_conn(|c| {
        let spec = db::get_spec(c, spec_id).unwrap().expect("the Spec exists");
        let project = db::get_project(c, &spec.project_id)
            .unwrap()
            .expect("its project exists");
        format!("{}-{}", project.key, spec.number)
    })
}

/// HORS PERIMETRE ASSUME (2026-08-18) — THE YES-WITH-A-WORKSPACE ANSWER ('go') with the project's
/// `worktrees_root` CONFIGURED: the Spec goes `ready` exactly as a plain 'yes' does — 'go' is an
/// affirmative word of its own, so the B3 gate reads the WHOLE answer as HEAD does — the dev
/// workspace is created UNDER that root with a derived name, ANCHORED to the Spec, and the human is
/// asked NOTHING about the path. ONE question in the whole commit, and its three answers are the
/// closed set: a second question here would mean the rider needed a turn of its own. The folder is
/// EMPTY: this branch settles the WHERE, never the filling.
#[test]
fn nspec_yes_with_workspace_uses_the_configured_root_without_asking() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "nspec");
    let (project_id, spec_id) = draft_spec(&db);
    let root = temp_root("root");
    db.with_conn(|c| db::set_project_worktrees_root(c, &project_id, Some(&root.to_string_lossy())))
        .unwrap();

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut grills = 0;
    let mut steps = 0;
    let mut saw_the_three_options = false;
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => last = Some(json!({ "diff": "d" })),
            "judge" => {
                grills += 1;
                last = Some(json!({ "verdict": if grills < 1 { "more" } else { "complete" } }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                assert!(
                    !p.contains("Where should the dev workspace folder be created"),
                    "a configured worktrees root must be USED, never asked about: {out}"
                );
                let ans = if p.contains("finalize") {
                    "grill"
                } else if p.contains("author's question") {
                    "an answer"
                } else {
                    // THE COMMIT QUESTION — the three-answer consent this finding adds. Both
                    // affirmatives are words the gate accepts WHOLE; the rider is WHICH one was
                    // given, never text appended to one.
                    assert_eq!(
                        out["action"]["options"],
                        json!(["yes", "go", "no"]),
                        "the commit consent offers yes / go / no"
                    );
                    saw_the_three_options = true;
                    "go"
                };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway nyx_next loop");
    }

    assert!(saw_the_three_options, "the commit question was reached");
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_READY,
        "'go' takes the SAME ready transition a plain 'yes' does"
    );
    // The registered `root_path` is the NORMALIZED one (nyx's canonical spelling), so compare there.
    let want_root = crate::pathnorm::normalize(&root.to_string_lossy());
    let ws = db
        .with_conn(|c| db::list_workspaces(c, &project_id))
        .unwrap()
        .into_iter()
        .find(|w| w.root_path.starts_with(&want_root) && w.root_path != want_root)
        .expect(
            "a dev workspace was created UNDER the configured worktrees root, with a derived name",
        );
    assert!(
        std::path::Path::new(&ws.root_path).is_dir(),
        "the folder really exists on disk: {}",
        ws.root_path
    );
    assert_eq!(
        std::fs::read_dir(&ws.root_path).unwrap().count(),
        0,
        "NOTHING fills it — no clone, no worktree (assumed; idea 01M04W3RZ333NC01R216MNXAFK)"
    );
    let spec = db
        .with_conn(|c| db::get_spec(c, &spec_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        spec.workspace_id.as_deref(),
        Some(ws.id.as_str()),
        "the workspace is ANCHORED to the Spec — the spec_id of create_workspace, reused"
    );
    assert_eq!(
        spec.status,
        db::SPEC_STATUS_READY,
        "and the anchor moved no status: the Spec is ready before and after"
    );
}

/// HORS PERIMETRE ASSUME (2026-08-18) — THE YES-WITH-A-WORKSPACE ANSWER ('go') with NO
/// `worktrees_root` configured: the graph ASKS, before creating anything, for a ROOT — and DERIVES the
/// Spec's own subfolder under it, exactly as the configured-root branch does.
///
/// THE ASSERTION THIS TEST MAKES IS THE ONE FINDING `01M0B5AG0XR4BG6DSGEFGKW6C4` REVERSED. It used to
/// answer the question with a folder and demand the workspace be registered EXACTLY there — which is
/// what shipped: the question asked for a root ("Set it once with `worktrees_root`"), the user
/// answered `d:\projects\_worktrees`, and nyx registered THE ROOT ITSELF as the Spec's workspace,
/// silently (the folder already existed, so the mkdir had nothing to report) and permanently (every
/// later `go` on that project then collided with the row). The two branches must derive the SAME
/// subfolder from the SAME kind of value, so this one now asserts the derivation, not the echo.
#[test]
fn nspec_yes_with_workspace_asks_for_the_path_when_no_root_is_configured() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "nspec");
    let (project_id, spec_id) = draft_spec(&db);
    // No `worktrees_root` on the project — the default.
    let chosen = temp_root("asked").join("here");

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut grills = 0;
    let mut steps = 0;
    let mut asked_for_a_root = false;
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => last = Some(json!({ "diff": "d" })),
            "judge" => {
                grills += 1;
                last = Some(json!({ "verdict": if grills < 1 { "more" } else { "complete" } }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("Under WHICH ROOT FOLDER") {
                    asked_for_a_root = true;
                    // The question must SAY it wants the parent, not the workspace folder: the
                    // incident was a human answering the question as worded (finding
                    // 01M0B5AG0XR4BG6DSGEFGKW6C4, done-criterion 2).
                    assert!(
                        p.contains("PARENT folder") && p.contains("subfolder named after the Spec"),
                        "the question names the value it wants — a ROOT — and says what nyx does \
                         with it: {p}"
                    );
                    assert!(
                        !chosen.exists(),
                        "nyx asks BEFORE it creates anything on disk"
                    );
                    chosen.to_string_lossy().into_owned()
                } else if p.contains("finalize") {
                    "grill".to_string()
                } else if p.contains("author's question") {
                    "an answer".to_string()
                } else {
                    "go".to_string()
                };
                last = answer(&db, &out, &ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway nyx_next loop");
    }

    assert!(
        asked_for_a_root,
        "with no configured root, the graph ASKS where the workspace goes"
    );
    let want =
        crate::pathnorm::normalize(&chosen.join(derived_name(&db, &spec_id)).to_string_lossy());
    let ws = db
        .with_conn(|c| db::list_workspaces(c, &project_id))
        .unwrap()
        .into_iter()
        .find(|w| w.root_path == want)
        .expect("the workspace was created in the Spec's OWN subfolder under the answered root");
    assert!(
        !db.with_conn(|c| db::list_workspaces(c, &project_id))
            .unwrap()
            .iter()
            .any(|w| w.root_path == crate::pathnorm::normalize(&chosen.to_string_lossy())),
        "and NOT on the root itself — that is the row that blocked every later run"
    );
    assert!(
        std::path::Path::new(&ws.root_path).is_dir(),
        "and the folder was created on disk"
    );
    let spec = db
        .with_conn(|c| db::get_spec(c, &spec_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        spec.workspace_id.as_deref(),
        Some(ws.id.as_str()),
        "anchored to the Spec, through the same create_workspace spec_id"
    );
}

/// A SECOND draft Spec on an EXISTING project — what a project looks like when a human runs /nspec
/// twice. Returns its id (its `number` is the project's next, which is what makes the derived
/// workspace names differ).
fn draft_spec_in(db: &Db, project_id: &str, title: &str) -> String {
    db.with_conn(|c| {
        let (spec, _rev) = db::create_spec(
            c,
            project_id,
            title,
            SpecSections {
                problem: "p".into(),
                solution: "s".into(),
                ..Default::default()
            },
        )
        .unwrap();
        spec.id
    })
}

/// Drive a WHOLE /nspec run on `spec_id` to its `done` tick, answering the commit question with
/// `commit` ("yes" / "go" / "no") and the workspace-root question — asked only when the project
/// configured none — with `root`. Returns the final `done` action, whose `notes` carry what the walk
/// had to say.
fn drive_nspec(
    db: &Db,
    rt: &EngineRuntime,
    wf_id: &str,
    spec_id: &str,
    commit: &str,
    root: Option<&str>,
) -> Value {
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, wf_id, Some(spec_id), None, "{}"))
        .unwrap()
        .id;
    let mut last: Option<Value> = None;
    let mut grills = 0;
    let mut steps = 0;
    loop {
        let out = tick(db, rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => return out,
            "spawn_subagent" => last = Some(json!({ "diff": "d" })),
            "judge" => {
                grills += 1;
                last = Some(json!({ "verdict": if grills < 1 { "more" } else { "complete" } }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("Under WHICH ROOT FOLDER") {
                    root.expect("this run was not expected to ask where workspaces go")
                        .to_string()
                } else if p.contains("finalize") {
                    "grill".to_string()
                } else if p.contains("author's question") {
                    "an answer".to_string()
                } else {
                    commit.to_string()
                };
                last = answer(db, &out, &ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway nyx_next loop");
    }
}

/// FINDING `01M0B5AG0XR4BG6DSGEFGKW6C4`, done-criterion 4 — THE SCENARIO THAT MUST WORK: two `go` runs
/// on the SAME project, both answering the SAME root, open TWO DISTINCT workspaces in two DIFFERENT
/// derived subfolders. No collision, and neither of them is the root.
///
/// It is the déroulé that broke in production, verbatim. The first `go` registered the answered root
/// ITSELF as the workspace (the answer was taken as the final path, no name joined); the second then
/// died on `invalid_state` "a workspace with this path already exists in the project" — and so did
/// every attempt after it, whatever the human answered, because the row occupying the root was never
/// going to move. Deriving `<project key>-<number>` under the root on BOTH branches is what makes two
/// runs on one root land in two folders.
#[test]
fn two_go_runs_under_one_root_open_two_distinct_workspaces() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "nspec");
    let (project_id, first) = draft_spec(&db);
    let second = draft_spec_in(&db, &project_id, "S2");
    // NO configured root: both runs are ASKED, and both answer the same root — the branch the
    // incident took, and the one whose derivation was missing.
    let root = temp_root("two-go");
    let answered = root.to_string_lossy().into_owned();

    let rt = EngineRuntime::new();
    drive_nspec(&db, &rt, &wf.id, &first, "go", Some(&answered));
    drive_nspec(&db, &rt, &wf.id, &second, "go", Some(&answered));

    for spec_id in [&first, &second] {
        assert_eq!(
            spec_status(&db, spec_id),
            db::SPEC_STATUS_READY,
            "both 'go' runs committed their Spec — the second was not refused"
        );
    }
    let normalized_root = crate::pathnorm::normalize(&answered);
    let workspaces = db
        .with_conn(|c| db::list_workspaces(c, &project_id))
        .unwrap();
    let anchored = |spec_id: &str| -> db::Workspace {
        let spec = db
            .with_conn(|c| db::get_spec(c, spec_id))
            .unwrap()
            .expect("the Spec exists");
        let id = spec
            .workspace_id
            .expect("the 'go' run anchored a workspace on its Spec");
        workspaces
            .iter()
            .find(|w| w.id == id)
            .cloned()
            .expect("the anchored workspace belongs to the project")
    };
    let (w1, w2) = (anchored(&first), anchored(&second));
    assert_ne!(
        w1.root_path, w2.root_path,
        "two Specs, two DIFFERENT folders under the one root"
    );
    for (spec_id, w) in [(&first, &w1), (&second, &w2)] {
        assert_eq!(
            w.root_path,
            crate::pathnorm::normalize(&root.join(derived_name(&db, spec_id)).to_string_lossy()),
            "each workspace is the Spec's OWN subfolder, derived from the answered root"
        );
        assert!(
            std::path::Path::new(&w.root_path).is_dir(),
            "and it really exists on disk: {}",
            w.root_path
        );
    }
    assert!(
        !workspaces.iter().any(|w| w.root_path == normalized_root),
        "and NOTHING was ever registered on the root itself — the row that froze the project"
    );
}

/// FINDING `01M0B5AG0XR4BG6DSGEFGKW6C4`, done-criterion 5 — 'go' IS ALL-OR-NOTHING: when the workspace
/// cannot be opened, the Spec does NOT come out `ready` without one.
///
/// The collision is set up the way a first `go` leaves it: a workspace of the project already occupies
/// the very folder this run would derive. Under the old shape the sequence was
/// `[transition "ready", …, create_workspace]`, so the transition was COMMITTED before the creation
/// was even attempted, and the failure left the Spec `ready` with no workspace — half of "ready AND a
/// workspace", which nobody asked for. The prevalidation now runs BEFORE the transition, so both
/// halves are refused together, out loud.
#[test]
fn a_go_that_cannot_open_its_workspace_leaves_the_spec_a_draft() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "nspec");
    let (project_id, spec_id) = draft_spec(&db);
    let root = temp_root("taken");
    db.with_conn(|c| db::set_project_worktrees_root(c, &project_id, Some(&root.to_string_lossy())))
        .unwrap();
    // The folder this run WOULD derive is already this project's workspace.
    let taken = root.join(derived_name(&db, &spec_id));
    db.with_conn(|c| db::create_workspace(c, &project_id, "squatter", &taken.to_string_lossy()))
        .unwrap();
    let before = db
        .with_conn(|c| db::list_workspaces(c, &project_id))
        .unwrap()
        .len();

    let rt = EngineRuntime::new();
    let out = drive_nspec(&db, &rt, &wf.id, &spec_id, "go", None);

    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_DRAFT,
        "the transition did NOT fire: no workspace, no ready"
    );
    let spec = db
        .with_conn(|c| db::get_spec(c, &spec_id))
        .unwrap()
        .unwrap();
    assert_eq!(spec.workspace_id, None, "and nothing was anchored to it");
    let workspaces = db
        .with_conn(|c| db::list_workspaces(c, &project_id))
        .unwrap();
    assert_eq!(
        workspaces.len(),
        before,
        "no workspace row was written either — the project holds exactly what it held"
    );
    // AND IT SAID SO. A refusal nobody can read is the silent half-state under another name, so the
    // fact's own reason rides the run's last word, naming the folder and what holds it.
    let notes = out["action"]["notes"].to_string();
    assert!(
        notes.contains("is ALREADY the workspace") && notes.contains("squatter"),
        "the run names the folder it refused and the workspace holding it: {notes}"
    );
    assert!(
        notes.contains("still a draft"),
        "and says what state the Spec is left in: {notes}"
    );
}

/// N13(b) — nspec on a READY Spec takes the FORK branch: query → confirm revision → the `fork_spec`
/// node actually FORKS it into a new editable draft → the grill reworks it → commit back to ready.
/// The Spec ends `ready` with a BUMPED revision — proof the fork happened (not a cosmetic note).
#[test]
fn nspec_forks_a_ready_spec_then_recommits_it() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "nspec");
    let (_project, spec_id) = ready_spec(&db);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;
    let before = spec_rev(&db, &spec_id);

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut grills = 0;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => last = Some(json!({ "diff": "d" })),
            "judge" => {
                grills += 1;
                last = Some(json!({ "verdict": if grills < 2 { "more" } else { "complete" } }));
            }
            "ask_human" => {
                // The grill 'steer' takes any answer; the two confirmations (revise / ready) take 'yes'.
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("author's question") { "an answer" } else { "yes" };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway nyx_next loop");
    }
    assert!(
        spec_rev(&db, &spec_id) > before,
        "the fork_spec node bumped the revision — the fork actually ran (N13(b))"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_READY,
        "the reworked Spec was re-committed to ready"
    );
}

/// N16 — a DECLINED confirmation has a CLEAN EXIT: answering 'no' to "confirm ready" routes to the
/// "left as draft" note and the run reaches `done` with the Spec still a DRAFT. It does NOT freeze on
/// a refusing transition (the exact bug N16 fixes — reproduced as two dead runs before this).
#[test]
fn nspec_declining_confirm_ready_leaves_a_draft_without_freezing() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "nspec");
    let (_project, spec_id) = draft_spec(&db);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut grills = 0;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => last = Some(json!({ "diff": "d" })),
            "judge" => {
                grills += 1;
                last = Some(json!({ "verdict": if grills < 1 { "more" } else { "complete" } }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("finalize") {
                    "grill"
                } else if p.contains("author's question") {
                    "an answer"
                } else {
                    "no" // DECLINE "confirm ready"
                };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "a declined confirm must NOT freeze the run (N16)");
    }
    assert_eq!(
        run_row(&db, &run_id).status,
        db::RUN_STATUS_DONE,
        "the run completed cleanly — a declined confirmation did not freeze it (N16)"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_DRAFT,
        "declining 'confirm ready' leaves the Spec a draft"
    );
}

/// H4 — a prototyper SUB-WORKFLOW that DIED does not let nspec walk on to the ready commit. The
/// child ends `failed` (exactly what was observed live: the parent's next tick answered
/// `{"blackboard_delta":{"proto_result":{"status":"failed"}}, "action":{"type":"done"}}` and the flow
/// went on to mark the Spec ready on a design nobody settled). The "proto result?" selector now TESTS
/// that status, asks the human, and their 'hold' stops the flow AT the commit: the run ends cleanly
/// `done`, the Spec stays a DRAFT, and "confirm ready" is never even proposed.
#[test]
fn nspec_stops_at_the_commit_when_the_prototyper_child_dies() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "nspec");
    let (_project, spec_id) = draft_spec(&db);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut asked_about_the_dead_proto = false;
    let mut confirm_ready_proposed = false;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => last = Some(json!({ "diff": "d" })),
            "judge" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                // The proto judge sees a UI surface; the grill judge ends the grill at once.
                last = Some(json!({
                    "verdict": if p.contains("graphical") { "ui" } else { "complete" }
                }));
            }
            // The prototyper child: KILL it (the live failure was a dead child run), then re-tick the
            // parent — which re-reads the CHILD STATUS, the source of truth.
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                db.with_conn(|c| {
                    db::update_workflow_run(c, &child, None, "{}", db::RUN_STATUS_FAILED, None)
                })
                .unwrap();
                last = None;
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("did not complete") {
                    asked_about_the_dead_proto = true;
                    "hold"
                } else if p.contains("finalize") {
                    "grill"
                } else if p.contains("author's question") {
                    "an answer"
                } else {
                    if p.contains("Mark the Spec ready") {
                        confirm_ready_proposed = true;
                    }
                    // 'go through a proto?' — and, were it ever reached, 'confirm ready'.
                    "yes"
                };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "a dead prototyper child must NOT freeze the run either");
    }

    assert!(
        asked_about_the_dead_proto,
        "the dead child is a FACT the graph tests — the human is asked, not walked past"
    );
    assert!(
        !confirm_ready_proposed,
        "'hold' stops the flow AT the commit — the ready confirmation is never even proposed"
    );
    assert_eq!(
        run_row(&db, &run_id).status,
        db::RUN_STATUS_DONE,
        "the run still ends cleanly (the stop is a route, not a freeze)"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_DRAFT,
        "a Spec whose prototype died is NOT marked ready"
    );
}

/// Drive a bound ndev run to `done`, answering every review ask with `review_answer` (the audit
/// always passes; every confirm ask takes 'yes'). Drives the coder sub-workflow child each build pass.
fn drive_ndev_review(db: &Db, run_id: &str, review_answer: &str) {
    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut steps = 0;
    loop {
        let out = tick(db, &rt, run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                drive_coder_child(db, &rt, &child);
                last = None;
            }
            "spawn_subagent" => last = Some(json!({ "diff": "d" })),
            "judge" => last = Some(json!({ "verdict": "ready" })),
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("in review") { review_answer } else { "yes" };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway nyx_next loop");
    }
}

/// N12(d) — a cancel is reached ONLY by an EXPLICIT review answer, never by fall-through. A reviewer
/// who answers 'cancel' drives the Spec to `cancelled`; an UNRECOGNISED answer hits the SAFE default
/// note and cancels nothing (the Spec is simply left in review).
#[test]
fn ndev_cancels_only_from_an_explicit_review_answer() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");

    // (a) an explicit 'cancel' → the Spec is cancelled.
    let (_p, spec_a) = ready_spec(&db);
    let run_a = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_a), None, "{}"))
        .unwrap()
        .id;
    drive_ndev_review(&db, &run_a, "cancel");
    assert_eq!(
        spec_status(&db, &spec_a),
        db::SPEC_STATUS_CANCELLED,
        "an explicit 'cancel' review answer cancels the Spec"
    );

    // (b) 'hold' → the SAFE default note (N13(a)): it does NOT loop-then-fail; the run ends cleanly
    // with the Spec left in review (the review is a single ask_human, not a self-defeating loop). An
    // OFF-vocabulary answer is instead refused + re-asked — that N19 behaviour is covered separately.
    let (_p3, spec_c) = ready_spec(&db);
    let run_c = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_c), None, "{}"))
        .unwrap()
        .id;
    drive_ndev_review(&db, &run_c, "hold");
    assert_eq!(
        run_row(&db, &run_c).status,
        db::RUN_STATUS_DONE,
        "'hold' ends the run cleanly — no LoopLimit failure (N13(a))"
    );
    assert_eq!(
        spec_status(&db, &spec_c),
        db::SPEC_STATUS_IN_REVIEW,
        "'hold' leaves the Spec in review"
    );
}

/// N17(c) — a Spec with NO open work has an ENTRY DOOR: /ndev SKIPS the build (the gate reads
/// `spec_open_tasks` — there is nothing left to build) and resumes straight at the review, then
/// completes. Driven on an already-in_review, fully-drained Spec: the coder is NEVER re-spawned, the
/// illegal `in_review → in_review` is never attempted (the guarded transition routes to its note), and
/// the Spec ends `completed`.
#[test]
fn ndev_resumes_an_in_review_spec_without_rebuilding() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (_project, spec_id) = spec_in_review(&db);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut coder_spawns = 0;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => {
                if out["action"]["agent_id"] == "coder" {
                    coder_spawns += 1;
                }
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("in review") { "approve" } else { "yes" };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway nyx_next loop");
    }
    assert_eq!(
        coder_spawns, 0,
        "a drained Spec skips the build — the coder is not re-spawned (N17c entry door)"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_COMPLETED,
        "it resumes at the review and completes"
    );
}

/// The NAMED-FACT rule — a condition tests THE FACT IT NAMES. The STEPPED case (per-phase review: the
/// phases built so far are in review, the rest still `todo`) BUILDS the remaining work instead of
/// skipping it. Under the old gate — `spec_can_transition(in_review) → can_build`, a PROXY for "is
/// there work to build?" — such a run had NO route at all: `review` is legal only FROM `in_progress`,
/// so the gate stayed false forever, and `done` was refused while tasks remained (N32). The build gate
/// now reads `spec_open_tasks`, so the build RUNS; the end-of-build transition is `spec_can_transition`-
/// GUARDED, so the illegal `in_review → in_review` is never attempted (it routes to a clean note — had
/// the transition been attempted, `nyx_next` would have refused it and `tick` would panic); and the
/// review then completes the Spec.
#[test]
fn ndev_builds_a_stepped_in_review_spec_that_still_has_open_tasks() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (_project, spec_id) = spec_in_review_with_tasks(&db, 2);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut coder_spawns = 0;
    let mut children = 0;
    let mut notes: Vec<String> = Vec::new();
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        // G4 — a note rides the action it precedes (the `done` included) instead of stopping the run.
        if let Some(riding) = out["action"]["notes"].as_array() {
            notes.extend(riding.iter().map(|n| n.as_str().unwrap_or("").to_string()));
        }
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "sub_workflow" => {
                // The BUILD branch was taken on an in_review Spec — the whole point of the fix.
                assert_eq!(
                    spec_status(&db, &spec_id),
                    db::SPEC_STATUS_IN_REVIEW,
                    "the Spec builds while it is already in review (the stepped case)"
                );
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                children += 1;
                coder_spawns += drive_coder_child(&db, &rt, &child);
                last = None;
            }
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "auditor");
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("in review") { "approve" } else { "yes" };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway nyx_next loop");
    }
    assert_eq!(children, 1, "one coder child drove the remaining phase");
    assert_eq!(
        coder_spawns, 2,
        "the BUILD ran on the in_review Spec — both remaining tasks were driven (not skipped)"
    );
    assert!(
        notes.iter().any(|t| t.contains("cannot enter review")),
        "the guarded transition routed to its clean note instead of an illegal in_review → in_review: {notes:?}"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_COMPLETED,
        "the stepped Spec finished its build and completed at the review — it had a route"
    );
}

/// F7 — the parent must never count work the child cannot see. A Spec whose ONLY open work is a
/// PHASE-LESS review finding is exactly that case: ndev's build gate reads `spec_open_tasks` (every
/// non-terminal task, no phase filter) and opens a coder child, while the child's loop reads
/// `phase_next_task` / `phase_open_tasks` — which saw nothing, because a finding carries no phase. The
/// child exited without building, the outer `phases` loop recounted the same finding and opened
/// another child, and the run spun to `max_iterations` on work nobody could reach.
///
/// Now the child SELECTS the finding, drives it (start → coder → gate → done) and the loop drains: one
/// child, real coder work, the finding closed, and the run ends `done` well inside its step budget.
#[test]
fn ndev_builds_a_phase_less_review_finding_instead_of_spinning() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (project_id, spec_id) = spec_in_review(&db);
    // The finding is work for the coder child, so its gate must be armed like a real project's.
    arm_coder_gate(&db, &project_id);
    let finding = file_unphased_finding(&db, &project_id, &spec_id, "story unbuilt");
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut coder_spawns = 0;
    let mut children = 0;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                children += 1;
                coder_spawns += drive_coder_child(&db, &rt, &child);
                last = None;
            }
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "auditor");
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("in review") { "approve" } else { "yes" };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "runaway nyx_next loop — the parent is counting work the child cannot see");
    }

    assert_eq!(
        task_status(&db, &finding),
        db::TASK_STATUS_DONE,
        "the coder child WORKED the phase-less finding instead of exiting on it"
    );
    assert_eq!(children, 1, "one coder child drained the open work — no re-open spin");
    assert_eq!(coder_spawns, 1, "the child spawned the coder for the finding (it built something)");
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}

/// THE TWO TEXTS AROUND THE AUDIT VERDICT SAY WHAT THE VERDICT MEASURES. `spec_open_findings` counts
/// only the findings that REOPEN a build ([`db::reopens_the_build`]), so an audit that filed four
/// LOWS derives `0` and lands on the "audit clean" note — the branch whose text used to claim the
/// audit "filed no finding". That claim is now false in a case the graph reaches routinely, and a
/// note the human reads is not a place to be approximately right. Same for the rework ask: it names
/// a count the human is being asked to spend a build round on, so it must say which findings it
/// counted and what became of the rest.
#[test]
fn the_ndev_audit_texts_name_the_severities_its_verdict_counts() {
    let db = seeded_db();
    let (_wf, nodes) = builtin(&db, "ndev");
    let text_of = |label: &str| {
        let n = nodes
            .iter()
            .find(|n| n.label == label)
            .unwrap_or_else(|| panic!("the ndev graph has a '{label}' node"));
        let cfg: Value = serde_json::from_str(&n.config).expect("node config");
        cfg["text"]
            .as_str()
            .or_else(|| cfg["prompt"].as_str())
            .expect("a human-facing text")
            .to_string()
    };

    // The CLEAN branch is reached with lows still open, so it must not claim the audit found nothing…
    let clean = text_of("audit clean");
    assert!(
        clean.contains("no high/medium finding"),
        "the clean note says which severities it cleared: {clean}"
    );
    assert!(
        clean.contains("low finding") && clean.contains("stays open"),
        "…and says the lows it filed are still on the Spec, not that there were none: {clean}"
    );

    // …and the budget question names the severities its count came from.
    let rework = text_of("rework or abandon");
    assert!(
        rework.contains("high/medium severity"),
        "the rework ask names what it counted: {rework}"
    );
    assert!(
        rework.contains("never re-opens a build"),
        "…and what a low does instead, since that is the spend the human is ruling on: {rework}"
    );
}

/// THE CLOSING QUESTION SAYS WHAT COMPLETING LEAVES BEHIND. A build whose audit filed only LOWS
/// derives a `0` verdict and exits — by design, a low never buys another round — and the Spec then
/// reaches the review carrying those findings still open. The human was nevertheless asked the bare
/// "Mark the Spec completed?", and only learned what it shipped with from the note that fires AFTER
/// the transition. A consent given without knowing what it leaves open is not an informed consent, so
/// the count is read one node before the question and interpolated INTO it — exactly what the "rework
/// or abandon" ask already does with `{{audit_verdict}}`.
#[test]
fn the_completion_question_names_the_findings_the_spec_would_ship_with() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (project_id, spec_id) = ready_spec_with_tasks(&db, 1);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut audit_passes = 0;
    let mut completion_prompt = String::new();
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                drive_coder_child(&db, &rt, &child);
                last = None;
            }
            "spawn_subagent" => {
                // The audit files TWO LOWS on its first pass. They are real findings and they stay
                // open — but a low never re-opens a build, so the derived verdict is 0 and the build
                // exits with them on the Spec. That is exactly the state the closing question is
                // asked in.
                if out["action"]["subject"] == "user_stories" {
                    audit_passes += 1;
                    if audit_passes == 1 {
                        for title in ["naming nit", "a stale comment"] {
                            db.with_conn(|c| {
                                db::create_task(
                                    c,
                                    &project_id,
                                    title,
                                    db::NewTaskInput {
                                        spec_id: Some(spec_id.clone()),
                                        phase_name: Some("Build".to_string()),
                                        ..db::NewTaskInput::review(db::TASK_SEVERITY_LOW)
                                    },
                                )
                            })
                            .expect("a low finding");
                        }
                    }
                }
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("").to_string();
                let ans = if p.contains("in review") {
                    "approve"
                } else {
                    completion_prompt = p;
                    "yes"
                };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 60, "runaway nyx_next loop");
    }

    assert!(
        completion_prompt.contains("2 open low-severity finding(s)"),
        "the question the human answers must NAME what completing ships with, interpolated from \
         nyx's own count: {completion_prompt}"
    );
    assert!(
        completion_prompt.contains("stay on the Spec"),
        "…and what becomes of them, since that is the whole of what the consent covers: \
         {completion_prompt}"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_COMPLETED,
        "and the informed 'yes' still completes the Spec — the question changed, not the route"
    );
    assert_eq!(
        db.with_conn(|c| db::list_review_findings_for_spec(c, &spec_id))
            .unwrap()
            .iter()
            .filter(|t| db::is_deferred_finding(t))
            .count(),
        2,
        "the two lows are still open on the completed Spec — the question said so beforehand"
    );
}

/// H9 (the FIRST live occurrence) — a `kind=human` task reaches a RESOLUTION instead of freezing the
/// run. "Provision PostgreSQL and expose DATABASE_URL" is an action reserved for the user: the coder
/// refuses it (out of mandate) and the unconditional `transition_task done` the graph used to apply
/// was refused too — "a kind=human task closes only through the verified path" — leaving the child run
/// blocked and re-raising that same `invalid_state` on EVERY later tick.
///
/// The graph now reads the close route BEFORE the work: a task that closes through `verify` is the
/// human's, so the coder is never spawned on it — nyx asks the human, and closes it through the
/// VERIFIED path once they say they performed it. The whole /ndev completes.
#[test]
fn a_human_task_is_settled_by_the_human_and_closed_through_the_verified_path() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (_project_id, spec_id, human_id) = ready_spec_with_a_human_task(&db);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut child_spawns: Vec<(String, String)> = Vec::new();
    let mut asked_the_human_for_their_action = false;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                child_spawns.extend(drive_coder_child_answering(&db, &rt, &child, |action| {
                    match action["type"].as_str().unwrap() {
                        // Reported for completeness — the assertions below prove the coder is never
                        // spawned on an action reserved for the user.
                        "spawn_subagent" => json!({ "diff": "d" }),
                        _ => {
                            let prompt = action["prompt"].as_str().unwrap_or("");
                            assert!(
                                prompt.contains("Provision PostgreSQL"),
                                "the question names the task it is about: {prompt}"
                            );
                            asked_the_human_for_their_action = true;
                            json!({ "decision": "done" })
                        }
                    }
                }));
                last = None;
            }
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "auditor");
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("in review") { "approve" } else { "yes" };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "a kind=human task must not park the run on a refused close");
    }

    assert!(
        asked_the_human_for_their_action,
        "the human is ASKED for the action reserved for them"
    );
    assert!(
        !child_spawns.iter().any(|(agent, _)| agent == "coder"),
        "the coder is NEVER spawned on a kind=human task (its own manual refuses it): {child_spawns:?}"
    );
    assert_eq!(
        task_status(&db, &human_id),
        db::TASK_STATUS_DONE,
        "the human action was closed through the VERIFIED path"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_COMPLETED);
}

/// H9 (the SECOND live occurrence) + H12 — a task the coder BLOCKED escalates to the human, and their
/// 'spec' ruling AUTHORIZES the one Spec-prose correction the freeze otherwise forbids.
///
/// The live sequence, reproduced: the audit files a finding, the coder fixes the code, and the next
/// round finds the SPEC now wrong about the code (`implementation_decisions` promises `GET /healthz`
/// where the code — rightly — serves `GET|HEAD`). The coder blocks the task, the graph's `done` is
/// refused ("task is 'blocked', which cannot transition to 'done'"), the run re-raises it forever, and
/// nobody — not even the human — can edit the Spec's text while it is `in_progress`.
///
/// Now: the blocked task routes to the escalation gate, 'spec' opens a run-scoped, engine-written
/// window, the spawned Spec author corrects the section IN PLACE through it, the note right after
/// closes it again, and the coder retries the task on the next pass and closes it. Three writes are
/// attempted around that window and only the middle one is allowed.
#[test]
fn a_blocked_task_escalates_and_the_human_alone_opens_the_spec_freeze() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (project_id, spec_id) = ready_spec_with_tasks(&db, 0);
    // The ONLY open work is the finding, so it IS the task the child drives. This Spec has NO phase
    // plan (nothing was ever planned into one), so the finding names no phase — an auditor does not
    // get to mint one on a Spec that has left `draft`.
    let finding =
        file_unphased_finding(&db, &project_id, &spec_id, "the Spec promises GET /healthz exactly");
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    // The three writes the window is measured by: refused before (no run_id names a human's ruling),
    // ALLOWED inside the authorized spawn, refused again once the note closed it.
    let write_section = |run: Option<&str>| -> Result<(), String> {
        let mut args = json!({
            "spec_id": spec_id,
            "section": "implementation_decisions",
            "value": "The health endpoint answers GET and HEAD.",
        });
        if let Some(run) = run {
            args.as_object_mut().unwrap().insert("run_id".into(), json!(run));
        }
        crate::mcp_tools_spec::spec_update_section(&db, &args)
            .map(|_| ())
            .map_err(|e| format!("{:?}: {}", e.code, e.message))
    };

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut blocked_once = false;
    let mut escalated = false;
    let mut refused_without_the_grant = String::new();
    let mut refused_after_the_window = String::new();
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                drive_coder_child_answering(&db, &rt, &child, |action| {
                    match action["type"].as_str().unwrap() {
                        // The CODER: on its first pass it BLOCKS the finding — the contradiction is in
                        // the Spec's text, and the coder never redefines the Spec.
                        "spawn_subagent" if action["agent_id"] == "coder" => {
                            if !blocked_once {
                                blocked_once = true;
                                db.with_conn(|c| {
                                    db::transition_task(
                                        c,
                                        &finding,
                                        db::TaskTransition::Block,
                                        Some("the contradiction is in the TEXT of the Spec: it promises GET /healthz exactly, the code serves GET|HEAD"),
                                    )
                                })
                                .expect("the coder's refusal");
                            } else {
                                // The RETRY pass, in the SAME still-live child: the window closed on
                                // the note that followed the authorized spawn, so this write — same
                                // run, same Spec — is refused again.
                                refused_after_the_window =
                                    write_section(Some(&child)).unwrap_err();
                            }
                            json!({ "diff": "d" })
                        }
                        // The SPEC AUTHOR nyx spawns on the 'spec' ruling — this spawn IS the window.
                        "spawn_subagent" => {
                            assert_eq!(action["agent_id"], "spec", "the escalation spawns the Spec author");
                            refused_without_the_grant = write_section(None).unwrap_err();
                            write_section(Some(&child))
                                .expect("the authorized correction goes through");
                            json!({ "diff": "spec corrected" })
                        }
                        _ => {
                            let prompt = action["prompt"].as_str().unwrap_or("");
                            assert!(
                                prompt.contains("contradiction is in the TEXT"),
                                "the escalation states the contradiction: {prompt}"
                            );
                            escalated = true;
                            json!({ "decision": "spec" })
                        }
                    }
                });
                last = None;
            }
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "auditor");
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("in review") { "approve" } else { "yes" };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "a blocked task must not park the run on a refused close");
    }

    assert!(escalated, "the blocked task reached the human's escalation gate");
    assert!(
        refused_without_the_grant.contains("frozen"),
        "without a run naming a human's ruling, the freeze holds: {refused_without_the_grant}"
    );
    assert!(
        refused_after_the_window.contains("frozen"),
        "and it holds again once the note closed the window: {refused_after_the_window}"
    );
    let rev = db
        .with_conn(|c| db::get_active_revision(c, &spec_id))
        .unwrap()
        .expect("an active revision");
    assert_eq!(
        rev.implementation_decisions, "The health endpoint answers GET and HEAD.",
        "the authorized correction actually landed on the Spec's active revision"
    );
    assert_eq!(
        task_status(&db, &finding),
        db::TASK_STATUS_DONE,
        "the coder retried the task after the ruling and closed it"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}

/// H12, the DEFER route — a finding the human chooses not to act on now is CLOSED with that reason
/// (so the build can finish) after nyx spawns the Spec author to record it as a backlog idea: the
/// debt is traced, not dropped, and the run does not stall on it.
#[test]
fn a_deferred_blocked_task_is_closed_with_its_reason_after_the_idea_is_recorded() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndev");
    let (project_id, spec_id) = ready_spec_with_tasks(&db, 0);
    // The ONLY open work is the finding, so it IS the task the child drives — and it names no phase,
    // this Spec having no plan to name one from.
    let finding = file_unphased_finding(&db, &project_id, &spec_id, "needs an access nobody has");
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut blocked_once = false;
    let mut child_spawns: Vec<(String, String)> = Vec::new();
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "sub_workflow" => {
                let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                child_spawns.extend(drive_coder_child_answering(&db, &rt, &child, |action| {
                    match action["type"].as_str().unwrap() {
                        "spawn_subagent" if action["agent_id"] == "coder" => {
                            if !blocked_once {
                                blocked_once = true;
                                db.with_conn(|c| {
                                    db::transition_task(
                                        c,
                                        &finding,
                                        db::TaskTransition::Block,
                                        Some("no access to the target environment"),
                                    )
                                })
                                .expect("the coder's refusal");
                            }
                            json!({ "diff": "d" })
                        }
                        "spawn_subagent" => json!({ "diff": "idea recorded" }),
                        _ => json!({ "decision": "defer" }),
                    }
                }));
                last = None;
            }
            "spawn_subagent" => {
                assert_eq!(out["action"]["agent_id"], "auditor");
                last = Some(json!({ "diff": "d" }));
            }
            "ask_human" => {
                let p = out["action"]["prompt"].as_str().unwrap_or("");
                let ans = if p.contains("in review") { "approve" } else { "yes" };
                last = answer(&db, &out, ans);
            }
            other => panic!("unexpected surfaced action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "a deferred task must not stall the build either");
    }

    assert!(
        child_spawns
            .iter()
            .any(|(agent, label)| agent == "spec" && label.contains("backlog idea")),
        "the debt is TRACED: nyx spawns the Spec author to record it: {child_spawns:?}"
    );
    let task = db.with_conn(|c| db::get_task(c, &finding)).unwrap().unwrap();
    assert_eq!(
        task.status,
        db::TASK_STATUS_CANCELLED,
        "the deferred finding is CLOSED, so the build can finish"
    );
    assert!(
        task.skip_reason.unwrap_or_default().contains("deferred by the human"),
        "and it is closed WITH the reason it was closed for"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}

/// ndoc's `query` guard routes on the REAL Spec status: a DRAFT (not completed) Spec skips the sync
/// entirely — the "not ready" note is surfaced and the doc effector is never spawned (B2).
#[test]
fn ndoc_query_guard_skips_a_non_completed_spec() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndoc");
    let (_project, spec_id) = draft_spec(&db); // draft ⇒ NOT completed
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    let out = tick(&db, &rt, &run_id, None);
    assert_eq!(
        out["action"]["type"], "done",
        "a non-completed Spec skips the whole sync — the doc effector is never spawned"
    );
    assert!(out["action"]["notes"][0]
        .as_str()
        .unwrap()
        .contains("not completed"));
    assert_eq!(
        out["blackboard_delta"]["spec_status"], "draft",
        "the query wrote the REAL status onto the blackboard (in the tick delta — N22)"
    );
}

/// J1, the OBSERVED failure, reproduced on the seeded ndoc built-in: the human reviews the doc-sync
/// with "changes — the DEL fix is right but it draws the parser's line in the wrong place". That is a
/// REQUEST FOR CHANGES, and it must never end as a green run that dropped it.
///
/// It used to. The `review` question declared no `options`, so the free text was folded onto
/// `decision`; the loop's `until decision != "changes"` was TRUE (the text is not `"changes"`) so the
/// sync EXITED instead of looping, and the fork's catch-all wrote "Doc sync abandoned" — the run
/// reported done with the correction never handled. Now the answer set is declared, so nyx REFUSES
/// the reply and re-asks the identical question; the human's second, on-vocabulary answer re-runs the
/// doc effector, which is what they were asking for all along.
#[test]
fn ndoc_review_refuses_a_free_text_change_request_instead_of_abandoning() {
    let db = seeded_db();
    let (wf, _n) = builtin(&db, "ndoc");
    let spec_id = completed_spec(&db);
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;

    let rt = EngineRuntime::new();
    // The doc effector syncs, then the run parks on the review.
    let synced = tick(&db, &rt, &run_id, None);
    assert_eq!(synced["action"]["type"], "spawn_subagent");
    assert_eq!(synced["action"]["agent_id"], "doc");
    let asked = tick(&db, &rt, &run_id, Some(json!({ "diff": "README +160" })));
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(
        asked["action"]["options"],
        json!(["approve", "changes", "cancel"]),
        "the question states the closed set its own loop and fork compare by equality"
    );

    // THE ANSWER FROM THE REAL SESSION — a request for changes, in the human's own words. It is put
    // to them and they reply, which 'changes' requires (J2: it buys another doc-sync round).
    let re_asked = {
        let last = answer(
            &db,
            &asked,
            "changes — the DEL fix is right but it draws the parser's line in the wrong place",
        );
        tick(&db, &rt, &run_id, last)
    };
    assert_eq!(
        re_asked["action"]["type"], "ask_human",
        "the off-vocabulary reply is REFUSED and the SAME question comes back (N19) — the run does \
         not advance on a value nothing constrains"
    );
    assert_eq!(
        re_asked["action"]["prompt"], asked["action"]["prompt"],
        "the identical question, so the human can answer it in the vocabulary the graph honours"
    );
    let board: Value = serde_json::from_str(&run_row(&db, &run_id).blackboard).unwrap();
    assert!(
        board.get("decision").is_none(),
        "and NOTHING was folded onto the key the branches read: {board}"
    );
    assert_ne!(
        run_row(&db, &run_id).status,
        db::RUN_STATUS_DONE,
        "the run is still parked on the question, not finished"
    );

    // The human re-answers in the vocabulary: 'changes' RE-RUNS the doc effector — the correction is
    // handled, which is the whole thing the old graph dropped.
    let resynced = tick(&db, &rt, &run_id, answer(&db, &re_asked, "changes"));
    assert_eq!(
        resynced["action"]["type"], "spawn_subagent",
        "'changes' loops the sync back through the doc effector"
    );
    let asked_again = tick(&db, &rt, &run_id, Some(json!({ "diff": "README fixed" })));
    assert_eq!(asked_again["action"]["type"], "ask_human");
    let done = tick(&db, &rt, &run_id, answer(&db, &asked_again, "approve"));
    assert_eq!(done["action"]["type"], "done");
    assert!(
        done["action"]["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("approved")),
        "the run ends on the APPROVED note — never on the abandon it used to fall into: {done}"
    );
}

/// J1 — ndoc's terminal fork NAMES the cancel it acts on: "Doc sync abandoned" is reached by the human
/// SAYING cancel, and by nothing else. It used to be the selector's catch-all, which is how an
/// unrecognised answer got read as an abandon (N12(d): a destructive outcome is never fallen into).
#[test]
fn ndoc_abandon_is_reached_only_by_an_explicit_cancel() {
    let db = seeded_db();
    let (_wf, nodes) = builtin(&db, "ndoc");
    let finish = nodes
        .iter()
        .find(|n| n.label == "finish or abandon")
        .expect("the sync's terminal fork");
    let cfg: Value = serde_json::from_str(&finish.config).unwrap();
    assert_eq!(
        cfg["branches"],
        json!([
            { "cond": { "var": "decision", "eq": "approve" }, "child": 0 },
            { "cond": { "var": "decision", "eq": "cancel" }, "child": 1 },
        ]),
        "every branch TESTS the answer it acts on — no catch-all stands in for one",
    );

    // And the deroulé: 'cancel' is what reaches the abandon.
    let spec_id = completed_spec(&db);
    let (wf, _n) = builtin(&db, "ndoc");
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}"))
        .unwrap()
        .id;
    let rt = EngineRuntime::new();
    tick(&db, &rt, &run_id, None);
    let asked = tick(&db, &rt, &run_id, Some(json!({ "diff": "d" })));
    let done = tick(&db, &rt, &run_id, answer(&db, &asked, "cancel"));
    assert_eq!(done["action"]["type"], "done");
    assert!(
        done["action"]["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("abandoned")),
        "the abandon note is what an explicit cancel reaches: {done}"
    );
}

/// J1 — the AUDIT of the five built-ins, held by the graphs themselves: every `ask_human` whose answer
/// a `selector` or a `loop` BRANCHES on declares its closed answer set. The rule is derived
/// ([`Tree::ask_human_unconstrained_branch`]), so this passes for a fork and a custom graph too — what
/// it pins here is that no built-in ships the ndoc defect again.
#[test]
fn no_builtin_branches_on_an_unconstrained_answer() {
    let db = seeded_db();
    for scope in ["ndev", "nspec", "ndoc", "coder", "prototyper"] {
        let (wf, nodes) = builtin(&db, scope);
        let tree = Tree::from_rows(&nodes, wf.root_node_id.as_deref()).expect("a valid tree");
        for n in nodes.iter().filter(|n| n.type_ == db::NODE_TYPE_ASK_HUMAN) {
            assert!(
                tree.ask_human_unconstrained_branch(&n.id).is_none(),
                "{scope}: ask_human '{}' is branched on but declares no options",
                n.label
            );
        }
    }
}

/// J2 — WHICH of the built-ins' questions the human alone may answer, read off the graphs by the
/// derivation itself. The set is not a list anyone maintains: it is every ask whose answer disposes
/// of something the session does not own — a member of [`crate::engine::SensitiveEffect`] (the Spec's
/// lifecycle, a new revision of it, a folder on the user's disk), or another round of sub-agent spend.
///
/// IT IS ALSO THE REGRESSION WITNESS for finding `01M0C3BZPWV832D802C0GZFTMC`: the set below is read
/// off the SHIPPED graphs, so a sensitive node type that the runtime stops recognising shows up here
/// as a question that quietly left the list.
///
/// ndev's "rework or abandon" is the one J2 exists for: it commits no transition, so it used to be
/// answerable by the session alone, and one live `/ndev` did exactly that — 3 h 35, $69.48, over
/// rounds that after the first only produced second-order findings. "coder blocked" writes the same
/// loop key and is the same decision after a dead child; ndoc's "review" buys another doc-sync pass
/// the same way.
///
/// AND THE PROTOTYPER APPEARS NOWHERE BELOW, WHICH IS A GAP THE DERIVATION CANNOT SEE (L2). Its
/// "feedback" {lock/changes} DOES buy another prototype round — 'changes' re-enters a loop body that
/// spawns the effector — but [`Tree::ask_human_commitment`] classifies an ask by whether it WRITES
/// the key a loop exits on, and this loop exits on `design_locked`, a FACT written by a `query`. So
/// no answer here is `AnotherRound`, and a session can buy the rounds on its own. The derivation is
/// not wrong — it measures what it names — but a loop whose exit is a fact hides its spend from it.
/// Closing that means giving the design-rounds loop an exit the human's answer can reach, which is a
/// change to what ends a design (a `design_locked` freeze, and nothing else), not a fix to make in
/// passing.
#[test]
fn the_builtin_questions_a_human_alone_may_answer_are_the_ones_that_commit() {
    use crate::engine::HumanCommitment;

    let db = seeded_db();
    let mut committing: Vec<(&str, String, HumanCommitment)> = Vec::new();
    for scope in ["ndev", "nspec", "ndoc", "coder", "prototyper"] {
        let (wf, nodes) = builtin(&db, scope);
        let tree = Tree::from_rows(&nodes, wf.root_node_id.as_deref()).expect("a valid tree");
        for n in nodes.iter().filter(|n| n.type_ == db::NODE_TYPE_ASK_HUMAN) {
            if let Some(commitment) = tree.ask_human_commitment(&n.id) {
                committing.push((scope, n.label.clone(), commitment));
            }
        }
    }
    committing.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));

    assert_eq!(
        committing,
        vec![
            // The BUILD ROUNDS (J2) — each re-opens a loop whose body opens a coder child / the doc
            // effector, so answering it buys another sub-agent pass.
            ("ndev", "coder blocked".to_string(), HumanCommitment::AnotherRound),
            // The Spec's LIFECYCLE (H6) — ndev's two `guarded_commit` confirms, one per terminal
            // commit (cancel / done).
            ("ndev", "confirm".to_string(), HumanCommitment::SpecTransition),
            ("ndev", "confirm".to_string(), HumanCommitment::SpecTransition),
            // AND "review" BESIDE THEM — the price of [`crate::engine::Tree::answer_decides`] now
            // evaluating BOTH of its readings instead of short-circuiting on `gate_on`. `review`
            // answers 'approve', and that answer is what routes the run into the `done`
            // `guarded_commit`; the transition down there gates on its own `done_ok`, so the old
            // early return said "review decides nothing" and stopped. It decides whether the run ever
            // reaches the question that commits, which is a decision, so it is in — and being in
            // costs exactly one human turn while committing nothing extra. The direction is the safe
            // one the widening was made for; the ordinary-vs-committing line moved on purpose here,
            // and NOWHERE ELSE in the five built-ins (this list is the proof of that: `review` is the
            // only addition, and nothing left).
            ("ndev", "review".to_string(), HumanCommitment::SpecTransition),
            ("ndev", "rework or abandon".to_string(), HumanCommitment::AnotherRound),
            ("ndoc", "review".to_string(), HumanCommitment::AnotherRound),
            ("nspec", "confirm ready".to_string(), HumanCommitment::SpecTransition),
            // FINDING `01M0C3BZPWV832D802C0GZFTMC` — THE SHIPPED HOLE, AND THE WITNESS THAT IT IS
            // SHUT. "This Spec is already READY. Create a new revision to rework it?" writes
            // `revise_ok`, and `seed.rs`'s `fork_spec {gate_on: "revise_ok"}` fires on it. Until the
            // two layers shared [`crate::engine::SensitiveEffect`], the authoring rule certified that
            // fork HUMAN-GATED while the runtime classified the question as ordinary: it rendered
            // `None` here, carried no `human_turn_required`, opened no `PendingConsent` — so a
            // session could answer "yes" by itself and fork a ready Spec.
            //
            // `CreateWorkspace` does NOT appear beside "confirm ready", and that is now for the
            // RIGHT reason. The seeded `create_workspace` declares `gate_on: "workspace_free"` — a
            // QUERY fact, not an answer — and `answer_decides` used to stop there and report that no
            // question governs the folder; the human turn survived only because the `transition`
            // sharing that branch gates on `ready_ok`, a coincidence of THIS graph. Both readings are
            // evaluated now, so `ready_ok` genuinely governs the mkdir too; `CreateWorkspace` is
            // absent only because [`crate::engine::SensitiveEffect::gravity`] tells the human about
            // the GRAVER of the two, and a Spec move outranks a folder.
            ("nspec", "confirm revision".to_string(), HumanCommitment::CreateRevision),
        ],
        "the questions the human alone answers, and what each of them commits",
    );
}

// --- finding #34: the thin flows spawn a REAL effector role ----------------

/// DONE-CRITERION ("build_nspec spawn agent_id='spec' et build_ndoc spawn agent_id='doc' ; les
/// nœuds spawn référencent des AgentRole valides ; un seed frais montre {{sub_agent:spec}}/
/// {{sub_agent:doc}}"): a FRESH seed spawns the real effector — nspec → `spec`, ndoc → `doc` — never
/// the workflow name, and ndev's spawns stay coder/auditor (untouched).
#[test]
fn fresh_thin_flows_spawn_the_real_effector_role() {
    let db = seeded_db();
    assert_eq!(
        thin_spawn(&db, "nspec").1,
        "spec",
        "nspec spawns the spec effector (AgentRole::Spec), not the workflow name",
    );
    assert_eq!(
        thin_spawn(&db, "ndoc").1,
        "doc",
        "ndoc spawns the doc effector (AgentRole::Doc), not the workflow name",
    );
    // ndev now spawns ONLY the auditor: the coder moved INTO the coder sub-workflow child (lot 2).
    let (_, ndev) = builtin(&db, "ndev");
    let ndev_spawns: HashSet<String> = ndev
        .iter()
        .filter(|n| n.type_ == db::NODE_TYPE_SPAWN_SUBAGENT)
        .map(|n| {
            serde_json::from_str::<Value>(&n.config).unwrap()["agent_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        ndev_spawns,
        HashSet::from(["auditor".to_string()]),
        "ndev spawns ONLY the auditor now — the coder moved into the coder sub-workflow",
    );
    // ndev DELEGATES the build to a coder sub-workflow (a `sub_workflow` node of scope `coder`).
    let has_coder_sub = ndev.iter().any(|n| {
        n.type_ == db::NODE_TYPE_SUB_WORKFLOW
            && serde_json::from_str::<Value>(&n.config).unwrap()["scope"] == json!("coder")
    });
    assert!(
        has_coder_sub,
        "ndev delegates the build to a sub_workflow of scope 'coder'",
    );
}

// --- The command SLOT: what keeps a shared graph shared ----------------------

/// A SEEDED graph may never name a command instance id, because a command instance id belongs to ONE
/// project and these graphs belong to all of them. Naming one left a project exactly one way to arm
/// its build check — FORK the built-in, which `ensure_builtin` never upgrades and
/// `resolve_active_workflow` prefers — so the first gesture a real project made cut it off from every
/// later graph fix (a live fork sat at r11 while the built-in shipped r16).
///
/// This is the invariant that keeps that from coming back by accident: EVERY `gate_check` in EVERY
/// built-in declares a SLOT and names no `command_ids`. It is not about today's single gate — it is
/// about the next one somebody adds.
#[test]
fn no_seeded_gate_names_a_command_only_the_slot_a_project_binds() {
    let db = seeded_db();
    let mut gates = 0;
    for scope in [
        db::WORKFLOW_SCOPE_NDEV,
        db::WORKFLOW_SCOPE_NSPEC,
        db::WORKFLOW_SCOPE_NDOC,
        db::WORKFLOW_SCOPE_CODER,
        db::WORKFLOW_SCOPE_PROTOTYPER,
    ] {
        let (wf, nodes) = builtin(&db, scope);
        for gate in nodes.iter().filter(|n| n.type_ == db::NODE_TYPE_GATE_CHECK) {
            gates += 1;
            let cfg: crate::engine::GateConfig =
                serde_json::from_str(&gate.config).expect("a decodable gate config");
            assert!(
                cfg.command_ids.is_empty(),
                "gate '{}' of the built-in '{}' names command ids ({:?}) — project data on a graph \
                 every project shares, which can only be armed by forking",
                gate.label,
                wf.name,
                cfg.command_ids,
            );
            assert!(
                cfg.slot.is_some(),
                "gate '{}' of the built-in '{}' declares no slot, so no project can arm it at all",
                gate.label,
                wf.name,
            );
        }
    }
    assert_eq!(gates, 1, "exactly one seeded gate today — the coder's `checks`");
}

/// The coder's `checks` gate, precisely: BLOCKING (so an unbound project is refused, not waved
/// through) and declaring [`BUILD_CHECK_SLOT`] — the name the refusal tells the project to bind, so
/// the two must be the same string or the instruction points at nothing.
#[test]
fn the_seeded_coder_gate_declares_the_build_check_slot() {
    let db = seeded_db();
    let (_wf, nodes) = builtin(&db, db::WORKFLOW_SCOPE_CODER);
    let gate = nodes
        .iter()
        .find(|n| n.type_ == db::NODE_TYPE_GATE_CHECK)
        .expect("the coder's checks gate");
    assert_eq!(gate.label, "checks");
    let cfg: Value = serde_json::from_str(&gate.config).expect("gate config is JSON");
    assert_eq!(cfg["slot"], json!(BUILD_CHECK_SLOT));
    assert_eq!(cfg["blocking"], json!(true));
    assert!(
        cfg.get("command_ids").is_none(),
        "and it carries no command list at all: {cfg}",
    );
}

// --- N11: an existing built-in is UPGRADED when the code revision is newer -----

/// DONE-CRITERION (v2.8 review N11): a built-in seeded at an OLDER revision (a pre-N11 DB has a NULL
/// `builtin_revision`, which reads as 0 < the code revision) is UPGRADED on the next boot — its tree
/// is REPLACED by the current `build_*` graph and the revision re-stamped — so a default-graph change
/// reaches an existing DB. A PROJECT FORK of the built-in is the user's customisation and is NOT
/// touched. A re-boot at the same revision is a no-op (idempotent).
#[test]
fn an_existing_built_in_is_upgraded_when_the_code_revision_is_newer() {
    let db = seeded_db();
    let (fresh_wf, fresh_nodes) = builtin(&db, "nspec");
    let full_count = fresh_nodes.len();
    assert!(
        full_count > 3,
        "the current nspec is a COMPLETE graph, not a thin one"
    );
    assert_eq!(
        fresh_wf.builtin_revision,
        Some(super::BUILTIN_REVISION),
        "a fresh seed stamps the current revision"
    );

    // A project FORK of nspec — the user's customisation, which must NOT be upgraded.
    let (project, _root) = db
        .with_conn(|c| db::create_project(c, "P", "/tmp/n11", None))
        .unwrap();
    let fork_id = db
        .with_conn(|c| {
            db::create_workflow(
                c,
                Some(&project.id),
                "my nspec",
                db::WORKFLOW_SCOPE_NSPEC,
                Some(&fresh_wf.id),
            )
        })
        .unwrap()
        .id;
    let fork_stub = db
        .with_conn(|c| node(c, &fork_id, None, 0, db::NODE_TYPE_NOTE, "mine", json!({ "text": "mine" })))
        .unwrap();
    db.with_conn(|c| db::set_workflow_root(c, &fork_id, Some(&fork_stub)))
        .unwrap();

    // Simulate a PRE-N11 built-in: shrink its tree to a stub AND set an OLDER revision (0 = what a
    // NULL `builtin_revision` reads as via `unwrap_or(0)`).
    db.with_conn(|c| db::delete_nodes_for_workflow(c, &fresh_wf.id))
        .unwrap();
    let old_root = db
        .with_conn(|c| node(c, &fresh_wf.id, None, 0, db::NODE_TYPE_NOTE, "old", json!({ "text": "old" })))
        .unwrap();
    db.with_conn(|c| db::set_workflow_root(c, &fresh_wf.id, Some(&old_root)))
        .unwrap();
    db.with_conn(|c| db::set_builtin_revision(c, &fresh_wf.id, 0))
        .unwrap();
    assert_eq!(
        builtin(&db, "nspec").1.len(),
        1,
        "precondition: the built-in was shrunk to a stub"
    );

    // A re-boot re-runs the seed → the stale built-in is REPLACED with the current graph.
    db.with_conn(seed_builtin_workflows).unwrap();
    let (upgraded, up_nodes) = builtin(&db, "nspec");
    assert_eq!(
        up_nodes.len(),
        full_count,
        "the built-in tree was rebuilt to the current graph"
    );
    let types: HashSet<&str> = up_nodes.iter().map(|n| n.type_.as_str()).collect();
    assert!(
        types.contains(db::NODE_TYPE_LOOP) && types.contains(db::NODE_TYPE_SELECTOR),
        "the rebuilt tree is the complete graph"
    );
    assert_eq!(
        upgraded.builtin_revision,
        Some(super::BUILTIN_REVISION),
        "the revision is re-stamped"
    );
    assert!(upgraded.root_node_id.is_some(), "the root is re-pointed");

    // The project FORK is untouched (still its single stub).
    let fork_nodes = db.with_conn(|c| db::list_nodes(c, &fork_id)).unwrap();
    assert_eq!(
        fork_nodes.len(),
        1,
        "the project fork is the user's customisation — never upgraded"
    );

    // Idempotent: another boot at the same revision leaves the built-in as-is.
    db.with_conn(seed_builtin_workflows).unwrap();
    assert_eq!(builtin(&db, "nspec").1.len(), full_count);
}

/// M4 — the upgrade above DESTROYS any live run walking the built-in (the wipe nulls its cursor and
/// strands its traversal). We still upgrade (the built-in is shared by every project), but the loss
/// is no longer incidental: each run that actually LOSES something is MARKED with what replaced its
/// graph, so `run_start` can name the cause. A run that has never ticked loses nothing (it restarts
/// from the root of the NEW tree) and a terminal run is already over — neither is marked.
#[test]
fn upgrading_a_built_in_records_the_graph_replacement_on_its_live_runs() {
    let db = seeded_db();
    let (wf, _) = builtin(&db, "nspec");

    // Three runs on the built-in: one that has WALKED, one never ticked, one already finished.
    let mk = |cursor: Option<&str>, traversal: Option<&str>, status: &str| {
        db.with_conn(|c| {
            let run = db::create_workflow_run(c, &wf.id, None, cursor, "{\"seen\":1}").unwrap();
            db::update_workflow_run(c, &run.id, cursor, "{\"seen\":1}", status, traversal).unwrap();
            run.id
        })
    };
    let walked = mk(
        wf.root_node_id.as_deref(),
        Some(r#"{"stack":[],"awaiting":null,"replay":false}"#),
        db::RUN_STATUS_RUNNING,
    );
    let fresh = mk(None, None, db::RUN_STATUS_RUNNING);
    let finished = mk(
        wf.root_node_id.as_deref(),
        Some(r#"{"stack":[],"awaiting":null,"replay":false}"#),
        db::RUN_STATUS_DONE,
    );

    // Make the built-in stale, then re-boot → it is UPGRADED (and the runs walk a graph that is gone).
    db.with_conn(|c| db::set_builtin_revision(c, &wf.id, 0)).unwrap();
    db.with_conn(seed_builtin_workflows).unwrap();
    assert_eq!(
        builtin(&db, "nspec").0.builtin_revision,
        Some(super::BUILTIN_REVISION),
        "the upgrade still happens — it is never deferred to a live run"
    );

    let board = |run_id: &str| -> Value {
        serde_json::from_str(
            &db.with_conn(|c| db::get_workflow_run(c, run_id))
                .unwrap()
                .expect("run row")
                .blackboard,
        )
        .expect("a JSON blackboard")
    };
    let marker = &board(&walked)[db::RUN_GRAPH_REPLACED_KEY];
    assert_eq!(marker["cause"], "builtin_upgrade", "the walking run is MARKED: {marker}");
    assert_eq!(marker["from_revision"], json!(0), "with the revision it was on");
    assert_eq!(marker["to_revision"], json!(super::BUILTIN_REVISION), "and the one replacing it");
    assert_eq!(board(&walked)["seen"], json!(1), "the run's own data is preserved");
    assert!(
        board(&fresh)[db::RUN_GRAPH_REPLACED_KEY].is_null(),
        "a run that never ticked loses nothing — not marked"
    );
    assert!(
        board(&finished)[db::RUN_GRAPH_REPLACED_KEY].is_null(),
        "a terminal run is already over — not marked"
    );
}

/// THE SWEEP the `answer_decides` correction called for, turned into an assertion instead of a
/// once-read: every SENSITIVE node of the five built-ins, the `gate_on` key it fires on, and WHICH
/// node types in that graph can put a value under that key.
///
/// The shape this pins is `seed.rs`'s `create_workspace {gate_on: "workspace_free"}` — a gate hanging
/// on a key a `query` fills, not on an answer. It is legal and INTENTIONAL there (the prevalidation
/// and the `ready` transition beside it are the all-or-nothing pair `/nspec`'s `go` needs), but it is
/// exactly the shape the short-circuiting `answer_decides` misread, and the one whose consent lives in
/// the SELECTOR above rather than in the gate. So the exception is named here, once: a SECOND
/// sensitive node acquiring a non-answer gate must be a deliberate act, visible in this list, and not
/// a line slipped into a graph builder.
#[test]
fn only_one_seeded_sensitive_gate_hangs_on_something_no_question_writes() {
    use crate::engine::{board_key_root, NodeType, SensitiveEffect};

    let db = seeded_db();
    let mut gates: Vec<(String, String, String, String)> = Vec::new();
    for scope in ["ndev", "nspec", "ndoc", "coder", "prototyper"] {
        let (_, nodes) = builtin(&db, scope);
        for n in &nodes {
            let Some(node_type) = NodeType::from_db(&n.type_) else {
                continue;
            };
            let cfg: Value = serde_json::from_str(&n.config).unwrap_or_else(|_| json!({}));
            if SensitiveEffect::of(node_type, &cfg).is_none() {
                continue;
            }
            let gate = SensitiveEffect::gate_key_root(&cfg)
                .map(str::to_string)
                .unwrap_or_else(|| "<UNGATED>".to_string());
            // WHO can fill that key: the node types whose `output_key` / `write` names it.
            let mut by: Vec<String> = nodes
                .iter()
                .filter(|w| {
                    let wcfg: Value = serde_json::from_str(&w.config).unwrap_or_else(|_| json!({}));
                    ["output_key", "write"].iter().any(|field| {
                        wcfg.get(field)
                            .and_then(Value::as_str)
                            .map(|s| board_key_root(s) == gate)
                            .unwrap_or(false)
                    })
                })
                .map(|w| w.type_.clone())
                .collect();
            by.sort();
            by.dedup();
            gates.push((scope.to_string(), n.label.clone(), gate, by.join("+")));
        }
    }
    gates.sort();

    let ask = db::NODE_TYPE_ASK_HUMAN;
    let query = db::NODE_TYPE_QUERY;
    assert_eq!(
        gates,
        vec![
            // ndev's two terminal commits — each gated on the answer of the `confirm` question
            // standing right above it in its own `guarded_commit` sequence.
            ("ndev", "cancel", "cancel_ok", ask),
            ("ndev", "done", "done_ok", ask),
            // THE ONE EXCEPTION, and it is a `query`: `/nspec`'s `go` arm gates the mkdir on the
            // FEASIBILITY fact, and puts the human's consent in the selector one level up
            // (`ready_ok eq "go"`). Both readings of `answer_decides` are needed to see it.
            ("nspec", "dev workspace", "workspace_free", query),
            ("nspec", "fork", "revise_ok", ask),
            // The two `ready` transitions — the plain 'yes' arm and the one inside the 'go' sequence.
            ("nspec", "ready", "ready_ok", ask),
            ("nspec", "ready", "ready_ok", ask),
        ]
        .into_iter()
        .map(|(s, l, g, w): (&str, &str, &str, &str)| {
            (s.to_string(), l.to_string(), g.to_string(), w.to_string())
        })
        .collect::<Vec<_>>(),
        "every sensitive gate of the built-ins, and what fills the key it reads",
    );
}
