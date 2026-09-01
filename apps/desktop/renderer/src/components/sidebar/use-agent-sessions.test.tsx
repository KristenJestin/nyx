import { describe, expect, it } from "vitest";

import sharedCases from "../../../../../../contract/home-secondary-info.json";
import { secondaryInfo, type AgentActivity, type AgentActivityKind } from "./use-agent-sessions";

/**
 * The SECONDARY INFORMATION — the short text beside an agent's label saying what it is asking or
 * what it is doing. One resolution order: the block if the agent is blocked, otherwise the tool in
 * flight, otherwise NOTHING (no filler sentence — the dot is the message).
 *
 * THE CASES ARE NOT WRITTEN HERE. The rule exists twice — `secondaryInfo` below and
 * `nyx_core::home::secondary_info` (`crates/nyx-core/src/home.rs`) — so its cases live in
 * `contract/home-secondary-info.json`, which BOTH suites replay: this one, and
 * `nyx_core::home::tests::secondary_info_prefers_the_block_then_the_tool_then_nothing`. Reorder the
 * rule on either side and the other side fails — the same mooring `home-state.ts` gets from
 * `STATE_PRIORITY`, applied across a boundary a single export cannot cross.
 */

interface SharedCase {
  id: string;
  why: string;
  activity: string;
  waiting_on: string | null;
  tool: string | null;
  secondary: string | null;
}

function activity(overrides: Partial<AgentActivity> = {}): AgentActivity {
  return {
    activity: "idle",
    readyUnread: false,
    errorUnread: false,
    pluginOutdated: false,
    waitingOn: null,
    tool: null,
    ...overrides,
  };
}

describe("secondaryInfo", () => {
  const cases = sharedCases.cases as SharedCase[];

  it("reads the shared contract cases", () => {
    // A shrink here would silently weaken the Rust half too, which replays the same file.
    expect(cases.length).toBeGreaterThanOrEqual(7);
  });

  it.each(cases.map((c) => [c.id, c] as const))("%s", (_id, shared) => {
    const resolved = secondaryInfo(
      activity({
        activity: shared.activity as AgentActivityKind,
        waitingOn: shared.waiting_on,
        tool: shared.tool,
      }),
    );
    expect(resolved, shared.why).toBe(shared.secondary);
  });
});
