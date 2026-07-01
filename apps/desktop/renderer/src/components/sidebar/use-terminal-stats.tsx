import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { nyxBridge } from "@/bridge";

/**
 * PER-TERMINAL CPU%/RAM (FEEDBACK #28). The backend's per-terminal stats poll samples
 * each live terminal's PROCESS TREE (the shell + every descendant — `claude`, `npm`,
 * `cargo`, …) and pushes a `terminal://stats` event keyed by the persistent terminal
 * record id. This hook mirrors `use-agent-sessions.tsx`: ONE subscription near the
 * sidebar root folds every reading into a `terminalId → {cpuPct, memBytes}` map the rows
 * read via context (no prop-drilling).
 *
 * Unlike the agent-sessions hook there is no initial `invoke` pull — stats are a pure
 * event stream (a terminal with no live PTY simply has no entry), so the map starts empty
 * and fills as the poll ticks (~1.5s cadence). A reading for a terminal with no row is
 * harmless (the row just reads its own key).
 */

/** The raw `terminal://stats` wire shape (snake_case, parity with the other
 * `terminal://*` events the renderer's raw subscribers read). */
interface TerminalStatsPayload {
  terminal_id: string;
  cpu_pct: number;
  mem_bytes: number;
}

/** One terminal's live resource reading the row consumes. */
export interface TerminalStats {
  /** Summed CPU% of the process tree (per single core, so it can exceed 100). */
  cpuPct: number;
  /** Summed resident memory of the process tree, in bytes. */
  memBytes: number;
}

const EMPTY_MAP: ReadonlyMap<string, TerminalStats> = new Map();

/**
 * Live per-terminal stats: subscribe to `terminal://stats` once and fold each reading
 * onto the keyed map. StrictMode-safe (the listener is torn down on cleanup; a late
 * `subscribe` resolve after unmount is unlistened immediately). The map is replaced
 * (new reference) on each reading so context consumers re-render.
 */
export function useTerminalStats(): ReadonlyMap<string, TerminalStats> {
  const [byId, setById] = useState<ReadonlyMap<string, TerminalStats>>(EMPTY_MAP);

  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;

    void nyxBridge
      .subscribe<TerminalStatsPayload>("terminal://stats", (p) => {
        if (torndown || !p || typeof p.terminal_id !== "string") return;
        setById((prev) => {
          const next = new Map(prev);
          next.set(p.terminal_id, { cpuPct: p.cpu_pct, memBytes: p.mem_bytes });
          return next;
        });
      })
      .then((un) => {
        if (torndown) {
          void Promise.resolve(un()).catch(() => {});
          return;
        }
        unlisten = un;
      })
      // A missing event channel (a test/host without the bridge) just means no live
      // stats; swallow so it never becomes an unhandled rejection.
      .catch(() => {});

    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, []);

  return byId;
}

/**
 * Context carrying the live stats map so every terminal ROW reads its CPU%/RAM WITHOUT
 * prop-drilling. A SINGLE [`TerminalStatsProvider`] near the sidebar root holds the one
 * subscription; rows read it cheaply. The default is the empty map so a row rendered
 * OUTSIDE the provider (e.g. an isolation test) simply shows no indicator — never throws.
 */
const TerminalStatsContext = createContext<ReadonlyMap<string, TerminalStats>>(EMPTY_MAP);

/** Provider that owns the ONE `terminal://stats` subscription. Mount once around the
 * sidebar (alongside `AgentSessionsProvider`). */
export function TerminalStatsProvider({ children }: { children: ReactNode }) {
  const byId = useTerminalStats();
  return <TerminalStatsContext.Provider value={byId}>{children}</TerminalStatsContext.Provider>;
}

/** Read the live stats for ONE terminal from the shared context, or `null` when none
 * have arrived yet. Total + side-effect-free: a row outside the provider reads `null`. */
export function useTerminalStatsFor(terminalId: string): TerminalStats | null {
  return useContext(TerminalStatsContext).get(terminalId) ?? null;
}

// ---------------------------------------------------------------------------
// Formatters (FEEDBACK #28) — a compact, MUTED indicator: `1.2% · 340 MB`.
// ---------------------------------------------------------------------------

/**
 * Format a CPU percentage compactly: one decimal below 10% (`1.2%`), whole numbers above
 * (`42%`, `380%`). A negative/NaN value clamps to `0%`. The tree sum can exceed 100% on a
 * multi-core build; we show it verbatim (it is informative, not a bug).
 */
export function formatCpuPct(cpuPct: number): string {
  if (!Number.isFinite(cpuPct) || cpuPct <= 0) return "0%";
  if (cpuPct < 10) return `${(Math.round(cpuPct * 10) / 10).toFixed(1)}%`;
  return `${Math.round(cpuPct)}%`;
}

/**
 * Format a byte count human-readably: `0 B`, `340 MB`, `1.2 GB`. Uses 1024-based units
 * but the `MB`/`GB` labels (the conventional task-manager display). Below 1 KiB shows
 * bytes; KB/MB show no decimals; GB+ show one decimal. A negative/NaN value → `0 B`.
 */
export function formatBytes(memBytes: number): string {
  if (!Number.isFinite(memBytes) || memBytes <= 0) return "0 B";
  const KIB = 1024;
  const MIB = KIB * 1024;
  const GIB = MIB * 1024;
  if (memBytes < KIB) return `${Math.round(memBytes)} B`;
  if (memBytes < MIB) return `${Math.round(memBytes / KIB)} KB`;
  if (memBytes < GIB) return `${Math.round(memBytes / MIB)} MB`;
  return `${(Math.round((memBytes / GIB) * 10) / 10).toFixed(1)} GB`;
}

/** The compact combined indicator string a row shows on hover: `1.2% · 340 MB`. */
export function formatTerminalStats(stats: TerminalStats): string {
  return `${formatCpuPct(stats.cpuPct)} · ${formatBytes(stats.memBytes)}`;
}
