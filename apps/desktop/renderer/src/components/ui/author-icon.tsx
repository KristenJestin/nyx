import { BotIcon, ServerCogIcon, UserIcon } from "lucide-react";

import { cn } from "@/lib/utils";

/** The actor kinds a review author glyph paints (the review feed only uses human/agent). */
export type AuthorKind = "human" | "agent" | "system";

const ICON = {
  agent: BotIcon,
  system: ServerCogIcon,
  human: UserIcon,
} as const;

/**
 * `AuthorIcon` — the bare author/actor glyph, sized to the surrounding text. Ported from the
 * prototype against the v2 design system: the single source for the human/agent/system icons in
 * the review threads (was re-declared inline across the review surfaces).
 */
export function AuthorIcon({ type, className }: { type: AuthorKind; className?: string }) {
  const Icon = ICON[type];
  return <Icon aria-hidden className={cn("size-3.5 shrink-0", className)} />;
}
