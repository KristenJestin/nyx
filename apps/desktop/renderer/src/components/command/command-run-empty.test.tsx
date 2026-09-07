import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CommandRunEmpty } from "./command-run-empty";
import { commandRunPhase, commandRunPhaseLabel, type CommandRunPhase } from "./command-run-phase";
import type { WorkspaceRuntimeService } from "@/components/runtime/use-workspace-runtime";

function service(overrides: Partial<WorkspaceRuntimeService> = {}): WorkspaceRuntimeService {
  return {
    id: "s1",
    command_id: "cmd-1",
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

/**
 * THE FIVE PHASES (review 01M0ABR6V501D0SPR2BYBVZC4A, minor). The runtime knows four states and
 * collapses "never started" and "stopped" onto `idle` — which is exactly the confusion the user
 * hit. Each phase must read as ITSELF.
 */
describe("commandRunPhase", () => {
  it("splits the runtime's idle into NEVER RUN and STOPPED", () => {
    expect(commandRunPhase({ state: "idle", ranThisSession: false })).toBe("never-run");
    expect(commandRunPhase({ state: "idle", ranThisSession: true })).toBe("stopped");
    // The default is the honest one: without having watched a run, nothing was stopped.
    expect(commandRunPhase({ state: "idle" })).toBe("never-run");
  });

  it("maps the three live states straight through", () => {
    expect(commandRunPhase({ state: "running" })).toBe("running");
    expect(commandRunPhase({ state: "success" })).toBe("succeeded");
    expect(commandRunPhase({ state: "error" })).toBe("failed");
    // A running command is running whatever the session history says.
    expect(commandRunPhase({ state: "running", ranThisSession: true })).toBe("running");
  });
});

describe("commandRunPhaseLabel", () => {
  const PHASES: CommandRunPhase[] = ["never-run", "running", "succeeded", "failed", "stopped"];

  it("gives each of the five phases a DISTINCT wording", () => {
    const labels = PHASES.map((phase) => commandRunPhaseLabel(phase, { exitCode: 1 }));
    expect(new Set(labels).size).toBe(PHASES.length);
  });

  it("never says `idle` — the word that read as inactivity for a command that had never run", () => {
    for (const phase of PHASES) {
      expect(commandRunPhaseLabel(phase, { exitCode: 2 }).toLowerCase()).not.toContain("idle");
    }
  });

  it("carries the EXIT CODE on a failure, and nothing invented when there is none", () => {
    expect(commandRunPhaseLabel("failed", { exitCode: 137 })).toContain("137");
    expect(commandRunPhaseLabel("failed", { exitCode: null })).not.toMatch(/\d/);
    expect(commandRunPhaseLabel("failed")).not.toMatch(/\d/);
  });

  it("tells a command that NEVER RAN from one that FINISHED — the original confusion", () => {
    expect(commandRunPhaseLabel("never-run")).not.toBe(commandRunPhaseLabel("succeeded"));
    expect(commandRunPhaseLabel("never-run")).not.toBe(commandRunPhaseLabel("failed"));
    expect(commandRunPhaseLabel("never-run")).not.toBe(commandRunPhaseLabel("stopped"));
  });
});

describe("CommandRunEmpty", () => {
  it("INVITES the action for a command that has never run — the name and its trigger, no `idle`", () => {
    render(<CommandRunEmpty service={service()} />);
    expect(screen.getByText("preview")).toBeTruthy();
    expect(screen.getByText(/press Start to run it/i)).toBeTruthy();
    expect(screen.queryByText("idle")).toBeNull();
    // …and the REJECTED card is still gone: no command line, no last-run block, no hint panel.
    expect(screen.queryByText(/npm run preview/)).toBeNull();
    expect(screen.queryByText(/Last run/)).toBeNull();
    expect(screen.queryByText(/Output appears here/)).toBeNull();
  });

  it("sits on the terminal-coloured background and centres its line", () => {
    const { container } = render(<CommandRunEmpty service={service()} />);
    const host = container.firstElementChild as HTMLElement;
    expect(host.className).toContain("bg-background");
    expect(host.className).toContain("items-center");
    expect(host.className).toContain("justify-center");
  });

  it("uses design tokens only (no raw hex / rgb colour)", () => {
    const { container } = render(<CommandRunEmpty service={service()} />);
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });

  it("renders a DISTINCT line for each of the five phases", () => {
    const seen = new Set<string>();
    // The two the empty state really receives (the runtime's `idle`, split by session history)…
    const { container, rerender } = render(<CommandRunEmpty service={service()} />);
    seen.add(container.textContent ?? "");
    // …and the three settled/live ones, which the component is total over.
    for (const state of ["running", "success", "error"] as const) {
      rerender(<CommandRunEmpty service={service({ run_state: state })} />);
      seen.add(container.textContent ?? "");
    }
    // "stopped" = idle after a run was observed; the pure phase mapping is asserted above, so what
    // is pinned here is that its wording is not any of the four already rendered.
    seen.add(commandRunPhaseLabel("stopped"));
    expect(seen.size).toBe(5);
  });
});
