//! Shell-agnostic MCP **runtime command tools** — the dispatch logic for
//! `start_command` / `stop_command` / `relaunch_command` / `get_command_output`,
//! lifted out of the Tauri `mcp_tools::NyxToolDispatcher` so BOTH shells route these
//! tools through the IDENTICAL code over a [`Db`] + a [`CommandRunner<S>`].
//!
//! The Tauri dispatcher held an `AppHandle<R>` and reached `tauri::State` for the
//! `Db`/runner; these free functions instead take the two `nyx-core` handles
//! directly, so NO shell type crosses them (the frozen "0 Tauri in nyx-core" rule).
//! The Electron core-host's napi MCP dispatcher calls straight into here, reaching
//! true Tauri parity for the runtime tools (no more `mcp_unavailable`).
//!
//! The JSON shapes are the SAME the Tauri dispatcher returns:
//! `{ instance_id, state, running, finished, exit_code, unread, … }`, plus the
//! action acks (`was_running`, `restarted`, `changed`) and the
//! `get_command_output` window (`output, total_bytes, returned_bytes, truncated,
//! reset, cursor, run`).

use std::time::Duration;

use serde_json::{json, Value};

use crate::command::{
    poll_until, CommandRunner, RunState, RunnerSink, WAIT_MAX_TIMEOUT, WAIT_POLL_INTERVAL,
};
use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};

/// Default `timeout_ms` for `wait_for_command` (ADR-0003 D12) when the caller omits it:
/// a 30 s bounded wait, clamped to [`WAIT_MAX_TIMEOUT`].
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;

/// The default `tail_bytes` window for `get_command_output` — the token-safe 12 KiB
/// the Tauri dispatcher uses (small enough to fit an agent's MCP budget on a default
/// read of a busy dev server).
pub const DEFAULT_TAIL_BYTES: usize = 12 * 1024;

// --- Argument helpers (the D8 vocabulary) ----------------------------------

/// A REQUIRED string argument; missing/empty/non-string → `invalid_argument`.
pub fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    match args.get(key).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("missing or empty required argument '{key}'"),
        )),
    }
}

/// An OPTIONAL string argument; absent / null / empty → `None`; non-string → error.
pub fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be a string"),
        )),
    }
}

/// An OPTIONAL string argument on an ANCHORING tool — [`optional_str`]'s STRICTER twin, and the
/// ONE place the three anchoring tools read their anchor from (`spec_set_workspace.workspace_id`,
/// `workspace_add.spec_id`, `create_workspace.spec_id`). Absent / null → `None`; an EMPTY string →
/// `invalid_argument`; a non-string → `invalid_argument`.
///
/// WHY THESE ARGUMENTS AND NOT THE OTHERS (review F2). [`optional_str`] maps `""` to `None`
/// deliberately, and for 122 call sites that is right: `None` means "the caller said nothing", so
/// normalizing an empty string to silence loses nothing. On an ANCHORING argument `None` does not
/// mean silence — IT IS AN EFFECT: detach the Spec from its workspace, or register a workspace with
/// no Spec linked. The normalization therefore converts a CALLER'S BUG (an uninitialized variable
/// reaching the wire as `""`) into a silent mutation: an agent that believes it is anchoring
/// detaches instead, and nothing anywhere says so (observed: a Spec left with `workspace_id` NULL
/// after a `spec_set_workspace(workspace_id: "")`). An empty string is not a way to say "none", so
/// here it is REFUSED rather than reinterpreted.
///
/// `absent_means` completes the refusal with what the caller should pass INSTEAD to obtain the
/// no-anchor behaviour they may actually have wanted — it differs per tool (detaching is not the
/// same act as not linking), and naming it is what tells the empty string apart from the absence.
///
/// [`optional_str`] itself is deliberately UNCHANGED: its semantics are a repo-wide convention
/// (`require_str` refuses `""`, `optional_str` absorbs it), and re-deciding it for 122 call sites is
/// its own subject, not a side effect of this one.
pub fn anchor_str<'a>(
    args: &'a Value,
    key: &str,
    absent_means: &str,
) -> Result<Option<&'a str>, RpcError> {
    if let Some(Value::String(s)) = args.get(key) {
        if s.is_empty() {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "argument '{key}' is an EMPTY string, which is not a way to say 'none' — it \
                     would silently {absent_means}. Pass a real id, or omit '{key}' (or send null) \
                     if that is what you meant"
                ),
            ));
        }
    }
    optional_str(args, key)
}

/// An OPTIONAL array-of-strings argument, ORDER-preserving. Absent / null → `None`; a JSON array
/// → `Some(Vec)` (an EMPTY array is accepted, yielding `Some(vec![])` so a caller can distinguish
/// "clear the list" from "leave it untouched"); a non-array, or a non-string / empty-string
/// element, → `invalid_argument`. The optional twin of [`crate::mcp_tools_core`]'s `require_str_vec`.
pub fn optional_str_vec(args: &Value, key: &str) -> Result<Option<Vec<String>>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, v) in arr.iter().enumerate() {
                match v.as_str() {
                    Some(s) if !s.is_empty() => out.push(s.to_string()),
                    _ => {
                        return Err(RpcError::new(
                            RpcCode::InvalidArgument,
                            format!("argument '{key}[{i}]' must be a non-empty string"),
                        ))
                    }
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be an array of strings"),
        )),
    }
}

/// An OPTIONAL per-run ENVIRONMENT map argument (`{ "KEY": "VALUE", … }`) decoded to the
/// `(KEY, VALUE)` pairs [`CommandRunner::start_with_env`] merges onto the inherited environment.
/// Absent / null → an EMPTY vec (no per-run env, the runner's inherit-only path). A non-object,
/// or ANY non-string value, → `invalid_argument` — the advertised schema states "Each value must
/// be a string", so a number/bool/array is a caller mistake worth naming rather than coercing.
///
/// SECRET HYGIENE: an env VALUE is a secret by default (the runner's "secret values are never
/// logged" invariant), so a refusal names the offending KEY and NEVER echoes the value.
pub fn optional_env_map(args: &Value, key: &str) -> Result<Vec<(String, String)>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Object(map)) => {
            let mut out = Vec::with_capacity(map.len());
            for (name, value) in map {
                match value.as_str() {
                    Some(s) => out.push((name.clone(), s.to_string())),
                    None => {
                        return Err(RpcError::new(
                            RpcCode::InvalidArgument,
                            format!(
                                "argument '{key}.{name}' must be a string (each env value is a \
                                 string)"
                            ),
                        ))
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be an object of KEY→VALUE strings"),
        )),
    }
}

/// An OPTIONAL non-negative integer argument; negative/non-integer → error.
pub fn optional_usize(args: &Value, key: &str) -> Result<Option<usize>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_i64().ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidArgument,
                    format!("argument '{key}' must be an integer"),
                )
            })?;
            if n < 0 {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!("argument '{key}' must be >= 0"),
                ));
            }
            Ok(Some(n as usize))
        }
    }
}

/// An OPTIONAL boolean argument; absent/null → `None`; non-bool → error.
pub fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be a boolean"),
        )),
    }
}

pub fn internal_db(e: diesel::result::Error) -> RpcError {
    RpcError::new(RpcCode::Internal, format!("db error: {e}"))
}

/// Map a [`crate::db::ContentWriteError`] onto the RPC vocabulary: a status refusal is a clean
/// `invalid_state` (the Spec's lifecycle froze this content), an unrecognised phase name is an
/// `invalid_argument` (the caller passed a name, and it is the wrong one), a DB error stays
/// `internal` (B4).
pub fn map_content_write_err(e: crate::db::ContentWriteError) -> RpcError {
    match e {
        crate::db::ContentWriteError::Forbidden { status, what } => RpcError::new(
            RpcCode::InvalidState,
            format!(
                "cannot write {what} while the Spec is '{status}': {what} are frozen once the Spec \
                 leaves its writable state (sections/tasks in draft, comments before it closes)"
            ),
        )
        .with_data(json!({ "reason": "spec_state_forbids_write", "status": status })),
        // K2 — a phase name the plan does not hold, on a Spec past `draft`. It used to MATERIALIZE
        // one, so a single mistyped free-text argument grew the plan of a Spec under build. The
        // refusal QUOTES the plan, because the mistake is always a near-miss (a rendered heading, a
        // paraphrase) and what the caller needs is the exact string that would have worked.
        crate::db::ContentWriteError::UnknownPhase {
            name,
            status,
            phases,
        } => {
            let plan = if phases.is_empty() {
                "it has NO phase plan — its tasks are flat, so omit `phase`".to_string()
            } else {
                format!(
                    "its phases are {}",
                    phases
                        .iter()
                        .map(|p| format!("'{p}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "phase '{name}' is not a phase of this Spec, and its plan is frozen while it is \
                     '{status}' — {plan}. Pass one of those names EXACTLY (the NAME alone, never the \
                     '2 · Some phase' heading a manual renders), or omit `phase` altogether: on a \
                     kind=review finding nyx anchors the phase itself. A name nyx does not know \
                     would ADD a phase to a Spec already being built, which no task may do"
                ),
            )
            .with_data(json!({
                "reason": "unknown_phase",
                "phase": name,
                "status": status,
                "phases": phases,
            }))
        }
        crate::db::ContentWriteError::Db(e) => internal_db(e),
    }
}

// --- The RESPONSE BUDGET: the hard cap EVERY agent-facing tool answers under ------------
//
// An MCP response the agent cannot read is a broken tool, and the failure mode is always the same:
// a surface that GROWS with the project (a catalogue, a prototype, a review) is echoed WHOLE on a
// naive call with no parameter. That is how a real front produced a 145 000-character
// `design_inventory` reply (review #43) — and the exact same wall was waiting on `proto_get` and
// `proto_composition`, which echoed every round, page, comment BODY and composition tree of a
// prototype (review 01KYMF0KFZQG5JYZDGZCR85BAM).
//
// The budget lived in `mcp_tools_design` and covered `design_inventory` ALONE, because that is where
// the overflow had been SEEN. It lives HERE now — beside the argument vocabulary every tool already
// shares — so a tool is capped by REUSING the mechanism, not by re-inventing one.
//
// THE PRINCIPLE: a naive call is safe BY CONSTRUCTION, never by the agent remembering to paginate.
// Three bounds compose:
//   1. [`bounded_row`] caps ONE echoed row (its prose fields cut, its machine payloads dropped
//      WHOLE — a half `*_json` is not a smaller payload, it is a CORRUPT one);
//   2. [`under_response_budget`] / [`under_response_budget_strict`] cap a LIST by the sum of its
//      items' serialized sizes;
//   3. the caller computes the list budget as [`MAX_RESPONSE_BYTES`] MINUS its fixed head and
//      [`RESPONSE_ENVELOPE_BYTES`], so the WHOLE serialized response — head, counters, separators —
//      stays under the cap.
// Every capped surface then says so explicitly (`truncated: true` + `total`/`returned`) and offers a
// way to get the rest (`next_offset`, or a targeted read of ONE item).

/// The RESPONSE-level byte budget shared by every agent-facing tool: items are emitted until it is
/// spent, then `truncated` is raised. 64 KiB leaves an ample margin under any MCP transport limit
/// while still handing the agent a couple of full source files (or a whole composition tree) per read.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// The slack a caller SUBTRACTS from [`MAX_RESPONSE_BYTES`] before budgeting its lists, to pay for
/// what the item budget does not measure: the response's own keys, its counters, the `,`/`[]`
/// separators between items. Deliberately generous — the point of the cap is that it holds, not that
/// it is tight.
pub const RESPONSE_ENVELOPE_BYTES: usize = 4 * 1024;

/// How many bytes of ONE string field of an echoed row survive [`bounded_row`]. 4 KiB is a long
/// comment / summary / template and a short novel; past it, a field is reported by its REAL length
/// instead of being carried.
pub const MAX_ROW_FIELD_BYTES: usize = 4 * 1024;

/// The ceiling on ONE echoed row AFTER its fields are capped ([`bounded_row`]). A row wider than
/// this degrades to its IDENTITY (every string cut to [`IDENTITY_FIELD_BYTES`], `oversized: true`),
/// so a single pathological row can never eat a whole response — which is what makes "at least one
/// item always fits a list budget" a fact rather than a hope.
pub const MAX_ROW_BYTES: usize = 16 * 1024;

/// How many bytes of each string survive the IDENTITY degradation of an over-wide row.
const IDENTITY_FIELD_BYTES: usize = 256;

/// The serialized byte length of `value` — the unit EVERY budget in this module counts in.
pub fn json_len(value: &Value) -> usize {
    serde_json::to_string(value).map(|s| s.len()).unwrap_or(0)
}

/// Cut `s` to at most `max` BYTES without splitting a UTF-8 character (a byte-sliced string would
/// panic, and a broken code point is not a shorter string — it is an invalid one). Returns the
/// (possibly cut) slice and whether anything was left out.
pub fn bounded_text(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

/// [`bounded_text`] without the flag, for the callers that only want the slice.
fn truncate_utf8(s: &str, max: usize) -> &str {
    bounded_text(s, max).0
}

/// Cap every top-level STRING field of `value` at `max_field` bytes. Returns the capped object and
/// the map `field name → REAL byte length` of what was cut (empty when nothing was).
///
/// A field whose name ends in `_json` is a MACHINE payload (`composition_json`, `params_json`,
/// `states_json`): a prefix of it does not parse, so it is dropped WHOLE (`null`) rather than
/// handed over corrupt. Everything else is PROSE, and a prefix of prose is still readable.
fn cap_row_fields(value: &Value, max_field: usize) -> (Value, serde_json::Map<String, Value>) {
    let mut cut = serde_json::Map::new();
    let Some(object) = value.as_object() else {
        return (value.clone(), cut);
    };
    let mut out = serde_json::Map::with_capacity(object.len());
    for (key, field) in object {
        match field.as_str() {
            Some(text) if text.len() > max_field => {
                cut.insert(key.clone(), json!(text.len()));
                if key.ends_with("_json") {
                    out.insert(key.clone(), Value::Null);
                } else {
                    out.insert(key.clone(), json!(truncate_utf8(text, max_field)));
                }
            }
            _ => {
                out.insert(key.clone(), field.clone());
            }
        }
    }
    (Value::Object(out), cut)
}

/// Bound ONE echoed row so no single item of a list can blow the response on its own.
///
/// EVERY string field is capped at [`MAX_ROW_FIELD_BYTES`]; the row then reports what it cut in
/// `truncated_fields` (`{ field: REAL byte length }`) so the agent reads a SHORTENED value as
/// shortened, never as the whole. A row still past [`MAX_ROW_BYTES`] after that (many wide fields at
/// once) degrades to its IDENTITY and says `oversized: true` with its real `bytes`.
///
/// NOT for a payload whose PARTIAL form would be a false contract — the elected `composition_json`
/// of `proto_composition` is paginated PAGE BY PAGE instead, never trimmed (review
/// 01KYMF0KFZQG5JYZDGZCR85BAM: a silently truncated placement is worse than an overflow).
pub fn bounded_row(value: &Value) -> Value {
    let (capped, cut) = cap_row_fields(value, MAX_ROW_FIELD_BYTES);
    if json_len(&capped) > MAX_ROW_BYTES {
        let real = json_len(value);
        let (mut identity, dropped) = cap_row_fields(value, IDENTITY_FIELD_BYTES);
        if let Some(object) = identity.as_object_mut() {
            object.insert("truncated_fields".into(), Value::Object(dropped));
            object.insert("oversized".into(), json!(true));
            object.insert("bytes".into(), json!(real));
        }
        return identity;
    }
    let mut capped = capped;
    if !cut.is_empty() {
        if let Some(object) = capped.as_object_mut() {
            object.insert("truncated_fields".into(), Value::Object(cut));
        }
    }
    capped
}

/// The shared emission loop: keep items while their serialized sizes fit `budget`. `always_first`
/// forces the FIRST item out even when it alone exceeds the budget.
fn fill_budget(items: Vec<Value>, budget: usize, always_first: bool) -> (Vec<Value>, bool) {
    let total = items.len();
    let mut kept: Vec<Value> = Vec::with_capacity(total);
    let mut spent = 0usize;
    for item in items {
        let size = json_len(&item);
        if spent + size > budget && !(always_first && kept.is_empty()) {
            break;
        }
        spent += size;
        kept.push(item);
    }
    let dropped = kept.len() < total;
    (kept, dropped)
}

/// Emit `items` under a RESPONSE byte budget, NEVER returning an empty list for a lack of room: the
/// first item is always emitted. Returns the kept items and whether anything was dropped.
///
/// This is the variant a PAGED traversal needs when an item has no hard ceiling of its own: a page
/// that returns zero rows while rows remain would publish `next_offset == offset` and loop forever
/// (or stop and silently lose the tail) — see `design_inventory`'s paging contract.
pub fn under_response_budget(items: Vec<Value>, budget: usize) -> (Vec<Value>, bool) {
    fill_budget(items, budget, true)
}

/// Emit `items` STRICTLY under the budget — nothing is emitted that does not fit, so the caller's
/// total is a HARD bound rather than "the budget plus one item".
///
/// Use it when every item is itself bounded ([`bounded_row`], or a page reduced to its identity), so
/// "at least one item fits" is guaranteed by construction ([`MAX_ROW_BYTES`] < any list budget) and
/// the paging can never stall.
pub fn under_response_budget_strict(items: Vec<Value>, budget: usize) -> (Vec<Value>, bool) {
    fill_budget(items, budget, false)
}

/// The GUARD OF LAST RESORT, applied by the agent-tool ROUTERS to every response they return.
///
/// The paginated reads above cap themselves gracefully. This catches what nobody thought to cap:
/// a tool that echoes back a document the agent itself sent, a config that grew, a shape added
/// later. It keeps the response's IDENTIFYING scalars (ids, counts, flags — everything that
/// serializes small) and DROPS the oversized fields, naming each with its real byte length, so an
/// over-large answer degrades into a legible one instead of overflowing the transport.
///
/// Reaching it is a BUG in the tool, not a normal mode: `response_truncated: true` says so, and the
/// hint names the paginating arguments. It is the reason "no agent-tool response can exceed the cap"
/// is a property of the SYSTEM and not a promise each tool has to keep on its own.
pub fn bounded_tool_result(name: &str, result: Value) -> Value {
    if json_len(&result) <= MAX_RESPONSE_BYTES {
        return result;
    }
    let real = json_len(&result);
    let Some(object) = result.as_object() else {
        return json!({
            "response_truncated": true,
            "response_bytes": real,
            "response_cap": MAX_RESPONSE_BYTES,
            "note": format!("the {name} response exceeded the response cap and was dropped"),
        });
    };
    let mut kept = serde_json::Map::new();
    let mut dropped = serde_json::Map::new();
    for (key, field) in object {
        let size = json_len(field);
        if size <= IDENTITY_FIELD_BYTES * 4 {
            kept.insert(key.clone(), field.clone());
        } else {
            dropped.insert(key.clone(), json!(size));
        }
    }
    kept.insert("response_truncated".into(), json!(true));
    kept.insert("response_bytes".into(), json!(real));
    kept.insert("response_cap".into(), json!(MAX_RESPONSE_BYTES));
    kept.insert("dropped_fields".into(), Value::Object(dropped));
    kept.insert(
        "note".into(),
        json!(format!(
            "the {name} response exceeded the {MAX_RESPONSE_BYTES}-byte cap, so the oversized \
             fields were DROPPED (never cut: half a payload is not a smaller payload). Narrow the \
             call — ask for ONE item, or page with offset/limit — and read the counters."
        )),
    );
    Value::Object(kept)
}

/// An OPTIONAL non-negative integer argument as a `u64` (for `timeout_ms`).
fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_i64().ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidArgument,
                    format!("argument '{key}' must be an integer"),
                )
            })?;
            if n < 0 {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!("argument '{key}' must be >= 0"),
                ));
            }
            Ok(Some(n as u64))
        }
    }
}

/// Parse the OPTIONAL `until` argument of `wait_for_command` into the set of
/// [`RunState`]s that resolve the wait (ADR-0003 D12). `"exited"` is the alias for
/// success+error. Absent/null/empty → the default settled set `success`+`error`. Parity
/// with the Tauri `parse_until`.
fn parse_until(args: &Value) -> Result<Vec<RunState>, RpcError> {
    let default = || vec![RunState::Success, RunState::Error];
    let raw = match args.get("until") {
        None | Some(Value::Null) => return Ok(default()),
        Some(Value::Array(items)) => items,
        Some(_) => {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                "argument 'until' must be an array of state strings \
                 (idle|running|success|error|exited)",
            ))
        }
    };
    let mut states: Vec<RunState> = Vec::new();
    let push = |s: RunState, states: &mut Vec<RunState>| {
        if !states.contains(&s) {
            states.push(s);
        }
    };
    for item in raw {
        let s = item.as_str().ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                "each 'until' entry must be a state string (idle|running|success|error|exited)",
            )
        })?;
        match s.trim().to_ascii_lowercase().as_str() {
            "idle" => push(RunState::Idle, &mut states),
            "running" => push(RunState::Running, &mut states),
            "success" => push(RunState::Success, &mut states),
            "error" => push(RunState::Error, &mut states),
            "exited" => {
                push(RunState::Success, &mut states);
                push(RunState::Error, &mut states);
            }
            other => {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!(
                    "unknown 'until' state '{other}' (accepted: idle|running|success|error|exited)"
                ),
                ))
            }
        }
    }
    Ok(states)
}

// --- Identity resolution (parity with the Tauri dispatcher) ----------------

/// Build the `invalid_id` error for an id that is NOT a launchable instance: if the id
/// is actually a TEMPLATE `command_id` (a common confusion), name the correct path;
/// otherwise the generic unknown-id error. Parity with the Tauri `bad_instance_id_error`.
fn bad_instance_id_error(db: &Db, id: &str) -> RpcError {
    let is_template = db
        .with_conn(|c| db::get_template(c, id))
        .ok()
        .flatten()
        .is_some();
    if is_template {
        RpcError::new(
            RpcCode::InvalidId,
            format!(
                "'{id}' is a command TEMPLATE id (command_id), which is not launchable. \
                 Pass an instance_id from list_commands(workspace_id=…) — command_id names \
                 a project template, instance_id names a workspace's launchable instance."
            ),
        )
    } else {
        RpcError::new(
            RpcCode::InvalidId,
            format!(
                "unknown command instance {id} (if this is a command_id from \
                 list_commands(project_id=…), pass instead an instance_id from \
                 list_commands(workspace_id=…))"
            ),
        )
    }
}

/// Resolve the target instance id from either `{ instance_id }` or `{ name,
/// workspace_id }` (parity with the Tauri `resolve_instance_id`): a name is resolved
/// within the workspace; an unknown/ambiguous name is a clear error.
fn resolve_instance_id(db: &Db, args: &Value) -> Result<String, RpcError> {
    if let Some(instance_id) = optional_str(args, "instance_id")? {
        return Ok(instance_id.to_string());
    }
    let name = optional_str(args, "name")?.ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            "provide instance_id, or { name, workspace_id } to resolve by name",
        )
    })?;
    let workspace_id = optional_str(args, "workspace_id")?.ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            "resolving a command by name requires workspace_id alongside name",
        )
    })?;
    let rows = db
        .with_conn(|c| db::list_instances_for_workspace(c, workspace_id))
        .map_err(internal_db)?;
    let mut matches = rows.into_iter().filter(|r| r.name == name);
    let first = matches.next().ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidId,
            format!("no command named '{name}' in workspace {workspace_id}"),
        )
    })?;
    if let Some(second) = matches.next() {
        let mut ids = vec![first.id, second.id];
        ids.extend(matches.map(|r| r.id));
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "command name '{name}' is ambiguous in workspace {workspace_id} \
                 ({} instances: {}); pass an explicit instance_id",
                ids.len(),
                ids.join(", ")
            ),
        ));
    }
    Ok(first.id)
}

/// Resolve an instance's command line + run cwd + PERSISTED environment, mapping to the D8
/// vocabulary: an unknown instance → `invalid_id` (disambiguating a template id), an
/// invalid/missing subfolder → `invalid_argument`. Errors BEFORE any spawn. Same triple as the
/// shared `command::resolve_command_and_cwd`, with the RPC error vocabulary this surface owes its
/// callers; the env is decoded by the SAME total decoder, so a malformed one yields an empty env
/// rather than an unstartable command.
fn resolve_command_and_cwd(
    db: &Db,
    instance_id: &str,
) -> Result<crate::command::ResolvedCommand, RpcError> {
    let ctx = db
        .with_conn(|c| db::instance_run_context(c, instance_id))
        .map_err(internal_db)?;
    let ctx = match ctx {
        Some(ctx) => ctx,
        None => return Err(bad_instance_id_error(db, instance_id)),
    };
    let cwd = crate::subfolder::resolve_run_dir(&ctx.workspace_path, ctx.subfolder.as_deref())
        .map_err(|e| RpcError::new(RpcCode::InvalidArgument, e))?;
    Ok((
        ctx.command,
        cwd,
        crate::command::decode_command_env(ctx.env.as_deref()),
    ))
}

/// Compose the environment ONE agent-initiated start hands the runner: the command's PERSISTED env
/// (the base, part of its definition) with the per-run `env` argument concatenated ON TOP. The
/// spawn applies the pairs in order, so a key present in BOTH takes the PER-RUN value — the caller
/// of this one start overrides the command's own setting for this run only, and never edits it.
/// Neither layer's VALUES are ever logged.
fn merge_run_env(
    persisted: Vec<(String, String)>,
    per_run: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut merged = persisted;
    merged.extend(per_run);
    merged
}

/// Assert the instance EXISTS (a launchable instance, not a template). Used by
/// `stop_command`, which needs an id check but no cwd resolution.
fn assert_instance_exists(db: &Db, instance_id: &str) -> Result<(), RpcError> {
    let exists = db
        .with_conn(|c| db::get_instance(c, instance_id))
        .map_err(internal_db)?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(bad_instance_id_error(db, instance_id))
    }
}

// --- Status JSON (parity with the Tauri dispatcher) ------------------------

/// The status object both the action tools and `get_command_output` splat in:
/// `{ state, running, finished, exit_code, unread }`. Only a FINISHED run carries a
/// meaningful `exit_code`.
pub fn status_json(state: RunState, last_exit_code: Option<i32>, unread: bool) -> Value {
    let running = state == RunState::Running;
    let finished = matches!(state, RunState::Success | RunState::Error);
    let exit_code = if finished { last_exit_code } else { None };
    json!({
        "state": state.as_db_str(),
        "running": running,
        "finished": finished,
        "exit_code": exit_code,
        "unread": unread,
    })
}

/// The live run status read straight off the runner (after a mutation): the runner
/// outcome, or a neutral idle status if no entry exists.
fn runner_status<S: RunnerSink>(runner: &CommandRunner<S>, instance_id: &str) -> Value {
    match runner.outcome(instance_id) {
        Some((state, exit_code, unread)) => status_json(state, exit_code, unread),
        None => status_json(RunState::Idle, None, false),
    }
}

/// `{ instance_id, …status }` for the action tools.
fn status_result(instance_id: &str, status: Value) -> Value {
    let mut obj = json!({ "instance_id": instance_id });
    if let (Some(map), Some(status_map)) = (obj.as_object_mut(), status.as_object()) {
        for (k, v) in status_map {
            map.insert(k.clone(), v.clone());
        }
    }
    obj
}

// --- The four runtime command tools ----------------------------------------

/// `start_command` — `{ instance_id | (name, workspace_id), env? }` → `{ instance_id,
/// state, running, finished, exit_code, unread, was_running, restarted }`. Idempotent
/// on an already-running instance (no double-spawn; the guard is at the runner
/// boundary). Resolves command + cwd BEFORE spawning. Parity with the Tauri
/// `start_command`.
///
/// `env` (the advertised per-run KEY→VALUE string map, R-WSCMD #7) is decoded by
/// [`optional_env_map`] and MERGED, by [`merge_run_env`], ON TOP of the command's PERSISTED env
/// (`managed_commands.env`, N31) before both are applied over the inherited environment: the
/// command's own definition is the base every path honours — a gate included — and this argument is
/// a TRANSIENT override for this one start, whose keys win a collision. It is parsed BEFORE the
/// spawn, so a malformed map is a clean `invalid_argument` and never a process started with half its
/// environment. On a no-op start (already running) the runner ignores it, exactly as the schema
/// states. The VALUES of neither layer are ever logged.
pub fn start_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    let instance_id = resolve_instance_id(db, args)?;
    let per_run_env = optional_env_map(args, "env")?;
    let (command, cwd, persisted_env) = resolve_command_and_cwd(db, &instance_id)?;
    let env = merge_run_env(persisted_env, per_run_env);
    let outcome = runner
        .start_with_env(&instance_id, &command, Some(&cwd), &env)
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("start failed: {e}")))?;
    let mut result = status_result(&instance_id, runner_status(runner, &instance_id));
    if let Some(map) = result.as_object_mut() {
        map.insert("was_running".to_string(), json!(outcome.was_running));
        map.insert("restarted".to_string(), json!(false));
    }
    Ok(result)
}

/// `stop_command` — `{ instance_id }` → `{ instance_id, …status, changed,
/// was_running }`. Idempotent on a non-running instance. `changed` ⇔ the stop killed a
/// live process (a natural exit in the race window is NOT a user stop). Parity with the
/// Tauri `stop_command`.
pub fn stop_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    let instance_id = require_str(args, "instance_id")?;
    assert_instance_exists(db, instance_id)?;
    let was_running = runner.is_running(instance_id);
    let state_after = runner
        .stop(instance_id)
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("stop failed: {e}")))?;
    let changed = was_running && state_after == RunState::Idle;
    let mut result = status_result(instance_id, runner_status(runner, instance_id));
    if let Some(map) = result.as_object_mut() {
        map.insert("changed".to_string(), json!(changed));
        map.insert("was_running".to_string(), json!(was_running));
    }
    Ok(result)
}

/// `relaunch_command` — `{ instance_id, env? }` → `{ instance_id, …status, was_running,
/// restarted }`. The EXPLICIT restart (always re-spawns; never two live processes).
/// `restarted:true`. Parity with the Tauri `relaunch_command`.
///
/// `env` is decoded by [`optional_env_map`] and MERGED ON TOP of the command's PERSISTED env
/// ([`merge_run_env`], N31), both applied onto the inherited environment of the FRESH process
/// (a relaunch always spawns, so the map always applies). The VALUES are never logged.
pub fn relaunch_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    let instance_id = require_str(args, "instance_id")?;
    let per_run_env = optional_env_map(args, "env")?;
    let (command, cwd, persisted_env) = resolve_command_and_cwd(db, instance_id)?;
    let env = merge_run_env(persisted_env, per_run_env);
    let outcome = runner
        .relaunch_with_env(instance_id, &command, Some(&cwd), &env)
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("relaunch failed: {e}")))?;
    let mut result = status_result(instance_id, runner_status(runner, instance_id));
    if let Some(map) = result.as_object_mut() {
        map.insert("was_running".to_string(), json!(outcome.was_running));
        map.insert("restarted".to_string(), json!(true));
    }
    Ok(result)
}

/// `get_command_output` — `{ instance_id | (name, workspace_id), tail_bytes?, max_bytes?,
/// since?, run? ("current"|"previous"), strip_ansi?, grep?, tail_lines?, mark_read? }` →
/// `{ instance_id, run, output, total_bytes, returned_bytes, truncated, reset, cursor,
/// …status }`.
///
/// The source mirrors `bridge::command_output` + the Tauri `get_command_output`: the
/// runner's LIVE in-memory tail while running, else the persisted scrollback rehydrated
/// from the DB; the previous-run selector reads the `prev_*` columns. The window is
/// bounded by [`bound_output`]; `since`/`cursor` support incremental polling.
/// `mark_read:true` ALSO acknowledges the current run (clears `unread`) after the window
/// is computed.
///
/// RENDER KNOBS (v1 parity — the semantics are lifted from the Tauri
/// `mcp_tools::render_output`): once the RAW bounded window is computed, the `output`
/// string is reshaped by [`render_output`] — `strip_ansi` (default TRUE) cleans terminal
/// control sequences, `grep` (a regex, matched per-line on the ANSI-stripped text) keeps
/// only matching lines, and `tail_lines` keeps the last N lines (applied AFTER `grep`).
/// `max_bytes` bounds the RAW window up front (`effective_tail = min(tail_bytes,
/// max_bytes)`), the same ceiling guard as [`wait_for_command`].
///
/// INCREMENTAL INVARIANT: `total_bytes`/`returned_bytes`/`truncated`/`cursor` stay
/// computed on the RAW [`bound_output`] window — the knobs only reshape the DISPLAYED
/// `output` string. So a follow-up `get_command_output(since=cursor)` resumes on the raw
/// byte stream with no gap/dup even when knobs are set. (`returned_bytes` therefore counts
/// the RAW window bytes, NOT the rendered `output` length, matching v1.)
pub fn get_command_output<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    let instance_id = resolve_instance_id(db, args)?;
    // Byte-window knobs. `max_bytes` is an alternative ceiling: the smaller of
    // (tail_bytes, max_bytes) wins, and either above MAX_TAIL_BYTES is `output_too_large`
    // — the SAME contract `wait_for_command` enforces (v1 `parse_window_knobs` parity).
    let tail_bytes = optional_usize(args, "tail_bytes")?.unwrap_or(DEFAULT_TAIL_BYTES);
    let max_bytes = optional_usize(args, "max_bytes")?;
    let ceiling = max_bytes.unwrap_or(crate::mcp_tools_core::MAX_TAIL_BYTES);
    if tail_bytes > crate::mcp_tools_core::MAX_TAIL_BYTES
        || ceiling > crate::mcp_tools_core::MAX_TAIL_BYTES
    {
        let requested = tail_bytes.max(ceiling);
        return Err(RpcError::new(
            RpcCode::OutputTooLarge,
            format!(
                "requested window exceeds max_bytes ({})",
                crate::mcp_tools_core::MAX_TAIL_BYTES
            ),
        )
        .with_data(
            json!({ "requested": requested, "limit": crate::mcp_tools_core::MAX_TAIL_BYTES }),
        ));
    }
    let effective_tail = tail_bytes.min(ceiling);
    let since = optional_usize(args, "since")?;
    // Render knobs (v1 parity): strip_ansi defaults TRUE; grep is a per-line regex on the
    // stripped text; tail_lines keeps the last N lines AFTER grep. These reshape only the
    // displayed `output`, never the byte cursor (see the fn doc's INCREMENTAL INVARIANT).
    let strip = optional_bool(args, "strip_ansi")?.unwrap_or(true);
    let grep = optional_regex(args, "grep")?;
    let tail_lines = optional_usize(args, "tail_lines")?;
    let mark_read = optional_bool(args, "mark_read")?.unwrap_or(false);
    let previous = matches!(optional_str(args, "run")?, Some("previous"));

    let (full, status) = if previous {
        let inst = db
            .with_conn(|c| db::get_instance(c, &instance_id))
            .map_err(internal_db)?
            .ok_or_else(|| bad_instance_id_error(db, &instance_id))?;
        let prev_state = inst.prev_last_state.as_deref().map(RunState::from_db_str);
        let status = status_json(
            prev_state.unwrap_or(RunState::Idle),
            inst.prev_exit_code,
            false,
        );
        (inst.prev_scrollback, status)
    } else if let Some(live) = runner.live_output(&instance_id) {
        // Running: the live tail + the runner-backed status.
        let status = match runner.outcome(&instance_id) {
            Some((state, exit_code, unread)) => status_json(state, exit_code, unread),
            None => status_json(RunState::Idle, None, false),
        };
        (live, status)
    } else {
        // Cold: rehydrate the persisted scrollback row and derive the status from it,
        // preferring a still-live finished-this-session outcome if the runner holds one.
        let inst = db
            .with_conn(|c| db::get_instance(c, &instance_id))
            .map_err(internal_db)?
            .ok_or_else(|| bad_instance_id_error(db, &instance_id))?;
        let status = match runner.outcome(&instance_id) {
            Some((state, exit_code, unread)) => status_json(state, exit_code, unread),
            None => status_json(
                RunState::from_db_str(&inst.last_state),
                inst.last_exit_code,
                inst.unread,
            ),
        };
        (inst.scrollback, status)
    };

    // A current-run byte cursor is meaningless against the previous-run buffer.
    let effective_since = if previous { None } else { since };
    let window = bound_output(&full, effective_tail, effective_since);
    // Reshape ONLY the displayed `output` per the render knobs; every byte field below
    // (total/returned/truncated/cursor) stays on the RAW window so `since=cursor` is exact.
    let output = render_output(&window.output, strip, grep.as_ref(), tail_lines);
    let mut result = json!({
        "instance_id": instance_id,
        "run": if previous { "previous" } else { "current" },
        "output": output,
        "total_bytes": window.total_bytes,
        "returned_bytes": window.returned_bytes,
        "truncated": window.truncated,
        "reset": window.reset,
        "cursor": window.cursor,
    });
    if let (Some(map), Some(status_map)) = (result.as_object_mut(), status.as_object()) {
        for (k, v) in status_map {
            map.insert(k.clone(), v.clone());
        }
    }
    // Explicit consumption: only for the CURRENT run, after the window is computed.
    if mark_read && !previous {
        runner.acknowledge(&instance_id);
    }
    Ok(result)
}

/// The CURRENT run's full output text (pre-bounding): the runner's live in-memory tail
/// while running, else the persisted scrollback from the DB row. The SAME source the
/// current-run branch of [`get_command_output`] uses, so the `wait_for_command` cursor
/// lines up with a follow-up `get_command_output`. Parity with the Tauri `current_output`.
fn current_output<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    instance_id: &str,
) -> Result<String, RpcError> {
    if let Some(live) = runner.live_output(instance_id) {
        return Ok(live);
    }
    let scrollback = db
        .with_conn(|c| db::get_instance(c, instance_id))
        .map_err(internal_db)?
        .map(|inst| inst.scrollback)
        .unwrap_or_default();
    Ok(scrollback)
}

/// The FACTUAL state for the poll: runner-first, DB fallback. Parity with the Tauri
/// `factual_state`. Observational — never acknowledges.
fn factual_state<S: RunnerSink>(db: &Db, runner: &CommandRunner<S>, instance_id: &str) -> RunState {
    if let Some((state, _, _)) = runner.outcome(instance_id) {
        return state;
    }
    db.with_conn(|c| db::get_instance(c, instance_id))
        .ok()
        .flatten()
        .map(|inst| RunState::from_db_str(&inst.last_state))
        .unwrap_or(RunState::Idle)
}

/// The FACTUAL outcome triple `(state, exit_code, ended_at)`: runner-first for state +
/// exit_code, DB row for `ended_at` (and the cold-path fallback). Parity with the Tauri
/// `factual_outcome`.
fn factual_outcome<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    instance_id: &str,
) -> Result<(RunState, Option<i32>, Option<i64>), RpcError> {
    let live = runner.outcome(instance_id);
    let inst = db
        .with_conn(|c| db::get_instance(c, instance_id))
        .map_err(internal_db)?;
    match live {
        Some((state, exit_code, _unread)) => {
            let ended_at = inst.as_ref().and_then(|i| i.ended_at).or_else(|| {
                matches!(state, RunState::Success | RunState::Error).then(db::now_millis)
            });
            Ok((state, exit_code, ended_at))
        }
        None => match inst {
            Some(inst) => Ok((
                RunState::from_db_str(&inst.last_state),
                inst.last_exit_code,
                inst.ended_at,
            )),
            None => Ok((RunState::Idle, None, None)),
        },
    }
}

/// `wait_for_command` — `{ instance_id, until?, timeout_ms?, since?, tail_bytes?,
/// max_bytes?, strip_ansi? }` → `{ instance_id, resolved, state, exit_code, ended_at,
/// waited_ms, cursor, reset, output_tail }`. A BOUNDED, observational long-poll. Parity
/// with the Tauri `wait_for_command` (over the shared runner/DB read paths).
pub fn wait_for_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    let instance_id = require_str(args, "instance_id")?.to_string();
    assert_instance_exists(db, &instance_id)?;
    let until = parse_until(args)?;
    let timeout_ms = optional_u64(args, "timeout_ms")?.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
    let timeout = Duration::from_millis(timeout_ms).min(WAIT_MAX_TIMEOUT);

    let tail_bytes = optional_usize(args, "tail_bytes")?.unwrap_or(DEFAULT_TAIL_BYTES);
    let max_bytes = optional_usize(args, "max_bytes")?;
    let strip = optional_bool(args, "strip_ansi")?.unwrap_or(true);
    let ceiling = max_bytes.unwrap_or(crate::mcp_tools_core::MAX_TAIL_BYTES);
    if tail_bytes > crate::mcp_tools_core::MAX_TAIL_BYTES
        || ceiling > crate::mcp_tools_core::MAX_TAIL_BYTES
    {
        let requested = tail_bytes.max(ceiling);
        return Err(RpcError::new(
            RpcCode::OutputTooLarge,
            format!(
                "requested window exceeds max_bytes ({})",
                crate::mcp_tools_core::MAX_TAIL_BYTES
            ),
        )
        .with_data(
            json!({ "requested": requested, "limit": crate::mcp_tools_core::MAX_TAIL_BYTES }),
        ));
    }
    let effective_tail = tail_bytes.min(ceiling);

    // FIRST-CALL BOUNDING (D12): default `since` to the current end-of-buffer so
    // `output_tail` returns only bytes produced AFTER this call.
    let since = match optional_usize(args, "since")? {
        Some(s) => s,
        None => current_output(db, runner, &instance_id)?.len(),
    };

    let outcome = poll_until(
        &until,
        timeout,
        WAIT_POLL_INTERVAL,
        || factual_state(db, runner, &instance_id),
        std::thread::sleep,
    );

    let (state, exit_code, ended_at) = factual_outcome(db, runner, &instance_id)?;
    let finished = matches!(state, RunState::Success | RunState::Error);

    let full = current_output(db, runner, &instance_id)?;
    let window = bound_output(&full, effective_tail, Some(since));
    let output_tail = if strip {
        crate::ansi::strip_ansi(&window.output)
    } else {
        window.output.clone()
    };

    Ok(json!({
        "instance_id": instance_id,
        "resolved": outcome.resolved,
        "state": state.as_db_str(),
        "exit_code": if finished { exit_code } else { None },
        "ended_at": if finished { ended_at } else { None },
        "waited_ms": outcome.waited.as_millis() as u64,
        "cursor": window.cursor,
        "reset": window.reset,
        "output_tail": output_tail,
    }))
}

/// The single entry point a shell's MCP dispatcher calls for the runtime command
/// tools: route `name` to the matching tool, or return `None` if `name` is not one of
/// them (so the caller can fall through to its other tools / `method_not_found`).
pub fn dispatch_command_tool<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    name: &str,
    args: &Value,
) -> Option<Result<Value, RpcError>> {
    match name {
        "start_command" => Some(start_command(db, runner, args)),
        "stop_command" => Some(stop_command(db, runner, args)),
        "relaunch_command" => Some(relaunch_command(db, runner, args)),
        "get_command_output" => Some(get_command_output(db, runner, args)),
        crate::mcp::WAIT_FOR_COMMAND_TOOL => Some(wait_for_command(db, runner, args)),
        _ => None,
    }
}

// --- Workspace registration tools (V1_TOOLS: workspace_add + create_workspace) ----------
//
// These two v1 (FROZEN) MCP tools are pure FS-validate + ONE `db::create_workspace` write
// (no live PTY registry / no runner), so they are shell-agnostic: extracted here so the
// Electron core-host's MCP dispatcher serves them at parity instead of returning
// `mcp_unavailable` (PRD-5 review #59). The ONE behavioral difference from the Tauri tool
// is the post-write `workspaces://changed` sidebar-refresh event: that is a SHELL event
// seam (not a runtime dependency), so a shell that has one fires it AFTER a successful
// [`dispatch_workspace_tool`] (the return value signals the mutation succeeded), keeping
// the persistence path identical across shells.

/// Validate that `path` exists on disk AND is a directory — the `workspace_add`
/// precondition. A non-existent path or one that resolves to a FILE is rejected with the
/// D8 `invalid_argument` vocabulary, so a typo can no longer register a phantom workspace
/// that points nowhere. Symlinks are followed (a symlink to a real dir is accepted).
fn validate_existing_dir(path: &str) -> Result<(), RpcError> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "path '{path}' exists but is not a directory; workspace_add registers an \
                 existing folder (use create_workspace to create a new folder)"
            ),
        )),
        Err(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "path '{path}' does not exist; workspace_add registers an EXISTING folder \
                 (use create_workspace to create the folder first)"
            ),
        )),
    }
}

/// Ensure the directory at `path` exists, creating it AND any missing parents
/// (`mkdir -p` semantics) — the `create_workspace` creating-intent precondition. Already a
/// directory → a no-op success (idempotent); a path that exists as a FILE, or that cannot
/// be created, is rejected with the D8 `invalid_argument` vocabulary.
///
/// Returns whether THIS call created the folder (`true`) or it was already there (`false`) —
/// the signal [`compensate_created_dir`] needs to undo a *fresh* mkdir when the DB write that
/// follows fails, without ever touching a pre-existing path.
fn ensure_dir_created(path: &str) -> Result<bool, RpcError> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_dir() {
            return Ok(false); // already a directory: idempotent create, nothing to compensate.
        }
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "path '{path}' exists but is not a directory; cannot create a workspace folder there"
            ),
        ));
    }
    std::fs::create_dir_all(path)
        .map_err(|e| {
            RpcError::new(
                RpcCode::InvalidArgument,
                format!("could not create directory '{path}': {e}"),
            )
        })
        .map(|()| true)
}

/// COMPENSATE a `create_workspace` whose DB write failed AFTER its `mkdir`: remove the folder
/// this run created, and ONLY if it is still EMPTY. Deliberately `remove_dir` (never
/// `remove_dir_all`): a folder that already has content is left alone, so a racing writer's
/// data can never be destroyed by a rollback. `created == false` (a pre-existing path) is a
/// no-op — nyx never removes a path it did not create. Best-effort: a failed cleanup is not
/// worth masking the original error the caller is about to return.
///
/// LEAF-ONLY, BY DECISION (phase-1 review): [`ensure_dir_created`] goes through
/// `create_dir_all`, so ONE call can bring `a`, `a/b` AND `a/b/c` into existence, yet this
/// compensation removes only `a/b/c`. Walking back up the parents is deliberately NOT done:
/// it turns a rollback into a recursive deletion — which this PRD puts out of scope — and
/// nothing distinguishes, after the fact, a parent THIS call created from one a concurrent
/// writer created in between. A single `remove_dir` on the leaf is the one removal that is
/// provably safe (it simply fails the moment anything lives in the folder). What survives is
/// therefore at most a chain of EMPTY directories on the path the caller itself asked for —
/// never data, never a workspace row. Frozen by
/// [`tests::the_compensation_of_a_multi_segment_mkdir_removes_the_leaf_only`].
///
/// # ⚠️ READ THIS BEFORE MAKING ANY STRATEGY *FILL* THE WORKSPACE FOLDER
///
/// Leaf-only also means this compensation SILENTLY STOPS WORKING the day the folder is no
/// longer empty when it runs — and A POPULATED GIT WORKTREE IS NOT AN EMPTY FOLDER. `remove_dir`
/// simply fails, best-effort swallows the error, and the rollback cleans up NOTHING: a DB
/// failure occurring after the fill would leave an orphan worktree BOTH on disk AND in the
/// parent repo's metadata (`.git/worktrees/<name>`). The atomicity guarantee this PRD just
/// established would be gone WITHOUT A SINGLE ERROR SURFACING — which is precisely why this
/// warning is written on the function itself and not only in an overview comment.
///
/// Consequences, decided 2026-08-18 and binding on the future fill PRD (depot idea
/// `01M04W3RZ333NC01R216MNXAFK`):
///
/// * ANY strategy that puts content in the folder MUST BRING ITS OWN COMPENSATION — for a git
///   worktree: `git worktree remove --force`, then `git worktree prune` on the parent repo, so
///   neither the disk nor `.git/worktrees` keeps an orphan. That compensation must itself be
///   assumed FALLIBLE (dirty tree, lock held by another process) and stay best-effort, exactly
///   like this one: a failed cleanup must never mask the original error being returned.
/// * Do NOT "fix" this function by promoting it to `remove_dir_all` to cover the filled case.
///   The leaf-only rule is what makes a rollback structurally unable to destroy a concurrent
///   writer's data; a filling strategy owns the removal of what IT created, and only that.
///
/// The rest of the fill-strategy decisions (extension point, strategy A, branch, git safety)
/// are recorded on [`create_workspace`].
fn compensate_created_dir(path: &str, created: bool) {
    if created {
        let _ = std::fs::remove_dir(path);
    }
}

/// Last path segment of `path` (the `workspace_add` default name): the basename after the
/// final `/` or `\`, or the whole string if it has no separator.
fn basename(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Map a `db::create_workspace` failure to the ADR-0003 D8 vocabulary: a FK violation
/// (unknown project) → `invalid_id`, a UNIQUE violation (duplicate path in the project) →
/// `invalid_state`, anything else → `internal`. SQLite sometimes reports the FK/UNIQUE
/// failure as a generic constraint message, so classify on the message as a fallback.
fn map_create_workspace_err(project_id: &str, e: diesel::result::Error) -> RpcError {
    use diesel::result::{DatabaseErrorKind, Error as DieselError};
    match &e {
        DieselError::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, _) => {
            RpcError::new(RpcCode::InvalidId, format!("unknown project {project_id}"))
        }
        DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => RpcError::new(
            RpcCode::InvalidState,
            "a workspace with this path already exists in the project",
        ),
        DieselError::DatabaseError(_, info) => {
            let msg = info.message().to_ascii_lowercase();
            if msg.contains("foreign key") {
                RpcError::new(RpcCode::InvalidId, format!("unknown project {project_id}"))
            } else if msg.contains("unique") {
                RpcError::new(
                    RpcCode::InvalidState,
                    "a workspace with this path already exists in the project",
                )
            } else {
                RpcError::new(RpcCode::Internal, format!("create workspace failed: {e}"))
            }
        }
        _ => RpcError::new(RpcCode::Internal, format!("create workspace failed: {e}")),
    }
}

/// Shared body of `workspace_add` / `create_workspace`: register the workspace through the
/// SHARED use case [`crate::usecase::register_workspace_anchored`] — the ONE application
/// service the UI bridge ALSO reaches, there through the UNANCHORED
/// `usecase::register_workspace` (PDR-0005, task #7). The use case composes
/// [`crate::repository::register_workspace`] (insert + auto-detect Git repos under
/// `root_path`, done_criterion #5) AND emits a `workspace.created` domain event through the
/// injected [`crate::usecase::EventSink`] seam (task #8). Its failure is mapped to the D8
/// vocabulary. The on-disk path handling (validate-existing vs mkdir-p) is done by the
/// caller BEFORE this, so the two tools differ only in their filesystem precondition, not
/// in the persistence path.
///
/// `spec_id` (v2.5, OPTIONAL) ANCHORS that Spec on the freshly registered workspace inside the
/// SAME DB transaction: either both writes land or neither does — a refused anchor leaves NO
/// partial workspace behind. Returns `{ workspace }`, plus `spec` (the anchored Spec, whose
/// status is untouched) when a `spec_id` was given — so the no-`spec_id` payload is
/// byte-for-byte the pre-v2.5 one.
fn create_workspace_inner(
    db: &Db,
    events: &dyn crate::usecase::EventSink,
    project_id: &str,
    name: &str,
    path: &str,
    spec_id: Option<&str>,
) -> Result<Value, RpcError> {
    let (workspace, spec) = db
        .with_conn(|c| {
            crate::usecase::register_workspace_anchored(
                c,
                events,
                project_id,
                name,
                path,
                spec_id,
                crate::usecase::DomainEventSource::Mcp,
            )
        })
        .map_err(|e| match e {
            crate::usecase::RegisterWorkspaceError::Db(e) => {
                map_create_workspace_err(project_id, e)
            }
            crate::usecase::RegisterWorkspaceError::Anchor(e) => {
                crate::mcp_tools_spec::map_set_workspace_err(e)
            }
        })?;
    match spec {
        Some(spec) => Ok(json!({ "workspace": workspace, "spec": spec })),
        None => Ok(json!({ "workspace": workspace })),
    }
}

/// PREVALIDATE the optional `spec_id` of `workspace_add` / `create_workspace` — READ-ONLY,
/// through the shared [`crate::db::check_spec_anchorable`] authority — BEFORE any filesystem
/// work. An unknown Spec, a `draft`/terminal one, or one belonging to another project is
/// refused HERE, so `create_workspace` never `mkdir`s a folder for a link it was always going
/// to reject. Returns the `spec_id` (borrowed) when present.
fn prevalidate_spec_anchor<'a>(
    db: &Db,
    args: &'a Value,
    project_id: &str,
) -> Result<Option<&'a str>, RpcError> {
    // [`anchor_str`], not `optional_str`: on THIS argument the empty string would mean "register the
    // workspace unlinked" — the exact opposite of what a caller passing a `spec_id` is asking for
    // (review F2). And it is read HERE, in the read-only prevalidation, which is what puts the
    // refusal BEFORE `create_workspace`'s `mkdir`: an argument nyx will never accept must not cost a
    // folder on disk.
    let Some(spec_id) = anchor_str(
        args,
        "spec_id",
        "register the workspace with NO Spec anchored",
    )?
    else {
        return Ok(None);
    };
    db.with_conn(|c| crate::db::check_spec_anchorable(c, spec_id, project_id))
        .map_err(crate::mcp_tools_spec::map_set_workspace_err)?;
    Ok(Some(spec_id))
}

/// `workspace_add` — `{ project_id, path, name?, spec_id? }` → `{ workspace, spec? }`.
/// Registers an EXISTING on-disk folder as a workspace (the *register an existing dir* tool —
/// contrast `create_workspace`, which CREATES the folder first). The path is VALIDATED on disk
/// BEFORE the shared-use-case write; `name` defaults to the path's last segment when
/// omitted. Delegates the persistence + repo detection + domain-event emission to
/// [`crate::usecase::register_workspace_anchored`] — the same use case the UI bridge reaches,
/// but through the UNANCHORED `usecase::register_workspace`, never this DIRECT entry point.
///
/// `spec_id` (OPTIONAL) anchors that `ready`/`in_progress` Spec on the workspace in the SAME
/// transaction, without ever transitioning it. It is PREVALIDATED before the FS check.
///
/// FUTURE FILL STRATEGIES: the decisions of 2026-08-18 (extension point, the compensation trap,
/// strategy A / branch / git safety) are recorded on [`create_workspace`] and
/// [`compensate_created_dir`] — READ THEM BEFORE TOUCHING EITHER TOOL, since both serve the same
/// use case and share [`create_workspace_inner`]. They land on `create_workspace` because it is
/// the one that owns the folder's existence; `workspace_add` registers a folder someone ELSE
/// created, so it neither fills nor compensates — but a strategy that provisions a worktree
/// out-of-band and then registers it arrives through THIS entry point, with the same
/// consequences to think through.
pub fn workspace_add(
    db: &Db,
    events: &dyn crate::usecase::EventSink,
    args: &Value,
) -> Result<Value, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let path = require_str(args, "path")?;
    let name = match optional_str(args, "name")? {
        Some(n) => n.to_string(),
        None => {
            let derived = basename(path);
            if derived.is_empty() {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!(
                        "could not derive a workspace name from path '{path}' — pass an explicit `name`"
                    ),
                ));
            }
            derived
        }
    };
    // The Spec anchor is prevalidated FIRST: a doomed link never reaches the filesystem.
    let spec_id = prevalidate_spec_anchor(db, args, project_id)?;
    validate_existing_dir(path)?;
    create_workspace_inner(db, events, project_id, &name, path, spec_id)
}

/// `create_workspace` — `{ project_id, name, path, spec_id? }` → `{ workspace, spec? }`. The
/// *creating-intent* sibling of `workspace_add`: it `mkdir -p`s the folder BEFORE registering,
/// so an agent can ask nyx to track a folder that does not exist on disk yet. Both then share
/// the SAME [`crate::usecase::register_workspace_anchored`] use case — the same one the UI
/// bridge reaches through the UNANCHORED `usecase::register_workspace`.
///
/// ORDER (v2.5): the optional `spec_id` is PREVALIDATED before the `mkdir`, so a Spec that can
/// never be anchored (unknown, `draft`, terminal, other project) leaves NO folder behind. And
/// if the DB write fails AFTER a successful `mkdir`, the folder THIS call created is
/// compensated — removed only while still empty, never touching a pre-existing path
/// ([`compensate_created_dir`]).
///
/// # FUTURE FILL STRATEGIES — decisions of 2026-08-18, recorded here on purpose
///
/// This tool `mkdir`s and does NOTHING else: the folder it creates stays EMPTY — no clone, no
/// worktree — because provisioning is explicitly out of scope for this PRD. A LATER PRD (depot
/// idea `01M04W3RZ333NC01R216MNXAFK`) will add FILL STRATEGIES, a git worktree being the first.
/// The five points below were already arbitrated with the user; they live HERE, on the function
/// an agent will actually open, so that agent re-discovers nothing and forgets nothing.
///
/// 1. EXTENSION POINT. The current sequence is: prevalidation → [`ensure_dir_created`] →
///    [`create_workspace_inner`] (DB) → [`compensate_created_dir`] if the DB fails. The fill
///    inserts itself BETWEEN `ensure_dir_created` and `create_workspace_inner` (the exact spot
///    is marked in the body below). Recommended shape: a `FillStrategy` enum consumed by an
///    EXHAUSTIVE `match` with NO underscore arm — adding a variant then breaks the build at
///    every site that must handle it, instead of silently falling through to the wrong
///    behaviour. That pattern was just validated on this very PRD: see `SpecAnchor` in
///    [`crate::mcp_context`] and the justification carried on its declaration. Do NOT build a
///    speculative plugin abstraction while only ONE real strategy exists.
///
/// 2. THE MAIN TRAP, AND THE COSTLIEST ONE TO FORGET: compensation. [`compensate_created_dir`]
///    does a LEAF-ONLY `remove_dir`, deliberately, so it will NOT clean a folder that has
///    content — and a populated git worktree is not empty. Every filling strategy must
///    therefore supply its own compensation (`git worktree remove --force`, then `prune`), and
///    that compensation can itself fail. The full warning is written on that function: read it
///    before writing any strategy.
///
/// 3. STRATEGY A — the only one planned for the first PRD: the source repo is THE ROOT, i.e.
///    the `root_path` of the project's PRIMARY workspace (`projects.primary_workspace_id`).
///    Design constraint to keep in mind, because it shapes the whole thing: nyx NEVER SCANS a
///    workspace to guess its repos (see the header of [`crate::repository`]) — repos are
///    EXPLICIT `relative_path` + `name` associations. So when the project declares explicit
///    repos, the root is the one whose `relative_path` carries the MONO-REPO convention
///    [`crate::repository::MONO_REPO_RELATIVE_PATH`] — `"."`, not the empty string. That is the
///    canonical stored value everywhere (the module header of [`crate::repository`], the
///    synthetic current-folder fallback); the empty string is tolerated ONLY as an alias when
///    JOINING a path (`repository::join_relative`), so a selector that looks for it finds
///    nothing on a normally configured mono-repo. Flat refusal if that path is not a git repo.
///    Note also that `relative_path` ALREADY EXISTS in the model, so a future sub-repo strategy
///    slots in without any redesign.
///
/// 4. BRANCH. A new branch is created SYSTEMATICALLY, its name derived from the workspace name.
///    Base = the LOCAL HEAD of the source repo, NOT `origin/<default>`: no network access, and
///    predictable behaviour offline. If the branch already exists, git refuses on its own —
///    surface that error AS IS rather than inventing a suffix.
///
/// 5. GIT SAFETY. gix 0.85 is the PRIMARY backend, but for READING; it does not write worktrees.
///    Worktree WRITES therefore go through the CLI fallback [`crate::proc_util::command`]
///    (`"git"`), already used by [`crate::git_diff`]. That is an ASSUMED choice, not an
///    oversight. `git worktree add` is atomic and takes its own locks, which sharply reduces the
///    safety work left on the nyx side; what remains is mapping git errors onto the [`RpcCode`]
///    vocabulary, on the model of [`map_create_workspace_err`], which already does exactly that
///    for diesel errors.
pub fn create_workspace(
    db: &Db,
    events: &dyn crate::usecase::EventSink,
    args: &Value,
) -> Result<Value, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let name = require_str(args, "name")?;
    let path = require_str(args, "path")?;
    // Every validation that can be done WITHOUT touching the disk happens first.
    let spec_id = prevalidate_spec_anchor(db, args, project_id)?;
    let created = ensure_dir_created(path)?;
    // ↓↓↓ THE FILL STRATEGY INSERTS ITSELF EXACTLY HERE ↓↓↓ — after the folder exists, before the
    // DB write. Today the folder stays EMPTY (provisioning is out of scope). See the
    // "FUTURE FILL STRATEGIES" section on this function for the five decisions of 2026-08-18, and
    // [`compensate_created_dir`] for the compensation trap: a filled folder is NOT cleaned by the
    // rollback below, so a strategy that fills MUST bring its own compensation here.
    create_workspace_inner(db, events, project_id, name, path, spec_id).inspect_err(|_| {
        // The DB half failed: undo the folder we just created (only if still empty).
        compensate_created_dir(path, created);
    })
}

/// The single entry point a shell's MCP dispatcher calls for the workspace registration
/// tools (`workspace_add` / `create_workspace`): route `name` to the matching tool, or
/// `None` if `name` is not one of them. A successful `Some(Ok(_))` signals a committed
/// mutation, so a shell with a sidebar-refresh event seam can fire it after this returns.
///
/// `events` is the [`crate::usecase::EventSink`] the shell injects: a real shell passes its
/// Phase-4 sink (a [`crate::usecase::NullEventSink`] in Phase 3, since no consumer exists
/// yet); a test passes a recording sink to assert the emitted domain events (task #8).
pub fn dispatch_workspace_tool(
    db: &Db,
    events: &dyn crate::usecase::EventSink,
    name: &str,
    args: &Value,
) -> Option<Result<Value, RpcError>> {
    match name {
        "workspace_add" => Some(workspace_add(db, events, args)),
        "create_workspace" => Some(create_workspace(db, events, args)),
        _ => None,
    }
}

// --- Bounded output window (extracted from the Tauri dispatcher) ------------

/// The bounded output window of [`bound_output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputWindow {
    pub output: String,
    pub total_bytes: usize,
    pub returned_bytes: usize,
    pub truncated: bool,
    pub reset: bool,
    pub cursor: usize,
}

/// Compute the bounded output window (ADR-0003 D7) — extracted VERBATIM from the Tauri
/// `mcp_tools::bound_output` so the byte cursor / paging behavior is identical. Operates
/// on BYTES, snapping cut points to UTF-8 char boundaries.
pub fn bound_output(full: &str, tail_bytes: usize, since: Option<usize>) -> OutputWindow {
    let bytes = full.as_bytes();
    let total_bytes = bytes.len();

    let reset = since.is_some_and(|s| s > total_bytes);
    let since = if reset { None } else { since };

    let start_after = since.unwrap_or(0).min(total_bytes);
    let start_after = ceil_char_boundary(full, start_after);
    let remaining = &bytes[start_after..];

    let (raw_start, raw_end, truncated) = if remaining.len() > tail_bytes {
        if since.is_some() {
            (start_after, start_after + tail_bytes, true)
        } else {
            (
                start_after + (remaining.len() - tail_bytes),
                total_bytes,
                true,
            )
        }
    } else {
        (start_after, total_bytes, false)
    };
    let window_start = ceil_char_boundary(full, raw_start);
    let window_end = ceil_char_boundary(full, raw_end)
        .min(total_bytes)
        .max(window_start);

    let slice = &full[window_start..window_end];
    OutputWindow {
        output: slice.to_string(),
        total_bytes,
        returned_bytes: slice.len(),
        truncated,
        reset,
        cursor: window_end,
    }
}

/// Round `idx` UP to the next UTF-8 char boundary in `s` (clamped to `s.len()`).
fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Read the OPTIONAL `grep` regex argument for `get_command_output` (v1
/// `mcp_tools::optional_regex` parity). `None` when absent/null/empty; a present
/// non-string or an invalid regex is an `invalid_argument` error naming the compile
/// failure so the agent can fix the pattern. Matched per-line on the ANSI-stripped text.
fn optional_regex(args: &Value, key: &str) -> Result<Option<regex::Regex>, RpcError> {
    match optional_str(args, key)? {
        None => Ok(None),
        Some("") => Ok(None),
        Some(pat) => regex::Regex::new(pat).map(Some).map_err(|e| {
            RpcError::new(
                RpcCode::InvalidArgument,
                format!("argument '{key}' is not a valid regex: {e}"),
            )
        }),
    }
}

/// Shape the `output` field of `get_command_output` from a RAW byte window, per the
/// v1 `mcp_tools::render_output` contract. The byte cursor/total are computed by the
/// caller on the RAW window, so reducing the rendered text here NEVER desyncs incremental
/// polling.
///
/// - `strip` (default true at the call site): when set, the window is run through
///   [`crate::ansi::strip_ansi`] and the cleaned text REPLACES `output` (one cleaned view,
///   no parallel raw+text). When false the raw window is returned verbatim. The line modes
///   below ALWAYS MATCH on the ANSI-stripped per-line text (regex anchors on readable
///   text), but the EMITTED lines honor `strip` (stripped when true, raw when false).
/// - `grep`: keep only the lines whose stripped form matches the regex. Applied BEFORE
///   `tail_lines`.
/// - `tail_lines`: keep at most the last N lines (after any `grep`). `n == 0` means "no
///   line cap" (keep all), NOT "drop everything" — draining to zero would report empty
///   output while `returned_bytes > 0`.
fn render_output(
    raw_window: &str,
    strip: bool,
    grep: Option<&regex::Regex>,
    tail_lines: Option<usize>,
) -> String {
    let cleaned = crate::ansi::strip_ansi(raw_window);

    // Fast path: no line modes → just honor `strip`.
    if grep.is_none() && tail_lines.is_none() {
        return if strip {
            cleaned
        } else {
            raw_window.to_string()
        };
    }

    // Line modes work line-by-line. We match on the cleaned per-line text but emit the
    // stripped or raw line to honor `strip`. In RAW mode split on '\n' WITHOUT dropping a
    // preceding '\r' (`str::lines` would rewrite CRLF→LF and break the raw-verbatim
    // promise); a trailing '\n' yields a final empty element we drop.
    let emit_source = if strip { cleaned.as_str() } else { raw_window };
    let emit_lines: Vec<&str> = if strip {
        emit_source.lines().collect()
    } else {
        let mut v: Vec<&str> = emit_source.split('\n').collect();
        if emit_source.ends_with('\n') {
            v.pop();
        }
        v
    };
    let mut kept: Vec<String> = Vec::new();
    for line in &emit_lines {
        // The haystack is the cleaned form: when strip=true the emitted line IS cleaned;
        // when strip=false strip it so embedded escapes never foil the pattern.
        let hay = if strip {
            (*line).to_string()
        } else {
            crate::ansi::strip_ansi(line)
        };
        if let Some(re) = grep {
            if !re.is_match(&hay) {
                continue;
            }
        }
        kept.push((*line).to_string());
    }
    if let Some(n) = tail_lines {
        if n > 0 && kept.len() > n {
            kept.drain(0..kept.len() - n);
        }
    }
    let trailing_nl = emit_source.ends_with('\n');
    let mut out = kept.join("\n");
    if trailing_nl && !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`bounded_row`] cuts PROSE and reports its real length, but DROPS a `*_json` payload whole —
    /// a prefix of a machine document does not parse, so half of it is not a smaller payload but a
    /// corrupt one (review 01KYMF0KFZQG5JYZDGZCR85BAM).
    #[test]
    fn bounded_row_cuts_prose_and_drops_machine_payloads_whole() {
        let row = json!({
            "id": "01ABC",
            "body": "é".repeat(MAX_ROW_FIELD_BYTES),
            "composition_json": "{\"component\":\"Page\"}".repeat(MAX_ROW_FIELD_BYTES),
            "position": 3,
        });
        let bounded = bounded_row(&row);
        assert_eq!(bounded["id"], json!("01ABC"), "the identity is untouched");
        assert_eq!(bounded["position"], json!(3), "so are the scalars");
        let body = bounded["body"].as_str().expect("prose survives, shortened");
        assert!(body.len() <= MAX_ROW_FIELD_BYTES && !body.is_empty());
        assert!(
            body.chars().all(|c| c == 'é'),
            "a multi-byte character is never split",
        );
        assert!(
            bounded["composition_json"].is_null(),
            "a machine payload is dropped WHOLE, never cut into invalid JSON",
        );
        // Both cuts are NAMED with the field's REAL byte length.
        assert_eq!(
            bounded["truncated_fields"]["body"],
            json!(MAX_ROW_FIELD_BYTES * 2),
        );
        assert!(bounded["truncated_fields"]["composition_json"]
            .as_u64()
            .is_some());
        // A row that fits is returned verbatim (no noise on the normal path).
        let small = json!({ "id": "x", "body": "short" });
        assert_eq!(bounded_row(&small), small);
    }

    /// The router's guard of last resort keeps a response's IDENTIFYING scalars and drops only the
    /// oversized fields, naming each with its real weight — an over-large answer degrades into a
    /// legible one instead of overflowing the transport.
    #[test]
    fn bounded_tool_result_degrades_an_oversized_answer_into_a_legible_one() {
        let fits = json!({ "prototype_id": "p1", "ok": true });
        assert_eq!(bounded_tool_result("proto_get", fits.clone()), fits);

        let huge = json!({
            "prototype_id": "p1",
            "variant": { "composition_json": "x".repeat(MAX_RESPONSE_BYTES * 2) },
        });
        let capped = bounded_tool_result("proto_add_variant", huge);
        assert!(json_len(&capped) <= MAX_RESPONSE_BYTES);
        assert_eq!(capped["prototype_id"], json!("p1"), "the id survives");
        assert_eq!(capped["response_truncated"], json!(true));
        assert_eq!(capped["response_cap"], json!(MAX_RESPONSE_BYTES));
        assert!(
            capped["dropped_fields"]["variant"].as_u64().unwrap() as usize > MAX_RESPONSE_BYTES,
            "what was dropped is named with its real weight",
        );
        assert!(capped["note"]
            .as_str()
            .unwrap()
            .contains("proto_add_variant"));
    }

    /// The two emission modes: [`under_response_budget`] never returns an empty page (a paged
    /// traversal must always progress), [`under_response_budget_strict`] never exceeds the budget
    /// (the caller's total is then a HARD bound).
    #[test]
    fn the_two_budget_modes_differ_only_on_an_item_that_does_not_fit() {
        let big = json!("x".repeat(1000));
        let items = vec![big.clone(), big.clone(), big];
        let (lenient, dropped) = under_response_budget(items.clone(), 10);
        assert_eq!(lenient.len(), 1, "the first item always comes out");
        assert!(dropped);
        let (strict, dropped) = under_response_budget_strict(items.clone(), 10);
        assert!(strict.is_empty(), "nothing that does not fit is emitted");
        assert!(dropped);
        // With room, both behave identically.
        assert_eq!(
            under_response_budget(items.clone(), 100_000).0.len(),
            under_response_budget_strict(items, 100_000).0.len(),
        );
    }

    #[test]
    fn bound_output_under_tail_returns_all_untruncated() {
        let w = bound_output("hello", 1024, None);
        assert_eq!(w.output, "hello");
        assert_eq!(w.total_bytes, 5);
        assert_eq!(w.returned_bytes, 5);
        assert!(!w.truncated);
        assert!(!w.reset);
        assert_eq!(w.cursor, 5);
    }

    #[test]
    fn bound_output_keeps_the_tail_and_flags_truncation() {
        // 10 bytes, tail 4 → keep the LAST 4 ("6789"), cursor at the end.
        let w = bound_output("0123456789", 4, None);
        assert_eq!(w.output, "6789");
        assert!(w.truncated);
        assert_eq!(w.cursor, 10);
        assert_eq!(w.total_bytes, 10);
    }

    #[test]
    fn bound_output_since_pages_forward_contiguously() {
        // since=2, tail 3 → keep the FIRST 3 of the new region ("234"), cursor at 5.
        let w = bound_output("0123456789", 3, Some(2));
        assert_eq!(w.output, "234");
        assert!(w.truncated);
        assert_eq!(w.cursor, 5);
        // Next page resumes with no gap/dup.
        let w2 = bound_output("0123456789", 3, Some(5));
        assert_eq!(w2.output, "567");
        assert_eq!(w2.cursor, 8);
    }

    #[test]
    fn bound_output_since_past_end_signals_reset() {
        // The buffer shrank: since beyond the end → reset + a fresh tail read.
        let w = bound_output("short", 1024, Some(99));
        assert!(w.reset);
        assert_eq!(w.output, "short");
        assert_eq!(w.cursor, 5);
    }

    // --- render knobs (v1 mcp_tools::render_output parity) ---------------------

    #[test]
    fn render_output_strips_ansi_by_default() {
        // strip=true (the call-site default) → ANSI/control codes gone.
        let raw = "\u{1b}[31merror: boom\u{1b}[0m\n";
        assert_eq!(render_output(raw, true, None, None), "error: boom\n");
    }

    #[test]
    fn render_output_strip_false_returns_raw_bytes() {
        // strip=false → the window verbatim, escapes preserved.
        let raw = "\u{1b}[31merror: boom\u{1b}[0m\n";
        assert_eq!(render_output(raw, false, None, None), raw);
    }

    #[test]
    fn render_output_grep_keeps_only_matching_lines() {
        let raw = "start\nERROR: boom\ninfo: ok\nERROR: bang\n";
        let re = optional_regex(&json!({ "grep": "ERROR" }), "grep")
            .unwrap()
            .unwrap();
        assert_eq!(
            render_output(raw, true, Some(&re), None),
            "ERROR: boom\nERROR: bang\n"
        );
    }

    #[test]
    fn render_output_grep_matches_on_stripped_text_even_when_strip_false() {
        // The pattern anchors on the ANSI-stripped line, but strip=false emits the RAW
        // line (escapes preserved) so the match works despite embedded color codes.
        let raw = "info\n\u{1b}[31mERROR: boom\u{1b}[0m\n";
        let re = optional_regex(&json!({ "grep": "ERROR" }), "grep")
            .unwrap()
            .unwrap();
        // The emitted line is the RAW (colored) line; the trailing newline of the source
        // is preserved. Only the non-matching "info" line was dropped.
        assert_eq!(
            render_output(raw, false, Some(&re), None),
            "\u{1b}[31mERROR: boom\u{1b}[0m\n"
        );
    }

    #[test]
    fn render_output_tail_lines_keeps_last_n_of_fifteen() {
        // 15 lines, tail_lines:5 → exactly the LAST 5 (the reported bug: 5 was ignored).
        let raw: String = (1..=15).map(|n| format!("line{n}\n")).collect();
        let out = render_output(&raw, true, None, Some(5));
        assert_eq!(out, "line11\nline12\nline13\nline14\nline15\n");
        assert_eq!(out.lines().count(), 5, "exactly 5 lines, not 15");
    }

    #[test]
    fn render_output_grep_then_tail_lines_compose() {
        // grep FIRST (keep ERROR lines), THEN tail_lines (last N of the matches).
        let raw = "ERROR a\nok\nERROR b\nok\nERROR c\n";
        let re = optional_regex(&json!({ "grep": "ERROR" }), "grep")
            .unwrap()
            .unwrap();
        assert_eq!(
            render_output(raw, true, Some(&re), Some(2)),
            "ERROR b\nERROR c\n"
        );
    }

    #[test]
    fn render_output_tail_lines_zero_keeps_all() {
        // n==0 means "no cap" (keep all), NOT "drop everything".
        let raw = "a\nb\nc\n";
        assert_eq!(render_output(raw, true, None, Some(0)), "a\nb\nc\n");
    }

    #[test]
    fn optional_regex_absent_or_empty_is_none_invalid_is_error() {
        assert!(optional_regex(&json!({}), "grep").unwrap().is_none());
        assert!(optional_regex(&json!({ "grep": "" }), "grep")
            .unwrap()
            .is_none());
        let err = optional_regex(&json!({ "grep": "(" }), "grep").unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
    }

    /// max_bytes acts as an alternative byte ceiling: the effective window is
    /// `min(tail_bytes, max_bytes)` computed on the RAW stream BEFORE rendering, so a
    /// tight `max_bytes` caps `returned_bytes`/`output` the same way a tight `tail_bytes`
    /// would (v1 `parse_window_knobs` parity).
    #[test]
    fn max_bytes_caps_the_raw_window() {
        let full = "0123456789";
        let effective_tail = 10usize.min(4); // tail_bytes=10, max_bytes=4 → 4 wins
        let w = bound_output(full, effective_tail, None);
        assert_eq!(w.returned_bytes, 4);
        assert_eq!(render_output(&w.output, true, None, None), "6789");
    }

    /// THE INCREMENTAL INVARIANT: the render knobs reshape ONLY the displayed `output`;
    /// `bound_output`'s `cursor`/`returned_bytes` are byte-exact on the RAW window, so a
    /// follow-up read with `since=cursor` resumes with no gap/dup — regardless of knobs.
    #[test]
    fn knobs_never_move_the_raw_cursor_so_since_stays_contiguous() {
        // A window carrying ANSI + many lines. First read: default knobs.
        let full = "\u{1b}[32mline1\u{1b}[0m\nline2\nERROR3\nline4\n";
        let w1 = bound_output(full, DEFAULT_TAIL_BYTES, None);
        let rendered = render_output(&w1.output, true, None, Some(2));
        // The rendered output is reshaped (stripped + last 2 lines)…
        assert_eq!(rendered, "ERROR3\nline4\n");
        // …but the cursor/returned_bytes still describe the RAW window byte-for-byte.
        assert_eq!(w1.returned_bytes, full.len());
        assert_eq!(w1.cursor, full.len());

        // Same window, but with a grep knob instead: the RAW cursor is IDENTICAL.
        let re = optional_regex(&json!({ "grep": "ERROR" }), "grep")
            .unwrap()
            .unwrap();
        let w1b = bound_output(full, DEFAULT_TAIL_BYTES, None);
        let rendered_b = render_output(&w1b.output, true, Some(&re), None);
        assert_eq!(rendered_b, "ERROR3\n");
        assert_eq!(w1b.cursor, w1.cursor, "grep does not move the raw cursor");

        // New bytes arrive; a follow-up read with since=cursor resumes on the RAW stream
        // with NO gap/dup, proving the knobs never desynced the incremental protocol.
        let grown = format!("{full}line5\nline6\n");
        let w2 = bound_output(&grown, DEFAULT_TAIL_BYTES, Some(w1.cursor));
        assert!(!w2.reset);
        assert_eq!(
            w2.output, "line5\nline6\n",
            "resumes exactly after the cursor"
        );
        assert_eq!(w2.cursor, grown.len());
    }

    #[test]
    fn status_json_only_finished_carries_exit_code() {
        let running = status_json(RunState::Running, Some(7), false);
        assert_eq!(running["running"], json!(true));
        assert_eq!(running["finished"], json!(false));
        assert_eq!(
            running["exit_code"],
            json!(null),
            "running has no exit code"
        );

        let err = status_json(RunState::Error, Some(2), true);
        assert_eq!(err["finished"], json!(true));
        assert_eq!(err["exit_code"], json!(2));
        assert_eq!(err["unread"], json!(true));
    }

    // --- Workspace registration tools (PRD-5 review #59) -----------------------

    /// `Db::in_memory()` opens the process-shared `file::memory:?cache=shared` DB, so two
    /// instances built concurrently race the same migration ("schema is locked"). The
    /// workspace tests each build their own `Db`, so serialize them behind one lock (the
    /// SAME pattern the cargo test harness would otherwise need `--test-threads=1` for).
    fn db_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn temp_subdir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("nyx-ws-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `workspace_add` validates the dir exists, then writes via `db::create_workspace`,
    /// returning `{ workspace }` — at parity with the Tauri tool (no `mcp_unavailable`).
    #[test]
    fn workspace_add_registers_an_existing_dir() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/p", None))
            .unwrap();
        let existing = temp_subdir("add");
        let sink = crate::usecase::test_support::RecordingEventSink::new();

        let res = dispatch_workspace_tool(
            &db,
            &sink,
            "workspace_add",
            &json!({ "project_id": project.id, "path": existing.to_string_lossy() }),
        )
        .expect("workspace_add is dispatched")
        .expect("workspace_add ok");
        let ws = &res["workspace"];
        assert_eq!(ws["project_id"], json!(project.id));
        // Name defaulted to the path's last segment.
        assert_eq!(
            ws["name"],
            json!(existing.file_name().unwrap().to_string_lossy())
        );
        // The MCP tool went through the shared use case, which emitted the domain event.
        assert_eq!(
            sink.kinds(),
            vec![crate::usecase::DomainEventKind::WorkspaceCreated],
            "workspace_add emitted a workspace.created domain event via the shared use case"
        );

        let _ = std::fs::remove_dir_all(&existing);
    }

    /// `workspace_add` rejects a path that does not exist with the D8 `invalid_argument`
    /// vocabulary, never registering a phantom workspace.
    #[test]
    fn workspace_add_rejects_a_missing_path() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/p2", None))
            .unwrap();
        let err = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "workspace_add",
            &json!({ "project_id": project.id, "path": "/nyx/does/not/exist/zzz" }),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
    }

    /// `create_workspace` `mkdir -p`s the folder first, then registers it — so a path that
    /// does not exist yet is created and tracked (creating intent, vs `workspace_add`).
    #[test]
    fn create_workspace_makes_the_dir_then_registers() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/p3", None))
            .unwrap();
        let base = temp_subdir("create");
        let fresh = base.join("nested/new-ws");
        assert!(!fresh.exists());

        let res = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "create_workspace",
            &json!({ "project_id": project.id, "name": "WS", "path": fresh.to_string_lossy() }),
        )
        .expect("create_workspace is dispatched")
        .expect("create_workspace ok");
        assert_eq!(res["workspace"]["name"], json!("WS"));
        assert!(fresh.is_dir(), "create_workspace mkdir -p'd the path");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// An unknown project id surfaces as the D8 `invalid_id` (FK) vocabulary.
    #[test]
    fn create_workspace_unknown_project_is_invalid_id() {
        let _g = db_guard();
        let db = Db::in_memory();
        let base = temp_subdir("fk");
        let err = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "create_workspace",
            &json!({ "project_id": "nope", "name": "WS", "path": base.to_string_lossy() }),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `dispatch_workspace_tool` returns `None` for a non-workspace tool name (so a caller
    /// falls through to its other tools).
    #[test]
    fn dispatch_workspace_tool_ignores_unrelated_names() {
        let _g = db_guard();
        let db = Db::in_memory();
        let sink = crate::usecase::NullEventSink;
        assert!(dispatch_workspace_tool(&db, &sink, "list_projects", &json!({})).is_none());
    }

    /// CONVERGENCE (task #7, PDR-0005): the UI bridge and the MCP surface share ONE
    /// application service for "create a workspace" — `usecase::register_workspace_anchored`.
    /// Since v2.5 they enter it through DISTINCT entry points and converge only INSIDE it.
    /// This test drives BOTH paths against the SAME in-process authority and asserts they
    /// produce the SAME persisted effect (a workspace row) AND the SAME domain event —
    /// proving neither path carries a private business path.
    ///
    /// - MCP path: `dispatch_workspace_tool("create_workspace", …)` (the exact call the
    ///   sidecar dispatcher makes) → `create_workspace_inner` → the anchored use case
    ///   DIRECTLY, so the tool can carry its optional `spec_id`.
    /// - UI path: `usecase::register_workspace` (the exact call the UI bridge's
    ///   `create_workspace` db-command makes — see `db_command.rs`): the UNANCHORED wrapper,
    ///   which reaches the same use case with `spec_id = None`.
    #[test]
    fn ui_and_mcp_create_workspace_share_one_use_case() {
        use crate::usecase::{self, test_support::RecordingEventSink, DomainEventKind};
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/conv", None))
            .unwrap();
        let base = temp_subdir("conv");
        let mcp_dir = base.join("mcp-ws");
        let ui_dir = base.join("ui-ws");
        std::fs::create_dir_all(&mcp_dir).unwrap();
        std::fs::create_dir_all(&ui_dir).unwrap();

        // MCP path — the dispatcher entry point, with a recording sink.
        let mcp_sink = RecordingEventSink::new();
        let mcp_res = dispatch_workspace_tool(
            &db,
            &mcp_sink,
            "create_workspace",
            &json!({ "project_id": project.id, "name": "mcp", "path": mcp_dir.to_string_lossy() }),
        )
        .expect("dispatched")
        .expect("ok");
        let mcp_ws_id = mcp_res["workspace"]["id"].as_str().unwrap().to_string();

        // UI path — the UNANCHORED wrapper the db-command bridge calls, with its own sink.
        let ui_sink = RecordingEventSink::new();
        let ui_ws = db
            .with_conn(|c| {
                usecase::register_workspace(
                    c,
                    &ui_sink,
                    &project.id,
                    "ui",
                    &ui_dir.to_string_lossy(),
                    usecase::DomainEventSource::Ui,
                )
            })
            .unwrap();

        // Both persisted a real workspace under the SAME project.
        let live = db
            .with_conn(|c| db::list_workspaces(c, &project.id))
            .unwrap();
        assert!(live.iter().any(|w| w.id == mcp_ws_id));
        assert!(live.iter().any(|w| w.id == ui_ws.id));

        // Both emitted the SAME domain-event kind through the SAME use case's seam.
        assert_eq!(mcp_sink.kinds(), vec![DomainEventKind::WorkspaceCreated]);
        assert_eq!(ui_sink.kinds(), vec![DomainEventKind::WorkspaceCreated]);

        let _ = std::fs::remove_dir_all(&base);
    }

    // --- `spec_id` anchoring on the workspace tools (v2.5) -----------------

    /// Seed a project plus a Spec driven to `status`, returning `(db, project_id, spec_id)`.
    /// `ready` and `in_progress` are the two ANCHORABLE statuses; `draft` is the refused one.
    fn seeded_project_with_spec(status: &str) -> (Db, String, String) {
        let db = Db::in_memory();
        let (project_id, spec_id) = db.with_conn(|c| {
            let (project, _root) = db::create_project(c, "P", "/tmp/spec-anchor", None).unwrap();
            let sections = db::SpecSections {
                problem: "p".into(),
                solution: "s".into(),
                ..db::SpecSections::empty()
            };
            let (spec, _) = db::create_spec(c, &project.id, "S", sections).unwrap();
            if status != db::SPEC_STATUS_DRAFT {
                db::transition_spec(c, &spec.id, db::SpecTransition::Ready, true).unwrap();
            }
            if status == db::SPEC_STATUS_IN_PROGRESS {
                db::transition_spec(c, &spec.id, db::SpecTransition::Start, true).unwrap();
            }
            (project.id, spec.id)
        });
        (db, project_id, spec_id)
    }

    /// DONE-CRITERION: "avec `spec_id`, les deux outils retournent un workspace dont la Spec
    /// ready/in_progress est déjà ancrée et dont le statut est inchangé". Drives BOTH tools
    /// (`create_workspace` on a fresh folder, `workspace_add` on an existing one) and asserts
    /// the returned workspace carries the anchor AND that the Spec never transitioned.
    #[test]
    fn both_workspace_tools_anchor_the_spec_without_transitioning_it() {
        let _g = db_guard();
        for status in [db::SPEC_STATUS_READY, db::SPEC_STATUS_IN_PROGRESS] {
            let (db, project_id, spec_id) = seeded_project_with_spec(status);
            let base = temp_subdir("anchor");

            // --- create_workspace: the folder does not exist yet.
            let fresh = base.join("created");
            let res = dispatch_workspace_tool(
                &db,
                &crate::usecase::NullEventSink,
                "create_workspace",
                &json!({
                    "project_id": project_id,
                    "name": "created",
                    "path": fresh.to_string_lossy(),
                    "spec_id": spec_id,
                }),
            )
            .expect("dispatched")
            .expect("create_workspace with spec_id ok");
            let ws_id = res["workspace"]["id"].as_str().unwrap().to_string();
            assert_eq!(
                res["spec"]["workspace_id"],
                json!(ws_id),
                "the tool echoes the Spec ALREADY anchored on the new workspace ({status})"
            );
            assert_eq!(
                res["spec"]["status"],
                json!(status),
                "anchoring NEVER transitions the Spec ({status})"
            );
            // The anchor is PERSISTED, not just echoed.
            let stored = db
                .with_conn(|c| db::get_spec(c, &spec_id))
                .unwrap()
                .unwrap();
            assert_eq!(stored.workspace_id.as_deref(), Some(ws_id.as_str()));
            assert_eq!(stored.status, status, "the status is untouched in the DB");

            // --- workspace_add: an EXISTING folder, re-anchoring the same Spec elsewhere.
            let existing = base.join("added");
            std::fs::create_dir_all(&existing).unwrap();
            let res = dispatch_workspace_tool(
                &db,
                &crate::usecase::NullEventSink,
                "workspace_add",
                &json!({
                    "project_id": project_id,
                    "path": existing.to_string_lossy(),
                    "spec_id": spec_id,
                }),
            )
            .expect("dispatched")
            .expect("workspace_add with spec_id ok");
            let added_id = res["workspace"]["id"].as_str().unwrap().to_string();
            assert_eq!(res["spec"]["workspace_id"], json!(added_id));
            assert_eq!(res["spec"]["status"], json!(status));
            let stored = db
                .with_conn(|c| db::get_spec(c, &spec_id))
                .unwrap()
                .unwrap();
            assert_eq!(
                stored.workspace_id.as_deref(),
                Some(added_id.as_str()),
                "workspace_add moved the anchor to the registered folder"
            );

            let _ = std::fs::remove_dir_all(&base);
        }
    }

    /// DONE-CRITERION: "draft, état terminal, Spec inconnue ou autre projet échouent avant
    /// écriture DB et avant `mkdir`". Every refused `spec_id` leaves the filesystem UNTOUCHED
    /// (no folder created) and the project workspace-less.
    #[test]
    fn create_workspace_refuses_a_bad_spec_before_touching_the_filesystem() {
        let _g = db_guard();
        let base = temp_subdir("prevalidate");

        // A REFUSED case is driven as (label, db, project_id, spec_id, expected code, a needle
        // the message must carry — the wording is agent-facing, so it is locked too).
        let cases: Vec<(&str, Db, String, String, RpcCode, &str)> = {
            let mut v = Vec::new();
            // draft → invalid_state (still being authored).
            let (db, project_id, spec_id) = seeded_project_with_spec(db::SPEC_STATUS_DRAFT);
            v.push((
                "draft",
                db,
                project_id,
                spec_id,
                RpcCode::InvalidState,
                "can only be set while it is ready or in_progress",
            ));
            // terminal (cancelled) → invalid_state.
            let (db, project_id, spec_id) = seeded_project_with_spec(db::SPEC_STATUS_READY);
            db.with_conn(|c| db::transition_spec(c, &spec_id, db::SpecTransition::Cancel, true))
                .unwrap();
            v.push((
                "cancelled",
                db,
                project_id,
                spec_id,
                RpcCode::InvalidState,
                "can only be set while it is ready or in_progress",
            ));
            // unknown Spec → invalid_id.
            let (db, project_id, _) = seeded_project_with_spec(db::SPEC_STATUS_READY);
            v.push((
                "unknown",
                db,
                project_id,
                "no-such-spec".to_string(),
                RpcCode::InvalidId,
                "unknown spec",
            ));
            // a Spec of ANOTHER project → invalid_argument. The refusal is worded against the
            // TARGET project: on this path NO workspace exists yet to belong to one.
            let (db, _spec_project, spec_id) = seeded_project_with_spec(db::SPEC_STATUS_READY);
            let foreign = db
                .with_conn(|c| db::create_project(c, "Other", "/tmp/other", None))
                .unwrap()
                .0;
            v.push((
                "other project",
                db,
                foreign.id,
                spec_id,
                RpcCode::InvalidArgument,
                "the workspace would be registered in project",
            ));
            v
        };

        for (label, db, project_id, spec_id, expected, needle) in cases {
            let fresh = base.join(format!("never-created-{}", label.replace(' ', "-")));
            assert!(!fresh.exists(), "precondition: {label} path is absent");
            let err = dispatch_workspace_tool(
                &db,
                &crate::usecase::NullEventSink,
                "create_workspace",
                &json!({
                    "project_id": project_id,
                    "name": "WS",
                    "path": fresh.to_string_lossy(),
                    "spec_id": spec_id,
                }),
            )
            .expect("dispatched")
            .unwrap_err();
            assert_eq!(err.code, expected, "{label} is refused with the D8 code");
            assert!(
                err.message.contains(needle),
                "{label} explains itself with '{needle}', got: {}",
                err.message
            );
            // NOTHING on this path may speak of a workspace BELONGING to a project: the
            // prevalidation runs before any workspace is created or even looked up.
            assert!(
                !err.message.contains("workspace belongs to project"),
                "{label} must not claim a workspace that does not exist yet, got: {}",
                err.message
            );
            // BEFORE mkdir: nothing on disk.
            assert!(
                !fresh.exists(),
                "{label} was refused BEFORE the folder was created"
            );
            // BEFORE the DB write: the project gained no workspace beyond its root.
            let live = db
                .with_conn(|c| db::list_workspaces(c, &project_id))
                .unwrap();
            assert!(
                live.iter().all(|w| w.name != "WS"),
                "{label} wrote no workspace row"
            );
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    /// REVIEW F2 — THE EMPTY `spec_id`, ON THE TWO PARAMETERS THIS PRD ADDED.
    ///
    /// The symmetric half of the `spec_set_workspace` bug, on the anchoring surface this PRD is
    /// itself responsible for: `workspace_add(spec_id: "")` and `create_workspace(spec_id: "")` went
    /// through `optional_str` → `None` → NO anchoring, with no error whatsoever. The caller believed
    /// it had linked the Spec and walked away with an UNLINKED workspace — the failure mode is
    /// exactly the observed one, only the direction differs (not anchoring instead of un-anchoring).
    ///
    /// Two properties are pinned per tool: `""` is REFUSED with `invalid_argument` naming the
    /// argument, and — for `create_workspace` — the refusal lands BEFORE the `mkdir`, so a mistyped
    /// anchor leaves nothing on disk; and `null` / ABSENT are untouched, still registering an
    /// UNLINKED workspace without an error, which is the whole point of the parameter being
    /// optional.
    #[test]
    fn an_empty_spec_id_is_refused_before_the_mkdir_while_null_and_absent_still_register() {
        let _g = db_guard();
        let base = temp_subdir("empty-spec-id");

        // --- create_workspace: "" is refused, and NOTHING is created (neither folder nor row).
        let (db, project_id, _spec_id) = seeded_project_with_spec(db::SPEC_STATUS_READY);
        let never = base.join("never-created");
        assert!(!never.exists(), "precondition: the folder is absent");
        let err = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "create_workspace",
            &json!({
                "project_id": project_id,
                "name": "WS-empty",
                "path": never.to_string_lossy(),
                "spec_id": "",
            }),
        )
        .expect("dispatched")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("spec_id") && err.message.contains("EMPTY string"),
            "the refusal names the offending argument and its fault, got: {}",
            err.message
        );
        assert!(
            err.message.contains("not a way to say 'none'"),
            "…and distinguishes the empty string from the ABSENCE, got: {}",
            err.message
        );
        assert!(
            !never.exists(),
            "the refusal landed BEFORE the mkdir — an argument nyx will never accept costs no folder"
        );
        assert!(
            db.with_conn(|c| db::list_workspaces(c, &project_id))
                .unwrap()
                .iter()
                .all(|w| w.name != "WS-empty"),
            "…and no workspace row either"
        );

        // --- create_workspace: null and ABSENT are UNCHANGED — the workspace is created, UNLINKED.
        for (label, anchor) in [
            ("null", json!({ "spec_id": Value::Null })),
            ("absent", json!({})),
        ] {
            let dir = base.join(format!("created-{label}"));
            let mut args = json!({
                "project_id": project_id,
                "name": format!("WS-{label}"),
                "path": dir.to_string_lossy(),
            });
            if let Some(spec_id) = anchor.get("spec_id") {
                args["spec_id"] = spec_id.clone();
            }
            let res = dispatch_workspace_tool(
                &db,
                &crate::usecase::NullEventSink,
                "create_workspace",
                &args,
            )
            .expect("dispatched")
            .unwrap_or_else(|e| panic!("{label} must still register a workspace: {e:?}"));
            assert!(dir.exists(), "{label}: the folder WAS created");
            assert!(
                res.get("spec").is_none(),
                "{label}: no Spec was anchored, and the payload says nothing about one: {res}"
            );
            assert!(
                res["workspace"]["id"].is_string(),
                "{label}: the workspace itself was registered: {res}"
            );
        }

        // --- workspace_add: the SAME two properties, on a folder that already exists.
        let existing = base.join("already-there");
        std::fs::create_dir_all(&existing).unwrap();
        let err = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "workspace_add",
            &json!({
                "project_id": project_id,
                "path": existing.to_string_lossy(),
                "name": "WS-add-empty",
                "spec_id": "",
            }),
        )
        .expect("dispatched")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("spec_id") && err.message.contains("EMPTY string"),
            "workspace_add refuses it the same way, got: {}",
            err.message
        );
        assert!(
            db.with_conn(|c| db::list_workspaces(c, &project_id))
                .unwrap()
                .iter()
                .all(|w| w.name != "WS-add-empty"),
            "the refused workspace_add wrote no row"
        );

        // …and the unlinked registration still works, on that very folder.
        let res = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "workspace_add",
            &json!({
                "project_id": project_id,
                "path": existing.to_string_lossy(),
                "name": "WS-add-null",
                "spec_id": Value::Null,
            }),
        )
        .expect("dispatched")
        .expect("null still means: register it, anchor nothing");
        assert!(
            res.get("spec").is_none(),
            "null anchors no Spec, without an error: {res}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// `workspace_add` applies the SAME prevalidation: a draft Spec is refused and NO workspace
    /// row is written, even though the folder legitimately exists.
    #[test]
    fn workspace_add_refuses_a_draft_spec_and_writes_nothing() {
        let _g = db_guard();
        let (db, project_id, spec_id) = seeded_project_with_spec(db::SPEC_STATUS_DRAFT);
        let existing = temp_subdir("add-draft");

        let err = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "workspace_add",
            &json!({
                "project_id": project_id,
                "path": existing.to_string_lossy(),
                "spec_id": spec_id,
            }),
        )
        .expect("dispatched")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(
            existing.is_dir(),
            "a refusal NEVER removes a pre-existing folder"
        );
        let live = db
            .with_conn(|c| db::list_workspaces(c, &project_id))
            .unwrap();
        assert_eq!(live.len(), 1, "only the project's root workspace remains");

        let _ = std::fs::remove_dir_all(&existing);
    }

    /// DONE-CRITERION: "une erreur DB ne laisse aucun workspace partiel ; un dossier neuf et
    /// vide est compensé, un chemin préexistant reste intact". The DB failure is a real one —
    /// an unknown project's FK violation, raised AFTER the `mkdir` — so the compensation path
    /// is the one under test.
    #[test]
    fn a_db_failure_compensates_a_freshly_created_dir_but_never_a_preexisting_one() {
        let _g = db_guard();
        let db = Db::in_memory();
        let base = temp_subdir("compensate");

        // (a) FRESH folder + failing DB write → the folder we created is removed.
        let fresh = base.join("fresh-ws");
        assert!(!fresh.exists());
        let err = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "create_workspace",
            &json!({ "project_id": "no-such-project", "name": "WS", "path": fresh.to_string_lossy() }),
        )
        .expect("dispatched")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
        assert!(
            !fresh.exists(),
            "the folder created by this call was compensated after the DB failure"
        );

        // (b) PRE-EXISTING folder + failing DB write → the folder stays, untouched.
        let preexisting = base.join("preexisting-ws");
        std::fs::create_dir_all(&preexisting).unwrap();
        std::fs::write(preexisting.join("keep.txt"), b"user data").unwrap();
        let err = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "create_workspace",
            &json!({ "project_id": "no-such-project", "name": "WS", "path": preexisting.to_string_lossy() }),
        )
        .expect("dispatched")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
        assert!(
            preexisting.is_dir() && preexisting.join("keep.txt").is_file(),
            "a path nyx did not create is NEVER removed, content intact"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The compensation is LEAF-ONLY and that limit is INTENTIONAL: `create_workspace` on a
    /// path with SEVERAL missing segments `mkdir -p`s them all, but a DB failure removes only
    /// the last one — the intermediate parents stay. Walking back up would make the rollback a
    /// recursive deletion, which the PRD puts out of scope (see [`compensate_created_dir`]).
    /// This test exists to make that residue a frozen, deliberate outcome rather than a
    /// surprise: what is left behind is EMPTY directories, and no workspace row.
    #[test]
    fn the_compensation_of_a_multi_segment_mkdir_removes_the_leaf_only() {
        let _g = db_guard();
        let db = Db::in_memory();
        let base = temp_subdir("multi-segment");
        let parent = base.join("a");
        let intermediate = parent.join("b");
        let leaf = intermediate.join("c");
        assert!(!parent.exists(), "precondition: 3 segments are all missing");

        // The DB write fails (unknown project) AFTER the mkdir -p created the three segments.
        let err = dispatch_workspace_tool(
            &db,
            &crate::usecase::NullEventSink,
            "create_workspace",
            &json!({ "project_id": "no-such-project", "name": "WS", "path": leaf.to_string_lossy() }),
        )
        .expect("dispatched")
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);

        assert!(!leaf.exists(), "the LEAF segment is compensated");
        assert!(
            intermediate.is_dir() && parent.is_dir(),
            "the parents created by the same mkdir -p SURVIVE — the compensation is leaf-only \
             by decision, and what remains is empty directories on the requested path"
        );
        assert!(
            std::fs::read_dir(&intermediate).unwrap().next().is_none(),
            "…and the residue is EMPTY: nothing was written under it"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The ANCHOR half failing rolls the WORKSPACE half back: the two writes share ONE
    /// transaction, so a refused anchor leaves NO partial workspace row. The refusal must be
    /// raised INSIDE the transaction, which the tool cannot be made to do (it would take the
    /// Spec being cancelled BETWEEN the prevalidation and the write) — so the test drives the
    /// shared use case directly, which is exactly what the tool wraps.
    ///
    /// Only the DB half is under test here: the folder pre-exists (`temp_subdir` creates it)
    /// and no tool-level compensation runs, so this test proves NOTHING about the filesystem.
    /// [`a_db_failure_compensates_a_freshly_created_dir_but_never_a_preexisting_one`] is the
    /// one that covers the `mkdir` compensation.
    #[test]
    fn a_refused_anchor_rolls_the_workspace_insert_back() {
        let _g = db_guard();
        let (db, project_id, spec_id) = seeded_project_with_spec(db::SPEC_STATUS_DRAFT);
        let dir = temp_subdir("rollback");

        let err = db
            .with_conn(|c| {
                crate::usecase::register_workspace_anchored(
                    c,
                    &crate::usecase::NullEventSink,
                    &project_id,
                    "WS",
                    &dir.to_string_lossy(),
                    Some(&spec_id),
                    crate::usecase::DomainEventSource::Mcp,
                )
            })
            .unwrap_err();
        assert!(
            matches!(err, crate::usecase::RegisterWorkspaceError::Anchor(_)),
            "the anchor half refused, got {err:?}"
        );
        let live = db
            .with_conn(|c| db::list_workspaces(c, &project_id))
            .unwrap();
        assert_eq!(
            live.len(),
            1,
            "the workspace insert rolled back with the refused anchor (root only)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DONE-CRITERION: "sans `spec_id`, le contrat existant est strictement inchangé". `spec_id`
    /// was added to BOTH tools, so BOTH are driven here: the result payload carries `workspace`
    /// and NOTHING else — no `spec` key — exactly as before v2.5. The two differ only in their
    /// filesystem precondition (`workspace_add` registers an EXISTING folder, `create_workspace`
    /// creates it), never in the shape of what they return.
    #[test]
    fn without_spec_id_the_result_payload_is_unchanged() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/no-spec", None))
            .unwrap();
        let base = temp_subdir("no-spec");

        // (tool, does the folder have to exist BEFORE the call?)
        for (tool, preexisting) in [("create_workspace", false), ("workspace_add", true)] {
            let dir = base.join(tool);
            if preexisting {
                std::fs::create_dir_all(&dir).unwrap();
            }

            let res = dispatch_workspace_tool(
                &db,
                &crate::usecase::NullEventSink,
                tool,
                &json!({ "project_id": project.id, "name": tool, "path": dir.to_string_lossy() }),
            )
            .expect("dispatched")
            .expect("ok");
            assert!(res["workspace"].is_object(), "{tool} returns the workspace");
            assert!(
                res.get("spec").is_none(),
                "{tool}: no spec_id ⇒ no `spec` key: the pre-v2.5 payload is preserved"
            );
            assert_eq!(
                res.as_object().map(|o| o.len()),
                Some(1),
                "{tool}: the payload carries EXACTLY the pre-v2.5 key set"
            );
            assert!(dir.is_dir(), "{tool}: the folder handling is unchanged");
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    // --- The per-run `env` map (R-WSCMD #7) --------------------------------

    /// A persistence-free [`RunnerSink`] that only ACCUMULATES each instance's output, so a test
    /// can read back what the spawned process actually printed.
    #[derive(Default)]
    struct OutputSink {
        output: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
    }

    impl OutputSink {
        fn output_of(&self, instance_id: &str) -> String {
            let map = self.output.lock().unwrap();
            String::from_utf8_lossy(map.get(instance_id).map(Vec::as_slice).unwrap_or(&[]))
                .to_string()
        }
    }

    impl RunnerSink for std::sync::Arc<OutputSink> {
        fn on_state(&self, _instance_id: &str, _state: RunState, _exit_code: Option<i32>) {}
        fn on_acknowledge(&self, _instance_id: &str) {}
        fn on_output(&self, instance_id: &str, bytes: &[u8]) {
            self.output
                .lock()
                .unwrap()
                .entry(instance_id.to_string())
                .or_default()
                .extend_from_slice(bytes);
        }
        fn persist_scrollback(&self, _instance_id: &str, _serialized: &str) {}
        fn archive_previous_run(&self, _instance_id: &str) {}
        fn clear_output(&self, _instance_id: &str) {}
    }

    /// A `Db` holding ONE project rooted at a real temp dir plus a managed-command template
    /// running `command`. Returns `(db, instance_id, root)` — the instance id is the launchable
    /// id `start_command` / `relaunch_command` resolve.
    fn db_with_command(tag: &str, command: &str) -> (Db, String, std::path::PathBuf) {
        let db = Db::in_memory();
        let root = temp_subdir(tag);
        let instance_id = db.with_conn(|c| {
            let (project, _ws) =
                db::create_project(c, "P", root.to_str().unwrap(), None).expect("project");
            db::create_template(
                c,
                &project.id,
                "dev",
                command,
                None,
                db::CommandSource::default(),
            )
            .expect("template");
            db::instance_ids_for_project(c, &project.id)
                .expect("instances")
                .into_iter()
                .next()
                .expect("one materialized instance")
        });
        (db, instance_id, root)
    }

    #[cfg(not(windows))]
    fn new_output_runner() -> (
        CommandRunner<std::sync::Arc<OutputSink>>,
        std::sync::Arc<OutputSink>,
    ) {
        let sink = std::sync::Arc::new(OutputSink::default());
        let runner = CommandRunner::new(
            std::sync::Arc::clone(&sink),
            portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        );
        (runner, sink)
    }

    /// Wait until `instance_id` reaches `state` (or time out).
    #[cfg(not(windows))]
    fn wait_state<S: RunnerSink>(
        runner: &CommandRunner<S>,
        instance_id: &str,
        state: RunState,
    ) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if runner.state_of(instance_id) == state {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// THE defect: the advertised `env` map reaches the SPAWNED PROCESS. Mirrors
    /// `command::tests::start_with_env_passes_env_to_the_spawned_process` one layer up — through
    /// the TOOL, which used to drop the argument on the floor and pass an empty slice.
    #[test]
    #[cfg(not(windows))]
    fn start_command_applies_the_env_argument_to_the_spawned_process() {
        let _g = db_guard();
        let (db, instance_id, root) = db_with_command("env-start", "echo VAR=$NYX_TEST_VAR; true");
        let (runner, sink) = new_output_runner();

        start_command(
            &db,
            &runner,
            &json!({ "instance_id": instance_id, "env": { "NYX_TEST_VAR": "vault-value-123" } }),
        )
        .expect("start");
        assert!(
            wait_state(&runner, &instance_id, RunState::Success),
            "the echo command finishes"
        );
        assert!(
            sink.output_of(&instance_id).contains("VAR=vault-value-123"),
            "the tool's env map must reach the spawned process, got: {:?}",
            sink.output_of(&instance_id)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// N31 — the TWO env layers compose: the command's PERSISTED env is the base every path applies
    /// (the gate included), and the per-run `env` argument is a TRANSIENT override merged ON TOP for
    /// this one start — so a key present in both takes the PER-RUN value while the others survive.
    #[test]
    #[cfg(not(windows))]
    fn start_command_merges_the_persisted_env_with_the_per_run_one() {
        let _g = db_guard();
        let (db, instance_id, root) = db_with_command(
            "env-merge",
            "echo BASE=$NYX_TEST_BASE SHARED=$NYX_TEST_SHARED; true",
        );
        db.with_conn(|c| {
            let command_id = db::get_instance(c, &instance_id)
                .expect("instance")
                .expect("materialized")
                .command_id;
            db::set_template_env(
                c,
                &command_id,
                Some(
                    r#"{"NYX_TEST_BASE":"from-the-command","NYX_TEST_SHARED":"from-the-command"}"#,
                ),
            )
            .expect("persist env");
        });
        let (runner, sink) = new_output_runner();

        start_command(
            &db,
            &runner,
            &json!({ "instance_id": instance_id, "env": { "NYX_TEST_SHARED": "from-this-run" } }),
        )
        .expect("start");
        assert!(
            wait_state(&runner, &instance_id, RunState::Success),
            "the echo command finishes"
        );
        let out = sink.output_of(&instance_id);
        assert!(
            out.contains("BASE=from-the-command"),
            "a key only the COMMAND declares survives the merge, got: {out:?}"
        );
        assert!(
            out.contains("SHARED=from-this-run"),
            "on a collision the PER-RUN value wins, got: {out:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// N31 — with NO per-run argument at all, an agent start still gives the command the environment
    /// its own definition declares: exactly what a gate does, so both surfaces agree.
    #[test]
    #[cfg(not(windows))]
    fn start_command_applies_the_persisted_env_without_any_argument() {
        let _g = db_guard();
        let (db, instance_id, root) =
            db_with_command("env-persisted", "echo VAR=$NYX_TEST_PERSISTED; true");
        db.with_conn(|c| {
            let command_id = db::get_instance(c, &instance_id)
                .expect("instance")
                .expect("materialized")
                .command_id;
            db::set_template_env(
                c,
                &command_id,
                Some(r#"{"NYX_TEST_PERSISTED":"stored-value"}"#),
            )
            .expect("persist env");
        });
        let (runner, sink) = new_output_runner();

        start_command(&db, &runner, &json!({ "instance_id": instance_id })).expect("start");
        assert!(
            wait_state(&runner, &instance_id, RunState::Success),
            "the echo command finishes"
        );
        assert!(
            sink.output_of(&instance_id).contains("VAR=stored-value"),
            "the persisted env applies with no per-run argument, got: {:?}",
            sink.output_of(&instance_id)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The relaunch twin: the FRESH process a relaunch spawns carries the map too.
    #[test]
    #[cfg(not(windows))]
    fn relaunch_command_applies_the_env_argument_to_the_fresh_process() {
        let _g = db_guard();
        let (db, instance_id, root) =
            db_with_command("env-relaunch", "echo VAR=$NYX_TEST_VAR; true");
        let (runner, sink) = new_output_runner();

        relaunch_command(
            &db,
            &runner,
            &json!({ "instance_id": instance_id, "env": { "NYX_TEST_VAR": "relaunch-val" } }),
        )
        .expect("relaunch");
        assert!(
            wait_state(&runner, &instance_id, RunState::Success),
            "the relaunched echo command finishes"
        );
        assert!(
            sink.output_of(&instance_id).contains("VAR=relaunch-val"),
            "the relaunch env map must reach the fresh process, got: {:?}",
            sink.output_of(&instance_id)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An ABSENT `env` still starts (the inherit-only path) — the map is optional.
    #[test]
    #[cfg(not(windows))]
    fn start_command_without_env_still_starts() {
        let _g = db_guard();
        let (db, instance_id, root) = db_with_command("env-absent", "echo NOENV; true");
        let (runner, sink) = new_output_runner();

        let result = start_command(&db, &runner, &json!({ "instance_id": instance_id }))
            .expect("start with no env");
        assert_eq!(result["instance_id"], json!(instance_id));
        assert!(wait_state(&runner, &instance_id, RunState::Success));
        assert!(sink.output_of(&instance_id).contains("NOENV"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A NON-STRING value is REFUSED before any spawn ("Each value must be a string"), and the
    /// refusal names the KEY only — an env value is a secret and is never echoed.
    #[test]
    fn start_command_refuses_a_non_string_env_value() {
        let _g = db_guard();
        let (db, instance_id, root) = db_with_command("env-bad", "true");
        let (runner, _sink) = {
            let sink = std::sync::Arc::new(OutputSink::default());
            let runner = CommandRunner::new(
                std::sync::Arc::clone(&sink),
                portable_pty::PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            );
            (runner, sink)
        };

        let err = start_command(
            &db,
            &runner,
            &json!({ "instance_id": instance_id, "env": { "PORT": 3111 } }),
        )
        .expect_err("a non-string env value is refused");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("env.PORT"), "got {:?}", err.message);
        assert_eq!(
            runner.state_of(&instance_id),
            RunState::Idle,
            "the refusal happens BEFORE any spawn"
        );

        // A non-object `env` is refused the same way.
        let err = start_command(
            &db,
            &runner,
            &json!({ "instance_id": instance_id, "env": "PORT=3111" }),
        )
        .expect_err("a non-object env is refused");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The parser itself: an absent map is empty, a well-formed one keeps every pair (an EMPTY
    /// string value is a legitimate env value), and a bad value never leaks into the message.
    #[test]
    fn optional_env_map_decodes_a_string_map_and_hides_values() {
        assert!(optional_env_map(&json!({}), "env").unwrap().is_empty());
        assert!(optional_env_map(&json!({ "env": null }), "env")
            .unwrap()
            .is_empty());

        let mut pairs = optional_env_map(
            &json!({ "env": { "A": "1", "B": "", "SECRET": "s3cr3t" } }),
            "env",
        )
        .unwrap();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "".to_string()),
                ("SECRET".to_string(), "s3cr3t".to_string()),
            ]
        );

        let err = optional_env_map(&json!({ "env": { "SECRET": ["s3cr3t"] } }), "env")
            .expect_err("a non-string value is refused");
        assert!(err.message.contains("env.SECRET"));
        assert!(
            !err.message.contains("s3cr3t"),
            "the VALUE is never echoed in the refusal: {:?}",
            err.message
        );
    }
}
