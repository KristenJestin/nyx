import { useEffect, useState, type ReactElement } from "react";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { emit, mockIPC } from "@/bridge/test-harness";
import { describe, expect, it, vi } from "vitest";

import type { ReviewComment, ReviewTask } from "@/bridge";
import { createQueryWrapper } from "@/lib/test-query";

import { CommentsPanel } from "./comments-panel";
import type { CommentActions } from "./comment-thread";
import { type ReviewRepo } from "./review-types";
import { useReviewComments } from "./use-review-comments";
import { useReviewTasks } from "./use-review-tasks";

/** Render under a FRESH TanStack Query client + the real `<ReviewLiveSync>` (PDR-0029). */
function renderWithClient(ui: ReactElement) {
  const { wrapper } = createQueryWrapper();
  return render(ui, { wrapper });
}

/**
 * Phase 5 — the promotion "grill" (finding = task) + the live unread counter, exercised through the
 * REAL `useReviewComments` + `useReviewTasks` hooks + `CommentsPanel` (the review page's right
 * flank, PDR-0028), WITHOUT the `@pierre/diffs` viewer (that surface is covered by the CDP gate):
 *  - the HUMAN promotes a comment (grill step 1) → a `proposed` task;
 *  - the AGENT structures it via the MCP façade (store + `repos://changed`) → `structured` LIVE;
 *  - the HUMAN validates it (grill step 3) → `validated`. No agent-validate, no UI structure-on-behalf.
 *  - the unread badge counts comments after the read watermark, updates LIVE, clears on mark-read.
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
  tasks: ReviewTask[];
  lastReadAt: number;
  agentStructure: (
    id: string,
    title: string,
    details: string,
    severity?: ReviewTask["severity"],
  ) => void;
  addComment: (body: string) => void;
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
  let clock = 1000;
  const now = () => (clock += 10);

  const store: Store = {
    comments: [],
    tasks: [],
    lastReadAt: 0,
    agentStructure(id, title, details, severity) {
      store.tasks = store.tasks.map((t) =>
        t.id === id
          ? { ...t, title, details, severity: severity ?? null, grill_state: "structured" }
          : t,
      );
      void emit("repos://changed");
    },
    addComment(body) {
      store.comments.push(mkComment({ body, created_at: now() }));
      void emit("repos://changed");
    },
  };

  const unreadOf = () => store.comments.filter((c) => c.created_at > store.lastReadAt).length;

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
          body: args?.body as string,
          created_at: now(),
        });
        store.comments.push(created);
        void emit("repos://changed");
        return created;
      }
      case "review_task_list":
        return store.tasks.filter(
          (t) => t.workspace_id === args?.workspaceId && t.repository_id === args?.repositoryId,
        );
      case "review_task_promote": {
        const comment = store.comments.find((c) => c.id === args?.commentId);
        const task: ReviewTask = {
          id: `t${store.tasks.length + 1}`,
          workspace_id: "wsA",
          repository_id: "app",
          from_comment_id: (args?.commentId as string) ?? "",
          title: (args?.title as string) ?? comment?.body ?? "",
          details: null,
          severity: (args?.severity as ReviewTask["severity"]) ?? null,
          grill_state: "proposed",
          created_at: now(),
          updated_at: now(),
          // Finding 53 — the promoted task inherits its source comment's Spec.
          spec_id: comment?.spec_id ?? null,
        };
        store.tasks.push(task);
        void emit("repos://changed");
        return task;
      }
      case "review_task_validate": {
        store.tasks = store.tasks.map((t) =>
          t.id === args?.id ? { ...t, grill_state: "validated" } : t,
        );
        void emit("repos://changed");
        return store.tasks.find((t) => t.id === args?.id);
      }
      case "review_unread":
        return {
          workspace_id: "wsA",
          repository_id: "app",
          unread: unreadOf(),
          last_read_at: store.lastReadAt,
        };
      case "review_mark_read":
        store.lastReadAt = now();
        void emit("repos://changed");
        return {
          workspace_id: "wsA",
          repository_id: "app",
          unread: 0,
          last_read_at: store.lastReadAt,
        };
      default:
        return null;
    }
  });
  return store;
}

/** Host wiring the real comment + task hooks to the panel, with a closable-panel + unread badge —
 *  the same wiring the full review page has, minus the @pierre viewer/router. */
function GrillHarness() {
  const { anchoredThreads, globalThreads, loading, error, postGlobal, reply, resolve, reopen } =
    useReviewComments(REPO, null);
  const {
    tasks,
    unread,
    promote,
    validate,
    markRead,
    error: tasksError,
  } = useReviewTasks(REPO, null);
  const [panelOpen, setPanelOpen] = useState(true);

  useEffect(() => {
    if (panelOpen && unread > 0) void markRead();
  }, [panelOpen, unread, markRead]);

  const actions: CommentActions = {
    onReply: (parentId, body) => void reply(parentId, body),
    onResolve: (id) => void resolve(id),
    onReopen: (id) => void reopen(id),
    onPromote: (commentId) => void promote(commentId),
  };
  return (
    <div>
      <button
        type="button"
        aria-label={panelOpen ? "Hide comments panel" : "Show comments panel"}
        onClick={() => setPanelOpen((v) => !v)}
      >
        toggle
      </button>
      {unread > 0 ? <span data-testid="unread-badge">{unread > 99 ? "99+" : unread}</span> : null}
      {panelOpen ? (
        <CommentsPanel
          anchoredThreads={anchoredThreads}
          globalThreads={globalThreads}
          selectedThreadId={null}
          onSelectThread={() => {}}
          onAddGlobal={(body) => void postGlobal(body)}
          actions={actions}
          loading={loading}
          error={error}
          tasks={tasks}
          tasksError={tasksError}
          onValidateTask={(id) => void validate(id)}
        />
      ) : null}
    </div>
  );
}

describe("Review promotion (grill) + unread counter", () => {
  it("HUMAN promotes a comment → the agent structures it (live) → the HUMAN validates it", async () => {
    const store = installStore();
    renderWithClient(<GrillHarness />);
    await screen.findByText("Comments");

    fireEvent.click(screen.getByRole("button", { name: /new comment/i }));
    fireEvent.change(screen.getByPlaceholderText(/general comment/i), {
      target: { value: "the login path looks fragile" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^comment$/i }));
    await waitFor(() =>
      expect(screen.getByText("the login path looks fragile")).toBeInTheDocument(),
    );

    // GRILL step 1 — the human lâche brut: Promote to task.
    fireEvent.click(await screen.findByTestId("promote-comment"));
    const taskRow = await screen.findByTestId("task-row");
    await waitFor(() => expect(taskRow.getAttribute("data-grill-state")).toBe("proposed"));
    expect(screen.getByText(/awaiting the agent/i)).toBeInTheDocument();

    // GRILL step 2 — the AGENT structures it via MCP (store injection + repos://changed).
    const proposed = store.tasks[0]!;
    store.agentStructure(proposed.id, "Harden the login path", "Add validation + a test.", "high");
    await waitFor(() =>
      expect(screen.getByTestId("task-row").getAttribute("data-grill-state")).toBe("structured"),
    );
    expect(screen.getByText("Harden the login path")).toBeInTheDocument();

    // GRILL step 3 — the HUMAN validates.
    fireEvent.click(screen.getByTestId("validate-task"));
    await waitFor(() =>
      expect(screen.getByTestId("task-row").getAttribute("data-grill-state")).toBe("validated"),
    );
    expect(screen.getByText(/ready for the coder/i)).toBeInTheDocument();
    expect(store.tasks[0]!.grill_state).toBe("validated");
  });

  it("shows the live UNREAD badge and clears it on mark-read", async () => {
    const store = installStore();
    store.comments.push(mkComment({ body: "one", created_at: 100 }));
    store.comments.push(mkComment({ body: "two", created_at: 200 }));

    renderWithClient(<GrillHarness />);
    const badge = await screen.findByTestId("unread-badge");
    expect(badge.textContent).toBe("2");

    await waitFor(() => expect(screen.queryByTestId("unread-badge")).not.toBeInTheDocument());
    expect(store.lastReadAt).toBeGreaterThan(0);
  });

  it("a NEW comment after mark-read raises the unread badge again — LIVE via repos://changed", async () => {
    const store = installStore();
    renderWithClient(<GrillHarness />);
    await screen.findByText("Comments");
    await waitFor(() => expect(screen.queryByTestId("unread-badge")).not.toBeInTheDocument());

    // Close the panel so auto-mark-read does not immediately re-zero it, then a new comment lands.
    fireEvent.click(screen.getByRole("button", { name: /hide comments panel/i }));
    store.addComment("a fresh remark");

    const badge = await screen.findByTestId("unread-badge");
    expect(Number(badge.textContent)).toBeGreaterThanOrEqual(1);
  });

  // FINDING 44 — promote gives visible feedback (an inline confirmation) once the task lands.
  it("shows promote FEEDBACK (a confirmation banner) after a comment is promoted", async () => {
    const store = installStore();
    store.comments.push(mkComment({ body: "promote this one", created_at: 100 }));
    renderWithClient(<GrillHarness />);
    await screen.findByText("Comments");

    fireEvent.click(await screen.findByTestId("promote-comment"));
    // The proposed task appears AND a feedback banner confirms the promotion (no longer silent).
    await screen.findByTestId("task-row");
    expect(await screen.findByTestId("promote-feedback")).toBeInTheDocument();
  });

  // FINDING 44 — a promote FAILURE surfaces (tasks-error) instead of being silent (previously the
  // panel only ever showed the COMMENTS hook's error, never the TASKS hook's).
  it("surfaces a promote FAILURE as a tasks-error (no longer silent)", async () => {
    const comments: ReviewComment[] = [mkComment({ body: "will fail to promote" })];
    mockIPC((cmd, args) => {
      switch (cmd) {
        case "review_comment_list":
          return comments.filter(
            (c) => c.workspace_id === args?.workspaceId && c.repository_id === args?.repositoryId,
          );
        case "review_task_list":
          return [];
        case "review_unread":
          return { workspace_id: "wsA", repository_id: "app", unread: 0, last_read_at: 0 };
        case "review_mark_read":
          return { workspace_id: "wsA", repository_id: "app", unread: 0, last_read_at: 1 };
        case "review_task_promote":
          throw new Error("promote backend unavailable");
        default:
          return null;
      }
    });

    renderWithClient(<GrillHarness />);
    await screen.findByText("Comments");
    fireEvent.click(await screen.findByTestId("promote-comment"));

    const err = await screen.findByTestId("tasks-error");
    expect(err.textContent).toMatch(/promote backend unavailable/i);
  });

  // FINDING 44 — "Promote to task" is now offered on ANCHORED panel rows too (not only the diff
  // cards + global threads). Pure-component check on `CommentsPanel`.
  it("offers Promote on an anchored thread row in the panel", () => {
    const anchoredRoot = mkComment({
      body: "anchored issue",
      file_path: "src/a.ts",
      side: "new",
      line_start: 4,
    });
    const onPromote = vi.fn();
    render(
      <CommentsPanel
        anchoredThreads={[{ root: anchoredRoot, replies: [] }]}
        globalThreads={[]}
        selectedThreadId={null}
        onSelectThread={() => {}}
        onAddGlobal={() => {}}
        actions={{ onReply() {}, onResolve() {}, onReopen() {}, onPromote }}
        loading={false}
        error={null}
        tasks={[]}
        onValidateTask={() => {}}
      />,
    );
    const row = screen.getByTestId("panel-thread-row");
    const promote = within(row).getByTestId("promote-comment");
    fireEvent.click(promote);
    expect(onPromote).toHaveBeenCalledWith(anchoredRoot.id);
  });

  it("uses design tokens only — no raw hex/rgb in the promotion/unread UI", async () => {
    const store = installStore();
    store.tasks.push({
      id: "t1",
      workspace_id: "wsA",
      repository_id: "app",
      from_comment_id: "c1",
      title: "seeded task",
      details: "some details",
      severity: "medium",
      grill_state: "structured",
      created_at: 1,
      updated_at: 1,
      spec_id: null,
    });
    const { container } = renderWithClient(<GrillHarness />);
    await waitFor(() => expect(screen.getByText("seeded task")).toBeInTheDocument());
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });
});
