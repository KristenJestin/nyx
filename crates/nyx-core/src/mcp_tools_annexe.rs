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

use crate::db::{self, AnnexeError, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{internal_db, optional_str, require_str};
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

/// The `invalid_argument` a `kind` outside the vocabulary maps to — the authority's own sentence
/// ([`db::unknown_annex_kind_message`]), so both `kind` arguments refuse in the same words.
fn unknown_kind(kind: &str) -> RpcError {
    RpcError::new(
        RpcCode::InvalidArgument,
        db::unknown_annex_kind_message(kind),
    )
}

/// Resolve the annexe `kind` arg to the vocabulary (`doc` | `artefact` | `url` | `prototype`).
/// REQUIRED (an annexe has a type). An unknown value → `invalid_argument`.
fn parse_kind(args: &Value) -> Result<&'static str, RpcError> {
    let raw = require_str(args, "kind")?;
    db::annex_kind_from_str(raw).ok_or_else(|| unknown_kind(raw))
}

/// Resolve an OPTIONAL annexe `kind` FILTER (the `annexe_list` narrowing). Absent / `null` → `None`
/// (the unfiltered read, byte-identical to the pre-v50 behaviour). Any other value must be IN the
/// vocabulary — `invalid_argument` otherwise, the same refusal as [`parse_kind`], because a typo
/// must never silently widen the read.
///
/// Read off the raw argument rather than through `optional_str`, which folds an EMPTY string into
/// "absent": for a CLOSED vocabulary that is exactly the silent widening this refuses — a
/// `kind: ""` would have returned EVERY kind instead of being named as the bad value it is.
fn parse_kind_filter(args: &Value) -> Result<Option<&'static str>, RpcError> {
    let raw = match args.get("kind") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => s.as_str(),
        Some(_) => {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                "argument 'kind' must be a string".to_string(),
            ))
        }
    };
    db::annex_kind_from_str(raw)
        .map(Some)
        .ok_or_else(|| unknown_kind(raw))
}

/// The `invalid_id` an unknown annexe id maps to.
fn unknown_annexe(id: &str) -> RpcError {
    RpcError::new(RpcCode::InvalidId, format!("unknown annexe {id}"))
}

/// Map an [`AnnexeError`] to its MCP code, keeping the authority's own sentence: a bad URL is an
/// `invalid_argument` (the caller passed a non-http(s) string, one carrying whitespace / control
/// characters inside it, or one past the 2 048-character bound — THREE sentences, one code,
/// because all three are the argument's own defect), a duplicate link is an `invalid_state` (the
/// link is already there — the Spec's state, not the argument, is what refuses), and anything else
/// is the usual `internal` DB mapping.
fn annexe_err(e: AnnexeError) -> RpcError {
    match e {
        AnnexeError::InvalidUrl(_)
        | AnnexeError::UrlHasWhitespace(_)
        | AnnexeError::UrlTooLong(_) => RpcError::new(RpcCode::InvalidArgument, e.to_string()),
        AnnexeError::DuplicateUrl(_) => RpcError::new(RpcCode::InvalidState, e.to_string()),
        AnnexeError::Db(db_err) => internal_db(db_err),
    }
}

// --- Tool handlers ---------------------------------------------------------

/// `annexe_add` — `{ spec_id, kind, title?, content }` → `{ annexe }`. Attaches an annexe to a
/// Spec, storing `content` in the content-addressed store (DEDUP: the same content attached
/// twice stores ONE physical copy). The Spec existence is VALIDATED up front (a bad id →
/// `invalid_id`). For a `url` annexe the "content" IS the URL string (an internal pointer).
///
/// The `url` kind adds the EXTERNAL-LINK rules of the authority, and they are the ones
/// [`annexe_err`] maps just above — same list, same order as `db::add_annexe` applies them:
///  * the URL is stored in CANONICAL form (surrounding blanks trimmed, scheme and host lowered, the
///    path's trailing slash dropped), so that form is what the caller gets back and what dedups;
///  * at most 2 048 characters of that canonical form (`invalid_argument`);
///  * it must be an absolute http(s) URL (`invalid_argument`), and a whitespace or control
///    character INSIDE it is refused too — with its OWN sentence, not the absolute-URL one;
///  * the same URL may not be attached twice to one Spec (`invalid_state`).
///
/// Only `url` carries any of them: a doc / artefact / prototype is a DOCUMENT, stored byte-for-byte
/// and not length-bounded. `title` is OPTIONAL for a `url` — absent, empty or blank all store the
/// URL itself as the title (`annexes.title` stays NOT NULL). Every other kind still REQUIRES a
/// title, exactly as before.
pub fn annexe_add(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let kind = parse_kind(args)?;
    let title = if kind == db::ANNEX_KIND_URL {
        optional_str(args, "title")?.unwrap_or("")
    } else {
        require_str(args, "title")?
    };
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
        .map_err(annexe_err)?;
    Ok(ToolOutcome::changed(
        json!({ "annexe": annexe }),
        ChangedTopic::Specs,
    ))
}

/// `annexe_list` — `{ spec_id, kind? }` → `{ annexes }`. Lists a Spec's annexes newest-first, each
/// with its metadata + blob `byte_len` (NO content — use `annexe_get` for the bytes). Read-only.
///
/// `kind` is the OPTIONAL narrowing (v50): `kind: "url"` reads the Spec's EXTERNAL LINKS and
/// nothing else, which is why the link surface needed NO new tool. Omitting it leaves the read
/// exactly as it was — same rows, same order.
pub fn annexe_list(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let kind = parse_kind_filter(args)?;
    let annexes = db
        .with_conn(|c| db::list_annexes(c, spec_id, kind))
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

        // REVIEW FINDING F3 — the length bound reaches the AGENT with the authority's own sentence,
        // under the code its two siblings already use: the argument is what is wrong, not the
        // Spec's state. Only `url` is bounded — a prototype four times the size goes through.
        let too_long = format!("https://x.test/{}", "a".repeat(2100));
        let refused = annexe_add(
            &db,
            &json!({ "spec_id": spec_id, "kind": "url", "content": too_long }),
        )
        .unwrap_err();
        assert_eq!(refused.code, RpcCode::InvalidArgument);
        assert_eq!(
            refused.message,
            "url too long: 2115 characters — a url annexe is limited to 2048 characters",
            "the tool SHOWS the authority's sentence, it does not re-word it"
        );
        annexe_add(
            &db,
            &json!({
                "spec_id": spec_id,
                "kind": "prototype",
                "title": "mockup",
                "content": "x".repeat(8192),
            }),
        )
        .expect("a prototype is not length-bounded");
    }

    #[test]
    fn dispatch_annexe_tool_returns_none_for_a_foreign_tool() {
        let _g = db_guard();
        let (db, _s) = seeded();
        assert!(dispatch_annexe_tool(&db, "list_projects", &json!({})).is_none());
    }

    /// DONE-CRITERION ("`annexe_list` avec `kind='url'` ne retourne que les liens ; sans `kind`, la
    /// sortie est identique à aujourd'hui"): the OPTIONAL narrowing is what makes the Spec's
    /// external LINKS readable without a new tool. The unfiltered call is asserted to be
    /// byte-identical to the filtered-off behaviour (same rows, same newest-first order), so the
    /// added argument cannot have moved the existing read; and a bogus `kind` is refused rather
    /// than silently widening it.
    #[test]
    fn annexe_list_filters_on_kind_and_is_unchanged_without_it() {
        let _g = db_guard();
        let (db, spec_id) = seeded();

        for (kind, title, content) in [
            ("doc", "the doc", "some prose"),
            ("url", "Jira", "https://jira.example.test/BROWSE-1"),
            ("prototype", "L1", "<html>mockup</html>"),
            ("url", "the PR", "https://github.test/o/r/pull/7"),
        ] {
            annexe_add(
                &db,
                &json!({ "spec_id": spec_id, "kind": kind, "title": title, "content": content }),
            )
            .expect("annexe_add ok");
        }

        // WITHOUT `kind`: every annexe, newest-first — the pre-v50 read, untouched.
        let all = annexe_list(&db, &json!({ "spec_id": spec_id })).unwrap();
        let all_rows = all.result["annexes"].as_array().unwrap().clone();
        assert_eq!(all_rows.len(), 4);
        assert_eq!(
            all_rows
                .iter()
                .map(|r| r["title"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["the PR", "L1", "Jira", "the doc"],
            "newest-first, every kind"
        );
        assert!(all.effects.is_empty(), "list is a read");

        // `kind: null` is the SAME unfiltered read (an absent optional is not a filter).
        let explicit_null = annexe_list(&db, &json!({ "spec_id": spec_id, "kind": null })).unwrap();
        assert_eq!(explicit_null.result["annexes"], json!(all_rows));

        // WITH `kind: "url"`: the LINKS only, in the same order, unchanged rows.
        let links = annexe_list(&db, &json!({ "spec_id": spec_id, "kind": "url" })).unwrap();
        let link_rows = links.result["annexes"].as_array().unwrap();
        assert_eq!(link_rows.len(), 2, "only the two url annexes");
        assert!(link_rows.iter().all(|r| r["kind"] == "url"));
        assert_eq!(
            link_rows
                .iter()
                .map(|r| r["title"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["the PR", "Jira"],
            "the links keep the newest-first order of the unfiltered read"
        );
        assert_eq!(
            link_rows.to_vec(),
            all_rows
                .iter()
                .filter(|r| r["kind"] == "url")
                .cloned()
                .collect::<Vec<_>>(),
            "the filtered read is EXACTLY the url subset of the unfiltered one"
        );

        // A kind outside the vocabulary is refused — never a silently unfiltered read. The EMPTY
        // string is one of those values and not "no filter": `optional_str` folds `""` into
        // absent, which for a CLOSED vocabulary is the silent widening this guards against.
        for bogus in ["bogus", ""] {
            assert_eq!(
                annexe_list(&db, &json!({ "spec_id": spec_id, "kind": bogus }))
                    .unwrap_err()
                    .code,
                RpcCode::InvalidArgument,
                "kind '{bogus}' must be refused, not read as the unfiltered list"
            );
        }
    }

    /// DONE-CRITERION ("Le schéma d'outil documente le paramètre optionnel" + "Le nombre total
    /// d'outils MCP est inchangé"): `tools/list` advertises `kind` on `annexe_list` WITHOUT making
    /// it required, and the annexe surface is still the SAME FOUR tools — the link feature added
    /// no tool, so the surface-count test elsewhere needs no edit.
    #[test]
    fn annexe_list_tool_schema_documents_the_optional_kind() {
        let descriptor = crate::mcp::tool_descriptors()
            .into_iter()
            .find(|d| d["name"] == ANNEXE_LIST_TOOL)
            .expect("annexe_list is advertised");
        assert!(
            descriptor["inputSchema"]["properties"]["kind"].is_object(),
            "the kind parameter is documented in the tool schema"
        );
        assert_eq!(
            descriptor["inputSchema"]["required"],
            json!(["spec_id"]),
            "kind is OPTIONAL — only spec_id is required"
        );
        assert_eq!(ANNEXE_TOOLS.len(), 4, "no tool was added for the links");
    }

    /// FINDINGS `01M033H5Y1765GH2V77XF8KDSF` / `01M032WP4YZFMBK9HNCHSBK490`: THE PUBLISHED SCHEMA OF
    /// `annexe_add` MUST SAY WHAT THE HANDLER DOES ABOUT `title`.
    ///
    /// v50 relaxed `title` for `kind='url'` ONLY (an unlabelled link is stored with the URL as its
    /// title), but the descriptor dropped `title` from `required` for the FOUR kinds — so an agent
    /// trusting `tools/list` and omitting `title` on a `doc` got an `invalid_argument` at call time.
    /// `tools/list` is the EXTERNAL contract, the one surface with no compiler behind it; this test
    /// pins both halves TOGETHER so neither can move alone: the JSON-Schema condition, and the
    /// handler's behaviour for each kind.
    #[test]
    fn annexe_add_tool_schema_matches_the_handlers_title_rule() {
        let descriptor = crate::mcp::tool_descriptors()
            .into_iter()
            .find(|d| d["name"] == ANNEXE_ADD_TOOL)
            .expect("annexe_add is advertised");
        let schema = &descriptor["inputSchema"];

        // The unconditional half: `title` is NOT in the flat `required` (a url may omit it)…
        assert_eq!(
            schema["required"],
            json!(["spec_id", "kind", "content"]),
            "the always-required arguments"
        );
        // …and the CONDITION lives in the schema, not only in the property's prose: every kind but
        // `url` still requires it.
        assert_eq!(
            schema["allOf"],
            json!([{
                "if": {
                    "properties": { "kind": { "enum": ["doc", "artefact", "prototype"] } },
                    "required": ["kind"],
                },
                "then": { "required": ["title"] },
            }]),
            "the schema itself says: kind != url ⇒ title required"
        );

        // THE HANDLER, kind by kind — the other half of the same contract.
        let _g = db_guard();
        let (db, spec_id) = seeded();
        for kind in ["doc", "artefact", "prototype"] {
            let refused = annexe_add(
                &db,
                &json!({ "spec_id": spec_id, "kind": kind, "content": "body" }),
            )
            .unwrap_err();
            assert_eq!(
                refused.code,
                RpcCode::InvalidArgument,
                "a {kind} still REQUIRES a title, exactly as the schema now says"
            );
        }
        let url = "https://jira.example.test/BROWSE-1";
        let unlabelled = annexe_add(
            &db,
            &json!({ "spec_id": spec_id, "kind": "url", "content": url }),
        )
        .expect("a url may be attached WITHOUT a title");
        assert_eq!(
            unlabelled.result["annexe"]["title"], url,
            "an omitted label stores the URL itself as the title"
        );

        // REVIEW FINDING F4 — the PROSE must say the same thing as the handler, for the THREE ways
        // to give no label. The behaviour does not change (an empty `title` was already accepted on
        // a url while an empty `content` is refused): what changes is that the published contract
        // now states it, instead of leaving a reader to discover the asymmetry at call time.
        let title_doc = schema["properties"]["title"]["description"]
            .as_str()
            .expect("title is documented");
        for case in ["absent", "empty", "blank"] {
            assert!(
                title_doc.to_ascii_lowercase().contains(case),
                "the published description names the '{case}' case; got: {title_doc}"
            );
        }
        for (label, url) in [
            (json!(""), "https://jira.example.test/BROWSE-2"),
            (json!("   "), "https://jira.example.test/BROWSE-3"),
        ] {
            let stored = annexe_add(
                &db,
                &json!({ "spec_id": spec_id, "kind": "url", "title": label, "content": url }),
            )
            .unwrap_or_else(|e| {
                panic!("a {label} title is 'no label' on a url, not a refusal: {e:?}")
            })
            .result["annexe"]["title"]
                .clone();
            assert_eq!(stored, json!(url), "…and the URL becomes the title");
        }
    }
}
