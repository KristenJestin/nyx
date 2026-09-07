//! GATE e2e (v2.5.8 agent harness, PRD 01KXC80E2CDSNN9PPMJFJFE3X7 / PDR-0020, re-pointed at
//! PDR-0031) — the Phase-4 done-criterion, proven GREEN and reproducible: a full
//! **create → dev → doc** cycle is drivable through the native nyx MCP tools, with the role
//! manual rendered LIVE at each step and ZERO depot in the path.
//!
//! WHAT THE PROSE ASSERTIONS ARE PINNED TO. Every string this gate asserts on comes from the
//! DELIVERED manuals — `crates/nyx-core/resources/agent-manuals/{nspec,ndev,ndoc}.md`, which are
//! `docs/product/agent-manuals.md` §§1-3 word for word — or from a `{{…}}` block nyx renders into
//! them. It used to assert on the retired orchestration engine's vocabulary (`nyx_next`, the
//! "thin dispatcher" preamble, a "Current phase — N · Name" headline resolved from task status);
//! none of that text exists any more, and asserting on it made the gate a test of a dead build.
//!
//! It exercises the REAL tool surface the sidecar dispatches (the exact functions
//! [`nyx_core::mcp_tools_core::dispatch_extension_tool`] delegates to):
//!   * `spec_create` / `spec_update_section` / `task_add` / `task_add_dependency` /
//!     `spec_transition` / `task_transition` — via [`dispatch_spec_tool`] (the same routing
//!     the MCP server uses), i.e. MCP = the actions;
//!   * `agent_context` — via [`dispatch_context_tool`] — renders each role's manual LIVE from
//!     nyx state (the Spec's sections, the whole task plan with its blocking edges, the
//!     workspace + repos, the managed commands). THE ROLES ARE THREE — `spec` / `dev` / `doc`,
//!     one per nyx command (PDR-0031, `docs/product/agent-manuals.md` §5 row 5). `coder`,
//!     `auditor` and `prototype` are GONE from this surface: those are sub-agents the /ndev
//!     orchestrator briefs itself, and what nyx gives them is the §4 CHARTER, relayed by the
//!     `SubagentStart` hook (proven in `hook_push_injection_e2e.rs` and in
//!     `mcp_tools_core::tests::agent_session_event_subagent_start_returns_the_charter_as_context`);
//!   * `review_comment_post` / `review_task_structure` / `review_comment_list` /
//!     `review_task_list` — the review loop the dev orchestrator runs.
//!
//! The three done-criterion volets, each proven below:
//!   1. `create_dev_doc_cycle_is_drivable_via_nyx_tools_without_depot` — the cycle, in
//!      process, zero depot (the rendered manuals pilot the workflow through the nyx tools
//!      and NEVER name depot).
//!   2. `user_scope_command_stubs_carry_the_injection_sentinel` + `agent_context_pull_is_the_
//!      universal_fallback_without_any_stub` — multi-provider: the BARE user-scope command
//!      stubs nyx deploys (`~/.claude/commands/`, invocable un-namespaced — finding 01KXEJVZ)
//!      each carry the role sentinel + `$ARGUMENTS`, and the `agent_context` PULL channel
//!      renders the manual with NO command file present at all (the provider-agnostic
//!      fallback). Only Claude Code's `~/.claude/commands/` user-scope dir is modeled; no
//!      user-scope `.agents` dir exists in the codebase, so that provider relies on the PULL
//!      channel (documented limit — we do not invent an un-modeled deploy target).
//!   3. `sessionstart_orientation_line_is_unchanged` + `confirmation_guardrails_are_intact`
//!      — non-regression: the SessionStart "You're in nyx" line keeps its form, and the
//!      sensitive-transition confirmation guard-rail still refuses ready/done without
//!      `user_confirmed`.
//!
//! Overlap with existing unit tests is CITED, not duplicated:
//!   * `mcp_tools_spec::tests::spec_transition_ready_honors_the_confirmation_guard_and_validations`
//!     (the guard through the tool) and `author_and_transition_a_spec_end_to_end_through_the_tools`;
//!   * `mcp_tools_core::tests::mcp_agent_cannot_validate_or_promote_a_task` and
//!     `mcp_agent_cannot_resolve_a_comment` (the human-only guard-rails);
//!   * `mcp_tools_core::tests::agent_session_event_start_returns_minimal_nyx_context`
//!     (the SessionStart line at runtime);
//!   * `plugin::tests::nyx_core_ships_the_user_scope_command_stubs` (the BARE stubs ship in
//!     `resources/claude-commands/`) and
//!     `install_deploys_bare_user_scope_stubs_via_settings_seam_and_uninstall_removes_them`
//!     (the install vehicle deploys them to `~/.claude/commands/` + removes them on uninstall).
//!
//! The REAL PUSH channel (the hook detecting the command invocation → POSTing `agent_context` →
//! emitting `additionalContext`) — which this gate proves only STRUCTURALLY (VOLET 2a: the stub
//! DECLARES what its command triggers) and via the PULL fallback (VOLET 2b) — is exercised
//! end-to-end against the real `nyx-session-hook.cjs` under `node` in the sibling integration
//! test `tests/hook_push_injection_e2e.rs` (review finding 01KXEFFTBEC6MC8S892NEFMJMN), which
//! also binds the hook's command table to the sentinel this gate asserts.

use std::path::PathBuf;

use nyx_core::agent_activity::AgentActivityStore;
use nyx_core::command::{CommandRunner, RunState, RunnerSink};
use nyx_core::db::{self, Db};
use nyx_core::mcp::RpcCode;
use nyx_core::mcp_context::dispatch_context_tool;
use nyx_core::mcp_tools_core::{
    agent_session_event, create_terminal, review_comment_list, review_comment_post,
    review_task_list, review_task_structure, NoTerminalHost, ToolOutcome,
};
use nyx_core::mcp_tools_spec::dispatch_spec_tool;
use serde_json::{json, Value};

/// A [`RunnerSink`] that records nothing, and the EMPTY [`CommandRunner`] built over it —
/// `agent_session_event` READS the runner (a `SessionStart`'s injected context states the FACTUAL
/// run state of the workspace's managed commands), and this gate drives a workspace with no managed
/// command at all, so an empty runner is the faithful shape.
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

/// A RAII guard over a test's on-disk temp dir: it removes the dir on drop, so the gate no
/// longer leaks `nyx-gate-2-5-8-*` dirs under `$TMPDIR`. This is the missing HALF of the
/// on-disk temp-DB pattern the cited sidecar gates apply — `gate_v2_1_identity_e2e.rs`
/// (`std::fs::remove_dir_all` at end of test) and `gate_v2_5_5_agent_lifecycle_e2e.rs` (a
/// `Drop` cleanup) — here as a scope guard so cleanup also survives a panicking assertion.
/// Callers keep it alive with a named `_tmp` binding until the end of the test.
struct TempDir(PathBuf);

impl TempDir {
    /// The temp dir's path (e.g. to join the on-disk `nyx.db`).
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A unique temp dir for a test's on-disk nyx.db (integration tests build the lib without
/// `cfg(test)`, so the in-memory helper is out of reach — the sidecar gate tests use the
/// same on-disk temp DB pattern). Returns a [`TempDir`] guard that cleans up on drop.
fn temp_dir(tag: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "nyx-gate-2-5-8-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// A live nyx project: one project, its root workspace, and one repo. Returns
/// `(tmp, db, project_id, workspace_id, workspace_root, repository_id)`. The first element is
/// the [`TempDir`] guard: the caller MUST bind it (as `_tmp`) for the whole test so the
/// on-disk temp dir is removed when the test ends (drop order removes the dir AFTER `db`
/// closes its connection, since pattern bindings drop in reverse).
fn live_project() -> (TempDir, Db, String, String, String, String) {
    let tmp = temp_dir("db");
    let db = Db::open(&tmp.path().join("nyx.db")).expect("open temp nyx.db");
    let (project_id, workspace_id, workspace_root, repository_id) = db.with_conn(|c| {
        let (project, root) =
            db::create_project(c, "Cockpit", "/tmp/nyx-gate-2-5-8", None).expect("project");
        let repo = db::create_repository(c, &project.id, "api", ".", false).expect("repo");
        (project.id, root.id.clone(), root.root_path.clone(), repo.id)
    });
    (
        tmp,
        db,
        project_id,
        workspace_id,
        workspace_root,
        repository_id,
    )
}

/// Drive a Spec authoring/transition tool through the REAL MCP routing
/// ([`dispatch_spec_tool`]) — the same entry point the sidecar dispatches. Panics on a
/// mis-route or a tool error (a green cycle must never hit one).
fn spec_tool(db: &Db, name: &str, args: Value) -> ToolOutcome {
    // `spec_transition` passe par le MÊME dispatcher que les autres depuis PDR-0031 : il n'y a plus
    // de nœud de graphe pour imposer une transition, l'agent la demande et nyx tranche.
    dispatch_spec_tool(db, name, &args)
        .unwrap_or_else(|| panic!("spec tool `{name}` was not routed"))
        .unwrap_or_else(|e| panic!("spec tool `{name}` failed: [{:?}] {}", e.code, e.message))
}

/// Render a role manual through the REAL `agent_context` routing
/// ([`dispatch_context_tool`]) and return the rendered TEXT.
fn render(db: &Db, args: Value) -> String {
    let out = dispatch_context_tool(db, "agent_context", &args)
        .expect("agent_context is routed")
        .expect("agent_context render ok");
    out.result["context"]
        .as_str()
        .expect("the render carries a `context` string")
        .to_string()
}

/// The nyx-core `resources/claude-commands` source tree — the canonical BARE user-scope
/// command stubs the install vehicle deploys to `~/.claude/commands/` (finding 01KXEJVZ;
/// deliberately OUTSIDE the plugin bundle so Claude Code never namespaces them).
fn commands_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/claude-commands")
}

/// A rendered role manual pilots the workflow through the NYX tools, never through depot:
/// the whole point of the harness is to cut the depot dependency (the v2.4 Gate). "depot"
/// must not appear anywhere in the injected text.
fn assert_no_depot(manual: &str, role: &str) {
    assert!(
        !manual.to_lowercase().contains("depot"),
        "the {role} manual must pilot via nyx tools, never name depot:\n{manual}"
    );
}

// ---------------------------------------------------------------------------
// VOLET 1 — create → dev → doc, drivable via /n… tools, ZERO depot.
// ---------------------------------------------------------------------------

/// THE gate: an agent drives a Spec from authoring, through implementation, to doc-sync
/// ENTIRELY over the native nyx MCP tools, with the role manual rendered LIVE at each
/// step and no depot anywhere. This is the v2.4 "workflow complet sans Depot" Gate, tenu.
#[test]
fn create_dev_doc_cycle_is_drivable_via_nyx_tools_without_depot() {
    let (_tmp, db, project_id, workspace_id, _root, repository_id) = live_project();

    // --- CREATE (/nspec): author a Spec through the tools, PROJECT-scoped and UNANCHORED
    //     (review finding #25 — the dev workspace anchor is posed at the start transition). ---
    let created = spec_tool(
        &db,
        "spec_create",
        json!({
            "project_id": project_id,
            "title": "Streaming import pipeline",
            "problem": "batch imports block the UI for minutes",
            "solution": "stream rows through a bounded channel with backpressure",
        }),
    );
    let sid = created.result["spec"]["id"].as_str().unwrap().to_string();
    assert_eq!(created.result["spec"]["status"], "draft");
    assert!(
        created.result["spec"]["workspace_id"].is_null(),
        "a freshly authored Spec is unanchored (project-scoped) — the anchor comes at start",
    );

    // The author refines a section via the tool (MCP = the action).
    spec_tool(
        &db,
        "spec_update_section",
        json!({
            "spec_id": sid,
            "section": "implementation_decisions",
            "value": "one bounded mpsc, a producer task and a consumer task",
        }),
    );

    // The SPEC-author manual renders LIVE: its authoring contract + the Spec's live sections
    // + the anti-duplication weaving — and it pilots via nyx tools, not depot.
    let spec_manual = render(&db, json!({ "role": "spec", "spec_id": sid }));
    assert!(
        spec_manual.starts_with("# nyx — Spec Author (/nspec)"),
        "the /nspec manual is the delivered §1 text, from its first line: {}",
        spec_manual.lines().next().unwrap_or_default(),
    );
    assert!(
        spec_manual.contains("You turn a raw human intent into a nyx Spec"),
        "the Spec Author mandate",
    );
    assert!(
        spec_manual.contains("## 1. Grill — the part that matters most")
            && spec_manual.contains("Interview the human until nothing is silently assumed"),
        "the grill contract is in the manual",
    );
    assert!(
        spec_manual
            .contains("## 3. Decompose — autonomous, you decide, the human does not review the tasks"),
        "autonomous slicing rule",
    );
    assert!(
        spec_manual.contains("Only on an explicit yes: spec_transition(spec_id, to=\"ready\", user_confirmed=true)"),
        "the manual routes `ready` through spec_transition with the human's explicit confirmation",
    );
    assert!(
        spec_manual.contains("batch imports block the UI")
            && spec_manual.contains("bounded channel with backpressure")
            && spec_manual.contains("bounded mpsc"),
        "the live Spec sections are woven in",
    );
    assert!(
        spec_manual.contains("Existing Specs in the project (anti-duplication)"),
        "anti-duplication weaving",
    );
    assert_no_depot(&spec_manual, "spec");

    // --- SLICE (autonomous): two phases, one task each, an ordering edge. ---
    //
    // The NAMED phase plan MATERIALIZES from `task_add(phase="Name")` (finding
    // 01KXG68DN5DSCFDFA6C87NJHGV): nyx builds `specs.phases = [{number,name}]` from the names
    // the tasks carry (get-or-create, 1-based, in FIRST-SEEN order) — there is NO separate
    // set-phases call, and no task can be orphaned (its phase exists in the plan by
    // construction). The CURRENT phase the dev manual shows is then resolved LIVE from those
    // task phases + statuses (`mcp_context::resolve_current_phase`: lowest phase number with
    // open work) named from the built plan. So the "N · Name" headline below is end-to-end
    // agent-driven — NO out-of-band seed, NO fixture, NO numbering to track.
    let t1 = spec_tool(
        &db,
        "task_add",
        json!({ "project_id": project_id, "title": "wire the bounded channel", "spec_id": sid, "phase": "Foundation" }),
    );
    let t1_id = t1.result["task"]["id"].as_str().unwrap().to_string();
    let t2 = spec_tool(
        &db,
        "task_add",
        json!({ "project_id": project_id, "title": "stream rows to the consumer", "spec_id": sid, "phase": "Delivery" }),
    );
    let t2_id = t2.result["task"]["id"].as_str().unwrap().to_string();
    // The phase NUMBER is materialized by nyx from the NAME: "Foundation" is the first phase
    // seen → 1, "Delivery" the next new name → 2 (this is what `resolve_current_phase` groups
    // the cadence by — aligned with the auto-built plan by construction).
    assert_eq!(
        t1.result["task"]["phase"], 1,
        "task_add(phase=\"Foundation\") materializes phase NUMBER 1 (first-seen) — tool-driven",
    );
    assert_eq!(
        t2.result["task"]["phase"], 2,
        "task_add(phase=\"Delivery\") materializes phase NUMBER 2 (next new name) — tool-driven",
    );
    // The named plan SELF-BUILT in specs.phases from the task names (not an out-of-band seed
    // and NOT a separate set call): the previously-empty plan is now the two named phases,
    // 1-based in first-seen order.
    let phases_after = db
        .with_conn(|c| db::get_spec(c, &sid))
        .expect("read spec")
        .expect("spec exists")
        .phases;
    assert_eq!(
        phases_after, r#"[{"number":1,"name":"Foundation"},{"number":2,"name":"Delivery"}]"#,
        "specs.phases materialized from task_add(phase name) — 1-based, first-seen order",
    );
    // Phase 2 depends on phase 1 (an ordering edge, refused if it would cycle).
    spec_tool(
        &db,
        "task_add_dependency",
        json!({ "task_id": t2_id, "depends_on_task_id": t1_id }),
    );

    // --- TRANSITION: ready (confirmed) → start (confirmed, POSING the workspace anchor in
    //     the same op — review finding #25). start is sensitive (it binds the Spec to a dev
    //     workspace), so it needs user_confirmed AND the workspace_id to anchor on. ---
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
    );
    let started = spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "start", "user_confirmed": true, "workspace_id": workspace_id }),
    );
    assert_eq!(started.result["spec"]["status"], "in_progress");
    assert_eq!(
        started.result["spec"]["workspace_id"], workspace_id,
        "the start transition posed the dev workspace anchor (so agent_context(role=dev) resolves)",
    );

    // --- DEV (/ndev): the delivered §2 manual — an ORCHESTRATOR that sends others to do the
    //     work and judges what comes back. There is no dispatcher, no `nyx_next`, no per-phase
    //     cadence: phases are grouping LABELS and the render hands over the WHOLE plan with its
    //     blocking edges, which is what lets the orchestrator run the frontier in parallel. ---
    let dev_manual = render(&db, json!({ "role": "dev", "workspace_id": workspace_id }));
    assert!(
        dev_manual.starts_with("# nyx — Dev Orchestrator (/ndev)"),
        "the /ndev manual is the delivered §2 text, from its first line: {}",
        dev_manual.lines().next().unwrap_or_default(),
    );
    assert!(
        dev_manual.contains("Your single objective: code that WORKS and RESPECTS the Spec."),
        "the orchestrator's single objective",
    );
    assert!(
        dev_manual.contains("## Rules of context hygiene")
            && dev_manual.contains("Every question about the code goes to an Explore sub-agent"),
        "the context-hygiene rules that make the orchestrator last",
    );
    assert!(
        dev_manual.contains("nyx injects the workspace, the managed commands and its rules")
            && dev_manual.contains("into every sub-agent when it starts; you paste none of it."),
        "the manual states that nyx — not the orchestrator — briefs a sub-agent (the §4 charter)",
    );
    assert!(
        dev_manual.contains("## 3. The review loop") && dev_manual.contains("review_comment_list"),
        "the review loop is back IN the session: the engine that used to own it is gone",
    );
    assert!(
        dev_manual.contains("You may not: edit source files"),
        "the orchestrator's interdictions",
    );
    // No verb of the retired engine survives in what nyx injects — the failure mode this gate
    // exists to catch is a manual sending a live session at a tool that no longer answers.
    for dead in ["nyx_next", "run_start", "workflow_", "slot_bind", "run_id"] {
        assert!(
            !dev_manual.contains(dead),
            "the retired `{dead}` must NOT survive in the injected /ndev manual",
        );
    }
    // The `{{tasks}}` block: the WHOLE plan, both tasks, their statuses and the blocking edge —
    // phases render as LABELS beside them and gate nothing.
    assert!(
        dev_manual.contains("## Tasks — the plan, in order"),
        "the plan block is rendered",
    );
    assert!(
        dev_manual.contains("[todo] wire the bounded channel")
            && dev_manual.contains("[todo] stream rows to the consumer"),
        "BOTH tasks are listed with their live status — nothing is filtered by a current phase:\n{dev_manual}",
    );
    assert!(
        dev_manual.contains("phase=\"Foundation\"") && dev_manual.contains("phase=\"Delivery\""),
        "the phase names materialized by task_add render as LABELS on the task lines",
    );
    assert!(
        dev_manual.contains("blocked by: [todo] wire the bounded channel"),
        "the blocking edge is named on the task it gates — the frontier, readable without walking the graph",
    );
    assert!(
        dev_manual.contains("## Workspace") && dev_manual.contains("## Managed commands of this workspace"),
        "the /ndev render carries the workspace and the managed-commands blocks",
    );
    assert_no_depot(&dev_manual, "dev");

    // THE ROLE SURFACE IS THE THREE COMMANDS. `coder` / `auditor` / `prototype` were manuals of
    // the retired engine; a sub-agent is now briefed by its orchestrator and given nyx's §4
    // CHARTER by the `SubagentStart` hook, so `agent_context` REFUSES them by name.
    for gone in ["coder", "auditor", "prototype"] {
        let err = dispatch_context_tool(&db, "agent_context", &json!({ "role": gone, "spec_id": sid }))
            .expect("agent_context is routed")
            .expect_err("a retired role is refused, not rendered");
        assert_eq!(err.code, RpcCode::InvalidArgument, "role `{gone}`");
        assert!(
            err.message.contains("spec, dev, doc"),
            "the refusal names the three roles that remain, got: {}",
            err.message,
        );
    }

    // --- Run the plan: close the first task → its status moves LIVE in the next render. ---
    spec_tool(
        &db,
        "task_transition",
        json!({ "task_id": t1_id, "transition": "start" }),
    );
    let t1_done = spec_tool(
        &db,
        "task_transition",
        json!({ "task_id": t1_id, "transition": "done" }),
    );
    assert_eq!(t1_done.result["task"]["status"], "done");
    let dev_after_t1 = render(&db, json!({ "role": "dev", "spec_id": sid }));
    assert!(
        dev_after_t1.contains("[done] wire the bounded channel"),
        "the plan block reflects the tool-driven status LIVE:\n{dev_after_t1}",
    );
    assert!(
        dev_after_t1.contains("blocked by: [done] wire the bounded channel"),
        "the second task now reads its blocker as DONE — the frontier opened, and nothing but the \
         task statuses said so",
    );

    // Close the second task, then ASK for review. Nothing auto-advances: the Spec's status is a
    // transition the agent REQUESTS and nyx grants (`specs.mode` is dead — §5 row 3).
    spec_tool(
        &db,
        "task_transition",
        json!({ "task_id": t2_id, "transition": "start" }),
    );
    spec_tool(
        &db,
        "task_transition",
        json!({ "task_id": t2_id, "transition": "done" }),
    );
    let reviewed = spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "review" }),
    );
    assert_eq!(reviewed.result["spec"]["status"], "in_review");

    // --- REVIEW LOOP: a human comes back with a comment → the dev grills it into a task,
    //     structures it, and REPLIES — all through the nyx tools, no depot, no re-prompt. ---
    let comment = db
        .with_conn(|c| {
            db::insert_review_comment(
                c,
                db::NewComment {
                    workspace_id: workspace_id.clone(),
                    repository_id: repository_id.clone(),
                    spec_id: Some(sid.clone()),
                    parent_id: None,
                    author_type: db::AUTHOR_HUMAN.to_string(),
                    author_name: "Kris".into(),
                    anchor: None,
                    body: "the consumer has no timeout — it can hang forever".into(),
                },
            )
        })
        .expect("human comment");
    // Promote onto the CANONICAL finding model (`tasks kind=review`, the v2.5.11 cutover) so the
    // MCP `review_task_structure` (which routes to `db::structure_finding` on `tasks`) can grill it.
    let promoted = db
        .with_conn(|c| {
            db::promote_comment_to_finding(c, &comment, "consumer hangs without a timeout", None)
        })
        .expect("promote to finding");
    assert_eq!(promoted.grill_state.as_deref(), Some(db::GRILL_PROPOSED));

    // The agent STRUCTURES the proposed finding (proposed → structured = coder-ready, #31).
    let structured = review_task_structure(
        &db,
        &json!({
            "id": promoted.id,
            "title": "bound the consumer with a timeout",
            "details": "add a recv timeout + a regression test",
            "severity": "high",
        }),
    )
    .expect("structure");
    assert_eq!(
        structured.result["task"]["grill_state"],
        json!("structured")
    );

    // The agent REPLIES in the comment thread (the result), never resolves it.
    review_comment_post(
        &db,
        &json!({
            "workspace_id": workspace_id,
            "repository_id": repository_id,
            "parent_id": comment.id,
            "body": "Done: added a bounded recv timeout + a regression test.",
        }),
    )
    .expect("reply");

    // The feed carries the human comment AND the agent reply.
    let feed = review_comment_list(
        &db,
        &json!({ "workspace_id": workspace_id, "repository_id": repository_id }),
    )
    .expect("feed");
    let bodies: Vec<String> = feed["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["body"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        bodies.iter().any(|b| b.contains("hang forever")),
        "human comment in feed"
    );
    assert!(
        bodies.iter().any(|b| b.contains("bounded recv timeout")),
        "agent reply in feed",
    );
    // Findings are Spec-scoped since the cutover, so the list is read by `spec_id`.
    let review_tasks = review_task_list(
        &db,
        &json!({ "workspace_id": workspace_id, "repository_id": repository_id, "spec_id": sid }),
    )
    .expect("task list");
    assert_eq!(
        review_tasks["tasks"].as_array().unwrap()[0]["grill_state"],
        json!("structured")
    );

    // --- DOC (/ndoc): the doc-sync manual renders LIVE, still via nyx, no depot. ---
    let doc_manual = render(&db, json!({ "role": "doc", "spec_id": sid }));
    assert!(
        doc_manual.starts_with("# nyx — Doc-sync (/ndoc)"),
        "the /ndoc manual is the delivered §3 text, from its first line: {}",
        doc_manual.lines().next().unwrap_or_default(),
    );
    assert!(
        doc_manual
            .contains("You reconcile the project's documentation with what the Spec actually shipped."),
        "the doc-sync mandate",
    );
    assert!(
        doc_manual.contains("Hand the result to the human as a REVIEW")
            && doc_manual.contains("commit unless the human asks."),
        "doc output is a review the human validates, and nothing is committed behind them",
    );
    assert!(
        doc_manual.contains("Streaming import pipeline"),
        "the live Spec title is woven into the doc manual",
    );
    assert!(
        doc_manual.contains("## Workspace"),
        "the /ndoc render carries the workspace block its Git range is read from",
    );
    assert_no_depot(&doc_manual, "doc");
}

// ---------------------------------------------------------------------------
// VOLET 2 — multi-provider.
// ---------------------------------------------------------------------------

/// (a) The three BARE user-scope command stubs (`resources/claude-commands/`, deployed to
/// `~/.claude/commands/` — finding 01KXEJVZ) each carry their role sentinel + `$ARGUMENTS` +
/// the `agent_context` pull-fallback note: the sentinel DECLARES the run the command opens (the
/// hook's `UserPromptSubmit` PUSH branch mirrors it, bound by `hook_push_injection_e2e`) and the
/// note lets a hook-less provider PULL. MULTI-PROVIDER: only Claude Code's `~/.claude/commands/`
/// user-scope dir is modeled in the codebase; there is NO user-scope `.agents` deploy target,
/// so `.agents`-style providers rely on the universal PULL channel (VOLET 2b) — a documented
/// limit, not an invented deploy. The compiled-in [`nyx_core::plugin::USER_COMMAND_STUBS`]
/// (what the install vehicle writes) is asserted byte-identical to this on-disk source.
#[test]
fn user_scope_command_stubs_carry_the_injection_sentinel() {
    let root = commands_root();
    let compiled = nyx_core::plugin::USER_COMMAND_STUBS;
    // ONE ROLE PER COMMAND, and no `scope=` (PDR-0031 §5 row 6): the role IS what selects a
    // manual now. The old table sent all three to `role=dev` and discriminated on a `scope`
    // naming a node of the retired engine.
    for (file, marker) in [
        ("nspec.md", "role=spec"),
        ("ndev.md", "role=dev"),
        ("ndoc.md", "role=doc"),
    ] {
        let path = root.join(file);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        // The vehicle deploys the compiled-in constant; it MUST equal the on-disk source the
        // push-injection e2e feeds the real hook (no drift between what ships and what deploys).
        let (_, content) = compiled
            .iter()
            .find(|(name, _)| *name == file)
            .unwrap_or_else(|| panic!("{file} is a deployed stub"));
        assert_eq!(
            bytes,
            content.as_bytes(),
            "`{file}`: the deployed (compiled-in) stub must match the on-disk source byte-for-byte",
        );
        let text = String::from_utf8(bytes).expect("utf-8 stub");
        assert!(
            text.contains(&format!("<!-- NYX-AGENT-CONTEXT {marker} -->")),
            "`{file}` carries the `{marker}` sentinel the hook keys on",
        );
        assert!(
            !text.contains("scope="),
            "`{file}` must declare no `scope=` — the engine that gave a scope meaning is retired",
        );
        assert!(
            text.contains("$ARGUMENTS"),
            "`{file}` forwards the human's free text"
        );
        assert!(
            text.contains("agent_context"),
            "`{file}` documents the pull fallback (call agent_context) when the hook is absent",
        );
        // A stub is what the session reads when the injected manual is ABSENT. One that still
        // named a retired verb would aim it at a tool that no longer answers, in exactly the
        // situation where nyx cannot correct it.
        for dead in ["nyx_next", "run_start", "workflow_", "slot_bind", "run_id"] {
            assert!(
                !text.contains(dead),
                "`{file}` still names the retired `{dead}`",
            );
        }
    }
}

/// (b) The `agent_context` PULL channel is the UNIVERSAL fallback: it renders the role
/// manual straight from nyx state with NO command stub present at all (this test never
/// touches the plugin files). It resolves the Spec by spec_id, workspace_id, OR cwd — the
/// three anchors a stub-less provider can supply — so any provider can pull the harness.
#[test]
fn agent_context_pull_is_the_universal_fallback_without_any_stub() {
    let (_tmp, db, project_id, workspace_id, root, _repo) = live_project();
    let created = spec_tool(
        &db,
        "spec_create",
        json!({
            "project_id": project_id,
            "title": "Fallback feature",
            "problem": "p",
            "solution": "s",
        }),
    );
    let sid = created.result["spec"]["id"].as_str().unwrap().to_string();
    // Anchor it on the dev workspace by walking to in_progress (the anchor is posed at the
    // start transition — review finding #25 — so the workspace_id/cwd pulls below resolve it).
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
    );
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "start", "user_confirmed": true, "workspace_id": workspace_id }),
    );

    // Pull by spec_id → the /nspec manual.
    assert!(render(&db, json!({ "role": "spec", "spec_id": sid }))
        .starts_with("# nyx — Spec Author (/nspec)"));
    // Pull by workspace_id (the anchored dev workspace) → the /ndev manual.
    assert!(
        render(&db, json!({ "role": "dev", "workspace_id": workspace_id }))
            .starts_with("# nyx — Dev Orchestrator (/ndev)")
    );
    // Pull by cwd (a path nested under the workspace root) — the hook's `{ role, cwd }` shape.
    let nested = format!("{root}/crates/api");
    assert!(
        render(&db, json!({ "role": "doc", "cwd": nested })).starts_with("# nyx — Doc-sync (/ndoc)")
    );
}

// ---------------------------------------------------------------------------
// VOLET 3 — non-regression.
// ---------------------------------------------------------------------------

/// (a) The SessionStart "You're in nyx" orientation line (FEEDBACK #22) keeps its exact
/// form after the harness delivery: driven at runtime through `agent_session_event`, a
/// workspace-anchored session yields `You're in nyx — project "…" · workspace "…" ·
/// terminal_id=…`. Delivering the /n… harness did not perturb this pre-existing line
/// (which is out of the harness's scope). Runtime twin of the unit test
/// `agent_session_event_start_returns_minimal_nyx_context`.
#[test]
fn sessionstart_orientation_line_is_unchanged() {
    let (_tmp, db, _project, _workspace, root, _repo) = live_project();
    let host = NoTerminalHost;
    let activity = AgentActivityStore::new();

    // A terminal opened AT the workspace root auto-attaches to that workspace.
    let created = create_terminal(&db, &host, &json!({ "cwd": root })).unwrap();
    let tid = created.result["terminal_id"].as_str().unwrap().to_string();
    assert!(
        created.result["workspace_id"].is_string(),
        "the terminal attached to the workspace (so the line carries project + workspace)",
    );

    let out = agent_session_event(
        &db,
        &noop_runner(),
        &activity,
        &json!({
            "hook_event_name": "SessionStart",
            "session_id": "sid-nonreg",
            "cwd": root,
            "NYX_TERMINAL_ID": tid,
        }),
    )
    .unwrap();
    let ctx = out.result["context"]
        .as_str()
        .expect("SessionStart still carries a `context` line");
    assert!(
        ctx.starts_with("You're in nyx — "),
        "the orientation line keeps its exact prefix + separator, got: {ctx}",
    );
    assert!(
        ctx.contains("project \"Cockpit\""),
        "project segment intact: {ctx}"
    );
    assert!(
        ctx.contains("workspace \""),
        "workspace segment intact: {ctx}"
    );
    assert!(
        ctx.contains("terminal_id="),
        "terminal_id segment intact: {ctx}"
    );
}

/// (b) The sensitive-transition confirmation guard-rail is intact through the tool: a
/// `ready` (and a `done`) WITHOUT `user_confirmed` is refused with `invalid_state` +
/// "sensitive"; with the human's confirmation (and a substantive draft) it goes through.
/// The human-only guard-rails on the review side (no validate / no resolve tool) are
/// CITED, not duplicated: `mcp_tools_core::tests::mcp_agent_cannot_validate_or_promote_a_task`
/// and `mcp_agent_cannot_resolve_a_comment`.
#[test]
fn confirmation_guardrails_are_intact() {
    let (_tmp, db, project_id, workspace_id, _root, _repo) = live_project();
    let created = spec_tool(
        &db,
        "spec_create",
        json!({
            "project_id": project_id,
            "title": "Guarded feature",
            "problem": "p",
            "solution": "s",
        }),
    );
    let sid = created.result["spec"]["id"].as_str().unwrap().to_string();

    // `ready` sans confirmation → refusé par le garde-fou, à travers le ROUTAGE réel : c'est bien
    // l'agent qui se fait refuser la transition, pas seulement la fonction.
    let err = dispatch_spec_tool(&db, "spec_transition", &json!({ "spec_id": sid, "to": "ready" }))
        .expect("spec_transition is routed")
        .unwrap_err();
    assert_eq!(err.code, RpcCode::InvalidState);
    assert!(
        err.message.contains("sensitive"),
        "the refusal names the sensitive guard"
    );

    // With the human's confirmation → ready (the draft has problem + solution).
    let ok = spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
    );
    assert_eq!(ok.result["spec"]["status"], "ready");
    // `start` is sensitive too (it poses the anchor): confirm it + anchor on the workspace.
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "start", "user_confirmed": true, "workspace_id": workspace_id }),
    );
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "review" }),
    );

    // `done` (also sensitive) without confirmation → refused too.
    let done_err =
        dispatch_spec_tool(&db, "spec_transition", &json!({ "spec_id": sid, "to": "completed" }))
            .expect("spec_transition is routed")
            .unwrap_err();
    assert_eq!(done_err.code, RpcCode::InvalidState);
    assert!(
        done_err.message.contains("sensitive"),
        "done is guarded too"
    );
}

/// (c) v2.5.8 finding #31: the DRAFT-only `task_delete` is drivable THROUGH the real MCP
/// routing. While the Spec is a draft an erroneous task is REALLY removed (its row AND its
/// dependency edges gone), the Spec's auto-built phase plan stays coherent (the emptied
/// phase entry is kept, so the plan is still a clean 1..N), and an unknown task is
/// `invalid_id`. Once the Spec is in_progress the delete is REFUSED with `invalid_state` —
/// there `skip` is the legitimate move (it keeps the audit trail).
#[test]
fn task_delete_is_draft_only_through_the_dispatch() {
    let (_tmp, db, project_id, workspace_id, _root, _repo) = live_project();
    let created = spec_tool(
        &db,
        "spec_create",
        json!({
            "project_id": project_id,
            "title": "Deletable draft",
            "problem": "p",
            "solution": "s",
        }),
    );
    let sid = created.result["spec"]["id"].as_str().unwrap().to_string();

    // Author three phased tasks (two named phases) + a dependency edge, all in draft.
    let add = |title: &str, phase: &str| {
        spec_tool(
            &db,
            "task_add",
            json!({ "project_id": project_id, "title": title, "spec_id": sid, "phase": phase }),
        )
        .result["task"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let a = add("erroneous slice", "Foundation");
    let b = add("keep me", "Delivery");
    let _c = add("also delivery", "Delivery");
    spec_tool(
        &db,
        "task_add_dependency",
        json!({ "task_id": a, "depends_on_task_id": b }),
    );

    // Unknown task → invalid_id.
    let err = dispatch_spec_tool(&db, "task_delete", &json!({ "task_id": "no-such-task" }))
        .expect("task_delete routed")
        .unwrap_err();
    assert_eq!(err.code, RpcCode::InvalidId);

    // DRAFT: delete the erroneous task — it is REALLY gone, and so is its dependency edge.
    let out = spec_tool(&db, "task_delete", json!({ "task_id": a }));
    assert_eq!(out.result["deleted"], true);
    assert!(
        db.with_conn(|c| db::get_task(c, &a)).unwrap().is_none(),
        "the deleted task is gone from the DB"
    );
    assert!(
        db.with_conn(|c| db::list_task_dependency_edges(c, &a))
            .unwrap()
            .is_empty(),
        "the deleted task's dependency edge is cleaned up"
    );
    // The Spec's phase plan is untouched and still coherent (the emptied "Foundation" entry
    // is kept — a clean 1..N — so the ready gate would still pass).
    let phases = db
        .with_conn(|c| db::get_spec(c, &sid))
        .unwrap()
        .unwrap()
        .phases;
    assert_eq!(
        phases, r#"[{"number":1,"name":"Foundation"},{"number":2,"name":"Delivery"}]"#,
        "the phase plan stays a coherent 1..N after the delete",
    );

    // Walk the Spec to in_progress, then a delete is REFUSED (skip is the legit move there).
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
    );
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "to": "start", "user_confirmed": true, "workspace_id": workspace_id }),
    );
    let refused = dispatch_spec_tool(&db, "task_delete", &json!({ "task_id": b }))
        .expect("task_delete routed")
        .unwrap_err();
    assert_eq!(refused.code, RpcCode::InvalidState);
    assert!(
        refused.message.contains("skip"),
        "the refusal points at skip as the legitimate move, got: {}",
        refused.message
    );
    assert!(
        db.with_conn(|c| db::get_task(c, &b)).unwrap().is_some(),
        "a refused delete removed nothing"
    );
}
