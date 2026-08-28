import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * THE DETAIL PANEL'S CHROME — the head, the subtitle and the rule, written once for the five things
 * the right-hand panel can be (nothing, a command, a folder, the import, a creation).
 *
 * It exists because the panel's whole promise is that the five READ THE SAME: the eye should land
 * on the title, the one-line statement of what is being edited, and the destructive action always in
 * the same corner. Five panels each spelling their own header is exactly how that promise decays —
 * it is the same failure, one scale down, as the two lists this screen replaced.
 */
export function DetailHeader({
  icon,
  title,
  subtitle,
  action,
}: {
  /** The family glyph, at the panel's title size. */
  icon: ReactNode;
  title: ReactNode;
  /** The one line that says WHAT is being edited — a position, a member count, a source. */
  subtitle?: ReactNode;
  /** The panel's own action, at the far end: a deletion, a dissolution. */
  action?: ReactNode;
}) {
  return (
    <header className={cn("mb-4 flex flex-col gap-1")}>
      <div className="flex min-w-0 items-center gap-2.5">
        <span className="flex shrink-0 items-center text-muted-foreground">{icon}</span>
        <h3 className="min-w-0 flex-1 truncate text-base font-semibold">{title}</h3>
        {action == null ? null : <div className="shrink-0">{action}</div>}
      </div>
      {subtitle == null ? null : (
        <p data-detail-subtitle className="text-xs text-muted-foreground">
          {subtitle}
        </p>
      )}
    </header>
  );
}

/** The rule between a panel's fields and what it says ABOUT them (provenance, layout). */
export function DetailSeparator({ className }: { className?: string }) {
  return <hr className={cn("my-4 border-border", className)} />;
}
