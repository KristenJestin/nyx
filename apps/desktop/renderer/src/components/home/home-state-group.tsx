import { cn } from "@/lib/utils";

import type { HomeSessionState } from "./home-state";

/**
 * The colour a state's WORD wears in its group head. It is the same semantic token family the dot
 * of that state uses (`<StatusDot>`), read as text — which is the whole link between the word and
 * the dots under it, and the reason nothing else has to repeat the state on a row.
 */
const GROUP_TONE: Readonly<Record<HomeSessionState, string>> = {
  waiting: "text-warning",
  error: "text-destructive",
  ready: "text-success",
  working: "text-info",
  idle: "text-muted-foreground",
};

/**
 * `<HomeStateGroup>` — the state WORD at the head of its run of rows, with the count, and a
 * hairline running out to the panel's edge.
 *
 * This is the elected design (prototype round v8→v11, `le mot idle en tête de son groupe` — an
 * explicit user decision the later rounds carried forward untouched). It is a LEGEND over a single
 * continuous list, not a container: the rows are not nested under it, the list is still ONE flow in
 * ONE sort, and no row carries a state word or badge of its own — the group head is where the word
 * is said, once.
 */
export function HomeStateGroup({ state, count }: { state: HomeSessionState; count: number }) {
  return (
    <div
      data-testid={`home-state-group-${state}`}
      className={cn(
        "flex items-center gap-2 px-2.5 pt-2 pb-1 font-mono text-2xs tracking-wider uppercase",
        GROUP_TONE[state],
      )}
    >
      <span>{state}</span>
      <span className="opacity-75">{count}</span>
      <span aria-hidden className="h-px flex-1 bg-border/45" />
    </div>
  );
}
