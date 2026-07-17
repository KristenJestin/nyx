import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Spec, SpecStatus } from "../specs/types";
import { OverlayPanelActions } from "./overlay-panel-actions";

// `useNavigate` is stubbed to a shared spy (via `vi.hoisted`, so the factory can reference it) —
// clicking an action must call THIS with the right typed target, and never touch a real router.
const { navigate } = vi.hoisted(() => ({ navigate: vi.fn() }));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => navigate,
}));

/** A minimal `Spec` fixture (only `id` + `status` matter for the actions). */
function spec(overrides: Partial<Spec>): Spec {
  return {
    id: "s1",
    project_id: "p1",
    workspace_id: null,
    title: "A spec",
    slug: null,
    number: 1,
    priority: "med",
    status: "in_progress",
    mode: "stepped",
    phases: "[]",
    current_revision_number: 1,
    position: 0,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
    archived_at: null,
    ...overrides,
  };
}

describe("<OverlayPanelActions> — status-aware header quick actions", () => {
  beforeEach(() => navigate.mockClear());

  it("ALWAYS renders 'Open the Spec' and navigates to /work/$specId (any status)", () => {
    const statuses: SpecStatus[] = [
      "draft",
      "ready",
      "in_progress",
      "blocked",
      "in_review",
      "completed",
    ];
    for (const status of statuses) {
      const { unmount } = render(<OverlayPanelActions spec={spec({ id: "s9", status })} />);
      const btn = screen.getByRole("button", { name: /open the spec/i });
      expect(btn).toBeInTheDocument();
      fireEvent.click(btn);
      expect(navigate).toHaveBeenLastCalledWith({
        to: "/work/$specId",
        params: { specId: "s9" },
      });
      unmount();
    }
  });

  it("shows 'Open the review' ONLY when in_review, and navigates to /work/$specId/review", () => {
    render(<OverlayPanelActions spec={spec({ id: "s7", status: "in_review" })} />);
    const btn = screen.getByRole("button", { name: /open the review/i });
    expect(btn).toBeInTheDocument();
    fireEvent.click(btn);
    expect(navigate).toHaveBeenCalledWith({
      to: "/work/$specId/review",
      params: { specId: "s7" },
    });
  });

  it("HIDES 'Open the review' for any non-in_review status (but keeps 'Open the Spec')", () => {
    const nonReview: SpecStatus[] = ["draft", "ready", "in_progress", "blocked", "completed"];
    for (const status of nonReview) {
      const { unmount } = render(<OverlayPanelActions spec={spec({ status })} />);
      expect(screen.queryByRole("button", { name: /open the review/i })).toBeNull();
      expect(screen.getByRole("button", { name: /open the spec/i })).toBeInTheDocument();
      unmount();
    }
  });

  it("does NOT bubble an action click to a parent handler (no panel close/toggle on click)", () => {
    // The action lives inside a header/languette that may carry a toggle/close handler; the click
    // must `stopPropagation` so it NEVER fires — the action only navigates.
    const onParentClick = vi.fn();
    render(
      <div onClick={onParentClick}>
        <OverlayPanelActions spec={spec({ status: "in_review" })} />
      </div>,
    );
    fireEvent.click(screen.getByRole("button", { name: /open the spec/i }));
    fireEvent.click(screen.getByRole("button", { name: /open the review/i }));
    expect(onParentClick).not.toHaveBeenCalled();
    expect(navigate).toHaveBeenCalledTimes(2);
  });
});
