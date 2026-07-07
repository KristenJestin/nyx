import { describe, expect, it } from "vitest";

import { computeGrid, hasUsableBox } from "./measure-cell";

describe("measure-cell", () => {
  describe("hasUsableBox (the 0×0 guard)", () => {
    it("rejects a null/undefined box", () => {
      expect(hasUsableBox(null)).toBe(false);
      expect(hasUsableBox(undefined)).toBe(false);
    });

    it("rejects a 0×0 box (a hidden / freshly-reattached pane)", () => {
      expect(hasUsableBox({ width: 0, height: 0 })).toBe(false);
    });

    it("rejects a box with EITHER dimension zero", () => {
      expect(hasUsableBox({ width: 100, height: 0 })).toBe(false);
      expect(hasUsableBox({ width: 0, height: 100 })).toBe(false);
    });

    it("accepts a box with both dimensions strictly positive", () => {
      expect(hasUsableBox({ width: 480, height: 300 })).toBe(true);
    });
  });

  describe("computeGrid", () => {
    it("floors width/height by the cell to cols/rows", () => {
      expect(computeGrid(800, 600, { charWidth: 8, rowHeight: 16 })).toEqual({
        cols: 100,
        rows: 37,
      });
    });

    it("clamps to at least 1×1 (never a degenerate 0-col grid)", () => {
      expect(computeGrid(4, 4, { charWidth: 8, rowHeight: 16 })).toEqual({ cols: 1, rows: 1 });
    });

    it("returns 1×1 for a non-positive cell metric rather than dividing by zero", () => {
      expect(computeGrid(800, 600, { charWidth: 0, rowHeight: 0 })).toEqual({ cols: 1, rows: 1 });
    });
  });
});
