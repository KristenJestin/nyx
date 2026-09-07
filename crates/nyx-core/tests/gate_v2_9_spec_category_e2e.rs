//! GATE e2e (v2.9 — « Catégorie unique par Spec (PDR-0017), et la DISCIPLINE que cette catégorie
//! injecte dans le contexte d'un agent », PRD `01KYA6K26JBYETDMDXYGZVVA03`, Phase 9 task
//! `01KYA6VCVMQZ6RJZQD97DXVB3F`) — the *juge de paix* of the category axis, walked on the REAL BOOT
//! PATH and through the REAL agent surfaces.
//!
//! # What this gate proves, and what the engine's withdrawal took away
//!
//! PDR-0017 gives a Spec ONE category, and ADR-0006 hangs a real obligation on one of them: a Spec
//! classified `bug` owes a red-before-green proof. Until PDR-0031 that obligation was EXECUTED — a
//! behaviour-tree branch of the seeded `coder` graph ran a reproduction gate `expect: red`, then the
//! same control `expect: green`, and a journal of gate attempts carried the evidence. The engine is
//! withdrawn; the six engine tables, the seeded graphs, the gate journal and the auditor role are
//! gone with it.
//!
//! WHAT SURVIVES IS THE AXIS AND THE TEXT, and both are load-bearing:
//!
//!   * the CATEGORY itself — the `categories` table, its per-project vocabulary, its boot backfill,
//!     `spec_set_category`, and the provenance trail that tells an agent's mint from a human's
//!     curation (`domain_events.source`);
//!   * the DISCIPLINE — `mcp_context::category_discipline`, the `{{category_discipline}}` block the
//!     three manuals and the sub-agent charter carry. Nothing EXECUTES the sequence any more: the
//!     orchestrator does, and what it owes is written into the text nyx hands it. A discipline that
//!     failed to reach the manual would be a `bug` Spec built with no proof at all and nothing
//!     saying so — which is exactly the failure the engine's gate used to make impossible.
//!
//! So every assertion that only described a GRAPH (a `set_category` node firing, a preflight judge,
//! a gate attempt recorded, a run re-routing or not) is gone from this file with the thing it
//! described. Nothing here is `#[ignore]`d and nothing is restated in a weaker form: what is left is
//! what is still true.
//!
//! # Why it cannot be a unit test
//!
//! `Db::open` is the ONLY caller of the category backfill on a real file. `Db::in_memory` (the unit
//! fixture) runs the migrations and NOTHING else, so every unit test seeds by hand and none of them
//! can answer "what does a real boot do to a real database that predates this Spec". That question
//! is [`the_boot_backfills_the_category_vocabulary_of_a_base_that_predates_it`] and no other.
//!
//! And the discipline's END is a rendered manual: the assertions below read
//! [`nyx_core::mcp_context::render_agent_context`] and
//! [`nyx_core::mcp_context::render_subagent_charter`] — the texts an agent actually receives —
//! rather than the pure `category_discipline` function, which `mcp_context`'s own unit tests already
//! snapshot. The joint between "the Spec carries a category" and "the agent reads the obligation" is
//! what nobody else walks.
//!
//! # The clauses, and where each is proven
//!
//!   1. « Les seeds globaux atteignent une base qui les précède. » →
//!      [`the_boot_backfills_the_category_vocabulary_of_a_base_that_predates_it`].
//!   2. « Le vocabulaire s'assigne, se mint, se retire — et jamais par surprise. » →
//!      [`the_vocabulary_assigns_mints_and_clears_through_the_one_agent_verb`].
//!   3. « L'agent qui mint et l'humain qui renomme sont distingués dans le journal. » →
//!      [`the_agent_that_mints_and_the_human_that_renames_are_told_apart_in_the_journal`].
//!   4. « La discipline `bug` atteint le manuel du rôle qui la doit, et personne d'autre. » →
//!      [`a_bug_spec_carries_its_discipline_into_the_manual_of_the_role_that_owes_it`].
//!   5. « Une recatégorisation n'est jamais refusée et la prochaine lecture en tient compte. » →
//!      [`a_recategorisation_is_never_refused_and_the_next_render_carries_the_new_discipline`].

use std::path::{Path, PathBuf};

use diesel::RunQueryDsl;
use nyx_core::db::{self, Db, SpecSections, SpecTransition};
use nyx_core::mcp::{RpcCode, RpcError};
use nyx_core::mcp_context::{AgentRole, CATEGORY_DISCIPLINE_HEADING};
use serde_json::{json, Value};

// ============================================================================
// Scaffolding — an on-disk temp nyx.db opened through the REAL boot path.
// ============================================================================

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

/// THE REAL BOOT. `Db::open` runs the migration chain, refuses on schema drift, brings up the
/// Système singleton AND backfills the per-project category vocabulary — the gestures a desktop
/// start performs, in that order, and the only place they happen together.
fn boot(tmp: &TempDir) -> Db {
    Db::open(&tmp.path().join("nyx.db")).expect("Db::open — the boot path")
}

/// A live project whose root workspace is the on-disk sandbox `dir`. Returns
/// `(project_id, workspace_id)`.
fn live_project(db: &Db, dir: &Path) -> (String, String) {
    db.with_conn(|c| {
        let (project, root) =
            db::create_project(c, "Category", dir.to_str().unwrap(), None).expect("project");
        (project.id, root.id)
    })
}

/// A Spec driven to `in_progress` and anchored on `workspace_id` — the state `/ndev` picks up, and
/// the one a discipline is read in.
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

/// Drive a Spec tool through the REAL MCP routing — the entry the sidecar's dispatcher calls.
/// Returns the raw `Result` so a clause can assert either the payload or the refusal. Panics only
/// on a MIS-ROUTE (the tool name not being ours).
fn spec_tool(db: &Db, name: &str, args: Value) -> Result<Value, RpcError> {
    nyx_core::mcp_tools_spec::dispatch_spec_tool(db, name, &args)
        .unwrap_or_else(|| panic!("`{name}` was not routed"))
        .map(|o| o.result)
}

/// [`spec_tool`] that must succeed.
fn spec_ok(db: &Db, name: &str, args: Value) -> Value {
    spec_tool(db, name, args)
        .unwrap_or_else(|e| panic!("`{name}` failed: [{}] {}", e.code, e.message))
}

/// CLASSIFY `spec_id` through the AGENT's verb — `spec_set_category`, the surface PDR-0017 gives an
/// agent, never a direct column write. Returns `(category payload, created)`.
fn classify(db: &Db, spec_id: &str, label: &str) -> (Value, bool) {
    let out = spec_ok(
        db,
        "spec_set_category",
        json!({ "spec_id": spec_id, "category": label }),
    );
    (
        out["category"].clone(),
        out["created"].as_bool().expect("a created flag"),
    )
}

/// The Spec's persisted category SLUG — read off the row, which is the fact every render reads.
fn spec_category_slug(db: &Db, spec_id: &str) -> Option<String> {
    db.with_conn(|c| {
        let spec = db::get_spec(c, spec_id).expect("spec")?;
        let id = spec.category_id?;
        db::get_category(c, &id).expect("category").map(|c| c.slug)
    })
}

/// The project's vocabulary, as the ordered slugs a chip row would draw.
fn vocabulary(db: &Db, project_id: &str) -> Vec<String> {
    db.with_conn(|c| db::list_categories(c, project_id))
        .expect("categories")
        .into_iter()
        .map(|c| c.slug)
        .collect()
}

/// RENDER a role's manual for `spec_id` — the text the agent of that role actually receives.
fn manual(db: &Db, role: AgentRole, spec_id: &str) -> String {
    nyx_core::mcp_context::render_agent_context(db, role, spec_id)
        .expect("the manual renders")
        .expect("…and it is bound to this Spec")
}

/// The three roles, walked as a set wherever a property is about WHO owes what.
const ROLES: [AgentRole; 3] = [AgentRole::Spec, AgentRole::Dev, AgentRole::Doc];

fn role_slug(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Spec => "spec",
        AgentRole::Dev => "dev",
        AgentRole::Doc => "doc",
    }
}

// ============================================================================
// CLAUSE 1 — the boot reaches a base that predates the vocabulary
// ============================================================================

/// `create_project` seeds the four system categories for every project born from now on, which
/// covers nothing that already exists. A database migrated onto `20260828180916_create_categories`
/// holds projects with NO category at all, and a Spec in one of them cannot be classified — so the
/// discipline can never fire there, and no gesture short of recreating the project would fix it.
/// The backfill is part of `Db::open`, and this is the only place it runs.
///
/// The category-less state is reproduced by DELETING the vocabulary of a project that has one,
/// which is the same row-shape a pre-migration base presents. The boot restores it — and restores it
/// IDEMPOTENTLY: a project that already answers the four slugs is left exactly as it is, RENAMES
/// INCLUDED, because a human's label is not something a boot may overwrite.
#[test]
fn the_boot_backfills_the_category_vocabulary_of_a_base_that_predates_it() {
    let tmp = temp_dir("boot-backfill");
    let db_path = tmp.path().join("nyx.db");

    let db = Db::open(&db_path).expect("first boot");
    let (project_id, _ws_id) = live_project(&db, tmp.path());
    assert_eq!(
        vocabulary(&db, &project_id),
        vec!["feature", "bug", "refactor", "chore"],
        "a project born after this Spec answers the four system slugs, in the curated order",
    );
    assert_eq!(
        db::SYSTEM_CATEGORY_SLUGS.to_vec(),
        vocabulary(&db, &project_id),
        "…and that list IS the crate's own, never a second copy written down here",
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
    assert!(vocabulary(&db, &project_id).is_empty());

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
        .with_conn(|c| db::find_category_by_slug(c, &project_id, db::CATEGORY_SLUG_BUG))
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
        .with_conn(|c| db::find_category_by_slug(c, &project_id, db::CATEGORY_SLUG_BUG))
        .expect("lookup")
        .expect("still there, under its stable slug");
    assert_eq!(
        renamed.name, "Défaut",
        "the boot re-seeded nothing over the human's label"
    );
    assert_eq!(
        renamed.id, bug_id,
        "and it is the same row — no duplicate was minted"
    );
    assert_eq!(
        vocabulary(&db, &project_id).len(),
        4,
        "still four, so the seed really is an upsert and not an insert",
    );
}

// ============================================================================
// CLAUSE 2 — the one verb an agent classifies through
// ============================================================================

/// `spec_set_category` IS the agent's whole power over the axis, and PDR-0017's governance is a
/// GRILL and not a lock: a label the vocabulary does not hold is MINTED rather than refused, because
/// a build blocked on an unknown word is worse than a fifth chip. What it must never do is classify
/// by surprise — so the two real refusals (a blank label, both references at once) and the explicit
/// REMOVAL are asserted beside the mint.
///
/// Everything here goes through the MCP verb, never the service behind it: the point is the surface
/// an agent reaches.
#[test]
fn the_vocabulary_assigns_mints_and_clears_through_the_one_agent_verb() {
    let tmp = temp_dir("vocabulary");
    let db = boot(&tmp);
    let (project_id, workspace_id) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);
    assert_eq!(
        spec_category_slug(&db, &spec_id),
        None,
        "a Spec is born unclassified — `spec_create` without a category says nothing about genre",
    );

    // --- BY SLUG, from the vocabulary the project already holds.
    let (category, created) = classify(&db, &spec_id, db::CATEGORY_SLUG_BUG);
    assert!(!created, "an existing slug is RESOLVED, never re-minted");
    assert_eq!(category["slug"], json!("bug"));
    assert_eq!(
        category["system"],
        json!(true),
        "…and the payload says it is one of the four the app protects: {category}",
    );
    assert_eq!(
        spec_category_slug(&db, &spec_id).as_deref(),
        Some("bug"),
        "the assignment is PERSISTED on the Spec, not merely echoed",
    );

    // --- BY HUMAN NAME, the other reference an agent may have read off a screen.
    let by_name = spec_ok(
        &db,
        "spec_set_category",
        json!({ "spec_id": spec_id, "category": "Refactor" }),
    );
    assert_eq!(
        by_name["category"]["slug"],
        json!("refactor"),
        "a NAME resolves to the same row as its slug — one axis, two spellings: {by_name}",
    );

    // --- AN UNKNOWN LABEL MINTS. PDR-0017's non-blocking fallback, and the reason a project's
    // vocabulary grows from the work rather than from a settings screen.
    let (minted, created) = classify(&db, &spec_id, "Flaky Test");
    assert!(created, "an unknown label is a CREATION, never a refusal");
    assert_eq!(
        minted["slug"],
        json!("flaky-test"),
        "slugified from the name — ASCII, lowercase, hyphens: {minted}",
    );
    assert_eq!(minted["created_by_type"], json!("agent"));
    assert_eq!(
        minted["system"],
        json!(false),
        "a minted category is NOT one of the protected four: {minted}",
    );
    assert!(
        minted.get("state").is_none(),
        "no proposed/confirmed word on the wire — the axis was retired (PDR-0017 amended \
         2026-09-02): {minted}",
    );
    assert_eq!(
        vocabulary(&db, &project_id),
        vec!["feature", "bug", "refactor", "chore", "flaky-test"],
        "…and it landed at the END of the project's list, after the four it was born beside",
    );

    // --- THE REMOVAL: naming NEITHER reference clears the Spec's category. An agent that got the
    // genre wrong can say so, and "unclassified" stays a reachable state.
    let cleared = spec_ok(&db, "spec_set_category", json!({ "spec_id": spec_id }));
    assert_eq!(
        cleared["category"],
        Value::Null,
        "the payload says the Spec now carries none: {cleared}",
    );
    assert_eq!(spec_category_slug(&db, &spec_id), None);
    assert_eq!(
        vocabulary(&db, &project_id).len(),
        5,
        "clearing a Spec does NOT delete the category it carried — the vocabulary is the project's",
    );

    // --- THE TWO REFUSALS THAT REMAIN, each an `invalid_argument` naming its own fault.
    let err = spec_tool(
        &db,
        "spec_set_category",
        json!({ "spec_id": spec_id, "category": "   " }),
    )
    .expect_err("a blank label labels nothing");
    assert_eq!(err.code, RpcCode::InvalidArgument, "{}", err.message);

    let err = spec_tool(
        &db,
        "spec_set_category",
        json!({ "spec_id": spec_id, "category": "bug", "category_id": minted["id"] }),
    )
    .expect_err("EITHER an exact row OR a label, never both");
    assert_eq!(err.code, RpcCode::InvalidArgument, "{}", err.message);

    // --- A CATEGORY OF ANOTHER PROJECT IS A MISMATCH, not a "not found": the caller has to learn
    // WHICH of the two facts is wrong.
    let (other_project, _other_ws) = db.with_conn(|c| {
        let (p, w) = db::create_project(
            c,
            "Elsewhere",
            tmp.path().join("elsewhere").to_str().unwrap(),
            None,
        )
        .expect("second project");
        (p.id, w.id)
    });
    let foreign = db
        .with_conn(|c| db::find_category_by_slug(c, &other_project, db::CATEGORY_SLUG_BUG))
        .expect("lookup")
        .expect("the other project's own bug")
        .id;
    let err = spec_tool(
        &db,
        "spec_set_category",
        json!({ "spec_id": spec_id, "category_id": foreign }),
    )
    .expect_err("a category belongs to ONE project");
    assert_eq!(err.code, RpcCode::InvalidArgument, "{}", err.message);
    assert!(
        err.message.contains(&project_id) && err.message.contains(&other_project),
        "the refusal names BOTH projects, got: {}",
        err.message
    );

    // …and none of the refusals wrote anything.
    assert_eq!(
        spec_category_slug(&db, &spec_id),
        None,
        "a refused classification leaves the Spec exactly as it was",
    );
    assert_eq!(vocabulary(&db, &project_id).len(), 5);
}

// ============================================================================
// CLAUSE 3 — the provenance nobody else records
// ============================================================================

/// PDR-0017's two halves, on ONE Spec's real journey: the agent MINTS, the human CURATES — and the
/// persisted trace tells them apart.
///
/// `created_by_type` says who minted the ROW. It does NOT say who took a DECISION, and both halves
/// of PDR-0017's governance are decisions: an agent classifying a Spec on the fly, and a human
/// re-labelling the vocabulary afterwards. Nothing but `domain_events.source` distinguishes them,
/// and nothing but this test walks both through the surfaces that really produce them — the MCP
/// verb (an agent path, `mcp`) and the curation service the sidecar's commands call (a human path,
/// `ui`).
///
/// SINCE THE 2026-09-02 AMENDMENT of PDR-0017 there is no confirmation: the agent's mint is valid as
/// it stands. The human half is therefore the RENAME (and, from the settings, the create / delete),
/// and `category.confirmed` is an event this build can no longer produce.
#[test]
fn the_agent_that_mints_and_the_human_that_renames_are_told_apart_in_the_journal() {
    let tmp = temp_dir("provenance");
    let db = boot(&tmp);
    let (project_id, workspace_id) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    // THE AGENT'S HALF — through `spec_set_category`, on a label the project's vocabulary does not
    // hold. It is MINTED, valid as it stands, never refused.
    let (payload, created) = classify(&db, &spec_id, "Flaky Test");
    assert!(created);
    let minted = db
        .with_conn(|c| db::get_category(c, payload["id"].as_str().expect("an id")))
        .expect("read the minted category")
        .expect("it exists");
    assert_eq!(minted.created_by_type, db::CATEGORY_CREATED_BY_AGENT);
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
        "…and so is the assignment the agent verb performed: {sources:?}",
    );
    assert!(
        sources.contains(&("category.renamed".to_string(), "ui".to_string())),
        "the RE-LABELLING is filed as a human gesture — the one fact `created_by_type` cannot \
         carry, since the row was and remains agent-minted: {sources:?}",
    );
    assert!(
        !sources.iter().any(|(kind, _)| kind == "category.confirmed"),
        "`category.confirmed` was RETIRED with the proposed/confirmed axis: {sources:?}",
    );

    let curated = db
        .with_conn(|c| db::get_category(c, &minted.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        curated.created_by_type, db::CATEGORY_CREATED_BY_AGENT,
        "renaming does NOT rewrite who minted the row — the two facts stay two facts",
    );
    assert_eq!(curated.slug, "flaky-test", "…and never the identity either");

    // ONE PAYLOAD, TWO SURFACES. The chip the renderer draws is derived HERE, once, by the function
    // the sidecar's `category_list` / `category_rename` and the MCP `spec_set_category` all call —
    // so the same row cannot read differently on two screens.
    let payload = nyx_core::mcp_tools_spec::category_payload(&curated);
    assert_eq!(payload["created_by_type"], json!("agent"));
    assert_eq!(payload["slug"], json!("flaky-test"));
    assert_eq!(payload["name"], json!("Test instable"));
    assert!(
        payload.get("state").is_none(),
        "no proposed/confirmed word on the wire: {payload}",
    );

    // AND THE HUMAN'S LABEL IS WHAT THE AGENT CAN TYPE NEXT: the rename did not orphan the chip, so
    // the very next classification by that name lands on the SAME row rather than minting a second.
    let other = spec_in_progress(&db, &project_id, &workspace_id);
    let (again, created) = classify(&db, &other, "Test instable");
    assert!(!created, "the renamed category answers its new name: {again}");
    assert_eq!(again["id"], json!(minted.id));
}

// ============================================================================
// CLAUSE 4 — the discipline, and the manual it has to reach
// ============================================================================

/// THE HEART OF THE FILE. A `bug` Spec owes a red-before-green proof (ADR-0006), and since PDR-0031
/// NOTHING EXECUTES THAT SEQUENCE: the orchestrator runs it, and the only reason it knows it owes
/// anything is the `{{category_discipline}}` block nyx renders into its manual. A discipline that
/// failed to reach the text is a `bug` built with no proof and no one saying so.
///
/// So this walks the joint end to end, on the manuals an agent really receives:
///
///   * `/ndev` (the role that OBSERVES the proof) gets the sequence: reproduce before repairing, the
///     same check after, and — the failure mode that motivated the whole PRD — a run that could not
///     EXECUTE is not a red;
///   * `/nspec` (the role that WRITES what makes a reproduction possible at all) gets the other
///     half: the reproduction contract, and its five named elements;
///   * `/ndoc` gets NONE. It reconciles documentation after the fact and decides nothing about a
///     proof, so a discipline block there would be noise in the one text that must stay short;
///   * a `feature` Spec draws NOTHING, for any role — the discipline is the `bug` category's, not a
///     ceremony every Spec pays;
///   * an UNCLASSIFIED Spec draws nothing either, and its manual SAYS the genre is undecided and
///     names the verb that decides it. That is what is left of "classify before you route" now that
///     no preflight node exists: the obligation moved into the text, and it is the author who reads
///     it.
///
/// The SUB-AGENT charter is asserted last and is not a repetition: it is a fourth surface, resolved
/// from the parent TERMINAL rather than from a Spec argument, and it carries the half of the
/// discipline that belongs to whoever actually writes the reproduction.
#[test]
fn a_bug_spec_carries_its_discipline_into_the_manual_of_the_role_that_owes_it() {
    let tmp = temp_dir("discipline");
    let db = boot(&tmp);
    let (project_id, workspace_id) = live_project(&db, tmp.path());

    // --- THE UNCLASSIFIED Spec: no discipline, and the manual says the genre is undecided.
    let undecided = spec_in_progress(&db, &project_id, &workspace_id);
    for role in ROLES {
        let text = manual(&db, role, &undecided);
        assert!(
            !text.contains(CATEGORY_DISCIPLINE_HEADING),
            "{}: an unclassified Spec owes no discipline — nyx cannot know what it would be",
            role_slug(role),
        );
        assert!(
            text.contains("(none — spec_set_category assigns one)"),
            "{}: …and the render SAYS so, naming the verb that decides it:\n{text}",
            role_slug(role),
        );
    }

    // --- THE `bug` Spec.
    let bug = spec_in_progress(&db, &project_id, &workspace_id);
    classify(&db, &bug, db::CATEGORY_SLUG_BUG);

    let dev = manual(&db, AgentRole::Dev, &bug);
    assert!(
        dev.contains(CATEGORY_DISCIPLINE_HEADING) && dev.contains("`bug`"),
        "the discipline block is injected for the role that OBSERVES the proof, and it names the \
         category it comes from:\n{dev}",
    );
    assert!(
        dev.contains("REPRODUCE BEFORE ANY REPAIR"),
        "…carrying the sequence itself:\n{dev}",
    );
    assert!(
        dev.contains("THE SAME CHECK, AFTER"),
        "…the identity of the two runs:\n{dev}",
    );
    assert!(
        dev.contains("A CHECK THAT COULD NOT RUN IS NOT A RED"),
        "…and the failure this whole PRD was written against: an attempt that produced no verdict \
         must never stand as the reproduction:\n{dev}",
    );
    assert!(
        dev.contains("IF THE REPRODUCTION CANNOT BE AUTOMATED"),
        "…with the declared manual route, so 'it needs a device' is an answer and not a way out:\n\
         {dev}",
    );

    let spec = manual(&db, AgentRole::Spec, &bug);
    assert!(
        spec.contains(CATEGORY_DISCIPLINE_HEADING),
        "the AUTHOR owes the other half — what makes a reproduction possible at all:\n{spec}",
    );
    assert!(
        spec.contains("REPRODUCTION CONTRACT"),
        "…named as such:\n{spec}",
    );
    for element in [
        "OBSERVED behaviour",
        "EXPECTED result",
        "COUNTER-EXAMPLE",
        "SCOPE",
        "REPRODUCTION STRATEGY",
    ] {
        assert!(
            spec.contains(element),
            "…and its five elements are NAMED, not summarised as 'be precise': '{element}' missing \
             from:\n{spec}",
        );
    }
    assert_ne!(
        spec, dev,
        "the two halves are DIFFERENT texts: the author writes the contract, the orchestrator \
         observes the proof",
    );

    let doc = manual(&db, AgentRole::Doc, &bug);
    assert!(
        !doc.contains(CATEGORY_DISCIPLINE_HEADING),
        "/ndoc syncs documentation after the fact and decides nothing about a proof:\n{doc}",
    );
    assert!(
        doc.contains("category : bug"),
        "…though it still READS the classification, which is a fact about the Spec:\n{doc}",
    );

    // --- A `feature` draws NOTHING, for anyone. The discipline is one category's obligation.
    let feature = spec_in_progress(&db, &project_id, &workspace_id);
    classify(&db, &feature, "feature");
    for role in ROLES {
        let text = manual(&db, role, &feature);
        assert!(
            !text.contains(CATEGORY_DISCIPLINE_HEADING),
            "{}: a `feature` owes no red-before-green — a ceremony every Spec pays is a ceremony \
             nobody reads:\n{text}",
            role_slug(role),
        );
        assert!(
            text.contains("category : feature"),
            "{}: …but its genre is still stated:\n{text}",
            role_slug(role),
        );
    }

    // --- THE FOURTH SURFACE: the sub-agent charter, resolved from the parent TERMINAL. A sub-agent
    // inherits its mission from the orchestrator and nothing else, so if the discipline does not
    // ride here it never reaches the agent that actually writes the reproduction.
    let terminal_id = db
        .with_conn(|c| {
            let terminal = db::create_terminal(c, tmp.path().to_str().unwrap(), None)?;
            db::attach_terminal(c, &terminal.id, &workspace_id, db::BINDING_MANUAL)?;
            diesel::result::QueryResult::Ok(terminal.id)
        })
        .expect("a terminal attached to the workspace");
    // The workspace anchors exactly one ACTIVE Spec at a time for this resolution to be unambiguous:
    // the `feature` and the undecided one are moved out of the way, leaving the `bug`.
    for other in [&undecided, &feature] {
        db.with_conn(|c| db::transition_spec(c, other, SpecTransition::Cancel, true))
            .expect("ship the other Specs out of the active set");
    }
    let charter = nyx_core::mcp_context::render_subagent_charter(&db, &terminal_id)
        .expect("the charter renders");
    assert!(
        charter.contains(&bug),
        "the charter names the Spec the parent terminal's workspace anchors:\n{charter}",
    );
    assert!(
        charter.contains("Category discipline — `bug`"),
        "…and carries the sub-agent's half of the discipline:\n{charter}",
    );
    assert!(
        charter.contains("Write the reproducing test FIRST"),
        "…which is the half that belongs to whoever writes the reproduction:\n{charter}",
    );
}

// ============================================================================
// CLAUSE 5 — a recategorisation is a legitimate act
// ============================================================================

/// THE PRD'S EXPLICIT RULING, in what survives of it: the category is deliberately NOT frozen by the
/// lifecycle (it is classification metadata, not a commitment), so it can change while the work is
/// under way, and it is never refused for that. What changes with it is what the NEXT read owes —
/// and since PDR-0031 that read is a rendered manual and nothing else.
///
/// The gesture is placed where it matters: on a Spec already `in_progress`. Both directions are
/// walked, because they fail differently — a Spec promoted TO `bug` must GAIN the obligation (a
/// defect discovered mid-build is the ordinary way a Spec becomes one), and a Spec moved AWAY from
/// it must LOSE it rather than keep a discipline nothing owes any more.
#[test]
fn a_recategorisation_is_never_refused_and_the_next_render_carries_the_new_discipline() {
    let tmp = temp_dir("recategorise");
    let db = boot(&tmp);
    let (project_id, workspace_id) = live_project(&db, tmp.path());
    let spec_id = spec_in_progress(&db, &project_id, &workspace_id);

    classify(&db, &spec_id, "feature");
    assert!(
        !manual(&db, AgentRole::Dev, &spec_id).contains(CATEGORY_DISCIPLINE_HEADING),
        "precondition: as a `feature` it owes nothing",
    );

    // --- TOWARDS `bug`: never refused mid-flight, and the obligation appears at the next read.
    classify(&db, &spec_id, db::CATEGORY_SLUG_BUG);
    assert_eq!(
        db.with_conn(|c| db::get_spec(c, &spec_id))
            .unwrap()
            .unwrap()
            .status,
        db::SPEC_STATUS_IN_PROGRESS,
        "a classification is metadata: it moves no lifecycle and is refused by none",
    );
    let dev = manual(&db, AgentRole::Dev, &spec_id);
    assert!(
        dev.contains(CATEGORY_DISCIPLINE_HEADING) && dev.contains("REPRODUCE BEFORE ANY REPAIR"),
        "the NEXT read carries the new obligation — a defect discovered mid-build is how a Spec \
         ordinarily becomes a `bug`:\n{dev}",
    );

    // --- AND BACK: the discipline is not sticky. Nothing owes a red-before-green once the genre
    // says otherwise, and a text that kept saying so would be a rule nobody could ever discharge.
    classify(&db, &spec_id, "chore");
    let dev = manual(&db, AgentRole::Dev, &spec_id);
    assert!(
        !dev.contains(CATEGORY_DISCIPLINE_HEADING),
        "the obligation left with the category that carried it:\n{dev}",
    );
    assert!(
        dev.contains("category : chore"),
        "…and the render states the genre it now has:\n{dev}",
    );
    assert_eq!(spec_category_slug(&db, &spec_id).as_deref(), Some("chore"));
}
