import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { NodeInspector } from "./node-inspector";

const CONTEXT = `=== NYX CONTEXT — CODER ===

You are the coder sub-agent.

When needed, spawn {{sub_agent:auditor}} and run {{check:typecheck}}.

## Rules

{{rules}}`;

// The STATIC role template the backend returns when NO run is bound (finding #35): the role manual,
// with the run-specific parts marked "resolved at run".
const TEMPLATE = `=== NYX CONTEXT — CODER (role template) ===
Spec    : (resolved at run — no run bound yet)

You are the coder sub-agent.

## Current phase — (resolved at run)

## Rules (project directives)

Resolved at run by scope (always + role:coder).`;

describe("NodeInspector", () => {
  it("shows the effector header + the LIVE context, with markers as clickable chips", () => {
    const onMarker = vi.fn();
    render(
      <NodeInspector
        agentId="coder"
        context={CONTEXT}
        bound
        builtIn
        loading={false}
        error={false}
        onMarker={onMarker}
        onClose={() => {}}
      />,
    );

    // The sub-agent identity — the opaque "Effector" jargon is gone (finding #31).
    expect(screen.getByText("Sub-agent · coder")).toBeTruthy();
    expect(screen.queryByText(/Effector/)).toBeNull();
    // A run is bound → the "live run" state badge, and the live-context note.
    expect(screen.getByText("live run")).toBeTruthy();
    expect(screen.getByText(/live context this sub-agent pulls/i)).toBeTruthy();
    // Declutter: the built-in/custom origin is muted secondary text (one line), not a standalone
    // badge — and it is DATA-DERIVED: a built-in workflow → "built-in sub-agent" (finding #40).
    expect(screen.getByText("built-in sub-agent")).toBeTruthy();
    expect(screen.queryByText("built-in native sub-agent")).toBeNull();
    // The verbatim context prose is rendered.
    expect(screen.getByText(/You are the coder sub-agent/)).toBeTruthy();

    // The {{sub_agent:auditor}} + {{check:typecheck}} markers are clickable chips.
    const subAgent = screen.getByRole("button", { name: "{{sub_agent:auditor}}" });
    fireEvent.click(subAgent);
    expect(onMarker).toHaveBeenCalledWith({ kind: "sub_agent", id: "auditor", nodeId: "" });

    fireEvent.click(screen.getByRole("button", { name: "{{check:typecheck}}" }));
    expect(onMarker).toHaveBeenCalledWith({ kind: "check", id: "typecheck", nodeId: "" });
  });

  // finding #40 — the origin label is DATA-DERIVED: a CUSTOM workflow (builtIn=false) with a known
  // role reads "custom sub-agent", and its role template still renders (never hard-coded "built-in").
  it("labels a known role on a custom workflow as 'custom sub-agent', template still rendered", () => {
    render(
      <NodeInspector
        agentId="coder"
        context={TEMPLATE}
        bound={false}
        builtIn={false}
        loading={false}
        error={false}
        onMarker={() => {}}
        onClose={() => {}}
      />,
    );
    expect(screen.getByText("custom sub-agent")).toBeTruthy();
    expect(screen.queryByText(/built-in/)).toBeNull();
    // A known role STILL renders its template (the role-template badge + the brief).
    expect(screen.getByText("role template")).toBeTruthy();
    expect(screen.getByText(/You are the coder sub-agent/)).toBeTruthy();
  });

  // finding #40 — a spawn whose agent_id is NOT a known role (e.g. `dragon-slayer`) is a CUSTOM
  // sub-agent: a clear custom state, no misleading "role template" badge, no "Could not render".
  it("shows a graceful custom state for an unknown/custom agent_id", () => {
    render(
      <NodeInspector
        agentId="dragon-slayer"
        context={null}
        bound={false}
        builtIn={false}
        loading={false}
        error
        onMarker={() => {}}
        onClose={() => {}}
      />,
    );
    // The custom badge stands in for the (misleading) "role template" badge.
    expect(screen.getByText("custom")).toBeTruthy();
    expect(screen.queryByText("role template")).toBeNull();
    expect(screen.queryByText("live run")).toBeNull();
    // A clear custom message names the unknown id and the known-role list — no error prose.
    expect(screen.getByText(/Custom sub-agent/)).toBeTruthy();
    expect(screen.getByText(/'dragon-slayer'/)).toBeTruthy();
    expect(screen.getByText(/No built-in role template/i)).toBeTruthy();
    expect(screen.queryByText(/Could not render/i)).toBeNull();
    // The origin line is also custom.
    expect(screen.getByText("custom sub-agent")).toBeTruthy();
  });

  // finding #35 — with NO run bound, the inspector shows the STATIC role template (the brief), NOT
  // the old empty "Context not resolved yet" state, and flags the spec-specific parts as run-resolved.
  it("shows the static role template with run-resolved guidance when no run is bound", () => {
    render(
      <NodeInspector
        agentId="coder"
        context={TEMPLATE}
        bound={false}
        builtIn
        loading={false}
        error={false}
        onMarker={() => {}}
        onClose={() => {}}
      />,
    );
    // The state badge + note announce a static template whose spec parts resolve at run.
    expect(screen.getByText("role template")).toBeTruthy();
    expect(screen.getByText(/static role template/i)).toBeTruthy();
    // "resolved at run" appears both in the note guidance and the template body — several matches.
    expect(screen.getAllByText(/resolved at run/i).length).toBeGreaterThan(0);
    // The template body (the sub-agent's brief) is rendered verbatim.
    expect(screen.getByText(/You are the coder sub-agent/)).toBeTruthy();
    expect(screen.getByText(/Current phase — \(resolved at run\)/)).toBeTruthy();
    // The OLD empty state is gone.
    expect(screen.queryByText(/Context not resolved yet/i)).toBeNull();
  });

  it("shows a graceful fallback when there is no rendered template at all", () => {
    render(
      <NodeInspector
        agentId="coder"
        context={null}
        bound={false}
        builtIn
        loading={false}
        error={false}
        onMarker={() => {}}
        onClose={() => {}}
      />,
    );
    expect(screen.getByText(/No role template available/i)).toBeTruthy();
  });

  it("calls onClose from the close control", () => {
    const onClose = vi.fn();
    render(
      <NodeInspector
        agentId="coder"
        context={CONTEXT}
        bound
        builtIn
        loading={false}
        error={false}
        onMarker={() => {}}
        onClose={onClose}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Close inspector" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
