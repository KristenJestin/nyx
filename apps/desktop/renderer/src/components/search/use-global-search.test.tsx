import { renderHook, waitFor } from "@testing-library/react";
import { bridgeFake, mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import type { GlobalSearchAnswer, GlobalSearchHit } from "@/bridge/contract";

import { useGlobalSearch } from "./use-global-search";

function hit(id: string): GlobalSearchHit {
  return {
    kind: "spec",
    id,
    project_id: "p1",
    project_name: "Alpha",
    workspace_id: null,
    workspace_name: null,
    spec_id: null,
    terminal_id: null,
    label: id,
    detail: null,
    extract: null,
    human_key: null,
    score: 1,
    updated_at: 10,
  };
}

/** The wire ANSWER: the hits, plus the install's `knowledge_empty` verdict. */
function answer(hits: GlobalSearchHit[], knowledgeEmpty = false): GlobalSearchAnswer {
  return { hits, knowledge_empty: knowledgeEmpty };
}

/** A promise a test resolves by hand — the only way to make responses settle OUT OF ORDER. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

/** How many `global_search` calls the fake has recorded so far. */
function searchCalls(): { command: string; args?: Record<string, unknown> }[] {
  return bridgeFake().calls.filter((c) => c.command === "global_search");
}

describe("useGlobalSearch", () => {
  it("pulls the SUGGESTIONS with an empty query on open, and reports the `empty` state", async () => {
    mockIPC((cmd) => (cmd === "global_search" ? answer([hit("s1"), hit("s2")]) : undefined));
    const { result } = renderHook(() => useGlobalSearch("", true));
    // Waited on the HITS rather than on the state: `empty` is now also the state a first request
    // starts from (it no longer passes through a `loading` state on its way), so the state alone
    // no longer tells "answered" from "about to ask".
    await waitFor(() => expect(result.current.hits).toHaveLength(2));
    expect(result.current.state).toBe("empty");
    expect(result.current.hits.map((h) => h.id)).toEqual(["s1", "s2"]);
    // The suggestions ARE the search: same kind, same rendering path, an empty `query` arg.
    expect(searchCalls()[0]?.args).toEqual({ query: "" });
  });

  // REVIEW-FINDING (« EmptyState "base vierge" déclenché sur une base NON vide ») : the flag is
  // the BACKEND's answer and is carried through untouched — the hook never derives it from `hits`,
  // which is capped and therefore cannot answer "does this install hold a Spec at all".
  it("carries the backend's `knowledge_empty` through, whatever the suggestions hold", async () => {
    mockIPC(() => answer([hit("s1")], true));
    const { result } = renderHook(() => useGlobalSearch("", true));
    await waitFor(() => expect(result.current.knowledgeEmpty).toBe(true));
    expect(result.current.hits).toHaveLength(1);
  });

  it("issues NOTHING while the palette is closed", async () => {
    mockIPC(() => answer([]));
    renderHook(() => useGlobalSearch("", false));
    await Promise.resolve();
    expect(searchCalls()).toHaveLength(0);
  });

  it("answers a ONE-CHARACTER query WITHOUT touching the backend", async () => {
    mockIPC(() => answer([hit("s1")]));
    const { result } = renderHook(() => useGlobalSearch("a", true));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(searchCalls()).toHaveLength(0);
  });

  // REVIEW-FINDING (« ça marche pas pendant le loading j'ai quand même la page vide » — the input
  // read `sc` while the body announced `No results for s`) : the sub-floor branch used to SETTLE an
  // answer — `state: "no-results"`, `hits: []`, `answered: q` — so the first letter typed flipped
  // the palette to a dead end although no search had been run, and the second letter's
  // stale-while-revalidate then faithfully preserved that false verdict.
  //
  // NOT ASKED is not FOUND NOTHING. Below the floor nothing settles at all, so what is on screen —
  // the suggestions, at that point in the sequence — simply stays.
  describe("a query BELOW THE FLOOR is not a verdict", () => {
    it("the FIRST LETTER keeps the suggestions, and pronounces no dead end", async () => {
      mockIPC((_cmd, args) =>
        answer((args?.query as string) === "" ? [hit("s1"), hit("s2")] : [hit("typed")]),
      );
      const { result, rerender } = renderHook(({ q }) => useGlobalSearch(q, true), {
        initialProps: { q: "" },
      });
      await waitFor(() => expect(result.current.hits).toHaveLength(2));
      expect(result.current.state).toBe("empty");

      rerender({ q: "s" });
      await waitFor(() => expect(result.current.loading).toBe(false));
      // THE PROPERTY: the body was not touched — same state, same hits, same answered query.
      expect(result.current.state).toBe("empty");
      expect(result.current.hits.map((h) => h.id)).toEqual(["s1", "s2"]);
      expect(result.current.answered).toBe("");
      // …and the floor still holds: one character reaches no backend.
      expect(searchCalls()).toHaveLength(1);
    });

    it("ERASING BACK to one letter keeps the results that were there", async () => {
      mockIPC((_cmd, args) => answer((args?.query as string) === "ab" ? [hit("found")] : []));
      const { result, rerender } = renderHook(({ q }) => useGlobalSearch(q, true), {
        initialProps: { q: "ab" },
      });
      await waitFor(() => expect(result.current.state).toBe("results"));

      rerender({ q: "a" });
      await waitFor(() => expect(result.current.loading).toBe(false));
      expect(result.current.state).toBe("results");
      expect(result.current.hits.map((h) => h.id)).toEqual(["found"]);
      expect(result.current.answered).toBe("ab");
      expect(searchCalls()).toHaveLength(1);
    });

    it("a REAL no-result — two characters that find nothing — is still a dead end, and names its query", async () => {
      mockIPC(() => answer([]));
      const { result } = renderHook(() => useGlobalSearch("zz", true));
      await waitFor(() => expect(result.current.state).toBe("no-results"));
      expect(result.current.answered).toBe("zz");
      expect(searchCalls()).toHaveLength(1);
    });

    it("ERASING EVERYTHING goes back to the suggestions", async () => {
      mockIPC((_cmd, args) =>
        answer((args?.query as string) === "" ? [hit("s1")] : [hit("found")]),
      );
      const { result, rerender } = renderHook(({ q }) => useGlobalSearch(q, true), {
        initialProps: { q: "ab" },
      });
      await waitFor(() => expect(result.current.state).toBe("results"));

      rerender({ q: "" });
      await waitFor(() => expect(result.current.state).toBe("empty"));
      expect(result.current.hits.map((h) => h.id)).toEqual(["s1"]);
      expect(result.current.answered).toBe("");
    });
  });

  it("debounces a typed query and reports `results` / `no-results`", async () => {
    mockIPC((_cmd, args) => answer((args?.query as string) === "found" ? [hit("s1")] : []));
    const { result, rerender } = renderHook(({ q }) => useGlobalSearch(q, true), {
      initialProps: { q: "found" },
    });
    // REVIEW-FINDING (« Chargement dans le champ de saisie ») : the in-flight request is reported
    // as `loading`, and NO LONGER as a state — `state` was asserted to be `"loading"` here, which
    // is precisely what made the wait exclusive of the answer. It now still says what the last
    // settled answer was (nothing yet, on the very first request).
    expect(result.current.loading).toBe(true);
    expect(result.current.state).toBe("empty");
    await waitFor(() => expect(result.current.state).toBe("results"));
    expect(result.current.loading).toBe(false);
    expect(result.current.hits.map((h) => h.id)).toEqual(["s1"]);
    rerender({ q: "missing" });
    await waitFor(() => expect(result.current.state).toBe("no-results"));
    expect(result.current.hits).toHaveLength(0);
    expect(result.current.answered).toBe("missing");
  });

  // REVIEW-FINDING (« Chargement dans le champ de saisie, et contenu conservé pendant une requête
  // en vol ») : the answer OUTLIVES the query that produced it. Nothing is blanked while a new
  // request is out — which is what stops the palette blinking once per letter at a 180 ms debounce
  // — and the three fields that describe the answer move TOGETHER, at the instant it lands.
  it("KEEPS the settled answer while the next request is in flight, and swaps it in one go", async () => {
    const first = deferred<GlobalSearchAnswer>();
    const second = deferred<GlobalSearchAnswer>();
    mockIPC((_cmd, args) => ((args?.query as string) === "ab" ? first.promise : second.promise));

    const { result, rerender } = renderHook(({ q }) => useGlobalSearch(q, true), {
      initialProps: { q: "ab" },
    });
    await waitFor(() => expect(searchCalls()).toHaveLength(1));
    first.resolve(answer([hit("old")]));
    await waitFor(() => expect(result.current.state).toBe("results"));
    expect(result.current.answered).toBe("ab");

    rerender({ q: "abc" });
    await waitFor(() => expect(result.current.loading).toBe(true));
    // THE PROPERTY: a request is out, and the previous answer is untouched — hits, state and the
    // query it answers.
    expect(result.current.hits.map((h) => h.id)).toEqual(["old"]);
    expect(result.current.state).toBe("results");
    expect(result.current.answered).toBe("ab");

    second.resolve(answer([hit("new")]));
    await waitFor(() => expect(result.current.hits.map((h) => h.id)).toEqual(["new"]));
    expect(result.current.answered).toBe("abc");
    expect(result.current.loading).toBe(false);
  });

  it("reports `error` when the backend rejects", async () => {
    mockIPC(() => {
      throw new Error("boom");
    });
    const { result } = renderHook(() => useGlobalSearch("query", true));
    await waitFor(() => expect(result.current.state).toBe("error"));
  });

  it("LATEST REQUEST WINS: a late answer never replaces the newest results", async () => {
    // The classic palette defect: a slow answer to `ab` lands AFTER the fast answer to `abc` and
    // repaints the list with results that no longer match what is typed.
    const slow = deferred<GlobalSearchAnswer>();
    const fast = deferred<GlobalSearchAnswer>();
    mockIPC((_cmd, args) => ((args?.query as string) === "ab" ? slow.promise : fast.promise));

    const { result, rerender } = renderHook(({ q }) => useGlobalSearch(q, true), {
      initialProps: { q: "ab" },
    });
    // Let the FIRST request actually be issued before the query moves on — otherwise the debounce
    // alone would have swallowed it and the ordering guard would never be exercised.
    await waitFor(() => expect(searchCalls()).toHaveLength(1));
    rerender({ q: "abc" });
    await waitFor(() => expect(searchCalls()).toHaveLength(2));

    fast.resolve(answer([hit("newest")]));
    await waitFor(() => expect(result.current.state).toBe("results"));
    expect(result.current.hits.map((h) => h.id)).toEqual(["newest"]);

    // …and NOW the stale one lands.
    slow.resolve(answer([hit("stale")]));
    await Promise.resolve();
    await Promise.resolve();
    expect(result.current.hits.map((h) => h.id)).toEqual(["newest"]);
    expect(result.current.state).toBe("results");
  });
});
