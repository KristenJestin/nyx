import { describe, expect, it } from "vitest";

import { spliceWorkspaceOrder } from "./reorder-utils";
import type { TerminalRecord } from "./use-terminals";

function term(id: string, workspaceId: string | null): TerminalRecord {
  return {
    id,
    cwd: "/",
    label: null,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    workspace_id: workspaceId,
    workspace_binding_mode: "manual",
  };
}

describe("spliceWorkspaceOrder (within-workspace reorder, scoped)", () => {
  it("reorders ONLY the target workspace's terminals, others keep their slots", () => {
    // Global order: A(ws1) B(ws2) C(ws1) D(ws1) E(ws2).
    const terminals = [
      term("A", "ws1"),
      term("B", "ws2"),
      term("C", "ws1"),
      term("D", "ws1"),
      term("E", "ws2"),
    ];
    // Reorder ws1's terminals from [A,C,D] → [D,A,C].
    const next = spliceWorkspaceOrder(terminals, "ws1", ["D", "A", "C"]);
    // ws1's slots (positions 0,2,3) get D,A,C in order; ws2 (B,E) stay put.
    expect(next).toEqual(["D", "B", "A", "C", "E"]);
  });

  it("is a faithful identity when the order is unchanged", () => {
    const terminals = [term("A", "ws1"), term("B", "ws1")];
    expect(spliceWorkspaceOrder(terminals, "ws1", ["A", "B"])).toEqual(["A", "B"]);
  });

  it("never moves terminals from OTHER workspaces or the loose section", () => {
    const terminals = [term("loose", null), term("A", "ws1"), term("B", "ws1")];
    const next = spliceWorkspaceOrder(terminals, "ws1", ["B", "A"]);
    // The loose terminal keeps slot 0; only ws1's two swap.
    expect(next).toEqual(["loose", "B", "A"]);
  });
});

describe("spliceWorkspaceOrder — LOOSE group (workspace_id == null, 01KV2V4AWT…)", () => {
  it("reorders ONLY the loose terminals; workspace terminals keep their slots", () => {
    // Global order: L1(loose) A(ws1) L2(loose) B(ws1) L3(loose).
    const terminals = [
      term("L1", null),
      term("A", "ws1"),
      term("L2", null),
      term("B", "ws1"),
      term("L3", null),
    ];
    // Reorder the loose group [L1,L2,L3] → [L3,L1,L2].
    const next = spliceWorkspaceOrder(terminals, null, ["L3", "L1", "L2"]);
    // Loose slots (0,2,4) get L3,L1,L2 in order; ws1 (A,B) stay at slots 1,3.
    expect(next).toEqual(["L3", "A", "L1", "B", "L2"]);
  });

  it("treats an ABSENT workspace_id as loose (undefined ≡ null group)", () => {
    const undef = { ...term("U", null), workspace_id: undefined };
    const terminals = [undef, term("V", null)];
    const next = spliceWorkspaceOrder(terminals, null, ["V", "U"]);
    expect(next).toEqual(["V", "U"]);
  });

  it("does not corrupt cross-group order when only the loose group moves", () => {
    const terminals = [term("A", "ws1"), term("L1", null), term("L2", null)];
    const next = spliceWorkspaceOrder(terminals, null, ["L2", "L1"]);
    // The workspace terminal A stays first; the two loose swap.
    expect(next).toEqual(["A", "L2", "L1"]);
  });
});
