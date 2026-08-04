import { isBridgeError } from "./contract";

/**
 * THE ONE WAY THE RENDERER READS A FAILURE (finding `01KYVT6HVA260DTAEREQA4DFJN`).
 *
 * # The defect this closes, found twice
 *
 * The bridge rejects with a serialized {@link BridgeError} **OBJECT**, never with an `Error`. Every
 * surface that read a refusal with `String(e)` — or with `e instanceof Error ? e.message : String(e)`,
 * which is the same thing with extra steps — therefore printed **`[object Object]`** on screen. It
 * was found in `node-params` (a frozen design lock), fixed there; found again in `useDesignRender`
 * (EVERY refusal of `design_render`, seen in the running app), fixed there. Nobody looked anywhere
 * else, and the pattern was in eleven more files.
 *
 * It is expensive here precisely because of WHICH messages it hits. The backend's refusals are
 * written with care — a removal verdict, a pinned component, a metacharacter in a path, a saturated
 * budget, a template carrying logic, an unknown reference, a cycle, and the nineteen refusals of a
 * scoped CSS slot — and this machinery's stated principle is that A GESTURE MUST NEVER LIE. A
 * refusal rendered as `[object Object]` is a lie by negligence: the user is told something failed
 * and denied the one sentence that says what to do.
 *
 * # The rule
 *
 * ONE function. It recognizes the BRIDGE ERROR FIRST, then the shapes that can also carry a
 * readable sentence, and it **never falls back to `String(e)`** — that call is the defect itself.
 * An unreadable failure gets the caller's `fallback`, or {@link UNREADABLE_ERROR}, which at least
 * says a failure happened rather than pretending to quote it.
 *
 * A guard-rail test (`error-text.guard.test.ts`) fails the suite when a renderer file reads an error
 * any other way, so the sweep cannot rot back.
 */

/** What is shown when a failure carries no readable message at all and the caller named no fallback. */
export const UNREADABLE_ERROR = "The call failed, and the failure carried no readable message.";

/** A non-null object, without asserting anything about its fields. */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** A message worth showing — trimmed, and `null` when it is empty. */
function nonEmpty(text: string): string | null {
  const trimmed = text.trim();
  return trimmed.length > 0 ? trimmed : null;
}

/**
 * The readable sentence a caught value carries, `null` when it carries none.
 *
 * The ORDER is the whole point: {@link BridgeError} first, because that is what the bridge actually
 * rejects with and testing `instanceof Error` before it is how the backend's own wording was lost.
 */
function readableMessage(e: unknown): string | null {
  if (isBridgeError(e)) return nonEmpty(e.message);
  if (e instanceof Error) return nonEmpty(e.message);
  if (typeof e === "string") return nonEmpty(e);
  if (typeof e === "number" || typeof e === "boolean") return String(e);
  // Anything else carrying a `message` — a plain object thrown by a library, a structured-cloned
  // error that lost its prototype crossing a worker/IPC boundary.
  if (isRecord(e) && typeof e.message === "string") return nonEmpty(e.message);
  return null;
}

/**
 * The text to SHOW for anything the bridge (or any other call) rejected with.
 *
 * @param fallback what to say when the failure carries no readable message — a surface that knows
 *   the gesture ("Could not rename the project.") says more than a generic line. Optional: without
 *   it the caller gets {@link UNREADABLE_ERROR}, never `[object Object]`, and never an empty string,
 *   because an error that renders as nothing is a refusal swallowed on screen.
 */
export function errorText(e: unknown, fallback?: string): string {
  return readableMessage(e) ?? fallback ?? UNREADABLE_ERROR;
}
