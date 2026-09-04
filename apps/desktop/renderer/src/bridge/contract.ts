/**
 * nyxBridge — the SINGLE frontend↔shell frontier.
 *
 * Every interaction the React app has with its host shell (today Tauri; tomorrow
 * Electron) goes through this typed contract. No component imports `@tauri-apps/*`
 * directly: they depend on `NyxBridge`, and a shell-specific adapter
 * (`./tauri.ts`, later `./electron.ts`) implements it. A test fake (`./fake.ts`)
 * implements the same surface so component tests never touch a real IPC layer.
 *
 * This file is the CONTRACT only — interfaces, payload types, error shape, and the
 * documented guarantees (unsubscribe, error serialization, timeouts, per-terminal
 * PTY ordering). It contains no runtime wiring.
 *
 * Scope was derived by inventorying every `@tauri-apps/api` and
 * `@tauri-apps/plugin-*` call-site under `apps/frontend/src` — see
 * `./INVENTORY.md` for the call-site → method/event map.
 *
 * `tauri-plugin-opener`: had NO frontend call-site (no `@tauri-apps/plugin-opener`
 * import, no `openUrl`/`openPath`/`revealItemInDir` anywhere in `src`). Confirmed
 * UNUSED → deliberately OUT of the contract AND removed from the Tauri shell (plugin
 * registration in `lib.rs`, the `tauri-plugin-opener` Cargo dependency, and the
 * `opener:default` capability are all gone). If an "open external URL/path"
 * capability is ever needed, add an `openExternal(target: string)` method here and
 * re-register the plugin.
 */

// ---------------------------------------------------------------------------
// Errors, ordering & timeouts (cross-cutting guarantees)
// ---------------------------------------------------------------------------

/**
 * The serialized error every bridge method rejects with. A shell maps its native
 * failure (a Tauri `invoke` rejection string, an Electron IPC error) into this
 * stable shape so callers never branch on a shell-specific error type.
 *
 * `kind` partitions the failure space so callers can react structurally:
 * - `ipc`      — the transport itself failed (no shell, channel closed, teardown
 *                race). Almost always swallowed by the caller (fail-open).
 * - `command`  — the backend command ran and returned an error (a `Result::Err`
 *                surfaced from Rust); `message` is the backend's error string.
 * - `timeout`  — the call exceeded {@link RequestOptions.timeoutMs} (see below).
 * - `canceled` — the call was aborted via {@link RequestOptions.signal}.
 */
export interface BridgeError {
  readonly kind: "ipc" | "command" | "timeout" | "canceled";
  /** Human-readable message (the backend error string for `command`). */
  readonly message: string;
  /** The command/event name in flight when it failed, when known. */
  readonly source?: string;
  /** The original thrown value, for logging/debugging. Never relied on for control flow. */
  readonly cause?: unknown;
}

/** Type guard: is an unknown caught value a {@link BridgeError}? */
export function isBridgeError(e: unknown): e is BridgeError {
  return (
    typeof e === "object" &&
    e !== null &&
    "kind" in e &&
    "message" in e &&
    typeof (e as BridgeError).message === "string"
  );
}

/** Per-request knobs. All optional; a shell applies sane defaults. */
export interface RequestOptions {
  /**
   * Reject with a `timeout` {@link BridgeError} after this many ms. Default:
   * shell-defined (the Tauri adapter uses no timeout — matching today's bare
   * `invoke` — except where a method documents one). `0`/omitted = no timeout.
   */
  readonly timeoutMs?: number;
  /** Abort the in-flight call; rejects with a `canceled` {@link BridgeError}. */
  readonly signal?: AbortSignal;
}

/**
 * Returned by every `subscribe*` method. Call it to stop receiving events and
 * release the shell-side listener. Idempotent: calling it twice is a safe no-op.
 * MUST be called on teardown to avoid a leaked listener (the Tauri adapter wraps
 * `@tauri-apps/api/event`'s `UnlistenFn`; the Electron adapter removes its IPC
 * handler). May be async under the hood but is exposed as fire-and-forget — the
 * shell swallows unsubscribe failures.
 */
export type Unsubscribe = () => void;

/** A subscription callback. Invoked once per event, in arrival order. */
export type Listener<T> = (payload: T) => void;

// ---------------------------------------------------------------------------
// PTY (interactive terminal) — binary, ordered per terminal
// ---------------------------------------------------------------------------

/**
 * Output chunk for one interactive PTY. `bytes` is RAW terminal output, carried
 * as a byte array (the Tauri adapter receives a JSON `number[]` and normalizes to
 * `Uint8Array`; an Electron adapter passes a `Uint8Array`/`Buffer` straight
 * through). The contract type is `Uint8Array` — adapters own the wire encoding.
 *
 * ORDERING GUARANTEE: for a given `id`, `subscribePtyOutput` callbacks fire in the
 * exact byte order the PTY produced — no reordering, no coalescing that crosses a
 * chunk boundary out of order. Chunks for DIFFERENT ids may interleave freely.
 */
export interface PtyOutput {
  /** The live PTY id (from {@link NyxBridge.ptySpawn}). */
  readonly id: number;
  /** Raw output bytes, in order. */
  readonly bytes: Uint8Array;
}

/** The interactive PTY exited. */
export interface PtyExit {
  readonly id: number;
  /** Natural exit code, or `null` when killed/unknown. */
  readonly code: number | null;
}

/** Options for {@link NyxBridge.ptySpawn}. */
export interface PtySpawnOptions {
  /** Working dir for the shell; `undefined` → backend default (nyx's cwd). */
  readonly cwd?: string;
  readonly cols: number;
  readonly rows: number;
  /**
   * The persistent terminal RECORD id (SQLite `terminals.id`) this session binds
   * to, so the backend maps live `pty_id → record id` for exec-state. `undefined`
   * for a record-less standalone terminal.
   */
  readonly terminalId?: string;
}

// ---------------------------------------------------------------------------
// Command / event payload types reused across the contract
// ---------------------------------------------------------------------------

/** `command://output` — coalesced output for a managed-command instance. */
export interface CommandOutput {
  readonly id: string;
  readonly bytes: Uint8Array;
}

/** `command://state` — a managed-command run-state transition. */
export interface CommandState {
  readonly id: string;
  readonly state: string;
  readonly exitCode: number | null;
}

/** `command://ack` — a managed-command "unseen result" flag was cleared. */
export interface CommandAck {
  readonly id: string;
}

/** `terminal://busy-state` — OS-derived busy/idle transition for a terminal. */
export interface TerminalBusyState {
  readonly id: string;
  readonly busy: boolean;
}

/** `terminal://exec-state` — OSC 133 / exec-state badge transition. */
export interface TerminalExecState {
  readonly id: string;
  readonly state: string;
  readonly exitCode: number | null;
}

// ---------------------------------------------------------------------------
// v2.4 review: the Git DIFF read model (the `repo_diff` / `repo_git_view` db-commands)
// ---------------------------------------------------------------------------
//
// These mirror, on the wire, the `nyx_core::git_diff` / `nyx_core::git_views` serde
// shapes (snake_case) the sidecar db-commands return. The review viewer (phase 3)
// consumes them through `nyxBridge.invoke("repo_diff", …)` / `invoke("repo_git_view", …)`
// — the SAME engine the MCP tools serve (PDR-0005: one authority for UI + MCP). No
// review entity is stored: a `RepoDiff` is a transient VUE DÉRIVÉE of a `(workspace,
// repository)` pair (PDR-0015), carrying that repo's OWN identity (`git_dir` + resolved
// `base_sha`/`target_sha`) so the aggregating UI never mixes two repos' Git identities.

/** Which backend produced a diff (gix primary, git CLI fallback). */
export type DiffBackend = "gix" | "git_cli";

/** LIVING (follows the current state) vs PINNED (frozen on a resolved SHA) — PDR-0015. */
export type DiffMode = "living" | "pinned";

/** How a file changed between the two diff sides. */
export type ChangeKind = "added" | "deleted" | "modified";

/** The role of a diff line within its hunk. */
export type LineKind = "context" | "removed" | "added";

/** One side of a file within a diff (its path + blob id, `null` when absent on that side). */
export interface FileSide {
  readonly path: string;
  readonly blob: string | null;
  readonly binary: boolean;
}

/** One line inside a {@link Hunk}, tagged by role. */
export interface DiffLine {
  readonly kind: LineKind;
  readonly content: string;
}

/** A contiguous change region within a file, in unified-diff (1-based) coordinates. */
export interface Hunk {
  readonly old_start: number;
  readonly old_lines: number;
  readonly new_start: number;
  readonly new_lines: number;
  readonly lines: readonly DiffLine[];
}

/** The diff of ONE file: both sides' identity + its hunks. */
export interface FileDiff {
  readonly change: ChangeKind;
  readonly old: FileSide | null;
  readonly new: FileSide | null;
  readonly binary: boolean;
  readonly hunks: readonly Hunk[];
}

/**
 * The full diff of ONE repository for one diff target — the unit the renderer draws as a
 * single review pane. Carries the repo's own identity so several can be aggregated in one
 * UI without mixing Git identities.
 */
export interface RepoDiff {
  readonly repo_path: string;
  readonly git_dir: string | null;
  readonly mode: DiffMode;
  readonly base_sha: string | null;
  readonly target_sha: string | null;
  readonly backend: DiffBackend;
  readonly files: readonly FileDiff[];
}

/** WHAT to diff for a repository — mirrors the engine's `target_from_params` vocabulary. */
export type DiffKind =
  | "working_tree"
  | "staged"
  | "unstaged"
  | "untracked"
  | "commit"
  | "range"
  | "branches";

/** Args for the `repo_diff` db-command. `rev` (commit) / `base`+`target` (range/branches)
 *  are the rev-specs the chosen `kind` needs; `mode` overrides the target's default. */
export interface RepoDiffArgs extends Record<string, unknown> {
  readonly workspaceId: string;
  readonly repositoryId: string;
  readonly kind: DiffKind;
  readonly rev?: string;
  readonly base?: string;
  readonly target?: string;
  readonly mode?: DiffMode;
}

/** The dirty file partition of a repo (`repo_git_view.dirty`). */
export interface DirtyState {
  readonly clean: boolean;
  readonly staged: readonly string[];
  readonly modified: readonly string[];
  readonly untracked: readonly string[];
}

/** The branch view of a repo (`repo_git_view.branch`): current + review base/target. */
export interface BranchView {
  readonly current: string | null;
  readonly detached: boolean;
  readonly base: string | null;
  readonly target: string | null;
}

/** Ahead/behind counts of `target` relative to `base`. */
export interface AheadBehind {
  readonly ahead: number;
  readonly behind: number;
}

/** The compact change summary of a repo view. */
export interface ChangeSummary {
  readonly changed_files: number;
  readonly staged: number;
  readonly modified: number;
  readonly untracked: number;
  readonly ahead: number;
  readonly behind: number;
}

/** The derived Git VIEW of one repository (`repo_git_view`) — branch/dirty/summary. */
export interface RepoGitView {
  readonly repo_path: string;
  readonly is_git: boolean;
  readonly dirty: DirtyState;
  readonly branch: BranchView;
  readonly ahead_behind: AheadBehind | null;
  readonly summary: ChangeSummary;
}

/** Args for `repo_git_view` / `repo_watch` (the review's optional base/target endpoints). */
export interface RepoGitViewArgs extends Record<string, unknown> {
  readonly workspaceId: string;
  readonly repositoryId: string;
  readonly base?: string;
  readonly target?: string;
}

/** Args for `repo_unwatch` — release a `(workspace, repository)` living-refresh watch. */
export interface RepoUnwatchArgs extends Record<string, unknown> {
  readonly workspaceId: string;
  readonly repositoryId: string;
}

// ---------------------------------------------------------------------------
// v2.4 review: the COMMENT model (the `review_comment_*` db-commands)
// ---------------------------------------------------------------------------
//
// These mirror, on the wire, the `nyx_core::db::ReviewComment` serde shape (snake_case) the
// sidecar db-commands return. A review comment is the ONLY persisted review artifact
// (PDR-0012/0015 — review is a vue dérivée): ONE auto-threaded table, keyed by
// `(workspace, repository)`. A comment is ANCHORED (a `file_path` + `side` + `line_start`
// [+ `line_end`] on the diff, with `blob`/`snippet`/`context` so it re-maps across commits)
// or GLOBAL (`file_path` null). Resolution is HUMAN-ONLY: the UI façade resolves; the agent
// (via MCP) only replies. `outdated` flags an anchor the code moved out from under.

/** WHO authored a review comment: a human reviewer or an agent (replies via MCP). */
export type CommentAuthorType = "human" | "agent";

/** A review comment's lifecycle: open until a HUMAN resolves it. */
export type ReviewCommentStatus = "open" | "resolved";

/** Which diff side a comment anchor is on. */
export type AnchorSide = "old" | "new";

/**
 * A `review_comments` row (mirror of `nyx_core::db::ReviewComment`). Auto-threaded by
 * `parent_id` (null = a thread ROOT). Anchored (`file_path` present) or global. `outdated`
 * flags an anchor the code moved out from under; `resolved_by_type` records WHO resolved it
 * (only ever `human`).
 */
export interface ReviewComment {
  readonly id: string;
  readonly workspace_id: string;
  readonly repository_id: string;
  readonly spec_id: string | null;
  readonly parent_id: string | null;
  readonly author_type: CommentAuthorType;
  readonly author_name: string;
  readonly file_path: string | null;
  readonly side: AnchorSide | null;
  readonly line_start: number | null;
  readonly line_end: number | null;
  readonly blob: string | null;
  readonly snippet: string | null;
  readonly context: string | null;
  readonly outdated: boolean;
  readonly body: string;
  readonly status: ReviewCommentStatus;
  readonly resolved_by_type: CommentAuthorType | null;
  readonly created_at: number;
}

/**
 * Args for `review_comment_list` — the auto-threaded feed of a `(workspace, repository)`.
 * `specId` SCOPES the feed to one Spec (Finding 41): the review is opened FROM a Spec, so its
 * feed shows ONLY that Spec's comments — omit it for the unscoped whole-repo read.
 */
export interface ReviewCommentListArgs extends Record<string, unknown> {
  readonly workspaceId: string;
  readonly repositoryId: string;
  readonly specId?: string;
}

/**
 * Args for `review_comment_post` — the HUMAN façade to leave a comment. A global comment
 * (no anchor), an ANCHORED comment (`filePath` + `side` + `lineStart` [+ `lineEnd`] with
 * optional `blob`/`snippet`/`context` for re-mapping), or a REPLY (`parentId`).
 */
export interface ReviewCommentPostArgs extends Record<string, unknown> {
  readonly workspaceId: string;
  readonly repositoryId: string;
  readonly body: string;
  readonly specId?: string;
  readonly parentId?: string;
  readonly filePath?: string;
  readonly side?: AnchorSide;
  readonly lineStart?: number;
  readonly lineEnd?: number;
  readonly blob?: string;
  readonly snippet?: string;
  readonly context?: string;
}

/** Args for `review_comment_resolve` / `review_comment_reopen` — the HUMAN-only transitions. */
export interface ReviewCommentIdArgs extends Record<string, unknown> {
  readonly id: string;
}

// ---------------------------------------------------------------------------
// v2.4 review: the TASK (finding = task) model + the UNREAD counter (Phase 5)
// ---------------------------------------------------------------------------
//
// A task is a review COMMENT PROMOTED for the coder — `finding = task` (PDR-0012). These
// mirror `nyx_core::db::ReviewTask` / `ReviewUnread` (snake_case on the wire). The promotion
// is a "grill": the HUMAN lâche brut (`review_task_promote` → `proposed`), the AGENT
// structures it (MCP `review_task_structure` → `structured`), the HUMAN validates it
// (`review_task_validate` → `validated`). ONLY a `validated` task is ready for the coder.
// The unread counter is a per-repo read watermark: `review_unread` reads the count, `review_mark_read`
// zeroes it; both refresh live on `repos://changed`.

/** A task's OPTIONAL severity (mirror of the `severity` CHECK vocabulary). */
export type TaskSeverity = "info" | "low" | "medium" | "high" | "critical";

/**
 * The promotion "grill" state (mirror of `nyx_core::db` `grill_state`): `proposed` (human lâche
 * brut) → `structured` (agent structured via MCP) → `validated` (human validated — ready for the
 * coder).
 */
export type GrillState = "proposed" | "structured" | "validated";

/**
 * A review FINDING in the grill panel (mirror of `nyx_core::db::ReviewFinding`). `finding = task`:
 * the promoted comment IS the coder task. As of v2.5.11 (PDR-0016) a finding is a `tasks kind=review`
 * row — the CANONICAL model the overlay reads — projected here into this wire shape; the deprecated
 * `review_tasks` table is no longer read at runtime. `from_comment_id` is the trail back to its
 * source comment (1 comment → N findings); `severity` is optional; `grill_state` walks the grill.
 */
export interface ReviewTask {
  readonly id: string;
  readonly workspace_id: string;
  readonly repository_id: string;
  readonly from_comment_id: string;
  readonly title: string;
  readonly details: string | null;
  readonly severity: TaskSeverity | null;
  readonly grill_state: GrillState;
  readonly created_at: number;
  readonly updated_at: number;
  /**
   * The OPTIONAL Spec this task is scoped to (`null` = un-scoped), inherited from its source
   * comment at promotion (Finding 53). Scopes `review_task_list` so a task promoted on one Spec
   * never leaks onto another Spec sharing the same `(workspace, repository)` — the tasks
   * counterpart of the comment scoping (Finding 41).
   */
  readonly spec_id: string | null;
}

/** The unread state of a `(workspace, repository)` review feed (mirror of `nyx_core::db::ReviewUnread`). */
export interface ReviewUnread {
  readonly workspace_id: string;
  readonly repository_id: string;
  /** Comments (roots + replies) created strictly after `last_read_at`. */
  readonly unread: number;
  /** Epoch-ms read watermark (0 = never read). */
  readonly last_read_at: number;
}

/** Args for `review_task_promote` — the HUMAN lâche brut: promote a comment into a coder task. */
export interface ReviewTaskPromoteArgs extends Record<string, unknown> {
  readonly commentId: string;
  /** Optional title (seeded from the comment body when absent). */
  readonly title?: string;
  /** Optional severity. */
  readonly severity?: TaskSeverity;
}

/** Args for `review_task_validate` — the HUMAN validates a structured task. */
export interface ReviewTaskIdArgs extends Record<string, unknown> {
  readonly id: string;
}

/** Args for `review_task_list` / `review_unread` / `review_mark_read` / `review_remap`. */
export interface ReviewRepoArgs extends Record<string, unknown> {
  readonly workspaceId: string;
  readonly repositoryId: string;
}

/**
 * The result of `review_remap` — how many anchored comments were re-written (relocated OR flipped
 * to `outdated`) by the re-mapping pass. `0` means every anchor still pointed at its captured blob
 * (nothing to do). The re-mapped/outdated rows are read back through `review_comment_list`.
 */
export interface ReviewRemapResult {
  readonly written: number;
}

// ---------------------------------------------------------------------------
// Agent-session lazy resume (v2.5.5) — the `workspace_restore_plan` read model
// consumed to drive the LAZY `claude --resume` injection at terminal open.
// ---------------------------------------------------------------------------

/**
 * One entry of `workspace_restore_plan.resume_sessions`: the `decide_resume` verdict for ONE of
 * a workspace's candidate agent sessions (mirrors the Rust `WorkspaceResumeEntry`). When
 * `resume` is true, `resume_command` is the exact `claude --resume <id>` line to inject;
 * otherwise `skip_reason` records WHY (e.g. `option_off`, `no_conversation`). `uncertain` flags a
 * resumed-but-doubtful (`unknown`/stale) session — informational only now (the v2.5.5 restore
 * panel that used to route on it was reverted; `use-lazy-resume` resumes every resumable entry).
 */
export interface WorkspaceResumeEntry {
  readonly terminal_id: string;
  readonly session_id: string;
  readonly agent_kind: string;
  readonly resume_command: string | null;
  readonly resume: boolean;
  readonly uncertain: boolean;
  readonly skip_reason: string | null;
}

/**
 * The `workspace_restore_plan` read model: after a restart, WHAT a workspace offers to resume —
 * REUSING the existing resume policy (no third system). `resume_sessions` is consumed by the
 * `use-lazy-resume` driver (silent resume gated by the project toggle); `restart_on_startup`
 * backs the service-restore read. (The v2.5.5 crash `interrupted` set was reverted in v2.5.8.)
 */
export interface WorkspaceRestorePlan {
  readonly workspace_id: string;
  readonly resume_sessions: readonly WorkspaceResumeEntry[];
  readonly restart_on_startup: readonly unknown[];
}

/** One resume to PARK in the sidecar (the payload of `set_resume_parks`). */
export interface ResumePark {
  readonly terminalId: string;
  readonly sessionId: string;
  readonly command: string;
}

// ---------------------------------------------------------------------------
// v2.5.10 terminal↔Spec overlay — the `terminal_specs` resolution read model
// (task 01KXN0HVF8T0CKB6C575RECTXG). Resolves a terminal to the Spec set of its
// ACTIVE session; the overlay UI (Phase 2) consumes it and re-pulls it live on
// `specs://changed` / `agent-sessions://changed` (no polling).
// ---------------------------------------------------------------------------

/**
 * The ACTIVE session summary returned by `terminal_specs` — the overlay's LIVENESS axis. `state`
 * is `agent_sessions.state` (`active` while the session lives); `agent_kind` names the provider
 * (the overlay is GENERIC — Claude today, others tomorrow). `null` in {@link TerminalSpecs.session}
 * means the terminal has no live session, so there is NO overlay.
 */
export interface TerminalSpecsSession {
  readonly id: string;
  readonly agent_kind: string;
  readonly state: string;
}

/**
 * The `terminal_specs` result: the Spec SET of a terminal's active session + that session's
 * summary. `session === null` ⇒ no session ⇒ no overlay (and `specs` is empty). Each entry of
 * `specs` is a Spec row shaped like the Specs board's `Spec` (id / title / status / priority /
 * phases / …); the overlay maps them to its own view type — kept as `unknown` here so the bridge
 * contract does not depend on the Specs UI layer.
 */
export interface TerminalSpecs {
  readonly session: TerminalSpecsSession | null;
  readonly specs: readonly unknown[];
}

// ---------------------------------------------------------------------------
// Settings → Integrations: the provider registry (`integration_list` /
// `integration_install` / `integration_remove`, served by `nyx_core::integrations`).
// ---------------------------------------------------------------------------

/**
 * One provider row of the Settings → Integrations list, as returned by `integration_list` (and by
 * `integration_install` / `integration_remove`, which each answer the single row they changed).
 *
 * Mirror of `nyx_core::integrations::IntegrationStatus` — `#[serde(rename_all = "camelCase")]` over
 * four single-word fields, so the wire keys are the field names as written here — served straight,
 * with no envelope around it.
 */
export interface IntegrationStatus {
  /** Provider key (`"claude_code"`, `"codex"`, `"opencode"`, `"custom"`) — the `provider` argument
   *  `integration_install` / `integration_remove` take back. */
  readonly provider: string;
  /** The display name, in the provider's own vocabulary. */
  readonly label: string;
  /**
   * Whether the nyx integration — the ONE bundled plugin (MCP server + session-capture hooks) — is
   * installed for this provider. A single flag, no split MCP/plugin state to desync. It is derived
   * by the backend from the provider's REAL config rather than from a nyx-side cache, so a plugin
   * removed outside nyx reads as uninstalled on the next `integration_list`.
   */
  readonly installed: boolean;
  /** `true` when the provider is fully functional; `false` = coming soon, no actions available. */
  readonly available: boolean;
  /**
   * A measured problem with an otherwise-INSTALLED integration, in one actionable sentence — absent
   * (`null` / `undefined`) when there is nothing to say, which is the common case.
   *
   * The one it carries today is the MCP port divergence: the agent plugin is global to the machine
   * while nyx installs and worktrees are not, so the port an agent session dials can be a port THIS
   * nyx is not listening on. Unsaid, that reaches the agent as a bare `ConnectionRefused` naming
   * nothing — which is why it is surfaced on the card rather than left to be discovered.
   */
  readonly diagnostic?: string | null;
}

// ---------------------------------------------------------------------------
// Settings → Terminal: the DEFAULT-SHELL preference (`shell_list` /
// `app_settings_get` / `set_default_shell`, served by `nyx_core::shells`).
// ---------------------------------------------------------------------------

/**
 * WHAT a terminal spawn runs: a program plus the fixed arguments that make it that shell.
 *
 * `args` is empty for every shell that is a plain executable (PowerShell, cmd, bash, zsh…) and
 * non-empty only where the shell has no executable of its own — a WSL distribution, which is
 * `wsl.exe -d <distro>`. Both halves travel together so the renderer never has to compose (or
 * parse) a command line: it hands back what {@link DetectedShell} gave it.
 */
export interface ShellChoice {
  readonly path: string;
  readonly args: readonly string[];
}

/**
 * One shell the HOST really has, as returned by `shell_list` — the picker's options.
 *
 * The backend verifies every entry against the disk before returning it, so this list never
 * offers a shell that would fail to spawn. `id` is the stable identity the picker keys its
 * options on (`windows-powershell`, `pwsh`, `cmd`, `git-bash`, `wsl:<distro>`, or the absolute
 * path of a `/etc/shells` entry); `label` is the name in the platform's own vocabulary.
 */
export interface DetectedShell extends ShellChoice {
  readonly id: string;
  readonly label: string;
}

/**
 * The app-level preferences of this install (`app_settings_get`).
 *
 * `defaultShell: null` is the "Auto" state — the user has expressed NOTHING, which is a different
 * fact from any shell that could be named here: it is what keeps the historical spawn resolution
 * (`$SHELL`, then the per-OS default) in force. A stored preference whose binary has since been
 * uninstalled is still reported here (so the picker can say so) but is NOT what a spawn uses — the
 * backend falls back to the same historical chain rather than failing to open a terminal.
 */
export interface AppSettings {
  readonly defaultShell: ShellChoice | null;
}

// ---------------------------------------------------------------------------
// GLOBAL SEARCH — the `Mod+K` palette's wire model. A faithful mirror of the
// `nyx_core::search::SearchHit` serde shape (snake_case), returned by the `global_search`
// db_command.
// ---------------------------------------------------------------------------

/**
 * The seven entity types the palette spans (mirror of `nyx_core::search::SearchKind`).
 *
 * The order of this union is the DISPLAY order of the result groups — Specs → Tasks → Ideas →
 * Workspaces → Projects → Managed commands → Agent sessions — and the backend returns its groups in
 * exactly that sequence. Fixed rather than score-ordered so the palette is muscle memory: a type is
 * always in the same place, whatever the query.
 */
export type GlobalSearchKind =
  | "spec"
  | "task"
  | "idea"
  | "workspace"
  | "project"
  | "command"
  | "session";

/**
 * ONE result of the palette: a TYPED TARGET plus what to draw for it.
 *
 * There is deliberately no url, href or route fragment here. The backend answers an IDENTITY —
 * "this is a Spec, in this project" — and the renderer owns the mapping to a route, because the
 * routes are the renderer's and change with it. `project_id` is what the renderer ACTIVATES before
 * navigating (every surface of the app reads through the entered project), and that activation
 * PERSISTS — which is why the palette pre-selects nothing while there is no query.
 *
 * The container fields are per-kind and null elsewhere: `workspace_id` for what lives in a
 * workspace, `spec_id` for a TASK (v1 opens the task's Spec, there is no task route — null for a
 * task attached to no Spec), `terminal_id` for an agent SESSION (a session is reached through its
 * terminal or not at all).
 *
 * Each container id travels with its NAME (`project_name`, `workspace_name`), joined backend-side.
 * The palette used to rebuild the project name from the rail's project tree — which only holds what
 * the rail loaded — and had no workspace name at all; and a name the backend does not read is a
 * name it cannot MATCH, which it now does (at a lower weight than the entity's own).
 *
 * A `command` hit's `id` is a `command_instances.id`, never a `managed_commands.id`: the instance is
 * what runs, what `WorkspaceRuntimeService.id` is, and what `?command=` deep-links. One template
 * materialized in two workspaces is therefore TWO hits, told apart by `workspace_name`.
 */
export interface GlobalSearchHit {
  readonly kind: GlobalSearchKind;
  /**
   * The entity to open. For a Spec matched through its PROSE this is the Spec, not the revision;
   * for a managed command it is the INSTANCE (`command_instances.id`), not the template.
   */
  readonly id: string;
  readonly project_id: string;
  /** The owning project's NAME — always present (every source reaches a project through a NOT NULL FK). */
  readonly project_name: string;
  readonly workspace_id: string | null;
  /** The workspace's NAME, `null` exactly where `workspace_id` is — the two are one pair. */
  readonly workspace_name: string | null;
  readonly spec_id: string | null;
  readonly terminal_id: string | null;
  /** The entity's name — what the row shows. */
  readonly label: string;
  /** A short secondary line, when the source has one (a command line, an agent kind). */
  readonly detail: string | null;
  /** The match window, when the source can produce one (the Spec prose, an idea's body). */
  readonly extract: string | null;
  /**
   * The entity's HUMAN KEY when it has one — a Spec's `KEY-number` (e.g. `NYX-42`), `null` for
   * every other kind. DRAWN, NEVER MATCHED: it is a field of its own rather than a second `detail`
   * because `detail` is part of the backend's match surface, and a key that ranks would move an
   * order settled without it. Searching a Spec BY its key is a separate capability.
   */
  readonly human_key: string | null;
  /** The blended relevance in `[0,1]`, higher = better. `0` on a suggestion, which is never scored. */
  readonly score: number;
  /** The recency (`updated_at`, or `last_seen_at` for a session) — the ordering of the suggestions. */
  readonly updated_at: number;
}

/**
 * The ANSWER of one `global_search` — the hits, plus the one fact about the install they cannot
 * carry (mirror of `nyx_core::search::SearchAnswer`).
 *
 * `hits` is CAPPED (8 per type, 40 overall), so "this install holds no Spec, no task and no idea"
 * is unanswerable from it: a Spec absent from the eight most recently touched entities looks
 * exactly like a Spec that does not exist — and that head is routinely saturated by sessions and
 * commands, whose recency moves at every agent start while the Specs sit still. The backend is the
 * only place that knows, so it says so in a field of its own.
 */
export interface GlobalSearchAnswer {
  readonly hits: readonly GlobalSearchHit[];
  /**
   * `true` when the install carries NO searchable Spec, NO task and NO idea — the "nothing to
   * search yet" of a fresh install, which the project and root workspace one just created do not
   * lift. The palette's empty-install state is keyed on THIS, never on the composition of `hits`.
   *
   * The below-`MIN_QUERY_LEN` regime issues no read and answers `false`: the conservative value,
   * which can only hide the empty state and never invent one.
   */
  readonly knowledge_empty: boolean;
}

/** The args of the `global_search` db_command. */
export interface GlobalSearchArgs extends Record<string, unknown> {
  /** The raw user text. Empty = the SUGGESTIONS; one character = an empty answer. */
  readonly query: string;
  /** The OVERALL cap, clamped by the core (it can narrow the answer, never widen it). */
  readonly limit?: number;
}

// ---------------------------------------------------------------------------
// v2.8 orchestration ENGINE — the behavior-tree graph read models (Phase 6). These mirror, on the
// wire, the `nyx_core::db` `Workflow` / `Node` / `WorkflowRun` serde shapes (snake_case) the sidecar
// engine read-commands return. The graph is DATA (PRD "Orchestration = donnée"); the renderer
// reconstructs the pure tree from the flat `EngineNode` rows (`parent_id` + `order_index`).
// ---------------------------------------------------------------------------

/** A workflow's `scope` — which built-in flow the tree IS (mirror of the DB CHECK vocabulary). */
export type WorkflowScope = "ndev" | "nspec" | "ndoc" | "custom";

/** A workflow_run's lifecycle status (mirror of the DB CHECK vocabulary). */
export type WorkflowRunStatus = "running" | "blocked" | "done" | "failed";

/**
 * A `workflows` row (mirror of `nyx_core::db::Workflow`). `project_id` null = a BUILT-IN nyx default
 * (the seeded ndev/nspec/ndoc); `root_node_id` null = an empty draft; `forked_from` null = not a fork.
 */
export interface EngineWorkflow {
  readonly id: string;
  readonly project_id: string | null;
  readonly name: string;
  readonly scope: WorkflowScope;
  readonly root_node_id: string | null;
  readonly forked_from: string | null;
  readonly created_at: number;
  readonly updated_at: number;
}

/**
 * A `nodes` row (mirror of `nyx_core::db::Node`). The tree is PURE: `parent_id` null = the workflow's
 * root, and a composite's ORDERED children are the rows with `parent_id = it` sorted by `order_index`.
 * `type_` is the behavior-tree vocabulary (the field is `type_` on the wire — `type` is a Rust
 * keyword); `config` is a typed JSON-object STRING decoded per node kind.
 */
export interface EngineNode {
  readonly id: string;
  readonly workflow_id: string;
  readonly parent_id: string | null;
  readonly order_index: number;
  readonly type_: string;
  readonly label: string;
  readonly config: string;
  readonly created_at: number;
  readonly updated_at: number;
}

/** `engine_workflow_graph` result — one workflow + its full node dump (for tree reconstruction). */
export interface EngineWorkflowGraph {
  readonly workflow: EngineWorkflow;
  readonly nodes: readonly EngineNode[];
}

/**
 * A `workflow_runs` row (mirror of `nyx_core::db::WorkflowRun`). `current_node_id` is the live cursor
 * (the node the run is parked on — the runtime view HIGHLIGHTS it); `blackboard` is the run's JSON
 * scratchpad STRING (last gate verdict, phase index, findings count, …) the runtime panel renders.
 */
export interface EngineWorkflowRun {
  readonly id: string;
  readonly workflow_id: string;
  readonly spec_id: string | null;
  /** The run that OPENED this one through a `sub_workflow` node, or `null` for a top-level run. A Spec
   *  therefore has one ROOT run per flow plus the child runs its graph delegated to (v2.8 lot 2). */
  readonly parent_run_id: string | null;
  readonly current_node_id: string | null;
  readonly blackboard: string;
  readonly status: WorkflowRunStatus;
  readonly started_at: number;
  readonly updated_at: number;
  /** The RECEIPT of the consent question the engine put to the human for this run (H6), as a JSON
   *  string, or `null` when there is none open. Written and cleared by the ENGINE alone — it records
   *  what was asked and to which session, so the tick that receives the answer can tell a human's
   *  from a session's. The UI does not answer it (the human answers in the conversation), it only
   *  mirrors the column. */
  readonly pending_consent: string | null;
}

/** `engine_effector_context` result — the rendered effector manual (null when the Spec is unknown). */
export interface EngineEffectorContext {
  readonly context: string | null;
}

// ---------------------------------------------------------------------------
// The bridge surface, grouped by capability
// ---------------------------------------------------------------------------

/**
 * The single typed frontier. A shell adapter implements ALL of it; a component
 * depends only on the slice it needs. Method names mirror the underlying backend
 * command where it clarifies intent. Every request method may reject with a
 * {@link BridgeError}; callers that fail-open (window controls, scrollback
 * persistence) simply `.catch()` it.
 */
export interface NyxBridge {
  /** Low-level escape hatch: invoke a backend command by name. Prefer the typed
   *  methods below; this exists so the inventory's long tail of one-off commands
   *  (project/workspace/command CRUD) is covered without 40 bespoke signatures.
   *  Every typed method is implementable in terms of this. */
  invoke<R>(
    command: BackendCommand,
    args?: Record<string, unknown>,
    opts?: RequestOptions,
  ): Promise<R>;

  /**
   * Low-level subscription escape hatch: deliver the RAW payload of `event` to
   * `listener`, returning an idempotent {@link Unsubscribe}. Mirrors a bare
   * `listen(event, e => listener(e.payload))`. The typed `subscribe*` methods below
   * are the curated surface (they normalize binary + field names); this exists so a
   * call-site that needs the exact backend payload shape (e.g. a per-instance filter
   * on the wire field name) migrates faithfully without re-deriving its type. */
  subscribe<T>(event: BackendEvent, listener: Listener<T>): Promise<Unsubscribe>;

  // --- Interactive PTY (binary, ordered) ---------------------------------
  ptySpawn(opts: PtySpawnOptions): Promise<number>;
  ptyWrite(id: number, data: Uint8Array): Promise<void>;
  ptyResize(id: number, cols: number, rows: number): Promise<void>;
  ptyClose(id: number): Promise<void>;
  /** Subscribe to ordered output for ALL terminals; filter by `id` in the callback. */
  subscribePtyOutput(listener: Listener<PtyOutput>): Promise<Unsubscribe>;
  subscribePtyExit(listener: Listener<PtyExit>): Promise<Unsubscribe>;
  /**
   * FLOW-CONTROL ACKNOWLEDGEMENT. The consumer calls this for `bytes` of a PTY's
   * output once the RENDERER has actually CONSUMED them — i.e. from `xterm.write`'s
   * completion callback, NOT when the chunk merely arrived. The Electron adapter
   * credits the per-terminal backlog so the host resumes the paused Rust reader
   * below the low-water mark (the lossless backpressure loop — PRD / annexe §E). On
   * shells with no flow control (Tauri/the fake) it is a no-op, so the single PTY
   * consumer (`use-pty`) calls it unconditionally and stays shell-agnostic.
   */
  ackPtyOutput(id: number, bytes: number): void;

  // --- Managed commands (services) ---------------------------------------
  subscribeCommandOutput(listener: Listener<CommandOutput>): Promise<Unsubscribe>;
  subscribeCommandState(listener: Listener<CommandState>): Promise<Unsubscribe>;
  subscribeCommandAck(listener: Listener<CommandAck>): Promise<Unsubscribe>;

  // --- Terminal exec/busy state ------------------------------------------
  subscribeTerminalBusyState(listener: Listener<TerminalBusyState>): Promise<Unsubscribe>;
  subscribeTerminalExecState(listener: Listener<TerminalExecState>): Promise<Unsubscribe>;

  // --- Window (frameless chrome) -----------------------------------------
  readonly window: WindowControls;

  // --- Native folder picker ----------------------------------------------
  /** Open the OS folder picker; resolve the chosen absolute path, or `null` if
   *  cancelled. Single selection, directories only. */
  pickDirectory(title?: string): Promise<string | null>;

  // --- Paths --------------------------------------------------------------
  readonly paths: AppPathsBridge;
}

/** The frameless-window control surface (min / toggle-maximize / close + drag). */
export interface WindowControls {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  /** Close the window. The CLOSE-INTERCEPTION policy (agent-session warnings) is
   *  centralized in the shell's main-process close guard, NOT a caller concern: under
   *  the Electron shell `close()` re-enters that guard, which withholds the OS close and
   *  relays it to the agent close-warning flow via {@link onCloseRequested} /
   *  {@link confirmClose} (which run `agent_close_warnings`, see {@link NyxBridge.invoke}).
   *  So `close()` is NOT an unconditional escape hatch — it proceeds only once the guard
   *  clears (no live session) or the user confirms via {@link confirmClose}. */
  close(): Promise<void>;
  /* NB: there is no `dragRegionProps()` seam any more. It existed to hand the chrome the
   * shell-specific way to mark its drag strip — `{ "data-tauri-drag-region": true }` under
   * Tauri/WRY. Tauri is gone (PDR-0006) and under Electron the drag region is PURE CSS
   * (`-webkit-app-region: drag`, the `drag-region` utility in globals.css): Chromium reads
   * it off the computed style, no bridge call involved. The seam never had a caller — the
   * chrome hard-coded the attribute — so it was a Tauri leftover advertising a capability
   * nothing used. */
  /** Subscribe to the OS "window close requested" signal (the user clicked the OS
   *  close button / Alt-F4 / quit). The handler runs BEFORE the window is destroyed —
   *  the shell WITHHOLDS the real close until the caller proceeds via {@link
   *  confirmClose}. Used to flush pending scrollback AND to run the agent close-warning
   *  guard (`agent_close_warnings`). Returns an idempotent {@link Unsubscribe}. Resolves
   *  with a no-op unsubscribe when the shell has no such hook (e.g. under tests). */
  onCloseRequested(handler: () => void): Promise<Unsubscribe>;
  /** Proceed with the REAL close after an {@link onCloseRequested} guard cleared it (no
   *  live agent session) or the user confirmed. Bypasses the shell's close guard so it
   *  does not re-prompt. A no-op (resolved) on shells with no such hook (tests). */
  confirmClose(): Promise<void>;
}

/** Filesystem paths the front asks the shell to resolve. */
export interface AppPathsBridge {
  /** The user's home directory, or `null` if it can't be resolved (caller falls
   *  back to `"."`). Mirrors `@tauri-apps/api/path`'s `homeDir`. */
  homeDir(): Promise<string | null>;
}

/**
 * The closed set of backend command names the front invokes today — every
 * `invoke("...")` call-site under `apps/frontend/src`. A string-literal union so a
 * typo or a dropped command is a compile error, and so a shell adapter can map
 * each name to its transport exhaustively.
 */
export type BackendCommand =
  // pty
  | "pty_spawn"
  | "pty_write"
  | "pty_resize"
  | "pty_close"
  | "register_terminal_pty"
  | "terminal_info"
  // The ANSI byte stream that repaints a terminal's screen (main + alt-screen) into a
  // freshly-created xterm: the shell a mounting pane TAKES OVER has been running without it —
  // opened by an agent over MCP, or still alive from before a reload — so without this the pane
  // opens blank on a session that is mid-work. `{ recordId }` → `{ bytes: number[] }`, empty
  // when there is nothing to restore. Non-destructive and self-resetting (idempotent).
  | "terminal_screen_rehydration"
  // terminals
  | "create_terminal"
  | "list_terminals"
  | "attach_terminal"
  | "auto_attach_terminal"
  | "close_terminal"
  | "set_active"
  | "rename"
  | "reorder"
  | "terminal_exec_mark_read"
  | "persist_scrollback"
  // FEEDBACK #32: persist a terminal's live cwd into its record so a relaunch
  // re-spawns at the LAST directory, not the stale spawn-time cwd.
  | "set_terminal_cwd"
  // Système scope (PRD « Scope Système »): the cross-project terminals and commands that belong
  // to no code project. Both commands are HUMAN-ONLY — there is no MCP twin, and the agent-facing
  // terminal tools cannot see what the first one creates.
  //
  // `create_system_terminal` is ATOMIC: one transaction writes the record with the absolute home
  // as its cwd, the Système workspace as its `workspaceId` and a MANUAL binding, and only then is
  // the shell opened. So a `cd` never re-homes it, and it never passes through the unattached
  // (loose) state. `{ label? }` → the created `Terminal` row, exactly like `create_terminal`.
  | "create_system_terminal"
  // `system_scope_detail` is the ONE read of the singleton: `list_projects` / `list_workspaces`
  // stay user-only (a technical container must not land in the re-orderable sidebar array), so
  // the Système page reads its project, its home workspace and the `home` path here instead.
  // `{}` → `{ project, workspace, home }`, or `null` before the scope is bootstrapped.
  | "system_scope_detail"
  // The HUMAN's half of the SYSTÈME APPROVAL protocol — the three commands behind the decision
  // surface. HUMAN-ONLY, and this is the entry where that matters most in the whole contract:
  // there is deliberately no MCP twin of `system_approval_grant`, because an agent that could
  // approve its own request would turn the protocol into a formality. The agent's counterpart is
  // the single MCP read `system_approval_status`, which answers only the request its token opens.
  //
  // `system_approval_list` ({}) → the PENDING requests, oldest first, deduplicated by fingerprint
  // and bounded by the backend. Each carries what a decision needs — the operation, a one-line
  // summary, the fingerprint, the expiry — and NOTHING of the opaque token or its hash.
  | "system_approval_list"
  // `system_approval_grant` / `system_approval_deny` ({ requestId }) → the decided request. Both
  // are CONDITIONAL transitions out of `pending`: a request already decided, or one whose
  // five-minute window has closed, is refused rather than silently re-decided. Each broadcasts
  // `system-approvals://changed`.
  | "system_approval_grant"
  | "system_approval_deny"
  // projects / workspaces
  | "list_projects"
  | "create_project"
  | "update_project"
  | "delete_project"
  | "projects_reorder"
  | "set_project_collapsed"
  | "set_project_resume_agent_sessions"
  | "list_workspaces"
  | "create_workspace"
  | "rename_workspace"
  | "workspace_delete"
  | "set_workspace_collapsed"
  // v2.2 runtime read model: a workspace's LIVE runtime aggregate (terminals + their
  // agent sessions, cross-terminal agent sessions, and managed-command services with
  // run state). Read-only; re-pulled (no polling) on any of the coarse invalidation
  // topics `commands://changed`, `agent-sessions://changed`, `terminals://changed`,
  // `workspaces://changed`, plus the fine `terminal://exec-state` event — see
  // `RUNTIME_CHANGED_EVENTS` in `components/terminals/use-runtime-live.ts` for the
  // source of truth (consumed by the project→workspaces Terminals page). `{ workspaceId }`.
  | "workspace_runtime_snapshot"
  // v2.2 FREE-WORK entry point (task 01KWHPV33H0FX8NA4C94T5W46J): the runtime aggregate of a
  // project's PRIMARY workspace — the default "travail libre" cockpit when a project is opened
  // WITHOUT a Spec. Same shape + same shared read model as `workspace_runtime_snapshot`, keyed
  // on the project (resolves `primary_workspace_id`, root fallback). `{ projectId }`.
  | "workspace_runtime_primary"
  // The HOME read model — the synthesis page in either scope: pass `{ projectId }` for a
  // project's home, NOTHING for the cross-project one. ONE read returns the four families
  // (agent sessions with their state, running/failed services, recently-touched Specs, bare
  // terminals), already sorted `waiting > error > ready > working > idle` with the oldest first
  // in each rank — a single flat flow, never grouped by project. Deliberately NOT a
  // `workspace_runtime_snapshot` per workspace: that costs one IPC each, which in the global
  // scope is dozens of round trips for one page. Read-only; re-pulled on the same coarse
  // invalidation topics the runtime aggregate uses.
  | "home_snapshot"
  // v2.2 workspace CONTEXT-RESTORE plan (task 01KWHPV33M4H5YSMGYNW9TG6J0): after a restart,
  // WHAT a workspace offers to resume — computed by REUSING the existing resume policies (no
  // 3rd system): per-session `decide_resume` verdicts (`resume_sessions`) + the
  // `restart_on_startup` services. `use-lazy-resume` drains the resumable sessions SILENTLY
  // (gated by the project toggle). Read-only, idempotent. `{ workspaceId }`. (The v2.5.5 crash
  // `interrupted` set / restore card was reverted in v2.5.8.)
  | "workspace_restore_plan"
  // v2.2 Git CHANGES read model: per non-optional repo of a workspace, its branch/dirty/
  // changed-files SUMMARY, re-read from Git (never authoritative in the DB — PDR-0013).
  // Read-only, on demand (no real-time refresh — Q-015, v2.3). `{ workspaceId }`.
  | "workspace_git_summary"
  // v2.2 workspace TIMELINE read model (task 01KWHPV33SAV9NV1STSY403CF0): a BOUNDED, oldest-
  // first fil of a workspace's most recent `domain_events` (v2.1) — the business "changements
  // notables" (workspace/repository/project lifecycle), NEVER a terminal byte. Read-only; re-
  // pulled (no polling) on `workspaces://changed` (the topic identity mutations invalidate).
  // NOT the full contextual activity timeline (v2.5). `{ workspaceId, limit? }`.
  | "workspace_timeline"
  // v2.4 Spec domain READ models (Phase 4 — the native Specs UI). Pure reads over the
  // `nyx_core::db` Spec/revision/task/workspace-context model (phases 1-3), served by the
  // sidecar's `db_command` dispatch (the SAME authority the Phase-5 MCP tools will reuse).
  // `spec_list` ({ projectId }) is the board read model: every LIVE Spec of a project +
  // its per-Spec task tally (done/total) + the project's Linear key, for the board cards.
  // `spec_detail` ({ specId }) is the PRD-detail read model: the Spec, its active revision
  // (the six structured sections), its tasks + dependency edges, and the DERIVED workspace
  // context (workspace → project → repos), consumed by the detail's tabs.
  | "spec_list"
  | "spec_detail"
  // `spec_activity` ({ specId, limit? }) is the PRD-detail ACTIVITY read model: a BOUNDED,
  // oldest-first fil of the Spec's most recent `spec`/`task` `domain_events` (its own lifecycle
  // — creation, section edits, transitions — plus its tasks' add/transition/dependency events),
  // NEVER a terminal byte. Mirrors `workspace_timeline`; consumed by the detail's Activity tab.
  | "spec_activity"
  // managed commands
  | "command_list"
  | "command_create"
  | "command_update"
  | "command_delete"
  | "command_start"
  | "command_stop"
  | "command_relaunch"
  | "command_acknowledge"
  | "command_output"
  // `command_clear_output` ({ instanceId }) — EMPTY a managed command's captured output: the live
  // in-memory tail AND the persisted scrollback, with a `command://output-cleared` push so every
  // mounted `<CommandOutputPanel>` for that instance wipes its xterm. The HUMAN twin of the MCP
  // `clear_command_output` tool — same runner call, same entry on the Système guard matrix.
  //
  // NO RENDERER CALLER TODAY. Phase 3 put an eraser on the Système command rows to give the verb a
  // hand at the keyboard; human review `01M1EC7Y2625Q07C5386GKHZFS` (finding U2) removed it. Seen
  // in the real app, the user judged a verb offered on ONE command surface and no other incoherent,
  // and, given the choice between generalising it and dropping it, chose to drop it — so no
  // `nyxBridge.invoke` names this command any more (`system-scope-page.tsx`, `workspace-runtime-
  // tree.tsx`). The IPC is KEPT on purpose: it is the human half of a capability whose sidecar half
  // is live, tested and gated, and a surface that generalises the verb re-uses it unchanged.
  //
  // A clear wipes BYTES, never the run result: no state transition, and a running instance keeps
  // streaming onto the freshly-emptied buffer.
  | "command_clear_output"
  | "command_instance_list"
  | "command_import_scripts"
  | "command_import_create"
  | "command_source_refresh"
  | "command_resync_source"
  | "command_unlink_source"
  // The ORDER OF THE COMMAND LIST — `{ ids }`, the project's command template ids in their new
  // order, exactly like the terminals' `reorder` and `projects_reorder` (review
  // 01M11WE6H9E5D5FCS6RT0ZXAXN). Nothing could write it before: the list simply kept creation
  // order, which is why a drag that takes a command OUT of a group could not leave it where it
  // was dropped — it fell back to its "natural" position. Broadcasts `commands://changed`; the
  // agent's half is the MCP `reorder_commands` tool, over the same `db::reorder_templates`.
  | "command_reorder"
  // managed-command GROUPS (the multi-pane grouping of commands). Unlike a TERMINAL group — a
  // per-machine view preference that lives in the renderer's `localStorage` — a command group is
  // DB state (`managed_command_groups`): the review that asked for it also requires an AGENT to
  // drive the very same operations over MCP (`manage_command_group`), and the sidecar cannot see
  // `localStorage`. Both surfaces therefore call the SAME `db::` transitions, and every mutation
  // broadcasts `commands://changed` so a hand edit and an agent edit repaint identically.
  //   `command_group_list`     `{ projectId }`               → the project's groups, members in
  //                                                            DISPOSITION order.
  //   `command_group_create`   `{ projectId, commandIds }`   → a group from ≥2 commands.
  //   `command_group_add`      `{ groupId, commandId }`      → append a member.
  //   `command_group_remove`   `{ commandId }`               → take a command out (its group
  //                                                            DISSOLVES if one member is left).
  //   `command_group_dissolve` `{ groupId }`                 → every member becomes solo.
  //   `command_group_labels`   `{ groupId, name?, color? }`  → an ABSENT key leaves the label
  //                                                            alone; an explicit `null` clears it.
  //   `command_group_layout`   `{ groupId, commandIds }`     → the ordered member list — WHICH
  //                                                            member goes where.
  //   `command_group_shape`    `{ groupId, shape }`          → the TILING SHAPE the members are
  //                                                            poured into — WHAT the grid looks
  //                                                            like. `auto` | `row` | `column` |
  //                                                            `grid` | `main-left` | `main-top`,
  //                                                            `auto` being the shape derived from
  //                                                            the member count (the behaviour of
  //                                                            before, and the default). It is a
  //                                                            LABEL, not a second layout record:
  //                                                            the pane tree is still DERIVED, now
  //                                                            from (ordered members + shape).
  //                                                            Separate from `command_group_layout`
  //                                                            because a re-order and a re-shape
  //                                                            are two edits, and one route could
  //                                                            not tell them apart.
  // Each of the seven above returns the affected group (or `null` where there is none left) and
  // broadcasts `commands://changed`.
  | "command_group_list"
  | "command_group_create"
  | "command_group_add"
  | "command_group_remove"
  | "command_group_dissolve"
  | "command_group_labels"
  | "command_group_layout"
  | "command_group_shape"
  // The GROUP-WIDE LIFECYCLE (review 01M11WE6H9E5D5FCS6RT0ZXAXN): play / stop / restart on every
  // member of a command group at once. `{ groupId, workspaceId }` — and the PAIR is the point: a
  // group holds command TEMPLATES (project-scoped) while running them drives `command_instances`
  // (workspace-scoped), which is why these controls belong to the SIDEBAR and not to the
  // project-settings modal, which knows no workspace.
  //   `command_group_start`    → starts ONLY the members that are not already running; a member
  //                              already up is left strictly alone, neither restarted nor
  //                              interrupted.
  //   `command_group_stop`     → stops every member (idempotent on the idle ones).
  //   `command_group_relaunch` → re-spawns the active members AND starts the inactive ones, so the
  //                              group ends fully active. The plan is computed from ONE snapshot
  //                              taken before anything runs — simultaneous, not one-after-another.
  // Each returns `{ group_id, workspace_id, action, members[], running, total }`: one entry per
  // member in disposition order carrying the SAME `{ instance_id, state, running, finished,
  // exit_code, unread }` a single-command call returns, plus `command_id`, `name`, `was_running`,
  // `acted` (false on a member a play deliberately skipped) and a per-member `error` — a member
  // that fails is reported on its own row and never aborts the others. `running`/`total` is the
  // `n/m` aggregate the group's head reads.
  //
  // The RULE lives in `nyx_core::command::run_command_group` and is written ONCE: the MCP tools
  // (`start_command`/`stop_command`/`relaunch_command` given `group_id` + `workspace_id`) call the
  // very same function, so a click and an agent call cannot drift. These routes broadcast nothing
  // of their own — the runner's sink already pushes `commands://changed` per member transition.
  | "command_group_start"
  | "command_group_stop"
  | "command_group_relaunch"
  // agents
  | "agent_active_sessions"
  | "agent_close_warnings"
  // v2.5.10 terminal↔Spec OVERLAY resolution (task 01KXN0HVF8T0CKB6C575RECTXG): the Specs a
  // terminal's ACTIVE session is working + a summary of that session (for the overlay's
  // liveness axis). `{ terminalId }` → {@link TerminalSpecs}. NO active session → `{ session:
  // null, specs: [] }` (no session ⇒ no overlay). Pure read over the SHARED `nyx_core::db`
  // resolver (`specs_for_terminal`). The live refresh rides the EXISTING unsolicited channels
  // with NO polling: the auto-map/un-map + any spec/task mutation push `specs://changed`, and
  // the session lifecycle pushes `agent-sessions://changed` — an open overlay re-pulls this on
  // either topic (the SAME re-pull pattern the runtime read model uses).
  | "terminal_specs"
  // The RUNTIME agent-activity map (the live dot): which terminals are working/waiting
  // or carry a "response ready" notification. NEVER persisted — an in-memory read off the
  // same store the Claude per-turn hooks write. Re-pulled on `agent-sessions://changed`.
  | "agent_activity_snapshot"
  // Clear the focus-aware "response ready" notification for a viewed terminal (the
  // activity analogue of `terminal_exec_mark_read`). `{ terminalId }`. Idempotent.
  | "agent_mark_ready_read"
  // v2.5.5 LAZY agent-session resume: push the resumable sessions the renderer read from
  // `workspace_restore_plan` (each `{ terminalId, sessionId, command }`) so the sidecar parks
  // each `claude --resume <id>` and injects it ONCE at that terminal's PTY spawn (or immediately
  // if already live). `{ parks: ResumePark[] }`. Idempotent (a one-shot guard per record id);
  // an injection whose write fails marks the session `resume_failed` (no retry loop).
  | "set_resume_parks"
  // v2.5.5 IDLE-SESSION UNLOAD: reclaim the resources of dormant agent sessions. The renderer
  // sweeps on a timer, passing the currently-visited (focused) terminal record ids and an
  // inactivity `thresholdSecs` (default 15 min). The sidecar closes the PTY of every agent-hosting
  // terminal that is Idle (NEVER Working/Waiting — an ACTIVE session is never unloaded), unvisited,
  // and idle beyond the threshold; the DB session stays `active` so a revisit RESUMES it via the
  // same lazy park+inject (phase 2). `{ thresholdSecs?, visited: string[] }` → the unloaded record
  // ids. Best-effort: a failed sweep just means no unload this tick.
  | "unload_idle_sessions"
  // v2.4 review — the Git DIFF read model (task 01KX0F6GY96096AHWD8RZ2BMM0, PDR-0007/0015).
  // `repo_diff` is the line-level diff of ONE `(workspace, repository)` for a review surface
  // (working_tree/staged/unstaged/untracked/commit/range/branches), LIVING or PINNED — the
  // EXACT same `nyx_core::git_diff` engine the MCP `repo_diff` tool serves (PDR-0005). A pure
  // read: no mutation, nothing persisted (PDR-0013). Returns a `RepoDiff`.
  | "repo_diff"
  // `repo_git_view` — the derived Git view (branch/dirty/base/target/summary) of one
  // `(workspace, repository)`, re-read from Git on demand (Q-015). Returns a `RepoGitView`.
  | "repo_git_view"
  // `repo_watch` — begin watching a `(workspace, repository)` so a work-tree/ref change pushes
  // `repos://changed` (v2.1 pipe, debounced, no polling). A LIVING review calls it as it opens.
  | "repo_watch"
  // `repo_unwatch` — release a `repo_watch` as the review surface closes. Idempotent.
  | "repo_unwatch"
  // v2.4 review COMMENTS (task 01KX0F6HEBPK34CV49EREHK3Y9, PDR-0012/0015). The ONLY persisted
  // review artifact: a single auto-threaded `review_comments` table (review = vue dérivée).
  // `review_comment_list` reads the auto-threaded feed of a `(workspace, repository)`.
  | "review_comment_list"
  // `review_comment_post` leaves a HUMAN comment/reply (global/anchored/threaded). The MCP
  // agent façade posts the SAME model as the agent. A mutation pushes `repos://changed`.
  | "review_comment_post"
  // `review_comment_resolve` / `review_comment_reopen` — the HUMAN-ONLY resolution transitions
  // (there is DELIBERATELY no MCP resolve tool — the agent never resolves).
  | "review_comment_resolve"
  | "review_comment_reopen"
  // v2.4 review TASKS (finding = task) + unread (task 01KX0F6JEKCEV2JCTMEBMM3P4Z, PDR-0012). The
  // promotion "grill": `review_task_promote` is the HUMAN lâche brut (a `proposed` task
  // from a comment); the agent structures it via the MCP `review_task_structure` tool; and
  // `review_task_validate` is the HUMAN validation (there is DELIBERATELY no MCP validate tool
  // — the agent never validates). `review_task_list` reads the promoted-task list. Each
  // mutation pushes `repos://changed`.
  | "review_task_promote"
  | "review_task_validate"
  | "review_task_list"
  // `review_unread` reads the unread comment count of a `(workspace, repository)` feed;
  // `review_mark_read` zeroes it (stamps the read watermark). A mark-read pushes `repos://changed`.
  | "review_unread"
  | "review_mark_read"
  // `review_remap` re-anchors every open anchored comment of a `(workspace, repository)` against
  // the repo's CURRENT HEAD (the phase-4 `remap_repo_comments` engine): an anchor whose snippet
  // moved FOLLOWS the code, one whose snippet vanished flips `outdated`. Invoked by a LIVING
  // review after a `repos://changed` push (a commit/checkout advanced the repo), so the anchors
  // stay true to the moving diff. A mutation → pushes `repos://changed` (the feed re-pulls the
  // re-mapped/outdated anchors live).
  | "review_remap"
  // v2.6 KNOWLEDGE READ models (Phase 4 — the native Idées view + the Spec-detail annexes/deps).
  // Pure reads over the SAME `nyx_core::db` / `nyx_core::idea` accessors the Phase-1/3 MCP tools
  // call (one authority for UI + MCP, PDR-0005): no mutation, no `changed` broadcast — the Idées
  // surface re-pulls off the coarse `ideas://changed` push a MCP idea mutation emits.
  //
  // `idea_themes` ({ projectId }) — the by-THEME grouping of a project's backlog (one row per
  // theme, `theme=null` folds the ungrouped; each with `total` + per-status breakdown), biggest
  // group first. Backs the Idées group headers (counter + status mini-dots) + the theme facet.
  | "idea_themes"
  // `idea_list` ({ projectId, status?, theme? }) — the BOUNDED backlog rows, newest-first, each
  // ENRICHED with its linked Specs (rows show Spec badges; the drawer renders detail from the same
  // item). Composes `db::list_ideas` + `db::linked_specs_for_idea` (parity with `spec_list`).
  | "idea_list"
  // `idea_search` ({ projectId, query, theme?, status?, limit? }) — the FTS5 + fuzzy scored search
  // (the SAME `nyx_core::idea::search_ideas` the MCP tool serves): each hit carries an extract, the
  // blended score, and the idea's linked Specs.
  | "idea_search"
  // `global_search` ({ query, limit? }) → `GlobalSearchAnswer` — the backend of the `Mod+K`
  // palette: ONE ranked, bounded list over the seven entity types of the product, ALL PROJECTS
  // CONFOUNDED (Specs + the prose of their current revision, tasks, ideas, workspaces, projects,
  // managed commands, agent sessions). The SAME `nyx_core::search::global_search` engine any other
  // surface would call (one authority, PDR-0005). A pure read: no mutation, no `changed` broadcast.
  //
  // An EMPTY `query` is NOT an error and NOT an empty answer: it returns the SUGGESTIONS — the most
  // recently touched entities of every type — so the palette has a single rendering path for "just
  // opened" and "typed something". Beware what that means in the copy: these reflect what was last
  // TOUCHED, which is often an agent's doing rather than the user's own visits (there is no journal
  // of opened entities, and this Spec deliberately creates none). A query of ONE character returns
  // an empty list without touching the database. `limit` is the OVERALL cap and is CLAMPED BY THE
  // CORE — the caps are the product rule, so asking for more widens nothing.
  //
  // The answer is `{ hits, knowledge_empty }`, not a bare array: `knowledge_empty` says whether the
  // install carries any searchable Spec, task or idea AT ALL — a fact about the database that the
  // capped `hits` cannot be asked, and what the palette's "nothing to search yet" is keyed on.
  //
  // Each hit is a TYPED TARGET, never a URL: the backend says "this is a Spec, in this project" and
  // the renderer decides which route a kind lands on.
  | "global_search"
  // `annexe_list` ({ specId }) — a Spec's annexes newest-first, each with its blob `byte_len` (no
  // content in the list). Backs the Spec-detail "Documents" block (the `url` rows it also returns
  // are drawn by the neighbouring "Links" block instead, off `annexe_link_list`).
  | "annexe_list"
  // v50 — `annexe_link_list` ({ specId }) → `SpecLink[]`: the Spec's EXTERNAL LINKS, RESOLVED. A
  // link is a `url` annexe whose stored content IS the URL, and `annexe_list` never ships content,
  // so this read composes the shared authority's `kind`-narrowed list with a per-row resolve and
  // hands back `{ id, spec_id, title, url, created_at }` newest-first. Backs the Spec-detail
  // "Links" block. Pure read — the live refresh rides `specs://changed`. `specId` is a required
  // string: absent or empty REJECTS (as the MCP `annexe_list` twin does), it is not an empty list.
  | "annexe_link_list"
  // v50 — the APP-side annexe WRITES (the Spec-detail "Links" block). Both go through the SAME
  // `nyx_core::db` annexe authority the MCP `annexe_*` tools call (PDR-0005), so a link added or
  // removed here lands in exactly the state the agent's tool would have produced — including the
  // `url` rules, which live in that authority: absolute http(s) only, stored in CANONICAL form —
  // surrounding blanks trimmed, scheme and host lowered, the path's trailing slash dropped, so a
  // pasted ` HTTPS://X/a/ ` is stored and deduplicated as `https://x/a`, while whitespace or
  // control characters INSIDE it are still refused with their own sentence —, at most 2048
  // characters (the other kinds are stored byte-for-byte, edges included, and are NOT
  // length-bounded), ONE occurrence of a URL per Spec, and a blank `title` stored as the URL
  // itself. Each mutation pushes `specs://changed`, so
  // the open Spec detail re-pulls with no optimistic patching.
  //
  // `annexe_add` ({ specId, kind, title?, content }) → `{ annexe }`. `kind` is a CLOSED vocabulary
  // — `doc | artefact | url | prototype` — and `title` is optional ONLY for `kind='url'`, where
  // ABSENT, EMPTY (`''`) and BLANK (`'   '`) are the SAME case — "no label given" — and all three
  // store the URL itself as the title; `doc` / `artefact` / `prototype` REQUIRE a non-empty one.
  // That is the rule the backend applies, not prose: the same conditionality the MCP tool publishes
  // in its JSON-Schema. A refused write rejects with the BACKEND's own sentence — the surface
  // DISPLAYS it, it does not re-word it. The refusal is either an ARGUMENT one, and there the rule
  // (not a list) is that EVERY required string is refused ABSENT **or** EMPTY, as the MCP surface's
  // `require_str` refuses it — `title` is the ONE exception, and only for `kind='url'`: it escapes
  // BOTH halves of that rule there, so an empty `title` on a link is accepted where an empty
  // `content` is not — while a `kind` outside the vocabulary is refused as such; or the refusal is
  // the annexe authority's (a non-http(s) URL, whitespace inside it, over 2048 characters, or a URL
  // already attached to this Spec).
  | "annexe_add"
  // `annexe_remove` ({ id }) → `{ removed, id }`. Idempotent for an UNKNOWN id (reports
  // `removed: false`) — but `id` is a required string, so absent or empty is the argument refusal
  // above, not a calm `false`; the stored content is GC'd once no annexe references it any more.
  | "annexe_remove"
  // `spec_dependency_view` ({ specId }) — a Spec's dependency view: the directional `blocked_by` /
  // `unblocks` edges + their computable badge counts + the `related` context links. Backs the
  // Spec-detail dependency badges ("bloquée par N" / "débloque M"); the board reads the counts off
  // the enriched `spec_list`.
  | "spec_dependency_view"
  // v2.9 CATEGORY — the HUMAN half of PDR-0017. The AGENT classifies and MINTS a category on the
  // fly through MCP; these are what a human does afterwards — assign / un-assign from the Spec
  // detail, and CURATE THE VOCABULARY ITSELF (create / rename / delete) from the project settings.
  // They exist on this surface ALONE. Each is a thin adapter over the `nyx_core::category`
  // service — the same-project refusal of an assignment, the project scope of a curation gesture,
  // the resolution of a name and its ambiguity, the IMMUTABILITY OF THE SLUG, the refusal of a
  // blank name and the TWO REFUSALS OF A DELETION (a system slug, a category a Spec still carries)
  // are all pronounced THERE, once, and reach this surface as the authority's own sentence.
  // Nothing here re-checks any of them.
  //
  // THE 2026-09-02 AMENDMENT of PDR-0017 retired the proposed/confirmed axis: `category_confirm`
  // is gone, and so is the `state` field of the payload — an agent's mint is valid as it stands.
  //
  // `category_list` ({ projectId }) → the project's vocabulary in CURATED order, each entry
  // `{ id, slug, name, color_token, created_by_type, confirmed_at, system }` — the SAME payload
  // the agent's `spec_category` read answers with. `system` is the BACKEND's verdict on the slug
  // (`db::SYSTEM_CATEGORY_SLUGS`, the very list `category::delete` refuses on), so the settings
  // can omit its Delete control on those rows without carrying a copy of the four literals.
  // A pure read.
  | "category_list"
  // `category_create` ({ projectId, name }) → `{ category }`. The human MINTS a vocabulary entry
  // from the settings. The slug is DERIVED from the name by the service (collision-safe, and never
  // one of the four system slugs) and is not an argument. Filed `created_by_type: human` with the
  // `ui` provenance the service freezes. Pushes `specs://changed`.
  | "category_create"
  // `category_delete` ({ projectId, categoryId }) → `{ deleted: true, category }` — the deleted
  // row comes BACK, in the same shape `category_list` answers with, so a caller can name what it
  // just removed without having kept it. REFUSED by the service on a system category (the four
  // slugs the harness reads) and on a category a Spec still carries — that second refusal NAMES
  // the count, so the app never has to compute it. Pushes `specs://changed`.
  | "category_delete"
  // `spec_category_assign` ({ specId, categoryId }) → `{ spec, category }`. Assign BY EXACT ID —
  // the selector has just read the list. There is no label path and no creation on THIS command:
  // minting from a typed label is the agent's ergonomics (PDR-0017), and a human creates
  // deliberately through `category_create`. A category of ANOTHER project is refused by the
  // service, naming both projects. Pushes `specs://changed`.
  | "spec_category_assign"
  // `spec_category_unassign` ({ specId }) → `{ spec, category: null }`. A FIRST-CLASS write with
  // its own event, not a no-op — and a SEPARATE command, so an absent `categoryId` can never read
  // as "un-categorize". Pushes `specs://changed`.
  | "spec_category_unassign"
  // `category_rename` ({ projectId, categoryId, name }) → `{ category, homonym_slugs }`. Changes
  // the LABEL only: the slug is not an argument, and the write path does not take one. A rename
  // INTO a homonym is ALLOWED (PDR-0017 — names are labels), so `homonym_slugs` is not a refusal
  // but the RESOLVER'S VERDICT on the new name: the slugs that now answer it, `[]` when it stayed
  // unique. The surface DRAWS that conflict; it never computes it. Pushes `specs://changed`.
  | "category_rename"
  // v2.8 ORCHESTRATION ENGINE read models (Phase 6 — the React Flow graph view). Pure reads over
  // the SAME `nyx_core::db` engine accessors + `render_agent_context` the Phase-2/4/5 interpreter,
  // authoring and context tools use (one authority for UI + MCP, PDR-0005). No mutation, nothing
  // persisted; the graph is READ-ONLY (authoring stays MCP-first).
  //
  // `engine_workflow_list` ({ projectId? }) — the workflows the picker offers: the BUILT-IN defaults
  // (seeded ndev/nspec/ndoc, `project_id` null) plus the active project's OWN forked/custom ones.
  | "engine_workflow_list"
  // `engine_workflow_graph` ({ workflowId }) — one workflow + ALL its nodes (the deterministic dump
  // the renderer reconstructs the pure tree from — `parent_id` + `order_index`).
  | "engine_workflow_graph"
  // `engine_run_list` ({ workflowId }) — the workflow's runs newest-first; the runtime view reads the
  // latest for `current_node_id` + the blackboard JSON.
  | "engine_run_list"
  // `engine_spec_runs` ({ specId }) — the runs bound to ONE Spec, ACROSS workflows, newest-first.
  // The SPEC-side runtime view (the run modal opened from `/work/$specId` and from the dev overlay)
  // reads it: it starts from a Spec, and a Spec is driven by its ndev run plus the child-workflow
  // runs ndev spawns. An unknown Spec is an EMPTY list (the modal shows its "no run yet" state).
  | "engine_spec_runs"
  // `engine_effector_context` ({ role, specId?, subject? }) — the EFFECTOR context the node inspector
  // shows for a spawn node. `specId` present (a run bound) → the LIVE `render_agent_context` (role
  // template + resolved `{{rules}}`); `specId` absent (no run, finding #35) → the STATIC role template
  // (the sub-agent's brief) with run-specific parts marked "resolved at run". `subject` is the spawn
  // node's own subject: naming a Spec section narrows the manual to it, as the run forwards it.
  | "engine_effector_context"
  // v2.7 DESIGN-SYSTEM read/render seam (Phase 3, PDR-0010). Pure reads/renders over the
  // `design` / `design_components` authority (Phase 1) + the composition renderer
  // (`nyx_core::render`, task #6), served by the sidecar `db_command` dispatch. No agent
  // ACTION here — the agent drives the proto via the MCP `proto_*` tools, never a UI button.
  // `design_get` ({ projectId }) — a project's design-system row: derived `theme_tokens_json`
  // (the iframe `@theme`) + the DESIGN.md `content`, or `null`.
  | "design_get"
  // `design_tokens_ingest` ({ projectId }) → the ingest RUN — DERIVE the project's THEME TOKEN
  // layer from the app's own stylesheet (review #42: nothing ever wrote `theme_tokens_json`, so the
  // iframe's `@theme` was empty and every `bg-primary` rendered grey). Walks the same repos as
  // `design_inventory`, under the project's RECIPE (its `exclude` globs + its `ignored_dirs`), and
  // records WHERE the tokens live — the layer is REFERENCED, never copied per component.
  //
  // `{ repos_scanned, candidates, sources: [{ repository_id, repo, path, variable_count }],
  //    skipped: [{ repo, path, reason }], skipped_count, pruned_dirs: string[], pruned_dirs_count,
  //    variable_count, dark_count, changed, provenance_changed, tokens_json }`.
  //
  // `skipped[]` / `skipped_count` / `pruned_dirs[]` are the PROVENANCE half (finding
  // 01KYSCX562SWDXZBBAPX397X8F): the run used to report only what it TOOK, so a layer merged from a
  // `.generated` mirror plus a docs theme looked exactly like a healthy one. `reason` is a CODE —
  // `recipe_exclude` / `doc_tool_theme` / `foreign_namespace` / `not_a_token_sheet` / `unreadable` /
  // `peripheral_sheet` / `outranked` / `not_ranked` — and `skipped` is the bounded, most-informative
  // head of the `skipped_count` discards, each list carrying its own TOTAL so a truncation never
  // reads as a short list; `pruned_dirs[]` names the directories the walk never entered
  // (`.generated` in there is the proof the generated mirror was not swept). A run that detects
  // nothing leaves a hand-authored layer untouched (`changed: false`, which is about the TOKENS) and
  // records only its report (`provenance_changed`) — that run stores no token at all, so everything
  // renders grey and the report is the ONLY diagnostic there is (finding
  // 01KYSNVW31C0E8YQPET0505HW2).
  //
  // Emits `prototypes://changed` when it moved. Driven by the AGENT (`design_tokens`); the `/design`
  // tokens panel only READS the layer and lets a human EDIT one value — its ingest button was
  // removed by finding 01KYKX7HBP4ZSSS6ZDM31T6A5E. That panel's PROVENANCE block renders whatever
  // the STORED layer carries of this shape (`sources`, plus `skipped`/`skipped_count`/`pruned_dirs`
  // — finding 01KYSHSPGSPC98C49X68MWY60D). The ingest PERSISTS those three keys into
  // `theme_tokens_json` (finding 01KYSK7GS9C3ZRJYW00ZA0H6GF), whatever it retained: the panel cannot
  // re-run an ingest, so anything living only in this response would be invisible to it. A stored
  // layer WITHOUT them is one NO ingest ever wrote (a theme authored by hand), and the panel says
  // exactly that rather than "rien n'a été écarté".
  | "design_tokens_ingest"
  // `design_tokens_set` ({ projectId, variables?, remove?, dark? }) → `{ tokens_json,
  // variable_count, dark_count }` — EDIT that single layer. One edit re-themes every component,
  // composition and preview at once. Emits `prototypes://changed`.
  | "design_tokens_set"
  // `design_render` ({ projectId, compositionJson, prototypeId? }) → `{ markup, node_count,
  // unrendered, unrendered_count, css_slots, css_slot_count, navigation, navigation_count,
  // navigation_broken_count }` — compile a composition (component tree + params) to MARKUP entirely
  // in the sidecar (task #6). The renderer compiles NOTHING; it only displays this markup in the
  // iframe (task #7).
  //
  // The render is LENIENT (finding `01KYT78699XG6Q6P3VZC64W8H8`): the frame ALWAYS gets markup, and
  // a node the sidecar cannot render comes back as a NAMED placeholder IN PLACE plus one entry in
  // `unrendered[]` — `{ node_key, component, reason, detail }` — instead of blanking the whole page.
  // A composition under design always holds `[NEW]` nodes, so "mostly renderable, with holes" is the
  // NORMAL state of a page in flight. The renderer therefore computes NO notion of a hole of its own
  // (`composition-holes.ts` was deleted with this): it DISPLAYS this list.
  //   * `node_count` — how many nodes the walk visited, placeholders included;
  //   * `unrendered[].reason` — one of five stable machine codes pinned, with their short labels, by
  //     `contract/design-fixtures.json` (`unrendered_nodes.reasons`), which is what the display maps
  //     (see `UNRENDERED_REASON_LABELS` in `components/design/use-design-render`);
  //   * `unrendered[]` is ordered depth-first CHILDREN BEFORE THEIR PARENT — a hole nested under a
  //     hole is listed FIRST. A surface that reads it as a pre-order tree lies about the hierarchy;
  //   * `unrendered_count` — the length of that list, stated by the sidecar itself.
  //
  // `prototypeId` IS WHAT MAKES A LINK NAVIGATE (finding `01KZ3JN69E32FYBF01NBE4FWJD`). A node may
  // state the page it opens (`navigate_to`, a `prototype_pages.id`); resolving that id to the slug
  // the shim navigates with needs the prototype's page DIRECTORY, and this argument is the only
  // thing that supplies it. It is OPTIONAL — a composition that links nowhere owes it nothing — and
  // that is exactly the trap: WITHOUT it the render knows of no page at all, so every link comes
  // back `unknown_page`, stamped `data-nyx-page-missing` with NO `data-nyx-page`, and the whole
  // inter-page parcours is dead with no error anywhere. Any host rendering a PROTOTYPE PAGE passes
  // it; the one caller that existed never did.
  //   * `navigation[]` — one entry per LINKING node, honoured or not:
  //     `{ node_key, page_id, page, page_title, status, detail }`. `page` is the destination SLUG
  //     (what `data-nyx-page` carries and the shim posts) and `page_title` what a human reads; both
  //     are resolved AT EVERY RENDER off the stored id, so a rename is free and neither can go stale
  //     — both `null` exactly when the id resolved to nothing;
  //   * `navigation[].status` — one of three stable machine codes pinned, with their short labels,
  //     by `contract/design-fixtures.json` (`navigation.statuses`): `linked` (the only one that
  //     navigates), `unknown_page`, `no_anchor`. There is no code meaning "nothing happened";
  //   * `navigation_count` / `navigation_broken_count` — the length of that list and how many of its
  //     entries do NOT navigate, both stated by the sidecar itself.
  // The only refusal left is a composition that does not PARSE: a readable error, never a blank.
  | "design_render"
  // `design_component_list` ({ projectId }) → `{ components, linked_names }` — the CATALOGUE read
  // model (task #10): the project's `design_components` (DS + NEW) + the names referenced by its
  // proto compositions (the "liés-page" filter). Portée PROJET.
  | "design_component_list"
  // `design_component_get` ({ componentId }) → `DesignComponent | null` — ONE component (the
  // composant-detail read model, task #11).
  | "design_component_get"
  // `design_component_nodes` ({ projectId, componentName }) → `{ nodes }` — every COMPOSITION NODE
  // across the project's proto compositions that instantiates the component, each pre-parameterized
  // (variant + node_key path + params_json). Backs "clic sur un nœud → détail pré-paramétré" (#11).
  | "design_component_nodes"
  // `proto_variant_set_node_params` ({ variantId, nodeKey, paramsJson }) → `{ variant }` — PERSIST a
  // node's params PER NODE into the variant's composition tree (task #11). Emits `prototypes://changed`.
  | "proto_variant_set_node_params"
  // `design_inventory` ({ projectId }) → `{ discovered, repos_scanned, pending_count,
  // synthesized_count, component_count, moved_count, materialized_count, orphan_count,
  // pruned_count, flagged_orphan_count, scan_error_count }` — the RUNTIME TRIGGER (reviews #23,
  // #40): scan the project's repos, group each component's files into a BUNDLE (templateUrl
  // resolved, non-components filtered) and UPSERT its discovery — the TEMPLATE is synthesized by an
  // agent (`design_refresh_component`), so `pending_count` reports the bundles still awaiting one.
  // It also RECONCILES the catalogue: a bundle MOVED on disk is re-anchored by CONTENT
  // (`moved_count`), a greenfield [NEW] the front now implements is MATERIALIZED
  // (`materialized_count`), and a row whose source is GONE is reported (`orphan_count`). Only a row
  // carrying NO AUTHORED CONTENT (no template, no hash, no params, no states) is pruned
  // (`pruned_count` counts the deletions that really happened — a synthesis committed by another
  // session mid-run makes the delete stand down); one carrying content is flagged and KEPT
  // (`flagged_orphan_count`, which also covers rows a run that could not judge left flagged — the
  // badge no longer blinks off on a transient I/O failure).
  // `scan_error_count` says whether the walk could read everything it
  // looked at — non-zero means NOTHING was pruned in the repos concerned. The `/design` header
  // `scan_error_count` says whether nothing was pruned in the repos concerned. Called by the AGENT
  // (its `design_inventory` MCP tool shares the same authority) — NO `/design` gesture triggers it
  // since finding 01KYKX7HBP4ZSSS6ZDM31T6A5E; the page repaints on the `prototypes://changed` this
  // emits, and only when the catalogue actually moved. GREENFIELD-safe.
  | "design_inventory"
  // `design_component_delete` ({ componentId, excludeSource? }) → the REMOVAL — take a component out
  // of the catalogue (review #44: the catalogue had NO removal at all, so an entry a wrong glob had
  // catalogued was stuck there forever). Backs the `/design` card's delete action; its MCP twin is
  // `design_delete_component`, over the same `nyx_core::design::remove_component` authority.
  //
  // `{ deleted, effect: { ephemeral, verdict, repo, source_path, recipe_source },
  //    excluded: string[], recipe_materialized, refused, warnings: string[] }`.
  //
  // IT SAYS WHAT IT REALLY ACHIEVES (finding 01KYSCX4PNKTMX17YVG9XD8KBZ, UI half
  // 01KYSHSPGSPC98C49X68MWY60D). A component's identity IS its source: deleting the row of a
  // component whose file still matches the project's recipe is UNDONE by the next `design_inventory`,
  // which re-discovers it as a pending component under a NEW id. The command used to answer a bare
  // `{ deleted: true }`, so the surface promised a removal that did not last. `effect.ephemeral` +
  // `effect.verdict` (the discovery's verdict on that path: `elected` / `pinned` / `excluded` /
  // `no_rule` / `no_marker` / `pruned_dir` / `missing` / `unreadable`, or `greenfield` for a row with
  // no source) say whether it lasts, and `warnings[]` carries the sentences that name the problem AND
  // the correction — a caller DISPLAYS them, it does not re-write them: only the backend knows the
  // verdict and the recipe.
  //
  // `excludeSource: true` is the DURABLE form of the same decision: the bundle's paths are appended
  // to the project's recipe `exclude` list (materializing that recipe when the project still ran on a
  // default — `recipe_materialized`) in the SAME transaction as the delete, so the next inventory
  // cannot bring the row back; `excluded[]` lists the patterns written.
  //
  // REFUSALS come back as an ERROR and delete NOTHING: an unknown id, a PINNED entry (a pin bypasses
  // `exclude`), a source path holding a glob character (`*` / `?` — an `exclude` entry IS a glob), an
  // exclusion the recipe cannot hold (budget full), or an exclusion asked for a greenfield row. The
  // caller must SHOW that message, not swallow it. Emits `prototypes://changed` on a real removal.
  | "design_component_delete"
  // --- PROTO read-model + UI-driven writes (review #24) — the data layer T25-T29 consume ------
  // `proto_tree` ({ specId }) → the FULL tree of a Spec's prototype (pages/versions/variants + rounds
  // + both-tier election is_main vs RETENU + anchored comments open/resolved), or `null` when the
  // Spec has no prototype. A pure read.
  | "proto_tree"
  // `proto_composition` ({ prototypeId }) → the FROZEN elected composition (the coder SEAM): per page
  // pinned in the design-LOCKED round, the arbre nommé = placement with each node's NATURE ([DS] with
  // a per-node `component_ref` / [NEW] to-build / [inline]), the [NEW] candidates, and the resolved
  // pre-dev review + open count. `null` until the design is LOCKED (the "pas encore figée" cue). A pure read.
  | "proto_composition"
  // `proto_round_manifest` ({ roundId }) → `RoundManifestPage[]` — the MANIFEST of ONE round,
  // resolved: per page pinned in it, the entry (the page VERSION that round froze + the USER's
  // RETENU) and the EFFECTIVE retained variant. The same pair `proto_tree` resolves for the CURRENT
  // round, for ANY round — which is what makes a PAST round CONSULTABLE. A pure read: consulting a
  // round creates nothing, copies nothing and moves no election.
  | "proto_round_manifest"
  // `proto_elect` ({ roundId, pageId, variantId, rationale?, decidedBy? }) → `{ round_page }` — POSE
  // the USER's RETENU. Refused on a design-locked round (élection figée). Emits `prototypes://changed`.
  | "proto_elect"
  // `proto_election_clear` ({ roundId, pageId }) → `{ round_page }` — RETIRE the USER's RETENU
  // (reversible). Same lock guard. Emits `prototypes://changed`.
  | "proto_election_clear"
  // `proto_comment_add` ({ prototypeId, body, nodeKey?, originVariantId?, authorKind?, authorRef? }) →
  // `{ feedback }` — ADD a pre-dev comment anchored to a tree node (authorKind defaults to `user`).
  // Emits `prototypes://changed`.
  | "proto_comment_add"
  // `proto_comment_resolve` ({ feedbackId, resolutionNote?, resolutionViaVariantId? }) → `{ feedback }`
  // — RESOLVE a pre-dev comment. Emits `prototypes://changed`.
  | "proto_comment_resolve"
  // `proto_round_rebase` ({ prototypeId, sourceRoundId, label, summary? }) → `{ round }` — ROLLBACK
  // onto an earlier round (opens a new round cloning its manifest). Emits `prototypes://changed`.
  | "proto_round_rebase"
  // Settings → Integrations: the provider registry (`nyx_core::integrations`).
  //
  // `integration_list` (no args) → {@link IntegrationStatus}[] — the four registry providers with
  // their available / coming-soon flags, each `installed` derived from the provider's REAL config
  // (Claude Code's `enabledPlugins`) rather than from a nyx-side cache. A pure read.
  | "integration_list"
  // `integration_install` ({ provider }) → {@link IntegrationStatus} — install the ONE bundled nyx
  // plugin (MCP server + session-capture hooks) for that provider, and answer its refreshed row.
  // Shells out to the provider's CLI, so it takes seconds and can fail with its message verbatim.
  | "integration_install"
  // `integration_remove` ({ provider }) → {@link IntegrationStatus} — the mirror gesture: remove the
  // bundled plugin and all nyx residue, and answer the refreshed row.
  | "integration_remove"
  // Settings → Terminal: the DEFAULT-SHELL preference (`nyx_core::shells`).
  //
  // `shell_list` (no args) → {@link DetectedShell}[] — the shells REALLY installed on the host,
  // discovered from the machine the way Windows Terminal discovers its profiles (Windows: Windows
  // PowerShell, PowerShell 7, Command Prompt, Git Bash, one entry per installed WSL distribution;
  // Unix: the `/etc/shells` registry). Every entry is verified against the disk, so nothing the
  // picker offers can fail at spawn. A pure probe — no DB, no mutation.
  | "shell_list"
  // `app_settings_get` (no args) → {@link AppSettings} — the app-level preferences of this
  // install, read from the `app_settings` store. `defaultShell: null` means the user has
  // expressed nothing ("Auto"), which is a DIFFERENT fact from any shell we could name: it is what
  // keeps the historical resolution (`$SHELL` → per-OS default) in force.
  | "app_settings_get"
  // `set_default_shell` ({ path: string | null, args?: string[] }) → void — RECORD the chosen
  // shell, or CLEAR the preference back to "Auto" with `path: null`. `path`/`args` are taken
  // verbatim from the picked {@link DetectedShell} (the renderer never composes a command line).
  // Applies to the NEXT terminal spawned: the sidecar resolves the preference at spawn time, so
  // terminals already on screen are deliberately left running their own shell. Emits NO `changed`
  // topic for that reason — there is no surface to invalidate.
  | "set_default_shell"
  // misc
  | "window_controls_visible";

/**
 * The closed set of backend EVENT channels the front subscribes to — every
 * `listen("...")` call-site. Adapters map each to their transport; the typed
 * `subscribe*` methods above wrap the common ones.
 */
export type BackendEvent =
  | "pty://output"
  | "pty://exit"
  | "command://output"
  | "command://output-cleared"
  | "command://state"
  | "command://ack"
  | "terminal://busy-state"
  | "terminal://exec-state"
  // Per-terminal CPU%/RAM live readings (FEEDBACK #28).
  | "terminal://stats"
  // Coarse "this collection changed → re-fetch it" invalidations (the
  // ChangedTopic axis). The components subscribe to these by their channel string.
  | "commands://changed"
  | "workspaces://changed"
  | "terminals://changed"
  | "agent-sessions://changed"
  // The Spec domain (Specs / revisions / tasks / dependencies) — the v2.4 authoring +
  // transition MCP tools push this (`ChangedTopic::Specs` → `specs://changed`) so an open
  // Specs board / PRD detail re-pulls after a mutation with NO reload or poll.
  | "specs://changed"
  // v2.6 (Phase 1/4): the IDEAS domain (ideas / idea_specs) — every idea authoring/maintenance
  // MCP tool pushes `ChangedTopic::Ideas` → `ideas://changed`, so an open Idées view re-pulls its
  // themes/rows after an agent mutation with NO reload or poll (mirror of `specs://changed`).
  | "ideas://changed"
  // v2.8 (Phase 4/6): the ORCHESTRATION graph (workflows / nodes) — every `workflow_*` / `node_*`
  // authoring MCP tool pushes `ChangedTopic::Workflows` → `workflows://changed`, so an open Engine
  // view re-pulls its re-built tree after an agent authoring mutation (mirror of `ideas://changed`).
  | "workflows://changed"
  // v2.7 (Phase 2/3): the PROTOTYPE domain (prototypes / pages / variants / feedback / rounds).
  // Every proto-driving MCP tool pushes `ChangedTopic::Prototypes` → `prototypes://changed`, so an
  // open design-system surface (catalogue / composant detail / renderer) re-pulls after an agent
  // mutation with NO reload or poll (mirror of `specs://changed`).
  | "prototypes://changed"
  // v2.4 (task 01KX0F6GDZEH3YN6QJDB7FC0WC / 01KX0F6GY96096AHWD8RZ2BMM0): a WATCHED repo's
  // work-tree/ref changed (edit / commit / checkout). The living review re-pulls the
  // affected repo's `repo_diff` / `repo_git_view` — the "sans polling" refresh (Q-015).
  | "repos://changed"
  // PRD « Scope Système » : the pending APPROVAL queue moved. Emitted from BOTH sides, which is
  // what makes one topic enough — the human's own `system_approval_grant` / `system_approval_deny`
  // routes emit it after the decision, and the MCP dispatcher emits it when a protected call is
  // REFUSED with `approval_required`. That second one is the odd case worth knowing: it is the one
  // place in nyx where a REFUSAL is the event, because the refusal is exactly the moment the human
  // has something to do (`ToolOutcome::effects` rides the success path and cannot carry it).
  | "system-approvals://changed";
