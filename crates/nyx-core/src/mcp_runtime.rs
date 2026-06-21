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

/// Resolve an instance's command line + run cwd, mapping to the D8 vocabulary: an
/// unknown instance → `invalid_id` (disambiguating a template id), an invalid/missing
/// subfolder → `invalid_argument`. Errors BEFORE any spawn. Reuses the shared
/// `command::resolve_command_and_cwd` for the resolution itself.
fn resolve_command_and_cwd(db: &Db, instance_id: &str) -> Result<(String, String), RpcError> {
    let ctx = db
        .with_conn(|c| db::instance_run_context(c, instance_id))
        .map_err(internal_db)?;
    let ctx = match ctx {
        Some(ctx) => ctx,
        None => return Err(bad_instance_id_error(db, instance_id)),
    };
    let cwd = crate::subfolder::resolve_run_dir(&ctx.workspace_path, ctx.subfolder.as_deref())
        .map_err(|e| RpcError::new(RpcCode::InvalidArgument, e))?;
    Ok((ctx.command, cwd))
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

/// `start_command` — `{ instance_id | (name, workspace_id) }` → `{ instance_id,
/// state, running, finished, exit_code, unread, was_running, restarted }`. Idempotent
/// on an already-running instance (no double-spawn; the guard is at the runner
/// boundary). Resolves command + cwd BEFORE spawning. Parity with the Tauri
/// `start_command`.
pub fn start_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    let instance_id = resolve_instance_id(db, args)?;
    let (command, cwd) = resolve_command_and_cwd(db, &instance_id)?;
    let outcome = runner
        .start_with_env(&instance_id, &command, Some(&cwd), &[])
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

/// `relaunch_command` — `{ instance_id }` → `{ instance_id, …status, was_running,
/// restarted }`. The EXPLICIT restart (always re-spawns; never two live processes).
/// `restarted:true`. Parity with the Tauri `relaunch_command`.
pub fn relaunch_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    let instance_id = require_str(args, "instance_id")?;
    let (command, cwd) = resolve_command_and_cwd(db, instance_id)?;
    let outcome = runner
        .relaunch_with_env(instance_id, &command, Some(&cwd), &[])
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("relaunch failed: {e}")))?;
    let mut result = status_result(instance_id, runner_status(runner, instance_id));
    if let Some(map) = result.as_object_mut() {
        map.insert("was_running".to_string(), json!(outcome.was_running));
        map.insert("restarted".to_string(), json!(true));
    }
    Ok(result)
}

/// `get_command_output` — `{ instance_id | (name, workspace_id), tail_bytes?, since?,
/// run? ("current"|"previous"), mark_read? }` → `{ instance_id, run, output,
/// total_bytes, returned_bytes, truncated, reset, cursor, …status }`.
///
/// The source mirrors `bridge::command_output` + the Tauri `get_command_output`: the
/// runner's LIVE in-memory tail while running, else the persisted scrollback rehydrated
/// from the DB; the previous-run selector reads the `prev_*` columns. The window is
/// bounded by [`bound_output`]; `since`/`cursor` support incremental polling.
/// `mark_read:true` ALSO acknowledges the current run (clears `unread`) after the window
/// is computed.
///
/// NOTE on scope: the Tauri tool also offers `strip_ansi`/`grep`/`tail_lines`/`max_bytes`
/// render knobs. Those are pure post-processing of the returned `output` string; this
/// shared core returns the RAW bounded window (the byte cursor/round-trip is identical),
/// which is the behavioral parity the runtime tools require. A shell that wants the
/// render knobs can post-process `output`.
pub fn get_command_output<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    let instance_id = resolve_instance_id(db, args)?;
    let tail_bytes = optional_usize(args, "tail_bytes")?.unwrap_or(DEFAULT_TAIL_BYTES);
    let since = optional_usize(args, "since")?;
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
    let window = bound_output(&full, tail_bytes, effective_since);
    let mut result = json!({
        "instance_id": instance_id,
        "run": if previous { "previous" } else { "current" },
        "output": window.output,
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
fn ensure_dir_created(path: &str) -> Result<(), RpcError> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_dir() {
            return Ok(()); // already a directory: idempotent create.
        }
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "path '{path}' exists but is not a directory; cannot create a workspace folder there"
            ),
        ));
    }
    std::fs::create_dir_all(path).map_err(|e| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!("could not create directory '{path}': {e}"),
        )
    })
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

/// Shared body of `workspace_add` / `create_workspace`: one `db::create_workspace` call,
/// mapping its failure to the D8 vocabulary. The on-disk path handling (validate-existing
/// vs mkdir-p) is done by the caller BEFORE this, so the two tools differ only in their
/// filesystem precondition, not in the persistence path. Returns `{ workspace }`.
fn create_workspace_inner(
    db: &Db,
    project_id: &str,
    name: &str,
    path: &str,
) -> Result<Value, RpcError> {
    let workspace = db
        .with_conn(|c| db::create_workspace(c, project_id, name, path))
        .map_err(|e| map_create_workspace_err(project_id, e))?;
    Ok(json!({ "workspace": workspace }))
}

/// `workspace_add` — `{ project_id, path, name? }` → `{ workspace }`. Registers an EXISTING
/// on-disk folder as a workspace (the *register an existing dir* tool — contrast
/// `create_workspace`, which CREATES the folder first). The path is VALIDATED on disk
/// BEFORE the DB write; `name` defaults to the path's last segment when omitted. Parity
/// with the Tauri `workspace_add` (minus the shell sidebar-refresh event seam).
pub fn workspace_add(db: &Db, args: &Value) -> Result<Value, RpcError> {
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
    validate_existing_dir(path)?;
    create_workspace_inner(db, project_id, &name, path)
}

/// `create_workspace` — `{ project_id, name, path }` → `{ workspace }`. The *creating-intent*
/// sibling of `workspace_add`: it `mkdir -p`s the folder BEFORE registering, so an agent can
/// ask nyx to track a folder that does not exist on disk yet. Both then share the SAME
/// `db::create_workspace` write. Parity with the Tauri `create_workspace`.
pub fn create_workspace(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let name = require_str(args, "name")?;
    let path = require_str(args, "path")?;
    ensure_dir_created(path)?;
    create_workspace_inner(db, project_id, name, path)
}

/// The single entry point a shell's MCP dispatcher calls for the workspace registration
/// tools (`workspace_add` / `create_workspace`): route `name` to the matching tool, or
/// `None` if `name` is not one of them. A successful `Some(Ok(_))` signals a committed
/// mutation, so a shell with a sidebar-refresh event seam can fire it after this returns.
pub fn dispatch_workspace_tool(
    db: &Db,
    name: &str,
    args: &Value,
) -> Option<Result<Value, RpcError>> {
    match name {
        "workspace_add" => Some(workspace_add(db, args)),
        "create_workspace" => Some(create_workspace(db, args)),
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

#[cfg(test)]
mod tests {
    use super::*;

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

        let res = dispatch_workspace_tool(
            &db,
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
        assert!(dispatch_workspace_tool(&db, "list_projects", &json!({})).is_none());
    }
}
