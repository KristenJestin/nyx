import { act, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import type { ReactElement } from "react";
import { emit, mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import type { ReviewComment } from "@/bridge";
import { createQueryWrapper } from "@/lib/test-query";

import { CommentsPanel } from "./comments-panel";
import type { CommentActions } from "./comment-thread";
import { type ReviewRepo } from "./review-types";
import { useReviewComments } from "./use-review-comments";

/** Render under a FRESH TanStack Query client + the real `<ReviewLiveSync>` (PDR-0029). */
function renderWithClient(ui: ReactElement) {
  const { wrapper } = createQueryWrapper();
  return render(ui, { wrapper });
}

/**
 * T6 — the comments UI: post/thread/reply, an anchored (outdated) comment shown, an AGENT reply
 * (via the MCP façade, simulated by the bridge store) present in the thread, and the human resolves
 * while the agent never can. Rendered through the REAL `useReviewComments` hook + `CommentsPanel`
 * (the right-flank panel of the full-screen review page, PDR-0028) — WITHOUT the `@pierre/diffs`
 * viewer (that surface is exercised by the CDP gate `review-gate-tab.e2e.cjs`, not jsdom).
 *
 * The backend is a STATEFUL fake: it stores posted comments (as the real `review_comments` table
 * would) and serves them from `review_comment_list`, so post → list → thread runs end-to-end. An
 * "agent reply via MCP" is an `agent`-authored comment injected into the store + a `repos://changed`
 * push (exactly what the MCP `review_comment_post` tool does).
 */

const REPO: ReviewRepo = {
  workspaceId: "wsA",
  repositoryId: "app",
  name: "app",
  workspaceName: "alpha",
  absolutePath: "/alpha/app",
  branch: "main",
  changedFileCount: 1,
};

interface Store {
  comments: ReviewComment[];
  agentReply: (parentId: string, body: string) => void;
  seed: (c: Partial<ReviewComment>) => void;
}

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

function installStore(): Store {
  const store: Store = {
    comments: [],
    agentReply(parentId, body) {
      store.comments.push(
        mkComment({ parent_id: parentId, author_type: "agent", author_name: "nyx-agent", body }),
      );
      void emit("repos://changed");
    },
    seed(c) {
      store.comments.push(mkComment(c));
    },
  };

  mockIPC((cmd, args) => {
    switch (cmd) {
      case "review_comment_list":
        return store.comments.filter(
          (c) => c.workspace_id === args?.workspaceId && c.repository_id === args?.repositoryId,
        );
      case "review_comment_post": {
        const created = mkComment({
          workspace_id: args?.workspaceId as string,
          repository_id: args?.repositoryId as string,
          parent_id: (args?.parentId as string) ?? null,
          author_type: "human",
          author_name: "You",
          file_path: (args?.filePath as string) ?? null,
          side: (args?.side as ReviewComment["side"]) ?? null,
          line_start: (args?.lineStart as number) ?? null,
          body: args?.body as string,
        });
        store.comments.push(created);
        void emit("repos://changed");
        return created;
      }
      case "review_comment_resolve": {
        store.comments = store.comments.map((x) =>
          x.id === args?.id ? { ...x, status: "resolved", resolved_by_type: "human" } : x,
        );
        void emit("repos://changed");
        return store.comments.find((x) => x.id === args?.id);
      }
      case "review_comment_reopen": {
        store.comments = store.comments.map((x) =>
          x.id === args?.id ? { ...x, status: "open", resolved_by_type: null } : x,
        );
        void emit("repos://changed");
        return store.comments.find((x) => x.id === args?.id);
      }
      default:
        return null;
    }
  });
  return store;
}

/** A minimal host that wires the REAL comment hook to the panel (no @pierre viewer, no router). */
function CommentsHarness({ specId = null }: { specId?: string | null } = {}) {
  const { anchoredThreads, globalThreads, loading, error, postGlobal, reply, resolve, reopen } =
    useReviewComments(REPO, specId);
  const actions: CommentActions = {
    onReply: (parentId, body) => void reply(parentId, body),
    onResolve: (id) => void resolve(id),
    onReopen: (id) => void reopen(id),
  };
  return (
    <CommentsPanel
      anchoredThreads={anchoredThreads}
      globalThreads={globalThreads}
      selectedThreadId={null}
      onSelectThread={() => {}}
      onAddGlobal={(body) => void postGlobal(body)}
      actions={actions}
      loading={loading}
      error={error}
      tasks={[]}
      onValidateTask={() => {}}
    />
  );
}

describe("Review comments — panel + threads", () => {
  it("shows the comments panel with the New-comment affordance", async () => {
    installStore();
    renderWithClient(<CommentsHarness />);
    expect(await screen.findByText("Comments")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /new comment/i })).toBeInTheDocument();
  });

  it("posts a GLOBAL comment which appears in the panel", async () => {
    installStore();
    renderWithClient(<CommentsHarness />);

    fireEvent.click(screen.getByRole("button", { name: /new comment/i }));
    fireEvent.change(screen.getByPlaceholderText(/general comment/i), {
      target: { value: "overall LGTM" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^comment$/i }));

    await waitFor(() => expect(screen.getByText("overall LGTM")).toBeInTheDocument());
    expect(screen.getByText("Global")).toBeInTheDocument();
  });

  it("an AGENT reply (via the MCP façade) appears in the thread live", async () => {
    const store = installStore();
    renderWithClient(<CommentsHarness />);
    await screen.findByText("Comments");

    fireEvent.click(screen.getByRole("button", { name: /new comment/i }));
    fireEvent.change(screen.getByPlaceholderText(/general comment/i), {
      target: { value: "is this right?" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^comment$/i }));
    await waitFor(() => expect(screen.getByText("is this right?")).toBeInTheDocument());

    const root = store.comments.find((c) => c.body === "is this right?")!;
    store.agentReply(root.id, "yes, it is correct");

    await waitFor(() => expect(screen.getByText("yes, it is correct")).toBeInTheDocument());
    expect(screen.getByText("nyx-agent")).toBeInTheDocument();
  });

  it("the HUMAN resolves a comment (resolved as human)", async () => {
    const store = installStore();
    store.seed({ body: "please fix", author_type: "agent", author_name: "nyx-agent" });
    renderWithClient(<CommentsHarness />);

    const resolveBtn = await screen.findByTestId("resolve-comment");
    fireEvent.click(resolveBtn);
    await waitFor(() => {
      const c = store.comments.find((x) => x.body === "please fix");
      expect(c?.status).toBe("resolved");
      expect(c?.resolved_by_type).toBe("human");
    });
  });

  it("an OUTDATED anchor is visibly flagged in the panel", async () => {
    const store = installStore();
    store.seed({
      body: "stale anchor",
      file_path: "a.ts",
      side: "new",
      line_start: 2,
      outdated: true,
    });
    renderWithClient(<CommentsHarness />);
    await waitFor(() => expect(screen.getByText("stale anchor")).toBeInTheDocument());
    expect(screen.getAllByText("outdated").length).toBeGreaterThan(0);
  });

  it("uses design tokens only — no raw hex/rgb in the comments UI", async () => {
    const store = installStore();
    store.seed({ body: "token check", file_path: "a.ts", side: "new", line_start: 1 });
    const { container } = renderWithClient(<CommentsHarness />);
    await waitFor(() => expect(screen.getByText("token check")).toBeInTheDocument());
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });

  // FINDING 41 — a comment posted while reviewing Spec A must appear ONLY on Spec A, never on a
  // Spec B sharing the same (workspace, repository). The mock stores `spec_id` on post and the
  // list FILTERS by the `specId` the hook sends, mirroring `db::list_review_comments`.
  it("scopes comments by Spec: a Spec A comment never leaks onto Spec B", async () => {
    const store: ReviewComment[] = [];
    mockIPC((cmd, args) => {
      switch (cmd) {
        case "review_comment_list":
          return store.filter(
            (c) =>
              c.workspace_id === args?.workspaceId &&
              c.repository_id === args?.repositoryId &&
              // The scoped read returns ONLY the requested Spec's comments (undefined = unscoped).
              (args?.specId == null || c.spec_id === args?.specId),
          );
        case "review_comment_post": {
          const created = mkComment({
            workspace_id: args?.workspaceId as string,
            repository_id: args?.repositoryId as string,
            spec_id: (args?.specId as string) ?? null,
            body: args?.body as string,
          });
          store.push(created);
          void emit("repos://changed");
          return created;
        }
        default:
          return null;
      }
    });

    // Post "spec A only" while the panel is scoped to Spec A.
    const { unmount } = renderWithClient(<CommentsHarness specId="specA" />);
    await screen.findByText("Comments");
    fireEvent.click(screen.getByRole("button", { name: /new comment/i }));
    fireEvent.change(screen.getByPlaceholderText(/general comment/i), {
      target: { value: "spec A only" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^comment$/i }));
    await waitFor(() => expect(screen.getByText("spec A only")).toBeInTheDocument());
    // The post carried Spec A's id (never NULL) — so it is bound to Spec A.
    expect(store.find((c) => c.body === "spec A only")?.spec_id).toBe("specA");
    unmount();

    // Re-render the SAME (workspace, repository) scoped to Spec B — the comment must NOT show.
    renderWithClient(<CommentsHarness specId="specB" />);
    await screen.findByText("Comments");
    await waitFor(() =>
      expect(screen.getByText(/No comments for this filter/i)).toBeInTheDocument(),
    );
    expect(screen.queryByText("spec A only")).not.toBeInTheDocument();
  });

  // PDR-0029 — OPTIMISTIC mutation: the item changes in the cache the moment the action fires,
  // BEFORE the backend round-trip settles (the lag fix). A resolve whose backend is gated behind a
  // pending promise flips the comment to `resolved` in `result.current` while the gate is still shut.
  it("resolve is OPTIMISTIC — the comment flips resolved before the backend round-trip settles", async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const store: ReviewComment[] = [
      mkComment({ id: "c1", body: "please fix", author_type: "agent", author_name: "nyx-agent" }),
    ];
    mockIPC(async (cmd, args) => {
      switch (cmd) {
        case "review_comment_list":
          return store;
        case "review_comment_resolve": {
          // The backend is HELD until the test releases the gate — proving the UI leads it.
          await gate;
          const i = store.findIndex((c) => c.id === args?.id);
          store[i] = { ...store[i]!, status: "resolved", resolved_by_type: "human" };
          return store[i];
        }
        default:
          return null;
      }
    });

    const { wrapper } = createQueryWrapper();
    const { result } = renderHook(() => useReviewComments(REPO, null), { wrapper });
    await waitFor(() => expect(result.current.comments).toHaveLength(1));
    expect(result.current.comments[0]!.status).toBe("open");

    // Fire resolve WITHOUT awaiting — the backend promise is still pending (gate shut).
    act(() => {
      void result.current.resolve("c1");
    });

    // The optimistic write already flipped the cached comment to resolved/human.
    await waitFor(() => expect(result.current.comments[0]!.status).toBe("resolved"));
    expect(result.current.comments[0]!.resolved_by_type).toBe("human");

    // Release the backend; the reconcile (onSettled invalidate) keeps it resolved.
    release();
    await waitFor(() => expect(result.current.comments[0]!.status).toBe("resolved"));
  });
});
