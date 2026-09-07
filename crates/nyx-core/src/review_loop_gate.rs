//! GATE e2e (v2.5.11 Phase 3, PRD 01KXR1AGWK08H9P800YHXPBCHS / task 01KXR1D06BJ0073YMQK3HT646W)
//! — the FULL review LOOP proven GREEN in ONE in-process scenario, over the real nyx-core surface
//! (the SAME functions the sidecar dispatches: the MCP tools of [`crate::mcp_tools_core`] /
//! [`crate::mcp_tools_spec`] and the shared [`crate::db`] engine). It exercises, end to end:
//!
//!   1. RETOUR by the TWO channels:
//!      (a) CHAT → the MCP verb [`review_submit_feedback`] → a HUMAN, GLOBAL `review_comment`
//!          immediately grilled into a `proposed` finding (agent-owned; provenance on the comment);
//!      (b) VIEWER → an ANCHORED (file/line) HUMAN comment ([`db::insert_review_comment`]) then
//!          [`db::promote_comment_to_finding`] — the exact pair the UI bridge's
//!          `review_comment_post` + `review_task_promote` commands call (that bridge path is proven
//!          at its own layer by
//!          `runtime::db_command::tests::human_comment_promotes_finding_agent_closes_then_comment_resolves`;
//!          here we drive the shared engine those commands wrap so the whole loop lives in one test).
//!   2. GRILL: proposed → structured (the AGENT, via the MCP [`review_task_structure`]) — the grill
//!      collapsed to TWO states (review follow-up #31): a `structured` finding is coder-ready, there
//!      is no human-only `validated` step (nor an MCP validate verb).
//!   3. FINDING = `tasks kind=review` carrying `from_comment_id`, `severity` (agent-owned; a finding
//!      has NO `origin` — it is always agent-created, v24; provenance lives on the comment's author).
//!   4. ROUNDS proven ON THE RETOUR-FINDINGS THEMSELVES: the grilled chat + viewer findings are
//!      PHASED to the last delivered phase (F1, [`db::current_phase_for_spec`], stored as
//!      `reviewing_phase`, F5) and ROUNDED at promotion. The chat + viewer retours land in the SAME
//!      OPEN pass, so they SHARE round "1.1" ([`db::open_round_for_phase`], F2 — one pass = one round
//!      holding N findings, not one round per comment), and an AUDITOR finding raised through
//!      `task_add` while that pass is open joins it — with no `round` passed, because nyx fills it
//!      from the Spec's own pass rather than trusting a sub-agent to know which round it is in. This
//!      is the fix for the gap where a retour stayed `phase=None`/`round=None` (excluded from the rail
//!      gauge) and for the one where every auditor finding stayed `round=NULL`. A DIFFERENT phase
//!      numbers its rounds independently ([`db::next_round_for_phase`]).
//!   5. CODER → done: the agent CLOSES every finding (retour + auditor) through the plain
//!      [`db::transition_task`] `done` path — a finding is agent-owned and agent-closable (v24; there
//!      is no human-only gate on findings). Its source `review_comment` then resolves (human-only)
//!      once its linked findings are done — traced BOTH ways.
//!   6. The Spec STAYS `in_review` throughout: no `in_review → in_progress` bounce (that edge does
//!      not exist), and the rework happens IN PLACE as rounds — a task transition never touches the
//!      Spec status.

use serde_json::json;

use crate::db::{
    self, Db, ResolveCommentError, SpecSections, SpecTransition, TaskTransition, AUTHOR_HUMAN,
    GRILL_PROPOSED, GRILL_STRUCTURED, SIDE_NEW, SPEC_STATUS_IN_REVIEW, TASK_SEVERITY_HIGH,
    TASK_SEVERITY_MEDIUM, TASK_STATUS_DONE,
};
use crate::mcp_tools_core::{review_submit_feedback, review_task_list, review_task_structure};
use crate::mcp_tools_spec::task_add;

/// The `{ ... }` result payload of an MCP tool, panicking on a mis-route or tool error (a green
/// loop must never hit one).
fn tool(
    outcome: Result<crate::mcp_tools_core::ToolOutcome, crate::mcp::RpcError>,
) -> serde_json::Value {
    outcome
        .unwrap_or_else(|e| panic!("tool failed: [{:?}] {}", e.code, e.message))
        .result
}

/// The current status of a Spec.
fn spec_status(db: &Db, spec_id: &str) -> String {
    db.with_conn(|c| db::get_spec(c, spec_id))
        .unwrap()
        .unwrap()
        .status
}

/// THE gate: a chat + viewer retour, grilled into `tasks kind=review`, grouped into rounds
/// per-phase, distinguished by channel + the comment's `author_type` (the finding itself carries
/// no `origin`, v24), resolved when the tasks are done, with the Spec staying `in_review` the
/// whole time (no bounce).
#[test]
fn review_loop_chat_and_viewer_grill_rounds_resolve_stay_in_review() {
    let db = Db::in_memory();

    // --- SETUP: a project + its root workspace + a Spec, driven to `in_review` ---------------
    let (project_id, workspace_id, spec_id) = db.with_conn(|c| {
        let (project, root) = db::create_project(c, "Cockpit", "/work", None).expect("project");
        let (spec, _rev) = db::create_spec(
            c,
            &project.id,
            "Review loop",
            SpecSections {
                problem: "The review loop does not hold.".into(),
                solution: "Findings = tasks kind=review, grouped in rounds.".into(),
                ..Default::default()
            },
        )
        .expect("spec");
        (project.id, root.id, spec.id)
    });

    // MATERIALIZE the phase plan (two phases, first-seen order) via plan slices — a coherent plan
    // is a `ready` pre-condition, and phase 1 = "Delivery" is the phase the rework will hang on.
    let mut delivery_slice_id = String::new();
    for (title, phase) in [("Deliver the loop", "Delivery"), ("Harden", "Hardening")] {
        let added = tool(task_add(
            &db,
            &json!({ "project_id": project_id, "title": title, "spec_id": spec_id, "phase": phase }),
        ));
        if phase == "Delivery" {
            delivery_slice_id = added["task"]["id"].as_str().unwrap().to_string();
        }
    }
    // DELIVER phase 1 (its slice → done) while phase 2 stays open — the STEPPED shape. So the LAST
    // DELIVERED phase is 1 (F1): the retours below rattach to phase 1 (the phase under review), and
    // `reviewing_phase` is posed to 1 at the in_review entry (F5).
    db.with_conn(|c| {
        db::transition_task(c, &delivery_slice_id, TaskTransition::Start, None)
            .expect("start slice");
        db::transition_task(c, &delivery_slice_id, TaskTransition::Done, None)
            .expect("deliver slice");
    });

    // draft ─ready─▶ ready ─start─▶ in_progress ─review─▶ in_review. `ready`/`done`/`cancel` are
    // sensitive (need user_confirmed); `start` et `review` sont routiniers. `start` anchors the
    // workspace.
    db.with_conn(|c| {
        db::transition_spec(c, &spec_id, SpecTransition::Ready, true).expect("ready");
        db::transition_spec_with_workspace(
            c,
            &spec_id,
            SpecTransition::Start,
            true,
            Some(&workspace_id),
        )
        .expect("start");
        db::transition_spec(c, &spec_id, SpecTransition::Review, false).expect("review");
    });
    assert_eq!(
        spec_status(&db, &spec_id),
        SPEC_STATUS_IN_REVIEW,
        "the Spec entered review"
    );
    // F5: `reviewing_phase` is POSED at the in_review entry to the last delivered phase (1 =
    // "Delivery"; phase 2 "Hardening" is still open) — the stable anchor for the retours below.
    assert_eq!(
        db.with_conn(|c| db::get_spec(c, &spec_id))
            .unwrap()
            .unwrap()
            .reviewing_phase,
        Some(1),
        "reviewing_phase is posed to the last delivered phase (1) at in_review entry (F5)"
    );

    // === 1a. CHAT channel → review_submit_feedback → human GLOBAL comment + proposed finding ===
    let chat = tool(review_submit_feedback(
        &db,
        &json!({
            "workspace_id": workspace_id,
            "repository_id": "repo",
            "spec_id": spec_id,
            "body": "the retry loop never terminates on a 5xx",
            "severity": TASK_SEVERITY_HIGH,
        }),
    ));
    let chat_comment_id = chat["comment"]["id"].as_str().unwrap().to_string();
    let chat_finding_id = chat["finding"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        chat["comment"]["author_type"], AUTHOR_HUMAN,
        "the chat retour is authored by the HUMAN (the agent only relays)"
    );
    assert!(
        chat["comment"]["file_path"].is_null(),
        "a chat retour is GLOBAL (no diff anchor)"
    );
    assert!(
        chat["finding"].get("origin").is_none(),
        "a finding carries NO origin (v24 — always agent-created; provenance is on the comment)"
    );
    assert_eq!(chat["finding"]["from_comment_id"], json!(chat_comment_id));
    assert_eq!(chat["finding"]["grill_state"], GRILL_PROPOSED);
    assert_eq!(chat["finding"]["kind"], json!("review"));
    // ROUNDS on the RETOUR itself (the fix): the chat retour-finding is PHASED to the current phase
    // (1 = "Delivery") and ROUNDED to that phase's FIRST rework round — "1.1". A retour is no longer
    // `phase=None`/`round=None` (which the rail gauge excludes); it is a real, visible round.
    assert_eq!(
        chat["finding"]["phase"],
        json!(1),
        "the chat retour-finding is PHASED to the current phase (1), so the rail gauge counts it"
    );
    assert_eq!(
        chat["finding"]["round"],
        json!(1),
        "the chat retour opens phase 1's first rework round → \"1.1\""
    );

    // === 1b. VIEWER channel → anchored HUMAN comment → promote (the UI-bridge pair) ===========
    let viewer_finding = db.with_conn(|c| {
        let comment = db::insert_review_comment(
            c,
            db::NewComment {
                workspace_id: workspace_id.clone(),
                repository_id: "repo".into(),
                spec_id: Some(spec_id.clone()),
                parent_id: None,
                author_type: AUTHOR_HUMAN.into(),
                author_name: "You".into(),
                anchor: Some(db::CommentAnchor {
                    file_path: "src/retry.rs".into(),
                    side: SIDE_NEW.into(),
                    line_start: 40,
                    line_end: 42,
                    blob: None,
                    snippet: Some("loop { .. }".into()),
                    context: None,
                }),
                body: "backoff is unbounded here".into(),
            },
        )
        .expect("anchored viewer comment");
        let finding = db::promote_comment_to_finding(
            c,
            &comment,
            "Bound the backoff",
            Some(TASK_SEVERITY_MEDIUM),
        )
        .expect("promote viewer comment");
        (comment, finding)
    });
    let (viewer_comment, viewer_finding) = viewer_finding;
    assert_eq!(
        viewer_comment.file_path.as_deref(),
        Some("src/retry.rs"),
        "the viewer retour is ANCHORED (file/line)"
    );
    assert_eq!(
        viewer_finding.from_comment_id.as_deref(),
        Some(viewer_comment.id.as_str())
    );
    // F2: a SECOND retour raised in the SAME open pass (the chat finding above is still open) SHARES
    // the phase's OPEN round — "1.1" — rather than opening a new one. One review pass = ONE round
    // holding N findings across BOTH channels (chat + viewer), not one round per comment.
    assert_eq!(
        viewer_finding.phase,
        Some(1),
        "the viewer retour-finding is PHASED to the phase under review (1)"
    );
    assert_eq!(
        viewer_finding.round,
        Some(1),
        "the SECOND retour of the SAME open pass SHARES phase 1's round 1 (\"1.1\") — not a new round"
    );

    // === 2. GRILL: proposed → structured (AGENT via MCP) — structured is coder-ready (#31) =====
    for id in [&chat_finding_id, &viewer_finding.id] {
        // The agent STRUCTUREs (via the MCP tool) — proposed → structured. This is the grill's
        // FINAL step: a structured finding is coder-ready (no human-only validate step, #31).
        let structured = tool(review_task_structure(
            &db,
            &json!({
                "id": id,
                "title": "Fix the retry backoff",
                "details": "Cap retries and terminate on non-retryable 5xx.",
            }),
        ));
        assert_eq!(
            structured["task"]["grill_state"], GRILL_STRUCTURED,
            "the agent's structure step moves the grill to `structured` (coder-ready)"
        );
        // Re-read the structured finding off the store to confirm the grill PRESERVES the phase +
        // round posed at promotion — the retour stays a real, phased round through to the coder (it
        // is never reset to Unphased).
        let stored = db.with_conn(|c| db::get_task(c, id)).unwrap().unwrap();
        assert!(
            stored.is_finding_ready_for_coder(),
            "a structured finding is ready for the coder (the grill's final state, #31)"
        );
        assert_eq!(
            stored.phase,
            Some(1),
            "the grill keeps the retour PHASED (phase 1) — it stays in the rail gauge"
        );
        assert!(
            stored.round.is_some_and(|r| r >= 1),
            "the grill keeps the retour ROUNDED (round ≥ 1)"
        );
    }

    // The AGENT MCP tool set has NO validate: the coder consumes `structured` findings directly, and
    // the grilled retours now appear in the Spec's finding list.
    let grilled = review_task_list(
        &db,
        &json!({ "workspace_id": workspace_id, "repository_id": "repo", "spec_id": spec_id }),
    )
    .unwrap();
    let grilled = grilled["tasks"].as_array().unwrap();
    assert_eq!(
        grilled.len(),
        2,
        "both retours are grilled findings on the Spec"
    );
    assert!(
        grilled.iter().all(|t| t.get("origin").is_none()),
        "grilled findings carry NO origin (v24 — always agent-created)"
    );

    // === 4. ROUNDS: proven ON THE RETOUR-FINDINGS THEMSELVES (not on `task_add` proxies) =======
    // Phase 1 = "Delivery". The chat + viewer retours were raised in ONE open pass, so both persist
    // the SAME round "1.1" (F2). Re-read them off the store to confirm the shared per-phase round.
    let round_of = |fid: &str| -> Option<i32> {
        db.with_conn(|c| db::get_task(c, fid))
            .unwrap()
            .unwrap()
            .round
    };
    assert_eq!(
        round_of(&chat_finding_id),
        Some(1),
        "the chat retour persists round 1 (\"1.1\")"
    );
    assert_eq!(
        round_of(&viewer_finding.id),
        Some(1),
        "the viewer retour shares round 1 (\"1.1\") — same open pass, one round holds both (F2)"
    );
    // The AUDITOR raises its finding through `task_add` and passes NO round — it cannot know which
    // round it is in, and making the axis depend on a sub-agent remembering to pass it is exactly how
    // every finding of a campaign ended up with `round = NULL`. nyx fills it: the retour findings of
    // round 1 are still OPEN, so the auditor's finding joins THAT pass rather than opening a new one
    // (F2 again — one pass = one round holding N findings, whatever channel raised them). Phase 1's
    // NEXT round is 2, and that is what a pass raised once this one settles gets.
    assert_eq!(
        db.with_conn(|c| db::next_round_for_phase(c, &spec_id, 1))
            .unwrap(),
        2,
        "the open pass holds round 1 → phase 1's next round is 2 (\"1.2\")"
    );
    let auditor = tool(task_add(
        &db,
        &json!({
            "project_id": project_id,
            "title": "Auditor: missing timeout guard",
            "spec_id": spec_id,
            "kind": "review",
            "severity": TASK_SEVERITY_HIGH,
            "phase": "Delivery",
        }),
    ));
    let auditor_id = auditor["task"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        auditor["task"]["phase"],
        json!(1),
        "the auditor finding is PHASED (phase 1) — it also rides the per-phase rounds"
    );
    assert_eq!(
        auditor["task"]["round"],
        json!(1),
        "…and nyx ROUNDS it into the pass that is open — \"1.1\" — without the auditor saying so"
    );
    assert!(
        auditor["task"].get("origin").is_none(),
        "the auditor finding carries NO origin (v24 — a finding is always agent-created)"
    );

    // A DIFFERENT phase numbers its rounds INDEPENDENTLY (phase 2 = "Hardening", first round 1).
    assert_eq!(
        db.with_conn(|c| db::next_round_for_phase(c, &spec_id, 2))
            .unwrap(),
        1,
        "phase 2's rounds are independent (its first is \"2.1\", not carried over from phase 1)"
    );

    // === 5. CODER → done, then RESOLVE (human-only for human findings) ========================
    // The coder picks each finding up (todo → in_progress), then CLOSES it via the plain `done`
    // path — a finding is agent-owned and agent-closable, whatever its channel (v24: no human-only
    // gate on findings). Both the auditor finding AND the chat/viewer retour-findings close the SAME
    // way.
    for id in [&auditor_id, &chat_finding_id, &viewer_finding.id] {
        db.with_conn(|c| db::transition_task(c, id, TaskTransition::Start, None))
            .expect("coder starts the finding");
        let closed = db
            .with_conn(|c| db::transition_task(c, id, TaskTransition::Done, None))
            .expect("the agent closes the finding via the plain done path");
        assert_eq!(
            closed.status, TASK_STATUS_DONE,
            "the finding is closed by the agent loop (no human-only refusal)"
        );
    }

    // === COMMENT ↔ FINDINGS: 2-way trace + human-only comment resolution once findings done ====
    // Forward: the comment → its findings. Back: the finding → its comment.
    let chat_links = db
        .with_conn(|c| db::findings_for_comment(c, &chat_comment_id))
        .unwrap();
    assert_eq!(
        chat_links.len(),
        1,
        "the chat comment → its one grilled finding"
    );
    assert_eq!(chat_links[0].id, chat_finding_id);
    assert_eq!(
        chat_links[0].from_comment_id.as_deref(),
        Some(chat_comment_id.as_str()),
        "the finding traces BACK to its source comment"
    );

    // The comment resolves (HUMAN-only) ONLY once its linked findings are done — it is done now.
    let resolved_chat_comment = db
        .with_conn(|c| db::resolve_review_comment_when_findings_done(c, &chat_comment_id))
        .expect("the chat comment resolves now its finding is done");
    assert_eq!(
        resolved_chat_comment.resolved_by_type.as_deref(),
        Some(AUTHOR_HUMAN),
        "comment resolution is stamped HUMAN (human-only)"
    );

    // GUARD proof: a comment with a still-open finding is REFUSED. Post a fresh retour, leave its
    // finding open, and confirm the resolution is refused (naming the pending count).
    let open_retour = tool(review_submit_feedback(
        &db,
        &json!({
            "workspace_id": workspace_id,
            "repository_id": "repo",
            "spec_id": spec_id,
            "body": "one more thing (left open on purpose)",
        }),
    ));
    let open_comment_id = open_retour["comment"]["id"].as_str().unwrap().to_string();
    let refused =
        db.with_conn(|c| db::resolve_review_comment_when_findings_done(c, &open_comment_id));
    assert!(
        matches!(
            refused,
            Err(ResolveCommentError::FindingsNotDone { pending: 1 })
        ),
        "a comment with an open finding cannot resolve (you resolve in knowledge of the fix)"
    );

    // === 6. THE SPEC STAYED `in_review` THE WHOLE TIME — no bounce, rounds carried the rework ==
    assert_eq!(
        spec_status(&db, &spec_id),
        SPEC_STATUS_IN_REVIEW,
        "after the entire review loop the Spec is STILL in_review (in-place, no bounce to in_progress)"
    );
    assert!(
        !SpecTransition::Start.source_allowed(SPEC_STATUS_IN_REVIEW),
        "there is NO in_review → in_progress edge — the bounce cannot happen by construction"
    );
    assert!(
        !SpecTransition::Review.source_allowed(SPEC_STATUS_IN_REVIEW),
        "review is not re-enterable from in_review — it stays put, no self-churn"
    );
    assert!(
        SpecTransition::Done.source_allowed(SPEC_STATUS_IN_REVIEW),
        "the only forward exit from in_review is done (with the human's confirmation)"
    );
}
