import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { bridgeFake, mockIPC } from "@/bridge/test-harness";

import type { ProtoTree, PrototypeRound } from "../proto-types";
import { RoundHistory } from "./round-history";

/**
 * T26 — the rounds history (findings `01KYW32N1BVDVTGS0WPMG2PJW2` then
 * `01KZ3AP34K1PKS6W0E75RP7DF6`). These assert that the control surfaces the rounds (the current one
 * flagged, a presented one marked frozen), that it stays hidden when there is no history, that a
 * row OPENS the round FOR CONSULTATION — and that NOTHING here creates one: no rollback affordance,
 * and no write reaching the sidecar.
 */

function round(id: string, label: string, presented: boolean, position: number): PrototypeRound {
  return {
    id,
    prototype_id: "proto1",
    label,
    summary: null,
    position,
    presented_at: presented ? 100 + position : null,
    created_at: position,
  };
}

/** A prototype with a presented (frozen) v1 and a mutable current v2. */
function tree(rounds: readonly PrototypeRound[], current: PrototypeRound | null): ProtoTree {
  return {
    prototype: {
      id: "proto1",
      spec_id: "s1",
      description: null,
      created_at: 0,
      updated_at: 0,
      archived_at: null,
    },
    rounds,
    current_round: current,
    design_lock: null,
    pages: [],
    open_feedback: [],
    resolved_feedback: [],
  };
}

describe("<RoundHistory> (T26 — rounds, read-only)", () => {
  it("surfaces the rounds and flags the current one", async () => {
    mockIPC(() => undefined);
    const v1 = round("r1", "v1", true, 0);
    const v2 = round("r2", "v2", false, 1);
    render(<RoundHistory tree={tree([v1, v2], v2)} />);

    fireEvent.click(screen.getByRole("button", { name: /rounds history/i }));

    expect(await screen.findByTestId("proto-round-r2")).toBeTruthy();
    expect(screen.getByTestId("proto-round-r1")).toBeTruthy();
    expect(screen.getByText("Current")).toBeTruthy();
  });

  /**
   * DONE-CRITERION of finding `01KZ3AP34K1PKS6W0E75RP7DF6` — a round is CONSULTABLE. « je vois une
   * liste d'un truc dont je ne peux pas voir le détail ». Clicking a row asks the host to show that
   * round; the CURRENT row is the way back, so it asks for `null`.
   */
  it("opens a round for CONSULTATION, and the current row is the way back", async () => {
    mockIPC(() => undefined);
    const v1 = round("r1", "v1", true, 0);
    const v2 = round("r2", "v2", false, 1);
    const asked: (string | null)[] = [];
    render(<RoundHistory tree={tree([v1, v2], v2)} onConsult={(id) => asked.push(id)} />);

    fireEvent.click(screen.getByRole("button", { name: /rounds history/i }));
    fireEvent.click(await screen.findByTestId("proto-round-r1"));
    expect(asked).toEqual(["r1"]);

    fireEvent.click(screen.getByTestId("proto-round-r2"));
    expect(asked).toEqual(["r1", null]);
  });

  /** The row being looked at is the one marked — the current row while nothing else is consulted. */
  it("marks which round is being consulted", async () => {
    mockIPC(() => undefined);
    const v1 = round("r1", "v1", true, 0);
    const v2 = round("r2", "v2", false, 1);
    const { rerender } = render(
      <RoundHistory tree={tree([v1, v2], v2)} consultedRoundId={null} onConsult={() => {}} />,
    );

    fireEvent.click(screen.getByRole("button", { name: /rounds history/i }));
    expect((await screen.findByTestId("proto-round-r2")).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByTestId("proto-round-r1").getAttribute("aria-pressed")).toBe("false");

    rerender(<RoundHistory tree={tree([v1, v2], v2)} consultedRoundId="r1" onConsult={() => {}} />);
    expect(screen.getByTestId("proto-round-r1").getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByTestId("proto-round-r2").getAttribute("aria-pressed")).toBe("false");
  });

  it("offers NO rollback: consulting a round is not opening one", async () => {
    mockIPC(() => undefined);
    const v1 = round("r1", "v1", true, 0);
    const v2 = round("r2", "v2", false, 1);
    render(<RoundHistory tree={tree([v1, v2], v2)} onConsult={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: /rounds history/i }));
    const earlier = await screen.findByTestId("proto-round-r1");

    // The affordance that OPENED a round is gone — and so is the paragraph that said so, since a
    // panel with no mutating control does not need a caption on the absence (finding
    // `01KZ46P0FTMJXRTT32H6G5TEEN`).
    expect(screen.queryByTestId("proto-round-rollback-r1")).toBeNull();
    expect(screen.queryByText(/go back to this round/i)).toBeNull();
    expect(screen.queryByTestId("proto-rounds-readonly")).toBeNull();
    expect(screen.queryByText(/read-only/i)).toBeNull();

    // …and clicking it writes NOTHING AT ALL: creating a round is the agent's work.
    fireEvent.click(earlier);
    expect(bridgeFake().calls.find((c) => c.command === "proto_round_rebase")).toBeUndefined();
    expect(bridgeFake().calls.filter((c) => c.command.startsWith("proto_"))).toEqual([]);
  });

  /**
   * DONE-CRITERION (finding `01KZ3JXCNK902DQXBD6QSYEE78`) — A ROW SAYS WHAT ITS ROUND IS.
   *
   * Seven rows reading `v7 … v1`, three padlocks, no date, no summary, no tooltip: nothing said
   * which round was which nor why it was opened. `prototype_rounds.summary` was FILLED all along —
   * six of the seven rounds of the user's own prototype carry one — and displayed nowhere in the
   * app. It is the agent's own prose, in the user's language, and it is shown verbatim.
   */
  it("says a round's date and summary, and explains the padlock", async () => {
    mockIPC(() => undefined);
    const v1: PrototypeRound = {
      ...round("r1", "v1", true, 0),
      // 27/07/2026, in epoch MILLISECONDS — the unit the proto rows really cross the wire in.
      created_at: Date.UTC(2026, 6, 27, 12, 0, 0),
      summary: "Rollback depuis v1",
    };
    const v2 = round("r2", "v2", false, 1);
    render(<RoundHistory tree={tree([v1, v2], v2)} onConsult={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: /rounds history/i }));
    const row = await screen.findByTestId("proto-round-r1");

    expect(screen.getByTestId("proto-round-summary-r1").textContent).toBe("Rollback depuis v1");
    expect(row.textContent).toMatch(/27\/07\/26/);
    // The whole sentence stays reachable — a summary is a sentence, and a menu is not a document.
    expect(row.getAttribute("title")).toBe("Rollback depuis v1");
    // A round with no summary claims none rather than showing an empty line.
    expect(screen.queryByTestId("proto-round-summary-r2")).toBeNull();
    // …and the padlock is explained ON THE PADLOCK (finding `01KZ46P0FTMJXRTT32H6G5TEEN`): a
    // tooltip on the icon, for the rows that carry one — not a paragraph over the whole list.
    expect(screen.getByTestId("proto-round-lock-r1").getAttribute("title")).toMatch(
      /presented round/i,
    );
    expect(screen.queryByTestId("proto-round-lock-r2")).toBeNull();
  });

  it("stays hidden when there is no history yet", () => {
    mockIPC(() => undefined);
    const v1 = round("r1", "v1", false, 0);
    render(<RoundHistory tree={tree([v1], v1)} />);

    expect(screen.queryByRole("button", { name: /rounds history/i })).toBeNull();
  });
});
