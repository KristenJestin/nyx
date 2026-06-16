import { useEffect, useRef, useState } from "react";

import type { ExecState } from "./use-terminals";

/**
 * Anti-flicker threshold (review 01KV8DYXEBZ3WW33MD14N19TB3 #14). A command whose
 * ENTIRE run (start → settle) is shorter than this shows NO badge at all — instant
 * commands (`echo`, `true`, a quick `exit 1`, …) are noise, not a notification.
 * Only a command still running after this delay reveals a badge: `running` at the
 * threshold, then its `success`/`error` result when it settles. ~500 ms reads as
 * "this is actually taking a moment". One named constant, trivially tunable.
 */
export const RUNNING_BADGE_DELAY_MS = 100;

/**
 * Debounce the exec-state for DISPLAY only (PRD-2.1 finding #14) so a fast command
 * never flashes a badge — REGARDLESS of how it ends ("si < 500 ms on n'affiche
 * pas, peu importe le statut").
 *
 * Rule: a fresh `running` episode is held for `delayMs` before ANY badge appears.
 *  - STILL running after the delay → reveal `running` (blue pulse), then show the
 *    `success`/`error` result when it settles (normal notification flow).
 *  - SETTLES before the delay (an instant command) → show NOTHING: neither the
 *    `running` dot nor the settled result. The displayed state falls back to
 *    `idle`, so there is no pop→depop of any colour. This deliberately suppresses
 *    the RESULT badge too, not just the running dot.
 *
 * A record that MOUNTS already settled (e.g. an unread result restored from the DB
 * on relaunch) is shown immediately — the suppression only applies to a fast
 * running→settled transition observed LIVE, never to a restored snapshot.
 *
 * Purely a function of the raw state + a timer; it does not depend on `active` and
 * does not touch persistence/unread/exit-code (those ride the record fields
 * unchanged — only the badge's `state` PROP is debounced here).
 *
 * @param raw the authoritative exec-state from the record/event.
 * @param delayMs threshold a `running` episode must outlast to reveal a badge.
 * @returns the exec-state to render.
 */
export function useRunningDebounce(
  raw: ExecState,
  delayMs: number = RUNNING_BADGE_DELAY_MS,
): ExecState {
  // The state we currently DISPLAY. Seeds from `raw` so a record that mounts
  // already-settled/running (a DB restore) renders at once.
  const [display, setDisplay] = useState<ExecState>(raw);
  // Whether the in-flight running episode has crossed the reveal threshold.
  const revealed = useRef(false);
  // Previous raw value, to recognise a running→settled transition (an instant
  // command) vs a settled snapshot that appeared without a live running episode.
  const prevRaw = useRef<ExecState>(raw);

  useEffect(() => {
    const prev = prevRaw.current;
    prevRaw.current = raw;

    if (raw === "running") {
      // Fresh running: do NOT reveal yet — keep showing the prior state. Reveal
      // only if it is still running after the threshold. If `raw` leaves running
      // before then, this effect re-runs and its cleanup clears the timer, so the
      // badge never appeared.
      revealed.current = false;
      const timer = setTimeout(() => {
        revealed.current = true;
        setDisplay("running");
      }, delayMs);
      return () => clearTimeout(timer);
    }

    // Settled or idle: compute the single display value, then commit it ONCE (one
    // setState per effect run — no cascade).
    //  - idle → idle (nothing running).
    //  - a settle that ENDS a sub-threshold running episode (an instant command:
    //    prev was running and we never crossed the reveal threshold) → idle, so
    //    neither the running dot nor the result ever flashes.
    //  - any other settle (a running episode that WAS revealed, or a restored /
    //    initial snapshot that appeared without a live running episode) → show it.
    const next: ExecState =
      raw === "idle" || (prev === "running" && !revealed.current) ? "idle" : raw;
    revealed.current = false;
    setDisplay(next);
  }, [raw, delayMs]);

  return display;
}
