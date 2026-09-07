import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CommandIdleEmpty } from "./command-idle-empty";
import type { WorkspaceRuntimeService } from "@/components/runtime/use-workspace-runtime";

function service(overrides: Partial<WorkspaceRuntimeService> = {}): WorkspaceRuntimeService {
  return {
    id: "s1",
    command_id: "c1",
    workspace_id: "ws1",
    name: "preview",
    command: "npm run preview",
    subfolder: null,
    description: null,
    run_state: "idle",
    exit_code: 0,
    unread: false,
    ...overrides,
  };
}

describe("CommandIdleEmpty", () => {
  it("renders a minimal, discrete state label (the run-state word) — no card, no summary", () => {
    render(<CommandIdleEmpty service={service()} />);
    // The ONLY text is the state word (idle) — the rejected card's name/command/last-run/hint
    // must all be gone.
    expect(screen.getByText("idle")).toBeTruthy();
    expect(screen.queryByText("preview")).toBeNull();
    expect(screen.queryByText(/npm run preview/)).toBeNull();
    expect(screen.queryByText(/Last run/)).toBeNull();
    expect(screen.queryByText(/Output appears here/)).toBeNull();
  });

  it("sits on the terminal-coloured background and centres the label", () => {
    const { container } = render(<CommandIdleEmpty service={service()} />);
    const host = container.firstElementChild as HTMLElement;
    expect(host.className).toContain("bg-background");
    expect(host.className).toContain("items-center");
    expect(host.className).toContain("justify-center");
  });

  it("uses design tokens only (no raw hex / rgb colour)", () => {
    const { container } = render(<CommandIdleEmpty service={service()} />);
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });
});
