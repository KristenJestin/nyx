import type { ComponentType, ReactNode } from "react";

import type { RunState } from "@/components/ui/status-dot";

import { HomeGlyph } from "./home-glyph";
import type { HomeProject } from "./types";

/**
 * `<HomeLineRow>` — the compact row the SERVICES and TERMINALS panels share: a bare type glyph,
 * a name, the command / path under it, and one optional trailing token.
 *
 * It is the same anatomy as the session row, minus the second information source and the age —
 * a service and a shell have neither. What matters is that it is ONE row for both panels and that
 * the state, when there is one, is said by the glyph's dot and by nothing else. The trailing slot
 * carries FACTS the dot cannot ("`:5173`", "`exit 2`", "last used") — never a word repeating the
 * state.
 */
export function HomeLineRow({
  icon,
  state,
  project,
  name,
  detail,
  trailing,
  onOpen,
  testId,
}: {
  icon: ComponentType<{ className?: string }>;
  /** The row's state, or `null` for a family that carries none. */
  state: RunState | null;
  /** The project mark (cross-project home), or `null`. */
  project: HomeProject | null;
  name: string;
  /** The line under the name — the command line, or the cwd. */
  detail: string;
  /** The trailing fact, when there is one. */
  trailing?: ReactNode;
  onOpen: () => void;
  testId: string;
}) {
  return (
    <button
      type="button"
      onClick={onOpen}
      data-testid={testId}
      className="flex w-full cursor-pointer items-center gap-3 rounded-lg px-2.5 py-2 text-left outline-none transition-colors hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
    >
      <HomeGlyph icon={icon} state={state} project={project} variant="icon" className="ml-1" />
      <span className="flex min-w-0 flex-1 flex-col">
        <span data-testid="home-row-name" className="truncate text-xs text-foreground">
          {name}
        </span>
        <span title={detail} className="truncate font-mono text-2xs text-muted-foreground">
          {detail}
        </span>
      </span>
      {trailing ? (
        <span className="shrink-0 font-mono text-2xs text-muted-foreground">{trailing}</span>
      ) : null}
    </button>
  );
}
