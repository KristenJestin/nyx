import { useEffect, useRef, useState } from "react";

import { nyxBridge } from "@/bridge";
import type { GlobalSearchAnswer, GlobalSearchArgs, GlobalSearchHit } from "@/bridge/contract";

/**
 * `use-global-search` — the LIVE read behind the `Mod+K` palette: one `global_search` db_command
 * per settled keystroke, over the seven entity types and ALL projects.
 *
 * It is the `useIdeaSearch` pattern (`components/knowledge/use-ideas.ts`) taken as-is — debounce +
 * a `cancelled` guard in the effect teardown — with the two rules the palette adds:
 *
 *  - the SUGGESTIONS are the same call with an EMPTY query, so "just opened" and "typed something"
 *    share ONE request path and ONE rendering path (the backend answers the same `SearchAnswer`);
 *  - LATEST REQUEST WINS, explicitly. The teardown guard already drops a response whose query has
 *    been superseded, but it says so per-effect; the sequence ref below says it once, for the whole
 *    hook, and is what a test can hold the property to: a slow answer to `a` landing after the fast
 *    answer to `ab` must not repaint the palette with the older list.
 *
 * A ONE-CHARACTER query ISSUES NO REQUEST (`MIN_QUERY_LEN` in `nyx_core::search` enforces the same
 * floor backend-side, and answers empty below it): one character matches nearly everything, so it
 * is not a search but a full-table scan with a ranking on top.
 *
 * WHAT THAT EMPTY ANSWER IS NOT is a verdict. A query below the floor has not FOUND NOTHING — it
 * has NOT BEEN ASKED, and the two cannot share a rendering. It used to: the branch below wrote
 * `no-results`, so the very first letter typed flipped the palette to "No results for s" while no
 * search had happened at all, and the next keystroke then preserved that false verdict for the
 * length of its request. So the sub-floor branch now settles NOTHING: the body keeps the last
 * answer it had — the suggestions, when the user has just started typing — exactly as it does
 * while a real request is in flight.
 *
 * ## STALE-WHILE-REVALIDATE: loading is not a state of the answer
 *
 * `loading` used to BE one of the states, which made it exclusive of the others: while a request was
 * in flight the palette had no results, no suggestions and no message — it had a spinner. With a
 * 180 ms debounce restarted on every keystroke, typing three words therefore emptied and refilled
 * the panel at every letter, and the user's verdict on that was « le loading en gros au milieu c'est
 * non ».
 *
 * So the answer and the request are two different facts now, and this hook reports both:
 *
 *  - {@link GlobalSearch.state} / {@link GlobalSearch.hits} / {@link GlobalSearch.answered} are THE
 *    LAST SETTLED ANSWER. They are replaced together, at the instant the new data lands, and never
 *    blanked in between — so what is on screen always answers SOME query the user typed;
 *  - {@link GlobalSearch.loading} says a request is in flight. It is an attribute of the header, not
 *    a state of the body.
 *
 * `answered` exists for exactly that reason: `state` and `hits` now outlive the query that produced
 * them, so the "No results for X" copy cannot read X off the live input without claiming an answer
 * it does not have.
 *
 * NOTHING OF THE ORDERING RULE MOVES. `loading` is written under the SAME staleness guard as the
 * hits (`cancelled` + the sequence ref), so a late response can no more turn the spinner off than it
 * can repaint the list: the latest request remains the only authority on what is displayed. Keeping
 * the previous content is a rendering decision; it buys a stale response no way back in.
 */

/** The debounce of a typed query, in ms — the `useIdeaSearch` value, unchanged. */
export const SEARCH_DEBOUNCE_MS = 180;

/** Shortest query that reaches the backend (mirror of `nyx_core::search::MIN_QUERY_LEN`). */
export const MIN_QUERY_LEN = 2;

/**
 * The states of the palette BODY — what the last settled answer was, and nothing about whether a
 * new one is on its way:
 *  - `empty`      : no query at all ⇒ the SUGGESTIONS section (or the empty-install `EmptyState`);
 *  - `results`    : the query found something ⇒ the list;
 *  - `no-results` : the query was ASKED and came back with nothing ⇒ a dead end;
 *  - `error`      : the backend rejected.
 *
 * A query below {@link MIN_QUERY_LEN} reaches NONE of them: it is not an answer, so it does not
 * replace the one on screen (see the module header).
 *
 * `loading` was a fifth member and is not one any more: it is not an ANSWER, and making it a state
 * made it exclusive of the four that are (see the module header).
 */
export type SearchState = "empty" | "results" | "no-results" | "error";

export interface GlobalSearch {
  readonly state: SearchState;
  /** The hits of the LAST settled response — the suggestions in the `empty` state. */
  readonly hits: readonly GlobalSearchHit[];
  /**
   * The backend's own verdict on the install: no Spec, no task, no idea exists AT ALL.
   *
   * It is carried straight through from `knowledge_empty` and never inferred here, because it
   * cannot be: `hits` is capped at eight suggestions, so "no Spec among them" and "no Spec in the
   * database" look identical from this side — and the first is the ordinary state of a machine
   * where agents run.
   */
  readonly knowledgeEmpty: boolean;
  /**
   * A request is IN FLIGHT — including the debounce window before it is issued, since from the
   * user's side the two are one wait.
   *
   * It is drawn in the query INPUT and never in the body: the body keeps showing the previous
   * answer until the new one arrives.
   */
  readonly loading: boolean;
  /**
   * The trimmed query that `state` and `hits` actually ANSWER — not necessarily the one being
   * typed.
   *
   * Since the body now outlives its query, the "No results for X" copy has to name the query that
   * came back empty. Reading X off the live input would announce a verdict on a query still in
   * flight — and announce it wrongly whenever the answer turns out to have results.
   */
  readonly answered: string;
}

/**
 * Run the palette's search for `query`, live.
 *
 * `enabled` is the palette's open state: a CLOSED palette issues nothing (no boot-time IPC) and
 * KEEPS its last answer, so the close animation plays over the list the user was looking at
 * instead of over a flash of the empty state.
 */
export function useGlobalSearch(query: string, enabled: boolean): GlobalSearch {
  const [state, setState] = useState<SearchState>("empty");
  const [hits, setHits] = useState<readonly GlobalSearchHit[]>([]);
  const [knowledgeEmpty, setKnowledgeEmpty] = useState(false);
  const [answered, setAnswered] = useState("");
  const [loading, setLoading] = useState(false);
  // The id of the most recently ISSUED request. A response carrying anything older is stale by
  // definition and is dropped — whatever order the promises settle in.
  const latest = useRef(0);

  useEffect(() => {
    if (!enabled) return;
    const q = query.trim();
    // BELOW THE FLOOR NO READ IS ISSUED — and NO ANSWER IS SETTLED EITHER. `state`, `hits`,
    // `knowledgeEmpty` and `answered` are left exactly where the last real answer put them, so the
    // body keeps showing it (the suggestions, on the first letter typed). Writing `no-results` here
    // was the defect: it pronounced a verdict on a query that was never asked, and the next
    // keystroke then dutifully preserved that verdict while its own request was out.
    //
    // Still a FRESH GENERATION, so an in-flight response from a longer previous query cannot land
    // behind this keystroke and repaint the body with an answer the user has already typed past.
    //
    // COUNTED IN CODE POINTS, not in UTF-16 units, because the core counts `chars()`. `"👍".length`
    // is 2 and would have cleared a floor the backend reads as 1: the request went out, came back
    // empty by the core's own rule, and the panel pronounced "no results" on a query that was never
    // searched — the very verdict this floor exists to avoid. The shared fixture pins the VALUE of
    // the floor, not the unit it is counted in, so only counting alike keeps the two halves honest.
    const length = [...q].length;
    if (length > 0 && length < MIN_QUERY_LEN) {
      latest.current += 1;
      // Nothing is in flight and nothing ever will be for this query: the field must not spin on a
      // wait that has no end.
      setLoading(false);
      return;
    }
    const seq = (latest.current += 1);
    let cancelled = false;
    // The ONLY thing that moves before the answer lands. `state`, `hits` and `answered` keep saying
    // what they said, so the panel below does not blink through an empty frame at every keystroke.
    setLoading(true);
    // An empty query is the SUGGESTIONS pull, fired on the open edge: nothing to debounce there,
    // so its delay is 0 and the two paths stay one piece of code.
    const handle = setTimeout(
      () => {
        const args: GlobalSearchArgs = { query: q };
        void nyxBridge
          .invoke<GlobalSearchAnswer>("global_search", args)
          .then((answer) => {
            // THE SAME GUARD AS EVER, and now it covers `loading` too: a stale response can neither
            // repaint the list nor put the spinner away while a newer request is still out.
            if (cancelled || seq !== latest.current) return;
            const list = answer?.hits ?? [];
            setHits(list);
            setKnowledgeEmpty(answer?.knowledge_empty === true);
            setAnswered(q);
            setState(q === "" ? "empty" : list.length > 0 ? "results" : "no-results");
            setLoading(false);
          })
          .catch(() => {
            if (cancelled || seq !== latest.current) return;
            setHits([]);
            setKnowledgeEmpty(false);
            setAnswered(q);
            setState("error");
            setLoading(false);
          });
      },
      q === "" ? 0 : SEARCH_DEBOUNCE_MS,
    );
    return () => {
      cancelled = true;
      clearTimeout(handle);
      // The request this effect owned is now abandoned — by a new keystroke, which will set the
      // flag again on its own, or by the palette CLOSING, after which nothing runs to lower it.
      // Left standing in that second case, `loading` outlived every request it described: the
      // spinner sat in the field through the whole close animation, and the hook reported a wait
      // that had no request behind it.
      setLoading(false);
    };
  }, [enabled, query]);

  return { state, hits, knowledgeEmpty, loading, answered };
}
