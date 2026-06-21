import { useEffect, useRef, useState } from "react";
import { nyxBridge } from "@/bridge";

import type { TerminalRecord } from "./use-terminals";

/**
 * Auto-naming for the sidebar: make each terminal's name readable WITHOUT manual
 * work, from its working directory and the program currently in the foreground —
 * both read live from the backend `terminal_info` (Linux `/proc`). A manual
 * rename always wins (see `resolveDisplayName`).
 */

/** Live introspection of a terminal, mirroring the backend `TerminalInfo`. */
export interface TerminalInfo {
  cwd: string | null;
  foreground: string | null;
}

/** The login shells whose presence in the foreground means "no program is running". */
const SHELL_COMMS = new Set([
  "bash",
  "zsh",
  "sh",
  "fish",
  "dash",
  "ash",
  "ksh",
  "tcsh",
  "csh",
  "nu",
  "elvish",
  "xonsh",
]);

/**
 * Whether `comm` names an interactive shell (so we should NOT surface it as a
 * "running program"). A leading `-` (login shell, e.g. `-bash`) is stripped
 * first. Pure.
 */
export function isShellComm(comm: string | null | undefined): boolean {
  if (!comm) return false;
  const c = comm.startsWith("-") ? comm.slice(1) : comm;
  return SHELL_COMMS.has(c);
}

/** The last path segment of `cwd` (its basename), or null if there is none. */
function basename(cwd: string | null | undefined): string | null {
  if (!cwd) return null;
  // Split on BOTH POSIX (`/`) and Windows (`\`) separators so a Windows cwd
  // (`C:\Users\kris\work`) yields the folder name (`work`), not the whole path.
  const parts = cwd.split(/[/\\]+/).filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : null;
}

/**
 * Compute the AUTO label from a live cwd + foreground program:
 *  - `basename(cwd)` when only the shell is in the foreground (`projetA`),
 *  - `basename(cwd) · <program>` when a real program runs (`projetA · htop`),
 *  - the program alone when the cwd is unusable (`vim`),
 *  - `null` when there is nothing to name (no cwd, only the shell / nothing).
 *
 * Pure → unit-tested. The caller (sidebar) uses it only as a FALLBACK under a
 * manual label (see `resolveDisplayName`).
 */
export function autoLabel(
  cwd: string | null | undefined,
  foreground: string | null | undefined,
): string | null {
  const dir = basename(cwd);
  const program = foreground && !isShellComm(foreground) ? foreground : null;

  if (dir && program) return `${dir} · ${program}`;
  if (dir) return dir;
  if (program) return program;
  return null;
}

/**
 * Resolve the name to DISPLAY for a terminal, in strict precedence order:
 *  1. a non-blank MANUAL `label` (rename) — always wins, persisted,
 *  2. the live `auto` label (cwd + foreground) when available,
 *  3. the cwd basename,
 *  4. a numbered fallback (`Terminal <n>`).
 *
 * Pure → unit-tested. This is the single authority the sidebar item renders.
 */
export function resolveDisplayName(
  record: TerminalRecord,
  index: number,
  auto: string | null,
): string {
  if (record.label && record.label.trim()) return record.label;
  if (auto && auto.trim()) return auto;
  const base = basename(record.cwd);
  if (base) return base;
  return `Terminal ${index + 1}`;
}

/** How often the sidebar re-reads `terminal_info` (the backend itself debounces
 * the underlying `/proc` syscalls to ~1s, so this poll never hammers the OS). */
const AUTO_LABEL_POLL_MS = 1000;

/**
 * Poll the backend `terminal_info(ptyId)` on a timer and return the live auto
 * label, recomputed only when the cwd/foreground actually changes (DEBOUNCED by
 * the backend cache + this fixed poll cadence — never per output byte). Returns
 * `null` until a reading is available or while there is no PTY id yet.
 *
 * `poll` is injectable so the hook is exercised in jsdom with a mocked
 * `terminal_info` and fake timers, independent of a real `/proc`.
 */
export function useAutoLabel(
  ptyId: number | null,
  options: {
    poll?: (ptyId: number) => Promise<TerminalInfo>;
    pollMs?: number;
  } = {},
): string | null {
  const { poll = defaultPoll, pollMs = AUTO_LABEL_POLL_MS } = options;
  const [auto, setAuto] = useState<string | null>(null);
  // Remember the last computed label so an unchanged reading does not churn state.
  const lastRef = useRef<string | null>(null);

  useEffect(() => {
    if (ptyId === null) {
      setAuto(null);
      lastRef.current = null;
      return;
    }
    let cancelled = false;

    const tick = async () => {
      try {
        const info = await poll(ptyId);
        if (cancelled) return;
        const next = autoLabel(info.cwd, info.foreground);
        // Only push state when the label actually changed (debounced recompute).
        if (next !== lastRef.current) {
          lastRef.current = next;
          setAuto(next);
        }
      } catch {
        // terminal_info can fail (pty gone / non-Linux): keep the last label.
      }
    };

    // Prime immediately, then poll on the cadence.
    void tick();
    const timer = setInterval(() => void tick(), pollMs);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [ptyId, poll, pollMs]);

  return auto;
}

/** Default poll: the backend `terminal_info` command keyed by live PTY id. */
function defaultPoll(ptyId: number): Promise<TerminalInfo> {
  return nyxBridge.invoke<TerminalInfo>("terminal_info", { id: ptyId });
}

/**
 * The short SHELL/PROGRAM suffix for the proto-aligned terminal row (the "· zsh"
 * in `web · zsh`, finding 01KV1NVQPT2Z84KKZHBXGNPMSN). It is the live foreground
 * program when a real one runs (e.g. `htop`), otherwise the interactive shell
 * name (e.g. `zsh`, with any login-shell leading `-` stripped). `null` until a
 * reading is available. Pure given `info`.
 */
export function shellSuffix(info: { foreground: string | null } | null | undefined): string | null {
  const fg = info?.foreground;
  if (!fg) return null;
  // Strip a login-shell leading `-` (`-zsh` → `zsh`) for a clean suffix.
  return fg.startsWith("-") ? fg.slice(1) : fg;
}

/**
 * Poll `terminal_info(ptyId)` and return the short shell/program SUFFIX for the
 * proto row (see {@link shellSuffix}). Shares the same backend command + cadence
 * as {@link useAutoLabel}; the backend debounces the underlying `/proc` reads, so
 * the two polls together still never hammer the OS. `poll` is injectable for
 * jsdom tests. Returns `null` while there is no PTY id / reading yet.
 */
export function useShellSuffix(
  ptyId: number | null,
  options: {
    poll?: (ptyId: number) => Promise<TerminalInfo>;
    pollMs?: number;
  } = {},
): string | null {
  const { poll = defaultPoll, pollMs = AUTO_LABEL_POLL_MS } = options;
  const [suffix, setSuffix] = useState<string | null>(null);
  const lastRef = useRef<string | null>(null);

  useEffect(() => {
    if (ptyId === null) {
      setSuffix(null);
      lastRef.current = null;
      return;
    }
    let cancelled = false;
    const tick = async () => {
      try {
        const info = await poll(ptyId);
        if (cancelled) return;
        const next = shellSuffix(info);
        if (next !== lastRef.current) {
          lastRef.current = next;
          setSuffix(next);
        }
      } catch {
        // keep the last suffix on a transient terminal_info failure.
      }
    };
    void tick();
    const timer = setInterval(() => void tick(), pollMs);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [ptyId, poll, pollMs]);

  return suffix;
}
