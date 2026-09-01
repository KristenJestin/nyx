import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import { createAppRouter } from "@/router";

const HOME = "/home/kris";

/**
 * The APP's own router, so the wiring under test is the real one: the pinned rail tile, the index
 * route's dispatch, and the page the two of them land on. A bare backend serves the empty project
 * tree plus the Système singleton.
 */
function installBackend() {
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "list_projects":
      case "list_workspaces":
      case "list_terminals":
      case "spec_list":
      case "command_list":
      case "command_group_list":
      case "command_instance_list":
      case "command_import_scripts":
      case "agent_active_sessions":
      case "agent_activity_snapshot":
        return [];
      case "system_scope_detail":
        return {
          project: {
            id: "sys-project",
            name: "System",
            collapsed: false,
            created_at: 0,
            updated_at: 0,
            resume_agent_sessions: false,
            kind: "system",
          },
          workspace: {
            id: "sys-ws",
            project_id: "sys-project",
            name: "system",
            path: HOME,
            branch: null,
            is_root: true,
            collapsed: false,
            created_at: 0,
            updated_at: 0,
          },
          home: HOME,
        };
      case "workspace_runtime_snapshot":
        return {
          workspace: { id: a.workspaceId as string, name: "system", path: HOME },
          terminals: [],
          agent_sessions: [],
          services: [],
        };
      default:
        return undefined;
    }
  });
}

describe("the Système page on the app's own router", () => {
  it("lands on the Système page from the rail tile, on its OWN route", async () => {
    installBackend();
    const router = createAppRouter(["/"]);
    render(<RouterProvider router={router} />);

    // With no project, `/` is the Terminals placeholder.
    expect(await screen.findByText(/Enter a project/)).toBeTruthy();

    fireEvent.click(await screen.findByRole("button", { name: "System" }));

    // A ROUTE OF ITS OWN (finding U1) — not a second screen dispatched inside `/`. Its rail, its
    // two sections, its `~` foot.
    const rail = await screen.findByRole("complementary", { name: "System" });
    expect(router.state.location.pathname).toBe("/system");
    expect(rail).toBeTruthy();
    await waitFor(() => expect(screen.queryByText(/Enter a project/)).toBeNull());
    expect(screen.getByText("default cwd")).toBeTruthy();
  });

  it("is reachable DIRECTLY at `/system`, with no click to get there", async () => {
    // The proof that it is a route and not a mode: the router alone puts the page on screen.
    installBackend();
    const router = createAppRouter(["/system"]);
    render(<RouterProvider router={router} />);
    expect(await screen.findByRole("complementary", { name: "System" })).toBeTruthy();
    expect(router.state.location.pathname).toBe("/system");
  });

  it("shows the empty runtime honestly — no terminals, no commands, and still both sections", async () => {
    installBackend();
    const router = createAppRouter(["/"]);
    render(<RouterProvider router={router} />);
    fireEvent.click(await screen.findByRole("button", { name: "System" }));
    await screen.findByRole("complementary", { name: "System" });
    expect(await screen.findByText("No open terminals.")).toBeTruthy();
    expect(screen.getByText("No managed commands.")).toBeTruthy();
    // The two creation entries are the way out of the empty state.
    expect(screen.getByRole("button", { name: "New terminal" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "New command" })).toBeTruthy();
  });
});
