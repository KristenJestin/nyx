/**
 * Parse the `{{…}}` MARKERS out of a rendered effector context (v2.8 Phase 6 — the node inspector).
 *
 * `render_agent_context` (nyx-core) emits a role manual whose `{{rules}}` marker is ALREADY resolved
 * server-side, but an orchestrator/dispatcher context still carries `{{sub_agent:<id>}}` and
 * `{{check:<id>}}` markers — the wiring the graph makes CLICKABLE (annexes `engine-graph` +
 * `context-effecteur`): a `sub_agent` marker jumps to the effector it spawns, a `check` marker jumps
 * to the gate that runs it. This splits the raw text into TEXT and MARKER segments so the inspector
 * renders each marker as a clickable chip and leaves the prose verbatim.
 */

/** A marker's kind — the three `{{…}}` families a role template can carry. */
export type MarkerKind = "sub_agent" | "check" | "rules";

/** One segment of a parsed context: literal prose, or a `{{…}}` marker (with its target `id`). */
export type ContextSegment =
  | { readonly type: "text"; readonly text: string }
  | {
      readonly type: "marker";
      readonly kind: MarkerKind;
      readonly id: string;
      readonly raw: string;
    };

/** Matches `{{sub_agent:coder}}`, `{{check:typecheck}}`, or a bare `{{rules}}`. The id is optional
 *  (a bare `{{rules}}` has none); ids are `[a-z0-9_.:-]`-ish (role slugs / command ids). */
const MARKER_RE = /\{\{(sub_agent|check|rules)(?::([^}]*))?\}\}/g;

/**
 * Split `text` into ordered {@link ContextSegment}s. Literal prose between markers is preserved
 * verbatim (whitespace included); each `{{…}}` becomes a `marker` segment carrying its `kind` + `id`.
 * A string with no markers yields a single `text` segment (or none for an empty string).
 */
export function parseContextMarkers(text: string): ContextSegment[] {
  const segments: ContextSegment[] = [];
  let lastIndex = 0;
  // `matchAll` gives every marker with its index; the text between is a literal segment.
  for (const match of text.matchAll(MARKER_RE)) {
    const start = match.index ?? 0;
    if (start > lastIndex) {
      segments.push({ type: "text", text: text.slice(lastIndex, start) });
    }
    const kind = match[1] as MarkerKind;
    const id = (match[2] ?? "").trim();
    segments.push({ type: "marker", kind, id, raw: match[0] });
    lastIndex = start + match[0].length;
  }
  if (lastIndex < text.length) {
    segments.push({ type: "text", text: text.slice(lastIndex) });
  }
  return segments;
}

/** Whether a marker is CLICKABLE (jumps to a graph node): `sub_agent` and `check` are; `rules` is a
 *  resolved, informational marker (not a jump target). */
export function isClickableMarker(kind: MarkerKind): boolean {
  return kind === "sub_agent" || kind === "check";
}
