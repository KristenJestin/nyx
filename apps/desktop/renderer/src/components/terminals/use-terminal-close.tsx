import { createContext, useContext, type ReactNode } from "react";

/**
 * CLOSE A TERMINAL FROM ITS ROW (v2.5.6, task `01KXBNX51EPQ69MVKYG4W960HC`). Until now a terminal
 * could only be closed with the Ctrl+W shortcut (`use-terminal-actions.closeSelected`), which
 * always targets the SELECTED terminal — there was no per-row affordance. This context hands a
 * `close(id)` down to every runtime terminal ROW so its hover "×" closes the RIGHT terminal (by
 * id), whether it is the selected one or not.
 *
 * A terminal ROW sits deep in the project → workspace → tree → row chain (and the loose-terminals
 * band renders the SAME row), so — exactly like the rename callback ([`use-terminal-rename`]) and
 * the stats map ([`use-terminal-stats`]) — the close callback flows via CONTEXT rather than
 * prop-drilling through every intermediate component.
 */

/** Close the terminal RECORD `id` (drops it and persists `closed`). */
export type CloseTerminal = (id: string) => void;

/**
 * Context carrying the close callback so any terminal ROW can close ITSELF (by id) without
 * prop-drilling. The default is a no-op so a row rendered OUTSIDE the provider (an isolation test)
 * simply cannot close — it never throws.
 */
const TerminalCloseContext = createContext<CloseTerminal>(() => {});

/** Provider that supplies the per-row close callback to the runtime terminal rows. Mount once
 *  around the Terminals page tree + loose band (alongside the rename/stats providers). */
export function TerminalCloseProvider({
  close,
  children,
}: {
  close: CloseTerminal;
  children: ReactNode;
}) {
  return <TerminalCloseContext.Provider value={close}>{children}</TerminalCloseContext.Provider>;
}

/** Read the close callback from the shared context. Returns a no-op outside a provider, so a row
 *  is always safe to render in isolation. */
export function useCloseTerminal(): CloseTerminal {
  return useContext(TerminalCloseContext);
}
