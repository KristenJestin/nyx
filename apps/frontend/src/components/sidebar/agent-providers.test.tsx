import { describe, expect, it } from "vitest";

import { AGENT_PROVIDERS, agentProviderFor } from "./agent-providers";

/**
 * The provider-aware agent registry (finding #55). The registry is the single generic
 * `agent_kind → { icon, label }` map; adding a provider is one entry. Today only
 * `claude_code` is wired. These tests pin the shape + the total resolver so a future
 * provider addition (or a typo) is caught.
 */
describe("agent provider registry", () => {
  it("resolves claude_code to a labelled provider with an icon component", () => {
    const claude = agentProviderFor("claude_code");
    expect(claude).toBeDefined();
    expect(claude?.label).toBe("Claude Code");
    // A renderable React component: a function (plain FC) OR an object (forwardRef /
    // memo, which is how the svgr-compiled brand icon comes out). Just assert it's there.
    expect(claude?.icon).toBeTruthy();
    expect(["function", "object"]).toContain(typeof claude?.icon);
  });

  it("returns undefined for an unknown / not-yet-wired agent kind", () => {
    // Codex / OpenCode / custom are anticipated by the SHAPE but not implemented yet.
    expect(agentProviderFor("codex")).toBeUndefined();
    expect(agentProviderFor("opencode")).toBeUndefined();
    expect(agentProviderFor("custom")).toBeUndefined();
    expect(agentProviderFor("totally-unknown")).toBeUndefined();
  });

  it("returns undefined for a null / empty agent kind (no live session)", () => {
    expect(agentProviderFor(null)).toBeUndefined();
    expect(agentProviderFor(undefined)).toBeUndefined();
    expect(agentProviderFor("")).toBeUndefined();
  });

  it("the registry is keyed by the raw DB agent_kind string", () => {
    expect(Object.keys(AGENT_PROVIDERS)).toContain("claude_code");
  });
});
