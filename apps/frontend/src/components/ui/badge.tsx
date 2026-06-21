import { cva, type VariantProps } from "class-variance-authority";
import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * `Badge` — a tiny pill label in the shadcn/cva style used across the commands
 * modal: the source provenance reference (`scripts.build`), the package-manager
 * tag (`bun`), the passive `changed in package.json` drift marker, and the
 * `edited` (detached / customized) marker. Pure presentational element; the
 * semantics (which variant) are the caller's.
 */
export const badgeVariants = cva(
  "inline-flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-[10px] font-medium leading-snug whitespace-nowrap [&_svg]:size-2.5 [&_svg]:opacity-85",
  {
    defaultVariants: { variant: "muted" },
    variants: {
      variant: {
        muted: "border-border bg-muted/40 text-muted-foreground",
        source: "border-border bg-white/[0.04] text-muted-foreground",
        info: "border-info/40 bg-info/10 text-info",
        warning: "border-warning/40 bg-warning/10 text-warning",
        success: "border-success/40 bg-success/10 text-success",
      },
    },
  },
);

export interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>, VariantProps<typeof badgeVariants> {}

export function Badge({ className, variant, ...props }: BadgeProps) {
  return <span className={cn(badgeVariants({ variant }), className)} {...props} />;
}
