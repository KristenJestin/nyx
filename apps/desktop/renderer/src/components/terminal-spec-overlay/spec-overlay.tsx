import { AnimatePresence, LayoutGroup } from "motion/react";

import { useSharedTerminalSpecs } from "./terminal-specs-provider";
import { useTerminalSpecs } from "./use-terminal-specs";
import { creationSpecs, devSpecs, overlayMode } from "./overlay-status";
import { SpecDrawerOverlay } from "./spec-drawer-overlay";
import { SpecRailOverlay } from "./spec-rail-overlay";

/**
 * `<SpecOverlay>` — the terminal↔Spec surcouche mounted over a terminal pane. It resolves the
 * terminal's ACTIVE session + its mapped Spec SET (`useTerminalSpecs`, live over
 * `specs://changed` + `agent-sessions://changed`, NO polling) and picks the overlay:
 *
 *   - a Spec being AUTHORED (draft/ready) ⇒ the CREATION tiroir (B, multi-Spec — task 6/7);
 *   - a Spec under DEV/REVIEW (in_progress/…) ⇒ the advancement RAIL (A — a single Spec);
 *   - no live session, or no Spec that drives either overlay ⇒ NOTHING (non-regression: a
 *     terminal with no mapped Spec renders no overlay, the terminals page is untouched).
 *
 * The component is anchored/pointer-transparent by each sub-overlay's own absolute root, so the
 * terminal underneath stays fully interactive.
 *
 * The chosen sub-overlay is wrapped in an `<AnimatePresence>` so it SLIDES IN + fades when a Spec is
 * mapped (or the mode switches) and SLIDES OUT when the Spec is un-mapped (`/clear`) — review
 * finding "animations apparition/disparition". `motion-safe` is honoured inside each sub-overlay
 * (`useReducedMotion`), and a `null` child (no session / no driving Spec) just runs the exit and
 * leaves nothing behind (non-regression: a terminal with no mapped Spec renders no overlay).
 */
export function SpecOverlay({ terminalId }: { terminalId: string }) {
  // WHERE THE DATA COMES FROM (review finding 01M11WF11C0H8G2MG515FBT6RD). Under a
  // `<TerminalSpecsProvider>` — which is how the tiling mounts it — the resolution is SHARED: one
  // resolver, one `specs://changed` and one `agent-sessions://changed` subscription for every pane
  // on screen, so N languettes cost exactly the subscriptions ONE used to. Without a provider the
  // component resolves its own terminal, exactly as it always did (that is the shape its own specs
  // drive). The `own` hook is called unconditionally — DISABLED with a `null` id under a provider,
  // which is already its documented "no session, no subscription" mode.
  const shared = useSharedTerminalSpecs();
  const own = useTerminalSpecs(shared == null ? terminalId : null);
  const { session, specs } = shared?.get(terminalId) ?? own;

  // Resolve the sub-overlay (or `null` for none). `null` ⇒ AnimatePresence exits the previous one.
  const content = resolveOverlay(session, specs);

  // ONE `<LayoutGroup>` PER TERMINAL. The active languette's accent bar travels on a SHARED
  // `layoutId` (`OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID`), which is global unless it is scoped: with
  // several overlays on screen at once — the whole point of this finding — two open panels would
  // otherwise claim the SAME layout id and Motion would glide one bar between two panes. Namespacing
  // by terminal keeps the FLIP animation exactly as designed *inside* a pane and independent across
  // panes.
  return (
    <LayoutGroup id={terminalId}>
      <AnimatePresence>{content}</AnimatePresence>
    </LayoutGroup>
  );
}

/** Pick the sub-overlay for a terminal's session + Spec set, keyed for AnimatePresence tracking. */
function resolveOverlay(
  session: ReturnType<typeof useTerminalSpecs>["session"],
  specs: ReturnType<typeof useTerminalSpecs>["specs"],
) {
  // No live session ⇒ no overlay. (`terminal_specs` only ever returns an ACTIVE session.)
  if (session === null) return null;

  const mode = overlayMode(specs);
  if (mode === null) return null;

  const live = session.state === "active";

  if (mode === "dev") {
    // Dev = a single Spec (no stacking). The set is ordered most-recently-worked first.
    const spec = devSpecs(specs)[0];
    if (spec === undefined) return null;
    return <SpecRailOverlay key="rail" spec={spec} />;
  }

  // Creation ⇒ the live tiroir. Multi-Spec (a feature cut into N Specs authored in one session)
  // stacks here — and ONLY here (the rail above is always a single Spec).
  return <SpecDrawerOverlay key="drawer" specs={creationSpecs(specs)} live={live} />;
}
