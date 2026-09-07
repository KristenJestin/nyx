//! GATE e2e — « Résoudre une Spec depuis une localisation, et REFUSER quand on ne peut pas »
//! (PRD `01KYA475AX5JBGSS8WK5J8NM1S`, phase 3, task `01KYA4D9G1VANEEM3F1D175ND4`).
//!
//! THE QUESTION THIS GATE ANSWERS is unchanged since the day it was written: a dev workspace is
//! declared FOR a Spec, and from then on every consumer must find that Spec **with no id in hand** —
//! 0 anchored Specs, 1, or N; an explicit reference in any of its four forms; and a REFUSAL that
//! names what to send when none of it works.
//!
//! WHAT CHANGED IS THE DOOR, and only the door. The refusal used to be `run_start`'s: the engine's
//! run would not open on a token that designated no Spec, or on a location that designated several.
//! The behaviour-tree engine is withdrawn (PDR-0031, `docs/product/decisions/`), so the verdict moved
//! to the call that actually resolves the anchor — [`nyx_core::mcp_context::agent_context`], the tool
//! that renders a command's manual. Everything this gate proved about RESOLUTION and REFUSAL survives
//! that move intact; everything it proved about a RUN (one being opened, bound to a Spec, anchored on
//! a workspace) is gone with the engine and is not restated here in another form.
//!
//! It walks the REAL production bricks, nothing stubbed:
//!
//!   * [`nyx_core::mcp_runtime::dispatch_workspace_tool`] — the EXACT entry the sidecar's MCP
//!     dispatcher calls for `workspace_add` / `create_workspace`, carrying the `spec_id` anchor;
//!   * [`nyx_core::mcp_context::agent_context`] — the manual render, whose Spec is resolved from a
//!     `spec_id` token, a `workspace_id` or a `cwd` alone, and whose refusals are the ones the
//!     session hook relays to the model;
//!   * [`nyx_core::mcp_tools_spec::spec_set_workspace`] — the standalone anchor tool, used here to
//!     build the ambiguous (N) state a real project reaches;
//!   * [`nyx_core::usecase::remove_workspace`] — the UI bridge's own workspace removal, used to
//!     prove the `ON DELETE SET NULL` detach is real and not merely declared in a migration.
//!
//! The Specs are driven through `db::transition_spec` (the lifecycle authority), and every
//! anchoring / resolution step goes through the tool an agent would call. The DB is an ON-DISK
//! `Db::open` (integration tests cannot reach the `#[cfg(test)]` in-memory one), i.e. the real boot
//! path: migrations, drift refusal, Système scope, category backfill.
//!
//! ## The one exception: the rollback half of clause 5, driven one level below the tools
//!
//! ONE block of this gate — in
//! [`a_wrong_project_a_db_rollback_and_a_fresh_folder_all_leave_nothing_behind`], the block
//! commented "The OTHER direction of the shared transaction" — calls
//! `usecase::register_workspace_anchored` directly instead of going through an MCP tool. That is
//! deliberate, and the property is UNREACHABLE through the MCP surface: `prevalidate_spec_anchor`
//! refuses an unanchorable Spec BEFORE any write, so no tool call can ever reach the shared
//! transaction with an anchor that then fails inside it — it would take the Spec being cancelled
//! BETWEEN the prevalidation and the write. The seam it drives is nonetheless the exact one both
//! tools funnel into (`create_workspace_inner` → `register_workspace_anchored`).
//!
//! The block is load-bearing, not decorative: under the mutation "ancrage hors transaction"
//! (`usecase.rs:596`, anchoring moved out of the shared transaction) it is the ONLY assertion of
//! this whole gate that turns red. It DELIBERATELY duplicates the unit test
//! `a_refused_anchor_rolls_the_workspace_insert_back` (`crates/nyx-core/src/mcp_runtime.rs` — same
//! use case, same draft Spec, same `RegisterWorkspaceError::Anchor` match): the duplication is the
//! price of the gate ITSELF biting on transactionality rather than delegating that property to a
//! test the gate never runs. Do not "deduplicate" either copy without re-running that mutation.
//!
//! # The clauses, each proven below
//!
//!   1. "Les deux chemins de création résolvent ensuite la Spec sans id explicite." —
//!      [`both_creation_paths_then_resolve_the_spec_with_no_explicit_id`].
//!   2. "Aucun chemin d'ancrage ne change ready ou in_progress ; draft est refusée." —
//!      [`no_anchoring_path_moves_the_status_and_an_unanchorable_spec_is_refused`].
//!   3. "Supprimer le workspace détache la Spec ; le comportement zéro candidate dépend du rôle
//!      sans erreur interne." —
//!      [`removing_the_workspace_detaches_the_spec_and_zero_candidates_answers_by_role`].
//!   4. "Deux Specs actives sur le même workspace produisent l'ambiguïté attendue et aucune n'est
//!      choisie." — [`two_active_specs_on_one_workspace_refuse_to_pick_either`].
//!   5. "Les cas mauvais projet, rollback DB et dossier neuf compensé sont couverts." —
//!      [`a_wrong_project_a_db_rollback_and_a_fresh_folder_all_leave_nothing_behind`], plus the
//!      no-`spec_id` compatibility clause in
//!      [`without_spec_id_both_tools_keep_their_pre_anchor_contract`].
//!   6. THE REFERENCE GRAMMAR, which is the other half of "sans id explicite" now that the token the
//!      human typed arrives on this tool —
//!      [`the_four_reference_forms_resolve_and_a_refusal_names_what_to_send`].
//!
//! # What this gate deliberately does NOT prove
//!
//! Multi-repo provisioning, worktree creation, implicit consent, and ANY selection-by-recency
//! policy are out of scope: nothing here creates a worktree or asserts that some candidate wins on
//! recency — the N case is proven to pick NOTHING, which is the opposite property. It renders
//! manuals but says nothing about their PROSE (that is `mcp_context`'s own sweeps): what it reads out
//! of a render is the Spec it resolved, the project it echoes, and the diagnosis it carries.

use std::path::{Path, PathBuf};

use nyx_core::db::{self, Db, SpecSections, SpecTransition};
use nyx_core::mcp::{RpcCode, RpcError};
use nyx_core::mcp_runtime::dispatch_workspace_tool;
use nyx_core::usecase::{self, DomainEventSource, NullEventSink};
use serde_json::{json, Value};

// ============================================================================
// Scaffolding — an on-disk temp nyx.db opened through the REAL boot path, RAII-cleaned.
// ============================================================================

/// RAII temp dir removed on drop (the pattern the crate's other gate e2e tests use). The caller
/// binds it as `tmp` — declared BEFORE `db` — so cleanup survives a panicking assertion and the
/// dir is removed AFTER `db` closes its pool (locals drop in reverse declaration order).
struct TempDir(PathBuf);
impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
    /// A child path under this sandbox, as the `String` the MCP tools take.
    fn child(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "nyx-gate-spec-anchor-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// Open an on-disk nyx.db under `tmp` through `Db::open` — the boot path itself (migrations, drift
/// refusal, Système scope, category backfill), never the in-memory test fixture.
fn open_db(tmp: &TempDir) -> Db {
    Db::open(&tmp.path().join("nyx.db")).expect("open temp nyx.db (the boot path)")
}

/// A project whose ROOT workspace is `root` (a path that never collides with the dev workspaces
/// this gate registers). Returns the project id.
fn project(db: &Db, name: &str, root: &str) -> String {
    project_row(db, name, root).id
}

/// The project ROW — used where the human KEY matters (the `KEY-3` reference form), which is minted
/// by `create_project` and therefore never guessed here.
fn project_row(db: &Db, name: &str, root: &str) -> db::Project {
    db.with_conn(|c| db::create_project(c, name, root, None))
        .expect("project")
        .0
}

/// A Spec of `project_id` driven to `status` through the LIFECYCLE authority (never a direct
/// column write): `draft` stays as created, `ready` takes the `ready` transition, `in_progress`
/// takes `ready` then `start`, `cancelled` takes `ready` then `cancel`. The sections are filled
/// because `validate_ready` demands a problem + solution.
fn spec_at(db: &Db, project_id: &str, title: &str, status: &str) -> String {
    db.with_conn(|c| {
        let (spec, _rev) = db::create_spec(
            c,
            project_id,
            title,
            SpecSections {
                problem: "the dev workspace exists before the work starts".into(),
                solution: "anchor it at declaration time and resolve the Spec by location".into(),
                ..SpecSections::empty()
            },
        )
        .expect("spec");
        if status != db::SPEC_STATUS_DRAFT {
            db::transition_spec(c, &spec.id, SpecTransition::Ready, true).expect("ready");
        }
        if status == db::SPEC_STATUS_IN_PROGRESS {
            db::transition_spec(c, &spec.id, SpecTransition::Start, true).expect("start");
        }
        if status == db::SPEC_STATUS_CANCELLED {
            db::transition_spec(c, &spec.id, SpecTransition::Cancel, true).expect("cancel");
        }
        spec.id
    })
}

fn spec_row(db: &Db, spec_id: &str) -> db::Spec {
    db.with_conn(|c| db::get_spec(c, spec_id))
        .expect("read spec")
        .expect("spec exists")
}

fn workspaces_of(db: &Db, project_id: &str) -> Vec<db::Workspace> {
    db.with_conn(|c| db::list_workspaces(c, project_id))
        .expect("list workspaces")
}

/// Drive a workspace registration verb through the REAL MCP routing — the exact call
/// `nyx-sidecar/src/runtime/mcp.rs` makes. Returns the raw `Result` so a clause can assert either
/// the payload or the refusal. Panics only on a MIS-ROUTE (the tool name not being ours).
fn ws_tool(db: &Db, name: &str, args: Value) -> Result<Value, RpcError> {
    dispatch_workspace_tool(db, &NullEventSink, name, &args)
        .unwrap_or_else(|| panic!("workspace tool `{name}` was not routed"))
}

/// [`ws_tool`] that must succeed.
fn ws_ok(db: &Db, name: &str, args: Value) -> Value {
    ws_tool(db, name, args)
        .unwrap_or_else(|e| panic!("`{name}` failed: [{:?}] {}", e.code, e.message))
}

/// Register a dev workspace for `spec_id` through `tool` (`create_workspace` on a folder that does
/// not exist yet, `workspace_add` on one that does) and return `(workspace_id, root_path)`.
///
/// The root path is taken from the PAYLOAD's `path` (the MCP wire name of the `root_path` column,
/// serde-renamed), never re-derived from the argument: `db::create_workspace` NORMALIZES the path
/// on insert (`pathnorm`), so the stored string is the only authority on what a later `cwd`
/// argument has to look like — and echoing it here is what makes the `cwd` assertions below
/// independent of this platform's path spelling.
fn anchor_via(
    db: &Db,
    tool: &str,
    project_id: &str,
    path: &str,
    spec_id: &str,
) -> (String, String) {
    if tool == "workspace_add" {
        std::fs::create_dir_all(path).expect("the folder workspace_add registers must exist");
    }
    let res = ws_ok(
        db,
        tool,
        json!({ "project_id": project_id, "name": "dev", "path": path, "spec_id": spec_id }),
    );
    let id = res["workspace"]["id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let root = res["workspace"]["path"]
        .as_str()
        .expect("workspace path")
        .to_string();
    (id, root)
}

/// `agent_context` through its REAL entry point, raw `Result`.
fn context(db: &Db, args: Value) -> Result<Value, RpcError> {
    nyx_core::mcp_context::agent_context(db, &args).map(|o| o.result)
}

/// The three ROLES `agent_context` renders (PDR-0031) — `spec` authors, `dev` builds, `doc`
/// reconciles. Walked as a set wherever a property is the RESOLVER's and not one role's, so a
/// fourth role could not be added resolving differently without this gate saying so.
const ROLES: [&str; 3] = ["spec", "dev", "doc"];

/// The two roles that work ON an existing Spec, and are therefore REFUSED rather than started on a
/// guess when none is anchored. `spec` is not one of them: a project with zero Specs is exactly
/// where a new one gets authored.
const EFFECTOR_ROLES: [&str; 2] = ["dev", "doc"];

/// The `spec_set_workspace` MCP tool — the STANDALONE anchor path (the other writer of
/// `specs.workspace_id`), raw `Result`.
fn set_workspace(db: &Db, args: Value) -> Result<Value, RpcError> {
    nyx_core::mcp_tools_spec::spec_set_workspace(db, &args).map(|o| o.result)
}

/// The machine handle a caller keys on to tell the AMBIGUOUS-location refusal apart from every
/// other `invalid_argument` — asserted here as the WIRE contract (`error.data.reason`), not through
/// the crate-private constant that produces it.
const AMBIGUOUS_REASON: &str = "spec_ambiguous_for_workspace";

/// The two case-0 diagnoses, as WIRE codes: a location that resolved a real workspace carrying no
/// active Spec, and a location that resolved no workspace at all (an invocation mistake).
const NONE_ON_LOCATION: &str = "spec_none_on_location";
const LOCATION_UNRESOLVED: &str = "spec_location_unresolved";

fn is_ambiguous(e: &RpcError) -> bool {
    e.code == RpcCode::InvalidArgument && reason_of(e).as_deref() == Some(AMBIGUOUS_REASON)
}

/// The `data.reason` of a refusal — the machine half a caller branches on.
fn reason_of(e: &RpcError) -> Option<String> {
    e.data
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A refusal must never be an `internal`: every case this gate drives is a MODELLED outcome with an
/// agent-facing sentence, so an `internal` here means a guard leaked a raw DB error.
fn assert_not_internal(label: &str, e: &RpcError) {
    assert_ne!(
        e.code,
        RpcCode::Internal,
        "{label} must be a modelled refusal, not an internal error: {}",
        e.message
    );
}

// ============================================================================
// CLAUSE 1 — "Les deux chemins de création résolvent ensuite la Spec sans id explicite."
// ============================================================================

/// THE DEROULÉ THE PRD EXISTS FOR, run four times: `{create_workspace, workspace_add}` ×
/// `{ready, in_progress}`.
///
/// A Spec is already at least `ready`, its dev folder is declared to nyx WITH the `spec_id`, and
/// from that moment on NO caller has to carry the Spec id again: `agent_context` resolves it from
/// the `workspace_id`, from the workspace's own `cwd`, AND from a NESTED cwd (the realistic case —
/// an agent runs from a subfolder of its workspace, which is what the shared deepest-ancestor
/// selector is for). The three ROLES are walked at each anchor, because resolution is the
/// RESOLVER's job and not the role's: `/nspec`, `/ndev` and `/ndoc` standing on the same folder must
/// land on the same Spec.
///
/// The status is re-read AFTER every render, not just after the anchor: clause 2's "aucun chemin
/// d'ancrage ne change ready ou in_progress" has to survive the resolution too, and `agent_context`
/// is a pure READ — an accidental transition there would be invisible anywhere else.
#[test]
fn both_creation_paths_then_resolve_the_spec_with_no_explicit_id() {
    for tool in ["create_workspace", "workspace_add"] {
        for status in [db::SPEC_STATUS_READY, db::SPEC_STATUS_IN_PROGRESS] {
            let label = format!("{tool}/{status}");
            let tmp = temp_dir("resolve");
            let db = open_db(&tmp);
            let project_id = project(&db, "Anchor", &tmp.child("root"));
            let spec_id = spec_at(&db, &project_id, "Dev spec", status);

            // --- The declaration: ONE call registers the workspace AND anchors the Spec.
            let (ws_id, ws_root) = anchor_via(&db, tool, &project_id, &tmp.child("dev"), &spec_id);
            let stored = spec_row(&db, &spec_id);
            assert_eq!(
                stored.workspace_id.as_deref(),
                Some(ws_id.as_str()),
                "{label}: the anchor is PERSISTED by the registration itself"
            );
            assert_eq!(
                stored.status, status,
                "{label}: anchoring is a CONTEXT operation — it never transitions the Spec"
            );

            // --- The consumers, each with NO spec_id anywhere in the args.
            let nested = format!("{ws_root}/crates/nyx-core/src");
            let anchors = [
                ("workspace_id", json!({ "workspace_id": ws_id })),
                ("cwd", json!({ "cwd": ws_root })),
                ("nested cwd", json!({ "cwd": nested })),
            ];
            for (kind, anchor) in anchors {
                for role in ROLES {
                    let mut args = anchor.clone();
                    let obj = args.as_object_mut().unwrap();
                    obj.insert("role".into(), json!(role));
                    let rendered = context(&db, args).unwrap_or_else(|e| {
                        panic!("{label}/{kind}/{role}: agent_context refused: {}", e.message)
                    });
                    assert_eq!(
                        rendered["spec_id"],
                        json!(spec_id),
                        "{label}/{role}: the anchored Spec is resolved from the {kind} alone"
                    );
                    assert_eq!(
                        rendered["project_id"],
                        json!(project_id),
                        "{label}/{kind}/{role}: …and the Spec's OWN project rides beside the manual"
                    );
                    assert!(
                        rendered["context"].as_str().is_some_and(|s| !s.is_empty()),
                        "{label}/{kind}/{role}: a resolved Spec renders a real manual"
                    );
                    assert_eq!(
                        rendered["warnings"],
                        json!([]),
                        "{label}/{kind}/{role}: a render that resolved carries NO diagnosis — the \
                         array is present and empty, never absent"
                    );
                }
            }

            // --- And nothing along the way moved the lifecycle.
            assert_eq!(
                spec_row(&db, &spec_id).status,
                status,
                "{label}: rendering a manual transitions nothing"
            );
        }
    }
}

// ============================================================================
// CLAUSE 2 — "Aucun chemin d'ancrage ne change ready ou in_progress ; draft est refusée."
// ============================================================================

/// THE TWO HALVES OF THE WIDENED GUARD, on BOTH tools and on the STANDALONE anchor tool.
///
/// The widening this PRD delivers is `{in_progress}` → `{ready, in_progress}`, and a widening is
/// only safe if what it did NOT open stays shut. So this clause drives, for each of the three
/// writers of `specs.workspace_id` an agent can reach:
///
///   * the two ANCHORABLE statuses — the anchor lands, the status is byte-identical afterwards
///     (read back from the DB, not from the echoed payload);
///   * `draft` — REFUSED with `invalid_state`, in a sentence that names the two legal statuses;
///   * `cancelled` — REFUSED the same way, so "widened to ready" never read as "guard removed"
///     (the PRD's own testing decision: "refus draft et états terminaux").
///
/// A refusal must also leave NOTHING behind: no workspace row, no folder that `create_workspace`
/// would otherwise have `mkdir`ed, and a pre-existing folder untouched.
#[test]
fn no_anchoring_path_moves_the_status_and_an_unanchorable_spec_is_refused() {
    // --- (a) The ANCHORABLE statuses: the link lands, the lifecycle does not move.
    for status in [db::SPEC_STATUS_READY, db::SPEC_STATUS_IN_PROGRESS] {
        let tmp = temp_dir("guard-ok");
        let db = open_db(&tmp);
        let project_id = project(&db, "Guard", &tmp.child("root"));
        let spec_id = spec_at(&db, &project_id, "Anchorable", status);
        let before = spec_row(&db, &spec_id);

        let (ws_id, _) = anchor_via(
            &db,
            "create_workspace",
            &project_id,
            &tmp.child("a"),
            &spec_id,
        );
        assert_eq!(
            spec_row(&db, &spec_id).status,
            status,
            "create_workspace/{status}"
        );

        // The STANDALONE tool re-anchors the very same Spec onto a second workspace — the other
        // writer of the column, sharing the SAME widened guard.
        let (other_ws, _) =
            anchor_via(&db, "workspace_add", &project_id, &tmp.child("b"), &spec_id);
        assert_ne!(other_ws, ws_id);
        let moved = set_workspace(&db, json!({ "spec_id": spec_id, "workspace_id": ws_id }))
            .expect("spec_set_workspace on an anchorable Spec");
        assert_eq!(moved["spec"]["workspace_id"], json!(ws_id));
        assert_eq!(
            moved["spec"]["status"],
            json!(status),
            "spec_set_workspace/{status}: the standalone anchor never transitions either"
        );

        let after = spec_row(&db, &spec_id);
        assert_eq!(
            after.status, before.status,
            "{status}: the status is untouched end to end"
        );
        assert_eq!(
            after.current_revision_number, before.current_revision_number,
            "{status}: anchoring is not an authoring act — it forks no revision"
        );
    }

    // --- (b) The REFUSED statuses, on every path an agent can take.
    for status in [db::SPEC_STATUS_DRAFT, db::SPEC_STATUS_CANCELLED] {
        let tmp = temp_dir("guard-ko");
        let db = open_db(&tmp);
        let project_id = project(&db, "Guard", &tmp.child("root"));
        let spec_id = spec_at(&db, &project_id, "Unanchorable", status);
        // A real target for the standalone tool, registered with no spec_id at all.
        let (ws_id, _) = {
            let path = tmp.child("target");
            std::fs::create_dir_all(&path).unwrap();
            let res = ws_ok(
                &db,
                "workspace_add",
                json!({ "project_id": project_id, "path": path }),
            );
            (res["workspace"]["id"].as_str().unwrap().to_string(), path)
        };

        // create_workspace — the folder must NOT be created for a link that is refused anyway.
        let fresh = tmp.child("never-created");
        assert!(
            !Path::new(&fresh).exists(),
            "precondition: {status} path is absent"
        );
        let err = ws_tool(
            &db,
            "create_workspace",
            json!({ "project_id": project_id, "name": "dev", "path": fresh, "spec_id": spec_id }),
        )
        .expect_err("an unanchorable Spec is refused");
        assert_not_internal(&format!("create_workspace/{status}"), &err);
        assert_eq!(err.code, RpcCode::InvalidState, "{status}: the D8 code");
        assert!(
            err.message.contains("ready or in_progress"),
            "{status}: the refusal NAMES the two legal statuses, got: {}",
            err.message
        );
        assert!(
            !Path::new(&fresh).exists(),
            "{status}: prevalidation ran BEFORE the mkdir — no folder was left behind"
        );

        // workspace_add — the same refusal, on a folder that legitimately exists and must survive.
        let existing = tmp.child("kept");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(Path::new(&existing).join("keep.txt"), b"user data").unwrap();
        let err = ws_tool(
            &db,
            "workspace_add",
            json!({ "project_id": project_id, "path": existing, "spec_id": spec_id }),
        )
        .expect_err("an unanchorable Spec is refused");
        assert_not_internal(&format!("workspace_add/{status}"), &err);
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(
            Path::new(&existing).join("keep.txt").is_file(),
            "{status}: a refusal NEVER removes a path nyx did not create"
        );

        // spec_set_workspace — the standalone path shares the guard.
        let err = set_workspace(&db, json!({ "spec_id": spec_id, "workspace_id": ws_id }))
            .expect_err("an unanchorable Spec is refused");
        assert_not_internal(&format!("spec_set_workspace/{status}"), &err);
        assert_eq!(err.code, RpcCode::InvalidState);

        // NOTHING was written by any of the three.
        let live = workspaces_of(&db, &project_id);
        assert_eq!(
            live.len(),
            2,
            "{status}: only the project root and the un-anchored target exist"
        );
        let after = spec_row(&db, &spec_id);
        assert_eq!(
            after.status, status,
            "{status}: a refusal transitions nothing"
        );
        assert_eq!(
            after.workspace_id, None,
            "{status}: and the Spec is still detached"
        );
    }
}

// ============================================================================
// CLAUSE 3 — "Supprimer le workspace détache la Spec ; le comportement zéro candidate dépend du
//            rôle sans erreur interne."
// ============================================================================

/// THE DETACH IS REAL, THE ZERO CASE IS AN ANSWER, AND THE LINK CAN BE RE-POSED.
///
/// `specs.workspace_id` has carried `REFERENCES workspaces(id) ON DELETE SET NULL` since migration
/// 13, so this PRD ADDS no migration — which is exactly why the behaviour has to be TESTED here
/// rather than assumed: nothing in the code says "detach on delete", only the schema does, and a
/// gate is the only place that reads the schema back.
///
/// Then the ZERO case, which is the one a real project hits constantly (a workspace with no active
/// Spec). It is NOT an error condition — it is an answer, and WHICH answer depends on WHO is
/// asking. The axis used to be the run's SCOPE; since PDR-0031 it is the ROLE, and it is the same
/// distinction:
///
///   * `agent_context(role=dev|doc)` — they work ON a Spec ⇒ `invalid_id`, actionable, carrying the
///     case-0 code as `data.reason`;
///   * `agent_context(role=spec)`    — the AUTHOR creates one ⇒ a graceful project-scoped authoring
///     manual, never an error, carrying the same code in `warnings`.
///
/// Every one of those is asserted to be a MODELLED outcome, never an `internal`.
///
/// AND THE TWO SUB-CASES OF ZERO ARE TOLD APART, on both surfaces: `spec_location_unresolved` for a
/// folder no workspace claims (an invocation mistake — the agent is in the wrong place),
/// `spec_none_on_location` for a live workspace carrying no active Spec (the normal state of a
/// project about to author its first). Silence there was the original finding: an agent calling from
/// the wrong folder received a perfectly normal-looking context.
#[test]
fn removing_the_workspace_detaches_the_spec_and_zero_candidates_answers_by_role() {
    let tmp = temp_dir("detach");
    let db = open_db(&tmp);
    let project_id = project(&db, "Detach", &tmp.child("root"));
    let spec_id = spec_at(&db, &project_id, "Detachable", db::SPEC_STATUS_READY);
    let (ws_id, ws_root) = anchor_via(
        &db,
        "create_workspace",
        &project_id,
        &tmp.child("dev"),
        &spec_id,
    );
    assert_eq!(
        spec_row(&db, &spec_id).workspace_id.as_deref(),
        Some(ws_id.as_str())
    );

    // --- The removal, through the UI bridge's own use case (a full destructive cascade).
    let deleted = db
        .with_conn(|c| usecase::remove_workspace(c, &NullEventSink, &ws_id, DomainEventSource::Ui))
        .expect("remove_workspace");
    assert_eq!(deleted, 1);
    let orphaned = spec_row(&db, &spec_id);
    assert_eq!(
        orphaned.workspace_id, None,
        "the FK's ON DELETE SET NULL detached the Spec — the row survives its workspace"
    );
    assert_eq!(
        orphaned.status,
        db::SPEC_STATUS_READY,
        "…and detaching is not a lifecycle event: the Spec is still ready"
    );

    // --- ZERO candidates, by ROLE. The cwd now names a folder no workspace claims.
    for role in EFFECTOR_ROLES {
        let err = context(&db, json!({ "role": role, "cwd": ws_root })).expect_err("no Spec");
        assert_not_internal(&format!("{role}/zero"), &err);
        assert_eq!(err.code, RpcCode::InvalidId, "{role}");
        assert!(
            err.message.contains("works ON a Spec"),
            "{role}: the refusal says WHY a guess is worse than a refusal, got: {}",
            err.message
        );
        assert!(
            err.message.contains("spec_id"),
            "{role}: …and names what to send instead, got: {}",
            err.message
        );
        assert_eq!(
            reason_of(&err).as_deref(),
            Some(LOCATION_UNRESOLVED),
            "{role}: a cwd matching NO workspace is the invocation-mistake sub-case, got {:?}",
            err.data
        );
    }

    let authoring = context(&db, json!({ "role": "spec", "cwd": ws_root }))
        .expect("the AUTHOR is never refused for lack of a Spec");
    assert_eq!(authoring["spec_id"], Value::Null);

    // …AND THE CASE 0 SAYS SO. Rendering ok with `spec_id: null` and not one word about it is what
    // let an agent calling from the wrong folder mistake an unscoped manual for a scoped one. The
    // diagnosis rides BOTH surfaces — the `warnings` array a caller reads, and one explicit line
    // inside the manual an agent reads — and it is the SAME sentence.
    let warnings = authoring["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("a warnings array, got {authoring}"));
    assert_eq!(
        warnings.len(),
        1,
        "the unscoped render carries exactly one diagnosis, got {warnings:?}"
    );
    assert_eq!(
        warnings[0]["code"], LOCATION_UNRESOLVED,
        "a cwd matching NO workspace is the invocation-mistake sub-case",
    );
    let sentence = warnings[0]["message"].as_str().expect("a human sentence");
    let text = authoring["context"].as_str().expect("a rendered manual");
    assert!(
        text.contains("NO SPEC ANCHORED") && text.contains(sentence),
        "the manual itself carries the SAME line as the warning, got: {text}"
    );

    // --- ZERO candidates on a LIVE workspace that simply anchors nothing — the shape a real
    // project has, and the one that separates "no workspace here" from "no Spec here".
    let bare = tmp.child("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let bare_ws = ws_ok(
        &db,
        "workspace_add",
        json!({ "project_id": project_id, "path": bare }),
    )["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for role in EFFECTOR_ROLES {
        let err = context(&db, json!({ "role": role, "workspace_id": bare_ws }))
            .expect_err("a live workspace with zero active Specs still resolves none");
        assert_not_internal(&format!("{role}/bare"), &err);
        assert_eq!(err.code, RpcCode::InvalidId, "{role}");
        assert_eq!(
            reason_of(&err).as_deref(),
            Some(NONE_ON_LOCATION),
            "{role}: a REAL workspace carrying no Spec is told apart from a folder nyx never heard \
             of, got {:?}",
            err.data
        );
        assert!(
            err.message.contains(&bare_ws),
            "{role}: …and the sentence names the workspace it looked at: {}",
            err.message
        );
    }
    let on_bare = context(&db, json!({ "role": "spec", "workspace_id": bare_ws }))
        .expect("the author renders on a Spec-less workspace — that is where a first Spec is born");
    assert_eq!(
        on_bare["warnings"][0]["code"], NONE_ON_LOCATION,
        "the same distinction on the surface that answers ok, got: {}",
        on_bare["warnings"]
    );
    assert_eq!(
        on_bare["project_id"],
        json!(project_id),
        "…and the author is still told WHICH project they are authoring in: {on_bare}",
    );

    // --- RE-ANCHORING: the detached Spec takes a new dev workspace and resolution resumes.
    let (re_ws, re_root) = anchor_via(
        &db,
        "workspace_add",
        &project_id,
        &tmp.child("dev-again"),
        &spec_id,
    );
    assert_eq!(
        spec_row(&db, &spec_id).workspace_id.as_deref(),
        Some(re_ws.as_str()),
        "the anchor is 'posé, écrasable, jamais perdu' — a detached Spec re-anchors freely"
    );
    assert_eq!(
        spec_row(&db, &spec_id).status,
        db::SPEC_STATUS_READY,
        "…still without a transition"
    );
    let rendered = context(&db, json!({ "role": "dev", "cwd": re_root }))
        .expect("the re-anchored Spec resolves by location again");
    assert_eq!(rendered["spec_id"], json!(spec_id));
}

// ============================================================================
// CLAUSE 4 — "Deux Specs actives sur le même workspace produisent l'ambiguïté attendue et aucune
//            n'est choisie."
// ============================================================================

/// THE N CASE, AND THE PROPERTY THAT MATTERS: NOTHING IS PICKED.
///
/// A workspace legitimately accumulates Specs, so "several active Specs share one folder" is a
/// normal state, not a corruption. The contract is that resolving ONE by location is then REFUSED —
/// structurally (`error.data.reason = spec_ambiguous_for_workspace`), listing the candidates so the
/// caller can pass an explicit `spec_id`.
///
/// The assertions are built so that a resolver which quietly returned the newest (or the oldest, or
/// the first row) would FAIL them: both location anchors must refuse, for every role — the ambiguity
/// is the RESOLVER's verdict and no role is allowed to break the tie for itself — and the refusal
/// must NAME BOTH candidates.
///
/// A TERMINAL Spec is then added to the same workspace to prove the candidate set is the ACTIVE one:
/// a shipped Spec does not manufacture ambiguity, and the escape hatch the refusal advertises (an
/// explicit `spec_id`) is verified to work.
#[test]
fn two_active_specs_on_one_workspace_refuse_to_pick_either() {
    let tmp = temp_dir("ambiguous");
    let db = open_db(&tmp);
    let project_id = project(&db, "Ambiguous", &tmp.child("root"));

    let first = spec_at(&db, &project_id, "First spec", db::SPEC_STATUS_READY);
    let (ws_id, ws_root) = anchor_via(
        &db,
        "create_workspace",
        &project_id,
        &tmp.child("dev"),
        &first,
    );

    // The SECOND Spec joins the same dev workspace through the standalone anchor tool — the
    // gesture that actually produces this state in a live project.
    let second = spec_at(&db, &project_id, "Second spec", db::SPEC_STATUS_IN_PROGRESS);
    set_workspace(&db, json!({ "spec_id": second, "workspace_id": ws_id }))
        .expect("a second active Spec may share the dev workspace");

    let candidates = db
        .with_conn(|c| db::active_specs_for_workspace(c, &ws_id))
        .expect("candidate set");
    assert_eq!(
        candidates.len(),
        2,
        "the workspace really does anchor two active Specs"
    );

    // --- BOTH location anchors refuse, IDENTICALLY, for EVERY role.
    for (kind, anchor) in [
        ("workspace_id", json!({ "workspace_id": ws_id })),
        ("cwd", json!({ "cwd": ws_root })),
    ] {
        for role in ROLES {
            let mut args = anchor.clone();
            args.as_object_mut()
                .unwrap()
                .insert("role".into(), json!(role));
            let err = context(&db, args).expect_err("N candidates cannot resolve one");
            assert_not_internal(&format!("agent_context/{kind}/{role}"), &err);
            assert!(
                is_ambiguous(&err),
                "{kind}/{role}: the refusal carries the STRUCTURED reason `{AMBIGUOUS_REASON}`, \
                 got {:?} / {:?}",
                err.code,
                err.data
            );
            assert!(
                err.message.contains(&first) && err.message.contains(&second),
                "{kind}/{role}: BOTH candidates are listed so the caller can choose, got: {}",
                err.message
            );
            assert!(
                err.message.contains("pass an explicit spec_id"),
                "{kind}/{role}: the refusal names its own escape hatch, got: {}",
                err.message
            );
        }
    }

    // --- Neither Spec was touched by any of the refusals.
    assert_eq!(spec_row(&db, &first).status, db::SPEC_STATUS_READY);
    assert_eq!(spec_row(&db, &second).status, db::SPEC_STATUS_IN_PROGRESS);
    assert_eq!(
        spec_row(&db, &first).workspace_id.as_deref(),
        Some(ws_id.as_str())
    );
    assert_eq!(
        spec_row(&db, &second).workspace_id.as_deref(),
        Some(ws_id.as_str())
    );

    // --- THE ESCAPE HATCH the refusal advertises actually works.
    let rendered = context(
        &db,
        json!({ "role": "dev", "cwd": ws_root, "spec_id": first }),
    )
    .expect("an explicit spec_id resolves through an ambiguous location");
    assert_eq!(rendered["spec_id"], json!(first));

    // --- And a TERMINAL Spec is NOT a candidate: shipping one un-ambiguates the workspace.
    db.with_conn(|c| db::transition_spec(c, &second, SpecTransition::Cancel, true))
        .expect("ship the second Spec out of the active set");
    let rendered = context(&db, json!({ "role": "dev", "cwd": ws_root }))
        .expect("one active Spec remains, so the location resolves again");
    assert_eq!(
        rendered["spec_id"],
        json!(first),
        "the surviving ACTIVE Spec is the answer — the terminal one stopped competing"
    );
}

// ============================================================================
// CLAUSE 5 — "Les cas mauvais projet, rollback DB et dossier neuf compensé sont couverts."
// ============================================================================

/// THE THREE FAILURE MODES OF THE DECLARATION, each proven to leave the world exactly as it was.
///
/// (a) WRONG PROJECT — a Spec of project A cannot be anchored on a workspace being registered in
///     project B. Refused by the PREVALIDATION, so on the `create_workspace` path not even the
///     folder is created. The RENDER's own cross-project guard is checked on the same data: a
///     `project_id` of B passed beside a Spec of A is CONFRONTED rather than silently dropped.
///
/// (b) DB ROLLBACK + (c) FRESH-FOLDER COMPENSATION, in ONE reachable production scenario. A
///     workspace row OUTLIVES its folder by design (PDR-0008: a worktree cleaned up at merge keeps
///     its row), so re-declaring that path is a real thing an agent does — and it hits the
///     `UNIQUE(project_id, root_path)` constraint AFTER `create_workspace` has already `mkdir`ed
///     the folder back. That is the exact shape the compensation exists for: the DB write fails
///     late, and what this call created on disk must disappear with it, while the anchor it was
///     going to pose must never have happened.
///
/// The contrast case — the same failing call on a path that PRE-EXISTS and holds data — closes the
/// clause: "dossier neuf compensé" only means something if a folder nyx did not create is provably
/// safe from the rollback.
#[test]
fn a_wrong_project_a_db_rollback_and_a_fresh_folder_all_leave_nothing_behind() {
    let tmp = temp_dir("failures");
    let db = open_db(&tmp);
    let alpha = project(&db, "Alpha", &tmp.child("alpha-root"));
    let beta = project(&db, "Beta", &tmp.child("beta-root"));
    let spec_id = spec_at(&db, &alpha, "Alpha spec", db::SPEC_STATUS_READY);

    // Its legitimate dev workspace in Alpha — the anchor every failure below must leave alone.
    let (home_ws, _) = anchor_via(
        &db,
        "create_workspace",
        &alpha,
        &tmp.child("alpha-dev"),
        &spec_id,
    );

    // --- (a) WRONG PROJECT, on both tools.
    let fresh = tmp.child("beta-never-created");
    let err = ws_tool(
        &db,
        "create_workspace",
        json!({ "project_id": beta, "name": "dev", "path": fresh, "spec_id": spec_id }),
    )
    .expect_err("a Spec cannot be anchored across projects");
    assert_not_internal("create_workspace/cross-project", &err);
    assert_eq!(err.code, RpcCode::InvalidArgument);
    assert!(
        err.message
            .contains("the workspace would be registered in project"),
        "the refusal is worded against the TARGET project (no workspace exists yet), got: {}",
        err.message
    );
    assert!(
        !Path::new(&fresh).exists(),
        "the cross-project refusal ran before the mkdir"
    );

    let beta_dir = tmp.child("beta-existing");
    std::fs::create_dir_all(&beta_dir).unwrap();
    let err = ws_tool(
        &db,
        "workspace_add",
        json!({ "project_id": beta, "path": beta_dir, "spec_id": spec_id }),
    )
    .expect_err("a Spec cannot be anchored across projects");
    assert_not_internal("workspace_add/cross-project", &err);
    assert_eq!(err.code, RpcCode::InvalidArgument);
    assert_eq!(
        workspaces_of(&db, &beta).len(),
        1,
        "Beta gained no workspace from the refused declaration (root only)"
    );

    // The RENDER's twin guard: a Spec of Alpha named beside a `project_id` of Beta is CONFRONTED
    // and not silently dropped. Dropping it left a byte-identical manual behind, and the caller
    // believed it had scoped the render.
    let err = context(
        &db,
        json!({ "role": "dev", "spec_id": spec_id, "project_id": beta }),
    )
    .expect_err("a spec_id and a project_id that disagree are refused");
    assert_not_internal("agent_context/cross-project", &err);
    assert_eq!(err.code, RpcCode::InvalidArgument);
    assert!(
        err.message.contains(&alpha) && err.message.contains(&beta),
        "the disagreement NAMES both projects, never silently resolves either way, got: {}",
        err.message
    );

    // --- (b)+(c) A LATE DB failure on a folder this call created.
    let reclaimed = tmp.child("alpha-worktree");
    std::fs::create_dir_all(&reclaimed).unwrap();
    ws_ok(
        &db,
        "workspace_add",
        json!({ "project_id": alpha, "path": reclaimed }),
    );
    let before = workspaces_of(&db, &alpha).len();
    // The folder is cleaned up (merge), the ROW stays — PDR-0008.
    std::fs::remove_dir_all(&reclaimed).unwrap();
    assert!(
        !Path::new(&reclaimed).exists(),
        "precondition: the folder is gone, the row is not"
    );

    let err = ws_tool(
        &db,
        "create_workspace",
        json!({ "project_id": alpha, "name": "dev", "path": reclaimed, "spec_id": spec_id }),
    )
    .expect_err("the path is already a workspace of this project");
    assert_not_internal("create_workspace/duplicate", &err);
    assert_eq!(err.code, RpcCode::InvalidState);
    assert!(
        !Path::new(&reclaimed).exists(),
        "COMPENSATION: the folder THIS call created was removed when the DB write failed"
    );
    assert_eq!(
        workspaces_of(&db, &alpha).len(),
        before,
        "ROLLBACK: no partial workspace row survived the failure"
    );
    let after = spec_row(&db, &spec_id);
    assert_eq!(
        after.workspace_id.as_deref(),
        Some(home_ws.as_str()),
        "ROLLBACK: the anchor the failed call was going to pose never landed — the Spec still \
         points at the workspace it had"
    );
    assert_eq!(
        after.status,
        db::SPEC_STATUS_READY,
        "…and its status never moved"
    );

    // --- The contrast: the SAME failure on a path nyx did not create leaves it fully intact.
    let precious = tmp.child("alpha-precious");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(Path::new(&precious).join("keep.txt"), b"user data").unwrap();
    ws_ok(
        &db,
        "workspace_add",
        json!({ "project_id": alpha, "path": precious }),
    );
    let err = ws_tool(
        &db,
        "create_workspace",
        json!({ "project_id": alpha, "name": "dev", "path": precious, "spec_id": spec_id }),
    )
    .expect_err("the path is already a workspace of this project");
    assert_eq!(err.code, RpcCode::InvalidState);
    assert!(
        Path::new(&precious).join("keep.txt").is_file(),
        "a pre-existing path is NEVER removed by the compensation — its content is untouched"
    );

    // --- The OTHER direction of the shared transaction: the ANCHOR half refusing must roll the
    // WORKSPACE half back. It is driven at the use-case seam ON PURPOSE, because it is UNREACHABLE
    // through the tools: `prevalidate_spec_anchor` rejects an unanchorable Spec before any write,
    // so making the tool fail here would take the Spec being cancelled BETWEEN the prevalidation
    // and the write. The seam is nonetheless the exact one both tools funnel into
    // (`create_workspace_inner` → `register_workspace_anchored`), so this is where "les deux
    // écritures partagent une transaction" is actually observable — and without it the clause would
    // only ever prove the half where the INSERT fails first and the anchor never got its turn.
    //
    // PROOF THAT IT BITES: in the PRD's mutation campaign, mutation M4 ("ancrage hors transaction",
    // `usecase.rs:596`) turns THIS block red and NOTHING ELSE in the gate. It knowingly duplicates
    // the unit test `a_refused_anchor_rolls_the_workspace_insert_back`
    // (`crates/nyx-core/src/mcp_runtime.rs`); the duplication is deliberate and is what keeps the
    // gate itself — not a test the gate never runs — mordant on transactionality. See the module
    // header's "one exception" section before touching either copy.
    let draft = spec_at(&db, &alpha, "Still authoring", db::SPEC_STATUS_DRAFT);
    let orphan_path = tmp.child("alpha-orphan");
    std::fs::create_dir_all(&orphan_path).unwrap();
    let before = workspaces_of(&db, &alpha).len();
    let err = db
        .with_conn(|c| {
            usecase::register_workspace_anchored(
                c,
                &NullEventSink,
                &alpha,
                "orphan",
                &orphan_path,
                Some(&draft),
                DomainEventSource::Mcp,
            )
        })
        .expect_err("a draft Spec cannot be anchored");
    assert!(
        matches!(err, usecase::RegisterWorkspaceError::Anchor(_)),
        "the ANCHOR half is what refused, got {err:?}"
    );
    assert_eq!(
        workspaces_of(&db, &alpha).len(),
        before,
        "ROLLBACK: the workspace INSERT went back with the refused anchor — no partial workspace"
    );
    assert_eq!(
        spec_row(&db, &draft).workspace_id,
        None,
        "…and the draft Spec is still detached"
    );
}

/// THE COMPATIBILITY CLAUSE — "compatibilité sans `spec_id`" (task scope). Both tools, driven with
/// NO `spec_id` at all, keep the pre-anchor contract EXACTLY: the payload carries `workspace` and
/// nothing else (no `spec` key, no extra key), and the resulting workspace anchors no Spec — so a
/// caller written before this PRD, and the UI bridge's own unanchored path, are untouched.
///
/// REVIEW F2 closes the clause with its opposite: an EMPTY STRING is not that absence. All three
/// anchoring tools (`workspace_add`, `create_workspace`, `spec_set_workspace`) refuse `""` with
/// `invalid_argument` instead of silently falling back to the no-anchor behaviour asserted above —
/// which is how a caller with an uninitialized variable used to detach a Spec, or register a
/// workspace it believed anchored, without a single error.
#[test]
fn without_spec_id_both_tools_keep_their_pre_anchor_contract() {
    let tmp = temp_dir("compat");
    let db = open_db(&tmp);
    let project_id = project(&db, "Compat", &tmp.child("root"));
    // A ready Spec EXISTS in the project and is anchored nowhere: a tool that helpfully guessed
    // would attach it, and this clause is what forbids guessing.
    let spec_id = spec_at(&db, &project_id, "Unrelated", db::SPEC_STATUS_READY);

    for (tool, preexisting) in [("create_workspace", false), ("workspace_add", true)] {
        let path = tmp.child(tool);
        if preexisting {
            std::fs::create_dir_all(&path).unwrap();
        }
        let res = ws_ok(
            &db,
            tool,
            json!({ "project_id": project_id, "name": tool, "path": path }),
        );
        assert!(
            res["workspace"].is_object(),
            "{tool}: the workspace is returned"
        );
        assert_eq!(
            res.as_object().map(|o| o.len()),
            Some(1),
            "{tool}: the payload carries EXACTLY the pre-anchor key set — no `spec` key"
        );
        assert!(
            Path::new(&path).is_dir(),
            "{tool}: the folder handling is unchanged"
        );

        let ws_id = res["workspace"]["id"].as_str().unwrap();
        assert!(
            db.with_conn(|c| db::active_specs_for_workspace(c, ws_id))
                .unwrap()
                .is_empty(),
            "{tool}: a declaration without spec_id anchors NOTHING — nyx never guesses"
        );
    }
    assert_eq!(
        spec_row(&db, &spec_id).workspace_id,
        None,
        "the project's lone ready Spec was left alone by both declarations"
    );

    // REVIEW F2 — …AND THE EMPTY STRING IS NOT THAT ABSENCE. The clause above is what `""` used to
    // silently fall back to on all three anchoring tools: `optional_str` mapped it to `None`, so a
    // caller passing an uninitialized variable got "register it unlinked" (here) or "detach it"
    // (`spec_set_workspace`, verified in production on a Spec left with `workspace_id` NULL) and no
    // error at all. The three now refuse it, each naming its own argument — and the refusal of
    // `create_workspace` still lands before the `mkdir`.
    let empty_dir = tmp.child("empty-anchor-never-created");
    let existing_dir = tmp.child("empty-anchor-existing");
    std::fs::create_dir_all(&existing_dir).unwrap();
    let refusals: [(&str, Value, &str); 3] = [
        (
            "create_workspace",
            json!({ "project_id": project_id, "name": "nope", "path": empty_dir, "spec_id": "" }),
            "spec_id",
        ),
        (
            "workspace_add",
            json!({ "project_id": project_id, "name": "nope", "path": existing_dir, "spec_id": "" }),
            "spec_id",
        ),
        (
            "spec_set_workspace",
            json!({ "spec_id": spec_id, "workspace_id": "" }),
            "workspace_id",
        ),
    ];
    for (tool, args, argument) in refusals {
        let err = if tool == "spec_set_workspace" {
            set_workspace(&db, args).expect_err("the empty string is refused")
        } else {
            ws_tool(&db, tool, args).expect_err("the empty string is refused")
        };
        assert_not_internal(tool, &err);
        assert_eq!(err.code, RpcCode::InvalidArgument, "{tool}");
        assert!(
            err.message.contains(argument) && err.message.contains("EMPTY string"),
            "{tool}: the refusal names '{argument}' and its fault, got: {}",
            err.message
        );
    }
    assert!(
        !Path::new(&empty_dir).exists(),
        "create_workspace refused the empty anchor BEFORE the mkdir — no folder left behind"
    );
    assert_eq!(
        spec_row(&db, &spec_id).workspace_id,
        None,
        "and nothing in that campaign touched the Spec's anchor"
    );
}

// ============================================================================
// CLAUSE 6 — THE REFERENCE THE HUMAN TYPED
// ============================================================================

/// THE OTHER HALF OF "SANS ID EXPLICITE": the human DOES type something, and it is almost never a
/// canonical id. `#3`, `KEY-3`, the bare `3` — the forms every nyx surface displays — reach
/// `agent_context` as the RAW token the hook forwarded, and nyx translates them
/// ([`nyx_core::spec_ref`] owns the grammar; the tool owns the refusal).
///
/// THE REFUSALS ARE THE POINT, and they are why this clause moved here with the rest (PDR-0031).
/// The session hook relays the sentence into the model in the manual's place, so it has to carry
/// three things: the token AS TYPED, the accepted forms, and what the project actually holds. That
/// is what lets an agent stop and ASK which Spec was meant instead of authoring a duplicate of the
/// very one that was named.
///
/// Two refusals, and they are deliberately DIFFERENT:
///
///   * a reference that designates no Spec ⇒ `invalid_id` — for EVERY role, `/nspec` included: they
///     named an existing Spec, and authoring a second is exactly how the duplicate gets ordered;
///   * a PROJECT-RELATIVE form (`#N`, bare `N`) on a call that resolves NO project ⇒
///     `invalid_argument` naming [`nyx_core::spec_ref::ANCHORLESS_FORMS`] — the Spec is not
///     unknown, the FORM is unusable here, and the two must not read alike.
#[test]
fn the_four_reference_forms_resolve_and_a_refusal_names_what_to_send() {
    let tmp = temp_dir("grammar");
    let db = open_db(&tmp);
    let project = project_row(&db, "Grammar", &tmp.child("root"));
    let key = project.key.clone();
    // Three Specs, so `3` is a real per-project number and not the only row in the table.
    spec_at(&db, &project.id, "First", db::SPEC_STATUS_DRAFT);
    spec_at(&db, &project.id, "Second", db::SPEC_STATUS_DRAFT);
    let third = spec_at(&db, &project.id, "Third", db::SPEC_STATUS_READY);
    assert_eq!(
        spec_row(&db, &third).number,
        3,
        "the subject Spec is the project's #3 — the reference a human types"
    );
    // Its dev workspace: the anchor a `#N` is counted in when no project_id is passed, which is the
    // shape the hook actually sends (`{ role, cwd, spec_id }`).
    let (_ws, ws_root) = anchor_via(
        &db,
        "create_workspace",
        &project.id,
        &tmp.child("dev"),
        &third,
    );

    // --- THE FOUR FORMS, each resolving to the same Spec, on every role.
    let forms: [(&str, String, Value); 4] = [
        (
            "canonical id",
            third.clone(),
            json!({ "project_id": project.id }),
        ),
        ("human key", format!("{key}-3"), json!({})),
        (
            "#N, counted in the project the cwd resolves",
            "#3".to_string(),
            json!({ "cwd": ws_root }),
        ),
        (
            "bare N, counted in an explicit project",
            "3".to_string(),
            json!({ "project_id": project.id }),
        ),
    ];
    for (label, token, anchor) in forms {
        for role in ROLES {
            let mut args = anchor.clone();
            let obj = args.as_object_mut().unwrap();
            obj.insert("role".into(), json!(role));
            obj.insert("spec_id".into(), json!(token));
            let rendered = context(&db, args)
                .unwrap_or_else(|e| panic!("{label}/{role}: '{token}' was refused: {}", e.message));
            assert_eq!(
                rendered["spec_id"],
                json!(third),
                "{label}/{role}: '{token}' designates Spec #3 and nyx resolved it to its \
                 canonical id — the caller never translates"
            );
        }
    }

    // --- A REFERENCE THAT DESIGNATES NOTHING — refused for EVERY role, `spec` included.
    for role in ROLES {
        let err = context(
            &db,
            json!({ "role": role, "project_id": project.id, "spec_id": "99" }),
        )
        .expect_err("'99' designates no Spec of this project");
        assert_not_internal(&format!("{role}/unknown"), &err);
        assert_eq!(
            err.code,
            RpcCode::InvalidId,
            "{role}: the REFERENCE is unknown, got: {}",
            err.message
        );
        assert!(
            err.message.contains("'99'"),
            "{role}: the refusal names the token AS TYPED, got: {}",
            err.message
        );
        assert!(
            err.message.contains(nyx_core::spec_ref::ACCEPTED_FORMS),
            "{role}: …the forms nyx accepts, read from the grammar's own constant, got: {}",
            err.message
        );
        assert!(
            err.message.contains(&third),
            "{role}: …and what the project actually HOLDS, by id, so the next call can be formed \
             from the answer: {}",
            err.message
        );
    }

    // --- A PROJECT-RELATIVE FORM ON A CALL THAT RESOLVES NO PROJECT — a different refusal, on
    // purpose: nothing is unknown, the FORM cannot be counted here.
    for token in ["#3", "3"] {
        let err = context(&db, json!({ "role": "dev", "spec_id": token }))
            .expect_err("a number cannot be counted with no project in scope");
        assert_not_internal(&format!("anchorless/{token}"), &err);
        assert_eq!(
            err.code,
            RpcCode::InvalidArgument,
            "{token}: the FORM is unusable here — the Spec is not unknown, got: {}",
            err.message
        );
        assert!(
            err.message.contains(nyx_core::spec_ref::ANCHORLESS_FORMS),
            "{token}: the refusal names what to send instead, got: {}",
            err.message
        );
    }

    // --- Nothing in that campaign wrote anything: a resolution is a READ.
    assert_eq!(
        db.with_conn(|c| db::list_specs(c, Some(&project.id)))
            .expect("list")
            .len(),
        3,
        "the project still holds its three Specs — no reference minted one"
    );
    assert_eq!(spec_row(&db, &third).status, db::SPEC_STATUS_READY);
}
