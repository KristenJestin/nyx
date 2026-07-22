import { KeyIcon, LockIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";

/**
 * `<DependencyBadges>` — the Spec↔Spec dependency badges (PRD "badges bloquée par N / débloque M"),
 * shared by the board card AND the Spec-detail header so both read identically. "Blocked by N" (the
 * directional prerequisites — a caution, `warning`) and "Unblocks M" (the directional dependents —
 * informational, `info`). Each renders ONLY when its count is > 0; when both are 0 the component is
 * absent (no empty chrome). Composes the existing `<Badge>` primitive + tokens — no raw colour.
 */
export function DependencyBadges({
  blockedBy,
  unblocks,
  className,
}: {
  blockedBy: number;
  unblocks: number;
  className?: string;
}) {
  if (blockedBy <= 0 && unblocks <= 0) return null;
  return (
    <span className={cn("inline-flex flex-wrap items-center gap-1", className)}>
      {blockedBy > 0 ? (
        <Badge variant="warning" title={`Blocked by ${blockedBy} spec${blockedBy > 1 ? "s" : ""}`}>
          <LockIcon />
          Blocked by {blockedBy}
        </Badge>
      ) : null}
      {unblocks > 0 ? (
        <Badge variant="info" title={`Unblocks ${unblocks} spec${unblocks > 1 ? "s" : ""}`}>
          <KeyIcon />
          Unblocks {unblocks}
        </Badge>
      ) : null}
    </span>
  );
}
