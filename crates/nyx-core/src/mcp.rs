//! Local MCP HTTP server (PRD-4, ADR-0003).
//!
//! nyx hosts a SINGLE loopback HTTP server that exposes its MCP surface to local
//! agents (Claude Code & other MCP clients). This module owns the transport and
//! single-instance lifecycle decided in ADR-0003:
//!
//! - **D1 — transport**: HTTP, bind STRICTLY to `127.0.0.1`. `GET /health` for a
//!   liveness probe; `POST /mcp` for JSON-RPC 2.0 (`initialize`, `tools/list`,
//!   `tools/call`). No LAN / public exposure, no daemon.
//! - **D2 — port**: fixed & configurable, STABLE between launches. Default
//!   [`DEFAULT_PORT`], overridable via `NYX_MCP_PORT`. Ephemeral / port 0 are
//!   rejected — a stable port is what keeps an onboarded client's config valid
//!   across restarts. v2 deliberately defaults to a DIFFERENT stable port than
//!   the v1 cockpit (which owns 8765) so the two can run side by side during the
//!   v1→v2 transition without an `AddrInUse` bind failure (still a fixed port,
//!   NOT port 0 — the stability guarantee is unchanged).
//! - **D3 — single-instance**: at most ONE server per machine. nyx is already
//!   single-instance (`tauri-plugin-single-instance`), and [`McpServer::start`]
//!   is itself start-once: a second call is a no-op, never a second listener.
//! - **D9 — loopback security**: bound to loopback only; `Origin`/`Host` are
//!   checked to reject cross-origin browser requests (anti DNS-rebinding).
//!
//! The command tools (`start_command`, …) delegate to the PRD-3 runtime/DB layer
//! (ADR-0003 D6); their bodies land in phase 2. This phase-1 slice stands up the
//! server, the health probe, and the MCP handshake (`initialize` + `tools/list`
//! advertising the frozen v1 tool surface).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{json, Value};

/// Default fixed MCP port (ADR-0003 D2). Above the ephemeral range, low collision
/// odds. Overridable via `NYX_MCP_PORT`, but STABLE between launches by default so
/// an onboarded client's `http://127.0.0.1:<port>/mcp` config stays valid.
///
/// DISTINCT from the v1 cockpit's port (8765): the user's production v1 build runs
/// permanently and holds 8765, and `McpServer::start` rejects port 0 / has no
/// fallback (ADR-0003 D2 — a stable port), so sharing 8765 would make v2's bind
/// fail with `AddrInUse` whenever v1 is up. v2 therefore takes 8766 so v1 and v2
/// coexist on their own stable ports through the v1→v2 transition.
pub const DEFAULT_PORT: u16 = 8766;

/// Env var that overrides [`DEFAULT_PORT`] (ADR-0003 D2). Must parse to a valid
/// non-zero `u16`; an invalid/zero value falls back to the default (port 0, i.e.
/// "pick any free port", is explicitly rejected so the port stays stable).
pub const PORT_ENV: &str = "NYX_MCP_PORT";

/// The HTTP request header that binds an MCP connection to its ORIGIN TERMINAL (v2.5.10
/// Phase 1 auto-map). The plugin `.mcp.json` sets it to `${NYX_TERMINAL_ID}` — the per-terminal
/// id `pty.rs` exports into the shell — so every `tools/call` an agent makes carries the
/// terminal it runs in. nyx reads it here and threads it into the dispatcher, which attributes a
/// spec-scoped mutation to that terminal's live session (the session↔Specs auto-map). Absent (a
/// bare MCP client, an un-templated config) → `None` → no attribution (best-effort). Matched
/// case-INSENSITIVELY (HTTP header names are case-insensitive).
pub const TERMINAL_ID_HEADER: &str = "X-Nyx-Terminal-Id";

/// The `probe` spike tool name (PRD-4 #7, ADR-0004). A trivial no-op tool used to
/// validate that a Claude Code `SessionStart` hook of type `mcp_tool` can reach
/// nyx's MCP surface. It is NOT part of the frozen v1 surface ([`V1_TOOLS`]) — it is
/// a diagnostic/liveness probe, advertised in `tools/list` alongside the v1 tools so
/// a hook can discover and call it, but kept separate so it can be removed/renamed
/// without touching the v1 contract.
pub const PROBE_TOOL: &str = "probe";

/// The `wait_for_command` long-poll tool name (PRD-4 dogfood, review
/// `01KV91ZY1K8VCFQ44PQ5631WK0`, ADR-0003 D12). A BOUNDED long-poll that returns at
/// the first of (a) the instance's state entering the caller's `until` set, or (b)
/// `timeout_ms` elapsing. Like [`PROBE_TOOL`] it is advertised in `tools/list` but is
/// deliberately NOT part of the frozen v1 surface ([`V1_TOOLS`]): it is a purely
/// observational convenience layered over the same runner/db read paths the v1 tools
/// use, so it can evolve without touching the v1 contract clients are onboarded to.
pub const WAIT_FOR_COMMAND_TOOL: &str = "wait_for_command";

/// The `add_command` tool name (PRD-4 dogfood, review `01KV9614CHC4092P05DV9R5KPG`,
/// finding `01KV9615DM7DN2DV0D0XGKJGVW`, ADR-0003 D13). Creates a per-project command
/// TEMPLATE via the SAME path as the UI's `bridge::command_create`. Like [`PROBE_TOOL`]
/// / [`WAIT_FOR_COMMAND_TOOL`] it is advertised in `tools/list` but kept OUT of the
/// frozen v1 surface ([`V1_TOOLS`]): the mutating command-CRUD extension can evolve
/// without touching the v1 read/lifecycle contract clients are onboarded to.
pub const ADD_COMMAND_TOOL: &str = "add_command";

/// The `update_command` tool name (same finding / D13). Modifies an existing template's
/// editable fields (`name`/`command`/`subfolder`) via the SAME path — including the
/// package.json source-detach rule — as the UI's `bridge::command_update`. Advertised
/// in `tools/list`, kept out of [`V1_TOOLS`] for the SAME reason as `add_command`.
pub const UPDATE_COMMAND_TOOL: &str = "update_command";

/// The `import_commands` tool name (same finding / D13). Imports a project workspace's
/// `package.json` scripts as templates, reusing the EXISTING import logic
/// (`pkgjson::discover_package_scripts` + `pkgjson::import_command`) — the SAME path as
/// the UI's `command_import_scripts`/`command_import_create`. Advertised in
/// `tools/list`, kept out of [`V1_TOOLS`] for the SAME reason as `add_command`.
pub const IMPORT_COMMANDS_TOOL: &str = "import_commands";

/// The `remove_workspace` tool name (review 01KV9CWK A2). Deletes a workspace by id,
/// guarding against live running instances and cascading to command instances. Advertised
/// in `tools/list`, kept out of [`V1_TOOLS`] (same pattern as CRUD tools).
pub const REMOVE_WORKSPACE_TOOL: &str = "remove_workspace";

/// The `remove_command` tool name (review 01KV9CWK A2). Deletes a command TEMPLATE and
/// its instances, guarding against running instances. Must NOT be called with an
/// `instance_id` — it operates on templates (`command_id`). Advertised in `tools/list`,
/// kept out of [`V1_TOOLS`] (same pattern as CRUD tools).
pub const REMOVE_COMMAND_TOOL: &str = "remove_command";

/// The `clear_command_output` tool name (PRD-4 review R-OUTPUT). Clears an instance's
/// captured output BUFFER (current + retained prior run) so an idle instance's stale
/// scrollback does not stay attached indefinitely, delegating to the PRD-3 runner
/// buffer and emitting the refresh event so the UI output panel reflects the clear. The
/// factual run outcome is left intact. Advertised in `tools/list`, kept out of
/// [`V1_TOOLS`] (same pattern as the other extension tools).
pub const CLEAR_COMMAND_OUTPUT_TOOL: &str = "clear_command_output";

/// The `list_importable_scripts` tool name (PRD-4 review R-IMPORT #5). Surfaces the
/// FILTERED, monorepo-aware import-discovery preview — the discoverable package.json
/// scripts (name, package, script_name, body, command) WITHOUT creating any template.
/// The read-only companion to `import_commands(preview:true)`. Advertised in
/// `tools/list`, kept out of [`V1_TOOLS`] (same pattern as the other extension tools).
pub const LIST_IMPORTABLE_SCRIPTS_TOOL: &str = "list_importable_scripts";

/// The `list_system_commands` tool name (PRD « Scope Système »). THE agent's ONE door onto the
/// Système scope: the cross-project services and launchers that belong to no code project (docker,
/// machine-wide daemons…), which live under a singleton technical container
/// ([`crate::db::PROJECT_KIND_SYSTEM`]) deliberately ABSENT from `list_projects` /
/// `list_workspaces`.
///
/// ## Why a dedicated tool rather than "just list the project"
///
/// The singleton is not a USER container and must not appear in the inventories the agent walks to
/// find projects to work in — it has no Spec, no repository, no workspace lifecycle. But its
/// COMMANDS must be discoverable, or an agent could never learn that `services` exists, let alone
/// read its output. So discovery gets its own door, which answers the scope, its commands (with
/// their `instance_id`, ready to `start_command` / `get_command_output`) and its groups — and
/// nothing else of the scope. In particular the Système TERMINALS are not here and are invisible to
/// the generic terminal tools: they are the human's.
///
/// Advertised in `tools/list`, kept OUT of [`V1_TOOLS`] (same pattern as every other extension
/// tool). Read-only.
pub const LIST_SYSTEM_COMMANDS_TOOL: &str = "list_system_commands";

/// The `system_approval_status` tool name (PRD « Scope Système », protocole d'approbation). The
/// ONE tool the approval protocol adds — and it is a READ of a single request, not a surface onto
/// the ledger.
///
/// ## Why exactly one tool, and why it is a read
///
/// A protected call now BLOCKS on the human and, when they approve, nyx runs the operation itself
/// and answers with its real result (PDR-0030). The block is capped by the MCP client's request
/// timeout, which is not ours — so the one case this tool exists for is the caller that reached
/// that ceiling: it holds a `request_id` and comes back for the answer that arrived without it.
/// Granting and denying are NEVER advertised — they are UI-only db-commands
/// ([`crate::system_scope::SYSTEM_SCOPE_IPC_CAPABILITIES`]), because a tool that lets the caller
/// approve its own request is not an approval.
///
/// ## No credential, because there is nothing to hold
///
/// It takes `request_id` alone. The agent never replays, so it carries no token; what this returns
/// is a decision and a result, never an authorisation. `wait_ms` long-polls (bounded to
/// [`crate::system_approvals::APPROVAL_WAIT_CAP_MS`]) so an agent can await the human without a
/// spin loop. Advertised in `tools/list`, kept OUT of [`V1_TOOLS`] like every other extension tool.
pub const SYSTEM_APPROVAL_STATUS_TOOL: &str = "system_approval_status";

/// The `remove_commands` tool name (PRD-4 review R-IMPORT #5). GROUPED deletion of
/// command TEMPLATES by id — the batch mirror of `remove_command`, so a mass import can
/// be undone in one call. Returns the removed count + per-id acks. Advertised in
/// `tools/list`, kept out of [`V1_TOOLS`] (same pattern as the other extension tools).
pub const REMOVE_COMMANDS_TOOL: &str = "remove_commands";

/// The `reorder_commands` tool name (review `01M11WE6H9E5D5FCS6RT0ZXAXN`, finding
/// `01M11XTRXVVPZ9XMDW5ARWKWHA`). Sets the ORDER OF THE COMMAND LIST itself — `managed_commands`'
/// `order` column — which until now nothing outside the DB layer could write: there was no IPC
/// route and no MCP tool, so the list's order was whatever creation order left behind.
///
/// WHY IT LANDED WITH THE GROUPS WORK. Composing a group became a DRAG, and a drag that takes a
/// command OUT of a group has to leave it WHERE IT WAS DROPPED. Without a writable list order it
/// would fall back to its "natural" position instead — a gesture that half works, which is worse
/// than one that does not exist. So the order is opened, on both surfaces at once: the UI's
/// `command_reorder` IPC route and this tool, over the SAME `db::reorder_templates`.
///
/// It is a TOOL and not a ninth `manage_command_group` action on purpose: the order of the
/// project's command LIST is not a property of any group (a solo command has one too), and folding
/// it in would make the group tool the owner of something that outlives every group.
/// Advertised in `tools/list`, kept out of [`V1_TOOLS`] (same pattern as the other extension tools).
pub const REORDER_COMMANDS_TOOL: &str = "reorder_commands";

/// The `manage_command_group` tool name (review `01M0ABR6V501D0SPR2BYBVZC4A` #90 — « Commandes
/// multi-panes … plus pilotage MCP »). The AGENT's half of the multi-pane grouping of managed
/// commands: create a group, move a command in or out, dissolve it, name it, colour it, fix its
/// disposition. It is ONE tool with an `action` discriminant rather than eight, because the eight
/// operations share their target vocabulary and an agent picking a verb from a closed enum cannot
/// reach for a tool that does not exist.
///
/// WHY IT EXISTS AT ALL. The review's condition is SYMMETRY — nothing the agent can do that the
/// hand cannot undo, nor the reverse — so both sides drive the SAME `nyx_core::db` transitions
/// (`create_command_group`, `add_to_command_group`, …), the ones the UI's `command_group_*` IPC
/// routes call. That is also why the grouping is DB state (`managed_command_groups`) and not a
/// renderer preference like the TERMINAL groups: MCP is served by the sidecar, which cannot see
/// the renderer's `localStorage`. Mutating → carries a `Commands` changed effect, so an agent's
/// grouping repaints the open sidebar with no polling.
///
/// The `action` enum in the descriptor is READ BACK by the renderer's
/// `command-groups.test.tsx`, which compares it name-for-name against the gestures the settings
/// screen really performs — so a NINTH action added here turns that spec red instead of letting
/// the two surfaces drift. The eighth, `shape`, arrived that way: review
/// `01M11WE6H9E5D5FCS6RT0ZXAXN` (finding `01M11XTRXVVPZ9XMDW5ARWKWHA`) made the tiling shape
/// CHOSEN instead of derived, and the symmetry rule then obliges the agent to be able to choose it
/// too. Advertised in `tools/list`, kept out of [`V1_TOOLS`] (same pattern as the other extension
/// tools).
pub const MANAGE_COMMAND_GROUP_TOOL: &str = "manage_command_group";

/// The `create_terminal` tool name (PRD-4 review R-TERM). Creates an INTERACTIVE terminal:
/// a terminal record (the front mounts an xterm + spawns the PTY on the `terminals://changed`
/// reconciliation), optionally auto-attached to a workspace via a `cwd`, and optionally
/// running a `command` at opening (injected once via the front's PTY, then the terminal stays
/// interactive). Advertised in `tools/list`, kept out of [`V1_TOOLS`] (same pattern as the
/// other extension tools — it reuses the existing terminal/PTY primitives, no second
/// lifecycle).
pub const CREATE_TERMINAL_TOOL: &str = "create_terminal";

/// The `send_to_terminal` tool name (PRD-4 review R-TERM). Writes a command + newline into an
/// already-open terminal (resolved by its terminal id → live PTY) via the existing `pty_write`
/// path; the output streams back through `pty://output` as usual. Advertised in `tools/list`,
/// kept out of [`V1_TOOLS`].
pub const SEND_TO_TERMINAL_TOOL: &str = "send_to_terminal";

/// The `send_keys` tool name (FEEDBACK #31). Writes RAW keystrokes (named keys like the
/// arrows / Esc / Ctrl+C, plus literal text) into an open terminal's PTY WITHOUT any forced
/// newline — the raw-key counterpart to `send_to_terminal` (which always submits a shell
/// command line). It lets an agent drive a raw-mode TUI (navigate, type without submitting,
/// interrupt). Advertised in `tools/list`, kept OUT of [`V1_TOOLS`] (same pattern as the
/// other interactive-terminal tools).
pub const SEND_KEYS_TOOL: &str = "send_keys";

/// The `list_terminals` tool name (PRD-4 review R-TERM). Lists the OPEN (alive) terminals with
/// their terminal id, cwd, label, workspace and the live terminal↔PTY id mapping, so an agent
/// knows what it can write to. Read-only. Advertised in `tools/list`, kept out of [`V1_TOOLS`].
pub const LIST_TERMINALS_TOOL: &str = "list_terminals";

/// The `close_terminal` tool name (PRD-4 review R-TERM). Closes a terminal by id, wrapping the
/// existing `close_terminal` record helper (record → closed) + `pty_close` (kill the PTY) and
/// emitting `terminals://changed` so the front retires the pane. Advertised in `tools/list`,
/// kept out of [`V1_TOOLS`].
pub const CLOSE_TERMINAL_TOOL: &str = "close_terminal";

/// The `read_terminal` tool name (PRD-4.1 task #1). Reads the BOUNDED tail of a terminal's
/// scrollback by terminal id — symmetric to `get_command_output`, so an agent that
/// `send_to_terminal`-ed can now read the result. It reuses the EXISTING front-serialized
/// scrollback (the blob the xterm `SerializeAddon` persists via `db::persist_scrollback`); it
/// introduces NO second buffer and NO backend PTY capture. Reads reflect the LAST front-persisted
/// (debounced) scrollback, so a read immediately after `send_to_terminal` may be slightly behind —
/// a documented trade-off of reusing the front serializer, not a bug. Advertised in `tools/list`,
/// kept out of [`V1_TOOLS`] (same pattern as the other interactive-terminal tools).
pub const READ_TERMINAL_TOOL: &str = "read_terminal";

/// The `wait_for_terminal` tool name — the BOUNDED wait on a terminal's AGENT activity, the twin
/// of [`WAIT_FOR_COMMAND_TOOL`] for the other thing nyx makes an agent wait on.
///
/// The asymmetry it closes was measured on the 2026-09-01 pilot run: a managed command has
/// `wait_for_command`, a terminal had NOTHING — so an agent driving a second Claude session polled
/// blind, paid one MCP round trip per attempt, and had no signal to poll anyway (`busy` was `null`
/// and `exec_state` never moved for the whole session, since the agent is the foreground process
/// throughout). The right signal existed all along in [`crate::agent_activity`]; this blocks on it.
///
/// Advertised in `tools/list`, kept OUT of [`V1_TOOLS`] for the SAME reason as `wait_for_command`:
/// it is an observational convenience over an existing signal, not a frozen-contract tool.
pub const WAIT_FOR_TERMINAL_TOOL: &str = "wait_for_terminal";

/// The `agent_session_event` tool name (PRD-5 #4, ADR-0004 / ADR-0010). The channel
/// a Claude Code `SessionStart`/`SessionEnd` hook of type `mcp_tool` reaches nyx on:
/// it is addressed `mcp__nyx__agent_session_event` and carries the agent's raw hook
/// payload (`hook_event_name`, `session_id`, `cwd`, `transcript_path`, `source`,
/// `NYX_TERMINAL_ID`). nyx normalizes it through the `claude_code` adapter and
/// upserts/ends the matching `agent_sessions` row. Advertised in `tools/list`, kept
/// OUT of [`V1_TOOLS`] (same pattern as `probe` and the other extension tools: it is
/// a PRD-5 capability layered over the existing DB, not part of the frozen contract
/// clients onboard against). Like `probe` it answers even with no managed runtime —
/// it just reports `mcp_unavailable` so the best-effort hook degrades cleanly.
pub const AGENT_SESSION_EVENT_TOOL: &str = "agent_session_event";

/// The `set_project_settings` tool name. Modifies a project's mutable SETTINGS — its
/// display `name` (rename) and its `resume_agent_sessions` opt-in (PRD-5 #5) — via the
/// SAME `db::update_project` / `db::set_project_resume_agent_sessions` path the UI's
/// project-settings modal drives. Partial update: only the fields supplied change. It
/// is the MCP mirror of the previously UI-only project config (the gap that forced a
/// manual DB edit to flip `resume_agent_sessions`). Advertised in `tools/list`, kept
/// OUT of [`V1_TOOLS`] (same pattern as the other extension tools — it layers over the
/// existing DB, it is not part of the frozen onboarding contract). It does NOT create a
/// project (that stays a UI gesture, per [`SERVER_INSTRUCTIONS`]).
pub const SET_PROJECT_SETTINGS_TOOL: &str = "set_project_settings";

/// The `project_set_repos` tool name (finding #27, PDR-0013). CONFIGURES the repositories
/// of a project EXPLICITLY — the association that replaces the removed disk auto-detection.
/// `{ project_id, repos: [{ relative_path, name? }] }` reconciles the project's logical
/// repo set (each a `relative_path` within the project, `"."` for a mono-repo at the
/// workspace root); a repo dropped from the list is archived. Every workspace/worktree of
/// the project then DERIVES these repos at `workspace.root_path + relative_path`. A project
/// left with NO configured repos falls back to the workspace's current folder. Routes
/// through the SHARED `db::set_project_repositories` (PDR-0005). Advertised in `tools/list`,
/// kept OUT of [`V1_TOOLS`] (same pattern as the other extension tools). NEVER scans disk.
pub const PROJECT_SET_REPOS_TOOL: &str = "project_set_repos";

/// The v2.2 cockpit read model tool (task `01KWHPV32YVCRBNY1JW5CDY4AY`): `{ workspace_id }`
/// → the workspace's LIVE runtime aggregate (terminals + their agent sessions, the
/// cross-terminal agent sessions, and the managed-command SERVICES with current run
/// state). Read-only, no polling. Advertised in `tools/list`, kept OUT of [`V1_TOOLS`]
/// (same pattern as the other extension tools — it layers a new read over the existing
/// DB + runner authorities). Routes through the SHARED `workspace_runtime::snapshot`,
/// the SAME read model the UI cockpit consumes (PDR-0005).
pub const WORKSPACE_RUNTIME_TOOL: &str = "workspace_runtime";

/// The v2.2 Git CHANGES read model tool (task `01KWHPV33EGTB3Z41J0RQR1BAK`, PDR-0013):
/// `{ workspace_id }` → per non-optional repository, the current branch, dirty flag, and
/// changed-file SUMMARY, RE-READ from Git and NEVER authoritative in the DB. NOT a
/// line-level diff (the review engine is v2.3). Advertised in `tools/list`, kept OUT of
/// [`V1_TOOLS`] (same pattern as the other extension tools). Routes through the SHARED
/// `workspace_git::summary`, the SAME read model the UI cockpit consumes (PDR-0005).
pub const WORKSPACE_GIT_TOOL: &str = "workspace_git";

/// The v2.2 workspace TIMELINE read model tool (task `01KWHPV33SAV9NV1STSY403CF0`):
/// `{ workspace_id, limit? }` → the workspace's most recent `domain_events` (v2.1) as a
/// BOUNDED, oldest-first activity fil — the business "changements notables" (workspace /
/// repository / project lifecycle), NEVER a terminal byte (the outbox carries none). NOT the
/// full contextual activity timeline (v2.5). Advertised in `tools/list`, kept OUT of
/// [`V1_TOOLS`] (same pattern as the other extension tools). Routes through the SHARED
/// `workspace_timeline::timeline`, the SAME read model the UI cockpit consumes (PDR-0005).
pub const WORKSPACE_TIMELINE_TOOL: &str = "workspace_timeline";

/// The v2.4 GIT DIFF read model tool (task `01KX0F6FD836YASVYT17YV3FTW`, PDR-0007 /
/// PDR-0015): `{ workspace_id, repository_id, kind, rev?, base?, target?, mode? }` → the
/// line-level diff of ONE repository within a workspace, for a review surface
/// (working_tree / staged / unstaged / untracked / commit / range / branches), LIVING or
/// PINNED. gix primary, git CLI fallback; the diff never mixes Git identities across
/// repos. NOT a stored review (the review is a vue dérivée, PDR-0015). Advertised in
/// `tools/list`, kept OUT of [`V1_TOOLS`]. Routes through the SHARED
/// `git_diff::diff_repo_with_mode`, the SAME engine the UI bridge's `repo_diff`
/// db-command drives (PDR-0005).
pub const REPO_DIFF_TOOL: &str = "repo_diff";

/// The v2.4 DERIVED GIT VIEWS read model tool (task `01KX0F6FXE6AJ9BR4BYHRX6B1R`, Q-015 /
/// PDR-0013): `{ workspace_id, repository_id, base?, target? }` → per-repo dirty state
/// (staged / modified / untracked), current branch + base/target, ahead/behind, and a
/// change summary — RE-READ from Git on demand, NEVER authoritative in the DB. Advertised
/// in `tools/list`, kept OUT of [`V1_TOOLS`]. Routes through the SHARED
/// `git_views::repo_view_with_target`, the SAME derivation the UI bridge's `repo_git_view`
/// db-command drives (PDR-0005).
pub const REPO_GIT_VIEW_TOOL: &str = "repo_git_view";

/// The v2.4 REAL-TIME REFRESH registration tool (task `01KX0F6GDZEH3YN6QJDB7FC0WC`,
/// Q-015): `{ workspace_id, repository_id, base?, target? }` → `{ watching: true }`. Begins
/// a debounced fs-watch of the `(workspace, repository)` so an edit / commit / checkout
/// recomputes the derived view and PUSHES `repos://changed` to BOTH the UI and MCP — the
/// SAME watcher the UI `repo_watch` db-command drives (PDR-0005). Handled sidecar-side (the
/// git-watch runtime is a shell-owned effect), advertised here, kept OUT of [`V1_TOOLS`].
pub const REPO_WATCH_TOOL: &str = "repo_watch";

/// The counterpart to [`REPO_WATCH_TOOL`]: `{ workspace_id, repository_id }` →
/// `{ watching: false }`. Stops the real-time watch of a `(workspace, repository)`.
pub const REPO_UNWATCH_TOOL: &str = "repo_unwatch";

/// The v2.4 review COMMENT LIST tool (task `01KX0F6HEBPK34CV49EREHK3Y9`, PDR-0012/0015):
/// `{ workspace_id, repository_id, spec_id? }` → `{ comments }`, the whole auto-threaded feed of a
/// `(workspace, repository)` review (roots + replies, anchored + global, with `outdated`
/// state). Routes through the SHARED `db::list_review_comments`, the SAME model the UI bridge's
/// `review_comment_list` db-command reads (PDR-0005). Advertised in `tools/list`, kept OUT of
/// [`V1_TOOLS`]. Read-only.
///
/// `spec_id` WAS HONOURED AND UNADVERTISED — the `01M1008NHM9RRA1ZAS4V7K1PM4` defect on a read.
/// [`crate::mcp_tools_core::review_comment_list`] has always narrowed the feed by it, and the
/// `inputSchema` named neither the argument nor, once the short grammar reached it, the forms it
/// accepts. An argument `tools/list` omits cannot be discovered — and the guard that holds the
/// Spec-reference advertisement reads the SCHEMA, so it could not see this field at all.
pub const REVIEW_COMMENT_LIST_TOOL: &str = "review_comment_list";

/// The v2.4 review COMMENT POST tool (task `01KX0F6HEBPK34CV49EREHK3Y9`):
/// `{ workspace_id, repository_id, body, spec_id?, parent_id?, file_path?, side?, line_start?,
/// line_end?, blob?, snippet?, context? }` → `{ comment }`. The AGENT façade to annotate a
/// review — a global comment, an anchored comment, OR a REPLY (`parent_id`). The author is
/// ALWAYS `agent` here (the human posts through the UI). The agent may reply and resolve ITS OWN
/// (`author_type=agent`) comments ([`REVIEW_COMMENT_RESOLVE_TOOL`]); a HUMAN comment stays
/// human-only. Routes through the SHARED `db::insert_review_comment` (PDR-0005). Advertised in
/// `tools/list`, kept OUT of [`V1_TOOLS`].
pub const REVIEW_COMMENT_POST_TOOL: &str = "review_comment_post";

/// The v2.5.11 review COMMENT RESOLVE tool (review follow-up finding `01KXZYWGBQEJZ9BM39NYGFKHMJ`):
/// `{ id }` → `{ comment }`. A comment is resolved BY ITS AUTHOR — this AGENT façade closes an
/// `author_type=agent` comment (the agent's own), and REFUSES an `author_type=human` one
/// (`invalid_state`): a human comment stays human-only, closed through the human/UI façade
/// (`review_comment_resolve` db-command). Routes through the SHARED `db::resolve_agent_comment`.
/// Advertised in `tools/list`, kept OUT of [`V1_TOOLS`]. Mutating (`Repos` changed).
pub const REVIEW_COMMENT_RESOLVE_TOOL: &str = "review_comment_resolve";

/// The review FINDING LIST tool (task `01KX0F6JEKCEV2JCTMEBMM3P4Z`; v2.5.11 cutover, PDR-0016):
/// `{ workspace_id, repository_id, spec_id? }` → `{ tasks }`, the GRILLED findings of a Spec — each
/// finding's `from_comment_id`, `title`, optional `severity`, and `grill_state` (proposed |
/// structured; `structured` = coder-ready). Routes through the SHARED `db::list_grilled_findings_for_spec` — the
/// CANONICAL `tasks kind=review` model (NOT the deprecated `review_tasks` table), the SAME model the
/// UI bridge reads. Findings are Spec-scoped. Advertised in `tools/list`, kept OUT of [`V1_TOOLS`].
/// Read-only.
pub const REVIEW_TASK_LIST_TOOL: &str = "review_task_list";

/// The review FINDING STRUCTURE tool (task `01KX0F6JEKCEV2JCTMEBMM3P4Z`): the AGENT's step of the
/// "grill" — and its FINAL step. `{ id, title, details?, severity? }` → `{ task }`. Rewrites a
/// PROPOSED (or still-structured) finding into a clean task and moves it to `structured` — the
/// coder-ready state. The grill collapsed to two states (review follow-up #31): there is NO
/// `validated` step and DELIBERATELY no `review_task_validate` tool of any façade. Routes through the
/// SHARED `db::structure_finding` — the CANONICAL `tasks kind=review` finding model (v2.5.11 cutover,
/// PDR-0016; NOT `review_tasks`). Advertised in `tools/list`, kept OUT of [`V1_TOOLS`].
pub const REVIEW_TASK_STRUCTURE_TOOL: &str = "review_task_structure";

/// The v2.5.11 RETOUR-CHAT tool (task `01KXR1CZ68C5S9CY2KFB073KDM` — "Retour-chat = canal de
/// review"): `{ workspace_id, repository_id, spec_id, body, severity?, title? }` →
/// `{ comment, finding }`. The agent RELAYS the human's chat feedback as a review channel — it
/// creates a HUMAN, GLOBAL (non-anchored) `review_comments` row (the human instructed directly, so
/// presence = authorization — no anchor needed) and immediately GRILLS it into a `proposed`
/// `tasks kind=review` finding (agent-owned, closable by the coder loop like any finding). This
/// gives a chat retour the SAME path to review as the viewer, which stays a valid channel in
/// parallel. Routes through the
/// SHARED `db::insert_review_comment` + `db::promote_comment_to_finding`. Advertised in `tools/list`,
/// kept OUT of [`V1_TOOLS`]. Mutating (`Repos` changed).
pub const REVIEW_SUBMIT_FEEDBACK_TOOL: &str = "review_submit_feedback";

/// The frozen MCP v1 tool surface (ADR-0003). Phase-1 advertises these names in
/// the `tools/list` handshake; the call bodies are wired in phase 2 over the PRD-3
/// runtime/DB layer (D6). Order is the ADR's listing order.
pub const V1_TOOLS: [&str; 9] = [
    "list_projects",
    "list_workspaces",
    "list_commands",
    "start_command",
    "stop_command",
    "relaunch_command",
    "get_command_output",
    "workspace_add",
    "create_workspace",
];

/// Reported MCP server name in the `initialize` handshake's `serverInfo`. v2-namespaced
/// (`nyx-v2`, NOT v1's `nyx`) so it is consistent with the bundled `.mcp.json` registration
/// key and v2 coexists with a v1 MCP server in a client's config (review #39).
const SERVER_NAME: &str = "nyx-v2";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// MCP protocol revision nyx speaks (date-versioned, per the MCP spec).
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Server-level guidance returned in the `initialize` result's `instructions`
/// field (per the MCP spec). This is the FIRST thing a connecting agent reads, so
/// it states — in plain, consumer-facing terms — what nyx is and when/why to reach
/// for it, before the agent even looks at the tool list. Kept concise and free of
/// internal/dev jargon.
const SERVER_INSTRUCTIONS: &str = "\
nyx is a local terminal and development cockpit running on the user's machine. It \
organizes their coding work into projects and workspaces (a workspace is a folder on \
disk), and it launches and supervises long-running development commands (dev servers, \
build watchers, test runners), capturing their output.

Use nyx's tools to:
- Discover what the user is working on: list their projects, the workspaces inside a \
project, and the commands available in (or running for) a workspace.
- Launch, stop, and relaunch a managed command, then read its captured output to see \
what happened (startup logs, errors, a dev server's URL, test results).
- Register or create a workspace folder when the user wants nyx to track a new one.
- Adjust an EXISTING project's settings: rename it, or turn agent-session resume on or \
off (`set_project_settings`).
- Reach the user's SYSTEM commands: machine-wide services that belong to no project, \
SHARED by every project and agent — stopping one stops it for all. Absent from \
list_projects and list_workspaces: `list_system_commands` is the door to them. Reading \
one and starting a stopped one are free; anything that CHANGES them waits for the user's \
approval in the nyx app, and nyx performs it itself.

Reach for nyx whenever a request is about the user's local projects or about running, \
inspecting, or controlling development commands on their machine. Commands are \
identified by an `instance_id` — use that to start, stop, relaunch, or read output. \
Output reads are incremental: a result returns a `cursor` you can pass back to fetch \
only what is new.

Do NOT use nyx when:
- The task is a one-off shell command unrelated to a registered project or workspace \
(e.g. \"run echo hello\") — run it directly in the shell instead.
- The user wants to create a new project or add it to nyx for the first time — that is \
a UI gesture in the nyx app; nyx tools operate on projects that already exist (you CAN \
rename one, or change its settings with set_project_settings).";

/// How many requests the MCP surface serves AT ONCE (v2.8 L1). The accept loop hands each request
/// to this pool instead of serving it on the accepting thread.
///
/// The single-thread assumption this replaces held only while every request was short. It is FALSE
/// for `nyx_next`: a `gate_check` runs the project's own test suite ON the dispatch path, for up to
/// [`crate::mcp_engine::GATE_TOTAL_TIMEOUT`]. Served inline, that one call accepted nothing else —
/// no other agent, no other session, no other tool call — for as long as the gate took, and the
/// user hit that freeze twice in a day on a suite of a few seconds run several times per task.
///
/// BOUNDED rather than one thread per request: an unbounded spawn turns a burst — or a client
/// retrying a call it wrongly believes hung — into unbounded threads, which is a worse failure than
/// the one being fixed. Four is sized on the only long operation there is: a gate holds one worker,
/// and three stay free for everyone else.
///
/// # A long operation that DEPENDS on an incoming request must not be able to fill this
///
/// `wait_for_terminal` is not like a gate: the signal that ends it (the agent's `Stop` hook) is a
/// `POST /mcp` served by THIS pool, so four concurrent waits would deadlock themselves — the hook
/// sits in the queue, gives up at its own 1 s timeout with no replay, and every wait times out on a
/// turn that finished long before. That circularity is closed at the source rather than by growing
/// this number ([`crate::mcp_tools_core::MAX_CONCURRENT_TERMINAL_WAITS`], which reads this and
/// keeps one worker free for the short requests).
pub(crate) const REQUEST_WORKERS: usize = 4;

/// How many ACCEPTED requests wait for a free worker. Only reached once all [`REQUEST_WORKERS`] are
/// busy; past it the accept loop itself blocks and the kernel's listen backlog holds the pending
/// connections. So a saturated pool QUEUES: no request is dropped, and none is ever answered by
/// something other than the handler that served it. Bounded so a client looping on a call it thinks
/// is wedged cannot grow nyx's memory (and its held sockets) without limit.
const REQUEST_QUEUE_CAP: usize = 64;

/// Resolve the MCP port (ADR-0003 D2): `NYX_MCP_PORT` when it parses to a non-zero
/// `u16`, else [`DEFAULT_PORT`]. Port 0 ("any free port") is rejected so the port
/// is stable between launches.
pub fn resolve_port() -> u16 {
    match std::env::var(PORT_ENV) {
        Ok(raw) => raw
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .unwrap_or(DEFAULT_PORT),
        Err(_) => DEFAULT_PORT,
    }
}

/// Managed Tauri state: the single, loopback-bound MCP server.
///
/// `start`-once is enforced by `started`: the first [`McpServer::start`] binds the
/// listener and spawns the accept loop; any later call is a no-op (ADR-0003 D3).
/// Combined with nyx's process-level single-instance, there is at most one server.
pub struct McpServer {
    /// Latches `true` on the first successful [`McpServer::start`]; guards against a
    /// second listener within the same process.
    started: AtomicBool,
    /// The port the listener actually bound (0 until started). Read by tests/health.
    bound_port: AtomicU16,
    /// The accept-loop thread handle, kept so the server's lifetime is tied to the
    /// process and the thread can be joined on shutdown if needed.
    handle: Mutex<Option<JoinHandle<()>>>,
    /// The live loopback listener, shared with the accept-loop thread. Held here so
    /// [`McpServer::stop`] can `unblock` the accept loop AND, once the thread is joined
    /// and this last clone is dropped, let `tiny_http::Server`'s `Drop` close the
    /// listening socket (subsequent connects are then refused). `None` until started.
    listener: Mutex<Option<Arc<tiny_http::Server>>>,
    /// The tool dispatcher invoked for `tools/call`. `None` until phase-2 wires the
    /// PRD-3-backed tools; the handshake (`initialize`/`tools/list`) works without it.
    dispatcher: Mutex<Option<Arc<dyn ToolDispatcher>>>,
}

impl Default for McpServer {
    fn default() -> Self {
        Self {
            started: AtomicBool::new(false),
            bound_port: AtomicU16::new(0),
            handle: Mutex::new(None),
            listener: Mutex::new(None),
            dispatcher: Mutex::new(None),
        }
    }
}

/// The phase-2 extension point: dispatches a `tools/call` to the PRD-3-backed tool
/// implementations (ADR-0003 D6). Phase-1 leaves it unset; `tools/call` then
/// returns a `method not yet available` error rather than a second lifecycle.
pub trait ToolDispatcher: Send + Sync + 'static {
    /// Invoke tool `name` with JSON `arguments`; return the tool's JSON result, or a
    /// [`RpcError`] (ADR-0003 D8 error vocabulary) on failure.
    fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError>;

    /// Invoke tool `name` KNOWING which terminal the MCP connection is bound to
    /// (`caller_terminal_id` — the [`TERMINAL_ID_HEADER`] the request carried, `None` when
    /// absent). This is the seam the v2.5.10 auto-map hangs on: a spec-scoped mutation is
    /// attributed to its calling terminal's live session so the worked Spec joins that
    /// session's set (see `mcp_tools_core::dispatch_extension_tool`). The DEFAULT ignores the
    /// caller and delegates to [`ToolDispatcher::call`], so a dispatcher that does not care
    /// (every test dispatcher, any non-terminal-aware host) is unchanged; the sidecar
    /// dispatcher overrides it to thread the caller into its dispatch.
    fn call_with_caller(
        &self,
        name: &str,
        arguments: &Value,
        caller_terminal_id: Option<&str>,
    ) -> Result<Value, RpcError> {
        let _ = caller_terminal_id;
        self.call(name, arguments)
    }
}

/// The ADR-0003 D8 error-code vocabulary, as a CLOSED enum. Modelling the code as a type
/// (not a free `&'static str`) means a typo can't compile, `rpc_code` maps it
/// EXHAUSTIVELY (no silent `_ => -32000` fallthrough for an unknown string), and the wire
/// `error.data.code` is always [`RpcCode::as_str`]. Adding a code is one edit here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcCode {
    /// An argument is missing, the wrong type, or otherwise invalid.
    InvalidArgument,
    /// A referenced id (instance / command / workspace / terminal) does not exist.
    InvalidId,
    /// The operation is invalid for the current state (e.g. stop a non-running run).
    InvalidState,
    /// The requested output window exceeds the byte ceiling.
    OutputTooLarge,
    /// The managed runtime (or a state it needs) is not available yet.
    McpUnavailable,
    /// The tool / method name is not recognized.
    MethodNotFound,
    /// The operation aims a capability the **Système** scope does not have (PRD « Scope
    /// Système »). DELIBERATELY DISTINCT FROM [`RpcCode::InvalidId`]: a caller must be able to
    /// tell "this container exists and you may not do that to it" from "this id names nothing",
    /// and the Système singleton is never protected by having its id hidden — the refusal keys on
    /// the container's NATURE, read from the database by the boundary the two adapters pose
    /// ([`crate::system_scope::guard_payload`]), so learning the id buys nothing. Merging the two
    /// codes would make that guarantee unobservable.
    SystemScopeForbidden,
    /// An unexpected internal failure (DB error, etc.).
    Internal,
}

// THERE IS NO `ApprovalRequired` CODE, and its absence is the protocol (PDR-0030, review finding
// 01M1C7CWMF4ABHRWT1DGBJTXW3). There used to be: a protected Système call answered with it and
// carried its ticket in `error.data`. A real agent showed that channel does not exist — no MCP
// client is obliged to render `error.data` to the model, and the common one renders only
// `error.message` — so the capability was unreachable in practice while every test was green.
//
// "Waiting on the user" is therefore no longer an error at all: the call BLOCKS, and if the human
// has not answered by the cap it returns a tool RESULT saying so, in the channel every client
// renders. What remains an error is a DECISION — refused, or expired — and those are
// [`RpcCode::InvalidState`], with everything the agent needs in `message`.

impl RpcCode {
    /// The stable wire string emitted in `error.data.code`.
    pub fn as_str(self) -> &'static str {
        match self {
            RpcCode::InvalidArgument => "invalid_argument",
            RpcCode::InvalidId => "invalid_id",
            RpcCode::InvalidState => "invalid_state",
            RpcCode::OutputTooLarge => "output_too_large",
            RpcCode::McpUnavailable => "mcp_unavailable",
            RpcCode::MethodNotFound => "method_not_found",
            RpcCode::SystemScopeForbidden => crate::system_scope::SYSTEM_SCOPE_FORBIDDEN,
            RpcCode::Internal => "internal",
        }
    }

    /// The JSON-RPC numeric code for the transport envelope: reserved-range codes for
    /// protocol-level faults, `-32000` (server error) for every domain/tool error.
    fn json_rpc(self) -> i64 {
        match self {
            RpcCode::MethodNotFound => -32601,
            RpcCode::InvalidArgument => -32602,
            RpcCode::InvalidId
            | RpcCode::InvalidState
            | RpcCode::OutputTooLarge
            | RpcCode::McpUnavailable
            | RpcCode::SystemScopeForbidden
            | RpcCode::Internal => -32000,
        }
    }
}

impl From<crate::system_scope::SystemScopeError> for RpcError {
    /// Carry a domain-boundary refusal onto the MCP wire WITHOUT collapsing it into
    /// `invalid_id`. [`crate::system_scope::SystemScopeError::Forbidden`] becomes the dedicated
    /// [`RpcCode::SystemScopeForbidden`], with the refused `capability` and the aimed-at entity in
    /// `error.data` so an agent can react without parsing prose. Every other arm is a failure of
    /// nyx itself (a SQL error, or a bootstrap state that should have refused the boot), so it maps
    /// to `internal`.
    fn from(err: crate::system_scope::SystemScopeError) -> Self {
        use crate::system_scope::SystemScopeError;
        match &err {
            SystemScopeError::Forbidden {
                entity,
                id,
                capability,
            } => RpcError::new(RpcCode::SystemScopeForbidden, err.to_string()).with_data(
                serde_json::json!({ "entity": entity, "id": id, "capability": capability }),
            ),
            _ => RpcError::new(RpcCode::Internal, err.to_string()),
        }
    }
}

impl std::fmt::Display for RpcCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for RpcError {
    /// `code: message`, WITHOUT the `data` bag — see [`RpcError::describe`] for why the omission
    /// is deliberate rather than an economy.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Compare directly against the wire string so tests keep reading naturally, e.g.
/// `assert_eq!(err.code, "invalid_id")`.
impl PartialEq<&str> for RpcCode {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// A standardized MCP/JSON-RPC error (ADR-0003 D8). `code` is the typed D8 vocabulary
/// ([`RpcCode`]); `message` is human-readable; `data` is OPTIONAL structured detail
/// merged into the wire envelope's `error.data` ALONGSIDE the string `code` (e.g.
/// `output_too_large` carries `{ requested, limit }` so an agent can react without
/// parsing the prose). It must be a JSON object (its keys are merged in); a non-object
/// `data` is ignored.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: RpcCode,
    pub message: String,
    pub data: Option<Value>,
}

/// What replaces a database's own sentence when one reaches [`RpcError::new`] — see
/// [`without_sql_text`]. It says the two things the withheld text could have said and the caller can
/// act on: the fault is in what was sent, and re-sending it unchanged will fail the same way.
const SQL_TEXT_WITHHELD: &str = "the storage layer refused the operation. Its own message is \
                                 withheld: it names schema internals you cannot act on. In \
                                 practice this refusal means an argument is wrong — an id naming \
                                 no row, or a value outside a field's accepted set — so re-read \
                                 the arguments you sent rather than retrying the same call";

/// The SENTENCES A DATABASE WRITES, which nothing renders to an agent.
///
/// The measured shape (2026-09-01): `task_add` with an unknown Spec answered `db error: FOREIGN KEY
/// constraint failed`, and with a bad `kind`, `db error: CHECK constraint failed: kind IN
/// ('slice','human','review')`. The right fix is upstream — the write path knows which argument is
/// at fault and now refuses by name — and this is the NET under it, so that a path nobody has
/// audited yet cannot put SQLite's prose on the wire. Every `RpcError` is built through
/// [`RpcError::new`], which makes this the one place the guarantee can be stated once.
///
/// It CUTS rather than blanks: the caller's own lead-in (`create workspace failed: …`) says which
/// operation died, which is useful and is not SQL; only the clause carrying the database's sentence
/// is replaced.
///
/// # It runs on [`RpcCode::Internal`] ONLY (#K2)
///
/// The markers are SQLite's literal wordings, and the reasoning was that they do not occur in nyx's
/// own refusals. That is true of nyx's PROSE and false of its refusals, because a good refusal
/// QUOTES THE CALLER: `task_add(kind: "no such table")` answered `unknown kind 'no such table'
/// (accepted: slice, human, review)`, whose marker sits before any `": "` — so the head was empty
/// and the WHOLE message, list of accepted values and all, was replaced by a sentence about the
/// storage layer that had never been consulted. Same for `get_command_output(grep: "syntax
/// error(")`, where the regex parse error vanished into a false SQL diagnosis.
///
/// A message that carries a database's sentence is, by construction, an `internal` — nyx does not
/// know what went wrong. `invalid_argument` / `invalid_id` / `invalid_state` are written by nyx
/// ABOUT the caller's own input, which is exactly the text this net must not touch. So the net
/// keeps everything it was built to catch and stops mutilating what it was never aimed at.
fn without_sql_text(message: String) -> String {
    const MARKERS: [&str; 6] = [
        "constraint failed",
        "no such table",
        "no such column",
        "sql logic error",
        "syntax error",
        "unrecognized token",
    ];
    let lower = message.to_ascii_lowercase();
    let Some(hit) = MARKERS.iter().filter_map(|m| lower.find(m)).min() else {
        return message;
    };
    // Everything before the clause the database's sentence sits in — `db error`,
    // `create workspace failed`, … — is nyx's own words and stays.
    let head = message[..hit]
        .rsplit_once(": ")
        .map(|(before, _)| before.trim())
        .unwrap_or("");
    if head.is_empty() {
        SQL_TEXT_WITHHELD.to_string()
    } else {
        format!("{head}: {SQL_TEXT_WITHHELD}")
    }
}

impl RpcError {
    /// Construct an error with the ADR-0003 D8 `code` and a message.
    /// Phase-2 tool implementations build their failures through this.
    ///
    /// An [`RpcCode::Internal`] message passes through [`without_sql_text`]: whatever a caller
    /// assembles, an agent never receives a database's own sentence. The other codes are nyx's own
    /// refusals ABOUT the caller's arguments — they quote the caller, never SQL, and go out
    /// verbatim (see [`without_sql_text`] for the false positives that cost).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(code: RpcCode, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            code,
            message: match code {
                RpcCode::Internal => without_sql_text(message),
                _ => message,
            },
            data: None,
        }
    }

    /// Attach structured `data` (a JSON OBJECT) carried into the wire envelope's
    /// `error.data` alongside the string `code` (PRD-4.1 task #3). The object's keys are
    /// merged into `error.data` so e.g. `output_too_large` exposes machine-readable
    /// `requested`/`limit`. Builder form so existing `RpcError::new(...)` sites are
    /// unaffected.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// The one-line rendering of this error, for the places that need a `Display` rather than a
    /// wire envelope — today the Système approval ledger's outcome journal, which records WHY an
    /// authorised effect failed (`system_approvals::grant_and_execute`, whose executor stringifies
    /// the failure through here).
    ///
    /// THE `data` BAG IS STILL NOT RENDERED, and the rule outlives its original reason. It was
    /// written because `data` could carry the approval token, and this string is persisted; there is
    /// no token any more (PDR-0030), but `data` remains the STRUCTURED half of an error — arbitrary,
    /// unbounded, and shaped by whichever tool built it — while this is a one-line human sentence
    /// destined for a 200-character journal column. Rendering it would put an unbounded blob in the
    /// database for no reader's benefit.
    fn describe(&self) -> String {
        format!("{}: {}", self.code.as_str(), self.message)
    }
    /// JSON-RPC numeric code for the transport envelope (delegates to the typed code).
    fn rpc_code(&self) -> i64 {
        self.code.json_rpc()
    }
}

impl McpServer {
    /// Bind the loopback listener on the resolved port and spawn the accept loop.
    /// START-ONCE (ADR-0003 D3): the first call wins; a later call is a no-op and
    /// returns the already-bound port. Returns the bound port on success.
    ///
    /// Binds STRICTLY to `127.0.0.1` (ADR-0003 D1): the socket address is
    /// `Ipv4Addr::LOCALHOST`, never `0.0.0.0`, so the surface is unreachable off-box.
    pub fn start(self: &Arc<Self>) -> std::io::Result<u16> {
        self.start_on(resolve_port())
    }

    /// TEST-ONLY: bind an EPHEMERAL port (`:0`) the OS picks, returning what it gave.
    ///
    /// The stability guarantee of [`start`](Self::start) is a PRODUCTION property (an onboarded
    /// client's `http://127.0.0.1:<port>/mcp` config must survive a restart), which is why
    /// [`resolve_port`] rejects 0. A TEST needs the opposite: several `cargo test` processes run
    /// at once here, so any shared number makes them fail each other's binds with `AddrInUse` —
    /// a suite that goes red for a reason unrelated to the code, which is how a real failure gets
    /// dismissed as noise.
    #[cfg(test)]
    pub fn start_ephemeral(self: &Arc<Self>) -> std::io::Result<u16> {
        self.start_on(0)
    }

    /// The shared bind body: latch the start, bind `127.0.0.1:port`, record the port the OS
    /// really gave (`port` is what was ASKED for — 0 means "any free one"), and spawn the accept
    /// loop.
    fn start_on(self: &Arc<Self>, port: u16) -> std::io::Result<u16> {
        // Latch the start: only the first caller proceeds to bind a listener.
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(self.bound_port.load(Ordering::SeqCst));
        }
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let server = match tiny_http::Server::http(addr) {
            Ok(s) => s,
            Err(e) => {
                // Unlatch so a later retry (e.g. after freeing the port) can bind.
                self.started.store(false, Ordering::SeqCst);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("MCP server could not bind 127.0.0.1:{port}: {e}"),
                ));
            }
        };
        // The bound port can differ from the asked-for one (an ephemeral `:0` bind, or tiny_http
        // picking when several share a default); record what we actually bound so
        // health/onboarding reflect reality.
        let real_port = server
            .server_addr()
            .to_ip()
            .map(|a| a.port())
            .unwrap_or(port);
        self.bound_port.store(real_port, Ordering::SeqCst);

        // Share the listener with the accept-loop thread through an `Arc`: the thread reads
        // requests off it, and [`McpServer::stop`] keeps a clone so it can `unblock` the
        // loop and — once the thread is joined and both clones drop — close the socket.
        let server = Arc::new(server);
        *self.listener.lock().unwrap() = Some(Arc::clone(&server));

        let this = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name("nyx-mcp-http".into())
            .spawn(move || this.accept_loop(server))
            .expect("spawn nyx-mcp-http thread");
        *self.handle.lock().unwrap() = Some(handle);
        Ok(real_port)
    }

    /// Gracefully STOP the HTTP listener: unblock the accept loop so its thread exits,
    /// join it, then drop the last `Arc<tiny_http::Server>` so `tiny_http`'s `Drop`
    /// closes the listening socket. After this returns, a new connection to the bound
    /// port is REFUSED — nothing accepts it — so a late `POST /mcp` (e.g. the
    /// `SessionEnd(reason=other)` hook of a Claude killed during shutdown) can no longer
    /// reach the dispatcher. Bounded: `unblock` + a single `join`, and the accept loop is only ever
    /// accepting or handing a request to the pool — never inside a handler, which is what lets an
    /// ordered shutdown proceed while a gate is still running (the workers are detached and drain
    /// themselves once this drops the queue's sender — see [`McpServer::accept_loop`]). Idempotent —
    /// a second call (or a call before `start`) is a no-op.
    pub fn stop(&self) {
        // Take the listener clone FIRST so it is dropped at the end of this scope, AFTER
        // the accept-loop thread has been joined (and thus dropped its own clone).
        let listener = self.listener.lock().unwrap().take();
        if let Some(server) = &listener {
            // Break the blocking `incoming_requests()` so `accept_loop` returns.
            server.unblock();
        }
        if let Some(handle) = self.handle.lock().unwrap().take() {
            let _ = handle.join();
        }
        // The accept-loop thread has released its clone; dropping this last one here runs
        // `tiny_http::Server::drop`, which closes the loopback listener socket.
        drop(listener);
        // Unlatch so a later `start` could rebind if ever needed (shutdown normally exits
        // the process right after, but keep the flag honest).
        self.started.store(false, Ordering::SeqCst);
    }

    /// Install the phase-2 tool dispatcher (ADR-0003 D6). Wired from the setup hook
    /// once the PRD-3 runtime/DB managed state exists.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_dispatcher(&self, dispatcher: Arc<dyn ToolDispatcher>) {
        *self.dispatcher.lock().unwrap() = Some(dispatcher);
    }

    /// The port the listener actually bound, or 0 if not started yet.
    pub fn bound_port(&self) -> u16 {
        self.bound_port.load(Ordering::SeqCst)
    }

    /// Whether the server has been started (latched).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// The blocking accept loop: take each request off the listener and hand it to the WORKER POOL
    /// ([`REQUEST_WORKERS`]) rather than serving it here. A request served on THIS thread is a
    /// request that stops the loop from accepting the next one, and one `tools/call` legitimately
    /// runs for minutes (a `nyx_next` whose `gate_check` runs the project's test suite) — so this
    /// thread only ever accepts.
    ///
    /// The workers are DETACHED on purpose. [`McpServer::stop`] joins THIS thread, which is only
    /// ever inside `incoming_requests` (or handing a request over), and dropping the queue's sender
    /// on the way out lets the workers finish what they hold, drain what is queued, and exit on
    /// their own. Joining them instead would make an ordered shutdown wait out whatever gate happens
    /// to be running. They hold nothing that outlives them: a `tiny_http::Request` owns its own
    /// socket, so a worker can still answer a connection accepted before the stop, and the listening
    /// socket still closes the moment `stop` drops the last `Arc<Server>`.
    fn accept_loop(self: Arc<Self>, server: Arc<tiny_http::Server>) {
        let (tx, rx) = std::sync::mpsc::sync_channel::<tiny_http::Request>(REQUEST_QUEUE_CAP);
        // One receiver shared by the pool: the workers compete for the next request, and whoever
        // wins serves it. The lock is held ACROSS the `recv` only — never across the request — so a
        // worker inside a 90s gate cannot keep the others off the queue.
        let rx = Arc::new(Mutex::new(rx));
        for n in 0..REQUEST_WORKERS {
            let this = Arc::clone(&self);
            let rx = Arc::clone(&rx);
            std::thread::Builder::new()
                .name(format!("nyx-mcp-worker-{n}"))
                .spawn(move || loop {
                    let next = rx.lock().unwrap_or_else(|p| p.into_inner()).recv();
                    match next {
                        Ok(request) => this.handle_request(request),
                        // The accept loop is gone and the queue is drained: nothing more will come.
                        Err(_) => break,
                    }
                })
                .expect("spawn nyx-mcp-worker thread");
        }
        for request in server.incoming_requests() {
            // A FULL queue blocks here rather than dropping the request: the connection waits in
            // the kernel's listen backlog and is served as soon as a worker frees up. Every worker
            // would have to be busy AND 64 requests already waiting to reach that, which on a
            // loopback surface driven by a handful of local agents means something is very wrong —
            // and queueing is still the honest answer to it.
            if tx.send(request).is_err() {
                break;
            }
        }
    }

    /// Route one HTTP request: `GET /health`, `POST /mcp`, else 404. Enforces the
    /// loopback `Origin`/`Host` guard (ADR-0003 D9) before any MCP handling.
    fn handle_request(&self, mut request: tiny_http::Request) {
        let method = request.method().clone();
        let url = request.url().to_string();
        // Anti DNS-rebinding (D9): reject a cross-origin browser request before it
        // reaches the MCP surface. The Host header is the primary gate — a rebinding
        // page's Host is the attacker's hostname (resolving to 127.0.0.1), NOT a loopback
        // literal, so it is rejected here even when the page strips its Origin. Any
        // PRESENT Origin must also be local. Browsers always send Host; non-browser MCP
        // clients connect to 127.0.0.1/localhost and so pass both checks.
        if !host_is_local(&request) || !origin_is_local(&request) {
            let _ = request.respond(text_response(403, "forbidden origin"));
            return;
        }
        let path = url.split('?').next().unwrap_or("/");
        match (method, path) {
            (tiny_http::Method::Get, "/health") => {
                let body = json!({
                    "status": "ok",
                    "server": SERVER_NAME,
                    "version": SERVER_VERSION,
                    "port": self.bound_port(),
                });
                let _ = request.respond(json_response(200, &body));
            }
            (tiny_http::Method::Post, "/mcp") => {
                // Bind this request to its ORIGIN TERMINAL (the auto-map seam): read the
                // `X-Nyx-Terminal-Id` header the plugin `.mcp.json` carries (case-insensitive;
                // an empty/absent value is `None`). Extracted BEFORE the body is drained.
                let caller_terminal_id = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv(TERMINAL_ID_HEADER))
                    .map(|h| h.value.as_str().trim().to_string())
                    .filter(|v| !v.is_empty());
                let mut buf = String::new();
                if request.as_reader().read_to_string(&mut buf).is_err() {
                    let _ = request.respond(json_response(
                        400,
                        &rpc_error_envelope(Value::Null, -32700, "could not read request body"),
                    ));
                    return;
                }
                let response = self.handle_rpc(&buf, caller_terminal_id.as_deref());
                let _ = request.respond(json_response(200, &response));
            }
            _ => {
                let _ = request.respond(text_response(404, "not found"));
            }
        }
    }

    /// Handle one JSON-RPC 2.0 request body and produce the response value. Supports
    /// the MCP handshake (`initialize`, `tools/list`) and dispatches `tools/call` to
    /// the phase-2 dispatcher when installed.
    fn handle_rpc(&self, body: &str, caller_terminal_id: Option<&str>) -> Value {
        let parsed: Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(_) => return rpc_error_envelope(Value::Null, -32700, "parse error"),
        };
        let id = parsed.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = parsed.get("method").and_then(Value::as_str) else {
            return rpc_error_envelope(id, -32600, "invalid request: missing method");
        };
        let params = parsed.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => rpc_result_envelope(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                    "instructions": SERVER_INSTRUCTIONS,
                }),
            ),
            // Notifications carry no id; acknowledge with an empty success envelope.
            "notifications/initialized" | "initialized" => rpc_result_envelope(id, json!({})),
            "ping" => rpc_result_envelope(id, json!({})),
            "tools/list" => rpc_result_envelope(id, json!({ "tools": tool_descriptors() })),
            "tools/call" => self.handle_tools_call(id, &params, caller_terminal_id),
            other => rpc_error_envelope(id, -32601, &format!("method not found: {other}")),
        }
    }

    /// Dispatch a `tools/call`. Phase-1 has no dispatcher installed, so this returns
    /// a `method_not_found` error rather than inventing a second command lifecycle
    /// (ADR-0003 D6). Phase-2 installs the PRD-3-backed dispatcher.
    fn handle_tools_call(
        &self,
        id: Value,
        params: &Value,
        caller_terminal_id: Option<&str>,
    ) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() {
            return rpc_error_envelope(id, -32602, "invalid params: missing tool name");
        }
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let dispatcher = self.dispatcher.lock().unwrap().clone();
        match dispatcher {
            None => rpc_error_envelope(
                id,
                -32601,
                &format!("tool '{name}' not yet available (phase 2)"),
            ),
            Some(d) => match d.call_with_caller(name, &arguments, caller_terminal_id) {
                Ok(result) => rpc_result_envelope(id, tool_result_envelope(result)),
                Err(e) => {
                    let mut env = rpc_error_envelope(id, e.rpc_code(), &e.message);
                    // Carry the ADR-0003 D8 string code in `error.data.code`, plus any
                    // structured detail the error attached (e.g. output_too_large's
                    // `requested`/`limit`), merged in alongside `code` (PRD-4.1 task #3).
                    // We seed `data` from the attached object (when present) and stamp
                    // `code` LAST, so the stable string code can never be clobbered by a
                    // `data` key that happens to be named "code".
                    if let Some(err) = env.get_mut("error") {
                        let mut data = match e.data.as_ref().and_then(Value::as_object) {
                            Some(obj) => Value::Object(obj.clone()),
                            None => json!({}),
                        };
                        data["code"] = json!(e.code.as_str());
                        err["data"] = data;
                    }
                    env
                }
            },
        }
    }
}

/// Whether the request's `Host` header is a loopback authority (ADR-0003 D9). This is
/// the PRIMARY anti-DNS-rebinding gate: a rebinding page's Host is the attacker's
/// hostname (which resolves to 127.0.0.1) — not a loopback literal — so it is rejected
/// here even when the page omits its Origin. An HTTP/1.1 request without a Host is
/// malformed and is treated as non-local (strict). Browsers always send Host; legitimate
/// MCP clients connect to `127.0.0.1`/`localhost` and so send a loopback Host too.
fn host_is_local(request: &tiny_http::Request) -> bool {
    match request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string())
    {
        None => false,
        Some(h) => authority_is_loopback(&h),
    }
}

/// Whether the request's `Origin` is local OR absent (ADR-0003 D9). A present Origin must
/// point at a loopback host; absence is allowed here (non-browser clients, incl. MCP
/// CLIs, send none) — the [`host_is_local`] gate is what actually blocks rebinding.
fn origin_is_local(request: &tiny_http::Request) -> bool {
    let origin = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Origin"))
        .map(|h| h.value.as_str().to_string());
    match origin {
        None => true,
        Some(o) => {
            // Strip scheme + path, keep the `host[:port]` authority.
            let authority = o
                .split("://")
                .nth(1)
                .unwrap_or(&o)
                .split('/')
                .next()
                .unwrap_or("");
            authority_is_loopback(authority)
        }
    }
}

/// Case-insensitive loopback check on a `host[:port]` authority. Handles bracketed IPv6
/// (`[::1]:port`) and any `127.0.0.0/8` literal (not just `127.0.0.1`), plus `localhost`
/// and `::1`. Per RFC, the Origin/Host host is case-insensitive, so `LOCALHOST` matches.
fn authority_is_loopback(authority: &str) -> bool {
    let authority = authority.trim();
    // Separate host from port. Bracketed IPv6 `[::1]:port` → host `::1`; otherwise the
    // host is everything before the first `:` (a bare IPv6 like `::1` has no port form
    // in an authority and is handled by the literal match below).
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else if authority.matches(':').count() == 1 {
        authority.split(':').next().unwrap_or("")
    } else {
        authority
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// The clause that marks a `spec_id` description as belonging to the CONDITIONAL-ANCHOR family —
/// the one whose short forms depend on something the call cannot guarantee. ONE string, written by
/// the descriptor builder in [`tool_descriptors`] and read by the guard test, so "this field states
/// its condition" is a fact the two halves share instead of a phrase a reader has to recognise by
/// eye. It stops at `ONLY when` so the condition itself can be about an ARGUMENT ("you also pass
/// workspace_id") or about what a required argument RESOLVES TO ("the run_id you pass names a run
/// anchored on a workspace") — the two shapes the family actually has.
const CONDITIONAL_ANCHOR_CLAUSE: &str = "ONLY when";

/// THE SIZE BUDGET OF `tools/list`, IN BYTES OF SERIALIZED JSON.
///
/// Every agent that connects to nyx reads this catalogue before it has done anything. Measured on
/// the 2026-09-01 reference journey it was 179 230 bytes for 111 tools — of the order of 45 000
/// tokens, paid up front, by everyone. It had grown there unremarked because nothing anywhere
/// stated what it was allowed to cost. This constant states it.
///
/// It is a CEILING, not a target, and the way to keep it is to write shorter, not to raise it.
/// Raising it is a decision about what every agent pays on connection, so it belongs in a commit
/// message with a reason — never in a "make the test pass" edit.
///
/// RAISED ONCE, 189 000 → 195 000, when two branches merged: the agent-piloting surface
/// (`wait_for_terminal`, `spec_get`, `spec_list`, `task_list`) and the classification surface
/// (`spec_set_category`) were authored in parallel, each trimmed to fit under the old ceiling on
/// its own, and together they measure 192 208 bytes for 116 tools. The alternative was to cut
/// prose that had already been cut once on both sides. The 9 exempted descriptors named in
/// [`OVERSIZE_DESCRIPTORS`] still hold ~70 KB between them — that, not this ceiling, is where the
/// next reduction is.
pub(crate) const TOOLS_LIST_BUDGET_BYTES: usize = 195_000;

/// The per-descriptor ceiling, in bytes of serialized JSON — the gate the whole-catalogue budget
/// cannot provide on its own. A total says the catalogue is too big; it never says WHICH tool made
/// it so, and 115 descriptors each growing 200 bytes look exactly like the total drifting on its
/// own. This one names the offender the moment it is added.
const TOOL_DESCRIPTOR_BUDGET_BYTES: usize = 4_000;

/// The descriptors admitted ABOVE [`TOOL_DESCRIPTOR_BUDGET_BYTES`], each by name.
///
/// A list, and not a raised ceiling, because the cost of an exception should be paid in visibility:
/// adding a name here is a deliberate act with a reviewer on the other side. The design/prototype
/// subsystem holds five of these and the orchestration engine two — both are out of scope for the
/// pass that set this budget, so they were measured and admitted rather than trimmed.
///
/// The guard test also refuses a STALE entry: a tool that came back under the ceiling must LEAVE
/// this list, so it can never quietly cover a future one.
const OVERSIZE_DESCRIPTORS: &[&str] = &[
    "agent_context",
    "design_inventory",
    "design_preview",
    "design_recipe",
    "design_refresh_component",
    "manage_command_group",
    "nyx_next",
    "proto_author_component",
    "run_start",
];

/// The `tools/list` descriptors for the frozen v1 surface (ADR-0003). Phase-2 fills
/// the per-tool argument schemas from the ADR's tool surface so a client can form
/// valid calls; the call bodies live in [`crate::mcp_tools`]. Every descriptor's
/// `name` stays in [`V1_TOOLS`] order; the schema mirrors each tool's `args` table.
///
/// `pub(crate)` so a tool's OWN module can bind its descriptor to its behaviour: the `nyx_next`
/// action inventory is checked against the payloads the engine really builds, in the engine's own
/// tests ([`crate::mcp_engine`]), where a new leaf is added.
pub(crate) fn tool_descriptors() -> Vec<Value> {
    // Reusable JSON-Schema fragments for the common id/string arguments.
    let str_prop = |desc: &str| json!({ "type": "string", "description": desc });
    // A `spec_id` FIELD BELONGS TO ONE OF THREE FAMILIES, and each has its own sentence. The
    // family is a property of the CALL — of which of the four anchors its schema exposes, and of
    // whether one of them is REQUIRED — never of the tool's name. The three builders below are the
    // only way a `spec_id` description is written, and the guard test
    // `every_spec_id_argument_advertises_the_forms_it_accepts` reads the same three families off
    // the schema, so a field cannot enter with the sentence of another family.
    //
    // 1. A CERTAIN ANCHOR — one that is REQUIRED and carries a project by construction, so the call
    // ALWAYS names a project and `#N` / `N` always count. The grammar
    // is announced from the SAME constant the parser and the refusals read
    // ([`crate::spec_ref::ACCEPTED_FORMS`]). An agent learns what to send from these descriptions —
    // nyx used to DISPLAY `#3` everywhere and accept only the canonical id, and the description said
    // nothing either way. Building the sentence from the constant is what keeps the advertised forms
    // and the accepted forms one fact.
    let spec_ref_desc = |desc: &str| {
        format!(
            "{desc}. Accepts {} — a '#N' or bare 'N' resolves in the project this call's own \
             anchors already determine (project_id / workspace_id / cwd / run_id); the canonical \
             id and the key carry their own project",
            crate::spec_ref::ACCEPTED_FORMS
        )
    };
    let spec_ref_prop = |desc: &str| str_prop(&spec_ref_desc(desc));
    // 2. AN ANCHOR THAT MAY BE ABSENT — the SAME call honours `#N` when a project is there and
    // cannot when it is not. A form that works or not depending on something that has nothing to do
    // with it is HARDER to diagnose than one that never works, so the CONDITION is part of the
    // advertisement and the refusal names itself (findings 01M0ZGHD137MDXFGS2PBG6GS68 for
    // `spec_update_section`, 01M0ZKWXS353TABS0WCT3XXB81 for `spec_set_workspace`, `agent_context`
    // and `run_start`, 01M0ZWAH9EQDAZR66AFS1YJFPY for `run_bind_spec`). TWO SHAPES OF ABSENCE sit
    // here: an anchor that may be OMITTED — `spec_set_workspace` is the sharpest, its only anchor is
    // optional BECAUSE omitting it IS the detach — and an anchor that is REQUIRED yet resolves
    // through a chain that can end on nothing, which is `run_bind_spec`'s `run_id`: a run's project
    // is its workspace's, and a run can carry no workspace.
    let conditional_spec_ref_prop = |desc: &str, condition: &str| {
        str_prop(&format!(
            "{desc}. Accepts {} — but a '#N' or bare 'N' {CONDITIONAL_ANCHOR_CLAUSE} {condition}; \
             otherwise there is no project to count a number in and the reference is REFUSED by \
             name, so send {} instead",
            crate::spec_ref::ACCEPTED_FORMS,
            crate::spec_ref::ANCHORLESS_FORMS
        ))
    };
    // 3. NO ANCHOR AT ALL — and the fields that cannot count a number SAY SO, which is the same rule
    // read backwards. A whole family of advertised `spec_id` FIELDS — fewer tools than fields,
    // since `spec_link` and `spec_unlink` carry two each — takes NONE of the four anchors, so
    // `#N` / `N` have no project to be counted in there (how many, and which, is asserted by
    // `tests::every_spec_id_argument_advertises_the_forms_it_accepts`, never counted in prose here)
    // — and a field that announced them anyway promised a resolution that could
    // not happen, which surfaced as the very `unknown spec 3` this grammar exists to retire
    // (finding 01M0ZGCCSQ2F25HPBS6NX5JAFS; arbitrage out-of-scope 01M0D95TFHNMGZ9ES20Z68R395).
    // They announce the SELF-ANCHORED subset instead ([`crate::spec_ref::ANCHORLESS_FORMS`]), say
    // WHY, and the two other forms are refused BY NAME at the entry point
    // ([`crate::mcp_context::require_spec_arg_project_relative_refused`]) rather than dying opaque
    // three layers down. Same constant on both sides, so the advertised and the accepted stay one
    // fact here too.
    let anchorless_spec_ref_prop = |desc: &str| {
        str_prop(&format!(
            "{desc}. Accepts {} — both carry their own project. A '#N' or bare 'N' is REFUSED \
             here, by name: this call takes no project_id / workspace_id / cwd / run_id, so there \
             is no project to count a number in",
            crate::spec_ref::ANCHORLESS_FORMS
        ))
    };
    // The SCOPED CSS SLOT, described IDENTICALLY on the two write paths that accept it
    // (`design_refresh_component` and `proto_author_component`) — one contract, one wording, so an
    // agent can never learn two different rules for one mechanism (finding
    // 01KYVMTWB71R0D0CT1SV88J8DX).
    let css_slot_prop = || {
        str_prop(
            "optional SCOPED CSS SLOT — the escape hatch, and the LAST resort. TAILWIND IS THE \
             CHANNEL of this project: the look is made of utility classes on the template and of the \
             axes' `classes`, first and by default. This slot is admitted ONLY for the four things no \
             utility can express — a custom @keyframes, the CONTENT of a ::before/::after, a \
             relational selector (:has), a @container query. A slot using NONE of them is REFUSED, \
             because it is then CSS written INSTEAD of the design system. FOUR RULES, every one a \
             REFUSAL and never a warning: (1) TOKENISED — a hard-coded colour (#hex, rgb(), oklch(), \
             even `red`) is refused, every var(--x) must name a token the project really declares \
             (design_tokens lists them), and a raw px on a property the system SCALES (padding, gap, \
             border-radius, font-size…) is refused too — use var(--token) or a relative unit; (2) \
             SCOPED — nyx REWRITES every selector under [data-nyx-css~=\"<scope>\"], the attribute it \
             stamps on the component's root element(s), and RENAMES every @keyframes under that \
             scope, so nothing you write can reach outside the component; write `:scope` (or `&`) to \
             address the component's own root; a selector on html/body/:root, or one starting with a \
             combinator (`+ .x`), is refused BY NAME rather than silently neutered; (3) BOUNDED — \
             2048 bytes; (4) SIGNALLED — css_slot_reason is MANDATORY, and the exception is shown in \
             the human review AND carried to the coder with that reason. Also refused: !important, \
             url() (the preview frame grants no network, and no CSS slot will ever relax its CSP), \
             @import / @font-face / @layer / @property / @apply, and markup. OMIT this argument to \
             leave a stored slot ALONE; send \"\" to remove it.",
        )
    };
    let css_slot_reason_prop = || {
        str_prop(
            "MANDATORY whenever css_slot is sent, at most 240 bytes: WHAT THE UTILITIES COULD NOT DO \
             (\"a custom pulse keyframe — Tailwind has no utility for a keyframe\"). It is what makes \
             the exception reviewable by a human, and it travels with the CSS all the way to the \
             coder. A css_slot without it is refused.",
        )
    };
    let int_prop = |desc: &str| json!({ "type": "integer", "minimum": 0, "description": desc });
    let bool_prop = |desc: &str| json!({ "type": "boolean", "description": desc });
    // The per-run `env` map (R-WSCMD #7): an object of KEY→VALUE strings merged onto the
    // inherited environment for the launched process. Values are strings (env values).
    let env_prop = || {
        json!({
            "type": "object",
            "additionalProperties": { "type": "string" },
            "description": "optional per-run environment variables (KEY→VALUE, every value a \
                            string) merged onto the inherited environment for this run — e.g. \
                            {\"VAULT_ENV\":\"dev\"}. Applied only to the process this call spawns, \
                            and ignored on a no-op start (already running). ON THE SYSTEM SCOPE it \
                            makes the call WAIT for the user's approval, otherwise free: an \
                            environment changes what a fixed command line executes, so the user is \
                            shown the exact map. Omit it to run a system command as defined, with \
                            no request.",
        })
    };
    // The WHOLE-GROUP target (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding 01M11XVEFEQ524XRT9ARPQFGSV):
    // `group_id` + `workspace_id` name a multi-pane command group instead of one instance, on the
    // three lifecycle tools. One wording, parameterised by the verb, so an agent learns the
    // mechanism once and it reads the same on all three.
    let group_target_prop = |done: &str| {
        str_prop(&format!(
            "act on a WHOLE multi-pane command group instead of one instance: the group id from \
             list_commands(project_id=…) `groups`. Requires `workspace_id` (a group holds command \
             TEMPLATES, which are project-scoped; running them is workspace-scoped), and is \
             mutually exclusive with instance_id/name. Every member of the group is {done} in that \
             workspace; the answer is then the group shape described above, not a single instance \
             status.",
        ))
    };
    // The PERSISTED `env` of a command TEMPLATE (v2.8 review N31): the environment stored WITH the
    // command, applied on EVERY path that runs it — including a gate_check nyx runs itself, which
    // the per-run `env` of start_command deliberately never reaches.
    let command_env_prop = || {
        json!({
            "type": "object",
            "additionalProperties": { "type": "string" },
            "description": "optional PERSISTENT environment for this command (KEY→VALUE string map, \
                            e.g. {\"DATABASE_URL\":\"postgres://…\"}), stored with the command and \
                            applied EVERY time it runs — started by hand, started by you, or run by \
                            nyx as a workflow gate. Use it for what the command NEEDS to be green \
                            (the per-run `env` of start_command is a one-off override on top, and \
                            never applies to a gate). Each value must be a string. Sent WHOLE: it \
                            replaces the stored map rather than patching it, and {} (or null) clears \
                            it. Reading back, a command reports only `env_keys` — values are never \
                            echoed.",
        })
    };
    // Agent-facing one-liner per v1 tool: WHEN/WHY to use it, in plain terms. These
    // mirror the order of [`V1_TOOLS`].
    let description = |name: &str| -> &'static str {
        match name {
            "list_projects" => "List the user's projects in nyx. Start here to discover what the user is working on; each project groups one or more workspaces (folders on disk).",
            "list_workspaces" => "List the workspaces (on-disk folders) inside a project. Use after list_projects to find the workspace you want to inspect or run commands in.",
            "list_commands" => "List the development commands for a workspace (or the command templates for a project). Use the workspace form to get each command's `instance_id` — the id you pass to start/stop/relaunch/get_command_output. The project form returns reusable templates that are NOT launchable on their own, PLUS the project's command `groups` (id, name, color, `layout_shape`, and `command_ids` in disposition order); every command carries its `group_id` (null = solo). A `group_id` + a `workspace_id` is also what start/stop/relaunch_command take to drive a WHOLE group at once. This is the read-only way to inspect the grouping manage_command_group edits — no write needed.",
            "start_command" => "Start a managed command (e.g. a dev server, build watcher, or test run) so nyx supervises it and captures its output. Identify it by `instance_id` from list_commands, or by `name` within a `workspace_id`. Starting an already-running command is a no-op (it does NOT launch a second process): the result reports `was_running:true, restarted:false` so you can tell — use relaunch_command to actually restart. Pass an optional `env` map (KEY→VALUE strings, e.g. {\"VAULT_ENV\":\"dev\"}) to add environment variables for this run, merged onto the inherited environment. After starting, read its output with get_command_output to see logs, errors, or a server URL. WHOLE GROUP: pass `group_id` (from list_commands(project_id=…) `groups`) together with `workspace_id` INSTEAD of an instance, and every member of that multi-pane group is started in that workspace — but ONLY the members that are not already running; a member already up is left strictly alone, neither restarted nor interrupted. The result is then `{ group_id, workspace_id, action, members[], running, total }`, one entry per member with its own status plus `was_running`/`acted`/`error`, and `running`/`total` as the group's aggregate. This is the exact same operation the user performs with the group's play button.",
            "stop_command" => "Stop a running managed command. Identify it by its `instance_id` from list_commands. The result reports `changed` (whether a live process was actually stopped) and `was_running`, so a stop on an already-idle command is a clear no-op rather than a false success. WHOLE GROUP: pass `group_id` together with `workspace_id` INSTEAD of `instance_id` to stop EVERY member of a multi-pane command group in that workspace (idempotent on the members already idle); the result is then the group shape `{ group_id, workspace_id, action, members[], running, total }`.",
            "relaunch_command" => "Restart a managed command in place (stop then start the same instance) — useful after a config or code change. This ALWAYS restarts (unlike start_command, which no-ops on a running command); the result reports `restarted:true`. Identify it by its `instance_id` from list_commands. Pass an optional `env` map (KEY→VALUE strings) to set environment variables for the fresh run, merged onto the inherited environment. WHOLE GROUP: pass `group_id` together with `workspace_id` INSTEAD of `instance_id` to restart a whole multi-pane command group in that workspace — the ACTIVE members are re-spawned and the INACTIVE ones are started, so the group ends fully active. All members are decided from one snapshot taken before anything runs (simultaneous, not one-after-another); the result is the group shape `{ group_id, workspace_id, action, members[], running, total }`.",
            "get_command_output" => "Read a managed command's captured output (logs, errors, a dev server's URL, test results). Identify the command by `instance_id`, or by `name` within a `workspace_id`. Returns a token-safe tail by default, with terminal color/control codes already stripped (set strip_ansi:false for raw bytes). Use `grep` to return only matching lines (e.g. errors) or `tail_lines` for the last N lines. Reads are incremental: the result includes a `cursor` you can pass back as `since` to fetch only new output on the next read. A window over the 1 MiB ceiling is refused — see `tail_bytes`.",
            "workspace_add" => "Register an EXISTING on-disk folder as a workspace under a project, so nyx tracks it and can run commands in it. The folder must already exist — a non-existent path or a file (not a directory) is rejected. Use create_workspace instead when nyx should CREATE the folder. Pass spec_id to ANCHOR a ready/in_progress Spec on the workspace in the same operation (the Spec's status is never changed). The result then carries the anchored Spec under `spec` next to `workspace`, so you do not need a second call to read it back; without spec_id the result is `{workspace}` alone.",
            "create_workspace" => "CREATE a new folder on disk (mkdir -p, including any missing parents) and register it as a workspace under a project. Use when the folder does not exist yet and the user wants nyx to start tracking a brand-new working folder. To register a folder that already exists, use workspace_add (which does NOT create anything). Pass spec_id to ANCHOR a ready/in_progress Spec on the workspace in the same operation (validated before the folder is created; the Spec's status is never changed). The result then carries the anchored Spec under `spec` next to `workspace`, so you do not need a second call to read it back; without spec_id the result is `{workspace}` alone.",
            _ => "",
        }
    };
    let mut descriptors: Vec<Value> = V1_TOOLS
        .iter()
        .map(|name| {
            // Build the per-tool `(properties, required)` from the ADR-0003 surface.
            let (properties, required): (Value, Vec<&str>) = match *name {
                "list_projects" => (json!({}), vec![]),
                "list_workspaces" => (
                    json!({
                        "project_id": str_prop("project whose workspaces to list"),
                        "cwd": str_prop("optional cwd FILTER (does not resolve a current workspace)"),
                    }),
                    vec!["project_id"],
                ),
                "list_commands" => (
                    json!({
                        "workspace_id": str_prop("workspace whose command INSTANCES to list (nominal form): each row's instance_id is the LAUNCHABLE id for start/stop/relaunch/get_command_output"),
                        "project_id": str_prop("project whose command TEMPLATES to list (alternative): each row's command_id is a template id and is NOT launchable — use the instance_id from the workspace_id form to act on a command"),
                    }),
                    // Neither is universally required (one OR the other); the tool
                    // validates the at-least-one rule, so leave `required` empty here.
                    vec![],
                ),
                // start_command also accepts { name, workspace_id } to resolve the
                // instance by name (#16). All three ALSO accept { group_id, workspace_id } to act
                // on a WHOLE multi-pane command group (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding
                // 01M11XVEFEQ524XRT9ARPQFGSV): rather than a fourth tool, the group is a second way
                // of NAMING the target — the same mechanism `name` + `workspace_id` already is —
                // so the runner, the dispatch and the status shape are reused untouched. Their
                // `required` is therefore empty: the tool itself validates that exactly one target
                // form was given.
                "start_command" => (
                    json!({
                        "instance_id": str_prop("LAUNCHABLE command instance id (from list_commands(workspace_id=…)), NOT a template command_id"),
                        "name": str_prop("command name to resolve within workspace_id (alternative to instance_id; needs workspace_id; ambiguous/unknown → error)"),
                        "group_id": group_target_prop("started (only the members that are not already running)"),
                        "workspace_id": str_prop("workspace to resolve `name` in (required when using `name`), or the workspace to act in (required when using `group_id`)"),
                        "env": env_prop(),
                    }),
                    // instance_id OR (name + workspace_id) OR (group_id + workspace_id); the tool
                    // validates.
                    vec![],
                ),
                "stop_command" => (
                    json!({
                        "instance_id": str_prop("LAUNCHABLE command instance id (from list_commands(workspace_id=…)), NOT a template command_id"),
                        "group_id": group_target_prop("stopped"),
                        "workspace_id": str_prop("the workspace to act in (required when using `group_id`)"),
                    }),
                    // instance_id OR (group_id + workspace_id); the tool validates.
                    vec![],
                ),
                "relaunch_command" => (
                    json!({
                        "instance_id": str_prop("LAUNCHABLE command instance id (from list_commands(workspace_id=…)), NOT a template command_id"),
                        "group_id": group_target_prop("restarted — active members re-spawned, inactive ones started"),
                        "workspace_id": str_prop("the workspace to act in (required when using `group_id`)"),
                        "env": env_prop(),
                    }),
                    // instance_id OR (group_id + workspace_id); the tool validates.
                    vec![],
                ),
                "get_command_output" => (
                    json!({
                        "instance_id": str_prop("LAUNCHABLE command instance id whose output to read (NOT a template command_id)"),
                        "name": str_prop("command name to resolve within workspace_id (alternative to instance_id; needs workspace_id)"),
                        "workspace_id": str_prop("workspace to resolve `name` in (required when using `name`)"),
                        "tail_bytes": int_prop("how many bytes to return from the tail of the output \
                                               (default 12288, a token-safe window). Use this for the \
                                               typical case; the effective window is \
                                               min(tail_bytes, max_bytes). Either above 1048576 fails \
                                               with error.data.code=\"output_too_large\" plus \
                                               error.data.requested/limit — detect that code and retry \
                                               smaller rather than guessing."),
                        "since": int_prop("byte offset from a previous `cursor` for incremental polling. If the buffer was cleared since (your `since` is past the new end) the response carries `reset:true` and returns the fresh output from the start instead of an empty window"),
                        "max_bytes": int_prop("alternative hard ceiling on the returned window \
                                              (default 1048576) — a safety guard; prefer tail_bytes for \
                                              normal use. Same 1048576 limit and same refusal."),
                        "strip_ansi": json!({ "type": "boolean", "description": "when true (the DEFAULT), `output` is the window with ANSI/terminal control sequences stripped — a single readable field. Set false to get the raw bytes in `output` instead. `cursor`/`total_bytes` are byte-exact either way." }),
                        "mark_read": json!({ "type": "boolean", "description": "when true, this read ALSO acknowledges the command's unseen result — flipping `unread` to false EXACTLY as a UI acknowledge does, while the factual outcome (state/exit_code) is left intact. Default false: a passive (polling) read NEVER consumes the `unread` notification. Only meaningful for the current run (a `previous`-selector read targets a settled prior run with no live `unread`)." }),
                        "grep": str_prop("optional regular expression; when set, `output` contains only the lines matching it (matched on the ANSI-stripped text). Use it to pull just the error lines instead of the whole tail."),
                        "tail_lines": int_prop("optional: keep only the last N lines of the window (applied after `grep`). A line-based alternative to tail_bytes for \"show me the last 20 lines\"."),
                        "run": json!({ "description": "which run to read: 0/\"current\" (default, latest) or -1/\"previous\" (the retained prior run, with its own exit_code/state). History is bounded to N=1; anything else is refused by RANGE, never by type. The result echoes `run`.", "oneOf": [ { "type": "integer", "enum": [0, -1] }, { "type": "string", "enum": ["current", "latest", "previous", "prev"] } ] }),
                    }),
                    // instance_id OR (name + workspace_id); the tool validates.
                    vec![],
                ),
                "workspace_add" => (
                    json!({
                        "project_id": str_prop("project to register the workspace in"),
                        "path": str_prop("path to an EXISTING on-disk folder (must already exist and be a directory; not created)"),
                        "name": str_prop("optional display name (defaults to the path's basename)"),
                        "spec_id": spec_ref_prop("optional Spec to ANCHOR on this workspace in the same operation, so later calls resolve the Spec from the workspace instead of needing its id. The Spec must be ready or in_progress and belong to the same project; anchoring never changes its status. An EMPTY string is refused rather than read as 'no Spec' — omit the argument to register the workspace unlinked"),
                    }),
                    vec!["project_id", "path"],
                ),
                "create_workspace" => (
                    json!({
                        "project_id": str_prop("project to create the workspace in"),
                        "name": str_prop("workspace display name"),
                        "path": str_prop("folder path to CREATE (mkdir -p, including missing parents) then register; use workspace_add if the folder already exists"),
                        "spec_id": spec_ref_prop("optional Spec to ANCHOR on this workspace in the same operation, so later calls resolve the Spec from the workspace instead of needing its id. The Spec must be ready or in_progress and belong to the same project; anchoring never changes its status. It is validated BEFORE the folder is created — an EMPTY string is refused there too, so a mistyped anchor leaves no folder behind; omit the argument to create the workspace unlinked"),
                    }),
                    vec!["project_id", "name", "path"],
                ),
                // Defensive default: should be unreachable while V1_TOOLS is the
                // source of truth, but keep a valid object schema rather than panic.
                _ => (json!({}), vec![]),
            };
            json!({
                "name": name,
                "description": description(name),
                "inputSchema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                },
            })
        })
        .collect();
    // Append the `probe` tool: a no-op health/connectivity check. Listed AFTER the
    // frozen v1 surface and kept out of [`V1_TOOLS`], so it never mutates the v1
    // contract clients are onboarded against.
    descriptors.push(json!({
        "name": PROBE_TOOL,
        "description": "Health/connectivity check: verifies nyx's MCP server is reachable. \
                        Returns { ok: true } with the server name and version, and has no \
                        side effects. Use it to confirm nyx is up before relying on the \
                        other tools.",
        "inputSchema": { "type": "object", "properties": {}, "required": [] },
    }));
    // Append the `wait_for_command` long-poll tool: a BOUNDED await of a command's
    // state change so an agent does not blind-poll. Listed AFTER the v1 surface and
    // kept out of [`V1_TOOLS`] for the SAME reason as `probe`: it is an observational
    // convenience, not a frozen-contract tool.
    descriptors.push(json!({
        "name": WAIT_FOR_COMMAND_TOOL,
        "description": "Wait for a managed command to reach a given state, instead of \
                        repeatedly polling. Blocks (up to timeout_ms, max ~60s) until the \
                        command instance's state enters `until`, then returns resolved:true; \
                        if the timeout elapses first it returns resolved:false (a normal \
                        outcome — call again to keep waiting). Read-only: it does not start, \
                        stop, or change the command. By default `output_tail` carries only \
                        the output produced AFTER this call (not the whole existing \
                        scrollback), with terminal control codes stripped. The returned \
                        `cursor` chains directly into get_command_output(since=cursor) so \
                        you can fetch the new output with no gap or duplication. A \
                        tail_bytes/max_bytes over the 1 MiB ceiling is refused — see `tail_bytes`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "instance_id": str_prop("LAUNCHABLE command instance id to wait on (NOT a template command_id)"),
                "until": json!({
                    "type": "array",
                    "items": { "type": "string", "enum": ["idle", "running", "success", "error", "exited"] },
                    "description": "states that resolve the wait (default: settled states success+error). Runner vocabulary idle|running|success|error; \"exited\" is an alias for success|error.",
                }),
                "timeout_ms": int_prop("max wait in ms before resolved:false (default 30000, clamped to ~60000 max)"),
                "since": int_prop("byte offset from a previous get_command_output/wait_for_command `cursor`, so output_tail/cursor resume incrementally. OMIT on the first call: it defaults to the current end-of-buffer, so output_tail returns only output produced after this call (never the whole existing scrollback)."),
                "tail_bytes": int_prop("how many bytes of new output to return in output_tail (default 12288, a token-safe window). Above 1048576 the call fails with error.data.code=\"output_too_large\" plus error.data.requested/limit — detect that code and retry smaller rather than guessing"),
                "max_bytes": int_prop("alternative hard ceiling on output_tail (default 1048576; the smaller of tail_bytes/max_bytes wins). Same limit, same refusal"),
                "strip_ansi": json!({ "type": "boolean", "description": "when true (the DEFAULT), output_tail has ANSI/terminal control sequences stripped; set false for raw bytes. cursor is byte-exact either way." }),
            },
            "required": ["instance_id"],
        },
    }));
    // Append the command-CRUD extension (PRD-4 dogfood, review
    // `01KV9614CHC4092P05DV9R5KPG`, ADR-0003 D13): the MUTATING command tools an agent
    // needs to create/modify/import commands, which the read/lifecycle v1 surface lacks.
    // Listed AFTER the v1 surface and kept OUT of [`V1_TOOLS`] for the SAME reason as
    // `probe`/`wait_for_command`: they delegate to the existing PRD-3 layer (no parallel
    // logic) and can evolve without touching the frozen contract clients onboard against.
    descriptors.push(json!({
        "name": ADD_COMMAND_TOOL,
        "description": "Create a per-project command TEMPLATE (name + command line, \
                        optional run subfolder, optional persistent environment), via the \
                        SAME path as the UI's command_create — one instance is materialized \
                        per existing workspace of the project. A command line that is itself \
                        a package manager invocation (`pnpm dev`, …) has its provenance \
                        inferred. Returns the created template (its `env_keys` list the \
                        stored environment's KEYS; the values are never echoed). A name \
                        already used in the project → invalid_state. \
                        THIS IS ALSO THE DOOR TO THE SYSTEM SCOPE: pass the `scope.project_id` \
                        returned by list_system_commands as project_id and you are creating a \
                        machine-wide SYSTEM command (it runs from the user's home folder, not \
                        from a project). That call is not refused, it WAITS: nyx asks the user \
                        to approve it, and on approval nyx creates the command itself and \
                        returns it to you.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project to create the command template in"),
                "name": str_prop("command display name (must be unique within the project)"),
                "command": str_prop("the command line to run"),
                "subfolder": str_prop("optional run path relative to EACH workspace (default: workspace root). A run-path, NOT a scope: the template is materialized into every workspace and this subfolder is appended even if it does not exist there"),
                "env": command_env_prop(),
            },
            "required": ["project_id", "name", "command"],
        },
    }));
    descriptors.push(json!({
        "name": UPDATE_COMMAND_TOOL,
        "description": "Modify an existing command TEMPLATE's editable fields (name, \
                        command, subfolder, env), via the SAME path as the UI's \
                        command_update. Only the fields supplied are changed; omitted fields \
                        keep their current value (pass subfolder:\"\" to clear it, env:{} or \
                        env:null to clear the stored environment). Editing the command \
                        of a package.json-linked template away from its canonical call \
                        DETACHES the source (same rule as the UI). Refused while any of the \
                        template's instances is running (invalid_state).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command_id": str_prop("TEMPLATE id to modify (the command_id from list_commands(project_id=…))"),
                "name": str_prop("new display name (optional; unchanged if omitted)"),
                "command": str_prop("new command line (optional; unchanged if omitted)"),
                "subfolder": str_prop("new run subfolder (optional; unchanged if omitted; empty string clears it to the workspace root)"),
                "env": command_env_prop(),
            },
            "required": ["command_id"],
        },
    }));
    descriptors.push(json!({
        "name": IMPORT_COMMANDS_TOOL,
        "description": "Import a project's package.json scripts as command templates, \
                        reusing the SAME discovery + import logic as the UI \
                        (command_import_scripts/command_import_create). Scans the project's \
                        workspace(s) with a FILTERED, monorepo-aware walk (skips \
                        node_modules, hidden dotdirs, and .gitignored paths; uses the root \
                        workspaces manifest globs when present, else a bounded depth) and \
                        creates a template per script (npm/pnpm/yarn/bun runner command, \
                        source provenance linked). Scripts whose proposed name is already \
                        used in the project are skipped (reason:\"already_exists\"). Returns \
                        { imported, skipped, manifests_found, preview }; manifests_found:0 \
                        (plus a skipped entry reason:\"no_manifest\") means no package.json \
                        was found — distinct from \"all already imported\". Provide \
                        project_id (scans every workspace) or workspace_id (scans that one). \
                        Use the optional `names` array to import only specific scripts \
                        (matched by the raw script name OR its proposed name, e.g. \
                        [\"dev\", \"build\"]); a requested name that matches nothing is \
                        reported in skipped with reason:\"not_found\". Pass preview:true to \
                        LIST the discoverable scripts (name, package, script_name, body, \
                        command) WITHOUT creating any template (see also \
                        list_importable_scripts).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project whose workspaces to scan for package.json scripts"),
                "workspace_id": str_prop("a single workspace to scan (alternative to project_id)"),
                "names": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "optional list of script names to import (e.g. [\"dev\", \"build\"]). \
                                   A name matches a script by its raw script_name OR its proposed name \
                                   (so [\"build\"] matches a build script in every package). Scripts not \
                                   requested are silently bypassed; a requested name that matches no \
                                   discovered script is returned in skipped with reason:\"not_found\". \
                                   Absent or null = import all discovered scripts.",
                }),
                "preview": json!({
                    "type": "boolean",
                    "description": "when true, list the discoverable scripts (name, package, \
                                   script_name, body, command) WITHOUT creating any template — a \
                                   dry-run. Default false (create the templates).",
                }),
            },
            // project_id OR workspace_id; the tool validates the at-least-one rule.
            "required": [],
        },
    }));
    // Append remove_workspace + remove_command (A2): the D of CRUD — advertised
    // alongside the v1 surface, kept OUT of V1_TOOLS for the same reason as the other
    // CRUD tools.
    descriptors.push(json!({
        "name": REMOVE_WORKSPACE_TOOL,
        "description": "Remove a workspace from nyx. This DETACHES it: its record, command \
                        instances, terminals and agent sessions go permanently, and any running \
                        command is stopped. THE FOLDER ON DISK IS NOT DELETED — the reply names \
                        the path it kept under `folder_kept`. Cannot be undone.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("id of the workspace to remove (from list_workspaces)"),
            },
            "required": ["workspace_id"],
        },
    }));
    descriptors.push(json!({
        "name": REMOVE_COMMAND_TOOL,
        "description": "Remove a command template from nyx. Deletes the template and all its \
                        workspace instances permanently. Refused if any instance is currently \
                        running — stop the command first. Pass the `command_id` from \
                        list_commands(project_id=…), NOT an instance_id. This action cannot \
                        be undone.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command_id": str_prop("TEMPLATE id to remove (from list_commands(project_id=…)), NOT an instance_id"),
            },
            "required": ["command_id"],
        },
    }));
    // Append set_project_settings: modify an EXISTING project's mutable settings (rename +
    // resume-agent-sessions opt-in), the MCP mirror of the UI's project-settings modal.
    // Advertised alongside the v1 surface, kept OUT of V1_TOOLS like the other extension
    // tools.
    descriptors.push(json!({
        "name": SET_PROJECT_SETTINGS_TOOL,
        "description": "Change an EXISTING project's settings, via the SAME path as the \
                        UI's project-settings modal. You can rename it (`name`), give it a \
                        visual identity (`accent` plus `icon` OR `image`) and/or \
                        turn its agent-session resume on/off (`resume_agent_sessions`): \
                        when on, nyx re-attaches that project's terminals' active agent \
                        sessions at relaunch (exact --resume) instead of opening a bare \
                        shell; when off (the default) it leaves a bare shell. Only the \
                        fields you supply are changed; omitted fields keep their current \
                        value. Returns the project's updated settings. Does NOT create a \
                        project (that is a UI gesture). Unknown project id → invalid_id; \
                        an empty/blank name → invalid_argument.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project to modify (the project id from list_projects)"),
                "name": str_prop("new display name (optional; unchanged if omitted; must be non-blank)"),
                "resume_agent_sessions": json!({
                    "type": "boolean",
                    "description": "optional; when true, resume this project's active agent \
                                   sessions at relaunch instead of a bare shell. Unchanged if omitted.",
                }),
                "worktrees_root": json!({
                    "type": "string",
                    "description": "optional; the ROOT FOLDER this project's dev workspaces are \
                                   created under. Set it once and a flow that opens a workspace \
                                   (e.g. /nspec answered 'go' at its commit) derives the folder \
                                   from it instead of asking you where. Pass an empty string (or \
                                   null) to CLEAR it, which puts the flow back on asking. Unchanged \
                                   if omitted.",
                }),
                "accent": json!({
                    "type": "string",
                    "description": "optional; the project's ACCENT COLOUR — how a human recognises \
                                   this project at a glance in a list that mixes several. Pass \
                                   null (or an empty string) to CLEAR it, which puts the project \
                                   back on the neutral tile. Unchanged if omitted.",
                }),
                "icon": json!({
                    "type": "string",
                    "description": "optional; the project's MARK as an icon name. Mutually \
                                   exclusive with `image` — supplying one CLEARS the other, and \
                                   supplying both is an error. Pass null to clear the mark, which \
                                   falls back to the project's initial. Unchanged if omitted.",
                }),
                "image": json!({
                    "type": "string",
                    "description": "optional; the project's MARK as an image (a file path or URL). \
                                   Mutually exclusive with `icon` — see it. Pass null to clear the \
                                   mark. Unchanged if omitted.",
                }),
            },
            "required": ["project_id"],
        },
    }));
    // Append project_set_repos (finding #27): configure a project's repositories EXPLICITLY
    // (the association that replaces disk auto-detection). Advertised alongside the v1
    // surface, kept OUT of V1_TOOLS like the other extension tools.
    descriptors.push(json!({
        "name": PROJECT_SET_REPOS_TOOL,
        "description": "Configure the repositories of a project EXPLICITLY (nyx does NOT \
                        auto-detect repos from disk). Pass `project_id` and `repos`: the \
                        FULL list of the project's repositories, each with a `relative_path` \
                        within the project (use \".\" for a single repo at the workspace \
                        root — a mono-repo) and an optional `name` (defaults to the path's \
                        last segment). This is a full SET/reconcile: repos you list are \
                        created or kept; a repo previously configured but NOT in the list is \
                        removed (archived). Configure ONCE per project — every workspace and \
                        dev worktree of the project then sees these repos at \
                        `workspace_root + relative_path` (so workspace_git / repo_diff / the \
                        review show each sub-repo's real changes). A project with NO \
                        configured repos falls back to the workspace's current folder as a \
                        single repo. Returns the project's configured repositories.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("the project whose repositories to configure (from list_projects)"),
                "repos": json!({
                    "type": "array",
                    "description": "the FULL set of the project's repositories (a full replace). \
                                   Empty array resets to the current-folder fallback.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "relative_path": str_prop("path of the repo within the project, e.g. \"api\" or \".\" for a repo at the workspace root"),
                            "name": str_prop("optional logical name (defaults to the last path segment)"),
                        },
                        "required": ["relative_path"],
                    },
                }),
            },
            "required": ["project_id", "repos"],
        },
    }));
    // Append clear_command_output (review R-OUTPUT): reset a command instance's captured
    // output buffer. Advertised alongside the v1 surface, kept OUT of V1_TOOLS like the
    // other extension tools.
    descriptors.push(json!({
        "name": CLEAR_COMMAND_OUTPUT_TOOL,
        "description": "Clear a command instance's captured output (its scrollback log), \
                        so the next get_command_output starts from an empty buffer. Useful \
                        to reset a long-running instance's accumulated log before watching \
                        for fresh output. Identify the command by its `instance_id` from \
                        list_commands. This clears only the captured TEXT — it does not \
                        stop, relaunch, or change the command, and the last run's result \
                        (success/error, exit code) is preserved.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "instance_id": str_prop("LAUNCHABLE command instance id whose output buffer to clear (from list_commands(workspace_id=…)), NOT a template command_id"),
            },
            "required": ["instance_id"],
        },
    }));
    // Append list_importable_scripts + remove_commands (review R-IMPORT #5): the import
    // preview surface and the grouped-delete mirror of remove_command. Advertised
    // alongside the v1 surface, kept OUT of V1_TOOLS like the other extension tools.
    descriptors.push(json!({
        "name": LIST_IMPORTABLE_SCRIPTS_TOOL,
        "description": "List the package.json scripts that could be imported as command \
                        templates, WITHOUT creating anything (a read-only preview). Uses the \
                        SAME filtered, monorepo-aware discovery as import_commands (skips \
                        node_modules, hidden dotdirs, and .gitignored paths; honors the root \
                        workspaces manifest globs when present). Each entry has the proposed \
                        `name`, its `package` (subfolder, \"\" = root), the raw `script_name`, \
                        the script `body`, the runner `command` an import would create, and \
                        the detected `package_manager`. Also returns `manifests_found` \
                        (0 means no package.json was found). Provide project_id (scans every \
                        workspace) or workspace_id (scans that one). Use this to decide what \
                        to import, then call import_commands with the chosen `names`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project whose workspaces to scan for importable scripts"),
                "workspace_id": str_prop("a single workspace to scan (alternative to project_id)"),
            },
            // project_id OR workspace_id; the tool validates the at-least-one rule.
            "required": [],
        },
    }));
    // Append list_system_commands (PRD « Scope Système »): the agent's dedicated DISCOVERY of the
    // project-less Système services. Listed AFTER the v1 surface and kept OUT of V1_TOOLS like
    // every other extension tool.
    descriptors.push(json!({
        "name": LIST_SYSTEM_COMMANDS_TOOL,
        "description": "Discover the SYSTÈME commands: the user's cross-project services and \
                        launchers (docker, machine daemons, …) that belong to no code project. \
                        They live in a single technical container that is deliberately ABSENT \
                        from list_projects / list_workspaces, so this is the only way to find \
                        them. Takes no argument. Returns `scope` (the container's project_id, \
                        workspace_id and `home` — every Système command runs from the user's home \
                        directory), `commands` (each with its `command_id`, its LAUNCHABLE \
                        `instance_id`, name, command line, resolved `cwd`, `group_id` and current \
                        run status) and `groups` (the multi-pane groups, with their members in \
                        order). Every entry carries `nature`:\"system\" — the same field \
                        list_commands now carries for ordinary commands, so one reader tells the \
                        two apart. WHAT `scope.project_id` IS FOR: it is the id you pass as \
                        `project_id` to ADDRESS this container with the ordinary command tools — \
                        add_command(project_id=scope.project_id, …) creates a new SYSTEM command, \
                        list_commands(project_id=…) reads its templates and groups. \
                        `scope.workspace_id` is its run context, the one start/stop/relaunch take \
                        beside a `name` or a `group_id`. WHAT YOU MAY DO WITH THEM: reading is \
                        free — pass an `instance_id` straight to get_command_output or \
                        wait_for_command; STARTING a stopped command is free too (start_command). \
                        Everything that CHANGES them — creating, importing, editing, removing, \
                        reordering, regrouping, clearing output, stopping a running command or \
                        relaunching one — asks the user first. Such a call is not refused: it \
                        WAITS for their answer, and when they approve, nyx performs the operation \
                        itself and returns its result to you. If they have not answered in time \
                        you get a `request_id` and keep waiting with system_approval_status. \
                        These are the user's own services: nyx will ask them, not you. The \
                        Système TERMINALS are not listed here and the terminal tools cannot see \
                        them; they are the human's alone.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "required": [],
        },
    }));
    // Append system_approval_status (PRD « Scope Système », protocole d'approbation): the ONE tool
    // the approval protocol adds. A READ of one request — never a list, and never a way to decide.
    // Grant/deny stay UI-only db-commands.
    descriptors.push(json!({
        "name": SYSTEM_APPROVAL_STATUS_TOOL,
        "description": "Collect the outcome of a SYSTEM-command operation the user is still \
                        deciding on. When you call a tool that CHANGES the user's system commands \
                        (create, import, edit, remove, reorder, regroup, clear output, stop a \
                        running command, relaunch), nyx asks them in the app and your call WAITS. \
                        If they answer in time you simply get the operation's real result. If they \
                        do not, your call returns \
                        status:\"approval_required\" with a `request_id` — nothing has happened \
                        yet. Call this tool with that request_id, and `wait_ms` to keep waiting. \
                        `state` is \"pending\" (still no answer), \"granted\" (they approved; nyx \
                        ran the operation and `result` is what it returned), \"denied\" (they \
                        refused — do not re-raise it, ask them instead) or \"expired\" (the \
                        5-minute window closed; nothing ran, re-issue the original call if you \
                        still want it). DO NOT retry the original tool while a request is pending: \
                        that asks the user twice for the same thing. You never carry a credential \
                        — approving is the user's gesture and nyx performs the operation itself.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "request_id": str_prop("the request_id nyx gave you when it asked the user to approve your operation"),
                "wait_ms": {
                    "type": "integer",
                    "description": "milliseconds to WAIT for the outcome before answering (0 = answer now). Capped at 45000, just under the usual client request timeout.",
                    "minimum": 0,
                    "maximum": 45000,
                },
            },
            "required": ["request_id"],
        },
    }));
    descriptors.push(json!({
        "name": REMOVE_COMMANDS_TOOL,
        "description": "Remove several command TEMPLATES at once (the grouped mirror of \
                        remove_command), so a mass import can be undone in a single call. \
                        Pass `command_ids`: an array of template ids (the command_id from \
                        list_commands(project_id=…), NOT instance_ids). Each is deleted with \
                        its workspace instances (permanently). A template that is currently \
                        running in any workspace is REFUSED for that id (stop it first) but \
                        does not block the others. Returns `removed` (count actually deleted) \
                        and `results` (a per-id ack with removed:true/false and, on failure, \
                        an error). This action cannot be undone.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command_ids": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "the TEMPLATE ids to remove (each a command_id from \
                                   list_commands(project_id=…), NOT an instance_id).",
                }),
            },
            "required": ["command_ids"],
        },
    }));
    descriptors.push(json!({
        "name": REORDER_COMMANDS_TOOL,
        "description": "Set the ORDER of a project's command list — the order the commands are \
                        listed in everywhere in nyx, and the one the user re-arranges by dragging \
                        a row. Pass `project_id` and `command_ids`: EXACTLY the project's command \
                        template ids (the command_id from list_commands(project_id=…), NOT \
                        instance_ids), in the order they should appear. A list that is not the \
                        same SET is refused — this re-orders, it never adds, drops or renames one \
                        — so read the current list first and send it back permuted. Nothing else \
                        changes: membership of a group, names, commands and run state are all \
                        untouched. Returns the project's commands in their NEW order. (A \
                        command's position INSIDE a group is a different thing: use \
                        manage_command_group(action=layout) for that.)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("the project whose command list to re-order"),
                "command_ids": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "EXACTLY the project's command TEMPLATE ids, in the wanted order \
                                   (each a command_id from list_commands(project_id=…), NOT an \
                                   instance_id).",
                }),
            },
            "required": ["project_id", "command_ids"],
        },
    }));
    // The MULTI-PANE GROUPING of managed commands, agent side (review 01M0ABR6V501D0SPR2BYBVZC4A
    // #90). ONE tool with an `action` discriminant over the eight operations the settings screen
    // offers the hand — and it delegates to the very same `nyx_core::db` transitions the UI's
    // `command_group_*` IPC routes call, which is what makes the two surfaces produce identical
    // state. The `action` enum below is compared name-for-name against the screen's gestures by
    // `command-groups.test.tsx`, which reads the FIRST inline `"enum": [ … ]` after the tool name
    // straight out of this source: keep `action` first in the properties, and keep its enum the
    // only INLINE array literal here (`shape`'s enum is the `COMMAND_GROUP_SHAPES` constant, not a
    // literal, precisely so the two cannot be confused). (`shape` is the
    // eighth, from review 01M11WE6H9E5D5FCS6RT0ZXAXN / finding 01M11XTRXVVPZ9XMDW5ARWKWHA: the
    // tiling shape became CHOSEN instead of derived, and the symmetry rule then requires the agent
    // to be able to choose it too.)
    descriptors.push(json!({
        "name": MANAGE_COMMAND_GROUP_TOOL,
        "description": "Compose and arrange a GROUP of managed commands — nyx's multi-pane \
                        grouping for commands, the exact counterpart of what the user does by \
                        hand in the commands settings screen (same DB transitions, so your edit \
                        and theirs are indistinguishable, in both directions). A command belongs \
                        to ZERO OR ONE group; a group holds AT LEAST TWO commands — taking it \
                        down to one DISSOLVES it and leaves the survivor solo. A group is born \
                        GREY and NAMELESS and stays perfectly usable that way: `name` and \
                        `color` are optional, independent, and clearing one never clears the \
                        other. The DISPOSITION has two halves and they are edited separately: the \
                        ordered member list (action=layout — WHICH member goes where) and the \
                        tiling SHAPE they are poured into (action=shape — auto | row | column | \
                        grid | main-left | main-top, `auto` being the shape derived from the member \
                        count: 2 side by side, the 3rd underneath, the 4th in a quadrant). The pane \
                        tree is derived from the two together, so re-ordering IS re-arranging. READ \
                        the current grouping WITHOUT writing anything: list_commands(project_id=…) \
                        returns the project's `groups` (id, name, color, layout_shape, command_ids \
                        in disposition order) and gives every command its `group_id` (null = solo). \
                        Returns the affected `group` \
                        — for action=remove that is the group AS IT SURVIVES, with its remaining \
                        members, and null ONLY when it really ceased to exist (dissolved, or the \
                        command was already solo) — plus the project's `groups` after the change.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": json!({
                    "type": "string",
                    "enum": ["create", "add", "remove", "dissolve", "name", "color", "layout", "shape"],
                    "description": "create: a NEW group from >=2 commands (project_id + command_ids, in layout order). \
                                   add: put a command INTO a group, at the END of its disposition (group_id + command_id; \
                                   a command already in another group leaves it first). \
                                   remove: take a command OUT of its group — it becomes solo (command_id; the group \
                                   dissolves if it is left with one member). \
                                   dissolve: ungroup ALL — every member becomes solo, none is deleted (group_id). \
                                   name: set or CLEAR the optional name (group_id + name; omit name or pass null to clear). \
                                   color: set or CLEAR the optional colour (group_id + color 1..6; omit or null to clear). \
                                   layout: fix the DISPOSITION (group_id + command_ids listing EXACTLY the current \
                                   members, in the wanted order — it re-arranges, it never adds or drops one). \
                                   shape: fix the TILING SHAPE the members are poured into (group_id + shape). \
                                   layout and shape are the two halves of the disposition and are deliberately \
                                   separate: layout is WHICH member goes where, shape is WHAT the grid looks like.",
                }),
                "project_id": str_prop("the project the group belongs to (required for action=create)"),
                "group_id": str_prop("the group to act on (required for add/dissolve/name/color/layout)"),
                "command_id": str_prop("the TEMPLATE id to move in or out (required for add/remove; a command_id from list_commands, NOT an instance_id)"),
                "command_ids": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "the TEMPLATE ids, IN LAYOUT ORDER: the founding members for action=create \
                                   (at least two), or exactly the current members for action=layout.",
                }),
                "shape": json!({
                    "type": "string",
                    // ONE spelling in Rust: the vocabulary comes from the same constant the DB
                    // transition validates against, so the schema an agent reads and the boundary
                    // that refuses a write can never disagree.
                    "enum": crate::db::COMMAND_GROUP_SHAPES,
                    "description": "action=shape only: the TILING SHAPE the members are poured into. \
                                   auto = the shape derived from the member count (2 side by side, the 3rd \
                                   underneath, the 4th in a quadrant) — the default, and what every group had \
                                   before shapes existed. row = all members side by side. column = all stacked. \
                                   grid = two columns filled row-major. main-left = the FIRST member takes the \
                                   full-height left half, the others stack on the right. main-top = the first \
                                   takes the full-width top half, the others sit in a row underneath.",
                }),
                "name": str_prop("action=name only: the group's new name; omit it (or pass null) to CLEAR the name — a blank string clears it too"),
                "color": json!({
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 6,
                    "description": "action=color only: the 1-based index of one of the six group colours; omit it (or pass null) to CLEAR the colour and go back to the neutral default.",
                }),
            },
            "required": ["action"],
        },
    }));
    // Append the interactive-terminal extension (PRD-4 review R-TERM): create / write to /
    // list / close an interactive terminal in nyx. Advertised alongside the v1 surface,
    // kept OUT of V1_TOOLS like the other extension tools. They reuse the existing terminal
    // + PTY primitives (no second terminal lifecycle).
    descriptors.push(json!({
        "name": CREATE_TERMINAL_TOOL,
        "description": "Open a new interactive terminal in nyx (a real shell the user can see \
                        and type into). Pass an optional `cwd` (a folder path): when it sits \
                        inside a known workspace the terminal is attached to that workspace and \
                        filed under it in the sidebar, otherwise it opens as a loose terminal at \
                        that directory. Pass an optional `command` to run a command the moment \
                        the shell opens (it is typed in and executed, and the terminal stays \
                        interactive afterwards) — omit it for a bare shell. The terminal's \
                        output streams live in nyx; use list_terminals to get its id, \
                        send_to_terminal to type more commands into it, and close_terminal to \
                        close it. Returns the new terminal's id.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "cwd": str_prop("optional working directory: if it is inside a known workspace the terminal is attached there, otherwise it opens loose at this directory (omitted = the default working directory, like a terminal the user opens by hand)"),
                "command": str_prop("optional command to run as soon as the terminal opens; the terminal stays interactive afterwards (omit for a bare shell)"),
                "label": str_prop("optional display label for the terminal in the sidebar"),
            },
            "required": [],
        },
    }));
    descriptors.push(json!({
        "name": SEND_TO_TERMINAL_TOOL,
        "description": "Run a command in an EXISTING open terminal: types the command plus a \
                        newline into the terminal so the shell executes it. Identify the \
                        terminal by its `terminal_id` from list_terminals. The command's output \
                        appears live in that terminal in nyx — read it with read_terminal. Use \
                        create_terminal first if no suitable terminal is open. An interactive \
                        terminal offers NO signal for \"that command has finished\": if you need \
                        to know, define it as a managed command (add_command) and run it with \
                        start_command + wait_for_command, which is what those tools are for.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "terminal_id": str_prop("id of the open terminal to run the command in (from list_terminals)"),
                "command": str_prop("the command line to run (a newline is appended so the shell executes it)"),
            },
            "required": ["terminal_id", "command"],
        },
    }));
    descriptors.push(json!({
        "name": SEND_KEYS_TOOL,
        "description": "Send raw keystrokes to an open terminal (`terminal_id` from \
                        list_terminals) to drive an interactive, full-screen (raw-mode) program — \
                        a TUI menu, a pager, an editor — or an AGENT running there. Unlike \
                        send_to_terminal (which types a shell COMMAND LINE and presses Enter for \
                        you), send_keys writes EXACTLY the keys you list and adds NO newline, so \
                        you control navigation, typing and submitting yourself. `keys` is an ARRAY \
                        processed left to right; each element is EITHER literal text (typed \
                        verbatim) OR a named key (case-insensitive): \"enter\", \"tab\", \
                        \"escape\"/\"esc\", \"backspace\", \"space\", \"up\", \"down\", \"left\", \
                        \"right\", \"home\", \"end\", \"pageup\", \"pagedown\", \"delete\", and \
                        any \"ctrl+<letter>\" combo. Anything else is literal text. EACH ELEMENT \
                        IS SENT AS ITS OWN PTY WRITE, so a full-screen app sees \"enter\" as a \
                        real Enter and not as the tail of the text before it — [\"prompt\", \
                        \"enter\"] in ONE call types AND submits. Examples: [\"down\", \"down\", \
                        \"enter\"] walks a menu and selects; [\"ctrl+c\"] interrupts; [\"draft\"] \
                        types without submitting. The result reports `bytes` sent and `writes` \
                        (how many PTY writes it took). Then wait_for_terminal (for an agent's \
                        answer) or read_terminal for what came out.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "terminal_id": str_prop("id of the open terminal to send keys to (from list_terminals)"),
                "keys": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "ordered keystrokes: each element is literal text (typed \
                                   verbatim, NO newline added) or a named key like \"enter\", \
                                   \"up\", \"escape\", \"tab\", \"ctrl+c\" (case-insensitive). \
                                   Include \"enter\" yourself to submit; omit it to type without \
                                   submitting.",
                }),
            },
            "required": ["terminal_id", "keys"],
        },
    }));
    descriptors.push(json!({
        "name": LIST_TERMINALS_TOOL,
        "description": "List the terminals in nyx. Each entry carries its `terminal_id` (what \
                        send_to_terminal / send_keys / read_terminal / wait_for_terminal / \
                        close_terminal take), its working directory, label, workspace (if \
                        attached), `status` (\"alive\"|\"closed\"), `live` (its shell has started \
                        and can accept input) and `pty_id`. Only OPEN terminals are listed by \
                        default; `include_closed:true` also lists closed ones — a closed terminal \
                        can no longer be written to, but read_terminal still returns its last \
                        saved scrollback, so this is how you rediscover a finished terminal's id. \
                        An entry hosting an AGENT (claude, codex, …) also carries \
                        `agent_activity` — \"working\" (mid-turn), \"waiting\" (blocked on a \
                        human: a question or a permission prompt), \"idle\" (between turns), null \
                        when no agent session runs there — and `agent_session` { session_id, \
                        agent_kind, external_session_id, state, transcript_path }: which agent \
                        runs there, which of its sessions, and the transcript file it writes. \
                        TO WAIT FOR SOMETHING, CALL THE TOOL THAT WAITS — never poll this one. \
                        wait_for_terminal blocks on an agent's turn; wait_for_command blocks on a \
                        managed command. The `busy` / `exec_state` (\"idle\"|\"running\"|\
                        \"success\"|\"error\") / `exec_exit_code` / `exec_state_updated_at` fields \
                        report the last foreground command the OS could see; they are BLIND to an \
                        agent's turn — the agent is the foreground process for its whole life, so \
                        they do not move while it answers — and `busy` is null wherever it cannot \
                        be derived. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "include_closed": { "type": "boolean", "description": "when true, also list CLOSED terminals (each carries status:\"closed\"); default false lists only open terminals" },
            },
            "required": [],
        },
    }));
    descriptors.push(json!({
        "name": CLOSE_TERMINAL_TOOL,
        "description": "Close an open terminal by id: ends its shell and removes it from the \
                        nyx sidebar. Identify it by its `terminal_id` from list_terminals. This \
                        cannot be undone (the terminal's live session is gone), but it does not \
                        affect any other terminal.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "terminal_id": str_prop("id of the open terminal to close (from list_terminals)"),
            },
            "required": ["terminal_id"],
        },
    }));
    descriptors.push(json!({
        "name": READ_TERMINAL_TOOL,
        "description": "Read a terminal's recent output (scrollback) by `terminal_id` — from \
                        list_terminals (with include_closed:true to also find CLOSED terminals) or \
                        from create_terminal. Returns a bounded tail (~12 KB by default, ANSI \
                        stripped); tune it with `tail_bytes` / `max_bytes`, pass \
                        `strip_ansi:false` for raw bytes, and page with `since` (the previous \
                        call's `cursor`). An unknown id is an error. \
                        WAIT FIRST, THEN READ — never read repeatedly to find out whether \
                        something finished. wait_for_terminal blocks on the turn of an AGENT \
                        running in the terminal (claude, codex, …) and hands back a `cursor` that \
                        chains straight into `since` here; wait_for_command blocks on a managed \
                        command. Polling `busy` / `exec_state` is the trap: they never move while \
                        an agent answers. \
                        FRESHNESS: nyx keeps the saved scrollback up to date only while the \
                        terminal is MOUNTED in the app (debounced), so a terminal that is off \
                        screen keeps running while its buffer stands still. Every answer carries \
                        `live` (a shell is running behind it right now) and `stale`. \
                        `stale:false` is the ONLY case you may read as the terminal's PRESENT \
                        state; `stale:true` is a PAST snapshot — either `live:false` (closed or \
                        unloaded: this is its final saved output) or output nyx has not saved yet \
                        (retry). The answer also carries `agent_activity` \
                        (\"working\"|\"waiting\"|\"idle\", null when no agent session runs there), \
                        so you can tell whether the agent was still mid-turn when you read. \
                        `since` IS BEST-EFFORT: scrollback is the re-serialized terminal grid, not \
                        an append-only log, so a reflow, an alt-screen app, a `clear` or an \
                        eviction can rewrite it. It pages forward without dropping the head of a \
                        burst, but if the buffer shrank below your cursor the answer carries \
                        `reset:true` with fresh content — re-read WITHOUT `since` to resync. \
                        FIDELITY: line-based output is faithful; a full-screen (alt-screen) TUI is \
                        drawn by cursor jumps, and the blank columns those stand for are restored \
                        as spaces so words stay separated — but the layout is approximate. \
                        A `tail_bytes`/`max_bytes` above the 1 MiB ceiling fails with \
                        `error.data.code=\"output_too_large\"` plus `error.data.requested` / \
                        `error.data.limit` — detect that code and retry smaller rather than \
                        guessing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "terminal_id": str_prop("id of the terminal to read (from list_terminals)"),
                "tail_bytes": int_prop("how many bytes from the END of the scrollback to return (default 12288, a token-safe window). Capped by max_bytes; above 1048576 → output_too_large"),
                "max_bytes": int_prop("alternative hard ceiling on the returned window (default 1048576; the smaller of tail_bytes/max_bytes wins). Same limit, same refusal"),
                "since": int_prop("byte offset to resume from (a previous call's `cursor`); omit to read the tail. BEST-EFFORT — check `reset` in the answer"),
                "strip_ansi": { "type": "boolean", "description": "strip ANSI escape sequences from the returned output (default true). false returns the raw bytes" },
            },
            "required": ["terminal_id"],
        },
    }));
    // Append `wait_for_terminal`: the BOUNDED wait on a terminal's AGENT activity — the twin of
    // `wait_for_command` for the other thing an agent waits on. Listed AFTER the v1 surface and
    // kept out of [`V1_TOOLS`] for the same reason as `probe` / `wait_for_command`.
    descriptors.push(json!({
        "name": WAIT_FOR_TERMINAL_TOOL,
        "description": "Wait for the AGENT running in a terminal (claude, codex, …) to finish its \
                        turn — the tool to call right after you send it a prompt with send_keys, \
                        instead of polling anything. Blocks up to `timeout_ms` (max ~60s) until \
                        the terminal's `agent_activity` enters `until`, then returns \
                        resolved:true; if the timeout elapses first it returns resolved:false — a \
                        NORMAL outcome, not an error: call again to keep waiting. Read-only: it \
                        sends nothing and changes nothing. \
                        Activity: \"working\" (mid-turn), \"waiting\" (blocked on a human — a \
                        question or a permission prompt), \"idle\" (between turns). The DEFAULT \
                        `until` is [\"idle\",\"waiting\"] — \"the agent has given you the hand \
                        back\" — which covers a finished answer and a blocked one alike. \
                        `output_tail` carries, by default, only the output produced AFTER this \
                        call (never the whole scrollback), ANSI-stripped, and the returned \
                        `cursor` chains directly into read_terminal(since=cursor) so you fetch the \
                        rest with no gap and no duplication. `agent_activity` is the same value \
                        list_terminals reports — what the hooks said, never a guess. \
                        `submission_pending:true` means nyx wrote an enter here and the agent has \
                        not spoken since: with resolved:false, wait again rather than send \
                        anything else. The answer also carries `agent_session` (which agent and \
                        which of its sessions runs there) and the usual `live`/`stale` freshness \
                        bits. \
                        ON A TERMINAL WITH NO AGENT SESSION this is not an error: `agent_session` \
                        is null and the activity is always \"idle\", so the default wait resolves \
                        immediately — use wait_for_command for a managed command. At most 3 may \
                        block at once (a worker must stay free for the hooks that end them); a 4th \
                        is refused with `invalid_state` — let one return, or read_terminal. A \
                        tail_bytes/max_bytes above the 1 MiB ceiling fails with \
                        `error.data.code=\"output_too_large\"` plus `error.data.requested` / \
                        `error.data.limit` — detect that code and retry smaller rather than \
                        guessing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "terminal_id": str_prop("id of the OPEN terminal to wait on (from list_terminals)"),
                "until": json!({
                    "type": "array",
                    "items": { "type": "string", "enum": ["idle", "working", "waiting", "settled"] },
                    "description": "activity states that resolve the wait (default: [\"idle\",\"waiting\"] — the agent gave the hand back). \"settled\" is an alias for idle+waiting.",
                }),
                "timeout_ms": int_prop("max wait in ms before resolved:false (default 30000, clamped to ~60000 max)"),
                "since": int_prop("byte offset from a previous read_terminal/wait_for_terminal `cursor`, so output_tail/cursor resume incrementally. OMIT on the first call: it then defaults to the current end-of-buffer."),
                "tail_bytes": int_prop("how many bytes of new output to return in output_tail (default 12288, a token-safe window); above 1048576 → output_too_large"),
                "max_bytes": int_prop("alternative hard ceiling on output_tail (default 1048576; the smaller of tail_bytes/max_bytes wins). Same limit, same refusal"),
                "strip_ansi": json!({ "type": "boolean", "description": "when true (the DEFAULT), output_tail has ANSI/terminal control sequences stripped; set false for raw bytes. cursor is byte-exact either way." }),
            },
            "required": ["terminal_id"],
        },
    }));
    // Append the agent-session-event channel (PRD-5 #4, ADR-0004 / ADR-0010): the MCP
    // tool a Claude Code SessionStart/SessionEnd hook calls so nyx can capture/end the
    // agent session bound to a terminal. Advertised alongside the v1 surface, kept OUT
    // of V1_TOOLS for the SAME reason as `probe`: it is a PRD-5 capability over the
    // existing DB, not part of the frozen contract clients onboard against. The schema
    // accepts the raw Claude hook fields directly so a `mcp_tool` hook can forward its
    // input verbatim.
    descriptors.push(json!({
        "name": AGENT_SESSION_EVENT_TOOL,
        "description": "Report an agent session lifecycle event to nyx (used by the nyx Claude \
                        Code plugin's SessionStart/SessionEnd hooks, not normally called by \
                        hand). Pass the hook's fields — `hook_event_name` (SessionStart or \
                        SessionEnd), `session_id`, and on start `cwd`, `transcript_path`, \
                        `source` — plus the `NYX_TERMINAL_ID` of the terminal the agent runs in \
                        so nyx can bind the session to that terminal. nyx records the session so \
                        it can be resumed later; SessionEnd marks it cleanly ended.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "hook_event_name": str_prop("the Claude hook event: \"SessionStart\" or \"SessionEnd\""),
                "session_id": str_prop("the agent's own session id (Claude session_id) — what resume is built from"),
                "NYX_TERMINAL_ID": str_prop("the nyx terminal record id the agent runs in (exported into the shell by nyx); used to correlate the session to a terminal"),
                "cwd": str_prop("working directory of the session (SessionStart)"),
                "transcript_path": str_prop("path to the agent transcript file (SessionStart, optional)"),
                "source": str_prop("SessionStart source: startup | resume | clear (optional)"),
                "agent_kind": str_prop("which agent this is (default \"claude_code\")"),
                "prompt": str_prop("UserPromptSubmit only: the raw text the HUMAN just submitted. nyx records that a human turn happened on this terminal (and its words), which is what lets the orchestration engine tell a human's answer from a session's"),
            },
            "required": ["hook_event_name", "session_id"],
        },
    }));
    // Append the v2.2 cockpit read model tool: a workspace's LIVE runtime aggregate.
    // Advertised alongside the v1 surface, kept OUT of V1_TOOLS (a new read over the
    // existing authorities, not part of the frozen contract).
    descriptors.push(json!({
        "name": WORKSPACE_RUNTIME_TOOL,
        "description": "A workspace's LIVE runtime: its terminals, its agent sessions and its \
                        services. Pass the `workspace_id` (from list_workspaces). NO OUTPUT IS \
                        INLINED: terminals, services and ended sessions report `scrollback_bytes` \
                        / `transcript_path` instead — read them with read_terminal / \
                        get_command_output. Sessions: LIVE FIRST, PAGED \
                        (`agent_sessions_next_offset`).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace whose live runtime to aggregate (from list_workspaces(project_id=…))"),
                "limit": int_prop("max agent sessions (default 50, max 200)"),
                "offset": int_prop("first agent session (0-based)"),
            },
            "required": ["workspace_id"],
        },
    }));
    // Append the v2.2 Git CHANGES read model tool: what changed in a workspace, per repo.
    descriptors.push(json!({
        "name": WORKSPACE_GIT_TOOL,
        "description": "See what changed in a workspace, read live from Git: for each of the \
                        workspace's repositories, its current branch, whether the work tree has \
                        uncommitted changes, and the list of changed files (with their Git status \
                        codes). This is a SUMMARY — file names and status, not a line-by-line \
                        diff. Pass the `workspace_id` (from list_workspaces). Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace whose Git changes to summarize (from list_workspaces(project_id=…))"),
            },
            "required": ["workspace_id"],
        },
    }));
    // Append the v2.2 workspace TIMELINE read model tool: the bounded activity fil of a
    // workspace's recent business events (from the v2.1 domain_events outbox).
    descriptors.push(json!({
        "name": WORKSPACE_TIMELINE_TOOL,
        "description": "See a workspace's recent activity as a BOUNDED, chronological timeline: \
                        the business events recorded for it (its own and its project's/repos' \
                        lifecycle — created, renamed, archived, primary-set, …), oldest first. \
                        This is a short, capped fil of notable changes — NOT a full activity log \
                        and NEVER terminal output. Pass the `workspace_id` (from list_workspaces) \
                        and an optional `limit`. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace whose recent activity timeline to read (from list_workspaces(project_id=…))"),
                "limit": int_prop("max number of most-recent events to return (bounded; defaults to a small ceiling when omitted or 0)"),
            },
            "required": ["workspace_id"],
        },
    }));
    // Append the v2.4 Phase-5 Spec AUTHORING + TRANSITION tools (PDR-0020): an agent drives
    // a Spec (PRD) end to end — create, author its sections, fork, transition its lifecycle,
    // anchor it on a dev workspace, and add/transition/link its tasks — from nyx state, with
    // ZERO depot CLI. Every one delegates to the SHARED nyx-core authority (the SAME lifecycle
    // guards + validations + guard-rails the native Specs UI uses), so the MCP mutation is the
    // single authority. Kept OUT of V1_TOOLS (a new surface, not the frozen v1 contract).
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_CREATE_TOOL,
        "description": "Create a new Spec (a product design document) in nyx from its state — no \
                        depot CLI. Starts a draft Spec with its first revision, optionally seeding \
                        the structured \
                        sections (problem / solution / implementation_decisions / \
                        testing_decisions / user_stories / out_of_scope). The Spec is authored \
                        PROJECT-scoped and UNANCHORED — the dev workspace anchor is posed later, \
                        when nyx starts the Spec (ready→in_progress), not here. \
                        Optionally CLASSIFY it with `category` (the genre of work: bug, feature, \
                        refactor, chore, or any category of the project) — assigned in the SAME \
                        transaction, so the Spec is never created half-classified. Returns the \
                        created spec, revision and category. Use spec_update_section to fill \
                        sections later, and spec_set_category to reclassify. THERE IS NO LIFECYCLE \
                        VERB ON THIS SURFACE, on purpose: nyx imposes each Spec transition (ready \
                        / start / review / done / cancel) itself, at the workflow step built for \
                        it — you reach that step by ticking nyx_next, and never move a Spec by \
                        hand.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project to create the Spec in (from list_projects)"),
                "title": str_prop("the Spec's title"),
                "category": str_prop("optional genre of work for the Spec — its slug (bug | feature | refactor | chore, or one the project added) or its human name. An UNKNOWN name is not an error: nyx mints that category in the project's vocabulary and it is VALID AS IT STANDS, no review pending (its slug is derived from the name and never lands on a system slug). Prefer an EXISTING category when one fits — a mint that only rephrases one already there dilutes the vocabulary for everybody. Omit it to leave the Spec unclassified"),
                "problem": str_prop("optional 'problem' section (markdown)"),
                "solution": str_prop("optional 'solution' section (markdown)"),
                "implementation_decisions": str_prop("optional 'implementation decisions' section (markdown)"),
                "testing_decisions": str_prop("optional 'testing decisions' section (markdown)"),
                "user_stories": str_prop("optional 'user stories' section (markdown)"),
                "out_of_scope": str_prop("optional 'out of scope' section (markdown)"),
            },
            "required": ["project_id", "title"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_UPDATE_SECTION_TOOL,
        "description": "Edit ONE structured section of a Spec's active revision, in place (this is \
                        NOT a fork). Pass the spec_id, the section name (problem, solution, \
                        implementation_decisions, testing_decisions, user_stories, out_of_scope), \
                        and the new markdown value (an empty string clears the section). Use \
                        spec_fork instead when you want to snapshot the current design into a new \
                        revision before diverging. Sections are FROZEN once the Spec leaves draft; \
                        the ONE exception is a correction a human authorized at a blocked-task \
                        escalation gate — pass the run_id handed to you with that spawn, and nyx \
                        checks the authorization itself (it is never granted by asking).",
        "inputSchema": {
            "type": "object",
            "properties": {
                // THE CONDITIONAL CASE, stated instead of left to be discovered. This tool's ONLY
                // project anchor is its OPTIONAL `run_id`, so `#N` / `N` resolve with one and
                // cannot without — on the nominal path (editing a draft, no run_id) they used to
                // die as `unknown spec 3` although the field announced them unconditionally
                // (finding 01M0ZGHD137MDXFGS2PBG6GS68).
                "spec_id": conditional_spec_ref_prop(
                    "the Spec whose active revision to edit",
                    "you also pass run_id, the one project anchor this call carries",
                ),
                "section": str_prop("which section: problem | solution | implementation_decisions | testing_decisions | user_stories | out_of_scope"),
                "value": str_prop("the section's new markdown content (empty string clears it)"),
                "run_id": str_prop("the run whose human authorized this correction on a Spec that has left draft (the run_id of your spawn); ignored while the Spec is still a draft"),
            },
            "required": ["spec_id", "section", "value"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_FORK_TOOL,
        "description": "Fork a Spec into a NEW revision: the current revision is snapshotted and \
                        marked superseded, and a fresh active revision (with the same sections) \
                        becomes the editable one. Use this to keep history when the design \
                        diverges. Pass the spec_id and an optional change_reason (why you forked). \
                        Returns the new active revision.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "spec_id": anchorless_spec_ref_prop("the Spec to fork"),
                "change_reason": str_prop("optional reason recorded on the new revision (why it supersedes the prior one)"),
            },
            "required": ["spec_id"],
        },
    }));
    // N17(b) — `spec_transition` is NOT advertised to the agent: nyx imposes every Spec lifecycle
    // transition at the graph node built for it (ndev/nspec), so the effector never transitions a Spec.
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_SET_WORKSPACE_TOOL,
        "description": "Anchor a Spec on a dev workspace (its repos + project are derived \
                        through that workspace), or detach it by omitting workspace_id. Allowed \
                        while the Spec is ready or in_progress — a draft Spec (still being \
                        authored) and a settled/parked one are refused. Linking NEVER changes the \
                        Spec's status. The workspace must exist and belong to the SAME project as \
                        the Spec. Re-applying the anchor the Spec already has is a no-op. Returns \
                        the updated spec.",
        "inputSchema": {
            "type": "object",
            "properties": {
                // THE SHARPEST OF THE CONDITIONAL FIELDS. This call's ONLY project anchor is
                // `workspace_id`, and it is optional BECAUSE omitting it IS the detach — so on the
                // detach path a `#N` can never resolve, and it used to die as a bare `unknown spec`
                // that did not even quote the token sent (finding 01M0ZKWXS353TABS0WCT3XXB81).
                "spec_id": conditional_spec_ref_prop(
                    "the ready or in_progress Spec to anchor/re-anchor/detach",
                    "you also pass workspace_id, this call's only project anchor — which the DETACH \
                     path omits by definition, so a number never resolves while detaching",
                ),
                "workspace_id": str_prop("the dev workspace to anchor on (from list_workspaces); omit or null to detach. An EMPTY string is refused, never read as a detach — detaching is an act you ask for by omitting the argument, not something an uninitialized variable should trigger"),
            },
            "required": ["spec_id"],
        },
    }));
    // PDR-0017 — the Spec's ONE genre-of-work axis. Assignment AND removal in one verb, like
    // `spec_set_workspace` above: naming no category IS the removal.
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_SET_CATEGORY_TOOL,
        "description": "Set a Spec's CATEGORY — its genre of work (bug, feature, refactor, chore, \
                        or any category the project added) — or REMOVE it by naming none. A Spec \
                        carries at most ONE category. Name it with `category` (its slug, or its \
                        human name) or with `category_id` (an exact category), never both. An \
                        UNKNOWN name is not an error: nyx MINTS that category in the project's \
                        vocabulary and assigns it — the response says created:true. The mint is \
                        VALID AS IT STANDS, nothing is pending on it, so PREFER AN EXISTING \
                        category whenever one fits: a mint that only rephrases one already there \
                        dilutes the project's vocabulary and only a human can take it back, from \
                        the project settings. A name that matches two categories is refused, \
                        listing their slugs, so you can re-ask by slug. The category must belong \
                        to the Spec's own project. Unlike the Spec's sections, the category is NOT \
                        frozen by the lifecycle: reclassifying a Spec is allowed at any status. \
                        Returns the updated spec and the category (id, slug, name, color_token, \
                        created_by_type, system).",
        "inputSchema": {
            "type": "object",
            "properties": {
                // This call carries NO project anchor of its own, and the one argument that could
                // be read as one is the OPTIONAL category — which the REMOVAL omits by definition.
                // So a `#N` never resolves here, exactly as on `spec_set_workspace`'s detach path.
                "spec_id": anchorless_spec_ref_prop("the Spec to classify (or to un-classify)"),
                "category": str_prop("the category's slug (bug | feature | refactor | chore, or one the project added) or its human name. Unknown → nyx mints it, valid as it stands (slug derived from the name, never a system slug) — so reach for an existing one first. Omit BOTH this and category_id to REMOVE the Spec's category. An EMPTY string is refused, never read as a removal"),
                "category_id": str_prop("an EXACT category id, when you already have one. Mutually exclusive with 'category'. An EMPTY string is refused, never read as a removal"),
            },
            "required": ["spec_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::TASK_ADD_TOOL,
        "description": "Add a task to a Spec (or standalone). kind selects slice (agent \
                        implementation work, the default), human (an action only the user can do — \
                        give it a verification_command so it is verifiable), or review (a finding — \
                        carry a severity). Link it to a Spec with spec_id, nest it under a parent \
                        with parent_task_id, and group it with phase. Returns the created task.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project the task belongs to (from list_projects)"),
                "title": str_prop("the task's title"),
                "spec_id": spec_ref_prop("optional Spec to attach the task to"),
                // THE FOUR CLOSED VOCABULARIES, advertised FROM the constants the write path
                // refuses outside of (`db::TASK_*`), never from a hand-written paraphrase: an
                // unknown value is an `invalid_argument` naming this very set, so the sentence an
                // agent reads and the set it is checked against are one fact.
                "kind": str_prop(&format!(
                    "optional kind ({}); defaults to slice. slice is agent implementation work, \
                     human is an action only the user can do (give it a verification_command), \
                     review is a finding (carry a severity). Any other value is refused",
                    crate::db::TASK_KINDS.join(" | ")
                )),
                "type": str_prop(&format!(
                    "optional task type ({}); defaults to implementation. Any other value is refused",
                    crate::db::TASK_TYPES.join(" | ")
                )),
                "priority": str_prop(&format!(
                    "optional priority ({}); defaults to med. Any other value is refused",
                    crate::db::TASK_PRIORITIES.join(" | ")
                )),
                "severity": str_prop(&format!(
                    "optional finding severity ({}), for kind=review; omit it on any other kind. \
                     Any other value is refused",
                    crate::db::TASK_SEVERITIES.join(" | ")
                )),
                "description": str_prop("optional long-form body"),
                "acceptance_criteria": str_prop("optional acceptance/done criteria"),
                "verification_command": str_prop("how a kind=human action is verified (required to close a human task)"),
                "parent_task_id": str_prop("optional parent task (subtask decomposition)"),
                "phase": str_prop("optional phase NAME — a STRING, never a number (needs spec_id). An integer is REFUSED, not ignored, and so is an all-digits string (\"2\") — nyx NUMBERS the phases itself, so 3 names nothing. A name CONTAINING a digit ('Phase 2') is fine. While the Spec is DRAFT nyx materializes its plan from the names, in first-seen order — that is how the plan is written. Once the Spec has left draft the plan is FROZEN: a name it does not already hold is refused, listing the ones it holds. Pass the NAME alone, never the '2 · Some phase' heading a manual renders. NAME IN, RANK OUT: the created task's `phase` field is the 1-based RANK nyx assigned, not the name you sent — so the response also carries `phase_name`, echoing the name, and the two together are how you re-read your own write. On a kind=review finding, OMIT IT — nyx anchors the finding on the phase under review itself"),
                "round": int_prop("rarely needed: the rework round of a kind=review finding. OMIT IT — nyx fills it from the Spec's own review pass (per-phase: 2.1, 4.1…), which it knows and you do not. Pass one only to attribute a finding to a pass other than the current one"),
            },
            "required": ["project_id", "title"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::TASK_TRANSITION_TOOL,
        "description": "Move a task through its lifecycle: start (→in_progress), done \
                        (in_progress→done), block (carry a reason), skip (cancel, carry a reason), \
                        or verify (the human-verified close for a kind=human task). Transitions are \
                        guarded (the current status must allow it). A kind=human task cannot be \
                        plain-done'd — use transition='verify' on a task that carries a \
                        verification_command. Returns the updated task.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task_id": str_prop("the task to transition"),
                "transition": str_prop("start | done | block | skip | verify"),
                "reason": str_prop("the block/skip reason (stamped onto the task)"),
            },
            "required": ["task_id", "transition"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::TASK_ADD_DEPENDENCY_TOOL,
        "description": "Add a dependency edge between two tasks: task_id depends on \
                        depends_on_task_id (it needs the latter done first). The graph is kept \
                        acyclic — an edge that would create a cycle, or a self-edge, is refused. \
                        type is blocks (ordering, default) or relates_to (a soft link). A duplicate \
                        edge is a no-op.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task_id": str_prop("the dependent task (the one that needs the other first)"),
                "depends_on_task_id": str_prop("the prerequisite task"),
                "type": str_prop("blocks (default, ordering) | relates_to (soft link)"),
            },
            "required": ["task_id", "depends_on_task_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::TASK_DELETE_TOOL,
        "description": "DELETE a task outright (and its dependency edges) — the draft-authoring \
                        removal. Use it while the parent Spec is still in draft to make a \
                        mis-sliced task disappear cleanly; it is REFUSED (invalid_state) once \
                        the Spec is ready/in_progress — there, skip the task instead \
                        (task_transition transition=skip), which keeps the audit trail. An \
                        unknown task is invalid_id. The Spec's phase plan is left intact.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task_id": str_prop("the task to delete (its parent Spec must be in draft)"),
            },
            "required": ["task_id"],
        },
    }));
    // Append the v2.6 Phase-3 Spec↔Spec DEPENDENCY tools: express structural constraints between
    // Specs ("don't merge A before B", "when X lands, do Y") as a cycle-checked directed graph,
    // and read the computable "bloquée par / débloque" badges. Delegating to the SHARED nyx-core
    // spec-dependency authority. Part of SPEC_TOOLS (kept OUT of V1_TOOLS).
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_LINK_TOOL,
        "description": "Link one Spec to another as a DEPENDENCY: from_spec_id depends on / comes \
                        after to_spec_id (to must land first; from is 'blocked by' to, to \
                        'unblocks' from). kind is 'blocks' (default, a hard merge gate) or 'after' \
                        (sequencing) — both directional and cycle-checked — or 'related' (context \
                        only, never gates and never cycles). An optional note records the rationale. \
                        A link that would create a cycle in the dependency graph is REFUSED \
                        (invalid_state); a self-link or an unknown Spec is refused too. Re-linking \
                        the same pair updates the kind/note.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "from_spec_id": anchorless_spec_ref_prop("the dependent Spec (the one that comes after / is blocked)"),
                "to_spec_id": anchorless_spec_ref_prop("the prerequisite Spec (the one that must land first)"),
                "kind": str_prop("blocks (default) | after (both directional) | related (context only)"),
                "note": str_prop("optional rationale for the constraint"),
            },
            "required": ["from_spec_id", "to_spec_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_UNLINK_TOOL,
        "description": "Remove a Spec↔Spec dependency edge (from_spec_id → to_spec_id). Idempotent \
                        (unlinking a pair that is not linked reports unlinked:false).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "from_spec_id": anchorless_spec_ref_prop("the dependent Spec of the edge to remove"),
                "to_spec_id": anchorless_spec_ref_prop("the prerequisite Spec of the edge to remove"),
            },
            "required": ["from_spec_id", "to_spec_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_DEPS_TOOL,
        "description": "Read a Spec's dependency view + computable badges: blocked_by (the Specs it \
                        depends on / comes after — the 'bloquée par N' badge) and unblocks (the \
                        Specs that depend on it — the 'débloque M' badge), each with the other \
                        Spec's title/number/status, plus related context links and the counts \
                        blocked_by_count / unblocks_count. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": { "spec_id": anchorless_spec_ref_prop("the Spec whose dependencies + badges to read") },
            "required": ["spec_id"],
        },
    }));
    // The v2.8 READ half of the Spec surface. It was WRITE-ONLY: an agent could author a Spec and
    // its tasks and had no way to read any of it back — the only door was `agent_context`, which
    // wraps the state in a ROLE MANUAL and refuses outright for coder/doc when no Spec resolves.
    // Three plain reads close it: ONE Spec whole, the Specs of a project, the tasks of a Spec.
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_GET_TOOL,
        "description": "READ one Spec whole — the way to re-read what you just wrote. Returns the \
                        Spec (status, priority, mode, its workspace anchor, and `phases`: the \
                        ordered plan as {number, name} entries), its ACTIVE revision (the six \
                        structured sections: problem / solution / implementation_decisions / \
                        testing_decisions / user_stories / out_of_scope), and its tasks — each \
                        carrying `phase_name` beside the numeric `phase`, so a rank is readable \
                        without a second call. The task list is PAGED (offset/limit): read \
                        `tasks_next_offset` and call again while it is non-null, or use task_list \
                        to filter. This needs no role and no run — unlike agent_context, which \
                        renders a role manual around the state. An unknown Spec is invalid_id. \
                        Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                // CONDITIONAL: `project_id` is the one anchor here and it is OPTIONAL, so `#N` / `N`
                // resolve with it and cannot without — stated rather than left to be discovered.
                "spec_id": conditional_spec_ref_prop(
                    "the Spec to read",
                    "you also pass project_id, the only project anchor this call carries",
                ),
                "project_id": str_prop("optional project the short '#N' / 'N' forms are counted in (from list_projects); not needed for a canonical id or a 'KEY-N'"),
                "offset": int_prop("first task to return (0-based; defaults to 0) — page with the `tasks_next_offset` the answer carries"),
                "limit": int_prop("how many tasks to return (defaults to 50, capped at 200)"),
            },
            "required": ["spec_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::SPEC_LIST_TOOL,
        "description": "LIST the live Specs — how you FIND a Spec whose id you do not have. Returns \
                        each Spec in board order with its canonical id, its human `key` (e.g. \
                        NYX-3, the form spec_get and task_add also accept), title, status, \
                        priority, number and phase plan. Archived Specs are excluded. project_id \
                        is OPTIONAL: pass it to scope to one project, pass a workspace_id or a cwd \
                        to have nyx resolve the project from where you are, or pass NOTHING to \
                        list across every project — which is what you want when you do not know \
                        which project a lost Spec is in — a project_id you DO pass must exist: an \
                        unknown one is refused, not answered with an empty list. `status` narrows \
                        to one lifecycle state (draft, ready, in_progress, in_review, …). OLDEST \
                        FIRST per project: with a project_id your new Spec is LAST (offset = \
                        total - 1); across projects use status:\"draft\". PAGED: read \
                        `next_offset` and call again while it is non-null. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("optional project to scope to (from list_projects); it must exist (an unknown id is refused). Omit it — with or without a workspace_id / cwd — to list across every project"),
                "workspace_id": str_prop("optional: resolve the project from this workspace instead of naming it (from list_workspaces)"),
                "cwd": str_prop("optional: resolve the project from a directory (the workspace whose root best matches this path)"),
                "status": str_prop("optional lifecycle status to narrow to (draft | ready | in_progress | blocked | in_review | completed | cancelled)"),
                "offset": int_prop("first Spec to return (0-based; defaults to 0) — page with the `next_offset` the answer carries, or jump to the newest with total - 1"),
                "limit": int_prop("how many Specs to return (defaults to 50, capped at 200)"),
            },
            "required": [],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_spec::TASK_LIST_TOOL,
        "description": "LIST a Spec's tasks — its plan AND its review findings, which are the same \
                        list (a finding is a task with kind=review). Each task carries \
                        `phase_name` beside its numeric `phase`, and the answer repeats the Spec's \
                        `phases` plan so a rank is never read without its name. Narrow with `kind` \
                        (slice | human | review), `status`, or `phase` — the phase NAME, the same \
                        word task_add takes, never its number. PAGED: read `next_offset` and call \
                        again while it is non-null. An unknown Spec is invalid_id. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "spec_id": conditional_spec_ref_prop(
                    "the Spec whose tasks to list",
                    "you also pass project_id, the only project anchor this call carries",
                ),
                "project_id": str_prop("optional project the short '#N' / 'N' forms are counted in (from list_projects); not needed for a canonical id or a 'KEY-N'"),
                "kind": str_prop(&format!(
                    "optional kind to narrow to ({}); omit for every task",
                    crate::db::TASK_KINDS.join(" | ")
                )),
                "status": str_prop("optional task status to narrow to (todo | ready | in_progress | blocked | in_review | done | cancelled)"),
                "phase": str_prop("optional phase NAME to narrow to — a STRING, the same name task_add takes, never its rank. A name the plan does not hold returns no task rather than every task"),
                "offset": int_prop("first task to return (0-based; defaults to 0) — page with the `next_offset` the answer carries"),
                "limit": int_prop("how many tasks to return (defaults to 50, capped at 200)"),
            },
            "required": ["spec_id"],
        },
    }));
    // Append the v2.4 Phase-5 agent-CONTEXT tool (PDR-0020): render a
    // spec/dev/coder/auditor/doc/prototype manual live from nyx state (the native
    // equivalent of `depot context …`).
    descriptors.push(json!({
        "name": crate::mcp_context::AGENT_CONTEXT_TOOL,
        "description": "Render an agent context manual (spec, dev, coder, auditor, doc, or prototype) LIVE from nyx \
                        state — the role's own contract plus the run's state. IF YOU WERE SPAWNED BY \
                        A `nyx_next` ACTION, CALL THIS FIRST: the action hands you an `agent_id`, a \
                        `label`, a `subject` and the `run_id` it is ticking, and nothing else — this \
                        is where you read who you are and what the run holds. \
                        Given a role and a run (run_id) or a Spec (spec_id, or resolved from the \
                        current workspace via cwd/workspace_id), it returns: the role's preamble and \
                        its prohibitions, the Spec's structured sections, its current phase and that \
                        phase's tasks, and its PROJECT + repos (plus the dev workspace when one is \
                        anchored). The answer \
                        carries project_id beside the manual: a Spec belongs to a project from the \
                        moment it exists, so the project is there even before any code and before \
                        any dev workspace — which is the NORMAL state for role=prototype, and that \
                        id is what every design_* tool takes. For role=spec with NO Spec yet (ZERO \
                        anchored on the location), it renders the PROJECT-scoped authoring manual \
                        instead (a Spec is a project artifact the author creates), resolving the \
                        project from run_id, project_id or the cwd/workspace — never an error for \
                        want of a Spec. That exemption covers ZERO ONLY: role=spec is NOT exempt \
                        from the AMBIGUOUS case, where a location anchoring SEVERAL active Specs is \
                        refused for EVERY role alike (the location is settled before the role is \
                        read) — see RESOLUTION ORDER. \
                        RESOLUTION ORDER: spec_id, then run_id (a run bound to no Spec resolves \
                        NONE — the answer stops at the run), then workspace_id, then cwd. Resolving \
                        BY LOCATION (workspace_id / cwd) is exact: the ONE active Spec anchored \
                        there resolves; zero resolves none — and SAYS so rather than rendering a \
                        manual that looks scoped: the answer stays ok for role=dev/spec but carries \
                        a `warnings` entry, plus the SAME sentence inside the rendered text as a \
                        'NO SPEC ANCHORED' line. THE THREE CASE-0 CODES, exhaustively — they say \
                        WHICH of three very different situations you are in, so read the code and \
                        not just the null spec_id: 'spec_none_on_location' when the location IS a \
                        workspace and it simply anchors no active Spec (legitimate — a project's \
                        first /nspec starts exactly there); 'spec_location_unresolved' when no \
                        workspace matched at all — that one means you are calling from the wrong \
                        folder; 'spec_run_unbound' when a run_id was passed and RESOLVED but the \
                        RUN drives no Spec — the location is not at fault and 'pass a cwd' would \
                        name the wrong fix, an authoring run simply has no Spec until it creates \
                        one. SEVERAL is refused with \
                        invalid_argument, data.reason='spec_ambiguous_for_workspace' and the \
                        candidates listed — nyx never picks the most recent. Terminal \
                        (completed/cancelled) and archived Specs are never candidates.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "role": str_prop("spec | dev | coder | auditor | doc | prototype — which manual to render"),
                "run_id": str_prop("the run you were spawned for — the ONE anchor a sub-agent needs: it carries the Spec the run drives and, before there is one, the workspace it opened against. Pass the run_id the spawn action was ticking; an unknown one is refused rather than rendered empty"),
                // CONDITIONAL: `role` is the only REQUIRED argument here, so every project anchor
                // this call could read is optional. `agent_context(role=…, spec_id="#3")` alone
                // resolves no project and used to answer the literal `unknown spec 3` this grammar
                // exists to retire (finding 01M0ZKWXS353TABS0WCT3XXB81).
                "spec_id": conditional_spec_ref_prop(
                    "the Spec to render the context for (preferred)",
                    "you also pass one of run_id / workspace_id / cwd / project_id — all optional \
                     here, since `role` is this call's only required argument",
                ),
                "workspace_id": str_prop("optional: resolve the Spec via its anchored dev workspace when spec_id is absent — exactly one active Spec must be anchored there (several is refused with the candidates listed, none resolves nothing)"),
                "cwd": str_prop("optional: resolve the Spec (or, for role=spec authoring, the project) via the workspace whose path matches this cwd when spec_id/workspace_id are absent. The deepest matching workspace root wins; then the same one-active-Spec rule as workspace_id applies"),
                "project_id": str_prop("optional: for role=spec authoring with no Spec yet, the project to author in (else resolved from cwd/workspace_id). When a Spec DOES resolve it is only checked, never applied — a Spec's project is a fact of the Spec, and naming another one is refused rather than ignored; read the project_id the answer returns instead"),
                "subject": str_prop("optional: the spawn action's own `subject`. When it names a Spec section (problem | solution | implementation_decisions | testing_decisions | user_stories | out_of_scope), ONLY that section is reproduced — pass back the subject you were spawned with, so the pass reads the commitment it is about and not the other five"),
            },
            "required": ["role"],
        },
    }));
    // Append the v2.4 Git DIFF read model tool: the line-level diff of ONE repository in a
    // workspace, for a review surface, in living or pinned mode (gix + git CLI fallback).
    descriptors.push(json!({
        "name": REPO_DIFF_TOOL,
        "description": "Get the line-level Git diff of ONE repository within a workspace, \
                        for local review. Choose a `kind`: working_tree (everything \
                        uncommitted), staged, unstaged, untracked, commit (needs `rev`), \
                        range or branches (need `base` and `target` rev-specs). Returns \
                        per-file hunks with old/new sides and blob ids — one repo at a \
                        time, so several repos' diffs never mix. Optional `mode` \
                        (living|pinned) records whether the view follows the current state \
                        or is frozen on the resolved SHA. gix-powered with a git CLI \
                        fallback. Read-only; nothing is stored.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace the repository belongs to (from list_workspaces(project_id=…))"),
                "repository_id": str_prop("the repository to diff (from workspace_git(workspace_id=…))"),
                "kind": str_prop("the diff surface: working_tree | staged | unstaged | untracked | commit | range | branches"),
                "rev": str_prop("for kind=commit: the commit rev-spec to diff against its parent"),
                "base": str_prop("for kind=range|branches: the BASE rev-spec (branch or SHA)"),
                "target": str_prop("for kind=range|branches: the TARGET rev-spec (branch or SHA)"),
                "mode": str_prop("optional: living (follows current state) | pinned (frozen on the resolved SHA)"),
            },
            "required": ["workspace_id", "repository_id", "kind"],
        },
    }));
    // Append the v2.4 DERIVED GIT VIEWS tool: the per-repo dirty/branch/ahead-behind/summary
    // state, on demand, in a stable shape (no line-level diff — that is repo_diff).
    descriptors.push(json!({
        "name": REPO_GIT_VIEW_TOOL,
        "description": "Get the overall Git STATE of ONE repository within a workspace, on \
                        demand: whether the work tree is dirty (with the staged / modified / \
                        untracked file lists), the current branch and its base/target, how \
                        many commits it is ahead/behind that base, and a compact change \
                        summary. This is the repo's state at a glance — NOT the line-level \
                        diff (use repo_diff for hunks). Pass an optional `base` (a local \
                        branch or SHA — there is no remote) to get ahead/behind, and an \
                        optional `target` to compare a specific endpoint instead of the \
                        current branch. Read-only; nothing is stored.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace the repository belongs to (from list_workspaces(project_id=…))"),
                "repository_id": str_prop("the repository whose Git state to derive (from workspace_git(workspace_id=…))"),
                "base": str_prop("optional review BASE endpoint (a local branch or SHA) to compute ahead/behind against"),
                "target": str_prop("optional review TARGET endpoint (branch or SHA); defaults to the current branch"),
            },
            "required": ["workspace_id", "repository_id"],
        },
    }));
    // Append the v2.4 REAL-TIME REFRESH tools (Q-015): begin/stop a debounced fs-watch of a
    // repo so a change pushes `repos://changed` to both surfaces without polling.
    descriptors.push(json!({
        "name": REPO_WATCH_TOOL,
        "description": "Start REAL-TIME watching of ONE repository within a workspace, so \
                        that when the repo changes (a file edit, a commit, a checkout) nyx \
                        recomputes its Git view and pushes the update live — no polling. Use \
                        this when opening a review so the diff/branch/dirty state stays fresh; \
                        pair with repo_unwatch when done. Idempotent per repository. Pass an \
                        optional `base`/`target` (a local branch or SHA — no remote) so \
                        ahead/behind stays correct across refreshes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace the repository belongs to (from list_workspaces(project_id=…))"),
                "repository_id": str_prop("the repository to watch in real time (from workspace_git(workspace_id=…))"),
                "base": str_prop("optional review BASE endpoint (a local branch or SHA) kept for ahead/behind"),
                "target": str_prop("optional review TARGET endpoint (branch or SHA); defaults to the current branch"),
            },
            "required": ["workspace_id", "repository_id"],
        },
    }));
    descriptors.push(json!({
        "name": REPO_UNWATCH_TOOL,
        "description": "Stop REAL-TIME watching of a repository previously registered with \
                        repo_watch. A repository that was not being watched is a no-op.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace the repository belongs to"),
                "repository_id": str_prop("the repository to stop watching"),
            },
            "required": ["workspace_id", "repository_id"],
        },
    }));
    // Append the v2.4 review COMMENT tools: LIST the auto-threaded feed, and POST an
    // agent comment/reply (NO resolve tool — the agent never resolves, only the human does).
    // `int_prop` (with `minimum: 0`, defined above) stays in scope here and is reused for the
    // 1-based `line_start`/`line_end` so their schema matches the file's other integer args.
    descriptors.push(json!({
        "name": REVIEW_COMMENT_LIST_TOOL,
        "description": "List the review COMMENTS of ONE repository within a workspace — the whole \
                        auto-threaded feed: thread roots and their replies, anchored comments \
                        (on a file/side/line or range) and global ones, each with its author \
                        (human or agent), status (open/resolved) and `outdated` flag (an anchor \
                        the code moved out from under). Use this to read the current review \
                        conversation before replying. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace the repository belongs to (from list_workspaces(project_id=…))"),
                "repository_id": str_prop("the repository whose review comments to list (from workspace_git(workspace_id=…))"),
                // HONOURED SINCE v2.4, NAMED ONLY NOW: the call has always narrowed the feed by
                // `spec_id` and the schema said nothing, so no caller reading `tools/list` could
                // use it — the same defect `run_start`'s `project_id` was just repaired for
                // (`01M1008NHM9RRA1ZAS4V7K1PM4`). `workspace_id` is REQUIRED here and carries a
                // project, so the four forms always resolve.
                "spec_id": spec_ref_prop("optional: keep only the comments bound to this Spec (omit for the repository's whole feed)"),
            },
            "required": ["workspace_id", "repository_id"],
        },
    }));
    descriptors.push(json!({
        "name": REVIEW_COMMENT_POST_TOOL,
        "description": "Post a review COMMENT as the AGENT on ONE repository within a workspace. \
                        Leave a GLOBAL comment (no anchor), an ANCHORED comment (pass `file_path` \
                        + `side` old|new + `line_start` [+ `line_end` for a range], optionally \
                        `blob`/`snippet`/`context` so it re-maps across commits), or a REPLY in a \
                        thread (pass `parent_id`). The author is always the agent. You may reply, \
                        and you may resolve YOUR OWN (agent) comments with review_comment_resolve; \
                        a HUMAN comment stays human-only (you cannot resolve it). Returns the \
                        created comment.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace the repository belongs to (from list_workspaces(project_id=…))"),
                "repository_id": str_prop("the repository to comment on (from workspace_git(workspace_id=…))"),
                "body": str_prop("the comment text"),
                "spec_id": spec_ref_prop("optional spec the review is bound to (omit for a generic review)"),
                "parent_id": str_prop("optional: the id of the comment to REPLY to (from review_comment_list) — omit for a new thread"),
                "file_path": str_prop("optional: for an ANCHORED comment, the changed-file path to hang it on (omit for a global comment)"),
                "side": str_prop("optional: the diff side the anchor is on: old | new"),
                "line_start": int_prop("optional: the 1-based anchored line (or range start) on `side`"),
                "line_end": int_prop("optional: the 1-based range end (defaults to line_start)"),
                "blob": str_prop("optional: the blob SHA the anchor is captured on (helps re-mapping)"),
                "snippet": str_prop("optional: the anchored line text (helps re-mapping)"),
                "context": str_prop("optional: a few surrounding lines (helps disambiguate re-mapping)"),
            },
            "required": ["workspace_id", "repository_id", "body"],
        },
    }));
    descriptors.push(json!({
        "name": REVIEW_COMMENT_RESOLVE_TOOL,
        "description": "Resolve one of YOUR OWN review comments (author = agent). A comment is \
                        resolved by its author: you close the agent comments you left; a HUMAN \
                        comment is human-only and refuses this call (reply to it instead — the \
                        human validates and closes it). Returns the resolved comment.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": str_prop("the id of YOUR (agent) comment to resolve (from review_comment_list)"),
            },
            "required": ["id"],
        },
    }));
    // Append the v2.4 review TASK tools (finding = task). LIST the promoted tasks, and
    // STRUCTURE a proposed promotion (the agent's grill step — and its final step; a structured
    // finding is coder-ready, the grill has no `validated` state, review follow-up #31).
    descriptors.push(json!({
        "name": REVIEW_TASK_LIST_TOOL,
        "description": "List the review TASKS (promoted coder tasks) of ONE repository within a \
                        workspace. A task IS a coder task promoted from a review comment \
                        (finding = task): each carries the source comment it came from \
                        (`from_comment_id`), a `title`, an optional `severity`, and a \
                        `grill_state` — `proposed` (you, the agent, captured the finding from the \
                        human's retour but have not structured it yet) or `structured` (you gave it \
                        a clean title/details — it is now ready for the coder). Use this to find the \
                        findings waiting for you to structure. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace the repository belongs to (from list_workspaces(project_id=…))"),
                "repository_id": str_prop("the repository whose tasks to list (from workspace_git(workspace_id=…))"),
                "spec_id": spec_ref_prop("the Spec whose findings to list (findings are Spec-scoped; omit for none)"),
            },
            "required": ["workspace_id", "repository_id"],
        },
    }));
    descriptors.push(json!({
        "name": REVIEW_TASK_STRUCTURE_TOOL,
        "description": "REWRITE a review finding's text. Two uses. (1) STRUCTURE a promoted retour — \
                        the human lâche brut, you captured it as a `proposed` finding, now turn it \
                        into a clean coder task: a sharp `title`, actionable `details`, an optional \
                        `severity` (info|low|medium|high|critical). That moves it to `structured`, \
                        READY for the coder (the grill has two states, no separate validate step). \
                        (2) REPAIR a finding whose own text came out wrong — a malformed tool call \
                        that left markup in the description, a title that says the wrong thing. This \
                        is the ONLY way to correct a finding mid-build: task_delete is draft-only, \
                        and skipping cancels the WORK when only the wording is wrong. It edits the \
                        text and nothing else — severity aside, the phase, round and status are \
                        untouched, and a finding you raised directly is not filed into the retour \
                        grill by being corrected. Returns the updated task.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": str_prop("the finding to rewrite (from review_task_list, or the id task_add returned)"),
                "title": str_prop("the finding's title — pass it again unchanged if only the details are wrong"),
                "details": str_prop("optional: the finding's body, REPLACED wholesale (omit to clear it)"),
                "severity": str_prop("optional: info | low | medium | high | critical (omit to leave it as it is)"),
            },
            "required": ["id", "title"],
        },
    }));
    // Append the v2.5.11 RETOUR-CHAT tool: relay the human's chat feedback into the review as a
    // HUMAN, GLOBAL comment + a `proposed` finding (the viewer stays a valid channel in parallel).
    descriptors.push(json!({
        "name": REVIEW_SUBMIT_FEEDBACK_TOOL,
        "description": "Submit the HUMAN's chat feedback as a review retour. When the user gives \
                        you feedback IN THE CHAT while a Spec is in review, relay it here: it \
                        creates a HUMAN, GLOBAL (non-anchored) review comment — the human \
                        instructed you directly, so no diff anchor is needed — and immediately \
                        grills it into a `proposed` review finding (a `tasks kind=review` you then \
                        STRUCTURE, and the human VALIDATES). This gives a chat retour the SAME path \
                        to review as the diff viewer, which stays a valid channel in parallel. The \
                        user's return always means 'work this feedback' — capture it here rather \
                        than brushing it off. Returns the created comment and finding.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("the workspace the repository belongs to (from list_workspaces(project_id=…))"),
                "repository_id": str_prop("the repository under review (from workspace_git(workspace_id=…))"),
                "spec_id": spec_ref_prop("the Spec under review — the finding lives on it (required)"),
                "body": str_prop("the human's feedback, verbatim from the chat"),
                "title": str_prop("optional: a short title for the finding (defaults to the feedback body)"),
                "severity": str_prop("optional: info | low | medium | high | critical"),
                "phase": json!({ "type": "integer", "minimum": 1, "description": "optional: the phase NUMBER (1-based) this finding belongs to. Overrides the phase nyx would infer (the phase under review). Use it to attribute a finding to a specific phase — e.g. in an autopilot final review where several phases were delivered." }),
            },
            "required": ["workspace_id", "repository_id", "spec_id", "body"],
        },
    }));
    // Append the v2.6 IDEAS tools: the MAINTAINED backlog (capture + scored search + close/
    // drop/merge maintenance + Spec links). Every mutating tool delegates to the SHARED
    // nyx-core idea authority; idea_search is the scored top-N surfacing (FTS5 + fuzzy) that
    // replaces a flat dump. Kept OUT of V1_TOOLS (a new surface, not the frozen v1 contract).
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_ADD_TOOL,
        "description": "Capture a project IDEA (a follow-up / improvement raised during dev, \
                        review, or spec work) into nyx's maintained backlog — the durable place \
                        these go instead of being lost. Give a title and an optional body; group \
                        it with an optional theme; optionally link it to one or more Specs (spec_ids, \
                        linked as 'addresses'). Before adding, prefer idea_search to avoid a \
                        duplicate. Returns the created idea and its linked Specs. \
                        \n\nIDEA LIFECYCLE (ideas.status): `open` (fresh backlog) and `planned` (a \
                        linked Spec is in dev) are LIVE; `implemented`, `closed` and `dropped` are \
                        TERMINAL. `planned`/`implemented` are SPEC-DRIVEN (AUTOMATIC): they follow \
                        the state of a Spec linked as kind='addresses' — the Spec going in_progress \
                        moves the idea open→planned, the Spec completing moves it →implemented. \
                        `closed` is the MANUAL fermeture-par-constat (idea_close, with a motif); \
                        `dropped` is a manual abandon. A terminal idea does not move again except \
                        via idea_reopen. Linking (here or idea_link) an idea to an already-active \
                        Spec reconciles its status immediately (in_progress⇒planned, \
                        completed⇒implemented).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project to capture the idea in (from list_projects)"),
                "title": str_prop("the idea's short title"),
                "body": str_prop("optional longer description (markdown)"),
                "theme": str_prop("optional grouping theme (e.g. 'auth', 'perf')"),
                "source": str_prop("optional capture source: session (default) | spec | manual"),
                // THE ONE PLURAL FIELD OF THE SURFACE, and it announces the SAME grammar as the
                // singular ones: `project_id` is REQUIRED here, so every entry has a project to be
                // counted in and a `#N` resolves exactly as it does on `task_add` (finding
                // 01M0ZMGJG9VXD4GCY6Y6TN3RYS). The description is built from `spec_ref_desc`, not
                // paraphrased, so the plural cannot drift from the singular.
                "spec_ids": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": spec_ref_desc("optional Spec references to link the idea to (each as kind='addresses')"),
                }),
            },
            "required": ["project_id", "title"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_SEARCH_TOOL,
        "description": "Search a project's idea backlog and get a SCORED top-N — NOT a flat dump. \
                        Combines full-text (FTS5) with a fuzzy/trigram pass so typos and spelling \
                        variants still surface. Use it at the start of spec/dev work to find ideas \
                        on a topic (to avoid re-creating a Spec for a need already tracked, and to \
                        link the relevant ones). Each result carries the idea's title, a match \
                        extract, a relevance score, and its linked Specs. Narrow with theme/status; \
                        cap with limit. Search spans EVERY status by default (a resolved idea is \
                        still findable) — pass status to restrict. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project whose ideas to search (from list_projects)"),
                "query": str_prop("the search text (words; typos tolerated)"),
                "theme": str_prop("optional: restrict to this theme"),
                "status": str_prop("optional: restrict to one status (open | planned | implemented | closed | dropped)"),
                "limit": int_prop("optional: max results to return (default 10, capped at 50)"),
            },
            "required": ["project_id", "query"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_GET_TOOL,
        "description": "Read ONE idea by id, with its linked Specs (each with the link kind and \
                        the Spec's title/status/number). Unknown id → invalid_id. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": str_prop("the idea id (from idea_search / idea_list)") },
            "required": ["id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_IMPLEMENT_TOOL,
        "description": "Mark an idea IMPLEMENTED (resolved) — the terminal a completed linked Spec \
                        also drives automatically; call this to record the resolution by hand. No \
                        motif (use idea_close for a manual fermeture-with-reason). REFUSED \
                        (invalid_state) if the idea is already terminal \
                        (implemented/closed/dropped) — use idea_reopen first. Unknown id → \
                        invalid_id.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": str_prop("the idea id to mark implemented") },
            "required": ["id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_CLOSE_TOOL,
        "description": "MANUALLY close an idea as `closed` with a motif (the 'fermeture par \
                        constat' — you found it was already covered / no longer relevant). This is \
                        the MANUAL closure, a status DISTINCT from `implemented` (which is the \
                        Spec-driven, automatic resolution). Records the reason. REFUSED \
                        (invalid_state) if the idea is already terminal — the existing motif is \
                        never overwritten; reopen it first if you must re-close it. Unknown id → \
                        invalid_id.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": str_prop("the idea id to close"),
                "reason": str_prop("why it is closed (the motif)"),
            },
            "required": ["id", "reason"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_DROP_TOOL,
        "description": "ABANDON an idea (mark it dropped) — it is no longer worth pursuing. \
                        Terminal state. REFUSED (invalid_state) if the idea is already terminal \
                        (implemented/closed/dropped). Unknown id → invalid_id.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": str_prop("the idea id to drop") },
            "required": ["id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_REOPEN_TOOL,
        "description": "REOPEN a terminal idea (implemented / closed / dropped) back to `open`, \
                        clearing its fermeture trail (closed_at / closed_reason). This is the ONE \
                        explicit way out of a terminal state — implement/close/drop deliberately \
                        refuse to resurrect a terminal idea, so use idea_reopen when a dropped or \
                        resolved idea is relevant again. A live idea is returned unchanged. Unknown \
                        id → invalid_id.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": str_prop("the idea id to reopen") },
            "required": ["id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_MERGE_TOOL,
        "description": "MERGE duplicate ideas into one: the target absorbs each source's points \
                        (bodies appended) and Spec links, and each source is retired (dropped) with \
                        a 'merged into' trail. Use it to de-duplicate the backlog. Pass the source \
                        ids and the target id. Unknown target → invalid_id. Returns the updated \
                        target and its linked Specs.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "source_ids": json!({ "type": "array", "items": { "type": "string" }, "description": "the idea ids to fold into the target (non-empty)" }),
                "target_id": str_prop("the idea that absorbs the sources"),
            },
            "required": ["source_ids", "target_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_LINK_TOOL,
        "description": "Link an idea to a Spec with a kind: 'addresses' (the Spec handles the idea — \
                        the resolution/promotion link that DRIVES the idea's lifecycle) or 'related' \
                        (context only, drives nothing). An 'addresses' link reconciles the idea's \
                        status to the Spec's CURRENT state right away: a Spec already in_progress \
                        moves the idea open→planned, a Spec already completed →implemented (+motif). \
                        Re-linking the same pair updates the kind. Both ids are validated (unknown → \
                        invalid_id). Returns the link.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "idea_id": str_prop("the idea to link"),
                "spec_id": anchorless_spec_ref_prop("the Spec to link it to"),
                "kind": str_prop("addresses (default) | related"),
            },
            "required": ["idea_id", "spec_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_UNLINK_TOOL,
        "description": "Remove an idée↔Spec link. Idempotent (unlinking a pair that is not linked \
                        is a no-op that reports unlinked:false).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "idea_id": str_prop("the idea"),
                "spec_id": anchorless_spec_ref_prop("the Spec to unlink from it"),
            },
            "required": ["idea_id", "spec_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_LIST_TOOL,
        "description": "List a project's ideas as a BOUNDED, paginated dump — the fallback when you \
                        want to browse rather than search. Prefer idea_search for finding ideas on a \
                        topic. By DEFAULT lists only the ACTIVE backlog (open + planned) so the \
                        actionable ideas are not buried under the resolved/abandoned tail; pass \
                        include_terminal:true (or status:\"all\") to include \
                        implemented/closed/dropped, or a concrete status to narrow to exactly one. \
                        Page with limit (capped at 200) and offset. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project whose ideas to list (from list_projects)"),
                "status": str_prop("optional: one status (open | planned | implemented | closed | dropped), or \"active\" (open+planned, the default) / \"all\""),
                "include_terminal": bool_prop("optional: include terminal ideas (implemented/closed/dropped) — default false (active only)"),
                "theme": str_prop("optional: filter by theme"),
                "limit": int_prop("optional: page size (default 50, capped at 200)"),
                "offset": int_prop("optional: page offset (default 0)"),
            },
            "required": ["project_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_idea::IDEA_THEMES_TOOL,
        "description": "Group a project's idea backlog BY THEME with counts — the maintenance view \
                        that keeps the backlog navigable. Returns one entry per theme (ungrouped \
                        ideas fold into a single theme=null group), each with a total and a \
                        per-status breakdown (open / planned / implemented / closed / dropped), \
                        biggest group first. By DEFAULT the counts reflect only the ACTIVE backlog \
                        (open + planned) — a theme's total matches its actionable ideas and a \
                        wholly-terminal theme drops out; pass include_terminal:true for the full \
                        counts across every status. Use it to see the shape of the backlog and \
                        where to dedup/merge. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project whose ideas to group by theme (from list_projects)"),
                "include_terminal": bool_prop("optional: count terminal ideas (implemented/closed/dropped) too — default false (active only)"),
            },
            "required": ["project_id"],
        },
    }));
    // Append the v2.6 ANNEXE tools: attach a doc/artefact/url/prototype to a Spec with INTERNAL
    // storage + DEDUP by content_hash (the same artefact attached twice stores one copy). Every
    // tool delegates to the SHARED nyx-core annexe authority. Kept OUT of V1_TOOLS (a new surface).
    descriptors.push(json!({
        "name": crate::mcp_tools_annexe::ANNEXE_ADD_TOOL,
        "description": "Attach an ANNEXE (a doc, artefact, URL, or prototype) to a Spec — stored \
                        INTERNALLY in nyx (not a fragile link to a repo file that will move). The \
                        content is content-addressed and DEDUPED: attaching the same content twice \
                        (to this Spec or another) stores ONE physical copy. For a 'url' annexe the \
                        content IS the URL string — that is how an EXTERNAL LINK (a ticket, a PR, a \
                        doc) is attached to a Spec, and it survives a fork. A 'url' carries extra \
                        rules: the content must be an ABSOLUTE http:// or https:// URL (anything else \
                        is refused), and the SAME url cannot be attached twice to one Spec (the \
                        second add is refused — doc/artefact/prototype may still be duplicated). \
                        A url is stored in CANONICAL form: surrounding blanks trimmed, scheme and \
                        host lowered, the path's trailing slash dropped — so ' HTTPS://X/a/ ' is \
                        stored, and deduplicated, as 'https://x/a' (the path, the query and the \
                        fragment keep their case, and 'www.x' stays a different host from 'x'). \
                        Whitespace or control characters INSIDE it are still refused, with their \
                        own message, and a url past 2048 characters is refused too. The other \
                        kinds are stored byte-for-byte and are NOT length-bounded. Returns the \
                        created annexe.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "spec_id": anchorless_spec_ref_prop("the Spec to attach the annexe to"),
                "kind": str_prop("annexe type: doc | artefact | url | prototype"),
                "title": str_prop("the annexe's title (the LABEL of a link). REQUIRED for kind=doc|artefact|prototype, where it is refused ABSENT or EMPTY. For kind=url it is OPTIONAL and the three ways to give no label are the SAME case: absent, empty ('') and blank ('   ') all store the URL itself as the title"),
                "content": str_prop("the annexe's content (the document/artefact text, or the absolute http(s) URL string for kind=url) — stored internally"),
            },
            // `title` is CONDITIONALLY required, and the condition is expressed HERE rather than in
            // the property's prose alone: the handler (`mcp_tools_annexe::annexe_add`) only relaxes
            // it for `kind='url'` (an unlabelled link is stored with the URL as its title), and
            // still calls `require_str` for doc / artefact / prototype. A flat
            // `required: [spec_id, kind, content]` would advertise a shape three of the four kinds
            // are refused for — the published contract must not describe writes the tool rejects.
            "required": ["spec_id", "kind", "content"],
            "allOf": [{
                "if": {
                    "properties": { "kind": { "enum": ["doc", "artefact", "prototype"] } },
                    "required": ["kind"],
                },
                "then": { "required": ["title"] },
            }],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_annexe::ANNEXE_LIST_TOOL,
        "description": "List a Spec's annexes, newest-first — each with its kind, title, \
                        content_hash and byte size (NOT the content; use annexe_get for the bytes). \
                        Pass kind to narrow the read to ONE facet: kind='url' lists the Spec's \
                        EXTERNAL LINKS and nothing else (there is no separate link tool — a link IS \
                        a url annexe). Omitting kind lists everything. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "spec_id": anchorless_spec_ref_prop("the Spec whose annexes to list"),
                "kind": str_prop("optional: keep only this annexe type (doc | artefact | url | prototype) — omit for all of them"),
            },
            "required": ["spec_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_annexe::ANNEXE_GET_TOOL,
        "description": "Read ONE annexe by id WITH its stored content (and byte size). Unknown id → \
                        invalid_id. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": str_prop("the annexe id (from annexe_list)") },
            "required": ["id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_annexe::ANNEXE_REMOVE_TOOL,
        "description": "Detach an annexe from its Spec by id. Idempotent (removing an unknown id is \
                        a no-op that reports removed:false). The internal content copy is garbage-\
                        collected once no annexe references it any more.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": str_prop("the annexe id to remove") },
            "required": ["id"],
        },
    }));
    // The v2.8 GRAPH-AUTHORING surface: the agent builds an orchestration behavior tree from raw
    // intent (the human validates it via the Phase-6 visualiser). Each mutation is guarded by a
    // tree-coherence check that REFUSES an incoherent graph (cycle / dangling parent / a leaf with
    // children / a second root). Advertised alongside, kept OUT of V1_TOOLS (a new v2.8 surface).
    descriptors.push(json!({
        "name": crate::mcp_authoring::WORKFLOW_CREATE_TOOL,
        "description": "Create a NEW (empty) orchestration workflow draft. Add its nodes with node_add \
                        afterwards. `project_id` binds it to a project (which flow that project uses). \
                        You may omit project_id ONLY for a global scope='custom' template: an unowned \
                        ndev/nspec/ndoc workflow is a SEEDED built-in and is refused (fork one instead).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": str_prop("human name of the workflow"),
                "scope": str_prop("optional: ndev | nspec | ndoc | custom | coder | prototyper (default custom). coder/prototyper are SUB-WORKFLOW scopes — a sub_workflow node's child target, never a top-level run"),
                "project_id": str_prop("the project that owns/uses this workflow (from list_projects); may be omitted ONLY for a scope='custom' global template"),
            },
            "required": ["name"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::WORKFLOW_FORK_TOOL,
        "description": "DEEP-COPY an existing workflow's whole node tree into a new PROJECT-OWNED \
                        workflow (forked_from = the source). The customisation path: fork a built-in \
                        default, then edit the copy — the source is never mutated. project_id is \
                        REQUIRED (a fork is a project's customisation); for a scoped family \
                        (ndev/nspec/ndoc) a project holds ONE, so a second such fork is refused.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workflow_id": str_prop("the SOURCE workflow to fork"),
                "name": str_prop("optional: name of the fork (defaults to '<source> (fork)')"),
                "project_id": str_prop("the project that owns the fork (from list_projects)"),
            },
            "required": ["workflow_id", "project_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::WORKFLOW_ASSIGN_TOOL,
        "description": "(Re)BIND a workflow to a project — the project↔workflow link (which workflow \
                        the project uses). Sets the workflow's owning project.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workflow_id": str_prop("the workflow to bind"),
                "project_id": str_prop("the project to bind it to (from list_projects)"),
            },
            "required": ["workflow_id", "project_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::SLOT_BIND_TOOL,
        "description": "BIND a graph's command SLOT to the managed commands THIS project answers it \
                        with. A gate_check in a SHARED graph (the seeded built-ins) declares WHAT it \
                        needs verified — `config.slot`, e.g. 'build_check' — never which command runs \
                        it, because a command instance id is project data. This is the other half, and \
                        it REPLACES forking a built-in to arm its gate: the built-in is left alone, so \
                        it keeps receiving nyx's graph fixes, while a fork would freeze it at today's \
                        revision forever. Register the check first (add_command → list_commands for \
                        the instance_id). The binding is re-read at every tick, so a run refused on an \
                        unbound slot resumes on the next nyx_next. Binding again REPLACES; an empty \
                        command_ids UNBINDS (the gate goes back to refusing). Bind a command that \
                        EXITS: a gate's verdict IS its exit code, so a watcher or a dev server is \
                        REFUSED here — bind `bun test`, never `bun test --watch`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("the project this binding is for (from list_projects)"),
                "slot": str_prop("the slot NAME the gate_check node declares — run_start's gate warning and the tick refusal both carry it (the warning under `slot`)"),
                "command_ids": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "the managed-command INSTANCE ids to run for that slot, in order (the instance_id from list_commands, not a template id). Empty = unbind",
                }),
            },
            "required": ["project_id", "slot", "command_ids"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::NODE_ADD_TOOL,
        "description": "Add a TYPED node to a workflow's tree. Types: composites sequence | loop | \
                        selector, leaves spawn_subagent | gate_check | judge | transition | wait | \
                        note | ask_human | produce. A parent-less node in a still-rootless workflow \
                        becomes its root; otherwise pass parent_id (a composite). Refused if it would \
                        make the tree incoherent (a leaf can't have children, no second root) — or if \
                        a RUN is currently walking this workflow (let it finish, or run_delete it, \
                        then edit the graph).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workflow_id": str_prop("the workflow to add the node to"),
                "type": str_prop("the node type (sequence/loop/selector/spawn_subagent/gate_check/judge/transition/wait/note/ask_human/produce)"),
                "label": str_prop("optional: human label for the node"),
                "config": json!({ "type": "object", "description": "the node's typed config JSON (unknown fields are REFUSED). Per type: spawn_subagent→{agent_id, inline?}; gate_check→{command_ids | slot, blocking?, expect?} (a gate names its OWN command instance ids, or declares a `slot` each project binds with slot_bind — the shape a SHARED graph uses; never both. expect declares WHICH OUTCOME satisfies the gate: \"green\" = the check must PASS, the default when the field is absent; \"red\" = the check must FAIL, which is how a REPRODUCTION is declared — a check that passes there has reproduced nothing and does not advance. A fault that left no exit status is `indeterminate` and satisfies NEITHER); transition→{target_state}; judge→{prompt, output_schema?, output_key?, append?}; ask_human→{prompt, output_key?, append?}; produce→{output_key?, append?}; loop→{until, max_iterations?}; selector→{branches:[{cond, child?}]}; note→{text}; sequence/wait→{}. output_key routes the result on the blackboard; append pushes onto an array instead of overwriting; inline runs the effector in-session (for an interactive one)" }),
                "parent_id": str_prop("optional: the PARENT composite node (omit for the workflow's root node)"),
                "order_index": json!({ "type": "integer", "description": "optional: sibling order under the parent (default 0)" }),
            },
            "required": ["workflow_id", "type"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::NODE_UPDATE_TOOL,
        "description": "Edit a node's label and/or config (its type is immutable). At least one of \
                        label / config is required. The one node edit ALLOWED while a run walks the \
                        workflow (it moves no node id): the answer then carries a `warnings` entry \
                        saying that run will execute the new config from its next tick.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "node_id": str_prop("the node to edit"),
                "label": str_prop("optional: new label"),
                "config": json!({ "type": "object", "description": "optional: the node's new typed config JSON object" }),
            },
            "required": ["node_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::NODE_REMOVE_TOOL,
        "description": "Delete a node AND its whole subtree (the children cascade). Removing the root \
                        leaves the workflow rootless (re-root it with a new parent-less node_add). \
                        Refused while a RUN is walking this workflow.",
        "inputSchema": {
            "type": "object",
            "properties": { "node_id": str_prop("the node to remove") },
            "required": ["node_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::NODE_REORDER_TOOL,
        "description": "Move a node: set its sibling order_index and, optionally, REPARENT it under a \
                        new parent_id (kept otherwise). A reparent that would create a cycle (a node \
                        under its own descendant) is refused, and so is any move while a RUN is \
                        walking this workflow.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "node_id": str_prop("the node to move"),
                "order_index": json!({ "type": "integer", "description": "the new sibling order under its parent" }),
                "parent_id": str_prop("optional: reparent the node under this composite (kept unchanged if omitted)"),
            },
            "required": ["node_id", "order_index"],
        },
    }));
    // The v2.8 authoring READ surface (review S1) + delete (review S4): discover workflows, read a
    // graph's node ids, list a workflow's runs, and delete a project-owned workflow. Kept OUT of
    // V1_TOOLS (part of the v2.8 authoring surface, counted via AUTHORING_TOOLS).
    descriptors.push(json!({
        "name": crate::mcp_authoring::WORKFLOW_LIST_TOOL,
        "description": "List orchestration workflows. WITH project_id: that project's OWN workflows \
                        plus the built-in defaults it falls back to; WITHOUT: the built-in defaults. \
                        Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("optional: also include this project's own workflows"),
            },
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::WORKFLOW_GET_TOOL,
        "description": "Get a workflow's metadata AND its whole node tree — the read that gives you \
                        the node ids that node_update / node_remove / node_reorder require. \
                        Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": { "workflow_id": str_prop("the workflow to read") },
            "required": ["workflow_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::RUN_LIST_TOOL,
        "description": "List a workflow's runs, newest first, each with its cursor, blackboard and \
                        status. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": { "workflow_id": str_prop("the workflow whose runs to list") },
            "required": ["workflow_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::WORKFLOW_DELETE_TOOL,
        "description": "Delete a PROJECT-OWNED workflow and its nodes + runs (cascade) — for cleaning \
                        up drafts and failed forks. A built-in default is REFUSED: fork it instead of \
                        editing or deleting the shared default. So is a workflow a RUN is still \
                        walking — the cascade would take that run down with it.",
        "inputSchema": {
            "type": "object",
            "properties": { "workflow_id": str_prop("the project-owned workflow to delete") },
            "required": ["workflow_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_authoring::RUN_DELETE_TOOL,
        "description": "Delete ONE workflow run by id — to clean up a run opened by mistake, including \
                        a run on a built-in flow (which workflow_delete cannot reach). A run is a \
                        leaf, so nothing else is affected.",
        "inputSchema": {
            "type": "object",
            "properties": { "run_id": str_prop("the run to delete (from run_list)") },
            "required": ["run_id"],
        },
    }));
    // The v2.8 ENGINE run-lifecycle ENTRY. Since G6 the nyx COMMAND HOOK calls it, so a /nspec /ndev
    // /ndoc session is handed its run_id with the manual and ticks nyx_next straight away; it stays
    // advertised because that hook reaches the sidecar through THIS surface (an un-advertised tool is
    // undispatchable — N17) and because a session that holds no run_id still needs a way to open one.
    descriptors.push(json!({
        "name": crate::mcp_engine::RUN_START_TOOL,
        "description": "OPEN an orchestration RUN and get its run_id. IF YOU WERE STARTED BY A NYX \
                        SLASH COMMAND (/nspec, /ndev, /ndoc) YOU ALREADY HAVE ONE: nyx's command hook \
                        opened the run and injected its run_id with your dispatcher manual — go \
                        straight to nyx_next. Call this only when you hold no run_id, then pass the \
                        returned one to nyx_next in a loop. IDEMPOTENT: at most ONE live run per \
                        (workflow, Spec), so a call made when a run is already live RESUMES it rather \
                        than opening a second. nyx resolves WHICH workflow to run — an explicit \
                        workflow_id, else the resolved project's active workflow of `scope` (default \
                        'ndev') — and binds the run to the resolved Spec (from spec_id, workspace_id, \
                        or cwd) when there is one. Returns { run_id, workflow_id, spec_id, scope, \
                        outcome, warnings }. `outcome` says WHICH of three things happened — \
                        'created' (a fresh run at the top of the flow), 'resumed' (the pair's live \
                        run handed back, so the next nyx_next answers from where IT stopped, not \
                        from the top) or 'replaced' (the live run could never tick again and was \
                        dropped for a fresh one): read it rather than assuming a run_id is a new \
                        run. \
                        RELAY every warnings[].message to the human — each names \
                        something they must act on (a run RESUMED mid-flow — code 'run_resumed', \
                        which names the node it is parked on and how long it has sat there; a run \
                        DROPPED because its graph changed, a gate \
                        that verifies nothing, a run started bound to no Spec, or a cwd that \
                        matched NO workspace — code 'run_location_unresolved', the run then opens \
                        with no workspace anchor at all and its effectors resolve no repos, which \
                        usually means the caller is running from the wrong folder). \
                        RESOLVING BY LOCATION (workspace_id / cwd) is exact: the ONE active Spec \
                        anchored there binds the run; zero or several bind none — nyx never picks \
                        the most recent. What happens next depends on the flow: one that works ON a \
                        Spec (ndev, ndoc, coder, prototyper) is REFUSED, with the candidates listed \
                        on the ambiguous case (data.reason='spec_ambiguous_for_workspace'); a flow \
                        that works on NO existing Spec — scope 'nspec', which creates its own, or \
                        'custom', which declares none — STARTS UNBOUND, keeps its workspace \
                        anchor, and reports the ambiguity as a warning \
                        (code 'spec_ambiguous_started_unbound'). That exception reads the `scope` \
                        ARGUMENT, so an explicit workflow_id — which leaves scope at its 'ndev' \
                        default — keeps the refusal. Pass an explicit spec_id to work on one of \
                        the candidates.",
        "inputSchema": {
            "type": "object",
            "properties": {
                // CONDITIONAL: every argument of `run_start` is optional, so a `#N` passed alone
                // anchors nothing. The refusal it meets is the engine's own
                // ([`crate::mcp_engine`]'s `unknown_spec_refusal`, which on that anchorless path
                // adds the anchors to name a project with — and, once the call DOES anchor one,
                // names what that project holds instead) rather than the entry point's — the
                // sentence below states the condition either way, so the advertisement matches on
                // both paths.
                "spec_id": conditional_spec_ref_prop(
                    "optional: the Spec to bind the run to (its lifecycle transitions then apply); \
                     resolved from workspace_id/cwd when omitted",
                    "you also pass project_id, workspace_id or cwd — all optional here, since this \
                     call requires no argument at all",
                ),
                // ADVERTISED BECAUSE IT IS HONOURED: `mcp_context::resolve_project` reads
                // `project_id` FIRST, and `run_start`'s own refusal for an unresolvable `#N` tells
                // the caller to add one. A tool that accepts an argument its `tools/list` does not
                // name cannot be discovered — and an agent sent after an argument the schema omits
                // is exactly the half-refusal `run_bind_spec` was just repaired for (findings
                // `01M0ZWAH9EQDAZR66AFS1YJFPY`, `01M1008NHM9RRA1ZAS4V7K1PM4`).
                "project_id": str_prop("optional: the PROJECT this call anchors — what a '#N' or a bare number is counted in, and which project's active workflow the run uses. It resolves NO Spec by itself (workspace_id / cwd do that, or name the spec_id): it is the anchor to add when a short reference has no project to resolve against. Passed BESIDE a spec_id of another project, the disagreement is refused, never silently overridden"),
                "workspace_id": str_prop("optional: resolve the Spec + project from this workspace — exactly one active Spec must be anchored there for the run to bind one"),
                "cwd": str_prop("optional: resolve the Spec + project from the workspace whose path matches this cwd (deepest matching root wins), then the same one-active-Spec rule as workspace_id"),
                "workflow_id": str_prop("optional: run THIS workflow explicitly, bypassing scope resolution"),
                "scope": str_prop("optional: which workflow family to run (ndev | nspec | ndoc); defaults to ndev. coder/prototyper are SUB-WORKFLOW-ONLY (opened by a sub_workflow node), not started here"),
            },
        },
    }));
    // The RECOVERY twin of `run_start`: a run already open and carrying no Spec. Advertised because
    // an un-advertised tool is undispatchable (N17) — and because the session that needs it is by
    // definition one that already went wrong once, so it must be able to find the way out.
    //
    // THE `data.reason` CODES BELOW ARE A CONTRACT, NOT PROSE: they are written here as literals
    // for readability, and `tests::run_bind_spec_advertises_its_refusal_codes_by_constant`
    // confronts every one of them with `mcp_engine`'s `bind_refusal` module — so renaming a code,
    // or adding one to that module (at any visibility), fails a test instead of leaving this
    // sentence quietly wrong.
    descriptors.push(json!({
        "name": crate::mcp_engine::RUN_BIND_SPEC_TOOL,
        "description": "RECOVERY, not the normal way to anchor a run: attach an ALREADY-OPEN run to \
                        the Spec it turned out to be about. The normal way is to open the run \
                        already anchored — run_start with the Spec named. Use this when a run is \
                        open, work has been done in it, and it carries no Spec (its spec_id is \
                        null): binding it arms the Spec facts the graph reads (spec_status, \
                        spec_open_tasks) and the transitions it applies, without throwing the \
                        session away. EVERY refusal NAMES the condition it broke, under \
                        data.reason — what follows is what you can rule out BEFORE calling, not a \
                        closed list of codes. ON THE RUN: it must not be finished \
                        ('run_is_terminal' — read FIRST, because a run this session drove to its \
                        end has already left the session registry), it must be a run THIS session \
                        opened ('run_not_in_this_session' — run_list shows every session's and \
                        every project's runs, so an id read there is not yours by default), it must \
                        carry NO Spec ('run_already_bound' — a run's Spec is never re-written), and \
                        it must be TOP-LEVEL ('run_is_a_child' — a sub-workflow child inherits its \
                        parent's Spec). ON THE SPEC: it must belong to the project the run's \
                        workspace anchors ('spec_project_disagrees_with_run' — a KEY-N carries its \
                        own project, so the short grammar is exactly the form that can name a Spec \
                        of somewhere else), and it must not already carry a live run of this \
                        workflow ('spec_already_has_a_live_run') — nyx holds at most ONE live run \
                        per (workflow, Spec), and a second would be destroyed by the next run_start \
                        on that Spec. Returns { run_id, spec_id, workflow_id }.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "run_id": str_prop("the OPEN run to attach — one this session drives (its id rides in your NYX RUN block, or came back from run_start)"),
                // REQUIRED, YET CONDITIONAL — the one member of the family whose anchor is a run.
                // A run's project comes from its WORKSPACE, and `run_start` deliberately opens a
                // run without one when its cwd matched nothing (`run_location_unresolved`), which
                // is precisely the session this verb repairs. So the short forms are promised with
                // their condition rather than unconditionally, and the anchorless way out is named.
                "spec_id": conditional_spec_ref_prop(
                    "the Spec the run is really about",
                    "the run_id you pass names a run anchored on a WORKSPACE (that workspace's \
                     project is what a number is counted in, and run_start opens a run WITHOUT one \
                     when its cwd matched nothing — the very session this verb exists to repair)",
                ),
            },
            "required": ["run_id", "spec_id"],
        },
    }));
    // The v2.8 ENGINE runtime tick. The session is a THIN dispatcher: call nyx_next in a loop,
    // perform exactly the action it returns, then call it again with the result — nothing else.
    // nyx holds the run state, advances the behavior tree, runs the quality gates ITSELF and
    // BLOCKS while any is red, imposes the state transitions, and hands back one atomic action.
    //
    // THE ACTION INVENTORY BELOW IS A CONTRACT, NOT PROSE: each type is written `type[field, …]`,
    // and a test BINDS that list to the payloads the engine really builds — every action type it can
    // emit must be named here, and no field may be claimed that no payload carries
    // (`nyx_next_descriptor_states_exactly_the_actions_the_engine_emits`). This descriptor is what
    // EVERY dispatcher reads, and the failure mode it had was drift: it kept telling agents to paste
    // a spawn's `context` for a whole release after the manuals were dissolved and the field removed.
    // So state fields inside the brackets, where the guard can see them.
    descriptors.push(json!({
        "name": crate::mcp_engine::NYX_NEXT_TOOL,
        "description": "Advance an orchestration RUN by one step and get the next action to perform. \
                        The calling session is a thin dispatcher: call nyx_next(run_id), do EXACTLY \
                        the action it returns, then call nyx_next again with last_result — repeat \
                        until it returns {\"type\":\"done\"}. nyx owns the run: it walks the workflow \
                        graph, runs the quality-gate commands itself and REFUSES to advance while a \
                        blocking gate is red, applies the guarded Spec state transitions, and \
                        updates the run's blackboard. \
                        A tick returns { run_id, action, current_node_id, blackboard_delta, \
                        blackboard_removed, status }. \
                        `current_node_id` is WHERE the run is — the single answer to that, so no \
                        action carries a node id of its own; `blackboard_delta` is only the run-state \
                        keys that CHANGED this tick, and `blackboard_removed` the keys that were \
                        DROPPED (a Spec-write grant revoked, a red gate cleared, a child run that \
                        ended) — a key listed there no longer holds, whatever an earlier delta said. \
                        THE ACTION IS EXACTLY ONE OF THESE, with exactly the fields listed \
                        (a `?` marks a field that is absent when the graph declares nothing for it): \
                        spawn_subagent[agent_id, label, mode, mechanism, subject?] — run agent_id as \
                        a sub-agent to do what `label` says (mode native_task = your provider's own \
                        sub-agent, inline = do it yourself in this session; `mechanism` names which). \
                        It ships NO prompt and NO context: the sub-agent calls \
                        agent_context(run_id=…, role=agent_id, subject=subject when present) and \
                        PULLS its own manual and this run's state. Report {\"diff\": …}. \
                        judge[prompt, output_schema] — answer `prompt` yourself, honouring \
                        output_schema when it is not null; report {\"verdict\": …}. \
                        ask_human[prompt, options, answer_key, child_run_id?, child_node_id?, via?, \
                        human_turn_required?, context?] — put `prompt` to the HUMAN and wait for a \
                        real answer; offer `options` verbatim when non-empty (nyx refuses an answer \
                        outside them) and send the answer back under the field `answer_key` names. \
                        When the action carries child_run_id, the question comes from a sub-workflow: \
                        send the answer to nyx_next(run_id=child_run_id), NOT to this run. When it \
                        carries human_turn_required, the answer COMMITS the Spec (an engaging \
                        transition: ready / done / cancel): nyx checks that the human actually SPOKE \
                        on this session between its question and your answer, and REFUSES an answer \
                        with no such turn behind it — including one they told you in advance. It \
                        checks that they SPOKE, not what they said. Ask, wait, report next tick. \
                        produce — produce the artefact the current node asks for; report its reference. \
                        wait — pause; report {} to resume. \
                        sub_workflow[child_run_id, context] — a CHILD run was opened for you: do what \
                        `context` says (loop nyx_next(child_run_id) until it is done), then tick THIS \
                        run again. \
                        done — the run is over; stop calling. \
                        AND NOTHING ELSE WILL EVER REACH YOU. gate_check, transition, \
                        transition_task, query, fork_spec, create_workspace, set_category and note \
                        are nyx's OWN effects: nyx holds \
                        the PTY, the Spec authority and the DB, so it runs them INSIDE the tick and \
                        you never see them. Do not wait for one — a dispatcher parked on a gate_check \
                        it expects to be handed waits for something that never comes; the tick that \
                        runs the gate returns the NEXT dispatcher-facing action instead. A `note` \
                        likewise never arrives as an action: the sentences the walk went past ride the \
                        next action as its `notes` array — relay them to the human, they are the \
                        run's own words. \
                        Pass last_result as the object the previous action produced (e.g. \
                        {\"diff\":…} for a spawn, {\"verdict\":…} for a judge).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "run_id": str_prop("the workflow run to advance"),
                "last_result": json!({
                    "type": "object",
                    "description": "the result of the action nyx_next last returned (e.g. {\"diff\":…} for a spawn_subagent, {\"verdict\":…} for a judge); omit on the first call",
                }),
                "provider": str_prop("optional: the agent provider driving the session (e.g. claude), used to pick the sub-agent spawn mechanism; defaults to claude"),
            },
            "required": ["run_id"],
        },
    }));
    // Append the v2.7 PROTOTYPE tools: the agent's CONSTRAINED contract for driving a Spec's
    // prototype annexe. Every action is a named, guarded tool with explicit states; each mutation
    // records a `proto.*` domain event so no action of the agent is unseen. Kept OUT of V1_TOOLS.
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_GET_TOOL,
        "description": "Read a Spec's PROTOTYPE and its EXPLICIT current state: the prototype, its \
                        rounds, the current round + its page manifest, the pages, THEIR VERSIONS \
                        AND VARIANTS (with the ids proto_recommend_variant / proto_elect_variant / \
                        design_preview take — each row names its parent, so the lists join), the \
                        DESIGN LOCK (`locked` + `design_lock`: which round is frozen, when, by \
                        whom, and whether it was ever presented), and the OPEN pre-dev review \
                        comments. Start here so you know where the prototype stands before acting — \
                        a resumed session gets every identifier it needs from this one call. \
                        `warnings` names the states worth saying out loud (a lock on a round nobody \
                        presented). A Spec with no prototype yet returns { prototype: null }. \
                        Read-only. \
                        BOUNDED: the lists grow with the prototype, so the response is CAPPED \
                        (64 KiB) — never assume it is complete. `sections.<name>` gives, per list, \
                        { total, returned, truncated, next_offset }, and the top-level `truncated` \
                        says whether anything was left out. Read the REST with section + offset \
                        (section: rounds | current_round_manifest | pages | versions | variants | \
                        open_feedback), following next_offset until it is null. A row whose text \
                        was too long carries truncated_fields (field → its REAL byte length). A \
                        variant row carries its IDENTITY, never its composition_json: render the \
                        tree with design_preview(variant_id).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "spec_id": anchorless_spec_ref_prop("the Spec whose prototype to read"),
                "section": str_prop("read ONE list as a page instead of the overview: rounds | current_round_manifest | pages | versions | variants | open_feedback"),
                "offset": int_prop("where to resume inside `section` (follow the next_offset the previous page returned)"),
                "limit": int_prop("how many rows to ask for (default 25, max 200 — the byte cap may still return fewer)"),
            },
            "required": ["spec_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_PRESENT_ROUND_TOOL,
        "description": "PRESENT (freeze) a prototype round to the user — a handback. A presented \
                        round is frozen: later feedback opens a NEW round, it is never mutated in \
                        place. Unknown round id → invalid_id. Returns the presented round.",
        "inputSchema": {
            "type": "object",
            "properties": { "round_id": str_prop("the round to present (from proto_get)") },
            "required": ["round_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_ELECT_VARIANT_TOOL,
        "description": "Record the USER's RETENU (chosen) variant for a page IN a round — the \
                        user's design decision, distinct from the agent's per-version is_main \
                        recommendation. The page must be pinned in the round (else invalid_state), \
                        the variant must exist (else invalid_id), and it must BELONG to the (page, \
                        version) that round pinned (else invalid_state, naming the variants that \
                        ARE electable): the RETENU is the tree the coder receives under this page's \
                        name, so another page's variant is a corruption of the contract, not a \
                        preference. Returns the updated manifest entry.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "round_id": str_prop("the round the decision belongs to"),
                "page_id": str_prop("the page being decided (must be pinned in the round)"),
                "variant_id": str_prop("the chosen (RETENU) variant — one of the PINNED version's own (proto_get's `variants` section lists them with their page_version_id)"),
                "rationale": str_prop("optional arbitration note for the audit log"),
                "decided_by": str_prop("optional who decided (e.g. a user handle)"),
            },
            "required": ["round_id", "page_id", "variant_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_RECOMMEND_VARIANT_TOOL,
        "description": "Pose YOUR (the agent's) RECOMMENDATION on a variant — is_main. This is the \
                        agent tier of the two-tier election, and it is NOT the RETENU: it never \
                        chooses for the user, it only says which variant you would keep. The user's \
                        RETENU is recorded separately, by proto_elect_variant, on the round's \
                        manifest. ONE-PER-VERSION and REVERSIBLE: recommending another variant of the \
                        same page version simply moves the reco. Unknown variant → invalid_id; \
                        refused (invalid_state) when the version is pinned in a design-LOCKED round \
                        (open a new round instead). Returns the recommended variant.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "variant_id": str_prop("the variant you recommend (is_main) within its page version"),
            },
            "required": ["variant_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_FEEDBACK_CREATE_TOOL,
        "description": "Raise a PRE-DEV review comment on a prototype, anchored to a NODE of the \
                        component tree (node_key), not a CSS selector. Comments are a review \
                        artifact used to correct the design BEFORE election — they are NEVER \
                        transmitted to the coder. Unknown prototype → invalid_id. Returns the \
                        created comment.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prototype_id": str_prop("the prototype to comment on (from proto_get)"),
                "body": str_prop("the comment text"),
                "node_key": str_prop("optional anchor: the tree-node key (omit for a global comment)"),
                "origin_variant_id": str_prop("optional variant the comment was raised on"),
                "author_kind": str_prop("optional: session (default, the agent) | user"),
                "author_ref": str_prop("optional author handle (a session id or user handle)"),
            },
            "required": ["prototype_id", "body"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_FEEDBACK_RESOLVE_TOOL,
        "description": "Resolve a pre-dev review comment (you addressed it), recording an optional \
                        note and the variant that addressed it for the audit log. Unknown comment \
                        → invalid_id. Returns the resolved comment.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "feedback_id": str_prop("the comment to resolve (from proto_get)"),
                "resolution_note": str_prop("optional note on how it was addressed"),
                "resolution_via_variant_id": str_prop("optional variant that addressed it"),
            },
            "required": ["feedback_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_REBASE_TOOL,
        "description": "ROLL BACK onto an earlier round: open a NEW current round whose page \
                        manifest COPIES the source round's pinned versions, so the design returns \
                        to that snapshot and you iterate forward WITHOUT mutating the frozen source \
                        round. Unknown or cross-prototype source → invalid_id. Returns the new \
                        round.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prototype_id": str_prop("the prototype to rebase (from proto_get)"),
                "source_round_id": str_prop("the earlier round to roll back onto"),
                "label": str_prop("the new round's label (unique per prototype)"),
                "summary": str_prop("optional summary of the new round"),
            },
            "required": ["prototype_id", "source_round_id", "label"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_DISTILL_TOOL,
        "description": "FREEZE the elected round as THE design for the Spec — the 'distill' step, \
                        which here just LOCKS the chosen round (the composition tree is already the \
                        contract; there is no separate placement to rewrite). Validates the round \
                        belongs to the prototype, and REFUSES to freeze what is not a decision: a \
                        round that was never PRESENTED to the human, a round that pins NO page, and \
                        a round where a pinned page still owes an arbitration (it would cross the \
                        coder seam EMPTY — the refusal names those pages). MOVING an existing lock \
                        is refused too unless you pass supersede: true — a second iteration \
                        legitimately ends on a new round, but the design the coder consumes never \
                        drifts by accident. Returns the design lock, the round it superseded (null \
                        for a first freeze) and warnings.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prototype_id": str_prop("the prototype whose design to freeze"),
                "round_id": str_prop("the elected round to freeze as THE design (presented, pinned and fully elected)"),
                "decided_by": str_prop("optional who froze it (e.g. a user handle)"),
                "supersede": bool_prop("set true to deliberately MOVE an already-posed design lock onto this round (what the coder consumes changes with it); re-freezing the SAME round never needs it"),
            },
            "required": ["prototype_id", "round_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_COMPOSITION_TOOL,
        "description": "READ the FROZEN elected composition of a prototype — the coder SEAM. Exposed \
                        ONLY once the design is LOCKED (proto_distill ran); an unlocked prototype \
                        returns { composition: null }, so the seam never surfaces a moving election. \
                        The payload gives, per page pinned in the locked round: the effective RETENU \
                        variant + its composition_json (the NAMED TREE that IS the placement — \
                        Regions/Order/Hierarchy intrinsic), each node classified by nature ([DS] a \
                        catalogue component / [NEW] a proposed component to build / [inline] bespoke \
                        markup), plus the pre-dev review as ANCHORS + COUNTS ONLY (which NODES were \
                        corrected before the election, and how many comments were resolved / are \
                        still open — NEVER the comment bodies: the pre-dev review is never \
                        transmitted to the coder) and the design-system as a \
                        REFERENCE (project + design + component ids, NOT injected content). The [NEW] \
                        components are the anchor for the agent's derived dev tasks. THE PARCOURS \
                        crosses too: a node that links to another page carries `navigates_to` (the \
                        destination's page_id + the slug and title resolved at the read) with a \
                        status — `pinned` (the destination is part of the frozen design), `unpinned` \
                        (the page exists but this round does not ship it) or `unknown` (the page is \
                        gone). Which page leads to which is a DESIGN decision, so it is handed over \
                        rather than re-invented. THE SCOPED CSS EXCEPTIONS CROSS WHOLE — \
                        `css_exceptions[]` per page: everything else here is a REFERENCE the app \
                        already holds, but a css_slot exists NOWHERE else, so it travels entire (the \
                        component it belongs to, its `[data-nyx-css~=\"…\"]` scope, the COMPILED CSS \
                        with its selectors anchored and its @keyframes renamed, the constructs it \
                        uses and the REASON it was written). Each node that lands on one points at it \
                        with `css_scope`. Build them WITH the component: dropping one rebuilds the \
                        component without what no utility could express. \
                        Unknown prototype → invalid_id. Read-only. \
                        BOUNDED PAGE BY PAGE: the response is CAPPED (64 KiB) and paginates by PAGE, \
                        never inside a tree — a truncated placement would be a FALSE contract. Read \
                        page_total / returned / truncated, follow next_offset until it is null, or \
                        read ONE page directly with page: \"<slug or page_id>\". A page whose tree \
                        exceeds the cap ON ITS OWN comes back with oversized: true, its real \
                        composition_bytes and NO partial tree (split the page instead). A page whose \
                        arbitration was never recorded is counted in unelected_pages and NAMED in \
                        warnings[]: it crosses EMPTY (no elected_variant_id, no composition_json, no \
                        nodes) and must not be built as if that were the design. The resolved \
                        review anchors page separately (feedback_offset / \
                        resolved_feedback_next_offset).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prototype_id": str_prop("the prototype whose frozen composition to read"),
                "page": str_prop("read ONE elected page by slug or page_id (the targeted read — the whole budget for one tree)"),
                "offset": int_prop("where to resume in the elected pages (follow the next_offset the previous page returned)"),
                "limit": int_prop("how many elected pages to ask for (default 25, max 200 — the byte cap may still return fewer)"),
                "feedback_offset": int_prop("where to resume in the resolved review ANCHORS (follow resolved_feedback_next_offset)"),
            },
            "required": ["prototype_id"],
        },
    }));
    // Append the v2.7 PROTOTYPE CREATION tools (review #34): the 8 tools above are read/lifecycle
    // and presuppose the proto tree exists; these CREATE + compose it so the agent can BOOTSTRAP a
    // prototype end-to-end via MCP. Each respects the Phase-4 lock and emits prototypes://changed.
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_CREATE_TOOL,
        "description": "CREATE the prototype on a Spec (one per Spec) and seed its first round (v1). \
                        Start the prototype flow here. The Spec must exist (else invalid_id) and must \
                        not already have a prototype (else invalid_state). Returns the prototype and \
                        its current round.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "spec_id": anchorless_spec_ref_prop("the Spec to attach the prototype to"),
                "description": str_prop("optional brief describing what the prototype covers"),
            },
            "required": ["spec_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_ADD_PAGE_TOOL,
        "description": "Add a PAGE (a logical screen) to a prototype. slug is unique per prototype \
                        (a duplicate → invalid_state); unknown prototype → invalid_id. Returns the \
                        created page — KEEP ITS ID: a composition node targets a page BY ID \
                        (`navigate_to`) to wire the PARCOURS, and an id is what survives a slug \
                        rename. Create the pages of the parcours BEFORE composing the trees that \
                        link to them. Then add a version (proto_add_version) and variants \
                        (proto_add_variant), and pin it into a round (proto_pin_page).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prototype_id": str_prop("the prototype to add the page to"),
                "slug": str_prop("the page slug (unique per prototype) — the name the frame \
                    navigates with, RESOLVED from the page id at every render, so renaming it never \
                    breaks a link"),
                "title": str_prop("the page's human title"),
                "position": json!({ "type": "integer", "minimum": 0, "description": "optional sort position (default 0)" }),
            },
            "required": ["prototype_id", "slug", "title"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_ADD_VERSION_TOOL,
        "description": "Add a frozen VERSION (an iteration) to a page. You iterate by minting a NEW \
                        version, never mutating an existing one. label is unique per page (duplicate \
                        → invalid_state); unknown page → invalid_id. Returns the created version.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "page_id": str_prop("the page to add a version to"),
                "label": str_prop("the version label (unique per page, e.g. v1)"),
                "summary": str_prop("optional summary of what this version changes"),
            },
            "required": ["page_id", "label"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_ADD_VARIANT_TOOL,
        "description": "Add a VARIANT (one concrete rendering) to a page version. composition_json is \
                        the component TREE (the named placement), NOT an HTML blob. label is unique \
                        per version (duplicate → invalid_state); unknown version → invalid_id. \
                        Refused (invalid_state) when the version is pinned in a design-LOCKED round \
                        (the frozen election must not drift — open a new round instead), and \
                        (invalid_id) when a node NAVIGATES to a page this prototype does not have. \
                        THE SHAPE IS CHECKED AT THE WRITE (invalid_argument): a composition_json \
                        that is not valid JSON, one whose root is an ARRAY instead of ONE node \
                        object (several blocks are the CHILDREN of one root), an [inline] node \
                        whose markup carries Handlebars LOGIC (a block helper or a partial — the \
                        same logic-less contract a component template obeys, named with the node's \
                        key), and a states_json nyx cannot read. A tree that parses to nothing \
                        renders nothing AND answers with an empty warnings[]. \
                        Returns the created variant AND `warnings`: every node whose params carry a \
                        key the component it references declares NOWHERE, or that omits one of that \
                        component's REQUIRED params. Read them — they are the difference between a \
                        typo and a node that renders empty in silence. They never refuse the write, \
                        and nyx never guesses a mapping between two names.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "page_version_id": str_prop("the page version to add a variant to"),
                "label": str_prop("the variant label (unique per version)"),
                "title": str_prop("the variant's human title"),
                "composition_json": str_prop("optional serialized component tree (the placement); \
                    each node's params keys must be params or axes the referenced component \
                    DECLARES; defaults to empty. A node may also carry `navigate_to`: the PAGE it \
                    opens when clicked, as a `prototype_pages.id` (what proto_add_page returned) — \
                    that is how a multi-page prototype shows a PARCOURS instead of screens that do \
                    not answer each other. The link lives on the NODE and not on the component, \
                    because a destination is a PLACEMENT decision (the same Button is a Continue \
                    here and a Back there); it is an ID and never a slug, and nyx resolves it to the \
                    current slug at every render, so renaming a page breaks nothing. A `navigate_to` \
                    naming a page this prototype does not have is REFUSED (invalid_id, listing the \
                    pages it does have), and so is one on a STRUCTURAL node (no component, no \
                    inline — nothing to click). CYCLES are fine: a navigation is a user gesture at \
                    click time, not an expansion at render time"),
                "states_json": str_prop("optional VARIANCE AXES for this variant — the SAME model as \
                    a component's: { \"axes\": [{ \"name\": \"loading\", \"default\": \"off\", \
                    \"values\": [{ \"name\": \"off\" }, { \"name\": \"on\", \"classes\": \
                    \"pointer-events-none opacity-70\" }] }] }. ONE AXIS PER NOTION THAT VARIES, each \
                    value carrying the CONCRETE classes the renderer applies; the proto frame shows \
                    ONE CHIP-BAR ROW PER AXIS. What classes cannot do is structural: \
                    data-nyx-if=\"loading=on\" / data-nyx-if-not on the element that only EXISTS at \
                    some values (data-nyx-state-case=\"<value>\" for a whole alternative rendering of \
                    the first axis). A BASELINE value legitimately carries no `classes` when the \
                    complement is conditioned on its axis. The FLAT list [\"idle\", \"loading\"] is \
                    still read, as ONE anonymous axis named `state` with no classes"),
                "position": json!({ "type": "integer", "minimum": 0, "description": "optional sort position (default 0)" }),
            },
            "required": ["page_version_id", "label", "title"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_PIN_PAGE_TOOL,
        "description": "PIN a (page, version) into a round's manifest, so the round includes the page \
                        and it becomes electable and renderable. Unknown round / page / version → \
                        invalid_id. Refused (invalid_state) when the round is design-LOCKED (frozen). \
                        Returns the manifest entry.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "round_id": str_prop("the round whose manifest to pin into"),
                "page_id": str_prop("the page to pin"),
                "page_version_id": str_prop("the page version to pin for that page"),
                "position": json!({ "type": "integer", "minimum": 0, "description": "optional sort position (default 0)" }),
            },
            "required": ["round_id", "page_id", "page_version_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_OPEN_ROUND_TOOL,
        "description": "OPEN a new, EMPTY current round to start a fresh iteration whose manifest you \
                        compose from scratch (contrast proto_rebase, which clones an earlier round's \
                        manifest). label is unique per prototype (duplicate → invalid_state); unknown \
                        prototype → invalid_id. Returns the new round.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prototype_id": str_prop("the prototype to open a round on"),
                "label": str_prop("the round label (unique per prototype)"),
                "summary": str_prop("optional summary of the round"),
            },
            "required": ["prototype_id", "label"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_proto::PROTO_AUTHOR_COMPONENT_TOOL,
        "description": "AUTHOR a [NEW] component into the project's design-system catalogue — for a \
                        component a prototype proposes that is ABSENT from the catalogue (greenfield, \
                        no on-disk source yet; the coder builds it to the app's standards). THE \
                        EXCEPTION, NOT THE RULE: composing from the catalogue is what you do, so run \
                        design_inventory FIRST and author only what it does not have — an authored \
                        [NEW] is a component somebody will have to build. The \
                        component appears in /design immediately. Unknown prototype → invalid_id; a \
                        name ALREADY in the catalogue → invalid_state (it is not [NEW] then). THE \
                        SHAPE OF params_json / states_json IS CHECKED HERE, exactly as on \
                        design_refresh_component (invalid_argument, naming the field) — all the \
                        more so since a [NEW] authored from its params alone IS its params. Returns \
                        the created component.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prototype_id": str_prop("the prototype proposing the [NEW] component (resolves the project)"),
                "name": str_prop("the component name (unique per project — must be absent from the catalogue)"),
                "template": str_prop("optional LOGIC-LESS Handlebars template for the component — \
                    plain {{param}} / {{{param}}} substitutions and {{{children}}} for a slot, ONE \
                    self-contained element (no block helper: conditionals and repetition are \
                    expressed by the composition TREE, variance by the axes below). It may REFERENCE \
                    a SHARED sub-component instead of copying it — data-nyx-use=\"Spinner\" on a \
                    placeholder element, data-nyx-param-<name> for the params it hands over, \
                    data-nyx-use-axes=\"size=sm\" for the sub-component's OWN axes (NEVER inherited \
                    from this component: an axis name is local, so you fix the values at the \
                    reference site). nyx resolves it in the sidecar at every render, so one fix to \
                    the sub-component reaches every host; an unknown target, a CYCLE (A -> B -> A) or \
                    a nesting deeper than 8 is refused by the render and WARNED about at this write. \
                    OMIT the template if you \
                    have none (a [NEW] can be authored from its params/states alone); a template that \
                    is present but BLANK is refused (invalid_argument)"),
                "params_json": str_prop("optional params — an ARRAY of DESCRIPTORS, one per value the \
                    template SUBSTITUTES: [{ name, type, label?, required?, default?, preview?, \
                    options }]. `type` is one of SIX and NOTHING else: `text` (a string, substituted \
                    escaped), `html` (raw markup, substituted verbatim), `number`, `boolean` (a value \
                    the template really substitutes — a flag that makes the component LOOK different \
                    is an AXIS with two values, not a param), `select` (a constrained value whose \
                    `options` list is then MANDATORY, and is ignored on every other type), and the \
                    DEPRECATED `state`. Any other word (\"enum\", \"string\", \"color\"…) is REFUSED, \
                    not repaired. `required` = the CALLER must provide it (the component has no value \
                    of its own), and a required param has no `default`; `default` = the component's \
                    OWN fallback, only when it really carries one; `preview` = an EXAMPLE value used \
                    ONLY to render an aperçu (resolution: preview > default > nothing), so give one \
                    to every required param. A VARIANCE AXIS IS NEVER A PARAM — it goes in \
                    states_json"),
                "states_json": str_prop("optional VARIANCE AXES: { \"axes\": [{ \"name\": \"variant\", \
                    \"default\": \"primary\", \"values\": [{ \"name\": \"primary\", \"classes\": \
                    \"bg-primary text-primary-foreground\" }, { \"name\": \"outline\", \"classes\": \
                    \"border border-input\" }] }] }. ONE AXIS PER NOTION THAT VARIES — `variant`, \
                    `size`, and a boolean like `loading` (an axis with TWO values) — each value \
                    carrying the CONCRETE classes the renderer applies to the root; the axes are \
                    ORTHOGONAL, so you never enumerate their cross product. What classes cannot do is \
                    structural: data-nyx-if=\"loading=on\" / data-nyx-if-not on the element that only \
                    EXISTS at some values (data-nyx-state-case=\"<value>\" for a whole alternative \
                    rendering of the first axis, data-nyx-axis=\"size\" to apply an axis on an INNER \
                    element). A value with NO `classes` and NOTHING conditioned on its axis cannot \
                    change one pixel and is WARNED about — but a BASELINE (`{ \"name\": \"off\" }`) is \
                    correct as long as the complement is conditioned on the axis. The FLAT list \
                    [\"default\", \"ghost\"] is still read, as ONE anonymous axis named `state`"),
                "css_slot": css_slot_prop(),
                "css_slot_reason": css_slot_reason_prop(),
            },
            "required": ["prototype_id", "name"],
        },
    }));
    // Append the v2.7 DESIGN-SYSTEM tools (reviews #23, #40, #45): `design_inventory` DISCOVERS the
    // component BUNDLES of the project's real front (and reads one on demand), and
    // `design_refresh_component` STORES the agent's synthesis of a bundle. Kept OUT of V1_TOOLS (a
    // new surface). The inventory's IPC twin is the `design_inventory` db-command (same
    // `nyx_core::design::inventory_project` authority); it is no longer bound to any UI gesture
    // (finding 01KYKX7HBP4ZSSS6ZDM31T6A5E) — `/design` only reads and repaints on the event.
    descriptors.push(json!({
        "name": crate::mcp_tools_design::DESIGN_INVENTORY_TOOL,
        "description": "DISCOVER a project's design-system components from its repos, as BUNDLES. \
                        COMPOSING FROM THIS CATALOGUE IS THE RULE; AUTHORING A COMPONENT IS THE \
                        EXCEPTION — call this before you write one, whether you are prototyping or \
                        coding, and re-invent no rendering the project already has. \
                        A component is a SET of files, not a file: nyx scans the project's source \
                        (the same repos the dev context sees, resolved from the root workspace) by \
                        each repo's DETECTED-stack entry globs (Angular / React / Vue …, from \
                        angular.json / package.json deps), groups each component's files (an \
                        Angular component CLASS — recognized by its @Component decorator, so both \
                        button.ts and button.component.ts work — plus the template resolved from \
                        its templateUrl, the sibling .variants.* and the styles; a React .tsx plus \
                        its css/variants siblings), FILTERS OUT the non-components (.service.ts / .types.ts / \
                        .variants.ts / spec / stories), and UPSERTs each bundle into the catalogue \
                        (idempotent — never duplicates, and NEVER clobbers a template you \
                        synthesized). nyx does NOT write templates: YOU synthesize each bundle into \
                        a self-contained logic-less Handlebars template + params + states and store \
                        it with design_refresh_component. The response is BOUNDED (counters + up to \
                        `limit` bundles, default 25, minimum 1, under a hard byte budget): \
                        pending_count says how much work is left, `pending_only` narrows the list to \
                        it, and `offset` PAGES through the rest whenever `truncated` is true. ONLY \
                        `offset: 0` SCANS the disk; a page with `offset > 0` is a NON-MUTATING read \
                        of the catalogue that scan persisted (mode: \"catalogue\"), so turning pages \
                        costs nothing. PAGING CONTRACT: start at `offset: 0` and follow \
                        `next_offset` until it is null — every entry of the population is then \
                        returned EXACTLY ONCE and `total` is the same on every page, with AND \
                        without `pending_only` (both halves enumerate the same set — what the LAST \
                        SCAN discovered — in the same catalogue-name order, under the same pending \
                        predicate). A scan someone else runs mid-traversal replaces that set: page \
                        back to `offset: 0` to restart coherently. Pass `component` to READ ONE \
                        bundle's files WITH their content \
                        (the matter to synthesize) instead of listing — quote the `input_hash` it \
                        returns when you store the synthesis. ONE BUNDLE AT A TIME: read it, \
                        synthesize it, store it (design_refresh_component), render it \
                        (design_preview), THEN take the next name off the pending list — fanning out \
                        over ten bundles loses the matter of the first nine to the response cap, and \
                        a synthesis is worth what its input was. Two components sharing a base name in \
                        different folders get DISTINCT path-qualified names (e.g. `list/data-table`): \
                        always use the name the tool returns. The loop is INCREMENTAL: a bundle \
                        whose sources have not changed since its last synthesis is never re-proposed \
                        — the change detector folds in EVERY byte of every bundle file, so an edit \
                        at any offset counts, however large the file. The scan also RECONCILES the \
                        catalogue with reality: a component MOVED on disk is re-anchored by CONTENT \
                        (same row, same synthesis — listed in `moved`), a greenfield [NEW] the front \
                        now implements is MATERIALIZED in place (listed in `materialized`), and a row \
                        whose source is GONE (deleted, or no longer matched by the globs) is reported \
                        in `orphans`. Such a row is PRUNED only when it was never synthesized — \
                        which loses nothing, a re-scan rebuilds it. A row CARRYING A SYNTHESIS is \
                        never destroyed automatically: it is flagged an orphan and kept, like a row \
                        the run is in no position to judge (its repo was not scanned, its scan met \
                        an I/O error — see `scan_error_count`, or that repo discovered nothing at \
                        all). `orphans[].kept_because` states which — `content_appeared` meaning a \
                        synthesis landed on that row while this very run was pruning, and the \
                        delete stood down. Use design_delete_component to \
                        remove what the prune spares. NOTE `scan_error_count`: when it is non-zero \
                        the walk could NOT read part of the tree, so NOTHING was pruned in the repos \
                        concerned and `discovered` under-reports — fix the cause and re-run rather \
                        than acting on the gap. The same rule applies when READING one bundle \
                        (`component`): `read_error_count` non-zero means part of the matter could \
                        NOT be read (each file names its failure in `files[].read_error`) — an \
                        unreadable file is not an empty one, so re-read rather than synthesize on \
                        the gap. A component is \
                        NEVER re-pointed at another file merely because it has the same name. WHAT \
                        COUNTS AS A COMPONENT here is not nyx's opinion, it is the project's \
                        RECIPE: read it (and write it, once, after exploring the repo) with \
                        design_recipe — that is the lever when this scan discovers far fewer \
                        bundles than the front actually has. Pass \
                        `globs` to OVERRIDE the detected-stack entry globs for this project \
                        (persisted; empty array clears the override) — a shortcut for the entry \
                        globs ALONE; design_recipe is the full lever. GREENFIELD-safe: a project \
                        with no front yet discovers nothing without error.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("the project whose design-system to inventory"),
                "component": str_prop("optional: READ this catalogue component's BUNDLE (its files \
                    WITH their content) instead of listing — the matter you synthesize from"),
                "limit": json!({ "type": "integer", "minimum": 1, "description": "how many bundles to echo (default 25, max 200, clamped to at least 1) — the counters carry the real scale" }),
                "offset": json!({ "type": "integer", "minimum": 0, "description": "how many bundles to SKIP before echoing (default 0). 0 = a fresh disk SCAN; any other value = a non-mutating page of the persisted catalogue. When the response is `truncated`, ask again with the `next_offset` it returns (NOT offset+limit — a byte budget can cut a page short); `next_offset` is never equal to `offset`, so following it always progresses" }),
                "pending_only": json!({ "type": "boolean", "description": "list only the bundles AWAITING a (re-)synthesis (never synthesized, or their sources changed)" }),
                "globs": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "optional per-project ENTRY glob OVERRIDE (e.g. \
                        [\"src/app/**/*.component.ts\"]); replaces the detected-stack default. \
                        An empty array clears a previously set override. Omit to leave it unchanged. \
                        REFUSED (invalid_argument) once the project has a design_recipe: the recipe \
                        supersedes this shortcut, so edit its `entries[].globs` instead of storing \
                        a setting that would be ignored. \
                        GLOB DIALECT (matched against each file's repo-relative, /-separated path): \
                        `**/` = zero or more WHOLE directories, so `**/*.tsx` also matches a \
                        root-level `Button.tsx` while `**/Button.tsx` does NOT match \
                        `MyButton.tsx`; a trailing `**` (as in `src/**`) = any run of characters, \
                        `/` included; `*` = any run WITHIN one path segment (never crosses a `/`); \
                        `?` = exactly one character. No brace `{a,b}` or bracket `[abc]` \
                        expansion, and no negation: pass several patterns instead.",
                }),
            },
            "required": ["project_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_design::DESIGN_RECIPE_TOOL,
        "description": "READ / WRITE the project's BUNDLE RECIPE — what constitutes a COMPONENT in \
                        THIS repo. nyx lists the files and hashes them; the RECIPE says which of \
                        them open a component and what its other files are, and it is YOURS to \
                        write: explore the repo ONCE, understand the convention, write it here. \
                        nyx then applies it deterministically at every scan (with the input hash \
                        and the incremental loop). This is a ONE-OFF per PROJECT, not per scan. \
                        Shapes: (1) { project_id } READS the effective recipe + `source` \
                        (`custom` = you wrote it, `globs_override` = derived from a legacy `globs` \
                        override, `stack_default` = derived from the DETECTED stack — what nyx \
                        assumed before anyone told it) + `repos[]`, EACH with its detected stack \
                        AND the recipe it is really scanned with, + `warnings[]`; (2) \
                        { project_id, recipe: {…} } WRITES it; (3) { project_id, pins: [...] } \
                        writes only the PINS; (4) { project_id, reset: true } goes back to the \
                        default. A SECTION YOU DO NOT MENTION IS KEPT from the recipe that is \
                        already effective (the response lists them in `inherited`); to CLEAR one, \
                        write it as `[]`. The `recipe` echoed back is always the EFFECTIVE one — \
                        every section, empty ones included — so what the next scan will apply is on \
                        screen. A recipe is PROJECT-WIDE: in a monorepo whose repos run different \
                        stacks it governs all of them (the globs are matched per repo), which the \
                        read WARNS about, and the document it offers already covers every repo. \
                        THE SHAPE: `entries` is an ORDERED list of rules — { globs, \
                        extensions?, qualifiers?, requires_content?, template_attribute? } — the \
                        FIRST rule matching a file claims it, and `requires_content` (e.g. \
                        \"@Component\", \"defineWidget(\") then decides whether that file really \
                        opens a component; `template_attribute` (e.g. \"templateUrl\", \"view\") is \
                        read out of the entry as `attribute: './x.html'` and resolved against the \
                        entry's directory. `siblings` maps a same-base-name neighbour to its ROLE \
                        ({ extensions?, qualifiers?, role: template|style|variants }, first match \
                        wins; a neighbour no rule claims is NOT in the bundle). \
                        `family_qualifiers` are the dotted segments STRIPPED when deriving the \
                        base name (so `button.component.ts` + `button.view.html` are ONE \
                        component); `non_component_qualifiers` never open one (`.service.ts`); \
                        `exclude` is a list of path globs (specs, stories, `.d.ts`); \
                        `ignored_dirs` is the list of directory NAMES the walk never descends into, \
                        pruned at every level (defaults to node_modules/.git/target/dist/build/out/ \
                        … — override it to keep a front that legitimately lives under `build/`, or \
                        to add `vendor`). READ THE \
                        DEFAULT FIRST — it is exactly what nyx used to assume, and the best \
                        template to edit. ESCAPE HATCH: what no rule can express is PINNED — \
                        `pins: [{ entry, files: [{ path, role }], name? }]` names the files of \
                        THAT component verbatim, bypassing every rule, and a pinned bundle is \
                        catalogued, hashed and kept incremental like any other. Its NAME, when you \
                        give none, is derived by YOUR recipe's `family_qualifiers`, exactly as a \
                        discovered bundle's. A pin path is repo-relative and taken LITERALLY: an \
                        absolute (`/etc/hosts`, `C:\\…`, `~/…`) or non-canonical (`./a`, `a//b`) \
                        one is REFUSED rather than re-interpreted, and at scan time a path that \
                        leaves the repo through a symlink is refused too (an `outside_repo` scan \
                        error) — a pin reaches exactly what the walk reaches. A pinned file that \
                        does not exist shows up in the inventory's `scan_errors` (fail-closed) \
                        rather than being silently dropped. This tool CHANGES NOTHING BY ITSELF: \
                        run design_inventory(project_id) afterwards to apply it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("the project whose bundle recipe to read / write"),
                "recipe": json!({
                    "type": "object",
                    "description": "the recipe DOCUMENT to persist ({ version, entries[], \
                        siblings[], family_qualifiers[], non_component_qualifiers[], exclude[], \
                        ignored_dirs[], pins[] }). Read the current one first and edit that shape — \
                        unknown fields are REFUSED rather than ignored, and `entries` may not be \
                        empty (a recipe with no entry rule would inventory NOTHING). A section you \
                        OMIT is KEPT from the current effective recipe (reported in `inherited`); \
                        write `[]` to clear one. The total size of all patterns is bounded, so a \
                        scan can never be made pathologically slow.",
                }),
                "pins": json!({
                    "type": "array",
                    "items": { "type": "object" },
                    "description": "the explicitly PINNED components — [{ entry, files: [{ path, \
                        role }], name? }] — for what no rule expresses. Writing this alone leaves \
                        the rules untouched; [] clears the pins.",
                }),
                "reset": json!({ "type": "boolean", "description": "drop the stored recipe and go back to the default derived from the detected stack" }),
            },
            "required": ["project_id"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_design::DESIGN_REFRESH_COMPONENT_TOOL,
        "description": "STORE your SYNTHESIS of a component that is ALREADY in the catalogue: \
                        replace its Handlebars template + params_json + states_json (its identity \
                        and its on-disk provenance are preserved), and record the bundle's INPUT \
                        hash so design_inventory knows it is up to date and never re-proposes it \
                        until its sources change. This is how you (re-)synthesize a bundle read \
                        through design_inventory(component=…), and how you FIX a botched synthesis \
                        — no delete-then-recreate. DISTINCT from proto_author_component, which \
                        CREATES a greenfield [NEW] component and still refuses a name already in \
                        the catalogue. The template must be LOGIC-LESS (PDR-0010): a block helper \
                        is refused (invalid_argument) — express conditionals and repetition through \
                        the composition TREE. It must also be NON-BLANK: this call REPLACES the \
                        component's content, so a whitespace-only template would erase the stored \
                        template AND its params/states — it is refused (invalid_argument). To remove \
                        a component on purpose, use design_delete_component. THE SHAPE OF BOTH JSON \
                        DOCUMENTS IS CHECKED HERE (invalid_argument, naming the field): params_json \
                        must be an ARRAY of descriptor objects each carrying a `name`; states_json \
                        must be valid JSON whose `axes` is an ARRAY of axes with a `name` and a \
                        non-empty `values`. A blob nyx cannot read declares NOTHING, and would come \
                        back with an empty warnings[] — which is the only 'done' this surface has. \
                        An unknown component name → invalid_id. ALWAYS pass \
                        back the `input_hash` design_inventory(component=…) gave you: if the \
                        bundle's sources moved while you were synthesizing, your template is still \
                        stored but NOT marked up to date (synthesized: false, source_changed: true) \
                        so the change is re-proposed instead of being silently swallowed.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("the project whose catalogue holds the component"),
                "name": str_prop("the EXISTING component's name (as listed by design_inventory)"),
                "template": str_prop("the synthesized Handlebars template (logic-less: plain \
                    {{param}} / {{{param}}} substitutions and {{{children}}} only). ONE element is \
                    enough: the variance is carried by the axes below, which the renderer applies as \
                    classes on top of it. A SHARED sub-component (a spinner, an icon) is REFERENCED, \
                    not copied — data-nyx-use=\"Spinner\" on a placeholder element, \
                    data-nyx-param-<name> for the params it hands over, data-nyx-use-axes=\"size=sm\" \
                    for the sub-component's OWN axes (NEVER inherited from this component: an axis \
                    name is local to a component, so you fix the values at the reference site). nyx \
                    resolves the reference in the sidecar at every render and hands the frame flat, \
                    inert markup, so correcting the sub-component ONCE reaches every host with no \
                    re-synthesis. An unknown target, a CYCLE (A -> B -> A, a self-reference included) \
                    or a nesting deeper than 8 is refused by the render and WARNED about here"),
                "params_json": str_prop("optional params — an ARRAY of DESCRIPTORS, one per value the \
                    template SUBSTITUTES, derived from the component's declared inputs/props: \
                    [{ name, type, label?, required?, default?, preview?, options }]. `type` is one \
                    of SIX and NOTHING else: `text` (a string, substituted escaped), `html` (raw \
                    markup, substituted verbatim), `number`, `boolean` (a value the template really \
                    substitutes — a flag that makes the component LOOK different is an AXIS with two \
                    values, not a param), `select` (a constrained value whose `options` list is then \
                    MANDATORY, and is ignored on every other type), and the DEPRECATED `state`. Any \
                    other word (\"enum\", \"string\", \"color\"…) is REFUSED, not repaired. \
                    `required` = the CALLER must provide it (the component \
                    has no value of its own — a button's label), and a required param has no \
                    `default`; `default` ONLY when the source \
                    declares one (an @Input with an initialiser, a React default prop); `preview` = \
                    an EXAMPLE value used ONLY to render an aperçu. The catalogue vignette resolves \
                    preview > default > nothing and REPORTS a required param that has neither, so \
                    give every required param a preview value rather than a made-up default. A \
                    VARIANCE AXIS IS NEVER A PARAM — it goes in states_json"),
                "states_json": str_prop("optional VARIANCE AXES — the CVA/variants of the bundle, \
                    RESOLVED: { \"axes\": [{ \"name\": \"variant\", \"default\": \"primary\", \
                    \"values\": [{ \"name\": \"primary\", \"classes\": \"bg-primary \
                    text-primary-foreground\" }, { \"name\": \"outline\", \"classes\": \"border \
                    border-input\" }] }] }. ONE AXIS PER CVA AXIS (`variant`, `size`, and a boolean \
                    like `loading` = an axis with TWO values), each value carrying the CONCRETE \
                    classes you resolved from the .variants.* sidecar — nyx does not execute \
                    TypeScript, so a dangling {{class}} renders nothing. The axes are ORTHOGONAL: the \
                    renderer applies the active value of EVERY axis onto the root, so you never \
                    enumerate the cross product. What classes cannot do is structural: \
                    data-nyx-if=\"loading=on\" / data-nyx-if-not on the element that only EXISTS at \
                    some values (data-nyx-state-case=\"<value>\" for a whole alternative rendering of \
                    the first axis, data-nyx-axis=\"size\" to apply an axis on an INNER element). A \
                    value with NO `classes` and NOTHING conditioned on its axis cannot change one \
                    pixel and is WARNED about — but a BASELINE (`{ \"name\": \"off\" }`) is correct as \
                    long as the complement is conditioned on the axis. The FLAT list [\"default\", \
                    \"ghost\"] is still read, as ONE anonymous axis named `state`"),
                "input_hash": str_prop("optional but RECOMMENDED: the bundle input_hash \
                    design_inventory(component=…) returned when you READ this bundle. The synthesis \
                    is marked up to date only if the sources are still at that state; otherwise the \
                    bundle stays pending (source_changed: true)"),
                "css_slot": css_slot_prop(),
                "css_slot_reason": css_slot_reason_prop(),
            },
            "required": ["project_id", "name", "template"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_design::DESIGN_DELETE_COMPONENT_TOOL,
        "description": "REMOVE a component from the project's design-system catalogue. Use it to \
                        clean up what does not belong there: a greenfield [NEW] authored by \
                        mistake, a duplicate, or an entry an earlier wrong glob catalogued that \
                        design_inventory deliberately SPARED (see `orphans[].kept_because` — the \
                        inventory prunes on its own the rows whose source really vanished, but \
                        never on a scan it could not judge). An unknown name → invalid_id, so you \
                        always know whether something was actually removed. This only touches the \
                        CATALOGUE: a prototype composition references components BY NAME, so a node \
                        pointing at a deleted component simply reads [NEW] (to be built) at the \
                        next proto_composition — nothing frozen is rewritten. OTHER COMPONENTS ARE \
                        NOT SO LUCKY: a template that REFERENCES this one (data-nyx-use) stops \
                        rendering the moment the row is gone, so the response NAMES every host that \
                        still does — re-synthesize each of them onto another component, or inline \
                        the markup it was borrowing. \
                        READ `ephemeral` AND `warnings[]`: a component's identity IS its SOURCE, so \
                        deleting the row of a component whose file STILL matches the recipe is \
                        UNDONE by the next design_inventory, which discovers it again as pending. \
                        The response says so and names the two lasting fixes — RESTRICT the recipe \
                        (design_recipe) when a whole family does not belong, or re-run with \
                        exclude_source: true for this one bundle.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("the project whose catalogue holds the component"),
                "name": str_prop("the component name to remove (exactly as design_inventory lists it)"),
                "exclude_source": json!({
                    "type": "boolean",
                    "description": "also EXCLUDE the component's source from future scans, so the \
                        removal LASTS: its bundle's paths are appended to the project's recipe \
                        `exclude` list (the very exclusion the discovery already honours), which \
                        MATERIALIZES the recipe as the project's own if it was still a default. \
                        Nothing on disk is touched. Not needed for a greenfield [NEW] or a source \
                        the recipe already ignores (nothing is written, and the response says so), \
                        and REFUSED — with the delete — for a PINNED entry (remove the pin instead) \
                        or when the recipe cannot hold one more exclusion (restrict it instead).",
                }),
            },
            "required": ["project_id", "name"],
        },
    }));
    descriptors.push(json!({
        "name": crate::mcp_tools_design::DESIGN_TOKENS_TOOL,
        "description": "READ / INGEST / EDIT the project's THEME TOKENS — the other half of a \
                        component's look. A synthesized template carries Tailwind CLASSES \
                        (bg-primary, text-muted-foreground); those classes render GREY until the \
                        project's tokens (--primary, --background, --radius, --font-sans …) exist, \
                        because that is what the classes resolve against. Three shapes, one tool: \
                        (1) { project_id } READS the layer; (2) { project_id, ingest: true } \
                        DERIVES it FROM THE APP — it walks the project's repos (the same repos \
                        design_inventory sees), ranks the stylesheets that look like the design \
                        system (globals.css / styles.css / theme.css / tokens.css, an @theme block, \
                        the canonical token names) and merges their GLOBAL custom properties \
                        (:root / html / @theme / @layer, with .dark and prefers-color-scheme: dark \
                        kept apart); (3) { project_id, set: {\"--primary\": \"oklch(…)\"}, remove: \
                        [\"--x\"], dark?: true } EDITS it. RUN THE INGEST FIRST on any project with a \
                        real front — before composing anything — and re-run it when the app's theme \
                        moves. The layer is a PROJECT SINGLETON, REFERENCED and never copied: a \
                        component must NOT carry its own colours, it carries the CLASS and this one \
                        layer gives the class its colour, so `primary` has ONE identity and an edit \
                        here applies to every component, composition and preview at once. It also \
                        records WHERE it came from (sources[]: repo + repo-relative path), so the \
                        link back to the app's stylesheet stays explicit. SAFE: an ingest that \
                        retains nothing (greenfield, or every sheet refused) leaves a hand-authored \
                        layer untouched and reports variable_count 0 — it can never erase a theme — \
                        and it still RECORDS what it refused, because that run stores no token at \
                        all, so the report is the only explanation a grey catalogue has (`changed` \
                        is about the tokens, `provenance_changed` about that report). \
                        THE INGEST OBEYS YOUR RECIPE: it prunes the same directories as \
                        design_inventory (the recipe's ignored_dirs + the floor, so a .generated \
                        mirror is never swept) and skips what your `exclude` globs match; a \
                        documentation-site theme (VitePress/Docusaurus/Storybook — its own --vp-* / \
                        --ifm-* namespace) can never outrank your application stylesheet. CHECK \
                        `sources[]` AND `skipped[]` / `pruned_dirs[]`: they say where the tokens came \
                        from AND why a sheet was refused — if your own stylesheet is in `skipped`, \
                        the reason names what to fix (in your recipe, most of the time). The LAYER \
                        KEEPS that report, so a plain read answers it too (`provenance`; null when \
                        the stored layer was written by hand rather than ingested).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("the project whose theme tokens to read / ingest / edit"),
                "ingest": json!({ "type": "boolean", "description": "re-derive the WHOLE layer from the app's own stylesheet(s) (cannot be combined with set/remove)" }),
                "set": json!({
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "token name → CSS value to upsert (`--primary` or the bare `primary`, same thing)",
                }),
                "remove": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "token names to drop from the layer",
                }),
                "dark": json!({ "type": "boolean", "description": "target the DARK theme map instead of the default/light one (set/remove only)" }),
            },
            "required": ["project_id"],
        },
    }));
    // The agent's own EYES on its synthesis (finding 01KYT0Z3RYGF3A881SDQN0X9D7) AND on the whole
    // page it presents (finding 01KYT45C6RC7K84V1CA3561SPF): every other design diagnostic judges
    // the STRUCTURE, so every visual defect of this feature was found by the human and by no tool.
    // nyx already knew how to render — this offers that to the agent that writes the component and
    // to the one that composes the page, and says, in the descriptor AND in the payload, where its
    // sight stops. ONE tool, TWO subjects, each named by its own argument (never a mode flag).
    descriptors.push(json!({
        "name": crate::mcp_tools_design::DESIGN_PREVIEW_TOOL,
        "description": "SEE WHAT YOU WROTE — the RESOLVED MARKUP, exactly as the frame ends up \
                        displaying it. NAME ONE SUBJECT: `component` (+ project_id) renders ONE \
                        catalogue component at ONE axis combination and ONE param set; `variant_id` \
                        renders a WHOLE proto COMPOSITION — the variant's node tree, which is the \
                        thing you actually present to the human. Same resolution either way: your \
                        templates substituted, every data-nyx-use reference expanded, each node at \
                        ITS OWN combination (a composition node carries its axis values in its \
                        params, which is where the shim reads them), the active value's classes \
                        applied to their targets, data-nyx-if / data-nyx-if-not / \
                        data-nyx-state-case evaluated (an absent element carrying `hidden`), and \
                        every data-nyx-* consumed. Nothing else on this surface looks at a \
                        rendering: the write-path warnings judge the STRUCTURE (a missing case, an \
                        inert axis value, an unknown key, a refused param type), never the result. \
                        THE LOOPS ARE: synthesize (design_refresh_component) -> render (this) -> \
                        LOOK -> correct -> render again, ONE combination at a time; and, before \
                        proto_present_round, RENDER THE COMPOSITION you are about to show and read \
                        it. It makes visible, with no browser: an element that should exist at ONE \
                        value but carries no condition (it is still there at the other one), a class \
                        that never lands where you thought, a param that substitutes to nothing, a \
                        reference that does not resolve, a page node nobody has built yet. \
                        A COMPOSITION ALWAYS COMPLETES: a node whose component is not in the \
                        catalogue ([NEW] not synthesized), carries no template, or fails to render \
                        comes back as a PLACEHOLDER naming itself in the markup plus an entry in \
                        `unrendered` (node_key, component, reason, correction) — the tree is never \
                        failed as a block, because the forty nodes that are right are what you are \
                        looking at. Use `node_key` to render ONE subtree when the whole page does \
                        not fit. THE PARCOURS comes back too: `navigation` lists every node that \
                        LINKS to another page (`navigate_to`), with the destination resolved from \
                        its id to the slug and TITLE a human reads, and `navigation_broken_count` \
                        says how many of them lead nowhere — a dead link is a click that silently \
                        does nothing, which is exactly what a prototype exists to catch first. AXES you omit render at their own `default` (else their first \
                        value) and the combination actually used comes BACK in `axes`, so a value \
                        the axis does not declare is visibly not the one you asked for. ON A \
                        COMPOSITION, `axes` pilots the VARIANT's OWN axes — the page-level ones its \
                        states_json declares, which the frame shows one chip-bar row per: they \
                        decide every data-nyx-if / data-nyx-if-not on them at EVERY node (an \
                        [inline] node included), and their classes land where the shim puts them. \
                        `declared_axes` says what this variant can be piloted on; an axis or a \
                        value it does not declare is named in warnings, never silently dropped. PARAMS you \
                        omit fall back to their `preview`, then their `default`, then NOTHING — the \
                        aperçu's own order, so a call with no `params` shows exactly what the \
                        catalogue tile shows; each one comes back with the `source` it resolved from \
                        and `renders_empty`. (A COMPOSITION substitutes the node's OWN params and \
                        nothing else: it shows the design as written, never a preview value the \
                        design does not carry.) `children` fills the slot. \
                        THE VIEWPORT, honestly: nyx LAYS NOTHING OUT — the markup is byte for byte \
                        the same at every width — so `viewport` (a width in px, or mobile / tablet / \
                        desktop) does exactly two real things: it is the width a frame must APPLY to \
                        display this, and it reports which responsive CLASSES are in force at it \
                        (the breakpoints, from your own --breakpoint-* tokens when you have them, \
                        and every md: / lg: / max-* variant the markup carries, each with `active`). \
                        Markup with NO responsive class at all is SAID to render identically at \
                        every width — which is the answer to `is this responsive?`, and often the \
                        defect. \
                        WHAT IT DOES NOT DO, and the answer repeats it in `cannot_show`: it is \
                        MARKUP, NOT AN IMAGE, and it is NOT a visual validation. It proves what is \
                        THERE — which elements, which classes, which substitutions. It proves \
                        nothing about how it LOOKS: no colour (you read `bg-primary`, not whether \
                        the token behind it resolves or contrasts), no spacing, no alignment, no \
                        size, no overflow, no font, no hover/focus state, and no idea what a page \
                        REFLOWS to at a width. A class you invented and a class the theme really \
                        defines read identically here. nyx produces NO IMAGE (no rendering engine in \
                        the sidecar, no capture anywhere in the product, and an image would not fit \
                        a response): do not wait for one, do not pretend you have one. Never report \
                        a component or a page as visually validated on the strength of this: say \
                        what you CHECKED (the structure) and what you did NOT (the rendering), and \
                        when the open question IS visual, PRESENT the round and ASK. READ-ONLY, it \
                        stores nothing. An unknown component or variant, a template still empty, a \
                        variant with no tree, a reference that cannot resolve, a reference cycle and \
                        an unknown node_key are EXPLICIT errors naming the correction, never a blank \
                        answer. `html` is capped — `html_bytes` (the WHOLE run) + \
                        `html_returned_bytes` + `truncated` say when it was cut.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("the project whose catalogue holds the component (COMPONENT subject only — with variant_id it is derived from the prototype's Spec)"),
                "component": str_prop("SUBJECT 1 — the component's name, exactly as design_inventory lists it"),
                "variant_id": str_prop("SUBJECT 2 — the proto VARIANT whose whole composition tree to render, against that project's catalogue (proto_add_variant returns the id; the elected one is `chosen_variant_id` in proto_get's round manifest and `elected_variant_id` in proto_composition)"),
                "node_key": str_prop("with variant_id: render ONE subtree instead of the whole tree — the '/'-joined path of node keys from the root (Root/Header/Title), the same anchor proto_feedback uses. The lever for a page whose markup does not fit one answer"),
                "viewport": json!({
                    "description": "a WIDTH in CSS px (390), or one of mobile / tablet / desktop. \
                                    It renders NOTHING differently — nyx lays nothing out — it \
                                    reports which responsive CLASSES are in force at that width \
                                    (and is the width a frame must apply to display the markup)",
                    "anyOf": [{ "type": "integer", "minimum": 1 }, { "type": "string" }],
                }),
                "axes": json!({
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "axis name -> the value to render at (e.g. { \"variant\": \
                                    \"ghost\", \"loading\": \"on\" }). COMPONENT subject: the axes \
                                    the component's own states_json declares. COMPOSITION subject: \
                                    the axes the VARIANT declares — they pilot the WHOLE page (one \
                                    chip-bar row each in the frame) and decide every data-nyx-if / \
                                    -if-not on them, at every node. \
                                    An axis you omit renders at its `default`, else its first \
                                    value; an axis, or a value, the subject does not declare is \
                                    IGNORED and NAMED in warnings. The combination really used \
                                    comes back in `axes`, and what the subject can be piloted on in \
                                    `declared_axes` (composition subject)",
                }),
                "params": json!({
                    "type": "object",
                    "description": "COMPONENT subject: param name -> the value to substitute. A \
                                    param you omit falls \
                                    back to its `preview`, then its `default`, then the empty string \
                                    (which is reported in `params[].renders_empty`). A key the \
                                    component declares nowhere is substituted anyway AND named in \
                                    warnings",
                }),
                "children": str_prop("COMPONENT subject: markup to put in the component's slot — what a composition's child nodes would render to (a slot component resolved with nothing in it renders to almost nothing)"),
            },
        },
    }));
    descriptors
}

/// Build a JSON-RPC 2.0 success envelope.
fn rpc_result_envelope(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Wrap a tool's JSON answer in an MCP `CallToolResult` — **once**.
///
/// nyx used to ship every answer TWICE: serialized into `content[0].text` AND as
/// `structuredContent`. On the reference journey's ordinary `list_terminals` reply that was
/// 3 701 bytes of envelope for 1 686 bytes of payload — the duplicate being **46 % of everything
/// the agent reads back** — and a loop driving a terminal pays it on every single read. Measured
/// again here on that same shape: **3 868 B → 1 806 B, 53 % smaller** (the reproduction runs 1 755 B
/// of payload rather than 1 686 B, same ratio). The MCP spec (2025-06-18) calls the text copy a
/// backwards-compatibility SHOULD, not a requirement: `content` is required to be *present*, and an
/// empty array satisfies that.
///
/// So the object answers — which is all of them but the degenerate case below — travel in
/// `structuredContent` alone. Verified, not assumed, against the client that matters: Claude Code
/// 2.1.246 driven headless over this exact envelope (`content: []` + `structuredContent`) received
/// the payload in full and echoed it back verbatim. The other in-tree consumer, the session hook
/// in `resources/claude-plugin/hooks/nyx-session-hook.cjs`, already reads
/// `result.structuredContent` and never looked at the text block.
///
/// THE ONE PLACE THE TEXT FORM SURVIVES is a non-object answer: MCP types `structuredContent` as an
/// object, so an array/string/number could not travel there and would leave the client with
/// nothing at all. Those keep the text block — and only those.
fn tool_result_envelope(result: Value) -> Value {
    if result.is_object() {
        json!({ "content": [], "structuredContent": result, "isError": false })
    } else {
        json!({ "content": [{ "type": "text", "text": result.to_string() }], "isError": false })
    }
}

/// Build a JSON-RPC 2.0 error envelope.
fn rpc_error_envelope(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A `200`/`4xx` JSON response for tiny_http.
fn json_response(status: u16, body: &Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static content-type header");
    tiny_http::Response::from_data(bytes)
        .with_status_code(status)
        .with_header(header)
}

/// A plain-text response for tiny_http (errors / 404).
fn text_response(status: u16, body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(body).with_status_code(status)
}

#[cfg(test)]
mod tests {
    //! Loopback integration tests: drive a REAL `McpServer` over a TCP socket on
    //! `127.0.0.1`, exercising the health probe, the MCP handshake, single-instance
    //! start-once, and port stability/configurability.

    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Once;
    use std::time::Duration;

    /// Serialize the env-var mutations (`NYX_MCP_PORT`) across tests in this binary,
    /// since the process environment is shared. Each test that sets the var clears
    /// it before/after under this guard.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// `tools/list` COSTS SOMETHING, AND THE NUMBER IS NOW WRITTEN DOWN.
    ///
    /// The catalogue is the first thing every agent reads and the one thing it cannot decline: on
    /// the 2026-09-01 reference journey it was 179 230 bytes for 111 tools — of the order of
    /// 45 000 tokens spent before a single call. It had drifted there because nothing stated what
    /// it was allowed to cost, and a total nobody measures is a total that only grows.
    ///
    /// THREE ASSERTIONS, because a single global number would be a weak guard. The total pins the
    /// bill. The per-descriptor ceiling names the tool that raised it, which the total never can —
    /// 115 descriptors each gaining 200 bytes read exactly like drift. And the exemption list is
    /// checked BOTH ways, so an admitted descriptor that later came back under the ceiling is
    /// forced out of the list instead of staying there to shelter the next one.
    #[test]
    fn tools_list_stays_under_its_size_budget() {
        let tools = tool_descriptors();
        let total = serde_json::to_string(&json!({ "tools": tools }))
            .expect("the catalogue serializes")
            .len();
        assert!(
            total <= TOOLS_LIST_BUDGET_BYTES,
            "tools/list is {total} bytes for {} tools — over the {TOOLS_LIST_BUDGET_BYTES}-byte \
             budget every agent pays on connection. SHORTEN a description; raising the budget is a \
             decision about what everyone pays, not a way to make this test pass",
            tools.len()
        );

        let size = |name: &str| -> Option<usize> {
            tools
                .iter()
                .find(|t| t["name"] == json!(name))
                .map(|t| serde_json::to_string(t).expect("a descriptor serializes").len())
        };

        for tool in &tools {
            let name = tool["name"].as_str().expect("every descriptor is named");
            let len = serde_json::to_string(tool)
                .expect("a descriptor serializes")
                .len();
            assert!(
                len <= TOOL_DESCRIPTOR_BUDGET_BYTES || OVERSIZE_DESCRIPTORS.contains(&name),
                "`{name}` weighs {len} bytes on its own, over the \
                 {TOOL_DESCRIPTOR_BUDGET_BYTES}-byte per-tool ceiling. Trim it, or admit it by \
                 name in OVERSIZE_DESCRIPTORS — deliberately, with a reviewer"
            );
        }

        for admitted in OVERSIZE_DESCRIPTORS {
            let len = size(admitted).unwrap_or_else(|| {
                panic!("OVERSIZE_DESCRIPTORS names `{admitted}`, which no longer exists")
            });
            assert!(
                len > TOOL_DESCRIPTOR_BUDGET_BYTES,
                "`{admitted}` is down to {len} bytes, under the ceiling — take it OUT of \
                 OVERSIZE_DESCRIPTORS, so the list keeps naming only real exceptions"
            );
        }
    }

    /// THE TERMINAL TOOLS TEACH THE GESTURE THAT WORKS, AND NO LONGER THE ONE THAT DOES NOT.
    ///
    /// `list_terminals` used to hand an agent a polling protocol — capture `exec_state_updated_at`,
    /// call, then poll until it advances to a settled `exec_state` — and `read_terminal` sent the
    /// reader to it. On the 2026-09-01 journey that protocol was measured against a real Claude
    /// session: `busy` was `null` for its whole life and `exec_state_updated_at` did not move once
    /// while the agent answered. A description that teaches a gesture which cannot work is worse
    /// than a short one that teaches nothing — the agent follows it, and loops.
    ///
    /// Both halves are asserted, because either alone can be satisfied by a description that says
    /// nothing: the retired formulations are gone AND the tool that actually waits is named.
    #[test]
    fn the_terminal_tools_send_you_to_wait_for_terminal_not_to_a_polling_loop() {
        let descriptors = tool_descriptors();
        let describe = |name: &str| -> String {
            descriptors
                .iter()
                .find(|t| t["name"] == json!(name))
                .unwrap_or_else(|| panic!("`{name}` is advertised"))["description"]
                .as_str()
                .expect("a description")
                .to_string()
        };

        // The retired protocol, in the words it was written in. Any of them coming back means the
        // loop came back with it.
        const RETIRED: &[&str] = &[
            "poll list_terminals",
            "poll `busy`",
            "timestamp ADVANCES",
            "advances to a settled",
            "exec_state_updated_at BEFORE",
        ];
        for tool in [
            LIST_TERMINALS_TOOL,
            READ_TERMINAL_TOOL,
            WAIT_FOR_TERMINAL_TOOL,
            SEND_KEYS_TOOL,
            SEND_TO_TERMINAL_TOOL,
        ] {
            let desc = describe(tool);
            for stale in RETIRED {
                assert!(
                    !desc.contains(stale),
                    "`{tool}` still teaches the retired polling protocol (\"{stale}\") — it does \
                     not work on a terminal running an agent, which is the case it was written \
                     for: {desc}"
                );
            }
        }

        // …and the two READ tools name the tool that does the waiting, so an agent that must wait
        // is not left to invent something.
        for tool in [LIST_TERMINALS_TOOL, READ_TERMINAL_TOOL] {
            let desc = describe(tool);
            assert!(
                desc.contains(WAIT_FOR_TERMINAL_TOOL),
                "`{tool}` must name `{WAIT_FOR_TERMINAL_TOOL}` — telling an agent not to poll \
                 without saying what to do instead only moves the problem: {desc}"
            );
        }

        // `read_terminal` states the ORDER, which is the whole lesson: wait, then read.
        let read = describe(READ_TERMINAL_TOOL);
        assert!(
            read.contains("WAIT FIRST, THEN READ"),
            "read_terminal must state the order it exists to teach: {read}"
        );

        // And `send_keys` — the tool that submits a prompt to an agent — hands over to the wait
        // rather than leaving the caller reading in a loop for an answer that is not there yet.
        let keys = describe(SEND_KEYS_TOOL);
        assert!(
            keys.contains(WAIT_FOR_TERMINAL_TOOL),
            "send_keys must point at the wait that follows a submitted prompt: {keys}"
        );
    }

    /// Send a raw HTTP request to `127.0.0.1:<port>` and return the (status_line,
    /// body) pair. A tiny hand-rolled client so the test needs no HTTP client dep.
    fn http_request(port: u16, method: &str, path: &str, body: Option<&str>) -> (String, String) {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect loopback");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let body = body.unwrap_or("");
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        read_http_response(&mut stream)
    }

    /// Read a whole HTTP response off `stream` and return its `(status_line, body)`.
    ///
    /// ON BYTES, NEVER THROUGH A `String`. This client used to `read_to_string` the whole
    /// exchange, which asks the RAW STREAM — headers, chunk-size lines and payload interleaved —
    /// to be valid UTF-8 on its own. It is not, as soon as the response is chunked and a chunk
    /// boundary lands INSIDE a multi-byte character: the two halves end up separated by a
    /// `\r\n<hex>\r\n` frame and the stream stops being decodable. Nothing in the catalogue's
    /// content is at fault — a descriptor added anywhere shifts every later boundary — so the test
    /// went red on the LENGTH of a tool description, saying nothing whatever about the code (and
    /// `dechunk` would then have panicked slicing at a non-char boundary anyway). Framing is
    /// decoded on bytes; UTF-8 is asserted ONCE, on the reassembled body, where it is real.
    ///
    /// ONE reader, shared by `http_request` and `http_post_mcp`: the two hand-rolled clients differ
    /// only in the request they write, so the response side has a single definition to keep right.
    fn read_http_response(stream: &mut TcpStream) -> (String, String) {
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let (head, body) = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
            Some(i) => (&raw[..i], &raw[i + 4..]),
            None => (&raw[..], &[][..]),
        };
        // The head is ASCII by protocol; `from_utf8_lossy` keeps a malformed one readable
        // in a failure message rather than panicking on it.
        let head = String::from_utf8_lossy(head).into_owned();
        let status_line = head.lines().next().unwrap_or("").to_string();
        // tiny_http streams a large response with `Transfer-Encoding: chunked` (no
        // Content-Length) once the body exceeds its buffering threshold — which the full
        // `tools/list` catalog now does. De-chunk here so this hand-rolled client returns
        // the raw JSON body regardless of the framing (a Content-Length body has no
        // chunk header and is returned verbatim).
        let is_chunked = head.lines().any(|l| {
            let l = l.to_ascii_lowercase();
            l.starts_with("transfer-encoding:") && l.contains("chunked")
        });
        let payload = if is_chunked {
            dechunk(body)
        } else {
            body.to_vec()
        };
        (
            status_line,
            String::from_utf8(payload).expect("the DECODED body is valid UTF-8"),
        )
    }

    /// Decode an HTTP/1.1 `Transfer-Encoding: chunked` body into its payload: repeatedly
    /// read a hex chunk-size line, then that many BYTES, until the terminating `0` chunk.
    /// A tiny decoder (the test bodies are small JSON) so the test HTTP client needs no
    /// HTTP crate.
    ///
    /// It works on `&[u8]` and not on `&str` for the reason spelled out in
    /// [`read_http_response`]: a chunk boundary is a BYTE boundary, not a character one, so
    /// slicing the frames out of a `&str` would either panic (`&s[..n]` off a char
    /// boundary) or never get there (the stream would not decode in the first place).
    fn dechunk(mut s: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        // The loop ends the moment the remaining input no longer starts with a
        // `size-line CRLF` header — the same exit the previous `let-else break`
        // expressed — while the inner `break`s (terminating `0` chunk, truncated
        // body) still short-circuit before the next iteration.
        while let Some(i) = s.windows(2).position(|w| w == b"\r\n") {
            // The chunk-size line may carry `;ext` extensions — take the hex prefix only.
            let size_line = String::from_utf8_lossy(&s[..i]).into_owned();
            let hex = size_line.split(';').next().unwrap_or("").trim();
            let n = usize::from_str_radix(hex, 16).unwrap_or(0);
            let rest = &s[i + 2..];
            if n == 0 {
                break;
            }
            if rest.len() < n {
                out.extend_from_slice(rest);
                break;
            }
            out.extend_from_slice(&rest[..n]);
            // Skip the chunk's trailing CRLF before the next size line.
            s = rest[n..].strip_prefix(&b"\r\n"[..]).unwrap_or(&rest[n..]);
        }
        out
    }

    /// Start a server on an OS-CHOSEN port and wait until it accepts connections, so the tests
    /// neither race the accept loop's bind nor share a number: several `cargo test` processes
    /// run at once here, and a hard-coded port made every server-backed test in the second one
    /// fail with `Address already in use` — a red suite that says nothing about the code, which
    /// is how a real failure gets dismissed as noise. Returns the server and its bound port.
    fn start_test_server() -> (Arc<McpServer>, u16) {
        let server = Arc::new(McpServer::default());
        let port = server.start_ephemeral().expect("server starts");
        // Spin until the listener is actually accepting.
        for _ in 0..100 {
            if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        (server, port)
    }

    /// POST a body to `/mcp`, OPTIONALLY carrying the `X-Nyx-Terminal-Id` header — the
    /// production auto-map seam (`Some(id)` sends the header, `None` omits it entirely).
    /// The `http_request` helper hard-codes only `Host`, so this variant is the only way a
    /// test can drive the real header→caller extraction path in `handle_request`. Returns
    /// `(status_line, body)`, de-chunking like `http_request`.
    fn http_post_mcp(port: u16, body: &str, terminal_id: Option<&str>) -> (String, String) {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect loopback");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let terminal_header = terminal_id
            .map(|t| format!("{TERMINAL_ID_HEADER}: {t}\r\n"))
            .unwrap_or_default();
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n{terminal_header}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        // Bytes, and the SAME reader as `http_request` — see [`read_http_response`].
        read_http_response(&mut stream)
    }

    #[test]
    fn resolve_port_defaults_and_honors_env() {
        let _g = ENV_LOCK.lock().unwrap();
        // The v2 default is 8766 — a STABLE port DISTINCT from the v1 cockpit's 8765,
        // so v1 and v2 coexist without an `AddrInUse` bind clash (ADR-0003 D2).
        assert_eq!(
            DEFAULT_PORT, 8766,
            "v2 default port is 8766 (distinct from v1)"
        );
        assert_ne!(DEFAULT_PORT, 8765, "v2 must NOT share v1's port");
        std::env::remove_var(PORT_ENV);
        assert_eq!(resolve_port(), DEFAULT_PORT, "default when unset");
        std::env::set_var(PORT_ENV, "19001");
        assert_eq!(resolve_port(), 19001, "honors a valid override");
        // Port 0 ("any free port") is rejected → stays stable on the default.
        std::env::set_var(PORT_ENV, "0");
        assert_eq!(resolve_port(), DEFAULT_PORT, "rejects port 0 for stability");
        std::env::set_var(PORT_ENV, "not-a-port");
        assert_eq!(resolve_port(), DEFAULT_PORT, "rejects garbage");
        std::env::remove_var(PORT_ENV);
    }

    #[test]
    fn binds_loopback_and_serves_health() {
        let (server, port) = start_test_server();
        let (status, body) = http_request(port, "GET", "/health", None);
        assert!(status.contains("200"), "health 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["port"], port);
        assert!(server.is_started());
    }

    #[test]
    fn mcp_initialize_handshake() {
        let (_server, port) = start_test_server();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let (status, body) = http_request(port, "POST", "/mcp", Some(req));
        assert!(status.contains("200"), "initialize 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(v["result"]["protocolVersion"].is_string());
    }

    #[test]
    fn initialize_returns_consumer_facing_instructions() {
        // The handshake must carry a non-empty, consumer-facing `instructions` string
        // (MCP spec initialize result) telling an agent what nyx is and when to use it,
        // and it must be free of internal/dev jargon.
        let (_server, port) = start_test_server();
        let req = r#"{"jsonrpc":"2.0","id":9,"method":"initialize","params":{}}"#;
        let (status, body) = http_request(port, "POST", "/mcp", Some(req));
        assert!(status.contains("200"), "initialize 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        let instructions = v["result"]["instructions"]
            .as_str()
            .expect("initialize result carries an `instructions` string");
        assert!(
            !instructions.trim().is_empty(),
            "instructions must be non-empty"
        );
        // Consumer-facing content: states what nyx is and points at its core verbs.
        let lc = instructions.to_lowercase();
        assert!(lc.contains("nyx"), "instructions name the product");
        assert!(
            lc.contains("project") && lc.contains("workspace") && lc.contains("command"),
            "instructions cover nyx's core concepts (projects/workspaces/commands)"
        );
        assert_no_internal_jargon("instructions", instructions);
        // GUARD (FEEDBACK #4): Claude Code TRUNCATES a server's `instructions` at ~2000
        // chars when ingesting them into the agent's context. A prior over-long version
        // (2610 chars) was cut mid-sentence, LOSING the tail — including this very "Do NOT
        // use nyx" guidance. Keep the instructions comfortably under that budget; put
        // detailed/edge-case guidance in the per-TOOL descriptions (which are NOT capped).
        let chars = instructions.chars().count();
        assert!(
            chars <= 2000,
            "instructions must stay under Claude Code's ~2000-char ingest cap (was {chars}); \
             move detail into tool descriptions"
        );
    }

    /// REVIEW FINDING 01M1C7F95Q05XQRR6FZTK71BMG — the SYSTEM scope was UNDISCOVERABLE.
    ///
    /// It is deliberately absent from `list_projects` and `list_workspaces` ("discovery gets its
    /// own door"), and the server's instructions — the first and often only thing a model reads
    /// about nyx — never mentioned that the door existed. On a client with deferred tool schemas
    /// (the common case), the agent saw the NAME `list_system_commands` among a hundred others and
    /// loaded it only because the user happened to say the words. Two sentences close that.
    #[test]
    fn the_instructions_name_the_system_scope_its_door_and_its_approval() {
        let lc = SERVER_INSTRUCTIONS.to_lowercase();
        assert!(
            lc.contains("system"),
            "the instructions must say the scope exists"
        );
        assert!(
            SERVER_INSTRUCTIONS.contains(LIST_SYSTEM_COMMANDS_TOOL),
            "…and name the tool that reaches it, since nothing else leads there"
        );
        assert!(
            lc.contains("approve") || lc.contains("approval"),
            "…and say that changing them needs the user"
        );
    }

    /// REVIEW FINDING 01M1C7FNAXYWVQ34R2ZD3DKMGS — nothing said that `add_command` is the CREATION
    /// door of the system scope, and `list_system_commands` returned a `scope.project_id` without
    /// saying what it was for. The agent that tested had to DEDUCE the link; a deduction that
    /// happens to be right is not a contract.
    ///
    /// Answered with a sentence in each description rather than a dedicated `add_system_command`
    /// tool: the guard matrix already routes by project id, so a second tool would duplicate a
    /// surface — and duplicated surfaces drift.
    #[test]
    fn the_command_tools_say_how_the_system_scope_is_addressed() {
        let descriptors = tool_descriptors();
        let describe = |name: &str| -> String {
            descriptors
                .iter()
                .find(|t| t["name"] == json!(name))
                .unwrap_or_else(|| panic!("`{name}` is advertised"))["description"]
                .as_str()
                .expect("a description")
                .to_string()
        };

        let add = describe(ADD_COMMAND_TOOL);
        assert!(
            add.contains("scope.project_id") && add.contains(LIST_SYSTEM_COMMANDS_TOOL),
            "add_command must say WHICH id turns it into the system-scope creation door: {add}"
        );
        assert!(
            add.to_lowercase().contains("approve"),
            "…and that the user is asked: {add}"
        );

        let list = describe(LIST_SYSTEM_COMMANDS_TOOL);
        assert!(
            list.contains("scope.project_id") && list.contains(ADD_COMMAND_TOOL),
            "list_system_commands must say what the id it returns is FOR: {list}"
        );
    }

    /// Internal/dev jargon markers that must never appear in agent-facing strings
    /// (the `instructions` and every advertised tool description). These are words
    /// written for nyx developers, not the consuming agent.
    ///
    /// NOTE: `SessionStart`/`SessionEnd` are deliberately NOT blacklisted — they are
    /// Claude Code's PUBLIC hook event names, i.e. the literal `hook_event_name`
    /// values an agent forwards to the PRD-5 `agent_session_event` tool. Naming them
    /// in that tool's description is agent-facing guidance, not internal dev-speak.
    fn assert_no_internal_jargon(what: &str, text: &str) {
        let lc = text.to_lowercase();
        for marker in ["prd", "spike", "adr-", "dogfood", "phase-2", "phase 2"] {
            assert!(
                !lc.contains(marker),
                "{what} leaks internal jargon marker {marker:?}: {text}"
            );
        }
    }

    #[test]
    fn advertised_tool_descriptions_are_agent_facing() {
        // Every advertised tool MUST carry a non-empty description, free of internal
        // jargon (no PRD / spike / SessionStart / ADR / dogfood / phase-2 dev-speak),
        // while preserving the useful instance_id-vs-command_id guidance for the agent.
        let (_server, port) = start_test_server();
        let req = r#"{"jsonrpc":"2.0","id":10,"method":"tools/list"}"#;
        let (_status, body) = http_request(port, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        let tools = v["result"]["tools"].as_array().expect("tools array");
        for tool in tools {
            let name = tool["name"].as_str().expect("tool name");
            let desc = tool["description"]
                .as_str()
                .unwrap_or_else(|| panic!("tool {name} must advertise a description"));
            assert!(
                !desc.trim().is_empty(),
                "tool {name} description must be non-empty"
            );
            assert_no_internal_jargon(&format!("tool {name} description"), desc);
        }
        // The instance_id vs command_id guidance is preserved (in agent terms): the
        // tools that act on an instance mention instance_id, and list_commands explains
        // command templates are not launchable.
        let desc_of = |n: &str| -> String {
            tools
                .iter()
                .find(|t| t["name"] == n)
                .and_then(|t| t["description"].as_str())
                .unwrap_or("")
                .to_lowercase()
        };
        assert!(
            desc_of("start_command").contains("instance_id"),
            "start_command keeps the instance_id guidance"
        );
        assert!(
            desc_of("list_commands").contains("instance_id"),
            "list_commands explains where instance_id comes from"
        );
        // D2: the two workspace tools must CONTRAST clearly — workspace_add registers an
        // existing folder, create_workspace creates a new one. Each names its own
        // intent and points at the sibling so an agent picks the right one.
        let add = desc_of("workspace_add");
        let create = desc_of("create_workspace");
        assert!(
            add.contains("existing") && add.contains("create_workspace"),
            "workspace_add says it registers an EXISTING folder and points at create_workspace, got: {add}"
        );
        assert!(
            create.contains("create") && create.contains("workspace_add"),
            "create_workspace says it CREATES a folder and points at workspace_add, got: {create}"
        );
        // v2.5: BOTH workspace tools advertise the OPTIONAL `spec_id` anchor — declared in
        // `properties` but ABSENT from `required`, so the pre-v2.5 call shape stays valid —
        // AND they announce what `spec_id` adds to the RESULT, since a tools/list reader has
        // no other way to learn that the anchored Spec comes back with the workspace.
        for name in ["workspace_add", "create_workspace"] {
            let desc = desc_of(name);
            assert!(
                desc.contains("spec_id") && desc.contains("under `spec`"),
                "{name} announces the conditional `spec` key of its RESULT, got: {desc}"
            );
            let schema = tools
                .iter()
                .find(|t| t["name"] == name)
                .map(|t| &t["inputSchema"])
                .unwrap_or_else(|| panic!("{name} is advertised"));
            assert!(
                schema["properties"]["spec_id"].is_object(),
                "{name} advertises the optional spec_id anchor"
            );
            let required = schema["required"].as_array().expect("required array");
            assert!(
                !required.iter().any(|r| r == "spec_id"),
                "{name} keeps spec_id OPTIONAL (the existing contract is unchanged without it)"
            );
        }
    }

    /// WHAT NYX ACCEPTS, NYX ADVERTISES — AND WHAT IT REFUSES, IT ADVERTISES TOO. An agent forms
    /// its call from these descriptions and from nothing else, and the defect this closes is
    /// exactly a surface that DISPLAYED `#3` while accepting only the canonical id — silently. So
    /// EVERY `spec_id`-shaped property (whatever tool, whatever the argument is called) must name
    /// the forms it takes, and must name them with a CONSTANT of [`crate::spec_ref`] rather than a
    /// hand-written paraphrase: a paraphrase is a second statement of the grammar, free to drift
    /// from the parser the day a form is added.
    ///
    /// THREE FAMILIES, AND THE SCHEMA ITSELF DECIDES WHICH. A `#N` / bare `N` is counted inside a
    /// project, and the project comes from the call's OWN anchors — so the family is decided by TWO
    /// facts the schema already carries: WHICH of `project_id` / `workspace_id` / `cwd` / `run_id`
    /// the tool exposes, and whether one of them is REQUIRED **and carries a project by
    /// construction**.
    ///   * A CERTAIN ANCHOR → the full [`crate::spec_ref::ACCEPTED_FORMS`] sentence, unconditional,
    ///     because the call can never arrive without a project.
    ///   * AN ANCHOR THAT MAY BE ABSENT → the four forms AND the condition, named
    ///     ([`CONDITIONAL_ANCHOR_CLAUSE`]), AND [`crate::spec_ref::ANCHORLESS_FORMS`] as the way
    ///     out. The same call honours `#N` with the anchor and cannot without it, and a form that
    ///     works or not depending on an unrelated argument is harder to diagnose than one that
    ///     never works. TWO WAYS TO BE ABSENT, and they land in the same family: the anchor may be
    ///     omitted, OR it may be required and still RESOLVE TO NOTHING. `run_bind_spec` is the
    ///     second shape — its required `run_id` names a run whose project comes from its WORKSPACE,
    ///     and `run_start` opens a run without one when its `cwd` matched none, which is the very
    ///     session that verb repairs. A required `cwd` would be that shape too: it resolves by
    ///     longest-prefix match over the workspace roots, and a valid path under none of them names
    ///     no project. Reading "required" as "certain" is what let it promise the four forms
    ///     unconditionally on a call that could not always keep them (finding
    ///     `01M0ZWAH9EQDAZR66AFS1YJFPY`; `01M1005GAY94XZ7BNKY4ASFTR5` for the `cwd` half).
    ///   * NO ANCHOR → `ANCHORLESS_FORMS`, and NOT the four-form sentence, because announcing a
    ///     form this tool must refuse is the original defect wearing another hat (finding
    ///     `01M0ZGCCSQ2F25HPBS6NX5JAFS`).
    ///
    /// READING THE FAMILY ON THE MERE PRESENCE OF AN ANCHOR IS WHAT LET TWO FIELDS THROUGH. That is
    /// how this test used to work, and under it `spec_set_workspace` and `agent_context` — whose
    /// only anchors are OPTIONAL — counted as anchored, so the test DEMANDED of them the very
    /// unconditional promise they could not keep (finding `01M0ZKWXS353TABS0WCT3XXB81`). Requiring
    /// the `required` array to name the anchor is what separates the two, and it is why the third
    /// family exists here rather than as a hand-kept exception beside a two-way test.
    ///
    /// Make one of the anchorless fields take a `cwd` and it moves to the CONDITIONAL family under
    /// this test, which then demands the condition; give it a REQUIRED `workspace_id` and it moves
    /// again, to the unconditional one — the advertisement follows the capability by construction,
    /// in all three directions. Marking that same `cwd` `required` does NOT make the last move, and
    /// that is the rule working: what promotes a field is an anchor that NAMES its project, not a
    /// schema keyword.
    ///
    /// THE SCAN ALSO SEES PLURAL FIELDS (`idea_add.spec_ids`): a field holding an ARRAY of
    /// references is a `spec_id` field with an `s`, and filtering on the singular alone left the
    /// 23rd field of the surface invisible to both halves of the lock (finding
    /// `01M0ZMGJG9VXD4GCY6Y6TN3RYS`).
    #[test]
    fn every_spec_id_argument_advertises_the_forms_it_accepts() {
        // The anchors `mcp_context::resolve_project` reads — the ONLY way a call can name a
        // project for a bare number to be counted in.
        const ANCHORS: [&str; 4] = ["project_id", "workspace_id", "cwd", "run_id"];
        // …and the subset that cannot END ON NOTHING — those that name their project
        // DIRECTLY. `project_id` IS a project; a `workspace_id` names a row that carries its
        // project in a column. The other two are LOOKUPS that can legitimately answer nothing, and
        // being REQUIRED does not change that:
        //   * a `cwd` resolves through `best_workspace_for_cwd`, a longest-prefix match over the
        //     workspace roots (`mcp_context.rs`, `resolve_project`) — a perfectly VALID path that
        //     sits under no workspace root resolves NO project, which is the very case `run_start`
        //     reports as `run_location_unresolved`;
        //   * a `run_id` names a run whose project is its WORKSPACE's, and `run_start` deliberately
        //     opens a run with no workspace when its `cwd` matched none — the session
        //     `run_bind_spec` exists to repair.
        // So requiring either leaves the project UNCERTAIN, and a field anchored on one alone
        // belongs to the CONDITIONAL family — the family rule read on what the anchor IS, not
        // merely on whether it is required (findings `01M0ZWAH9EQDAZR66AFS1YJFPY` for `run_id`,
        // `01M1005GAY94XZ7BNKY4ASFTR5` for `cwd`). This set and the `required_anchor` assertion
        // message below name the SAME anchors; when they disagreed, the message was the one telling
        // the truth.
        const ANCHORS_THAT_CARRY_A_PROJECT: [&str; 2] = ["project_id", "workspace_id"];
        let mut required_anchor = 0usize;
        let mut optional_anchor = 0usize;
        let mut anchorless = 0usize;
        for tool in tool_descriptors() {
            let name = tool["name"].as_str().unwrap_or_default().to_string();
            let Some(props) = tool["inputSchema"]["properties"].as_object() else {
                continue;
            };
            // A tool with no `required` array requires nothing — `run_start` is exactly that.
            let required: Vec<&str> = tool["inputSchema"]["required"]
                .as_array()
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let anchors_exposed: Vec<&str> = props
                .keys()
                .map(String::as_str)
                .filter(|k| ANCHORS.contains(k))
                .collect();
            let anchor_is_certain = anchors_exposed
                .iter()
                .any(|a| required.contains(a) && ANCHORS_THAT_CARRY_A_PROJECT.contains(a));
            for (arg, schema) in props {
                // Singular AND plural: `spec_id`, `from_spec_id`, `to_spec_id`, `spec_ids`.
                if !(arg.ends_with("spec_id") || arg.ends_with("spec_ids")) {
                    continue;
                }
                let desc = schema["description"].as_str().unwrap_or_default();
                if anchors_exposed.is_empty() {
                    assert!(
                        desc.contains(crate::spec_ref::ANCHORLESS_FORMS),
                        "{name}.{arg} anchors no project, so it must name the self-anchored forms \
                         it CAN resolve: {desc}"
                    );
                    assert!(
                        !desc.contains(crate::spec_ref::ACCEPTED_FORMS),
                        "{name}.{arg} advertises '#N' / 'N' on a call that can never count them — \
                         that promise is the defect, not the fix: {desc}"
                    );
                    assert!(
                        desc.contains("REFUSED"),
                        "{name}.{arg} must say the two other forms are refused, not leave an agent \
                         to discover it: {desc}"
                    );
                    anchorless += 1;
                } else if anchor_is_certain {
                    assert!(
                        desc.contains(crate::spec_ref::ACCEPTED_FORMS),
                        "{name}.{arg} always anchors a project, so it must name the four forms: \
                         {desc}"
                    );
                    assert!(
                        !desc.contains(CONDITIONAL_ANCHOR_CLAUSE),
                        "{name}.{arg} states a condition it does not have — one of {anchors_exposed:?} \
                         is REQUIRED and carries a project by construction, so the four forms \
                         always resolve here: {desc}"
                    );
                    required_anchor += 1;
                } else {
                    assert!(
                        desc.contains(crate::spec_ref::ACCEPTED_FORMS),
                        "{name}.{arg} CAN anchor a project, so it must name the four forms: {desc}"
                    );
                    assert!(
                        desc.contains(CONDITIONAL_ANCHOR_CLAUSE),
                        "{name}.{arg} announces '#N' / 'N' while no anchor it reads \
                         ({anchors_exposed:?}) is both REQUIRED and project-bearing — the CONDITION \
                         is part of the promise, or the promise is one the call cannot always \
                         keep: {desc}"
                    );
                    assert!(
                        desc.contains(crate::spec_ref::ANCHORLESS_FORMS)
                            && desc.contains("REFUSED"),
                        "{name}.{arg} must say what happens when the condition does not hold, and \
                         what to send instead: {desc}"
                    );
                    assert!(
                        anchors_exposed.iter().any(|a| desc.contains(a)),
                        "{name}.{arg} must NAME the anchor its short forms depend on (one of \
                         {anchors_exposed:?}) — a condition an agent cannot act on is not one: \
                         {desc}"
                    );
                    optional_anchor += 1;
                }
            }
        }
        assert_eq!(
            anchorless, 13,
            "the anchorless `spec_id` FIELDS (fewer tools than fields — `spec_link` and \
             `spec_unlink` carry two each) are the ones the finding enumerates — a change in that \
             count is a change of contract, not a detail. It went from 12 to 13 with \
             `spec_set_category` (PDR-0017), whose only other arguments name a CATEGORY: no \
             project_id / workspace_id / cwd / run_id, so a bare number has nothing to be counted in"
        );
        assert_eq!(
            optional_anchor, 7,
            "the CONDITIONAL fields — every one whose project can be absent at call time, whether \
             because its anchor may be omitted (`spec_update_section`, `spec_set_workspace`, \
             `agent_context`, `run_start`, and the v2.8 reads `spec_get` / `task_list`, whose \
             `project_id` exists only to let a '#N' resolve) or because the anchor it REQUIRES may \
             itself resolve to nothing (`run_bind_spec`, whose `run_id` names a run that may carry \
             no workspace). One arriving here without its condition is exactly what this family \
             exists to catch"
        );
        assert_eq!(
            required_anchor, 8,
            "the fields whose call REQUIRES an anchor that carries a project BY CONSTRUCTION — a \
             `project_id`, or a `workspace_id` that has one in a column — so a number always has a \
             project to be counted in there. The singular ones plus `idea_add.spec_ids`; the scan \
             must actually reach the plural too. It went from 7 to 8 when \
             `review_comment_list.spec_id` was finally ADVERTISED: the call had honoured it since \
             v2.4 while the schema named it nowhere, so this very scan — which reads the schema — \
             could not see it (`01M1008NHM9RRA1ZAS4V7K1PM4` on a read)"
        );
    }

    /// ONE MEMBER OF THE CONDITIONAL FAMILY, pinned on its own because the BEHAVIOUR its sentence
    /// promises is pinned right next to it (`mcp_tools_spec::tests`), and the two halves are worth
    /// reading together. HOW MANY members that family has is not said here and never should be: the
    /// count lives in the assertion that fails when it drifts
    /// ([`tests::every_spec_id_argument_advertises_the_forms_it_accepts`](mod@tests)'s
    /// `optional_anchor`), and a number repeated in prose beside it is a number nothing checks —
    /// which is how "the four conditional fields" survived the commit that made them five (finding
    /// `01M10059A8ZKK4961GYPNERT07`).
    ///
    /// `spec_update_section` carries exactly one project anchor and it is OPTIONAL (`run_id`, the
    /// human-gated correction path), so the same call honours `#N` with one and refuses it without.
    /// That is a form which works or not depending on an argument unrelated to it — harder to
    /// diagnose than one that never works — so the CONDITION itself is part of the advertisement
    /// (finding `01M0ZGHD137MDXFGS2PBG6GS68`). The family as a whole is held by
    /// [`tests::every_spec_id_argument_advertises_the_forms_it_accepts`](mod@tests).
    #[test]
    fn spec_update_section_states_the_condition_its_short_forms_depend_on() {
        let tools = tool_descriptors();
        let desc = tools
            .iter()
            .find(|t| t["name"] == crate::mcp_tools_spec::SPEC_UPDATE_SECTION_TOOL)
            .map(|t| t["inputSchema"]["properties"]["spec_id"]["description"].clone())
            .expect("spec_update_section is advertised");
        let desc = desc.as_str().unwrap_or_default();
        assert!(
            desc.contains("run_id"),
            "the condition is NAMED, not left to be discovered: {desc}"
        );
        assert!(
            desc.contains(crate::spec_ref::ANCHORLESS_FORMS),
            "and the sentence says what to send when the condition does not hold: {desc}"
        );
    }

    /// THE ONE WAY THIS MODULE'S SOURCE SCANS READ A `const` DECLARATION. Several guards here pin a
    /// descriptor against the constants a sibling module declares, by reading that module's own
    /// source — and every one of them must agree on what a declaration LOOKS like.
    ///
    /// NEITHER VISIBILITY NOR INDENTATION IS PART OF THE PATTERN: `const`, `pub const` and
    /// `pub(crate) const` all read the same. Promoting a code to `pub(crate)` so a test can pin it
    /// by name — precisely what `mcp_engine.rs`'s `RUN_LOCATION_UNRESOLVED_WARNING_CODE` was put
    /// through, and what `mod bind_refusal` was put through right after — must not be the move that
    /// drops it out of a scan. `mcp_engine.rs` already mixes the two forms on one family of
    /// constants, so a pattern keyed on `"pub const "` is not a hypothetical gap (finding
    /// `01M10YD9QZYJJDAQQ1S8TGWSKH`).
    ///
    /// Returns what FOLLOWS the keyword — the name, type and initialiser — leaving each caller to
    /// say which declarations it is after and how their literal is spelled.
    fn const_declaration(line: &str) -> Option<&str> {
        let (vis, rest) = line.trim_start().split_once("const ")?;
        let vis = vis.trim_end();
        (vis.is_empty() || vis == "pub" || (vis.starts_with("pub(") && vis.ends_with(')')))
            .then_some(rest)
    }

    /// DONE-CRITERION (task `01KYA4D8ZWDK8D0H28PVRE5PGJ`): the surfaces an agent reads BEFORE it
    /// calls anything — the tool descriptors — must state the resolution contract EXACTLY, because
    /// a descriptor that leaves it implicit is how "nyx probably picks the one I mean" gets
    /// believed. What is asserted, on the tools concerned:
    ///   * the ANCHORING tools bound the link to `ready`/`in_progress` and promise NO status change
    ///     (and the corrective `spec_set_workspace` says a draft is refused);
    ///   * the RESOLVING tools spell out 0/1/N — the ambiguity `reason`, the candidate list, and
    ///     the explicit denial that the most recent Spec wins;
    ///   * `run_start` documents the AUTHORING exception: unbound + a warning, never an arbitrary
    ///     Spec;
    ///   * and the ZERO case speaks by CODE on both tools — `agent_context` names every case-0
    ///     code it can emit (scanned from `mcp_context.rs`, so a new one added mute goes red) and
    ///     `run_start` names `run_location_unresolved`.
    #[test]
    fn the_spec_anchor_and_resolution_tools_document_the_contract_they_enforce() {
        let tools = tool_descriptors();
        let desc_of = |n: &str| -> String {
            tools
                .iter()
                .find(|t| t["name"] == n)
                .and_then(|t| t["description"].as_str())
                .unwrap_or_else(|| panic!("{n} is advertised"))
                .to_lowercase()
        };

        // ANCHORING — bounded to ready/in_progress, and never a transition.
        for name in ["workspace_add", "create_workspace"] {
            let desc = desc_of(name);
            assert!(
                desc.contains("ready/in_progress") || desc.contains("ready or in_progress"),
                "{name} bounds the anchor to the two anchorable statuses, got: {desc}"
            );
            assert!(
                desc.contains("status is never changed"),
                "{name} promises the link poses no transition, got: {desc}"
            );
        }
        let set_ws = desc_of(crate::mcp_tools_spec::SPEC_SET_WORKSPACE_TOOL);
        assert!(
            set_ws.contains("ready or in_progress") && set_ws.contains("never changes"),
            "spec_set_workspace states the same two facts, got: {set_ws}"
        );
        assert!(
            set_ws.contains("draft") && set_ws.contains("refused"),
            "spec_set_workspace says a draft Spec is REFUSED (no anchoring an unfinished Spec), \
             got: {set_ws}"
        );

        // RESOLVING — the 0/1/N contract, named, with the recency pick explicitly denied.
        for name in [
            crate::mcp_context::AGENT_CONTEXT_TOOL,
            crate::mcp_engine::RUN_START_TOOL,
        ] {
            let desc = desc_of(name);
            assert!(
                desc.contains("spec_ambiguous_for_workspace"),
                "{name} names the machine-readable ambiguity reason, got: {desc}"
            );
            assert!(
                desc.contains("never picks the most recent"),
                "{name} denies the newest-wins reading outright, got: {desc}"
            );
        }

        // …and the AUTHORING exception, which only run_start has.
        let run_start = desc_of(crate::mcp_engine::RUN_START_TOOL);
        assert!(
            run_start.contains("spec_ambiguous_started_unbound")
                && run_start.contains("starts unbound"),
            "run_start documents that an authoring flow proceeds unbound with a warning, got: \
             {run_start}"
        );

        // THE CASE-0 CODES of `agent_context` — the OTHER end of the same 0/1/N contract, and the
        // one an agent meets on its nominal path (a first `/nspec` has no Spec yet).
        //
        // The list is NOT hand-kept here. It is SCANNED out of `mcp_context.rs`, whose
        // `*_WARNING_CODE` constants ARE that channel — so a NEW code declared there without a
        // word in the descriptor turns this test red, which is exactly the failure being closed: an
        // enumeration that names all but one of its codes reads as EXHAUSTIVE, and an agent meeting
        // the missing one concludes nyx is broken rather than that its run is simply unbound.
        //
        // The reading is [`const_declaration`]'s — whatever the visibility, whatever the
        // indentation. And the scan AUDITS ITS OWN READING: every line declaring a
        // `*_WARNING_CODE` must also yield a literal, so a declaration this pattern cannot parse
        // (wrapped onto two lines, say) fails the count instead of vanishing from it in silence.
        const CONTEXT_SRC: &str =
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_context.rs"));
        let agent_ctx = desc_of(crate::mcp_context::AGENT_CONTEXT_TOOL);
        let mut declared = 0usize;
        let mut scanned: Vec<&str> = Vec::new();
        for line in CONTEXT_SRC.lines() {
            let Some(rest) = const_declaration(line) else {
                continue;
            };
            if !rest.contains("_WARNING_CODE") {
                continue;
            }
            declared += 1;
            let Some((_, literal)) = rest.split_once("_WARNING_CODE: &str = \"") else {
                continue;
            };
            let code = literal.trim_end_matches("\";").trim_end_matches('"');
            assert!(
                agent_ctx.contains(code),
                "agent_context must NAME the warning code '{code}' it can emit — a descriptor that \
                 lists only some of its codes is read as the whole list, got: {agent_ctx}"
            );
            scanned.push(code);
        }
        assert_eq!(
            scanned.len(),
            declared,
            "every `*_WARNING_CODE` DECLARED in mcp_context.rs was actually read by the scan — a \
             declaration whose literal the pattern cannot reach must fail here rather than slip \
             through unconfronted; {declared} declared, read {scanned:?}"
        );
        assert_eq!(
            scanned.len(),
            3,
            "the scan really found mcp_context.rs's 3 case-0 codes (a pattern that silently matched \
             nothing would make the loop above vacuous); got {scanned:?}"
        );

        // run_start's OWN location code — the run-side half of the same review finding: a `cwd`
        // matching no workspace opens a run with no anchor and effectors with no repos.
        //
        // Pinned by NAME only, not by the same scan: `mcp_engine.rs` declares warning codes of its
        // own, and several are covered by the descriptor as a CATEGORY rather than by their code (a
        // stale graph fork, a query on an unbound run). Widening the rule to that module is a
        // separate decision from this PRD's.
        assert!(
            run_start.contains(crate::mcp_engine::RUN_LOCATION_UNRESOLVED_WARNING_CODE),
            "run_start's description must NAME the code '{}' — this pins the code's PRESENCE in the \
             description, not the section of it that carries the code, got: {run_start}",
            crate::mcp_engine::RUN_LOCATION_UNRESOLVED_WARNING_CODE
        );
        // …and the RESUME code, pinned the same way and for the same reason: renaming the constant
        // without re-wording the description would leave `tools/list` naming a code no warning
        // carries. This pins THE CODE'S PRESENCE in the description — nothing about the sentence
        // around it, which is free to be re-written.
        assert!(
            run_start.contains(crate::mcp_engine::RUN_RESUMED_WARNING_CODE),
            "run_start's description must NAME the code '{}' — same pin, same reserve: the sentence \
             around it is free to be re-written, got: {run_start}",
            crate::mcp_engine::RUN_RESUMED_WARNING_CODE
        );
    }

    /// `outcome` IS A CONTRACT, and `tools/list` is where a caller reads what it may hold.
    /// `run_start` returns one of [`crate::mcp_engine::RUN_START_OUTCOME_TAGS`] and its description
    /// spells all three out — two spellings of the same three strings, with nothing but this test
    /// between them. Renaming a tag on the enum alone would leave the descriptor announcing values
    /// no run ever returns, which is the `01M1008NHM9RRA1ZAS4V7K1PM4` defect on the return side.
    ///
    /// THE LITERALS STAY IN THE DESCRIPTION, on the model of `run_bind_spec`'s refusal codes just
    /// above: they read as prose there, and what keeps them honest is a lock, not a `format!` over a
    /// paragraph. What is walked is the CONSTANT LIST against the rendered text — so a tag added to
    /// [`crate::mcp_engine::RUN_START_OUTCOME_TAGS`], or renamed on it, fails here rather than
    /// shipping with a descriptor that names something else.
    ///
    /// AND NO FURTHER, which is why the name says "listed". Nothing binds that list to the enum: a
    /// fourth VARIANT given a tag but not a slot on the list compiles and passes HERE. What stands
    /// in the way is a second, exhaustive match —
    /// `mcp_engine::tests::every_run_start_outcome_carries_a_listed_tag` — which stops COMPILING on
    /// a fourth variant and states the rule where its author lands. The array's own doc carries why
    /// that is a visit rather than a derivation.
    #[test]
    fn run_start_advertises_every_listed_outcome_tag() {
        let tools = tool_descriptors();
        let run_start = tools
            .iter()
            .find(|t| t["name"] == crate::mcp_engine::RUN_START_TOOL)
            .and_then(|t| t["description"].as_str())
            .expect("run_start is advertised");
        for tag in crate::mcp_engine::RUN_START_OUTCOME_TAGS {
            assert!(
                run_start.contains(&format!("'{tag}'")),
                "run_start's description must name the outcome tag '{tag}' it can return, got: \
                 {run_start}",
            );
        }
    }

    /// A REFUSAL MAY ONLY ADVISE AN ARGUMENT THE VERB ADVERTISES (finding
    /// `01M1008NHM9RRA1ZAS4V7K1PM4`). When a `#N` resolves no project, `run_start` tells the caller
    /// to ADD an anchor — and the entire value of that sentence is that the caller can act on it.
    /// What a caller reads is `tools/list`, not `mcp_context::resolve_project`: `project_id` was
    /// honoured at runtime and named in the trailer while ABSENT from the schema, which is the very
    /// defect `run_bind_spec` had just been repaired for — on the verb that repair cited as the
    /// honest one.
    ///
    /// THE LOCK READS THE CONSTANT, NOT A COPY OF IT
    /// ([`crate::mcp_engine::RUN_START_ANCHOR_TRAILER`]), and it DERIVES the names the trailer
    /// carries instead of testing it against names this test already knows: a hand-kept vocabulary
    /// only catches a re-wording that stays inside it, so `"…a cwd / workspace_id / project_key"`
    /// would have passed green while sending an agent after an argument `run_start` does not take
    /// (finding `01M10YE1HTXTA76CFJMW1G4XHZ`). The domain carries both words — `project_id`, and
    /// the KEY of a `KEY-N` — so that confusion is not hypothetical.
    ///
    /// THE DERIVATION HAS A SHAPE, AND THE SHAPE IS ASSERTED FIRST. The advice is a slash-separated
    /// list of argument names, one per segment; re-write it as prose and the shape assertion goes
    /// red rather than the derivation quietly reading nothing. That is the whole reach of this
    /// guard, stated so it is not read as more: every name the trailer advertises IN THAT SHAPE
    /// must be a key of `run_start`'s own `inputSchema.properties`, and any other shape has to come
    /// back through this test.
    /// **`node_add`'s CONFIG DESCRIPTOR ANNOUNCES EVERY FIELD `gate_check` ACCEPTS** — derived from
    /// the authoring side's own list, not re-typed beside it.
    ///
    /// WHY THIS ONE TYPE GETS A GUARD. The descriptor's sentence opens with "unknown fields are
    /// REFUSED" and then ENUMERATES the fields of each type, so for a `tools/list` reader that
    /// enumeration IS the contract: a field missing from it reads as a field the tool will reject.
    /// `expect` shipped validated ([`crate::mcp_authoring::known_config_fields`], and a value
    /// outside the vocabulary is refused with the two words spelled out) and UNANNOUNCED — an agent
    /// reading the descriptor could not discover the one field that declares a reproduction.
    ///
    /// WHAT IT REACHES, stated so it is not read as more: the `gate_check→{…}` enumeration, against
    /// [`crate::mcp_authoring::known_config_fields`] for that type. Other types' enumerations are
    /// partial by inheritance (`set_category`, `query`, `sub_workflow` and others are absent
    /// entirely) and that pre-existing debt is not what this guard is about; extend it to them the
    /// day their enumerations are completed.
    #[test]
    fn node_add_announces_every_config_field_a_gate_check_accepts() {
        let tools = tool_descriptors();
        let desc = tools
            .iter()
            .find(|t| t["name"] == crate::mcp_authoring::NODE_ADD_TOOL)
            .and_then(|t| t["inputSchema"]["properties"]["config"]["description"].as_str())
            .expect("node_add advertises a config description")
            .to_string();
        // The `type→{fields}` shape the descriptor writes its enumerations in — asserted before it
        // is used, so a re-write into prose fails HERE instead of leaving the loop below vacuous.
        let announced = desc
            .split("gate_check→{")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect(
                "the descriptor enumerates gate_check's fields in the `gate_check→{…}` shape this \
                 guard derives them from",
            )
            .to_string();
        for field in crate::mcp_authoring::known_config_fields(crate::db::NODE_TYPE_GATE_CHECK) {
            assert!(
                announced.contains(field),
                "the authoring side accepts a 'gate_check' config field '{field}' the descriptor \
                 does not announce — and the same sentence says unknown fields are REFUSED, so a \
                 reader concludes it may not use it: {announced}",
            );
        }
        // …and `expect`'s VOCABULARY is spelled out, since knowing the field exists without knowing
        // which two words it takes is not a discoverable field.
        assert!(
            desc.contains(crate::db::GATE_EXPECT_RED)
                && desc.contains(crate::db::GATE_EXPECT_GREEN)
                && desc.contains("the default when the field is absent"),
            "the descriptor states what `red` and `green` mean and that absent means green: {desc}",
        );
    }

    /// **`spec_set_category` ANNOUNCES THE CATEGORY OBJECT IT ACTUALLY RETURNS** — derived from
    /// [`crate::mcp_tools_spec::category_payload`], not re-typed beside it.
    ///
    /// WHY THIS DESCRIPTOR GETS A GUARD, on the model of
    /// [`node_add_announces_every_config_field_a_gate_check_accepts`] just above. The sentence
    /// enumerates the response's category fields, so for a `tools/list` reader that enumeration IS
    /// the contract. It shipped announcing `state: proposed | confirmed` — a key
    /// [`crate::mcp_tools_spec::category_payload`] never emitted after the 2026-09-02 amendment of
    /// PDR-0017 retired the proposed/confirmed axis, so an agent branching on `category.state` read
    /// `undefined` (finding `01M1HB6GZD0EZC6NBAFDR2GV4M`). Walking the payload's OWN keys is what
    /// makes the next removal fail here instead of shipping.
    ///
    /// ONE KEY IS DELIBERATELY NOT ANNOUNCED, and it is named rather than silently skipped:
    /// `confirmed_at` is the VESTIGIAL column ([`crate::db::Category::confirmed_at`]) — always set,
    /// read by nobody, slated to be dropped the next time `categories` is migrated. The payload
    /// mirrors the row rather than hiding half of it; the DESCRIPTOR is an agent's contract, and
    /// advertising a field whose only meaning is "ignore me" is how the axis got re-derived in the
    /// first place. A key added to the payload for any other reason lands in the loop below.
    ///
    /// AND THE RETIRED AXIS STAYS OUT OF EVERY `spec_*` DESCRIPTOR — description and schema alike.
    /// The four surviving mentions were spread over `spec_create` and `spec_set_category`, so the
    /// second half of this guard sweeps the whole family rather than the one verb that was fixed.
    #[test]
    fn spec_set_category_announces_the_category_object_it_returns() {
        let tools = tool_descriptors();
        let desc = tools
            .iter()
            .find(|t| t["name"] == crate::mcp_tools_spec::SPEC_SET_CATEGORY_TOOL)
            .and_then(|t| t["description"].as_str())
            .expect("spec_set_category is advertised")
            .to_string();
        let sample = crate::db::Category {
            id: "cat_1".to_string(),
            project_id: "prj_1".to_string(),
            slug: "dette-technique".to_string(),
            name: "Dette technique".to_string(),
            color_token: Some("group-3".to_string()),
            position: 4,
            created_by_type: crate::db::CATEGORY_CREATED_BY_AGENT.to_string(),
            confirmed_at: Some(1),
            created_at: 1,
            updated_at: 1,
        };
        let payload = crate::mcp_tools_spec::category_payload(&sample);
        let keys = payload.as_object().expect("the payload is an object");
        for key in keys.keys() {
            // The vestigial column — carried by the payload, deliberately unadvertised (see above).
            if key == "confirmed_at" {
                continue;
            }
            assert!(
                desc.contains(key.as_str()),
                "spec_set_category returns a category field '{key}' its description does not \
                 announce — and that enumeration is what a `tools/list` reader takes for the \
                 response's shape: {desc}",
            );
        }
        assert!(
            !desc.contains("state"),
            "spec_set_category must not announce a `state` key: `category_payload` emits none \
             since PDR-0017 was amended on 2026-09-02, and an agent branching on it reads \
             undefined: {desc}",
        );
        // NO `spec_*` DESCRIPTOR RE-ANNOUNCES THE RETIRED AXIS — description and schema alike.
        for tool in &tools {
            let name = tool["name"].as_str().unwrap_or_default();
            if !name.starts_with("spec_") {
                continue;
            }
            let rendered = tool.to_string().to_lowercase();
            assert!(
                !rendered.contains("proposed"),
                "'{name}' still describes the proposed/confirmed axis PDR-0017 retired on \
                 2026-09-02 — this text is what an agent reads before it classifies: {rendered}",
            );
        }
    }

    #[test]
    fn run_start_advises_only_anchors_its_own_schema_advertises() {
        let tools = tool_descriptors();
        let props = tools
            .iter()
            .find(|t| t["name"] == crate::mcp_engine::RUN_START_TOOL)
            .and_then(|t| t["inputSchema"]["properties"].as_object().cloned())
            .expect("run_start is advertised, with a schema");
        let trailer = crate::mcp_engine::RUN_START_ANCHOR_TRAILER;
        // ", or add a cwd / workspace_id / project_id" — slash-separated segments, the argument
        // being the LAST word of each. Read that way, EVERY name the sentence carries is checked,
        // including one no test anticipated; a list written out here would only check the ones it
        // already contained.
        let advised: Vec<&str> = trailer
            .split('/')
            .map(|segment| {
                segment
                    .split_whitespace()
                    .next_back()
                    .unwrap_or_default()
                    .trim_end_matches(['.', ',', ';'])
            })
            .collect();
        // THE SHAPE THE DERIVATION ASSUMES, ASSERTED BEFORE IT IS USED — the same discipline as the
        // scans that audit their own reading. A trailer re-written as prose would leave the loop
        // below matching empty strings and the guard vacuous; it fails HERE instead, so whoever
        // changes the shape says how the new one is to be read.
        assert!(
            trailer.contains('/') && advised.iter().all(|name| !name.is_empty()),
            "the advice must stay a SLASH-SEPARATED LIST of argument names — that shape is what \
             this guard derives them from, and without it it would assert nothing while the \
             refusal it guards says nothing actionable: {trailer}"
        );
        for anchor in advised {
            assert!(
                props.contains_key(anchor),
                "run_start's refusal tells the caller to add '{anchor}', which its own inputSchema \
                 does not advertise — advice a tools/list reader cannot act on is half a refusal: \
                 {trailer}"
            );
        }
    }

    /// THE `data.reason` CODES `run_bind_spec` ADVERTISES ARE THE CONSTANTS IT EMITS (finding
    /// `01M10063H5B65Q81V4HRW94W71`). Its descriptor tells a caller which condition each refusal
    /// names, so those codes are a CONTRACT — and they were re-typed literals, exact but bound to
    /// nothing: renaming a value in `mcp_engine`'s `bind_refusal` would have left the descriptor
    /// mistaken in silence. The repository had already settled this the other way for
    /// `RUN_LOCATION_UNRESOLVED_WARNING_CODE`, promoted to `pub(crate)` so its descriptor is pinned
    /// "against the constant itself rather than against a re-typed literal"; this is the same move
    /// on the same kind of surface.
    ///
    /// TWO HALVES, AND THE SECOND IS THE ONE A HAND-KEPT LIST WOULD LOSE. Each advertised code is
    /// checked against its constant — that catches a RENAME. And the module's declarations are
    /// SCANNED out of the source and confronted with the codes this test knows about — that catches
    /// an ADDITION, i.e. a new refusal shipped without a word to the caller. The one code
    /// deliberately NOT advertised is `AUTHORITY_REFUSED`: the descriptor's own sentence promises
    /// "what you can rule out BEFORE calling, not a closed list of codes", and a race lost inside
    /// the authority's `WHERE` is precisely what no caller can pre-check. Its ABSENCE is asserted,
    /// so the exception stays a decision instead of becoming an oversight.
    ///
    /// WHAT THE ADDITION HALF REACHES, EXACTLY — because a guard that overstates its reach is worse
    /// than none. It reads the `const` declarations of `mod bind_refusal`, at ANY visibility and
    /// any indentation ([`const_declaration`]), and it fails on a declaration it could not parse
    /// rather than skipping it. So: a code added there in any form is either advertised, or red.
    /// What it does NOT reach is a `data.reason` that never becomes a constant of that module — a
    /// bare literal handed straight to `mcp_engine::bind_refusal`. Every call site passes a
    /// constant today, and that is the convention this guard assumes rather than enforces.
    #[test]
    fn run_bind_spec_advertises_its_refusal_codes_by_constant() {
        use crate::mcp_engine::bind_refusal;

        let tools = tool_descriptors();
        let desc = tools
            .iter()
            .find(|t| t["name"] == crate::mcp_engine::RUN_BIND_SPEC_TOOL)
            .and_then(|t| t["description"].as_str())
            .expect("run_bind_spec is advertised")
            .to_string();

        // The preconditions a caller CAN rule out before calling — every one named in the
        // descriptor, by the constant the engine answers with.
        let advertised = [
            bind_refusal::TERMINAL_RUN,
            bind_refusal::FOREIGN_RUN,
            bind_refusal::ALREADY_BOUND,
            bind_refusal::CHILD_RUN,
            bind_refusal::PROJECT_DISAGREEMENT,
            bind_refusal::SPEC_HAS_A_LIVE_RUN,
        ];
        for code in advertised {
            assert!(
                desc.contains(code),
                "run_bind_spec must name the refusal code '{code}' it can answer with — a caller \
                 keys on data.reason, and a code taught nowhere is one it meets as a surprise: \
                 {desc}"
            );
        }
        assert!(
            !desc.contains(bind_refusal::AUTHORITY_REFUSED),
            "'{}' is the ONE code deliberately left out: it is a race lost inside the authority's \
             WHERE, and the descriptor offers what can be ruled out BEFORE calling. Advertising it \
             would invite a pre-check that cannot exist: {desc}",
            bind_refusal::AUTHORITY_REFUSED
        );

        // …AND NOTHING MAY BE DECLARED THERE MUTE. The module's own source is the list: a new
        // precondition added to `bind_refusal` without a word in the descriptor (and without a line
        // above) fails here rather than reaching a caller as an unexplained `data.reason`.
        const ENGINE_SRC: &str =
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_engine.rs"));
        let block = ENGINE_SRC
            .split_once("mod bind_refusal {")
            .expect("the refusal codes are declared in a module of that name")
            .1
            .split_once("\n}")
            .expect("…which closes at column zero, being a top-level item")
            .0;
        // THE READING IS [`const_declaration`]'s, and for the reason written there: a pattern keyed
        // on `"pub const "` would make a VISIBILITY into an escape hatch. The module is `pub(crate)`
        // — `pub const` and `pub(crate) const` reach exactly as far inside it — so a code declared
        // either way must be seen, and `mcp_engine.rs` already mixes the two forms on one family of
        // constants.
        //
        // AND THE SCAN AUDITS ITS OWN READING: every line that declares a `const` here must also
        // yield a literal, so a declaration this pattern cannot parse — a value rustfmt wrapped
        // onto a second line, a type spelled otherwise than `&str` — fails the COUNT rather than
        // dropping out of the comparison below unseen. Without that audit, "unreadable" and
        // "absent" were the same thing to this test, and a new code could ship mute by being merely
        // unreadable (finding `01M10YD9QZYJJDAQQ1S8TGWSKH`).
        //
        // `line.trim()`, both ends: the block's LAST line keeps its `\r` on a CRLF checkout (it is
        // not followed by the `\n` that would let `lines()` strip it), and a code carrying an
        // invisible carriage return would match nothing and fail as a mystery.
        let mut declarations = 0usize;
        let mut declared: Vec<&str> = Vec::new();
        for line in block.lines() {
            let Some(rest) = const_declaration(line.trim()) else {
                continue;
            };
            declarations += 1;
            let Some((_, literal)) = rest.split_once("&str = \"") else {
                continue;
            };
            declared.push(literal.trim_end_matches("\";").trim_end_matches('"'));
        }
        assert_eq!(
            declared.len(),
            declarations,
            "every `const` DECLARED in `bind_refusal` was actually read by the scan — a declaration \
             whose literal this pattern cannot reach must fail here rather than slip through \
             unconfronted; {declarations} declared, read {declared:?}"
        );
        let mut known: Vec<&str> = advertised.to_vec();
        known.push(bind_refusal::AUTHORITY_REFUSED);
        declared.sort_unstable();
        known.sort_unstable();
        assert_eq!(
            declared, known,
            "every code `bind_refusal` declares is accounted for here — one this test does not \
             know about is a refusal that would reach a caller as an unexplained `data.reason`. \
             (A declaration the scan could NOT read has already failed the count above; what is \
             left to this comparison is the codes it did read.)"
        );
    }

    /// DONE-CRITERION (finding `01KYSCY526C9N9B2BBH68A3N2X`): A TOOL SCHEMA MAY NOT DESCRIBE A SHAPE
    /// THE TOOL NO LONGER ACCEPTS.
    ///
    /// The manual was rewritten at every step of the variance model; the tool descriptions followed
    /// only partly, so `tools/list` still announced `states_json` as an "array of names" and
    /// `params_json` as a bag with an `options` field — the OLD model, with no axes, no six types
    /// and no `data-nyx-*`. That is what an agent inspecting the schemas WITHOUT reading the role
    /// manual reads, and it is enough to make it write something the write path then warns about.
    ///
    /// The guard is the twin of the manual's (`!text.to_lowercase().contains("depot")`): it sweeps
    /// EVERY agent-facing string of EVERY descriptor — the tool description and each input-schema
    /// property description — and fails on a STALE FORMULATION, plus a positive half so a
    /// description cannot pass by simply saying nothing.
    #[test]
    fn tool_descriptors_describe_the_variance_model_the_tools_actually_accept() {
        // (tool, field, text) for every agent-facing string in the advertised surface.
        let descriptors = tool_descriptors();
        let mut texts: Vec<(String, String, String)> = Vec::new();
        for descriptor in &descriptors {
            let tool = descriptor["name"].as_str().unwrap_or_default().to_string();
            if let Some(text) = descriptor["description"].as_str() {
                texts.push((tool.clone(), "description".to_string(), text.to_string()));
            }
            if let Some(props) = descriptor["inputSchema"]["properties"].as_object() {
                for (prop, schema) in props {
                    if let Some(text) = schema["description"].as_str() {
                        texts.push((tool.clone(), prop.clone(), text.to_string()));
                    }
                }
            }
        }
        assert!(
            texts.len() > 50,
            "the sweep must actually see the surface, got {} strings",
            texts.len()
        );

        // 1. THE STALE FORMULATIONS — each one is a shape the model stopped accepting, and each one
        //    was really advertised before this finding. The list is SHARED with the guard over the
        //    other agent-facing surface, the role manual (finding `01KYSNSBBT4N22ZTWQHFNQBDJQ`):
        //    two copies would have drifted apart exactly as the two surfaces did.
        for (tool, field, text) in &texts {
            let lowered = text.to_lowercase();
            for (stale, why) in crate::mcp_context::STALE_AGENT_FORMULATIONS {
                assert!(
                    !lowered.contains(stale),
                    "tool `{tool}` ({field}) still advertises the stale form {stale:?}: {why}\n{text}"
                );
            }
        }

        // 2. THE POSITIVE HALF — what the two fields MUST say, wherever they are an argument. A
        //    descriptor an agent reads instead of the manual has to carry the model itself.
        let mut checked: Vec<&str> = Vec::new();
        for (tool, field, text) in &texts {
            match field.as_str() {
                "states_json" => {
                    checked.push(field);
                    for needle in [
                        "AXES",
                        "\"axes\"",
                        "classes",
                        "data-nyx-if",
                        "data-nyx-state-case",
                        "`state`",
                    ] {
                        assert!(
                            text.contains(needle),
                            "tool `{tool}`: the states_json schema must describe the axes model \
                             (missing {needle:?})\n{text}"
                        );
                    }
                }
                "params_json" => {
                    checked.push(field);
                    for needle in [
                        "`text`",
                        "`html`",
                        "`number`",
                        "`boolean`",
                        "`select`",
                        "`state`",
                        "`options`",
                        "`required`",
                        "`default`",
                        "`preview`",
                        "REFUSED",
                    ] {
                        assert!(
                            text.contains(needle),
                            "tool `{tool}`: the params_json schema must describe the SIX types and \
                             the required/default/preview contract (missing {needle:?})\n{text}"
                        );
                    }
                }
                // A `template` argument is where a component's markup is WRITTEN, so it is where the
                // REFERENCE capability has to be advertised (finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`):
                // an agent reading only the schema must learn that a shared sub-component is
                // referenced rather than copied, that its axes are NOT inherited, and that a cycle
                // is refused. `proto_add_variant`'s `composition_json` is a TREE, not a template —
                // only the two catalogue-writing tools carry this one.
                "template" => {
                    checked.push(field);
                    for needle in [
                        "data-nyx-use",
                        "data-nyx-param-",
                        "data-nyx-use-axes",
                        "NEVER inherited",
                        "CYCLE",
                    ] {
                        assert!(
                            text.contains(needle),
                            "tool `{tool}`: the template schema must describe the REFERENCE model \
                             (missing {needle:?})\n{text}"
                        );
                    }
                }
                // A `composition_json` argument is where the PLACEMENT is written, so it is where the
                // inter-page link has to be advertised (finding `01KYW34347CZSNXCYS35KDG3EY`): an
                // agent reading only the schema must learn that a node can open another PAGE, that
                // the link is an ID and not a slug, where it lives and why, what a dead one does —
                // and that a cycle is fine, which is the question a reader would otherwise answer
                // wrongly by analogy with `data-nyx-use`.
                "composition_json" => {
                    checked.push(field);
                    for needle in [
                        "navigate_to",
                        "PARCOURS",
                        "prototype_pages.id",
                        "never a slug",
                        "on the NODE and not on the component",
                        "REFUSED",
                        "CYCLES are fine",
                    ] {
                        assert!(
                            text.contains(needle),
                            "tool `{tool}`: the composition_json schema must describe the \
                             inter-page link (missing {needle:?})\n{text}"
                        );
                    }
                }
                // The SCOPED CSS SLOT (finding `01KYVMTWB71R0D0CT1SV88J8DX`). Wherever it is an
                // argument, the descriptor must state the ORDER (Tailwind is the channel), the four
                // constructs that are the only admitted reasons, and the four constraints — each as
                // a REFUSAL. An agent reading only the schema must not be able to conclude that the
                // slot is a general-purpose stylesheet.
                "css_slot" => {
                    checked.push(field);
                    for needle in [
                        "TAILWIND IS THE CHANNEL",
                        "LAST resort",
                        "@keyframes",
                        "::before/::after",
                        ":has",
                        "@container",
                        "REFUSED",
                        "TOKENISED",
                        "SCOPED",
                        "BOUNDED",
                        "SIGNALLED",
                        "data-nyx-css",
                        "css_slot_reason",
                        "relax its CSP",
                    ] {
                        assert!(
                            text.contains(needle),
                            "tool `{tool}`: the css_slot schema must state the rule and its four \
                             constraints (missing {needle:?})\n{text}"
                        );
                    }
                }
                "css_slot_reason" => {
                    checked.push(field);
                    assert!(
                        text.contains("MANDATORY")
                            && text.contains("WHAT THE UTILITIES COULD NOT DO"),
                        "tool `{tool}`: the reason is mandatory and says what the utilities could \
                         not do\n{text}"
                    );
                }
                _ => {}
            }
        }
        // …and the sweep cannot pass vacuously: the three tools that carry these arguments are
        // named, so renaming or dropping one is a failure, not a silent hole.
        for expected in [
            crate::mcp_tools_proto::PROTO_ADD_VARIANT_TOOL,
            crate::mcp_tools_proto::PROTO_AUTHOR_COMPONENT_TOOL,
            crate::mcp_tools_design::DESIGN_REFRESH_COMPONENT_TOOL,
        ] {
            assert!(
                texts
                    .iter()
                    .any(|(tool, field, _)| tool == expected && field == "states_json"),
                "{expected} must still advertise a states_json argument"
            );
        }
        assert!(
            checked.iter().filter(|f| **f == "params_json").count() >= 2
                && checked.iter().filter(|f| **f == "states_json").count() >= 3
                && checked.iter().filter(|f| **f == "template").count() >= 2
                // BOTH catalogue-writing tools advertise the slot, in the SAME words — one
                // mechanism, one contract, so an agent cannot learn two different rules for it.
                && checked.iter().filter(|f| **f == "css_slot").count() == 2
                && checked.iter().filter(|f| **f == "css_slot_reason").count() == 2
                // The PLACEMENT argument: wherever a tree is written, the parcours is advertised.
                && checked.iter().filter(|f| **f == "composition_json").count() >= 1,
            "every field is checked on every tool that takes it: {checked:?}"
        );
    }

    /// Every agent-facing string of the advertised surface, as `(tool, field, text)` — the sweep the
    /// descriptor guards share.
    fn descriptor_texts() -> Vec<(String, String, String)> {
        let mut texts: Vec<(String, String, String)> = Vec::new();
        for descriptor in &tool_descriptors() {
            let tool = descriptor["name"].as_str().unwrap_or_default().to_string();
            if let Some(text) = descriptor["description"].as_str() {
                texts.push((tool.clone(), "description".to_string(), text.to_string()));
            }
            if let Some(props) = descriptor["inputSchema"]["properties"].as_object() {
                for (prop, schema) in props {
                    if let Some(text) = schema["description"].as_str() {
                        texts.push((tool.clone(), prop.clone(), text.to_string()));
                    }
                }
            }
        }
        texts
    }

    /// DONE-CRITERION (task `01M1F8X44HSK742EVVG8F94H6H`): **no message this server renders carries
    /// a database's own sentence.**
    ///
    /// Measured 2026-09-01: `task_add` answered `-32000 internal` with `db error: FOREIGN KEY
    /// constraint failed`, and `db error: CHECK constraint failed: kind IN
    /// ('slice','human','review')`. The actionable refusals live upstream now (the write path knows
    /// which argument is at fault); this pins the NET, so a path nobody has audited cannot put
    /// SQLite's prose back on the wire — whatever string it assembles.
    #[test]
    fn no_rpc_error_message_carries_sql_text() {
        // The database's sentences, as they really arrive — bare, and wrapped in a caller's lead-in.
        for raw in [
            "db error: FOREIGN KEY constraint failed",
            "db error: CHECK constraint failed: kind IN ('slice','human','review')",
            "FOREIGN KEY constraint failed",
            "create workspace failed: UNIQUE constraint failed: workspaces.path",
            "db error: no such table: app_settings",
            "query spec_status: no such column: specs.bogus",
        ] {
            let err = RpcError::new(RpcCode::Internal, raw);
            let lower = err.message.to_ascii_lowercase();
            for marker in [
                "constraint failed",
                "foreign key",
                "no such table",
                "no such column",
                "kind in (",
            ] {
                assert!(
                    !lower.contains(marker),
                    "the SQL marker {marker:?} survived redaction of {raw:?}: {}",
                    err.message
                );
            }
            // What is withheld is REPLACED by something the caller can act on, never blanked.
            assert!(
                err.message.contains("argument"),
                "the replacement tells the caller where to look: {}",
                err.message
            );
        }
        // The caller's OWN lead-in survives — it says which operation died and is not SQL.
        assert!(
            RpcError::new(RpcCode::Internal, "create workspace failed: UNIQUE constraint failed")
                .message
                .starts_with("create workspace failed: "),
            "the redaction cuts the database's clause, not the whole message"
        );
        // And a refusal that never touched a database is passed through UNCHANGED — the net must
        // not rewrite the actionable errors it sits under.
        let untouched = "missing or empty required argument 'project_id'";
        assert_eq!(
            RpcError::new(RpcCode::InvalidArgument, untouched).message,
            untouched
        );
    }

    /// #K2 — THE NET MUST NOT EAT A REFUSAL THAT QUOTES THE CALLER.
    ///
    /// A good refusal repeats the argument it is refusing, so an argument that happens to READ like
    /// SQLite's prose used to be enough to have the whole sentence replaced by a diagnosis about a
    /// storage layer nothing had consulted — and the accepted-values list, the only actionable part,
    /// went with it. Both measurements are pinned here, on the exact messages the two tools build.
    #[test]
    fn an_argument_refusal_that_quotes_the_caller_survives_the_sql_net() {
        // `task_add(kind: "no such table")` — `mcp_tools_spec::optional_enum_str`'s wording. The
        // marker sits BEFORE any `": "`, so the redaction had no head to keep and blanked it all.
        let kinds = crate::db::TASK_KINDS.join(", ");
        let refusal = RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown kind 'no such table' (accepted: {kinds})"),
        );
        assert!(
            refusal.message.contains(&kinds),
            "the accepted values are the whole point of the refusal: {}",
            refusal.message
        );
        assert!(
            !refusal.message.contains("storage layer"),
            "and nothing about a database nyx never consulted: {}",
            refusal.message
        );

        // `get_command_output(grep: "syntax error(")` — `mcp_runtime::optional_regex`'s wording,
        // where the regex crate's own parse detail is what tells the agent how to fix the pattern.
        let parse_error = regex::Regex::new("syntax error(").unwrap_err().to_string();
        let refusal = RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument 'grep' is not a valid regex: {parse_error}"),
        );
        assert!(
            refusal.message.contains("unclosed group"),
            "the parse error must survive rather than become a false SQL diagnosis: {}",
            refusal.message
        );

        // The two other caller-facing codes are covered by the same rule.
        for code in [RpcCode::InvalidId, RpcCode::InvalidState] {
            let raw = "unknown phase 'no such column' (accepted: 1, 2, 3)";
            assert_eq!(RpcError::new(code, raw).message, raw);
        }
    }

    /// Every agent-facing prose surface nyx SERVES, as `(surface, text)`: the `initialize`
    /// instructions, every advertised tool description + property description, and every role
    /// MANUAL `agent_context` renders. These are the only texts an agent reads before it forms a
    /// call — so they are the exact set the "no phantom tool" guard below sweeps.
    fn agent_facing_texts() -> Vec<(String, String)> {
        let mut texts: Vec<(String, String)> = vec![(
            "initialize instructions".to_string(),
            SERVER_INSTRUCTIONS.to_string(),
        )];
        for (tool, field, text) in descriptor_texts() {
            texts.push((format!("tool `{tool}` / {field}"), text));
        }
        for role in crate::mcp_context::AgentRole::ALL {
            texts.push((
                format!("agent_context manual {role:?}"),
                crate::mcp_context::render_role_template(role),
            ));
        }
        texts
    }

    /// The TOOL NAMES nyx's MCP layer declares anywhere in its own source — harvested from the
    /// `*_TOOL: &str = "…"` constants of the MCP modules, read through [`const_declaration`] like
    /// every other source scan here. This is the VOCABULARY the guard below checks the prose
    /// against: a name in it that `tools/list` does not carry is a name that exists in the code and
    /// NOT on the wire, which is exactly the shape of the phantom.
    ///
    /// Only the `mcp*` modules are scanned, deliberately: `agent_activity.rs` declares
    /// `ASK_USER_QUESTION_TOOL`, which names CLAUDE CODE's own tool (a `PreToolUse` payload nyx
    /// reads), not a tool nyx serves — it has no business being required in `tools/list`.
    fn declared_tool_names() -> Vec<String> {
        const SOURCES: [&str; 11] = [
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_authoring.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_context.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_engine.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_runtime.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_tools_annexe.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_tools_core.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_tools_spec.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_tools_design.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_tools_idea.rs")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp_tools_proto.rs")),
        ];
        let mut names: Vec<String> = Vec::new();
        for src in SOURCES {
            for line in src.lines() {
                let Some(rest) = const_declaration(line) else {
                    continue;
                };
                let Some((name, value)) = rest.split_once(": &str = ") else {
                    continue;
                };
                if !name.trim().ends_with("_TOOL") {
                    continue;
                }
                let value = value.trim().trim_end_matches(';');
                let Some(literal) = value
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .filter(|v| !v.is_empty())
                else {
                    continue;
                };
                names.push(literal.to_string());
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// The snake_case identifiers a prose text writes in CALL SHAPE — `name(` — which is how this
    /// surface's texts point at a tool an agent should reach for (`list_commands(project_id=…)`,
    /// `task_transition(transition=skip)`). Everything else is punctuation-agnostic prose, and
    /// filtering on the call shape is what keeps `spec_id` / `strip_ansi` / `css_slot` (arguments,
    /// not tools) out of a tool-name guard.
    fn call_shaped_identifiers(text: &str) -> Vec<String> {
        let bytes: Vec<char> = text.chars().collect();
        let mut out: Vec<String> = Vec::new();
        let mut i = 0usize;
        while i < bytes.len() {
            if !(bytes[i].is_ascii_lowercase()) {
                i += 1;
                continue;
            }
            // Do not start mid-identifier (`design_preview` must not also yield `preview`).
            if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == '_') {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == '_') {
                i += 1;
            }
            let word: String = bytes[start..i].iter().collect();
            if i < bytes.len() && bytes[i] == '(' && word.contains('_') {
                out.push(word);
            }
        }
        out
    }

    /// DONE-CRITERION (task `01M1F8X2WSPGJN75PEYYBVHD5K`): **no agent-facing text names a tool that
    /// `tools/list` does not carry.**
    ///
    /// The defect this closes was measured live: `spec_create`'s description told an agent to "use
    /// spec_transition to move its lifecycle", and the Spec role manual told it "you never call
    /// spec_transition" — while `spec_transition` is DELIBERATELY absent from the advertised surface
    /// (N17(b): nyx imposes every Spec transition at the graph node built for it). An agent that
    /// followed the prose got `-32601 method_not_found` on a tool that does not exist, and a
    /// session burned a round trip discovering it. Naming a phantom to FORBID it is the same defect
    /// as naming one to recommend it: both teach that the verb exists.
    ///
    /// TWO NETS, because a phantom has two origins:
    ///   * a name nyx's own code DECLARES but does not advertise ([`declared_tool_names`]) — the
    ///     `spec_transition` shape, a tool retired from `tools/list` while the prose kept pointing
    ///     at it. Checked wherever it appears, call shape or not, since that is how the two live
    ///     occurrences were written.
    ///   * a name written in CALL SHAPE ([`call_shaped_identifiers`]) that no tool answers — the
    ///     invented verb, and the shape every "read it back with spec_get(…)" instinct takes.
    #[test]
    fn no_agent_facing_text_names_a_tool_that_is_not_advertised() {
        let advertised: Vec<String> = tool_descriptors()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        let declared = declared_tool_names();
        assert!(
            declared.len() > 50,
            "the source scan must actually find the tool-name constants, got {declared:?}"
        );
        // The names the code declares and the wire does not carry — the phantom vocabulary.
        let unadvertised: Vec<&String> = declared
            .iter()
            .filter(|n| !advertised.contains(n))
            .collect();

        // NO ALLOWLIST, and none is needed: on this surface a snake_case word followed by `(` is
        // always a tool citation. Should a descriptor one day need to quote a CSS/JS call in that
        // shape (`prefers_reduced_motion(…)`), the exception belongs here, named — never as a
        // loosening of the net.
        //
        // THE TWO NETS MUST BE IN THE WATER, and that is asserted rather than assumed: a scan that
        // silently matched nothing would be a green test guarding nothing at all.
        assert!(
            unadvertised.contains(&&crate::mcp_tools_spec::SPEC_TRANSITION_TOOL.to_string()),
            "spec_transition is declared and NOT advertised — the phantom this guard was written \
             for must be in the vocabulary it sweeps: {unadvertised:?}"
        );
        let mut call_shapes = 0usize;

        for (surface, text) in agent_facing_texts() {
            for phantom in &unadvertised {
                assert!(
                    !text.contains(phantom.as_str()),
                    "{surface} names `{phantom}`, which nyx declares but does NOT advertise in \
                     tools/list — an agent following this text gets method_not_found. Either \
                     advertise the tool, or name the mechanism that really applies.\n{text}"
                );
            }
            for candidate in call_shaped_identifiers(&text) {
                call_shapes += 1;
                assert!(
                    advertised.contains(&candidate),
                    "{surface} writes `{candidate}(…)` — a call shape naming no advertised tool. \
                     Every tool name a text cites must exist in tools/list.\n{text}"
                );
            }
        }
        assert!(
            call_shapes > 20,
            "the call-shape net must actually see the surface's `tool(…)` citations, saw \
             {call_shapes}"
        );
    }

    /// DONE-CRITERION (task `01M1F8X7SE5B3BQTKSXCFQGTNY`): **no text this server renders names a
    /// role that does not exist.**
    ///
    /// A role name is a word an agent SENDS — `agent_context(role: …)` refuses anything outside the
    /// six — so prose naming a seventh sends a session into a refusal it cannot diagnose, and prose
    /// omitting one hides a capability. The advertised vocabulary is therefore replayed through the
    /// very parser that accepts it, in BOTH directions, and the words nyx has never had (a
    /// "reviewer": the role that audits is `auditor`) are swept out of every agent-facing text.
    ///
    /// `prototyper` is deliberately NOT swept: it is a real WORKFLOW SCOPE (`run_start`,
    /// `workflow_create`), and the role beside it is `prototype`. Two vocabularies, one syllable
    /// apart — which is exactly why the ROLE half is pinned to its parser rather than to a list
    /// someone keeps by eye.
    #[test]
    fn no_agent_facing_text_names_a_role_that_does_not_exist() {
        use crate::mcp_context::AgentRole;
        let role_desc = tool_descriptors()
            .iter()
            .find(|t| t["name"] == json!(crate::mcp_context::AGENT_CONTEXT_TOOL))
            .map(|t| t["inputSchema"]["properties"]["role"]["description"].clone())
            .expect("agent_context advertises a `role` argument");
        let role_desc = role_desc.as_str().unwrap_or_default();
        // The advertised vocabulary: the `a | b | c` head of the sentence.
        let advertised: Vec<&str> = role_desc
            .split('—')
            .next()
            .unwrap_or_default()
            .split('|')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();

        // EVERY ADVERTISED NAME IS ONE THE PARSER ACCEPTS…
        for name in &advertised {
            assert!(
                AgentRole::parse(name).is_some(),
                "the `role` argument advertises '{name}', which agent_context refuses: {role_desc}"
            );
        }
        // …AND EVERY ROLE THAT EXISTS IS ADVERTISED. Both directions, or the vocabulary drifts.
        for role in AgentRole::ALL {
            assert!(
                advertised.contains(&role.slug()),
                "the role '{}' exists and is advertised nowhere: {role_desc}",
                role.slug()
            );
        }
        assert_eq!(
            advertised.len(),
            AgentRole::ALL.len(),
            "exactly the roles that exist, no more: {advertised:?}"
        );

        // …and the names nyx never had appear in NO agent-facing text — descriptions, property
        // descriptions, the initialize instructions, and every role manual.
        for (surface, text) in agent_facing_texts() {
            let lower = text.to_lowercase();
            for phantom in ["reviewer", "role=prototyper", "role: prototyper"] {
                assert!(
                    !lower.contains(phantom),
                    "{surface} names '{phantom}', which is not a role nyx has (the six are \
                     {:?})\n{text}",
                    AgentRole::ALL.map(AgentRole::slug)
                );
            }
        }
    }

    /// THE SIX PARAM TYPES, MOORED TO THE FIXTURE — re-pointed (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`,
    /// family 1). The list of accepted `type` words lives at three places: the constant the write
    /// path refuses outside of, the renderer's constant, and the PROSE an agent reads. That prose
    /// used to be the Prototype role manual's; it is now the tool DESCRIPTORS, which every session
    /// receives from `tools/list` whether or not it ever renders a manual — so this is where the
    /// mooring goes. A seventh type accepted by the write path alone would be taught nowhere; a type
    /// dropped from the constant would go on being taught, and every agent believing the prose would
    /// have its synthesis refused.
    #[test]
    fn every_params_json_descriptor_teaches_exactly_the_types_the_write_path_accepts() {
        let section = crate::mcp_tools_design::design_fixture_section("param_types");
        let pinned: Vec<&str> = section["types"]
            .as_array()
            .expect("the types")
            .iter()
            .map(|t| t.as_str().expect("a type"))
            .collect();
        let deprecated = section["deprecated"].as_str().expect("the deprecated type");

        let mut seen = 0usize;
        for (tool, field, text) in descriptor_texts() {
            if field != "params_json" {
                continue;
            }
            seen += 1;
            // EXACTLY the pinned set: each one taught…
            for ty in &pinned {
                assert!(
                    text.contains(&format!("`{ty}`")),
                    "tool `{tool}`: the params_json prose must teach the accepted type `{ty}`\n{text}"
                );
            }
            // …and no OTHER word offered as a type (the ones a write really refuses).
            for absent in ["`enum`", "`string`", "`color`", "`variant`", "`asset`"] {
                assert!(
                    !text.contains(absent),
                    "tool `{tool}`: {absent} is not an accepted type and must not be taught\n{text}"
                );
            }
            // The deprecated one is taught AS deprecated, never offered as a plain choice…
            assert!(
                text.contains("DEPRECATED") && text.contains(&format!("`{deprecated}`")),
                "tool `{tool}`: `{deprecated}` must be taught as DEPRECATED\n{text}"
            );
            // …and an unknown type is REFUSED, not repaired into `text`.
            assert_eq!(
                section["unknown_type_is_refused"],
                Value::Bool(true),
                "the fixture states the write-path rule"
            );
            assert!(
                text.contains("REFUSED, \\\n                    not repaired")
                    || text.contains("REFUSED, not repaired"),
                "tool `{tool}`: an unknown type is refused, never silently turned into `text`\n{text}"
            );
        }
        assert!(
            seen >= 2,
            "both catalogue-writing tools advertise params_json, saw {seen}"
        );
    }

    /// THE REFERENCE ATTRIBUTES ARE THE ONES THE RENDERER PARSES (finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`,
    /// re-pointed). The `data-nyx-use` vocabulary is taught by the `template` descriptors and read by
    /// `crate::render::reference_sites`: a `data-nyx-uses` typo in the prose would teach something
    /// nothing reads. So the taught names are assembled into a real element and handed to the very
    /// reader the render uses and the very validator the write path applies.
    #[test]
    fn the_taught_reference_attributes_are_the_ones_the_render_reads() {
        let mut seen = 0usize;
        for (tool, field, text) in descriptor_texts() {
            if field != "template" || !text.contains("data-nyx-use") {
                continue;
            }
            seen += 1;
            for taught in ["data-nyx-use=", "data-nyx-param-", "data-nyx-use-axes="] {
                assert!(
                    text.contains(taught),
                    "tool `{tool}`: the reference vocabulary must name `{taught}`\n{text}"
                );
            }
            // The taught names, assembled — exactly as an agent would write them.
            let element = "<span data-nyx-use=\"Spinner\" data-nyx-use-axes=\"size=sm\" \
                           data-nyx-param-label=\"{{label}}\"></span>";
            let sites = crate::render::reference_sites(element);
            assert_eq!(sites.len(), 1, "tool `{tool}`: one reference site: {element}");
            assert_eq!(sites[0].component, "Spinner");
            assert_eq!(
                sites[0].params,
                vec![("label".to_string(), "{{label}}".to_string())],
                "tool `{tool}`: the taught attribute really names the param it claims to"
            );
            assert_eq!(sites[0].axes, vec![("size".to_string(), "sm".to_string())]);

            // …and the write path's validator accepts it against the component it describes.
            let catalogue = vec![crate::render::RenderComponent {
                name: "Spinner".into(),
                template: "<i class=\"animate-spin\">{{label}}</i>".into(),
                states_json: Some(
                    r#"{"axes":[{"name":"size","values":[{"name":"sm","classes":"h-3 w-3"}]}]}"#
                        .into(),
                ),
                params_json: Some(r#"[{"name":"label","type":"text","required":true}]"#.into()),
                css_slot: None,
            }];
            let warnings =
                crate::mcp_tools_design::reference_warnings("Button", element, &catalogue);
            assert!(
                warnings.is_empty(),
                "tool `{tool}`: the taught reference must not be warned about by the validator that \
                 reads it: {warnings:?}"
            );
        }
        assert!(seen >= 2, "both catalogue-writing tools teach it, saw {seen}");
    }

    /// THE PHASE PLAN EMERGES FROM THE NAMES, and `task_add` is where that is said. It used to be
    /// taught by the spec-author manual too; the manual now carries the ROLE, so the call contract
    /// lives with the call — where an agent gets it from `tools/list` without rendering anything.
    ///
    /// K2 — and it must say WHERE that stops. An auditor passed a phase's rendered LABEL on an
    /// `in_progress` Spec and minted a third phase in its plan, because the descriptor promised
    /// materialization and said nothing of the freeze. The rule and the text it is read from are one
    /// thing: an agent that only reads this line must not be surprised by the refusal.
    #[test]
    fn the_task_add_descriptor_teaches_the_phase_plan_emerges_from_the_names() {
        let phase = descriptor_texts()
            .into_iter()
            .find(|(tool, field, _)| {
                tool == crate::mcp_tools_spec::TASK_ADD_TOOL && field == "phase"
            })
            .map(|(_, _, text)| text)
            .expect("task_add advertises a `phase` argument");
        assert!(
            phase.contains("NAME") && phase.contains("first-seen order"),
            "the plan is built from the phase NAMES, in first-seen order: {phase}"
        );
        assert!(
            phase.contains("DRAFT") && phase.contains("FROZEN"),
            "…while the Spec is DRAFT, and the plan is FROZEN after: {phase}"
        );
        assert!(
            phase.contains("kind=review") && phase.contains("OMIT IT"),
            "…and an auditor names no phase at all — nyx anchors its finding: {phase}"
        );
        // DONE-CRITERION (task `01M1F8X5B1WJ40XY2QRJ5G4W17`): the field states its TYPE and the
        // NAME↔RANK relation. Both were missing, and their absence compounded: an integer looked
        // legitimate (every manual says "phase 1"), was silently dropped, and the number that DID
        // come back was a rank the caller had never sent.
        assert!(
            phase.contains("STRING") && phase.contains("REFUSED"),
            "the expected TYPE is stated, and an integer is refused rather than ignored: {phase}"
        );
        assert!(
            phase.contains("RANK") && phase.contains("phase_name"),
            "…and the name↔rank relation is stated, with the field that echoes the name: {phase}"
        );
        for gone in ["spec_set_phases", "1..N"] {
            assert!(
                !phase.contains(gone),
                "`{gone}` is not part of the contract any more: {phase}"
            );
        }
    }

    /// DONE-CRITERIA (« MCP : découverte dédiée ») — over the REAL JSON-RPC transport:
    /// `list_system_commands` is advertised, takes NO argument, and its description states the
    /// authorization rule an agent has to know BEFORE it calls anything — reading and starting a
    /// stopped command are free, every MUTATION needs the human's approval.
    ///
    /// The description is the one surface with no compiler behind it, and it is the only place an
    /// agent learns this rule: a tool that answered ids without saying what may be done with them
    /// would have agents discover the boundary by being refused.
    #[test]
    fn list_system_commands_is_advertised_and_states_its_authorization_rule() {
        let (_server, port) = start_test_server();
        let req = r#"{"jsonrpc":"2.0","id":21,"method":"tools/list"}"#;
        let (_status, body) = http_request(port, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        let tool = v["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|t| t["name"] == json!(LIST_SYSTEM_COMMANDS_TOOL))
            .cloned()
            .expect("list_system_commands is advertised over tools/list");

        // NO ARGUMENT: the scope is a singleton, so there is nothing to name.
        assert_eq!(
            tool["inputSchema"]["required"],
            json!([]),
            "the singleton needs no argument: {tool}"
        );
        assert_eq!(tool["inputSchema"]["properties"], json!({}));

        let desc = tool["description"].as_str().expect("a description");
        // WHAT IT ANSWERS — an agent must know it gets a launchable id, and where it runs.
        for named in ["instance_id", "nature", "groups", "home"] {
            assert!(desc.contains(named), "the answer names `{named}`: {desc}");
        }
        // WHAT MAY BE DONE — the free half and the gated half, both said.
        assert!(
            desc.contains("start_command") && desc.contains("get_command_output"),
            "the FREE operations are named, with the tools that perform them: {desc}"
        );
        assert!(
            desc.to_lowercase().contains("free"),
            "…and said to be free: {desc}"
        );
        assert!(
            desc.contains("approval") && desc.contains("refused"),
            "the GATED half is named too, with what happens without it: {desc}"
        );
        for mutation in ["creating", "removing", "relaunching", "stopping"] {
            assert!(
                desc.contains(mutation),
                "the description must name `{mutation}` among the gated operations: {desc}"
            );
        }
        // …and the terminals are declared out of reach, so an agent does not go looking.
        assert!(
            desc.contains("TERMINALS") || desc.contains("terminals"),
            "the Système terminals are declared invisible: {desc}"
        );
    }

    #[test]
    fn tools_list_advertises_v1_surface() {
        let (_server, port) = start_test_server();
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let (_status, body) = http_request(port, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        let tools = v["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in V1_TOOLS {
            assert!(
                names.contains(&expected),
                "v1 tool {expected} must be listed"
            );
        }
        // The frozen v1 surface, plus the spike `probe` tool (ADR-0004) and the
        // `wait_for_command` long-poll (ADR-0003 D12) advertised alongside — both
        // kept OUT of V1_TOOLS so the frozen contract is untouched, yet discoverable.
        assert!(
            names.contains(&PROBE_TOOL),
            "probe spike tool must be listed"
        );
        assert!(
            names.contains(&WAIT_FOR_COMMAND_TOOL),
            "wait_for_command long-poll tool must be listed"
        );
        // The command-CRUD extension (ADR-0003 D13): mutating tools advertised alongside
        // the v1 surface, kept OUT of V1_TOOLS like probe/wait_for_command.
        assert!(
            names.contains(&ADD_COMMAND_TOOL),
            "add_command tool must be listed"
        );
        assert!(
            names.contains(&UPDATE_COMMAND_TOOL),
            "update_command tool must be listed"
        );
        assert!(
            names.contains(&IMPORT_COMMANDS_TOOL),
            "import_commands tool must be listed"
        );
        // A2: remove tools advertised alongside CRUD.
        assert!(
            names.contains(&REMOVE_WORKSPACE_TOOL),
            "remove_workspace tool must be listed"
        );
        assert!(
            names.contains(&REMOVE_COMMAND_TOOL),
            "remove_command tool must be listed"
        );
        // R-OUTPUT: the clear_command_output buffer-reset tool advertised alongside.
        assert!(
            names.contains(&CLEAR_COMMAND_OUTPUT_TOOL),
            "clear_command_output tool must be listed"
        );
        // R-IMPORT #5: the import-preview + grouped-delete tools advertised alongside.
        assert!(
            names.contains(&LIST_IMPORTABLE_SCRIPTS_TOOL),
            "list_importable_scripts tool must be listed"
        );
        assert!(
            names.contains(&REMOVE_COMMANDS_TOOL),
            "remove_commands tool must be listed"
        );
        // Review 01M0ABR6V501D0SPR2BYBVZC4A #90: the multi-pane GROUPING of commands, agent side.
        assert!(
            names.contains(&MANAGE_COMMAND_GROUP_TOOL),
            "manage_command_group tool must be listed"
        );
        // Review 01M11WE6H9E5D5FCS6RT0ZXAXN: the command LIST order, agent side.
        assert!(
            names.contains(&REORDER_COMMANDS_TOOL),
            "reorder_commands tool must be listed"
        );
        // R-TERM: the interactive-terminal tools advertised alongside, kept OUT of V1_TOOLS.
        assert!(
            names.contains(&CREATE_TERMINAL_TOOL),
            "create_terminal tool must be listed"
        );
        assert!(
            names.contains(&SEND_TO_TERMINAL_TOOL),
            "send_to_terminal tool must be listed"
        );
        // FEEDBACK #31: the raw-key injection tool advertised alongside, kept OUT of V1_TOOLS.
        assert!(
            names.contains(&SEND_KEYS_TOOL),
            "send_keys tool must be listed"
        );
        assert!(
            names.contains(&LIST_TERMINALS_TOOL),
            "list_terminals tool must be listed"
        );
        assert!(
            names.contains(&CLOSE_TERMINAL_TOOL),
            "close_terminal tool must be listed"
        );
        // PRD-4.1 #1: the read_terminal scrollback-read tool advertised alongside, kept OUT of V1_TOOLS.
        assert!(
            names.contains(&READ_TERMINAL_TOOL),
            "read_terminal tool must be listed"
        );
        // PRD-5 #4 (ADR-0004): the agent-session channel advertised alongside, kept OUT of V1_TOOLS.
        assert!(
            names.contains(&AGENT_SESSION_EVENT_TOOL),
            "agent_session_event tool must be listed"
        );
        // The project-settings (rename + resume) tool advertised alongside, kept OUT of V1_TOOLS.
        assert!(
            names.contains(&SET_PROJECT_SETTINGS_TOOL),
            "set_project_settings tool must be listed"
        );
        // finding #27: the explicit repository-config tool advertised alongside, kept OUT of V1_TOOLS.
        assert!(
            names.contains(&PROJECT_SET_REPOS_TOOL),
            "project_set_repos tool must be listed"
        );
        // v2.2 cockpit read model: the workspace-runtime aggregate tool advertised alongside,
        // kept OUT of V1_TOOLS (a new read over the existing DB + runner authorities).
        assert!(
            names.contains(&WORKSPACE_RUNTIME_TOOL),
            "workspace_runtime tool must be listed"
        );
        // v2.2 Git CHANGES read model: the workspace-git summary tool advertised alongside.
        assert!(
            names.contains(&WORKSPACE_GIT_TOOL),
            "workspace_git tool must be listed"
        );
        // v2.2 TIMELINE read model: the bounded workspace activity fil tool advertised alongside.
        assert!(
            names.contains(&WORKSPACE_TIMELINE_TOOL),
            "workspace_timeline tool must be listed"
        );
        // v2.4 Phase-5 (PDR-0020): the Spec AUTHORING + TRANSITION tools advertised alongside,
        // kept OUT of V1_TOOLS (a new authoring surface, not the frozen v1 contract).
        for expected in crate::mcp_tools_spec::SPEC_TOOLS {
            assert!(
                names.contains(&expected),
                "phase-5 spec tool {expected} must be listed"
            );
        }
        // v2.5.8 finding #31: the draft-only task DELETE tool is advertised alongside (it is
        // part of SPEC_TOOLS, so it also counts toward the total below).
        assert!(
            names.contains(&crate::mcp_tools_spec::TASK_DELETE_TOOL),
            "task_delete tool must be listed"
        );
        // The phase plan is now MATERIALIZED from task_add(phase="Name") — the separate
        // `spec_set_phases` tool was removed (finding 01KXG68DN5DSCFDFA6C87NJHGV), so it must
        // NOT be advertised (symmetric to the no-resolve / no-validate guard-rails).
        assert!(
            !names.contains(&"spec_set_phases"),
            "spec_set_phases was removed — the plan self-builds from task_add(phase name)",
        );
        // v2.4 Phase-5: the agent-context render tool advertised alongside.
        assert!(
            names.contains(&crate::mcp_context::AGENT_CONTEXT_TOOL),
            "agent_context tool must be listed"
        );
        // v2.4 Git DIFF read model: the per-repo diff tool advertised alongside.
        assert!(
            names.contains(&REPO_DIFF_TOOL),
            "repo_diff tool must be listed"
        );
        // v2.4 DERIVED GIT VIEWS read model: the per-repo state tool advertised alongside.
        assert!(
            names.contains(&REPO_GIT_VIEW_TOOL),
            "repo_git_view tool must be listed"
        );
        // v2.4 REAL-TIME REFRESH (Q-015): the review watch/unwatch tools advertised alongside.
        assert!(
            names.contains(&REPO_WATCH_TOOL) && names.contains(&REPO_UNWATCH_TOOL),
            "repo_watch + repo_unwatch tools must be listed"
        );
        // v2.4 review COMMENTS: the list + post tools advertised alongside. v2.5.11: the agent
        // resolves ITS OWN comments too (review_comment_resolve) — a HUMAN comment stays human-only.
        assert!(
            names.contains(&REVIEW_COMMENT_LIST_TOOL) && names.contains(&REVIEW_COMMENT_POST_TOOL),
            "review_comment_list + review_comment_post tools must be listed"
        );
        assert!(
            names.contains(&REVIEW_COMMENT_RESOLVE_TOOL),
            "review_comment_resolve is listed — the agent resolves its own (agent) comments"
        );
        // v2.4 review TASKS (finding = task): the list + structure tools advertised. The grill has
        // just two states (review follow-up #31) — a structured finding is coder-ready, so there is
        // DELIBERATELY no validate tool of any name.
        assert!(
            names.contains(&REVIEW_TASK_LIST_TOOL) && names.contains(&REVIEW_TASK_STRUCTURE_TOOL),
            "review_task_list + review_task_structure tools must be listed"
        );
        assert!(
            !names.contains(&"review_task_validate"),
            "there is DELIBERATELY no validate tool — a structured finding is already coder-ready"
        );
        // v2.5.11 RETOUR-CHAT: the agent relays the human's chat feedback into the review (a HUMAN
        // global comment + a proposed finding); the viewer stays a valid channel in parallel.
        assert!(
            names.contains(&REVIEW_SUBMIT_FEEDBACK_TOOL),
            "review_submit_feedback tool must be listed (chat is a review channel)"
        );
        // v2.6 Phase 1: the IDEAS tools (capture + scored search + close/drop/merge + Spec
        // links) advertised alongside, kept OUT of V1_TOOLS (a new surface).
        for expected in crate::mcp_tools_idea::IDEA_TOOLS {
            assert!(
                names.contains(&expected),
                "v2.6 idea tool {expected} must be listed"
            );
        }
        // v2.6 Phase 3: the ANNEXE tools (attach doc/artefact/url/prototype to a Spec, dedup by
        // content_hash) advertised alongside, kept OUT of V1_TOOLS (a new surface). The v2.6
        // Phase-3 Spec-dependency tools (spec_link / spec_unlink / spec_deps) live IN SPEC_TOOLS,
        // so they are covered by the SPEC_TOOLS loop above and counted via SPEC_TOOLS.len().
        for expected in crate::mcp_tools_annexe::ANNEXE_TOOLS {
            assert!(
                names.contains(&expected),
                "v2.6 annexe tool {expected} must be listed"
            );
        }
        // The v2.8 engine runtime tick (nyx_next) is advertised alongside, kept OUT of V1_TOOLS.
        assert!(
            names.contains(&crate::mcp_engine::NYX_NEXT_TOOL),
            "v2.8 nyx_next engine tool must be listed"
        );
        // …and the run-lifecycle RECOVERY beside the entry: an un-advertised tool is
        // undispatchable (N17), and a session that opened its run unbound has no other way out.
        assert!(
            names.contains(&crate::mcp_engine::RUN_BIND_SPEC_TOOL),
            "run_bind_spec must be listed"
        );
        // The v2.8 GRAPH-AUTHORING tools (workflow_create/fork/assign + node CRUD + the v2.8-review
        // reads workflow_list/workflow_get/run_list + workflow_delete) advertised alongside, kept
        // OUT of V1_TOOLS (a new v2.8 surface).
        for expected in crate::mcp_authoring::AUTHORING_TOOLS {
            assert!(
                names.contains(&expected),
                "v2.8 authoring tool {expected} must be listed"
            );
        }
        // v2.7 Phase 2: the PROTOTYPE tools (the agent's constrained proto contract) advertised
        // alongside, kept OUT of V1_TOOLS (a new surface).
        for expected in crate::mcp_tools_proto::PROTO_TOOLS {
            assert!(
                names.contains(&expected),
                "v2.7 proto tool {expected} must be listed"
            );
        }
        // v2.7 review #23: the DESIGN-SYSTEM trigger (`design_inventory`) advertised alongside,
        // kept OUT of V1_TOOLS (a new surface).
        for expected in crate::mcp_tools_design::DESIGN_TOOLS {
            assert!(
                names.contains(&expected),
                "v2.7 design tool {expected} must be listed"
            );
        }
        // The Système scope's DEDICATED discovery, advertised alongside and kept OUT of V1_TOOLS.
        // It is the agent's ONLY door onto a container `list_projects` / `list_workspaces`
        // deliberately omit — so if it were not listed, the Système services would be
        // undiscoverable rather than merely unlisted.
        assert!(
            names.contains(&LIST_SYSTEM_COMMANDS_TOOL),
            "list_system_commands must be listed"
        );
        assert!(
            !V1_TOOLS.contains(&LIST_SYSTEM_COMMANDS_TOOL),
            "…and it stays OUT of the frozen v1 surface, like every extension tool"
        );
        // …and the approval protocol's ONE extra tool beside it: the token-owned read of a
        // request. It is advertised because an agent has no other way to learn what the human
        // decided; grant/deny are deliberately NOT advertised (UI-only db-commands), which
        // `system_scope::tests::the_two_halves_of_the_allow_list_do_not_share_a_name_with_the_live_mcp_surface`
        // pins.
        assert!(
            names.contains(&SYSTEM_APPROVAL_STATUS_TOOL),
            "system_approval_status must be listed"
        );
        assert!(
            !V1_TOOLS.contains(&SYSTEM_APPROVAL_STATUS_TOOL),
            "…and it stays OUT of the frozen v1 surface, like every extension tool"
        );
        assert_eq!(
            names.len(),
            V1_TOOLS.len()
                // 29 since an agent driving another agent got its bounded wait
                // (wait_for_terminal); it was 28 when the Système approval protocol got its one
                // status tool (system_approval_status), 27 when the scope got its dedicated
                // discovery (list_system_commands), 26 when the command-LIST order was opened
                // (reorder_commands), and 25 when the multi-pane grouping landed
                // (manage_command_group).
                + 29
                + 9
                + 3
                + crate::mcp_tools_spec::SPEC_TOOLS.len()
                + crate::mcp_tools_idea::IDEA_TOOLS.len()
                + crate::mcp_tools_annexe::ANNEXE_TOOLS.len()
                + crate::mcp_authoring::AUTHORING_TOOLS.len()
                + crate::mcp_tools_proto::PROTO_TOOLS.len()
                + crate::mcp_tools_design::DESIGN_TOOLS.len(),
            "exactly the v1 surface plus probe + wait_for_command + \
             add_command/update_command/import_commands + remove_workspace/remove_command + \
             clear_command_output + list_importable_scripts/remove_commands + \
             manage_command_group + reorder_commands + list_system_commands + \
             system_approval_status + \
             create_terminal/send_to_terminal/send_keys/list_terminals/close_terminal/read_terminal + \
             wait_for_terminal + \
             agent_session_event + set_project_settings + project_set_repos + workspace_runtime + \
             workspace_git + workspace_timeline + the spec authoring/transition tools \
             (incl. task_delete + the v2.6 spec_link/spec_unlink/spec_deps) + \
             agent_context + repo_diff + repo_git_view + repo_watch + repo_unwatch + \
             review_comment_list + review_comment_post + review_comment_resolve + review_task_list + \
             review_task_structure + review_submit_feedback + the 12 v2.6 idea tools + \
             the 4 v2.6 annexe tools + the 3 v2.8 engine runtime tools (run_start + its \
             recovery twin run_bind_spec + nyx_next) + \
             the 13 v2.8 graph-authoring tools (7 mutations + slot_bind + \
             workflow_list/workflow_get/run_list reads + workflow_delete/run_delete) + the 16 v2.7 proto tools (9 read/lifecycle, \
             incl. the agent recommendation proto_recommend_variant, + 7 creation/amorçage) + \
             the 6 v2.7 design tools (design_inventory + design_recipe + design_refresh_component + \
             design_delete_component + design_tokens + design_preview) extension tools"
        );
    }

    #[test]
    fn tools_call_without_dispatcher_is_method_not_found() {
        let (_server, port) = start_test_server();
        let req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_projects","arguments":{}}}"#;
        let (_status, body) = http_request(port, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["error"]["code"], -32601,
            "no dispatcher → method not found"
        );
    }

    #[test]
    fn dispatcher_routes_tools_call() {
        // A recording dispatcher proves the phase-2 extension point is wired and
        // that a tool result is wrapped in the MCP `tools/call` envelope.
        struct Echo;
        impl ToolDispatcher for Echo {
            fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
                Ok(json!({ "tool": name, "args": arguments }))
            }
        }
        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(Echo));
        let req = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_projects","arguments":{"k":1}}}"#;
        let (_status, body) = http_request(port, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["isError"], false);
        assert_eq!(v["result"]["structuredContent"]["tool"], "list_projects");
        assert_eq!(v["result"]["structuredContent"]["args"]["k"], 1);
    }

    /// An answer travels ONCE. nyx used to serialize every tool payload into `content[0].text`
    /// *and* hand it over again as `structuredContent`; measured on an ordinary `list_terminals`
    /// reply that was 3 701 envelope bytes for 1 686 bytes of payload — **+46 %** — paid on every
    /// read of a loop that reads a terminal dozens of times.
    ///
    /// The assertion is on the FACT, not on a byte count: the payload appears in the envelope
    /// exactly once. The size guard that follows is deliberately loose — it only has to fail if
    /// the mirror ever comes back, and doubling is what coming back looks like.
    #[test]
    fn a_tool_answer_travels_once_not_twice() {
        // The shape a terminal-driving loop actually reads back, in miniature.
        let payload = json!({
            "terminals": [
                { "id": "01a05e7e", "label": "probe-claude", "agent_activity": "working" },
                { "id": "01a05e7f", "label": "build", "agent_activity": "idle" },
            ]
        });
        let envelope = tool_result_envelope(payload.clone());

        assert_eq!(
            envelope["content"],
            json!([]),
            "an object answer keeps NO text mirror — `content` is present (MCP requires the key) \
             and empty: {envelope}"
        );
        assert_eq!(
            envelope["structuredContent"], payload,
            "…and the payload itself is intact in the one channel that carries it"
        );
        assert_eq!(envelope["isError"], json!(false));

        let payload_len = serde_json::to_string(&payload).unwrap().len();
        let envelope_len = serde_json::to_string(&envelope).unwrap().len();
        assert!(
            envelope_len < payload_len + payload_len / 2,
            "the envelope costs {envelope_len} bytes for {payload_len} bytes of payload — the \
             text mirror is back"
        );

        // THE ONE EXCEPTION, and it is a correctness one, not a size one: MCP types
        // `structuredContent` as an OBJECT, so a non-object answer cannot travel there at all and
        // would reach the client as nothing. Those — and only those — keep the text form.
        let scalar = tool_result_envelope(json!([1, 2, 3]));
        assert_eq!(scalar["content"][0]["text"], json!("[1,2,3]"));
        assert!(
            scalar.get("structuredContent").is_none(),
            "a non-object answer never claims to be structured content: {scalar}"
        );
    }

    /// A dispatcher whose `slow` tool PARKS inside the handler until the test releases it, and
    /// whose every other tool returns at once. The block is a condvar, not a sleep: the test knows
    /// exactly when a call is inside the handler (it is told), and exactly when it leaves (it says
    /// so) — so the two threading tests below assert on facts, never on a race they hope to win.
    struct ParkingDispatcher {
        /// Announces that a `slow` call has REACHED the handler (one message per call).
        entered: Mutex<std::sync::mpsc::Sender<()>>,
        released: Mutex<bool>,
        release: std::sync::Condvar,
    }

    impl ParkingDispatcher {
        fn new() -> (Arc<Self>, std::sync::mpsc::Receiver<()>) {
            let (tx, rx) = std::sync::mpsc::channel();
            (
                Arc::new(ParkingDispatcher {
                    entered: Mutex::new(tx),
                    released: Mutex::new(false),
                    release: std::sync::Condvar::new(),
                }),
                rx,
            )
        }

        /// Let every parked `slow` call return.
        fn release_all(&self) {
            *self.released.lock().unwrap() = true;
            self.release.notify_all();
        }
    }

    impl ToolDispatcher for ParkingDispatcher {
        fn call(&self, name: &str, _arguments: &Value) -> Result<Value, RpcError> {
            if name != "slow" {
                return Ok(json!({ "served": name }));
            }
            let _ = self.entered.lock().unwrap().send(());
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.release.wait(released).unwrap();
            }
            Ok(json!({ "served": "slow" }))
        }
    }

    fn tools_call(id: u32, tool: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{tool}","arguments":{{}}}}}}"#
        )
    }

    /// POST a `tools/call` from its OWN thread, handing the response back over a channel so the test
    /// can put a DEADLINE on being served (a `.join()` could only ever wait forever).
    fn tools_call_async(port: u16, id: u32, tool: &str) -> std::sync::mpsc::Receiver<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let body = tools_call(id, tool);
        std::thread::spawn(move || {
            let (_status, body) = http_request(port, "POST", "/mcp", Some(&body));
            let _ = tx.send(body);
        });
        rx
    }

    /// v2.8 L1 — THE freeze the user hit twice in one day, reproduced: a LONG `tools/call` must not
    /// stop the MCP surface from serving anything else.
    ///
    /// The real shape is a `nyx_next` whose `gate_check` runs the project's test suite inline (up to
    /// `mcp_engine::GATE_TOTAL_TIMEOUT` = 90s); the user reproduced it by binding `sleep 60; bun
    /// test` and watching the app freeze for a full minute. Here the duration is CONTROLLED — the
    /// slow tool parks until this test releases it — so the test asserts the structural property
    /// (the surface stays served) with no timing luck involved.
    ///
    /// Served on the accept thread, the second call gets nothing until the first returns; on the
    /// worker pool it is answered while the first is still parked.
    #[test]
    fn a_parked_tool_call_does_not_freeze_the_rest_of_the_surface() {
        let (dispatcher, entered) = ParkingDispatcher::new();
        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::clone(&dispatcher) as Arc<dyn ToolDispatcher>);

        let slow = tools_call_async(port, 1, "slow");
        // Proceed only once the slow call is PROVABLY inside the handler, holding a worker.
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("the slow tools/call reached the dispatcher");

        // The surface must still answer, and answer CORRECTLY, while that call is in there.
        let fast = tools_call_async(port, 2, "fast");
        let body = fast.recv_timeout(Duration::from_secs(3)).expect(
            "the MCP surface answered NOTHING while one tools/call was in flight — a long call is \
             being served on the accept thread again, which freezes every other agent, session and \
             tool call for as long as it runs",
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["structuredContent"]["served"], "fast");
        assert_eq!(v["result"]["isError"], false);

        // The liveness probe the cockpit polls is on the same surface and must survive it too.
        let (health_status, health_body) = http_request(port, "GET", "/health", None);
        assert!(
            health_status.contains("200"),
            "GET /health during a long tools/call: {health_status}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&health_body).unwrap()["status"],
            "ok"
        );

        // And the long call itself still completes normally once its work is done.
        dispatcher.release_all();
        let slow_body = slow
            .recv_timeout(Duration::from_secs(10))
            .expect("the released slow call returns");
        let v: Value = serde_json::from_str(&slow_body).unwrap();
        assert_eq!(v["result"]["structuredContent"]["served"], "slow");
    }

    /// v2.8 L1 — what happens when the pool SATURATES: the request WAITS for a free worker, and is
    /// then served normally. It is never dropped, never answered by anything other than the handler,
    /// and never met with a lie.
    ///
    /// Every worker is parked in the dispatcher, so the extra call cannot be served yet; the test
    /// proves it is still QUEUED (it never reached the dispatcher, and nothing answered it), then
    /// releases the pool and proves the SAME call is served.
    #[test]
    fn a_saturated_pool_queues_the_next_request_instead_of_dropping_it() {
        let (dispatcher, entered) = ParkingDispatcher::new();
        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::clone(&dispatcher) as Arc<dyn ToolDispatcher>);

        // Fill every worker, and wait until each one is provably inside the handler.
        let parked: Vec<_> = (0..REQUEST_WORKERS)
            .map(|i| tools_call_async(port, i as u32, "slow"))
            .collect();
        for i in 0..REQUEST_WORKERS {
            entered
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|e| panic!("worker {i} took a parked call: {e}"));
        }

        // One more, with nowhere to run it.
        let queued = tools_call_async(port, 99, "queued");
        assert!(
            queued
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "with every worker busy the extra request must WAIT — it was answered, so something \
             other than a worker replied to it"
        );

        // Freeing the pool serves it — the request was held, not thrown away.
        dispatcher.release_all();
        let body = queued.recv_timeout(Duration::from_secs(10)).expect(
            "a request queued behind a saturated pool must be SERVED once a worker frees up, never \
             dropped",
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["structuredContent"]["served"], "queued");
        for (i, slow) in parked.into_iter().enumerate() {
            slow.recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|e| panic!("parked call {i} returns: {e}"));
        }
    }

    /// The dispatcher the two auto-map wire tests below share: it mirrors the real
    /// `SidecarDispatcher::call_with_caller` by threading the caller into
    /// `dispatch_extension_tool` against a live in-memory `Db`, over a throwaway no-op runner +
    /// host (the sidecar's own pre-runtime path).
    ///
    /// It is at module level rather than inside one test because BOTH the `spec_create` seam and
    /// the `run_start` seam need exactly it, and a second copy is a second thing to keep in step
    /// with the production dispatcher.
    struct AutoMapDispatcher {
        db: crate::db::Db,
    }
    impl AutoMapDispatcher {
        fn dispatch(
            &self,
            name: &str,
            args: &Value,
            caller: Option<&str>,
        ) -> Result<Value, RpcError> {
            struct NoopSink;
            impl crate::command::RunnerSink for NoopSink {
                fn on_state(&self, _: &str, _: crate::command::RunState, _: Option<i32>) {}
                fn on_acknowledge(&self, _: &str) {}
                fn on_output(&self, _: &str, _: &[u8]) {}
                fn persist_scrollback(&self, _: &str, _: &str) {}
                fn archive_previous_run(&self, _: &str) {}
                fn clear_output(&self, _: &str) {}
            }
            let runner = crate::command::CommandRunner::new(
                NoopSink,
                portable_pty::PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            );
            let host = crate::mcp_tools_core::NoTerminalHost;
            let activity = crate::agent_activity::AgentActivityStore::new();
            match crate::mcp_tools_core::dispatch_extension_tool(
                &self.db, &runner, &host, &activity, name, args, caller,
            ) {
                Some(res) => res.map(|o| o.result),
                None => Err(RpcError::new(
                    RpcCode::MethodNotFound,
                    format!("unknown tool '{name}'"),
                )),
            }
        }
    }
    impl ToolDispatcher for AutoMapDispatcher {
        fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
            self.dispatch(name, arguments, None)
        }
        fn call_with_caller(
            &self,
            name: &str,
            arguments: &Value,
            caller_terminal_id: Option<&str>,
        ) -> Result<Value, RpcError> {
            self.dispatch(name, arguments, caller_terminal_id)
        }
    }

    /// #K18 — A BLOCKING WAIT MUST NOT EAT THE WORKER ITS OWN RESOLVER NEEDS.
    ///
    /// `wait_for_terminal` parks a worker for up to a minute, and the signal that ends it is the
    /// agent's `Stop` hook — which reaches nyx as a `POST /mcp` served by THIS SAME pool. Four
    /// concurrent waits (piloting four sessions, the whole point of these tools) therefore used to
    /// deadlock themselves: the hook POSTs queued behind the waits, each hook abandoned at its own
    /// 1 s timeout with no replay, and all four waits came back `resolved: false` after a minute on
    /// turns that had ended immediately.
    ///
    /// The cap closes it at the source. This drives the REAL HTTP surface: `REQUEST_WORKERS`
    /// simultaneous waits, of which the last is REFUSED BY NAME straight away — which is what frees
    /// the worker that then serves a concurrent hook POST while the other three are still blocked.
    #[test]
    fn concurrent_terminal_waits_always_leave_a_worker_for_the_hook_that_ends_them() {
        use crate::mcp_tools_core::MAX_CONCURRENT_TERMINAL_WAITS;
        // The slot counter is process-global — take the same lock every other wait test takes.
        let _g = crate::mcp_tools_core::tests::db_guard();

        /// `wait` runs the REAL `wait_for_terminal` against a live in-memory DB; any other name is
        /// the short request a hook is (it answers and gives its worker straight back).
        struct WaitDispatcher {
            db: crate::db::Db,
            activity: Arc<crate::agent_activity::AgentActivityStore>,
            terminal_id: String,
        }
        impl ToolDispatcher for WaitDispatcher {
            fn call(&self, name: &str, _arguments: &Value) -> Result<Value, RpcError> {
                if name != "wait" {
                    return Ok(json!({ "served": name }));
                }
                // `until: ["working"]` on an idle store never resolves, so each accepted wait
                // blocks for its whole timeout — the shape of a real wait on a turn in flight.
                crate::mcp_tools_core::wait_for_terminal(
                    &self.db,
                    &crate::mcp_tools_core::NoTerminalHost,
                    &self.activity,
                    &json!({
                        "terminal_id": self.terminal_id,
                        "until": ["working"],
                        "timeout_ms": 3_000,
                    }),
                )
            }
        }

        let db = crate::db::Db::in_memory();
        let activity = Arc::new(crate::agent_activity::AgentActivityStore::new());
        let terminal_id = crate::mcp_tools_core::create_terminal(
            &db,
            &crate::mcp_tools_core::NoTerminalHost,
            &json!({}),
        )
        .unwrap()
        .result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();

        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(WaitDispatcher {
            db,
            activity,
            terminal_id,
        }) as Arc<dyn ToolDispatcher>);

        // Fire ONE WAIT PER WORKER. Only `MAX_CONCURRENT_TERMINAL_WAITS` of them may block.
        let waits: Vec<_> = (0..REQUEST_WORKERS)
            .map(|i| tools_call_async(port, i as u32, "wait"))
            .collect();

        // Exactly one comes back FAST, and with a refusal that says what to do instead.
        let mut refused: Option<Value> = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while refused.is_none() && std::time::Instant::now() < deadline {
            for w in &waits {
                if let Ok(body) = w.recv_timeout(Duration::from_millis(50)) {
                    refused = Some(serde_json::from_str(&body).unwrap());
                    break;
                }
            }
        }
        let refused = refused.expect(
            "with every worker taking a blocking wait, the one over the cap must be REFUSED \
             immediately — it was still blocking, so the cap is not enforced",
        );
        let message = refused["error"]["message"].as_str().unwrap_or_default();
        assert_eq!(
            refused["error"]["data"]["code"],
            json!("invalid_state"),
            "the refusal is a named domain error, not a timeout: {refused}"
        );
        assert!(
            message.contains("too many blocking calls"),
            "the refusal NAMES the cap: {message}"
        );
        assert!(
            message.contains("read_terminal"),
            "and tells the agent what to do instead: {message}"
        );
        assert_eq!(
            refused["error"]["data"]["limit"],
            json!(MAX_CONCURRENT_TERMINAL_WAITS)
        );

        // THE POINT OF THE CAP: a hook POST is served WHILE the other waits are still blocked —
        // well inside the 1 s the plugin's hook gives up after.
        let hook = tools_call_async(port, 99, "agent_session_event");
        let body = hook.recv_timeout(Duration::from_millis(900)).expect(
            "the hook that ENDS these waits could not be served while they were blocking — the \
             waits are starving their own resolver",
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["structuredContent"]["served"], "agent_session_event");

        // And the accepted waits still return normally on their own timeout (a NORMAL outcome).
        for w in waits {
            if let Ok(body) = w.recv_timeout(Duration::from_secs(10)) {
                let v: Value = serde_json::from_str(&body).unwrap();
                if v.get("error").is_none() {
                    assert_eq!(v["result"]["structuredContent"]["resolved"], json!(false));
                }
            }
        }
    }

    #[test]
    fn header_x_nyx_terminal_id_threads_the_caller_into_the_auto_map() {
        // v2.5.10 Phase-1 review finding 01KXNC0KHS2YX3KHRMK9QW803P: the PRODUCTION seam
        // — the `X-Nyx-Terminal-Id` header on a `POST /mcp` → `caller_terminal_id`
        // extracted in `handle_request` → threaded `handle_rpc` → `handle_tools_call` →
        // `ToolDispatcher::call_with_caller` → the auto-map — was proven only from
        // `dispatch_extension_tool` INWARD (the caller injected directly in
        // `mcp_tools_core::tests::auto_map_adds_worked_specs_to_the_caller_session_not_reads`).
        // The test dispatchers never override `call_with_caller`, so a POST carrying the
        // header was unobservable. This drives the WHOLE chain over the REAL loopback wire:
        // a `spec_create` carrying the header lands the worked Spec in the header terminal's
        // live-session set; the SAME call WITHOUT the header maps nothing.
        //
        // The dispatcher is [`AutoMapDispatcher`] above: it mirrors the real
        // `SidecarDispatcher::call_with_caller`.

        // Live substrate: a project + a terminal hosting a LIVE agent session (the caller).
        let db = crate::db::Db::in_memory();
        let (project_id, terminal_id) = db.with_conn(|c| {
            let (project, _root) = crate::db::create_project(c, "Proj", "/work", None).unwrap();
            let terminal = crate::db::create_terminal(c, "/work", None).unwrap();
            crate::db::record_session_start(
                c,
                &terminal.id,
                crate::db::AGENT_KIND_CLAUDE_CODE,
                crate::db::SessionCapture {
                    external_session_id: "ext-hdr-1".into(),
                    cwd: "/work".into(),
                    ..Default::default()
                },
            )
            .unwrap();
            (project.id, terminal.id)
        });
        let set = |db: &crate::db::Db| {
            db.with_conn(|c| crate::db::specs_for_terminal(c, &terminal_id))
                .unwrap()
        };

        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(AutoMapDispatcher { db: db.clone() }));

        // 1) `spec_create` carrying the X-Nyx-Terminal-Id header → the worked Spec joins the
        //    HEADER terminal's live-session set (header → caller → auto-map, over the wire).
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{}","arguments":{{"project_id":"{}","title":"Spec via header"}}}}}}"#,
            crate::mcp_tools_spec::SPEC_CREATE_TOOL,
            project_id
        );
        let (status, body) = http_post_mcp(port, &req, Some(&terminal_id));
        assert!(status.contains("200"), "spec_create call 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["result"]["isError"], false,
            "spec_create must succeed: {body}"
        );
        let mapped = set(&db);
        assert_eq!(
            mapped.len(),
            1,
            "the header terminal's session set gains the worked Spec (header→caller→auto-map)"
        );
        let spec_with_header = v["result"]["structuredContent"]["spec"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            mapped[0].id, spec_with_header,
            "the auto-mapped Spec is exactly the one the header call created"
        );

        // 2) The SAME call WITHOUT the header → caller is `None` → the auto-map arms nothing.
        //    (Guards the seam: if the header extraction/threading regressed to always-Some,
        //    this would wrongly map a second Spec and the assert would fail.)
        let req_no_hdr = format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"{}","arguments":{{"project_id":"{}","title":"Spec no header"}}}}}}"#,
            crate::mcp_tools_spec::SPEC_CREATE_TOOL,
            project_id
        );
        let (status2, body2) = http_post_mcp(port, &req_no_hdr, None);
        assert!(status2.contains("200"), "second call 200, got {status2}");
        let v2: Value = serde_json::from_str(&body2).unwrap();
        assert_eq!(
            v2["result"]["isError"], false,
            "spec_create still succeeds without the header: {body2}"
        );
        assert_eq!(
            set(&db).len(),
            1,
            "no X-Nyx-Terminal-Id header → no caller → the set is unchanged (no auto-map)"
        );
    }

    /// THE SEAM THE SESSION HOOK ACTUALLY USES — `run_start`, over the wire, with and without the
    /// header (human review 01M1H5QY53WMGA8Y46DC2T655W).
    ///
    /// The test above proves the chain on `spec_create`, which is the tool `/nspec` reaches through
    /// `.mcp.json` — a transport that has always carried `X-Nyx-Terminal-Id`. `/ndev` reaches
    /// nothing of the sort at its start: the `UserPromptSubmit` HOOK
    /// (`resources/claude-plugin/hooks/nyx-session-hook.cjs`) opens the run with a `run_start` of
    /// its own, and it sent `content-type` / `content-length` and nothing else. So `run_start`
    /// arrived with `caller_terminal_id: None`, `dispatch_extension_tool` skipped
    /// `link_terminal_active_session_spec`, and the terminal's dev overlay never learnt which Spec
    /// the run was on — `/nspec` mapped, `/ndev` never did.
    ///
    /// `run_start` is a MUTATING tool that reports the Spec it bound (`spec_id` in its result), so
    /// it is auto-mappable exactly like `spec_create`; the header was the only missing half. This
    /// pins BOTH directions over the real loopback wire so the fix cannot silently come undone.
    #[test]
    fn run_start_over_the_wire_auto_maps_only_when_the_terminal_header_is_sent() {
        use crate::db::{self as ndb, SpecSections, SpecTransition};

        let db = ndb::Db::in_memory();
        db.with_conn(crate::seed::seed_builtin_workflows)
            .expect("the built-in ndev/coder graphs the run resolves");

        // A project with its root workspace, an in-progress Spec anchored on it, and a terminal
        // hosting a LIVE agent session — the caller the header names.
        let (spec_id, terminal_id) = db.with_conn(|c| {
            let (project, root) =
                ndb::create_project(c, "Proj", "/work-runstart", None).expect("project");
            let (spec, _rev) = ndb::create_spec(
                c,
                &project.id,
                "Spec driven by /ndev",
                SpecSections {
                    problem: "the overlay must know".into(),
                    solution: "send the header".into(),
                    ..Default::default()
                },
            )
            .expect("spec");
            ndb::transition_spec(c, &spec.id, SpecTransition::Ready, true).expect("ready");
            ndb::transition_spec_with_workspace(
                c,
                &spec.id,
                SpecTransition::Start,
                true,
                Some(&root.id),
            )
            .expect("start");
            let terminal = ndb::create_terminal(c, "/work-runstart", None).expect("terminal");
            ndb::record_session_start(
                c,
                &terminal.id,
                ndb::AGENT_KIND_CLAUDE_CODE,
                ndb::SessionCapture {
                    external_session_id: "ext-runstart-1".into(),
                    cwd: "/work-runstart".into(),
                    ..Default::default()
                },
            )
            .expect("session");
            (spec.id, terminal.id)
        });
        let mapped = |db: &ndb::Db| {
            db.with_conn(|c| ndb::specs_for_terminal(c, &terminal_id))
                .unwrap()
        };
        assert!(mapped(&db).is_empty(), "nothing is mapped before the call");

        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(AutoMapDispatcher { db: db.clone() }));
        let run_start = |id: u32| {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{}","arguments":{{"spec_id":"{spec_id}"}}}}}}"#,
                crate::mcp_engine::RUN_START_TOOL,
            )
        };

        // 1) WITHOUT the header — the shape the hook shipped. The run opens, and NOTHING is mapped:
        //    no caller, no `agent_session_specs` row, no overlay.
        let (status, body) = http_post_mcp(port, &run_start(1), None);
        assert!(status.contains("200"), "run_start call 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["isError"], false, "run_start must succeed: {body}");
        assert_eq!(
            v["result"]["structuredContent"]["spec_id"], spec_id,
            "the run IS bound to the Spec — the mapping's raw material was there all along: {body}"
        );
        assert!(
            mapped(&db).is_empty(),
            "no X-Nyx-Terminal-Id → no caller → no auto-map: this is the bug, pinned"
        );

        // 2) WITH the header — the shape the hook ships now. ONE `agent_session_specs` row, naming
        //    that terminal's live session and that Spec.
        let (status2, body2) = http_post_mcp(port, &run_start(2), Some(&terminal_id));
        assert!(status2.contains("200"), "second run_start 200, got {status2}");
        let v2: Value = serde_json::from_str(&body2).unwrap();
        assert_eq!(v2["result"]["isError"], false, "run_start must succeed: {body2}");
        let linked = mapped(&db);
        assert_eq!(
            linked.len(),
            1,
            "the header terminal's live session gains exactly the Spec the run is bound to"
        );
        assert_eq!(linked[0].id, spec_id);
    }

    #[test]
    fn tools_call_error_carries_named_d8_code_in_error_data() {
        // Review R-OUTPUT task #3: a tool error must surface its NAMED D8 code in the
        // structured `error.data.code` field so an agent can distinguish e.g.
        // `output_too_large` from any other server error — while the numeric JSON-RPC
        // `error.code` stays -32000 (the server-error envelope). A tiny dispatcher
        // returns each D8 code; we assert both fields over the REAL loopback wire.
        struct D8Errors;
        impl ToolDispatcher for D8Errors {
            fn call(&self, name: &str, _arguments: &Value) -> Result<Value, RpcError> {
                // The tool name doubles as the D8 code to emit, so one dispatcher
                // covers every code in the vocabulary.
                let code: RpcCode = match name {
                    "output_too_large" => RpcCode::OutputTooLarge,
                    "invalid_id" => RpcCode::InvalidId,
                    "invalid_state" => RpcCode::InvalidState,
                    "mcp_unavailable" => RpcCode::McpUnavailable,
                    "internal" => RpcCode::Internal,
                    _ => RpcCode::InvalidArgument,
                };
                Err(RpcError::new(code, format!("synthetic {code}")))
            }
        }
        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(D8Errors));

        let call_err = |id: u32, name: &str| -> Value {
            let req = format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{{}}}}}}"#
            );
            let (_status, body) = http_request(port, "POST", "/mcp", Some(&req));
            serde_json::from_str::<Value>(&body).unwrap()
        };

        // get_command_output over the cap returns error.data.code = output_too_large,
        // numeric code -32000 (server error), per the task's primary done_criterion.
        let v = call_err(1, "output_too_large");
        assert_eq!(
            v["error"]["data"]["code"], "output_too_large",
            "the named D8 code is on the wire in error.data.code"
        );
        assert_eq!(
            v["error"]["code"], -32000,
            "the numeric JSON-RPC code stays -32000 for a domain error"
        );

        // Every other D8 code surfaces its name in error.data.code too. invalid_argument
        // keeps the JSON-RPC -32602 numeric code (it is a params fault) but still names
        // its D8 code in data; the rest are -32000.
        for (name, numeric) in [
            ("invalid_id", -32000),
            ("invalid_state", -32000),
            ("mcp_unavailable", -32000),
            ("internal", -32000),
            ("invalid_argument", -32602),
        ] {
            let v = call_err(2, name);
            assert_eq!(
                v["error"]["data"]["code"], name,
                "{name} must surface in error.data.code"
            );
            assert_eq!(v["error"]["code"], numeric, "{name} numeric JSON-RPC code");
        }
    }

    /// DONE-CRITERION (« Protocole MCP », revue humaine 01M1C7C8K0Q1WBHDQKZ29GH8YJ) — the WHOLE
    /// loop over the real JSON-RPC wire, and with NO terminal identity anywhere.
    ///
    /// The absent header is half the point. The approval protocol exists precisely for a caller
    /// that has no terminal of its own — a headless agent, an MCP client the human never opened a
    /// pane for — so every request below is posted WITHOUT `X-Nyx-Terminal-Id`.
    ///
    /// THE ASSERTIONS READ ONLY WHAT A POOR CLIENT RENDERS: `result`, and an error's `code` and
    /// `message`. Never `error.data`. That restriction is the finding this test was rewritten for
    /// (01M1C7CWMF4ABHRWT1DGBJTXW3): the previous version drove the same wire and passed while the
    /// capability was unusable, because it read a bag no MCP client is obliged to show the model.
    ///
    /// The last assertion is the other half: the UI's decision bridge is NOT reachable here. An
    /// agent that could call `system_approval_grant` would be approving its own request.
    #[test]
    fn the_system_approval_protocol_runs_end_to_end_over_json_rpc_without_a_terminal_identity() {
        struct NoopSink;
        impl crate::command::RunnerSink for NoopSink {
            fn on_state(&self, _: &str, _: crate::command::RunState, _: Option<i32>) {}
            fn on_acknowledge(&self, _: &str) {}
            fn on_output(&self, _: &str, _: &[u8]) {}
            fn persist_scrollback(&self, _: &str, _: &str) {}
            fn archive_previous_run(&self, _: &str) {}
            fn clear_output(&self, _: &str) {}
        }
        /// The SAME routing a shell installs: every advertised tool through
        /// `dispatch_extension_tool`, and `method_not_found` for anything it does not own.
        struct SystemDispatcher {
            db: crate::db::Db,
        }
        impl ToolDispatcher for SystemDispatcher {
            fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
                let runner = crate::command::CommandRunner::new(
                    NoopSink,
                    portable_pty::PtySize {
                        rows: 24,
                        cols: 80,
                        pixel_width: 0,
                        pixel_height: 0,
                    },
                );
                let activity = crate::agent_activity::AgentActivityStore::new();
                // The blocking window, shortened ON THE WORKER THREAD that actually serves the
                // call — the seam is thread-local, so setting it in the test's own thread would
                // reach nothing here.
                crate::system_approvals::with_wait_cap(200, || {
                    match crate::mcp_tools_core::dispatch_extension_tool(
                        &self.db,
                        &runner,
                        &crate::mcp_tools_core::NoTerminalHost,
                        &activity,
                        name,
                        arguments,
                        None,
                    ) {
                        Some(res) => res.map(|o| o.result),
                        None => Err(RpcError::new(
                            RpcCode::MethodNotFound,
                            format!("unknown tool `{name}`"),
                        )),
                    }
                })
            }
        }

        let db = crate::db::Db::in_memory();
        let project_id = db
            .with_conn(|c| {
                crate::system_scope::ensure_system_scope(
                    c,
                    &crate::system_scope::FixedHomeDirProvider::at(if cfg!(windows) {
                        "C:\\Users\\kris"
                    } else {
                        "/home/kris"
                    }),
                )
            })
            .expect("bootstrap")
            .project
            .id;

        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(SystemDispatcher { db: db.clone() }));
        let call = |id: u32, name: &str, arguments: Value| -> Value {
            let req = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments },
            })
            .to_string();
            // `None` — NO terminal identity header. The protocol must work for a caller that has
            // no terminal at all.
            let (_status, body) = http_post_mcp(port, &req, None);
            serde_json::from_str::<Value>(&body).unwrap()
        };
        let templates = || {
            db.with_conn(|c| crate::db::list_templates(c, &project_id))
                .unwrap()
                .len()
        };

        // 1) THE FIRST ATTEMPT — it WAITS for the human, and when the window closes it answers with
        //    a tool RESULT (never an error bag), naming the request and how to keep waiting.
        let args = json!({ "project_id": project_id, "name": "svc", "command": "echo hi" });
        let v = call(1, "add_command", args.clone());
        assert!(
            v.get("error").is_none(),
            "waiting on the user is not a failure of the call: {v}"
        );
        let answer = &v["result"]["structuredContent"];
        assert_eq!(answer["ok"], json!(false), "{v}");
        assert_eq!(
            answer["status"],
            json!(crate::system_approvals::APPROVAL_REQUIRED_CODE)
        );
        let request_id = answer["request_id"].as_str().expect("request_id").to_string();
        assert!(answer["expires_at"].as_i64().is_some());
        // THE WHOLE ANSWER IS READABLE PROSE, in the channel every client renders: the tool
        // RESULT. That is the property the old protocol lacked, and no amount of correct
        // `error.data` replaced it. The prose lives IN the payload (`next`), so it rides the
        // result whichever way the envelope carries it — since the de-duplication that is the
        // whole point of `tool_result_envelope`, that way is `structuredContent`.
        let text = answer["next"].as_str().expect("the rendered prose");
        assert!(text.contains(&request_id), "{text}");
        assert!(text.contains(SYSTEM_APPROVAL_STATUS_TOOL), "{text}");
        assert!(
            !text.contains("token") && !text.contains("replay"),
            "nothing secret travels any more: {text}"
        );
        assert_eq!(templates(), 0, "nothing was written");

        // 2) THE AGENT KEEPS WAITING — by request id alone. There is no credential to carry.
        let v = call(2, SYSTEM_APPROVAL_STATUS_TOOL, json!({ "request_id": request_id }));
        assert_eq!(
            v["result"]["structuredContent"]["state"],
            json!(crate::system_approvals::STATE_PENDING),
            "{v}"
        );

        // 3) THE HUMAN DECIDES, and the DECISION is what performs the operation — through the same
        //    entry point the UI bridge drives, with the effect dispatched under the granted
        //    authority (the sidecar's `McpRuntime::execute_granted`, in miniature).
        let executor = SystemDispatcher { db: db.clone() };
        crate::system_approvals::grant_and_execute(
            &db,
            &crate::system_approvals::SystemClock,
            &request_id,
            Some("ui"),
            &|tool: &str, tool_args: &Value| {
                crate::system_approvals::with_granted_authority(|| {
                    executor.call(tool, tool_args).map_err(|e| e.to_string())
                })
            },
        )
        .expect("the human grants");
        assert_eq!(templates(), 1, "the approval IS the execution");

        // 4) THE AGENT COMES BACK and collects the operation's REAL result — the answer that
        //    arrived while it was not there.
        let v = call(4, SYSTEM_APPROVAL_STATUS_TOOL, json!({ "request_id": request_id }));
        let seen = &v["result"]["structuredContent"];
        assert_eq!(seen["state"], json!(crate::system_approvals::STATE_GRANTED));
        assert_eq!(seen["ok"], json!(true), "{v}");
        assert_eq!(seen["result"]["command"]["name"], json!("svc"), "{v}");

        // 5) ONE SHOT — the same request cannot be decided twice, so nothing runs a second time.
        let again = crate::system_approvals::grant_and_execute(
            &db,
            &crate::system_approvals::SystemClock,
            &request_id,
            Some("ui"),
            &|_: &str, _: &Value| Ok(json!(null)),
        );
        assert!(again.is_err(), "a spent approval is not decidable again");
        assert_eq!(templates(), 1, "and nothing ran twice");

        // 6) THE UI BRIDGE IS NOT AN MCP SURFACE — the decision verbs answer `method_not_found`,
        //    so no agent can grant or deny anything, its own request included.
        for name in [
            "system_approval_grant",
            "system_approval_deny",
            "system_approval_list",
        ] {
            let v = call(6, name, json!({ "requestId": request_id }));
            assert_eq!(
                v["error"]["code"], -32601,
                "`{name}` must not be callable over MCP: {v}"
            );
        }
    }

    #[test]
    fn tools_call_error_data_merges_structured_detail_alongside_code() {
        // PRD-4.1 #3: an RpcError's structured `data` (e.g. output_too_large's
        // `requested`/`limit`) is merged into the wire `error.data` ALONGSIDE the string
        // `code`, so an agent can react to size info without parsing the prose message.
        struct OversizeWithData;
        impl ToolDispatcher for OversizeWithData {
            fn call(&self, _name: &str, _arguments: &Value) -> Result<Value, RpcError> {
                Err(RpcError::new(
                    RpcCode::OutputTooLarge,
                    "requested window exceeds max_bytes (1048576)",
                )
                .with_data(json!({ "requested": 2_000_000, "limit": 1_048_576 })))
            }
        }
        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(OversizeWithData));
        let req = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_terminal","arguments":{}}}"#;
        let (_status, body) = http_request(port, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        // The string code is still present (not clobbered by the merge)...
        assert_eq!(
            v["error"]["data"]["code"], "output_too_large",
            "code stays on error.data"
        );
        // ...and the structured detail is merged in alongside it.
        assert_eq!(
            v["error"]["data"]["requested"], 2_000_000,
            "requested is on the wire"
        );
        assert_eq!(
            v["error"]["data"]["limit"], 1_048_576,
            "limit is on the wire"
        );
        assert_eq!(
            v["error"]["code"], -32000,
            "numeric JSON-RPC code unchanged"
        );
    }

    #[test]
    fn tools_call_error_data_code_is_never_clobbered_by_attached_data() {
        // Defense in depth: even if an error attaches a `data` object that itself
        // carries a key named "code", the canonical ADR-0003 D8 string code is stamped
        // LAST and wins — an agent dispatching on `error.data.code` is never misled.
        struct CodeKeyInData;
        impl ToolDispatcher for CodeKeyInData {
            fn call(&self, _name: &str, _arguments: &Value) -> Result<Value, RpcError> {
                Err(RpcError::new(RpcCode::InvalidArgument, "bad input")
                    .with_data(json!({ "code": "ATTACKER_OVERRIDE", "field": "x" })))
            }
        }
        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(CodeKeyInData));
        let req = r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"probe","arguments":{}}}"#;
        let (_status, body) = http_request(port, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["error"]["data"]["code"], "invalid_argument",
            "the canonical string code wins over a `code` key in attached data"
        );
        assert_eq!(
            v["error"]["data"]["field"], "x",
            "other attached keys still merge through"
        );
    }

    #[test]
    fn probe_tool_round_trips_over_loopback() {
        // The spike's loopback proof (PRD-4 #7, ADR-0004): drive `tools/call probe`
        // over a REAL HTTP socket — exactly the JSON-RPC a Claude Code SessionStart
        // `mcp_tool` hook would send — and confirm the `{ ok: true }` no-op result is
        // wrapped in the MCP envelope. A tiny in-test dispatcher reproduces the real
        // `NyxToolDispatcher::probe` body (a no-op needing no managed state), so this
        // test stays free of the Tauri runtime while proving the transport path.
        struct ProbeOnly;
        impl ToolDispatcher for ProbeOnly {
            fn call(&self, name: &str, _arguments: &Value) -> Result<Value, RpcError> {
                if name == PROBE_TOOL {
                    Ok(json!({ "ok": true, "server": SERVER_NAME, "version": SERVER_VERSION }))
                } else {
                    Err(RpcError::new(
                        RpcCode::MethodNotFound,
                        format!("unknown tool '{name}'"),
                    ))
                }
            }
        }
        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(ProbeOnly));
        let req = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"probe","arguments":{}}}"#;
        let (status, body) = http_request(port, "POST", "/mcp", Some(req));
        assert!(status.contains("200"), "probe call 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["isError"], false);
        assert_eq!(v["result"]["structuredContent"]["ok"], true);
        assert_eq!(v["result"]["structuredContent"]["server"], SERVER_NAME);
    }

    #[test]
    fn agent_session_event_round_trips_over_loopback() {
        // PRD-5 #4 (ADR-0004): drive `tools/call agent_session_event` over a REAL
        // 127.0.0.1 HTTP socket — exactly the JSON-RPC a Claude Code SessionStart
        // `mcp_tool` hook (`mcp__nyx__agent_session_event`) sends, carrying the hook
        // payload (`hook_event_name`, `session_id`, `cwd`, `source`, `NYX_TERMINAL_ID`)
        // as the tool `arguments` — and confirm the captured-session result is wrapped
        // in the MCP envelope. A tiny in-test dispatcher echoes the captured fields so
        // this proves the TRANSPORT path (the same wire the real hook uses); the
        // SAME tool against the real `NyxToolDispatcher` + a live DB is proven in
        // `mcp_tools::tests::session_start_creates_active_row`. Together they cover the
        // loopback transport here and the DB-backed capture there.
        struct SessionChannel;
        impl ToolDispatcher for SessionChannel {
            fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
                if name == "agent_session_event" {
                    // Echo the correlation + capture the hook sent (the shape the real
                    // tool persists), proving the arguments reached the dispatcher.
                    Ok(json!({
                        "event": arguments.get("hook_event_name"),
                        "terminal_id": arguments.get("NYX_TERMINAL_ID"),
                        "external_session_id": arguments.get("session_id"),
                        "state": "active",
                    }))
                } else {
                    Err(RpcError::new(
                        RpcCode::MethodNotFound,
                        format!("unknown tool '{name}'"),
                    ))
                }
            }
        }
        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(SessionChannel));
        // The EXACT JSON-RPC a SessionStart `mcp_tool` hook would send.
        let req = r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"agent_session_event","arguments":{"hook_event_name":"SessionStart","session_id":"claude-sid-1","cwd":"/work/proj","source":"startup","NYX_TERMINAL_ID":"term-xyz"}}}"#;
        let (status, body) = http_request(port, "POST", "/mcp", Some(req));
        assert!(
            status.contains("200"),
            "session event call 200, got {status}"
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["isError"], false);
        let sc = &v["result"]["structuredContent"];
        assert_eq!(sc["event"], "SessionStart");
        assert_eq!(
            sc["terminal_id"], "term-xyz",
            "NYX_TERMINAL_ID round-trips as the correlation key"
        );
        assert_eq!(sc["external_session_id"], "claude-sid-1");
        assert_eq!(sc["state"], "active");
    }

    #[test]
    fn mcp_command_lifecycle_round_trips_over_loopback() {
        // PRD-4 phase-5 GATE (#8), the TRANSPORT half of done_criterion #1
        // ("E2E ou dogfood prouve list/start/relaunch/output"): drive the full
        // command lifecycle — `list_commands` → `start_command` →
        // `relaunch_command` → `get_command_output` — as `tools/call` JSON-RPC
        // over a REAL `127.0.0.1` HTTP socket (the exact wire an agent in nyx
        // uses), and confirm each tool's request/result round-trips inside the MCP
        // envelope. A tiny stateful dispatcher stands in for the Tauri runtime so
        // this test stays free of `tauri::test`; the SAME lifecycle against the
        // REAL `NyxToolDispatcher` + a live `ManagedCommandRunner` + DB is proven
        // in `bridge::tests::mcp_dogfood_lifecycle_is_the_same_instance_the_ui_sees`.
        // Together they cover both seams the gate asks for: the loopback transport
        // here, the same-state-as-the-UI invariant there.
        struct Lifecycle {
            // The single instance's last_state, mutated by start/relaunch so the
            // round-trip reflects a real state transition across calls.
            state: Mutex<String>,
        }
        impl ToolDispatcher for Lifecycle {
            fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
                let id = arguments
                    .get("instance_id")
                    .and_then(Value::as_str)
                    .unwrap_or("inst-1");
                match name {
                    "list_commands" => {
                        let st = self.state.lock().unwrap().clone();
                        Ok(json!({ "commands": [
                            { "instance_id": "inst-1", "command": "echo HELLO", "last_state": st }
                        ] }))
                    }
                    "start_command" | "relaunch_command" => {
                        *self.state.lock().unwrap() = "running".to_string();
                        Ok(json!({ "instance_id": id, "state": "running" }))
                    }
                    "get_command_output" => Ok(json!({
                        "instance_id": id,
                        "output": "HELLO\n",
                        "total_bytes": 6,
                        "returned_bytes": 6,
                        "truncated": false,
                        "cursor": 6,
                    })),
                    other => Err(RpcError::new(
                        RpcCode::MethodNotFound,
                        format!("unknown tool '{other}'"),
                    )),
                }
            }
        }
        let (server, port) = start_test_server();
        server.set_dispatcher(Arc::new(Lifecycle {
            state: Mutex::new("idle".to_string()),
        }));

        // Helper: POST one `tools/call` over the socket and return its
        // `structuredContent` (the tool's JSON result), asserting a clean envelope.
        let call = |id: u32, name: &str, args: &str| -> Value {
            let req = format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{args}}}}}"#
            );
            let (status, body) = http_request(port, "POST", "/mcp", Some(&req));
            assert!(status.contains("200"), "{name} call 200, got {status}");
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["result"]["isError"], false, "{name} must not be an error");
            v["result"]["structuredContent"].clone()
        };

        // 1) list_commands — the instance is discoverable and idle before start.
        let listed = call(1, "list_commands", r#"{"workspace_id":"w1"}"#);
        assert_eq!(listed["commands"][0]["instance_id"], "inst-1");
        assert_eq!(listed["commands"][0]["last_state"], "idle");

        // 2) start_command — the instance goes running.
        let started = call(2, "start_command", r#"{"instance_id":"inst-1"}"#);
        assert_eq!(started["state"], "running");

        // 3) relaunch_command — the SAME instance, still one process, still running.
        let relaunched = call(3, "relaunch_command", r#"{"instance_id":"inst-1"}"#);
        assert_eq!(relaunched["instance_id"], "inst-1");
        assert_eq!(relaunched["state"], "running");

        // list now reflects the transition the prior calls drove (shared state).
        let listed2 = call(4, "list_commands", r#"{"workspace_id":"w1"}"#);
        assert_eq!(listed2["commands"][0]["last_state"], "running");

        // 4) get_command_output — the bounded window + integer cursor round-trips.
        let out = call(5, "get_command_output", r#"{"instance_id":"inst-1"}"#);
        assert_eq!(out["output"], "HELLO\n");
        assert!(
            out["cursor"].is_u64(),
            "cursor is an integer for incremental polling"
        );
    }

    #[test]
    fn start_is_single_instance_no_op_on_second_call() {
        let (server, port) = start_test_server();
        // A second start() on the SAME server is a no-op (latched): returns the same
        // bound port, does not bind a second listener.
        let again = server.start().expect("second start is a no-op");
        assert_eq!(again, port);

        // A SECOND McpServer configured on the SAME port fails to bind (the OS refuses the
        // duplicate listener) — proving a second nyx would not create a second server on the
        // configured port. That port is the one THIS test's server was given, never a shared
        // constant, so a concurrent test process can not be what makes the bind fail.
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(PORT_ENV, port.to_string());
        let second = Arc::new(McpServer::default());
        let result = second.start();
        std::env::remove_var(PORT_ENV);
        assert!(
            result.is_err(),
            "a second server cannot bind the in-use fixed port"
        );
    }

    #[test]
    fn port_is_stable_across_restarts() {
        // Two sequential lifecycles with the same config resolve to the SAME port —
        // the property an onboarded client's config relies on.
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(PORT_ENV, "19017");
        let first = resolve_port();
        let second = resolve_port();
        std::env::remove_var(PORT_ENV);
        assert_eq!(
            first, second,
            "port resolution is stable for the same config"
        );

        static ONCE: Once = Once::new();
        ONCE.call_once(|| { /* document: no global state leaks between resolves */ });
    }

    #[test]
    fn rejects_cross_origin_browser_request() {
        let (_server, port) = start_test_server();
        // A browser Origin pointing off-localhost is rejected (D9, anti-rebinding).
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let req = "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://evil.example.com\r\nConnection: close\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let status = raw.lines().next().unwrap_or("");
        assert!(
            status.contains("403"),
            "off-localhost Origin → 403, got {status}"
        );

        // A localhost Origin is accepted.
        let (status_ok, _) = {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            let r = "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://localhost:1420\r\nConnection: close\r\n\r\n";
            s.write_all(r.as_bytes()).unwrap();
            let mut raw2 = String::new();
            s.read_to_string(&mut raw2).unwrap();
            (raw2.lines().next().unwrap_or("").to_string(), ())
        };
        assert!(
            status_ok.contains("200"),
            "localhost Origin allowed, got {status_ok}"
        );
    }

    #[test]
    fn rejects_rebinding_host_even_without_origin() {
        // DNS-rebinding (D9): a page on attacker.com rebound to 127.0.0.1 issues a
        // SAME-ORIGIN request that carries NO Origin header but a Host of the attacker's
        // hostname. The Host gate must reject it even though Origin is absent — this is
        // the bypass the Origin-only check missed.
        let (_server, port) = start_test_server();
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let req = "POST /mcp HTTP/1.1\r\nHost: attacker.example.com\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let status = raw.lines().next().unwrap_or("");
        assert!(
            status.contains("403"),
            "rebinding Host with no Origin → 403, got {status}"
        );

        // A loopback Host with no Origin (a legitimate non-browser MCP client, incl.
        // case-insensitive `localhost`) is allowed.
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let r = "GET /health HTTP/1.1\r\nHost: LOCALHOST:1420\r\nConnection: close\r\n\r\n";
        s.write_all(r.as_bytes()).unwrap();
        let mut raw2 = String::new();
        s.read_to_string(&mut raw2).unwrap();
        let status_ok = raw2.lines().next().unwrap_or("");
        assert!(
            status_ok.contains("200"),
            "loopback Host, no Origin → 200, got {status_ok}"
        );
    }
}
