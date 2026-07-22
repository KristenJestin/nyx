//! Shell-agnostic MCP **IDEAS tools** — capture, search, maintenance (close / drop / merge)
//! and Spec-linking of the maintained backlog (v2.6 — Idées entretenues, Phase 1, task
//! `01KXZ9K207NGKHS45DN1Y8ZAPJ`).
//!
//! # Single mutation authority (no double path)
//!
//! Every tool here is a THIN adapter: it parses the JSON args, then delegates to the
//! `nyx_core::db` idea authority (`create_idea` / `close_idea` / `merge_ideas` /
//! `link_idea_spec` / `unlink_idea_spec` / `list_ideas`) or to the `nyx_core::idea`
//! search+scoring layer (`search_ideas`). The lifecycle vocabulary, the FTS+fuzzy scoring and
//! the merge carry-over all live BELOW the tool, so the MCP mutation is the single authority
//! the native Idées UI (Phase 4) will read from too — no parallel implementation to drift.
//!
//! # The surface (PRD Impl Decisions)
//!
//! `idea_add`, `idea_search`, `idea_get`, `idea_implement` / `idea_close` / `idea_drop`,
//! `idea_merge`, `idea_link` / `idea_unlink`, and a BOUNDED `idea_list` fetch-all fallback.
//! `idea_search` is the star: it returns a top-N SCORED slice (title + extract + score +
//! linked Specs), not a flat dump — the antidote to Depot's write-only cimetière.
//!
//! # Effects
//!
//! Each mutating tool returns [`ChangedTopic::Ideas`] so an open Idées surface re-pulls (the
//! front's re-pull wiring is Phase 4; the topic is named here so the seam is complete).

use diesel::Connection;
use serde_json::{json, Value};

use crate::db::{self, Db};
use crate::idea::{self, DEFAULT_SEARCH_LIMIT};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{internal_db, optional_bool, optional_str, optional_usize, require_str};
use crate::mcp_tools_core::{ChangedTopic, ToolOutcome};

// --- Tool name constants (dispatched below, advertised in `mcp::tool_descriptors`) --------

/// `idea_add` — capture a new idea (optionally seeding theme/source + Spec links).
pub const IDEA_ADD_TOOL: &str = "idea_add";
/// `idea_search` — the scored top-N surfacing (FTS5 + fuzzy).
pub const IDEA_SEARCH_TOOL: &str = "idea_search";
/// `idea_get` — read one idea + its linked Specs.
pub const IDEA_GET_TOOL: &str = "idea_get";
/// `idea_implement` — close an idea as implemented (resolved).
pub const IDEA_IMPLEMENT_TOOL: &str = "idea_implement";
/// `idea_close` — MANUAL fermeture-par-constat: close an idea as `closed` with a motif.
pub const IDEA_CLOSE_TOOL: &str = "idea_close";
/// `idea_drop` — abandon an idea.
pub const IDEA_DROP_TOOL: &str = "idea_drop";
/// `idea_reopen` — return a TERMINAL idea to `open` (clearing its fermeture trail).
pub const IDEA_REOPEN_TOOL: &str = "idea_reopen";
/// `idea_merge` — fold duplicate ideas into a target (points + Spec links preserved).
pub const IDEA_MERGE_TOOL: &str = "idea_merge";
/// `idea_link` — link an idea to a Spec with a kind (addresses | related).
pub const IDEA_LINK_TOOL: &str = "idea_link";
/// `idea_unlink` — remove an idée↔Spec link.
pub const IDEA_UNLINK_TOOL: &str = "idea_unlink";
/// `idea_list` — the BOUNDED/paginated fetch-all fallback (the "secours").
pub const IDEA_LIST_TOOL: &str = "idea_list";
/// `idea_themes` — the by-theme grouping with per-theme counts + status breakdown.
pub const IDEA_THEMES_TOOL: &str = "idea_themes";

/// The idea tool names — routed by [`dispatch_idea_tool`]. A closed slice so a new tool is
/// added in exactly one place.
pub const IDEA_TOOLS: [&str; 12] = [
    IDEA_ADD_TOOL,
    IDEA_SEARCH_TOOL,
    IDEA_GET_TOOL,
    IDEA_IMPLEMENT_TOOL,
    IDEA_CLOSE_TOOL,
    IDEA_DROP_TOOL,
    IDEA_REOPEN_TOOL,
    IDEA_MERGE_TOOL,
    IDEA_LINK_TOOL,
    IDEA_UNLINK_TOOL,
    IDEA_LIST_TOOL,
    IDEA_THEMES_TOOL,
];

/// Upper bound on a single `idea_list` page (the fetch-all fallback stays BOUNDED so it can
/// never re-create Depot's unbounded flat list). Default page when `limit` is omitted.
const LIST_MAX_LIMIT: usize = 200;
const LIST_DEFAULT_LIMIT: usize = 50;

// --- Argument helpers ------------------------------------------------------

/// An OPTIONAL array-of-strings argument as an ordered `Vec` (absent/null → empty). A
/// non-array value, or a non-string element, → `invalid_argument`.
fn optional_str_array(args: &Value, key: &str) -> Result<Vec<String>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for el in arr {
                match el.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(RpcError::new(
                            RpcCode::InvalidArgument,
                            format!("argument '{key}' must be an array of strings"),
                        ))
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be an array of strings"),
        )),
    }
}

/// Resolve the link-`kind` arg to the vocabulary (`addresses` | `related`), defaulting to
/// `addresses` (the resolution/promotion link) when absent. An unknown value → `invalid_argument`.
fn parse_kind(args: &Value) -> Result<&'static str, RpcError> {
    match optional_str(args, "kind")? {
        None => Ok(db::IDEA_KIND_ADDRESSES),
        Some("addresses") => Ok(db::IDEA_KIND_ADDRESSES),
        Some("related") => Ok(db::IDEA_KIND_RELATED),
        Some(other) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown kind '{other}' (accepted: addresses, related)"),
        )),
    }
}

/// Resolve the capture-`source` arg to the vocabulary (`session` | `spec` | `manual`),
/// defaulting to `session` (the agent-capture path). An unknown value → `invalid_argument`.
fn parse_source(args: &Value) -> Result<&'static str, RpcError> {
    match optional_str(args, "source")? {
        None => Ok(db::IDEA_SOURCE_SESSION),
        Some("session") => Ok(db::IDEA_SOURCE_SESSION),
        Some("spec") => Ok(db::IDEA_SOURCE_SPEC),
        Some("manual") => Ok(db::IDEA_SOURCE_MANUAL),
        Some(other) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown source '{other}' (accepted: session, spec, manual)"),
        )),
    }
}

/// The `invalid_id` an unknown idea id maps to.
fn unknown_idea(id: &str) -> RpcError {
    RpcError::new(RpcCode::InvalidId, format!("unknown idea {id}"))
}

// --- Tool handlers ---------------------------------------------------------

/// `idea_add` — `{ project_id, title, body?, theme?, source?, spec_ids? }` → `{ idea,
/// linked_specs }`. Captures an `open` idea and, when `spec_ids` are given, links each as
/// `addresses`. The Spec existence is VALIDATED up front (a bad id → `invalid_id`), and the
/// create+links run in ONE transaction so a half-linked idea is never persisted.
pub fn idea_add(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let title = require_str(args, "title")?;
    let body = optional_str(args, "body")?.unwrap_or("");
    let theme = optional_str(args, "theme")?;
    let source = parse_source(args)?;
    let spec_ids = optional_str_array(args, "spec_ids")?;

    // Validate the Spec links up front so a bad id is a clean invalid_id (not a FK failure).
    for spec_id in &spec_ids {
        if db
            .with_conn(|c| db::get_spec(c, spec_id))
            .map_err(internal_db)?
            .is_none()
        {
            return Err(RpcError::new(
                RpcCode::InvalidId,
                format!("unknown spec {spec_id}"),
            ));
        }
    }

    let (idea, linked_specs) = db
        .with_conn(|c| {
            c.transaction::<_, diesel::result::Error, _>(|c| {
                let idea = db::create_idea(c, project_id, title, body, theme, source)?;
                for spec_id in &spec_ids {
                    db::link_idea_spec(c, &idea.id, spec_id, db::IDEA_KIND_ADDRESSES)?;
                    // Reconcile the fresh idea's status with the linked Spec's CURRENT state (a
                    // Spec already in_progress ⇒ planned, already completed ⇒ implemented) — so
                    // `planned` is reachable by linking to an already-active Spec, not only at the
                    // instant the Spec transitions (finding 01KY4S0FM3EFQNX0V0NHEPGPRR).
                    db::reconcile_idea_status_to_spec(c, &idea.id, spec_id)?;
                }
                // Re-read so the returned idea reflects any reconciled status/fermeture trail.
                let idea = db::get_idea(c, &idea.id)?.unwrap_or(idea);
                let links = db::linked_specs_for_idea(c, &idea.id)?;
                Ok((idea, links))
            })
        })
        .map_err(internal_db)?;

    Ok(ToolOutcome::changed(
        json!({ "idea": idea, "linked_specs": linked_specs }),
        ChangedTopic::Ideas,
    ))
}

/// `idea_search` — `{ project_id, query, theme?, status?, limit? }` → `{ results }`. The
/// scored top-N surfacing (FTS5 + fuzzy trigram): each result carries the idea's title, a
/// match EXTRACT, the blended SCORE, and its LINKED Specs. `limit` is clamped to the module
/// ceiling. Read-only.
pub fn idea_search(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let query = require_str(args, "query")?;
    let theme = optional_str(args, "theme")?;
    let status = optional_str(args, "status")?;
    let limit = optional_usize(args, "limit")?.unwrap_or(DEFAULT_SEARCH_LIMIT);

    let results = db
        .with_conn(|c| idea::search_ideas(c, project_id, query, theme, status, limit))
        .map_err(internal_db)?;
    Ok(ToolOutcome::read(json!({ "results": results })))
}

/// `idea_get` — `{ id }` → `{ idea, linked_specs }`. Reads one idea + its linked Specs.
/// Unknown id → `invalid_id`. Read-only.
pub fn idea_get(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let id = require_str(args, "id")?;
    let idea = db
        .with_conn(|c| db::get_idea(c, id))
        .map_err(internal_db)?
        .ok_or_else(|| unknown_idea(id))?;
    let linked_specs = db
        .with_conn(|c| db::linked_specs_for_idea(c, id))
        .map_err(internal_db)?;
    Ok(ToolOutcome::read(
        json!({ "idea": idea, "linked_specs": linked_specs }),
    ))
}

/// Shared GUARDED terminal-close body for `idea_implement` / `idea_close` / `idea_drop`: refuses to
/// move an ALREADY-terminal idea (`invalid_state`) so a resolved/closed/dropped idea is never
/// silently resurrected NOR its recorded `closed_reason` overwritten — the explicit `idea_reopen`
/// is the one way back to the backlog. Unknown id → `invalid_id`.
fn close_to(
    db: &Db,
    id: &str,
    status: &str,
    reason: Option<&str>,
) -> Result<ToolOutcome, RpcError> {
    let outcome = db
        .with_conn(|c| db::close_idea_if_live(c, id, status, reason))
        .map_err(internal_db)?;
    match outcome {
        db::TerminalTransition::Closed(idea) => Ok(ToolOutcome::changed(
            json!({ "idea": idea }),
            ChangedTopic::Ideas,
        )),
        db::TerminalTransition::Unknown => Err(unknown_idea(id)),
        db::TerminalTransition::AlreadyTerminal(idea) => Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "idea {id} is already {} (terminal) — use idea_reopen to return it to the backlog",
                idea.status
            ),
        )),
    }
}

/// `idea_implement` — `{ id }` → `{ idea }`. Closes the idea as `implemented` (the AUTOMATIC-style
/// resolution — the same terminal a completed linked Spec drives). No motif (that is `idea_close`).
/// Refused (`invalid_state`) if the idea is already terminal. Unknown id → `invalid_id`.
pub fn idea_implement(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let id = require_str(args, "id")?;
    close_to(db, id, db::IDEA_STATUS_IMPLEMENTED, None)
}

/// `idea_close` — `{ id, reason }` → `{ idea }`. The MANUAL fermeture-par-constat: close the idea as
/// `closed` (a terminal DISTINCT from `implemented`) with a `reason` motif — you found it was
/// already covered / no longer relevant. Refused (`invalid_state`) if already terminal (so the
/// motif of an existing closure is never overwritten). Unknown id → `invalid_id`.
pub fn idea_close(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let id = require_str(args, "id")?;
    let reason = require_str(args, "reason")?;
    close_to(db, id, db::IDEA_STATUS_CLOSED, Some(reason))
}

/// `idea_drop` — `{ id }` → `{ idea }`. Abandons the idea (`dropped`). Refused (`invalid_state`) if
/// already terminal. Unknown id → `invalid_id`.
pub fn idea_drop(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let id = require_str(args, "id")?;
    close_to(db, id, db::IDEA_STATUS_DROPPED, None)
}

/// `idea_reopen` — `{ id }` → `{ idea }`. Returns a TERMINAL idea (`implemented`/`closed`/`dropped`)
/// to `open`, CLEARING its fermeture trail (`closed_at`/`closed_reason` → null) — the explicit
/// un-terminal path (no more "implement is a hidden un-drop"). A LIVE idea is returned unchanged.
/// Unknown id → `invalid_id`.
pub fn idea_reopen(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let id = require_str(args, "id")?;
    let idea = db
        .with_conn(|c| db::reopen_idea(c, id))
        .map_err(internal_db)?
        .ok_or_else(|| unknown_idea(id))?;
    Ok(ToolOutcome::changed(
        json!({ "idea": idea }),
        ChangedTopic::Ideas,
    ))
}

/// `idea_merge` — `{ source_ids, target_id }` → `{ idea, linked_specs }`. Folds the sources
/// into the target — points (bodies) preserved, Spec links carried over, sources retired as
/// dropped tombstones. Unknown target → `invalid_id`; unknown/self sources are skipped.
pub fn idea_merge(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let source_ids = optional_str_array(args, "source_ids")?;
    if source_ids.is_empty() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "argument 'source_ids' must be a non-empty array of idea ids to merge",
        ));
    }
    let target_id = require_str(args, "target_id")?;

    let idea = db
        .with_conn(|c| db::merge_ideas(c, &source_ids, target_id))
        .map_err(internal_db)?
        .ok_or_else(|| unknown_idea(target_id))?;
    let linked_specs = db
        .with_conn(|c| db::linked_specs_for_idea(c, target_id))
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "idea": idea, "linked_specs": linked_specs }),
        ChangedTopic::Ideas,
    ))
}

/// `idea_link` — `{ idea_id, spec_id, kind? }` → `{ link }`. Links an idea to a Spec with a
/// `kind` (`addresses`, default | `related`) — an UPSERT (re-linking updates the kind). Both
/// ids are VALIDATED (a bad id → `invalid_id`).
pub fn idea_link(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let idea_id = require_str(args, "idea_id")?;
    let spec_id = require_str(args, "spec_id")?;
    let kind = parse_kind(args)?;

    if db
        .with_conn(|c| db::get_idea(c, idea_id))
        .map_err(internal_db)?
        .is_none()
    {
        return Err(unknown_idea(idea_id));
    }
    if db
        .with_conn(|c| db::get_spec(c, spec_id))
        .map_err(internal_db)?
        .is_none()
    {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!("unknown spec {spec_id}"),
        ));
    }

    let link = db
        .with_conn(|c| {
            c.transaction::<_, diesel::result::Error, _>(|c| {
                let link = db::link_idea_spec(c, idea_id, spec_id, kind)?;
                // On an `addresses` link, reconcile the idea's status with the Spec's CURRENT
                // state (in_progress ⇒ planned, completed ⇒ implemented+motif) — the same rule as
                // the transition automation, so linking to an ALREADY-active Spec is not a dead
                // end (finding 01KY4S0FM3EFQNX0V0NHEPGPRR). `related` links reconcile nothing.
                if kind == db::IDEA_KIND_ADDRESSES {
                    db::reconcile_idea_status_to_spec(c, idea_id, spec_id)?;
                }
                Ok(link)
            })
        })
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "link": link }),
        ChangedTopic::Ideas,
    ))
}

/// `idea_unlink` — `{ idea_id, spec_id }` → `{ unlinked }`. Removes an idée↔Spec link
/// (idempotent: `unlinked` is `false` when the pair was not linked).
pub fn idea_unlink(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let idea_id = require_str(args, "idea_id")?;
    let spec_id = require_str(args, "spec_id")?;
    let removed = db
        .with_conn(|c| db::unlink_idea_spec(c, idea_id, spec_id))
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "unlinked": removed > 0, "idea_id": idea_id, "spec_id": spec_id }),
        ChangedTopic::Ideas,
    ))
}

/// `idea_list` — `{ project_id, status?, include_terminal?, theme?, limit?, offset? }` → `{ ideas,
/// limit, offset }`. The BOUNDED/paginated fetch-all fallback (the "secours"): `idea_search` is the
/// primary read; this is the raw dump, always capped ([`LIST_MAX_LIMIT`]). By DEFAULT it lists only
/// the ACTIVE backlog (`open`+`planned`) so the actionable ideas are not buried under the
/// resolved/abandoned tail; pass `include_terminal: true` (or `status: "all"`) to include
/// `implemented`/`closed`/`dropped`, or a concrete `status` to narrow to exactly one (finding
/// 01KY4S0JCT7E7MMVT80Q1BSKE1). Read-only.
pub fn idea_list(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let status = optional_str(args, "status")?;
    let include_terminal = optional_bool(args, "include_terminal")?.unwrap_or(false);
    let theme = optional_str(args, "theme")?;
    let limit = optional_usize(args, "limit")?
        .unwrap_or(LIST_DEFAULT_LIMIT)
        .clamp(1, LIST_MAX_LIMIT);
    let offset = optional_usize(args, "offset")?.unwrap_or(0);

    // An explicit `status` wins (the "all"/"active" shortcuts, or a concrete status honoured even
    // when terminal); with no `status`, default to the ACTIVE backlog unless include_terminal.
    let filter = match status {
        Some("all") => db::IdeaStatusFilter::All,
        Some("active") => db::IdeaStatusFilter::Active,
        Some(s) => db::IdeaStatusFilter::One(s),
        None if include_terminal => db::IdeaStatusFilter::All,
        None => db::IdeaStatusFilter::Active,
    };

    let ideas = db
        .with_conn(|c| db::list_ideas(c, project_id, filter, theme, limit as i64, offset as i64))
        .map_err(internal_db)?;
    Ok(ToolOutcome::read(
        json!({ "ideas": ideas, "limit": limit, "offset": offset }),
    ))
}

/// `idea_themes` — `{ project_id, include_terminal? }` → `{ themes }`. The by-THEME grouping of a
/// project's backlog: one entry per theme (ungrouped ideas fold into a single `theme = null`
/// group), each with the total count and the per-status breakdown
/// (`open`/`planned`/`implemented`/`closed`/`dropped`), biggest group first. By DEFAULT the counts
/// reflect only the ACTIVE backlog (`open`+`planned`) — a group's `total` matches its actionable
/// ideas and a wholly-terminal theme drops out; pass `include_terminal: true` for the full counts
/// across every status (finding 01KY4S0JCT7E7MMVT80Q1BSKE1). Read-only.
pub fn idea_themes(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let include_terminal = optional_bool(args, "include_terminal")?.unwrap_or(false);
    let themes = db
        .with_conn(|c| db::list_idea_themes(c, project_id, include_terminal))
        .map_err(internal_db)?;
    Ok(ToolOutcome::read(json!({ "themes": themes })))
}

// --- Dispatch --------------------------------------------------------------

/// Route an IDEAS tool call, or `None` when `name` is not one of ours. Called from
/// [`crate::mcp_tools_core::dispatch_extension_tool`] so the idea tools share the SAME dispatch
/// entry point (one dispatcher, one pool) as every other extension tool.
pub fn dispatch_idea_tool(
    db: &Db,
    name: &str,
    args: &Value,
) -> Option<Result<ToolOutcome, RpcError>> {
    match name {
        IDEA_ADD_TOOL => Some(idea_add(db, args)),
        IDEA_SEARCH_TOOL => Some(idea_search(db, args)),
        IDEA_GET_TOOL => Some(idea_get(db, args)),
        IDEA_IMPLEMENT_TOOL => Some(idea_implement(db, args)),
        IDEA_CLOSE_TOOL => Some(idea_close(db, args)),
        IDEA_DROP_TOOL => Some(idea_drop(db, args)),
        IDEA_REOPEN_TOOL => Some(idea_reopen(db, args)),
        IDEA_MERGE_TOOL => Some(idea_merge(db, args)),
        IDEA_LINK_TOOL => Some(idea_link(db, args)),
        IDEA_UNLINK_TOOL => Some(idea_unlink(db, args)),
        IDEA_LIST_TOOL => Some(idea_list(db, args)),
        IDEA_THEMES_TOOL => Some(idea_themes(db, args)),
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

    /// A memory DB seeded with one project + one Spec. Returns `(db, project_id, spec_id)`.
    fn seeded() -> (Db, String, String) {
        let db = Db::in_memory();
        let (project_id, spec_id) = db.with_conn(|c| {
            let (project, _root) = db::create_project(c, "Proj", "/tmp/idea-mcp", None).unwrap();
            let (spec, _rev) =
                db::create_spec(c, &project.id, "Auth Spec", db::SpecSections::empty()).unwrap();
            (project.id, spec.id)
        });
        (db, project_id, spec_id)
    }

    fn idea_id_of(out: &ToolOutcome) -> String {
        out.result["idea"]["id"].as_str().unwrap().to_string()
    }

    /// `idea_add` captures an idea, seeds its Spec links (validated), and reports a change on
    /// the Ideas topic; a bad spec_id is a clean invalid_id.
    #[test]
    fn idea_add_captures_and_links_and_changes_ideas() {
        let _g = db_guard();
        let (db, project_id, spec_id) = seeded();
        let out = idea_add(
            &db,
            &json!({
                "project_id": project_id,
                "title": "Rework login",
                "body": "the empty-password branch is fragile",
                "theme": "auth",
                "spec_ids": [spec_id],
            }),
        )
        .unwrap();
        assert_eq!(out.effects, vec![ChangedTopic::Ideas]);
        assert_eq!(out.result["idea"]["status"], "open");
        assert_eq!(out.result["idea"]["theme"], "auth");
        assert_eq!(out.result["linked_specs"].as_array().unwrap().len(), 1);
        assert_eq!(out.result["linked_specs"][0]["kind"], "addresses");

        // A bad spec_id → invalid_id, and nothing is persisted (validated up front).
        let err = idea_add(
            &db,
            &json!({ "project_id": project_id, "title": "x", "spec_ids": ["nope"] }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    /// DONE-CRITERION: `idea_search` returns a top-N SCORED (extract + score + linked Specs),
    /// blending FTS5 and fuzzy, through the real dispatch.
    #[test]
    fn idea_search_returns_scored_top_n_with_specs_through_dispatch() {
        let _g = db_guard();
        let (db, project_id, spec_id) = seeded();
        let added = idea_add(
            &db,
            &json!({
                "project_id": project_id,
                "title": "Authentication rework",
                "body": "harden the login path",
                "spec_ids": [spec_id],
            }),
        )
        .unwrap();
        let idea_id = idea_id_of(&added);
        idea_add(
            &db,
            &json!({ "project_id": project_id, "title": "Dark mode", "body": "a theme toggle" }),
        )
        .unwrap();

        let out = dispatch_idea_tool(
            &db,
            IDEA_SEARCH_TOOL,
            &json!({ "project_id": project_id, "query": "authentication", "limit": 5 }),
        )
        .expect("idea_search routed")
        .expect("idea_search ok");
        assert!(out.effects.is_empty(), "search is a read");
        let results = out.result["results"].as_array().unwrap();
        assert_eq!(
            results.len(),
            1,
            "only the relevant idea, not the unrelated one"
        );
        assert_eq!(results[0]["id"], idea_id);
        assert!(
            results[0]["score"].as_f64().unwrap() > 0.0,
            "carries a score"
        );
        assert!(
            !results[0]["extract"].as_str().unwrap().is_empty(),
            "carries an extract"
        );
        assert_eq!(
            results[0]["linked_specs"][0]["spec_id"], spec_id,
            "carries linked Specs"
        );
    }

    /// The maintenance transitions through the tools: implement → `implemented`, close(reason) →
    /// the DISTINCT `closed` (manual fermeture) with a motif, drop → `dropped`; an unknown id is
    /// invalid_id.
    #[test]
    fn implement_close_drop_transitions_through_the_tools() {
        let _g = db_guard();
        let (db, project_id, _spec) = seeded();
        let mk = |title: &str| {
            idea_id_of(
                &idea_add(&db, &json!({ "project_id": project_id, "title": title })).unwrap(),
            )
        };

        let a = mk("A");
        let implemented = idea_implement(&db, &json!({ "id": a })).unwrap();
        assert_eq!(implemented.result["idea"]["status"], "implemented");
        assert!(!implemented.result["idea"]["closed_at"].is_null());

        let b = mk("B");
        let closed = idea_close(&db, &json!({ "id": b, "reason": "handled inline" })).unwrap();
        assert_eq!(
            closed.result["idea"]["status"], "closed",
            "manual close → the DISTINCT `closed` status, not `implemented`"
        );
        assert_eq!(closed.result["idea"]["closed_reason"], "handled inline");

        let c = mk("C");
        let dropped = idea_drop(&db, &json!({ "id": c })).unwrap();
        assert_eq!(dropped.result["idea"]["status"], "dropped");

        let err = idea_implement(&db, &json!({ "id": "nope" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    /// TERMINAL GUARDS + reopen (findings 01KY4S0G5GF4RV96YZF5HW4QZQ +
    /// 01KY4S0GQJS54GYY4DAFAQQDMP): implement/close/drop REFUSE a transition out of a terminal
    /// state (invalid_state), so a dropped idea can't be resurrected AND a recorded close motif is
    /// never overwritten; `idea_reopen` is the one explicit way back to `open`.
    #[test]
    fn terminal_guards_reject_and_reopen_restores() {
        let _g = db_guard();
        let (db, project_id, _spec) = seeded();
        let mk = |title: &str| {
            idea_id_of(
                &idea_add(&db, &json!({ "project_id": project_id, "title": title })).unwrap(),
            )
        };

        // A dropped idea is NOT silently resurrected by implement/close/drop.
        let d = mk("Dropped");
        idea_drop(&db, &json!({ "id": d })).unwrap();
        for again in [
            idea_implement(&db, &json!({ "id": d })),
            idea_close(&db, &json!({ "id": d, "reason": "x" })),
            idea_drop(&db, &json!({ "id": d })),
        ] {
            assert_eq!(
                again.unwrap_err().code,
                RpcCode::InvalidState,
                "a terminal idea rejects a further transition (invalid_state)"
            );
        }
        // The dropped status is intact (no resurrection).
        assert_eq!(
            db.with_conn(|c| db::get_idea(c, &d))
                .unwrap()
                .unwrap()
                .status,
            "dropped"
        );

        // A close motif is never lost: closing with a reason then any re-close fails, motif kept.
        let m = mk("Closed with motif");
        idea_close(&db, &json!({ "id": m, "reason": "already covered by #12" })).unwrap();
        assert_eq!(
            idea_implement(&db, &json!({ "id": m })).unwrap_err().code,
            RpcCode::InvalidState
        );
        let still = db.with_conn(|c| db::get_idea(c, &m)).unwrap().unwrap();
        assert_eq!(still.status, "closed");
        assert_eq!(
            still.closed_reason.as_deref(),
            Some("already covered by #12"),
            "the close motif is preserved (never overwritten by null)"
        );

        // idea_reopen brings a terminal idea back to `open`, clearing the fermeture trail.
        let reopened = idea_reopen(&db, &json!({ "id": m })).unwrap();
        assert_eq!(reopened.effects, vec![ChangedTopic::Ideas]);
        assert_eq!(reopened.result["idea"]["status"], "open");
        assert!(
            reopened.result["idea"]["closed_at"].is_null()
                && reopened.result["idea"]["closed_reason"].is_null(),
            "reopen clears closed_at/closed_reason"
        );
        // Reopening is idempotent-ish on a live idea (returns it unchanged, still open).
        assert_eq!(
            idea_reopen(&db, &json!({ "id": m })).unwrap().result["idea"]["status"],
            "open"
        );
        // Unknown id → invalid_id.
        assert_eq!(
            idea_reopen(&db, &json!({ "id": "nope" })).unwrap_err().code,
            RpcCode::InvalidId
        );
    }

    /// `idea_link` (kind) upserts + validates ids; `idea_unlink` is idempotent; `idea_merge`
    /// folds sources into the target (points + links) through the tools.
    #[test]
    fn link_kind_unlink_and_merge_through_the_tools() {
        let _g = db_guard();
        let (db, project_id, spec_id) = seeded();
        let target = idea_id_of(
            &idea_add(
                &db,
                &json!({ "project_id": project_id, "title": "Faster search", "body": "one" }),
            )
            .unwrap(),
        );
        let source = idea_id_of(
            &idea_add(
                &db,
                &json!({ "project_id": project_id, "title": "Speed up search", "body": "two" }),
            )
            .unwrap(),
        );

        // link with an explicit kind, then upsert it.
        let linked = idea_link(
            &db,
            &json!({ "idea_id": source, "spec_id": spec_id, "kind": "related" }),
        )
        .unwrap();
        assert_eq!(linked.result["link"]["kind"], "related");
        let relinked = idea_link(
            &db,
            &json!({ "idea_id": source, "spec_id": spec_id, "kind": "addresses" }),
        )
        .unwrap();
        assert_eq!(
            relinked.result["link"]["kind"], "addresses",
            "re-link upserts the kind"
        );

        // a bad kind is rejected; a bad id is invalid_id.
        assert_eq!(
            idea_link(
                &db,
                &json!({ "idea_id": source, "spec_id": spec_id, "kind": "bogus" })
            )
            .unwrap_err()
            .code,
            RpcCode::InvalidArgument
        );
        assert_eq!(
            idea_link(&db, &json!({ "idea_id": "nope", "spec_id": spec_id }))
                .unwrap_err()
                .code,
            RpcCode::InvalidId
        );

        // MERGE source → target: points preserved + links carried over; source dropped.
        let merged =
            idea_merge(&db, &json!({ "source_ids": [source], "target_id": target })).unwrap();
        assert_eq!(merged.effects, vec![ChangedTopic::Ideas]);
        let body = merged.result["idea"]["body"].as_str().unwrap();
        assert!(
            body.contains("one") && body.contains("two"),
            "points preserved"
        );
        assert_eq!(
            merged.result["linked_specs"].as_array().unwrap().len(),
            1,
            "source's link carried over"
        );
        let src = db.with_conn(|c| db::get_idea(c, &source)).unwrap().unwrap();
        assert_eq!(src.status, "dropped", "source retired as a merge tombstone");

        // unlink is idempotent.
        let un = idea_unlink(&db, &json!({ "idea_id": target, "spec_id": spec_id })).unwrap();
        assert_eq!(un.result["unlinked"], true);
        let un2 = idea_unlink(&db, &json!({ "idea_id": target, "spec_id": spec_id })).unwrap();
        assert_eq!(un2.result["unlinked"], false, "already unlinked → no-op");

        // merge with an unknown target → invalid_id.
        assert_eq!(
            idea_merge(&db, &json!({ "source_ids": [source], "target_id": "nope" }))
                .unwrap_err()
                .code,
            RpcCode::InvalidId
        );
    }

    /// `idea_list` is the BOUNDED fetch-all fallback: it filters + caps, never dumping unbounded.
    #[test]
    fn idea_list_is_bounded_and_filters() {
        let _g = db_guard();
        let (db, project_id, _spec) = seeded();
        for i in 0..4 {
            idea_add(
                &db,
                &json!({ "project_id": project_id, "title": format!("idea {i}"), "theme": "t" }),
            )
            .unwrap();
        }
        idea_add(
            &db,
            &json!({ "project_id": project_id, "title": "other", "theme": "u" }),
        )
        .unwrap();

        let all = idea_list(&db, &json!({ "project_id": project_id })).unwrap();
        assert!(all.effects.is_empty(), "list is a read");
        assert_eq!(all.result["ideas"].as_array().unwrap().len(), 5);
        // theme filter narrows it.
        let t = idea_list(&db, &json!({ "project_id": project_id, "theme": "t" })).unwrap();
        assert_eq!(t.result["ideas"].as_array().unwrap().len(), 4);
        // the page cap bounds the dump.
        let capped = idea_list(&db, &json!({ "project_id": project_id, "limit": 2 })).unwrap();
        assert_eq!(capped.result["ideas"].as_array().unwrap().len(), 2);
        assert_eq!(capped.result["limit"], 2);
    }

    /// DONE-CRITERION ("regroupement par thème avec compteurs"): `idea_themes` returns the
    /// by-theme grouping with a total + per-status breakdown per theme, through the real dispatch.
    /// Also the DEFAULT active-only scope + the `include_terminal` opt-in (finding
    /// 01KY4S0JCT7E7MMVT80Q1BSKE1) and the DISTINCT `closed` status.
    #[test]
    fn idea_themes_groups_with_counts_through_dispatch() {
        let _g = db_guard();
        let (db, project_id, _spec) = seeded();
        // Two ideas in "auth" (one manually CLOSED with a motif), one in "ui".
        let a = idea_add(
            &db,
            &json!({ "project_id": project_id, "title": "a1", "theme": "auth" }),
        )
        .unwrap();
        idea_add(
            &db,
            &json!({ "project_id": project_id, "title": "a2", "theme": "auth" }),
        )
        .unwrap();
        idea_add(
            &db,
            &json!({ "project_id": project_id, "title": "u1", "theme": "ui" }),
        )
        .unwrap();
        // Manual close-with-motif (fermeture par constat) → the DISTINCT `closed` status.
        idea_close(
            &db,
            &json!({ "id": idea_id_of(&a), "reason": "handled inline" }),
        )
        .unwrap();

        // DEFAULT (active-only): only the live auth idea counts; the closed one is excluded, so
        // auth.total = 1, and "ui" (still open) shows too.
        let out = dispatch_idea_tool(&db, IDEA_THEMES_TOOL, &json!({ "project_id": project_id }))
            .expect("idea_themes routed")
            .expect("idea_themes ok");
        assert!(out.effects.is_empty(), "themes listing is a read");
        let themes = out.result["themes"].as_array().unwrap();
        assert_eq!(themes.len(), 2, "auth + ui, both with a live idea");
        let auth = themes.iter().find(|t| t["theme"] == "auth").unwrap();
        assert_eq!(auth["total"], 1, "active default excludes the closed idea");
        assert_eq!(auth["open"], 1);
        assert_eq!(
            auth["closed"], 0,
            "closed idea not counted in the active scope"
        );

        // include_terminal: the FULL breakdown — auth.total = 2 with a `closed` bucket of 1.
        let full = dispatch_idea_tool(
            &db,
            IDEA_THEMES_TOOL,
            &json!({ "project_id": project_id, "include_terminal": true }),
        )
        .expect("idea_themes routed")
        .expect("idea_themes ok");
        let full_themes = full.result["themes"].as_array().unwrap();
        let auth_full = full_themes.iter().find(|t| t["theme"] == "auth").unwrap();
        assert_eq!(auth_full["total"], 2);
        assert_eq!(auth_full["open"], 1);
        assert_eq!(
            auth_full["closed"], 1,
            "the manual closure lands in the `closed` bucket, not `implemented`"
        );
        assert_eq!(auth_full["implemented"], 0);
    }

    /// RECONCILE-ON-LINK (finding 01KY4S0FM3EFQNX0V0NHEPGPRR): linking an idea `addresses` to a
    /// Spec that is ALREADY in_progress moves it `open → planned` at LINK time (not only at the
    /// instant the Spec transitions); a `related` link reconciles nothing.
    #[test]
    fn addresses_link_reconciles_status_to_current_spec_state() {
        let _g = db_guard();
        let (db, project_id, _seed_spec) = seeded();
        // A Spec with problem+solution (so it passes the ready gate), driven to in_progress.
        let spec_id = db
            .with_conn(|c| {
                let sections = db::SpecSections {
                    problem: "a real problem".into(),
                    solution: "a real solution".into(),
                    ..db::SpecSections::empty()
                };
                let (spec, _rev) = db::create_spec(c, &project_id, "Active spec", sections)?;
                db::transition_spec(c, &spec.id, db::SpecTransition::Ready, true).unwrap();
                db::transition_spec(c, &spec.id, db::SpecTransition::Start, true).unwrap();
                Ok::<_, diesel::result::Error>(spec.id)
            })
            .unwrap();

        // idea_link addresses → the idea reconciles open → planned immediately.
        let idea = idea_id_of(
            &idea_add(
                &db,
                &json!({ "project_id": project_id, "title": "Reconcile me" }),
            )
            .unwrap(),
        );
        idea_link(
            &db,
            &json!({ "idea_id": idea, "spec_id": spec_id, "kind": "addresses" }),
        )
        .unwrap();
        assert_eq!(
            db.with_conn(|c| db::get_idea(c, &idea))
                .unwrap()
                .unwrap()
                .status,
            "planned",
            "linking addresses to an already in_progress Spec reconciles open → planned"
        );

        // A `related` link to the same active Spec drives NOTHING.
        let ctx = idea_id_of(
            &idea_add(
                &db,
                &json!({ "project_id": project_id, "title": "Context only" }),
            )
            .unwrap(),
        );
        idea_link(
            &db,
            &json!({ "idea_id": ctx, "spec_id": spec_id, "kind": "related" }),
        )
        .unwrap();
        assert_eq!(
            db.with_conn(|c| db::get_idea(c, &ctx))
                .unwrap()
                .unwrap()
                .status,
            "open",
            "a related link never reconciles the status"
        );

        // idea_add with spec_ids reconciles at creation too (the Spec is in_progress).
        let created = idea_add(
            &db,
            &json!({ "project_id": project_id, "title": "Born planned", "spec_ids": [spec_id] }),
        )
        .unwrap();
        assert_eq!(
            created.result["idea"]["status"], "planned",
            "idea_add spec_ids reconciles the fresh idea to the active Spec"
        );
    }

    #[test]
    fn dispatch_idea_tool_returns_none_for_a_foreign_tool() {
        let _g = db_guard();
        let (db, _p, _s) = seeded();
        assert!(dispatch_idea_tool(&db, "list_projects", &json!({})).is_none());
    }
}
