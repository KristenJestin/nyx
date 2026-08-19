import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * `<PagePlaceholder>` — the shared empty-state a routed page renders before its real
 * content is ported: a centred title + a short muted line (and an optional icon). Used
 * by the Work / Knowledge stub pages so every "coming later" page reads identically.
 *
 * Every value is a design-system token / the token-backed Tailwind scale — no raw hex,
 * no magic px.
 */
export function PagePlaceholder({
  title,
  description,
  icon,
  className,
}: {
  title: string;
  description: string;
  icon?: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex h-app w-full flex-col items-center justify-center gap-3 bg-background px-6 text-center",
        className,
      )}
    >
      {icon != null ? <div className="text-muted-foreground/60">{icon}</div> : null}
      <h1 className="font-semibold text-lg text-foreground">{title}</h1>
      <p className="max-w-md text-sm text-muted-foreground">{description}</p>
    </div>
  );
}
