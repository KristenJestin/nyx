import type { ReactNode } from "react";

import { cn } from "@/lib/utils";
import { CountBadge, EmptyHint, MicroLabel } from "./tree-primitives";

/**
 * `<RuntimeSection>` — one always-open group of a workspace's runtime tree (Terminals,
 * Commands). A dense header (micro label + trailing count pill) over its rows. There is NO
 * nested accordion here: inside a workspace the runtime is a flat, glanceable list, so every
 * section stays expanded. Chrome only — the rows are handed in. Tokens / the token-backed
 * scale only, no raw hex.
 */
export function RuntimeSection({
  title,
  count,
  children,
  className,
  actions,
}: {
  title: ReactNode;
  count: number;
  children: ReactNode;
  className?: string;
  /** Optional trailing action cluster in the header (e.g. the + new terminal, the gear). */
  actions?: ReactNode;
}) {
  return (
    <div className={cn("mt-3 first:mt-0", className)}>
      <div className="flex items-center gap-2 px-3 pb-1">
        <MicroLabel>{title}</MicroLabel>
        <CountBadge className="ml-auto">{count}</CountBadge>
        {actions != null ? <span className="flex items-center gap-0.5">{actions}</span> : null}
      </div>
      {children}
    </div>
  );
}

/**
 * `<RuntimeSectionList>` — the rows of a section, or the muted empty hint when the section is
 * empty. Rows are laid on a tight vertical rhythm; the empty state reuses `<EmptyHint>`.
 */
export function RuntimeSectionList({ rows, empty }: { rows: readonly ReactNode[]; empty: string }) {
  if (rows.length === 0) {
    return <EmptyHint>{empty}</EmptyHint>;
  }
  return <div className="flex flex-col gap-0.5 px-2">{rows}</div>;
}
