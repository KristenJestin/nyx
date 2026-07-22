//! Shell-agnostic MCP **ANNEXE tools** — attach a doc / artefact / URL / prototype to a Spec
//! with INTERNAL storage + DEDUP by content_hash (v2.6 — Idées entretenues, annexes &
//! dépendances Spec, Phase 3, task `01KXZ9K43C9ZMQ3M5JR07M78HV`).
//!
//! # Single mutation authority (no double path)
//!
//! Every tool here is a THIN adapter: it parses the JSON args, then delegates to the
//! `nyx_core::db` annexe authority (`add_annexe` / `list_annexes` / `get_annexe` /
//! `remove_annexe`). The content-addressed dedup (one physical blob per distinct content,
//! GLOBAL scope) and the last-reference blob GC live BELOW the tool, so the MCP mutation is the
//! single authority the native Spec-detail "Annexes" UI (Phase 4) will read from too — no
//! parallel implementation to drift.
//!
//! # Effects
//!
//! An annexe lives ON a Spec, so each mutating tool returns [`ChangedTopic::Specs`] — an open
//! Spec-detail surface re-pulls its "Annexes" block (the front's re-pull wiring is Phase 4; the
//! topic is named here so the seam is complete).

use serde_json::{json, Value};

use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{internal_db, require_str};
use crate::mcp_tools_core::{ChangedTopic, ToolOutcome};

// --- Tool name constants (advertised in `tools/list`, dispatched below) ------

/// `annexe_add` — attach an annexe (doc/artefact/url/prototype) to a Spec.
pub const ANNEXE_ADD_TOOL: &str = "annexe_add";
/// `annexe_list` — list a Spec's annexes (metadata + size, no content).
pub const ANNEXE_LIST_TOOL: &str = "annexe_list";
/// `annexe_get` — read ONE annexe WITH its stored content.
pub const ANNEXE_GET_TOOL: &str = "annexe_get";
/// `annexe_remove` — detach an annexe (GC-ing its blob on the last reference).
pub const ANNEXE_REMOVE_TOOL: &str = "annexe_remove";

/// The annexe tool names — advertised in `tools/list` and routed by [`dispatch_annexe_tool`].
/// A closed slice so a new tool is added in exactly one place.
pub const ANNEXE_TOOLS: [&str; 4] = [
    ANNEXE_ADD_TOOL,
    ANNEXE_LIST_TOOL,
    ANNEXE_GET_TOOL,
    ANNEXE_REMOVE_TOOL,
];

// --- Argument helpers ------------------------------------------------------

/// Resolve the annexe `kind` arg to the vocabulary (`doc` | `artefact` | `url` | `prototype`).
/// REQUIRED (an annexe has a type). An unknown value → `invalid_argument`.
fn parse_kind(args: &Value) -> Result<&'static str, RpcError> {
    match require_str(args, "kind")? {
        "doc" => Ok(db::ANNEX_KIND_DOC),
        "artefact" => Ok(db::ANNEX_KIND_ARTEFACT),
        "url" => Ok(db::ANNEX_KIND_URL),
        "prototype" => Ok(db::ANNEX_KIND_PROTOTYPE),
        other => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown kind '{other}' (accepted: doc, artefact, url, prototype)"),
        )),
    }
}

/// The `invalid_id` an unknown annexe id maps to.
fn unknown_annexe(id: &str) -> RpcError {
    RpcError::new(RpcCode::InvalidId, format!("unknown annexe {id}"))
}

// --- Tool handlers ---------------------------------------------------------

/// `annexe_add` — `{ spec_id, kind, title, content }` → `{ annexe }`. Attaches an annexe to a
/// Spec, storing `content` in the content-addressed store (DEDUP: the same content attached
/// twice stores ONE physical copy). The Spec existence is VALIDATED up front (a bad id →
/// `invalid_id`). For a `url` annexe the "content" IS the URL string (an internal pointer).
pub fn annexe_add(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let kind = parse_kind(args)?;
    let title = require_str(args, "title")?;
    let content = require_str(args, "content")?;

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

    let annexe = db
        .with_conn(|c| db::add_annexe(c, spec_id, kind, title, content))
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "annexe": annexe }),
        ChangedTopic::Specs,
    ))
}

/// `annexe_list` — `{ spec_id }` → `{ annexes }`. Lists a Spec's annexes newest-first, each with
/// its metadata + blob `byte_len` (NO content — use `annexe_get` for the bytes). Read-only.
pub fn annexe_list(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let annexes = db
        .with_conn(|c| db::list_annexes(c, spec_id))
        .map_err(internal_db)?;
    Ok(ToolOutcome::read(json!({ "annexes": annexes })))
}

/// `annexe_get` — `{ id }` → `{ annexe }` (the row + its stored `content` + `byte_len`). Unknown
/// id → `invalid_id`. Read-only.
pub fn annexe_get(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let id = require_str(args, "id")?;
    let annexe = db
        .with_conn(|c| db::get_annexe(c, id))
        .map_err(internal_db)?
        .ok_or_else(|| unknown_annexe(id))?;
    Ok(ToolOutcome::read(json!({ "annexe": annexe })))
}

/// `annexe_remove` — `{ id }` → `{ removed }`. Detaches an annexe (idempotent: `removed` is
/// `false` when the id was unknown). The underlying blob is GC'd when this was its last
/// reference (the content-addressed store never accretes orphans).
pub fn annexe_remove(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let id = require_str(args, "id")?;
    let removed = db
        .with_conn(|c| db::remove_annexe(c, id))
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "removed": removed, "id": id }),
        ChangedTopic::Specs,
    ))
}

// --- Dispatch --------------------------------------------------------------

/// Route an ANNEXE tool call, or `None` when `name` is not one of ours. Called from
/// [`crate::mcp_tools_core::dispatch_extension_tool`] so the annexe tools share the SAME dispatch
/// entry point (one dispatcher, one pool) as every other extension tool.
pub fn dispatch_annexe_tool(
    db: &Db,
    name: &str,
    args: &Value,
) -> Option<Result<ToolOutcome, RpcError>> {
    match name {
        ANNEXE_ADD_TOOL => Some(annexe_add(db, args)),
        ANNEXE_LIST_TOOL => Some(annexe_list(db, args)),
        ANNEXE_GET_TOOL => Some(annexe_get(db, args)),
        ANNEXE_REMOVE_TOOL => Some(annexe_remove(db, args)),
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

    /// A memory DB seeded with one project + one Spec. Returns `(db, spec_id)`.
    fn seeded() -> (Db, String) {
        let db = Db::in_memory();
        let spec_id = db.with_conn(|c| {
            let (project, _root) = db::create_project(c, "Proj", "/tmp/annexe-mcp", None).unwrap();
            let (spec, _rev) =
                db::create_spec(c, &project.id, "Auth Spec", db::SpecSections::empty()).unwrap();
            spec.id
        });
        (db, spec_id)
    }

    fn annexe_id_of(out: &ToolOutcome) -> String {
        out.result["annexe"]["id"].as_str().unwrap().to_string()
    }

    /// DONE-CRITERION ("MCP add/list/get/remove ; même artefact 2x = une copie"): the full
    /// annexe surface through the real dispatch — add (validated Spec + kind), the dedup
    /// (same content twice = one stored copy), list (no content), get (content), remove
    /// (idempotent). Each mutation reports a change on the Specs topic.
    #[test]
    fn annexe_add_list_get_remove_and_dedup_through_dispatch() {
        let _g = db_guard();
        let (db, spec_id) = seeded();

        let content = "the prototype HTML bytes";
        let a1 = dispatch_annexe_tool(
            &db,
            ANNEXE_ADD_TOOL,
            &json!({ "spec_id": spec_id, "kind": "prototype", "title": "L1", "content": content }),
        )
        .expect("annexe_add routed")
        .expect("annexe_add ok");
        assert_eq!(a1.effects, vec![ChangedTopic::Specs]);
        assert_eq!(a1.result["annexe"]["kind"], "prototype");
        let id1 = annexe_id_of(&a1);

        // Same content again → ONE stored blob (dedup), but a distinct attachment row.
        let a2 = annexe_add(
            &db,
            &json!({ "spec_id": spec_id, "kind": "doc", "title": "dup", "content": content }),
        )
        .unwrap();
        assert_eq!(
            a1.result["annexe"]["content_hash"], a2.result["annexe"]["content_hash"],
            "same content → same hash"
        );
        assert_eq!(
            db.with_conn(db::annex_blob_count).unwrap(),
            1,
            "one physical copy for two identical attachments"
        );

        // list is a read, per Spec, carrying byte_len but no content.
        let listed = annexe_list(&db, &json!({ "spec_id": spec_id })).unwrap();
        assert!(listed.effects.is_empty(), "list is a read");
        assert_eq!(listed.result["annexes"].as_array().unwrap().len(), 2);
        assert_eq!(
            listed.result["annexes"][0]["byte_len"],
            content.len() as i64
        );
        assert!(
            listed.result["annexes"][0].get("content").is_none(),
            "list omits content"
        );

        // get returns the stored content.
        let got = dispatch_annexe_tool(&db, ANNEXE_GET_TOOL, &json!({ "id": id1 }))
            .expect("annexe_get routed")
            .expect("annexe_get ok");
        assert!(got.effects.is_empty(), "get is a read");
        assert_eq!(got.result["annexe"]["content"], content);
        assert_eq!(got.result["annexe"]["title"], "L1");

        // remove is idempotent + reports a Specs change; the shared blob survives until last ref.
        let rm1 = annexe_remove(&db, &json!({ "id": id1 })).unwrap();
        assert_eq!(rm1.effects, vec![ChangedTopic::Specs]);
        assert_eq!(rm1.result["removed"], true);
        assert_eq!(
            db.with_conn(db::annex_blob_count).unwrap(),
            1,
            "blob survives (a2 still references it)"
        );
        annexe_remove(&db, &json!({ "id": annexe_id_of(&a2) })).unwrap();
        assert_eq!(
            db.with_conn(db::annex_blob_count).unwrap(),
            0,
            "blob GC'd on last reference"
        );
        let rm_unknown = annexe_remove(&db, &json!({ "id": id1 })).unwrap();
        assert_eq!(
            rm_unknown.result["removed"], false,
            "already removed → no-op"
        );
    }

    /// Validation: a bad Spec id is `invalid_id`, a bad kind is `invalid_argument`, an unknown
    /// annexe id (get) is `invalid_id`.
    #[test]
    fn annexe_validation_errors() {
        let _g = db_guard();
        let (db, spec_id) = seeded();

        assert_eq!(
            annexe_add(
                &db,
                &json!({ "spec_id": "nope", "kind": "doc", "title": "t", "content": "c" })
            )
            .unwrap_err()
            .code,
            RpcCode::InvalidId
        );
        assert_eq!(
            annexe_add(
                &db,
                &json!({ "spec_id": spec_id, "kind": "bogus", "title": "t", "content": "c" })
            )
            .unwrap_err()
            .code,
            RpcCode::InvalidArgument
        );
        assert_eq!(
            annexe_get(&db, &json!({ "id": "nope" })).unwrap_err().code,
            RpcCode::InvalidId
        );
    }

    #[test]
    fn dispatch_annexe_tool_returns_none_for_a_foreign_tool() {
        let _g = db_guard();
        let (db, _s) = seeded();
        assert!(dispatch_annexe_tool(&db, "list_projects", &json!({})).is_none());
    }
}
