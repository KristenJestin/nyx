import { ArrowUpRightIcon, MessagesSquareIcon } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";

import { Button } from "@/components/ui/button";

import type { Spec } from "../specs/types";

/**
 * `<OverlayPanelActions>` — the status-aware QUICK ACTIONS rendered in the OPEN overlay panel's
 * HEADER. It is dropped into BOTH open headers (the creation tiroir `<SpecLivePanel>` and the dev
 * rail `<SpecRailOverlay>` panel), sitting between the status badge and the close chevron, so the
 * two headers do NOT diverge. Two jumps to the full-page Spec surfaces (the overlay lives on the
 * Terminals route `/`, so these navigations LEAVE that page on purpose — that is the point):
 *
 *  - "Open the Spec" — ALWAYS shown (any status). A DISCREET ghost icon-button that jumps to the
 *    Spec detail (`/work/$specId`).
 *  - "Open the review" — shown ONLY when `spec.status === "in_review"`. A slightly MORE VISIBLE
 *    labelled button ("Review") that jumps to the full-screen review page (`/work/$specId/review`,
 *    which re-derives the Spec's review context automatically — no route search-param pre-selection
 *    yet, that is a later iteration).
 *
 * Navigation uses the `@tanstack/react-router` `useNavigate` idiom (same as `runtime-tab` /
 * `base-layout`). `e.stopPropagation()` on each click keeps a header/languette toggle handler from
 * firing — an action must NEVER close/toggle the panel; it only navigates.
 */
export function OverlayPanelActions({ spec }: { spec: Spec }) {
  const navigate = useNavigate();

  return (
    <>
      <Button
        variant="ghost"
        size="icon-xs"
        aria-label="Open the Spec"
        onClick={(e) => {
          e.stopPropagation();
          void navigate({ to: "/work/$specId", params: { specId: spec.id } });
        }}
      >
        <ArrowUpRightIcon />
      </Button>
      {spec.status === "in_review" ? (
        <Button
          variant="outline"
          size="xs"
          aria-label="Open the review"
          onClick={(e) => {
            e.stopPropagation();
            void navigate({ to: "/work/$specId/review", params: { specId: spec.id } });
          }}
        >
          <MessagesSquareIcon />
          Review
        </Button>
      ) : null}
    </>
  );
}
