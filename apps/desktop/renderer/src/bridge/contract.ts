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
 * A `review_tasks` row (mirror of `nyx_core::db::ReviewTask`). `finding = task`: the promoted
 * comment IS the coder task. `from_comment_id` is the trail back to its source comment; `severity`
 * is optional; `grill_state` walks the grill.
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
  /** Close the window. The CLOSE-INTERCEPTION policy (agent-session warnings) is a
   *  caller concern: the UI first asks the backend via `agent_close_warnings`
   *  (see {@link NyxBridge.invoke}) and only calls `close()` when clear / confirmed. */
  close(): Promise<void>;
  /** Props to spread on the draggable chrome element so the shell can move the
   *  window by it. The Tauri adapter returns `{ "data-tauri-drag-region": true }`;
   *  an Electron adapter returns the CSS `-webkit-app-region: drag` equivalent. */
  dragRegionProps(): Record<string, unknown>;
  /** Subscribe to the OS "window close requested" signal (the user clicked the OS
   *  close button / Alt-F4). The handler runs BEFORE the window is destroyed — used
   *  to flush pending scrollback. Returns an idempotent {@link Unsubscribe}. Resolves
   *  with a no-op unsubscribe when the shell has no such hook (e.g. under tests). */
  onCloseRequested(handler: () => void): Promise<Unsubscribe>;
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
  // v2.2 workspace CONTEXT-RESTORE plan (task 01KWHPV33M4H5YSMGYNW9TG6J0): after a restart,
  // WHAT a workspace offers to resume — computed by REUSING the existing resume policies (no
  // 3rd system): per-session `decide_resume` verdicts (`resume_sessions`), the
  // `restart_on_startup` services, and the crash-`interrupted` instances. The USER chooses
  // what to resume; nyx never blind-resumes. Read-only, idempotent. `{ workspaceId }`.
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
  | "command_instance_list"
  | "command_import_scripts"
  | "command_import_create"
  | "command_source_refresh"
  | "command_resync_source"
  | "command_unlink_source"
  // agents
  | "agent_active_sessions"
  | "agent_close_warnings"
  // The RUNTIME agent-activity map (the live dot): which terminals are working/waiting
  // or carry a "response ready" notification. NEVER persisted — an in-memory read off the
  // same store the Claude per-turn hooks write. Re-pulled on `agent-sessions://changed`.
  | "agent_activity_snapshot"
  // Clear the focus-aware "response ready" notification for a viewed terminal (the
  // activity analogue of `terminal_exec_mark_read`). `{ terminalId }`. Idempotent.
  | "agent_mark_ready_read"
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
  // integrations
  | "integration_list"
  | "integration_install"
  | "integration_remove"
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
  // v2.4 (task 01KX0F6GDZEH3YN6QJDB7FC0WC / 01KX0F6GY96096AHWD8RZ2BMM0): a WATCHED repo's
  // work-tree/ref changed (edit / commit / checkout). The living review re-pulls the
  // affected repo's `repo_diff` / `repo_git_view` — the "sans polling" refresh (Q-015).
  | "repos://changed";
