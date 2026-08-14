import { describe, expect, it } from "vitest";

import { isClickableMarker, parseContextMarkers } from "./context-markers";

describe("parseContextMarkers", () => {
  it("splits sub_agent / check / rules markers out of the prose, preserving text", () => {
    const text = "spawn {{sub_agent:coder}} then run {{check:typecheck}}. Rules: {{rules}}";
    const segments = parseContextMarkers(text);
    expect(segments).toEqual([
      { type: "text", text: "spawn " },
      { type: "marker", kind: "sub_agent", id: "coder", raw: "{{sub_agent:coder}}" },
      { type: "text", text: " then run " },
      { type: "marker", kind: "check", id: "typecheck", raw: "{{check:typecheck}}" },
      { type: "text", text: ". Rules: " },
      { type: "marker", kind: "rules", id: "", raw: "{{rules}}" },
    ]);
  });

  it("returns a single text segment when there are no markers", () => {
    expect(parseContextMarkers("plain prose")).toEqual([{ type: "text", text: "plain prose" }]);
  });

  it("marks sub_agent + check clickable, but not the resolved rules marker", () => {
    expect(isClickableMarker("sub_agent")).toBe(true);
    expect(isClickableMarker("check")).toBe(true);
    expect(isClickableMarker("rules")).toBe(false);
  });
});
