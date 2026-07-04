import { createContext, type ReactNode, useContext, useMemo, useState } from "react";

/**
 * The ACTIVE PROJECT — which project the user has entered from the left rail. It is a
 * single shared selection so the rail highlights the entered project AND the pages under
 * the `<Outlet/>` (the Terminals page) render that project's runtime. One enters a
 * PROJECT, never a workspace (PDR-0008): the workspace stays the runtime pivot in the
 * database and appears only as a grouping INSIDE the entered project.
 *
 * Kept as a tiny context (no business logic) so the rail and the routed pages read one
 * source of truth. `activeProjectId` is `null` before any project is entered (or when
 * there are no projects yet).
 */
interface ActiveProjectContextValue {
  readonly activeProjectId: string | null;
  readonly setActiveProjectId: (id: string | null) => void;
}

const ActiveProjectContext = createContext<ActiveProjectContextValue | null>(null);

export function ActiveProjectProvider({
  children,
  initialProjectId = null,
}: {
  children: ReactNode;
  initialProjectId?: string | null;
}) {
  const [activeProjectId, setActiveProjectId] = useState<string | null>(initialProjectId);
  const value = useMemo<ActiveProjectContextValue>(
    () => ({ activeProjectId, setActiveProjectId }),
    [activeProjectId],
  );
  return <ActiveProjectContext.Provider value={value}>{children}</ActiveProjectContext.Provider>;
}

export function useActiveProject(): ActiveProjectContextValue {
  const ctx = useContext(ActiveProjectContext);
  if (!ctx) {
    throw new Error("useActiveProject must be used inside <ActiveProjectProvider>");
  }
  return ctx;
}
