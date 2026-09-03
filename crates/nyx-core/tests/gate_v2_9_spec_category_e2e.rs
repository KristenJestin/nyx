//! GATE e2e (v2.9 — « Catégorie unique par Spec (PDR-0017) + rouge-avant-vert contraint par le
//! moteur pour les Specs `bug` », PRD `01KYA6K26JBYETDMDXYGZVVA03`, Phase 9 task
//! `01KYA6VCVMQZ6RJZQD97DXVB3F`) — the *juge de paix* that confronts phases 1 → 8 WITH EACH OTHER.
//!
//! Every earlier phase was verified against itself: the migration against the schema, the graph
//! against its seeder, the discipline text against its snapshot, the gate against its grader. NONE
//! of them was ever run against another, and the failures this Spec is about live exactly there —
//! in the joints. So this file walks the assembly on the REAL BOOT PATH ([`Db::open`], which runs
//! the migration chain, seeds the built-in workflows AND backfills the category vocabulary), on an
//! on-disk database, through the REAL verbs ([`run_start`] / [`nyx_next`] /
//! [`dispatch_authoring_tool`]) and the REAL PTY gate runner.
//!
//! # What is exercised, and why it cannot be a unit test
//!
//!   * `Db::open` is the ONLY caller of `seed_builtin_workflows` + `seed_system_categories`
//!     together. `Db::in_memory` (the unit fixture) runs the migrations and NOTHING else, so every
//!     unit test seeds by hand and none of them can answer "what does a real boot do to a real
//!     database that predates this Spec". Criterion 1 is that question and no other.
//!   * The reproduction sequence spans the seeded `coder` graph (phase 6), the tri-valued gate
//!     (phase 5), the category service (phase 1), the `spec_category` fact (phase 2), the
//!     `set_category` node (phase 3) and the auditor's evidence block (phase 4). A test that stubs
//!     any one of them stops being a test of the assembly.
//!
//! # The gate commands are `node` SCRIPTS, and `node` is this file's one external prerequisite
//!
//! A gate spawns under the platform shell, so a literal command line means whatever that shell
//! makes of it. This file therefore arms every gate with a script it writes itself
//! (`repro_check_command` / `always_green_command`), which is the one spelling `bash -c` and
//! `powershell.exe -Command` read identically — the same move phase 10's CDP script made. Replay it
//! from any shell with `bun run test:core:e2e`.
//!
//! # The `repro_check` slot is bound BY THE FIXTURE, on purpose
//!
//! Phase 7 — arming `repro_check` on nyx-v2 itself — was SKIPPED on the user's decision, so this
//! repository binds nothing to that slot. The fixtures below bind it themselves, exactly as the
//! neighbouring gates have always bound `build_check` ([`gate_v2_8_engine_e2e`]'s `arm_coder_gate`):
//! a slot is per-project data, and a test IS a project. What that costs is stated plainly in the
//! report of this phase — the real-app CDP validation of phase 10 can watch the categorisation and
//! the curation, but not a live red-before-green on nyx-v2 itself, because nyx-v2 has no
//! reproduction command bound.
//!
//! # The done-criteria of the task (verbatim), and where each is proven
//!
//!   1. « Une base revision 22 migre jusqu'aux seeds globaux 24 ; les workflows forkés restent
//!      inchangés et sont signalés. » →
//!      [`a_base_at_revision_22_boots_to_the_global_seeds_at_24_and_a_fork_stays_frozen_and_signalled`]
//!      and [`the_boot_backfills_the_category_vocabulary_of_a_base_that_predates_it`].
//!   2. « Un bug automatisé exécute exactement le même repro-check rouge puis vert, ensuite le
//!      build vert. » → [`an_automated_bug_runs_the_same_repro_check_red_then_green_then_the_build`].
//!   3. « Un timeout, une commande introuvable ou une erreur d'infrastructure ne peut satisfaire le
//!      rouge. » → the three named tests of `mcp_engine`'s `mod tests` (they need the crate-private
//!      budget injection and the crate-private grader), plus
//!      [`an_infrastructure_fault_on_the_reproduction_gate_is_never_a_red`] here, which walks one of
//!      the three through the seeded graph on the real boot path.
//!   4. « Un bug manuel exige décision d'impraticabilité, preuve avant, preuve après et validations
//!      humaines persistées. » → [`a_manual_bug_persists_both_human_observations_and_runs_no_gate`].
//!   5. « Une Spec legacy sans catégorie est classée avant le routage ; une feature ne reçoit pas de
//!      faux warning de repro. » →
//!      [`a_legacy_spec_is_classified_before_the_build_routes_and_a_feature_draws_no_repro_warning`].
//!   6. « Les preuves de toutes les gates restent consultables et sont transmises à l'auditeur. » →
//!      [`the_evidence_of_every_gate_outlives_the_run_and_reaches_the_auditor`].
//!   7. « Les suites Rust et renderer pertinentes sont vertes avant la validation réelle CDP. » →
//!      not a test: the state this file leaves behind. See the phase report.
//!
//! Two further joints nobody owned are proven here because nobody else could: the PROVENANCE of a
//! category minted by an agent versus renamed by a human — the two halves the journal still tells
//! apart now that PDR-0017's proposed/confirmed axis is retired (amendment of 2026-09-02)
//! ([`the_agent_that_mints_and_the_human_that_renames_are_told_apart_in_the_journal`]) and the
//! PRD's explicit ruling that a recategorisation mid-build replays nothing
//! ([`a_recategorization_mid_build_replays_none_of_the_gates_already_crossed`]).
//!
//! And one shape none of the criteria named because nobody had walked it: a phase with MORE THAN
//! ONE open task ([`a_second_task_on_a_bug_spec_does_not_re_run_the_reproduction`]) — the case that
//! turned the reproduction into a per-task obligation and wedged the run on its own proof.

use std::path::{Path, PathBuf};

use diesel::RunQueryDsl;
use nyx_core::command::{CommandRunner, RunState, RunnerSink};
use nyx_core::db::{self, CommandSource, Db, SpecSections, SpecTransition};
use nyx_core::mcp_authoring::dispatch_authoring_tool;
use nyx_core::mcp_engine::{nyx_next, run_start, CallerSession, EngineRuntime};
use serde_json::{json, Value};

// ============================================================================
// Scaffolding — an on-disk temp nyx.db opened through the REAL boot path.
// ============================================================================

/// The runner `agent_session_event` reads the LIVE managed-command instances through when it
/// builds the agent's context line. This gate arms no managed command through it — its gates go
/// through the engine's own PTY runner — so an EMPTY runner is the faithful shape, exactly as in
/// [`gate_v2_5_8_agent_harness_e2e`].
struct NoopSink;
impl RunnerSink for NoopSink {
    fn on_state(&self, _: &str, _: RunState, _: Option<i32>) {}
    fn on_acknowledge(&self, _: &str) {}
    fn on_output(&self, _: &str, _: &[u8]) {}
    fn persist_scrollback(&self, _: &str, _: &str) {}
    fn archive_previous_run(&self, _: &str) {}
    fn clear_output(&self, _: &str) {}
}

fn noop_runner() -> CommandRunner<NoopSink> {
    CommandRunner::new(
        NoopSink,
        portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
    )
}

/// RAII temp dir removed on drop (the pattern every gate e2e in this crate uses). Bind it as `tmp`
/// BEFORE `db`, so locals drop in reverse order and the directory is removed after the pool closes.
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
        "nyx-gate-2-9-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// THE REAL BOOT. `Db::open` runs the migration chain, refuses on schema drift, seeds the five
/// built-in workflows AND backfills the per-project category vocabulary — the four gestures a
/// desktop start performs, in that order, and the only place they happen together.
fn boot(tmp: &TempDir) -> Db {
    Db::open(&tmp.path().join("nyx.db")).expect("Db::open — the boot path")
}

/// A live project whose root workspace is the on-disk sandbox `dir` (so a gate command really runs
/// there and a sentinel file really lands in it). Returns `(project_id, workspace_id, root_path)`.
fn live_project(db: &Db, dir: &Path) -> (String, String, String) {
    db.with_conn(|c| {
        let (project, root) =
            db::create_project(c, "Category", dir.to_str().unwrap(), None).expect("project");
        (project.id, root.id.clone(), root.root_path)
    })
}

/// A Spec driven to `in_progress` and anchored on `workspace_id` — the state `/ndev` picks up.
fn spec_in_progress(db: &Db, project_id: &str, workspace_id: &str) -> String {
    db.with_conn(|c| {
        let (spec, _rev) = db::create_spec(
            c,
            project_id,
            "A defect",
            SpecSections {
                problem: "something is broken".into(),
                solution: "prove it, then repair it".into(),
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

/// POSE the Spec's phase plan — a task may only name a phase the plan already holds once the Spec
/// has left `draft` (K2), and these Specs are `in_progress` by the time they get their work.
fn declare_phases(db: &Db, spec_id: &str, names: &[&str]) {
    let plan: Vec<Value> = names
        .iter()
        .enumerate()
        .map(|(i, name)| json!({ "number": i + 1, "name": name }))
        .collect();
    db.with_conn(|c| db::set_spec_phases(c, spec_id, &Value::Array(plan).to_string()))
        .expect("phase plan");
}

/// ONE open build task on `spec_id`, in the phase the coder child drains.
fn build_task(db: &Db, project_id: &str, spec_id: &str, title: &str) -> String {
    db.with_conn(|c| {
        db::create_task(
            c,
            project_id,
            title,
            db::NewTaskInput {
                spec_id: Some(spec_id.to_string()),
                phase_name: Some("Build".to_string()),
                ..db::NewTaskInput::slice()
            },
        )
    })
    .expect("task")
    .id
}

/// Register a managed command running at the project's root workspace and return the materialized
/// INSTANCE id — the identity a `gate_check` (or a slot binding) references.
///
/// The COMMAND LINE is never a shell built-in: see [`repro_check_command`] and
/// [`always_green_command`], the only two callers, for why.
fn gate_command(db: &Db, project_id: &str, name: &str, cmdline: &str) -> String {
    db.with_conn(|c| {
        let tpl = db::create_template(c, project_id, name, cmdline, None, CommandSource::default())
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
// A gate spawns its command through [`nyx_core::pty::resolve_shell`], i.e. under WHATEVER shell
// this machine offers: `bash -c` on a Unix box or a Git Bash session, `powershell.exe -Command` on
// a plain Windows one. A literal command line therefore does not mean one thing — it means
// whatever the shell of the day makes of it. `true` and `test -f fixed` are POSIX BUILT-INS: under
// PowerShell `true` is not a command at all (non-zero exit → the always-green gate comes out RED),
// and `test -f fixed` never returns zero (→ the green half of the reproduction is unreachable). The
// file was consequently green only when launched from Git Bash — and, since `test:core` runs
// `--lib` and skips `tests/`, most often not launched at all (finding
// `01M1GZA8PCJGJ6SPHSAHWDCJTN`).
//
// The fix is the one phase 10's CDP script already adopted for the same reason: express the check
// as a `node` SCRIPT written into the workspace the gate runs in. `node <file>` is one program with
// one argument — the same sentence under `-c` and under `-Command` — and its exit code is what the
// shell reports either way. What each check MEANS is unchanged, so what the assertions prove is
// unchanged: the reproduction is still red-then-green on the SAME bound command in the SAME cwd,
// still driven by the sentinel the graph's fix spawn creates, and still resolved relative to the
// cwd (`existsSync("fixed")` is `test -f fixed`, letter for letter).
// ----------------------------------------------------------------------------

/// The REPRODUCTION check's script name and command line. RED (exit 1) while the sentinel `fixed`
/// is absent from the working directory, GREEN (exit 0) once the spawn that stands for the fix has
/// created it.
const REPRO_CHECK_SCRIPT: &str = "nyx-repro-check.js";
const REPRO_CHECK_CMDLINE: &str = "node nyx-repro-check.js";

/// The ALWAYS-GREEN check's script name and command line — the stand-in for a build control, and
/// for a reproduction gate that can never answer `expect: red`.
const ALWAYS_GREEN_SCRIPT: &str = "nyx-always-green.js";
const ALWAYS_GREEN_CMDLINE: &str = "node nyx-always-green.js";

/// Write `body` as `name` in `dir` — the workspace the gate will run in, so the relative command
/// line above resolves against the gate's own cwd and nothing else.
fn write_check_script(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("the check script");
}

/// The REPRODUCTION check, armed on `project_id` and materialized in `dir`: the ONE command both
/// halves of the red-before-green resolve to.
fn repro_check_command(db: &Db, project_id: &str, dir: &Path, name: &str) -> String {
    write_check_script(
        dir,
        REPRO_CHECK_SCRIPT,
        // `existsSync("fixed")` is relative to the process's cwd — which is the gate's cwd, i.e.
        // exactly what `test -f fixed` used to read.
        "// The REPRODUCTION check: red while the defect is there, green once it is repaired.\n\
         const fs = require('node:fs');\n\
         const repaired = fs.existsSync('fixed');\n\
         console.log(repaired ? 'repro: the defect no longer reproduces' : 'repro: the defect reproduces');\n\
         process.exit(repaired ? 0 : 1);\n",
    );
    gate_command(db, project_id, name, REPRO_CHECK_CMDLINE)
}

/// An ALWAYS-GREEN check, armed on `project_id` and materialized in `dir`.
fn always_green_command(db: &Db, project_id: &str, dir: &Path, name: &str) -> String {
    write_check_script(
        dir,
        ALWAYS_GREEN_SCRIPT,
        "// An always-green check — the portable spelling of `true`.\n\
         console.log('checks green');\n\
         process.exit(0);\n",
    );
    gate_command(db, project_id, name, ALWAYS_GREEN_CMDLINE)
}

/// RETIRE the managed command an instance materializes — the project gesture that leaves a slot
/// bound to an id nothing answers any more. Deleting the TEMPLATE is the real route (an instance is
/// the template's materialization in a workspace); the binding is untouched, which is the point.
fn retire_command(db: &Db, instance_id: &str) {
    db.with_conn(|c| {
        let command_id = db::get_instance(c, instance_id)
            .expect("instance")
            .expect("materialized")
            .command_id;
        db::delete_template(c, &command_id).expect("the managed command is retired");
    });
}

/// BIND `slot` through the REAL authoring verb — the day-one gesture, never a DB shortcut, and
/// never a fork (the whole point of a slot is that arming a shared gate costs no fork).
fn bind_slot(db: &Db, project_id: &str, slot: &str, command_ids: &[&str]) {
    author(
        db,
        "slot_bind",
        json!({
            "project_id": project_id,
            "slot": slot,
            "command_ids": command_ids,
        }),
    );
}

/// Drive an authoring verb through the REAL MCP routing.
fn author(db: &Db, name: &str, args: Value) -> Value {
    dispatch_authoring_tool(db, name, &args)
        .unwrap_or_else(|| panic!("`{name}` is not routed"))
        .unwrap_or_else(|e| panic!("`{name}` failed: {}", e.message))
        .result
}

/// One `nyx_next` for `run_id` with no session behind it (nobody at a keyboard).
fn tick(db: &Db, rt: &EngineRuntime, run_id: &str, last: Option<Value>) -> Value {
    nyx_next(db, rt, no_session(), &tick_args(run_id, last))
        .expect("nyx_next")
        .result
}

/// One `nyx_next` for `run_id` that MUST be refused.
fn tick_err(
    db: &Db,
    rt: &EngineRuntime,
    run_id: &str,
    last: Option<Value>,
) -> nyx_core::mcp::RpcError {
    nyx_next(db, rt, no_session(), &tick_args(run_id, last)).expect_err("nyx_next must refuse")
}

fn tick_args(run_id: &str, last: Option<Value>) -> Value {
    let mut args = json!({ "run_id": run_id });
    if let Some(last) = last {
        args.as_object_mut().unwrap().insert("last_result".into(), last);
    }
    args
}

fn no_session() -> CallerSession<'static> {
    CallerSession { terminal_id: None }
}

/// The `(workflow, nodes)` of the BUILT-IN flow of `scope` — the seeded row, `forked_from IS NULL`.
fn builtin(db: &Db, scope: &str) -> (db::Workflow, Vec<db::Node>) {
    db.with_conn(|c| {
        let wf = db::list_workflows(c, None)
            .expect("workflows")
            .into_iter()
            .find(|w| w.scope == scope && w.forked_from.is_none())
            .unwrap_or_else(|| panic!("the built-in '{scope}' is seeded by the boot"));
        let nodes = db::list_nodes(c, &wf.id).expect("nodes");
        (wf, nodes)
    })
}

/// Every node LABEL of the built-in flow of `scope`.
fn builtin_labels(db: &Db, scope: &str) -> Vec<String> {
    builtin(db, scope).1.into_iter().map(|n| n.label).collect()
}

/// The Spec's persisted category SLUG, read the way the harness reads it.
fn spec_category_slug(db: &Db, spec_id: &str) -> Option<String> {
    db.with_conn(|c| {
        let spec = db::get_spec(c, spec_id).expect("spec")?;
        let id = spec.category_id?;
        db::get_category(c, &id).expect("category").map(|c| c.slug)
    })
}

/// The Spec's gate journal, oldest first, as `(expect, outcome, satisfied, slot)` — the four facts
/// the auditor's discipline tells them to read.
fn journal(db: &Db, spec_id: &str) -> Vec<(String, String, bool, Option<String>)> {
    db.with_conn(|c| db::list_gate_attempts_for_spec(c, spec_id))
        .expect("the journal")
        .into_iter()
        .map(|a| (a.expect, a.outcome, a.satisfied, a.slot))
        .collect()
}

/// CLASSIFY `spec_id`, through the shared service every writer of `specs.category_id` goes through.
fn classify(db: &Db, spec_id: &str, label: &str, source: db::DomainEventSource) {
    db.with_conn(|c| nyx_core::category::set_spec_category_by_label(c, spec_id, label, source))
        .unwrap_or_else(|e| panic!("classify `{label}`: {e}"));
}

/// Open `/ndev` on `spec_id` and walk to the CODER CHILD — the reproduction lives in the sub-flow,
/// never in the graph `run_start` opens. Returns `(parent_run_id, child_run_id, warnings)`.
fn coder_child_of_ndev(db: &Db, rt: &EngineRuntime, spec_id: &str) -> (String, String, Vec<Value>) {
    let opened = run_start(db, rt, &json!({ "scope": "ndev", "spec_id": spec_id }))
        .expect("run_start")
        .result;
    let warnings = opened["warnings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let run_id = opened["run_id"].as_str().expect("run_id").to_string();
    let first = tick(db, rt, &run_id, None);
    assert_eq!(
        first["action"]["type"], "sub_workflow",
        "ndev delegates its build pass to the coder child: {first}"
    );
    let child = first["action"]["child_run_id"]
        .as_str()
        .expect("a child run")
        .to_string();
    (run_id, child, warnings)
}

/// WHICH of the reproduction branch's questions a surfaced `ask_human` is — keyed on a phrase of the
/// SEEDED prompt, so a re-worded question fails here loudly instead of being silently mis-answered.
fn coder_question(prompt: &str) -> &'static str {
    if prompt.contains("Answer 'automated'") {
        "automatable?"
    } else if prompt.contains("YOU are the check") {
        "the protocol + the BEFORE observation"
    } else if prompt.contains("The fix is in.") {
        "the SAME protocol, after"
    } else {
        "?"
    }
}

// ============================================================================
// Criterion 1 — the boot chain 22 → 24, and the fork that stays put AND is named
// ============================================================================

/// DONE-CRITERION (« Une base revision 22 migre jusqu'aux seeds globaux 24 ; les workflows forkés
/// restent inchangés et sont signalés. »)
///
/// WHY THIS IS THE ONE TEST THAT CANNOT BE A UNIT TEST. `ensure_builtin` is exercised in
/// `seed::tests` on an in-memory database seeded by hand, one hop at a time (23 → 24). What nobody
/// had ever run is the thing that actually happens to a user: a nyx.db written by the PREVIOUS
/// release — built-ins stamped 22, a fork taken from them — being opened by THIS one. The upgrade
/// then has to cross TWO revisions in one boot, and it has to do it through `Db::open`, whose order
/// (migrations → drift refusal → workflow seed → category backfill) is itself a claim nothing else
/// checks.
///
/// The three clauses, each asserted below:
///
///   * 22 → 24 IN ONE BOOT. The built-ins come back at 24 carrying BOTH revisions' work: the
///     categorisation `/nspec` gained at 23 (a judge + a `set_category`) and the preflight and the
///     bug branches `/ndev`'s coder gained at 24. A single-hop upgrade would satisfy the revision
///     number and leave half the graph missing, which is exactly what the labels below catch.
///   * THE FORK IS UNTOUCHED — same nodes, same revision stamp. That is not a bug being tolerated,
///     it is the contract: a fork is the project's own customisation and the boot seeder owns only
///     the rows it seeded.
///   * THE FORK IS SIGNALLED. Untouched and unmentioned would be the real defect — a project
///     silently frozen two revisions behind, missing a discipline it does not know exists. So
///     `run_start` reports it (`workflow_fork_stale`) with BOTH numbers, through the `sub_workflow`
///     hop it never opens.
#[test]
fn a_base_at_revision_22_boots_to_the_global_seeds_at_24_and_a_fork_stays_frozen_and_signalled() {
    let tmp = temp_dir("boot-22-to-24");
    let db_path = tmp.path().join("nyx.db");

    // --- The base the PREVIOUS release left behind ---------------------------------------------
    let db = Db::open(&db_path).expect("first boot");
    let (project_id, workspace_id, _ws) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // Roll the three flows this PRD touches back to what revision 22 shipped. The TREE is replaced
    // by a marker note — what matters is that it is NOT the current one, so an upgrade that fails to
    // fire is visible as a missing node rather than as a lucky pass.
    for scope in [
        db::WORKFLOW_SCOPE_NSPEC,
        db::WORKFLOW_SCOPE_NDEV,
        db::WORKFLOW_SCOPE_CODER,
    ] {
        let (wf, _n) = builtin(&db, scope);
        db.with_conn(|c| {
            db::delete_nodes_for_workflow(c, &wf.id).expect("wipe");
            let root = db::create_node(
                c,
                &wf.id,
                None,
                0,
                db::NODE_TYPE_NOTE,
                "rev22",
                &json!({ "text": "the revision 22 tree" }).to_string(),
            )
            .expect("root");
            db::set_workflow_root(c, &wf.id, Some(&root.id)).expect("root");
            db::set_builtin_revision(c, &wf.id, 22).expect("stamp 22");
        });
    }
    let coder_builtin_id = builtin(&db, db::WORKFLOW_SCOPE_CODER).0.id;

    // …and the project that had forked its coder, at that very revision — the real shape of the
    // problem: `workflow_fork` stamps the SOURCE's revision on the copy, so the fork knows it is a
    // copy of 22 and nothing will ever move it.
    let fork_id = author(
        &db,
        "workflow_fork",
        json!({ "workflow_id": coder_builtin_id, "project_id": project_id }),
    )["workflow"]["id"]
        .as_str()
        .expect("the fork's id")
        .to_string();
    let fork_nodes_before: Vec<String> = db
        .with_conn(|c| db::list_nodes(c, &fork_id))
        .expect("fork nodes")
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(fork_nodes_before.len(), 1, "the fork copied the revision-22 tree");
    assert_eq!(
        db.with_conn(|c| db::get_workflow(c, &fork_id))
            .unwrap()
            .unwrap()
            .forked_from_revision,
        Some(22),
        "and it recorded WHICH revision it is a copy of — the whole basis of the staleness report",
    );

    // --- THE BOOT. The same file, opened by this release. ---------------------------------------
    drop(db);
    let db = Db::open(&db_path).expect("second boot — the upgrade");

    // 22 → 24, in ONE boot, carrying BOTH revisions' work.
    for scope in [
        db::WORKFLOW_SCOPE_NSPEC,
        db::WORKFLOW_SCOPE_NDEV,
        db::WORKFLOW_SCOPE_CODER,
    ] {
        assert_eq!(
            builtin(&db, scope).0.builtin_revision,
            Some(24),
            "the built-in '{scope}' was stamped 22 and the code ships 24 — the boot upgrades it",
        );
    }
    let nspec = builtin_labels(&db, db::WORKFLOW_SCOPE_NSPEC);
    for owed in ["categorize", "which category?", "classify"] {
        assert!(
            nspec.iter().any(|l| l == owed),
            "revision 23's categorisation reached the DB: '{owed}' missing from {nspec:?}",
        );
    }
    let coder = builtin_labels(&db, db::WORKFLOW_SCOPE_CODER);
    for owed in [
        "preflight",
        "classify",
        "bug discipline?",
        "reproduction",
        "the same reproduction, green",
        "can this be reproduced automatically?",
        "the SAME protocol, after the fix",
    ] {
        assert!(
            coder.iter().any(|l| l == owed),
            "revision 24's preflight + bug branches reached the DB: '{owed}' missing from {coder:?}",
        );
    }
    assert_eq!(
        builtin(&db, db::WORKFLOW_SCOPE_CODER)
            .1
            .iter()
            .filter(|n| n.type_ == db::NODE_TYPE_SET_CATEGORY)
            .count(),
        1,
        "including the 17th node type, put to work by the preflight",
    );

    // THE FORK IS UNTOUCHED — the same node rows, the same stamp. Its owner keeps their graph, and
    // therefore does NOT get the discipline: known, assumed, and asserted rather than discovered.
    let fork_after = db
        .with_conn(|c| db::get_workflow(c, &fork_id))
        .unwrap()
        .expect("the fork survived the boot");
    let fork_nodes_after: Vec<String> = db
        .with_conn(|c| db::list_nodes(c, &fork_id))
        .expect("fork nodes")
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(
        fork_nodes_after, fork_nodes_before,
        "the boot rewrote nothing in the project's own copy — not even the row identities",
    );
    assert_eq!(fork_after.forked_from_revision, Some(22), "nor its stamp");

    // …AND IT IS SIGNALLED. `run_start` opens ndev and never the coder graph, so the alarm has to
    // walk the `sub_workflow` edge the run will walk — which is precisely why an untold fork could
    // rot for months.
    let opened = run_start(
        &db,
        &EngineRuntime::new(),
        &json!({ "scope": "ndev", "spec_id": spec_id }),
    )
    .expect("run_start")
    .result;
    let warnings = opened["warnings"].as_array().expect("a warnings array");
    let stale = warnings
        .iter()
        .find(|w| w["code"] == "workflow_fork_stale")
        .unwrap_or_else(|| {
            panic!("a fork frozen two revisions behind must be TOLD, not merely left alone: {warnings:?}")
        });
    assert_eq!(stale["workflow_id"], json!(fork_id), "it names THEIR fork");
    assert_eq!(stale["forked_from_revision"], json!(22));
    assert_eq!(stale["builtin_revision"], json!(24));
    assert_ne!(
        opened["workflow_id"], json!(fork_id),
        "and it came through the hop: the run itself opened ndev",
    );

    // A THIRD boot changes nothing — the seeder is idempotent at the current revision.
    let labels_after_two = builtin_labels(&db, db::WORKFLOW_SCOPE_CODER);
    drop(db);
    let db = Db::open(&db_path).expect("third boot");
    assert_eq!(builtin_labels(&db, db::WORKFLOW_SCOPE_CODER), labels_after_two);
    assert_eq!(
        builtin(&db, db::WORKFLOW_SCOPE_CODER).0.builtin_revision,
        Some(24)
    );
}

/// DONE-CRITERION (« Une base revision 22 migre jusqu'aux seeds globaux 24 » — the CATEGORY half of
/// "les seeds globaux").
///
/// `create_project` seeds the four system categories for every project born from now on, which
/// covers nothing that already exists. A database migrated onto `20260828180916_create_categories`
/// holds projects with NO category at all, and a Spec in one of them cannot be classified — so the
/// discipline can never fire there, and no gesture short of recreating the project would fix it.
/// The backfill is the second half of `Db::open`'s seeding, and this is the only place it runs.
///
/// The projectless state is reproduced by DELETING the vocabulary of a project that has one, which
/// is the same row-shape a pre-migration base presents. The boot restores it — and restores it
/// IDEMPOTENTLY: a project that already answers the four slugs is left exactly as it is, RENAMES
/// INCLUDED, because a human's label is not something a boot may overwrite.
#[test]
fn the_boot_backfills_the_category_vocabulary_of_a_base_that_predates_it() {
    let tmp = temp_dir("boot-backfill");
    let db_path = tmp.path().join("nyx.db");

    let db = Db::open(&db_path).expect("first boot");
    let (project_id, _ws_id, _ws) = live_project(&db, tmp.path());
    let seeded: Vec<String> = db
        .with_conn(|c| db::list_categories(c, &project_id))
        .expect("categories")
        .into_iter()
        .map(|c| c.slug)
        .collect();
    assert_eq!(
        seeded,
        vec!["feature", "bug", "refactor", "chore"],
        "a project born after this Spec answers the four system slugs, in the curated order",
    );

    // A base that predates the migration: the project exists, the vocabulary does not. The public
    // deletion (`category::delete`, and the sidecar's `category_delete`) REFUSES a system slug by
    // design, so it cannot produce this state at all — hence the raw SQL, which is honest about what
    // it is: the row-shape a database migrated onto `20260828180916_create_categories` really
    // presents, and the only one the backfill exists for.
    db.with_conn(|c| {
        diesel::sql_query("DELETE FROM categories")
            .execute(c)
            .expect("empty the vocabulary the way a pre-migration base is empty");
    });
    assert!(db
        .with_conn(|c| db::list_categories(c, &project_id))
        .unwrap()
        .is_empty());

    drop(db);
    let db = Db::open(&db_path).expect("second boot — the backfill");
    let backfilled: Vec<(String, String, bool)> = db
        .with_conn(|c| db::list_categories(c, &project_id))
        .expect("categories")
        .into_iter()
        .map(|c| (c.slug, c.created_by_type, c.confirmed_at.is_some()))
        .collect();
    assert_eq!(
        backfilled,
        vec![
            ("feature".into(), "system".into(), true),
            ("bug".into(), "system".into(), true),
            ("refactor".into(), "system".into(), true),
            ("chore".into(), "system".into(), true),
        ],
        "the boot gave the existing project its vocabulary — born `system` and born confirmed",
    );

    // A HUMAN RENAME SURVIVES THE NEXT BOOT. The seed is keyed on `(project_id, slug)` and does
    // nothing on conflict, so the curation gesture PDR-0017 puts in the human's hands is not undone
    // by the next start of the app — which is the difference between an idempotent seed and a reset.
    let bug_id = db
        .with_conn(|c| db::find_category_by_slug(c, &project_id, "bug"))
        .expect("lookup")
        .expect("the seeded bug")
        .id;
    db.with_conn(|c| {
        nyx_core::category::rename(
            c,
            &project_id,
            nyx_core::category::CategoryRef::Id(&bug_id),
            "Défaut",
        )
    })
    .expect("rename");
    drop(db);
    let db = Db::open(&db_path).expect("third boot");
    let renamed = db
        .with_conn(|c| db::find_category_by_slug(c, &project_id, "bug"))
        .expect("lookup")
        .expect("still there, under its stable slug");
    assert_eq!(renamed.name, "Défaut", "the boot re-seeded nothing over the human's label");
    assert_eq!(renamed.id, bug_id, "and it is the same row — no duplicate was minted");
    assert_eq!(
        db.with_conn(|c| db::list_categories(c, &project_id))
            .unwrap()
            .len(),
        4,
        "still four, so the seed really is an upsert and not an insert",
    );
}

// ============================================================================
// Criterion 2 + 6 — the automated route, and the proof that outlives it
// ============================================================================

/// DONE-CRITERION (« Un bug automatisé exécute exactement le même repro-check rouge puis vert,
/// ensuite le build vert. ») — walked on the graph a real boot seeded, in a real folder, over a
/// real PTY.
///
/// The check is ONE command bound to the project's `repro_check` slot ([`repro_check_command`]): it
/// FAILS while the sentinel is absent and PASSES once the spawn that stands for the fix creates it.
/// That makes
/// the red and the green the same check in the only sense that can be verified from the outside —
/// same slot, same RESOLVED command line, same cwd — and the journal is what lets an auditor SEE
/// it rather than take the coder's word for it.
///
/// The ORDER is the claim: the fix is applied on the spawn the graph places AFTER the red gate.
/// Creating the sentinel any earlier would make the reproduction pass, and a passing reproduction
/// is the failure this whole PRD exists to prevent — so the walk below records what nyx surfaced,
/// in sequence, and compares the whole list rather than spot-checking it.
#[test]
fn an_automated_bug_runs_the_same_repro_check_red_then_green_then_the_build() {
    let tmp = temp_dir("automated-bug");
    let db = boot(&tmp);
    let (project_id, workspace_id, ws_path) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    classify(&db, &spec_id, "bug", db::DomainEventSource::Mcp);
    declare_phases(&db, &spec_id, &["Build"]);
    let task_id = build_task(&db, &project_id, &spec_id, "repair the defect");

    // PHASE 7 WAS SKIPPED ON nyx-v2, SO THE FIXTURE ARMS THE SLOT ITSELF — the same two calls the
    // refusal of an unarmed gate prescribes, through the same verb, forking nothing.
    let repro = repro_check_command(&db, &project_id, tmp.path(), "repro");
    let build = always_green_command(&db, &project_id, tmp.path(), "build");
    bind_slot(&db, &project_id, nyx_core::seed::REPRO_CHECK_SLOT, &[&repro]);
    bind_slot(&db, &project_id, nyx_core::seed::BUILD_CHECK_SLOT, &[&build]);
    assert!(
        db.with_conn(|c| db::list_workflows(c, Some(&project_id)))
            .expect("project workflows")
            .is_empty(),
        "arming the reproduction gate forked nothing — the project runs nyx's own coder graph",
    );

    let rt = EngineRuntime::new();
    let (_parent, child, _w) = coder_child_of_ndev(&db, &rt, &spec_id);

    let mut surfaced: Vec<String> = Vec::new();
    let mut last: Option<Value> = None;
    for _ in 0..40 {
        let out = tick(&db, &rt, &child, last.take());
        let action = out["action"].clone();
        let ty = action["type"].as_str().expect("an action type").to_string();
        if ty == "done" {
            surfaced.push("done".to_string());
            break;
        }
        let label = action["label"].as_str().unwrap_or_default().to_string();
        let prompt = action["prompt"].as_str().unwrap_or_default();
        surfaced.push(match ty.as_str() {
            "ask_human" => format!("ask_human:{}", coder_question(prompt)),
            _ => format!("{ty}:{label}"),
        });
        last = Some(match ty.as_str() {
            // THE FIX — and it lands on the spawn the graph placed AFTER the red gate, never before.
            "spawn_subagent" if label == "repair the defect" => {
                std::fs::write(Path::new(&ws_path).join("fixed"), "repaired").expect("the fix");
                json!({ "diff": "the defect is repaired" })
            }
            "spawn_subagent" => json!({ "diff": "a check that fails on this defect" }),
            "ask_human" => json!({ "decision": "automated" }),
            other => panic!("unexpected coder action '{other}': {out}"),
        });
    }
    assert_eq!(
        surfaced,
        vec![
            // No `judge`: the Spec is already classified, and the preflight never re-decides a
            // classification somebody already made.
            "spawn_subagent:write the reproduction".to_string(),
            "ask_human:automatable?".to_string(),
            // …the RED gate runs HERE, inline: nyx runs its own gates and never surfaces one.
            "spawn_subagent:repair the defect".to_string(),
            // …then the GREEN gate, then the build loop's spawn and its `checks`.
            "spawn_subagent:implement".to_string(),
            "done".to_string(),
        ],
        "the shipped order: write the check, rule on automation, (RED), fix, (GREEN), then build",
    );
    assert_eq!(
        db.with_conn(|c| db::get_task(c, &task_id))
            .unwrap()
            .unwrap()
            .status,
        db::TASK_STATUS_DONE,
        "and the task closed on the far side of the whole sequence",
    );

    // THE JOURNAL — three attempts, in the order they happened, and only then the build control.
    assert_eq!(
        journal(&db, &spec_id),
        vec![
            (
                "red".to_string(),
                "failed".to_string(),
                true,
                Some(nyx_core::seed::REPRO_CHECK_SLOT.to_string())
            ),
            (
                "green".to_string(),
                "passed".to_string(),
                true,
                Some(nyx_core::seed::REPRO_CHECK_SLOT.to_string())
            ),
            (
                "green".to_string(),
                "passed".to_string(),
                true,
                Some(nyx_core::seed::BUILD_CHECK_SLOT.to_string())
            ),
        ],
        "a satisfied RED, then a satisfied GREEN on the same slot, and only THEN the build check",
    );

    // THE IDENTITY OF THE CONTROL — the two fields the auditor's discipline names.
    let attempts = db
        .with_conn(|c| db::list_gate_attempts_for_spec(c, &spec_id))
        .expect("the journal");
    let (red, green, checks) = (&attempts[0], &attempts[1], &attempts[2]);
    assert_ne!(red.node_id, green.node_id, "two DISTINCT gate nodes declared it");
    assert_eq!(
        (&red.command_line, &red.cwd),
        (&green.command_line, &green.cwd),
        "…resolved through ONE binding, so the same command ran in the same folder",
    );
    assert_eq!(red.cwd, ws_path, "and that folder is the Spec's workspace");
    assert_ne!(
        red.command_line, checks.command_line,
        "the build control is a DIFFERENT command — which is why it can stand in for neither",
    );
    assert_eq!(red.exit_code, Some(1), "the red is a NORMAL termination, non-zero");
    assert_eq!(green.exit_code, Some(0), "and the green a normal termination at zero");
}

/// **ONE REPRODUCTION PER RUN, NOT ONE PER TASK** — the regression that closes review finding C19.
///
/// The reproduction branch first shipped INSIDE the coder's per-task loop, under `build the task`.
/// That is invisible on a phase carrying a single task, which is the only shape every other test in
/// this file walks — and fatal on the shape the graph exists for: `phase_next_task` /
/// `phase_open_tasks` are there precisely because a phase holds several tasks and the count GROWS as
/// the coder and the review file findings.
///
/// What the second pass did: `spec_category` still says `bug` (the preflight wrote it above the
/// loop and nothing clears it), so the discipline selector takes the same branch, `ask_human` has no
/// "already answered" short-circuit and re-asks, and the `expect: red` gate re-runs — on a defect
/// the FIRST pass repaired. It PASSES, `red` is not satisfied, and the mismatch refusal is sticky by
/// construction: every later `nyx_next` re-runs the same passing check and refuses identically. The
/// documented ways out are `node_update` on the gate (refused — `builtin_immutable`) or
/// `run_delete`. The run is wedged by the very proof it just produced.
///
/// So the walk below drives TWO open tasks over one `bug` Spec and holds the whole journal to it:
/// ONE satisfied red, ONE green closing it, and then one build control per task. The run reaching
/// `done` at all is half the assertion — before the fix it could not.
#[test]
fn a_second_task_on_a_bug_spec_does_not_re_run_the_reproduction() {
    let tmp = temp_dir("two-tasks-one-repro");
    let db = boot(&tmp);
    let (project_id, workspace_id, ws_path) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    classify(&db, &spec_id, "bug", db::DomainEventSource::Mcp);
    declare_phases(&db, &spec_id, &["Build"]);
    // TWO open tasks in the SAME phase — the shape the task loop is written for.
    let first = build_task(&db, &project_id, &spec_id, "repair the defect");
    let second = build_task(&db, &project_id, &spec_id, "cover the neighbouring case");

    let repro = repro_check_command(&db, &project_id, tmp.path(), "repro");
    let build = always_green_command(&db, &project_id, tmp.path(), "build");
    bind_slot(&db, &project_id, nyx_core::seed::REPRO_CHECK_SLOT, &[&repro]);
    bind_slot(&db, &project_id, nyx_core::seed::BUILD_CHECK_SLOT, &[&build]);

    let rt = EngineRuntime::new();
    let (_parent, child, _w) = coder_child_of_ndev(&db, &rt, &spec_id);

    let mut surfaced: Vec<String> = Vec::new();
    let mut last: Option<Value> = None;
    for _ in 0..60 {
        let out = tick(&db, &rt, &child, last.take());
        let action = out["action"].clone();
        let ty = action["type"].as_str().expect("an action type").to_string();
        if ty == "done" {
            surfaced.push("done".to_string());
            break;
        }
        let label = action["label"].as_str().unwrap_or_default().to_string();
        let prompt = action["prompt"].as_str().unwrap_or_default();
        surfaced.push(match ty.as_str() {
            "ask_human" => format!("ask_human:{}", coder_question(prompt)),
            _ => format!("{ty}:{label}"),
        });
        last = Some(match ty.as_str() {
            "spawn_subagent" if label == "repair the defect" => {
                std::fs::write(Path::new(&ws_path).join("fixed"), "repaired").expect("the fix");
                json!({ "diff": "the defect is repaired" })
            }
            "spawn_subagent" => json!({ "diff": "a check that fails on this defect" }),
            "ask_human" => json!({ "decision": "automated" }),
            other => panic!("unexpected coder action '{other}': {out}"),
        });
    }
    assert_eq!(
        surfaced,
        vec![
            "spawn_subagent:write the reproduction".to_string(),
            "ask_human:automatable?".to_string(),
            // (RED) — inline
            "spawn_subagent:repair the defect".to_string(),
            // (GREEN) — inline, and the reproduction is OVER for this run
            "spawn_subagent:implement".to_string(),
            // …the SECOND task re-enters the build loop and NOTHING ELSE: no second
            // "write the reproduction", no second impracticability question.
            "spawn_subagent:implement".to_string(),
            "done".to_string(),
        ],
        "the reproduction is owed ONCE per run — the second task only builds",
    );

    assert_eq!(
        journal(&db, &spec_id),
        vec![
            (
                "red".to_string(),
                "failed".to_string(),
                true,
                Some(nyx_core::seed::REPRO_CHECK_SLOT.to_string())
            ),
            (
                "green".to_string(),
                "passed".to_string(),
                true,
                Some(nyx_core::seed::REPRO_CHECK_SLOT.to_string())
            ),
            (
                "green".to_string(),
                "passed".to_string(),
                true,
                Some(nyx_core::seed::BUILD_CHECK_SLOT.to_string())
            ),
            (
                "green".to_string(),
                "passed".to_string(),
                true,
                Some(nyx_core::seed::BUILD_CHECK_SLOT.to_string())
            ),
        ],
        "EXACTLY ONE `expect: red` attempt for the whole run — a second one would have re-run the \
         repaired check, come back PASSED against a declared `red`, and wedged the run for good",
    );

    for task_id in [&first, &second] {
        assert_eq!(
            db.with_conn(|c| db::get_task(c, task_id))
                .unwrap()
                .unwrap()
                .status,
            db::TASK_STATUS_DONE,
            "both tasks of the phase closed — the run was never parked on a passing reproduction",
        );
    }
    assert_eq!(
        db.with_conn(|c| db::get_workflow_run(c, &child))
            .unwrap()
            .unwrap()
            .status,
        db::RUN_STATUS_DONE,
    );
}

/// DONE-CRITERION (« Les preuves de toutes les gates restent consultables et sont transmises à
/// l'auditeur. »)
///
/// AFTER THE BUILD, WHICH IS THE ONLY MOMENT THAT PROVES ANYTHING. The run's board carries
/// `last_gate_result` — ONE attempt, under a fixed key, overwritten by the next gate — so the
/// build control's own attempt lands on top of the green that closed the bug, which landed on top
/// of the red that opened it. Read from the board, the proof disappears at the exact moment it
/// becomes provable. That is why it is a journal, and this test reads it at the one point in the
/// run where a board would already have lost it.
///
/// « Transmises à l'auditeur » is not "available if someone queries the table": the auditor is an
/// agent whose whole world is its rendered context. So the assertion is on
/// `render_agent_context(Auditor, …)` — the discipline block, the sequence verdict, and the three
/// attempts, none of them elided.
#[test]
fn the_evidence_of_every_gate_outlives_the_run_and_reaches_the_auditor() {
    let tmp = temp_dir("evidence-outlives");
    let db = boot(&tmp);
    let (project_id, workspace_id, ws_path) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    classify(&db, &spec_id, "bug", db::DomainEventSource::Mcp);
    declare_phases(&db, &spec_id, &["Build"]);
    build_task(&db, &project_id, &spec_id, "repair the defect");
    let repro = repro_check_command(&db, &project_id, tmp.path(), "repro");
    let build = always_green_command(&db, &project_id, tmp.path(), "build");
    bind_slot(&db, &project_id, nyx_core::seed::REPRO_CHECK_SLOT, &[&repro]);
    bind_slot(&db, &project_id, nyx_core::seed::BUILD_CHECK_SLOT, &[&build]);

    let rt = EngineRuntime::new();
    let (_parent, child, _w) = coder_child_of_ndev(&db, &rt, &spec_id);
    let mut last: Option<Value> = None;
    for _ in 0..40 {
        let out = tick(&db, &rt, &child, last.take());
        let ty = out["action"]["type"].as_str().expect("a type").to_string();
        if ty == "done" {
            break;
        }
        let label = out["action"]["label"].as_str().unwrap_or_default().to_string();
        last = Some(match ty.as_str() {
            "spawn_subagent" if label == "repair the defect" => {
                std::fs::write(Path::new(&ws_path).join("fixed"), "repaired").expect("the fix");
                json!({ "diff": "repaired" })
            }
            "spawn_subagent" => json!({ "diff": "d" }),
            "ask_human" => json!({ "decision": "automated" }),
            other => panic!("unexpected coder action '{other}': {out}"),
        });
    }

    // THE BOARD HAS ALREADY FORGOTTEN. Its single key holds the LAST attempt — the build control's.
    let board: Value = serde_json::from_str(
        &db.with_conn(|c| db::get_workflow_run(c, &child))
            .unwrap()
            .unwrap()
            .blackboard,
    )
    .expect("a board");
    let attempts = db
        .with_conn(|c| db::list_gate_attempts_for_spec(c, &spec_id))
        .expect("the journal");
    assert_eq!(attempts.len(), 3);
    assert_eq!(
        board["last_gate_result"]["node_id"], attempts[2].node_id,
        "the board carries the BUILD gate's attempt, the reproduction's having been overwritten: {board}",
    );

    // THE AUDITOR STILL GETS ALL THREE.
    let audit = nyx_core::mcp_context::render_agent_context(
        &db,
        nyx_core::mcp_context::AgentRole::Auditor,
        &spec_id,
        None,
    )
    .expect("the auditor context renders")
    .expect("…and it is bound to this Spec");
    assert!(
        audit.contains(nyx_core::mcp_context::CATEGORY_DISCIPLINE_HEADING),
        "the `bug` discipline is injected for the role that judges the proof",
    );
    assert!(
        audit.contains("3 recorded gate attempt"),
        "every attempt is still there, AFTER the build gate wrote its own: {audit}",
    );
    assert!(
        audit.contains("SAME control"),
        "and the sequence is READ for the auditor — red before green, on one control: {audit}",
    );
    for attempt in &attempts {
        assert!(
            audit.contains(&attempt.command_line),
            "each attempt's RESOLVED command line rides in the context, which is what makes \
             'same slot, same commands, same cwd' a comparison the auditor can actually make",
        );
    }
    assert!(
        audit.contains(&attempts[0].cwd),
        "…and so does the cwd it ran in",
    );

    // AND IT SURVIVES THE PROCESS. The journal is a table, not a runtime cache: re-open the very
    // same file and the proof is still the proof.
    let db_path = tmp.path().join("nyx.db");
    drop(db);
    let db = Db::open(&db_path).expect("re-open");
    assert_eq!(
        journal(&db, &spec_id).len(),
        3,
        "the evidence outlived the pool that wrote it",
    );
}

// ============================================================================
// Criterion 3 — the safety clause, walked through the seeded graph
// ============================================================================

/// DONE-CRITERION (« Un timeout, une commande introuvable ou une erreur d'infrastructure ne peut
/// satisfaire le rouge. ») — the END-TO-END half.
///
/// The three CAUSES are graded and refused in `mcp_engine`'s own tests (they need the crate-private
/// budget injection to produce a timeout in milliseconds instead of ninety seconds, and the
/// crate-private grader to build the fold's table). What those cannot show — because a hand-built
/// two-node tree is not the shipped graph — is that the refusal actually lands ON THE REPRODUCTION
/// GATE of the coder flow a `bug` Spec walks. That is what this test does, with the cause that IS
/// reproducible from outside the crate: the gate's bound command is resolvable at `slot_bind` time
/// and GONE by the time the gate runs, which is an infrastructure fault and nothing else.
///
/// WHAT MUST NOT HAPPEN is the whole PRD in one sentence: the attempt produced no verdict, so it
/// must not be read as "the check failed", because "the check failed" is exactly what an
/// `expect: red` reproduction gate is waiting for. A gate that verified nothing standing as the
/// proof that a defect reproduces is the failure this Spec was written to make impossible.
#[test]
fn an_infrastructure_fault_on_the_reproduction_gate_is_never_a_red() {
    let tmp = temp_dir("infra-never-red");
    let db = boot(&tmp);
    let (project_id, workspace_id, _ws) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    classify(&db, &spec_id, "bug", db::DomainEventSource::Mcp);
    declare_phases(&db, &spec_id, &["Build"]);
    build_task(&db, &project_id, &spec_id, "repair the defect");
    let repro = repro_check_command(&db, &project_id, tmp.path(), "repro");
    let build = always_green_command(&db, &project_id, tmp.path(), "build");
    bind_slot(&db, &project_id, nyx_core::seed::REPRO_CHECK_SLOT, &[&repro]);
    bind_slot(&db, &project_id, nyx_core::seed::BUILD_CHECK_SLOT, &[&build]);

    // THE FAULT: the bound command is gone by the time the gate resolves it. `slot_bind` accepted
    // the id (the instance existed then), and a binding holds IDS, not rows — so retiring the
    // managed command leaves a slot pointing at nothing, which is what a stale project
    // configuration really looks like.
    retire_command(&db, &repro);

    let rt = EngineRuntime::new();
    let (_parent, child, _w) = coder_child_of_ndev(&db, &rt, &spec_id);
    let mut last: Option<Value> = None;
    let refusal = loop {
        match nyx_next(&db, &rt, no_session(), &tick_args(&child, last.take())) {
            Ok(out) => {
                let out = out.result;
                let ty = out["action"]["type"].as_str().expect("a type").to_string();
                assert_ne!(
                    ty, "done",
                    "the run must NOT complete: its reproduction gate never produced a verdict",
                );
                last = Some(match ty.as_str() {
                    "spawn_subagent" => json!({ "diff": "a check for this defect" }),
                    "ask_human" => json!({ "decision": "automated" }),
                    other => panic!("unexpected coder action '{other}': {out}"),
                });
            }
            Err(e) => break e,
        }
    };
    assert!(
        refusal.message.contains("gate command"),
        "the refusal names WHAT could not be run, so the fault is repairable: {}",
        refusal.message,
    );
    assert_eq!(
        db.with_conn(|c| db::get_workflow_run(c, &child))
            .unwrap()
            .unwrap()
            .status,
        db::RUN_STATUS_BLOCKED,
        "the run is PARKED on the gate, not walked past it",
    );
    // THE ATTEMPT IS RECORDED, and recorded as what it was.
    let attempts = db
        .with_conn(|c| db::list_gate_attempts_for_spec(c, &spec_id))
        .expect("the journal");
    assert_eq!(
        attempts.len(),
        1,
        "a fault that produced no verdict is still an ATTEMPT, and it is journaled: {attempts:?}",
    );
    let attempt = &attempts[0];
    assert_eq!(attempt.expect, "red", "it is the reproduction gate that was reached");
    assert_eq!(
        attempt.outcome, "indeterminate",
        "an unresolvable command produced NO verdict — not a failure of the work",
    );
    assert!(
        !attempt.satisfied,
        "AND IT SATISFIES NOTHING. This single boolean is the safety property of the whole PRD: \
         read in binary, a gate that could not run would have counted as 'not green', therefore as \
         a reproduction, and a check that verified nothing would have stood as the proof that the \
         defect reproduces",
    );
    assert_eq!(attempt.exit_code, None, "no status was ever read");

    // …AND THE REFUSAL IS STICKY. The leaf is marked for replay, so the next call meets the same
    // wall instead of decoding the failure as an acknowledgement and advancing to the fix the gate
    // exists to guard — and the second attempt is APPENDED beside the first, never over it.
    let again = tick_err(&db, &rt, &child, None);
    assert_eq!(again.code, refusal.code, "the same refusal, on the very next tick");
    let after = db
        .with_conn(|c| db::list_gate_attempts_for_spec(c, &spec_id))
        .expect("the journal");
    assert_eq!(
        after
            .iter()
            .map(|a| (a.expect.as_str(), a.outcome.as_str(), a.satisfied))
            .collect::<Vec<_>>(),
        vec![
            ("red", "indeterminate", false),
            ("red", "indeterminate", false),
        ],
        "two unverified attempts, both recorded, neither satisfying anything",
    );
    assert_eq!(after[0].sequence, attempt.sequence, "the first is untouched");

    // THE SPEC IS STILL UNPROVEN, and the auditor is told so in those terms.
    let audit = nyx_core::mcp_context::render_agent_context(
        &db,
        nyx_core::mcp_context::AgentRole::Auditor,
        &spec_id,
        None,
    )
    .expect("renders")
    .expect("bound");
    assert!(
        audit.contains("indeterminate"),
        "the auditor sees the grade that answers nothing: {audit}",
    );
}

// ============================================================================
// Criterion 4 — the manual route
// ============================================================================

/// DONE-CRITERION (« Un bug manuel exige décision d'impraticabilité, preuve avant, preuve après et
/// validations humaines persistées. »)
///
/// THE BRANCH EXISTS BECAUSE A GATE CANNOT READ A SENTENCE. The `bug` discipline announces an exit
/// for a defect no command can reproduce; announced in prose alone that exit would be worse than
/// absent, because the blocking `expect: red` nothing can satisfy would park the run at that node
/// with NO QUESTION EVER PUT. So the graph asks — and here the human rules `manual` on a project
/// whose `repro_check` is bound to a check that would have REFUSED the run had it been reached
/// (an always-green check answers no red). The run reaching `done` at all is the proof the route
/// bypasses the GATE; the two recorded observations are the proof it does not bypass the OBLIGATION.
///
/// The human is real: "the SAME protocol, after the fix" writes the key a loop over a spawning body
/// exits on, so nyx classifies the answer as a commitment and refuses it from a session it has not
/// witnessed a human speak on. A dispatcher may answer the impracticability question by itself and
/// still cannot close the branch alone.
#[test]
fn a_manual_bug_persists_both_human_observations_and_runs_no_gate() {
    let tmp = temp_dir("manual-bug");
    let db = boot(&tmp);
    let (project_id, workspace_id, _ws) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    classify(&db, &spec_id, "bug", db::DomainEventSource::Mcp);
    declare_phases(&db, &spec_id, &["Build"]);
    let task_id = build_task(&db, &project_id, &spec_id, "repair the defect");
    // Bound to a check that can NEVER answer `expect: red`: had the automated route been taken,
    // this run would be parked on the reproduction gate instead of finishing.
    let never_red = always_green_command(&db, &project_id, tmp.path(), "never-red");
    bind_slot(&db, &project_id, nyx_core::seed::REPRO_CHECK_SLOT, &[&never_red]);
    bind_slot(&db, &project_id, nyx_core::seed::BUILD_CHECK_SLOT, &[&never_red]);

    const PROTOCOL: &str = "unplug the reader, scan the badge, watch the LED stay red";
    let terminal = "term-manual-repro-e2e";
    let activity = nyx_core::agent_activity::AgentActivityStore::new();
    let human_says = |text: &str| {
        nyx_core::mcp_tools_core::agent_session_event(
            &db,
            &noop_runner(),
            &activity,
            &json!({
                "NYX_TERMINAL_ID": terminal,
                "hook_event_name": "UserPromptSubmit",
                "prompt": text,
            }),
        )
        .expect("the UserPromptSubmit hook is accepted");
    };
    let caller = CallerSession {
        terminal_id: Some(terminal),
    };

    let rt = EngineRuntime::new();
    let (_parent, child, _w) = coder_child_of_ndev(&db, &rt, &spec_id);

    let mut asks: Vec<String> = Vec::new();
    let mut quoted_back = false;
    let mut after_observations = 0;
    let mut last: Option<Value> = None;
    for _ in 0..40 {
        human_says("at the keyboard, running the protocol");
        let out = nyx_next(&db, &rt, caller, &tick_args(&child, last.take()))
            .expect("nyx_next")
            .result;
        let ty = out["action"]["type"].as_str().expect("a type").to_string();
        if ty == "done" {
            break;
        }
        let prompt = out["action"]["prompt"].as_str().unwrap_or_default().to_string();
        last = Some(match ty.as_str() {
            "spawn_subagent" => json!({ "diff": "d" }),
            "ask_human" => {
                let question = coder_question(&prompt);
                asks.push(question.to_string());
                match question {
                    "automatable?" => json!({ "decision": "manual" }),
                    "the protocol + the BEFORE observation" => json!({ "decision": PROTOCOL }),
                    _ => {
                        // THE SAME PROTOCOL — and the question proves it by QUOTING it. "run the one
                        // you wrote" is a request; the text on the screen is an imposition.
                        assert!(
                            prompt.contains(PROTOCOL),
                            "the after-question quotes the protocol verbatim: {prompt}",
                        );
                        quoted_back = true;
                        after_observations += 1;
                        if after_observations == 1 {
                            json!({ "decision": "still-broken" })
                        } else {
                            json!({ "decision": "fixed" })
                        }
                    }
                }
            }
            other => panic!("unexpected coder action '{other}': {out}"),
        });
    }

    assert_eq!(
        asks,
        vec![
            "automatable?".to_string(),
            "the protocol + the BEFORE observation".to_string(),
            "the SAME protocol, after".to_string(),
            "the SAME protocol, after".to_string(),
        ],
        "the impracticability decision, the BEFORE observation, then the AFTER one — twice, \
         because the first said the defect was still there and that buys a repair pass",
    );
    assert!(quoted_back);
    assert_eq!(
        db.with_conn(|c| db::get_task(c, &task_id))
            .unwrap()
            .unwrap()
            .status,
        db::TASK_STATUS_DONE,
        "the manual route CLOSES the task — it is a declared route, not a dead end",
    );

    // THE DOUBLE VALIDATION, PERSISTED: two keys side by side on the stored board, neither
    // overwriting the other — which is what makes them a PAIR an auditor can compare.
    let board: Value = serde_json::from_str(
        &db.with_conn(|c| db::get_workflow_run(c, &child))
            .unwrap()
            .unwrap()
            .blackboard,
    )
    .expect("a board");
    assert_eq!(board["manual_repro_before"], json!(PROTOCOL));
    assert_eq!(board["manual_repro_after"], json!("fixed"));

    // AND NO REPRODUCTION GATE RAN. The only attempt is the build control — proof the route skipped
    // the gate and nothing else, since an always-green check could never have answered `expect: red`.
    assert_eq!(
        journal(&db, &spec_id)
            .into_iter()
            .map(|(_e, _o, _s, slot)| slot.unwrap_or_default())
            .collect::<Vec<_>>(),
        vec![nyx_core::seed::BUILD_CHECK_SLOT.to_string()],
    );

    // …WHICH IS EXACTLY WHY THE AUDITOR'S CONTEXT HAD TO LEARN THIS ROUTE. One attempt, green, and
    // no red: read off the gate journal alone, this Spec looked like a fix with no reproduction, and
    // the auditor was told in so many words that "the reproduction is missing — and an absent red on
    // a `bug` Spec is a finding of its own". About a proof that exists, carried by the graph, and
    // validated TWICE by a human. So the context now renders the pair, and reads it as the proof.
    let audit = nyx_core::mcp_context::render_agent_context(
        &db,
        nyx_core::mcp_context::AgentRole::Auditor,
        &spec_id,
        None,
    )
    .expect("the auditor context renders")
    .expect("…and it is bound to this Spec");
    assert!(
        audit.contains(PROTOCOL) && audit.contains("BEFORE the fix — the protocol"),
        "the protocol and the BEFORE observation reach the only role that judges them:\n{audit}",
    );
    assert!(
        audit.contains("AFTER the fix — the SAME protocol"),
        "…and so does the AFTER observation, named as the same protocol:\n{audit}",
    );
    assert!(
        audit.contains(&child),
        "…with the run they were recorded on, so the full text is reachable:\n{audit}",
    );
    assert!(
        audit.contains("The reproduction is NOT missing"),
        "…and the sequence reading names the manual route as the proof:\n{audit}",
    );
    for absent in [
        "the reproduction is missing",
        "NO satisfied red",
        "none recorded for this Spec",
    ] {
        assert!(
            !audit.contains(absent),
            "no rendered phrase calls this Spec's proof absent — found '{absent}':\n{audit}",
        );
    }
    // The build control's attempt is still there, as the attempt it is: the manual pair does not
    // hide the journal, it stops the journal from being read as the whole sequence.
    assert!(
        audit.contains("1 recorded gate attempt") && audit.contains("1. `passed`"),
        "the build attempt is still rendered under the pair:\n{audit}",
    );
}

// ============================================================================
// Criterion 5 — classify before routing, and no false alarm on a feature
// ============================================================================

/// DONE-CRITERION (« Une Spec legacy sans catégorie est classée avant le routage ; une feature ne
/// reçoit pas de faux warning de repro. ») — the two halves, which are the same decision read from
/// both ends.
///
/// THE HOLE THE PREFLIGHT SHUTS. `spec_category` answers `null` for an unclassified Spec, `null`
/// matches no condition, and the "bug discipline?" selector's safe default is "no reproduction
/// owed" — so before revision 24 a Spec nobody had classified walked past the red-before-green
/// obligation having never met it. That is not an exotic population: it is every Spec authored
/// before PDR-0017 and every `spec_create` without a `category`.
///
/// THE FALSE ALARM THE SCAN AVOIDS is the mirror image. `run_start` promises what a run will hit,
/// and the reproduction gate lives on a branch only a `bug` takes. Reporting an unbound
/// `repro_check` to every `/ndev` on every `feature` would be a wrong warning on the overwhelming
/// majority of runs — and the surest way to teach a dispatcher to skim past the array that also
/// carries the `build_check` warning, which is true every time. Note what is NOT weakened: the
/// runtime refusal is unconditional, and the test below proves the scan's silence is about the
/// WARNING alone.
#[test]
fn a_legacy_spec_is_classified_before_the_build_routes_and_a_feature_draws_no_repro_warning() {
    let tmp = temp_dir("preflight-and-scan");
    let db = boot(&tmp);
    let (project_id, workspace_id, _ws) = live_project(&db, tmp.path());
    let rt = EngineRuntime::new();

    // --- HALF ONE: a legacy Spec, carrying nothing, is CLASSIFIED before anything routes ---------
    let legacy = spec_in_progress(&db, &project_id, &workspace_id);
    declare_phases(&db, &legacy, &["Build"]);
    build_task(&db, &project_id, &legacy, "repair the defect");
    let anything = always_green_command(&db, &project_id, tmp.path(), "anything");
    bind_slot(&db, &project_id, nyx_core::seed::REPRO_CHECK_SLOT, &[&anything]);
    bind_slot(&db, &project_id, nyx_core::seed::BUILD_CHECK_SLOT, &[&anything]);
    assert_eq!(
        spec_category_slug(&db, &legacy),
        None,
        "precondition: it reaches the build carrying no category at all",
    );

    let (_p, child, warnings) = coder_child_of_ndev(&db, &rt, &legacy);
    assert!(
        !warnings
            .iter()
            .any(|w| w["slot"] == json!(nyx_core::seed::REPRO_CHECK_SLOT)),
        "the scan says nothing about the reproduction slot for a Spec whose genre is not yet \
         decided — it cannot know what the judge will answer: {warnings:?}",
    );

    let preflight = tick(&db, &rt, &child, None);
    assert_eq!(
        preflight["action"]["type"], "judge",
        "THE FIRST THING THE FLOW DOES is ask what genre this is — a NODE, not an instruction: {preflight}",
    );
    assert_eq!(
        preflight["blackboard_delta"]["spec_category"],
        Value::Null,
        "and it fires precisely because the persisted category is null: {preflight}",
    );
    let after = tick(&db, &rt, &child, Some(json!({ "verdict": "bug" })));
    assert_eq!(
        spec_category_slug(&db, &legacy).as_deref(),
        Some("bug"),
        "the judge's word was WRITTEN through the category service, not left on a board",
    );
    assert_eq!(
        after["blackboard_delta"]["spec_category"], json!("bug"),
        "…and READ BACK from the domain before anything routes on it: {after}",
    );
    assert_eq!(
        after["action"]["label"], "write the reproduction",
        "so the Spec nobody had classified lands INSIDE the discipline, not past it: {after}",
    );

    // --- HALF TWO: a `feature`, on a project that has bound NOTHING, draws no repro warning ------
    let tmp2 = temp_dir("feature-no-warning");
    let db2 = boot(&tmp2);
    let (project2, workspace2, _w2) = live_project(&db2, tmp2.path());
    let feature = spec_in_progress(&db2, &project2, &workspace2);
    classify(&db2, &feature, "feature", db::DomainEventSource::Mcp);
    let bug = spec_in_progress(&db2, &project2, &workspace2);
    classify(&db2, &bug, "bug", db::DomainEventSource::Mcp);

    let slots_warned = |db: &Db, spec_id: &str| -> Vec<String> {
        run_start(
            db,
            &EngineRuntime::new(),
            &json!({ "scope": "ndev", "spec_id": spec_id }),
        )
        .expect("run_start")
        .result["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .filter(|w| w["code"] == json!("gate_unconfigured"))
            .filter_map(|w| w["slot"].as_str().map(str::to_string))
            .collect()
    };
    assert_eq!(
        slots_warned(&db2, &feature),
        vec![nyx_core::seed::BUILD_CHECK_SLOT.to_string()],
        "a `feature` is warned about the gate it WILL hit and about nothing else — no reproduction \
         alarm for a branch it never walks",
    );
    let mut bug_slots = slots_warned(&db2, &bug);
    bug_slots.sort();
    bug_slots.dedup();
    assert_eq!(
        bug_slots,
        vec![
            nyx_core::seed::BUILD_CHECK_SLOT.to_string(),
            nyx_core::seed::REPRO_CHECK_SLOT.to_string(),
        ],
        "…while a `bug` on the same unbound project is told about BOTH, before its first spawn",
    );
}

// ============================================================================
// The joints nobody owned
// ============================================================================

/// PDR-0017's two halves, on ONE Spec's real journey: the agent MINTS, the human CURATES — and the
/// persisted trace tells them apart.
///
/// `created_by_type` says who minted the ROW. It does NOT say who took a DECISION, and both halves
/// of PDR-0017's governance are decisions: an agent classifying a Spec on the fly, and a human
/// re-labelling the vocabulary afterwards. Nothing but `domain_events.source` distinguishes them,
/// and nothing but this test walks both through the surfaces that really produce them — the
/// engine's `set_category` node (an agent path, `mcp`) and the curation service the sidecar's
/// commands call (a human path, `ui`).
///
/// SINCE THE 2026-09-02 AMENDMENT of PDR-0017 there is no confirmation: the agent's mint is valid
/// as it stands. The human half is therefore the RENAME (and, from the settings, the create /
/// delete), and `category.confirmed` is an event this build can no longer produce.
#[test]
fn the_agent_that_mints_and_the_human_that_renames_are_told_apart_in_the_journal() {
    let tmp = temp_dir("provenance");
    let db = boot(&tmp);
    let (project_id, workspace_id, _ws) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    declare_phases(&db, &spec_id, &["Build"]);
    build_task(&db, &project_id, &spec_id, "do the work");
    let anything = always_green_command(&db, &project_id, tmp.path(), "anything");
    bind_slot(&db, &project_id, nyx_core::seed::BUILD_CHECK_SLOT, &[&anything]);

    // THE AGENT'S HALF — the preflight's judge answers a label the project's vocabulary does not
    // hold, and PDR-0017's governance is a grill and not a lock: it is MINTED, valid as it stands,
    // never refused.
    let rt = EngineRuntime::new();
    let (_p, child, _w) = coder_child_of_ndev(&db, &rt, &spec_id);
    let preflight = tick(&db, &rt, &child, None);
    assert_eq!(preflight["action"]["type"], "judge");
    tick(&db, &rt, &child, Some(json!({ "verdict": "Flaky Test" })));

    let minted = db
        .with_conn(|c| db::list_categories(c, &project_id))
        .expect("categories")
        .into_iter()
        .find(|c| c.name == "Flaky Test")
        .expect("an unknown label MINTS a category rather than blocking the build");
    assert_eq!(
        minted.slug, "flaky-test",
        "slugified from the name — ASCII, lowercase, hyphens",
    );
    assert_eq!(minted.created_by_type, "agent");
    assert!(
        minted.confirmed_at.is_some(),
        "VALID AS IT STANDS — the agent needs nobody's word (PDR-0017 amended 2026-09-02)"
    );
    assert_eq!(spec_category_slug(&db, &spec_id).as_deref(), Some("flaky-test"));

    // THE HUMAN'S HALF — the curation gesture the sidecar exposes, whose provenance the service
    // FREEZES at `ui` because renaming is human by definition.
    db.with_conn(|c| {
        nyx_core::category::rename(
            c,
            &project_id,
            nyx_core::category::CategoryRef::Id(&minted.id),
            "Test instable",
        )
    })
    .expect("rename");

    let sources: Vec<(String, String)> = db
        .with_conn(|c| db::list_domain_events(c))
        .expect("events")
        .into_iter()
        .filter(|e| e.event_type.starts_with("category.") || e.event_type == "spec.category_set")
        .map(|e| (e.event_type, e.source))
        .collect();
    assert!(
        sources.contains(&("category.created".to_string(), "mcp".to_string())),
        "the MINT is filed as the agent path it came from: {sources:?}",
    );
    assert!(
        sources.contains(&("spec.category_set".to_string(), "mcp".to_string())),
        "…and so is the assignment the engine node performed: {sources:?}",
    );
    assert!(
        sources.contains(&("category.renamed".to_string(), "ui".to_string())),
        "the RE-LABELLING is filed as a human gesture — the one fact `created_by_type` cannot \
         carry, since the row was and remains agent-minted: {sources:?}",
    );
    assert!(
        !sources
            .iter()
            .any(|(kind, _)| kind == "category.confirmed"),
        "`category.confirmed` was RETIRED with the proposed/confirmed axis: {sources:?}",
    );
    let curated = db
        .with_conn(|c| db::get_category(c, &minted.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        curated.created_by_type, "agent",
        "renaming does NOT rewrite who minted the row — the two facts stay two facts",
    );
    assert_eq!(curated.slug, "flaky-test", "…and never the identity either");

    // ONE PAYLOAD, TWO SURFACES. The chip the renderer draws is derived HERE, once, by the function
    // the sidecar's `category_list` / `category_rename` and the MCP `spec_set_category` all call —
    // so the same row cannot read differently on two screens. And it carries NO curation state at
    // all: the axis is gone from the wire, not merely ignored by the app.
    let payload = nyx_core::mcp_tools_spec::category_payload(&curated);
    assert_eq!(payload["created_by_type"], json!("agent"));
    assert_eq!(payload["slug"], json!("flaky-test"));
    assert_eq!(payload["name"], json!("Test instable"));
    assert!(
        payload.get("state").is_none(),
        "no proposed/confirmed word on the wire: {payload}",
    );
}

/// THE PRD'S EXPLICIT RULING: « une recatégorisation en cours de build ne rejoue pas les gates déjà
/// franchis ».
///
/// The category is deliberately NOT frozen by the lifecycle (it is classification metadata, not a
/// commitment), so it can change while a build is walking. What must not follow is a re-route: the
/// run's cursor is INSIDE the branch its preflight chose, nothing re-walks the selector that chose
/// it, and the attempts already recorded are facts about executions that really happened. A
/// recategorisation is a legitimate act with a legitimate effect — on the NEXT run, whose preflight
/// reads the new slug — and it is not a way out of a proof already under way, nor a way to make one
/// disappear.
///
/// The gesture is placed at the worst possible moment: between the satisfied RED and the GREEN that
/// closes it.
#[test]
fn a_recategorization_mid_build_replays_none_of_the_gates_already_crossed() {
    let tmp = temp_dir("recategorize-midbuild");
    let db = boot(&tmp);
    let (project_id, workspace_id, ws_path) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    classify(&db, &spec_id, "bug", db::DomainEventSource::Mcp);
    declare_phases(&db, &spec_id, &["Build"]);
    build_task(&db, &project_id, &spec_id, "repair the defect");
    let repro = repro_check_command(&db, &project_id, tmp.path(), "repro");
    let build = always_green_command(&db, &project_id, tmp.path(), "build");
    bind_slot(&db, &project_id, nyx_core::seed::REPRO_CHECK_SLOT, &[&repro]);
    bind_slot(&db, &project_id, nyx_core::seed::BUILD_CHECK_SLOT, &[&build]);

    let rt = EngineRuntime::new();
    let (_p, child, _w) = coder_child_of_ndev(&db, &rt, &spec_id);
    let mut red_sequence: Option<i64> = None;
    let mut last: Option<Value> = None;
    for _ in 0..40 {
        let out = tick(&db, &rt, &child, last.take());
        let ty = out["action"]["type"].as_str().expect("a type").to_string();
        if ty == "done" {
            break;
        }
        let label = out["action"]["label"].as_str().unwrap_or_default().to_string();
        last = Some(match ty.as_str() {
            "spawn_subagent" if label == "repair the defect" => {
                // THE RED IS BEHIND US and the green has not run: the exact instant a
                // recategorisation could plausibly be used to escape the obligation.
                let recorded = db
                    .with_conn(|c| db::list_gate_attempts_for_spec(c, &spec_id))
                    .expect("journal");
                assert_eq!(recorded.len(), 1, "one attempt so far: the satisfied red");
                assert_eq!((recorded[0].expect.as_str(), recorded[0].satisfied), ("red", true));
                red_sequence = Some(recorded[0].sequence);
                classify(&db, &spec_id, "chore", db::DomainEventSource::Ui);
                std::fs::write(Path::new(&ws_path).join("fixed"), "repaired").expect("the fix");
                json!({ "diff": "repaired" })
            }
            "spawn_subagent" => json!({ "diff": "d" }),
            "ask_human" => json!({ "decision": "automated" }),
            other => panic!("unexpected coder action '{other}': {out}"),
        });
    }

    assert_eq!(
        spec_category_slug(&db, &spec_id).as_deref(),
        Some("chore"),
        "the recategorisation took effect — it is a legitimate act, never refused",
    );
    let after = db
        .with_conn(|c| db::list_gate_attempts_for_spec(c, &spec_id))
        .expect("journal");
    assert_eq!(
        after
            .iter()
            .map(|a| (a.expect.as_str(), a.outcome.as_str(), a.satisfied))
            .collect::<Vec<_>>(),
        vec![
            ("red", "failed", true),
            ("green", "passed", true),
            ("green", "passed", true),
        ],
        "the run finished the branch it was IN — the reproduction it had already opened is closed \
         by its green, and the build control runs after: {after:?}",
    );
    assert_eq!(
        Some(after[0].sequence),
        red_sequence,
        "and the RED already crossed was neither replayed nor rewritten: same row, same rank in \
         the journal — an append-only proof a recategorisation could erase would not be one",
    );
}
