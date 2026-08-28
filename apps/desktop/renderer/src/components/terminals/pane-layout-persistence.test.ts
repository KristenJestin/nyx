import { beforeEach, describe, expect, it } from "vitest";

import {
  MIN_PANE_WIDTH_PX,
  paneRects,
  paneSeparators,
  separatorRatioBounds,
  type HostSize,
  type PaneSeparator,
} from "@/components/panes/pane-geometry";
import {
  loadPaneCollection,
  migrateV1,
  PANE_LAYOUT_VERSION,
  PANE_LAYOUT_VERSION_V1,
  restorePaneCollection,
  savePaneCollection,
} from "./pane-layout-persistence";
import {
  EMPTY_COLLECTION,
  isPaneCollection,
  type PaneCollection,
} from "@/components/panes/pane-groups";
import {
  createPaneLayout,
  isPaneNode,
  MAX_STORED_RATIO,
  MIN_STORED_RATIO,
  paneCount,
  paneTerminalIds,
  splitPane,
  type PaneNode,
} from "@/components/panes/pane-tree";
import { saveLastTerminalId } from "./ui-persistence";

/** A two-pane layout (`t1` | `t2`) for a project. */
function twoPanes(): PaneNode {
  const root = splitPane(createPaneLayout("t1", "p1"), {
    paneId: "p1",
    direction: "row",
    terminalId: "t2",
    newPaneId: "p2",
  });
  return root as PaneNode;
}

/** A second two-pane layout (`t9` | `t10`), for the cross-project isolation checks. */
function otherPanes(): PaneNode {
  return splitPane(createPaneLayout("t9", "p9"), {
    paneId: "p9",
    direction: "row",
    terminalId: "t10",
    newPaneId: "p10",
  }) as PaneNode;
}

const KEY = (projectId: string) => `nyx.ui.paneLayout:${projectId}`;

/**
 * The three helpers this file drives the v2 collection through. Everything a project persists is
 * now a COLLECTION of groups, but almost every scenario below is about ONE group's tree (ratios,
 * px compaction, staleness), so these wrap that one group in and out — the API under test is still
 * `savePaneCollection` / `loadPaneCollection` / `restorePaneCollection`.
 */
function collectionOf(tree: PaneNode): PaneCollection {
  return { groups: [{ id: "g1", tree, name: null, color: null }] };
}
/** Seed `projectId` with ONE group holding `tree`. */
function seedGroup(projectId: string, tree: PaneNode | null): void {
  savePaneCollection(projectId, tree == null ? EMPTY_COLLECTION : collectionOf(tree));
}
/** The tree of the ONE restored group, or `null` when no group survived. */
function restoredTree(
  projectId: string,
  live: readonly string[],
  host: HostSize | null = null,
): PaneNode | null {
  return restorePaneCollection(projectId, live, host).groups[0]?.tree ?? null;
}
/** The tree of the ONE stored group, or `null` when the entry is absent/rejected. */
function storedTree(projectId: string): PaneNode | null {
  return loadPaneCollection(projectId)?.groups[0]?.tree ?? null;
}
/** Write a RAW envelope by hand (the corrupted-entry path the guards must survive). */
function persistRaw(projectId: string, data: unknown, version = PANE_LAYOUT_VERSION): void {
  window.localStorage.setItem(KEY(projectId), JSON.stringify({ v: version, data }));
}
/** Write a RAW envelope holding ONE group around `tree`. */
function persistRawTree(projectId: string, tree: unknown, version = PANE_LAYOUT_VERSION): void {
  persistRaw(projectId, { groups: [{ id: "g1", tree, name: null, color: null }] }, version);
}

beforeEach(() => {
  window.localStorage.clear();
});

describe("savePaneCollection / loadPaneCollection", () => {
  it("round-trips a layout under the project-scoped key", () => {
    const layout = twoPanes();
    seedGroup("proj-a", layout);
    expect(window.localStorage.getItem(KEY("proj-a"))).not.toBeNull();
    expect(storedTree("proj-a")).toEqual(layout);
  });

  it("NEVER shares a layout between two projects", () => {
    seedGroup("proj-a", twoPanes());
    seedGroup("proj-b", otherPanes());
    expect(paneTerminalIds(storedTree("proj-a") as PaneNode)).toEqual(["t1", "t2"]);
    expect(paneTerminalIds(storedTree("proj-b") as PaneNode)).toEqual(["t9", "t10"]);
    expect(storedTree("proj-c")).toBeNull();
  });

  it("REFUSES a stored group of fewer than two members — that is a solo terminal, not a group", () => {
    persistRawTree("proj-one", createPaneLayout("t9", "p9"));
    expect(loadPaneCollection("proj-one")).toBeNull();
  });

  it("REFUSES a terminal sitting in TWO groups — the one-group-per-terminal invariant", () => {
    persistRaw("proj-dup", {
      groups: [
        { id: "g1", tree: twoPanes(), name: null, color: null },
        { id: "g2", tree: twoPanes(), name: null, color: null },
      ],
    });
    expect(loadPaneCollection("proj-dup")).toBeNull();
  });

  it("round-trips a group's NAME and COLOUR, and keeps them independent", () => {
    savePaneCollection("proj-n", {
      groups: [{ id: "g1", tree: twoPanes(), name: "build", color: 4 }],
    });
    expect(loadPaneCollection("proj-n")?.groups[0]).toMatchObject({ name: "build", color: 4 });
    savePaneCollection("proj-c2", {
      groups: [{ id: "g1", tree: twoPanes(), name: null, color: 2 }],
    });
    expect(loadPaneCollection("proj-c2")?.groups[0]).toMatchObject({ name: null, color: 2 });
    // An out-of-range colour is not a colour: the whole entry is discarded, never coerced.
    persistRaw("proj-bad", { groups: [{ id: "g1", tree: twoPanes(), name: null, color: 9 }] });
    expect(loadPaneCollection("proj-bad")).toBeNull();
  });

  it("keeps the projects separate even when their terminals/workspaces change", () => {
    seedGroup("proj-a", twoPanes());
    seedGroup("proj-b", otherPanes());
    // proj-b loses its terminals; proj-a is untouched.
    expect(restoredTree("proj-b", [])).toBeNull();
    expect(paneTerminalIds(restoredTree("proj-a", ["t1", "t2"]) as PaneNode)).toEqual(["t1", "t2"]);
  });

  it("forgets the layout on null", () => {
    seedGroup("proj-a", twoPanes());
    seedGroup("proj-a", null);
    expect(window.localStorage.getItem(KEY("proj-a"))).toBeNull();
    expect(storedTree("proj-a")).toBeNull();
  });

  it("rejects an UNKNOWN VERSION", () => {
    persistRawTree("proj-a", twoPanes(), PANE_LAYOUT_VERSION + 1);
    expect(storedTree("proj-a")).toBeNull();
  });

  it("rejects a malformed / invariant-breaking tree", () => {
    persistRawTree("proj-a", { kind: "split", id: "s1" });
    expect(storedTree("proj-a")).toBeNull();

    // Same terminal in two panes — the uniqueness invariant.
    persistRawTree("proj-b", {
      kind: "split",
      id: "s1",
      direction: "row",
      ratio: 0.5,
      children: [
        { kind: "leaf", id: "p1", terminalId: "t1" },
        { kind: "leaf", id: "p2", terminalId: "t1" },
      ],
    });
    expect(storedTree("proj-b")).toBeNull();
  });
});

describe("restorePaneCollection", () => {
  it("returns the persisted layout when every terminal is still live", () => {
    seedGroup("proj-a", twoPanes());
    const restored = restoredTree("proj-a", ["t1", "t2", "t3"]);
    expect(paneTerminalIds(restored as PaneNode)).toEqual(["t1", "t2"]);
    expect(isPaneNode(restored)).toBe(true);
  });

  it("drops the terminals that disappeared and COMPACTS the group's tree", () => {
    const three = splitPane(twoPanes(), {
      paneId: "p2",
      direction: "column",
      terminalId: "t3",
      newPaneId: "p3",
    }) as PaneNode;
    seedGroup("proj-a", three);
    const restored = restoredTree("proj-a", ["t2", "t3"]) as PaneNode;
    // `t1` is gone: its leaf is dropped and the split it was in collapses onto the survivor.
    expect(restored.kind).toBe("split");
    expect(restored.kind === "split" && restored.direction).toBe("column");
    expect(restored.kind === "split" && restored.children).toEqual([
      { kind: "leaf", id: "p2", terminalId: "t2" },
      { kind: "leaf", id: "p3", terminalId: "t3" },
    ]);
    expect(paneCount(restored)).toBe(2);
    expect(isPaneNode(restored)).toBe(true);
  });

  it("DISSOLVES a group that fell to one live member — one terminal is not a group", () => {
    seedGroup("proj-a", twoPanes());
    // `t1` disappeared: `t2` is left alone, so there is no group any more — the page shows it
    // SOLO, which is the whole point of the escapable model.
    expect(restorePaneCollection("proj-a", ["t2"]).groups).toEqual([]);
  });

  it("yields NO group when the whole tree is stale (the page then shows a terminal SOLO)", () => {
    seedGroup("proj-a", twoPanes());
    saveLastTerminalId("proj-a", "t7");
    // No fallback tree is invented here any more: which terminal to SHOW is the page's call
    // (`usePaneLayout` reads the same `lastTerminalId` hint), and it shows it alone.
    expect(restorePaneCollection("proj-a", ["t5", "t7"]).groups).toEqual([]);
  });

  it("discards an UNKNOWN VERSION entirely", () => {
    persistRawTree("proj-a", twoPanes(), PANE_LAYOUT_VERSION + 1);
    expect(restorePaneCollection("proj-a", ["t1", "t2"])).toEqual(EMPTY_COLLECTION);
  });

  it("yields no group when nothing was ever persisted", () => {
    expect(restorePaneCollection("fresh", ["t4"])).toEqual(EMPTY_COLLECTION);
  });

  it("yields no group when the project has NO terminal at all", () => {
    seedGroup("proj-a", twoPanes());
    expect(restorePaneCollection("proj-a", []).groups).toEqual([]);
    expect(restorePaneCollection("empty", []).groups).toEqual([]);
  });

  it("keeps SEVERAL groups apart, each with its own name and colour", () => {
    savePaneCollection("proj-multi", {
      groups: [
        { id: "g1", tree: twoPanes(), name: "build", color: 4 },
        { id: "g2", tree: otherPanes(), name: null, color: null },
      ],
    });
    const restored = restorePaneCollection("proj-multi", ["t1", "t2", "t9", "t10", "solo"]);
    expect(restored.groups.map((g) => [g.id, g.name, g.color])).toEqual([
      ["g1", "build", 4],
      ["g2", null, null],
    ]);
    // `solo` is live and in NO group — solo membership is not stored, it is the absence of one.
    expect(restored.groups.flatMap((g) => paneTerminalIds(g.tree))).not.toContain("solo");
  });

  it("never returns a tree referencing a dead terminal", () => {
    seedGroup("proj-a", twoPanes());
    const live = ["t2", "t8"];
    const restored = restoredTree("proj-a", live) as PaneNode;
    for (const terminalId of paneTerminalIds(restored)) expect(live).toContain(terminalId);
  });
});

/**
 * THE PX HALF OF THE RESTORE (review round 3).
 *
 * `isPaneNode` checks the SHAPE and `pruneStaleTerminals` the TERMINALS; neither looked at a single
 * PIXEL. A ratio of 0.98 is INSIDE the storable band (which is where it must stay: the px floor is
 * allowed to park a real seam outside 15..85, and rejecting those would throw a legitimate tiling
 * away), so a hand-edited or corrupted `nyx.ui.paneLayout:<projectId>` sailed through and restored
 * a 14.40px pane on a 720px host — under the 18px reading that motivated the floor in the first
 * place. No GESTURE can write such a ratio; the persisted entry is the one path that bypasses
 * `separatorRatioBounds`, and this is the pass that closes it.
 */
describe("restorePaneCollection — the px compaction of a corrupted entry", () => {
  const host: HostSize = { width: 720, height: 900 };
  const widths = (root: PaneNode, on: HostSize = host) =>
    paneRects(root).map((p) => (on.width * p.rect.width) / 100);

  /** A hand-written entry: two panes, one seam, an arbitrary STORABLE ratio. */
  const persistRatio = (projectId: string, ratio: number) =>
    persistRawTree(projectId, {
      kind: "split",
      id: "s1",
      direction: "row",
      ratio,
      children: [
        { kind: "leaf", id: "p1", terminalId: "t1" },
        { kind: "leaf", id: "p2", terminalId: "t2" },
      ],
    });

  it("restores 0.98 as a 540 / 180px tiling instead of 705.60 / 14.40px", () => {
    persistRatio("proj-a", MAX_STORED_RATIO);
    // The entry really IS accepted by the shape guard — which is precisely why a px pass was needed.
    expect(storedTree("proj-a")).not.toBeNull();
    // WITHOUT a measured host nothing can be judged, and the pre-round-3 behaviour is unchanged.
    expect(widths(restoredTree("proj-a", ["t1", "t2"]) as PaneNode)).toEqual([705.6, 14.4]);
    // WITH one, the seam lands on the stop the band really allows at 720px — 0.75, not 0.98.
    const restored = restoredTree("proj-a", ["t1", "t2"], host) as PaneNode;
    expect(widths(restored)).toEqual([540, 180]);
    expect(Math.min(...widths(restored))).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX);
    // …and what comes back is still a valid tree, not a repaired-looking wreck.
    expect(isPaneNode(restored)).toBe(true);
    expect(paneTerminalIds(restored)).toEqual(["t1", "t2"]);
  });

  it("repairs the OTHER end too, and leaves a legitimate ratio exactly where it was", () => {
    persistRatio("proj-min", MIN_STORED_RATIO); // 0.02 → a 14.40px FIRST pane
    expect(widths(restoredTree("proj-min", ["t1", "t2"], host) as PaneNode)).toEqual([180, 540]);
    persistRatio("proj-mid", 0.05); // 36.00px — the 15..85 band never even sees it
    expect(widths(restoredTree("proj-mid", ["t1", "t2"], host) as PaneNode)).toEqual([180, 540]);
    // A ratio the band allows AT THIS HOST is not touched. That is narrower than "it does not
    // normalise", and the difference is measurable: a ratio legitimately outside 15..85 (the
    // saturated regime) stops being allowed the moment a pane CLOSE drops its subtree's demand, and
    // the restore then pulls it back onto the ceiling — 108px of travel at 1920×1080 on a layout
    // whose every pane was already over the floor (`pane-geometry.test.ts`, "RE-CASTS a legitimate
    // out-of-band seam"). What is asserted here is the common case: inside the band ⇒ untouched.
    persistRatio("proj-ok", 0.4);
    expect(widths(restoredTree("proj-ok", ["t1", "t2"], host) as PaneNode)).toEqual([288, 432]);
  });

  // REFERENCE IDENTITY IS NOT CLAIMED HERE, and it deliberately no longer says it is (review round
  // 4): `restorePaneLayout` reads the entry through `JSON.parse`, so every node it hands back is a
  // fresh object whatever the compaction does — no assertion on this surface could ever tell a
  // returned-by-reference subtree from a rebuilt one. The identity property IS load-bearing (it is
  // `compactPaneRatios`' fixpoint stop condition) and it is asserted with `toBe` where it can be
  // seen: `pane-geometry.test.ts`, "hands a healthy tree back BY REFERENCE". What this pins is the
  // measured host's HARMLESSNESS on a healthy tree: passing it changes nothing at all.
  it("a healthy tree restores IDENTICALLY whether or not the host is measured", () => {
    seedGroup("proj-a", twoPanes());
    const withoutHost = restoredTree("proj-a", ["t1", "t2"]) as PaneNode;
    const withHost = restoredTree("proj-a", ["t1", "t2"], host) as PaneNode;
    expect(withHost).toEqual(withoutHost);
    expect(withHost).toEqual(storedTree("proj-a") as PaneNode);
  });

  /**
   * THE REPAIR MUST NOT PRODUCE WHAT THE NEXT LAUNCH REJECTS (review round 4, major).
   *
   * The compaction clamps through the PX band, and in the SATURATED regime that band is the raw px
   * one — which, over a 9000px-wide (or 6000px-tall) sub-box, can sit ENTIRELY above the band a
   * ratio may be STORED in. Written as-is, the restored tree is one `isPaneNode` throws away, so
   * the persist effect saves it and the NEXT launch loses the whole tiling: 5 panes become 1. The
   * fix is one call (`clampRatio`, where every other ratio writer already ends); this is the round
   * trip that used to end in a single pane.
   */
  describe("the round trip a restored tree has to survive", () => {
    /** The 5-pane / 4-seam tiling of the round-4 report, reachable by gestures alone. */
    const OVERSIZED: HostSize = { width: 9001, height: 4320 };
    const REPRO_IDS = ["t90", "t97", "t114", "t135", "t26"];
    const repro = (): PaneNode => ({
      kind: "split",
      id: "s0",
      direction: "row",
      ratio: MAX_STORED_RATIO,
      children: [
        {
          kind: "split",
          id: "s1",
          direction: "row",
          ratio: 0.5,
          children: [
            { kind: "leaf", id: "p90", terminalId: "t90" },
            {
              kind: "split",
              id: "s2",
              direction: "row",
              ratio: 0.85,
              children: [
                { kind: "leaf", id: "p97", terminalId: "t97" },
                {
                  kind: "split",
                  id: "s3",
                  direction: "row",
                  // Chosen so `s1`'s demand lands on EXACTLY 98% of a 9001px box.
                  ratio: 0.7279213874195385,
                  children: [
                    { kind: "leaf", id: "p114", terminalId: "t114" },
                    { kind: "leaf", id: "p135", terminalId: "t135" },
                  ],
                },
              ],
            },
          ],
        },
        { kind: "leaf", id: "p26", terminalId: "t26" },
      ],
    });

    it("restore → persist → restore keeps the 5 panes at 9001×4320 (it used to leave ONE)", () => {
      const stored = repro() as PaneNode;
      // This is a tree the MUTATORS can produce, not a corrupted entry: 0.98 is what `setSplitRatio`
      // WROTE when the px band asked for more than the storable one allows.
      expect(isPaneNode(stored)).toBe(true);
      seedGroup("proj-wide", stored);

      // WHY THE BAND LEAVES THE STORABLE ONE, in the numbers: the root's sub-box is 9001.0000px and
      // owes 8820.9800 + 180.0000px — payable, with 0.02px to spare — so the saturated regime hands
      // its px band back raw, and 8820.98 / 9001 is 98% to the last bit. BOTH ends land above 0.98.
      const seam = paneSeparators(stored)[0] as PaneSeparator;
      expect(seam.minFirstPx).toBeCloseTo(8820.98, 9);
      expect(seam.minSecondPx).toBe(MIN_PANE_WIDTH_PX);
      const band = separatorRatioBounds(seam, OVERSIZED);
      expect([band.min, band.max]).toEqual([0.9800000000000002, 0.980002221975336]);
      expect(band.min).toBeGreaterThan(MAX_STORED_RATIO);

      // LAUNCH 1 — the restore repairs, and what it hands back is still a STORABLE tree.
      const first = restoredTree("proj-wide", REPRO_IDS, OVERSIZED) as PaneNode;
      expect(paneCount(first)).toBe(5);
      expect(
        isPaneNode(first),
        "the repaired tree must survive the shape guard, or the NEXT launch discards the entry",
      ).toBe(true);

      // …the persist effect (`use-pane-layout.ts`) writes exactly that tree back…
      seedGroup("proj-wide", first);

      // LAUNCH 2 — the entry survives the shape guard, and the tiling comes back whole.
      expect(storedTree("proj-wide")).not.toBeNull();
      const second = restoredTree("proj-wide", REPRO_IDS, OVERSIZED) as PaneNode;
      expect(paneCount(second)).toBe(5);
      expect(paneTerminalIds(second)).toEqual(paneTerminalIds(stored));
      // …and it has SETTLED: a third launch changes nothing more.
      seedGroup("proj-wide", second);
      expect(paneRects(restoredTree("proj-wide", REPRO_IDS, OVERSIZED) as PaneNode)).toEqual(
        paneRects(second),
      );
    });

    it("the threshold is arithmetic, not luck: 8999 and 9000 were already whole, 9001 was not", () => {
      // The break opens exactly where the sub-box first becomes able to pay both demands while the
      // first side still owes over `MAX_STORED_RATIO` of it — i.e. just past
      // `MIN_PANE_WIDTH_PX / (1 − MAX_STORED_RATIO)` = 9000px. Under 9000 the seam reads as STARVED
      // and is (rightly) left alone; at 9001 it is repairable, and that is where the ratio used to
      // leave the storable band.
      for (const width of [8999, 9000, 9001]) {
        seedGroup("proj-sweep", repro() as PaneNode);
        const host: HostSize = { width, height: 4320 };
        const restored = restoredTree("proj-sweep", REPRO_IDS, host) as PaneNode;
        expect(isPaneNode(restored), `${width}×4320 must restore a storable tree`).toBe(true);
        seedGroup("proj-sweep", restored);
        expect(paneCount(restoredTree("proj-sweep", REPRO_IDS, host) as PaneNode)).toBe(5);
      }
    });
  });

  it("leaves a STARVED sub-box alone — the arbitrated best-effort regime, not a repair site", () => {
    // 340px cannot hold 2 × 180: NO ratio satisfies both sides, so moving the seam would only pick
    // a victim, and the arbitrage keeps the user's manual recovery (drag one pane back over 180px).
    persistRatio("proj-cramped", MAX_STORED_RATIO);
    const cramped: HostSize = { width: 340, height: 900 };
    const restored = restoredTree("proj-cramped", ["t1", "t2"], cramped) as PaneNode;
    expect(restored.kind === "split" && restored.ratio).toBe(MAX_STORED_RATIO);
  });

  it("repairs a NESTED corruption to a FIXPOINT, where one pass would leave a 32px pane", () => {
    // Two nested row splits at 0.98. The INNER one claims 180/0.02 = 9000px, so on a 1600px host
    // the OUTER seam reads as starved and the first pass rightly leaves it alone — which on its own
    // would restore the third pane at 32px. Repairing the inner seam drops its demand to 1200px,
    // and the second pass can then bring the outer one back into band.
    persistRawTree("proj-deep", {
      kind: "split",
      id: "s1",
      direction: "row",
      ratio: MAX_STORED_RATIO,
      children: [
        {
          kind: "split",
          id: "s2",
          direction: "row",
          ratio: MAX_STORED_RATIO,
          children: [
            { kind: "leaf", id: "p1", terminalId: "t1" },
            { kind: "leaf", id: "p2", terminalId: "t2" },
          ],
        },
        { kind: "leaf", id: "p3", terminalId: "t3" },
      ],
    });
    const roomy: HostSize = { width: 1600, height: 900 };
    const restored = restoredTree("proj-deep", ["t1", "t2", "t3"], roomy) as PaneNode;
    const boxes = widths(restored, roomy);
    expect(Math.min(...boxes)).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX);
    expect(boxes.reduce((a, b) => a + b, 0)).toBeCloseTo(roomy.width, 9);
  });
});

/**
 * THE v1 → v2 MIGRATION (review 01M0ABR6V501D0SPR2BYBVZC4A, critical).
 *
 * v1 stored ONE tree per project. A user upgrading must not lose their tiling, so a v1 entry is
 * READ and upgraded into a single unnamed, uncoloured group — same terminals, same ratios. A v1
 * tree with a single leaf becomes NO group: it was already one terminal filling the frame, which
 * the new model expresses as a solo terminal.
 */
describe("the v1 → v2 migration", () => {
  /** A v1 entry: the bare tree under version 1. */
  const persistV1 = (projectId: string, tree: PaneNode) =>
    persistRaw(projectId, tree, PANE_LAYOUT_VERSION_V1);

  it("reads a VERSION 1 entry back as ONE group — same terminals, same ratios", () => {
    const v1 = splitPane(twoPanes(), {
      paneId: "p2",
      direction: "column",
      terminalId: "t3",
      newPaneId: "p3",
      ratio: 0.31,
    }) as PaneNode;
    persistV1("proj-v1", v1);

    const collection = loadPaneCollection("proj-v1") as PaneCollection;
    expect(collection.groups).toHaveLength(1);
    const group = collection.groups[0];
    expect(group.name).toBeNull();
    expect(group.color).toBeNull();
    expect(paneTerminalIds(group.tree)).toEqual(paneTerminalIds(v1));
    // SAME RATIOS, to the last bit — the rectangles are what the user was looking at.
    expect(paneRects(group.tree)).toEqual(paneRects(v1));
    expect(isPaneCollection(collection)).toBe(true);
  });

  it("restores that migrated group through the normal reconcile", () => {
    persistV1("proj-v1", twoPanes());
    const restored = restorePaneCollection("proj-v1", ["t1", "t2"]);
    expect(restored.groups).toHaveLength(1);
    expect(paneTerminalIds(restored.groups[0].tree)).toEqual(["t1", "t2"]);
  });

  it("a SINGLE-PANE v1 entry becomes no group at all — one terminal is not a group", () => {
    persistV1("proj-solo", createPaneLayout("t1", "p1"));
    expect(loadPaneCollection("proj-solo")).toEqual(EMPTY_COLLECTION);
    expect(restorePaneCollection("proj-solo", ["t1"]).groups).toEqual([]);
    expect(migrateV1(createPaneLayout("t1", "p1"))).toEqual(EMPTY_COLLECTION);
  });

  it("an UNREADABLE v1 entry falls back to the default, exactly like an unreadable v2 one", () => {
    persistRaw("proj-junk", { kind: "split", id: "s1" }, PANE_LAYOUT_VERSION_V1);
    expect(loadPaneCollection("proj-junk")).toBeNull();
    expect(restorePaneCollection("proj-junk", ["t1", "t2"])).toEqual(EMPTY_COLLECTION);

    window.localStorage.setItem(KEY("proj-trunc"), "{not json");
    expect(loadPaneCollection("proj-trunc")).toBeNull();
    expect(restorePaneCollection("proj-trunc", ["t1", "t2"])).toEqual(EMPTY_COLLECTION);
  });

  it("a v2 entry WINS over a stale v1 one under the same key (no double read)", () => {
    savePaneCollection("proj-both", collectionOf(otherPanes()));
    expect(paneTerminalIds(storedTree("proj-both") as PaneNode)).toEqual(["t9", "t10"]);
  });
});

/**
 * THE ROUND TRIP THE WHOLE MODEL RESTS ON: restore → persist → restore must CONVERGE and lose
 * nothing — not a group, not a membership, not a terminal.
 */
describe("restore → persist → restore converges", () => {
  it("keeps every group, every membership and every terminal", () => {
    const before: PaneCollection = {
      groups: [
        { id: "g1", tree: twoPanes(), name: "build", color: 4 },
        { id: "g2", tree: otherPanes(), name: null, color: null },
      ],
    };
    const live = ["t1", "t2", "t9", "t10", "alone"];
    savePaneCollection("proj-rt", before);

    const first = restorePaneCollection("proj-rt", live);
    savePaneCollection("proj-rt", first);
    const second = restorePaneCollection("proj-rt", live);

    expect(second).toEqual(first);
    expect(second.groups.map((g) => g.id)).toEqual(["g1", "g2"]);
    expect(second.groups.map((g) => paneTerminalIds(g.tree))).toEqual([
      ["t1", "t2"],
      ["t9", "t10"],
    ]);
    expect(second.groups.map((g) => [g.name, g.color])).toEqual([
      ["build", 4],
      [null, null],
    ]);
    // The solo terminal is still solo: it belongs to no group, and none claims it.
    expect(second.groups.flatMap((g) => paneTerminalIds(g.tree))).not.toContain("alone");
  });
});
