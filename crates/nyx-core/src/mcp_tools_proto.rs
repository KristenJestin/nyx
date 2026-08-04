//! Shell-agnostic MCP **PROTOTYPE tools** — the agent's CONSTRAINED contract for driving a
//! Spec's prototype annexe (v2.7 — Prototype : machinerie, Phase 2, task
//! `01KY4DFP6DRG8TK7J4VD4TSNQA` — "Domain events proto + contrat d'outil de l'agent proto").
//!
//! # The contrat d'outil (the real fix for the "fouilli", PDR Impl Decisions)
//!
//! The v1 prototype agent fumbled because nothing CONSTRAINED its actions: it navigated and
//! mutated the design in ways the user never saw. The fix is NOT the schema — it is this TOOL
//! CONTRACT. Every action the agent can take on a prototype is a NAMED, GUARDED tool with
//! EXPLICIT states:
//!
//!   * [`proto_get`] surfaces the EXPLICIT current state (the prototype, its rounds, the current
//!     round + its manifest, the open pre-dev comments) so the agent READS where it is instead of
//!     guessing — no blind navigation.
//!   * [`proto_composition`] READS the FROZEN elected composition — the coder SEAM (v2.7 Phase 4):
//!     the arbre nommé = placement (3 natures `[DS]`/`[NEW]`/`[inline]`) + the pre-dev review as
//!     ANCHORS + COUNTS (never the comment bodies — PDR-0009, review #47) + the design-system as a
//!     REFERENCE. Exposed ONLY once the design is LOCKED, so what a future harness consumes can never
//!     drift under it.
//!   * the six MUTATING tools ([`proto_present_round`], [`proto_elect_variant`],
//!     [`proto_feedback_create`], [`proto_feedback_resolve`], [`proto_rebase`], [`proto_distill`])
//!     each perform ONE explicit action, validate their preconditions up front (an unknown /
//!     not-pinned id is a structured `invalid_id` / `invalid_state`, never a silent no-op), and go
//!     through the SHARED `nyx_core::db` authority — which records the matching `proto.*` DOMAIN
//!     EVENT in the same transaction. So every meaningful action leaves a VISIBLE, ORDERED trace:
//!     "aucune action/navigation de l'agent inaperçue".
//!
//! Together with the two reads (`proto_get`, `proto_composition`), that is 8 read/lifecycle tools
//! (2 reads + 6 mutations). Review #34 adds a CREATION/amorçage half — the tools that BOOTSTRAP a
//! prototype (create the proto on a Spec, add pages / versions / variants, pin pages into a round,
//! open a round, and author a `[NEW]` greenfield catalogue component) — because the 8 above all
//! PRESUPPOSE the tree exists and NONE creates it, so the flow was un-amorçable from the agent seat.
//! Every creator reuses the `db::create_*` authority, respects the Phase-4 "élection figée" lock,
//! and emits `prototypes://changed`. Review #46 adds the missing SIXTEENTH,
//! [`proto_recommend_variant`]: the manual describes `is_main` as the AGENT's recommendation
//! (distinct from the USER's RETENU) but no tool posed it, so the agent could not express it. That is
//! [`PROTO_TOOLS`] = 16 (8 read/lifecycle + 7 creation + 1 reco).
//!
//! # Single mutation authority (no double path)
//!
//! Every tool is a THIN adapter over the `db` prototype authority (`present_round` /
//! `elect_round_page_variant` / `create_feedback` / `resolve_feedback` + the reads). The domain
//! events, the freeze semantics and the durable-comment model all live BELOW the tool, so the MCP
//! mutation is the single authority the native proto UI (a later phase) reads from too.
//!
//! # Effects
//!
//! A prototype lives ON a Spec, so each mutating tool returns [`ChangedTopic::Prototypes`] — an
//! open proto surface (and the future proto overlay, PDR-0009) re-pulls after an agent action.
//!
//! # Every answer is CAPPED (review 01KYMF0KFZQG5JYZDGZCR85BAM)
//!
//! A constrained contract that answers with an unreadable payload is not constrained. Both READS
//! grow with the prototype — [`proto_get`] carried every round, page and open comment WITH ITS BODY;
//! [`proto_composition`] carried every page's whole tree — so both now BOUND their answer under the
//! shared [`crate::mcp_runtime::MAX_RESPONSE_BYTES`], report what they left out (`truncated` +
//! `total`/`returned`) and hand back the offset to resume from. The NAIVE call — the one the manual
//! tells an agent to start with — is safe BY CONSTRUCTION, not by the agent remembering to paginate.
//!
//! Two rules shape HOW they cut. The elected `composition_json` is never trimmed: it IS the coder
//! contract, so [`proto_composition`] paginates PAGE BY PAGE and an over-large page is REPORTED
//! (`oversized`) rather than mutilated — half a placement would be a FALSE contract, worse than the
//! overflow it avoids. And the cap lives on the ROUTER ([`dispatch_proto_tool`]), so a tool that
//! never learned to page still cannot answer past it.

use serde_json::{json, Value};

use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{
    bounded_row, bounded_tool_result, internal_db, json_len, optional_bool, optional_str,
    optional_usize, require_str, under_response_budget_strict, MAX_RESPONSE_BYTES,
    RESPONSE_ENVELOPE_BYTES,
};
use crate::mcp_tools_core::{ChangedTopic, ToolOutcome};

// --- Tool name constants (advertised in `tools/list`, dispatched below) ------

/// `proto_get` — read a Spec's prototype and its EXPLICIT current state (rounds / manifest /
/// open comments). The agent's "where am I" read.
pub const PROTO_GET_TOOL: &str = "proto_get";
/// `proto_present_round` — PRESENT (freeze) a round to the user (a handback).
pub const PROTO_PRESENT_ROUND_TOOL: &str = "proto_present_round";
/// `proto_elect_variant` — record the USER's RETENU variant for a page in a round.
pub const PROTO_ELECT_VARIANT_TOOL: &str = "proto_elect_variant";
/// `proto_recommend_variant` — pose the AGENT's `is_main` RECOMMENDATION on a variant (≠ the RETENU).
pub const PROTO_RECOMMEND_VARIANT_TOOL: &str = "proto_recommend_variant";
/// `proto_feedback_create` — raise a pre-dev review comment anchored to a tree node.
pub const PROTO_FEEDBACK_CREATE_TOOL: &str = "proto_feedback_create";
/// `proto_feedback_resolve` — resolve a pre-dev review comment (the agent addressed it).
pub const PROTO_FEEDBACK_RESOLVE_TOOL: &str = "proto_feedback_resolve";
/// `proto_rebase` — roll back onto an earlier round (open a new round cloning its manifest).
pub const PROTO_REBASE_TOOL: &str = "proto_rebase";
/// `proto_distill` — freeze the elected round as THE design for the Spec (figer l'élection).
pub const PROTO_DISTILL_TOOL: &str = "proto_distill";
/// `proto_composition` — READ the FROZEN elected composition (the coder SEAM: arbre = placement, 3
/// natures, resolved review, design-system as reference). Read-only.
pub const PROTO_COMPOSITION_TOOL: &str = "proto_composition";

// --- CREATION / amorçage tools (review #34) --------------------------------
// The 8 tools above are read/lifecycle and PRESUPPOSE the proto tree exists; NONE creates one, so
// the flow was un-amorçable from the agent seat. These CREATE + compose the structure (reusing the
// `db::create_*` authorities that previously fired only in tests) so an agent can BOOTSTRAP a
// prototype end-to-end via MCP. Every one respects the Phase-4 "élection figée" lock (creation on a
// design-locked round is refused) and emits `prototypes://changed`.

/// `proto_create` — CREATE the prototype on a Spec (1/Spec, seeds round `v1`).
pub const PROTO_CREATE_TOOL: &str = "proto_create";
/// `proto_add_page` — add a PAGE (logical screen) to a prototype.
pub const PROTO_ADD_PAGE_TOOL: &str = "proto_add_page";
/// `proto_add_version` — add a frozen VERSION (iteration) to a page.
pub const PROTO_ADD_VERSION_TOOL: &str = "proto_add_version";
/// `proto_add_variant` — add a VARIANT (one concrete rendering) to a page version.
pub const PROTO_ADD_VARIANT_TOOL: &str = "proto_add_variant";
/// `proto_pin_page` — PIN a (page, version) into a round's manifest.
pub const PROTO_PIN_PAGE_TOOL: &str = "proto_pin_page";
/// `proto_open_round` — OPEN a new (empty) current round (reuses `create_round`).
pub const PROTO_OPEN_ROUND_TOOL: &str = "proto_open_round";
/// `proto_author_component` — AUTHOR a `[NEW]` greenfield catalogue component from a proto (PDR-0010).
pub const PROTO_AUTHOR_COMPONENT_TOOL: &str = "proto_author_component";

/// The prototype tool names — advertised in `tools/list` and routed by [`dispatch_proto_tool`].
/// A closed slice so a new tool is added in exactly one place. 8 read/lifecycle (v2.7 Phase 2) +
/// 7 creation/amorçage (review #34) + the agent's reco (review #46).
pub const PROTO_TOOLS: [&str; 16] = [
    PROTO_GET_TOOL,
    PROTO_PRESENT_ROUND_TOOL,
    PROTO_ELECT_VARIANT_TOOL,
    PROTO_RECOMMEND_VARIANT_TOOL,
    PROTO_FEEDBACK_CREATE_TOOL,
    PROTO_FEEDBACK_RESOLVE_TOOL,
    PROTO_REBASE_TOOL,
    PROTO_DISTILL_TOOL,
    PROTO_COMPOSITION_TOOL,
    PROTO_CREATE_TOOL,
    PROTO_ADD_PAGE_TOOL,
    PROTO_ADD_VERSION_TOOL,
    PROTO_ADD_VARIANT_TOOL,
    PROTO_PIN_PAGE_TOOL,
    PROTO_OPEN_ROUND_TOOL,
    PROTO_AUTHOR_COMPONENT_TOOL,
];

// --- Argument helpers ------------------------------------------------------

/// The `invalid_id` an unknown prototype-domain id maps to.
fn unknown(kind: &str, id: &str) -> RpcError {
    RpcError::new(RpcCode::InvalidId, format!("unknown {kind} {id}"))
}

/// The `invalid_state` a mutation on a FROZEN (design-locked) round maps to — the "élection figée"
/// refusal (PDR-0009). Orients the caller toward opening a NEW round (`proto_rebase`) instead of
/// mutating the frozen one, so the elected composition the coder consumes can NEVER drift.
fn round_locked(locked_round_id: &str) -> RpcError {
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "round {locked_round_id} is FROZEN (its design is locked as THE design): its election \
             is immutable. To iterate, OPEN A NEW ROUND (proto_rebase) — a frozen round is never \
             mutated (PDR-0009: feedback ⇒ nouveau round, jamais muter)."
        ),
    )
}

/// Resolve the optional `author_kind` arg to the vocabulary (`session` | `user`). Defaults to
/// `session` (the agent authors via MCP). An unknown value → `invalid_argument`.
fn parse_author_kind(args: &Value) -> Result<&'static str, RpcError> {
    match optional_str(args, "author_kind")? {
        None | Some("session") => Ok(db::FEEDBACK_AUTHOR_SESSION),
        Some("user") => Ok(db::FEEDBACK_AUTHOR_USER),
        Some(other) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown author_kind '{other}' (accepted: session, user)"),
        )),
    }
}

/// An OPTIONAL non-negative `position` argument as `i32` (defaulting to 0), for the CREATION tools'
/// ordering. Reuses [`optional_usize`] (rejects negatives / non-integers) then narrows to `i32` —
/// the schema's column type — so an absurd position can never overflow the row.
fn optional_position(args: &Value, key: &str) -> Result<i32, RpcError> {
    Ok(optional_usize(args, key)?
        .map(|n| n.min(i32::MAX as usize) as i32)
        .unwrap_or(0))
}

/// The DUPLICATE-LABEL refusal, with THE ID OF THE ROW IT COLLIDED WITH (finding
/// `01KZ3JS646G83NF4NRKY8VX5CP`).
///
/// A conflict is the ONE moment nyx knows the id an agent is looking for and the agent does not: it
/// just proved the row exists. Answering "a version labelled 'v1' already exists" and nothing else
/// left a resumed session strictly worse off than before it asked — it had a name, it still had no
/// id, and no read exposed one. So every creator's conflict names it, and the message says what to
/// do with it.
fn duplicate(
    kind: &str,
    field: &str,
    value: &str,
    parent_kind: &str,
    parent_id: &str,
    existing_id: Option<String>,
) -> String {
    match existing_id {
        Some(id) => format!(
            "a {kind} with {field} '{value}' already exists in {parent_kind} {parent_id}: its id is \
             {id} — use it, or create this one under another {field}"
        ),
        // The row disappeared between the collision and this read (a concurrent delete). Say so
        // rather than invent an id.
        None => format!(
            "a {kind} with {field} '{value}' already exists in {parent_kind} {parent_id} (its id \
             could not be re-read — call proto_get(spec_id) to list them)"
        ),
    }
}

/// Map a `db::create_*` result to the tool boundary, turning a UNIQUE-constraint violation (a
/// duplicate slug / label, or a second prototype on a Spec) into an explicit `invalid_state` with
/// `conflict_msg` rather than a raw internal DB error — so the agent gets a legible refusal it can
/// act on. Any other DB error propagates (→ `internal_db`). The `Ok(Err(..))` shape matches the
/// `with_conn(|c| ...)?? ` pattern the other guarded tools use.
fn created_or_conflict<T>(
    result: diesel::QueryResult<T>,
    conflict_msg: impl FnOnce() -> String,
) -> Result<Result<T, RpcError>, diesel::result::Error> {
    match result {
        Ok(v) => Ok(Ok(v)),
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => Ok(Err(RpcError::new(RpcCode::InvalidState, conflict_msg()))),
        Err(other) => Err(other),
    }
}

/// Map a [`db::RoundMutationError`] to the tool boundary: the Phase-4 `RoundLocked` refusal becomes
/// the shared [`round_locked`] `invalid_state`; a DB error propagates. The single place the CREATION
/// tools ([`proto_add_variant`], [`proto_pin_page`]) translate the "élection figée" lock guard.
fn map_round_mutation<T>(
    result: Result<T, db::RoundMutationError>,
) -> Result<Result<T, RpcError>, diesel::result::Error> {
    match result {
        Ok(v) => Ok(Ok(v)),
        Err(db::RoundMutationError::RoundLocked { locked_round_id }) => {
            Ok(Err(round_locked(&locked_round_id)))
        }
        Err(db::RoundMutationError::Db(e)) => Err(e),
    }
}

// --- Tool handlers ---------------------------------------------------------

/// The LISTS [`proto_get`] carries — each one GROWS with the prototype, so each one is a section the
/// tool bounds, counts and pages independently. Named exactly as the response keys, so
/// `section: "<the key whose list was cut>"` needs no translation table.
///
/// `versions` and `variants` were added by finding `01KZ3JS646G83NF4NRKY8VX5CP`: they used to exist
/// NOWHERE in the agent surface. A version id and a variant id were recoverable only from the
/// response of the call that CREATED them — and a duplicate creation did not even hand them back —
/// so a resumed session, or a second agent, could no longer recommend, elect or preview anything. A
/// state read that cannot name what an action takes is not a state read.
const PROTO_GET_SECTIONS: [&str; 6] = [
    "rounds",
    "current_round_manifest",
    "pages",
    "versions",
    "variants",
    "open_feedback",
];

/// How many rows a bounded proto LIST returns by default. The byte budget usually binds first; this
/// keeps a well-behaved prototype's answer small anyway.
const DEFAULT_LIST_LIMIT: usize = 25;

/// The ceiling on `limit`, whatever a caller passes. The rest is reached by PAGING (`next_offset`).
const MAX_LIST_LIMIT: usize = 200;

/// `proto_get` — the agent's "where am I" read, in TWO shapes:
///
/// * `{ spec_id }` → `{ prototype, current_round, design_lock, locked, rounds,
///   current_round_manifest, pages, versions, variants, open_feedback, truncated, sections,
///   warnings }` — the OVERVIEW: the prototype's fixed head plus a BOUNDED slice of each of its
///   growing lists, with `sections.<name> = { total, returned, truncated, next_offset }`;
/// * `{ spec_id, section, offset?, limit? }` → `{ mode: "section", section, total, offset,
///   returned, truncated, next_offset, items }` — a PAGE of ONE list ([`PROTO_GET_SECTIONS`]).
///
/// A Spec with NO prototype is a valid state, returned as `{ prototype: null }` (not an error) —
/// the agent's cue that there is nothing to drive yet.
///
/// # What a resumed session needs, and used to be unable to get (finding `01KZ3JS646G83NF4NRKY8VX5CP`)
///
/// Two holes in the same family, both closed here.
///
/// * THE IDENTIFIERS. Pages carried `id`/`slug`/`title` and nothing under them: versions and
///   variants appeared in no read at all. Their ids came back ONLY from the call that created
///   them — and a duplicate creation refused WITHOUT the id — so an agent that lost its transcript
///   could no longer `proto_recommend_variant`, `proto_elect_variant` or `design_preview(variant_id)`
///   on anything but a page already elected. The `versions` and `variants` sections carry them, each
///   row naming its parent (`page_id` / `page_version_id`) so the three lists JOIN, and a variant is
///   projected to its IDENTITY (never its `composition_json`: a state read must not carry the trees).
/// * THE LOCK. The freeze was readable nowhere: a locked prototype said nothing about it, and the
///   only ways to find out were to be REFUSED by a mutation or to call `proto_composition`. The head
///   now carries `locked` + `design_lock` (which round, when, by whom, whether that round was ever
///   presented), so "can I still elect here?" is answered by the read the manual makes you do first.
///
/// # Why the naive call is capped (review 01KYMF0KFZQG5JYZDGZCR85BAM)
///
/// This tool used to echo EVERYTHING: every round, the whole manifest, every page, and every OPEN
/// comment WITH ITS BODY. All four grow with the prototype, so `proto_get(spec_id)` — the very first
/// call the manual tells an agent to make — walked into the same wall `design_inventory` hit on a
/// real front (a 145 000-character reply). Bounding it here makes the NAIVE call safe BY
/// CONSTRUCTION: the budget is spent on the head first, then shared between the four sections, each
/// row [`bounded_row`]-capped, and what did not fit is NAMED (`total` vs `returned`) with the offset
/// to ask for next. The agent never has to remember to paginate; it only has to READ `truncated`.
pub fn proto_get(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let section = optional_str(args, "section")?;
    if let Some(name) = section {
        if !PROTO_GET_SECTIONS.contains(&name) {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "unknown section '{name}' (accepted: {})",
                    PROTO_GET_SECTIONS.join(", ")
                ),
            ));
        }
    }
    let offset = optional_usize(args, "offset")?.unwrap_or(0);
    let limit = optional_usize(args, "limit")?
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);

    let state = db
        .with_conn(|c| {
            let Some(prototype) = db::get_prototype_for_spec(c, spec_id)? else {
                return Ok::<Option<ProtoState>, diesel::result::Error>(None);
            };
            let rounds = db::list_rounds(c, &prototype.id)?;
            let current_round = db::current_round(c, &prototype.id)?;
            let manifest = match &current_round {
                Some(r) => db::list_round_pages(c, &r.id)?,
                None => vec![],
            };
            let pages = db::list_pages(c, &prototype.id)?;
            // THE ITERATIONS, page by page then version by version — the ids every proto action
            // takes, which no read exposed (finding `01KZ3JS646G83NF4NRKY8VX5CP`). Archived
            // versions are INCLUDED: a rebase rolls onto them, so a state read that hid them would
            // hide half the rollback surface. Each row already names its parent, so the three lists
            // join on `page_id` / `page_version_id` with no extra key.
            let mut versions: Vec<db::PrototypePageVersion> = Vec::new();
            let mut variants: Vec<db::PrototypeVariant> = Vec::new();
            for page in &pages {
                for version in db::list_page_versions(c, &page.id, true)? {
                    variants.extend(db::list_variants(c, &version.id)?);
                    versions.push(version);
                }
            }
            let open_feedback: Vec<db::PrototypeFeedback> = db::list_feedback(c, &prototype.id)?
                .into_iter()
                .filter(|f| f.status == db::FEEDBACK_STATUS_OPEN)
                .collect();
            // THE FREEZE, read here rather than discovered through a refusal.
            let design_lock = db::get_design_lock(c, &prototype.id)?;
            Ok(Some(ProtoState {
                prototype,
                rounds,
                current_round,
                manifest,
                pages,
                versions,
                variants,
                open_feedback,
                design_lock,
            }))
        })
        .map_err(internal_db)?;

    let Some(state) = state else {
        return Ok(ToolOutcome::read(json!({ "prototype": null })));
    };
    // Every row is capped BEFORE the lists are budgeted: a single 200 KiB comment body can then
    // never eat a section (and, since a capped row is smaller than any section budget, "at least one
    // row fits" is a fact — the paging can never stall on `next_offset == offset`).
    let sections: [Vec<Value>; 6] = [
        state
            .rounds
            .iter()
            .map(|r| bounded_row(&round_row(r, state.design_lock.as_ref())))
            .collect(),
        state
            .manifest
            .iter()
            .map(|m| bounded_row(&json!(m)))
            .collect(),
        state.pages.iter().map(|p| bounded_row(&json!(p))).collect(),
        state
            .versions
            .iter()
            .map(|v| bounded_row(&json!(v)))
            .collect(),
        state
            .variants
            .iter()
            .map(|v| bounded_row(&variant_identity(v)))
            .collect(),
        state
            .open_feedback
            .iter()
            .map(|f| bounded_row(&json!(f)))
            .collect(),
    ];

    // --- SECTION shape: a PAGE of ONE list, with the whole budget to itself. ---
    if let Some(name) = section {
        let index = PROTO_GET_SECTIONS
            .iter()
            .position(|s| *s == name)
            .expect("validated above");
        let items = &sections[index];
        let window: Vec<Value> = items.iter().skip(offset).take(limit).cloned().collect();
        let budget = MAX_RESPONSE_BYTES.saturating_sub(RESPONSE_ENVELOPE_BYTES);
        let (listed, dropped) = under_response_budget_strict(window, budget);
        let next = offset + listed.len();
        let truncated = dropped || next < items.len();
        return Ok(ToolOutcome::read(json!({
            "mode": "section",
            "section": name,
            "prototype_id": state.prototype.id,
            "total": items.len(),
            "offset": offset,
            "returned": listed.len(),
            "truncated": truncated,
            // `offset + returned`, never `offset + limit`: the byte budget can cut a page short and
            // paging by `limit` would then SKIP what it dropped. Never equal to `offset` (a capped
            // row always fits the full budget), so following it always progresses.
            "next_offset": if next < items.len() && next > offset { Some(next) } else { None },
            "items": listed,
        })));
    }

    // --- OVERVIEW shape: the head + a bounded slice of each list. ---
    // THE LOCK IS PART OF THE HEAD (finding `01KZ3JS646G83NF4NRKY8VX5CP`): a frozen prototype used
    // to say so nowhere, and the only ways to learn it were to be refused by a mutation or to call
    // another tool. It is a fixed-size fact, so it costs the lists nothing.
    let head = json!({
        "prototype": bounded_row(&json!(state.prototype)),
        "current_round": state.current_round.as_ref()
            .map(|r| bounded_row(&round_row(r, state.design_lock.as_ref()))),
        "locked": state.design_lock.is_some(),
        "design_lock": design_lock_payload(&state),
    });
    // The lists share what is LEFT after the fixed head and the envelope, so the WHOLE serialized
    // response — head, counters, separators — stays under the cap.
    let mut remaining =
        MAX_RESPONSE_BYTES.saturating_sub(json_len(&head) + RESPONSE_ENVELOPE_BYTES);
    let mut listed: Vec<Value> = Vec::with_capacity(PROTO_GET_SECTIONS.len());
    let mut meta = serde_json::Map::new();
    let mut any_truncated = false;
    for (i, items) in sections.iter().enumerate() {
        // An EQUAL share of what is still unspent, so an early section cannot starve a later one and
        // a frugal one hands its slack forward.
        let share = remaining / (PROTO_GET_SECTIONS.len() - i);
        let (kept, dropped) = under_response_budget_strict(items.clone(), share);
        remaining -= kept.iter().map(json_len).sum::<usize>();
        let truncated = dropped || kept.len() < items.len();
        any_truncated |= truncated;
        meta.insert(
            PROTO_GET_SECTIONS[i].to_string(),
            json!({
                "total": items.len(),
                "returned": kept.len(),
                "truncated": truncated,
                // Where to resume with `section: "<name>", offset: <this>` — `null` when the
                // section is complete.
                "next_offset": if kept.len() < items.len() { Some(kept.len()) } else { None },
            }),
        );
        listed.push(Value::Array(kept));
    }
    let mut result = head;
    let object = result.as_object_mut().expect("head is an object");
    for (i, items) in listed.into_iter().enumerate() {
        object.insert(PROTO_GET_SECTIONS[i].to_string(), items);
    }
    object.insert("truncated".into(), json!(any_truncated));
    object.insert("sections".into(), Value::Object(meta));
    object.insert("warnings".into(), json!(state_warnings(&state)));
    Ok(ToolOutcome::read(result))
}

/// The prototype state [`proto_get`] reads in ONE transaction, before any of it is bounded. Kept as
/// a struct so the DB read stays a single pass and the BOUNDING is pure (and testable) on top of it.
struct ProtoState {
    prototype: db::Prototype,
    rounds: Vec<db::PrototypeRound>,
    current_round: Option<db::PrototypeRound>,
    manifest: Vec<db::PrototypeRoundPage>,
    pages: Vec<db::PrototypePage>,
    /// Every version of every page, ARCHIVED ONES INCLUDED — a rebase rolls onto them.
    versions: Vec<db::PrototypePageVersion>,
    /// Every variant of every version, in page/version order.
    variants: Vec<db::PrototypeVariant>,
    open_feedback: Vec<db::PrototypeFeedback>,
    /// The FREEZE marker, `None` while the design is still moving.
    design_lock: Option<db::PrototypeDesignLock>,
}

/// A ROUND row for [`proto_get`], with the one fact the row itself cannot carry: is THIS round the
/// one a design lock froze? A frozen round refuses every mutation, so an agent reading the list must
/// be able to tell which one it is without provoking a refusal.
fn round_row(round: &db::PrototypeRound, lock: Option<&db::PrototypeDesignLock>) -> Value {
    let mut value = json!(round);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "locked".into(),
            json!(lock.is_some_and(|l| l.locked_round_id == round.id)),
        );
    }
    value
}

/// A VARIANT reduced to its IDENTITY for the state read (finding `01KZ3JS646G83NF4NRKY8VX5CP`):
/// the ids and labels an action takes, never the `composition_json`.
///
/// The tree is the one field of this row that GROWS without bound, and it is not what this read is
/// for: an agent that wants a variant's tree asks `design_preview(variant_id)` (the rendering) or
/// `proto_composition` (the frozen contract). Carrying it here would spend a whole section on ONE
/// variant and hand back something nobody asked for — so the row says how big it is and stops.
fn variant_identity(variant: &db::PrototypeVariant) -> Value {
    json!({
        "id": variant.id,
        "page_version_id": variant.page_version_id,
        "label": variant.label,
        "title": variant.title,
        // The AGENT's recommendation (`is_main`) — NOT the RETENU, which lives on the round manifest.
        "is_main": variant.is_main,
        "position": variant.position,
        "created_at": variant.created_at,
        // The TREE is named by its size, not carried: `design_preview(variant_id)` renders it.
        "composition_bytes": variant.composition_json.len(),
        "has_composition": !variant.composition_json.trim().is_empty(),
        // The variant's OWN axes — `design_preview(variant_id, axes)` pilots them, so an agent must
        // be able to see that they exist without fetching the tree.
        "states_json": variant.states_json,
    })
}

/// The `design_lock` block of the overview: the freeze, said in full — WHICH round, WHEN, BY WHOM,
/// and whether that round was ever PRESENTED (a lock on a never-presented round is a real state, and
/// one nothing used to signal). `null` when the design is not frozen.
fn design_lock_payload(state: &ProtoState) -> Value {
    let Some(lock) = &state.design_lock else {
        return Value::Null;
    };
    let round = state.rounds.iter().find(|r| r.id == lock.locked_round_id);
    json!({
        "locked_round_id": lock.locked_round_id,
        "locked_round_label": round.map(|r| r.label.clone()),
        "locked_at": lock.locked_at,
        "decided_by": lock.decided_by,
        "presented_at": round.and_then(|r| r.presented_at),
        "note": "this round's election is FROZEN: proto_elect_variant / proto_add_variant / \
                 proto_pin_page on it are refused. To iterate, open a NEW round (proto_rebase or \
                 proto_open_round) — the frozen one is what proto_composition hands the coder.",
    })
}

/// What this state read has to SAY beyond its rows — the states that are legal, readable in the
/// payload, and still worth naming out loud (finding `01KZ3JS646G83NF4NRKY8VX5CP`).
fn state_warnings(state: &ProtoState) -> Vec<String> {
    let mut out = Vec::new();
    let Some(lock) = &state.design_lock else {
        return out;
    };
    let round = state.rounds.iter().find(|r| r.id == lock.locked_round_id);
    match round {
        None => out.push(format!(
            "the design lock points at round {} — which this prototype does not have. The seam \
             (proto_composition) will expose nothing: re-distill a round this prototype really \
             carries (proto_distill(prototype_id, round_id))",
            lock.locked_round_id
        )),
        Some(round) if round.presented_at.is_none() => out.push(format!(
            "round '{}' ({}) is FROZEN as the design but was never PRESENTED to the human — the \
             coder is about to consume a design nobody was shown. Open a new round (proto_rebase), \
             present it, have the user elect, and distill THAT one",
            round.label, round.id
        )),
        Some(_) => {}
    }
    out
}

/// `proto_present_round` — `{ round_id }` → `{ round }`. PRESENTS (freezes) the round. Unknown
/// round → `invalid_id`. Emits `proto.round_presented`. Mutating → `Prototypes` changed.
pub fn proto_present_round(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let round_id = require_str(args, "round_id")?;
    let round = db
        .with_conn(|c| db::present_round(c, round_id))
        .map_err(internal_db)?
        .ok_or_else(|| unknown("round", round_id))?;
    Ok(ToolOutcome::changed(
        json!({ "round": round }),
        ChangedTopic::Prototypes,
    ))
}

/// `proto_elect_variant` — `{ round_id, page_id, variant_id, rationale?, decided_by? }` →
/// `{ round_page }`. Records the USER's RETENU variant for the page in the round. EXPLICIT
/// preconditions: the page must be PINNED in the round (`invalid_state` otherwise), the variant must
/// exist (`invalid_id` otherwise) and it must BELONG to the (page, version) that round pinned
/// (`invalid_state` otherwise). Emits `proto.variant_elected`. Mutating → `Prototypes`.
///
/// # Why the belonging check exists (finding `01KZ3JTV1AY53QDTGB8V8P8YZE`, A)
///
/// The validation used to stop at "the page is pinned" and "the variant exists" — two facts that say
/// NOTHING about each other. So electing another PAGE's variant was accepted, and the round manifest
/// then said "page Checkout, version v2" while `elected_variant_id` pointed at a tree composed for
/// Pricing. That id is what `elected_composition` reads, so the corruption crossed the seam whole:
/// the coder received one page's placement under another page's name, with every tool answering `ok`.
/// The same reasoning covers the VERSION: a round pins a (page, version) pair, so a variant of
/// another version of the right page would make the manifest's own version field a lie.
pub fn proto_elect_variant(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let round_id = require_str(args, "round_id")?;
    let page_id = require_str(args, "page_id")?;
    let variant_id = require_str(args, "variant_id")?;
    let rationale = optional_str(args, "rationale")?;
    let decided_by = optional_str(args, "decided_by")?;

    type ElectResult = Result<(db::PrototypeRoundPage, String), RpcError>;
    let (round_page, prototype_id) = db
        .with_conn(|c| -> Result<ElectResult, diesel::result::Error> {
            // The page must be pinned in this round (an explicit state, not a silent skip).
            let Some(pinned) = db::get_round_page(c, round_id, page_id)? else {
                return Ok(Err(RpcError::new(
                    RpcCode::InvalidState,
                    format!("page {page_id} is not pinned in round {round_id}"),
                )));
            };
            // The elected variant must exist.
            let Some(variant) = db::get_variant(c, variant_id)? else {
                return Ok(Err(unknown("variant", variant_id)));
            };
            // …AND IT MUST BE ONE OF THIS (page, version)'S OWN. See the doc comment: the RETENU is
            // what crosses the seam under this page's name, so a variant from elsewhere is a
            // corruption of the coder contract, not a preference.
            if variant.page_version_id != pinned.page_version_id {
                let electable = db::list_variants(c, &pinned.page_version_id)?;
                let owner_page = match db::get_page_version(c, &variant.page_version_id)? {
                    Some(version) => db::get_page(c, &version.page_id)?,
                    None => None,
                };
                let belongs_to = match &owner_page {
                    Some(page) if page.id == page_id => format!(
                        "it belongs to ANOTHER VERSION of this page (version {})",
                        variant.page_version_id
                    ),
                    Some(page) => format!(
                        "it belongs to ANOTHER PAGE — '{}' ({}, id {})",
                        page.title, page.slug, page.id
                    ),
                    None => "its page version is unreachable".to_string(),
                };
                return Ok(Err(RpcError::new(
                    RpcCode::InvalidState,
                    format!(
                        "variant '{}' ({variant_id}) cannot be the RETENU of page {page_id} in \
                         round {round_id}: {belongs_to}, while that round pinned version {}. The \
                         RETENU is the tree the coder receives UNDER THIS PAGE'S NAME, so it must \
                         be one of the pinned version's own. Elect one of {}, or re-pin this page \
                         on the version you meant (proto_pin_page(round_id, page_id, \
                         page_version_id))",
                        variant.label,
                        pinned.page_version_id,
                        if electable.is_empty() {
                            "— that version carries NO variant yet (compose one with \
                             proto_add_variant)"
                                .to_string()
                        } else {
                            electable
                                .iter()
                                .map(|v| format!("'{}' (id {})", v.label, v.id))
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                    ),
                )));
            }
            // GUARDED: electing on a design-LOCKED round is refused (the "élection figée"
            // invariant) so the frozen elected composition can never drift.
            let updated = match db::elect_round_page_variant(
                c, round_id, page_id, variant_id, rationale, decided_by,
            ) {
                Ok(rp) => rp,
                Err(db::RoundMutationError::RoundLocked { locked_round_id }) => {
                    return Ok(Err(round_locked(&locked_round_id)));
                }
                Err(db::RoundMutationError::Db(e)) => return Err(e),
            };
            let Some(rp) = updated else {
                return Ok(Err(RpcError::new(
                    RpcCode::InvalidState,
                    format!("page {page_id} is not pinned in round {round_id}"),
                )));
            };
            // Resolve the prototype the elected round belongs to, so the result carries it (the
            // auto-map's signal — see `worked_prototype_id`).
            let prototype_id = db::get_round(c, round_id)?
                .map(|r| r.prototype_id)
                .unwrap_or_default();
            Ok(Ok((rp, prototype_id)))
        })
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(
        json!({ "round_page": round_page, "prototype_id": prototype_id }),
        ChangedTopic::Prototypes,
    ))
}

/// `proto_recommend_variant` — `{ variant_id }` → `{ variant, prototype_id }`. Pose the AGENT's
/// `is_main` RECOMMENDATION on a variant (review #46). The manual has always described `is_main` as
/// "the agent's reco, distinct from the user's RETENU" — but no tool posed it, so the agent could not
/// express the very recommendation it was told to make (`db::set_main_variant` only ever fired in
/// tests).
///
/// STRICTLY DISTINCT FROM THE RETENU (PDR-0009, the two-tier election):
/// * this writes `prototype_variants.is_main` and NOTHING else — the RETENU lives on the ROUND's
///   manifest (`prototype_round_pages.chosen_variant_id`) and is written ONLY by
///   [`proto_elect_variant`], recording the USER's decision. Recommending therefore never elects;
/// * it is ONE-PER-VERSION and REVERSIBLE: `set_main_variant` clears the flag on every sibling of the
///   page version before setting it, so re-recommending simply moves the agent's reco.
///
/// A DEDICATED tool rather than a `proto_add_variant` parameter, because a reco is a decision the
/// agent revises AFTER seeing its own variants side by side (and can move to a sibling); a
/// creation-time flag could neither be moved nor re-posed on an existing variant.
///
/// GUARDED by the Phase-4 lock exactly like the other mutations: recommending inside a version pinned
/// in a design-LOCKED round is refused (`invalid_state` → open a new round). Unknown variant →
/// `invalid_id`. Emits nothing new; mutating → `Prototypes` changed.
pub fn proto_recommend_variant(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let variant_id = require_str(args, "variant_id")?;
    let result = db
        .with_conn(
            |c| -> Result<Result<Value, RpcError>, diesel::result::Error> {
                if db::get_variant(c, variant_id)?.is_none() {
                    return Ok(Err(unknown("variant", variant_id)));
                }
                // The Phase-4 "élection figée" lock guard lives in `set_main_variant`; map its refusal.
                let variant = match map_round_mutation(db::set_main_variant(c, variant_id))? {
                    Ok(Some(v)) => v,
                    // Raced with a delete between the existence check and the write.
                    Ok(None) => return Ok(Err(unknown("variant", variant_id))),
                    Err(e) => return Ok(Err(e)),
                };
                // Resolve the owning prototype (version → page → prototype) for the auto-map signal.
                let prototype_id = match db::get_page_version(c, &variant.page_version_id)? {
                    Some(version) => db::get_page(c, &version.page_id)?.map(|p| p.prototype_id),
                    None => None,
                };
                Ok(Ok(
                    json!({ "variant": variant, "prototype_id": prototype_id }),
                ))
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(result, ChangedTopic::Prototypes))
}

/// `proto_feedback_create` — `{ prototype_id, body, node_key?, origin_variant_id?, author_kind?,
/// author_ref? }` → `{ feedback }`. Raises a PRE-DEV review comment anchored to a tree node
/// (`node_key`). Unknown prototype → `invalid_id`. Emits `proto.feedback_created`. Mutating →
/// `Prototypes`. NOTE: this comment is a REVIEW artifact — never transmitted to the coder.
pub fn proto_feedback_create(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let prototype_id = require_str(args, "prototype_id")?;
    let body = require_str(args, "body")?;
    let node_key = optional_str(args, "node_key")?;
    let origin_variant_id = optional_str(args, "origin_variant_id")?;
    let author_kind = parse_author_kind(args)?;
    let author_ref = optional_str(args, "author_ref")?;

    let input = db::FeedbackInput {
        origin_variant_id: origin_variant_id.map(str::to_string),
        node_key: node_key.map(str::to_string),
        body: body.to_string(),
        author_kind: author_kind.to_string(),
        author_ref: author_ref.map(str::to_string),
    };
    let feedback = db
        .with_conn(
            |c| -> Result<Result<db::PrototypeFeedback, RpcError>, diesel::result::Error> {
                if db::get_prototype(c, prototype_id)?.is_none() {
                    return Ok(Err(unknown("prototype", prototype_id)));
                }
                Ok(Ok(db::create_feedback(c, prototype_id, &input)?))
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(
        json!({ "feedback": feedback }),
        ChangedTopic::Prototypes,
    ))
}

/// `proto_feedback_resolve` — `{ feedback_id, resolution_note?, resolution_via_variant_id? }` →
/// `{ feedback }`. Resolves a pre-dev review comment (the agent addressed it). Unknown feedback →
/// `invalid_id`. Emits `proto.feedback_resolved`. Mutating → `Prototypes`.
pub fn proto_feedback_resolve(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let feedback_id = require_str(args, "feedback_id")?;
    let resolution_note = optional_str(args, "resolution_note")?;
    let resolution_via_variant_id = optional_str(args, "resolution_via_variant_id")?;

    let feedback = db
        .with_conn(|c| {
            db::resolve_feedback(c, feedback_id, resolution_note, resolution_via_variant_id)
        })
        .map_err(internal_db)?
        .ok_or_else(|| unknown("feedback", feedback_id))?;
    Ok(ToolOutcome::changed(
        json!({ "feedback": feedback }),
        ChangedTopic::Prototypes,
    ))
}

/// `proto_rebase` — `{ prototype_id, source_round_id, label, summary? }` → `{ round }`. ROLLBACK:
/// open a NEW current round whose manifest COPIES `source_round_id`'s pinned versions, so the
/// design returns to that snapshot and iteration continues forward WITHOUT mutating the frozen
/// source (PDR-0009: rounds gardés comme rollback). Unknown / cross-prototype source →
/// `invalid_id`. Mutating → `Prototypes` changed.
pub fn proto_rebase(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let prototype_id = require_str(args, "prototype_id")?;
    let source_round_id = require_str(args, "source_round_id")?;
    let label = require_str(args, "label")?;
    let summary = optional_str(args, "summary")?;
    let round = db
        .with_conn(|c| db::rebase_round(c, prototype_id, source_round_id, label, summary))
        .map_err(internal_db)?
        .ok_or_else(|| unknown("round", source_round_id))?;
    Ok(ToolOutcome::changed(
        json!({ "round": round }),
        ChangedTopic::Prototypes,
    ))
}

/// `proto_distill` — `{ prototype_id, round_id, decided_by?, supersede? }` →
/// `{ lock, superseded_round_id, warnings }`. FREEZE the elected round as THE design for the Spec
/// (the reframed "distill" = FIGER l'élection, PDR-0010 — no separate placement blob; the
/// composition tree IS the contract). Validates the round belongs to the prototype (`invalid_id` /
/// `invalid_state` otherwise). Mutating → `Prototypes` changed.
///
/// # What it refuses to freeze, and why (finding `01KZ3JTV1AY53QDTGB8V8P8YZE`, B)
///
/// It used to freeze ANYTHING: a round with no pinned page, no election and no presentation locked
/// as cleanly as a settled one, and the seam then handed the coder an empty design as if that were
/// the design. And it MOVED an existing lock in one call, silently — the manual promises "the design
/// the coder will consume can never drift under it", and it drifted in one call. So:
///
/// * A round the human was never SHOWN (`presented_at` null) is refused: distilling records what the
///   USER decided, and there is no decision on a round nobody presented.
/// * A round with NO pinned page is refused: there is nothing to freeze.
/// * A pinned page with NO effective RETENU is refused, by name: that page crosses the seam EMPTY,
///   and half a design is a false contract exactly as half a placement is (the `oversized` rule,
///   applied one level up). Elect it, or unpin it.
/// * MOVING a posed lock is refused unless the caller says `supersede: true`. The forward path stays
///   open — a second iteration legitimately ends on a new round — but it is now a DELIBERATE gesture
///   naming what it replaces, instead of a lock that walks off on its own.
///
/// Re-distilling the SAME round is idempotent and needs no `supersede`: it re-records `decided_by`
/// and moves nothing.
pub fn proto_distill(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let prototype_id = require_str(args, "prototype_id")?;
    let round_id = require_str(args, "round_id")?;
    let decided_by = optional_str(args, "decided_by")?;
    let supersede = optional_bool(args, "supersede")?.unwrap_or(false);
    let result = db
        .with_conn(
            |c| -> Result<Result<Value, RpcError>, diesel::result::Error> {
                let Some(round) = db::get_round(c, round_id)? else {
                    return Ok(Err(unknown("round", round_id)));
                };
                if round.prototype_id != prototype_id {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidState,
                        format!("round {round_id} does not belong to prototype {prototype_id}"),
                    )));
                }
                // THE LOCK DOES NOT WALK OFF ON ITS OWN.
                let previous = db::get_design_lock(c, prototype_id)?;
                let superseded = match &previous {
                    Some(lock) if lock.locked_round_id == round_id => None,
                    Some(lock) if !supersede => {
                        let frozen = db::get_round(c, &lock.locked_round_id)?;
                        return Ok(Err(RpcError::new(
                            RpcCode::InvalidState,
                            format!(
                                "prototype {prototype_id}'s design is ALREADY frozen on round {} \
                                 ({}) — freezing {round_id} would MOVE the lock, and what the coder \
                                 consumes would change under it. If that is what you mean (a new \
                                 iteration was presented, elected and is now THE design), say so: \
                                 pass supersede: true. Otherwise iterate without touching the \
                                 freeze (proto_rebase opens a new round from the frozen one)",
                                lock.locked_round_id,
                                frozen
                                    .as_ref()
                                    .map(|r| r.label.as_str())
                                    .unwrap_or("label unknown"),
                            ),
                        )));
                    }
                    Some(lock) => Some(lock.locked_round_id.clone()),
                    None => None,
                };
                // A ROUND NOBODY WAS SHOWN IS NOT A DECISION.
                if round.presented_at.is_none() {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidState,
                        format!(
                            "round '{}' ({round_id}) was never PRESENTED, so no user ever decided \
                             on it — distilling records the USER's verdict, it does not create one. \
                             Present it (proto_present_round(round_id)), have the user elect a \
                             RETENU per page (proto_elect_variant), then distill",
                            round.label
                        ),
                    )));
                }
                // …AND A FREEZE WITHOUT AN ELECTION FREEZES NOTHING.
                let manifest = db::list_round_pages(c, round_id)?;
                if manifest.is_empty() {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidState,
                        format!(
                            "round '{}' ({round_id}) pins NO page, so there is nothing to freeze — \
                             the coder would receive an empty design. Pin the round's pages \
                             (proto_pin_page(round_id, page_id, page_version_id)) before distilling",
                            round.label
                        ),
                    )));
                }
                let mut unelected: Vec<String> = Vec::new();
                for entry in &manifest {
                    if db::effective_retained_variant(c, round_id, &entry.page_id)?.is_none() {
                        let page = db::get_page(c, &entry.page_id)?;
                        unelected.push(match page {
                            Some(p) => format!("'{}' ({}, id {})", p.title, p.slug, p.id),
                            None => entry.page_id.clone(),
                        });
                    }
                }
                if !unelected.is_empty() {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidState,
                        format!(
                            "round '{}' ({round_id}) still owes an arbitration on {} of its {} \
                             pinned page(s): {} — each of those crosses the seam with NO tree, and \
                             the coder would read an empty page as the design. Record the USER's \
                             RETENU on each (proto_elect_variant(round_id, page_id, variant_id)), \
                             or unpin the pages this round does not ship",
                            round.label,
                            unelected.len(),
                            manifest.len(),
                            unelected.join(", ")
                        ),
                    )));
                }
                let lock = db::lock_design(c, prototype_id, round_id, decided_by)?;
                let mut warnings: Vec<String> = Vec::new();
                if let Some(previous_round) = &superseded {
                    warnings.push(format!(
                        "the design lock MOVED from round {previous_round} to {round_id} — what \
                         proto_composition hands the coder changed with it. Anything derived from \
                         the previous freeze (dev tasks, a harness read) is now describing a design \
                         nobody ships"
                    ));
                }
                Ok(Ok(json!({
                    "lock": lock,
                    // The round this freeze REPLACED, `null` for a first freeze or a re-freeze of
                    // the same round — never a silent move.
                    "superseded_round_id": superseded,
                    "warnings": warnings,
                })))
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(result, ChangedTopic::Prototypes))
}

/// `proto_composition` — `{ prototype_id }` → `{ composition }`. READ the FROZEN elected composition
/// — the coder SEAM (v2.7 Phase 4, task #8). The composition is exposed ONLY when the design is
/// LOCKED (`proto_distill` ran): an unlocked prototype returns `{ composition: null }` (the harness's
/// cue the election is not frozen yet), so the seam NEVER surfaces a moving election. The payload is
/// the arbre nommé = placement (3 natures `[DS]`/`[NEW]`/`[inline]`) + the pre-dev review record +
/// the design-system as a REFERENCE (ids, not injected content). Unknown prototype → `invalid_id`.
/// Read-only — no effects, and it never auto-maps (a READ is not "working the proto").
///
/// # The review crosses as ANCHORS, never as PROSE (review #47)
///
/// PDR-0009 freezes "le feedback pré-dev n'est JAMAIS transmis au coder", and this tool is exactly
/// what a coder harness calls. The seam therefore carries `resolved_feedback` as
/// [`db::ResolvedFeedbackAnchor`]s — WHICH tree nodes were corrected before the election, and when —
/// plus the resolved/open COUNTS, and no `body`, resolution note or author. The guarantee is a
/// property of the payload, not a rule the reader must respect; the bodies stay on the LIVE
/// authoring surface (`proto_tree` / the proto UI), which no coder reads.
///
/// # Bounded PAGE BY PAGE — an elected tree is never cut (review 01KYMF0KFZQG5JYZDGZCR85BAM)
///
/// The payload grows with the prototype: one whole `composition_json` per page, its classified nodes,
/// and every resolved anchor. So the tool pages:
///
/// * `{ prototype_id, offset?, limit?, feedback_offset? }` → the composition with a WINDOW of pages;
/// * `{ prototype_id, page }` → ONE page by `slug` or `page_id` (the targeted read, `design_inventory`'s
///   "read ONE bundle" applied to the seam).
///
/// The unit of pagination is the PAGE, never the tree, because the elected `composition_json` IS the
/// coder contract: a silently truncated placement would be WORSE than an overflow — the harness would
/// build a mutilated design believing it complete. A page is therefore emitted WHOLE or not at all,
/// and a page whose tree exceeds the budget ON ITS OWN is returned in an explicitly REDUCED form
/// (`oversized: true`, its real `composition_bytes`, no partial tree and no partial node list) that
/// SAYS the tree could not cross the seam, rather than pretending a fragment is the design.
///
/// # A page with NO elected variant is NAMED (finding `01KZ3JTV1AY53QDTGB8V8P8YZE`, C)
///
/// A pinned page whose election was never recorded reaches the seam with `elected_variant_id: null`,
/// `composition_json: null` and no node — an EMPTY page. The payload used to carry no counter and no
/// `warnings` key at all, so a harness read those pages as "this page has no design" instead of "this
/// page's design was never chosen". That is the exact failure the `oversized` treatment already
/// closes one level down, and it gets the same answer here: `unelected_pages` counts them,
/// `warnings[]` names them with the correction. [`proto_distill`] now REFUSES to freeze such a round,
/// so this signal covers what a refusal cannot reach — a lock posed before that guard existed, or a
/// page whose only variant disappeared after the freeze.
pub fn proto_composition(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let prototype_id = require_str(args, "prototype_id")?;
    let page_ref = optional_str(args, "page")?;
    let offset = optional_usize(args, "offset")?.unwrap_or(0);
    let limit = optional_usize(args, "limit")?
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let feedback_offset = optional_usize(args, "feedback_offset")?.unwrap_or(0);

    let composition = db
        .with_conn(
            |c| -> Result<Result<Option<db::ElectedComposition>, RpcError>, diesel::result::Error> {
                if db::get_prototype(c, prototype_id)?.is_none() {
                    return Ok(Err(unknown("prototype", prototype_id)));
                }
                Ok(Ok(db::elected_composition(c, prototype_id)?))
            },
        )
        .map_err(internal_db)??;

    // Not locked yet (or nothing to expose): the seam stays silent, exactly as before.
    let Some(composition) = composition else {
        return Ok(ToolOutcome::read(json!({ "composition": null })));
    };

    // The targeted read resolves its page FIRST, so an unknown one is an `invalid_id` naming what is
    // available, never an empty page list the caller has to interpret.
    let selected: Option<usize> = match page_ref {
        Some(reference) => Some(
            composition
                .pages
                .iter()
                .position(|p| p.slug == reference || p.page_id == reference)
                .ok_or_else(|| {
                    RpcError::new(
                        RpcCode::InvalidId,
                        format!(
                            "page '{reference}' is not pinned in the frozen round of prototype \
                             {prototype_id} (its elected pages are: {})",
                            composition
                                .pages
                                .iter()
                                .map(|p| p.slug.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                })?,
        ),
        None => None,
    };

    // The fixed HEAD: the freeze marker, the design-system reference and the review COUNTS — small,
    // and the part that must always be there for the payload to mean anything.
    let head = json!({
        "prototype_id": composition.prototype_id,
        "spec_id": composition.spec_id,
        "locked": composition.locked,
        "design_ref": composition.design_ref,
        "resolved_feedback_count": composition.resolved_feedback_count,
        "open_feedback_count": composition.open_feedback_count,
    });
    let mut remaining =
        MAX_RESPONSE_BYTES.saturating_sub(json_len(&head) + RESPONSE_ENVELOPE_BYTES);

    // The review ANCHORS (no prose — review #47) take a quarter of the budget at most, so the review
    // record can never crowd out the PLACEMENT, which is what the coder actually consumes.
    let anchors: Vec<Value> = composition
        .resolved_feedback
        .iter()
        .skip(feedback_offset)
        .map(|a| json!(a))
        .collect();
    let anchor_total = composition.resolved_feedback.len();
    let (anchors, anchors_dropped) = under_response_budget_strict(anchors, remaining / 4);
    remaining -= anchors.iter().map(json_len).sum::<usize>();
    let anchors_next = feedback_offset + anchors.len();
    let anchors_truncated = anchors_dropped || anchors_next < anchor_total;

    // The PAGES: whole trees only, in manifest order.
    let window: Vec<&db::ElectedPage> = match selected {
        Some(index) => vec![&composition.pages[index]],
        None => composition.pages.iter().skip(offset).take(limit).collect(),
    };
    let mut pages: Vec<Value> = Vec::with_capacity(window.len());
    let mut oversized = 0usize;
    let mut pages_dropped = false;
    for page in window {
        let full = json!(page);
        let size = json_len(&full);
        if size <= remaining {
            remaining -= size;
            pages.push(full);
            continue;
        }
        if pages.is_empty() {
            // This page's tree does not fit ON ITS OWN. Say so — do not mutilate it.
            oversized += 1;
            pages.push(reduced_elected_page(page, size));
        } else {
            pages_dropped = true;
        }
        break;
    }

    let page_total = composition.pages.len();
    // THE PAGES NOBODY ELECTED — counted over the WHOLE frozen round, never over the window, so a
    // paginated read cannot hide them behind an offset (finding `01KZ3JTV1AY53QDTGB8V8P8YZE`, C).
    let unelected: Vec<&db::ElectedPage> = composition
        .pages
        .iter()
        .filter(|p| p.elected_variant_id.is_none())
        .collect();
    let mut warnings: Vec<String> = Vec::new();
    if !unelected.is_empty() {
        warnings.push(format!(
            "{} of this frozen round's {} page(s) carry NO elected variant: {} — they cross this \
             seam EMPTY (`elected_variant_id`, `composition_json` and `nodes` all absent), and an \
             empty page is NOT a design, it is an arbitration nobody made. Do not build them: \
             record the USER's RETENU on each (proto_elect_variant(round_id, page_id, variant_id)) \
             in a NEW round and distill that one, or unpin the pages the round does not ship",
            unelected.len(),
            page_total,
            unelected
                .iter()
                .take(10)
                .map(|p| format!("'{}' ({})", p.title, p.slug))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let (offset, next) = match selected {
        Some(index) => (index, index + 1),
        None => (offset, offset + pages.len()),
    };
    let truncated = anchors_truncated || pages_dropped || oversized > 0 || next < page_total;
    let mut exposed = head;
    if let Some(object) = exposed.as_object_mut() {
        object.insert("pages".into(), Value::Array(pages.clone()));
        object.insert("resolved_feedback".into(), Value::Array(anchors.clone()));
    }
    Ok(ToolOutcome::read(json!({
        "mode": if selected.is_some() { "page" } else { "composition" },
        "composition": exposed,
        "page_total": page_total,
        "offset": offset,
        "returned": pages.len(),
        "oversized_pages": oversized,
        // How many of the FROZEN ROUND's pages (not of this window) owe an arbitration — the
        // counter beside `oversized_pages`, and for the same reason: what the seam could not
        // deliver is a number the harness reads, never something it has to infer from a null.
        "unelected_pages": unelected.len(),
        "truncated": truncated,
        // `offset + returned`, never `offset + limit`. A targeted read never advertises a next page
        // (it answered the question that was asked).
        "next_offset": if selected.is_none() && next < page_total && next > offset {
            Some(next)
        } else {
            None
        },
        "resolved_feedback_total": anchor_total,
        "resolved_feedback_offset": feedback_offset,
        "resolved_feedback_returned": anchors.len(),
        "resolved_feedback_truncated": anchors_truncated,
        "resolved_feedback_next_offset": if anchors_next < anchor_total && anchors_next > feedback_offset {
            Some(anchors_next)
        } else {
            None
        },
        // What this seam could NOT deliver, in words — ALWAYS present (empty = the whole frozen
        // design crossed). A key that only appears when something is wrong is a key a reader learns
        // to stop looking for.
        "warnings": warnings,
    })))
}

/// The REDUCED form of an elected page whose tree exceeds the response budget on its own: its
/// IDENTITY and its counts, with the tree and the classified nodes ABSENT rather than cut.
///
/// This is the one place the seam refuses to answer, and it refuses LOUDLY. `composition_json` is
/// the coder contract; a prefix of it is not a smaller contract but a FALSE one (a placement missing
/// its last regions, silently). So the page says what it is (`oversized`), how big the tree really is
/// (`composition_bytes`), how many nodes it holds, and what to do about it — and the harness that
/// reads it cannot mistake the absence for an empty design.
fn reduced_elected_page(page: &db::ElectedPage, bytes: usize) -> Value {
    json!({
        "page_id": page.page_id,
        "slug": page.slug,
        "title": page.title,
        "page_version_id": page.page_version_id,
        "elected_variant_id": page.elected_variant_id,
        // ABSENT, not truncated — see the doc comment.
        "composition_json": Value::Null,
        "nodes": Vec::<Value>::new(),
        "node_count": page.nodes.len(),
        "new_components": page.new_components,
        // The CSS EXCEPTIONS cross even here. They are bounded and deduplicated per page, so they
        // are never what made the page oversized — and an exception that did not cross would be an
        // exception the coder never learns about, which is the defect this list exists to close.
        "css_exceptions": page.css_exceptions,
        "oversized": true,
        "composition_bytes": bytes,
        "note": format!(
            "this page's elected tree is {bytes} bytes on its own, past the {MAX_RESPONSE_BYTES}-byte \
             response cap: it is NOT returned rather than returned MUTILATED — the composition_json \
             IS the coder contract, and half a placement is a false one. Split the page, or extract \
             its sub-trees into [NEW] components, so the elected tree can cross the seam whole."
        ),
    })
}

// --- CREATION / amorçage handlers (review #34) -----------------------------

/// `proto_create` — `{ spec_id, description? }` → `{ prototype, current_round, prototype_id }`.
/// CREATE the prototype on a Spec (1/Spec, PDR-0009), seeding its always-present `v1` round. The
/// Spec must exist (`invalid_id`) and must NOT already have a prototype (`invalid_state` — the
/// "1 proto/Spec" invariant). Mutating → `Prototypes` changed (`prototypes://changed`).
pub fn proto_create(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let description = optional_str(args, "description")?;
    let result = db
        .with_conn(
            |c| -> Result<Result<Value, RpcError>, diesel::result::Error> {
                if db::get_spec(c, spec_id)?.is_none() {
                    return Ok(Err(unknown("spec", spec_id)));
                }
                if let Some(existing) = db::get_prototype_for_spec(c, spec_id)? {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidState,
                        format!(
                            "spec {spec_id} already has a prototype (1 proto/Spec, PDR-0009): its \
                             prototype_id is {} — drive THAT one (proto_get(spec_id) reads its \
                             whole state)",
                            existing.id
                        ),
                    )));
                }
                let prototype = match created_or_conflict(
                    db::create_prototype(c, spec_id, description),
                    || format!("spec {spec_id} already has a prototype (1 proto/Spec, PDR-0009)"),
                )? {
                    Ok(p) => p,
                    Err(e) => return Ok(Err(e)),
                };
                let current_round = db::current_round(c, &prototype.id)?;
                let prototype_id = prototype.id.clone();
                Ok(Ok(json!({
                    "prototype": prototype,
                    "current_round": current_round,
                    "prototype_id": prototype_id,
                })))
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(result, ChangedTopic::Prototypes))
}

/// `proto_add_page` — `{ prototype_id, slug, title, position? }` → `{ page, prototype_id }`. Add a
/// PAGE (logical screen). Unknown prototype → `invalid_id`; a duplicate `slug` in the prototype →
/// `invalid_state`. Mutating → `Prototypes` changed.
pub fn proto_add_page(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let prototype_id = require_str(args, "prototype_id")?;
    let slug = require_str(args, "slug")?;
    let title = require_str(args, "title")?;
    let position = optional_position(args, "position")?;
    let page = db
        .with_conn(
            |c| -> Result<Result<db::PrototypePage, RpcError>, diesel::result::Error> {
                if db::get_prototype(c, prototype_id)?.is_none() {
                    return Ok(Err(unknown("prototype", prototype_id)));
                }
                // A CONFLICT HANDS BACK THE ID IT COLLIDED WITH (finding
                // `01KZ3JS646G83NF4NRKY8VX5CP`): a refusal that names only the label leaves the
                // caller with no way to reach the row it just proved exists.
                let existing = db::list_pages(c, prototype_id)?
                    .into_iter()
                    .find(|p| p.slug == slug)
                    .map(|p| p.id);
                created_or_conflict(
                    db::create_page(c, prototype_id, slug, title, position),
                    || duplicate("page", "slug", slug, "prototype", prototype_id, existing),
                )
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(
        json!({ "page": page, "prototype_id": prototype_id }),
        ChangedTopic::Prototypes,
    ))
}

/// `proto_add_version` — `{ page_id, label, summary? }` → `{ version, prototype_id }`. Add a FROZEN
/// version (iteration) to a page — the agent iterates by minting a new version, never mutating.
/// Unknown page → `invalid_id`; a duplicate `label` on the page → `invalid_state`. Mutating →
/// `Prototypes` changed.
pub fn proto_add_version(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let page_id = require_str(args, "page_id")?;
    let label = require_str(args, "label")?;
    let summary = optional_str(args, "summary")?;
    let result = db
        .with_conn(
            |c| -> Result<Result<Value, RpcError>, diesel::result::Error> {
                let Some(page) = db::get_page(c, page_id)? else {
                    return Ok(Err(unknown("page", page_id)));
                };
                let existing = db::list_page_versions(c, page_id, true)?
                    .into_iter()
                    .find(|v| v.label == label)
                    .map(|v| v.id);
                let version = match created_or_conflict(
                    db::create_page_version(c, page_id, label, summary),
                    || duplicate("version", "label", label, "page", page_id, existing),
                )? {
                    Ok(v) => v,
                    Err(e) => return Ok(Err(e)),
                };
                Ok(Ok(
                    json!({ "version": version, "prototype_id": page.prototype_id }),
                ))
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(result, ChangedTopic::Prototypes))
}

/// The component CONTRACTS a `composition_json` REFERENCES — the catalogue rows its nodes NAME,
/// fetched one by one rather than by listing the whole catalogue (a real project carries hundreds of
/// components; a composition names a handful). A name absent from the catalogue yields no contract:
/// that node is a `[NEW]`, which the composition seam already reports as a build candidate.
fn referenced_contracts(
    conn: &mut diesel::sqlite::SqliteConnection,
    project_id: &str,
    composition_json: &str,
) -> Result<Vec<crate::mcp_tools_design::ComponentContract>, diesel::result::Error> {
    let mut out = Vec::new();
    for name in crate::render::referenced_component_names(composition_json) {
        if let Some(row) = db::get_component_by_name(conn, project_id, &name)? {
            out.push(crate::mcp_tools_design::ComponentContract {
                name: row.name,
                params_json: row.params_json,
                states_json: row.states_json,
            });
        }
    }
    Ok(out)
}

/// REFUSE a composition whose nodes NAVIGATE to a page this prototype does not have (finding
/// `01KYW34347CZSNXCYS35KDG3EY`). `None` when every link resolves — including the usual case of a
/// tree that states no link at all.
///
/// # Why this one REFUSES where the node-contract check only WARNS
///
/// A node's params are checked against a component that may legitimately not exist yet: `[NEW]` is a
/// normal state, the composition is written before anybody synthesizes it, so refusing there would
/// refuse the normal road. A PAGE has no such state. There is no "page to be built later": a page is
/// one `proto_add_page` call, it comes FIRST in the cycle, and its id is what that call hands back.
/// So a `navigate_to` that resolves to nothing at the write is always a mistake — a stale id, a
/// slug written where an id belongs, another prototype's page — and the doctrine everywhere here is
/// that what cannot be admitted is refused BY NAME rather than left to be discovered later. Refusing
/// at the write is also what keeps the render-time marker rare and meaningful: it then means a page
/// that DISAPPEARED, not a link that was never valid.
fn refuse_dangling_navigation(
    conn: &mut diesel::sqlite::SqliteConnection,
    prototype_id: &str,
    composition_json: &str,
) -> Result<Option<RpcError>, diesel::result::Error> {
    let links: Vec<(String, String, bool)> = crate::render::composition_nodes(composition_json)
        .into_iter()
        .filter_map(|node| {
            let anchorable = node.component.is_some() || node.inline;
            node.navigate_to
                .map(|page| (node.node_key, page, anchorable))
        })
        .collect();
    if links.is_empty() {
        return Ok(None);
    }
    // A link on a node with no markup of its own can never be clicked — refused with the SAME
    // wording the render uses, so the author reads one rule and not two.
    if let Some((node_key, _, _)) = links.iter().find(|(_, _, anchorable)| !anchorable) {
        return Ok(Some(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "node '{node_key}' navigates somewhere but is a STRUCTURAL node — {}",
                crate::render::NO_ANCHOR_CORRECTION
            ),
        )));
    }
    let pages = db::list_pages(conn, prototype_id)?;
    let Some((node_key, page_id, _)) = links
        .into_iter()
        .find(|(_, page_id, _)| !pages.iter().any(|p| &p.id == page_id))
    else {
        return Ok(None);
    };
    let available = if pages.is_empty() {
        "this prototype has NO page yet — create one with proto_add_page(prototype_id, slug, \
         title), which returns the id to link to"
            .to_string()
    } else {
        format!(
            "its pages are: {}",
            pages
                .iter()
                .map(|p| format!("{} ({}, id {})", p.title, p.slug, p.id))
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    Ok(Some(RpcError::new(
        RpcCode::InvalidId,
        format!(
            "node '{node_key}' navigates to page '{page_id}', which is not a page of prototype \
             {prototype_id} — {available}. A node links to a page BY ID and never by slug (a slug \
             is renamed, an id is not); the render resolves the id to the slug the shim navigates \
             with, at every render"
        ),
    )))
}

/// `proto_add_variant` — `{ page_version_id, label, title, composition_json?, states_json?,
/// position? }` → `{ variant, prototype_id, warnings }`. Add a VARIANT (one concrete rendering) to a
/// page version. Unknown page version → `invalid_id`; a duplicate `label` on the version →
/// `invalid_state`; a node navigating to a page the prototype does not have → `invalid_id`
/// ([`refuse_dangling_navigation`]). GUARDED by the Phase-4 lock: adding a variant under a version
/// PINNED in a design-LOCKED round is refused (`invalid_state`) so the frozen election can never
/// drift.
/// Mutating → `Prototypes` changed.
///
/// `warnings` is THE NODE CONTRACT, checked at the WRITE (finding `01KYQ0R9414Q5K63TBV9CCMC9J`):
/// every node of the tree is confronted with the params AND axes the component it references
/// declares, and a key declared nowhere / a `required` param not provided is NAMED with its
/// correction ([`crate::mcp_tools_design::composition_contract_warnings`]). It is a WARNING and not
/// a refusal — a composition is legitimately written before its `[NEW]` components are synthesized —
/// and the list is BOUNDED, so a 300-node composition cannot answer with 600 lines.
pub fn proto_add_variant(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let page_version_id = require_str(args, "page_version_id")?;
    let label = require_str(args, "label")?;
    let title = require_str(args, "title")?;
    let composition_json = optional_str(args, "composition_json")?
        .unwrap_or_default()
        .to_string();
    // A variant's `states_json` is read as AXES too (`prototype-tab.tsx` calls the same
    // `parseAxes`), so the tolerated MAP form is canonicalized here as well — one model, one shape
    // stored, whichever writer produced it (finding `01KYSNPWWG9DA0AXP6D22GV5BK`).
    let states_json =
        crate::mcp_tools_design::canonical_states_json(optional_str(args, "states_json")?);
    let position = optional_position(args, "position")?;
    // THE SHAPE OF WHAT IS BEING STORED, checked before anything is (finding
    // `01KZ3JS6WFVGYS6JKB9C4J50HR`): a tree that does not parse, an ARRAY root, an `[inline]` node
    // carrying Handlebars LOGIC, and a `states_json` that declares no axis it could read. All four
    // used to store with `warnings: []` — a clean bill of health on a document nothing can read.
    crate::mcp_tools_design::ensure_composition_accepted(&composition_json)?;
    crate::mcp_tools_design::ensure_states_json_accepted(states_json.as_deref())?;

    let result = db
        .with_conn(|c| -> Result<Result<Value, RpcError>, diesel::result::Error> {
            let Some(version) = db::get_page_version(c, page_version_id)? else {
                return Ok(Err(unknown("page version", page_version_id)));
            };
            // A duplicate label is a legible `invalid_state` — pre-check it here (a variant's UNIQUE
            // violation would otherwise be wrapped in `RoundMutationError::Db` and surface as an
            // opaque internal error, unlike the `created_or_conflict` path the other creators use).
            if let Some(existing) = db::list_variants(c, page_version_id)?
                .into_iter()
                .find(|v| v.label == label)
            {
                return Ok(Err(RpcError::new(
                    RpcCode::InvalidState,
                    duplicate(
                        "variant",
                        "label",
                        label,
                        "version",
                        page_version_id,
                        Some(existing.id),
                    ),
                )));
            }
            // Resolve the owning prototype (page → prototype) for the auto-map signal…
            let prototype_id = db::get_page(c, &version.page_id)?.map(|p| p.prototype_id);
            // …which is also the directory a node's `navigate_to` is confronted with. A parcours
            // pointing at a page that does not exist is REFUSED here, before anything is stored:
            // unlike a `[NEW]` component, a page is never "not created yet on purpose".
            if let Some(pid) = &prototype_id {
                if let Some(refusal) = refuse_dangling_navigation(c, pid, &composition_json)? {
                    return Ok(Err(refusal));
                }
            }
            // …and, through it, the PROJECT whose catalogue this composition's nodes are CONFRONTED
            // with (finding `01KYQ0R9414Q5K63TBV9CCMC9J`): a node's keys used to be checked against
            // NOTHING, so `text` on a component declaring `buttonLabel` rendered empty in silence.
            // This is the AUTHORING half of the signal — the moment the agent still holds what it
            // composed. It WARNS, never refuses: a composition is legitimately written before the
            // components it references are synthesized.
            let project_id = match &prototype_id {
                Some(pid) => match db::get_prototype(c, pid)? {
                    Some(p) => db::get_spec(c, &p.spec_id)?.map(|s| s.project_id),
                    None => None,
                },
                None => None,
            };
            let warnings = match &project_id {
                Some(project_id) => crate::mcp_tools_design::composition_contract_warnings(
                    &composition_json,
                    &referenced_contracts(c, project_id, &composition_json)?,
                ),
                None => Vec::new(),
            };
            let input = db::VariantInput {
                label: label.to_string(),
                title: title.to_string(),
                composition_json,
                states_json,
                position,
            };
            // The Phase-4 "élection figée" lock guard lives in `create_variant`; map its refusal.
            let variant = match map_round_mutation(db::create_variant(c, page_version_id, &input))? {
                Ok(v) => v,
                Err(e) => return Ok(Err(e)),
            };
            Ok(Ok(
                json!({ "variant": variant, "prototype_id": prototype_id, "warnings": warnings }),
            ))
        })
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(result, ChangedTopic::Prototypes))
}

/// `proto_pin_page` — `{ round_id, page_id, page_version_id, position? }` → `{ round_page,
/// prototype_id }`. PIN a (page, version) into a round's manifest (so the round "sees" the page and
/// it becomes electable / renderable). Unknown round / page / version → `invalid_id`. GUARDED by
/// the Phase-4 lock: pinning into a design-LOCKED round is refused (`invalid_state`). Mutating →
/// `Prototypes` changed.
pub fn proto_pin_page(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let round_id = require_str(args, "round_id")?;
    let page_id = require_str(args, "page_id")?;
    let page_version_id = require_str(args, "page_version_id")?;
    let position = optional_position(args, "position")?;
    let result = db
        .with_conn(
            |c| -> Result<Result<Value, RpcError>, diesel::result::Error> {
                let Some(round) = db::get_round(c, round_id)? else {
                    return Ok(Err(unknown("round", round_id)));
                };
                if db::get_page(c, page_id)?.is_none() {
                    return Ok(Err(unknown("page", page_id)));
                }
                if db::get_page_version(c, page_version_id)?.is_none() {
                    return Ok(Err(unknown("page version", page_version_id)));
                }
                let round_page = match map_round_mutation(db::set_round_page(
                    c,
                    round_id,
                    page_id,
                    page_version_id,
                    position,
                ))? {
                    Ok(rp) => rp,
                    Err(e) => return Ok(Err(e)),
                };
                Ok(Ok(
                    json!({ "round_page": round_page, "prototype_id": round.prototype_id }),
                ))
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(result, ChangedTopic::Prototypes))
}

/// `proto_open_round` — `{ prototype_id, label, summary? }` → `{ round, prototype_id }`. OPEN a new
/// EMPTY current round (reuses `db::create_round`) — for starting a fresh iteration whose manifest
/// you compose from scratch (contrast `proto_rebase`, which clones an earlier round's manifest).
/// Unknown prototype → `invalid_id`; a duplicate `label` on the prototype → `invalid_state`.
/// Mutating → `Prototypes` changed.
pub fn proto_open_round(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let prototype_id = require_str(args, "prototype_id")?;
    let label = require_str(args, "label")?;
    let summary = optional_str(args, "summary")?;
    let round = db
        .with_conn(
            |c| -> Result<Result<db::PrototypeRound, RpcError>, diesel::result::Error> {
                if db::get_prototype(c, prototype_id)?.is_none() {
                    return Ok(Err(unknown("prototype", prototype_id)));
                }
                let existing = db::list_rounds(c, prototype_id)?
                    .into_iter()
                    .find(|r| r.label == label)
                    .map(|r| r.id);
                created_or_conflict(db::create_round(c, prototype_id, label, summary), || {
                    duplicate("round", "label", label, "prototype", prototype_id, existing)
                })
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(
        json!({ "round": round, "prototype_id": prototype_id }),
        ChangedTopic::Prototypes,
    ))
}

/// `proto_author_component` — `{ prototype_id, name, template?, params_json?, states_json? }` →
/// `{ component, prototype_id }`. AUTHOR a `[NEW]` GREENFIELD catalogue component (PDR-0010): when a
/// prototype proposes a component ABSENT from the project's catalogue, create the `design_components`
/// row (no `source_*` — born before any front) so it APPEARS in the catalogue for the coder to build.
/// Unknown prototype → `invalid_id`; a name ALREADY in the catalogue → `invalid_state` (it is not a
/// `[NEW]` then — REFRESH it with `design_refresh_component`, the explicit update path of review
/// #45, rather than shadowing it here). The stored template must be LOGIC-LESS, the same guard the
/// refresh path applies ([`crate::mcp_tools_design::ensure_template_accepted`]) — both store an LLM's
/// output into the catalogue the project-wide `/design` render reads. Mutating → `Prototypes`
/// changed (an open `/design` catalogue surface re-pulls).
///
/// `template` stays OPTIONAL — a `[NEW]` is legitimately authored from its `params_json` /
/// `states_json` alone, before anyone has written its markup (review 01KYHV9F1JXCCKYMYYZR6Y1JYD). But
/// a template that is PRESENT AND BLANK is a mistake, not an omission: it is refused
/// ([`crate::mcp_tools_design::ensure_not_blank`]) exactly as the refresh path refuses it, so "no
/// template" is said by leaving the argument out, never by sending whitespace.
pub fn proto_author_component(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let prototype_id = require_str(args, "prototype_id")?;
    let name = require_str(args, "name")?;
    let template = match optional_str(args, "template")? {
        Some(t) => {
            crate::mcp_tools_design::ensure_not_blank(t)?;
            t.to_string()
        }
        None => String::new(),
    };
    crate::mcp_tools_design::ensure_template_accepted(&template)?;
    let params_json = optional_str(args, "params_json")?.map(str::to_string);
    // The tolerated MAP form of `states_json` is CANONICALIZED before anything reads or stores it,
    // so an axis ORDER never depends on how a JSON object is iterated (finding
    // `01KYSNPWWG9DA0AXP6D22GV5BK`, `crate::mcp_tools_design::canonical_states_json`).
    let states_json =
        crate::mcp_tools_design::canonical_states_json(optional_str(args, "states_json")?);
    // A param type the UI cannot render is REFUSED here too (finding `01KYPSG0HPG2D28CMJEXRKNEVK`):
    // a `[NEW]` is often authored from its params ALONE, so this is precisely where a `"type":
    // "enum"` used to enter the catalogue and come out as a free-text field. The SHAPE of both
    // documents is held to the same rule (finding `01KZ3JS6WFVGYS6JKB9C4J50HR`) — and all the more
    // so here, since a `[NEW]` authored from its params alone IS its params.
    crate::mcp_tools_design::ensure_params_json_accepted(params_json.as_deref())?;
    crate::mcp_tools_design::ensure_states_json_accepted(states_json.as_deref())?;
    // The SCOPED CSS SLOT, validated exactly as on the refresh path (finding
    // `01KYVMTWB71R0D0CT1SV88J8DX`): a `[NEW]` is the component a proto INVENTS, so it is where an
    // exception utilities cannot express is most likely to be born — and where it must reach the
    // coder with its scope and its reason instead of disappearing.
    let css_slot = crate::mcp_tools_design::read_css_slot_arg(args, name)?;
    // The STATE-CASE contract, diagnosed exactly as on the refresh path (finding
    // `01KYPSG1HM2GPJMNH2SP3DDADH`): `states_json` declares the names, the TEMPLATE carries the
    // `data-nyx-state-case` elements. A `[NEW]` authored WITHOUT a template yet warns about
    // nothing — there is no rendering to judge.
    let variance = crate::mcp_tools_design::variance_warnings(
        name,
        &template,
        states_json.as_deref(),
        params_json.as_deref(),
    );
    let result = db
        .with_conn(
            |c| -> Result<Result<Value, RpcError>, diesel::result::Error> {
                let Some(prototype) = db::get_prototype(c, prototype_id)? else {
                    return Ok(Err(unknown("prototype", prototype_id)));
                };
                // prototype → spec → project (the catalogue is project-scoped).
                let Some(spec) = db::get_spec(c, &prototype.spec_id)? else {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidState,
                        format!("prototype {prototype_id}'s Spec has no project"),
                    )));
                };
                // A `[NEW]` is by definition ABSENT from the catalogue — refuse to shadow an existing one.
                if db::get_component_by_name(c, &spec.project_id, name)?.is_some() {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidState,
                        format!(
                        "component '{name}' is already in the catalogue of project {} — a [NEW] \
                         component is one ABSENT from the catalogue (nothing to author)",
                        spec.project_id
                    ),
                    )));
                }
                // The REFERENCE contract needs the CATALOGUE (finding
                // `01KYSCZ822K4ZFAAYTG6GT6YWZ`): a `[NEW]` authored with a `data-nyx-use` must name
                // a component that exists, and must not close a cycle with one that references it
                // back. Checked here, where the project is finally resolved.
                let mut warnings = variance.clone();
                warnings.extend(crate::mcp_tools_design::reference_warnings(
                    name,
                    &template,
                    &crate::mcp_tools_design::reference_catalogue(c, &spec.project_id)?,
                ));
                if let Err(refusal) =
                    crate::mcp_tools_design::ensure_slot_tokens(c, &spec.project_id, &css_slot)?
                {
                    return Ok(Err(refusal));
                }
                let css_slot_json = match css_slot.write() {
                    db::CssSlotWrite::Set(blob) => Some(blob),
                    // A brand-new component has no stored slot to keep or clear.
                    db::CssSlotWrite::Keep | db::CssSlotWrite::Clear => None,
                };
                let component = db::upsert_component(
                    c,
                    &spec.project_id,
                    &db::ComponentInput {
                        name: name.to_string(),
                        template: template.clone(),
                        params_json: params_json.clone(),
                        states_json: states_json.clone(),
                        css_slot_json,
                        // Greenfield: born from a proto, no on-disk source yet (the coder builds it).
                        ..Default::default()
                    },
                )?;
                let css_slot_echo = crate::mcp_tools_design::stored_slot_echo(
                    name,
                    component.css_slot_json.as_deref(),
                );
                Ok(Ok(json!({
                    "component": component,
                    "prototype_id": prototype_id,
                    "warnings": &warnings,
                    // The EXCEPTION the [NEW] carries, with its scope, its compiled CSS and its
                    // reason — `null` when it carries none.
                    "css_slot": css_slot_echo,
                })))
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(result, ChangedTopic::Prototypes))
}

/// The AUTO-MAP signal for a `proto_*` MUTATION (task #12): the `prototype_id` a successful tool
/// worked, so the calling terminal's active session can be linked to it (the proto twin of
/// [`crate::mcp_tools_spec::worked_spec_id`]). Only the MUTATING tools are work signals — a
/// `proto_get` READ returns `None`, so a read never maps. Each mutation echoes an entity carrying
/// the prototype (directly, or through its round / feedback), so the id is read off the result.
pub fn worked_prototype_id(name: &str, result: &Value) -> Option<String> {
    let str_at = |obj: &Value, path: &[&str]| -> Option<String> {
        let mut cur = obj;
        for key in path {
            cur = cur.get(key)?;
        }
        cur.as_str().map(str::to_string)
    };
    match name {
        // Create / resolve a comment return `result.feedback`, which carries `prototype_id`.
        PROTO_FEEDBACK_CREATE_TOOL | PROTO_FEEDBACK_RESOLVE_TOOL => {
            str_at(result, &["feedback", "prototype_id"])
        }
        // Present / rebase return `result.round`, which carries `prototype_id`.
        PROTO_PRESENT_ROUND_TOOL | PROTO_REBASE_TOOL => str_at(result, &["round", "prototype_id"]),
        // Distill returns `result.lock`, which carries `prototype_id`.
        PROTO_DISTILL_TOOL => str_at(result, &["lock", "prototype_id"]),
        // Elect echoes the manifest entry PLUS a top-level `prototype_id` (resolved from the round)
        // so it maps too; the agent's reco (review #46) echoes it the same way.
        PROTO_ELECT_VARIANT_TOOL | PROTO_RECOMMEND_VARIANT_TOOL => {
            str_at(result, &["prototype_id"])
        }
        // The CREATION tools (review #34) each echo a top-level `prototype_id` (the created or the
        // owning prototype), so working the amorçage links the session to the proto too.
        PROTO_CREATE_TOOL
        | PROTO_ADD_PAGE_TOOL
        | PROTO_ADD_VERSION_TOOL
        | PROTO_ADD_VARIANT_TOOL
        | PROTO_PIN_PAGE_TOOL
        | PROTO_OPEN_ROUND_TOOL
        | PROTO_AUTHOR_COMPONENT_TOOL => str_at(result, &["prototype_id"]),
        // The READS (`proto_get`, `proto_composition`) and any non-proto tool never map (a read is
        // not "working the proto").
        _ => None,
    }
}

// --- Dispatch --------------------------------------------------------------

/// Route a PROTOTYPE tool call, or `None` when `name` is not one of ours. Called from
/// [`crate::mcp_tools_core::dispatch_extension_tool`] so the proto tools share the SAME dispatch
/// entry point (one dispatcher, one pool) as every other extension tool.
///
/// EVERY answer leaves through [`bounded_tool_result`] (review 01KYMF0KFZQG5JYZDGZCR85BAM): the two
/// growing READS ([`proto_get`], [`proto_composition`]) cap themselves gracefully, and the router's
/// net catches whatever else could grow — a variant echoing back a huge `composition_json` the agent
/// posted, a shape added later. The cap is a property of the ROUTER, so no future tool can be added
/// past it by forgetting.
pub fn dispatch_proto_tool(
    db: &Db,
    name: &str,
    args: &Value,
) -> Option<Result<ToolOutcome, RpcError>> {
    let outcome = route_proto_tool(db, name, args)?;
    Some(outcome.map(|mut outcome| {
        outcome.result = bounded_tool_result(name, outcome.result);
        outcome
    }))
}

/// The routing table itself — [`dispatch_proto_tool`] minus the response cap.
fn route_proto_tool(db: &Db, name: &str, args: &Value) -> Option<Result<ToolOutcome, RpcError>> {
    match name {
        PROTO_GET_TOOL => Some(proto_get(db, args)),
        PROTO_PRESENT_ROUND_TOOL => Some(proto_present_round(db, args)),
        PROTO_ELECT_VARIANT_TOOL => Some(proto_elect_variant(db, args)),
        PROTO_RECOMMEND_VARIANT_TOOL => Some(proto_recommend_variant(db, args)),
        PROTO_FEEDBACK_CREATE_TOOL => Some(proto_feedback_create(db, args)),
        PROTO_FEEDBACK_RESOLVE_TOOL => Some(proto_feedback_resolve(db, args)),
        PROTO_REBASE_TOOL => Some(proto_rebase(db, args)),
        PROTO_DISTILL_TOOL => Some(proto_distill(db, args)),
        PROTO_COMPOSITION_TOOL => Some(proto_composition(db, args)),
        PROTO_CREATE_TOOL => Some(proto_create(db, args)),
        PROTO_ADD_PAGE_TOOL => Some(proto_add_page(db, args)),
        PROTO_ADD_VERSION_TOOL => Some(proto_add_version(db, args)),
        PROTO_ADD_VARIANT_TOOL => Some(proto_add_variant(db, args)),
        PROTO_PIN_PAGE_TOOL => Some(proto_pin_page(db, args)),
        PROTO_OPEN_ROUND_TOOL => Some(proto_open_round(db, args)),
        PROTO_AUTHOR_COMPONENT_TOOL => Some(proto_author_component(db, args)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A memory DB seeded with a project + Spec + prototype + one page/version/variant pinned into
    /// the current round. Returns `(db, spec_id, prototype_id, round_id, page_id, variant_id)`.
    #[allow(clippy::type_complexity)]
    fn seeded() -> (Db, String, String, String, String, String) {
        let db = Db::in_memory();
        db.with_conn(|c| {
            let (project, _root) = db::create_project(c, "Proj", "/tmp/proto-mcp", None).unwrap();
            let (spec, _rev) =
                db::create_spec(c, &project.id, "Feature", db::SpecSections::empty()).unwrap();
            let proto = db::create_prototype(c, &spec.id, None).unwrap();
            let round = db::current_round(c, &proto.id).unwrap().unwrap();
            let page = db::create_page(c, &proto.id, "home", "Home", 0).unwrap();
            let version = db::create_page_version(c, &page.id, "v1", None).unwrap();
            let variant = db::create_variant(
                c,
                &version.id,
                &db::VariantInput {
                    label: "a".into(),
                    title: "A".into(),
                    ..Default::default()
                },
            )
            .unwrap();
            db::set_round_page(c, &round.id, &page.id, &version.id, 0).unwrap();
            (db.clone(), spec.id, proto.id, round.id, page.id, variant.id)
        })
    }

    /// PRESENT a round through the real dispatch. Distilling a round nobody was SHOWN is refused
    /// (finding `01KZ3JTV1AY53QDTGB8V8P8YZE`, B), so every test that freezes has to walk the
    /// sequence the manual states: present → elect → distill.
    fn present(db: &Db, round_id: &str) {
        dispatch_proto_tool(
            db,
            PROTO_PRESENT_ROUND_TOOL,
            &json!({ "round_id": round_id }),
        )
        .expect("routed")
        .expect("presented");
    }

    /// The full agent contract through the real dispatch: `proto_get` surfaces the explicit state,
    /// then the four mutating tools drive the prototype — each reporting a `Prototypes` change and
    /// leaving its `proto.*` domain event (asserted via the outbox).
    #[test]
    fn proto_contract_drives_state_and_emits_events_through_dispatch() {
        let _g = db_guard();
        let (db, spec_id, prototype_id, round_id, page_id, variant_id) = seeded();

        // proto_get: explicit state (a read — no effects).
        let got = dispatch_proto_tool(&db, PROTO_GET_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        assert!(got.effects.is_empty(), "proto_get is a read");
        assert_eq!(got.result["prototype"]["id"], json!(prototype_id));
        assert_eq!(got.result["current_round"]["id"], json!(round_id));

        // present round → Prototypes changed.
        let presented = dispatch_proto_tool(
            &db,
            PROTO_PRESENT_ROUND_TOOL,
            &json!({ "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(presented.effects, vec![ChangedTopic::Prototypes]);
        assert!(presented.result["round"]["presented_at"].is_number());

        // elect the RETENU variant.
        let elected = dispatch_proto_tool(
            &db,
            PROTO_ELECT_VARIANT_TOOL,
            &json!({ "round_id": round_id, "page_id": page_id, "variant_id": variant_id, "decided_by": "user:k" }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(
            elected.result["round_page"]["chosen_variant_id"],
            json!(variant_id)
        );

        // create + resolve a pre-dev comment.
        let created = dispatch_proto_tool(
            &db,
            PROTO_FEEDBACK_CREATE_TOOL,
            &json!({ "prototype_id": prototype_id, "body": "too small", "node_key": "Root/Title" }),
        )
        .expect("routed")
        .expect("ok");
        let feedback_id = created.result["feedback"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(created.result["feedback"]["author_kind"], json!("session"));
        let resolved = dispatch_proto_tool(
            &db,
            PROTO_FEEDBACK_RESOLVE_TOOL,
            &json!({ "feedback_id": feedback_id }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(resolved.result["feedback"]["status"], json!("resolved"));

        // The four proto.* events landed in the outbox (aucune action inaperçue).
        let types: Vec<String> = db
            .with_conn(db::list_domain_events)
            .unwrap()
            .into_iter()
            .filter(|e| e.aggregate == "prototype")
            .map(|e| e.event_type)
            .collect();
        for expected in [
            db::EVENT_PROTO_ROUND_PRESENTED,
            db::EVENT_PROTO_VARIANT_ELECTED,
            db::EVENT_PROTO_FEEDBACK_CREATED,
            db::EVENT_PROTO_FEEDBACK_RESOLVED,
        ] {
            assert!(types.iter().any(|t| t == expected), "{expected} emitted");
        }
    }

    /// The contract is GUARDED with explicit states: an unknown round / feedback is `invalid_id`,
    /// and electing on a page NOT pinned in the round is `invalid_state` (never a silent no-op).
    #[test]
    fn proto_contract_guards_are_explicit() {
        let _g = db_guard();
        let (db, _spec, prototype_id, round_id, _page, variant_id) = seeded();

        assert_eq!(
            dispatch_proto_tool(
                &db,
                PROTO_PRESENT_ROUND_TOOL,
                &json!({ "round_id": "nope" })
            )
            .unwrap()
            .unwrap_err()
            .code,
            RpcCode::InvalidId
        );
        assert_eq!(
            dispatch_proto_tool(
                &db,
                PROTO_FEEDBACK_RESOLVE_TOOL,
                &json!({ "feedback_id": "nope" })
            )
            .unwrap()
            .unwrap_err()
            .code,
            RpcCode::InvalidId
        );
        // Electing on a page that is NOT pinned in the round → explicit invalid_state.
        assert_eq!(
            dispatch_proto_tool(
                &db,
                PROTO_ELECT_VARIANT_TOOL,
                &json!({ "round_id": round_id, "page_id": "not-pinned", "variant_id": variant_id })
            )
            .unwrap()
            .unwrap_err()
            .code,
            RpcCode::InvalidState
        );
        // A well-formed create needs a real prototype.
        assert_eq!(
            dispatch_proto_tool(
                &db,
                PROTO_FEEDBACK_CREATE_TOOL,
                &json!({ "prototype_id": "nope", "body": "x" })
            )
            .unwrap()
            .unwrap_err()
            .code,
            RpcCode::InvalidId
        );
        let _ = prototype_id;
    }

    /// DONE-CRITERION (finding `01KZ3JS646G83NF4NRKY8VX5CP`): AN AGENT THAT HAS ONLY THE `spec_id`
    /// CAN NAME EVERY ID IT NEEDS TO ACT. Versions and variants used to exist in no read at all, so
    /// a resumed session could neither recommend, nor elect, nor preview. Pinned here:
    ///   * `proto_get(spec_id)` carries `versions` and `variants`, each row naming its parent, and a
    ///     variant row carries its IDENTITY (never the tree — `composition_bytes` says how big it is);
    ///   * `versions` / `variants` are real paginated SECTIONS, not an overview-only courtesy;
    ///   * the DESIGN LOCK is readable in the state (`locked` + `design_lock`), not only through a
    ///     refusal;
    ///   * a duplicate creation hands back THE EXISTING ID — the one moment nyx knows it and the
    ///     caller does not.
    #[test]
    fn a_resumed_session_reads_every_id_and_the_lock_from_the_state() {
        let _g = db_guard();
        let (db, spec_id, prototype_id, round_id, page_id, variant_id) = seeded();
        let version_id = db
            .with_conn(|c| db::get_variant(c, &variant_id))
            .unwrap()
            .unwrap()
            .page_version_id;

        let state = dispatch_proto_tool(&db, PROTO_GET_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        // The two lists that did not exist — with the ids the actions take.
        assert_eq!(state.result["versions"][0]["id"], json!(version_id));
        assert_eq!(state.result["versions"][0]["page_id"], json!(page_id));
        assert_eq!(state.result["variants"][0]["id"], json!(variant_id));
        assert_eq!(
            state.result["variants"][0]["page_version_id"],
            json!(version_id),
            "a variant row names its parent, so the three lists JOIN"
        );
        assert!(
            state.result["variants"][0]
                .get("composition_json")
                .is_none()
                && state.result["variants"][0]["composition_bytes"].is_number(),
            "a state read names the tree's SIZE and never carries the tree"
        );
        // …and they page like every other section.
        let paged = dispatch_proto_tool(
            &db,
            PROTO_GET_TOOL,
            &json!({ "spec_id": spec_id, "section": "variants" }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(paged.result["mode"], json!("section"));
        assert_eq!(paged.result["items"][0]["id"], json!(variant_id));

        // THE LOCK, before and after — read, never discovered through a refusal.
        assert_eq!(state.result["locked"], json!(false));
        assert!(state.result["design_lock"].is_null());
        present(&db, &round_id);
        dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");
        let frozen = dispatch_proto_tool(&db, PROTO_GET_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        assert_eq!(frozen.result["locked"], json!(true));
        assert_eq!(
            frozen.result["design_lock"]["locked_round_id"],
            json!(round_id)
        );
        assert_eq!(frozen.result["current_round"]["locked"], json!(true));

        // A CONFLICT HANDS BACK THE ID. Every creator, one shape.
        let duplicate_page = dispatch_proto_tool(
            &db,
            PROTO_ADD_PAGE_TOOL,
            &json!({ "prototype_id": prototype_id, "slug": "home", "title": "Home again" }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(duplicate_page.code, RpcCode::InvalidState);
        assert!(
            duplicate_page.message.contains(&page_id),
            "the slug conflict names the existing page id: {}",
            duplicate_page.message
        );
        let duplicate_version = dispatch_proto_tool(
            &db,
            PROTO_ADD_VERSION_TOOL,
            &json!({ "page_id": page_id, "label": "v1" }),
        )
        .expect("routed")
        .unwrap_err();
        assert!(
            duplicate_version.message.contains(&version_id),
            "the label conflict names the existing version id: {}",
            duplicate_version.message
        );
        let duplicate_variant = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({ "page_version_id": version_id, "label": "a", "title": "A bis" }),
        )
        .expect("routed")
        .unwrap_err();
        assert!(
            duplicate_variant.message.contains(&variant_id),
            "the label conflict names the existing variant id: {}",
            duplicate_variant.message
        );
        let duplicate_round = dispatch_proto_tool(
            &db,
            PROTO_OPEN_ROUND_TOOL,
            &json!({ "prototype_id": prototype_id, "label": "v1" }),
        )
        .expect("routed")
        .unwrap_err();
        assert!(
            duplicate_round.message.contains(&round_id),
            "the label conflict names the existing round id: {}",
            duplicate_round.message
        );
    }

    /// DONE-CRITERION (finding `01KZ3JS6WFVGYS6JKB9C4J50HR`): A COMPOSITION NYX CANNOT READ IS
    /// REFUSED AT THE WRITE, and an `[inline]` node obeys the SAME logic-less contract as a
    /// component template.
    ///
    /// The malformations used to answer `warnings: []` — the worst possible answer, because that
    /// empty list is the manual's only "done": the node-contract check ran over a tree that parsed
    /// to NOTHING, found no node, and reported nothing wrong. And `{{#if}}` in an inline node passed
    /// without a word while the very same construct in a component template is refused hard.
    #[test]
    fn a_composition_nyx_cannot_read_is_refused_at_the_write() {
        let _g = db_guard();
        let (db, _spec, _prototype_id, _round, _page, variant_id) = seeded();
        let version_id = db
            .with_conn(|c| db::get_variant(c, &variant_id))
            .unwrap()
            .unwrap()
            .page_version_id;
        let add = |label: &str, composition: &str| {
            dispatch_proto_tool(
                &db,
                PROTO_ADD_VARIANT_TOOL,
                &json!({
                    "page_version_id": version_id,
                    "label": label,
                    "title": label,
                    "composition_json": composition,
                }),
            )
            .expect("routed")
        };

        let broken = add("broken", "{\"component\": \"Card\"").unwrap_err();
        assert_eq!(broken.code, RpcCode::InvalidArgument);
        assert!(
            broken.message.contains("is not valid JSON"),
            "{}",
            broken.message
        );
        let array_root = add("array", "[{\"component\": \"Card\"}]").unwrap_err();
        assert!(
            array_root.message.contains("is an ARRAY at its root")
                && array_root.message.contains("children"),
            "an array root is refused with the wrapping to write: {}",
            array_root.message
        );
        // …and the LOGIC an inline node used to smuggle in.
        let logic = add(
            "logic",
            &json!({ "inline": "<p>{{#if ok}}yes{{/if}}</p>" }).to_string(),
        )
        .unwrap_err();
        assert!(
            logic.message.contains("[inline]") && logic.message.contains("composition TREE"),
            "an inline node is held to the component template's contract, in its own words: {}",
            logic.message
        );

        // NOTHING of the three was stored — the guards run before the transaction.
        let stored = db.with_conn(|c| db::list_variants(c, &version_id)).unwrap();
        assert_eq!(
            stored.len(),
            1,
            "only the fixture's variant is there: {:?}",
            stored.iter().map(|v| &v.label).collect::<Vec<_>>()
        );
        // …and a well-formed tree with an inline node still goes through untouched.
        add(
            "fine",
            &json!({ "key": "root", "children": [{ "inline": "<p>{{body}}</p>" }] }).to_string(),
        )
        .expect("a legitimate composition is unaffected");
    }

    /// DONE-CRITERION (finding `01KZ3JTV1AY53QDTGB8V8P8YZE`): THE THREE SILENT CORRUPTIONS OF THE
    /// CODER CONTRACT ARE REFUSED OR NAMED.
    ///   A. electing a variant that belongs to ANOTHER page is refused, naming what IS electable;
    ///   B. distilling refuses a round nobody presented, a round pinning nothing, and a round whose
    ///      pinned page owes an arbitration — and it refuses to MOVE a posed lock without
    ///      `supersede: true`, saying which round it superseded when it does;
    ///   C. `proto_composition` COUNTS and NAMES the frozen pages that carry no elected variant.
    #[test]
    fn the_coder_contract_refuses_what_would_corrupt_it_and_names_what_it_cannot_deliver() {
        let _g = db_guard();
        let (db, _spec, prototype_id, round_id, page_id, variant_id) = seeded();

        // --- A. A VARIANT OF ANOTHER PAGE IS NOT THIS PAGE'S RETENU ---------------------------
        let foreign_variant = db
            .with_conn(|c| {
                let other = db::create_page(c, &prototype_id, "pricing", "Pricing", 1)?;
                let version = db::create_page_version(c, &other.id, "v1", None)?;
                db::create_variant(
                    c,
                    &version.id,
                    &db::VariantInput {
                        label: "a".into(),
                        title: "Pricing A".into(),
                        ..Default::default()
                    },
                )
                .map(|v| v.id)
                .map_err(|e| match e {
                    db::RoundMutationError::Db(e) => e,
                    other => panic!("unexpected: {other:?}"),
                })
            })
            .unwrap();
        let refused = dispatch_proto_tool(
            &db,
            PROTO_ELECT_VARIANT_TOOL,
            &json!({ "round_id": round_id, "page_id": page_id, "variant_id": foreign_variant }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(refused.code, RpcCode::InvalidState);
        assert!(
            refused.message.contains("ANOTHER PAGE") && refused.message.contains(&variant_id),
            "the refusal says WHERE the variant comes from and WHICH ones are electable: {}",
            refused.message
        );

        // --- B. DISTILL FREEZES A DECISION, NEVER "ANYTHING" ----------------------------------
        let empty_round = db
            .with_conn(|c| db::create_round(c, &prototype_id, "v-empty", None))
            .unwrap();
        let never_presented = dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": empty_round.id }),
        )
        .expect("routed")
        .unwrap_err();
        assert!(
            never_presented.message.contains("never PRESENTED"),
            "a round nobody was shown is not a decision: {}",
            never_presented.message
        );
        present(&db, &empty_round.id);
        let pins_nothing = dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": empty_round.id }),
        )
        .expect("routed")
        .unwrap_err();
        assert!(
            pins_nothing.message.contains("pins NO page"),
            "an empty manifest freezes nothing: {}",
            pins_nothing.message
        );
        // A page pinned on a version carrying TWO variants and no RETENU owes an arbitration.
        let (arbitration_page, arbitration_version) = db
            .with_conn(|c| {
                let page = db::create_page(c, &prototype_id, "checkout", "Checkout", 2)?;
                let version = db::create_page_version(c, &page.id, "v1", None)?;
                for label in ["a", "b"] {
                    db::create_variant(
                        c,
                        &version.id,
                        &db::VariantInput {
                            label: label.into(),
                            title: label.to_uppercase(),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                }
                db::set_round_page(c, &empty_round.id, &page.id, &version.id, 0).unwrap();
                Ok::<_, diesel::result::Error>((page.id, version.id))
            })
            .unwrap();
        let owes = dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": empty_round.id }),
        )
        .expect("routed")
        .unwrap_err();
        assert!(
            owes.message.contains("owes an arbitration") && owes.message.contains("Checkout"),
            "an unelected pinned page is refused BY NAME: {}",
            owes.message
        );

        // The nominal freeze, once the round really is a decision.
        present(&db, &round_id);
        let first = dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");
        assert!(
            first.result["superseded_round_id"].is_null(),
            "a first freeze supersedes nothing"
        );

        // …and the lock does not MOVE without being told to.
        let arbitration_variant = db
            .with_conn(|c| db::list_variants(c, &arbitration_version))
            .unwrap()[0]
            .id
            .clone();
        dispatch_proto_tool(
            &db,
            PROTO_ELECT_VARIANT_TOOL,
            &json!({
                "round_id": empty_round.id,
                "page_id": arbitration_page,
                "variant_id": arbitration_variant,
            }),
        )
        .expect("routed")
        .expect("ok");
        let would_move = dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": empty_round.id }),
        )
        .expect("routed")
        .unwrap_err();
        assert!(
            would_move.message.contains("ALREADY frozen")
                && would_move.message.contains("supersede: true"),
            "moving a posed lock is refused, and the refusal names the argument: {}",
            would_move.message
        );
        let moved = dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": empty_round.id, "supersede": true }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(moved.result["superseded_round_id"], json!(round_id));
        assert!(
            moved.result["warnings"]
                .as_array()
                .is_some_and(|w| !w.is_empty()),
            "a lock that moved SAYS it moved"
        );

        // --- C. A FROZEN PAGE WITH NO ELECTION IS COUNTED AND NAMED ---------------------------
        // B just made this state UNREACHABLE through the tools, which is the point — so it is posed
        // at the STORAGE level here, because that is exactly what this signal exists for: a lock
        // posed before that guard, or a page whose only variant vanished under a live freeze.
        let legacy_round = db
            .with_conn(|c| db::create_round(c, &prototype_id, "v-legacy", None))
            .unwrap();
        db.with_conn(|c| {
            let page = db::create_page(c, &prototype_id, "invoice", "Invoice", 3).unwrap();
            let version = db::create_page_version(c, &page.id, "v1", None).unwrap();
            for label in ["a", "b"] {
                db::create_variant(
                    c,
                    &version.id,
                    &db::VariantInput {
                        label: label.into(),
                        title: label.to_uppercase(),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
            db::set_round_page(c, &legacy_round.id, &page.id, &version.id, 0)
        })
        .unwrap();
        db.with_conn(|c| db::lock_design(c, &prototype_id, &legacy_round.id, None))
            .unwrap();
        let seam = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(seam.result["unelected_pages"], json!(1));
        let warnings = seam.result["warnings"]
            .as_array()
            .expect("warnings is always present")
            .iter()
            .filter_map(|w| w.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            warnings.contains("Invoice") && warnings.contains("NO elected variant"),
            "the seam NAMES the page it could not deliver, on the model of `oversized`: {warnings}"
        );
    }

    /// DONE-CRITERION (review #46): an agent can DESIGNATE its recommended variant (`is_main`) via
    /// MCP, and that reco is STRICTLY DISTINCT from the user's RETENU. Pinned here:
    ///   * `proto_recommend_variant` sets `is_main` and leaves `chosen_variant_id` NULL — posing a
    ///     reco NEVER poses the RETENU;
    ///   * ONE main per version, REVERSIBLE — recommending a sibling moves the flag;
    ///   * the two tiers coexist: the user's RETENU can be a DIFFERENT variant than the agent's reco;
    ///   * the Phase-4 lock refuses a reco on a frozen round, like every other mutation;
    ///   * unknown variant → `invalid_id`, missing arg → `invalid_argument`.
    #[test]
    fn agent_recommendation_is_posable_and_never_poses_the_retenu() {
        let _g = db_guard();
        let (db, _spec, prototype_id, round_id, page_id, variant_a) = seeded();
        // A second variant on the SAME version, so the reco has something to move between.
        let page_version_id = db
            .with_conn(|c| Ok::<_, diesel::result::Error>(db::get_variant(c, &variant_a)?.unwrap()))
            .unwrap()
            .page_version_id;
        let variant_b = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({ "page_version_id": page_version_id, "label": "b", "title": "B" }),
        )
        .expect("routed")
        .expect("ok")
        .result["variant"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // The agent RECOMMENDS `a`. It is a mutation (Prototypes changed) that maps to the proto.
        let reco = dispatch_proto_tool(
            &db,
            PROTO_RECOMMEND_VARIANT_TOOL,
            &json!({ "variant_id": variant_a }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(reco.effects, vec![ChangedTopic::Prototypes]);
        assert_eq!(reco.result["variant"]["is_main"], json!(true));
        assert_eq!(reco.result["prototype_id"], json!(prototype_id));
        assert_eq!(
            worked_prototype_id(PROTO_RECOMMEND_VARIANT_TOOL, &reco.result),
            Some(prototype_id.clone()),
            "the reco is work on the proto (auto-map)",
        );
        // …and NOTHING was elected: the RETENU lives on the round manifest, untouched.
        let manifest = db
            .with_conn(|c| db::get_round_page(c, &round_id, &page_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            manifest.chosen_variant_id, None,
            "posing is_main NEVER poses the RETENU",
        );

        // ONE main per version, REVERSIBLE: recommending `b` moves the reco off `a`.
        let moved = dispatch_proto_tool(
            &db,
            PROTO_RECOMMEND_VARIANT_TOOL,
            &json!({ "variant_id": variant_b }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(moved.result["variant"]["is_main"], json!(true));
        let (a_row, b_row) = db
            .with_conn(|c| {
                Ok::<_, diesel::result::Error>((
                    db::get_variant(c, &variant_a)?.unwrap(),
                    db::get_variant(c, &variant_b)?.unwrap(),
                ))
            })
            .unwrap();
        assert!(!a_row.is_main, "the previous reco is cleared");
        assert!(b_row.is_main, "exactly one main per version");

        // The USER then retains `a` — a DIFFERENT variant. The two tiers coexist untouched.
        dispatch_proto_tool(
            &db,
            PROTO_ELECT_VARIANT_TOOL,
            &json!({ "round_id": round_id, "page_id": page_id, "variant_id": variant_a }),
        )
        .expect("routed")
        .expect("ok");
        let (manifest, b_row) = db
            .with_conn(|c| {
                Ok::<_, diesel::result::Error>((
                    db::get_round_page(c, &round_id, &page_id)?.unwrap(),
                    db::get_variant(c, &variant_b)?.unwrap(),
                ))
            })
            .unwrap();
        assert_eq!(
            manifest.chosen_variant_id.as_deref(),
            Some(variant_a.as_str())
        );
        assert!(b_row.is_main, "the RETENU does not move the agent's reco");

        // The Phase-4 lock refuses a reco on the FROZEN round, like every other mutation.
        present(&db, &round_id);
        dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");
        let refused = dispatch_proto_tool(
            &db,
            PROTO_RECOMMEND_VARIANT_TOOL,
            &json!({ "variant_id": variant_a }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(refused.code, RpcCode::InvalidState);
        assert!(refused.message.contains("FROZEN"));
        let a_row = db
            .with_conn(|c| db::get_variant(c, &variant_a))
            .unwrap()
            .unwrap();
        assert!(!a_row.is_main, "a refused reco writes nothing");

        // Structured refusals for the argument edges.
        assert_eq!(
            dispatch_proto_tool(
                &db,
                PROTO_RECOMMEND_VARIANT_TOOL,
                &json!({ "variant_id": "nope" })
            )
            .unwrap()
            .unwrap_err()
            .code,
            RpcCode::InvalidId
        );
        assert_eq!(
            dispatch_proto_tool(&db, PROTO_RECOMMEND_VARIANT_TOOL, &json!({}))
                .unwrap()
                .unwrap_err()
                .code,
            RpcCode::InvalidArgument
        );
    }

    /// The `rebase` (rollback) and `distill` (freeze) actions run via MCP, and `worked_prototype_id`
    /// signals the worked prototype for EVERY mutation (so the auto-map links the session) while a
    /// `proto_get` READ never signals — the seam for "actions via MCP" + "proto lié à la session".
    #[test]
    fn proto_rebase_distill_and_automap_signal() {
        let _g = db_guard();
        let (db, spec_id, prototype_id, round_id, _page, _variant) = seeded();

        // rebase: rollback onto the current round → a new round carrying `prototype_id`.
        let rebased = dispatch_proto_tool(
            &db,
            PROTO_REBASE_TOOL,
            &json!({ "prototype_id": prototype_id, "source_round_id": round_id, "label": "v2" }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(rebased.effects, vec![ChangedTopic::Prototypes]);
        assert_eq!(rebased.result["round"]["prototype_id"], json!(prototype_id));
        assert_eq!(
            worked_prototype_id(PROTO_REBASE_TOOL, &rebased.result).as_deref(),
            Some(prototype_id.as_str())
        );

        // distill: freeze the current round → a design lock carrying `prototype_id`.
        present(&db, &round_id);
        let distilled = dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": round_id, "decided_by": "user:k" }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(distilled.result["lock"]["locked_round_id"], json!(round_id));
        assert_eq!(
            worked_prototype_id(PROTO_DISTILL_TOOL, &distilled.result).as_deref(),
            Some(prototype_id.as_str())
        );

        // distill on a round that is NOT this prototype's → explicit invalid_state.
        assert_eq!(
            dispatch_proto_tool(
                &db,
                PROTO_DISTILL_TOOL,
                &json!({ "prototype_id": prototype_id, "round_id": "foreign" })
            )
            .unwrap()
            .unwrap_err()
            .code,
            RpcCode::InvalidId
        );

        // A `proto_get` READ never signals a worked prototype (no auto-map on a read).
        let got = proto_get(&db, &json!({ "spec_id": spec_id })).unwrap();
        assert!(worked_prototype_id(PROTO_GET_TOOL, &got.result).is_none());
    }

    #[test]
    fn dispatch_proto_tool_returns_none_for_a_foreign_tool() {
        let _g = db_guard();
        let (db, ..) = seeded();
        assert!(dispatch_proto_tool(&db, "list_projects", &json!({})).is_none());
    }

    /// `proto_composition` is the coder SEAM READ (v2.7 Phase 4, task #8): it exposes NOTHING until
    /// the design is LOCKED (`{ composition: null }`), then surfaces the FROZEN composition (its
    /// locked round + the pinned pages). It is READ-ONLY (no effects) and never auto-maps (a read is
    /// not "working the proto"); an unknown prototype is an explicit `invalid_id`.
    #[test]
    fn proto_composition_exposes_only_the_frozen_election() {
        let _g = db_guard();
        let (db, _spec, prototype_id, round_id, _page, _variant) = seeded();

        // Before the lock: the seam exposes nothing (élection figée guarantee), and it is a READ.
        let before = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        assert!(before.effects.is_empty(), "proto_composition is a read");
        assert!(
            before.result["composition"].is_null(),
            "an unlocked prototype exposes no composition"
        );
        assert!(worked_prototype_id(PROTO_COMPOSITION_TOOL, &before.result).is_none());

        // Freeze the elected round (distill), then the seam exposes the FROZEN composition.
        present(&db, &round_id);
        dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");
        let after = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        assert!(after.effects.is_empty(), "still a read after lock");
        assert_eq!(
            after.result["composition"]["locked"]["locked_round_id"],
            json!(round_id),
            "the seam reads off the LOCK"
        );
        assert_eq!(
            after.result["composition"]["pages"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "the frozen round's one pinned page is exposed"
        );

        // An unknown prototype is an explicit invalid_id (guarded, not a silent null).
        assert_eq!(
            dispatch_proto_tool(
                &db,
                PROTO_COMPOSITION_TOOL,
                &json!({ "prototype_id": "nope" })
            )
            .unwrap()
            .unwrap_err()
            .code,
            RpcCode::InvalidId
        );
    }

    /// The "élection figée" immutability guard at the MCP surface (review #22): once `proto_distill`
    /// FREEZES the round, `proto_elect_variant` on that round is REFUSED with `invalid_state` (the
    /// freeze is ENFORCED, not just an exposure gate), and `proto_composition` keeps exposing the
    /// SAME frozen election — it never drifts. `proto_rebase` re-opens iteration on a NEW, non-locked
    /// round where electing works again (the rollback discipline is preserved).
    #[test]
    fn electing_on_a_frozen_round_is_refused_and_composition_never_drifts() {
        let _g = db_guard();
        let (db, _spec, prototype_id, round_id, page_id, variant_id) = seeded();

        // Elect the RETENU, then FREEZE the round (distill).
        dispatch_proto_tool(
            &db,
            PROTO_ELECT_VARIANT_TOOL,
            &json!({ "round_id": round_id, "page_id": page_id, "variant_id": variant_id }),
        )
        .expect("routed")
        .expect("ok");
        present(&db, &round_id);
        dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");

        // The frozen composition, snapshotted to compare against.
        let before = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");

        // Electing on the FROZEN round is refused as invalid_state (not a silent no-op).
        let err = dispatch_proto_tool(
            &db,
            PROTO_ELECT_VARIANT_TOOL,
            &json!({ "round_id": round_id, "page_id": page_id, "variant_id": variant_id }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);

        // The exposed composition is unchanged — élection figée, no drift.
        let after = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(
            after.result, before.result,
            "the frozen composition never drifted after a refused election"
        );

        // Rebase re-opens iteration: the NEW round is not locked, so electing works again.
        let rebased = dispatch_proto_tool(
            &db,
            PROTO_REBASE_TOOL,
            &json!({ "prototype_id": prototype_id, "source_round_id": round_id, "label": "v2" }),
        )
        .expect("routed")
        .expect("ok");
        let new_round_id = rebased.result["round"]["id"].as_str().unwrap().to_string();
        dispatch_proto_tool(
            &db,
            PROTO_ELECT_VARIANT_TOOL,
            &json!({ "round_id": new_round_id, "page_id": page_id, "variant_id": variant_id }),
        )
        .expect("routed")
        .expect("electing on a fresh (non-locked) round still works");
    }

    // --- CREATION / amorçage tools (review #34) --------------------------------

    /// A memory DB seeded with a project + a bare Spec (NO prototype yet). Returns
    /// `(db, spec_id, project_id)` — the starting point the CREATION tools amorce from.
    fn bare_spec() -> (Db, String, String) {
        let db = Db::in_memory();
        db.with_conn(|c| {
            let (project, _root) =
                db::create_project(c, "Proj", "/tmp/proto-create", None).unwrap();
            let (spec, _rev) =
                db::create_spec(c, &project.id, "Feature", db::SpecSections::empty()).unwrap();
            (db.clone(), spec.id, project.id)
        })
    }

    /// DONE-CRITERION (review #34): an agent can, via MCP ONLY, AMORCE a prototype end-to-end —
    /// create it on a Spec, add a page + version + variants, pin the page into the round, present it
    /// — and both `proto_get` (MCP) and `proto_tree` (the UI read-model) then SEE the whole tree.
    #[test]
    fn proto_creation_amorce_end_to_end_via_mcp() {
        let _g = db_guard();
        let (db, spec_id, _project_id) = bare_spec();

        // 1. Create the prototype on the Spec → v1 round seeded, Prototypes changed, session maps.
        let created = dispatch_proto_tool(&db, PROTO_CREATE_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        assert_eq!(created.effects, vec![ChangedTopic::Prototypes]);
        let prototype_id = created.result["prototype"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let round_id = created.result["current_round"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            worked_prototype_id(PROTO_CREATE_TOOL, &created.result).as_deref(),
            Some(prototype_id.as_str())
        );

        // 2. Add a page.
        let page = dispatch_proto_tool(
            &db,
            PROTO_ADD_PAGE_TOOL,
            &json!({ "prototype_id": prototype_id, "slug": "home", "title": "Home" }),
        )
        .expect("routed")
        .expect("ok");
        let page_id = page.result["page"]["id"].as_str().unwrap().to_string();
        assert_eq!(page.result["prototype_id"], json!(prototype_id));

        // 3. Add a version.
        let version = dispatch_proto_tool(
            &db,
            PROTO_ADD_VERSION_TOOL,
            &json!({ "page_id": page_id, "label": "v1" }),
        )
        .expect("routed")
        .expect("ok");
        let version_id = version.result["version"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            version.result["prototype_id"],
            json!(prototype_id),
            "version echoes its owning prototype (auto-map)"
        );

        // 4. Add TWO variants.
        let variant_a = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({ "page_version_id": version_id, "label": "a", "title": "A", "composition_json": "{\"root\":true}" }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(variant_a.effects, vec![ChangedTopic::Prototypes]);
        dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({ "page_version_id": version_id, "label": "b", "title": "B" }),
        )
        .expect("routed")
        .expect("ok");

        // 5. Pin the (page, version) into the round's manifest.
        let pinned = dispatch_proto_tool(
            &db,
            PROTO_PIN_PAGE_TOOL,
            &json!({ "round_id": round_id, "page_id": page_id, "page_version_id": version_id }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(pinned.result["round_page"]["page_id"], json!(page_id));

        // 6. Present the round (handback).
        dispatch_proto_tool(
            &db,
            PROTO_PRESENT_ROUND_TOOL,
            &json!({ "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");

        // proto_get (the AGENT read) sees the whole structure, incl. the pinned manifest.
        let got = dispatch_proto_tool(&db, PROTO_GET_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        assert_eq!(got.result["prototype"]["id"], json!(prototype_id));
        assert_eq!(got.result["pages"].as_array().unwrap().len(), 1);
        assert_eq!(
            got.result["current_round_manifest"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "the pinned page is in the current round manifest"
        );

        // proto_tree (the UI read-model) ALSO sees it → the tab becomes e2e-testable.
        let tree = db
            .with_conn(|c| db::proto_tree(c, &spec_id))
            .unwrap()
            .expect("prototype tree exists");
        assert_eq!(tree.pages.len(), 1, "proto_tree sees the page");
        assert_eq!(tree.pages[0].versions.len(), 1, "…its version");
        assert_eq!(
            tree.pages[0].versions[0].variants.len(),
            2,
            "…both variants"
        );
        assert!(
            tree.pages[0].manifest_entry.is_some(),
            "…and the page is pinned in the current round"
        );
    }

    /// DONE-CRITERION (review #34): the CREATION tools RESPECT the Phase-4 "élection figée" lock —
    /// once the round is design-locked (`proto_distill`), adding a variant under the pinned version
    /// AND pinning a page into the locked round are both refused with `invalid_state` (never a silent
    /// write that would drift the frozen election).
    #[test]
    fn proto_creation_respects_the_phase4_lock() {
        let _g = db_guard();
        let (db, _spec, prototype_id, round_id, page_id, variant_id) = seeded();
        let version_id = db
            .with_conn(|c| db::get_variant(c, &variant_id))
            .unwrap()
            .unwrap()
            .page_version_id;

        // Elect the RETENU, then FREEZE the round.
        dispatch_proto_tool(
            &db,
            PROTO_ELECT_VARIANT_TOOL,
            &json!({ "round_id": round_id, "page_id": page_id, "variant_id": variant_id }),
        )
        .expect("routed")
        .expect("ok");
        present(&db, &round_id);
        dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");

        // Adding a variant under the PINNED (frozen) version is refused.
        let err = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({ "page_version_id": version_id, "label": "c", "title": "C" }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(
            err.code,
            RpcCode::InvalidState,
            "add_variant on a frozen version refused"
        );

        // Pinning a page into the LOCKED round is refused.
        let err = dispatch_proto_tool(
            &db,
            PROTO_PIN_PAGE_TOOL,
            &json!({ "round_id": round_id, "page_id": page_id, "page_version_id": version_id }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(
            err.code,
            RpcCode::InvalidState,
            "pin into a locked round refused"
        );
    }

    /// DONE-CRITERION (review #34): authoring a `[NEW]` component from a proto makes it APPEAR in the
    /// project catalogue (greenfield — no source). A name ALREADY in the catalogue is refused
    /// (`invalid_state`: not a `[NEW]`), and an unknown prototype is `invalid_id`.
    #[test]
    fn proto_author_new_component_appears_in_catalogue() {
        let _g = db_guard();
        let (db, spec_id, project_id) = bare_spec();
        let created = dispatch_proto_tool(&db, PROTO_CREATE_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        let prototype_id = created.result["prototype"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // Author a [NEW] component → it lands in the catalogue as a greenfield row.
        let authored = dispatch_proto_tool(
            &db,
            PROTO_AUTHOR_COMPONENT_TOOL,
            &json!({ "prototype_id": prototype_id, "name": "FancyCard", "template": "<div/>" }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(authored.effects, vec![ChangedTopic::Prototypes]);
        assert_eq!(authored.result["component"]["name"], json!("FancyCard"));
        assert!(
            authored.result["component"]["source_repository_id"].is_null(),
            "a [NEW] component is greenfield (no source repo)"
        );
        assert_eq!(
            worked_prototype_id(PROTO_AUTHOR_COMPONENT_TOOL, &authored.result).as_deref(),
            Some(prototype_id.as_str())
        );

        // It APPEARS in the project catalogue (what /design lists).
        let catalogue = db
            .with_conn(|c| db::list_components(c, &project_id))
            .unwrap();
        assert_eq!(
            catalogue
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["FancyCard"],
            "the [NEW] component is in the catalogue"
        );

        // Re-authoring the SAME name is refused (it is not [NEW] anymore).
        let err = dispatch_proto_tool(
            &db,
            PROTO_AUTHOR_COMPONENT_TOOL,
            &json!({ "prototype_id": prototype_id, "name": "FancyCard" }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);

        // An unknown prototype is an explicit invalid_id.
        let err = dispatch_proto_tool(
            &db,
            PROTO_AUTHOR_COMPONENT_TOOL,
            &json!({ "prototype_id": "nope", "name": "X" }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    /// DONE-CRITERION (audit 01KYHYVD5V78FED8WRZ8NKZVF0), the authoring half: a template that is
    /// PRESENT AND BLANK is refused here too — the same guard the refresh path applies — while an
    /// OMITTED template stays perfectly legal (a `[NEW]` is authored from its params/states alone,
    /// review 01KYHV9F1JXCCKYMYYZR6Y1JYD). "No template" is said by leaving the argument out.
    #[test]
    fn proto_author_component_refuses_a_blank_template_but_accepts_none() {
        let _g = db_guard();
        let (db, spec_id, project_id) = bare_spec();
        let created = dispatch_proto_tool(&db, PROTO_CREATE_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        let prototype_id = created.result["prototype"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        for blank in ["   ", "\n", "\u{00a0}"] {
            let err = dispatch_proto_tool(
                &db,
                PROTO_AUTHOR_COMPONENT_TOOL,
                &json!({ "prototype_id": prototype_id, "name": "Ghost", "template": blank }),
            )
            .expect("routed")
            .unwrap_err();
            assert_eq!(
                err.code,
                RpcCode::InvalidArgument,
                "blank {blank:?} refused"
            );
            assert!(err.message.contains("BLANK"));
        }
        assert!(
            db.with_conn(|c| db::list_components(c, &project_id))
                .unwrap()
                .is_empty(),
            "a refused authoring creates NOTHING",
        );

        // OMITTING the template is the supported way to author a shape-only `[NEW]`.
        let authored = dispatch_proto_tool(
            &db,
            PROTO_AUTHOR_COMPONENT_TOOL,
            &json!({
                "prototype_id": prototype_id,
                "name": "Ghost",
                "params_json": "[{\"name\":\"title\"}]",
            }),
        )
        .expect("routed")
        .expect("a [NEW] without a template is legal");
        assert_eq!(authored.result["component"]["template"], json!(""));
        assert_eq!(
            authored.result["component"]["params_json"],
            json!("[{\"name\":\"title\"}]")
        );
        // A shape-only `[NEW]` has no rendering to judge: the state-case diagnostic stays silent.
        assert_eq!(authored.result["warnings"], json!([]));
    }

    /// DONE-CRITERION (finding 01KYPSG0HPG2D28CMJEXRKNEVK): the `[NEW]` author path refuses an
    /// UNKNOWN param type exactly as the refresh path does — this is precisely where a component is
    /// authored from its `params_json` ALONE, so a `"type": "enum"` used to enter the catalogue and
    /// come back out as a free-text field with its `options` dropped, in silence.
    #[test]
    fn proto_author_component_refuses_an_unknown_param_type() {
        let _g = db_guard();
        let (db, spec_id, project_id) = bare_spec();
        let created = dispatch_proto_tool(&db, PROTO_CREATE_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        let prototype_id = created.result["prototype"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let err = dispatch_proto_tool(
            &db,
            PROTO_AUTHOR_COMPONENT_TOOL,
            &json!({
                "prototype_id": prototype_id,
                "name": "Ghost",
                "params_json": "[{\"name\":\"buttonVariant\",\"type\":\"enum\",\"options\":[\"a\"]}]",
            }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("UNKNOWN type") && err.message.contains("'buttonVariant'"));
        assert!(
            db.with_conn(|c| db::list_components(c, &project_id))
                .unwrap()
                .is_empty(),
            "a refused authoring creates NOTHING",
        );

        // The documented word goes through — the guard is a vocabulary, not a ban on choices.
        dispatch_proto_tool(
            &db,
            PROTO_AUTHOR_COMPONENT_TOOL,
            &json!({
                "prototype_id": prototype_id,
                "name": "Ghost",
                "params_json": "[{\"name\":\"align\",\"type\":\"select\",\"options\":[\"l\",\"r\"]}]",
            }),
        )
        .expect("routed")
        .expect("`select` + options is the documented way to declare a constrained value");
    }

    /// DONE-CRITERION (review #41 — this test's assertion is INVERTED on purpose): authoring a
    /// referenced `[NEW]` after the lock must NOT reclassify it `[DS]`.
    ///
    /// It used to pin the opposite, and the opposite was the bug: `proto_author_component` INSERTS a
    /// catalogue row, and the nature was read off "is the name in the catalogue?", so a component
    /// that had just been PROPOSED came back `[DS]` with an EMPTY `new_components` — the coder read
    /// "it already exists" and never created the task to build it, for the very component the
    /// prototype had asked for. The nature is now the component's ORIGIN: no `source_path` ⇒ not in
    /// the app ⇒ still `[NEW]`, still an anchor, now carrying the authored row as its reference.
    ///
    /// What the original test rightly pinned is KEPT: authoring after the lock is not refused, the
    /// nature is recalculated LIVE at every read, and the frozen ARBRE (`composition_json`) + the
    /// ELECTION (retained variant + locked round) do NOT derive. The `[NEW]`→`[DS]` transition still
    /// exists — it happens when the app really IMPLEMENTS the component (a `source_path` appears),
    /// which is asserted at the end.
    #[test]
    fn authoring_a_referenced_new_component_after_lock_keeps_it_new_until_it_is_materialized() {
        let _g = db_guard();
        let db = Db::in_memory();
        // A prototype whose FROZEN composition references `PricingTable` — a component ABSENT from
        // the catalogue, so it is a `[NEW]` at lock time.
        let (prototype_id, round_id) = db.with_conn(|c| {
            let (project, _root) = db::create_project(c, "Proj", "/tmp/proto-lock", None).unwrap();
            let (spec, _rev) =
                db::create_spec(c, &project.id, "Feature", db::SpecSections::empty()).unwrap();
            let proto = db::create_prototype(c, &spec.id, None).unwrap();
            let round = db::current_round(c, &proto.id).unwrap().unwrap();
            let page = db::create_page(c, &proto.id, "home", "Home", 0).unwrap();
            let version = db::create_page_version(c, &page.id, "v1", None).unwrap();
            db::create_variant(
                c,
                &version.id,
                &db::VariantInput {
                    label: "a".into(),
                    title: "A".into(),
                    composition_json: json!({
                        "component": "PricingTable",
                        "params": { "plan": "pro" }
                    })
                    .to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
            db::set_round_page(c, &round.id, &page.id, &version.id, 0).unwrap();
            (proto.id.clone(), round.id.clone())
        });

        // FREEZE the round → the coder seam exposes the frozen election.
        present(&db, &round_id);
        dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");

        // BEFORE authoring: the node is `[NEW]` (referenced, not in the catalogue).
        let before = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        let before_page = &before.result["composition"]["pages"][0];
        assert_eq!(
            before_page["new_components"],
            json!(["PricingTable"]),
            "PricingTable is a [NEW] candidate before it is authored"
        );
        let before_node = &before_page["nodes"][0];
        assert_eq!(before_node["component"], json!("PricingTable"));
        assert_eq!(before_node["nature"], json!("new"));
        // Snapshot the FROZEN election to prove it does not derive.
        let frozen_composition = before_page["composition_json"].clone();
        let frozen_variant = before_page["elected_variant_id"].clone();
        let frozen_round = before.result["composition"]["locked"]["locked_round_id"].clone();

        // AUTHOR the referenced [NEW] component AFTER the lock — NOT refused (the only guard is the
        // name-not-already-in-catalogue check), the intended way to make a [NEW] buildable.
        dispatch_proto_tool(
            &db,
            PROTO_AUTHOR_COMPONENT_TOOL,
            &json!({
                "prototype_id": prototype_id,
                "name": "PricingTable",
                "template": "<div class=\"pricing\">{{plan}}</div>"
            }),
        )
        .expect("routed")
        .expect("ok");

        // AFTER: re-read the SAME frozen election. The nature is recalculated LIVE — and it stays
        // `[NEW]`: the component was PROPOSED, not BUILT. It keeps its place in new_components (the
        // dev-task anchor) and now carries the authored row as its reference.
        let after = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        let after_page = &after.result["composition"]["pages"][0];
        let after_node = &after_page["nodes"][0];
        assert_eq!(
            after_node["nature"],
            json!("new"),
            "AUTHORING a [NEW] does not build it: it stays [NEW] until the app implements it"
        );
        let authored_ref = after_node["component_ref"]
            .as_str()
            .expect("the authored row IS the spec of what to build — it is referenced")
            .to_string();
        assert_eq!(
            after_page["new_components"],
            json!(["PricingTable"]),
            "PricingTable stays a derived-task anchor — the coder must still build it"
        );

        // The frozen ARBRE + ELECTION do NOT derive (the immutability that matters holds).
        assert_eq!(
            after_page["composition_json"], frozen_composition,
            "the frozen composition_json is unchanged"
        );
        assert_eq!(
            after_page["elected_variant_id"], frozen_variant,
            "the retained variant is unchanged"
        );
        assert_eq!(
            after.result["composition"]["locked"]["locked_round_id"], frozen_round,
            "the same round stays locked (the election never moved)"
        );

        // MATERIALIZED — the coder ships it and the inventory binds its on-disk source. THAT is what
        // flips the node `[NEW]`→`[DS]`, on the SAME row (no duplicate) and the SAME frozen tree.
        db.with_conn(|c| {
            db::bind_component_source(c, &authored_ref, None, "src/ui/PricingTable.tsx")
        })
        .expect("bind the source the coder created")
        .expect("the authored row is still there");
        let built = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        let built_page = &built.result["composition"]["pages"][0];
        assert_eq!(
            built_page["nodes"][0]["nature"],
            json!("ds"),
            "once the component EXISTS in the app, the node is [DS]"
        );
        assert_eq!(built_page["nodes"][0]["component_ref"], json!(authored_ref));
        assert_eq!(built_page["new_components"], json!([]));
        assert_eq!(
            built_page["composition_json"], frozen_composition,
            "the frozen tree never derived through the whole round trip"
        );
    }

    /// DONE-CRITERION (finding `01KYSZPG9Y8QGGVED208SG5BTB`): a component reached ONLY through
    /// `data-nyx-use` reaches the SEAM — transitively — even though it occupies no node of the tree.
    ///
    /// The nature calculation read the composition TREE, and since references exist a component has
    /// a second way of being used. A `Spinner` referenced by a `[NEW]` Card is expanded into the
    /// page's markup and must exist for the Card to work, yet it appeared in no node list: no dev
    /// task was ever derived for it. It gets no [`db::ElectedNode`] (it sits at no position of the
    /// placement, and inventing a `node_key` would be a lie about the tree) — it belongs in
    /// `new_components`, which is what the agent derives tasks FROM.
    #[test]
    fn a_component_reached_only_through_a_reference_reaches_the_seam() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (prototype_id, round_id) = db.with_conn(|c| {
            let (project, _root) = db::create_project(c, "Proj", "/tmp/proto-ref", None).unwrap();
            // The catalogue: a `[NEW]` Card (no source_path) whose TEMPLATE references Spinner,
            // which itself references Icon — and a Spinner/Icon nobody ever places in a tree.
            for (name, template) in [
                (
                    "Card",
                    "<section><i data-nyx-use=\"Spinner\"></i></section>",
                ),
                ("Spinner", "<span data-nyx-use=\"Icon\"></span>"),
                ("Icon", "<svg/>"),
            ] {
                db::upsert_component(
                    c,
                    &project.id,
                    &db::ComponentInput {
                        name: name.into(),
                        template: template.into(),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
            let (spec, _rev) =
                db::create_spec(c, &project.id, "Feature", db::SpecSections::empty()).unwrap();
            let proto = db::create_prototype(c, &spec.id, None).unwrap();
            let round = db::current_round(c, &proto.id).unwrap().unwrap();
            let page = db::create_page(c, &proto.id, "home", "Home", 0).unwrap();
            let version = db::create_page_version(c, &page.id, "v1", None).unwrap();
            db::create_variant(
                c,
                &version.id,
                &db::VariantInput {
                    label: "a".into(),
                    title: "A".into(),
                    // The tree places ONE component. The other two are reached only by reference.
                    composition_json: json!({ "component": "Card" }).to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
            db::set_round_page(c, &round.id, &page.id, &version.id, 0).unwrap();
            (proto.id.clone(), round.id.clone())
        });
        present(&db, &round_id);
        dispatch_proto_tool(
            &db,
            PROTO_DISTILL_TOOL,
            &json!({ "prototype_id": prototype_id, "round_id": round_id }),
        )
        .expect("routed")
        .expect("ok");

        let seam = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        let page = &seam.result["composition"]["pages"][0];
        assert_eq!(
            page["new_components"],
            json!(["Card", "Spinner", "Icon"]),
            "the placement first, then what its template reaches — TRANSITIVELY: the render expands \
             Icon into the page, so the coder has to build it too"
        );
        assert_eq!(
            page["nodes"].as_array().unwrap().len(),
            1,
            "a referenced component occupies NO node of the placement — it is used, not placed"
        );
    }

    /// The CREATION tools are GUARDED with explicit states: `proto_create` on an unknown Spec is
    /// `invalid_id` and on a Spec that ALREADY has a prototype is `invalid_state` (1 proto/Spec);
    /// adding under an unknown parent (prototype / page / version / round) is `invalid_id`; a
    /// duplicate slug / label is `invalid_state`; `proto_open_round` opens a fresh round.
    #[test]
    fn proto_creation_guards_are_explicit() {
        let _g = db_guard();
        let (db, spec_id, _project_id) = bare_spec();

        // Unknown Spec → invalid_id.
        assert_eq!(
            dispatch_proto_tool(&db, PROTO_CREATE_TOOL, &json!({ "spec_id": "nope" }))
                .unwrap()
                .unwrap_err()
                .code,
            RpcCode::InvalidId
        );

        // Create, then a SECOND create on the same Spec → invalid_state (1 proto/Spec).
        let created = dispatch_proto_tool(&db, PROTO_CREATE_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        let prototype_id = created.result["prototype"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            dispatch_proto_tool(&db, PROTO_CREATE_TOOL, &json!({ "spec_id": spec_id }))
                .unwrap()
                .unwrap_err()
                .code,
            RpcCode::InvalidState
        );

        // Unknown parents → invalid_id.
        for call in [
            (
                PROTO_ADD_PAGE_TOOL,
                json!({ "prototype_id": "nope", "slug": "s", "title": "T" }),
            ),
            (
                PROTO_ADD_VERSION_TOOL,
                json!({ "page_id": "nope", "label": "v1" }),
            ),
            (
                PROTO_ADD_VARIANT_TOOL,
                json!({ "page_version_id": "nope", "label": "a", "title": "A" }),
            ),
            (
                PROTO_OPEN_ROUND_TOOL,
                json!({ "prototype_id": "nope", "label": "v2" }),
            ),
            (
                PROTO_PIN_PAGE_TOOL,
                json!({ "round_id": "nope", "page_id": "p", "page_version_id": "v" }),
            ),
        ] {
            assert_eq!(
                dispatch_proto_tool(&db, call.0, &call.1)
                    .unwrap()
                    .unwrap_err()
                    .code,
                RpcCode::InvalidId,
                "{} on an unknown parent is invalid_id",
                call.0
            );
        }

        // A DUPLICATE slug is invalid_state.
        dispatch_proto_tool(
            &db,
            PROTO_ADD_PAGE_TOOL,
            &json!({ "prototype_id": prototype_id, "slug": "home", "title": "Home" }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(
            dispatch_proto_tool(
                &db,
                PROTO_ADD_PAGE_TOOL,
                &json!({ "prototype_id": prototype_id, "slug": "home", "title": "Dup" })
            )
            .unwrap()
            .unwrap_err()
            .code,
            RpcCode::InvalidState
        );

        // proto_open_round opens a fresh, non-empty-labelled round; a duplicate label is invalid_state.
        let opened = dispatch_proto_tool(
            &db,
            PROTO_OPEN_ROUND_TOOL,
            &json!({ "prototype_id": prototype_id, "label": "v2" }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(opened.result["round"]["label"], json!("v2"));
        assert_eq!(
            dispatch_proto_tool(
                &db,
                PROTO_OPEN_ROUND_TOOL,
                &json!({ "prototype_id": prototype_id, "label": "v2" })
            )
            .unwrap()
            .unwrap_err()
            .code,
            RpcCode::InvalidState
        );
    }

    // --- The RESPONSE CAP on a VOLUMINOUS prototype (review 01KYMF0KFZQG5JYZDGZCR85BAM) ---------

    /// A composition tree of roughly `nodes` named components — the real shape (`children` /
    /// `component` / `params`), so what the cap measures is what a real elected placement weighs.
    fn fat_composition(nodes: usize) -> String {
        let children: Vec<Value> = (0..nodes)
            .map(|i| {
                json!({
                    "component": format!("Card{i}"),
                    "params": { "title": format!("Card number {i}"), "body": "x".repeat(64) },
                })
            })
            .collect();
        serde_json::to_string(&json!({ "component": "Page", "children": children }))
            .expect("serialize")
    }

    /// A VOLUMINOUS prototype, DISTILLED: many pages (each with several variants carrying a big
    /// `composition_json`), many rounds, and many long-bodied comments — the scale at which the two
    /// reads used to hand an agent an unreadable response. Returns `(db, spec_id, prototype_id)`.
    fn voluminous() -> (Db, String, String) {
        const PAGES: usize = 24;
        const VARIANTS_PER_PAGE: usize = 3;
        const ROUNDS: usize = 20;
        const COMMENTS: usize = 60;
        let db = Db::in_memory();
        let (spec_id, prototype_id) = db.with_conn(|c| {
            let (project, _root) = db::create_project(c, "Big", "/tmp/proto-big", None).unwrap();
            let (spec, _rev) =
                db::create_spec(c, &project.id, "Fat", db::SpecSections::empty()).unwrap();
            let proto = db::create_prototype(c, &spec.id, Some(&"brief ".repeat(2000))).unwrap();
            // Rounds (rollback points) FIRST — the second growing list of the overview. The pages
            // are then pinned into the LAST one, which is the CURRENT round the overview reports.
            for r in 0..ROUNDS {
                db::create_round(
                    c,
                    &proto.id,
                    &format!("r{r}"),
                    Some(&"summary ".repeat(500)),
                )
                .unwrap();
            }
            let round = db::current_round(c, &proto.id).unwrap().unwrap();
            for p in 0..PAGES {
                let page = db::create_page(
                    c,
                    &proto.id,
                    &format!("page-{p}"),
                    &format!("Page {p}"),
                    p as i32,
                )
                .unwrap();
                let version = db::create_page_version(c, &page.id, "v1", None).unwrap();
                let mut first = None;
                for v in 0..VARIANTS_PER_PAGE {
                    let variant = db::create_variant(
                        c,
                        &version.id,
                        &db::VariantInput {
                            label: format!("var-{v}"),
                            title: format!("Variant {v} of page {p}"),
                            composition_json: fat_composition(40),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    first.get_or_insert(variant.id);
                }
                db::set_round_page(c, &round.id, &page.id, &version.id, p as i32).unwrap();
                db::elect_round_page_variant(
                    c,
                    &round.id,
                    &page.id,
                    &first.unwrap(),
                    Some(&"because ".repeat(1000)),
                    Some("user:k"),
                )
                .unwrap();
            }
            // Comments: half still OPEN (with LONG bodies — the overview's heaviest section), half
            // RESOLVED (the anchors the coder seam exposes).
            for i in 0..COMMENTS {
                let feedback = db::create_feedback(
                    c,
                    &proto.id,
                    &db::FeedbackInput {
                        node_key: Some(format!("Root/Card{i}")),
                        body: "this whole region needs rework. ".repeat(200),
                        author_kind: db::FEEDBACK_AUTHOR_SESSION.into(),
                        ..Default::default()
                    },
                )
                .unwrap();
                if i % 2 == 0 {
                    db::resolve_feedback(c, &feedback.id, Some("done in v2"), None).unwrap();
                }
            }
            // FREEZE the first round as THE design, so the coder seam exposes all 24 pages.
            db::lock_design(c, &proto.id, &round.id, Some("user:k")).unwrap();
            (spec.id, proto.id)
        });
        (db, spec_id, prototype_id)
    }

    /// The serialized size of a tool response — the unit the cap is expressed in.
    fn response_bytes(value: &Value) -> usize {
        serde_json::to_string(value).unwrap().len()
    }

    /// DONE-CRITERION (review 01KYMF0KFZQG5JYZDGZCR85BAM): on a VOLUMINOUS prototype, EVERY response
    /// of the two growing reads stays under the cap — the NAIVE call with no parameter first — and
    /// says what it left out, with the offset to ask for next.
    #[test]
    fn voluminous_prototype_never_answers_past_the_response_cap() {
        let _g = db_guard();
        let (db, spec_id, prototype_id) = voluminous();

        // What the SAME matter weighed with NO cap — the shape these two reads returned before
        // this review. Measured here so the test states the problem it exists for.
        let (uncapped_get, uncapped_composition) = db
            .with_conn(|c| {
                let proto = db::get_prototype_for_spec(c, &spec_id)?.unwrap();
                let round = db::current_round(c, &proto.id)?;
                let get = json!({
                    "prototype": proto,
                    "rounds": db::list_rounds(c, &proto.id)?,
                    "current_round": round,
                    "current_round_manifest": db::list_round_pages(c, &round.unwrap().id)?,
                    "pages": db::list_pages(c, &proto.id)?,
                    "open_feedback": db::list_feedback(c, &proto.id)?,
                });
                let composition = json!(db::elected_composition(c, &proto.id)?);
                Ok::<_, diesel::result::Error>((response_bytes(&get), response_bytes(&composition)))
            })
            .unwrap();
        assert!(
            uncapped_get > 4 * MAX_RESPONSE_BYTES && uncapped_composition > 4 * MAX_RESPONSE_BYTES,
            "the fixture really is the overflowing scale (proto_get {uncapped_get} B, \
             proto_composition {uncapped_composition} B uncapped)",
        );

        // --- proto_get, called NAIVELY (the shape the manual tells an agent to start with). ---
        let naive = dispatch_proto_tool(&db, PROTO_GET_TOOL, &json!({ "spec_id": spec_id }))
            .expect("routed")
            .expect("ok");
        let size = response_bytes(&naive.result);
        assert!(
            size <= MAX_RESPONSE_BYTES,
            "the naive proto_get answered {size} bytes, past the {MAX_RESPONSE_BYTES}-byte cap",
        );
        // It SAYS it is partial, and every section carries its total/returned/next_offset.
        assert_eq!(naive.result["truncated"], json!(true));
        for name in PROTO_GET_SECTIONS {
            let meta = &naive.result["sections"][name];
            assert!(meta["total"].as_u64().unwrap() > 0, "{name} has rows");
            assert!(
                meta["returned"].as_u64().unwrap() <= meta["total"].as_u64().unwrap(),
                "{name} cannot return more than it has",
            );
        }
        // The heavy section (comment BODIES) is the one that gets cut, and it says so.
        assert_eq!(
            naive.result["sections"]["open_feedback"]["truncated"],
            json!(true)
        );

        // --- Every SECTION page: capped, and a traversal following next_offset is EXACT. ---
        for name in PROTO_GET_SECTIONS {
            let total = naive.result["sections"][name]["total"].as_u64().unwrap() as usize;
            let mut seen: Vec<String> = Vec::new();
            let mut offset = 0usize;
            loop {
                let page = dispatch_proto_tool(
                    &db,
                    PROTO_GET_TOOL,
                    &json!({ "spec_id": spec_id, "section": name, "offset": offset }),
                )
                .expect("routed")
                .expect("ok");
                let size = response_bytes(&page.result);
                assert!(
                    size <= MAX_RESPONSE_BYTES,
                    "section {name} at offset {offset} answered {size} bytes, past the cap",
                );
                let items = page.result["items"].as_array().unwrap();
                assert!(!items.is_empty(), "section {name} must always progress");
                for item in items {
                    seen.push(item["id"].as_str().unwrap().to_string());
                }
                match page.result["next_offset"].as_u64() {
                    Some(next) => {
                        assert!(next as usize > offset, "next_offset must progress");
                        offset = next as usize;
                    }
                    None => break,
                }
            }
            assert_eq!(seen.len(), total, "section {name}: the traversal is exact");
            let unique: std::collections::BTreeSet<&String> = seen.iter().collect();
            assert_eq!(unique.len(), total, "section {name}: no row twice");
        }

        // --- proto_composition, called NAIVELY: capped, whole trees only. ---
        let naive = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        let size = response_bytes(&naive.result);
        assert!(
            size <= MAX_RESPONSE_BYTES,
            "the naive proto_composition answered {size} bytes, past the cap",
        );
        assert_eq!(naive.result["truncated"], json!(true));
        assert_eq!(naive.result["page_total"], json!(24));
        assert!(
            naive.result["returned"].as_u64().unwrap() < 24,
            "the budget cut the page list — that is the whole point",
        );

        // --- Paging the composition: EVERY page arrives exactly once, and NO tree is ever cut. ---
        let mut slugs: Vec<String> = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = dispatch_proto_tool(
                &db,
                PROTO_COMPOSITION_TOOL,
                &json!({ "prototype_id": prototype_id, "offset": offset }),
            )
            .expect("routed")
            .expect("ok");
            let size = response_bytes(&page.result);
            assert!(
                size <= MAX_RESPONSE_BYTES,
                "composition at offset {offset} answered {size} bytes, past the cap",
            );
            let pages = page.result["composition"]["pages"].as_array().unwrap();
            assert!(!pages.is_empty(), "the composition paging must progress");
            for elected in pages {
                slugs.push(elected["slug"].as_str().unwrap().to_string());
                // THE INVARIANT: a returned tree is a WHOLE tree — it parses, and it still holds
                // every node it was elected with. A cut placement would be a false contract.
                let tree = elected["composition_json"].as_str().expect("the tree");
                let parsed: Value = serde_json::from_str(tree).expect("a whole, parsable tree");
                assert_eq!(
                    parsed["children"].as_array().unwrap().len(),
                    40,
                    "the elected tree is never trimmed",
                );
            }
            // The pre-dev review crosses as ANCHORS ONLY, at every offset (#47 never regresses).
            for anchor in page.result["composition"]["resolved_feedback"]
                .as_array()
                .unwrap()
            {
                assert!(anchor.get("body").is_none(), "no comment BODY on the seam");
                assert!(anchor.get("resolution_note").is_none(), "no note either");
            }
            match page.result["next_offset"].as_u64() {
                Some(next) => {
                    assert!(next as usize > offset, "next_offset must progress");
                    offset = next as usize;
                }
                None => break,
            }
        }
        assert_eq!(slugs.len(), 24, "every elected page arrived");
        let unique: std::collections::BTreeSet<&String> = slugs.iter().collect();
        assert_eq!(unique.len(), 24, "and none of them twice");

        // --- The TARGETED read: ONE page by slug, the whole budget for its tree. ---
        let one = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id, "page": "page-17" }),
        )
        .expect("routed")
        .expect("ok");
        assert!(response_bytes(&one.result) <= MAX_RESPONSE_BYTES);
        assert_eq!(one.result["mode"], json!("page"));
        assert_eq!(one.result["returned"], json!(1));
        assert_eq!(
            one.result["composition"]["pages"][0]["slug"],
            json!("page-17")
        );
        assert!(
            one.result["next_offset"].is_null(),
            "a targeted read is not a page of a list"
        );
        // An unknown page is an explicit invalid_id naming what IS elected, never an empty list.
        let err = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id, "page": "nope" }),
        )
        .expect("routed")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
        assert!(
            err.message.contains("page-17"),
            "the refusal names the elected pages"
        );

        // --- The review ANCHORS page on their own axis, and never carry prose. ---
        let anchors_total = naive.result["resolved_feedback_total"].as_u64().unwrap();
        assert_eq!(anchors_total, 30, "half the comments were resolved");
        let mut seen = 0u64;
        let mut feedback_offset = 0u64;
        loop {
            let page = dispatch_proto_tool(
                &db,
                PROTO_COMPOSITION_TOOL,
                &json!({ "prototype_id": prototype_id, "feedback_offset": feedback_offset }),
            )
            .expect("routed")
            .expect("ok");
            seen += page.result["resolved_feedback_returned"].as_u64().unwrap();
            match page.result["resolved_feedback_next_offset"].as_u64() {
                Some(next) => feedback_offset = next,
                None => break,
            }
        }
        assert_eq!(seen, anchors_total, "the anchor traversal is exact too");
    }

    /// A SINGLE elected page whose tree exceeds the whole budget is REPORTED, never MUTILATED: the
    /// page comes back in its reduced form (identity + counts + `oversized`), with NO partial tree —
    /// because a truncated `composition_json` would be a FALSE coder contract, worse than the
    /// overflow it avoids.
    #[test]
    fn a_single_oversized_elected_tree_is_reported_never_mutilated() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (prototype_id, real_bytes) = db.with_conn(|c| {
            let (project, _root) = db::create_project(c, "Huge", "/tmp/proto-huge", None).unwrap();
            let (spec, _rev) =
                db::create_spec(c, &project.id, "Huge", db::SpecSections::empty()).unwrap();
            let proto = db::create_prototype(c, &spec.id, None).unwrap();
            let round = db::current_round(c, &proto.id).unwrap().unwrap();
            let page = db::create_page(c, &proto.id, "monster", "Monster", 0).unwrap();
            let version = db::create_page_version(c, &page.id, "v1", None).unwrap();
            // ONE page whose elected tree alone is far past the 64 KiB cap.
            let tree = fat_composition(2000);
            let bytes = tree.len();
            let variant = db::create_variant(
                c,
                &version.id,
                &db::VariantInput {
                    label: "only".into(),
                    title: "Only".into(),
                    composition_json: tree,
                    ..Default::default()
                },
            )
            .unwrap();
            db::set_round_page(c, &round.id, &page.id, &version.id, 0).unwrap();
            db::elect_round_page_variant(c, &round.id, &page.id, &variant.id, None, None).unwrap();
            db::lock_design(c, &proto.id, &round.id, None).unwrap();
            (proto.id, bytes)
        });
        assert!(
            real_bytes > MAX_RESPONSE_BYTES,
            "the fixture really is oversized"
        );

        let read = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id }),
        )
        .expect("routed")
        .expect("ok");
        let size = response_bytes(&read.result);
        assert!(
            size <= MAX_RESPONSE_BYTES,
            "even an oversized page answers under the cap ({size} bytes)",
        );
        assert_eq!(read.result["oversized_pages"], json!(1));
        assert_eq!(read.result["truncated"], json!(true));
        let page = &read.result["composition"]["pages"][0];
        assert_eq!(
            page["slug"],
            json!("monster"),
            "the page is still IDENTIFIED"
        );
        assert_eq!(page["oversized"], json!(true));
        assert!(
            page["composition_json"].is_null(),
            "the tree is ABSENT, never a fragment",
        );
        assert!(page["nodes"].as_array().unwrap().is_empty());
        assert_eq!(
            page["node_count"],
            json!(2001),
            "its real size is still reported (the 2000 cards + their page root)",
        );
        assert!(
            page["composition_bytes"].as_u64().unwrap() as usize > MAX_RESPONSE_BYTES,
            "and so is the tree's real weight",
        );
        // The TARGETED read of that same page says exactly the same thing (no hidden second path).
        let targeted = dispatch_proto_tool(
            &db,
            PROTO_COMPOSITION_TOOL,
            &json!({ "prototype_id": prototype_id, "page": "monster" }),
        )
        .expect("routed")
        .expect("ok");
        assert!(response_bytes(&targeted.result) <= MAX_RESPONSE_BYTES);
        assert_eq!(
            targeted.result["composition"]["pages"][0]["oversized"],
            json!(true)
        );
    }

    /// The GUARD OF LAST RESORT is on the ROUTER, so a tool that never learned to page — here a
    /// creation tool echoing back the huge `composition_json` the agent itself posted — still cannot
    /// answer past the cap. It degrades to its identifying scalars and NAMES what it dropped.
    #[test]
    fn the_router_caps_even_a_tool_that_does_not_paginate() {
        let _g = db_guard();
        let (db, _spec, prototype_id, _round, page_id, _variant) = seeded();
        let version = db
            .with_conn(|c| db::create_page_version(c, &page_id, "v2", None))
            .unwrap();
        let tree = fat_composition(3000);
        assert!(
            tree.len() > MAX_RESPONSE_BYTES,
            "the posted tree is oversized"
        );

        let added = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({
                "page_version_id": version.id,
                "label": "fat",
                "title": "Fat",
                "composition_json": tree,
            }),
        )
        .expect("routed")
        .expect("ok");
        let size = response_bytes(&added.result);
        assert!(
            size <= MAX_RESPONSE_BYTES,
            "the router capped the echo ({size} bytes)",
        );
        assert_eq!(added.result["response_truncated"], json!(true));
        assert!(
            added.result["dropped_fields"]["variant"].as_u64().unwrap() as usize
                > MAX_RESPONSE_BYTES,
            "the dropped field is named with its real weight",
        );
        // The IDENTIFYING scalar survives, so the auto-map still resolves the worked prototype.
        assert_eq!(added.result["prototype_id"], json!(prototype_id));
        assert_eq!(
            worked_prototype_id(PROTO_ADD_VARIANT_TOOL, &added.result).as_deref(),
            Some(prototype_id.as_str()),
        );
        // …and the write itself HAPPENED: the cap is on the ANSWER, never on the act.
        let stored = db.with_conn(|c| db::list_variants(c, &version.id)).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].composition_json.len() > MAX_RESPONSE_BYTES);
    }

    /// DONE-CRITERION (finding `01KYQ0R9414Q5K63TBV9CCMC9J`): the AUTHORING half of the node
    /// contract. `proto_add_variant` — the one tool that WRITES a composition — confronts every node
    /// of the tree with the params AND axes the component it references declares, and ANSWERS with
    /// the mismatch: the reported shape (`text` on a component wanting `buttonLabel` + an axis) used
    /// to be written, stored and rendered EMPTY with no signal at all. It stays a WARNING (the
    /// variant IS created), and a node honouring the contract is silent.
    #[test]
    fn proto_add_variant_confronts_each_nodes_keys_with_the_component_it_references() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (page_version_id, _project_id) = db.with_conn(|c| {
            let (project, _root) =
                db::create_project(c, "Proj", "/tmp/proto-node-contract", None).unwrap();
            let (spec, _rev) =
                db::create_spec(c, &project.id, "Feature", db::SpecSections::empty()).unwrap();
            let proto = db::create_prototype(c, &spec.id, None).unwrap();
            let page = db::create_page(c, &proto.id, "home", "Home", 0).unwrap();
            let version = db::create_page_version(c, &page.id, "v1", None).unwrap();
            // The catalogue component the composition will reference: `buttonLabel` is REQUIRED and
            // `variant` is an AXIS — neither of them is called `text`.
            db::upsert_component(
                c,
                &project.id,
                &db::ComponentInput {
                    name: "Button".into(),
                    template: "<button>{{buttonLabel}}</button>".into(),
                    params_json: Some(
                        r#"[{"name":"buttonLabel","type":"text","required":true}]"#.into(),
                    ),
                    states_json: Some(
                        r#"{"axes":[{"name":"variant","values":[{"name":"ghost","classes":"bg-transparent"}]}]}"#
                            .into(),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
            (version.id, project.id)
        });

        // THE REPORTED SHAPE — a node carrying `text`, and no `buttonLabel`.
        let broken = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({
                "page_version_id": page_version_id,
                "label": "a",
                "title": "A",
                "composition_json": json!({
                    "key": "Root",
                    "children": [
                        { "component": "Button", "key": "Reset", "params": { "text": "Reset filters" } }
                    ]
                }).to_string(),
            }),
        )
        .expect("routed")
        .expect("the variant IS created — this is a warning, never a refusal");
        assert!(
            broken.result["variant"]["id"].is_string(),
            "the composition is stored: nyx signals, it does not refuse"
        );
        let warnings: Vec<String> = broken.result["warnings"]
            .as_array()
            .expect("warnings are exposed at the write")
            .iter()
            .map(|w| w.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(warnings.len(), 2, "TWO distinct defects: {warnings:?}");
        assert!(
            warnings[0].contains("Root/Reset")
                && warnings[0].contains("'Button'")
                && warnings[0].contains("'text'")
                && warnings[0].contains("'buttonLabel'")
                && warnings[0].contains("'variant'"),
            "the unknown key names the node, the component, the key AND what is declared: {}",
            warnings[0]
        );
        assert!(
            warnings[1].contains("Root/Reset")
                && warnings[1].contains("REQUIRED")
                && warnings[1].contains("'buttonLabel'"),
            "…and the unprovided REQUIRED param is a SEPARATE message: {}",
            warnings[1]
        );

        // The corrected composition — every key declared, the required one provided — is SILENT.
        let fixed = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({
                "page_version_id": page_version_id,
                "label": "b",
                "title": "B",
                "composition_json": json!({
                    "key": "Root",
                    "children": [
                        { "component": "Button", "key": "Reset",
                          "params": { "buttonLabel": "Reset filters", "variant": "ghost" } }
                    ]
                }).to_string(),
            }),
        )
        .expect("routed")
        .expect("ok");
        assert_eq!(
            fixed.result["warnings"],
            json!([]),
            "a node honouring the contract says nothing"
        );
    }

    /// A node LINKS to another page of the prototype, by ID — and a link that leads nowhere is
    /// REFUSED at the write (finding `01KYW34347CZSNXCYS35KDG3EY`).
    ///
    /// This is the one place the parcours differs from the node CONTRACT, which only warns: a `[NEW]`
    /// component legitimately does not exist yet, a PAGE never does. `proto_add_page` comes first in
    /// the cycle and hands the id back, so a `navigate_to` that resolves to nothing at the write is
    /// always a mistake — and refusing it here is what makes the render-time marker mean "the page
    /// DISAPPEARED" rather than "this link was never valid".
    #[test]
    fn a_node_links_to_a_page_by_id_and_a_dead_link_is_refused_at_the_write() {
        let _g = db_guard();
        let (db, _spec_id, prototype_id, _round_id, page_id, _variant_id) = seeded();
        let (page_version_id, other_page_id) = db.with_conn(|c| {
            let version = db::list_page_versions(c, &page_id, true).unwrap()[0]
                .id
                .clone();
            let pricing = db::create_page(c, &prototype_id, "pricing", "Tarifs", 1).unwrap();
            (version, pricing.id)
        });

        // (1) THE LINK IS ACCEPTED, and it is the page's ID that is stored — never its slug.
        let linked = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({
                "page_version_id": page_version_id,
                "label": "linked",
                "title": "Linked",
                "composition_json": json!({
                    "key": "Root",
                    "children": [
                        { "component": "Button", "key": "ToPricing", "navigate_to": other_page_id }
                    ]
                }).to_string(),
            }),
        )
        .expect("routed")
        .expect("a link to a real page is written");
        assert!(
            linked.result["variant"]["composition_json"]
                .as_str()
                .expect("the stored tree")
                .contains(&other_page_id),
            "the link SURVIVES in the model, as an id"
        );

        // (2) A LINK TO A PAGE THAT IS NOT THIS PROTOTYPE'S IS REFUSED, and the refusal NAMES the
        // pages that are — a hole is never left for the render to discover.
        let err = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({
                "page_version_id": page_version_id,
                "label": "dead",
                "title": "Dead",
                "composition_json": json!({
                    "component": "Button", "navigate_to": "pg-does-not-exist"
                }).to_string(),
            }),
        )
        .expect("routed")
        .expect_err("a link to nowhere is refused");
        assert_eq!(err.code, RpcCode::InvalidId);
        assert!(
            err.message.contains("pricing") && err.message.contains("home"),
            "the refusal names the real pages: {}",
            err.message
        );

        // (3) A LINK ON A STRUCTURAL NODE is refused too: it has no markup of its own, so nothing
        // could ever be clicked — and stamping its children would make each of them a link nobody
        // asked for.
        let err = dispatch_proto_tool(
            &db,
            PROTO_ADD_VARIANT_TOOL,
            &json!({
                "page_version_id": page_version_id,
                "label": "structural",
                "title": "Structural",
                "composition_json": json!({
                    "key": "Root", "navigate_to": other_page_id,
                    "children": [ { "component": "Button" } ]
                }).to_string(),
            }),
        )
        .expect("routed")
        .expect_err("a structural node cannot carry a link");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("STRUCTURAL"),
            "the refusal says WHY: {}",
            err.message
        );
    }
}
