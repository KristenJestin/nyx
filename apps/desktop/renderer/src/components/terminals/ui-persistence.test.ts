import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  loadLastProjectId,
  loadLastTerminalId,
  loadVersionedJson,
  saveLastProjectId,
  saveLastTerminalId,
  saveVersionedJson,
} from "./ui-persistence";

interface Shape {
  readonly a: string;
}

const isShape = (value: unknown): value is Shape =>
  typeof value === "object" && value !== null && typeof (value as Partial<Shape>).a === "string";

beforeEach(() => {
  window.localStorage.clear();
});

afterEach(() => {
  vi.restoreAllMocks();
  window.localStorage.clear();
});

describe("flat id hints (non-regression)", () => {
  it("round-trips the last project and per-project terminal ids", () => {
    saveLastProjectId("proj-a");
    saveLastTerminalId("proj-a", "t1");
    saveLastTerminalId("proj-b", "t2");
    expect(loadLastProjectId()).toBe("proj-a");
    expect(loadLastTerminalId("proj-a")).toBe("t1");
    expect(loadLastTerminalId("proj-b")).toBe("t2");
  });

  it("forgets on null", () => {
    saveLastProjectId("proj-a");
    saveLastProjectId(null);
    expect(loadLastProjectId()).toBeNull();
  });
});

describe("versioned JSON", () => {
  it("round-trips a validated value", () => {
    saveVersionedJson<Shape>("k", 1, { a: "x" });
    expect(loadVersionedJson("k", 1, isShape)).toEqual({ a: "x" });
  });

  it("stores the value inside a version envelope", () => {
    saveVersionedJson<Shape>("k", 3, { a: "x" });
    expect(JSON.parse(window.localStorage.getItem("k") as string)).toEqual({
      v: 3,
      data: { a: "x" },
    });
  });

  it("returns null for a missing key", () => {
    expect(loadVersionedJson("absent", 1, isShape)).toBeNull();
  });

  it("REJECTS an unknown version (older or newer) without deleting it", () => {
    saveVersionedJson<Shape>("k", 2, { a: "x" });
    expect(loadVersionedJson("k", 1, isShape)).toBeNull();
    expect(loadVersionedJson("k", 3, isShape)).toBeNull();
    // A downgrade must not destroy the newer entry.
    expect(window.localStorage.getItem("k")).not.toBeNull();
    expect(loadVersionedJson("k", 2, isShape)).toEqual({ a: "x" });
  });

  it("rejects an unparseable or non-envelope entry instead of throwing", () => {
    window.localStorage.setItem("k", "{not json");
    expect(loadVersionedJson("k", 1, isShape)).toBeNull();
    window.localStorage.setItem("k", '"a string"');
    expect(loadVersionedJson("k", 1, isShape)).toBeNull();
    window.localStorage.setItem("k", "null");
    expect(loadVersionedJson("k", 1, isShape)).toBeNull();
    window.localStorage.setItem("k", '{"data":{"a":"x"}}');
    expect(loadVersionedJson("k", 1, isShape)).toBeNull();
  });

  it("rejects a right-version payload of the WRONG SHAPE", () => {
    window.localStorage.setItem("k", '{"v":1,"data":{"a":42}}');
    expect(loadVersionedJson("k", 1, isShape)).toBeNull();
  });

  it("forgets the key on a null value", () => {
    saveVersionedJson<Shape>("k", 1, { a: "x" });
    saveVersionedJson<Shape>("k", 1, null);
    expect(window.localStorage.getItem("k")).toBeNull();
    expect(loadVersionedJson("k", 1, isShape)).toBeNull();
  });

  it("degrades to no-memory when localStorage throws (private mode / quota)", () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("QuotaExceeded");
    });
    expect(() => saveVersionedJson<Shape>("k", 1, { a: "x" })).not.toThrow();
    setItem.mockRestore();
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("SecurityError");
    });
    expect(loadVersionedJson("k", 1, isShape)).toBeNull();
  });
});
