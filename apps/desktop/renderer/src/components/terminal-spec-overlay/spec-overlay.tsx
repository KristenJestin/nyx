import { AnimatePresence } from "motion/react";

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
  const { session, specs } = useTerminalSpecs(terminalId);

  // Resolve the sub-overlay (or `null` for none). `null` ⇒ AnimatePresence exits the previous one.
  const content = resolveOverlay(session, specs);

  return <AnimatePresence>{content}</AnimatePresence>;
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
