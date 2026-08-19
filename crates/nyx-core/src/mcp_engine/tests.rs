//! Tests for the `nyx_next` runtime tick (Phase 3, tasks `01KY71J0Q1DZX4MYQ6D6M1Y2VN` — the verb —
//! and `01KY71J1AKNE9SQQVW2DNVEQB5` — the handler enforcement).
//!
//! These drive the REAL handler (`super::nyx_next`) over a REAL in-memory DB and a REAL PTY gate
//! ([`crate::command::CommandPty`]) — the exact primitives the P1 spike proved — with NO provider
//! hook anywhere in the loop: the enforcement is anchored on the verb, so a red gate never yields
//! "advance" and a forbidden transition is refused in the handler.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;
use crate::db::{self, Db, SpecSections, SpecTransition};

// ---------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------

/// Serialize DB-touching tests (mirrors the other MCP tool test modules).
fn db_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// RAII temp dir removed on drop — the sandbox the gate runs in (its cwd).
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
        "nyx-v2-8-engine-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// A project whose ROOT workspace is an on-disk sandbox dir (so a gate can run there). Returns the
/// project id, workspace id, and the workspace's resolved on-disk path.
fn seed_project(db: &Db, dir: &Path) -> (String, String, String) {
    db.with_conn(|c| {
        let (project, root) =
            db::create_project(c, "Engine", dir.to_str().unwrap(), None).expect("project");
        (project.id, root.id.clone(), root.root_path)
    })
}

/// [`gate_command`] whose template carries a PERSISTED environment (`env` stored verbatim, so a
/// test can also feed it a MALFORMED value on purpose).
fn gate_command_with_env(db: &Db, project_id: &str, cmdline: &str, env: &str) -> String {
    let instance_id = gate_command(db, project_id, cmdline);
    db.with_conn(|c| {
        let command_id = db::get_instance(c, &instance_id)
            .expect("instance")
            .expect("materialized")
            .command_id;
        db::set_template_env(c, &command_id, Some(env)).expect("persist the gate command env");
    });
    instance_id
}

/// Create a gate managed command (`cmdline`, run at the project's root workspace) and return the
/// materialized instance id — the `command_id` a `gate_check` node references.
fn gate_command(db: &Db, project_id: &str, cmdline: &str) -> String {
    // A UNIQUE template name per cmdline so a test can create several gates in one project (the
    // template name is unique per project).
    let name = format!("gate-{}", cmdline.replace(|c: char| !c.is_alphanumeric(), "_"));
    db.with_conn(|c| {
        let tpl = db::create_template(
            c,
            project_id,
            &name,
            cmdline,
            None,
            db::CommandSource::default(),
        )
        .expect("gate template");
        db::instance_ids_for_template(c, &tpl.id)
            .expect("gate instances")
            .into_iter()
            .next()
            .expect("one gate instance materialized")
    })
}

/// A Spec driven to `in_progress` (so a `transition(review)` can fire), anchored on `workspace_id`.
fn spec_in_progress(db: &Db, project_id: &str, workspace_id: &str) -> String {
    db.with_conn(|c| {
        let (spec, _rev) = db::create_spec(
            c,
            project_id,
            "Engine spec",
            SpecSections {
                problem: "the loop must hold".into(),
                solution: "walk the tree".into(),
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

/// POSE a Spec's phase plan, the way its author does before the build starts. The tests below drive
/// a Spec that is ALREADY `in_progress`, where a task may only name a phase the plan already holds
/// (K2) — so the plan is written here rather than minted by the first task that mentions a name.
fn declare_phases(db: &Db, spec_id: &str, names: &[&str]) {
    let plan: Vec<Value> = names
        .iter()
        .enumerate()
        .map(|(i, name)| json!({ "number": i + 1, "name": name }))
        .collect();
    db.with_conn(|c| db::set_spec_phases(c, spec_id, &Value::Array(plan).to_string()))
        .expect("phase plan");
}

/// A fresh `draft` Spec (for the forbidden-transition test).
fn spec_draft(db: &Db, project_id: &str) -> String {
    db.with_conn(|c| {
        db::create_spec(
            c,
            project_id,
            "Draft spec",
            SpecSections {
                problem: "p".into(),
                solution: "s".into(),
                ..Default::default()
            },
        )
        .expect("spec")
        .0
        .id
    })
}

/// Add a node, returning its id.
fn add_node(
    db: &Db,
    workflow_id: &str,
    parent: Option<&str>,
    order: i32,
    node_type: &str,
    config: Value,
) -> String {
    db.with_conn(|c| {
        db::create_node(
            c,
            workflow_id,
            parent,
            order,
            node_type,
            node_type,
            &config.to_string(),
        )
        .expect("node")
        .id
    })
}

/// Create a workflow bound to `spec_id`, returning `(workflow_id, run_id)`. The tree is built by
/// `build` against the workflow id; the run starts un-cursored with an empty blackboard.
fn workflow_run(db: &Db, spec_id: Option<&str>, build: impl FnOnce(&Db, &str)) -> (String, String) {
    let workflow_id = db.with_conn(|c| {
        db::create_workflow(c, None, "wf", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("workflow")
            .id
    });
    build(db, &workflow_id);
    let run_id = db.with_conn(|c| {
        db::create_workflow_run(c, &workflow_id, spec_id, None, "{}")
            .expect("run")
            .id
    });
    (workflow_id, run_id)
}

/// One `nyx_next` call for `run_id`, optionally carrying `last_result`. Panics on a handler error
/// (the caller asserts on the returned action).
fn tick(db: &Db, rt: &EngineRuntime, run_id: &str, last: Option<Value>) -> Value {
    let mut args = json!({ "run_id": run_id });
    if let Some(last) = last {
        args.as_object_mut()
            .unwrap()
            .insert("last_result".into(), last);
    }
    super::nyx_next(db, rt, no_session(), &args)
        .expect("nyx_next")
        .result
}

/// The CALLER SESSION a tick comes from when the test simulates NO human at the keyboard: no
/// terminal at all. nyx can attribute no human turn to it, which is what every test that is not
/// about consent wants — and what makes a consent answer refused by default.
fn no_session() -> super::CallerSession<'static> {
    super::CallerSession { terminal_id: None }
}

/// The persisted run row.
fn run_row(db: &Db, run_id: &str) -> db::WorkflowRun {
    db.with_conn(|c| db::get_workflow_run(c, run_id))
        .unwrap()
        .unwrap()
}

fn spec_status(db: &Db, spec_id: &str) -> String {
    db.with_conn(|c| db::get_spec(c, spec_id))
        .unwrap()
        .unwrap()
        .status
}

/// A READY Spec on `project_id` (draft → Ready).
fn spec_ready(db: &Db, project_id: &str) -> String {
    db.with_conn(|c| {
        let (spec, _rev) = db::create_spec(
            c,
            project_id,
            "Ready spec",
            SpecSections {
                problem: "p".into(),
                solution: "s".into(),
                ..Default::default()
            },
        )
        .expect("spec");
        db::transition_spec(c, &spec.id, SpecTransition::Ready, true).expect("ready");
        spec.id
    })
}

/// The Spec's current (active) revision number — a fork bumps it.
fn spec_rev(db: &Db, spec_id: &str) -> i32 {
    db.with_conn(|c| db::get_spec(c, spec_id))
        .unwrap()
        .unwrap()
        .current_revision_number
}

// ---------------------------------------------------------------------------
// #4 — the tick advances correctly; a red gate never returns "advance"
// ---------------------------------------------------------------------------

/// THE integration proof (task #4): the full ndev-shaped loop
/// `sequence{ loop(until gate_green){ spawn(coder) → gate_check } → transition(review) }`
/// unrolls in ONE session through `nyx_next` — a RED gate forces a real RE-SPAWN, a GREEN gate
/// advances, the state transition fires, and the run reaches `done` with a coherent, persisted
/// blackboard. No provider hook anywhere; the gate is run by nyx over the PTY.
#[test]
fn nyx_next_drives_the_ndev_loop_respawn_on_red_then_advance_in_one_session() {
    let _g = db_guard();
    let sandbox = temp_dir("loop");
    let db = Db::in_memory();
    let (project_id, workspace_id, ws_path) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    // The gate: exit 0 only once the coder has produced the sentinel `gate_ok` (the "green fix").
    let gate = gate_command(&db, &project_id, "test -f gate_ok");

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        let phase = add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_LOOP,
            json!({ "until": { "var": "gate_green", "eq": true } }),
        );
        add_node(
            db,
            wf,
            Some(&phase),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
        add_node(
            db,
            wf,
            Some(&phase),
            1,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut spawns = 0;
    let mut actions: Vec<String> = Vec::new();
    loop {
        let result = tick(&db, &rt, &run_id, last.take());
        let ty = result["action"]["type"].as_str().unwrap().to_string();
        actions.push(ty.clone());
        match ty.as_str() {
            "spawn_subagent" => {
                assert_eq!(result["action"]["agent_id"], "coder");
                assert_eq!(result["status"], "running");
                // The cursor is persisted while the dispatcher works the sub-agent.
                assert!(result["current_node_id"].is_string());
                spawns += 1;
                // Only the SECOND coder fixes the code (green fix) — the first gate is red and
                // FORCES a real re-spawn.
                if spawns == 2 {
                    std::fs::write(format!("{ws_path}/gate_ok"), "ok").unwrap();
                }
                last = Some(json!({ "diff": format!("attempt {spawns}") }));
            }
            "done" => break,
            other => panic!("unexpected action {other}: {result}"),
        }
        assert!(spawns <= 5, "loop failed to converge: {actions:?}");
    }

    // spawn → (red gate) re-spawn → (green gate) advance → done: the red gate NEVER produced a
    // "done"/advance between the two spawns.
    assert_eq!(actions, vec!["spawn_subagent", "spawn_subagent", "done"]);
    assert_eq!(spawns, 2, "the red gate forced exactly one re-spawn");

    // The transition FIRED (nyx imposed it): the Spec advanced to in_review.
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_IN_REVIEW);

    // The run is done, un-cursored, with a coherent persisted blackboard.
    let row = run_row(&db, &run_id);
    assert_eq!(row.status, db::RUN_STATUS_DONE);
    assert!(row.current_node_id.is_none());
    let board: Value = serde_json::from_str(&row.blackboard).unwrap();
    assert_eq!(
        board["gate_green"],
        json!(true),
        "the last gate nyx ran was green"
    );
    assert_eq!(board["spawns"], json!(2), "two coder sub-agents recorded");
}

/// The isolated ENFORCEMENT invariant (task #4 done-criterion + task #5): on a graph that WOULD
/// otherwise advance past the gate (`sequence{ gate_check(blocking) → transition }`), a RED gate
/// makes `nyx_next` REFUSE — it never returns "advance"; the verb errors in its handler and the run
/// is parked `blocked`. Proven with NO provider hook.
#[test]
fn a_red_blocking_gate_makes_nyx_next_refuse_to_advance() {
    let _g = db_guard();
    let sandbox = temp_dir("redgate");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    // No sentinel is ever created → the gate stays RED.
    let gate = gate_command(&db, &project_id, "test -f never_created");

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state", "the progression verb is refused");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("gate_red")
    );

    // The Spec was NOT transitioned (nyx never advanced past the red gate), and the run is blocked.
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_IN_PROGRESS);
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_BLOCKED);
}

/// `nyx_next` returns the RIGHT action for each successive leaf state (task #4): a `judge`, then a
/// `spawn_subagent`, each with its decoded payload — the thin dispatcher gets one atomic instruction
/// per call. And the `note` the walk went past costs no call of its own: it RIDES the judge that
/// follows it, under `notes` (G4).
#[test]
fn nyx_next_returns_the_right_action_per_leaf() {
    let _g = db_guard();
    let sandbox = temp_dir("perleaf");
    let db = Db::in_memory();
    let (project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());
    let _ = project_id;

    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_NOTE,
            json!({ "text": "read me" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_JUDGE,
            json!({ "prompt": "is it clear?" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            2,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "auditor" }),
        );
    });

    let rt = EngineRuntime::new();

    let a = tick(&db, &rt, &run_id, None);
    assert_eq!(
        a["action"]["type"], "judge",
        "the note suspended nothing — the walk went straight to the judge"
    );
    assert_eq!(a["action"]["prompt"], "is it clear?");
    assert_eq!(
        a["action"]["notes"],
        json!(["read me"]),
        "the note rides the action it precedes"
    );

    let b = tick(&db, &rt, &run_id, Some(json!({ "verdict": "clair" })));
    assert_eq!(b["action"]["type"], "spawn_subagent");
    assert_eq!(b["action"]["agent_id"], "auditor");
    assert!(
        b["action"]["notes"].is_null(),
        "no note on the way here, no key at all: {b}"
    );

    // The judge verdict landed on the persisted blackboard (state carried forward).
    let c = tick(&db, &rt, &run_id, Some(json!({ "diff": "done" })));
    assert_eq!(c["action"]["type"], "done");
    let board: Value = serde_json::from_str(&run_row(&db, &run_id).blackboard).unwrap();
    assert_eq!(board["verdict"], "clair");
}

/// G4 — a `note` costs no ROUND-TRIP of its own. `sequence[ note, note, ask_human ]` used to take
/// THREE `nyx_next` calls, two of them handing back a sentence the dispatcher could only read and
/// re-tick past (one MCP call, one response, one conversation turn each). It now takes ONE: both notes
/// ACCUMULATE in graph order and ride the `ask_human` under `notes`, the cursor stops on the ask, and
/// the journal records ONE stop — the notes were walked PAST, not skipped.
#[test]
fn notes_ride_the_next_action_instead_of_costing_a_round_trip_each() {
    let _g = db_guard();
    let sandbox = temp_dir("notes");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let mut ask_id = String::new();
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_NOTE,
            json!({ "text": "No Spec yet — author a NEW one from scratch." }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_NOTE,
            json!({ "text": "and mind the phases" }),
        );
        ask_id = add_node(
            db,
            wf,
            Some(&root),
            2,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "ready?", "output_key": "d" }),
        );
    });

    let rt = EngineRuntime::new();
    let out = tick(&db, &rt, &run_id, None);
    assert_eq!(
        out["action"]["type"], "ask_human",
        "the first call already hands back the question, not a sentence: {out}"
    );
    assert_eq!(
        out["action"]["notes"],
        json!([
            "No Spec yet — author a NEW one from scratch.",
            "and mind the phases"
        ]),
        "both notes ride it, in graph order"
    );
    assert_eq!(
        out["current_node_id"],
        json!(ask_id),
        "the cursor advanced past both notes onto the ask"
    );
    assert_eq!(
        tick_rows(&db, &run_id)
            .iter()
            .map(|p| p["action_type"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["ask_human"],
        "ONE stop journalled — a note is not one"
    );

    // The answer folds as usual, and a run with nothing left to say carries no `notes` key at all.
    let done = tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(done["action"]["type"], "done");
    assert!(done["action"]["notes"].is_null(), "{done}");
}

/// G5 — NO action repeats the cursor as its own `node_id`. `action.node_id` and the tick's top-level
/// `current_node_id` were the same value at every stop by construction — two independent sources for
/// one fact. `current_node_id` is the ONE that answers "where is the run", so the action drops it,
/// across every leaf kind the dispatcher actually receives.
#[test]
fn no_action_repeats_the_cursor_as_its_own_node_id() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_JUDGE,
            json!({ "prompt": "clear?" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            2,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "ok?", "output_key": "d" }),
        );
        add_node(db, wf, Some(&root), 3, db::NODE_TYPE_PRODUCE, json!({}));
        add_node(db, wf, Some(&root), 4, db::NODE_TYPE_WAIT, json!({}));
    });

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        let ty = out["action"]["type"].as_str().unwrap().to_string();
        assert!(
            out["action"]["node_id"].is_null(),
            "the {ty} action names no node: {out}"
        );
        if ty == "done" {
            assert!(out["current_node_id"].is_null(), "a done run has no cursor");
            break;
        }
        assert!(
            out["current_node_id"].is_string(),
            "…because the tick itself says where the run is: {out}"
        );
        seen.push(ty);
        last = Some(json!({ "diff": "d", "verdict": "v", "decision": "yes", "artifact": "a" }));
        assert!(seen.len() < 10, "runaway loop: {seen:?}");
    }
    assert_eq!(
        seen,
        vec!["spawn_subagent", "judge", "ask_human", "produce", "wait"],
        "every dispatcher-facing leaf kind was exercised"
    );
}

/// A spawn node's per-node SUBJECT reaches the dispatcher: the `spawn_subagent` action carries the
/// `subject` the node declared, so the SAME role spawned over several objects is told WHICH one it is
/// about (ndev's four audit passes, one per Spec commitment section). A spawn WITHOUT a subject keeps
/// the exact pre-existing payload — no `subject` key at all, no regression for every existing spawn.
#[test]
fn a_spawn_node_hands_its_subject_to_the_dispatcher() {
    let _g = db_guard();
    let sandbox = temp_dir("subject");
    let db = Db::in_memory();
    let (_project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());

    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "auditor", "subject": "out_of_scope" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "auditor" }),
        );
    });

    let rt = EngineRuntime::new();

    let with_subject = tick(&db, &rt, &run_id, None);
    assert_eq!(with_subject["action"]["type"], "spawn_subagent");
    assert_eq!(with_subject["action"]["agent_id"], "auditor");
    assert_eq!(
        with_subject["action"]["subject"], "out_of_scope",
        "the node's subject rides the action — the dispatcher hands the sub-agent its focus"
    );

    let without = tick(&db, &rt, &run_id, Some(json!({ "diff": "d" })));
    assert_eq!(without["action"]["type"], "spawn_subagent");
    assert!(
        without["action"].get("subject").is_none(),
        "a spawn with no subject carries no subject key: {}",
        without["action"]
    );
}

// ---------------------------------------------------------------------------
// #5 — enforcement in the handler, hooks debranched, provider-independent
// ---------------------------------------------------------------------------

/// STATE RULE (task #5): a `transition` node routes through the guarded Spec authority, so a
/// FORBIDDEN transition (`draft → in_review`, no legal edge) is REFUSED in the `nyx_next` handler —
/// via `source_allowed`, with no provider hook — and the Spec is untouched.
#[test]
fn a_forbidden_transition_is_refused_in_the_handler() {
    let _g = db_guard();
    let sandbox = temp_dir("badtrans");
    let db = Db::in_memory();
    let (project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_draft(&db, &project_id); // stays draft

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(
        err.code, "invalid_state",
        "the illegal transition is refused (source_allowed)"
    );
    assert!(
        err.message.contains("cannot transition"),
        "the refusal names the illegal edge: {}",
        err.message
    );
    // The Spec never moved off draft, and the run is parked blocked.
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_DRAFT);
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_BLOCKED);
}

/// The gate enforcement is IDENTICAL whatever the provider (task #5): the SAME red-gate refusal
/// holds when the session declares `provider=claude` and when it declares `provider=opencode` —
/// the enforcement is anchored on the verb, not on any provider hook.
#[test]
fn gate_enforcement_is_identical_across_providers() {
    let _g = db_guard();
    let sandbox = temp_dir("crossprov");
    let db = Db::in_memory();
    let (project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());
    let gate = gate_command(&db, &project_id, "test -f never_created");

    let build = |db: &Db, wf: &str| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    };

    for provider in ["claude", "opencode"] {
        let (_wf, run_id) = workflow_run(&db, None, build);
        let rt = EngineRuntime::new();
        let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id, "provider": provider }))
            .unwrap_err();
        assert_eq!(
            err.code, "invalid_state",
            "provider {provider}: a red blocking gate refuses to advance",
        );
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
            Some("gate_red"),
            "provider {provider}: the refusal reason is identical",
        );
    }
}

/// The CONSOLIDATED enforcement proof (task #5), deterministic and HOOK-FREE: a single test that
/// asserts BOTH refusals happen IN THE HANDLER — (a) a progression verb is refused when a blocking
/// gate is RED, and (b) a progression verb is refused when the state transition is FORBIDDEN
/// (`source_allowed`). No `PreToolUse` / `SubagentStop` provider hook and no `AgentActivityStore` is
/// wired anywhere in this test: the enforcement is anchored on the `nyx_next` verb, so it holds with
/// the hooks entirely debranched — the hooks drive only the comfort signal, never the enforcement.
#[test]
fn enforcement_is_anchored_on_the_verb_not_on_any_hook() {
    let _g = db_guard();
    let sandbox = temp_dir("enforce");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());

    // (a) GATE RED → the progression verb is refused in the handler (no hook involved).
    let in_progress = spec_in_progress(&db, &project_id, &workspace_id);
    let gate = gate_command(&db, &project_id, "test -f never_created");
    let (_wf_a, run_a) = workflow_run(&db, Some(&in_progress), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });
    let rt_a = EngineRuntime::new();
    let err_a = super::nyx_next(&db, &rt_a, no_session(), &json!({ "run_id": run_a })).unwrap_err();
    assert_eq!(err_a.code, "invalid_state", "(a) red gate refuses the verb");
    assert_eq!(
        spec_status(&db, &in_progress),
        db::SPEC_STATUS_IN_PROGRESS,
        "(a) not advanced"
    );

    // (b) TRANSITION FORBIDDEN → the progression verb is refused in the handler (source_allowed).
    let draft = spec_draft(&db, &project_id);
    let (_wf_b, run_b) = workflow_run(&db, Some(&draft), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });
    let rt_b = EngineRuntime::new();
    let err_b = super::nyx_next(&db, &rt_b, no_session(), &json!({ "run_id": run_b })).unwrap_err();
    assert_eq!(
        err_b.code, "invalid_state",
        "(b) forbidden transition refuses the verb"
    );
    assert_eq!(
        spec_status(&db, &draft),
        db::SPEC_STATUS_DRAFT,
        "(b) not advanced"
    );
}

// ---------------------------------------------------------------------------
// #6 — the spawn_subagent action carries the resolved native/inline mode
// ---------------------------------------------------------------------------

/// The `nyx_next` handler wires the multi-provider spawn ADAPTER (task #6): the `spawn_subagent`
/// action it returns carries the mode the provider resolves to — NATIVE `Task` on Claude, INLINE
/// fallback where there are no native sub-agents (OpenCode). Same graph, same node; the switch is
/// visible in the emitted action, and neither mode is an external session spawn.
#[test]
fn spawn_action_carries_the_provider_resolved_mode() {
    let _g = db_guard();
    let sandbox = temp_dir("spawnmode");
    let db = Db::in_memory();
    let (project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());
    let _ = project_id;

    let build = |db: &Db, wf: &str| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
    };

    // Claude → native Task.
    let (_wf_c, run_c) = workflow_run(&db, None, build);
    let rt_c = EngineRuntime::new();
    let out_c = super::nyx_next(
        &db,
        &rt_c, no_session(),
        &json!({ "run_id": run_c, "provider": "claude" }),
    )
    .expect("nyx_next")
    .result;
    assert_eq!(out_c["action"]["type"], "spawn_subagent");
    assert_eq!(out_c["action"]["agent_id"], "coder");
    assert_eq!(out_c["action"]["mode"], "native_task");
    assert_eq!(out_c["action"]["mechanism"], "claude-native-task");

    // OpenCode → inline fallback, SAME graph/node.
    let (_wf_o, run_o) = workflow_run(&db, None, build);
    let rt_o = EngineRuntime::new();
    let out_o = super::nyx_next(
        &db,
        &rt_o, no_session(),
        &json!({ "run_id": run_o, "provider": "opencode" }),
    )
    .expect("nyx_next")
    .result;
    assert_eq!(out_o["action"]["mode"], "inline");
    assert_eq!(out_o["action"]["mechanism"], "inline-fallback");

    // The default provider (no `provider` arg) is Claude's native Task.
    let (_wf_d, run_d) = workflow_run(&db, None, build);
    let rt_d = EngineRuntime::new();
    let out_d = super::nyx_next(&db, &rt_d, no_session(), &json!({ "run_id": run_d }))
        .expect("nyx_next")
        .result;
    assert_eq!(out_d["action"]["mode"], "native_task");
}

/// THE DELIVERY: a `spawn_subagent` action ships IDENTIFIERS — who (`agent_id`), what for (the
/// node's `label`), on which object (`subject`) and how (`mode`) — and NO manual. The sub-agent
/// pulls its own role + state from the `run_id` it is handed, exactly as a `sub_workflow`'s child
/// dispatcher always did. The number is the point: the action is a few hundred bytes whatever the
/// role, where forwarding the `prototype` manual made ONE tick weigh 73 547 characters.
#[test]
fn spawn_action_carries_the_node_label_and_no_manual() {
    let _g = db_guard();
    let sandbox = temp_dir("spawnctx");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // A LABELLED spawn node — the label is the graph author's words for this step.
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        db.with_conn(|c| {
            db::create_node(
                c,
                wf,
                Some(&root),
                0,
                db::NODE_TYPE_SPAWN_SUBAGENT,
                "audit user stories",
                &json!({ "agent_id": "auditor", "subject": "user_stories" }).to_string(),
            )
            .expect("node")
            .id
        });
    });
    let rt = EngineRuntime::new();
    let out = tick(&db, &rt, &run_id, None);
    let action = &out["action"];

    assert_eq!(action["type"], "spawn_subagent");
    assert_eq!(action["agent_id"], "auditor");
    assert_eq!(
        action["label"], "audit user stories",
        "the node's LABEL travels: without it the dispatcher spawns an auditor without saying \
         what for",
    );
    assert_eq!(action["subject"], "user_stories");
    assert_eq!(action["mode"], "native_task");

    // NO manual, and none of the machinery that existed to avoid re-sending one.
    for gone in ["context", "context_already_sent", "context_recall"] {
        assert!(
            action.get(gone).is_none(),
            "the action must not carry `{gone}`: {action}",
        );
    }
    let size = action.to_string().len();
    assert!(
        size < 400,
        "a spawn action is identifiers, not prose — {size} chars: {action}",
    );

    // …and the run's blackboard carries no forwarding bookkeeping either.
    let board = run_row(&db, &run_id).blackboard;
    assert!(
        !board.contains("__context_sent__"),
        "no `__context_sent__` mark survives on the run: {board}",
    );
}

/// …AND THE SUB-AGENT PULLS WHAT THE ACTION NO LONGER CARRIES. This is the whole delivery mechanism
/// end to end: take the `run_id` the dispatcher is ticking plus the `agent_id` / `subject` the action
/// names, call `agent_context`, and get the role's manual narrowed to THAT commitment — the
/// subject-scoping F6 introduced, now reached by the call instead of by a forward.
#[test]
fn a_spawned_sub_agent_pulls_its_own_context_from_the_run_id() {
    let _g = db_guard();
    let sandbox = temp_dir("ctxpull");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    // Two commitment sections with recognisable bodies — the sections are frozen once the Spec
    // leaves `draft`, so they are written at creation.
    let spec_id = db.with_conn(|c| {
        let (spec, _rev) = db::create_spec(
            c,
            &project_id,
            "Engine spec",
            SpecSections {
                problem: "the loop must hold".into(),
                solution: "walk the tree".into(),
                user_stories: "BODY-STORIES".into(),
                testing_decisions: "BODY-TESTING".into(),
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
            Some(&workspace_id),
        )
        .expect("start");
        spec.id
    });

    // The ndev audit shape: the SAME `auditor` role, several passes, different subjects.
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        for (order, subject) in ["user_stories", "testing_decisions"]
            .into_iter()
            .enumerate()
        {
            add_node(
                db,
                wf,
                Some(&root),
                order as i32,
                db::NODE_TYPE_SPAWN_SUBAGENT,
                json!({ "agent_id": "auditor", "subject": subject }),
            );
        }
    });
    let rt = EngineRuntime::new();

    // What a sub-agent does with the action it was handed: agent_context(run_id, role, subject).
    let pull = |action: &Value| -> String {
        let out = crate::mcp_context::agent_context(
            &db,
            &json!({
                "run_id": run_id,
                "role": action["agent_id"],
                "subject": action["subject"],
            }),
        )
        .expect("the sub-agent pulls its own context");
        out.result["context"].as_str().expect("a context").to_string()
    };

    let stories = pull(&tick(&db, &rt, &run_id, None)["action"]);
    assert!(stories.contains("Context: Auditor Agent"), "its ROLE");
    assert!(
        stories.contains("BODY-STORIES") && !stories.contains("BODY-TESTING"),
        "narrowed to the commitment its spawn named: {stories}",
    );

    // The NEXT pass names another section, so the SAME call gives it another manual — no state
    // kept anywhere, nothing to invalidate.
    let testing = pull(&tick(&db, &rt, &run_id, Some(json!({ "diff": "…" })))["action"]);
    assert!(
        testing.contains("BODY-TESTING") && !testing.contains("BODY-STORIES"),
        "the second pass reads its own section: {testing}",
    );

    // The `run_id` is the ONLY anchor it needs: it resolves the Spec (F5) with no spec_id passed.
    let out = crate::mcp_context::agent_context(
        &db,
        &json!({ "run_id": run_id, "role": "coder" }),
    )
    .expect("a run_id alone resolves the render");
    assert_eq!(out.result["spec_id"], json!(spec_id));

    // An unknown run is refused, not silently rendered empty.
    let err = crate::mcp_context::agent_context(
        &db,
        &json!({ "run_id": "nope", "role": "coder" }),
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_id");
}

/// THE RUN IS THE ANCHOR, INCLUDING WHEN IT ANSWERS "NO SPEC". An UNBOUND run pulled by `run_id`
/// must render, not refuse — even from a location that anchors several active Specs.
///
/// This is where G2 and G6 meet. G2 stopped `run_start` from failing an AUTHORING flow over a Spec
/// ambiguity it never wanted (the flow creates its own), so `/nspec` in a real project — every one
/// of which holds several active Specs — now opens a run bound to no Spec and anchored on the
/// workspace. G6 then has the hook pull that run's context to inject it. If the pull re-resolved a
/// Spec BY LOCATION it would raise the very ambiguity `run_start` just declined to raise, the hook
/// would inject nothing, and the two items would cancel each other out in exactly the case that
/// motivated them.
///
/// So a `run_id` anchor stops at the run: bound ⇒ that Spec, unbound ⇒ none. Reaching past it to
/// the workspace would also be wrong on its own merits — it would render a Spec the run is not
/// driving, and attribute state to work that has nothing to do with it.
#[test]
fn an_unbound_run_pulled_by_id_renders_instead_of_re_raising_the_ambiguity() {
    let _g = db_guard();
    let sandbox = temp_dir("unbound-pull");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    // The shape every real project has, and the one that broke the live /nspec: SEVERAL active
    // Specs anchored on the same workspace, so no location can name one.
    let _a = spec_in_progress(&db, &project_id, &workspace_id);
    let _b = spec_in_progress(&db, &project_id, &workspace_id);

    // An authoring run: no Spec (it exists to create one), anchored on that workspace (G2).
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(db, wf, Some(&root), 0, db::NODE_TYPE_NOTE, json!({ "text": "author" }));
    });
    db.with_conn(|c| db::set_run_workspace(c, &run_id, &workspace_id))
        .expect("anchor");

    let out = crate::mcp_context::agent_context(&db, &json!({ "run_id": run_id, "role": "dev" }))
        .expect("an unbound run renders its dispatcher context — the ambiguity is not re-raised");
    assert_eq!(
        out.result["spec_id"],
        Value::Null,
        "the run drives no Spec, and says so",
    );
    assert!(
        out.result["context"]
            .as_str()
            .expect("a context")
            .contains("Dev — dispatcher"),
        "…and still gets the dispatcher contract it was pulled for",
    );
}

/// An unknown `run_id` is an `invalid_id`, not a panic.
#[test]
fn nyx_next_rejects_an_unknown_run() {
    let _g = db_guard();
    let db = Db::in_memory();
    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": "nope" })).unwrap_err();
    assert_eq!(err.code, "invalid_id");
}

// ---------------------------------------------------------------------------
// The refusals are STICKY (they survive the dispatcher's next call)
// ---------------------------------------------------------------------------

/// The red-gate anchor spans CALLS, not just one tick. This is the seeded-ndev shape: a
/// dispatcher-facing leaf (`judge`) sits BETWEEN the blocking gate and the `transition`, so the gate
/// and the transition are reached on DIFFERENT `nyx_next` calls. A call-local anchor would be dropped
/// at the judge and the Spec would slip into review with the gate still red; the anchor lives on the
/// PERSISTED blackboard, so the transition is refused all the same.
#[test]
fn a_red_gate_still_blocks_a_transition_reached_on_a_later_call() {
    let _g = db_guard();
    let sandbox = temp_dir("redgate-latercall");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    // No sentinel is ever created → the gate stays RED.
    let gate = gate_command(&db, &project_id, "test -f never_created");

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        // The dispatcher round-trip that used to drop the anchor.
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_JUDGE,
            json!({ "prompt": "ready?" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            2,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    // Call 1: nyx runs the gate (RED) and parks on the judge — the gate is not surfaced.
    let first = tick(&db, &rt, &run_id, None);
    assert_eq!(first["action"]["type"], "judge");
    assert!(
        first["blackboard_delta"]["blocking_gate_red"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "the red-gate anchor SET was set this tick, so it is in the delta (N22): {first}"
    );

    // Call 2: the judge answers and the graph reaches the transition — REFUSED, gate still red.
    let err = super::nyx_next(
        &db,
        &rt, no_session(),
        &json!({ "run_id": run_id, "last_result": { "verdict": "ready" } }),
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("gate_red"),
        "the refusal is the red gate, across the call boundary"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_IN_PROGRESS,
        "the Spec never entered review behind a red gate"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_BLOCKED);

    // And the refusal is STICKY: simply calling again (what the dispatcher is told to do) refuses
    // again — with the SAME `gate_red` reason, never walking past the transition to report `done`.
    let again = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(again.code, "invalid_state");
    assert_eq!(
        again
            .data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("gate_red"),
        "re-calling the verb hits the same red-gate refusal, not a different error"
    );
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_IN_PROGRESS);
}

/// A REFUSED transition is RE-ATTEMPTED on the next call, never skipped. Without the replay the next
/// tick would ack the parked leaf and advance, so the state-rule refusal would be a one-shot the
/// dispatcher walks straight through — and the run would report `done` with the Spec untouched.
#[test]
fn a_refused_transition_is_re_attempted_not_skipped() {
    let _g = db_guard();
    let sandbox = temp_dir("stickytrans");
    let db = Db::in_memory();
    let (project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_draft(&db, &project_id); // draft → `in_review` has no legal edge

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    for attempt in 1..=3 {
        let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
        assert_eq!(
            err.code, "invalid_state",
            "attempt {attempt}: the illegal transition is refused again, not skipped"
        );
        assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_DRAFT);
        assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_BLOCKED);
    }

    // Make the transition LEGAL (the human unblocks the Spec) — the re-attempt now succeeds and the
    // run completes: the sticky refusal is a park, not a dead end.
    db.with_conn(|c| {
        db::transition_spec(c, &spec_id, SpecTransition::Ready, true).expect("ready");
        db::transition_spec(c, &spec_id, SpecTransition::Start, true).expect("start");
    });
    let out = tick(&db, &rt, &run_id, None);
    assert_eq!(out["action"]["type"], "done");
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_IN_REVIEW);
}

/// A RED NON-blocking gate must not CLEAR another gate's blocking refusal (only a GREEN gate does).
#[test]
fn a_red_non_blocking_gate_does_not_clear_the_blocking_refusal() {
    let _g = db_guard();
    let sandbox = temp_dir("nonblocking");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let gate = gate_command(&db, &project_id, "test -f never_created");

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate.clone()], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": false }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            2,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("gate_red"),
        "the red non-blocking gate did not wash out the blocking one"
    );
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_IN_PROGRESS);
}

/// B3 — an ENGAGING transition (done/cancel/ready) whose gate is NOT satisfied (no affirmative human
/// answer on its `gate_on` key) is REFUSED at runtime, and the Spec is never advanced.
#[test]
fn an_engaging_transition_without_its_gate_is_refused() {
    let _g = db_guard();
    let sandbox = temp_dir("engaging-ungated");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // Built directly (bypassing the authoring tree guard) — the RUNTIME must still refuse.
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "done", "gate_on": "approved" }),
        );
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("engaging_transition_ungated"),
        "the refusal names the missing human approval: {}",
        err.message
    );
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_IN_PROGRESS);
}

// ---------------------------------------------------------------------------
// H6 — the answer that commits the Spec needs a human turn behind it
// ---------------------------------------------------------------------------

/// A SESSION at a terminal — the tick's [`super::CallerSession`], owned so a test can hand the same
/// session to several ticks, plus the gestures that make its human speak and its session end.
struct Session {
    terminal_id: String,
    /// The RUNTIME activity store the hook path writes (the sidecar holds one per process). Held
    /// here so the session boundary a test crosses is the REAL one: `SessionEnd` empties this, and
    /// the consent check must go on working across it.
    activity: crate::agent_activity::AgentActivityStore,
}

impl Session {
    fn new(terminal_id: &str) -> Session {
        Session {
            terminal_id: terminal_id.to_string(),
            activity: crate::agent_activity::AgentActivityStore::new(),
        }
    }

    fn caller(&self) -> super::CallerSession<'_> {
        super::CallerSession {
            terminal_id: Some(&self.terminal_id),
        }
    }

    /// THE HUMAN TYPES. Drives the REAL hook path — `agent_session_event(UserPromptSubmit)`, the
    /// one signal on the whole dispatch path an agent cannot produce — so what these tests exercise
    /// is what a live session actually writes, `prompt_id` and all.
    fn human_says(&self, db: &Db, prompt: &str) {
        self.human_says_with_id(db, prompt, None);
    }

    /// The same, with the `prompt_id` Claude Code stamps on the prompt (`None` = a provider that
    /// sends none, where nyx mints the turn's identity itself).
    fn human_says_with_id(&self, db: &Db, prompt: &str, prompt_id: Option<&str>) {
        let mut args = json!({
            "NYX_TERMINAL_ID": self.terminal_id,
            "hook_event_name": "UserPromptSubmit",
            "prompt": prompt,
        });
        if let Some(id) = prompt_id {
            args.as_object_mut()
                .unwrap()
                .insert("prompt_id".into(), json!(id));
        }
        crate::mcp_tools_core::agent_session_event(db, &self.activity, &args)
            .expect("the UserPromptSubmit hook is accepted");
    }

    /// THE SESSION ENDS — through the REAL `SessionEnd` hook, which is where the terminal's runtime
    /// activity is dropped. This is what every `claude -p` invocation does after its single prompt,
    /// and it is the lifetime boundary the consent check must survive.
    fn session_ends(&self, db: &Db) {
        crate::mcp_tools_core::agent_session_event(
            db,
            &self.activity,
            &json!({
                "NYX_TERMINAL_ID": self.terminal_id,
                "hook_event_name": "SessionEnd",
                "session_id": "external-session",
                "reason": "prompt_input_exit",
            }),
        )
        .expect("the SessionEnd hook is accepted");
        assert!(
            self.activity.snapshot(&self.terminal_id).is_none(),
            "the session really ended: nothing runtime survives it on this terminal"
        );
    }

    /// One `nyx_next` for `run_id` as THIS session, returning the raw result.
    fn tick(&self, db: &Db, rt: &EngineRuntime, run_id: &str, last: Option<Value>) -> Value {
        super::nyx_next(db, rt, self.caller(), &self.args(run_id, last))
            .expect("nyx_next")
            .result
    }

    /// One `nyx_next` for `run_id` as THIS session, expecting a REFUSAL.
    fn tick_err(
        &self,
        db: &Db,
        rt: &EngineRuntime,
        run_id: &str,
        last: Option<Value>,
    ) -> crate::mcp::RpcError {
        super::nyx_next(db, rt, self.caller(), &self.args(run_id, last))
            .expect_err("nyx_next must refuse")
    }

    fn args(&self, run_id: &str, last: Option<Value>) -> Value {
        let mut args = json!({ "run_id": run_id });
        if let Some(last) = last {
            args.as_object_mut()
                .unwrap()
                .insert("last_result".into(), last);
        }
        args
    }
}

/// The `sequence[ ask_human → approved, transition(ready, gate_on: approved) ]` the whole of H6 is
/// about: the ONE question whose answer commits the Spec. Returns `(spec_id, run_id)`.
fn consent_gated_ready_run(db: &Db, project_id: &str) -> (String, String) {
    let spec_id = spec_draft(db, project_id);
    let (_wf, run_id) = workflow_run(db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({
                "prompt": "pass it to ready?",
                "output_key": "approved",
                "options": ["yes", "no"],
            }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "ready", "gate_on": "approved" }),
        );
    });
    (spec_id, run_id)
}

/// The receipt of the asking, as it stands on the run row.
fn pending_consent(db: &Db, run_id: &str) -> Option<db::PendingConsent> {
    db::PendingConsent::of(&run_row(db, run_id))
}

/// H6 — THE SCENARIO, REPRODUCED, in its exact order. The human states their intent ONE TURN
/// EARLIER — "when nyx asks me to finalise, I want it moved to ready" — and THEN nyx asks. The
/// session, in good faith, applies that instruction and relays `yes` without going back to them.
///
/// That is the live failure: Spec #7 reached `ready` on an answer the user never gave to the question
/// they were never actually asked. Every field of the call was right; what was missing is that no
/// human turn happened between the question and the answer. So the Spec must NOT move.
#[test]
fn an_answer_relayed_with_no_human_turn_since_the_question_never_moves_the_spec() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("h6-forge").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let session = Session::new("term-h6");
    let rt = EngineRuntime::new();

    // ONE TURN EARLIER: the human says what they want done when the question comes.
    session.human_says(&db, "when nyx asks me to finalise, I want it moved to ready");

    // nyx asks — as an ordinary `ask_human`, answerable through the ordinary field. What it also
    // says is that this particular answer will be checked against a real human turn.
    let asked = session.tick(&db, &rt, &run_id, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(asked["action"]["prompt"], "pass it to ready?");
    assert_eq!(asked["action"]["answer_key"], "decision");
    assert_eq!(asked["action"]["human_turn_required"], true);

    // THE FAILURE: the session answers on their behalf, from the instruction it already had.
    let err = session.tick_err(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("human_consent_without_human_turn"),
        "the refusal must NAME why, or a well-meaning session retries in good faith: {}",
        err.message
    );
    assert!(
        err.message.contains("ASK THEM"),
        "the refusal must send the session back to the human, not to another field: {}",
        err.message
    );

    // THE POINT: the Spec did not move, and the run is still parked on the question it asked.
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_DRAFT,
        "an answer the human never gave carried the Spec to ready — the exact failure H6 exists to stop"
    );
    let pending = pending_consent(&db, &run_id).expect("the question is still open");
    assert_eq!(
        run_row(&db, &run_id).current_node_id.as_deref(),
        Some(pending.node_id.as_str())
    );

    // And re-ticking hands back the SAME question: nothing advanced on the refusal.
    let again = session.tick(&db, &rt, &run_id, None);
    assert_eq!(again["action"]["type"], "ask_human");
    assert_eq!(again["action"]["human_turn_required"], true);
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_DRAFT);
}

/// H6 — and the NORMAL path, which must cost nothing: nyx asks, the session puts the question to the
/// human, the human answers IN THE CHAT, the session relays that answer on the next tick — and the
/// Spec moves. The route is the ordinary `ask_human` one end to end (same action, same `answer_key`,
/// same fold onto the same blackboard key); the only thing that changed is that a human turn now has
/// to sit between the question and the answer.
#[test]
fn the_spec_moves_when_the_human_answers_between_the_question_and_the_relay() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("h6-ok").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let session = Session::new("term-h6-ok");
    let rt = EngineRuntime::new();

    session.human_says(&db, "go on with the spec");
    assert_eq!(
        session.tick(&db, &rt, &run_id, None)["action"]["type"],
        "ask_human"
    );

    // The session asks; the HUMAN answers, in the conversation. That turn is what nyx witnesses.
    session.human_says(&db, "yes, mark it ready");

    let out = session.tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(out["action"]["type"], "done");
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_READY);
    assert_eq!(
        out["blackboard_delta"]["approved"], "yes",
        "the answer folded through the ORDINARY ask_human path, onto the same key: {out}"
    );
    assert_eq!(
        pending_consent(&db, &run_id),
        None,
        "an accepted question is closed — its receipt cannot be spent twice"
    );
}

/// H6 — the check is about the TURN, not the clock: an answer relayed a second time, after the human
/// has since spoken, is ACCEPTED. This is what makes the refusal a detour rather than a dead end —
/// the session that was refused goes and asks, and the very same call then works.
#[test]
fn the_refused_answer_is_accepted_once_the_session_actually_asks() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("h6-retry").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let session = Session::new("term-h6-retry");
    let rt = EngineRuntime::new();

    session.human_says(&db, "finalise it when nyx asks");
    session.tick(&db, &rt, &run_id, None);
    session.tick_err(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_DRAFT);

    // The session does what the refusal told it to: it asks, and the human replies.
    session.human_says(&db, "yes");
    let out = session.tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(out["action"]["type"], "done");
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_READY);
}

/// H6, THE LIFETIME — the `claude -p --resume` split, measured live over two days: the SAME run,
/// the SAME question, the SAME human. Answered from an interactive session it went through at the
/// first try; answered from `claude -p --resume` it was REFUSED on two authentic `yes` in a row.
///
/// The two sides of the check did not share a lifetime. The human-turn side was a COUNTER held in
/// the sidecar's memory and dropped at `SessionEnd`; the reference it was compared against lives on
/// the run row. A `claude -p` invocation is a WHOLE session — SessionStart, one prompt, SessionEnd —
/// so the counter restarted at zero on each one while the receipt still held a number counted in the
/// previous session's lifetime. Nothing about the human changed; the arithmetic did.
///
/// So this is the test the fix owes: ONE run, the question asked in one session and answered in the
/// NEXT, and the outcome must be the same as answering within one session. What crosses the boundary
/// is a NAMED turn on both sides, and the boundary here is the real one — the `SessionEnd` hook.
#[test]
fn a_consent_answered_in_the_next_session_moves_the_spec_like_one_answered_in_the_same() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("h6-split").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let session = Session::new("term-h6-split");
    let rt = EngineRuntime::new();

    // --- `claude -p "/ndev …"` : one whole session, which asks the question ------------------
    session.human_says(&db, "/ndev finalise la Spec");
    let asked = session.tick(&db, &rt, &run_id, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(asked["action"]["human_turn_required"], true);
    session.session_ends(&db);

    // --- `claude -p --resume "yes"` : the NEXT session, where the human answers ---------------
    // A new session on the same terminal. The person reads the question and types their answer; the
    // resumed session relays it on its first tick.
    session.human_says(&db, "yes, passe-la en ready");
    let out = session.tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));

    assert_eq!(
        out["action"]["type"], "done",
        "the answer given in the next session is an answer: {out}"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_READY,
        "the same run, the same human, answered across a session boundary — same outcome as within one"
    );
    assert_eq!(
        pending_consent(&db, &run_id),
        None,
        "the question is closed, not left open for a third invocation to answer again"
    );
    // The audit row carries the words from the session that actually answered — the ledger crossed
    // the boundary whole, raw text included, not just its existence.
    let journaled = db
        .with_conn(db::list_domain_events)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == db::EVENT_RUN_CONSENT_ANSWERED)
        .expect("the accepted consent is journaled");
    let payload: Value = serde_json::from_str(&journaled.payload).unwrap();
    assert_eq!(payload["human_prompt"], "yes, passe-la en ready");
}

/// H6, THE OTHER DIRECTION of the same boundary — and the one that matters most, because it is
/// silent. A session that ENDS and comes back does not thereby produce a human turn: the resumed
/// session answering straight out of its own context, with nobody typing, must still be refused.
/// Comparing a counter that restarts at zero against a number from a previous lifetime could go
/// either way; comparing two named turns cannot.
///
/// It is also where the REFUSAL has to tell the truth. Telling someone who has just taken a turn
/// that nyx "has seen no human turn" is false, and a session told a falsehood about the state of the
/// world argues with it instead of acting on it. The message says what nyx actually holds.
#[test]
fn a_new_session_is_not_a_human_turn_and_the_refusal_says_what_is_actually_the_case() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("h6-split-no").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let session = Session::new("term-h6-split-no");
    let rt = EngineRuntime::new();

    session.human_says(&db, "/ndev finalise la Spec");
    session.tick(&db, &rt, &run_id, None);
    session.session_ends(&db);

    // A fresh session on the same terminal — a headless re-tick, a `--continue`, anything that
    // resumes without a person typing — relays the answer it believes it already has.
    let err = session.tick_err(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("human_consent_without_human_turn"),
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_DRAFT,
        "a session boundary is not a human turn — the Spec must not move"
    );

    // THE MESSAGE IS TRUE: it does not claim nyx saw no human turn (it saw one — the prompt that
    // launched the run); it says that turn is still the last one, i.e. nobody has typed since.
    assert!(
        err.message.contains("nobody has typed since"),
        "the refusal must state what is the case: {}",
        err.message
    );
    assert!(
        err.message.contains("/ndev finalise la Spec"),
        "…and NAME the turn it is talking about, so the human recognises it: {}",
        err.message
    );

    // And the way out is the ordinary one: the human types, the same call goes through.
    session.human_says(&db, "oui");
    session.tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_READY);
}

/// H6 — the turn's IDENTITY comes from Claude Code's own `prompt_id` when the hook carries one, and
/// re-DELIVERING the same prompt is not a new turn. The hook POST is best-effort and can land twice;
/// a duplicate must never manufacture the very proof the check is built on. (A counter incremented
/// on each delivery did exactly that.)
#[test]
fn a_redelivered_prompt_hook_is_the_same_turn_and_buys_nothing() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("h6-dup").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let session = Session::new("term-h6-dup");
    let rt = EngineRuntime::new();

    session.human_says_with_id(&db, "lance /ndev", Some("prompt-42"));
    session.tick(&db, &rt, &run_id, None);

    // The SAME hook lands a second time (a retry, a duplicated POST). Same prompt, same id.
    session.human_says_with_id(&db, "lance /ndev", Some("prompt-42"));
    session.tick_err(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_DRAFT,
        "a re-delivered hook is the same turn — it cannot pay for a consent"
    );

    // A genuinely NEW prompt carries a NEW id, and that one is a turn.
    session.human_says_with_id(&db, "yes", Some("prompt-43"));
    session.tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_READY);
}

/// ONE HUMAN TURN AUTHORISES ONE COMMITMENT — the defect, reproduced exactly as reported.
///
/// TWO runs parked on a commitment question, both asked from the SAME terminal. That is not a corner:
/// two `/ndev` on two Specs from one terminal is the ordinary way to work, and it takes no concurrency
/// — the two ticks below are sequential. Each receipt named the turn that stood at ITS asking; the
/// human then typed ONCE; both runs read a last turn different from their own, and both accepted. One
/// sentence carried two Spec commitments, and the second of them is precisely what H6 exists to
/// refuse: an answer nobody gave to THAT question.
///
/// So the second must not move — and it must be told the truth about why, which is NOT "nobody has
/// typed since". Somebody did. Their words went to another question, and only saying so tells a
/// well-meaning session what to do next: ask the human THIS one.
#[test]
fn one_human_turn_authorises_one_commitment_and_the_second_run_is_told_whose_answer_it_was() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("h6-one-turn").path());
    let (spec_a, run_a) = consent_gated_ready_run(&db, &project_id);
    let (spec_b, run_b) = consent_gated_ready_run(&db, &project_id);
    let session = Session::new("term-one-turn");
    let rt = EngineRuntime::new();

    // Both runs park on their own consent question, put to the same session.
    session.human_says(&db, "/ndev les deux Specs");
    assert_eq!(
        session.tick(&db, &rt, &run_a, None)["action"]["human_turn_required"],
        json!(true)
    );
    assert_eq!(
        session.tick(&db, &rt, &run_b, None)["action"]["human_turn_required"],
        json!(true)
    );

    // THE HUMAN TYPES ONCE.
    session.human_says(&db, "oui, vas-y");

    // The first run takes that turn: a real human answered, and its Spec moves. Nothing here is new.
    assert_eq!(
        session.tick(&db, &rt, &run_a, Some(json!({ "decision": "yes" })))["action"]["type"],
        "done"
    );
    assert_eq!(spec_status(&db, &spec_a), db::SPEC_STATUS_READY);

    // THE POINT: the SAME sentence buys the second run nothing.
    let err = session.tick_err(&db, &rt, &run_b, Some(json!({ "decision": "yes" })));
    assert_eq!(
        spec_status(&db, &spec_b),
        db::SPEC_STATUS_DRAFT,
        "one sentence authorised TWO Spec commitments — the second is an answer nobody gave to it"
    );
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("human_consent_turn_already_spent"),
        "ITS OWN case: this is not 'no human turn since it asked': {}",
        err.message
    );
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("spent_on_run"))
            .and_then(Value::as_str),
        Some(run_a.as_str()),
        "…and it names the commitment those words did pay for: {}",
        err.message
    );

    // THE SENTENCE SAYS WHAT IS THE CASE. The human spoke — telling a session that just saw them
    // type that nyx has seen nothing reads as a bug in nyx, and it argues instead of acting.
    assert!(
        err.message.contains("HAS spoken since nyx asked") && err.message.contains("oui, vas-y"),
        "the refusal must credit the turn the human really took: {}",
        err.message
    );
    assert!(
        !err.message.contains("nobody has typed since"),
        "…and must not claim the opposite: {}",
        err.message
    );
    assert!(
        err.message.contains("ASK THEM THIS question"),
        "…and must say what makes progress — this question, put to them: {}",
        err.message
    );

    // Nothing was written for the refused run: it is still parked on the same question, still asking.
    assert!(pending_consent(&db, &run_b).is_some());
    assert_eq!(
        session.tick(&db, &rt, &run_b, None)["action"]["prompt"],
        "pass it to ready?"
    );

    // AND TWO QUESTIONS IN TWO TURNS STILL WORK, which is the flow that must not break: the human
    // answers THIS one too, and that second turn — nobody's yet — carries it.
    session.human_says(&db, "oui pour l'autre aussi");
    assert_eq!(
        session.tick(&db, &rt, &run_b, Some(json!({ "decision": "yes" })))["action"]["type"],
        "done"
    );
    assert_eq!(spec_status(&db, &spec_b), db::SPEC_STATUS_READY);
}

/// H6, then K1 — a tick nyx cannot attribute to a SESSION can never satisfy a consent question, so
/// it is never ASKED one. A bare MCP client (no `X-Nyx-Terminal-Id`) carries no terminal, and the
/// human-turn ledger is keyed by terminal: there is nothing to check an answer against, so every
/// answer such a caller relays is refused however genuine it is. The refusal therefore lands on the
/// ASKING — and leaves NO receipt, because a receipt naming no terminal is one nothing can satisfy.
///
/// Dropping the header is still not a bypass: relaying an answer anyway is refused too, and the Spec
/// does not move.
#[test]
fn a_tick_from_an_unattributable_caller_is_never_asked_and_leaves_no_receipt() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("h6-anon").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let rt = EngineRuntime::new();

    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("consent_caller_unattributable"),
        "the question is refused at the asking, not at the answer: {}",
        err.message
    );
    assert!(
        err.message.contains(crate::mcp::TERMINAL_ID_HEADER)
            && err.message.contains("nyx terminal"),
        "…and the sentence says what is missing and where to get it: {}",
        err.message
    );
    assert_eq!(
        pending_consent(&db, &run_id),
        None,
        "nothing was written: a receipt posed to nobody is exactly what must not exist"
    );

    // The header is not droppable as a bypass: an answer relayed anyway is refused all the same.
    let err = super::nyx_next(
        &db,
        &rt,
        no_session(),
        &json!({ "run_id": run_id, "last_result": { "decision": "yes" } }),
    )
    .unwrap_err();
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("human_consent_without_human_turn"),
    );
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_DRAFT);
}

/// FORGE the receipt the live failure left behind: a consent question whose `terminal_id` is null,
/// written straight onto the run row because nyx no longer produces one (the tick that would is
/// refused). It is the state the runs that carry one are IN, and what the fix must get them out of.
/// Returns the `ask_human` node the receipt is anchored on.
fn forge_receipt_posed_to_nobody(db: &Db, run_id: &str, prompt: &str) -> String {
    use diesel::prelude::*;

    let run = run_row(db, run_id);
    let ask = db
        .with_conn(|c| db::list_nodes(c, &run.workflow_id))
        .expect("nodes")
        .into_iter()
        .find(|n| n.type_ == db::NODE_TYPE_ASK_HUMAN)
        .expect("the run's ask_human node");
    let receipt = json!({
        "node_id": ask.id,
        "prompt": prompt,
        "options": ["yes", "no"],
        "asked_at": db::now_millis(),
        "terminal_id": Value::Null,
        "human_turn_at_ask": Value::Null,
    });
    db.with_conn(|c| {
        diesel::update(crate::schema::workflow_runs::table.find(run_id))
            .set(crate::schema::workflow_runs::pending_consent.eq(receipt.to_string()))
            .execute(c)
    })
    .expect("forge the receipt");
    ask.id
}

/// K1 — THE LIVE FAILURE, REPRODUCED. Run `019ffc3c` (`/nspec` on Spec #5) sat parked on "Mark the
/// Spec ready for build?" with `"terminal_id": null` — the tick that FIRST asked came from a caller
/// presenting no terminal. Two real sessions then ticked the run and relayed the question, three
/// authentic `yes` from `claude -p --resume` were refused, and a full app restart changed nothing:
/// the receipt was idempotent per NODE, so it kept a reference no human turn could ever move.
///
/// The question belongs to whoever it is PUT TO. A receipt naming no session cannot be satisfied by
/// the session now being asked, so the question is re-posed to THEM and re-armed on their ledger —
/// and their next turn is what settles it. The Spec finishes.
#[test]
fn a_receipt_posed_to_nobody_is_re_armed_on_the_session_that_picks_the_run_up() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("k1-null").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    forge_receipt_posed_to_nobody(&db, &run_id, "pass it to ready?");
    let session = Session::new("term-k1");
    let rt = EngineRuntime::new();

    // A REAL session picks the run up — the human launched it from a nyx terminal.
    session.human_says(&db, "/nspec reprends la Spec");
    let asked = session.tick(&db, &rt, &run_id, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    let receipt = pending_consent(&db, &run_id).expect("the run is still parked on the question");
    assert_eq!(
        receipt.terminal_id.as_deref(),
        Some("term-k1"),
        "the question is RE-POSED to the session that picked the run up: {receipt:?}"
    );

    // The human answers, in the conversation; the session relays it. That is all it ever took.
    session.human_says(&db, "oui, passe-la en ready");
    let out = session.tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(
        out["action"]["type"], "done",
        "the answer is accepted — the Spec is finishable again: {out}"
    );
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_READY);
    assert_eq!(
        pending_consent(&db, &run_id),
        None,
        "and the question is closed, not left to brick the next run on this Spec"
    );
}

/// K1, THE OTHER HALF — the FREEZE the re-arm must not break, which is why the receipt was ever
/// idempotent. A dispatcher waiting on the human POLLS `nyx_next`, and every poll re-poses the
/// question. Re-arming on each ask would make the reference FOLLOW the human: they type (last turn =
/// B), the next poll re-arms to B, and the answer then compares B against B and is refused — the
/// question would be unanswerable by construction.
///
/// So for the session already being asked, nothing moves: same reference, same instant, poll after
/// poll, human turn or not — and the answer goes through.
#[test]
fn polling_the_same_question_never_moves_the_reference_it_is_checked_against() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("k1-poll").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let session = Session::new("term-k1-poll");
    let rt = EngineRuntime::new();

    session.human_says(&db, "/nspec finalise la Spec");
    session.tick(&db, &rt, &run_id, None);
    let first = pending_consent(&db, &run_id).expect("the asking is recorded");

    // The dispatcher polls while it waits — same question, same session.
    session.tick(&db, &rt, &run_id, None);
    assert_eq!(
        pending_consent(&db, &run_id).as_ref(),
        Some(&first),
        "a poll re-presents the question and changes NOTHING about the receipt"
    );

    // THE HUMAN TYPES — and the dispatcher polls once more before relaying it. This is the poll that
    // would steal the answer's proof if the reference followed the ledger.
    session.human_says(&db, "oui");
    session.tick(&db, &rt, &run_id, None);
    assert_eq!(
        pending_consent(&db, &run_id).as_ref(),
        Some(&first),
        "the reference still names the turn that stood at the ASKING, not the one that answers it"
    );

    let out = session.tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(out["action"]["type"], "done", "the answer is accepted: {out}");
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_READY);
}

/// K1 — the same rule, from the other side: a run PICKED UP by another session re-poses its question
/// to that session. The answer is weighed against ONE terminal's ledger, so a receipt still naming
/// the session that walked away is one the session now being asked can never move — and their own
/// human, who is the one actually being asked, would answer into a void.
#[test]
fn a_question_a_second_session_picks_up_is_re_posed_to_it() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("k1-handover").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let first = Session::new("term-k1-first");
    let second = Session::new("term-k1-second");
    let rt = EngineRuntime::new();

    first.human_says(&db, "/nspec finalise la Spec");
    first.tick(&db, &rt, &run_id, None);
    assert_eq!(
        pending_consent(&db, &run_id).unwrap().terminal_id.as_deref(),
        Some("term-k1-first")
    );

    // The human moves to another terminal and drives the same run from there.
    second.human_says(&db, "/nspec reprends la Spec");
    second.tick(&db, &rt, &run_id, None);
    let receipt = pending_consent(&db, &run_id).expect("still parked on the question");
    assert_eq!(
        receipt.terminal_id.as_deref(),
        Some("term-k1-second"),
        "the question is now posed to the session that is being asked it: {receipt:?}"
    );

    // The FIRST session's human is not the one being asked any more: relaying from there is refused.
    first.tick_err(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_DRAFT);

    // The human types where the question actually was put, and that settles it.
    second.human_says(&db, "oui");
    second.tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_READY);
}

/// H6 — a NON-sensitive `ask_human` is UNCHANGED, and unchecked. Only the answer that COMMITS the
/// Spec costs a human turn; every other question folds from a reported `last_result` with no ledger
/// consulted and no receipt opened. Without this the fix would have cost the headless path
/// everywhere instead of at the one checkpoint that needed it.
#[test]
fn an_ask_human_that_gates_nothing_engaging_needs_no_human_turn() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, workspace_id, _cwd) = seed_project(&db, temp_dir("h6-plain").path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // The SAME shape, one word apart: the transition it precedes is `in_review` — non-sensitive, so
    // this question commits nothing.
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({
                "prompt": "which way?",
                "output_key": "approved",
                "options": ["left", "right"],
            }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review", "gate_on": "approved" }),
        );
    });

    // Driven with NO session at all — the caller nyx cannot attribute, which a consent question
    // refuses outright.
    let rt = EngineRuntime::new();
    let asked = tick(&db, &rt, &run_id, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(asked["action"]["answer_key"], "decision");
    assert!(
        asked["action"].get("human_turn_required").is_none(),
        "a question that commits nothing must not demand a human turn: {asked}"
    );
    assert_eq!(
        pending_consent(&db, &run_id),
        None,
        "a question that commits nothing opens no receipt on the run"
    );

    let out = tick(&db, &rt, &run_id, Some(json!({ "decision": "left" })));
    assert_eq!(out["action"]["type"], "done");
    assert_eq!(out["blackboard_delta"]["approved"], "left");
}

/// The `loop[ spawn, ask_human → the loop's own `until` key ]` shape ndev's build is: the question
/// decides whether the body — and its sub-agent — runs AGAIN. `spends` builds the real thing; `false`
/// swaps the spawn for a `note`, which is the same decision over a body that costs nothing.
fn round_buying_run(db: &Db, spec_id: &str, spends: bool) -> String {
    let (_wf, run_id) = workflow_run(db, Some(spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        let build = add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_LOOP,
            json!({ "until": { "var": "audit_verdict", "ne": "revise" } }),
        );
        if spends {
            add_node(db, wf, Some(&build), 0, db::NODE_TYPE_SPAWN_SUBAGENT, json!({ "agent_id": "coder" }));
        } else {
            add_node(db, wf, Some(&build), 0, db::NODE_TYPE_NOTE, json!({ "text": "counted" }));
        }
        add_node(
            db,
            wf,
            Some(&build),
            1,
            db::NODE_TYPE_ASK_HUMAN,
            json!({
                "prompt": "rework or abandon?",
                "output_key": "audit_verdict",
                "options": ["revise", "abandon"],
            }),
        );
    });
    run_id
}

/// J2 — the question that decides whether to SPEND ANOTHER BUILD ROUND is the human's to answer.
///
/// The observed run: a `/ndev` on a one-phase Spec ran 3 h 35, 109 ticks, $69.48, over audit rounds
/// that after the first only produced second-order lows — because "rework or abandon" was not a
/// human-turn question and the session answered it alone, in a minute, on a general authorisation
/// given earlier. It commits no Spec transition, which is why H6 left it out; it commits a coder
/// sub-workflow and four audit passes, which is why it is in now. The derivation says exactly that:
/// the answer re-opens a loop whose body SPAWNS.
#[test]
fn buying_another_round_is_not_the_sessions_answer_to_give() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, workspace_id, _cwd) = seed_project(&db, temp_dir("j2-round").path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let run_id = round_buying_run(&db, &spec_id, true);
    let session = Session::new("term-j2");
    let rt = EngineRuntime::new();

    // Round one runs, then the question: rework, or stop?
    session.human_says(&db, "lance le build");
    assert_eq!(
        session.tick(&db, &rt, &run_id, None)["action"]["type"],
        "spawn_subagent"
    );
    let asked = session.tick(&db, &rt, &run_id, Some(json!({ "diff": "round 1" })));
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(asked["action"]["human_turn_required"], true);
    assert!(
        asked["action"]["context"]
            .as_str()
            .unwrap()
            .contains("another build round"),
        "the session is told WHAT the answer buys, not merely that it is checked: {asked}"
    );

    // THE FAILURE: the session answers 'revise' by itself, the way the live one did.
    let err = session.tick_err(&db, &rt, &run_id, Some(json!({ "decision": "revise" })));
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("human_consent_without_human_turn"),
    );
    assert!(
        err.message.contains("another build round")
            && err.message.contains("the time and the money it spends"),
        "the refusal names the spend the session was about to commit: {}",
        err.message
    );

    // NOTHING was bought: the run is parked on the question and the board holds no verdict.
    let row = run_row(&db, &run_id);
    let board: Value = serde_json::from_str(&row.blackboard).unwrap();
    assert!(board.get("audit_verdict").is_none(), "no answer was folded: {board}");
    assert_eq!(
        session.tick(&db, &rt, &run_id, None)["action"]["type"],
        "ask_human",
        "the same question comes back — no coder was opened on the way"
    );

    // The human is asked and REPLIES: the identical answer now buys the round they chose.
    session.human_says(&db, "oui, refais un tour");
    assert_eq!(
        session.tick(&db, &rt, &run_id, Some(json!({ "decision": "revise" })))["action"]["type"],
        "spawn_subagent",
        "the round runs on the human's word"
    );
    let asked = session.tick(&db, &rt, &run_id, Some(json!({ "diff": "round 2" })));
    assert_eq!(asked["action"]["type"], "ask_human");
    session.human_says(&db, "non, ça suffit");
    assert_eq!(
        session.tick(&db, &rt, &run_id, Some(json!({ "decision": "abandon" })))["action"]["type"],
        "done"
    );
}

/// J2 — and the derivation says WHY, so it does not fire on a loop that costs nothing. The SAME
/// question over the SAME `until` key, with a `note` where the spawn was: re-opening it buys a tick,
/// not a sub-agent, so nothing is being disposed of and the headless path stays headless.
#[test]
fn re_opening_a_loop_that_spends_nothing_needs_no_human_turn() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, workspace_id, _cwd) = seed_project(&db, temp_dir("j2-cheap").path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let run_id = round_buying_run(&db, &spec_id, false);

    // NO session at all — the caller nyx can attribute no human turn to.
    let rt = EngineRuntime::new();
    let asked = tick(&db, &rt, &run_id, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    assert!(
        asked["action"].get("human_turn_required").is_none(),
        "a round that spends nothing is not a commitment: {asked}"
    );
    assert_eq!(pending_consent(&db, &run_id), None, "and opens no receipt");
    assert_eq!(
        tick(&db, &rt, &run_id, Some(json!({ "decision": "abandon" })))["action"]["type"],
        "done"
    );
}

/// H6 — a consent is worth an AUDIT PAIR. The outbox journals the question ASKED and the answer
/// ACCEPTED, the latter carrying the RAW words of the human turn that authorised it — so "who
/// validated this Spec's move, and in what terms" is answerable from the record and not from anyone's
/// memory. That record is also what makes the check's known hole survivable: nyx accepts an answer
/// relayed after ANY human turn, so the turn it accepted is written down verbatim.
#[test]
fn a_consent_journals_the_question_and_the_words_that_authorised_the_answer() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("h6-audit").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let session = Session::new("term-h6-audit");
    let rt = EngineRuntime::new();

    session.human_says(&db, "drive it");
    session.tick(&db, &rt, &run_id, None);
    session.tick(&db, &rt, &run_id, None); // a second poll must not re-journal the same question
    session.human_says(&db, "oui, passe-la en ready");
    session.tick(&db, &rt, &run_id, Some(json!({ "decision": "yes" })));

    let events = db.with_conn(db::list_domain_events).expect("the outbox");
    let consent: Vec<&db::DomainEvent> = events
        .iter()
        .filter(|e| e.event_type.starts_with("run.consent_"))
        .collect();
    let types: Vec<&str> = consent.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        types,
        vec![db::EVENT_RUN_CONSENT_ASKED, db::EVENT_RUN_CONSENT_ANSWERED],
        "asked once (polling is not re-asking), then answered: {types:?}"
    );
    let payload: Value = serde_json::from_str(&consent[1].payload).expect("a JSON payload");
    assert_eq!(payload["run_id"], run_id);
    assert_eq!(payload["spec_id"], spec_id);
    assert_eq!(payload["prompt"], "pass it to ready?");
    assert_eq!(payload["answer"], "yes");
    assert_eq!(
        payload["human_prompt"], "oui, passe-la en ready",
        "the audit row must quote the turn that authorised it, verbatim: {payload}"
    );
}

/// A finished run's engine is DROPPED from the registry (it will never be ticked again), so a
/// long-lived session does not accumulate the tree + blackboard of every run it ever drove.
#[test]
fn a_finished_run_is_evicted_from_the_registry() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
    });

    let rt = EngineRuntime::new();
    assert_eq!(
        tick(&db, &rt, &run_id, None)["action"]["type"],
        "spawn_subagent"
    );
    assert!(rt.slot(&run_id).is_some(), "the live run stays resident");
    assert_eq!(
        tick(&db, &rt, &run_id, Some(json!({ "diff": "x" })))["action"]["type"],
        "done"
    );
    assert!(
        rt.slot(&run_id).is_none(),
        "the done run's engine is evicted"
    );
}

// ---------------------------------------------------------------------------
// v2.8 review — the run-lifecycle ENTRY (B2), the resume guard (B3), the
// finished-run message (m3)
// ---------------------------------------------------------------------------

/// B2 — `run_start` OPENS a drivable run the way `/ndev` does (a VERB, not a test INSERT): it
/// resolves the project's OWN `ndev` workflow, binds the run to the resolved Spec, and the very next
/// `nyx_next` drives it from the root. Before this verb existed, nothing produced a `run_id`, so the
/// dispatcher manual demanded one nothing could give.
#[test]
fn run_start_opens_a_drivable_run_bound_to_the_projects_workflow() {
    let _g = db_guard();
    let sandbox = temp_dir("runstart");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // The project's OWN ndev workflow: sequence{ spawn(coder) }.
    let wf = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "ndev", db::WORKFLOW_SCOPE_NDEV, None)
            .expect("wf")
            .id
    });
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_SPAWN_SUBAGENT,
        json!({ "agent_id": "coder" }),
    );

    // The ENTRY POINT: run_start resolves the workflow + Spec — no INSERT anywhere.
    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id }))
        .expect("run_start")
        .result;
    assert_eq!(opened["workflow_id"], wf, "it resolved the project's ndev workflow");
    assert_eq!(opened["workflow_name"], "ndev", "and returns its NAME, not just the id (N6)");
    assert_eq!(opened["scope"], "ndev");
    assert_eq!(opened["spec_id"], spec_id, "and bound the run to the Spec");
    let run_id = opened["run_id"].as_str().expect("a run_id").to_string();

    // And the run is immediately drivable from the root.
    let rt = EngineRuntime::new();
    let out = tick(&db, &rt, &run_id, None);
    assert_eq!(out["action"]["type"], "spawn_subagent");
    assert_eq!(
        out["action"]["agent_id"], "coder",
        "the bound spawn names the effector to run"
    );
}

/// H14 — A RUN LEFT FOR DEAD IS TOLD, AND NEVER DELETED BEHIND THE HUMAN'S BACK. `blocked` is a
/// nominal state (M5 re-runs the gate on the next tick), so nothing surfaced it — three `nspec` runs
/// and a `coder` run sat blocked for a week, unseen. The one bound to a Spec is not inert: it is
/// exactly what the (workflow, spec) convergence RESUMES the next time that Spec is driven. So
/// `run_start` reports every run blocked past the threshold, names the Spec it strands and the verb
/// that clears it — and a run blocked a minute ago, which somebody is coming back to, stays quiet.
#[test]
fn run_start_reports_the_runs_left_blocked_for_days_and_deletes_none_of_them() {
    use diesel::prelude::*;

    let _g = db_guard();
    let sandbox = temp_dir("runstart-stranded");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let stranded_spec = spec_in_progress(&db, &project_id, &workspace_id);
    let other_spec = spec_in_progress(&db, &project_id, &workspace_id);
    let driven_spec = spec_in_progress(&db, &project_id, &workspace_id);
    let wf = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "ndev", db::WORKFLOW_SCOPE_NDEV, None)
            .expect("wf")
            .id
    });
    add_node(&db, &wf, None, 0, db::NODE_TYPE_NOTE, json!({ "text": "build" }));

    // A run BLOCKED six days ago on `stranded_spec` (the corpse), and one blocked JUST NOW on
    // `other_spec` — the same status, and the elapsed time is the only thing that tells them apart.
    let block = |spec: &str, aged_days: i64| {
        let run = db
            .with_conn(|c| db::create_workflow_run(c, &wf, Some(spec), None, "{}"))
            .expect("run")
            .id;
        db.with_conn(|c| {
            db::update_workflow_run(c, &run, None, "{}", db::RUN_STATUS_BLOCKED, None)
        })
        .expect("blocked");
        if aged_days > 0 {
            let then = db::now_millis() - aged_days * 24 * 60 * 60 * 1000;
            db.with_conn(|c| {
                diesel::update(crate::schema::workflow_runs::table.find(&run))
                    .set(crate::schema::workflow_runs::updated_at.eq(then))
                    .execute(c)
            })
            .expect("age the run");
        }
        run
    };
    let dead = block(&stranded_spec, 6);
    let recent = block(&other_spec, 0);

    // ACT 1 — a /ndev on a THIRD Spec is told about the corpse it is not even touching.
    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": driven_spec }))
        .expect("run_start")
        .result;
    let stranded: Vec<&Value> = opened["warnings"]
        .as_array()
        .expect("a warnings array")
        .iter()
        .filter(|w| w["code"] == "run_blocked_stranded")
        .collect();
    assert_eq!(
        stranded.len(),
        1,
        "only the run nobody came back to is reported: {:?}",
        opened["warnings"]
    );
    let warning = stranded[0];
    assert_eq!(warning["run_id"], dead, "the corpse, by id");
    assert_eq!(warning["spec_id"], stranded_spec, "and the Spec it strands");
    assert_eq!(warning["blocked_days"], 6);
    assert_eq!(
        warning["resumed_here"], false,
        "this call did not hand that run back — it is the human's OTHER work that is stuck"
    );
    let message = warning["message"].as_str().expect("a sentence for the human");
    let spec_number = db
        .with_conn(|c| db::get_spec(c, &stranded_spec))
        .unwrap()
        .unwrap()
        .number;
    assert!(
        message.contains(&format!("Spec #{spec_number}")),
        "it names WHICH Spec is stranded, by its human key: {message}"
    );
    assert!(
        message.contains(&format!("run_delete run_id='{dead}'")),
        "…and the verb that clears it: {message}"
    );
    assert!(
        db.with_conn(|c| db::get_workflow_run(c, &dead))
            .unwrap()
            .is_some()
            && db
                .with_conn(|c| db::get_workflow_run(c, &recent))
                .unwrap()
                .is_some(),
        "nyx deleted nothing: reporting is not disposing"
    );

    // ACT 2 — the confiscation itself: starting on the STRANDED Spec hands back that very run, and
    // the caller is told so rather than believing it opened a fresh one. The `workspace_id` is what
    // makes this bite twice: anchoring the run rewrites its `updated_at`, so a report taken after
    // the anchor would find a rejuvenated corpse and say nothing.
    let resumed = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": stranded_spec, "workspace_id": workspace_id }),
    )
    .expect("run_start")
    .result;
    assert_eq!(resumed["run_id"], dead, "N20 converged onto the dead run");
    let warning = resumed["warnings"]
        .as_array()
        .expect("a warnings array")
        .iter()
        .find(|w| w["code"] == "run_blocked_stranded")
        .unwrap_or_else(|| panic!("the resumed corpse is reported: {:?}", resumed["warnings"]));
    assert_eq!(
        warning["resumed_here"], true,
        "and reported as the run THIS call handed back: {warning}"
    );
    assert!(
        warning["message"]
            .as_str()
            .unwrap()
            .contains("THIS CALL just handed back"),
        "the sentence says the caller is about to drive it: {warning}"
    );
}

/// K1 — A RUN NOBODY CAN ANSWER SAYS SO. `run_start` converges onto the live run of a (workflow,
/// Spec) pair, so the run carrying a receipt posed to no session was handed back with
/// `"warnings": []` — every later `/nspec` on that Spec landed on a question it could not close, and
/// nothing anywhere said why. The receipt is repairable (the next tick from a real terminal re-poses
/// the question), but only a caller TOLD so knows to make that tick.
#[test]
fn run_start_says_when_the_run_it_hands_back_is_parked_on_a_question_posed_to_nobody() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("k1-warn").path());
    let (spec_id, run_id) = consent_gated_ready_run(&db, &project_id);
    let workflow_id = run_row(&db, &run_id).workflow_id;
    forge_receipt_posed_to_nobody(&db, &run_id, "Mark the Spec ready for build?");

    let opened = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": spec_id, "workflow_id": workflow_id }),
    )
    .expect("run_start")
    .result;
    assert_eq!(
        opened["run_id"], run_id,
        "the call converged onto the very run carrying the receipt"
    );
    let warning = opened["warnings"]
        .as_array()
        .expect("a warnings array")
        .iter()
        .find(|w| w["code"] == "run_consent_unanswerable")
        .unwrap_or_else(|| panic!("the run must not present itself as healthy: {opened}"));
    assert_eq!(warning["run_id"], run_id, "it names the run");
    assert_eq!(warning["spec_id"], spec_id, "and the Spec it holds up");
    let spec_number = db
        .with_conn(|c| db::get_spec(c, &spec_id))
        .unwrap()
        .unwrap()
        .number;
    let message = warning["message"].as_str().expect("a sentence for the human");
    assert!(
        message.contains(&format!("Spec #{spec_number}"))
            && message.contains("Mark the Spec ready for build?"),
        "…by its human key, quoting the question it is stuck on: {message}"
    );
    assert!(
        message.contains("nyx terminal") && message.contains("nyx_next"),
        "…and the gesture that clears it: {message}"
    );

    // A run whose receipt names a session is healthy and says nothing.
    let session = Session::new("term-k1-warn");
    let rt = EngineRuntime::new();
    session.human_says(&db, "/nspec reprends la Spec");
    session.tick(&db, &rt, &run_id, None);
    let reopened = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": spec_id, "workflow_id": workflow_id }),
    )
    .expect("run_start")
    .result;
    assert!(
        !reopened["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "run_consent_unanswerable"),
        "a question posed to a real session is not a warning: {reopened}"
    );
}

/// `run_start` fails CLEARLY when no workflow of the scope resolves (no project one, no built-in) —
/// rather than creating a run that the first `nyx_next` would choke on.
#[test]
fn run_start_errors_when_no_workflow_resolves() {
    let _g = db_guard();
    let sandbox = temp_dir("runstart-empty");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    let err = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains("no 'ndev' workflow"),
        "it names the missing scope: {}",
        err.message
    );
}

/// Lot 2 — a `run_start` on an UNSEEDED sub-workflow scope (`coder`) is refused with the SAME
/// "no workflow" InvalidState as any other unresolved scope. The scope is a valid vocabulary word
/// (so resolution runs), it just has no seeded built-in — the intermediate state is correct (no
/// `/coder` command, no default wiring lands one).
#[test]
fn run_start_on_the_unseeded_coder_scope_reports_no_workflow() {
    let _g = db_guard();
    let sandbox = temp_dir("runstart-coder");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    let err = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "scope": "coder" })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains("no 'coder' workflow"),
        "it names the missing scope: {}",
        err.message
    );
}

/// N24 — `run_start` stores the run's dev WORKSPACE (from `workspace_id`), and the graph's ENTRY
/// `start` transition (ready → in_progress) ANCHORS it onto the Spec — exactly what the manual
/// `spec_transition start` used to pose. A ready Spec starts un-anchored; after `run_start` +
/// one tick (the query + start fire inline), the Spec carries the workspace.
#[test]
fn run_start_anchors_the_spec_workspace_at_the_entry_start_transition() {
    let _g = db_guard();
    let sandbox = temp_dir("runstart-ws");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_ready(&db, &project_id);
    // Precondition: a READY Spec is not yet anchored (the anchor is posed at start, never authoring).
    assert!(
        db.with_conn(|c| db::get_spec(c, &spec_id))
            .unwrap()
            .unwrap()
            .workspace_id
            .is_none(),
        "a ready Spec has no workspace anchor yet"
    );

    // A minimal ndev-shaped workflow: query status → selector[ (ready) transition in_progress / note ].
    let wf = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "ndev", db::WORKFLOW_SCOPE_NDEV, None)
            .expect("wf")
            .id
    });
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_QUERY,
        json!({ "query": "spec_status", "write": "spec_status" }),
    );
    let sel = add_node(
        &db,
        &wf,
        Some(&root),
        1,
        db::NODE_TYPE_SELECTOR,
        json!({ "branches": [
            { "cond": { "var": "spec_status", "eq": "ready" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    );
    add_node(
        &db,
        &wf,
        Some(&sel),
        0,
        db::NODE_TYPE_TRANSITION,
        json!({ "target_state": "in_progress" }),
    );
    add_node(&db, &wf, Some(&sel), 1, db::NODE_TYPE_NOTE, json!({ "text": "not ready" }));

    // run_start with an explicit workspace → the run row carries it.
    let opened = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": spec_id, "workspace_id": workspace_id, "workflow_id": wf }),
    )
    .expect("run_start")
    .result;
    let run_id = opened["run_id"].as_str().expect("run_id").to_string();
    assert_eq!(
        run_row(&db, &run_id).workspace_id.as_deref(),
        Some(workspace_id.as_str()),
        "run_start stored the run's workspace"
    );

    // One tick folds the query + the entry start transition INLINE → done.
    let rt = EngineRuntime::new();
    assert_eq!(tick(&db, &rt, &run_id, None)["action"]["type"], "done");

    let spec = db.with_conn(|c| db::get_spec(c, &spec_id)).unwrap().unwrap();
    assert_eq!(spec.status, db::SPEC_STATUS_IN_PROGRESS, "the Spec started");
    assert_eq!(
        spec.workspace_id.as_deref(),
        Some(workspace_id.as_str()),
        "the entry start transition anchored the run's workspace onto the Spec (N24)"
    );
}

/// G2 — the workspace ANCHOR is posed whenever one resolves, on a RESUMED run as much as on a fresh
/// one: a run opened by `spec_id` alone starts unanchored and could never acquire the anchor
/// afterwards, though the next `run_start` (N20 hands back the SAME run) carries a workspace. And an
/// anchor already posed is never MOVED — a later call from another workspace resumes the run without
/// re-homing it.
#[test]
fn run_start_anchors_a_resumed_run_and_never_moves_an_anchor_already_posed() {
    let _g = db_guard();
    let sandbox = temp_dir("runstart-anchor-resume");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let other_sandbox = temp_dir("runstart-anchor-other");
    let other_ws = db.with_conn(|c| {
        db::create_workspace(c, &project_id, "other", other_sandbox.path().to_str().unwrap())
            .expect("second workspace")
            .id
    });
    let spec_id = spec_ready(&db, &project_id);
    let wf = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "ndev", db::WORKFLOW_SCOPE_NDEV, None)
            .expect("wf")
            .id
    });
    add_node(&db, &wf, None, 0, db::NODE_TYPE_NOTE, json!({ "text": "x" }));

    // Opened by id ALONE: nothing resolves a workspace, so there is no anchor to pose yet.
    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result;
    let run_id = opened["run_id"].as_str().expect("run_id").to_string();
    assert!(
        run_row(&db, &run_id).workspace_id.is_none(),
        "started by spec_id alone: nothing resolved a workspace"
    );

    // The SAME run, re-opened WITH a workspace: N20 resumes it, and the missing anchor is posed now.
    let resumed = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": spec_id, "workflow_id": wf, "workspace_id": workspace_id }),
    )
    .expect("run_start")
    .result;
    assert_eq!(resumed["run_id"], json!(run_id), "N20 resumed the same run");
    assert_eq!(
        run_row(&db, &run_id).workspace_id.as_deref(),
        Some(workspace_id.as_str()),
        "a resumed run that now resolves a workspace ends up anchored"
    );

    // A THIRD call from ANOTHER workspace resumes the run and leaves its anchor alone.
    super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": spec_id, "workflow_id": wf, "workspace_id": other_ws }),
    )
    .expect("run_start");
    assert_eq!(
        run_row(&db, &run_id).workspace_id.as_deref(),
        Some(workspace_id.as_str()),
        "an anchor already posed is never moved"
    );
}

/// G2 — an AMBIGUOUS location no longer costs an AUTHORING flow its `cwd`. A workspace anchoring
/// SEVERAL active Specs made `run_start(scope:"nspec", cwd:…)` refuse over a Spec that flow never
/// wanted, and the caller's only way through was to retry WITHOUT the `cwd` — which is how a live run
/// ended up with no workspace at all. The ambiguity now resolves NO Spec (never an arbitrary one): the
/// authoring run starts unbound, ANCHORED on the cwd's workspace, and says so in `warnings`. A flow
/// that works ON a Spec is refused exactly as before (N14b).
#[test]
fn an_ambiguous_location_starts_an_authoring_run_unbound_but_anchored() {
    let _g = db_guard();
    let sandbox = temp_dir("runstart-ambiguous");
    let db = Db::in_memory();
    let (project_id, workspace_id, ws_path) = seed_project(&db, sandbox.path());
    // TWO active Specs anchored on the ONE workspace ⇒ resolving one by location is ambiguous.
    let _first = spec_in_progress(&db, &project_id, &workspace_id);
    let _second = spec_in_progress(&db, &project_id, &workspace_id);
    let nspec_wf = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "nspec", db::WORKFLOW_SCOPE_NSPEC, None)
            .expect("wf")
            .id
    });
    add_node(&db, &nspec_wf, None, 0, db::NODE_TYPE_NOTE, json!({ "text": "author" }));

    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": "nspec", "cwd": ws_path }))
        .expect("an authoring flow starts on an ambiguous location")
        .result;
    let run_id = opened["run_id"].as_str().expect("run_id").to_string();
    assert!(
        opened["spec_id"].is_null(),
        "no arbitrary Spec was picked: {opened}"
    );
    assert_eq!(
        run_row(&db, &run_id).workspace_id.as_deref(),
        Some(workspace_id.as_str()),
        "the cwd still anchored the run on its workspace"
    );
    let warning = opened["warnings"]
        .as_array()
        .expect("a warnings array")
        .iter()
        .find(|w| w["code"] == "spec_ambiguous_started_unbound")
        .expect("the ambiguity is reported, never swallowed")
        .clone();
    assert!(
        warning["message"]
            .as_str()
            .unwrap()
            .contains("2 active Specs"),
        "the warning carries the refusal's own sentence: {warning}"
    );

    // A flow that WORKS ON a Spec still refuses the same location — N14b is untouched.
    let err = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": "ndev", "cwd": ws_path }))
        .expect_err("a spec-scoped flow cannot start on an ambiguous location");
    assert!(
        err.message.contains("active Specs"),
        "the ambiguity is still a refusal there: {}",
        err.message
    );
}

/// REVIEW F1, the `run_start` half of the case-0 PARITY ruling — the half that was IMPLEMENTED.
///
/// The finding required `run_start` to be aligned with `agent_context`'s case 0 or to state in code
/// why it is not. The ruling (spelled out at the call site) declines the "no Spec" half — a
/// Spec-scoped flow already REFUSES to start unbound, and `nspec` is DESIGNED to start without a
/// Spec, so warning there would fire on every authoring start — and implements the "wrong folder"
/// half, which nothing covered: a `cwd` matching no known workspace opens a run with NO workspace
/// anchor, i.e. an effector with no repos, and used to say nothing at all.
///
/// The two negative halves are the point of the test as much as the positive one: a warning that
/// also fires from the RIGHT folder, or on an id-only call that names no folder on purpose, is noise
/// the dispatcher would have to relay to the human every single time.
#[test]
fn a_cwd_matching_no_workspace_says_the_run_opened_with_no_anchor() {
    let _g = db_guard();
    let sandbox = temp_dir("runstart-badcwd");
    let db = Db::in_memory();
    let (project_id, workspace_id, ws_path) = seed_project(&db, sandbox.path());
    let wf = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "ndev", db::WORKFLOW_SCOPE_NDEV, None)
            .expect("wf")
            .id
    });
    add_node(
        &db,
        &wf,
        None,
        0,
        db::NODE_TYPE_NOTE,
        json!({ "text": "build" }),
    );
    let located = |result: &Value| -> Option<Value> {
        result["warnings"]
            .as_array()
            .expect("a warnings array")
            .iter()
            .find(|w| w["code"] == "run_location_unresolved")
            .cloned()
    };

    // THE MISTAKE — a cwd nyx tracks no workspace for. The run opens (nothing here is a refusal),
    // unanchored, and SAYS so.
    let wrong_folder = "/nowhere/nyx-tracks-no-workspace-here";
    let stray = spec_in_progress(&db, &project_id, &workspace_id);
    let opened = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": stray, "workflow_id": wf, "cwd": wrong_folder }),
    )
    .expect("a bad cwd is signalled, not refused")
    .result;
    let run_id = opened["run_id"].as_str().expect("run_id").to_string();
    assert!(
        run_row(&db, &run_id).workspace_id.is_none(),
        "the premise: nothing anchored the run"
    );
    let warned = located(&opened)
        .unwrap_or_else(|| panic!("a run left with no workspace anchor must say so: {opened}"));
    assert!(
        warned["message"].as_str().unwrap().contains(wrong_folder),
        "the sentence NAMES the folder that resolved nothing: {warned}"
    );

    // FROM THE RIGHT FOLDER — silence. The run is anchored, so there is nothing to report.
    let good = spec_in_progress(&db, &project_id, &workspace_id);
    let anchored = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": good, "workflow_id": wf, "cwd": ws_path }),
    )
    .expect("run_start")
    .result;
    assert_eq!(
        run_row(&db, anchored["run_id"].as_str().unwrap())
            .workspace_id
            .as_deref(),
        Some(workspace_id.as_str()),
        "the premise of the negative half: this cwd DID resolve its workspace"
    );
    assert!(
        located(&anchored).is_none(),
        "the nominal call must not carry the alarm: {anchored}"
    );

    // AND NAMING NO FOLDER AT ALL is not a mistake: `run_start(spec_id=…)` deliberately passes no
    // location, so it is not told its location resolved nothing.
    let by_id = spec_in_progress(&db, &project_id, &workspace_id);
    let idonly = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": by_id, "workflow_id": wf }),
    )
    .expect("run_start")
    .result;
    assert!(
        located(&idonly).is_none(),
        "an id-only start names no folder — there is no wrong folder to report: {idonly}"
    );
}

/// H11 — the gate warning REACHES THE GRAPH THAT HAS THE DEFECT. The seeded empty gate lives in the
/// `coder` flow, which `run_start` NEVER opens: a `sub_workflow` node does, several ticks in. Scanning
/// only the workflow it opens made `run_start(scope:"ndev")` answer `"warnings": []` on a nyx whose one
/// quality gate verified nothing (observed live). The scan now FOLLOWS the sub_workflow hop, so the
/// warning surfaces at the `/ndev` entry — naming the node AND the workflow it is in — before anything
/// runs. A workflow whose gate carries real command_ids still warns about nothing.
#[test]
fn run_start_warns_through_the_sub_workflow_hop_about_a_gate_it_never_opens() {
    let _g = db_guard();
    let sandbox = temp_dir("runstart-gatewarn");
    let db = Db::in_memory();
    db.with_conn(crate::seed::seed_builtin_workflows).unwrap();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // THE USER'S ENTRY POINT: `run_start(scope:"ndev")`. ndev itself carries NO gate (lot 2 moved it
    // into the coder child), so this warning can only come through the sub_workflow hop.
    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": "ndev", "spec_id": spec_id }))
        .expect("run_start")
        .result;
    let warnings = opened["warnings"].as_array().expect("a warnings array");
    let warned = warnings
        .iter()
        .find(|w| w["code"] == "gate_unconfigured")
        .unwrap_or_else(|| {
            panic!("/ndev must be told at run_start that the flow it opens cannot verify anything: {warnings:?}")
        });

    // The graph the defect is in is NOT the one run_start opened — that is the whole finding.
    let coder_wf = db.with_conn(|c| {
        db::list_workflows(c, None)
            .unwrap()
            .into_iter()
            .find(|w| w.scope == "coder")
            .expect("coder built-in")
            .id
    });
    assert_ne!(
        opened["workflow_id"], json!(coder_wf),
        "run_start opened ndev, never the coder graph"
    );
    assert_eq!(
        warned["workflow_id"],
        json!(coder_wf),
        "…yet the warning names the CODER workflow — the scan followed the sub_workflow node",
    );
    let gate_node = db.with_conn(|c| {
        db::list_nodes(c, &coder_wf)
            .unwrap()
            .into_iter()
            .find(|n| n.type_ == db::NODE_TYPE_GATE_CHECK)
            .expect("the coder's checks gate")
            .id
    });
    assert_eq!(warned["node_id"], json!(gate_node), "and the NODE to fix");
    assert_eq!(
        warned["blocking"],
        json!(true),
        "a BLOCKING empty gate is announced as the refusal it will be, not as advice",
    );
    let message = warned["message"].as_str().expect("a human sentence");
    assert!(
        message.contains("WILL BE REFUSED")
            && message.contains("add_command")
            && message.contains("slot_bind"),
        "the warning says what will happen and how to fix it: {message}",
    );
    // The SLOT as a FIELD, not only inside the sentence: a dispatcher that means to bind it must not
    // have to parse prose for the one argument the fix takes.
    assert_eq!(
        warned["slot"],
        json!(crate::seed::BUILD_CHECK_SLOT),
        "the warning carries the slot to bind: {warned:?}",
    );

    // A workflow whose gate HAS a real command → no gate warning.
    let gate = gate_command(&db, &project_id, "true");
    let wf_ok = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "wf-ok", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("wf")
            .id
    });
    let root = add_node(&db, &wf_ok, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf_ok,
        Some(&root),
        0,
        db::NODE_TYPE_GATE_CHECK,
        json!({ "command_ids": [gate], "blocking": true }),
    );
    let opened2 = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf_ok }))
        .expect("run_start")
        .result;
    assert!(
        opened2["warnings"].as_array().expect("warnings array").is_empty(),
        "a configured gate yields no warning: {:?}",
        opened2["warnings"]
    );
}

/// A FORK THAT HAS FALLEN BEHIND SAYS SO — through the `sub_workflow` hop, which is exactly where the
/// stale one lives. THE USER'S SITUATION, reproduced: their project forked the `coder` built-in to arm
/// its gate (the only route there was), the fork sat at r11 while the built-in shipped r16, and three
/// graph fixes written since never reached them — silently, for months, because `ensure_builtin`
/// upgrades only the rows it seeded and `resolve_active_workflow` prefers the project's own.
///
/// `run_start(scope:"ndev")` never opens the coder graph, so this alarm has to walk the same edges the
/// run will, like the gate scan beside it. And it is a WARNING, not a refusal: a fork may be deliberate
/// customisation, and only its owner can judge whether the drift matters. What must not happen is that
/// nobody is told.
#[test]
fn run_start_tells_a_project_its_coder_fork_is_frozen_behind_the_builtin() {
    let _g = db_guard();
    let sandbox = temp_dir("stale-fork");
    let db = Db::in_memory();
    db.with_conn(crate::seed::seed_builtin_workflows).unwrap();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // The built-in coder, rolled back to r11 — the revision the user's fork was taken at.
    let coder_builtin = db.with_conn(|c| {
        db::list_workflows(c, None)
            .unwrap()
            .into_iter()
            .find(|w| w.scope == db::WORKFLOW_SCOPE_CODER)
            .expect("coder built-in")
            .id
    });
    db.with_conn(|c| db::set_builtin_revision(c, &coder_builtin, 11))
        .unwrap();
    let fork_id = crate::mcp_authoring::dispatch_authoring_tool(
        &db,
        "workflow_fork",
        &json!({ "workflow_id": coder_builtin, "project_id": project_id }),
    )
    .expect("routed")
    .expect("fork ok")
    .result["workflow"]["id"]
        .as_str()
        .expect("the fork's id")
        .to_string();
    // …and nyx moves on: the built-in ships r16, the fork does not.
    db.with_conn(|c| db::set_builtin_revision(c, &coder_builtin, 16))
        .unwrap();

    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": "ndev", "spec_id": spec_id }))
        .expect("run_start")
        .result;
    let warnings = opened["warnings"].as_array().expect("a warnings array");
    let stale = warnings
        .iter()
        .find(|w| w["code"] == "workflow_fork_stale")
        .unwrap_or_else(|| {
            panic!("a project running a fork frozen 5 revisions back must be TOLD: {warnings:?}")
        });
    assert_eq!(stale["workflow_id"], json!(fork_id), "it names THEIR fork");
    assert_eq!(stale["source_workflow_id"], json!(coder_builtin));
    assert_eq!(stale["forked_from_revision"], json!(11), "the revision it is frozen at");
    assert_eq!(stale["builtin_revision"], json!(16), "and the one nyx now ships");
    let message = stale["message"].as_str().expect("a human sentence");
    assert!(
        message.contains("revision 11") && message.contains("revision 16"),
        "the sentence carries BOTH numbers — that comparison is the whole finding: {message}",
    );
    assert!(
        message.contains("slot_bind") && message.contains("workflow_delete"),
        "and the way back onto the built-in, now that arming needs no fork: {message}",
    );
    // The alarm came through the HOP: run_start opened ndev, never the coder graph.
    assert_ne!(opened["workflow_id"], json!(fork_id));
    assert_ne!(opened["workflow_id"], json!(coder_builtin));

    // A fork LEVEL with its source says nothing — the warning is about drift, not about forking.
    db.with_conn(|c| db::set_forked_from_revision(c, &fork_id, Some(16)))
        .unwrap();
    let spec2 = spec_in_progress(&db, &project_id, &workspace_id);
    let quiet = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": "ndev", "spec_id": spec2 }))
        .expect("run_start")
        .result;
    assert!(
        !quiet["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|w| w["code"] == "workflow_fork_stale"),
        "a fork at its source's revision is not stale: {:?}",
        quiet["warnings"]
    );

    // A fork taken BEFORE the stamp existed (no recorded revision) is reported too, in its own terms:
    // "an unrecorded revision" is honest, and inventing a number for it would not be. This is the case
    // EVERY fork made before this change falls into.
    db.with_conn(|c| db::set_forked_from_revision(c, &fork_id, None))
        .unwrap();
    let spec3 = spec_in_progress(&db, &project_id, &workspace_id);
    let unknown = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": "ndev", "spec_id": spec3 }))
        .expect("run_start")
        .result;
    let reported = unknown["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .find(|w| w["code"] == "workflow_fork_stale")
        .unwrap_or_else(|| panic!("a fork of unknown age is still a fork nobody has compared"));
    assert!(
        reported["message"]
            .as_str()
            .expect("a sentence")
            .contains("unrecorded revision"),
        "it says the age is unknown rather than inventing one: {reported:?}",
    );
}

/// The gate scan follows `sub_workflow` edges, and a graph can reach ITSELF — nothing forbids a
/// `sub_workflow` whose target resolves back to an ancestor. Without the visited guard the walk would
/// recurse until the stack blew, turning a merely odd graph into a crash at `run_start`. It terminates,
/// and reports the cycle's gate exactly ONCE.
#[test]
fn the_gate_scan_survives_a_workflow_that_opens_itself() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("gatescan-cycle").path());

    let wf = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "ouroboros", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("wf")
            .id
    });
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_GATE_CHECK,
        json!({ "command_ids": [], "blocking": true }),
    );
    // The node that closes the loop: it opens THIS very workflow.
    add_node(
        &db,
        &wf,
        Some(&root),
        1,
        db::NODE_TYPE_SUB_WORKFLOW,
        json!({ "scope": "custom", "workflow_id": wf }),
    );

    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "workflow_id": wf }))
        .expect("a self-opening graph must not hang or blow the stack at run_start")
        .result;
    let flagged: Vec<&Value> = opened["warnings"]
        .as_array()
        .expect("a warnings array")
        .iter()
        .filter(|w| w["code"] == "gate_unconfigured")
        .collect();
    assert_eq!(
        flagged.len(),
        1,
        "the cycle's gate is reported once, not once per lap: {flagged:?}",
    );
}

/// A NON-BLOCKING gate with no commands stays a WARNING, not a refusal: it is advisory by
/// construction, so nothing downstream reads its verdict as proof — it misleads a human reading the
/// graph without ever certifying a thing. Only the BLOCKING one makes the promise H10 refuses to let
/// it break. The run walks straight past it.
#[test]
fn a_non_blocking_gate_with_no_commands_only_warns_and_the_run_walks_past_it() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("advisory-gate").path());

    let wf = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "advisory", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("wf")
            .id
    });
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    let gate = add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_GATE_CHECK,
        json!({ "command_ids": [], "blocking": false }),
    );

    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "workflow_id": wf }))
        .expect("run_start")
        .result;
    let warnings = opened["warnings"].as_array().expect("a warnings array");
    let warned = warnings
        .iter()
        .find(|w| w["node_id"] == json!(gate))
        .unwrap_or_else(|| panic!("the empty advisory gate is still flagged: {warnings:?}"));
    assert_eq!(warned["code"], json!("gate_unconfigured"));
    assert_eq!(warned["blocking"], json!(false));
    assert!(
        !warned["message"].as_str().unwrap().contains("REFUSED"),
        "an advisory gate promises nothing, so nothing is refused: {warned}",
    );

    let rt = EngineRuntime::new();
    let run_id = opened["run_id"].as_str().expect("run_id").to_string();
    assert_eq!(
        tick(&db, &rt, &run_id, None)["action"]["type"],
        "done",
        "the run walks past an advisory gate, warning and all"
    );
}

/// J1 — a graph authored BEFORE the options rule existed must not misbehave in silence. Built here
/// by the raw row CRUD (which is what a pre-check fork on disk amounts to): an `ask_human` with no
/// `options` whose answer a selector compares by equality. `nyx_next` REFUSES to even ask it — a
/// question the graph cannot honour yields no verdict, and a run must not advance on the absence of
/// one — and the refusal is STICKY and NAMES the edit that fixes it. Declaring the options resumes
/// the very same run on the very same question, so the refusal is a door and not a wall.
#[test]
fn the_runtime_refuses_to_ask_a_question_a_branch_answers_by_equality() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (_project_id, _ws, _cwd) = seed_project(&db, temp_dir("j1-legacy").path());
    let mut ask_node = String::new();
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        ask_node = add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "review the sync?", "output_key": "decision" }),
        );
        let sel = add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_SELECTOR,
            json!({ "branches": [
                { "cond": { "var": "decision", "eq": "approve" }, "child": 0 },
                { "cond": { "always": true }, "child": 1 },
            ] }),
        );
        add_node(db, wf, Some(&sel), 0, db::NODE_TYPE_NOTE, json!({ "text": "synced" }));
        add_node(db, wf, Some(&sel), 1, db::NODE_TYPE_NOTE, json!({ "text": "abandoned" }));
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id }))
        .expect_err("a question a branch answers by equality may not be asked free-form");
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("ask_human_unconstrained_branch"),
    );
    let message = &err.message;
    assert!(
        message.contains("ask_human 'ask_human'") && message.contains("'selector'"),
        "it names the question and the branch that decides on it: {message}"
    );
    assert!(
        message.contains(r#"decision == "approve""#),
        "and the comparison the free answer would have faced: {message}"
    );
    assert!(
        message.contains("node_update") && message.contains("options"),
        "and the edit that fixes it: {message}"
    );

    // NOTHING was asked and nothing walked: the run is parked ON the question, and calling again hits
    // the same refusal rather than acking past it.
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_BLOCKED);
    assert_eq!(
        run_row(&db, &run_id).current_node_id.as_deref(),
        Some(ask_node.as_str())
    );
    let again = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(
        again.data.as_ref().and_then(|d| d.get("reason")).and_then(Value::as_str),
        Some("ask_human_unconstrained_branch"),
        "the refusal is sticky",
    );

    // THE WAY OUT the message names: declare the answers the branch already compares.
    crate::mcp_authoring::dispatch_authoring_tool(
        &db,
        "node_update",
        &json!({
            "node_id": ask_node,
            "config": { "prompt": "review the sync?", "output_key": "decision",
                        "options": ["approve", "cancel"] }
        }),
    )
    .expect("node_update routed")
    .expect("node_update ok");
    let asked = tick(&db, &rt, &run_id, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(asked["action"]["options"], json!(["approve", "cancel"]));
}

/// H10 — THE USER'S SCENARIO, end to end, and the SLOT that lets them stay on nyx's graph. A `/ndev`
/// on a fresh nyx opens the seeded ndev, delegates the build to the seeded CODER child, and that child
/// reaches its `checks` gate. The gate used to run vacuously GREEN — measured live, `gate_green =
/// true`, `last_diff = "no files changed — no code written"`, one round — so the "make it green" loop
/// exited and the next node closed the task `done`. That is how a Spec reached `completed` with its
/// central feature absent, and it is why the gate now REFUSES (`invalid_state` at the tick, task never
/// closed).
///
/// What this test pins is the ROUTE OUT of that refusal, because the first one cost the project every
/// future graph fix. The gate lives in a graph SHARED by every project and used to name its own
/// `command_ids` — project data — so arming it meant FORKING the immutable built-in, and a fork is
/// never upgraded by the seeder while `resolve_active_workflow` prefers it: day one, freeze forever.
/// The gate now declares a SLOT, so the route is `add_command` + `slot_bind` and the graph is not
/// touched. Everything after the refusal here is that claim, checked: nothing is forked, THE VERY RUN
/// that was refused resumes and builds the task to `done`, and when nyx then ships a new revision of
/// the coder graph, this project — armed on day one — resolves the upgraded BUILT-IN, still armed.
#[test]
fn the_seeded_coders_empty_gate_refuses_to_certify_a_build_it_never_checked() {
    let _g = db_guard();
    let sandbox = temp_dir("vacuous-gate");
    let db = Db::in_memory();
    db.with_conn(crate::seed::seed_builtin_workflows).unwrap();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    declare_phases(&db, &spec_id, &["Build"]);
    let task_id = db
        .with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                "build the thing",
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    phase_name: Some("Build".to_string()),
                    ..db::NewTaskInput::slice()
                },
            )
        })
        .expect("task")
        .id;

    let run_id = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": "ndev", "spec_id": spec_id }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .expect("run_id")
        .to_string();

    // /ndev delegates the build to the coder child — the graph the empty gate lives in.
    let rt = EngineRuntime::new();
    let opened_child = tick(&db, &rt, &run_id, None);
    assert_eq!(opened_child["action"]["type"], "sub_workflow");
    let child = opened_child["action"]["child_run_id"]
        .as_str()
        .expect("a child run")
        .to_string();

    // The child starts the task and spawns the coder…
    assert_eq!(
        tick(&db, &rt, &child, None)["action"]["type"],
        "spawn_subagent"
    );
    assert_eq!(
        task_status(&db, &task_id),
        db::TASK_STATUS_IN_PROGRESS,
        "the task was started before the coder ran"
    );

    // …which reports, VERBATIM, what the live run reported: nothing was written. The next node is the
    // gate, and the gate is what must not bless this.
    let err = super::nyx_next(
        &db,
        &rt, no_session(),
        &json!({
            "run_id": child,
            "last_result": { "diff": "no files changed — no code written" },
        }),
    )
    .expect_err("a gate that verifies nothing may not let the loop exit");

    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("gate_unconfigured"),
    );
    let gate_node = db.with_conn(|c| {
        let coder_wf = db::get_workflow_run(c, &child).unwrap().unwrap().workflow_id;
        db::list_nodes(c, &coder_wf)
            .unwrap()
            .into_iter()
            .find(|n| n.type_ == db::NODE_TYPE_GATE_CHECK)
            .expect("the coder's checks gate")
            .id
    });
    assert_eq!(
        err.data.as_ref().and_then(|d| d.get("node_id")).and_then(Value::as_str),
        Some(gate_node.as_str()),
        "the refusal points at the gate node the run is parked on",
    );
    // The sentence a user must be able to ACT on: which node, what the gate is missing, and a route
    // out that WORKS. The gate lives in a graph SHARED by every project, so the missing thing is not
    // in the graph at all — it is this project's answer to the slot the graph declares.
    let message = &err.message;
    assert!(message.contains("gate_check 'checks'"), "it names the node: {message}");
    assert!(
        message.contains("without verifying anything"),
        "it says what advancing would certify: {message}"
    );
    assert!(
        message.contains("build_check"),
        "it names the SLOT — the argument the fix takes: {message}"
    );
    assert!(
        message.contains("add_command") && message.contains("slot_bind"),
        "it names the verbs that fix it: {message}"
    );
    assert!(
        message.contains(&project_id),
        "…and the project the binding is for: {message}"
    );
    // The route that must NOT be named any more: forking is what froze a project's coder graph at the
    // revision of the day it was armed, and a message still recommending it would keep re-creating the
    // very defect the slot removes.
    assert!(
        !message.contains("workflow_fork"),
        "the fork is no longer the route out of an unarmed shared gate: {message}"
    );

    // NOTHING was certified: the task is still open and the run is parked, not walked past.
    assert_eq!(
        task_status(&db, &task_id),
        db::TASK_STATUS_IN_PROGRESS,
        "the gate refused, so the task was never marked done",
    );
    assert_eq!(run_row(&db, &child).status, db::RUN_STATUS_BLOCKED);
    // STICKY — calling again re-attempts the gate and hits the same refusal, never walks past it.
    let again = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": child })).unwrap_err();
    assert_eq!(
        again.data.as_ref().and_then(|d| d.get("reason")).and_then(Value::as_str),
        Some("gate_unconfigured"),
        "the refusal is sticky — re-ticking does not ack past it",
    );

    // THE WAY OUT, walked EXACTLY as the message dictates and through the REAL verbs: register the
    // command, bind it to the project's `build_check` slot. TWO calls, and NOTHING is forked — the
    // graph is not touched at all, which is the whole point: the coder the project runs stays the
    // shared built-in and keeps receiving its upgrades.
    let command = gate_command(&db, &project_id, "true");
    let authoring = |name: &str, args: Value| {
        crate::mcp_authoring::dispatch_authoring_tool(&db, name, &args)
            .unwrap_or_else(|| panic!("`{name}` was not routed"))
            .unwrap_or_else(|e| panic!("`{name}` failed: {}", e.message))
            .result
    };
    authoring(
        "slot_bind",
        json!({
            "project_id": project_id,
            "slot": crate::seed::BUILD_CHECK_SLOT,
            "command_ids": [command],
        }),
    );
    let coder_builtin = db.with_conn(|c| {
        db::get_workflow_run(c, &child).unwrap().unwrap().workflow_id
    });
    assert!(
        db.with_conn(|c| db::list_workflows(c, Some(&project_id))).unwrap().is_empty(),
        "arming the gate created NO project workflow — nothing was forked",
    );

    // …and THIS run resumes. The binding is re-read at every tick, so the refused child picks the
    // command up on its very next call — no fresh child, no fork, no run surgery. That is what the
    // refusal promised, and it is only true because the armed node is the one the run is walking.
    let mut last: Option<Value> = None;
    let mut steps = 0;
    loop {
        let out = tick(&db, &rt, &child, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "done" => break,
            "spawn_subagent" => last = Some(json!({ "diff": "the real fix" })),
            other => panic!("unexpected coder child action '{other}': {out}"),
        }
        steps += 1;
        assert!(steps < 40, "the newly bound slot must let the refused child finish");
    }
    assert_eq!(
        task_status(&db, &task_id),
        db::TASK_STATUS_DONE,
        "with the slot bound, the very run that was refused builds the task to done",
    );

    // A SECOND `/ndev` is told nothing is wrong: the same scan that raised the alarm confirms the fix,
    // and it confirms it on the BUILT-IN — the graph the run resolves is still nyx's own.
    let spec2 = spec_in_progress(&db, &project_id, &workspace_id);
    declare_phases(&db, &spec2, &["Build"]);
    db.with_conn(|c| {
        db::create_task(
            c,
            &project_id,
            "build the other thing",
            db::NewTaskInput {
                spec_id: Some(spec2.clone()),
                phase_name: Some("Build".to_string()),
                ..db::NewTaskInput::slice()
            },
        )
    })
    .expect("task");
    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": "ndev", "spec_id": spec2 }))
        .expect("run_start")
        .result;
    assert!(
        opened["warnings"].as_array().expect("a warnings array").is_empty(),
        "the bound slot silences the alarm, and no fork means no staleness warning either: {:?}",
        opened["warnings"]
    );

    // THE POINT OF THE WHOLE THING: nyx ships a NEW revision of the coder graph, and this project —
    // which armed its gate on day one — GETS it. The stored built-in is rolled back to an older
    // revision with a mutilated tree (a graph from before the fix), then boot re-seeds: the row is
    // rebuilt at the current revision, and it is still the row this project's runs resolve.
    db.with_conn(|c| {
        db::set_builtin_revision(c, &coder_builtin, 0).unwrap();
        let gate = db::list_nodes(c, &coder_builtin)
            .unwrap()
            .into_iter()
            .find(|n| n.type_ == db::NODE_TYPE_GATE_CHECK)
            .expect("the checks gate");
        db::update_node(c, &gate.id, Some("stale label from an old revision"), None).unwrap();
    });
    db.with_conn(crate::seed::seed_builtin_workflows).unwrap();
    let (revision, gate_label) = db.with_conn(|c| {
        let wf = db::get_workflow(c, &coder_builtin).unwrap().expect("the built-in survives");
        let label = db::list_nodes(c, &coder_builtin)
            .unwrap()
            .into_iter()
            .find(|n| n.type_ == db::NODE_TYPE_GATE_CHECK)
            .expect("the rebuilt gate")
            .label;
        (wf.builtin_revision, label)
    });
    assert!(
        revision.unwrap_or(0) > 0 && gate_label == "checks",
        "the boot upgrade replaced the coder's tree in place (revision {revision:?}, gate '{gate_label}')",
    );
    let spec3 = spec_in_progress(&db, &project_id, &workspace_id);
    let resolved = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": "ndev", "spec_id": spec3 }))
        .expect("run_start")
        .result;
    let child_wf = db.with_conn(|c| {
        let run = db::get_workflow_run(c, resolved["run_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        db::resolve_active_workflow(c, Some(&project_id), db::WORKFLOW_SCOPE_CODER)
            .unwrap()
            .map(|w| (w.id, run.id))
            .expect("a coder workflow resolves for this project")
    });
    assert_eq!(
        child_wf.0, coder_builtin,
        "the project still resolves the BUILT-IN coder — so the upgraded graph is the one it runs",
    );
    assert!(
        resolved["warnings"].as_array().expect("warnings").is_empty(),
        "and the upgraded graph is armed by the same binding — no re-arming after an upgrade: {:?}",
        resolved["warnings"]
    );
}

/// B1 (superseding the B3 cursor-signal refusal) — a run parked on its FIRST leaf has an EMPTY
/// blackboard yet a cursor; a cold session must still RESUME it from the persisted traversal. The old
/// blackboard-emptiness heuristic would have mistaken this for a FRESH run and replayed from the
/// root, re-spawning the first sub-agent — the exact edge this locks. The proof: a fresh runtime
/// folds the first spawn's diff and advances to the SECOND leaf; it does not re-yield the spawn.
#[test]
fn a_first_leaf_run_with_an_empty_blackboard_resumes_on_a_cold_session() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
        add_node(db, wf, Some(&root), 1, db::NODE_TYPE_NOTE, json!({ "text": "next" }));
    });

    // Session A parks on the first spawn — NO diff reported, so the blackboard stays empty.
    let rt_a = EngineRuntime::new();
    assert_eq!(
        tick(&db, &rt_a, &run_id, None)["action"]["type"],
        "spawn_subagent"
    );
    let row = run_row(&db, &run_id);
    // The board holds NO graph key — nothing has been folded yet. What it does hold is nyx's own
    // namespaced mark that the spawn's context was forwarded (F6), which is not a progress signal
    // any more than `__child_run_id__<node>` is.
    let board: serde_json::Map<String, Value> = serde_json::from_str(&row.blackboard).unwrap();
    assert!(
        board.keys().all(|k| k.starts_with("__")),
        "the first-leaf blackboard carries no graph key: {board:?}",
    );
    assert!(
        row.current_node_id.is_some(),
        "yet the cursor is set — the B3 gap"
    );
    assert!(
        row.traversal.is_some(),
        "and the traversal was persisted — so it is resumable (B1)"
    );

    // Session B (a fresh runtime): resumes from the traversal, folds the spawn's diff, and advances
    // to the SECOND leaf — it does NOT re-yield (re-spawn) the first.
    let rt_b = EngineRuntime::new();
    let out = tick(&db, &rt_b, &run_id, Some(json!({ "diff": "work" })));
    assert_eq!(
        out["action"]["type"], "done",
        "resumed and advanced past the spawn, not re-spawned"
    );
    assert_eq!(
        out["action"]["notes"],
        json!(["next"]),
        "the note it walked past on the way out rides the done"
    );
    assert_eq!(
        out["blackboard_delta"]["spawns"],
        json!(1),
        "the coder's diff folded this tick — spawns=1 in the delta (N22)"
    );
}

/// m3 — a FINISHED run reports it is finished, not the "traversal state gone" resume refusal a
/// mid-flight run gets. Two distinct situations, two distinct messages.
#[test]
fn a_finished_run_reports_finished_not_lost() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(db, wf, Some(&root), 0, db::NODE_TYPE_NOTE, json!({}));
    });

    // Drive it to done in session A.
    let rt_a = EngineRuntime::new();
    assert_eq!(tick(&db, &rt_a, &run_id, None)["action"]["type"], "done");

    // A fresh session ticking the DONE run gets a clear "finished" reason, not "run_not_resumable".
    let rt_b = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt_b, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("run_finished")
    );
}

/// H1 — in a MULTI-GATE sequence, a GREEN gate must NOT clear a DIFFERENT gate's red anchor. With
/// `sequence[ gate_A(red,blocking), gate_B(green,blocking), transition ]`, nyx runs both gates inline
/// then reaches the transition: it is REFUSED because A is still red — B going green does not lift
/// A's block, and the Spec is never advanced. (The old single-anchor logic cleared A on B's green.)
#[test]
fn a_green_gate_does_not_clear_another_gates_red_block() {
    let _g = db_guard();
    let sandbox = temp_dir("multigate");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let red = gate_command(&db, &project_id, "false");
    let green = gate_command(&db, &project_id, "true");

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [red], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [green], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            2,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("gate_red"),
        "the still-red gate A blocks the transition despite green gate B"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_IN_PROGRESS,
        "the Spec is never advanced past the red gate"
    );
}

/// L-a — `run_start` with an explicit UNKNOWN spec_id fails clean (`invalid_id`), not an opaque FK
/// error from the run insert.
#[test]
fn run_start_rejects_an_unknown_spec_id() {
    let _g = db_guard();
    let db = Db::in_memory();
    let err = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": "no-such-spec" })).unwrap_err();
    assert_eq!(err.code, "invalid_id");
}

// ---------------------------------------------------------------------------
// A SPEC-SCOPED flow refuses to start UNBOUND (the orphan-run defect)
// ---------------------------------------------------------------------------

/// `resolve_run_target` yields no Spec when NOTHING anchors one (no spec_id, no cwd/workspace_id, or a
/// workspace holding none). A spec-scoped flow used to open an ORPHAN run then: its `spec_status` /
/// `spec_open_tasks` / `spec_can_transition` queries resolve against nothing, its Spec transitions
/// apply to nothing, and the run cannot be found FROM the Spec it was meant to drive. It now REFUSES,
/// the same way the several-Specs case already did.
#[test]
fn run_start_refuses_a_spec_scoped_flow_with_no_resolvable_spec() {
    let _g = db_guard();
    let db = Db::in_memory();
    // A workflow must EXIST for each scope, or the "no workflow to run" refusal fires first (the
    // guard is judged on the RESOLVED workflow, so it cannot speak before one is resolved).
    for scope in [
        db::WORKFLOW_SCOPE_NDEV,
        db::WORKFLOW_SCOPE_NDOC,
        db::WORKFLOW_SCOPE_CODER,
        db::WORKFLOW_SCOPE_PROTOTYPER,
    ] {
        let wf = db
            .with_conn(|c| db::create_workflow(c, None, scope, scope, None))
            .expect("workflow");
        let err = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": scope, "workflow_id": wf.id })).unwrap_err();
        assert_eq!(
            err.code, "invalid_argument",
            "{scope} must refuse to start unbound, got: {}",
            err.message
        );
        assert!(
            err.message.contains("spec_id"),
            "{scope}'s refusal must say how to fix it, got: {}",
            err.message
        );
    }
}

/// `run_start` SIGNALS a SPEC-SCOPED question asked by a graph whose run is bound to NO Spec. Every
/// `query` fact answers the EMPTY value on an unbound run rather than erroring — right for the fact,
/// silent for the decision taken on it: `0 open tasks` reads exactly like a finished plan, so a
/// branch routes to "everything is done" and nothing says why. The warning makes it visible BEFORE
/// the first tick.
#[test]
fn run_start_warns_when_a_graph_questions_a_spec_its_run_has_none_of() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("runstart-queryspec").path());

    // A `custom` graph — the one scope that may start unbound WITHOUT having been designed for it.
    let wf = db
        .with_conn(|c| {
            db::create_workflow(c, Some(&project_id), "asks", db::WORKFLOW_SCOPE_CUSTOM, None)
        })
        .expect("workflow")
        .id;
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    let asks = add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_QUERY,
        json!({ "query": "spec_open_tasks", "write": "open" }),
    );

    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "workflow_id": wf }))
        .expect("a custom flow may start unbound")
        .result;
    assert_eq!(opened["spec_id"], Value::Null, "the run is indeed unbound");
    let warnings = opened["warnings"].as_array().expect("a warnings array");
    let warned = warnings
        .iter()
        .find(|w| w["code"] == "query_without_spec")
        .unwrap_or_else(|| panic!("the spec-scoped question is flagged: {warnings:?}"));
    assert_eq!(warned["node_id"], json!(asks), "the warning names the NODE");
    assert_eq!(
        warned["query"],
        json!("spec_open_tasks"),
        "…and the FACT it asks, so the human knows what will answer empty",
    );

    // THE SILENCE THE WARNING IS ABOUT: the run ticks happily and the fact answers `0` — a value a
    // branch cannot tell from a plan with nothing left. The warning is the only thing that says so.
    let rt = EngineRuntime::new();
    let run_id = opened["run_id"].as_str().expect("run_id").to_string();
    assert_eq!(tick(&db, &rt, &run_id, None)["action"]["type"], "done");
    assert_eq!(
        Blackboard::from_json_str(&run_row(&db, &run_id).blackboard).get("open"),
        Some(&json!(0)),
        "the unbound fact answered EMPTY, exactly as a finished plan would",
    );
}

/// …and `nspec` is NOT warned about, because it questions a Spec it has not created yet ON PURPOSE:
/// its first node reads `spec_status` and its second BRANCHES on the null to create one. Warning
/// there would fire on every authoring start — a false alarm on the designed unbound flow.
#[test]
fn run_start_does_not_warn_the_flow_designed_to_start_without_a_spec() {
    let _g = db_guard();
    let db = Db::in_memory();
    db.with_conn(crate::seed::seed_builtin_workflows).unwrap();

    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": db::WORKFLOW_SCOPE_NSPEC }))
        .expect("authoring starts unbound")
        .result;
    assert_eq!(opened["spec_id"], Value::Null, "the nspec run is unbound");
    let warnings = opened["warnings"].as_array().expect("a warnings array");
    assert!(
        !warnings.iter().any(|w| w["code"] == "query_without_spec"),
        "nspec reads spec_status with no Spec BY DESIGN — not a finding: {warnings:?}",
    );
}

/// `nspec` is the deliberate EXCEPTION: authoring is the one flow allowed to begin with no Spec (its
/// entry gate has a "create a new Spec" branch), so it must NOT be caught by the guard above. It fails
/// here only for the unrelated reason that nothing is seeded — never `invalid_argument`.
#[test]
fn run_start_still_lets_nspec_start_without_a_spec() {
    let _g = db_guard();
    let db = Db::in_memory();
    let wf = db
        .with_conn(|c| db::create_workflow(c, None, "nspec", db::WORKFLOW_SCOPE_NSPEC, None))
        .expect("workflow");
    let out = super::run_start(&db, &EngineRuntime::new(), &json!({ "workflow_id": wf.id }))
        .expect("authoring must START without a Spec — it is the flow that creates one");
    assert_eq!(out.result["spec_id"], Value::Null, "the nspec run is legitimately unbound");
}

/// The guard only closes the UNBOUND door: a spec-scoped flow given an explicit Spec still starts.
#[test]
fn run_start_still_starts_a_spec_scoped_flow_when_the_spec_is_explicit() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("spec-scoped-ok").path());
    let spec_id = spec_ready(&db, &project_id);
    let wf = spawn_workflow(&db, &project_id);
    let out = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "scope": db::WORKFLOW_SCOPE_NDEV, "spec_id": spec_id, "workflow_id": wf }),
    )
    .expect("an explicitly-bound ndev run starts");
    assert_eq!(out.result["spec_id"], json!(spec_id));
}

// ---------------------------------------------------------------------------
// N20 — run_start is IDEMPOTENT: at most ONE live run per (workflow, spec)
// ---------------------------------------------------------------------------

/// A minimal ndev-shaped workflow on `project_id` — `sequence{ spawn(coder) }` — returning its id.
/// Enough for `run_start` to resolve a run to; the tests below never need to drive it.
fn spawn_workflow(db: &Db, project_id: &str) -> String {
    let wf = db.with_conn(|c| {
        db::create_workflow(c, Some(project_id), "ndev", db::WORKFLOW_SCOPE_NDEV, None)
            .expect("wf")
            .id
    });
    let root = add_node(db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_SPAWN_SUBAGENT,
        json!({ "agent_id": "coder" }),
    );
    wf
}

/// The runs of a (workflow, spec) pair, newest-first.
fn runs_for(db: &Db, wf: &str, spec_id: &str) -> Vec<db::WorkflowRun> {
    db.with_conn(|c| db::find_runs_for_spec(c, wf, Some(spec_id)))
        .expect("find_runs_for_spec")
}

/// N20 — `run_start` TWICE for the same (spec, scope) is IDEMPOTENT: it returns the SAME run_id and
/// leaves EXACTLY ONE run row (no second row), and the idempotent return still carries the full
/// payload shape (incl. the N26a `warnings`).
#[test]
fn run_start_twice_is_idempotent_same_run_one_row() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("n20-idem").path());
    let spec_id = spec_ready(&db, &project_id);
    let wf = spawn_workflow(&db, &project_id);

    let first = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result;
    let id1 = first["run_id"].as_str().expect("run_id").to_string();

    let second = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result;
    let id2 = second["run_id"].as_str().expect("run_id").to_string();

    assert_eq!(id1, id2, "idempotent: the second run_start returns the SAME run_id");
    assert_eq!(runs_for(&db, &wf, &spec_id).len(), 1, "no second row was created");

    // The idempotent return keeps the full payload shape callers/tests depend on.
    assert_eq!(second["workflow_id"], wf);
    assert_eq!(second["workflow_name"], "ndev");
    assert_eq!(second["spec_id"], spec_id);
    assert_eq!(second["scope"], "ndev");
    assert!(second["warnings"].is_array(), "warnings recomputed on the idempotent return");
}

/// H2 — an INTERRUPTED `/nspec` must not CONFISCATE the next one. Observed live: `/nspec <a new
/// subject>` landed, silently, mid-prototype-round of an ABANDONED authoring session from the day
/// before, on another subject, in another PROJECT — the typed request was never read. The
/// convergence is keyed on the Spec; an authoring run has none yet, and the `nspec` workflow is a
/// BUILT-IN (`project_id IS NULL`) shared by every project — so the pair `(nspec, NULL)` was ONE
/// slot for the whole installation and the first unfinished run held it forever.
///
/// The user's own two shapes, on the ONE built-in workflow: another PROJECT, and the SAME workspace
/// with another subject. Both must open their OWN run, and neither may destroy the abandoned one.
#[test]
fn a_second_nspec_start_opens_its_own_run_never_the_abandoned_one() {
    let _g = db_guard();
    let db = Db::in_memory();
    db.with_conn(crate::seed::seed_builtin_workflows).expect("built-ins");
    let sandbox_a = temp_dir("h2-nspec-a");
    let sandbox_b = temp_dir("h2-nspec-b");
    let path_a = db.with_conn(|c| {
        db::create_project(c, "Alpha", sandbox_a.path().to_str().unwrap(), None)
            .expect("project A")
            .1
            .root_path
    });
    let path_b = db.with_conn(|c| {
        db::create_project(c, "Beta", sandbox_b.path().to_str().unwrap(), None)
            .expect("project B")
            .1
            .root_path
    });
    let start_nspec = |cwd: &str| {
        super::run_start(&db, &EngineRuntime::new(), &json!({ "scope": db::WORKFLOW_SCOPE_NSPEC, "cwd": cwd }))
            .expect("an authoring run starts unbound")
            .result
    };

    // Yesterday, in project A: an authoring session left UNFINISHED (still `running`, unbound).
    let abandoned = start_nspec(&path_a);
    let abandoned_id = abandoned["run_id"].as_str().expect("run_id").to_string();
    let builtin = abandoned["workflow_id"].as_str().expect("workflow_id").to_string();
    assert!(abandoned["spec_id"].is_null(), "the authoring run is unbound: {abandoned}");

    // Today, in ANOTHER project: a brand-new subject.
    let other_project = start_nspec(&path_b);
    // …and today again in project A's OWN workspace, on yet another subject.
    let same_workspace = start_nspec(&path_a);

    let other_id = other_project["run_id"].as_str().expect("run_id");
    let same_ws_id = same_workspace["run_id"].as_str().expect("run_id");
    // All three resolved the SAME built-in row — that sharing is exactly what made it one slot.
    assert_eq!(other_project["workflow_id"], json!(builtin));
    assert_eq!(same_workspace["workflow_id"], json!(builtin));
    assert_ne!(other_id, abandoned_id, "another project's /nspec opened its OWN run");
    assert_ne!(
        same_ws_id, abandoned_id,
        "a new subject in the SAME workspace opened its OWN run, not the stranger's session",
    );
    assert_ne!(other_id, same_ws_id, "and the two new sessions are distinct runs");

    // The abandoned session is neither resumed nor destroyed — it is somebody else's, still there.
    assert!(
        db.with_conn(|c| db::get_workflow_run(c, &abandoned_id)).unwrap().is_some(),
        "the abandoned run was left alone, never deleted as a duplicate",
    );
    let unbound = db
        .with_conn(|c| db::find_runs_for_spec(c, &builtin, None))
        .expect("the built-in's unbound runs");
    assert_eq!(unbound.len(), 3, "three independent authoring sessions coexist: {unbound:?}");
}

/// N20 — a run that reached a TERMINAL status, then `run_start` again ⇒ a FRESH run (new id); the
/// terminal one is deleted, and exactly one run remains.
#[test]
fn run_start_after_a_terminal_run_creates_a_fresh_one() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("n20-term").path());
    let spec_id = spec_ready(&db, &project_id);
    let wf = spawn_workflow(&db, &project_id);

    let id1 = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Drive it to a TERMINAL status.
    db.with_conn(|c| db::update_workflow_run(c, &id1, None, "{}", db::RUN_STATUS_DONE, None))
        .unwrap();

    let id2 = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_ne!(id1, id2, "a terminal run yields a FRESH run, not the finished one");
    let runs = runs_for(&db, &wf, &spec_id);
    assert_eq!(runs.len(), 1, "exactly one run remains");
    assert_eq!(runs[0].id, id2, "and it is the fresh one");
    assert!(
        db.with_conn(|c| db::get_workflow_run(c, &id1)).unwrap().is_none(),
        "the terminal run was deleted"
    );
}

/// N20 — a BLOCKED run (parked by a RED blocking gate, M5's setup) is RESUMABLE: `run_start` RESUMES
/// it (same id, no new row), proving `blocked` is not treated as terminal.
#[test]
fn run_start_resumes_a_blocked_run() {
    let _g = db_guard();
    let sandbox = temp_dir("n20-blocked");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    // The sentinel is never created → the blocking gate stays RED and parks the run `blocked` (M5).
    let gate = gate_command(&db, &project_id, "test -f never_created");
    let wf = db.with_conn(|c| {
        db::create_workflow(c, Some(&project_id), "ndev", db::WORKFLOW_SCOPE_NDEV, None)
            .expect("wf")
            .id
    });
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_GATE_CHECK,
        json!({ "command_ids": [gate], "blocking": true }),
    );
    add_node(
        &db,
        &wf,
        Some(&root),
        1,
        db::NODE_TYPE_TRANSITION,
        json!({ "target_state": "in_review" }),
    );

    let id1 = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    // One tick runs the red gate → refused, run parked `blocked`.
    let rt = EngineRuntime::new();
    super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": id1 })).unwrap_err();
    assert_eq!(run_row(&db, &id1).status, db::RUN_STATUS_BLOCKED, "parked blocked");

    // run_start again → RESUMES the blocked run (same id), no new row.
    let id2 = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(id1, id2, "a blocked run is RESUMED, not replaced (blocked is resumable — M5)");
    assert_eq!(runs_for(&db, &wf, &spec_id).len(), 1, "no new row");
    assert_eq!(
        run_row(&db, &id1).status,
        db::RUN_STATUS_BLOCKED,
        "still the same blocked run"
    );
}

/// N20 judged resumability on STATUS ALONE, so a run whose persisted `traversal` points at a node
/// the workflow no longer holds (the graph was edited under it — the N11 built-in replacement does
/// exactly this) stayed `running` in the DB while `build_engine` REFUSED to resume it. `run_start`
/// handed that corpse back forever; the user had to delete the run by hand. It must now DETECT the
/// dead run, replace it, and give the caller a run that actually TICKS.
#[test]
fn run_start_replaces_a_run_whose_traversal_no_longer_fits_the_tree() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("n20-stale").path());
    let spec_id = spec_ready(&db, &project_id);
    let wf = spawn_workflow(&db, &project_id);

    let id1 = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Tick once so the run PROGRESSES: it parks on the spawn leaf, persisting a cursor AND the
    // traversal stack keyed by the node ids of the CURRENT tree.
    let rt = EngineRuntime::new();
    let out = tick(&db, &rt, &id1, None);
    assert_eq!(out["action"]["type"], "spawn_subagent", "parked on the spawn");
    let before = run_row(&db, &id1);
    assert!(before.traversal.is_some(), "the walk was persisted");
    assert!(!db::run_status_is_terminal(&before.status), "still live by STATUS");

    // EDIT the graph structurally (the N11 wipe-and-rebuild): every node id changes, so the
    // persisted traversal now references nodes that no longer exist.
    db.with_conn(|c| db::delete_nodes_for_workflow(c, &wf)).expect("wipe");
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_SPAWN_SUBAGENT,
        json!({ "agent_id": "coder" }),
    );
    // The run is now DEAD: live by status, but un-tickable.
    let dead = run_row(&db, &id1);
    assert!(!db::run_status_is_terminal(&dead.status), "the corpse still reads live");
    super::nyx_next(&db, &EngineRuntime::new(), no_session(), &json!({ "run_id": id1 }))
        .expect_err("a stale traversal refuses to replay");

    // run_start REPLACES it: a new run id, and still exactly ONE row for the pair (N20 holds).
    let id2 = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(id1, id2, "the dead run is REPLACED, not handed back");
    let after = runs_for(&db, &wf, &spec_id);
    assert_eq!(after.len(), 1, "still at most one live run for the pair");
    assert_eq!(after[0].id, id2, "and it is the fresh one");
    assert!(
        db.with_conn(|c| db::get_workflow_run(c, &id1)).unwrap().is_none(),
        "the dead run was deleted"
    );

    // And the run the caller got actually TICKS (the whole point — no more dead end).
    let fresh = tick(&db, &EngineRuntime::new(), &id2, None);
    assert_eq!(
        fresh["action"]["type"], "spawn_subagent",
        "the replacement run drives the rebuilt graph"
    );
}

/// The other side of the same check: a run that is GENUINELY resumable (it has progressed and its
/// traversal still fits the tree) is RETURNED AS-IS. The verification must never replace a healthy
/// run — N20's "a resumable run is resumed, never replaced" contract.
#[test]
fn run_start_still_resumes_a_progressed_run_whose_traversal_fits() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("n20-alive").path());
    let spec_id = spec_ready(&db, &project_id);
    let wf = spawn_workflow(&db, &project_id);

    let id1 = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let rt = EngineRuntime::new();
    tick(&db, &rt, &id1, None);
    let progressed = run_row(&db, &id1);
    assert!(progressed.current_node_id.is_some() && progressed.traversal.is_some());

    let resumed = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result;
    let id2 = resumed["run_id"].as_str().unwrap().to_string();
    assert_eq!(id1, id2, "a healthy progressed run is RESUMED, not replaced");
    assert_eq!(runs_for(&db, &wf, &spec_id).len(), 1, "no new row");
    let after = run_row(&db, &id1);
    assert_eq!(
        after.traversal, progressed.traversal,
        "and its walk is untouched (no reset, no replay)"
    );
    assert_eq!(after.current_node_id, progressed.current_node_id);
    // M4 — and nothing is announced: a plain resume dropped no run, so it carries NO replacement
    // warning (the warning must mean something when it appears).
    assert!(
        !resumed["warnings"]
            .as_array()
            .expect("a warnings array")
            .iter()
            .any(|w| w["code"] == "run_replaced_graph_changed"),
        "a normal resume warns about no replacement: {:?}",
        resumed["warnings"]
    );
}

/// M4 — the D3 replacement must not be SILENT. When `run_start` drops a run whose graph changed
/// under it, the caller gets the fresh run AND a `warnings` entry naming the DROPPED run and why its
/// progress could not be resumed — the whole defect was that the user lost a run's progress and was
/// told nothing.
#[test]
fn run_start_warns_that_it_replaced_a_run_whose_graph_changed() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("m4-warn").path());
    let spec_id = spec_ready(&db, &project_id);
    let wf = spawn_workflow(&db, &project_id);

    let dropped = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Progress it, so it has a real walk to lose.
    tick(&db, &EngineRuntime::new(), &dropped, None);
    assert!(run_row(&db, &dropped).traversal.is_some(), "the walk was persisted");

    // REPLACE the graph under it (the N11 wipe-and-rebuild): every node id changes.
    db.with_conn(|c| db::delete_nodes_for_workflow(c, &wf)).expect("wipe");
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_SPAWN_SUBAGENT,
        json!({ "agent_id": "coder" }),
    );

    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result;
    let fresh = opened["run_id"].as_str().unwrap().to_string();
    assert_ne!(fresh, dropped, "the caller gets a FRESH run (D3 is unchanged)");

    let warning = opened["warnings"]
        .as_array()
        .expect("a warnings array")
        .iter()
        .find(|w| w["code"] == "run_replaced_graph_changed")
        .unwrap_or_else(|| panic!("the replacement is announced: {:?}", opened["warnings"]));
    assert_eq!(
        warning["dropped_run_id"], json!(dropped),
        "the warning NAMES the run that was destroyed"
    );
    assert_eq!(
        warning["reason"], "run_traversal_stale",
        "and carries build_engine's own machine reason"
    );
    let message = warning["message"].as_str().expect("a human sentence");
    assert!(
        message.contains(&dropped) && message.contains("graph changed under it"),
        "the sentence says WHICH run was dropped and WHY: {message}"
    );
}

/// L1, THE SECOND DELETER — `run_start` REPLACING a dead run must take that run's tick guard, exactly
/// as `run_delete` does.
///
/// The guard exists because a tick is a read-modify-write and deleting the row under it throws away
/// what it already did. `run_delete` takes it and says so; this path deleted the same kind of row
/// holding nothing — and it is not a theoretical second door. A tick already inside the run reached it
/// from its RESIDENT engine slot, so it never re-ran `build_engine` and never saw what makes the row
/// look dead to whoever opens it next; it can be mid-gate, and its `persist` would then match no row
/// (`run_vanished`) with the gate run and the transition imposed. `persist`'s own doc argues that
/// cannot happen "because run_delete holds the same tick guard": this is the site that made it false.
#[test]
fn run_start_refuses_to_replace_a_run_a_tick_is_walking() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("l1-replace-busy").path());
    let spec_id = spec_ready(&db, &project_id);
    let wf = spawn_workflow(&db, &project_id);
    let rt = EngineRuntime::new();

    let doomed = super::run_start(
        &db,
        &rt,
        &json!({ "spec_id": spec_id, "workflow_id": wf }),
    )
    .expect("run_start")
    .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Progress it (through THIS runtime, so the registry holds its slot) and then replace the graph
    // under it: the row now reads `running` while `build_engine` refuses it — the corpse shape.
    tick(&db, &rt, &doomed, None);
    db.with_conn(|c| db::delete_nodes_for_workflow(c, &wf))
        .expect("wipe");
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_SPAWN_SUBAGENT,
        json!({ "agent_id": "coder" }),
    );

    // A tick is INSIDE the doomed run (what a second worker's `nyx_next` holds for the whole of its
    // read-walk-persist).
    let held = rt.enter_tick(&doomed).expect("the guard was free");
    let err = super::run_start(
        &db,
        &rt,
        &json!({ "spec_id": spec_id, "workflow_id": wf }),
    )
    .expect_err("the replacement waits for the tick that is inside the run");
    assert_eq!(
        err.data.as_ref().unwrap()["reason"],
        "run_busy",
        "refused with the SAME marker the other two guard sites use: {}",
        err.message
    );
    assert!(
        db.with_conn(|c| db::get_workflow_run(c, &doomed))
            .unwrap()
            .is_some(),
        "and NOTHING was destroyed — the tick still has a row to persist onto"
    );

    // The tick returns → the very same call goes through, drops the corpse and hands back a fresh run.
    drop(held);
    let opened = super::run_start(
        &db,
        &rt,
        &json!({ "spec_id": spec_id, "workflow_id": wf }),
    )
    .expect("run_start")
    .result;
    assert_ne!(opened["run_id"], json!(doomed), "a FRESH run (D3 unchanged)");
    assert!(
        !rt.session_run_ids().contains(&doomed),
        "and the dead run's engine slot went with its row: the registry would otherwise keep its \
         whole tree + blackboard, and still count it among the runs this session drives"
    );
}

/// M4 — when the run carries the marker the seeder poses at a built-in UPGRADE, the warning names
/// THAT cause (and echoes the record) instead of the generic "the graph changed": the difference
/// between "nyx replaced this graph itself at boot" and "someone edited it" is the whole point of
/// recording the replacement.
#[test]
fn the_replacement_warning_names_a_recorded_built_in_upgrade() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("m4-cause").path());
    let spec_id = spec_ready(&db, &project_id);
    let wf = spawn_workflow(&db, &project_id);

    let dropped = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    tick(&db, &EngineRuntime::new(), &dropped, None);

    // What the seeder does at a stale built-in: RECORD the replacement, then wipe + rebuild.
    let marked = db
        .with_conn(|c| {
            db::mark_runs_graph_replaced(
                c,
                &wf,
                &json!({ "cause": "builtin_upgrade", "from_revision": 10, "to_revision": 11 }),
            )
        })
        .expect("mark");
    assert_eq!(marked, vec![dropped.clone()], "the progressed run is the one marked");
    db.with_conn(|c| db::delete_nodes_for_workflow(c, &wf)).expect("wipe");
    let root = add_node(&db, &wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf,
        Some(&root),
        0,
        db::NODE_TYPE_SPAWN_SUBAGENT,
        json!({ "agent_id": "coder" }),
    );

    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result;
    let warning = opened["warnings"]
        .as_array()
        .expect("a warnings array")
        .iter()
        .find(|w| w["code"] == "run_replaced_graph_changed")
        .unwrap_or_else(|| panic!("the replacement is announced: {:?}", opened["warnings"]));
    assert_eq!(
        warning["graph_replaced"],
        json!({ "cause": "builtin_upgrade", "from_revision": 10, "to_revision": 11 }),
        "the recorded marker rides along verbatim"
    );
    let message = warning["message"].as_str().expect("a human sentence");
    assert!(
        message.contains("UPGRADED") && message.contains("10 → 11"),
        "the sentence names the real cause, revisions included: {message}"
    );
}

/// N20 — legacy-mess CONVERGENCE: several runs already exist for one (workflow, spec) (the observed
/// mess). `run_start` returns the MOST-RECENT resumable one and deletes ALL the others (older
/// resumable duplicates + terminal), leaving EXACTLY ONE live run — the returned one.
#[test]
fn run_start_converges_a_legacy_mess_to_one_live_run() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, temp_dir("n20-legacy").path());
    let spec_id = spec_ready(&db, &project_id);
    let wf = spawn_workflow(&db, &project_id);

    // Seed the mess: a jumble of resumable + terminal runs for the SAME (workflow, spec).
    let seed_run = |status: &str| -> String {
        let id = db
            .with_conn(|c| db::create_workflow_run(c, &wf, Some(&spec_id), None, "{}"))
            .expect("seed run")
            .id;
        if status != db::RUN_STATUS_RUNNING {
            db.with_conn(|c| db::update_workflow_run(c, &id, None, "{}", status, None))
                .unwrap();
        }
        id
    };
    seed_run(db::RUN_STATUS_RUNNING);
    seed_run(db::RUN_STATUS_DONE);
    seed_run(db::RUN_STATUS_BLOCKED);
    seed_run(db::RUN_STATUS_FAILED);
    seed_run(db::RUN_STATUS_RUNNING);
    assert!(runs_for(&db, &wf, &spec_id).len() >= 5, "the legacy mess is seeded");

    // The expected survivor: the most-recent resumable run (same finder + rule run_start applies).
    let expected = runs_for(&db, &wf, &spec_id)
        .into_iter()
        .find(|r| !db::run_status_is_terminal(&r.status))
        .expect("a resumable run in the mess")
        .id;

    let returned = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(returned, expected, "run_start returns the most-recent resumable run");
    let after = runs_for(&db, &wf, &spec_id);
    assert_eq!(after.len(), 1, "converged to exactly one live run");
    assert_eq!(after[0].id, returned, "and it is the returned one");
    assert!(
        !db::run_status_is_terminal(&after[0].status),
        "the survivor is a live (resumable) run"
    );
}

/// N2 — a spawn node with `inline: true` FORCES the inline mode even under a native-Task provider
/// (claude), so an interactive effector (the spec grill) runs in-session, not as a blind Task.
#[test]
fn a_node_can_force_inline_spawn_regardless_of_provider() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "spec", "inline": true }),
        );
    });
    let rt = EngineRuntime::new();
    let out = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id, "provider": "claude" }))
        .expect("nyx_next")
        .result;
    assert_eq!(out["action"]["type"], "spawn_subagent");
    assert_eq!(
        out["action"]["mode"], "inline",
        "the node's inline flag overrode claude's native Task"
    );
    assert_eq!(
        out["action"]["mechanism"], "graph-inline",
        "a graph-forced inline is honestly labelled, not the provider fallback (N10c)"
    );
}

// ---------------------------------------------------------------------------
// B2 — the `query` node reads a verified nyx fact inline and branches on it
// ---------------------------------------------------------------------------

/// A `query` node reads the bound Spec's REAL status onto the blackboard — a nyx-own inline effect,
/// resolved in `drive` and never surfaced to the dispatcher — and a following `selector` branches on
/// that VERIFIED fact rather than an LLM's claim (v2.8 review B2). The Spec is `in_progress`, so the
/// matching branch's `note` ("live") — and only that one — is what the first `nyx_next` hands back
/// with the action, and the queried status is persisted on the blackboard.
#[test]
fn a_query_node_reads_spec_status_inline_and_the_selector_branches_on_it() {
    let _g = db_guard();
    let sandbox = temp_dir("query");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_QUERY,
            json!({ "query": "spec_status", "write": "status" }),
        );
        let sel = add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_SELECTOR,
            json!({ "branches": [
                { "cond": { "var": "status", "eq": "in_progress" }, "child": 0 },
                { "cond": { "always": true }, "child": 1 },
            ] }),
        );
        add_node(db, wf, Some(&sel), 0, db::NODE_TYPE_NOTE, json!({ "text": "live" }));
        add_node(db, wf, Some(&sel), 1, db::NODE_TYPE_NOTE, json!({ "text": "other" }));
    });

    let rt = EngineRuntime::new();
    let out = tick(&db, &rt, &run_id, None);

    // The selector took the `in_progress` branch (the queried fact drove the choice), so the note
    // riding the tick is "live" and NOT "other" — and the verified status is on the persisted board.
    assert_eq!(out["action"]["type"], json!("done"));
    assert_eq!(out["action"]["notes"], json!(["live"]));
    assert_eq!(out["blackboard_delta"]["status"], json!("in_progress"));
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}

// ---------------------------------------------------------------------------
// B1 — a progressed run resumes across a session boundary from its traversal
// ---------------------------------------------------------------------------

/// B1 — a run PROGRESSED in one session resumes in a FRESH session (a new `EngineRuntime`, empty
/// registry) from its PERSISTED traversal — NOT by replaying the graph from the root, which would
/// re-execute its effects. The proof: after the spawn ran (`spawns == 1`) and the run parked on the
/// ask_human, a brand-new runtime picks it up, folds the human decision, and walks past the final
/// `note` to `done` — with `spawns` STILL 1 (the sub-agent was not re-spawned).
#[test]
fn a_progressed_run_resumes_in_a_fresh_session_without_re_executing_effects() {
    let _g = db_guard();
    let sandbox = temp_dir("resume");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
        add_node(db, wf, Some(&root), 1, db::NODE_TYPE_ASK_HUMAN, json!({ "prompt": "ok?" }));
        add_node(db, wf, Some(&root), 2, db::NODE_TYPE_NOTE, json!({ "text": "end" }));
    });

    // Session A: tick to the spawn, report a diff, tick to the ask_human. Then the "session" ends.
    let rt_a = EngineRuntime::new();
    let spawn = tick(&db, &rt_a, &run_id, None);
    assert_eq!(spawn["action"]["type"], "spawn_subagent");
    let ask = tick(&db, &rt_a, &run_id, Some(json!({ "diff": "--- a\n+++ b\n" })));
    assert_eq!(ask["action"]["type"], "ask_human");
    assert_eq!(ask["blackboard_delta"]["spawns"], json!(1), "the spawn folded this tick (N22 delta)");

    // Session B: a FRESH runtime (the in-memory registry is gone). It must RESTORE the persisted
    // traversal and continue — NOT replay the spawn from the root.
    let rt_b = EngineRuntime::new();
    let done = tick(&db, &rt_b, &run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(
        done["action"]["type"], "done",
        "the fresh session resumed at the ask_human and advanced — it did NOT replay the spawn"
    );
    assert_eq!(done["action"]["notes"], json!(["end"]));
    // `spawns` did not change this tick, so it is NOT in the delta (N22) — read the PERSISTED board.
    let persisted: Value = serde_json::from_str(&run_row(&db, &run_id).blackboard).unwrap();
    assert_eq!(
        persisted["spawns"],
        json!(1),
        "the sub-agent was spawned exactly once across the two sessions (no re-execution)"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}

/// B1 — a PROGRESSED run with NO persisted traversal (a pre-B1 run, or one whose stack was never
/// written) is REFUSED cross-session rather than replayed from the root — replaying would re-execute
/// its effects. Simulated by a run created ALREADY parked on a node it was never ticked through.
#[test]
fn a_progressed_run_without_persisted_traversal_is_refused() {
    let _g = db_guard();
    let sandbox = temp_dir("noresume");
    let db = Db::in_memory();
    let (project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());

    let wf_id = db.with_conn(|c| {
        db::create_workflow(c, None, "wf", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("workflow")
            .id
    });
    let _ = project_id; // the run is unbound; only the workflow tree matters for this guard.
    let root = add_node(&db, &wf_id, None, 0, db::NODE_TYPE_NOTE, json!({ "text": "x" }));
    // A run created ALREADY parked on `root` (cursor set) but NEVER ticked → its traversal is NULL.
    let run_id = db.with_conn(|c| {
        db::create_workflow_run(c, &wf_id, None, Some(&root), "{}")
            .expect("run")
            .id
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("run_not_resumable"),
        "a cursor with no persisted traversal cannot be cold-resumed"
    );
}

/// N11 — a run whose cursor was SET-NULL by a built-in REPLACEMENT (the node wipe) but whose stale
/// traversal remains is REFUSED, not silently replayed from the root. The tightened `build_engine`
/// routes a `(cursor NULL, traversal Some)` run to restore, where the stale node ids fail — so a run
/// mid-flight on an upgraded built-in dies cleanly instead of re-executing its effects.
#[test]
fn a_null_cursor_with_a_stale_traversal_is_refused_not_replayed() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        add_node(db, wf, None, 0, db::NODE_TYPE_NOTE, json!({ "text": "x" }));
    });
    // Force the exact state a node wipe leaves behind: a NULL cursor, but a traversal pointing at a
    // node id the (rebuilt) tree no longer has.
    db.with_conn(|c| {
        db::update_workflow_run(
            c,
            &run_id,
            None,
            "{}",
            db::RUN_STATUS_RUNNING,
            Some(r#"{"stack":[],"awaiting":"ghost","replay":false}"#),
        )
    })
    .unwrap();

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("run_traversal_stale"),
        "a NULL cursor with a stale traversal is refused, never replayed from the root"
    );
}

// ---------------------------------------------------------------------------
// N13(b) — the `fork_spec` node forks the bound Spec, gated on a preceding ask_human
// ---------------------------------------------------------------------------

/// N13(b) — a `fork_spec` node FORKS the bound Spec into a new editable draft revision, but ONLY when
/// the preceding ask_human approved (a nyx-own inline effect, never surfaced). An affirmative answer
/// bumps the revision and reopens the Spec to `draft`; a decline is a NO-OP — the ready Spec is
/// untouched. This makes "revise a ready Spec" an EXPLICIT graph step, not an implicit prompt hint.
///
/// DRIVEN FROM A REAL SESSION, with a real human turn behind each answer. It used to be driven from
/// `no_session()`, and that passed — which was finding `01M0C3BZPWV832D802C0GZFTMC`: the answer that
/// forks a Spec was one the session could give itself. See
/// [`a_fork_spec_and_a_create_workspace_now_demand_a_human_turn_like_a_transition_does`] for the
/// refusal that now stands in that path; this test is its other half — the guard must not become a
/// WALL, so the effect still fires when a human really answered.
#[test]
fn a_fork_spec_node_forks_the_bound_spec_only_when_the_human_approves() {
    let _g = db_guard();
    let sandbox = temp_dir("fork");
    let db = Db::in_memory();
    let (project_id, _wsid, _ws) = seed_project(&db, sandbox.path());

    let build = |db: &Db, wf: &str| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "revise?", "output_key": "revise_ok" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_FORK_SPEC,
            json!({ "gate_on": "revise_ok" }),
        );
    };

    // (approved) the human confirms — ON THEIR OWN TURN → the Spec is forked + reopened to draft.
    let spec_yes = spec_ready(&db, &project_id);
    let (_wf, run_yes) = workflow_run(&db, Some(&spec_yes), build);
    let before = spec_rev(&db, &spec_yes);
    let session = Session::new("term-fork-yes");
    let rt = EngineRuntime::new();
    session.human_says(&db, "let us look at this ready spec again");
    let asked = session.tick(&db, &rt, &run_yes, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(
        asked["action"]["human_turn_required"], true,
        "forking a Spec is a commitment, so the asking says the answer must be a human's"
    );
    session.human_says(&db, "yes, reopen it");
    let done = session.tick(&db, &rt, &run_yes, Some(json!({ "decision": "yes" })));
    assert_eq!(done["action"]["type"], "done");
    assert_eq!(
        spec_status(&db, &spec_yes),
        db::SPEC_STATUS_DRAFT,
        "an approved fork reopens the Spec to an editable draft"
    );
    assert!(
        spec_rev(&db, &spec_yes) > before,
        "an approved fork bumps the active revision"
    );

    // (declined) the human says no → the fork is a NO-OP; the ready Spec is untouched.
    let spec_no = spec_ready(&db, &project_id);
    let (_wf2, run_no) = workflow_run(&db, Some(&spec_no), build);
    let before_no = spec_rev(&db, &spec_no);
    let session_no = Session::new("term-fork-no");
    let rt2 = EngineRuntime::new();
    session_no.human_says(&db, "what does nyx want");
    session_no.tick(&db, &rt2, &run_no, None);
    session_no.human_says(&db, "no, leave it alone");
    let done_no = session_no.tick(&db, &rt2, &run_no, Some(json!({ "decision": "no" })));
    assert_eq!(done_no["action"]["type"], "done");
    assert_eq!(
        spec_status(&db, &spec_no),
        db::SPEC_STATUS_READY,
        "a declined fork leaves the ready Spec untouched"
    );
    assert_eq!(
        spec_rev(&db, &spec_no),
        before_no,
        "a declined fork does not bump the revision"
    );
}

/// FINDING `01M0C3BZPWV832D802C0GZFTMC` — CONSENT WAS BYPASSABLE FOR TWO OF THE THREE SENSITIVE NODE
/// TYPES, AND BOTH BYPASSES ARE REPRODUCED HERE, from the very shape the audit ran.
///
/// THE SEAM, not the rule. `mcp_authoring`'s rule (4) counted THREE sensitive nodes (an engaging
/// `transition`, a `fork_spec`, a `create_workspace`) and certified each of the graphs below
/// HUMAN-GATED — correctly, on its own terms: the question necessarily runs, its answer is what the
/// `gate_on` reads, and nothing else writes that key. But its doc-comment says out loud that it does
/// NOT check that the answer is a HUMAN's, and delegates that to `mcp_engine`'s
/// `human_turn_required` / `accept_consent_answer`. And the runtime classification behind those
/// (`Tree::ask_human_commitment`) recognised only the engaging `transition` and the spending `loop`.
///
/// So for these two graphs `awaiting_commitment` returned `None`, no `PendingConsent` was opened, the
/// action carried no `human_turn_required` — and the SESSION DRIVING THE RUN answered "yes" for the
/// human. Reproduced in execution: a folder really created on disk, a ready Spec really forked. Each
/// layer was right alone; their INTERSECTION covered one node type out of three.
///
/// THE FIX IS ONE SET READ TWICE ([`crate::engine::SensitiveEffect`]), so what this test pins is that
/// the two graphs now behave exactly like the `transition{ready}` one that was already safe: the
/// question is refused AT THE ASKING with `consent_caller_unattributable` (the reference refusal, see
/// [`a_tick_from_an_unattributable_caller_is_never_asked_and_leaves_no_receipt`]), and NOTHING
/// happens.
#[test]
fn a_fork_spec_and_a_create_workspace_now_demand_a_human_turn_like_a_transition_does() {
    let _g = db_guard();
    let sandbox = temp_dir("consent-seam");
    let db = Db::in_memory();
    let (project_id, _wsid, root_path) = seed_project(&db, sandbox.path());
    let roots = temp_dir("consent-seam-roots");

    // (A) `sequence[ ask_human{ws_ok}, create_workspace{root_key, gate_on: ws_ok} ]` — the audit's
    //     graph A. The board carries the worktrees root the `go` branch of /nspec resolves first.
    let spec_ws = spec_ready(&db, &project_id);
    let wf_ws = db.with_conn(|c| {
        db::create_workflow(c, None, "seam-ws", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("workflow")
            .id
    });
    let ws_root_node = add_node(&db, &wf_ws, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf_ws,
        Some(&ws_root_node),
        0,
        db::NODE_TYPE_ASK_HUMAN,
        json!({ "prompt": "open a dev workspace?", "output_key": "ws_ok", "options": ["yes", "no"] }),
    );
    add_node(
        &db,
        &wf_ws,
        Some(&ws_root_node),
        1,
        db::NODE_TYPE_CREATE_WORKSPACE,
        json!({ "root_key": "worktrees_root", "gate_on": "ws_ok" }),
    );
    // The AUTHORING rule accepts this graph — it always did, and it still must: the hole was never
    // that rule (4) let a bad shape through, it was that the runtime asked nothing of a good one.
    let ws_nodes = db
        .with_conn(|c| db::list_nodes(c, &wf_ws))
        .expect("the workspace graph's nodes");
    assert!(
        crate::mcp_authoring::validate_tree(&ws_nodes, Some(&ws_root_node)).is_ok(),
        "rule (4) certifies this graph HUMAN-GATED — which is exactly why the runtime must ask"
    );
    let board =
        json!({ crate::engine::DEFAULT_WORKTREES_ROOT_KEY: roots.path().to_string_lossy() })
            .to_string();
    let run_ws = db
        .with_conn(|c| db::create_workflow_run(c, &wf_ws, Some(&spec_ws), None, &board))
        .expect("run")
        .id;

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_ws })).unwrap_err();
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("consent_caller_unattributable"),
        "a folder on the user's disk is a commitment: the question is refused at the ASKING, \
         exactly as it already was for an engaging transition — {}",
        err.message
    );
    // The bypass RELAYED the answer anyway — the exact call the audit made, `{"decision": "yes"}`
    // from a session nobody witnessed. That road is shut too, on the answer this time.
    let err = super::nyx_next(
        &db,
        &rt,
        no_session(),
        &json!({ "run_id": run_ws, "last_result": { "decision": "yes" } }),
    )
    .unwrap_err();
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("human_consent_without_human_turn"),
        "{}",
        err.message
    );
    // AND NOTHING HAPPENED — the three effects the audit observed, each denied by name.
    let after: Vec<db::Workspace> = db
        .with_conn(|c| db::list_workspaces(c, &project_id))
        .expect("workspaces");
    assert_eq!(
        after.len(),
        1,
        "no workspace ROW was written: only the project's own root workspace stands"
    );
    assert_eq!(
        after[0].root_path,
        crate::pathnorm::normalize(&root_path),
        "…and it is the project root, not a workspace the run opened"
    );
    assert_eq!(
        std::fs::read_dir(roots.path())
            .expect("the worktrees root is readable")
            .count(),
        0,
        "no FOLDER was created on the disk under the worktrees root"
    );
    assert_eq!(
        db.with_conn(|c| db::get_spec(c, &spec_ws))
            .unwrap()
            .unwrap()
            .workspace_id,
        None,
        "…and `specs.workspace_id` was not rewritten"
    );

    // (B) `sequence[ ask_human{fork_ok}, fork_spec{gate_on: fork_ok} ]` — the audit's graph B.
    let spec_fork = spec_ready(&db, &project_id);
    let (_wf_fork, run_fork) = workflow_run(&db, Some(&spec_fork), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "fork it?", "output_key": "fork_ok", "options": ["yes", "no"] }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_FORK_SPEC,
            json!({ "gate_on": "fork_ok" }),
        );
    });
    let revision_before = spec_rev(&db, &spec_fork);
    let rt2 = EngineRuntime::new();
    // Refused at the ASKING (no receipt is posed to nobody), then refused again on the ANSWER the
    // audit relayed anyway — the same two refusals an engaging transition already gave.
    for (last, reason) in [
        (None, "consent_caller_unattributable"),
        (
            Some(json!({ "decision": "yes" })),
            "human_consent_without_human_turn",
        ),
    ] {
        let mut args = json!({ "run_id": run_fork });
        if let Some(last) = last {
            args.as_object_mut()
                .unwrap()
                .insert("last_result".into(), last);
        }
        let err = super::nyx_next(&db, &rt2, no_session(), &args).unwrap_err();
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
            Some(reason),
            "reopening a ready Spec as a draft is a commitment too: {}",
            err.message
        );
    }
    assert_eq!(
        pending_consent(&db, &run_fork),
        None,
        "and no receipt was written: a consent question posed to nobody must not exist"
    );
    assert_eq!(
        spec_rev(&db, &spec_fork),
        revision_before,
        "the Spec's revision was NOT bumped"
    );
    assert_eq!(
        spec_status(&db, &spec_fork),
        db::SPEC_STATUS_READY,
        "…and the ready Spec did not fall back to draft"
    );

    // (C) THE OTHER READING — a `create_workspace` with NO `gate_on` at all, reached only through a
    //     `selector` branch that compares the answer to `go`. Rule (4)(ii) accepts that shape (the
    //     BRANCH is the consent, and it is the shape /nspec's own `go` arm is built on), so the
    //     runtime has to demand the human turn on it too — reading the node's `gate_on` alone would
    //     have left a second, equally authorable route to the same mkdir.
    let spec_branch = spec_ready(&db, &project_id);
    let wf_branch = db.with_conn(|c| {
        db::create_workflow(c, None, "seam-branch", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("workflow")
            .id
    });
    let branch_root = add_node(&db, &wf_branch, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &wf_branch,
        Some(&branch_root),
        0,
        db::NODE_TYPE_ASK_HUMAN,
        json!({ "prompt": "ready, and a workspace?", "output_key": "ready_ok", "options": ["go", "no"] }),
    );
    let route = add_node(
        &db,
        &wf_branch,
        Some(&branch_root),
        1,
        db::NODE_TYPE_SELECTOR,
        json!({ "branches": [
            { "cond": { "var": "ready_ok", "eq": "go" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    );
    add_node(
        &db,
        &wf_branch,
        Some(&route),
        0,
        db::NODE_TYPE_CREATE_WORKSPACE,
        json!({ "root_key": "worktrees_root" }),
    );
    add_node(
        &db,
        &wf_branch,
        Some(&route),
        1,
        db::NODE_TYPE_NOTE,
        json!({ "text": "left as a draft" }),
    );
    let branch_nodes = db
        .with_conn(|c| db::list_nodes(c, &wf_branch))
        .expect("the branch graph's nodes");
    assert!(
        crate::mcp_authoring::validate_tree(&branch_nodes, Some(&branch_root)).is_ok(),
        "the branch-gated shape is authorable — it is the one /nspec ships"
    );
    let run_branch = db
        .with_conn(|c| db::create_workflow_run(c, &wf_branch, Some(&spec_branch), None, &board))
        .expect("run")
        .id;
    let err = super::nyx_next(
        &db,
        &EngineRuntime::new(),
        no_session(),
        &json!({ "run_id": run_branch }),
    )
    .unwrap_err();
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("consent_caller_unattributable"),
        "a consent carried by the BRANCH is a consent: {}",
        err.message
    );

    // AND THE GUARD IS NOT A WALL: the very same graph, put to a session nyx can name, asks the
    // question and takes the answer the moment a real human turn stands behind it.
    let session = Session::new("term-seam-ws");
    session.human_says(&db, "open the workspace for this spec");
    let asked = session.tick(&db, &rt, &run_ws, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(
        asked["action"]["human_turn_required"], true,
        "the asking NAMES the demand instead of leaving the session to discover it"
    );
    assert!(
        asked["action"]["context"]
            .as_str()
            .unwrap_or_default()
            .contains("folder"),
        "…and it names what the answer commits — a FOLDER, not 'the Spec's lifecycle': {}",
        asked["action"]["context"]
    );
    session.human_says(&db, "yes, go ahead");
    let done = session.tick(&db, &rt, &run_ws, Some(json!({ "decision": "yes" })));
    assert_eq!(done["action"]["type"], "done");
    let opened = db
        .with_conn(|c| db::list_workspaces(c, &project_id))
        .expect("workspaces")
        .into_iter()
        .find(|w| w.root_path != crate::pathnorm::normalize(&root_path))
        .expect("the human's yes really opened the dev workspace");
    assert!(
        std::path::Path::new(&opened.root_path).is_dir(),
        "the folder exists on disk — the refusal above was about WHO answered, not about WHAT"
    );
}

// ---------------------------------------------------------------------------
// N14 — run_start anchor resolution is coherent (no silent mismatch / ambiguity)
// ---------------------------------------------------------------------------

/// N14(a) — run_start REFUSES a spec_id whose project disagrees with the cwd / workspace_id anchor,
/// rather than silently picking one (the guard agent_context already applies).
#[test]
fn run_start_refuses_a_spec_from_a_project_that_disagrees_with_the_anchor() {
    let _g = db_guard();
    let s1 = temp_dir("n14a-1");
    let s2 = temp_dir("n14a-2");
    let db = Db::in_memory();
    let (p1, _w1, _) = seed_project(&db, s1.path());
    let (_p2, w2, _) = seed_project(&db, s2.path());
    let spec_p1 = spec_ready(&db, &p1);

    let err = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_p1, "workspace_id": w2 })).unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("disagree") || err.message.contains("belongs"),
        "the refusal names the disagreement: {}",
        err.message
    );
}

/// N14(b) — run_start REFUSES a workspace anchoring SEVERAL Specs (ambiguous by location), listing
/// the candidates, instead of arbitrarily picking the newest.
#[test]
fn run_start_refuses_an_ambiguous_workspace_with_several_specs() {
    let _g = db_guard();
    let sandbox = temp_dir("n14b");
    let db = Db::in_memory();
    let (project, ws, _) = seed_project(&db, sandbox.path());
    let _a = spec_in_progress(&db, &project, &ws); // anchored on ws
    let _b = spec_in_progress(&db, &project, &ws); // a SECOND anchored on the SAME ws → ambiguous

    let err = super::run_start(&db, &EngineRuntime::new(), &json!({ "workspace_id": ws })).unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("ambiguous") && err.message.contains("Candidates"),
        "the refusal lists the candidates: {}",
        err.message
    );
}

/// A workspace accumulates the Specs it has SHIPPED. A TERMINAL Spec (`completed` / `cancelled`) is
/// not work in progress, so it must not compete in a location-based resolution: a workspace whose
/// only ACTIVE Spec is X resolves to X however many finished Specs share it. The candidate set used
/// to be "every non-archived Spec", which made such a workspace read ambiguous over dead work.
#[test]
fn resolving_by_workspace_ignores_terminal_specs() {
    let _g = db_guard();
    let sandbox = temp_dir("terminal-specs");
    let db = Db::in_memory();
    let (project, ws, _) = seed_project(&db, sandbox.path());

    // Two SHIPPED Specs on the workspace: one completed, one cancelled.
    let completed = spec_in_progress(&db, &project, &ws);
    db.with_conn(|c| {
        db::transition_spec(c, &completed, SpecTransition::Review, false).expect("review");
        db::transition_spec(c, &completed, SpecTransition::Done, true).expect("done");
    });
    let cancelled = spec_in_progress(&db, &project, &ws);
    db.with_conn(|c| {
        db::transition_spec(c, &cancelled, SpecTransition::Cancel, true).expect("cancel");
    });
    // …and exactly ONE live Spec.
    let live = spec_in_progress(&db, &project, &ws);

    assert_eq!(
        db.with_conn(|c| db::active_specs_for_workspace(c, &ws))
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect::<Vec<_>>(),
        vec![live.clone()],
        "only the live Spec is a candidate"
    );

    // The location resolves UNAMBIGUOUSLY to the live one.
    let wf = spawn_workflow(&db, &project);
    let out = super::run_start(&db, &EngineRuntime::new(), &json!({ "workspace_id": ws, "workflow_id": wf }))
        .expect("run_start resolves the single active Spec")
        .result;
    assert_eq!(out["spec_id"], live, "the run binds the ACTIVE Spec, not a shipped one");
}

/// …and the ambiguity GUARD survives the narrowing: two genuinely-ACTIVE Specs on one workspace
/// still refuse with "pass an explicit spec_id", even alongside a terminal one.
#[test]
fn several_active_specs_still_refuse_even_beside_a_terminal_one() {
    let _g = db_guard();
    let sandbox = temp_dir("still-ambiguous");
    let db = Db::in_memory();
    let (project, ws, _) = seed_project(&db, sandbox.path());

    let shipped = spec_in_progress(&db, &project, &ws);
    db.with_conn(|c| {
        db::transition_spec(c, &shipped, SpecTransition::Review, false).expect("review");
        db::transition_spec(c, &shipped, SpecTransition::Done, true).expect("done");
    });
    let _a = spec_in_progress(&db, &project, &ws);
    let _b = spec_in_progress(&db, &project, &ws);

    let err = super::run_start(&db, &EngineRuntime::new(), &json!({ "workspace_id": ws })).unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("ambiguous") && err.message.contains("spec_id"),
        "two active Specs are still ambiguous: {}",
        err.message
    );
    assert!(
        !err.message.contains(&shipped),
        "the shipped Spec is not even listed as a candidate: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// THE CALLER CONTRACTS over the ONE 0/1/N resolver: agent_context, run_start,
// and the authoring flow that is allowed to proceed unbound
// ---------------------------------------------------------------------------

/// ONE ANCHOR, ONE SPEC. `agent_context` and `run_start` must never disagree about which Spec a
/// location names — they are the two halves of the same session (the run drives the Spec, the
/// manual describes it), so a divergence would have an agent working from the state of a Spec its
/// run does not touch. Both read the SAME `resolve_spec_id`, and this pins the consequence on BOTH
/// location anchors: an explicit `workspace_id` and a `cwd`.
#[test]
fn agent_context_and_run_start_target_the_same_spec_from_the_same_anchor() {
    let _g = db_guard();
    let sandbox = temp_dir("one-anchor");
    let db = Db::in_memory();
    let (project, ws, ws_path) = seed_project(&db, sandbox.path());
    let spec = spec_in_progress(&db, &project, &ws);
    let wf = spawn_workflow(&db, &project);

    for anchor in [json!({ "workspace_id": ws }), json!({ "cwd": ws_path })] {
        let mut run_args = anchor.clone();
        run_args["workflow_id"] = json!(wf);
        let run = super::run_start(&db, &EngineRuntime::new(), &run_args)
            .expect("the anchor resolves the one active Spec")
            .result;
        assert_eq!(run["spec_id"], spec, "run_start binds THE Spec: {anchor}");

        let mut ctx_args = anchor.clone();
        ctx_args["role"] = json!("coder");
        let ctx = crate::mcp_context::agent_context(&db, &ctx_args)
            .expect("the same anchor renders the same Spec")
            .result;
        assert_eq!(
            ctx["spec_id"], run["spec_id"],
            "agent_context and run_start agree on the Spec for {anchor}"
        );
        // …and on its PROJECT, the other half of the target.
        assert_eq!(ctx["project_id"], json!(project));
    }
}

/// THE AMBIGUITY, per caller. One location, N active Specs — and three DELIBERATELY different
/// answers, none of which is "the newest Spec":
///   * `agent_context` REFUSES, whatever the role, listing the candidates: a manual rendered for an
///     arbitrary Spec is worse than no manual;
///   * a SPEC-SCOPED flow (`ndev`, and every scope that works ON a Spec) REFUSES the same way;
///   * an AUTHORING flow (`nspec`) — which CREATES its own Spec and needs none — PROCEEDS, bound to
///     NO Spec, keeping its `cwd` anchor, and relays the ambiguity as a WARNING.
#[test]
fn an_ambiguous_location_refuses_for_every_spec_consumer_but_only_warns_for_authoring() {
    let _g = db_guard();
    let sandbox = temp_dir("caller-ambiguity");
    let db = Db::in_memory();
    let (project, ws, ws_path) = seed_project(&db, sandbox.path());
    let first = spec_in_progress(&db, &project, &ws);
    let second = spec_in_progress(&db, &project, &ws);

    // agent_context — REFUSED, with the machine handle AND both candidates, for the effector role
    // that works on the Spec and for the dispatcher alike.
    for role in ["coder", "dev"] {
        let err =
            crate::mcp_context::agent_context(&db, &json!({ "role": role, "workspace_id": ws }))
                .expect_err("an ambiguous location renders no manual");
        assert_eq!(err.code, "invalid_argument", "role {role}");
        assert!(
            crate::mcp_context::is_ambiguous_spec(&err),
            "role {role}: the refusal carries reason=spec_ambiguous_for_workspace: {err:?}"
        );
        assert!(
            err.message.contains(&first) && err.message.contains(&second),
            "role {role}: both candidates are listed: {}",
            err.message
        );
    }

    // A SPEC-SCOPED flow — refused identically (the run would drive an arbitrary Spec).
    let ndev = spawn_workflow(&db, &project);
    let err = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "scope": "ndev", "workflow_id": ndev, "cwd": ws_path }),
    )
    .expect_err("a spec-scoped flow cannot start on an ambiguous location");
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains(&first) && err.message.contains(&second),
        "the candidates are listed: {}",
        err.message
    );

    // The AUTHORING flow — proceeds UNBOUND, still anchored, and SAYS why.
    let nspec = db.with_conn(|c| {
        db::create_workflow(c, Some(&project), "nspec", db::WORKFLOW_SCOPE_NSPEC, None)
            .expect("wf")
            .id
    });
    add_node(
        &db,
        &nspec,
        None,
        0,
        db::NODE_TYPE_NOTE,
        json!({ "text": "author" }),
    );
    let opened = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "scope": "nspec", "cwd": ws_path }),
    )
    .expect("an authoring flow starts on an ambiguous location")
    .result;
    assert!(
        opened["spec_id"].is_null(),
        "NO Spec was chosen — not the newest, not any: {opened}"
    );
    assert_eq!(
        run_row(&db, opened["run_id"].as_str().unwrap())
            .workspace_id
            .as_deref(),
        Some(ws.as_str()),
        "the cwd still anchors the run on its workspace"
    );
    assert!(
        opened["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|w| w["code"] == "spec_ambiguous_started_unbound"),
        "the ambiguity is relayed as a warning, never swallowed: {opened}"
    );
}

/// THE AUTHOR IS NOT EXEMPT FROM THE AMBIGUITY — the one 0/1/N × caller cell the neighbours above
/// and below leave uncovered, and the riskiest one, because the author IS granted an exception on
/// the two adjacent cells: at ZERO candidates `agent_context(role="spec")` renders the
/// project-scoped authoring manual instead of refusing (see
/// [`zero_candidates_refuses_the_spec_scoped_callers_and_leaves_authoring_unbound`]), and on THIS
/// very ambiguous location `run_start(scope="nspec")` is deliberately let through with a warning.
/// Neither exception extends to N through `agent_context`: `resolve_spec_id` settles the location
/// BEFORE `agent_context` ever looks at the role (`mcp_context.rs`, the `resolve_spec_id(db, args)?`
/// at the head of the role match), so the author gets the SAME structured refusal as `coder` and
/// `dev` — same code, same `reason`, same candidate list.
///
/// This pins the behaviour AS SHIPPED. Turning `role="spec"` into a graceful project-scoped render
/// on N would be a product-contract change, not a bug fix; if that is ever wanted, this test is the
/// one that must be rewritten deliberately rather than the assertion quietly relaxed.
#[test]
fn an_ambiguous_location_refuses_the_spec_author_exactly_like_every_other_role() {
    let _g = db_guard();
    let sandbox = temp_dir("author-ambiguity");
    let db = Db::in_memory();
    let (project, ws, ws_path) = seed_project(&db, sandbox.path());
    let first = spec_in_progress(&db, &project, &ws);
    let second = spec_in_progress(&db, &project, &ws);
    assert_ne!(
        first, second,
        "two DISTINCT active Specs share the workspace"
    );

    // BOTH location anchors — an explicit workspace_id and a cwd — refuse for the author.
    for (kind, anchor) in [
        ("workspace_id", json!({ "workspace_id": ws })),
        ("cwd", json!({ "cwd": ws_path })),
    ] {
        let mut args = anchor.clone();
        args.as_object_mut()
            .unwrap()
            .insert("role".into(), json!("spec"));
        let err = crate::mcp_context::agent_context(&db, &args)
            .expect_err("the author is NOT exempt: N candidates render no manual");
        assert_eq!(err.code, "invalid_argument", "{kind}");
        assert!(
            crate::mcp_context::is_ambiguous_spec(&err),
            "{kind}: role=spec carries the SAME machine handle \
             reason=spec_ambiguous_for_workspace: {err:?}"
        );
        assert!(
            err.message.contains(&first) && err.message.contains(&second),
            "{kind}: BOTH candidates are listed for the author too: {}",
            err.message
        );

        // …and it is the SAME refusal as the effector's, byte for byte: the role plays NO part in
        // it. A divergence here would mean an ambiguity branch was grafted per role.
        let mut coder_args = anchor.clone();
        coder_args
            .as_object_mut()
            .unwrap()
            .insert("role".into(), json!("coder"));
        let coder_err = crate::mcp_context::agent_context(&db, &coder_args)
            .expect_err("the coder refuses on the same location");
        assert_eq!(err.code, coder_err.code, "{kind}: same code as role=coder");
        assert_eq!(
            err.message, coder_err.message,
            "{kind}: same message — the ambiguity is settled BEFORE the role is read"
        );
        assert_eq!(err.data, coder_err.data, "{kind}: same structured data");
    }

    // THE CONTRAST that makes the exception real elsewhere: the authoring RUN, on the SAME
    // location, still goes through unbound with a warning. Refusing the manual is not the flow
    // being closed — it is the manual having no Spec to honestly render.
    let nspec = db.with_conn(|c| {
        db::create_workflow(c, Some(&project), "nspec", db::WORKFLOW_SCOPE_NSPEC, None)
            .expect("wf")
            .id
    });
    add_node(
        &db,
        &nspec,
        None,
        0,
        db::NODE_TYPE_NOTE,
        json!({ "text": "author" }),
    );
    let opened = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "scope": "nspec", "cwd": ws_path }),
    )
    .expect("the authoring RUN is still let through on the same ambiguous location")
    .result;
    assert!(
        opened["spec_id"].is_null(),
        "…bound to NEITHER candidate: {opened}"
    );
}

/// ZERO candidates — the other half of the contract, and the one that used to open ORPHAN runs. A
/// location anchoring NO active Spec is not an error in itself; what happens next depends on what
/// the caller needs:
///   * a flow that works ON a Spec REFUSES (a run whose Spec queries apply to nothing is inert);
///   * an AUTHORING flow starts, legitimately unbound — it is about to create the Spec;
///   * `agent_context` refuses for a role that describes a Spec, and renders the project-scoped
///     authoring manual for the role that WRITES one.
#[test]
fn zero_candidates_refuses_the_spec_scoped_callers_and_leaves_authoring_unbound() {
    let _g = db_guard();
    let sandbox = temp_dir("zero-candidates");
    let db = Db::in_memory();
    let (project, ws, ws_path) = seed_project(&db, sandbox.path());

    // A SPEC-SCOPED flow on a workspace with no Spec: refused, and the refusal says how to fix it.
    let ndev = spawn_workflow(&db, &project);
    let err = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "workflow_id": ndev, "cwd": ws_path }),
    )
    .expect_err("a spec-scoped flow refuses to start unbound");
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("spec_id"),
        "the refusal names the way through: {}",
        err.message
    );

    // The AUTHORING flow starts unbound on the very same location.
    let nspec = db.with_conn(|c| {
        db::create_workflow(c, Some(&project), "nspec", db::WORKFLOW_SCOPE_NSPEC, None)
            .expect("wf")
            .id
    });
    add_node(
        &db,
        &nspec,
        None,
        0,
        db::NODE_TYPE_NOTE,
        json!({ "text": "author" }),
    );
    let opened = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "scope": "nspec", "cwd": ws_path }),
    )
    .expect("authoring starts with no Spec")
    .result;
    assert!(opened["spec_id"].is_null(), "and stays unbound: {opened}");

    // agent_context: refused for the role that DESCRIBES a Spec…
    let err =
        crate::mcp_context::agent_context(&db, &json!({ "role": "coder", "workspace_id": ws }))
            .expect_err("there is no Spec to render a coder manual from");
    assert_eq!(err.code, "invalid_id");
    // …and a project-scoped authoring manual for the role that WRITES one.
    let authoring =
        crate::mcp_context::agent_context(&db, &json!({ "role": "spec", "workspace_id": ws }))
            .expect("the author is never refused for having no Spec yet")
            .result;
    assert!(authoring["spec_id"].is_null(), "no Spec was invented");
    assert_eq!(authoring["project_id"], json!(project));
}

/// A CONTRADICTORY `project_id` is REFUSED by both callers, never silently dropped. A Spec's project
/// is a fact OF the Spec: an argument naming another project decided nothing, so honouring it is
/// impossible and ignoring it leaves the caller believing it scoped the call. `run_start` confronts
/// it while resolving its target; `agent_context` confronts it beside the resolved Spec.
#[test]
fn a_project_id_that_contradicts_the_resolved_spec_is_refused_by_both_callers() {
    let _g = db_guard();
    let s1 = temp_dir("contradiction-1");
    let s2 = temp_dir("contradiction-2");
    let db = Db::in_memory();
    let (p1, w1, _) = seed_project(&db, s1.path());
    let (p2, _w2, _) = seed_project(&db, s2.path());
    let spec_of_p1 = spec_in_progress(&db, &p1, &w1);

    let err = super::run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "spec_id": spec_of_p1, "project_id": p2 }),
    )
    .expect_err("run_start confronts the disagreement");
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains(&p1) && err.message.contains(&p2),
        "the refusal names both projects: {}",
        err.message
    );

    let err = crate::mcp_context::agent_context(
        &db,
        &json!({ "role": "coder", "spec_id": spec_of_p1, "project_id": p2 }),
    )
    .expect_err("agent_context confronts the same disagreement");
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains(&p1) && err.message.contains(&p2),
        "the refusal names both projects: {}",
        err.message
    );

    // The AGREEING project_id is a no-op, not a refusal — only the contradiction is refused.
    let ok = crate::mcp_context::agent_context(
        &db,
        &json!({ "role": "coder", "spec_id": spec_of_p1, "project_id": p1 }),
    )
    .expect("naming the Spec's own project changes nothing")
    .result;
    assert_eq!(ok["spec_id"], spec_of_p1);
}

// ---------------------------------------------------------------------------
// N19 — ask_human answers are validated against a closed `options` set
// ---------------------------------------------------------------------------

/// N19 — an `ask_human` with `options` returns them WITH the action (the dispatcher offers them), and
/// an answer NOT among them is REFUSED (the SAME question is re-asked) instead of silently folding an
/// off-vocabulary value a selector would then miss. A valid answer advances the graph.
#[test]
fn an_ask_human_answer_outside_its_options_is_refused_and_re_asked() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "go?", "output_key": "d", "options": ["go", "stop"] }),
        );
        add_node(db, wf, Some(&root), 1, db::NODE_TYPE_NOTE, json!({ "text": "done" }));
    });

    let rt = EngineRuntime::new();
    // The action carries the options so the dispatcher proposes them (it does not invent) AND the
    // FIELD to answer under (F4) — the whole contract, stated, so nothing has to be guessed.
    let ask = tick(&db, &rt, &run_id, None);
    assert_eq!(ask["action"]["type"], "ask_human");
    assert_eq!(ask["action"]["options"], json!(["go", "stop"]));
    assert_eq!(ask["action"]["answer_key"], "decision");

    // An answer NOT in the options → the SAME ask_human is re-yielded (re-asked), never folded. The
    // re-ask is no longer mute: it carries the field + the options the answer missed (F4).
    let again = tick(&db, &rt, &run_id, Some(json!({ "decision": "maybe" })));
    assert_eq!(
        again["action"]["type"], "ask_human",
        "a WELL-FORMED but off-vocabulary answer is refused and re-asked (N19)"
    );
    assert_eq!(again["action"]["answer_key"], "decision");
    assert_eq!(again["action"]["options"], json!(["go", "stop"]));

    // A VALID answer → the graph advances past the ask_human (through the note, to the end).
    let after = tick(&db, &rt, &run_id, Some(json!({ "decision": "go" })));
    assert_eq!(after["action"]["type"], "done");
    assert_eq!(after["action"]["notes"], json!(["done"]));
}

// ---------------------------------------------------------------------------
// H5 — a parked ask_human ticked with NO answer RE-PRESENTS, it never advances
// ---------------------------------------------------------------------------

/// H5 — the user's own scenario, the prototyper's free-form `election` park: `nyx_next` called on it
/// with NO `last_result` answered `"blackboard_delta": {"election": null}` and walked on. The
/// question a human was asked was lost in silence and the variable took `null` — and that tick is
/// exactly what a dispatcher RESUMING after a crash produces, which this engine states as a
/// principle. The SAME question must come back, unchanged, with its contract.
#[test]
fn an_ask_human_ticked_with_no_answer_hands_back_the_same_question() {
    let _g = db_guard();
    let db = Db::in_memory();
    let prompt = "Elect the RETAINED variant per page (elect it in the nyx UI, or state it here).";
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": prompt, "output_key": "election" }),
        );
        add_node(db, wf, Some(&root), 1, db::NODE_TYPE_NOTE, json!({ "text": "next round" }));
    });

    let rt = EngineRuntime::new();
    let asked = tick(&db, &rt, &run_id, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(asked["action"]["prompt"], json!(prompt));
    let parked_on = asked["current_node_id"].as_str().expect("a cursor").to_string();

    // THE TICK OF THE BUG: no `last_result` at all, on a run parked at the ask.
    let again = tick(&db, &rt, &run_id, None);
    assert_eq!(
        again["action"]["type"], "ask_human",
        "an ABSENT answer re-presents the question — it never advances: {again}"
    );
    assert_eq!(again["action"]["prompt"], json!(prompt), "the SAME question, verbatim");
    // F4's re-present path, so the re-ask is not mute: it states the field to answer under.
    assert_eq!(again["action"]["answer_key"], "decision");
    assert_eq!(
        again["current_node_id"].as_str(),
        Some(parked_on.as_str()),
        "the run stays parked on the very same node"
    );
    assert!(
        again["blackboard_delta"].get("election").is_none(),
        "nothing was written for the unanswered question: {again}"
    );
    let row = run_row(&db, &run_id);
    assert_eq!(row.status, db::RUN_STATUS_RUNNING);
    assert!(
        Blackboard::from_json_str(&row.blackboard).get("election").is_none(),
        "…and no `null` landed on the PERSISTED board either: {}",
        row.blackboard
    );

    // The human finally answers: the run folds it and advances, as it always did.
    let answered = tick(&db, &rt, &run_id, Some(json!({ "decision": "variant B" })));
    assert_eq!(answered["action"]["type"], "done");
    assert_eq!(answered["blackboard_delta"]["election"], json!("variant B"));
}

/// …and ONLY a parked `ask_human`. A leaf that legitimately reports nothing — a `produce`/`wait` the
/// dispatcher acked, a `note` the walk goes past — still advances on a result-less tick, which is
/// most of the dispatcher loop.
#[test]
fn a_result_less_tick_still_advances_a_leaf_that_reports_nothing() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(db, wf, Some(&root), 0, db::NODE_TYPE_WAIT, json!({}));
        add_node(db, wf, Some(&root), 1, db::NODE_TYPE_PRODUCE, json!({}));
        add_node(db, wf, Some(&root), 2, db::NODE_TYPE_NOTE, json!({ "text": "walked past" }));
    });

    let rt = EngineRuntime::new();
    assert_eq!(tick(&db, &rt, &run_id, None)["action"]["type"], "wait");
    assert_eq!(tick(&db, &rt, &run_id, None)["action"]["type"], "produce");
    let end = tick(&db, &rt, &run_id, None);
    assert_eq!(end["action"]["type"], "done", "the acked leaves advanced on nothing: {end}");
    assert_eq!(end["action"]["notes"], json!(["walked past"]));
}

// ---------------------------------------------------------------------------
// F4 — an ask_human answer is READ leniently and REFUSED out loud
// ---------------------------------------------------------------------------

/// A workflow whose root is `sequence[ ask_human(options go|stop) , note ]`, and one run of it.
fn ask_human_run(db: &Db) -> String {
    let (_wf, run_id) = workflow_run(db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "go?", "output_key": "d", "options": ["go", "stop"] }),
        );
        add_node(db, wf, Some(&root), 1, db::NODE_TYPE_NOTE, json!({ "text": "done" }));
    });
    run_id
}

/// F4 — a LLM dispatcher naturally answers `{"answer": …}` / `{"choice": …}` / `{"value": …}` /
/// `{"selected": …}` (or the bare string). Each is READ as the answer, exactly like the declared
/// `decision` field: the graph advances and the value lands on the blackboard. Before this, only
/// `decision` worked and every synonym was silently discarded — a live dispatcher burned three blind
/// attempts before stumbling on the key.
#[test]
fn an_ask_human_reads_the_natural_answer_synonyms() {
    let _g = db_guard();
    let db = Db::in_memory();
    let rt = EngineRuntime::new();

    for answer in [
        json!({ "decision": "go" }),
        json!({ "answer": "go" }),
        json!({ "choice": "go" }),
        json!({ "value": "go" }),
        json!({ "selected": "go" }),
        json!("go"),
    ] {
        let run_id = ask_human_run(&db);
        assert_eq!(tick(&db, &rt, &run_id, None)["action"]["type"], "ask_human");
        let after = tick(&db, &rt, &run_id, Some(answer.clone()));
        assert_eq!(
            after["action"]["type"], "done",
            "{answer} is a usable answer — the graph advances"
        );
        assert_eq!(
            after["blackboard_delta"]["d"], "go",
            "{answer} lands on the blackboard as the answer itself"
        );
    }
}

/// F4 — an answer whose SHAPE is unusable (an unknown key, or an object under a known one) is
/// REFUSED OUT LOUD: an `invalid_argument` naming the expected FIELD and the allowed VALUES. It used
/// to be discarded in silence and the identical question re-presented, unbounded and unexplained, so
/// nothing in the loop could tell a wrong key from a rejected value. The run stays parked on the ask
/// (nothing advances, nothing fails), so the corrected answer still lands.
#[test]
fn an_ask_human_answer_of_an_unusable_shape_is_refused_out_loud() {
    let _g = db_guard();
    let db = Db::in_memory();
    let rt = EngineRuntime::new();
    let run_id = ask_human_run(&db);
    assert_eq!(tick(&db, &rt, &run_id, None)["action"]["type"], "ask_human");

    for garbage in [
        json!({ "verdict": "go" }),
        json!({ "decision": { "value": "go" } }),
        json!([ "go" ]),
    ] {
        let err = super::nyx_next(
            &db,
            &rt, no_session(),
            &json!({ "run_id": run_id, "last_result": garbage.clone() }),
        )
        .expect_err("an unusable answer shape is refused, not silently re-asked");
        assert_eq!(err.code, RpcCode::InvalidArgument, "{garbage}");
        assert!(
            err.message.contains("decision"),
            "the refusal names the expected FIELD: {}",
            err.message
        );
        assert!(
            err.message.contains("go, stop"),
            "the refusal names the allowed VALUES: {}",
            err.message
        );
        assert_eq!(
            run_row(&db, &run_id).status,
            db::RUN_STATUS_RUNNING,
            "the refusal parks the run, it never fails it"
        );
    }

    // The run is still on the ask, and a well-formed answer advances it as if nothing happened.
    assert_eq!(
        tick(&db, &rt, &run_id, Some(json!({ "answer": "go" })))["action"]["type"],
        "done"
    );
}

// ---------------------------------------------------------------------------
// F5 — a run is BOUND to the Spec it creates mid-flight
// ---------------------------------------------------------------------------

/// A run of a `sequence[ ask_human ]` workflow bound to `spec_id` (`None` = the /nspec case: a run
/// that starts with NO Spec), already TICKED — so it is live, parked, and in the session registry.
fn parked_run(db: &Db, rt: &EngineRuntime, spec_id: Option<&str>) -> String {
    let (_wf, run_id) = workflow_run(db, spec_id, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "hold", "output_key": "held" }),
        );
    });
    assert_eq!(tick(db, rt, &run_id, None)["action"]["type"], "ask_human");
    run_id
}

/// A `draft` Spec named `title` on `project_id`.
fn draft_spec(db: &Db, project_id: &str, title: &str) -> String {
    db.with_conn(|c| {
        db::create_spec(
            c,
            project_id,
            title,
            SpecSections {
                problem: "p".into(),
                solution: "s".into(),
                ..Default::default()
            },
        )
        .expect("spec")
        .0
        .id
    })
}

/// F5 — a `/nspec` run legitimately starts with NO Spec and CREATES one; creating it BINDS the run.
/// Until it was bound the run was inert — `query spec_status` answered `null`, so the commit
/// selector's `final_status == "draft"` never matched, the `[always]` note won, the `ready`
/// transition never ran and the flow ended `done` on a Spec left `draft`, in silence. Binding arms
/// the run's Spec facts: the SAME query then resolves.
#[test]
fn a_spec_created_during_an_unbound_run_binds_that_run() {
    let _g = db_guard();
    let sandbox = temp_dir("bind-run");
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, sandbox.path());
    let rt = EngineRuntime::new();
    let run_id = parked_run(&db, &rt, None);

    // The cascade's root: unbound, every Spec fact of the run answers nothing.
    assert_eq!(
        super::resolve_query(&db, &run_row(&db, &run_id), "spec_status").expect("resolve"),
        Value::Null,
        "an unbound run has no status to read — this is what disarmed the whole flow"
    );

    let spec_id = draft_spec(&db, &project_id, "Authored mid-run");
    assert_eq!(
        super::bind_session_run_to_spec(&db, &rt, &spec_id).as_deref(),
        Some(run_id.as_str()),
        "the session's in-flight run is bound to the Spec it just created"
    );
    assert_eq!(run_row(&db, &run_id).spec_id.as_deref(), Some(spec_id.as_str()));
    assert_eq!(
        super::resolve_query(&db, &run_row(&db, &run_id), "spec_status").expect("resolve"),
        json!("draft"),
        "the bound run's spec_status now RESOLVES — the commit selector can match again"
    );
}

/// F5 — a run already bound is NEVER re-bound: creating another Spec in the same session leaves it
/// on its own Spec, and the authority itself refuses the overwrite (0 rows), so nothing can steal a
/// run that is already driving a Spec.
#[test]
fn a_run_already_bound_to_a_spec_is_never_re_bound() {
    let _g = db_guard();
    let sandbox = temp_dir("bind-nosteal");
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, sandbox.path());
    let rt = EngineRuntime::new();
    let own = draft_spec(&db, &project_id, "Its own Spec");
    let run_id = parked_run(&db, &rt, Some(&own));

    let other = draft_spec(&db, &project_id, "Someone else's Spec");
    assert_eq!(
        super::bind_session_run_to_spec(&db, &rt, &other),
        None,
        "a bound run is not a candidate"
    );
    assert_eq!(run_row(&db, &run_id).spec_id.as_deref(), Some(own.as_str()));
    assert_eq!(
        db.with_conn(|c| db::set_run_spec(c, &run_id, &other))
            .expect("set_run_spec"),
        0,
        "the authority binds an UNBOUND run only — a bound one is 0 rows, never overwritten"
    );
    assert_eq!(run_row(&db, &run_id).spec_id.as_deref(), Some(own.as_str()));
}

/// F5 — the binding is TARGETED: another run of the same session, already driving its own Spec, is
/// left strictly alone.
#[test]
fn binding_a_run_leaves_the_sessions_other_runs_untouched() {
    let _g = db_guard();
    let sandbox = temp_dir("bind-other");
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, sandbox.path());
    let rt = EngineRuntime::new();
    let other_spec = draft_spec(&db, &project_id, "A Spec being driven");
    let other_run = parked_run(&db, &rt, Some(&other_spec));
    let authoring_run = parked_run(&db, &rt, None);

    let spec_id = draft_spec(&db, &project_id, "Authored mid-run");
    assert_eq!(
        super::bind_session_run_to_spec(&db, &rt, &spec_id).as_deref(),
        Some(authoring_run.as_str())
    );
    assert_eq!(
        run_row(&db, &other_run).spec_id.as_deref(),
        Some(other_spec.as_str()),
        "the run that already drives a Spec is untouched"
    );
}

/// F5 — with TWO unbound in-flight runs, "the run that created this Spec" is a guess: nyx binds
/// NEITHER rather than tying a Spec to the wrong flow.
#[test]
fn two_unbound_runs_make_the_binding_ambiguous_so_nothing_is_bound() {
    let _g = db_guard();
    let sandbox = temp_dir("bind-ambig");
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, sandbox.path());
    let rt = EngineRuntime::new();
    let first = parked_run(&db, &rt, None);
    let second = parked_run(&db, &rt, None);

    let spec_id = draft_spec(&db, &project_id, "Ambiguous");
    assert_eq!(super::bind_session_run_to_spec(&db, &rt, &spec_id), None);
    assert!(run_row(&db, &first).spec_id.is_none());
    assert!(run_row(&db, &second).spec_id.is_none());
}

/// F5 — PROPAGATION: a `sub_workflow` child inherits its parent's Spec at creation, and that still
/// holds when the parent was bound LATE — the child opened after the binding is born bound (the
/// prototyper child of /nspec's proto branch, which runs after the Spec is authored).
#[test]
fn a_child_opened_after_a_late_binding_inherits_the_parents_spec() {
    let _g = db_guard();
    let sandbox = temp_dir("bind-child");
    let db = Db::in_memory();
    let (project_id, _ws, _p) = seed_project(&db, sandbox.path());
    let rt = EngineRuntime::new();

    // CHILD: sequence { query(spec_status → child_status) }.
    let child_wf = child_workflow(&db, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_QUERY,
            json!({ "query": "spec_status", "write": "child_status" }),
        );
    });
    // PARENT: an UNBOUND run that parks on an ask_human BEFORE opening the sub_workflow.
    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "author it", "output_key": "authored" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_SUB_WORKFLOW,
            json!({ "workflow_id": child_wf, "inject": [], "retrieve": [] }),
        );
    });
    assert_eq!(tick(&db, &rt, &run_id, None)["action"]["type"], "ask_human");

    // The flow authors its Spec mid-run → the parent is bound LATE.
    let spec_id = draft_spec(&db, &project_id, "Authored before the child");
    assert_eq!(
        super::bind_session_run_to_spec(&db, &rt, &spec_id).as_deref(),
        Some(run_id.as_str())
    );

    // The next tick opens the child — which is born on the parent's freshly-bound Spec.
    let opened = tick(&db, &rt, &run_id, Some(json!({ "decision": "done" })));
    assert_eq!(opened["action"]["type"], "sub_workflow");
    let child_run_id = opened["action"]["child_run_id"].as_str().unwrap().to_string();
    assert_eq!(
        run_row(&db, &child_run_id).spec_id.as_deref(),
        Some(spec_id.as_str()),
        "the child inherits the parent's Spec even when the parent was bound late"
    );
    // And the child's own Spec queries are armed by that inheritance.
    let child_done = tick(&db, &rt, &child_run_id, None);
    assert_eq!(child_done["action"]["type"], "done");
    assert_eq!(child_done["blackboard_delta"]["child_status"], "draft");
}

// ---------------------------------------------------------------------------
// N32 — spec_can_transition(done) reflects COMPLETION (open tasks block closure)
// ---------------------------------------------------------------------------

/// N32 — `spec_can_transition` target `done` runs the COMPLETION gate ([`db::validate_done`]),
/// so an `in_review` Spec with an OPEN task answers `(false, reason mentioning open tasks)` —
/// the graph routes it to the "cannot complete" note instead of proposing closure at, say,
/// 3/10 tasks. Closing the task empties the open set → the SAME fact flips to `(true, _)`.
#[test]
fn spec_can_transition_done_reflects_open_tasks() {
    let _g = db_guard();
    let sandbox = temp_dir("cantransition-done");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());

    // A Spec driven to `in_review` (the only legal source for `done`), carrying one OPEN task.
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    db.with_conn(|c| db::transition_spec(c, &spec_id, SpecTransition::Review, false))
        .expect("review");
    let task = db
        .with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                "slice",
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    ..db::NewTaskInput::slice()
                },
            )
        })
        .expect("task");

    // A run bound to the Spec — the parameter resolve_can_transition reads its state through.
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |_, _| {});
    let run = run_row(&db, &run_id);

    // WITH an open task → the completion gate refuses, and the reason names the open tasks.
    let (can, reason) = super::resolve_can_transition(&db, &run, Some("done")).expect("resolve");
    assert!(!can, "an in_review Spec with an open task cannot complete");
    assert!(
        reason.contains("not complete") && reason.contains("OpenTasks"),
        "the reason names the open tasks, got {reason:?}"
    );

    // Close the task → 0 open → the SAME fact flips to (true, _).
    db.with_conn(|c| db::transition_task(c, &task.id, db::TaskTransition::Start, None))
        .expect("start task");
    db.with_conn(|c| db::transition_task(c, &task.id, db::TaskTransition::Done, None))
        .expect("done task");
    let (can, _reason) = super::resolve_can_transition(&db, &run, Some("done")).expect("resolve");
    assert!(can, "with no open task the in_review Spec can complete");
}

// ---------------------------------------------------------------------------
// spec_open_findings — the audit fact (review findings alone)
// ---------------------------------------------------------------------------

/// `spec_open_findings` counts the bound Spec's OPEN REVIEW FINDINGS THAT REOPEN THE BUILD and
/// NOTHING else: a build task never counts (that is `spec_open_tasks`, the COMPLETION fact), a
/// finding stops counting once it is terminal (done OR cancelled — the same predicate its sibling
/// folds), a DEFERRED (`low`/`info`) finding never counted in the first place, and an unbound run
/// answers 0. This is the fact a graph DERIVES its audit verdict from, so it must measure exactly
/// what it names.
#[test]
fn spec_open_findings_counts_only_the_open_review_findings() {
    let _g = db_guard();
    let sandbox = temp_dir("open-findings");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    declare_phases(&db, &spec_id, &["Build"]);

    let add = |title: &str, input: db::NewTaskInput| {
        db.with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                title,
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    phase_name: Some("Build".to_string()),
                    ..input
                },
            )
        })
        .expect("task")
        .id
    };

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |_, _| {});
    let run = run_row(&db, &run_id);
    let findings = || super::resolve_query(&db, &run, "spec_open_findings").expect("resolve");
    let tasks = || super::resolve_query(&db, &run, "spec_open_tasks").expect("resolve");

    // A BUILD task alone: work to do, but nothing the audit found.
    let build = add("implement it", db::NewTaskInput::slice());
    assert_eq!(findings(), json!(0), "a build task is not a finding");
    assert_eq!(tasks(), json!(1), "…while the completion fact does count it");

    // Two auditor findings of REOPENING severity.
    let f1 = add("f1", db::NewTaskInput::review("high"));
    let f2 = add("f2", db::NewTaskInput::review("medium"));
    assert_eq!(findings(), json!(2), "both open findings count");
    assert_eq!(tasks(), json!(3), "the completion fact folds build + findings");

    // A DEFERRED finding counts for NEITHER fact: only a high/medium buys another build round, and
    // both facts read the one `reopens_the_build` predicate, so they cannot disagree about it.
    let low = add("low", db::NewTaskInput::review("low"));
    let info = add("info", db::NewTaskInput::review("info"));
    assert_eq!(findings(), json!(2), "a low/info finding does not arm the verdict");
    assert_eq!(tasks(), json!(3), "…nor does it hold the build open");

    // A finding closed by the coder DROPS out of the count…
    db.with_conn(|c| db::transition_task(c, &f1, db::TaskTransition::Start, None)).expect("start");
    db.with_conn(|c| db::transition_task(c, &f1, db::TaskTransition::Done, None)).expect("done");
    assert_eq!(findings(), json!(1), "a done finding is closed");

    // …and so does a CANCELLED one (the same terminal predicate `spec_open_tasks` uses).
    db.with_conn(|c| {
        db::transition_task(c, &f2, db::TaskTransition::Skip, Some("won't fix"))
    })
    .expect("skip");
    assert_eq!(findings(), json!(0), "a cancelled finding is closed too");
    assert_eq!(
        tasks(),
        json!(1),
        "the still-open build task keeps the completion fact non-zero — the two facts are distinct"
    );

    // The two deferred findings are STILL OPEN throughout — excluded from the counts, not closed
    // by them. That is the whole trace: they stay on the Spec, with their severity, for next cycle.
    for deferred in [&low, &info] {
        assert_eq!(
            db.with_conn(|c| db::get_task(c, deferred)).unwrap().unwrap().status,
            db::TASK_STATUS_TODO,
            "a deferred finding is excluded from the counts, never closed by them"
        );
    }
    let _ = build;

    // An UNBOUND run has no Spec, so no finding — 0, never an error.
    let (_wf2, unbound) = workflow_run(&db, None, |_, _| {});
    let unbound_run = run_row(&db, &unbound);
    assert_eq!(
        super::resolve_query(&db, &unbound_run, "spec_open_findings").expect("resolve"),
        json!(0)
    );
}

// ---------------------------------------------------------------------------
// Lot 2 — phase_open_tasks + design_locked query facts
// ---------------------------------------------------------------------------

/// Lot 2 — `phase_open_tasks` counts the bound Spec's OPEN tasks IN ITS CURRENT BUILD phase
/// (INCLUDING that phase's review findings), never a later phase's. The current build phase is the
/// LOWEST phase with open work; closing all of it moves the phase forward. An unbound run → 0.
#[test]
fn phase_open_tasks_counts_the_current_build_phase_including_findings() {
    let _g = db_guard();
    let sandbox = temp_dir("phase-open");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    declare_phases(&db, &spec_id, &["Alpha", "Beta"]);

    let add = |title: &str, phase: &str, input: db::NewTaskInput| {
        db.with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                title,
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    phase_name: Some(phase.to_string()),
                    ..input
                },
            )
        })
        .expect("task")
        .id
    };

    // Phase "Alpha" (materialized FIRST → phase 1): two open slices + one open review FINDING.
    let a1 = add("a1", "Alpha", db::NewTaskInput::slice());
    let a2 = add("a2", "Alpha", db::NewTaskInput::slice());
    let finding = add("f", "Alpha", db::NewTaskInput::review("high"));
    // Phase "Beta" (phase 2): one open slice — NOT counted while Alpha is the current build phase.
    let _b1 = add("b1", "Beta", db::NewTaskInput::slice());

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |_, _| {});
    let run = run_row(&db, &run_id);

    // The current build phase is Alpha (lowest open): 2 slices + 1 finding = 3 open; Beta excluded.
    assert_eq!(
        super::resolve_query(&db, &run, "phase_open_tasks").expect("resolve"),
        json!(3)
    );

    // Close ALL of Alpha's work → the current build phase moves to Beta → its 1 open task counts.
    for t in [&a1, &a2, &finding] {
        db.with_conn(|c| db::transition_task(c, t, db::TaskTransition::Start, None))
            .expect("start");
        db.with_conn(|c| db::transition_task(c, t, db::TaskTransition::Done, None))
            .expect("done");
    }
    assert_eq!(
        super::resolve_query(&db, &run, "phase_open_tasks").expect("resolve"),
        json!(1)
    );

    // An UNBOUND run has no current phase → 0 (never an error).
    let (_wf2, unbound) = workflow_run(&db, None, |_, _| {});
    let unbound_run = run_row(&db, &unbound);
    assert_eq!(
        super::resolve_query(&db, &unbound_run, "phase_open_tasks").expect("resolve"),
        json!(0)
    );
}

/// Lot 2 — `phase_next_task` yields the id of the NEXT open task in the CURRENT build phase (in
/// `list_tasks_for_spec` order), and `null` once that phase is drained / the run is unbound. It never
/// reaches into a later phase.
#[test]
fn phase_next_task_returns_the_next_open_task_of_the_current_phase() {
    let _g = db_guard();
    let sandbox = temp_dir("phase-next");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    declare_phases(&db, &spec_id, &["Alpha", "Beta"]);

    let add = |title: &str, phase: &str, input: db::NewTaskInput| {
        db.with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                title,
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    phase_name: Some(phase.to_string()),
                    ..input
                },
            )
        })
        .expect("task")
        .id
    };

    // Phase "Alpha" (phase 1): two open slices. Phase "Beta" (phase 2): one slice — never surfaced
    // while Alpha is the current build phase.
    let a1 = add("a1", "Alpha", db::NewTaskInput::slice());
    let a2 = add("a2", "Alpha", db::NewTaskInput::slice());
    let _b1 = add("b1", "Beta", db::NewTaskInput::slice());

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |_, _| {});
    let run = run_row(&db, &run_id);

    // The next open task is Alpha's FIRST (list order), not Beta's.
    assert_eq!(
        super::resolve_query(&db, &run, "phase_next_task").expect("resolve"),
        json!(a1)
    );

    // Close a1 → the next open task of Alpha becomes a2.
    db.with_conn(|c| db::transition_task(c, &a1, db::TaskTransition::Start, None)).expect("start");
    db.with_conn(|c| db::transition_task(c, &a1, db::TaskTransition::Done, None)).expect("done");
    assert_eq!(
        super::resolve_query(&db, &run, "phase_next_task").expect("resolve"),
        json!(a2)
    );

    // Drain Alpha entirely → the current build phase moves to Beta → its slice is next (never null
    // while another phase still has open work).
    db.with_conn(|c| db::transition_task(c, &a2, db::TaskTransition::Start, None)).expect("start");
    db.with_conn(|c| db::transition_task(c, &a2, db::TaskTransition::Done, None)).expect("done");
    assert_eq!(
        super::resolve_query(&db, &run, "phase_next_task").expect("resolve"),
        json!(_b1)
    );

    // An UNBOUND run → null (no current phase, never an error).
    let (_wf2, unbound) = workflow_run(&db, None, |_, _| {});
    let unbound_run = run_row(&db, &unbound);
    assert_eq!(
        super::resolve_query(&db, &unbound_run, "phase_next_task").expect("resolve"),
        Value::Null
    );
}

/// F7 — THE COUNTING END AND THE SELECTING END ANSWER OVER THE SAME SET. A review finding carries no
/// phase (it is not part of the plan — the auditor files it un-phased), so the parent's
/// `spec_open_tasks` counted work the child's `phase_*` pair could not see: the child read
/// `task_id: null`, exited without building, and the outer loop recounted and re-opened another, to
/// `max_iterations`. Once the plan is drained the phase-less work becomes SELECTABLE, so the child
/// works the findings and the two ends agree on when there is nothing left. The nominal order is
/// untouched: while the plan still has open work, the plan comes first.
#[test]
fn the_phase_facts_fall_back_to_the_phase_less_work_once_the_plan_is_drained() {
    let _g = db_guard();
    let sandbox = temp_dir("phase-fallback");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    declare_phases(&db, &spec_id, &["Build"]);

    let add = |title: &str, phase: Option<&str>, input: db::NewTaskInput| {
        db.with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                title,
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    phase_name: phase.map(str::to_string),
                    ..input
                },
            )
        })
        .expect("task")
        .id
    };
    // One PLANNED slice in phase "Build", and one review FINDING with NO phase at all.
    let planned = add("planned", Some("Build"), db::NewTaskInput::slice());
    let finding = add("finding", None, db::NewTaskInput::review("high"));

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |_, _| {});
    let run = run_row(&db, &run_id);
    let fact = |name: &str| super::resolve_query(&db, &run, name).expect("resolve");
    let close = |task: &str| {
        db.with_conn(|c| db::transition_task(c, task, db::TaskTransition::Start, None))
            .expect("start");
        db.with_conn(|c| db::transition_task(c, task, db::TaskTransition::Done, None))
            .expect("done");
    };

    // NOMINAL, unchanged: the plan has open work, so the child drives the PLAN — the un-phased
    // finding waits its turn.
    assert_eq!(fact("spec_open_tasks"), json!(2));
    assert_eq!(fact("phase_next_task"), json!(planned));
    assert_eq!(fact("phase_open_tasks"), json!(1));

    // The plan is DRAINED and only the phase-less finding is left — the parent counts it, so the
    // child must be able to SELECT it.
    close(&planned);
    let run = run_row(&db, &run_id);
    let fact = |name: &str| super::resolve_query(&db, &run, name).expect("resolve");
    assert_eq!(fact("spec_open_tasks"), json!(1));
    assert_eq!(
        fact("phase_next_task"),
        json!(finding),
        "the child works the finding instead of exiting on work the parent still counts"
    );
    assert_eq!(fact("phase_open_tasks"), json!(1));

    // Nothing left: both ends answer empty TOGETHER — no count without a selection, which is what
    // makes the spin impossible.
    close(&finding);
    assert_eq!(fact("spec_open_tasks"), json!(0));
    assert_eq!(fact("phase_open_tasks"), json!(0));
    assert_eq!(fact("phase_next_task"), Value::Null);
}

/// F7, ACROSS THE FOUR ENDS THE DEFERRAL TOUCHES — the invariant that keeps the run from spinning:
/// the set the coder CHILD selects from is empty EXACTLY when the parent's count is zero, and
/// [`db::validate_done`] blocks on EXACTLY that same count. Four ends decide what "open work" means
/// — `spec_open_findings` (the audit verdict), `spec_open_tasks` (ndev's build gate), the
/// `phase_open_tasks`/`phase_next_task` pair (what the child works), and `validate_done` (what
/// refuses `completed`) — and F7 already cost a runaway `nyx_next` loop when two of them agreed in
/// principle but not on the set. A test that only checked the verdict would let them drift again.
///
/// Deferred findings are the new way to drift: exclude a low at the counting end but not at the
/// selecting end and the child re-picks work the parent stopped counting; exclude it at the
/// selecting end but not at `validate_done` and the Spec can never complete. So every mutation
/// below re-asserts ALL FOUR together, with lows open the whole way through.
#[test]
fn the_four_ends_agree_on_open_work_with_deferred_findings_in_play() {
    let _g = db_guard();
    let sandbox = temp_dir("four-ends");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    declare_phases(&db, &spec_id, &["Build"]);

    let add = |title: &str, phase: Option<&str>, input: db::NewTaskInput| {
        db.with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                title,
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    phase_name: phase.map(str::to_string),
                    ..input
                },
            )
        })
        .expect("task")
        .id
    };
    // The PLAN: one slice, plus a slice that carries `severity='low'`. The second one is the trap
    // the predicate must not fall into — severity is only meaningful on a FINDING, so a low-severity
    // BUILD task is ordinary planned work and MUST still count and be selectable.
    let planned = add("planned", Some("Build"), db::NewTaskInput::slice());
    let low_slice = add(
        "low slice",
        Some("Build"),
        db::NewTaskInput {
            severity: Some(db::TASK_SEVERITY_LOW.to_string()),
            ..db::NewTaskInput::slice()
        },
    );
    // The AUDIT: one high finding (reopens), and three deferred ones — a low IN the build phase, a
    // low with NO phase (the F7 shape: the auditor files un-phased), and an `info`.
    let high = add("high finding", None, db::NewTaskInput::review("high"));
    let low_phased = add("low in phase", Some("Build"), db::NewTaskInput::review("low"));
    let low_flat = add("low un-phased", None, db::NewTaskInput::review("low"));
    let info = add("info", None, db::NewTaskInput::review("info"));

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |_, _| {});
    let close = |task: &str| {
        db.with_conn(|c| db::transition_task(c, task, db::TaskTransition::Start, None))
            .expect("start");
        db.with_conn(|c| db::transition_task(c, task, db::TaskTransition::Done, None))
            .expect("done");
    };

    // The four ends, read TOGETHER: (spec_open_tasks, spec_open_findings, phase_open_tasks,
    // phase_next_task, validate_done's BLOCKING count, validate_done's DEFERRED count).
    let ends = || {
        let run = run_row(&db, &run_id);
        let n = |q: &str| {
            super::resolve_query(&db, &run, q)
                .expect("resolve")
                .as_i64()
                .expect("a count")
        };
        let next = super::resolve_query(&db, &run, "phase_next_task").expect("resolve");
        let verdict = db.with_conn(|c| db::validate_done(c, &spec_id)).expect("validate");
        let blocking = verdict
            .iter()
            .find_map(|f| match f {
                db::DoneValidation::OpenTasks { count } => Some(*count as i64),
                _ => None,
            })
            .unwrap_or(0);
        let deferred = verdict
            .iter()
            .find_map(|f| match f {
                db::DoneValidation::DeferredFindings { count } => Some(*count as i64),
                _ => None,
            })
            .unwrap_or(0);
        (n("spec_open_tasks"), n("spec_open_findings"), n("phase_open_tasks"), next, blocking, deferred)
    };

    // THE INVARIANT: the child's selection is empty exactly when the parent's count is zero, and
    // validate_done blocks on exactly that count. Asserted after EVERY mutation, never once.
    let invariant = |label: &str| {
        let (spec_open, _findings, phase_open, next, blocking, _deferred) = ends();
        assert_eq!(
            phase_open == 0,
            next.is_null(),
            "{label}: phase_open_tasks and phase_next_task must answer over one set",
        );
        assert_eq!(
            spec_open == 0,
            next.is_null(),
            "{label}: the child selects nothing EXACTLY when the parent counts nothing",
        );
        assert_eq!(
            blocking, spec_open,
            "{label}: validate_done blocks on the same set the build gate counts",
        );
    };

    // The plan first (unchanged F7 nominal): 2 slices + the high = 3 open; the phase holds the two
    // slices; the deferred three are counted by NOBODY yet reported by validate_done.
    invariant("plan open");
    assert_eq!(ends(), (3, 1, 2, json!(planned), 3, 3));

    // The low-severity SLICE is ordinary work: draining it is what moves the phase on, and it was
    // selectable in its turn.
    close(&planned);
    invariant("first slice closed");
    assert_eq!(ends(), (2, 1, 1, json!(low_slice), 2, 3));

    // The plan is drained. What is left is the un-phased HIGH — and NOT the un-phased lows, which the
    // fallback must not hand the child either.
    close(&low_slice);
    invariant("plan drained");
    assert_eq!(
        ends(),
        (1, 1, 1, json!(high), 1, 3),
        "the child falls back to the high finding alone — never onto a deferred one",
    );

    // The high is fixed. NOTHING reopens the build any more, while THREE findings are still open:
    // every counting end reads 0, the child selects nothing, and `validate_done` refuses nothing —
    // it REPORTS the three.
    close(&high);
    invariant("only deferred left");
    assert_eq!(ends(), (0, 0, 0, Value::Null, 0, 3));
    for deferred in [&low_phased, &low_flat, &info] {
        assert_eq!(
            db.with_conn(|c| db::get_task(c, deferred)).unwrap().unwrap().status,
            db::TASK_STATUS_TODO,
            "the deferred findings stay OPEN on the Spec — traced, not closed and not worked",
        );
    }
}

/// Lot 2 — `design_locked` is true IFF the bound Spec's prototype carries a whole-design FREEZE (a
/// `design_lock`), and false with no prototype / no lock / an unbound run.
#[test]
fn design_locked_is_true_only_when_a_design_lock_exists() {
    let _g = db_guard();
    let sandbox = temp_dir("design-locked");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |_, _| {});
    let run = run_row(&db, &run_id);

    // No prototype yet → false.
    assert_eq!(
        super::resolve_query(&db, &run, "design_locked").expect("resolve"),
        json!(false)
    );

    // A prototype with a round, but no lock → still false.
    let (proto_id, round_id) = db.with_conn(|c| {
        let proto = db::create_prototype(c, &spec_id, None).expect("prototype");
        let round = db::create_round(c, &proto.id, "r1", None).expect("round");
        (proto.id, round.id)
    });
    assert_eq!(
        super::resolve_query(&db, &run, "design_locked").expect("resolve"),
        json!(false)
    );

    // Freeze the design → true.
    db.with_conn(|c| db::lock_design(c, &proto_id, &round_id, None))
        .expect("lock");
    assert_eq!(
        super::resolve_query(&db, &run, "design_locked").expect("resolve"),
        json!(true)
    );

    // An UNBOUND run → false (nothing to read).
    let (_wf2, unbound) = workflow_run(&db, None, |_, _| {});
    let unbound_run = run_row(&db, &unbound);
    assert_eq!(
        super::resolve_query(&db, &unbound_run, "design_locked").expect("resolve"),
        json!(false)
    );
}

// ---------------------------------------------------------------------------
// L2 — a question is not put to a human about an EMPTY set
// ---------------------------------------------------------------------------

/// THE USER'S OWN PROTOTYPE, rebuilt row by row: 2 pages, 1 version, 1 throwaway `probe` variant,
/// the round `create_prototype` seeds — and ZERO pinned pages. The effector laid down a skeleton and
/// stopped, which is the state the election was asked over. Returns `(prototype_id, round_id,
/// page_ids, version_id)` so a test can pin into it and watch the answer change.
fn skeleton_prototype(db: &Db, spec_id: &str) -> (String, String, Vec<String>, String) {
    db.with_conn(|c| {
        let proto = db::create_prototype(c, spec_id, None).expect("prototype");
        let round = db::current_round(c, &proto.id)
            .expect("round")
            .expect("create_prototype seeds v1");
        let home = db::create_page(c, &proto.id, "accueil", "Accueil", 0).expect("page");
        let settings = db::create_page(c, &proto.id, "reglages", "Réglages", 1).expect("page");
        let version = db::create_page_version(c, &home.id, "v1", None).expect("version");
        db::create_variant(
            c,
            &version.id,
            &db::VariantInput {
                label: "probe".into(),
                title: "Shape probe".into(),
                ..Default::default()
            },
        )
        .expect("variant");
        (proto.id, round.id, vec![home.id, settings.id], version.id)
    })
}

/// A run on the SEEDED `prototyper` built-in, bound to `spec_id` — the real graph, not a stand-in.
fn prototyper_run(db: &Db, spec_id: &str) -> String {
    db.with_conn(|c| {
        let wf = db::list_workflows(c, None)
            .expect("workflows")
            .into_iter()
            .find(|w| w.scope == db::WORKFLOW_SCOPE_PROTOTYPER)
            .expect("the seeded prototyper built-in");
        db::create_workflow_run(c, &wf.id, Some(spec_id), None, "{}")
            .expect("run")
            .id
    })
}

/// L2 — `design_round_pages` COUNTS what the current round puts to the election, and its companion
/// `<write>_reason` NAMES it. The fact reads the pinned manifest of [`db::current_round`] — the same
/// set `proto_tree` serves the UI from and `proto_distill` refuses to freeze when empty — so a page
/// that exists but is not PINNED is not a candidate, which is exactly the distinction the live run
/// lacked (2 pages on the prototype, 0 in the round, and nyx asked for an election anyway).
#[test]
fn design_round_pages_counts_only_what_the_round_actually_pins() {
    let _g = db_guard();
    let sandbox = temp_dir("l2-round-pages");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |_, _| {});
    let run = run_row(&db, &run_id);

    // No prototype at all → 0, and the sentence says which absence it is.
    let (count, why) = super::resolve_design_round_pages(&db, &run).expect("resolve");
    assert_eq!(count, json!(0));
    assert!(why.contains("no prototype"), "the empty answer says why: {why}");

    // THE USER'S STATE: pages, a version, a variant — and nothing pinned into the round.
    let (_proto, round_id, pages, version_id) = skeleton_prototype(&db, &spec_id);
    let (count, why) = super::resolve_design_round_pages(&db, &run).expect("resolve");
    assert_eq!(
        count,
        json!(0),
        "2 pages EXIST on the prototype, but the round pins none — there is nothing to elect"
    );
    assert!(
        why.contains("pins NO page"),
        "…and the sentence says so, in the round's own terms: {why}"
    );

    // PIN one page → 1, and the listing names the page AND the variants on offer.
    db.with_conn(|c| db::set_round_page(c, &round_id, &pages[0], &version_id, 0)).expect("pin");
    let (count, why) = super::resolve_design_round_pages(&db, &run).expect("resolve");
    assert_eq!(count, json!(1), "one pinned page is one candidate");
    assert!(
        why.contains("'Accueil' (accueil)") && why.contains("probe"),
        "the listing NAMES the page and the variant the human must choose between: {why}"
    );

    // An UNBOUND run reads no prototype — 0, never an error.
    let (_wf2, unbound) = workflow_run(&db, None, |_, _| {});
    let (count, _) = super::resolve_design_round_pages(&db, &run_row(&db, &unbound)).expect("resolve");
    assert_eq!(count, json!(0));
}

/// L2 — THE LIVE FAILURE, on the REAL seeded graph: a prototype with pages and versions but ZERO
/// pinned candidates in the open round must NEVER reach the election question.
///
/// What the first NOMINAL prototyper run did instead: the effector laid down a skeleton and stopped
/// at a throwaway probe variant, and nyx asked the human to "Elect the RETAINED variant per page"
/// over a round holding no page. Free-form, so any answer passed; the next question routed to
/// `distill`, which distilled nothing; `design_locked` stayed false; and the loop bought a fresh
/// prototype sub-agent to learn the same thing. H3's guard cannot fire here — the run DOES carry a
/// Spec, so `design_locked` is reachable in principle, just not with this content.
///
/// The graph now asks the effector for the round, TWICE, and then dies naming what never held —
/// which is the failed child nspec's H4 branch turns into the real question for the human.
#[test]
fn a_round_that_pins_nothing_never_reaches_the_election() {
    let _g = db_guard();
    let sandbox = temp_dir("l2-empty-round");
    let db = Db::in_memory();
    db.with_conn(crate::seed::seed_builtin_workflows).expect("built-ins");
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    skeleton_prototype(&db, &spec_id);
    let run_id = prototyper_run(&db, &spec_id);
    assert!(
        run_row(&db, &run_id).spec_id.is_some(),
        "the run CARRIES a Spec — H3's unbound-fact guard is not what stops this one",
    );

    // Drive the child exactly as its dispatcher would: perform the action, report it, call again.
    let rt = EngineRuntime::new();
    let mut spawns = 0;
    let mut asked: Vec<String> = Vec::new();
    let mut failure = None;
    let mut last: Option<Value> = None;
    for _ in 0..30 {
        let mut args = json!({ "run_id": run_id });
        if let Some(l) = last.take() {
            args.as_object_mut().unwrap().insert("last_result".into(), l);
        }
        match super::nyx_next(&db, &rt, no_session(), &args) {
            Ok(out) => {
                let action = &out.result["action"];
                assert_ne!(
                    action["type"], "done",
                    "a round that composed nothing must not report success: {}",
                    out.result
                );
                if action["type"] == "ask_human" {
                    asked.push(action["prompt"].as_str().unwrap_or_default().to_string());
                }
                if action["type"] == "spawn_subagent" {
                    spawns += 1;
                }
                last = Some(json!({ "diff": "a skeleton, and no pinned page" }));
            }
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }

    assert!(
        asked.is_empty(),
        "NOTHING was asked of the human over an empty round — got {asked:?}",
    );
    assert_eq!(
        spawns, 2,
        "the effector was asked for the round twice (`seed::PROTO_ROUND_ATTEMPTS`) and no further — \
         the outer loop's twenty rounds were never bought",
    );
    let error = failure.expect("an effector that composes nothing fails the child, it does not spin");
    assert!(
        error.message.contains("design_round_pages is truthy"),
        "the failure names WHICH condition never held: {}",
        error.message
    );
    assert!(
        error.message.contains("design_round_pages = 0"),
        "…and what it last observed for it: {}",
        error.message
    );
    assert_eq!(
        run_row(&db, &run_id).status,
        db::RUN_STATUS_FAILED,
        "the child is FAILED, which is what nspec's 'proto did not complete' branch reads",
    );
}

/// L2, the other half — a round that DOES pin pages reaches the election in ONE effector pass (no
/// cost added to the nominal path), and the question NAMES the pages and the variants on the table
/// instead of asking a human to choose from a surface nyx never showed them.
#[test]
fn the_election_names_what_is_actually_on_the_table() {
    let _g = db_guard();
    let sandbox = temp_dir("l2-elect-names");
    let db = Db::in_memory();
    db.with_conn(crate::seed::seed_builtin_workflows).expect("built-ins");
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let (_proto, round_id, pages, version_id) = skeleton_prototype(&db, &spec_id);
    db.with_conn(|c| {
        db::create_variant(
            c,
            &version_id,
            &db::VariantInput {
                label: "dense".into(),
                title: "Dense".into(),
                position: 1,
                ..Default::default()
            },
        )
        .expect("a second variant to arbitrate between");
        db::set_round_page(c, &round_id, &pages[0], &version_id, 0).expect("pin");
    });

    let run_id = prototyper_run(&db, &spec_id);
    let rt = EngineRuntime::new();
    let first = tick(&db, &rt, &run_id, None);
    assert_eq!(first["action"]["type"], "spawn_subagent", "the effector composes: {first}");
    let elected = tick(&db, &rt, &run_id, Some(json!({ "diff": "a composed round" })));
    assert_eq!(
        elected["action"]["type"], "ask_human",
        "ONE pass sufficed — a productive round costs exactly what it always did: {elected}"
    );
    let prompt = elected["action"]["prompt"].as_str().expect("a prompt");
    assert!(prompt.contains("Elect the RETAINED variant"), "it IS the election: {prompt}");
    assert!(
        prompt.contains("'Accueil' (accueil)"),
        "the question names the page it is about: {prompt}"
    );
    assert!(
        prompt.contains("probe") && prompt.contains("dense"),
        "…and the variants it asks the human to choose between: {prompt}"
    );
}

// ---------------------------------------------------------------------------
// Lot 2 — the transition_task node (nyx-inline task lifecycle effect)
// ---------------------------------------------------------------------------

/// Create an OPEN `slice` task on `spec_id`, returning its id.
fn open_slice(db: &Db, project_id: &str, spec_id: &str) -> String {
    db.with_conn(|c| {
        db::create_task(
            c,
            project_id,
            "t",
            db::NewTaskInput {
                spec_id: Some(spec_id.to_string()),
                ..db::NewTaskInput::slice()
            },
        )
    })
    .expect("task")
    .id
}

/// Build a single-node workflow and a run seeded with `board`, returning the run id.
fn run_with_board(db: &Db, spec_id: &str, board: Value, build: impl FnOnce(&Db, &str)) -> String {
    let wf = db.with_conn(|c| {
        db::create_workflow(c, None, "wf", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("wf")
            .id
    });
    build(db, &wf);
    db.with_conn(|c| {
        db::create_workflow_run(c, &wf, Some(spec_id), None, &board.to_string())
            .expect("run")
            .id
    })
}

fn task_status(db: &Db, task_id: &str) -> String {
    db.with_conn(|c| db::get_task(c, task_id))
        .unwrap()
        .unwrap()
        .status
}

/// Lot 2 — a `transition_task` node marks the task named by the board key `task_id`: a
/// `sequence{ transition_task(start) → transition_task(done) }` walks INLINE (never surfaced to the
/// dispatcher) and drives the task todo → in_progress → done in ONE tick.
#[test]
fn a_transition_task_node_marks_the_task_start_then_done() {
    let _g = db_guard();
    let sandbox = temp_dir("tt-mark");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let task_id = open_slice(&db, &project_id, &spec_id);

    let run_id = run_with_board(&db, &spec_id, json!({ "task_id": task_id }), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(db, wf, Some(&root), 0, db::NODE_TYPE_TRANSITION_TASK, json!({ "transition": "start" }));
        add_node(db, wf, Some(&root), 1, db::NODE_TYPE_TRANSITION_TASK, json!({ "transition": "done" }));
    });

    let rt = EngineRuntime::new();
    let done = tick(&db, &rt, &run_id, None);
    assert_eq!(
        done["action"]["type"], "done",
        "both transition_task effects run inline — the run reaches done in one tick"
    );
    assert_eq!(task_status(&db, &task_id), db::TASK_STATUS_DONE);
}

/// Lot 2 — an ILLEGAL task transition (a `done` on a `todo` task — `done` needs `in_progress`) is
/// REFUSED: the run BLOCKS and the leaf is RE-ATTEMPTED on the next call. Starting the task
/// out-of-band then re-ticking lets the replayed leaf succeed and the run reaches done.
#[test]
fn an_illegal_transition_task_refuses_and_blocks_for_replay() {
    let _g = db_guard();
    let sandbox = temp_dir("tt-illegal");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let task_id = open_slice(&db, &project_id, &spec_id);

    let run_id = run_with_board(&db, &spec_id, json!({ "task_id": task_id }), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(db, wf, Some(&root), 0, db::NODE_TYPE_TRANSITION_TASK, json!({ "transition": "done" }));
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state", "an illegal source is refused");
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_BLOCKED);
    assert_eq!(task_status(&db, &task_id), db::TASK_STATUS_TODO, "the refused effect wrote nothing");

    // Start the task out-of-band → the replayed `done` now has a legal source → the run advances.
    db.with_conn(|c| db::transition_task(c, &task_id, db::TaskTransition::Start, None))
        .expect("start");
    let done = tick(&db, &rt, &run_id, None);
    assert_eq!(done["action"]["type"], "done", "the re-attempted leaf succeeded");
    assert_eq!(task_status(&db, &task_id), db::TASK_STATUS_DONE);
}

/// Lot 2 — a `done` on a `kind=human` task is REFUSED (a human action closes only through the
/// verified path), so the run BLOCKS rather than silently closing it through the plain path.
#[test]
fn a_transition_task_done_on_a_human_task_refuses() {
    let _g = db_guard();
    let sandbox = temp_dir("tt-human");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let task_id = db
        .with_conn(|c| {
            db::create_task(
                c,
                &project_id,
                "human",
                db::NewTaskInput {
                    spec_id: Some(spec_id.clone()),
                    ..db::NewTaskInput::human("verify-me")
                },
            )
        })
        .expect("human task")
        .id;

    let run_id = run_with_board(&db, &spec_id, json!({ "task_id": task_id }), |db, wf| {
        add_node(db, wf, None, 0, db::NODE_TYPE_TRANSITION_TASK, json!({ "transition": "done" }));
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains("verified path"),
        "the refusal points at the verified path: {}",
        err.message
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_BLOCKED);
    assert_eq!(task_status(&db, &task_id), db::TASK_STATUS_TODO, "the human task is untouched");
}

// ---------------------------------------------------------------------------
// Lot 2 — the sub_workflow node (child run, isolated board, verified result)
// ---------------------------------------------------------------------------

/// Build a child workflow from `build` and return its id.
fn child_workflow(db: &Db, build: impl FnOnce(&Db, &str)) -> String {
    let wf = db.with_conn(|c| {
        db::create_workflow(c, None, "child", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("child wf")
            .id
    });
    build(db, &wf);
    wf
}

/// A parent tree `sequence{ sub_workflow(→ child_wf) ; selector( sub_result.status==done ? ok : fail ) }`.
fn parent_tree_with_sub(child_wf: &str) -> impl FnOnce(&Db, &str) + '_ {
    move |db: &Db, wf: &str| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SUB_WORKFLOW,
            json!({
                "workflow_id": child_wf,
                "inject": [{ "from": "task_id", "to": "task_id" }],
                "retrieve": [{ "from": "child_status", "to": "child_status" }],
            }),
        );
        let sel = add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_SELECTOR,
            json!({ "branches": [
                { "cond": { "var": "sub_result.status", "eq": "done" }, "child": 0 },
                { "cond": { "always": true }, "child": 1 },
            ] }),
        );
        add_node(db, wf, Some(&sel), 0, db::NODE_TYPE_NOTE, json!({ "text": "child-ok" }));
        add_node(db, wf, Some(&sel), 1, db::NODE_TYPE_NOTE, json!({ "text": "child-fail" }));
    }
}

/// Lot 2 — a `sub_workflow` node opens a CHILD run bound to the parent's Spec, with `parent_run_id`
/// set and the child board seeded ONLY with the injected keys; on the child COMPLETING, its VERIFIED
/// `retrieve` keys fold onto the parent board and the parent selector branches on the result.
#[test]
fn a_sub_workflow_opens_an_isolated_child_and_folds_its_verified_result() {
    let _g = db_guard();
    let sandbox = temp_dir("subwf-ok");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // CHILD: sequence { query(spec_status → child_status) } — writes a VERIFIED key inline.
    let child_wf = child_workflow(&db, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_QUERY,
            json!({ "query": "spec_status", "write": "child_status" }),
        );
    });

    let run_id = run_with_board(
        &db,
        &spec_id,
        json!({ "task_id": "T", "secret": "parent-only" }),
        parent_tree_with_sub(&child_wf),
    );
    // Anchor the parent on its workspace (as run_start does) — the child must inherit it.
    db.with_conn(|c| db::set_run_workspace(c, &run_id, &workspace_id))
        .expect("anchor parent workspace");

    let rt = EngineRuntime::new();
    // Tick 1: the parent opens the child and SUSPENDS with a dispatcher action carrying the child id.
    let opened = tick(&db, &rt, &run_id, None);
    assert_eq!(opened["action"]["type"], "sub_workflow");
    let child_run_id = opened["action"]["child_run_id"].as_str().unwrap().to_string();
    assert!(
        opened["action"]["context"].as_str().unwrap().contains("nyx_next"),
        "the action carries a generic dispatcher context"
    );

    // The child is bound to the SAME spec + workspace, carries parent_run_id, and its board holds
    // ONLY the injected key (the parent's `secret` never leaked into the isolated child).
    let child = run_row(&db, &child_run_id);
    assert_eq!(child.spec_id.as_deref(), Some(spec_id.as_str()));
    assert_eq!(child.parent_run_id.as_deref(), Some(run_id.as_str()));
    assert_eq!(child.workspace_id.as_deref(), Some(workspace_id.as_str()));
    let child_board: Value = serde_json::from_str(&child.blackboard).unwrap();
    assert_eq!(child_board["task_id"], "T");
    assert!(
        child_board.get("secret").is_none(),
        "the child sees ONLY the injected keys (isolated board)"
    );

    // Drive the CHILD to done (its query runs inline → done in one tick).
    let child_done = tick(&db, &rt, &child_run_id, None);
    assert_eq!(child_done["action"]["type"], "done");
    assert_eq!(run_row(&db, &child_run_id).status, db::RUN_STATUS_DONE);

    // Tick the PARENT again: it reads the child's VERIFIED result, folds `child_status`, and the
    // selector branches on sub_result.status == done → the "child-ok" note, riding the end of the run.
    let advanced = tick(&db, &rt, &run_id, None);
    assert_eq!(advanced["action"]["type"], "done");
    assert_eq!(advanced["action"]["notes"], json!(["child-ok"]));
    assert_eq!(advanced["blackboard_delta"]["child_status"], "in_progress");
    assert_eq!(advanced["blackboard_delta"]["sub_result"]["status"], "done");
}

/// H3 — A LOOP WHOSE EXIT CANNOT BE SATISFIED DIES ON ITS FIRST ROUND, NOT ITS TWENTIETH. Observed
/// live, the `prototyper` shape: a `sub_workflow` child of a Spec-less parent loops
/// `until design_locked == true`, but `design_locked` is a HARD `false` for a run bound to no Spec,
/// so the exit is unreachable BY CONSTRUCTION. The human answered, the design was distilled, and the
/// next round spawned a prototype again — twenty times, 22 sub-agents asked for and billed, before
/// `exceeded 20 iterations without exiting`.
///
/// The child must now fail after ONE body pass (repeat-until owes a first one), and the failure must
/// SAY which condition never held, what it last read, and why it can never change.
#[test]
fn an_unreachable_loop_exit_kills_the_child_on_its_first_round() {
    let _g = db_guard();
    let db = Db::in_memory();

    // CHILD, the prototyper shape: loop until design_locked == true [ spawn, query design_locked ].
    let child_wf = child_workflow(&db, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        let rounds = add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_LOOP,
            json!({ "until": { "var": "design_locked", "eq": true }, "max_iterations": 20 }),
        );
        add_node(
            db,
            wf,
            Some(&rounds),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "prototype" }),
        );
        add_node(
            db,
            wf,
            Some(&rounds),
            1,
            db::NODE_TYPE_QUERY,
            json!({ "query": "design_locked", "write": "design_locked" }),
        );
    });

    // PARENT, bound to NO Spec — the /nspec authoring run that opened the proto branch.
    let parent_wf = db.with_conn(|c| {
        db::create_workflow(c, None, "unbound-parent", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("wf")
            .id
    });
    let root = add_node(&db, &parent_wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        &db,
        &parent_wf,
        Some(&root),
        0,
        db::NODE_TYPE_SUB_WORKFLOW,
        json!({ "workflow_id": child_wf }),
    );
    let parent_run = db.with_conn(|c| {
        db::create_workflow_run(c, &parent_wf, None, None, "{}")
            .expect("run")
            .id
    });

    let rt = EngineRuntime::new();
    let opened = tick(&db, &rt, &parent_run, None);
    let child_run_id = opened["action"]["child_run_id"].as_str().expect("child").to_string();
    let child = run_row(&db, &child_run_id);
    assert!(child.spec_id.is_none(), "the child inherited the parent's absence of Spec");
    assert_eq!(child.parent_run_id.as_deref(), Some(parent_run.as_str()));

    // Drive the CHILD exactly as its dispatcher would: perform each action, report, call again.
    let mut spawns = 0;
    let mut failure = None;
    let mut last: Option<Value> = None;
    for _ in 0..30 {
        let mut args = json!({ "run_id": child_run_id });
        if let Some(l) = last.take() {
            args.as_object_mut().unwrap().insert("last_result".into(), l);
        }
        match super::nyx_next(&db, &rt, no_session(), &args) {
            Ok(out) => {
                assert_ne!(
                    out.result["action"]["type"], "done",
                    "a loop that can never lock the design must not report success"
                );
                assert_eq!(out.result["action"]["type"], "spawn_subagent");
                spawns += 1;
                last = Some(json!({ "diff": "a prototype round" }));
            }
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }

    assert_eq!(spawns, 1, "ONE sub-agent was asked for, not twenty-two");
    let error = failure.expect("the trapped loop fails instead of spinning");
    assert!(
        error.message.contains("design_locked == true"),
        "the failure names WHICH condition never held: {}",
        error.message
    );
    assert!(
        error.message.contains("design_locked = false"),
        "…and what it last observed for it: {}",
        error.message
    );
    assert!(
        error.message.contains("no Spec"),
        "…and why that value can never change: {}",
        error.message
    );
    assert_eq!(run_row(&db, &child_run_id).status, db::RUN_STATUS_FAILED);
}

/// Lot 2 (idempotency) — a resumed parent whose child is STILL running RE-DISPATCHES the SAME child
/// (same run_id) rather than opening a SECOND child run.
#[test]
fn a_resumed_sub_workflow_parent_does_not_open_a_second_child() {
    let _g = db_guard();
    let sandbox = temp_dir("subwf-idem");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // CHILD: never driven → it stays RUNNING (a fresh run is `running` until ticked).
    let child_wf = child_workflow(&db, |db, wf| {
        add_node(db, wf, None, 0, db::NODE_TYPE_NOTE, json!({ "text": "child" }));
    });
    let run_id = run_with_board(&db, &spec_id, json!({}), parent_tree_with_sub(&child_wf));

    let rt = EngineRuntime::new();
    let first = tick(&db, &rt, &run_id, None);
    let child1 = first["action"]["child_run_id"].as_str().unwrap().to_string();

    // Resume the parent WITHOUT driving the child: it re-surfaces the SAME child, no second run.
    let second = tick(&db, &rt, &run_id, None);
    assert_eq!(second["action"]["type"], "sub_workflow");
    assert_eq!(
        second["action"]["child_run_id"].as_str(),
        Some(child1.as_str()),
        "the resumed parent re-dispatches the SAME child"
    );
    let runs = db
        .with_conn(|c| db::find_runs_for_spec(c, &child_wf, Some(&spec_id)))
        .unwrap();
    assert_eq!(runs.len(), 1, "no second child run was opened");
}

/// A CHILD tree that PARKS on a spawn — enough for a run to have a real persisted walk to lose.
fn child_tree_that_parks(db: &Db, wf: &str) {
    let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
    add_node(
        db,
        wf,
        Some(&root),
        0,
        db::NODE_TYPE_SPAWN_SUBAGENT,
        json!({ "agent_id": "coder" }),
    );
}

/// A workflow named `name` with no project (the shape of a built-in), built by `build`.
fn workflow_named(db: &Db, name: &str, build: impl FnOnce(&Db, &str)) -> String {
    let wf = db.with_conn(|c| {
        db::create_workflow(c, None, name, db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("workflow")
            .id
    });
    build(db, &wf);
    wf
}

/// The BUILT-IN UPGRADE, exactly as the boot seeder performs it on a workflow whose revision is
/// behind: RECORD the replacement on every live run that loses a walk, WIPE the tree, rebuild it.
fn upgrade_graph(db: &Db, wf: &str, from: i32, to: i32, build: impl FnOnce(&Db, &str)) {
    db.with_conn(|c| {
        db::mark_runs_graph_replaced(
            c,
            wf,
            &json!({ "cause": "builtin_upgrade", "from_revision": from, "to_revision": to }),
        )
    })
    .expect("mark the live runs");
    db.with_conn(|c| db::delete_nodes_for_workflow(c, wf)).expect("wipe the tree");
    build(db, wf);
}

/// `run_start` on `(spec, workflow)`, returning the run id.
fn start_run(db: &Db, spec_id: &str, workflow_id: &str) -> String {
    super::run_start(db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": workflow_id }))
        .expect("run_start")
        .result["run_id"]
        .as_str()
        .expect("run_id")
        .to_string()
}

/// M1 — THE USER'S SEQUENCE, end to end. A parent with a LIVE child; the built-in graphs upgraded
/// under BOTH (the 18 → 19 bump); then the FRESH parent reaching its `sub_workflow` node.
///
/// `run_start` already verified the run IT resolved, so the parked parent was correctly replaced. The
/// `sub_workflow` node did not: the N20 idempotency on (child workflow, spec) handed it the child from
/// BEFORE the bump — a run that still reads `running` and can never tick again — and it adopted it,
/// re-attaching it to the fresh parent. The parent then re-emitted its `sub_workflow` action at every
/// tick, forever, and because the PARENT had been replaced correctly nothing pointed at the child as
/// the cause (the user's only way out was `run_delete` on the child by hand). The node must verify what
/// the resolution hands it, exactly as `run_start` does, and SAY what it replaced.
#[test]
fn a_fresh_parent_does_not_adopt_a_child_that_can_no_longer_tick() {
    let _g = db_guard();
    let sandbox = temp_dir("m1-user-sequence");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    let child_wf = workflow_named(&db, "coder", child_tree_that_parks);
    let parent_wf = workflow_named(&db, "ndev", parent_tree_with_sub(&child_wf));

    // The run the user was driving, and the child it opened — driven one tick, so the child has a
    // REAL walk to lose (the corpse-to-be).
    let rt = EngineRuntime::new();
    let parent1 = start_run(&db, &spec_id, &parent_wf);
    let opened = tick(&db, &rt, &parent1, None);
    let child1 = opened["action"]["child_run_id"].as_str().expect("a child").to_string();
    assert_eq!(tick(&db, &rt, &child1, None)["action"]["type"], "spawn_subagent");
    assert!(run_row(&db, &child1).traversal.is_some(), "the child has a walk to lose");

    // THE BUMP — nyx upgrades its built-ins at boot: the parent's graph AND the child's.
    upgrade_graph(&db, &parent_wf, 18, 19, parent_tree_with_sub(&child_wf));
    upgrade_graph(&db, &child_wf, 18, 19, child_tree_that_parks);

    // The child is now a CORPSE: live by status, refused by its own tick.
    assert!(
        !db::run_status_is_terminal(&run_row(&db, &child1).status),
        "the corpse still reads live"
    );
    super::nyx_next(&db, &EngineRuntime::new(), no_session(), &json!({ "run_id": child1 }))
        .expect_err("the child's persisted traversal no longer fits its tree");

    // run_start replaces the PARENT (M4, unchanged) — the fresh parent is what reaches the node.
    let parent2 = start_run(&db, &spec_id, &parent_wf);
    assert_ne!(parent2, parent1, "the parked parent is replaced, as it already was");

    let rt2 = EngineRuntime::new();
    let reopened = tick(&db, &rt2, &parent2, None);
    assert_eq!(reopened["action"]["type"], "sub_workflow");
    let child2 = reopened["action"]["child_run_id"].as_str().expect("a child").to_string();
    assert_ne!(child2, child1, "the corpse is NOT re-adopted");
    assert!(
        db.with_conn(|c| db::get_workflow_run(c, &child1)).unwrap().is_none(),
        "it is DROPPED, exactly as run_start drops the run it refuses"
    );
    assert_eq!(
        runs_for(&db, &child_wf, &spec_id).len(),
        1,
        "and N20 still holds for the pair: one row replaced by one row"
    );
    assert_eq!(
        run_row(&db, &child2).parent_run_id.as_deref(),
        Some(parent2.as_str()),
        "the fresh child is attached to the fresh parent"
    );
    // The whole point: the child the parent hands back can actually be DRIVEN.
    assert_eq!(
        tick(&db, &rt2, &child2, None)["action"]["type"],
        "spawn_subagent",
        "the replacement child drives the rebuilt graph"
    );

    // …and the parent SAYS what it did, on the notes riding the very action that carries the new
    // child id — a session that had been driving the old child must not silently switch to another.
    let notes = reopened["action"]["notes"]
        .as_array()
        .unwrap_or_else(|| panic!("the notes ride the action: {}", reopened["action"]));
    let said = notes
        .iter()
        .filter_map(Value::as_str)
        .find(|n| n.contains(&child1))
        .unwrap_or_else(|| panic!("the replacement is announced: {notes:?}"));
    assert!(said.contains("DROPPED"), "the note says the child was destroyed: {said}");
    assert!(
        said.contains("UPGRADED") && said.contains("18 → 19"),
        "…names the boot upgrade that killed it, revisions included: {said}"
    );
    assert!(
        said.contains(&child2) && said.contains("FRESH"),
        "…and that the child in this action is a fresh one from the top: {said}"
    );
}

/// The other side of the same check: a child the resolution hands back that is GENUINELY alive is
/// ADOPTED. The N20 idempotency on (child workflow, spec) is right in itself — a second parent must not
/// open a second child on the same Spec — so the verification must replace a corpse and NOTHING else,
/// and it must announce nothing when it replaced nothing.
#[test]
fn a_sub_workflow_adopts_a_live_child_it_resolves() {
    let _g = db_guard();
    let sandbox = temp_dir("m1-live-child");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    let child_wf = workflow_named(&db, "coder", child_tree_that_parks);
    let parent_wf = workflow_named(&db, "ndev", parent_tree_with_sub(&child_wf));

    let rt = EngineRuntime::new();
    let parent1 = start_run(&db, &spec_id, &parent_wf);
    let child1 = tick(&db, &rt, &parent1, None)["action"]["child_run_id"]
        .as_str()
        .expect("a child")
        .to_string();
    let walked = tick(&db, &rt, &child1, None);
    assert_eq!(walked["action"]["type"], "spawn_subagent");
    let progressed = run_row(&db, &child1);

    // A SECOND parent on the same Spec (the run_start idempotency handed this one a fresh parent row
    // only because we deleted the first) reaches the node and resolves the SAME live child.
    db.with_conn(|c| db::delete_workflow_run(c, &parent1)).expect("drop the first parent");
    let parent2 = start_run(&db, &spec_id, &parent_wf);
    let reopened = tick(&db, &rt, &parent2, None);
    assert_eq!(
        reopened["action"]["child_run_id"].as_str(),
        Some(child1.as_str()),
        "a healthy child is ADOPTED, not replaced"
    );
    let after = run_row(&db, &child1);
    assert_eq!(
        (after.traversal, after.current_node_id),
        (progressed.traversal, progressed.current_node_id),
        "and its walk is untouched — no reset, no replay"
    );
    assert!(
        reopened["action"].get("notes").is_none(),
        "nothing was replaced, so nothing is announced: {}",
        reopened["action"]
    );
}

/// M1 (the same blind spot, one tick later) — a child that dies WHILE the parent waits on it. Only the
/// CHILD's graph moved (a project FORK of the parent flow is not upgraded with the built-in it came
/// from), so the parent survives and keeps re-checking a child that reads `running` and can never reach
/// a terminal state: it re-dispatched a sub-agent onto it at every tick, forever.
///
/// A child that cannot be advanced is a child that DIED, which is the fact the parent graph is already
/// built to test (H4): it folds as `failed`, the parent's own selector decides, and the note says which
/// child died and why.
#[test]
fn a_child_that_dies_while_the_parent_waits_folds_as_failed() {
    let _g = db_guard();
    let sandbox = temp_dir("m1-child-dies");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    let child_wf = workflow_named(&db, "coder", child_tree_that_parks);
    let run_id = run_with_board(&db, &spec_id, json!({}), parent_tree_with_sub(&child_wf));

    let rt = EngineRuntime::new();
    let child_run_id = tick(&db, &rt, &run_id, None)["action"]["child_run_id"]
        .as_str()
        .expect("a child")
        .to_string();
    tick(&db, &rt, &child_run_id, None);

    // The CHILD's built-in is upgraded under it; the parent's own graph is untouched.
    upgrade_graph(&db, &child_wf, 18, 19, child_tree_that_parks);

    let advanced = tick(&db, &rt, &run_id, None);
    assert_eq!(
        advanced["blackboard_delta"]["sub_result"]["status"], "failed",
        "the dead child is a FAILED child, not something to wait on: {}",
        advanced["action"]
    );
    assert_eq!(advanced["action"]["type"], "done");
    let notes = advanced["action"]["notes"]
        .as_array()
        .unwrap_or_else(|| panic!("the notes ride the action: {}", advanced["action"]));
    let said = notes
        .iter()
        .filter_map(Value::as_str)
        .find(|n| n.contains(&child_run_id))
        .unwrap_or_else(|| panic!("the death is announced: {notes:?}"));
    assert!(
        said.contains("no longer be advanced") && said.contains("18 → 19"),
        "the note names the child and what killed it: {said}"
    );
    assert!(
        notes.iter().any(|n| n == "child-fail"),
        "and the parent's OWN selector took the failure branch: {notes:?}"
    );
}

/// Lot 2 (refinement) — a child driven to BLOCKED (a red blocking gate, M5) surfaces a
/// `sub_result.status == "blocked"` to the parent WITHOUT an indefinite wait, and the parent selector
/// branches on it (retry/abandon lives at the parent — the level with human agency).
#[test]
fn a_blocked_child_surfaces_a_blocked_sub_result_without_waiting() {
    let _g = db_guard();
    let sandbox = temp_dir("subwf-blocked");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let red = gate_command(&db, &project_id, "sh -c 'exit 1'");

    // CHILD: sequence { gate_check(red, blocking) } — driving it BLOCKS at the end (M5 re-eval).
    let child_wf = child_workflow(&db, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [red], "blocking": true }),
        );
    });
    let run_id = run_with_board(&db, &spec_id, json!({}), parent_tree_with_sub(&child_wf));

    let rt = EngineRuntime::new();
    let opened = tick(&db, &rt, &run_id, None);
    let child_run_id = opened["action"]["child_run_id"].as_str().unwrap().to_string();

    // Drive the CHILD → it BLOCKS on the red gate (M5): nyx_next refuses and persists BLOCKED.
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": child_run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(run_row(&db, &child_run_id).status, db::RUN_STATUS_BLOCKED);

    // Tick the PARENT: it does NOT wait — it surfaces status "blocked" and the selector routes to fail.
    let advanced = tick(&db, &rt, &run_id, None);
    assert_eq!(advanced["action"]["type"], "done");
    assert_eq!(advanced["action"]["notes"], json!(["child-fail"]));
    assert_eq!(advanced["blackboard_delta"]["sub_result"]["status"], "blocked");
}

/// Lot 2 (Design A — child ask_human BUBBLE-UP) — a CHILD parked on an `ask_human` surfaces THROUGH
/// the parent's sub_workflow node: the parent re-emits the child's question carrying `child_run_id` +
/// `child_node_id` + `via:"sub_workflow"`. The answer routes to `nyx_next(child_run_id)` (advancing the
/// CHILD), and a later parent tick folds the child's done result. While the child waits, re-ticking the
/// parent BUBBLES again — never an infinite sub_workflow re-dispatch.
#[test]
fn a_child_ask_human_bubbles_up_through_the_parent_sub_workflow() {
    let _g = db_guard();
    let sandbox = temp_dir("subwf-bubble");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // CHILD: sequence { ask_human(elect, options a|b) } — parks on the ask, completes on the answer.
    let child_wf = child_workflow(&db, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "Elect a variant", "output_key": "election", "options": ["a", "b"] }),
        );
    });
    let run_id = run_with_board(&db, &spec_id, json!({}), parent_tree_with_sub(&child_wf));
    db.with_conn(|c| db::set_run_workspace(c, &run_id, &workspace_id)).expect("anchor");

    let rt = EngineRuntime::new();
    // Parent opens the child and suspends on its sub_workflow node.
    let opened = tick(&db, &rt, &run_id, None);
    assert_eq!(opened["action"]["type"], "sub_workflow");
    let child_run_id = opened["action"]["child_run_id"].as_str().unwrap().to_string();

    // Drive the CHILD to its ask_human PARK (the dispatcher's own tick on the child run).
    let child_ask = tick(&db, &rt, &child_run_id, None);
    assert_eq!(child_ask["action"]["type"], "ask_human");
    // WHERE the child is parked is its OWN tick's `current_node_id` — the action carries no node id
    // (G5); only the BUBBLED form does, under `child_node_id`, and that is what is checked below.
    let child_node_id = child_ask["current_node_id"].as_str().unwrap().to_string();

    // Re-tick the PARENT: it BUBBLES the child's ask up through the sub_workflow node.
    let bubble = tick(&db, &rt, &run_id, None);
    assert_eq!(bubble["action"]["type"], "ask_human", "the parent re-emits the child's ask");
    assert_eq!(bubble["action"]["via"], "sub_workflow");
    assert_eq!(bubble["action"]["child_run_id"], json!(child_run_id));
    assert_eq!(
        bubble["action"]["child_node_id"],
        json!(child_node_id),
        "the thread points at the child's ask node"
    );
    assert_eq!(bubble["action"]["prompt"], "Elect a variant");
    assert_eq!(bubble["action"]["options"], json!(["a", "b"]));
    // F4 — the THREADED form states the same answer contract as a top-level ask.
    assert_eq!(
        bubble["action"]["answer_key"], "decision",
        "the bubbled ask names the field the answer must be sent under"
    );

    // NO infinite re-dispatch: re-ticking the parent (still unanswered) bubbles the SAME ask, never a
    // sub_workflow re-dispatch.
    let bubble2 = tick(&db, &rt, &run_id, None);
    assert_eq!(bubble2["action"]["type"], "ask_human");
    assert_eq!(bubble2["action"]["child_run_id"], json!(child_run_id));

    // The answer routes to nyx_next(CHILD): the child's own ask resume + N19 handle it → child done.
    let child_after = tick(&db, &rt, &child_run_id, Some(json!({ "decision": "a" })));
    assert_eq!(child_after["action"]["type"], "done");
    assert_eq!(run_row(&db, &child_run_id).status, db::RUN_STATUS_DONE);

    // Re-tick the PARENT: the child is done → it FOLDS the result and the selector routes to child-ok.
    let folded = tick(&db, &rt, &run_id, None);
    assert_eq!(folded["action"]["type"], "done");
    assert_eq!(folded["action"]["notes"], json!(["child-ok"]));
    assert_eq!(folded["blackboard_delta"]["sub_result"]["status"], "done");
}

/// H6 × lot 2 — a CONSENT question inside a CHILD run bubbles up STILL FLAGGED. The bubble is where
/// the top-level session first learns the question, so it is where it must also learn that answering
/// it takes a real human turn — a bubble that dropped the flag would let the forgery through with one
/// indirection, since the answer is routed straight to the child. The check itself lives on the
/// child's own tick, which is where the answer lands.
#[test]
fn a_child_consent_question_bubbles_up_still_demanding_a_human_turn() {
    let _g = db_guard();
    let sandbox = temp_dir("subwf-consent");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // CHILD: the consent shape — an ask whose answer is what an engaging `done` hangs on.
    let child_wf = child_workflow(&db, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            json!({ "prompt": "close it?", "output_key": "approved", "options": ["yes", "no"] }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "cancel", "gate_on": "approved" }),
        );
    });
    let run_id = run_with_board(&db, &spec_id, json!({}), parent_tree_with_sub(&child_wf));
    db.with_conn(|c| db::set_run_workspace(c, &run_id, &workspace_id))
        .expect("anchor");

    let session = Session::new("term-h6-sub");
    let rt = EngineRuntime::new();
    session.human_says(&db, "drive the sub-flow");
    let opened = session.tick(&db, &rt, &run_id, None);
    let child_run_id = opened["action"]["child_run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let asked = session.tick(&db, &rt, &child_run_id, None);
    assert_eq!(asked["action"]["type"], "ask_human");
    assert_eq!(asked["action"]["human_turn_required"], true);

    // The PARENT bubbles it — same question, same demand, routed to the child.
    let bubble = session.tick(&db, &rt, &run_id, None);
    assert_eq!(bubble["action"]["type"], "ask_human");
    assert_eq!(bubble["action"]["prompt"], "close it?");
    assert_eq!(bubble["action"]["child_run_id"], child_run_id);
    assert_eq!(bubble["action"]["human_turn_required"], true);

    // And the CHILD refuses the answer relayed through that thread with no human turn behind it.
    let err = session.tick_err(
        &db,
        &rt,
        &child_run_id,
        Some(json!({ "decision": "yes" })),
    );
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("human_consent_without_human_turn"),
    );
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_IN_PROGRESS);

    // The human answers; the same relay, through the same thread, now moves the Spec.
    session.human_says(&db, "yes, close it");
    session.tick(&db, &rt, &child_run_id, Some(json!({ "decision": "yes" })));
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_CANCELLED);
}

// ---------------------------------------------------------------------------
// M6 — the gate SURFACES its output (why a red gate is red) + a global time bound
// ---------------------------------------------------------------------------

/// M6 — a gate whose command FAILS surfaces WHY: the gate-red refusal carries the failing command's
/// non-zero exit code, `passed:false`, and a non-empty output tail, so the dispatcher can report the
/// cause immediately instead of guessing among a bad env / wrong subfolder / genuinely failing test.
#[test]
fn a_red_gate_surfaces_its_failing_command_in_the_refusal() {
    let _g = db_guard();
    let sandbox = temp_dir("m6red");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let gate = gate_command(&db, &project_id, "sh -c 'echo boom; exit 1'");

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    let data = err.data.as_ref().expect("the refusal carries data");
    assert_eq!(data.get("reason").and_then(Value::as_str), Some("gate_red"));

    let gate_result = data
        .get("gate_result")
        .expect("the refusal surfaces the red gate's result (M6)");
    assert_eq!(gate_result["passed"], json!(false));
    let cmd = &gate_result["commands"][0];
    assert_eq!(cmd["exit_code"], json!(1), "the non-zero exit code is surfaced");
    assert_eq!(cmd["passed"], json!(false));
    assert!(
        cmd["output_tail"].as_str().unwrap().contains("boom"),
        "the failing command's output tail is captured: {}",
        cmd["output_tail"]
    );
    // The cmdline + cwd are surfaced too (they disambiguate a wrong-subfolder / bad-env red).
    assert!(cmd["cmdline"].as_str().unwrap().contains("boom"));
    assert!(cmd["cwd"].is_string());
}

/// M6 — a gate whose command PASSES is captured too: `passed:true`, exit 0, output present. The tick
/// payload carries it (the per-tick `blackboard_delta`), and it persists on the run.
#[test]
fn a_green_gate_captures_its_command_output() {
    let _g = db_guard();
    let sandbox = temp_dir("m6green");
    let db = Db::in_memory();
    let (project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());
    let gate = gate_command(&db, &project_id, "sh -c 'echo hello; exit 0'");

    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_NOTE,
            json!({ "text": "gate passed" }),
        );
    });

    let rt = EngineRuntime::new();
    let out = tick(&db, &rt, &run_id, None);
    assert_eq!(
        out["action"]["type"], "done",
        "a green gate advances past the note to the end"
    );
    assert_eq!(out["action"]["notes"], json!(["gate passed"]));

    // The tick payload carries the gate result via the blackboard delta.
    let gate_result = &out["blackboard_delta"]["last_gate_result"];
    assert_eq!(gate_result["passed"], json!(true));
    let cmd = &gate_result["commands"][0];
    assert_eq!(cmd["exit_code"], json!(0));
    assert_eq!(cmd["passed"], json!(true));
    assert!(cmd["output_tail"].as_str().unwrap().contains("hello"));

    // And it PERSISTS on the run for the UI to read after the fact.
    let board: Value = serde_json::from_str(&run_row(&db, &run_id).blackboard).unwrap();
    assert_eq!(board["last_gate_result"]["passed"], json!(true));
    assert!(board["last_gate_result"]["node_id"].is_string());
}

/// M6 — the persisted gate result is READABLE AFTER THE FACT through the SAME read the UI uses
/// (`list_workflow_runs`): after a red gate, the blocked run's blackboard carries `last_gate_result`.
#[test]
fn a_red_gate_result_is_persisted_and_readable_via_list_workflow_runs() {
    let _g = db_guard();
    let sandbox = temp_dir("m6persist");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let gate = gate_command(&db, &project_id, "sh -c 'echo nope; exit 2'");

    let (wf_id, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();

    // The SAME accessor the sidecar `engine_run_list` read-command wraps.
    let runs = db
        .with_conn(|c| db::list_workflow_runs(c, &wf_id))
        .unwrap();
    let run = runs
        .iter()
        .find(|r| r.id == run_id)
        .expect("the run is listed");
    assert_eq!(run.status, db::RUN_STATUS_BLOCKED);
    let board: Value = serde_json::from_str(&run.blackboard).unwrap();
    let gate_result = &board["last_gate_result"];
    assert_eq!(gate_result["passed"], json!(false));
    assert_eq!(gate_result["commands"][0]["exit_code"], json!(2));
    assert!(gate_result["commands"][0]["output_tail"]
        .as_str()
        .unwrap()
        .contains("nope"));
}

/// M6 — the captured output tail is BOUNDED (the last `cap` bytes, keeping the END). Pure test of the
/// tail buffer, so it is deterministic and independent of PTY timing.
#[test]
fn the_gate_output_tail_buffer_is_bounded_and_keeps_the_end() {
    let mut tail = super::GateTail::new(8);
    tail.push(b"0123456789ABCDEF");
    assert_eq!(tail.into_string(), "89ABCDEF", "keeps the last `cap` bytes");

    // Across many small pushes the bound still holds and the END is retained.
    let mut tail = super::GateTail::new(100);
    for _ in 0..1000 {
        tail.push(b"0123456789");
    }
    let s = tail.into_string();
    assert!(s.len() <= 100, "bounded to the cap regardless of total pushed");
    assert!(s.ends_with("0123456789"), "keeps the END");
}

/// M6 — a CHATTY gate command (far more output than the cap) yields a tail ≤ the cap that keeps the
/// END: the real capture path, over a real PTY, respects [`GATE_OUTPUT_TAIL_BYTES`].
#[test]
fn a_chatty_gate_command_tail_is_capped_to_the_end() {
    let _g = db_guard();
    let sandbox = temp_dir("m6cap");
    let db = Db::in_memory();
    let (project_id, _ws_id, _ws) = seed_project(&db, sandbox.path());
    // ~45 KB of filler, then a sentinel, then fail: the kept tail must drop the front, stay ≤ cap,
    // and still hold the sentinel printed at the END.
    let gate = gate_command(
        &db,
        &project_id,
        "sh -c 'yes ABCDEFGH | head -n 5000; echo TAIL_SENTINEL; exit 1'",
    );
    let outcome = super::run_gate_bounded(&db, &[gate], super::GATE_TOTAL_TIMEOUT).expect("gate");
    assert!(!outcome.passed, "the command exits non-zero");
    let tail = &outcome.commands[0].output_tail;
    assert!(
        tail.len() <= super::GATE_OUTPUT_TAIL_BYTES,
        "the tail is bounded to the cap, got {} bytes",
        tail.len()
    );
    assert!(
        tail.contains("TAIL_SENTINEL"),
        "the END (the most recent output) is kept, not the front"
    );
    assert!(
        !tail.starts_with("ABCDEFGH"),
        "the FRONT was dropped once over the cap"
    );
}

/// N31 (THE fix) — a gate command carrying a PERSISTED environment runs WITH it. The command echoes
/// a variable nyx's own process does not set; seeing its value in the captured output tail (M6)
/// proves the stored env reached the process nyx spawned ITSELF for the gate — the path that used to
/// hardcode an empty env while `start_command` honoured one, so the same command was green by hand
/// and red in the gate.
#[test]
#[cfg(not(windows))]
fn a_gate_command_runs_with_the_environment_persisted_on_the_command() {
    let _g = db_guard();
    let sandbox = temp_dir("n31-gate-env");
    let db = Db::in_memory();
    let (project_id, _ws_id, _ws) = seed_project(&db, sandbox.path());
    let gate = gate_command_with_env(
        &db,
        &project_id,
        "sh -c 'echo GOT=$NYX_GATE_PERSISTED; test -n \"$NYX_GATE_PERSISTED\"'",
        r#"{"NYX_GATE_PERSISTED":"from-the-command-definition"}"#,
    );

    let outcome = super::run_gate_bounded(&db, &[gate], super::GATE_TOTAL_TIMEOUT).expect("gate");
    assert!(
        outcome.passed,
        "the gate is GREEN because the variable IS set; got {:?}",
        outcome.commands[0]
    );
    assert!(
        outcome.commands[0]
            .output_tail
            .contains("GOT=from-the-command-definition"),
        "the persisted env must reach the spawned gate process, got {:?}",
        outcome.commands[0].output_tail
    );
}

/// N31 — a MALFORMED persisted env does not make the command unresolvable: the gate still RUNS, with
/// no env from that column. A typo in a stored map must cost the variables, never the ability to run
/// the command (and therefore to fix it).
#[test]
#[cfg(not(windows))]
fn a_malformed_persisted_env_still_lets_the_gate_run() {
    let _g = db_guard();
    let sandbox = temp_dir("n31-gate-badenv");
    let db = Db::in_memory();
    let (project_id, _ws_id, _ws) = seed_project(&db, sandbox.path());
    // Not an object at all, and — on the second command — an object with a non-string value.
    let broken = gate_command_with_env(
        &db,
        &project_id,
        "sh -c 'echo BROKEN_RAN; exit 0'",
        "not json at all",
    );
    let non_string = gate_command_with_env(
        &db,
        &project_id,
        "sh -c 'echo NONSTRING=[$NYX_GATE_NONSTRING]; exit 0'",
        r#"{"NYX_GATE_NONSTRING":5433}"#,
    );

    let outcome = super::run_gate_bounded(&db, &[broken, non_string], super::GATE_TOTAL_TIMEOUT)
        .expect("gate");
    assert!(outcome.passed, "both commands ran and exited 0");
    assert!(outcome.commands[0].output_tail.contains("BROKEN_RAN"));
    assert!(
        outcome.commands[1].output_tail.contains("NONSTRING=[]"),
        "the non-string entry is dropped, the command still runs; got {:?}",
        outcome.commands[1].output_tail
    );
}

/// M6 — the GLOBAL time bound: a gate that would run past its total budget is RED with a clear
/// "total time budget" marker, never a hang. The budget is INJECTED (100 ms) so the bound is proven
/// without sleeping the real 90s; the marker helper is asserted to format the configured budget.
#[test]
fn the_global_budget_marks_a_slow_gate_red_without_hanging() {
    let _g = db_guard();
    let sandbox = temp_dir("m6budget");
    let db = Db::in_memory();
    let (project_id, _ws_id, _ws) = seed_project(&db, sandbox.path());
    // Would sleep far past the injected budget — the global bound must cut it and mark the gate red.
    let gate = gate_command(&db, &project_id, "sh -c 'sleep 30'");
    let outcome = super::run_gate_bounded(&db, &[gate], std::time::Duration::from_millis(100))
        .expect("gate");
    assert!(!outcome.passed, "a gate over its total budget is RED");
    let cmd = &outcome.commands[0];
    assert!(
        cmd.timeout
            .as_deref()
            .unwrap_or_default()
            .contains("total time budget"),
        "the result records the global-budget marker, got {:?}",
        cmd.timeout
    );
    assert_eq!(cmd.exit_code, None, "a timed-out command has no exit code");
    assert!(!cmd.passed);

    // The marker helper formats the configured GATE_TOTAL_TIMEOUT.
    assert!(super::gate_total_budget_marker().contains("90s"));
}

// ---------------------------------------------------------------------------
// M5 — a run parked on a RED gate has an EXIT (re-run at the block point + abandon)
// ---------------------------------------------------------------------------

/// M5 (the CORE proof) — a run parked `blocked` on a RED blocking gate is no longer a dead-end. The
/// gate is RE-RUN at the block point (the transition guard) on every attempt to pass it: fixing the
/// cause and calling `nyx_next` again re-runs the gate GREEN, clears the anchor, and the run advances
/// past the transition — no cursor surgery. The gate `test -f gate_ok` is red until the sentinel lands.
#[test]
fn a_parked_red_gate_re_runs_at_the_block_point_and_advances_once_fixed() {
    let _g = db_guard();
    let sandbox = temp_dir("m5-exit");
    let db = Db::in_memory();
    let (project_id, workspace_id, ws_path) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    // Red until the coder's fix (the sentinel) exists.
    let gate = gate_command(&db, &project_id, "test -f gate_ok");

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    // Call 1: the gate is RED → the run parks `blocked`, the transition refused (M6 result carried).
    let err = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("gate_red")
    );
    assert!(
        err.data
            .as_ref()
            .and_then(|d| d.get("gate_result"))
            .is_some(),
        "the refusal carries M6's gate_result"
    );
    assert_eq!(spec_status(&db, &spec_id), db::SPEC_STATUS_IN_PROGRESS);
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_BLOCKED);

    // FIX the cause (the sentinel now exists), then call the SAME verb again — the parked gate RE-RUNS
    // at the block point, GREEN this time, clears the anchor, and the run advances past the transition.
    std::fs::write(format!("{ws_path}/gate_ok"), "ok").unwrap();
    let out = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id }))
        .expect("the fixed gate re-runs green — no more refusal")
        .result;
    assert_eq!(
        out["action"]["type"], "done",
        "the fixed gate re-ran green and the run advanced past the transition"
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_IN_REVIEW,
        "the transition fired once the re-run gate was green"
    );
    let row = run_row(&db, &run_id);
    assert_eq!(row.status, db::RUN_STATUS_DONE);
    let board: Value = serde_json::from_str(&row.blackboard).unwrap();
    assert!(
        board.get("blocking_gate_red").is_none(),
        "the red-gate anchor was cleared by the green re-run"
    );
}

/// M5 (the ABANDON path) — a persistently-RED gate whose human answers "abandon" routes through the
/// `clear_gate_block` note to a CLEAN end WITHOUT a `refuse_gate`: no `gate_red` error, the Spec is left
/// un-transitioned, the anchor is cleared, and the run reaches `done`. The transition the "retry" branch
/// guards is SKIPPED. This mirrors the seeded ndev's gate-red abandon shape (a red gate → retry/abandon
/// ask → a clear_gate_block note that skips the in_review transition).
#[test]
fn the_abandon_path_clears_the_gate_block_and_ends_clean_without_refusing() {
    let _g = db_guard();
    let sandbox = temp_dir("m5-abandon");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let gate = gate_command(&db, &project_id, "false"); // never green

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_ASK_HUMAN,
            json!({
                "prompt": "checks failed: retry or abandon?",
                "output_key": "gate_choice",
                "options": ["retry", "abandon"],
            }),
        );
        let sel = add_node(
            db,
            wf,
            Some(&root),
            2,
            db::NODE_TYPE_SELECTOR,
            json!({ "branches": [
                { "cond": { "var": "gate_choice", "eq": "retry" }, "child": 0 },
                { "cond": { "always": true }, "child": 1 },
            ] }),
        );
        // retry → the transition (would refuse while red — never taken here).
        add_node(
            db,
            wf,
            Some(&sel),
            0,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
        // abandon (the safe default) → the clear_gate_block note = the clean end.
        add_node(
            db,
            wf,
            Some(&sel),
            1,
            db::NODE_TYPE_NOTE,
            json!({ "text": "build abandoned — gate not satisfied", "clear_gate_block": true }),
        );
    });

    let rt = EngineRuntime::new();
    // Call 1: the gate runs RED (anchored), the run parks on the ask_human (the gate is not surfaced).
    let ask = tick(&db, &rt, &run_id, None);
    assert_eq!(ask["action"]["type"], "ask_human");
    assert_eq!(ask["action"]["options"], json!(["retry", "abandon"]));
    assert!(
        ask["blackboard_delta"]["blocking_gate_red"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "the red blocking gate anchored an id: {ask}"
    );

    // Call 2: the human ABANDONS → the selector default routes to the clear_gate_block note, which
    // CLEARS the anchor, and the walk carries on to `done` in the SAME tick WITHOUT a refuse_gate (the
    // Done re-eval sees an EMPTY anchor, so it never re-runs the still-red gate). The note RIDES it.
    let done = super::nyx_next(
        &db,
        &rt, no_session(),
        &json!({ "run_id": run_id, "last_result": { "decision": "abandon" } }),
    )
    .expect("the abandon path reaches done with NO refusal")
    .result;
    assert_eq!(done["action"]["type"], "done");
    assert!(
        done["action"]["notes"][0]
            .as_str()
            .unwrap_or_default()
            .contains("abandoned"),
        "the abandon note rides the done: {done}"
    );

    // The Spec is LEFT un-transitioned, the anchor is cleared, and the run ended clean.
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_IN_PROGRESS,
        "the in_review transition was skipped by the abandon route"
    );
    let row = run_row(&db, &run_id);
    assert_eq!(row.status, db::RUN_STATUS_DONE);
    let board: Value = serde_json::from_str(&row.blackboard).unwrap();
    assert!(
        board.get("blocking_gate_red").is_none(),
        "the anchor was cleared by the clear_gate_block note"
    );
    // …and the tick SAYS it dropped that key (N22). The delta is a forward diff, so a caller tracking
    // the board from ticks alone — which is exactly what the delta exists to let it do — went on
    // reading the anchor set in call 1 and believed a blocking gate was still red.
    assert_eq!(
        done["blackboard_removed"],
        json!(["blocking_gate_red"]),
        "the cleared anchor is reported as REMOVED, not merely absent from the delta: {done}"
    );
}

/// G4 — a REFUSED tick still says what the walk said. The notes are the channel the walk speaks to
/// the human on ("the child run this step was waiting on was DROPPED — SAY SO"), and they rode the
/// returned ACTION only: a tick that ends in a refusal hands back no action, so every note it had
/// walked past died with the stack frame. The caller was told the gate was red and nothing else.
#[test]
fn a_refused_tick_carries_the_notes_the_walk_went_past() {
    let _g = db_guard();
    let sandbox = temp_dir("g4-refusal-notes");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let gate = gate_command(&db, &project_id, "false"); // never green

    // A note the walk goes past, then a blocking gate that refuses the tick it is walked in.
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_NOTE,
            json!({ "text": "the child run this step waited on was DROPPED" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
    });

    let err = super::nyx_next(
        &db,
        &EngineRuntime::new(),
        no_session(),
        &json!({ "run_id": run_id }),
    )
    .expect_err("the red blocking gate refuses");
    let data = err.data.as_ref().expect("a refusal carries its markers");
    assert_eq!(
        data["reason"], "gate_red",
        "the refusal's own markers are untouched — they are what the dispatcher branches on"
    );
    assert_eq!(
        data["notes"],
        json!(["the child run this step waited on was DROPPED"]),
        "and the note rides the refusal, under the same `notes` array an action carries: {data}"
    );
}

/// M5 — a run REFUSED at its `done` by a red blocking gate stays RESUMABLE IN A LATER SESSION.
///
/// Reaching the end of the tree is not the same thing as being RECORDED done: the re-eval refuses the
/// completion while a gate is red and parks the run `blocked` on it. That stop used to persist a NULL
/// `traversal` (the engine declined to export one once it had reached `Step::Done`), so the next
/// session read a progressed run with no walk to restore, refused it as `run_not_resumable`, and
/// `start_or_replace_run` DESTROYED it and restarted the flow from the top — the exact opposite of the
/// exit M5 exists to give. A cold runtime must land on the SAME refusal, not on a dead run.
#[test]
fn a_run_refused_at_done_by_a_red_gate_still_resumes_in_the_next_session() {
    let _g = db_guard();
    let sandbox = temp_dir("m5-done-resume");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let gate = gate_command(&db, &project_id, "false"); // never green

    // A gate and NOTHING after it: the walk reaches `Step::Done` in the very tick that runs it.
    let (wf_id, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
    });

    let first = super::nyx_next(
        &db,
        &EngineRuntime::new(),
        no_session(),
        &json!({ "run_id": run_id }),
    )
    .expect_err("the red blocking gate refuses the done");
    assert_eq!(first.data.as_ref().unwrap()["reason"], "gate_red");

    let row = run_row(&db, &run_id);
    assert_eq!(row.status, db::RUN_STATUS_BLOCKED);
    assert!(
        row.traversal.is_some(),
        "a run parked BLOCKED still carries the walk that got it there — it is only a TERMINAL \
         status that clears it; got {row:?}"
    );

    // A COLD runtime = the next session. It must re-refuse on the gate, never die as unresumable.
    let resumed = super::nyx_next(
        &db,
        &EngineRuntime::new(),
        no_session(),
        &json!({ "run_id": run_id }),
    )
    .expect_err("the still-red gate refuses again");
    assert_eq!(
        resumed.data.as_ref().unwrap()["reason"],
        "gate_red",
        "the next session resumes onto the SAME refusal, not `run_not_resumable`: {}",
        resumed.message
    );

    // …and `run_start` therefore RESUMES it rather than dropping it as a corpse.
    let opened = super::run_start(&db, &EngineRuntime::new(), &json!({ "spec_id": spec_id, "workflow_id": wf_id }))
        .expect("run_start resumes the blocked run")
        .result;
    assert_eq!(
        opened["run_id"], run_id,
        "the blocked run is the one handed back — it was not destroyed and replaced: {opened}"
    );
}

/// M5 (the RETRY path, the SEED shape) — a gate-red `ask_human` answered "revise" RE-RUNS the loop body
/// (coder → gate). The loop is keyed on `audit_verdict`; the gate-red ask writes audit_verdict='revise',
/// so the inner loop re-runs and the gate is RE-EVALUATED (exactly the seeded ndev "checks passed?"
/// shape). Attempt 1 is red (parks on the ask), the human says revise, the coder's fix makes attempt 2
/// green → the audit path runs and the run advances to done. Proves "retry loops back so the gate re-runs".
#[test]
fn a_gate_red_retry_re_runs_the_loop_body_and_the_gate() {
    let _g = db_guard();
    let sandbox = temp_dir("m5-retry");
    let db = Db::in_memory();
    let (project_id, workspace_id, ws_path) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    // Red until the coder's fix (the sentinel) exists.
    let gate = gate_command(&db, &project_id, "test -f gate_ok");

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        let build = add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_LOOP,
            json!({ "until": { "var": "audit_verdict", "ne": "revise" } }),
        );
        add_node(
            db,
            wf,
            Some(&build),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
        add_node(
            db,
            wf,
            Some(&build),
            1,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        let checks = add_node(
            db,
            wf,
            Some(&build),
            2,
            db::NODE_TYPE_SELECTOR,
            json!({ "branches": [
                { "cond": { "var": "gate_green", "eq": true }, "child": 0 },
                { "cond": { "always": true }, "child": 1 },
            ] }),
        );
        // GREEN → the audit judge (writes audit_verdict = 'ready' ⇒ loop exits).
        add_node(
            db,
            wf,
            Some(&checks),
            0,
            db::NODE_TYPE_JUDGE,
            json!({ "prompt": "ready?", "output_key": "audit_verdict" }),
        );
        // RED → the retry/abandon ask (writes audit_verdict; 'revise' ⇒ re-loop ⇒ gate re-runs).
        add_node(
            db,
            wf,
            Some(&checks),
            1,
            db::NODE_TYPE_ASK_HUMAN,
            json!({
                "prompt": "checks failed",
                "output_key": "audit_verdict",
                "options": ["revise", "abandon"],
            }),
        );
    });

    // The retry ask BUYS ANOTHER ROUND — it re-opens a loop whose body spawns — so its answer is the
    // human's to give (J2) and this deroulé must be a real session that puts the question to them.
    let session = Session::new("term-m5-retry");
    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut coder_spawns = 0;
    let mut saw_gate_red_ask = false;
    let mut steps = 0;
    loop {
        let out = session.tick(&db, &rt, &run_id, last.take());
        match out["action"]["type"].as_str().unwrap() {
            "spawn_subagent" => {
                coder_spawns += 1;
                // The SECOND coder writes the fix so attempt 2's gate is GREEN.
                if coder_spawns == 2 {
                    std::fs::write(format!("{ws_path}/gate_ok"), "ok").unwrap();
                }
                last = Some(json!({ "diff": format!("attempt {coder_spawns}") }));
            }
            "ask_human" => {
                // The only ask is the gate-red retry/abandon one — answer 'revise' to RETRY (re-run gate).
                saw_gate_red_ask = true;
                assert_eq!(
                    out["action"]["human_turn_required"], true,
                    "spending another round is not the session's call to make: {out}"
                );
                session.human_says(&db, "oui, refais un tour");
                last = Some(json!({ "decision": "revise" }));
            }
            "judge" => last = Some(json!({ "verdict": "ready" })), // the green-pass audit passes
            "done" => break,
            other => panic!("unexpected action {other}: {out}"),
        }
        steps += 1;
        assert!(steps < 20, "runaway retry loop");
    }

    assert!(saw_gate_red_ask, "the red gate surfaced the retry/abandon ask");
    assert_eq!(
        coder_spawns, 2,
        "'revise' re-ran the loop body — the coder ran again and the gate was RE-EVALUATED"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
    let board: Value = serde_json::from_str(&run_row(&db, &run_id).blackboard).unwrap();
    assert_eq!(board["gate_green"], json!(true), "the re-run gate ended green");
    assert!(
        board.get("blocking_gate_red").is_none(),
        "the green re-run cleared the anchor its first red raised"
    );
}

// ---------------------------------------------------------------------------
// F3 — every tick leaves an auditable trace in the outbox
// ---------------------------------------------------------------------------

/// The `run.tick` outbox rows of `run_id`, oldest-first — the run's journal.
fn tick_rows(db: &Db, run_id: &str) -> Vec<Value> {
    db.with_conn(db::list_domain_events)
        .unwrap()
        .into_iter()
        .filter(|e| e.aggregate == db::AGGREGATE_RUN)
        .map(|e| serde_json::from_str::<Value>(&e.payload).expect("a JSON payload"))
        .filter(|p| p["run_id"] == run_id)
        .collect()
}

/// F3 (the 1:1 proof) — a STOP of `nyx_next` leaves EXACTLY ONE `run.tick` row, and an engine step
/// walked THROUGH leaves none. The run unrolls the whole ndev-shaped loop in one session: two
/// spawns, two `gate_check` runs (the first RED), one `transition` — six engine actions — but only
/// THREE stops, because the gate and the transition are nyx's own inline effects and were never
/// handed to the dispatcher. The journal counts what the dispatcher was TOLD.
#[test]
fn every_stop_of_nyx_next_journals_exactly_one_run_tick_row() {
    let _g = db_guard();
    let sandbox = temp_dir("f3-1to1");
    let db = Db::in_memory();
    let (project_id, workspace_id, ws_path) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let gate = gate_command(&db, &project_id, "test -f gate_ok");

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        let phase = add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_LOOP,
            json!({ "until": { "var": "gate_green", "eq": true } }),
        );
        add_node(
            db,
            wf,
            Some(&phase),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
        add_node(
            db,
            wf,
            Some(&phase),
            1,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "in_review" }),
        );
    });

    let rt = EngineRuntime::new();
    let mut last: Option<Value> = None;
    let mut ticks = 0;
    let mut spawns = 0;
    loop {
        let out = tick(&db, &rt, &run_id, last.take());
        ticks += 1;
        match out["action"]["type"].as_str().unwrap() {
            "spawn_subagent" => {
                spawns += 1;
                // Only the second coder fixes the code, so the FIRST gate runs RED.
                if spawns == 2 {
                    std::fs::write(format!("{ws_path}/gate_ok"), "ok").unwrap();
                }
                last = Some(json!({ "diff": format!("attempt {spawns}") }));
            }
            "done" => break,
            other => panic!("unexpected action {other}: {out}"),
        }
        assert!(ticks < 10, "runaway loop");
    }
    assert_eq!(ticks, 3, "two spawns and the end — the walk the journal must match");

    let rows = tick_rows(&db, &run_id);
    assert_eq!(rows.len(), ticks, "ONE journal row per STOP: {rows:#?}");
    // What each stop DECIDED, in order — the two dispatcher actions then the end.
    let decided: Vec<&str> = rows
        .iter()
        .map(|p| p["action_type"].as_str().unwrap())
        .collect();
    assert_eq!(decided, vec!["spawn_subagent", "spawn_subagent", "done"]);
    // …and the INPUT each one decided on: nothing on the first tick, then what the dispatcher
    // reported back. The excerpt is the whole value here (far under the cap), with its true size.
    assert!(
        rows[0].get("last_result_excerpt").is_none(),
        "the opening tick decided on no input: {:#?}",
        rows[0]
    );
    assert_eq!(
        rows[1]["last_result_excerpt"],
        json!("{\"diff\":\"attempt 1\"}")
    );
    assert_eq!(rows[1]["last_result_bytes"], json!(20));

    // The correlations an auditor navigates by, on every row.
    for row in &rows {
        assert_eq!(row["run_id"], json!(run_id));
        assert_eq!(row["spec_id"], json!(spec_id));
        assert!(row.get("parent_run_id").is_none(), "a top-level run");
    }
    // A stop on a leaf names its node; the END is un-cursored and says so by omission.
    assert_eq!(rows[0]["node_type"], json!(db::NODE_TYPE_SPAWN_SUBAGENT));
    assert_eq!(rows[0]["status"], json!(db::RUN_STATUS_RUNNING));
    assert!(rows[2].get("node_id").is_none(), "the run ended un-cursored");
    assert_eq!(rows[2]["status"], json!(db::RUN_STATUS_DONE));

    // The Spec activity feed is UNTOUCHED by the journal: it filters on the `spec`/`task`
    // aggregates, and a tick payload carries a `spec_id` that would otherwise match it whole.
    let feed = db.with_conn(|c| db::recent_domain_events_for_spec(c, &spec_id, 50)).unwrap();
    assert!(
        feed.iter().all(|e| e.event_type != db::EVENT_RUN_TICK),
        "the Spec fil shows the Spec's life, not the engine's ticks"
    );
}

/// F3 (the BOUND) — a journal row stays small whatever the tick carried. The result an effector
/// reports back is a WHOLE DIFF: the row keeps the action's `type` and an EXCERPT of the input,
/// capped, with the REAL byte size of the full value beside it. The outbox is an audit trail, not a
/// log sink. (The other half of the old bound — the forwarded manual — is gone from the action
/// itself now, so there is nothing left to keep out of the row on that side.)
#[test]
fn a_run_tick_row_carries_an_excerpt_of_the_last_result_never_the_whole() {
    let _g = db_guard();
    let sandbox = temp_dir("f3-bound");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
    });

    let rt = EngineRuntime::new();
    let spawn = tick(&db, &rt, &run_id, None);
    assert!(
        spawn["action"].get("context").is_none(),
        "the action itself no longer carries a manual: {}",
        spawn["action"],
    );

    // A 50k-char report — an effector's entire diff — reported back into the closing tick.
    let diff = "+".repeat(50_000);
    tick(&db, &rt, &run_id, Some(json!({ "diff": diff })));

    let rows = tick_rows(&db, &run_id);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["action_type"], json!("spawn_subagent"));
    assert!(rows[0].get("context").is_none(), "no `context` key at all");

    let excerpt = rows[1]["last_result_excerpt"].as_str().expect("an excerpt");
    assert!(
        excerpt.ends_with("…[truncated]"),
        "a cut excerpt says so in its own tail ({} chars)",
        excerpt.chars().count()
    );
    assert_eq!(
        excerpt.chars().count(),
        2_000 + "…[truncated]".chars().count(),
        "capped at 2000 chars plus the marker"
    );
    // The size of the FULL value survives the cut — the auditor knows what was there.
    assert_eq!(
        rows[1]["last_result_bytes"].as_u64().unwrap(),
        json!({ "diff": diff }).to_string().len() as u64
    );
    // The whole row stays a business bag: the cap is what makes that true.
    let biggest = rows.iter().map(|r| r.to_string().len()).max().unwrap();
    assert!(biggest < 3_000, "a tick row is bounded: {biggest} bytes");
}

/// F3 — a run that resolves to NO project is not journaled, and the tick still succeeds.
/// `domain_events.project_id` is not nullable and a run bound to neither a Spec nor a workspace has
/// no project to file under; inventing one would put a row nobody can scope in the outbox, and
/// failing the tick would break a legitimate run over its own audit trail.
#[test]
fn a_run_bound_to_no_project_ticks_normally_and_journals_nothing() {
    let _g = db_guard();
    let sandbox = temp_dir("f3-unbound");
    let db = Db::in_memory();
    let _ = seed_project(&db, sandbox.path());

    let (_wf, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
    });

    let rt = EngineRuntime::new();
    assert_eq!(tick(&db, &rt, &run_id, None)["action"]["type"], "spawn_subagent");
    assert_eq!(tick(&db, &rt, &run_id, Some(json!({ "diff": "x" })))["action"]["type"], "done");
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE, "the run ran to the end");
    assert!(tick_rows(&db, &run_id).is_empty(), "nothing to correlate, nothing filed");
}

/// H12 — the Spec-write authorization is a fact about a RUN, checked three ways, and each way is one
/// of the doors it must not leave open: the grant must be on the board (only the engine puts it
/// there, only from a human's answer), the run must still be LIVE (a permission does not outlive the
/// flow it was given for) and it must drive THIS Spec (a grant on one build can never write another).
#[test]
fn the_spec_write_grant_is_run_scoped_live_and_spec_bound() {
    let _g = db_guard();
    let sandbox = temp_dir("h12-grant");
    let db = Db::in_memory();
    let (project, workspace, _root) = seed_project(&db, sandbox.path());
    let spec = spec_in_progress(&db, &project, &workspace);
    let other_spec = spec_draft(&db, &project);

    let (_wf, run_id) = workflow_run(&db, Some(&spec), |db, wf| {
        add_node(db, wf, None, 0, db::NODE_TYPE_NOTE, json!({ "text": "n" }));
    });
    let set_board = |board: Value| {
        db.with_conn(|c| {
            db::update_workflow_run(c, &run_id, None, &board.to_string(), db::RUN_STATUS_RUNNING, None)
        })
        .expect("board");
    };

    // No grant on the board — nothing is authorized, whatever the caller passes.
    set_board(json!({ "blocked_route": "spec" }));
    assert!(
        !spec_section_write_is_authorized(&db, &run_id, &spec),
        "the ROUTE the human took is not the grant — only the engine's own key is"
    );

    // The grant, on the live run driving that Spec: authorized.
    set_board(json!({ crate::engine::SPEC_WRITE_GRANT_KEY: true }));
    assert!(spec_section_write_is_authorized(&db, &run_id, &spec));

    // Same grant, ANOTHER Spec: refused (a build never authorizes a write on a Spec it does not drive).
    assert!(!spec_section_write_is_authorized(&db, &run_id, &other_spec));
    // An unknown run: refused.
    assert!(!spec_section_write_is_authorized(&db, "no-such-run", &spec));

    // The run FINISHES: the permission dies with it, grant or no grant.
    db.with_conn(|c| {
        db::update_workflow_run(
            c,
            &run_id,
            None,
            &json!({ crate::engine::SPEC_WRITE_GRANT_KEY: true }).to_string(),
            db::RUN_STATUS_DONE,
            None,
        )
    })
    .expect("terminal");
    assert!(
        !spec_section_write_is_authorized(&db, &run_id, &spec),
        "a permission does not outlive the run it was granted to"
    );
}

// ---------------------------------------------------------------------------
// H7 — the `nyx_next` DESCRIPTOR is bound to the payloads the engine really builds
// ---------------------------------------------------------------------------

/// The `nyx_next` tool description EVERY dispatcher reads, from the live `tools/list` surface.
fn nyx_next_description() -> String {
    crate::mcp::tool_descriptors()
        .into_iter()
        .find(|d| d["name"] == NYX_NEXT_TOOL)
        .expect("nyx_next must be advertised")["description"]
        .as_str()
        .expect("a description")
        .to_string()
}

/// ONE sample of EVERY [`Action`] the engine can yield, each with its optional fields POPULATED (so
/// the payloads below carry their maximal key set — a field absent from a sparse sample would read as
/// a field the engine never emits).
fn every_action() -> Vec<Action> {
    vec![
        Action::SpawnSubagent {
            node_id: "n".into(),
            agent_id: "coder".into(),
            label: "implement".into(),
            inline: false,
            subject: Some("Commitments".into()),
        },
        Action::GateCheck {
            node_id: "n".into(),
            command_ids: vec!["cmd".into()],
            slot: Some("build_check".into()),
            blocking: true,
        },
        Action::Judge {
            node_id: "n".into(),
            prompt: "is it clear?".into(),
            output_schema: Some(json!({ "type": "object" })),
        },
        Action::Transition {
            node_id: "n".into(),
            target_state: "in_review".into(),
            gate_on: Some("approved".into()),
        },
        Action::Query {
            node_id: "n".into(),
            query: "spec_status".into(),
            write: "status".into(),
            target: Some("ready".into()),
            task_key: Some("task".into()),
            root_key: Some("worktrees_root".into()),
            name_key: Some("workspace_name".into()),
        },
        Action::ForkSpec {
            node_id: "n".into(),
            gate_on: Some("approved".into()),
        },
        Action::TransitionTask {
            node_id: "n".into(),
            task_id: "t".into(),
            transition: "done".into(),
            reason: Some("shipped".into()),
            gate_on: Some("approved".into()),
        },
        Action::CreateWorkspace {
            node_id: "n".into(),
            root: "D:/work".into(),
            name: "NYX-42".into(),
            gate_on: Some("approved".into()),
        },
        Action::Wait {
            node_id: "n".into(),
        },
        Action::Note {
            node_id: "n".into(),
            text: "no Spec yet".into(),
            clear_gate_block: true,
            revoke_spec_write: true,
        },
        Action::AskHuman {
            node_id: "n".into(),
            prompt: "ship it?".into(),
            options: vec!["yes".into(), "no".into()],
        },
        Action::Produce {
            node_id: "n".into(),
        },
        Action::SubWorkflow {
            node_id: "n".into(),
            scope: "coder".into(),
            workflow_id: Some("wf".into()),
            inject: vec![crate::engine::BoardCopy {
                from: "task".into(),
                to: "task".into(),
            }],
            retrieve: vec![crate::engine::BoardCopy {
                from: "gate_green".into(),
                to: "gate_green".into(),
            }],
            output_key: Some("sub_result".into()),
        },
    ]
}

/// Every WIRE form an action can travel in — the truth the descriptor is checked against. The match is
/// EXHAUSTIVE over [`Action`] on purpose: the day a leaf is added, THIS fails to compile until its wire
/// form is stated, and the assertions below then confront the descriptor with it.
fn wire_forms(action: &Action) -> Vec<Value> {
    match action {
        // `sub_workflow` never travels as [`action_json`] renders it: the leaf is resolved INSIDE the
        // tick (the child run is opened) and what the dispatcher gets is the child-driving wait.
        Action::SubWorkflow { .. } => vec![subworkflow_wait_json("child-run")],
        // An `ask_human` travels in FOUR forms: the plain one, the THREADED one it takes when it
        // bubbles up from a child run (three fields more), and one per COMMITMENT its answer can
        // carry (two fields more, H6/J2) — same type, same contract, one warning naming what is at
        // stake.
        Action::AskHuman { prompt, options, .. } => vec![
            action_json(action, "claude"),
            ask_human_json(prompt, options, Some(("child-run", "child-node")), None),
            ask_human_json(prompt, options, None, Some(crate::engine::HumanCommitment::SpecTransition)),
            ask_human_json(prompt, options, None, Some(crate::engine::HumanCommitment::AnotherRound)),
        ],
        // nyx's OWN inline effects: nyx performs them itself inside the tick, so their `action_json`
        // (kept only to make the match total) never reaches a dispatcher. Their wire form is the type
        // ALONE — the descriptor names them so a dispatcher knows not to wait for one, and documents no
        // field of theirs, because none is ever handed over.
        Action::GateCheck { .. }
        | Action::Transition { .. }
        | Action::Query { .. }
        | Action::ForkSpec { .. }
        | Action::TransitionTask { .. }
        | Action::CreateWorkspace { .. }
        | Action::Note { .. } => vec![json!({ "type": action_json(action, "claude")["type"] })],
        Action::SpawnSubagent { .. } | Action::Judge { .. } | Action::Wait { .. } | Action::Produce { .. } => {
            vec![action_json(action, "claude")]
        }
    }
}

/// Whether `description` NAMES `wire_type` as a whole word (so `transition` is not found inside
/// `transition_task`, nor `note` inside `notes`).
fn names_type(description: &str, wire_type: &str) -> bool {
    type_mentions(description, wire_type).next().is_some()
}

/// The byte offsets at which `description` mentions `wire_type` as a whole word.
fn type_mentions<'a>(
    description: &'a str,
    wire_type: &'a str,
) -> impl Iterator<Item = usize> + 'a {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    description
        .match_indices(wire_type)
        .filter(move |(at, _)| {
            let before = description[..*at].chars().next_back();
            let after = description[at + wire_type.len()..].chars().next();
            !before.is_some_and(word) && !after.is_some_and(word)
        })
        .map(|(at, _)| at)
}

/// The fields the descriptor DOCUMENTS for `wire_type`: the names in the `[…]` list that IMMEDIATELY
/// follows the type name (`spawn_subagent[agent_id, label, …]`), unioned over every mention — a type
/// mentioned without a list documents no field. A trailing `?` is stripped: it marks a field the graph
/// may leave out, and the guard is about WHICH fields exist, not when they do.
fn documented_fields(description: &str, wire_type: &str) -> std::collections::BTreeSet<String> {
    let mut fields = std::collections::BTreeSet::new();
    for at in type_mentions(description, wire_type) {
        let rest = &description[at + wire_type.len()..];
        let Some(list) = rest.strip_prefix('[') else {
            continue;
        };
        let list = &list[..list.find(']').expect("an unterminated field list in the descriptor")];
        for field in list.split(',') {
            let field = field.trim().trim_end_matches('?').trim();
            if !field.is_empty() {
                fields.insert(field.to_string());
            }
        }
    }
    fields
}

/// The keys `forms` carry, minus `type` (the discriminant, not a field) — what the descriptor must
/// document for that action type, no more and no less.
fn emitted_fields(forms: &[Value]) -> std::collections::BTreeSet<String> {
    forms
        .iter()
        .flat_map(|f| f.as_object().expect("an action object").keys().cloned())
        .filter(|k| k != "type")
        .collect()
}

/// H7 — THE GUARD. The `nyx_next` descriptor is the only thing every dispatcher reads, and its failure
/// mode is DRIFT: it went on telling agents to paste a spawn's `context` VERBATIM for a whole release
/// after G7 dissolved the role manuals and removed the field (an agent following it hunted for
/// something that did not exist), while `query`, `transition`, `sub_workflow`, `fork_spec` and
/// `transition_task` were missing from the list entirely. A sentence cannot promise not to drift, so
/// this binds it to the code: every action type the engine can emit must be NAMED, with EXACTLY the
/// fields its real payload carries.
///
/// Three independent teeth, so a new leaf cannot land quietly: [`wire_forms`] is exhaustive over
/// [`Action`] (a new variant fails to COMPILE), [`every_action`] is checked against
/// [`db::NODE_TYPES`] (a new leaf node type with no sample fails HERE), and each sample's payload is
/// diffed against the descriptor (a renamed or added field fails HERE).
#[test]
fn nyx_next_descriptor_states_exactly_the_actions_the_engine_emits() {
    let description = nyx_next_description();
    let mut documented: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for action in every_action() {
        let forms = wire_forms(&action);
        let wire_type = forms[0]["type"].as_str().expect("a wire type").to_string();
        assert!(
            names_type(&description, &wire_type),
            "the descriptor never names the '{wire_type}' action the engine can emit — a dispatcher \
             cannot perform (or knowingly ignore) what it was never told about: {description}"
        );
        assert_eq!(
            documented_fields(&description, &wire_type),
            emitted_fields(&forms),
            "the descriptor's fields for '{wire_type}' are not the ones the engine emits \
             ({forms:?}) — a field it invents sends an agent hunting for something that does not \
             exist, and one it omits is one nobody uses"
        );
        documented.insert(wire_type);
    }

    // `done` is not an [`Action`] — it is [`Step::Done`], the end of the walk — but it IS a `type` the
    // dispatcher receives, and the loop only stops on it.
    assert!(names_type(&description, "done"), "the descriptor must name 'done': {description}");
    assert_eq!(
        documented_fields(&description, "done"),
        std::collections::BTreeSet::new(),
        "`done` carries no field of its own"
    );

    // The sample list is COMPLETE: every LEAF node type the DB knows (the authoring layer validates
    // against this very list, so a new leaf must appear in it) produced a wire form above.
    for node_type in db::NODE_TYPES {
        let composite = crate::engine::NodeType::from_db(node_type)
            .expect("a known node type")
            .is_composite();
        if composite {
            continue;
        }
        assert!(
            documented.contains(node_type),
            "the leaf node type '{node_type}' yields an action nothing checked the descriptor \
             against — add it to every_action()"
        );
    }

    // The notes the walk went past ride the NEXT action under `notes` ([`with_notes`]) instead of
    // suspending on one, so the descriptor has to say the word — a dispatcher that ignores the array
    // silently drops the run's own words to the human.
    assert!(
        description.contains("`notes`"),
        "the descriptor must say that notes RIDE the next action: {description}"
    );
}

// ---------------------------------------------------------------------------
// M4 — an edit made mid-run reaches the VERY NEXT tick of that run
// ---------------------------------------------------------------------------

/// THE PROMISE, KEPT. `node_update` is the one node mutation allowed under a live run, and it tells the
/// caller the run "will execute the edited node from its very next tick". It did not: the session's
/// [`EngineRuntime`] held the tree — decoded configs included — as it stood at the run's first tick, so
/// the edit reached nothing until a later session rebuilt from the persisted traversal.
///
/// That is load-bearing since a blocking `gate_check` with no `command_ids` REFUSES to advance (H10):
/// the route out the refusal itself prescribes is `add_command` then `node_update … command_ids`, on
/// the run that is parked. If the edit does not reach the walk, the caller does exactly what they were
/// told, calls again, and gets the same refusal forever.
///
/// So: park a run on an unarmed blocking gate, arm it through the REAL `node_update` verb, and tick the
/// SAME run on the SAME runtime — the gate must now RUN the command and the run advance.
#[test]
fn arming_a_gate_mid_run_reaches_the_very_next_tick_of_that_run() {
    let _g = db_guard();
    let sandbox = temp_dir("live-edit");
    let db = Db::in_memory();
    let (project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());

    // A blocking gate with NO command — the graph as a caller first authored it.
    let (workflow_id, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [], "blocking": true }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_NOTE,
            json!({ "text": "the build was verified" }),
        );
    });
    let gate_node = db.with_conn(|c| {
        db::list_nodes(c, &workflow_id)
            .unwrap()
            .into_iter()
            .find(|n| n.type_ == db::NODE_TYPE_GATE_CHECK)
            .expect("the gate")
            .id
    });

    // The run is REFUSED at the gate: it verifies nothing, so it certifies nothing.
    let rt = EngineRuntime::new();
    let refused = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id }))
        .expect_err("a blocking gate with no command may not advance");
    assert_eq!(
        refused
            .data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("gate_unconfigured"),
    );

    // The caller walks the route the refusal names, on the run that is parked: register the check,
    // then `node_update` the gate — the real verb, warnings and all.
    let command = gate_command(&db, &project_id, "true");
    let updated = crate::mcp_authoring::dispatch_authoring_tool(
        &db,
        "node_update",
        &json!({
            "node_id": gate_node,
            "config": { "command_ids": [command], "blocking": true },
        }),
    )
    .expect("node_update is routed")
    .expect("node_update succeeds")
    .result;
    // The authoring layer SAYS the live run will execute the edit from its next tick…
    let warned = updated["warnings"]
        .as_array()
        .expect("a warnings array")
        .iter()
        .find(|w| w["run_id"] == json!(run_id.clone()))
        .expect("the walking run is named");
    assert!(
        warned["message"]
            .as_str()
            .unwrap()
            .contains("very next tick"),
        "the warning promises the next tick: {warned}"
    );

    // …and it does: the SAME session, the SAME runtime, the next tick runs the armed gate GREEN and the
    // run walks past it to the end.
    let advanced = tick(&db, &rt, &run_id, None);
    assert_eq!(
        advanced["action"]["type"], "done",
        "the armed gate ran and the run advanced: {advanced}"
    );
    assert_eq!(
        advanced["blackboard_delta"]["gate_green"],
        json!(true),
        "the gate was RUN this tick, not read from the stale tree: {advanced}"
    );
    assert_eq!(
        advanced["action"]["notes"],
        json!(["the build was verified"]),
        "the walk went on past the gate: {advanced}"
    );
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}

/// THE OTHER HALF of re-reading the tree each tick: an edit that MOVES the graph under a walking run
/// must not be adopted. The walk's frames are arena positions (and a sequence frame a sibling cursor),
/// so a reshaped tree would resume it on a node the frames no longer point at — silently, on the wrong
/// child. `node_add` / `node_remove` / `node_reorder` are refused while a run walks, so this is reached
/// only when a graph moved some other way; the tick then REFUSES and says the run must be dropped.
#[test]
fn a_tree_reshaped_under_a_walking_run_refuses_the_tick_instead_of_guessing() {
    let _g = db_guard();
    let sandbox = temp_dir("reshaped");
    let db = Db::in_memory();
    let (_project_id, _workspace_id, _ws) = seed_project(&db, sandbox.path());

    let (workflow_id, run_id) = workflow_run(&db, None, |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(db, wf, Some(&root), 0, db::NODE_TYPE_WAIT, json!({}));
        add_node(db, wf, Some(&root), 1, db::NODE_TYPE_WAIT, json!({}));
    });

    // The run is walking: parked on the first `wait`.
    let rt = EngineRuntime::new();
    assert_eq!(tick(&db, &rt, &run_id, None)["action"]["type"], "wait");

    // The tree GROWS under it — the authoring layer refuses this gesture, so it is done behind its
    // back, exactly as a rogue writer would.
    let root = db.with_conn(|c| {
        db::list_nodes(c, &workflow_id)
            .unwrap()
            .into_iter()
            .find(|n| n.parent_id.is_none())
            .expect("the root")
            .id
    });
    add_node(&db, &workflow_id, Some(&root), 2, db::NODE_TYPE_WAIT, json!({}));

    let refused = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id }))
        .expect_err("a walk cannot be resumed over a tree that moved");
    assert_eq!(
        refused
            .data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("run_tree_reshaped"),
    );
    assert!(
        refused.message.contains("run_delete"),
        "the refusal names the way out: {}",
        refused.message
    );
}

// ---------------------------------------------------------------------------
// L1 — ONE tick at a time on a run (the MCP surface is threaded now)
// ---------------------------------------------------------------------------

/// L1 — the invariant the single accept thread used to provide BY ACCIDENT: two `nyx_next` calls on
/// the SAME run must not both advance it.
///
/// The gate command is the clock: it announces that it has STARTED and then parks until this test
/// releases it, so the second tick is fired at a moment when the first is PROVABLY inside the run —
/// no sleep, no race to win. Without the tick guard both ticks walk from the same persisted cursor,
/// both run the gate (the `ran` ledger would show two lines) and the last to persist overwrites the
/// other in silence.
#[test]
#[cfg(not(windows))]
fn two_concurrent_ticks_on_one_run_do_not_both_advance_it() {
    let _g = db_guard();
    let sandbox = temp_dir("l1-serialize");
    let db = Db::in_memory();
    let (project_id, workspace_id, ws_path) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    // Appends ONE line per execution (the ledger this test reads), says when it is running, and
    // stays running until `release` appears — so the gate's duration is CONTROLLED, not guessed. It
    // also gives up on its own after a few seconds and exits GREEN, because a build that let the
    // second tick through would park this test's own thread inside a second gate run that nothing
    // would ever release: the test must FAIL on its assertions, not hang.
    let gate = gate_command(
        &db,
        &project_id,
        "sh -c 'echo ran >> ran; touch started; i=0; while [ ! -f release ] && [ $i -lt 250 ]; do \
         sleep 0.02; i=$((i+1)); done; exit 0'",
    );
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [gate], "blocking": true }),
        );
    });

    let rt = EngineRuntime::new();
    let started = Path::new(&ws_path).join("started");
    let release = Path::new(&ws_path).join("release");
    let ran = Path::new(&ws_path).join("ran");

    let refused = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id }))
        });
        // Wait until the gate is PROVABLY running — i.e. the first tick is inside this run.
        for _ in 0..1000 {
            if started.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(started.exists(), "the gate command must have started");

        // THE second tick, fired while the first still holds the run.
        let refused = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id }))
            .expect_err("a run already being ticked cannot be ticked again");

        std::fs::write(&release, "go").expect("release the gate");
        first.join().expect("the first tick's thread").expect("the first tick advances the run");
        refused
    });

    assert_eq!(
        refused
            .data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("run_busy"),
    );
    assert!(
        refused.message.contains("NOTHING was changed"),
        "the refusal says the call was a no-op, so the caller knows to simply retry: {}",
        refused.message
    );

    // The gate ran ONCE: the refused tick neither re-ran the effect nor re-walked the graph.
    let ledger = std::fs::read_to_string(&ran).expect("the gate ledger");
    assert_eq!(
        ledger.lines().count(),
        1,
        "the gate must have run exactly once — a second tick walked the same cursor: {ledger:?}"
    );
    // And the run advanced exactly once, to its end.
    assert_eq!(run_row(&db, &run_id).status, db::RUN_STATUS_DONE);
}

/// L1 — the refusal is a NO-OP on the run, and the run is tickable again the moment the tick that
/// held it returns. A guard that leaked would wedge a run forever, which is a worse outcome than the
/// freeze it replaces.
#[test]
fn a_refused_concurrent_tick_changes_nothing_and_releases_the_run() {
    let _g = db_guard();
    let sandbox = temp_dir("l1-noop");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
    });
    let rt = EngineRuntime::new();
    let before = run_row(&db, &run_id);

    // A tick is inside this run.
    let held = rt.enter_tick(&run_id).expect("the run is free");
    let refused = super::nyx_next(&db, &rt, no_session(), &json!({ "run_id": run_id }))
        .expect_err("the run is busy");
    assert_eq!(refused.code, "invalid_state");
    let after = run_row(&db, &run_id);
    assert_eq!(after.current_node_id, before.current_node_id);
    assert_eq!(after.blackboard, before.blackboard);
    assert_eq!(after.traversal, before.traversal);
    assert_eq!(after.status, before.status);

    // The tick returns; the run is free again and the SAME call now advances it.
    drop(held);
    assert_eq!(
        tick(&db, &rt, &run_id, None)["action"]["type"],
        "spawn_subagent"
    );
}

// ---------------------------------------------------------------------------
// L1 — a gate CUT SHORT by its budget is not a red quality verdict
// ---------------------------------------------------------------------------

/// L1 — the two ways a gate fails to go green are DIFFERENT FACTS, and the refusal must not conflate
/// them.
///
/// A non-zero exit is a VERDICT: the check ran and the work did not pass. A timeout is the ABSENCE of
/// a verdict: nothing was judged, the clock ran out. Told "your gate is RED", a session goes looking
/// for a defect in code the gate never finished reading — and the real cause (a command that does not
/// terminate, or a suite that needs more room) goes unnamed.
///
/// Both outcomes here are produced by the REAL gate runner over a REAL PTY; only the total budget is
/// shortened, so the timeout is reached in milliseconds instead of the configured 90s.
#[test]
#[cfg(not(windows))]
fn a_gate_cut_short_by_its_budget_is_refused_as_cut_short_not_as_red() {
    let _g = db_guard();
    let sandbox = temp_dir("l1-cutshort");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // A command that never exits on its own, cut off by the gate's TOTAL budget.
    let hangs = gate_command(&db, &project_id, "sh -c 'sleep 60'");
    let cut_short = super::run_gate_bounded(&db, &[hangs], std::time::Duration::from_millis(300))
        .expect("the gate runs");
    assert!(!cut_short.passed);
    assert!(
        cut_short.commands[0].timeout.is_some() && cut_short.commands[0].exit_code.is_none(),
        "a cut-short command has a timeout marker and no exit code, got {:?}",
        cut_short.commands[0]
    );

    // A command that RAN and failed: the same red, an entirely different fact.
    let fails = gate_command(&db, &project_id, "sh -c 'echo boom; exit 1'");
    let failed = super::run_gate_bounded(&db, &[fails], super::GATE_TOTAL_TIMEOUT).expect("gate");
    assert!(!failed.passed);
    assert_eq!(failed.commands[0].exit_code, Some(1));
    assert!(failed.commands[0].timeout.is_none());

    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_GATE_CHECK,
            json!({ "command_ids": [], "blocking": false }),
        );
    });
    let run = run_row(&db, &run_id);
    let gate_node = "gate-node";

    // The refusal the dispatcher reads, for each of the two outcomes.
    let refuse_with = |outcome: super::GateOutcome| {
        let mut engine = super::build_engine(&db, &run).expect("engine");
        engine.set_board(
            super::LAST_GATE_RESULT_KEY,
            super::gate_result_json(gate_node, outcome),
        );
        super::refuse_gate(&db, &run, &mut engine, gate_node, None)
    };

    let cut = refuse_with(cut_short);
    assert!(
        cut.message.contains("CUT SHORT") && cut.message.contains("NO verdict"),
        "a gate stopped by the clock is reported as cut short: {}",
        cut.message
    );
    assert!(
        !cut.message.contains("is RED"),
        "and never as a failing check: {}",
        cut.message
    );
    assert_eq!(
        cut.data.as_ref().and_then(|d| d.get("cut_short")),
        Some(&json!(true)),
        "the same fact, machine-readable, beside the message",
    );

    let red = refuse_with(failed);
    assert!(
        red.message.contains("is RED"),
        "a check that RAN and failed is still reported as red: {}",
        red.message
    );
    assert_eq!(
        red.data.as_ref().and_then(|d| d.get("cut_short")),
        Some(&json!(false)),
    );
    // The ENFORCEMENT is the same either way — neither advances the run.
    assert_eq!(cut.code, "invalid_state");
    assert_eq!(red.code, "invalid_state");
}

/// L1 — `run_delete` and a walking tick contend for the SAME run, and the pool is what lets them
/// overlap: a tick holds its run for as long as its gate takes, and dropping a live run is precisely
/// what nyx's own refusals tell a caller to do about one. Deleting the row underneath the tick would
/// make the tick's `persist` match nothing while nyx has already run the gate and imposed the
/// transition — so `run_delete` takes the tick guard too, and refuses while a tick holds it.
#[test]
fn run_delete_refuses_a_run_a_tick_is_walking_and_drops_its_engine_after() {
    let _g = db_guard();
    let sandbox = temp_dir("l1-rundelete");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
    });
    let rt = EngineRuntime::new();
    // A tick parks the run on its spawn leaf, so the run has a resident engine in the registry.
    assert_eq!(
        tick(&db, &rt, &run_id, None)["action"]["type"],
        "spawn_subagent"
    );

    // A tick is inside the run — the delete must not land.
    let held = rt.enter_tick(&run_id).expect("the run is free between ticks");
    let refused =
        crate::mcp_authoring::run_delete(&db, &rt, &json!({ "run_id": run_id })).unwrap_err();
    assert_eq!(refused.code, "invalid_state");
    assert_eq!(
        refused
            .data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("run_busy"),
    );
    assert!(
        refused.message.contains("NOTHING was deleted"),
        "the refusal says the run is untouched: {}",
        refused.message
    );
    assert!(
        db.with_conn(|c| db::get_workflow_run(c, &run_id))
            .unwrap()
            .is_some(),
        "the run is still there — a refused delete deletes nothing",
    );

    // Once the tick returns, the delete goes through AND takes the resident engine with it.
    drop(held);
    crate::mcp_authoring::run_delete(&db, &rt, &json!({ "run_id": run_id }))
        .expect("the run is free now");
    assert!(db
        .with_conn(|c| db::get_workflow_run(c, &run_id))
        .unwrap()
        .is_none());
    assert!(
        !rt.session_run_ids().contains(&run_id),
        "the deleted run's engine left the registry with its row",
    );
}

/// L1 — a tick whose run VANISHED under it fails LOUDLY. The walk really happened (nyx ran the gate,
/// imposed the transition); persisting nothing and returning the action anyway would hand the
/// dispatcher a step to perform on a run that no longer exists. `run_delete` can no longer be the
/// cause, but a cascade from elsewhere still can, and this is where it stops being silent.
#[test]
fn a_tick_whose_run_vanished_under_it_reports_it_instead_of_persisting_nothing() {
    let _g = db_guard();
    let sandbox = temp_dir("l1-vanished");
    let db = Db::in_memory();
    let (project_id, workspace_id, _ws) = seed_project(&db, sandbox.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            json!({ "agent_id": "coder" }),
        );
    });
    let run = run_row(&db, &run_id);
    let engine = super::build_engine(&db, &run).expect("engine");

    // The row disappears behind the tick's back (a cascade, not the guarded verb).
    db.with_conn(|c| db::delete_workflow_run(c, &run_id)).unwrap();

    let err = super::persist(
        &db,
        &run,
        &engine,
        None,
        db::RUN_STATUS_RUNNING,
        &super::tick_trace(Some("spawn_subagent"), None),
    )
    .expect_err("persisting onto a deleted run is not a success");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("run_vanished"),
    );
    assert!(err.message.contains(&run_id), "it names the run: {}", err.message);
}

/// B3 — THE GATE READS THE WHOLE ANSWER, and that is the only correct reading. `validate_tree`
/// requires `options` only of an `ask_human` whose written key is read by a `selector` `cond` / a
/// `loop` `until` (rule (5) / J1) — never of one read by a `gate_on` — so a gated question may
/// legitimately take FREE HUMAN TEXT, where a first word is the opening of a sentence and not a
/// vocabulary. An answer that opens with an affirmative and then takes it back approves NOTHING, and
/// so does a rider the graph never authored: absent, `false`, a refusal, a structured value, a
/// sentence. Widen this test to a first word again and the family below goes green on answers that
/// say no.
#[test]
fn a_gate_reads_the_whole_answer_so_one_that_only_opens_with_yes_approves_nothing() {
    for no in [
        // A rider on a yes — the shape /nspec's third answer has. It is ROUTED by a selector
        // comparing the whole answer, never read as an approval.
        json!("yes with workspace"),
        json!("yes, with a dev workspace"),
        // A yes taken back, at every length.
        json!("yes, but not before the tests pass"),
        json!("ok, actually no"),
        json!("go back"),
        json!("go back, I changed my mind"),
        json!("ready? not yet"),
        json!("yes... no"),
        json!("later yes"),
        // Plain refusals, and everything that is not a word at all.
        json!("no"),
        json!("no with workspace"),
        json!("workspace"),
        json!(false),
        json!(""),
        json!({ "decision": "yes" }),
    ] {
        assert!(
            !super::board_value_is_affirmative(Some(&no)),
            "{no} approves nothing"
        );
    }
    // The plain affirmatives approve — trimmed and case-folded, exactly as at HEAD.
    for yes in [
        json!("yes"),
        json!("Yes"),
        json!("  Yes "),
        json!("ok"),
        json!("go"),
        json!("approve"),
        json!(true),
    ] {
        assert!(
            super::board_value_is_affirmative(Some(&yes)),
            "{yes} is an approval"
        );
    }
    assert!(
        !super::board_value_is_affirmative(None),
        "an absent answer approves nothing"
    );
}

/// B3, END TO END — the graph the finding describes, run for real: `sequence[ ask_human (NO options)
/// → approved, transition(ready, gate_on: approved) ]`. It passes `validate_tree`, so nyx must be
/// right about it at RUNTIME: the human answers "ok, actually no", which folds onto the gate key, and
/// the engaging transition is REFUSED — the Spec never leaves draft. Wire the first-word widening
/// back onto free gates and this test goes red on the Spec's status.
#[test]
fn a_free_form_gate_refuses_a_transition_when_the_answer_only_starts_affirmative() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("free-form-gate").path());
    let spec_id = spec_draft(&db, &project_id);
    let (_wf, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            // NO `options`: a question the graph leaves free, which authoring allows because only a
            // `gate_on` reads the key it writes.
            json!({ "prompt": "mark the Spec ready?", "output_key": "approved" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "ready", "gate_on": "approved" }),
        );
    });
    let session = Session::new("term-free-form");
    let rt = EngineRuntime::new();

    session.human_says(&db, "let us look at the spec");
    assert_eq!(
        session.tick(&db, &rt, &run_id, None)["action"]["type"],
        "ask_human"
    );
    session.human_says(&db, "ok, actually no");

    let err = session.tick_err(&db, &rt, &run_id, Some(json!("ok, actually no")));
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("engaging_transition_ungated"),
        "an answer that opens with yes and then says no gates nothing: {}",
        err.message
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_DRAFT,
        "the Spec never moved"
    );
}

/// B3, END TO END — THE GRAPH A GATE MUST BE RIGHT ABOUT, and the one an author writes without
/// meaning to: TWO `ask_human` with NO `output_key`, so both land on the SHARED default key
/// (`engine::DEFAULT_ASK_HUMAN_OUTPUT_KEY` = `human`), the FIRST carrying `options` and the SECOND
/// free, then `transition {ready, gate_on: "human"}`.
///
/// AUTHORING NOW REFUSES IT (finding `01M0C048HM5CWJBX0JB6K46D73`, condition (iii) of
/// [`crate::mcp_authoring::sensitive_node_gate`]: the gated key must have exactly ONE writer). It used
/// to be accepted, and this test asserted so — with reason, since nothing about the shape is exotic.
/// What the rule now says out loud is what this very test demonstrated: with two writers on `human`,
/// "the ask_human that gates this transition" has no answer. The old rule picked the FIRST question
/// ("pick a mode"), while the value the gate actually reads is whatever the SECOND one left there.
///
/// THE RUNTIME HALF IS UNCHANGED AND STILL ASSERTED, because authoring is not the only door: rows
/// predating the rule, a seeded tree, `db::create_node` under a test — all reach `drive` with this
/// shape. So the run is walked anyway: the human picks `fast`, then answers the FREE question "ok,
/// actually no", and the engaging transition is REFUSED. The `options` of the FIRST question
/// constrain NOTHING the second one wrote, so a gate that tried to DERIVE "this key only ever holds
/// words the graph authored" would read "ok" here and take the Spec `ready` on a NO. It derives
/// nothing: it compares the whole answer.
#[test]
fn two_questions_on_the_default_key_do_not_lend_the_free_one_a_closed_vocabulary() {
    let _g = db_guard();
    let db = Db::in_memory();
    let (project_id, _ws, _cwd) = seed_project(&db, temp_dir("shared-default-key").path());
    let spec_id = spec_draft(&db, &project_id);
    let (workflow_id, run_id) = workflow_run(&db, Some(&spec_id), |db, wf| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            // A CLOSED question — and NO `output_key`, so it writes `human`.
            json!({ "prompt": "pick a mode", "options": ["fast", "careful"] }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            1,
            db::NODE_TYPE_ASK_HUMAN,
            // A FREE question — and NO `output_key` either, so it writes the SAME key.
            json!({ "prompt": "mark the Spec ready?" }),
        );
        add_node(
            db,
            wf,
            Some(&root),
            2,
            db::NODE_TYPE_TRANSITION,
            json!({ "target_state": "ready", "gate_on": "human" }),
        );
    });

    // (a) AUTHORING REFUSES IT NOW — two writers on `human`, so no question owns the gated key. The
    //     refusal NAMES that condition rather than merely failing.
    let nodes = db.with_conn(|c| db::list_nodes(c, &workflow_id)).unwrap();
    let refusal = crate::mcp_authoring::validate_tree(&nodes, None)
        .expect_err("two ask_human sharing the default key no longer gate anything");
    assert!(
        refusal.contains("not HUMAN-GATED") && refusal.contains("NOTHING ELSE"),
        "the refusal says the gated key has a second writer: {refusal}"
    );

    let session = Session::new("term-shared-default-key");
    let rt = EngineRuntime::new();

    session.human_says(&db, "let us look at the spec");
    assert_eq!(
        session.tick(&db, &rt, &run_id, None)["action"]["type"],
        "ask_human"
    );
    // The CLOSED question is answered from its own vocabulary — nothing off-list is involved.
    session.human_says(&db, "fast");
    assert_eq!(
        session.tick(&db, &rt, &run_id, Some(json!("fast")))["action"]["type"],
        "ask_human",
        "the second question is asked in turn"
    );

    // (b) The FREE question is answered NO, at length. The value now on `human` is free human text.
    session.human_says(&db, "ok, actually no");
    let err = session.tick_err(&db, &rt, &run_id, Some(json!("ok, actually no")));
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("engaging_transition_ungated"),
        "the first question's `options` constrain nothing the SECOND one wrote: {}",
        err.message
    );
    assert_eq!(
        spec_status(&db, &spec_id),
        db::SPEC_STATUS_DRAFT,
        "the Spec never moved"
    );
}

/// FINDING `01M0BN0R0Q0VXK9JM7TGXX9D7W` — A TICK THAT OPENS A WORKSPACE SAYS THE WORKSPACE POOL
/// CHANGED. The pin is on the GRAPH path, which is the one that had no voice at all.
///
/// THE TWO ROADS TO THE SIDEBAR, AND WHY ONLY ONE WAS PAVED. The `create_workspace` MCP TOOL is
/// reached by the sidecar through `mcp_runtime::dispatch_workspace_tool`, which injects the shell's
/// `PushEventSink`; the use case emits `workspace.created` after its transaction commits, the sink
/// turns it into the unsolicited `workspaces://changed`, and the sidebar re-pulls. A `create_workspace`
/// NODE calls the very same tool — but from inside a `nyx_next` tick, where no shell sink reaches, so
/// it passes a `NullEventSink` and that push never happens. The graph path's own channel to the shell
/// is the tick's `ToolOutcome::effects`, which the sidecar drains into one `<topic>://changed` per
/// entry — and it named `Specs` and only `Specs`, while the sidebar listens to `workspaces://changed`
/// and to nothing else. Shipped on 2026-08-18: `go` created DEPOT-6, anchored it, wrote the row and
/// made the folder, and the sidebar stayed empty until the app was relaunched. A workspace that is
/// created and invisible is indistinguishable from one that was never created.
///
/// BOTH HALVES ARE ASSERTED HERE, because "the effects contain `Workspaces`" would also hold on a
/// tick that named it unconditionally, which would be its own lie (every tick asking the sidebar to
/// re-pull). So: the workspace tick names BOTH topics, and a tick of the SAME shape whose node
/// mutates nothing outside the run names `Specs` ALONE.
#[test]
fn a_create_workspace_tick_names_the_workspaces_topic_the_sidebar_re_pulls_on() {
    let _g = db_guard();
    let sandbox = temp_dir("ws-effect-project");
    let db = Db::in_memory();
    let (project_id, _root_ws, _root_path) = seed_project(&db, sandbox.path());
    let spec_id = spec_ready(&db, &project_id);

    // (a) THE WORKSPACE TICK. One `create_workspace` node, UNGATED, reading the worktrees root the
    //     board carries — the shape the `go` branch of `nspec` reaches with the root already resolved.
    let roots = temp_dir("ws-effect-root");
    let workflow_id = db.with_conn(|c| {
        db::create_workflow(c, None, "ws-effect", db::WORKFLOW_SCOPE_CUSTOM, None)
            .expect("workflow")
            .id
    });
    add_node(
        &db,
        &workflow_id,
        None,
        0,
        db::NODE_TYPE_CREATE_WORKSPACE,
        json!({}),
    );
    let board =
        json!({ crate::engine::DEFAULT_WORKTREES_ROOT_KEY: roots.path().to_string_lossy() })
            .to_string();
    let run_id = db
        .with_conn(|c| db::create_workflow_run(c, &workflow_id, Some(&spec_id), None, &board))
        .expect("run")
        .id;

    let out = super::nyx_next(
        &db,
        &EngineRuntime::new(),
        no_session(),
        &json!({ "run_id": run_id }),
    )
    .expect("nyx_next");

    // The effect really landed — otherwise the topic below would be a claim about nothing.
    let ws = db
        .with_conn(|c| db::list_workspaces(c, &project_id))
        .expect("workspaces")
        .into_iter()
        .find(|w| w.root_path != crate::pathnorm::normalize(&sandbox.path().to_string_lossy()))
        .expect("the node opened the Spec's dev workspace");
    assert_eq!(
        db.with_conn(|c| db::get_spec(c, &spec_id))
            .unwrap()
            .unwrap()
            .workspace_id
            .as_deref(),
        Some(ws.id.as_str()),
        "and anchored it on the Spec, through the tool's own spec_id"
    );

    assert!(
        out.effects.contains(&ChangedTopic::Workspaces),
        "the tick that WROTE the workspace pool must name it — `workspaces://changed` is the only \
         topic the sidebar re-pulls on, and without it the row exists and nothing on screen knows: \
         {:?}",
        out.effects
    );
    assert!(
        out.effects.contains(&ChangedTopic::Specs),
        "and it still names the run's own domain (the anchor moved onto the Spec): {:?}",
        out.effects
    );

    // (b) THE CONTROL. A tick of the same shape whose node touches nothing outside the run keeps the
    //     narrow topic — the assertion above is about what the tick DID, not a constant widened once.
    let (_wf2, quiet_run) = workflow_run(&db, Some(&spec_id), |db, wf| {
        add_node(
            db,
            wf,
            None,
            0,
            db::NODE_TYPE_NOTE,
            json!({ "text": "nothing" }),
        );
    });
    let quiet = super::nyx_next(
        &db,
        &EngineRuntime::new(),
        no_session(),
        &json!({ "run_id": quiet_run }),
    )
    .expect("nyx_next");
    assert_eq!(
        quiet.effects,
        vec![ChangedTopic::Specs],
        "a tick that opened no workspace does not ask the sidebar to re-pull"
    );
}

/// FINDING `01M0BVZXM49HQ6N9H9V789HP5K` (defect 2) — `project_worktrees_root` RESOLVES THE PROJECT
/// THROUGH THE RUN'S WORKSPACE WHEN THERE IS NO SPEC YET, so a run never asks the user for a path
/// they have already frozen.
///
/// The fact used to read the project through the bound Spec ALONE ([`super::run_project_id`]), and
/// answer `""` the moment `spec_id` was `None`. Two ordinary runs are in that state: a `custom` flow,
/// and a `/nspec` run BEFORE its graph creates the Spec — `/nspec` legitimately starts unbound
/// ([`db::bind_session_run_to_spec`]). Both are started from a cwd inside a registered workspace, and
/// a workspace belongs to exactly one project, so the project was knowable all along. Reading `""`
/// made `worktrees_root_configured` false and routed the seeded selector to the ask_human that
/// demands a worktrees root — one the project had ALREADY set.
///
/// THE WIDENING IS ON THE FACT, NOT ON THE SHARED RESOLVER. `run_project_id` also decides which
/// workflow a `sub_workflow` opens and which commands a slot gate runs; widening it there would have
/// changed what a Spec-less run is ALLOWED TO DO (a project's own `coder` graph instead of the
/// built-in; a blocking gate arming itself where it used to refuse). This fact only READS a project
/// setting, so it resolves the wider way alone — see [`super::run_workspace_project_id`].
#[test]
fn the_worktrees_root_fact_reads_the_project_through_the_runs_workspace_when_no_spec_is_bound() {
    let _g = db_guard();
    let sandbox = temp_dir("worktrees-root-via-workspace");
    let db = Db::in_memory();
    let (project_id, workspace_id, _root_path) = seed_project(&db, sandbox.path());
    // The project FROZE where its dev workspaces go — the setting the user is not to be asked twice.
    db.with_conn(|c| db::set_project_worktrees_root(c, &project_id, Some("D:/work/_worktrees")))
        .expect("the project's worktrees root is configured");

    let build = |db: &Db, wf: &str| {
        let root = add_node(db, wf, None, 0, db::NODE_TYPE_SEQUENCE, json!({}));
        add_node(
            db,
            wf,
            Some(&root),
            0,
            db::NODE_TYPE_QUERY,
            json!({ "query": "project_worktrees_root", "write": "worktrees_root" }),
        );
    };

    // (a) NO SPEC, BUT A WORKSPACE — the case that was wrong. The run reads the configured root, and
    //     the BRANCHABLE half says "configured", so the seeded selector takes the "no question
    //     needed" branch instead of asking.
    let (_wf, run_id) = workflow_run(&db, None, build);
    db.with_conn(|c| db::set_run_workspace(c, &run_id, &workspace_id))
        .expect("the run is parked in a workspace of the project");
    let rt = EngineRuntime::new();
    tick(&db, &rt, &run_id, None);
    let board: Value = serde_json::from_str(&run_row(&db, &run_id).blackboard).unwrap();
    assert_eq!(
        board["worktrees_root"],
        json!("D:/work/_worktrees"),
        "a run with a workspace knows its project, Spec or no Spec"
    );
    assert_eq!(
        board["worktrees_root_configured"],
        json!(true),
        "…so the graph does not ask for a root the project already set"
    );

    // (b) NEITHER SPEC NOR WORKSPACE — nothing resolves a project, and the fact keeps its ONE spelling
    //     of "unset": the empty string (falsy for the branch, interpolating to nothing in a path).
    let (_wf2, orphan_run) = workflow_run(&db, None, build);
    tick(&db, &rt, &orphan_run, None);
    let board: Value = serde_json::from_str(&run_row(&db, &orphan_run).blackboard).unwrap();
    assert_eq!(
        board["worktrees_root"],
        json!(""),
        "no anchor at all → \"\""
    );
    assert_eq!(board["worktrees_root_configured"], json!(false));
}
