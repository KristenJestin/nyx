import { createContext, useContext, type ReactNode } from "react";

/**
 * RENAME A TERMINAL (FEEDBACK #30). A terminal's displayed name is normally
 * AUTO-derived (cwd basename + foreground program, computed live at display — see
 * `auto-label.ts`). This context lets the user pin a MANUAL name that always wins:
 * `resolveDisplayName` returns a non-blank `record.label` verbatim, and the
 * auto-naming is only ever a DISPLAY-TIME fallback (it is never persisted into
 * `terminals.label`), so a manual rename can never be clobbered by the live
 * `terminal_info` poll.
 *
 * The rename callback lives at the sidebar root (`useTerminals().rename`, which
 * optimistically updates the record and persists via the `rename` IPC →
 * `terminals.label`). A terminal ROW sits at the bottom of the
 * project → workspace → list → item chain, so we hand it down via CONTEXT exactly
 * like the agent-sessions and per-terminal stats maps — no prop-drilling through
 * every intermediate component.
 */

/**
 * Set or clear a terminal's manual label. A trimmed non-empty `label` pins a
 * manual name (wins over the auto label); `null` clears it back to auto-naming.
 */
export type RenameTerminal = (id: string, label: string | null) => void;

/**
 * Context carrying the rename callback so any terminal ROW can rename itself
 * WITHOUT prop-drilling. The default is a no-op so a row rendered OUTSIDE the
 * provider (e.g. an isolation test that does not exercise rename) simply cannot
 * rename — it never throws.
 */
const TerminalRenameContext = createContext<RenameTerminal>(() => {});

/** Provider that supplies the rename callback to the sidebar's terminal rows.
 *  Mount once around the sidebar (alongside `AgentSessionsProvider`). */
export function TerminalRenameProvider({
  rename,
  children,
}: {
  rename: RenameTerminal;
  children: ReactNode;
}) {
  return <TerminalRenameContext.Provider value={rename}>{children}</TerminalRenameContext.Provider>;
}

/** Read the rename callback from the shared context. Returns a no-op when used
 *  outside a provider, so a row is always safe to render in isolation. */
export function useRenameTerminal(): RenameTerminal {
  return useContext(TerminalRenameContext);
}
