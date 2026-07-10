import { createContext, type ReactNode, useCallback, useContext, useMemo, useState } from "react";

import { saveLastProjectId } from "./ui-persistence";

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
  const [activeProjectId, setActiveProjectIdState] = useState<string | null>(initialProjectId);
  // Every entered-project change is REMEMBERED (renderer localStorage) so a relaunch — or a
  // return to Terminals — restores the LAST project the user was in, not the first. The rail
  // restore (`<BaseLayout>`) reads this hint back and honours it only when the project still
  // exists (a deleted project's stale id falls back cleanly to the first).
  const setActiveProjectId = useCallback((id: string | null) => {
    setActiveProjectIdState(id);
    saveLastProjectId(id);
  }, []);
  const value = useMemo<ActiveProjectContextValue>(
    () => ({ activeProjectId, setActiveProjectId }),
    [activeProjectId, setActiveProjectId],
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
