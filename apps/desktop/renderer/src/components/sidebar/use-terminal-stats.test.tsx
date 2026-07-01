import { describe, expect, it } from "vitest";

import { formatBytes, formatCpuPct, formatTerminalStats } from "./use-terminal-stats";

/**
 * FEEDBACK #28 — the per-terminal CPU%/RAM display formatters. Pure functions, so they
 * are tested in isolation (the live `terminal://stats` folding is exercised by the hook
 * itself; here we pin the human-readable output the row shows).
 */
describe("formatCpuPct", () => {
  it("one decimal below 10%, whole numbers above", () => {
    expect(formatCpuPct(1.234)).toBe("1.2%");
    expect(formatCpuPct(9.95)).toBe("10.0%"); // rounds up across the 10 boundary
    expect(formatCpuPct(42.4)).toBe("42%");
    expect(formatCpuPct(380)).toBe("380%"); // multi-core tree can exceed 100%
  });

  it("zero / negative / NaN clamp to 0%", () => {
    expect(formatCpuPct(0)).toBe("0%");
    expect(formatCpuPct(-5)).toBe("0%");
    expect(formatCpuPct(Number.NaN)).toBe("0%");
  });
});

describe("formatBytes", () => {
  it("scales B → KB → MB → GB on 1024 boundaries", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2 * 1024)).toBe("2 KB");
    expect(formatBytes(340 * 1024 * 1024)).toBe("340 MB");
    expect(formatBytes(1.5 * 1024 * 1024 * 1024)).toBe("1.5 GB");
  });

  it("negative / NaN → 0 B", () => {
    expect(formatBytes(-1)).toBe("0 B");
    expect(formatBytes(Number.NaN)).toBe("0 B");
  });
});

describe("formatTerminalStats", () => {
  it("joins CPU and RAM with a middle dot", () => {
    expect(formatTerminalStats({ cpuPct: 1.2, memBytes: 340 * 1024 * 1024 })).toBe("1.2% · 340 MB");
  });
});
