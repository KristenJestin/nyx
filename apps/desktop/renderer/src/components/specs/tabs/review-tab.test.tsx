import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import type { ReviewComment } from "@/bridge";
import type { SpecDetail, SpecWorkspaceContext } from "@/components/specs/types";
import { createQueryWrapper } from "@/lib/test-query";

import { ReviewTab } from "./review-tab";

/**
 * The Review TAB (PDR-0028): it is NOT the diff viewer anymore — it shows the workspace's review
 * COMMENTS grouped by repo + an "Open review" button that opens the full-screen review page
 * (`/work/$specId/review`). This suite drives the real hooks (`useSpecReviewRepos` +
 * `useWorkspaceReview`) through a bridge store and a memory router (the button is a real `<Link>`).
 */

const WS_CTX: SpecWorkspaceContext = {
  spec_id: "s1",
  workspace: {
    id: "wsA",
    project_id: "p1",
    name: "alpha",
    path: "/alpha",
    branch: "main",
    is_root: false,
    folder_removed_at: null,
  },
  project: { id: "p1", name: "Proj", key: "PRJ" },
  repos: [],
};

const detailWith = (workspace_context: SpecWorkspaceContext | null): SpecDetail =>
  ({ spec: { id: "s1" }, workspace_context }) as unknown as SpecDetail;

const mkComment = (over: Partial<ReviewComment>): ReviewComment => ({
  id: `c${Math.random().toString(36).slice(2, 8)}`,
  workspace_id: "wsA",
  repository_id: "app",
  spec_id: null,
  parent_id: null,
  author_type: "human",
  author_name: "You",
  file_path: null,
  side: null,
  line_start: null,
  line_end: null,
  blob: null,
  snippet: null,
  context: null,
  outdated: false,
  body: "",
  status: "open",
  resolved_by_type: null,
  created_at: Date.now(),
  ...over,
});

/** Install a single-repo workspace whose feed serves the given comments. */
function installProject(comments: ReviewComment[]) {
  mockIPC((cmd) => {
    switch (cmd) {
      case "workspace_git_summary":
        return {
          workspace_id: "wsA",
          repos: [
            {
              repository_id: "app",
              name: "app",
              relative_path: ".",
              absolute_path: "/alpha/app",
              branch: "main",
              dirty: true,
              changed_files: [{ status: " M", path: "a.ts" }],
            },
          ],
        };
      case "review_comment_list":
        return comments;
      default:
        return null;
    }
  });
}

/** Render the tab under a memory router carrying the `/work/$specId/review` target route. */
function renderTab(detail: SpecDetail) {
  const root = createRootRoute();
  const index = createRoute({
    getParentRoute: () => root,
    path: "/",
    component: () => <ReviewTab detail={detail} />,
  });
  const review = createRoute({
    getParentRoute: () => root,
    path: "/work/$specId/review",
    component: () => <div data-testid="review-page-stub">REVIEW PAGE</div>,
  });
  const router = createRouter({
    routeTree: root.addChildren([index, review]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  // The tab's hooks (`useSpecReviewRepos` + `useWorkspaceReview`) are TanStack Query now (PDR-0029),
  // so render under a fresh query client + the real `<ReviewLiveSync>`.
  const { wrapper: QueryWrapper } = createQueryWrapper();
  return render(<RouterProvider router={router} />, { wrapper: QueryWrapper });
}

describe("Review TAB — comments + Open review button (no diff viewer)", () => {
  it("shows the entry card with counts + an Open review button, and NO diff viewer", async () => {
    installProject([
      mkComment({ body: "open one" }),
      mkComment({ body: "done one", status: "resolved" }),
    ]);
    renderTab(detailWith(WS_CTX));

    // Counts: 1 open · 2 total.
    await waitFor(() => expect(screen.getByText(/1 open comment · 2 total/i)).toBeInTheDocument());
    expect(screen.getByTestId("open-review")).toBeInTheDocument();
    // No diff viewer chrome lives in the tab.
    expect(screen.queryByTestId("diff-scroll")).not.toBeInTheDocument();
    expect(screen.queryByTestId("review-diff")).not.toBeInTheDocument();
  });

  it("groups comments by repo with open/resolved badges and expands a thread in place", async () => {
    installProject([
      mkComment({ body: "open one" }),
      mkComment({ body: "done one", status: "resolved" }),
    ]);
    renderTab(detailWith(WS_CTX));

    await screen.findByText("open one");
    // The repo group header (SectionPanel) names the repo.
    expect(screen.getByText("app")).toBeInTheDocument();
    // Expand the open comment in place → its thread (with a Resolve control) appears.
    fireEvent.click(screen.getByText("open one"));
    expect(await screen.findByTestId("resolve-comment")).toBeInTheDocument();
  });

  it("filters All/Open/Resolved", async () => {
    installProject([
      mkComment({ body: "open one" }),
      mkComment({ body: "done one", status: "resolved" }),
    ]);
    renderTab(detailWith(WS_CTX));
    await screen.findByText("open one");

    // Resolved filter hides the open comment.
    fireEvent.click(screen.getByRole("button", { name: "Resolved" }));
    await waitFor(() => expect(screen.queryByText("open one")).not.toBeInTheDocument());
    expect(screen.getByText("done one")).toBeInTheDocument();
  });

  it("navigates to /work/$specId/review when Open review is clicked", async () => {
    installProject([]);
    renderTab(detailWith(WS_CTX));
    fireEvent.click(await screen.findByTestId("open-review"));
    expect(await screen.findByTestId("review-page-stub")).toBeInTheDocument();
  });

  it("shows a clean empty state when there are no comments yet", async () => {
    installProject([]);
    renderTab(detailWith(WS_CTX));
    expect(await screen.findByText(/no review comments yet/i)).toBeInTheDocument();
  });

  it("shows the null-workspace empty state when the Spec has no dev workspace", async () => {
    installProject([]);
    renderTab(detailWith(null));
    expect(await screen.findByText(/this Spec has no dev workspace yet/i)).toBeInTheDocument();
    expect(screen.queryByTestId("open-review")).not.toBeInTheDocument();
  });
});
