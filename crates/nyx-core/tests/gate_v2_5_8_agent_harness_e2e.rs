//! GATE e2e (v2.5.8 agent harness, PRD 01KXC80E2CDSNN9PPMJFJFE3X7 / PDR-0020) — the
//! Phase-4 done-criterion, proven GREEN and reproducible: a full **create → dev → doc**
//! cycle is drivable through the native nyx MCP tools, with the role harness rendered LIVE
//! at each step and ZERO depot in the path.
//!
//! It exercises the REAL tool surface the sidecar dispatches (the exact functions
//! [`nyx_core::mcp_tools_core::dispatch_extension_tool`] delegates to):
//!   * `spec_create` / `spec_update_section` / `task_add` / `task_add_dependency` /
//!     `spec_transition` / `task_transition` — via [`dispatch_spec_tool`] (the same routing
//!     the MCP server uses), i.e. MCP = the actions;
//!   * `agent_context` — via [`dispatch_context_tool`] — renders each role's manual (spec →
//!     dev → coder/auditor → doc) LIVE from nyx state (the Spec's sections, the CURRENT
//!     phase resolved from task status, the workspace + repos);
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
use nyx_core::db::{self, Db};
use nyx_core::mcp::RpcCode;
use nyx_core::mcp_context::dispatch_context_tool;
use nyx_core::mcp_tools_core::{
    agent_session_event, create_terminal, review_comment_list, review_comment_post,
    review_task_list, review_task_structure, NoTerminalHost, ToolOutcome,
};
use nyx_core::mcp_tools_spec::dispatch_spec_tool;
use serde_json::{json, Value};

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
    // N17(b): `spec_transition` is no longer routed to the AGENT (nyx imposes every Spec transition
    // at the graph node). The FUNCTION + its guards remain — the UI uses them, and this harness drives
    // the lifecycle through it directly (the confirmation guard it asserts lives on the function).
    let outcome = if name == "spec_transition" {
        Some(nyx_core::mcp_tools_spec::spec_transition(db, &args))
    } else {
        dispatch_spec_tool(db, name, &args)
    };
    outcome
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
        spec_manual.contains("=== NYX CONTEXT — SPEC ==="),
        "spec header"
    );
    assert!(spec_manual.contains("Spec Author"), "spec-author preamble");
    assert!(
        spec_manual.contains("the human DUMPS raw, you GRILL")
            || spec_manual.contains("The human DUMPS raw, you GRILL"),
        "the grill contract is in the manual",
    );
    assert!(
        spec_manual.contains("the human validates the SPEC, you decide the TASKS"),
        "autonomous slicing rule",
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
        json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
    );
    let started = spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "transition": "start", "user_confirmed": true, "workspace_id": workspace_id }),
    );
    assert_eq!(started.result["spec"]["status"], "in_progress");
    assert_eq!(
        started.result["spec"]["workspace_id"], workspace_id,
        "the start transition posed the dev workspace anchor (so agent_context(role=dev) resolves)",
    );

    // --- DEV (/ndev): v2.8 — the dev context is now a THIN DISPATCHER (annex
    //     `context-dev-dispatcher`). It calls `nyx_next` and performs EXACTLY the atomic action nyx
    //     returns — it orients NOTHING itself (no state rule, no per-phase cadence, no review loop
    //     in the session: that whole orchestration is the behavior-tree graph). The render still
    //     weaves the LIVE current phase resolved from the task statuses. ---
    let dev_manual = render(&db, json!({ "role": "dev", "workspace_id": workspace_id }));
    assert!(
        dev_manual.contains("=== NYX CONTEXT — DEV ==="),
        "dev header"
    );
    assert!(
        dev_manual.contains("Dev — dispatcher"),
        "thin dispatcher preamble"
    );
    assert!(
        dev_manual.contains("You do NOT orchestrate") && dev_manual.contains("nyx_next"),
        "dispatcher mandate: call nyx_next, do the returned action, orchestrate nothing",
    );
    assert!(
        dev_manual.contains("orchestrate, loop, branch, or transition on your own initiative"),
        "the session never loops/branches/transitions of its own initiative",
    );
    // The ex-orchestrator prose (state rule, 2-axis auditor drive, review loop) is GONE from the
    // session — it now lives in the graph, not the dispatcher context.
    for gone in [
        "Dev Orchestrator",
        "The state rule",
        "2-axis AUDITOR ONCE",
        "review_comment_list",
    ] {
        assert!(
            !dev_manual.contains(gone),
            "the ex-orchestrator prose `{gone}` must NOT survive in the dispatcher context",
        );
    }
    // Live phase resolution: phase 1 is current (it still has open work), its task is listed,
    // the phase-2 task is NOT. The NUMBER "1" is resolved LIVE from the task phases `task_add`
    // materialized + task statuses; the NAME "Foundation" comes from the plan `task_add` built
    // from the names — together they render the "N · Name" headline, end-to-end tool-driven.
    assert!(
        dev_manual.contains("Current phase — 1 · Foundation"),
        "current phase NUMBER (1) resolved LIVE from tool-driven task phase+status; name from the auto-built plan",
    );
    assert!(
        dev_manual.contains("wire the bounded channel"),
        "phase-1 task listed"
    );
    assert!(
        !dev_manual.contains("stream rows to the consumer"),
        "phase-2 task excluded from the current-phase list",
    );
    assert_no_depot(&dev_manual, "dev");

    // The dev FORWARDS sub-agents by pulling their manuals itself (generic, no hook): the
    // coder and the auditor render on demand. v2.8 — the auditor has ONE manual: it audits the
    // Spec COMMITMENT its spawn names (`subject`), so the render carries no review "axis" any more.
    let coder_manual = render(&db, json!({ "role": "coder", "spec_id": sid }));
    assert!(coder_manual.contains("Coder Agent"), "coder manual pulls");
    let auditor = render(&db, json!({ "role": "auditor", "spec_id": sid }));
    assert!(
        auditor.contains("=== NYX CONTEXT — AUDITOR ==="),
        "auditor manual pulls"
    );
    assert!(
        auditor.contains("ONE COMMITMENT") && auditor.contains("`subject`"),
        "the auditor audits the commitment its spawn names",
    );
    assert!(
        !auditor.contains("Your axis"),
        "the invented review axes are gone from the manual",
    );

    // --- Run the phases: close phase 1 → the current phase advances to 2 LIVE. ---
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
    let dev_after_p1 = render(&db, json!({ "role": "dev", "spec_id": sid }));
    assert!(
        dev_after_p1.contains("Current phase — 2 · Delivery"),
        "the CURRENT phase NUMBER advanced to 2 (LIVE) once the tool-driven phase-1 task was \
         done — the cadence is agent-driven; the '2 · Delivery' name comes from the auto-built plan",
    );

    // Close phase 2, then (autopilot: all phases done) move the Spec to review.
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
        json!({ "spec_id": sid, "transition": "review" }),
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
        doc_manual.contains("=== NYX CONTEXT — DOC ==="),
        "doc header"
    );
    assert!(doc_manual.contains("Doc-sync"), "doc-sync preamble");
    assert!(
        doc_manual.contains("validate your own output — it is a REVIEW"),
        "doc output is a review the human validates",
    );
    assert!(
        doc_manual.contains("Streaming import pipeline"),
        "the live Spec title is woven into the doc manual",
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
    // All three stubs now inject the DISPATCHER (`role=dev`); nspec/ndoc carry their `scope=` so
    // run_start opens the right flow (v2.8 review N1).
    for (file, marker) in [
        ("nspec.md", "role=dev scope=nspec"),
        ("ndev.md", "role=dev"),
        ("ndoc.md", "role=dev scope=ndoc"),
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
            text.contains(&format!("NYX-AGENT-CONTEXT {marker}")),
            "`{file}` carries the `{marker}` sentinel the hook keys on",
        );
        assert!(
            text.contains("$ARGUMENTS"),
            "`{file}` forwards the human's free text"
        );
        assert!(
            text.contains("agent_context"),
            "`{file}` documents the pull fallback (call agent_context) when the hook is absent",
        );
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
        json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
    );
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "transition": "start", "user_confirmed": true, "workspace_id": workspace_id }),
    );

    // Pull by spec_id.
    assert!(render(&db, json!({ "role": "spec", "spec_id": sid })).contains("Spec Author"));
    // Pull by workspace_id (the anchored dev workspace) — v2.8 thin dispatcher context.
    assert!(
        render(&db, json!({ "role": "dev", "workspace_id": workspace_id }))
            .contains("Dev — dispatcher")
    );
    // Pull by cwd (a path nested under the workspace root) — the hook's `{ role, cwd }` shape.
    let nested = format!("{root}/crates/api");
    assert!(render(&db, json!({ "role": "doc", "cwd": nested })).contains("Doc-sync"));
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

    // `ready` without confirmation → refused by the sensitive-transition guard (tested on the
    // function directly — spec_transition is no longer agent-routed, N17(b)).
    let err = nyx_core::mcp_tools_spec::spec_transition(
        &db,
        &json!({ "spec_id": sid, "transition": "ready" }),
    )
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
        json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
    );
    assert_eq!(ok.result["spec"]["status"], "ready");
    // `start` is sensitive too (it poses the anchor): confirm it + anchor on the workspace.
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "transition": "start", "user_confirmed": true, "workspace_id": workspace_id }),
    );
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "transition": "review" }),
    );

    // `done` (also sensitive) without confirmation → refused too.
    let done_err = nyx_core::mcp_tools_spec::spec_transition(
        &db,
        &json!({ "spec_id": sid, "transition": "done" }),
    )
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
        json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
    );
    spec_tool(
        &db,
        "spec_transition",
        json!({ "spec_id": sid, "transition": "start", "user_confirmed": true, "workspace_id": workspace_id }),
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
