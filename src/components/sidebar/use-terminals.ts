import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/**
 * A `terminals` row as returned by the backend record commands (see
 * `db::Terminal`). This is the DB-record identity of a terminal — distinct from
 * the live PTY id, which is owned per-`<Terminal>`-instance by `usePty`.
 *
 * THE ID COORDINATION (read this before touching the lifecycle):
 *  - `id` here is the SQLite record id (`i64`), the stable key for the sidebar,
 *    ordering, close, rename and reorder.
 *  - The PTY id (`u64` from `pty_spawn`) is internal to the mounted `<Terminal>`
 *    for this record; the front never needs to see it. `<Terminal>` spawns its
 *    PTY at the record's `cwd` and routes `pty://output` by that PTY id itself.
 *  - So a terminal = ONE record (this row) + ONE PTY (spawned by its `<Terminal>`
 *    at `cwd`). Create = `create_terminal(cwd)` then mount `<Terminal cwd>`.
 *    Close = unmount `<Terminal>` (its teardown calls `pty_close`) AND
 *    `close_terminal(id)` (flips the record to `closed` so it is not re-spawned).
 *  - There is intentionally no backend command linking the two id-spaces; the
 *    join lives here, in the front, by keying each `<Terminal>` on its record id.
 */
export interface TerminalRecord {
  id: string;
  cwd: string;
  label: string | null;
  scrollback: string;
  status: "alive" | "closed";
  order_index: number;
  /** Epoch milliseconds (see `db::Terminal`); a plain JS number. */
  created_at: number;
  /** Epoch milliseconds; bumped on every mutation (rename/reorder/scrollback/close). */
  updated_at: number;
  /** Epoch milliseconds when closed, or `null` while the terminal is alive. */
  closed_at: number | null;
  /**
   * Epoch ms of the last time this terminal was active, or `null`/absent if it
   * was never the active one. On relaunch the launcher reopens on the alive
   * terminal with the GREATEST value so the user returns to where they left off.
   */
  last_active_at?: number | null;
  /**
   * Workspace this terminal is bound to (`null`/absent = unattached). Set by the
   * Phase-1 backend binding (attach/pin/auto-attach); the sidebar spine groups
   * terminals under their workspace by this id.
   */
  workspace_id?: string | null;
  /**
   * `auto` (follows the resolved cwd) or `manual` (pinned). Mirrors
   * `db::Terminal.workspace_binding_mode`; absent/`auto` for a fresh terminal.
   */
  workspace_binding_mode?: "auto" | "manual";
  /**
   * Run-state of the terminal's foreground process, the SELECTION-orthogonal
   * "run-state channel": drives the `<TerminalStateBadge>` / `<StatusDot>`.
   * Optional + defaults to `'idle'`. PRD-2.1 feeds REAL states from the backend:
   * `running` while a foreground command is live, `success`/`error` when it
   * exits (driven by OSC 133 shell integration), via the `terminal://exec-state`
   * event keyed by record id and persisted on the record (the authority after a
   * restart).
   */
  exec_state?: ExecState;
  /**
   * Exit code of the last finished command (`null`/absent = none yet, or an end
   * event with no parseable code). Mirrors `db::Terminal.exec_exit_code`; the
   * backend maps `0` → `success`, non-zero → `error`.
   */
  exec_exit_code?: number | null;
  /**
   * Whether the terminal's SETTLED result (`success`/`error`) is UNREAD — the
   * user has not yet viewed it since it finished. Mirrors the persisted
   * `db::Terminal.exec_state_unread`. This — NOT live selection — drives the
   * settled `<TerminalStateBadge>` visibility, so a viewed badge stays hidden
   * even after re-deselecting the terminal (PRD-2.1 user story #3). The backend
   * is the sole authority for this bit; the front clears it via
   * `terminal_exec_mark_read` when the terminal is viewed (and optimistically
   * mirrors the clear locally).
   */
  exec_state_unread?: boolean;
}

/**
 * The four run-states a terminal/command can be in (the run-state channel,
 * orthogonal to selection):
 *  - `idle`    — nothing running (gray dot, NO terminal badge);
 *  - `running` — a foreground process is live (blue `--info`, pulsing);
 *  - `success` — last process exited 0 (green `--success`, static);
 *  - `error`   — last process exited non-zero (red `--destructive`, static).
 */
export type ExecState = "idle" | "running" | "success" | "error";

/**
 * Payload of the backend `terminal://exec-state` event (PRD-2.1): a terminal
 * RECORD's exec-state transition, keyed by the persistent `terminal_id`. Mirrors
 * the Rust `TerminalExecStatePayload` (`bridge.rs`) — deliberately `snake_case`
 * (the DB-record shape), so it folds straight onto the matching `TerminalRecord`
 * fields. `unread` is the persisted notification bit; `idle`/`running` are never
 * unread.
 */
export interface TerminalExecStatePayload {
  terminal_id: string;
  state: ExecState;
  exit_code: number | null;
  unread: boolean;
  updated_at: number;
}

/** The imperative surface the sidebar + keyboard shortcuts drive. */
export interface UseTerminals {
  /** Records in sidebar order; only `alive` ones are mounted/shown. */
  terminals: TerminalRecord[];
  /** Record id of the active (visible) terminal, or null while loading/empty. */
  activeId: string | null;
  /** True until the initial `list_terminals` has resolved. */
  loading: boolean;
  /**
   * Create a new terminal, append it, and activate it. With no argument it uses
   * the default cwd (home); an explicit `cwd` overrides it (used by the e2e seam
   * to open terminals at distinct directories, and by the per-workspace `+` to
   * open at the workspace path). Resolves with the new record.
   */
  create: (cwd?: string) => Promise<TerminalRecord>;
  /**
   * Attach a terminal to a workspace (binding `mode`, default `manual`) and
   * reflect the new `workspace_id` locally so the sidebar spine groups it under
   * that workspace. Used by the per-workspace `+` to scope a freshly-created
   * terminal to the workspace it was launched from.
   */
  attach: (id: string, workspaceId: string, mode?: "auto" | "manual") => Promise<void>;
  /**
   * Run the backend auto-attach for a terminal RECORD given its live `cwd`
   * (read from `terminal_info` by the caller): the backend resolves the
   * longest-ancestor KNOWN workspace and, for an `auto`-mode terminal, binds it.
   * Reflects the resulting `workspace_id` locally (so the sidebar moves it out of
   * the loose TERMINALS section into the matched workspace). A no-op for a
   * `null`/unmatched cwd. Resolves with whether the binding changed.
   */
  autoAttach: (id: string, cwd: string | null) => Promise<boolean>;
  /**
   * Mark every terminal bound to one of `workspaceIds` as LOOSE (workspace_id →
   * null, mode `auto`) locally — mirroring the backend's `ON DELETE SET NULL`
   * when those workspaces are removed (e.g. a project delete). Without this the
   * sidebar would keep grouping a live terminal under a workspace that no longer
   * exists, hiding it from both its (gone) project and the loose section.
   */
  detachFromWorkspaces: (workspaceIds: string[]) => void;
  /** Close a terminal: drop it from the list, persist `closed`, re-target active. */
  close: (id: string) => Promise<void>;
  /** Make `id` the active (visible) terminal. */
  setActive: (id: string) => void;
  /** Activate the next terminal in order (wraps). */
  activeNext: () => void;
  /** Activate the previous terminal in order (wraps). */
  activePrev: () => void;
  /** Reorder the list to the given id sequence and persist it. */
  reorder: (ids: string[]) => Promise<void>;
  /** Rename a terminal's label (optimistic; persisted). */
  rename: (id: string, label: string | null) => Promise<void>;
  /**
   * Mark a terminal's settled exec-state as READ (PRD-2.1): clear its local
   * `exec_state_unread` flag immediately (so the settled badge disappears at
   * once) and persist the clear via the backend `terminal_exec_mark_read`. The
   * settled `exec_state` + exit code are PRESERVED — only the unread bit clears.
   * A no-op for a terminal that is not currently unread. Called when a terminal
   * is VIEWED, and immediately when a `success`/`error` arrives for the
   * already-active terminal.
   */
  markRead: (id: string) => void;
}

/**
 * Resolve the default cwd for a freshly created terminal. Tries the user's home
 * directory via the Tauri path API; on any failure falls back to `"."` (the
 * backend resolves it relative to nyx's own cwd). Lazily imported so the path
 * plugin is only touched when a terminal is actually created.
 */
async function resolveDefaultCwd(): Promise<string> {
  try {
    const { homeDir } = await import("@tauri-apps/api/path");
    const home = await homeDir();
    return home || ".";
  } catch {
    return ".";
  }
}

/**
 * Pick which terminal becomes active after `removedIdx` is removed from a list
 * of `length` items. Returns the index INTO THE NEW (shortened) list, or null if
 * the list is now empty. Prefers the item that took the removed slot (same
 * index), else the new last item. Pure → unit-testable independent of React.
 */
function nextActiveIndex(removedIdx: number, newLength: number): number | null {
  if (newLength <= 0) return null;
  return Math.min(removedIdx, newLength - 1);
}

/**
 * `useTerminals` — the multi-terminal state machine that backs the sidebar.
 *
 * Owns the ordered list of terminal RECORDS, the active record id, and every
 * mutation (create/close/switch/next/prev/reorder/rename), each wired to the
 * backend record commands. The list is the single source of truth for which
 * `<Terminal>` instances are mounted; the consumer renders one per `alive`
 * record and shows only the active one.
 *
 * On mount it loads existing records (`list_terminals`): if any `alive` records
 * exist they are adopted (so a reload keeps the same terminals), and each
 * record's persisted `scrollback` is restored — the consumer passes it down as
 * `deadHistory` (see `TerminalDeck` → `<Terminal deadHistory>`), which reinjects
 * it as read-only dead history (+ a visual separator) while a fresh shell spawns
 * at the record's `cwd`. If no `alive` record exists, one default terminal is
 * created so the app always opens with a usable shell.
 */
export function useTerminals(): UseTerminals {
  const [terminals, setTerminals] = useState<TerminalRecord[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // A live mirror of `terminals` for synchronous reads inside callbacks that must
  // decide BEFORE scheduling a state update (e.g. `markRead` deciding whether a
  // backend round-trip is needed). Kept in sync on every commit.
  const terminalsRef = useRef<TerminalRecord[]>(terminals);
  terminalsRef.current = terminals;

  // Guard against React.StrictMode double-mount creating two default terminals:
  // the bootstrap runs at most once per real mount.
  const bootstrapped = useRef(false);

  const create = useCallback(async (cwd?: string) => {
    const resolved = cwd ?? (await resolveDefaultCwd());
    const row = await invoke<TerminalRecord>("create_terminal", {
      cwd: resolved,
      label: null,
    });
    setTerminals((prev) => [...prev, row]);
    setActiveId(row.id);
    return row;
  }, []);

  const attach = useCallback(
    async (id: string, workspaceId: string, mode: "auto" | "manual" = "manual") => {
      // Reflect the binding locally so the spine groups it immediately; persist
      // it via the Phase-1 backend command (failure leaves the UI grouping in
      // place, corrected by the next list/auto-attach pass).
      setTerminals((prev) =>
        prev.map((t) =>
          t.id === id ? { ...t, workspace_id: workspaceId, workspace_binding_mode: mode } : t,
        ),
      );
      await invoke("attach_terminal", {
        terminalId: id,
        workspaceId,
        mode,
      }).catch(() => {});
    },
    [],
  );

  const autoAttach = useCallback(async (id: string, cwd: string | null) => {
    const res = await invoke<{
      workspace_id: string | null;
      changed: boolean;
    }>("auto_attach_terminal", { terminalId: id, cwd }).catch(() => null);
    if (res?.changed && res.workspace_id) {
      // Reflect the (auto) binding so the spine moves it under the workspace.
      setTerminals((prev) =>
        prev.map((t) =>
          t.id === id
            ? {
                ...t,
                workspace_id: res.workspace_id,
                workspace_binding_mode: "auto",
              }
            : t,
        ),
      );
    }
    return res?.changed ?? false;
  }, []);

  const detachFromWorkspaces = useCallback((workspaceIds: string[]) => {
    if (workspaceIds.length === 0) return;
    const targets = new Set(workspaceIds);
    setTerminals((prev) => {
      // Only allocate a new array if at least one terminal actually detaches.
      if (!prev.some((t) => t.workspace_id && targets.has(t.workspace_id))) return prev;
      return prev.map((t) =>
        t.workspace_id && targets.has(t.workspace_id)
          ? { ...t, workspace_id: null, workspace_binding_mode: "auto" }
          : t,
      );
    });
  }, []);

  useEffect(() => {
    // The `bootstrapped` ref already guarantees this runs EXACTLY ONCE for the
    // component's lifetime — including across React StrictMode's dev-only
    // mount→unmount→remount, since refs survive it. So we must NOT discard the
    // result on the StrictMode cleanup: a prior version used a `cancelled` flag
    // set in cleanup, which (StrictMode: run1 starts → cleanup cancels run1 →
    // run2 bails on the guard) meant NEITHER run adopted/created → an empty app
    // in `tauri dev` (the bug only shows in dev; prod StrictMode is inert).
    if (bootstrapped.current) return;
    bootstrapped.current = true;

    void (async () => {
      try {
        const rows = await invoke<TerminalRecord[]>("list_terminals");
        const alive = rows.filter((r) => r.status === "alive");
        if (alive.length > 0) {
          // Adopt the existing alive records as-is, each still carrying its
          // persisted `scrollback`; `TerminalDeck` feeds that to `<Terminal
          // deadHistory>` so the prior output is restored as read-only dead
          // history (a fresh shell is spawned at the record's cwd).
          setTerminals(alive);
          // Reopen on the terminal that was active LAST (greatest
          // last_active_at), so a relaunch returns to where the user left off.
          // Fall back to the first if none was ever recorded active.
          const restored = alive.reduce(
            (best, r) => ((r.last_active_at ?? 0) > (best.last_active_at ?? 0) ? r : best),
            alive[0],
          );
          setActiveId(restored.id);
        } else {
          await create();
        }
      } catch {
        // `list_terminals` failed (transient IPC/DB error). The bootstrap guard
        // (`bootstrapped`) prevents a retry, so without a fallback the app would
        // stay permanently empty — open a default terminal instead so there is
        // always a usable shell.
        await create().catch(() => {});
      } finally {
        setLoading(false);
      }
    })();
  }, [create]);

  // Persist the active terminal (stamp `last_active_at`) whenever it changes, so
  // a relaunch reopens on it (see the bootstrap restore above). Fire-and-forget:
  // a failure just means the next launch falls back to the first terminal.
  useEffect(() => {
    if (!activeId) return;
    void invoke("set_active", { id: activeId }).catch(() => {});
  }, [activeId]);

  // Live exec-state (PRD-2.1): fold each backend `terminal://exec-state`
  // transition onto its record WITHOUT a full re-list, so the sidebar badge
  // updates immediately. Keyed by `terminal_id`; an event for a record we do not
  // hold (e.g. a closed one) is ignored. The listener subscribes ONCE (no
  // `terminals`/`activeId` deps → no churn) and uses the functional updater. The
  // backend is the authority for `unread`; the consumer owns "mark read while the
  // terminal is active" (it knows the focused terminal) by calling `markRead`.
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void listen<TerminalExecStatePayload>("terminal://exec-state", (event) => {
      if (torndown) return;
      const { terminal_id, state, exit_code, unread } = event.payload;
      setTerminals((prev) => {
        if (!prev.some((t) => t.id === terminal_id)) return prev;
        return prev.map((t) =>
          t.id === terminal_id
            ? { ...t, exec_state: state, exec_exit_code: exit_code, exec_state_unread: unread }
            : t,
        );
      });
    }).then((un) => {
      if (torndown) {
        void Promise.resolve(un()).catch(() => {});
        return;
      }
      unlisten = un;
    });
    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, []);

  const close = useCallback(async (id: string) => {
    setTerminals((prev) => {
      const idx = prev.findIndex((t) => t.id === id);
      if (idx === -1) return prev;
      const next = prev.filter((t) => t.id !== id);
      // Re-target the active terminal if we closed it.
      setActiveId((active) => {
        if (active !== id) return active;
        const ni = nextActiveIndex(idx, next.length);
        return ni === null ? null : next[ni].id;
      });
      return next;
    });
    // Persist the closed status so it is not re-spawned at launch. Failures are
    // swallowed: the UI already dropped it; a stale `alive` row is corrected by
    // the next reorder/list and is harmless within this phase.
    await invoke("close_terminal", { id }).catch(() => {});
  }, []);

  const setActive = useCallback((id: string) => setActiveId(id), []);

  const step = useCallback((delta: number) => {
    setTerminals((prev) => {
      if (prev.length === 0) return prev;
      setActiveId((active) => {
        const idx = prev.findIndex((t) => t.id === active);
        // From an unknown active, step from the start/end.
        const base = idx === -1 ? (delta > 0 ? -1 : 0) : idx;
        const nextIdx = (base + delta + prev.length) % prev.length;
        return prev[nextIdx].id;
      });
      return prev;
    });
  }, []);

  const activeNext = useCallback(() => step(1), [step]);
  const activePrev = useCallback(() => step(-1), [step]);

  const reorder = useCallback(async (ids: string[]) => {
    setTerminals((prev) => {
      const byId = new Map(prev.map((t) => [t.id, t]));
      const reordered = ids
        .map((id) => byId.get(id))
        .filter((t): t is TerminalRecord => t !== undefined)
        .map((t, idx) => ({ ...t, order_index: idx }));
      // Keep any records not present in `ids` appended (defensive; normally all).
      const missing = prev.filter((t) => !ids.includes(t.id));
      return [...reordered, ...missing];
    });
    await invoke("reorder", { ids }).catch(() => {});
  }, []);

  const rename = useCallback(async (id: string, label: string | null) => {
    setTerminals((prev) => prev.map((t) => (t.id === id ? { ...t, label } : t)));
    await invoke("rename", { id, label }).catch(() => {});
  }, []);

  // Mark a terminal's settled result READ: clear the local unread flag at once
  // (the settled badge disappears immediately) and persist via the backend.
  // PRESERVES `exec_state`/`exec_exit_code` — only the unread bit clears. Skips
  // the work (and the round-trip) when the terminal is already read. The decision
  // reads the live `terminalsRef` synchronously (not a side-effect in the state
  // updater), so the persist fires deterministically.
  const markRead = useCallback((id: string) => {
    if (!terminalsRef.current.some((t) => t.id === id && t.exec_state_unread)) return;
    setTerminals((prev) =>
      prev.map((t) => (t.id === id ? { ...t, exec_state_unread: false } : t)),
    );
    void invoke("terminal_exec_mark_read", { id }).catch(() => {});
  }, []);

  return {
    terminals,
    activeId,
    loading,
    create,
    attach,
    autoAttach,
    detachFromWorkspaces,
    close,
    setActive,
    activeNext,
    activePrev,
    reorder,
    rename,
    markRead,
  };
}
