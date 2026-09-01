const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * The AGE readout at the right of a home row — how long ago the row's timestamp was, in the
 * shortest form that is still true (`now` · `18m` · `1h` · `3d`).
 *
 * One unit, never two: the age is a glance, not a duration to reason about, and it shares its
 * gutter with nothing else. A timestamp in the FUTURE (a clock skew between the DB write and the
 * renderer) reads `now` rather than a negative age.
 *
 * Pure — `now` is injected so the unit test does not race the clock.
 */
export function formatAge(timestampMs: number, now: number): string {
  const elapsed = now - timestampMs;
  if (!Number.isFinite(elapsed) || elapsed < MINUTE) return "now";
  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)}m`;
  if (elapsed < DAY) return `${Math.floor(elapsed / HOUR)}h`;
  return `${Math.floor(elapsed / DAY)}d`;
}
