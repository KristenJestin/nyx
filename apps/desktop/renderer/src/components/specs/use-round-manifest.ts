import { useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { RoundManifestPage } from "./proto-types";

/**
 * `useRoundManifest` — the MANIFEST of ONE round (`proto_round_manifest`, finding
 * `01KZ3AP34K1PKS6W0E75RP7DF6`): per page pinned in it, which page VERSION it froze and which
 * variant was RETAINED there.
 *
 * It is the one thing the workbench lacked to make a past round consultable. `proto_tree` already
 * hands over every version and every variant a page ever had — only the ELECTION in it is resolved
 * against the CURRENT round. This read supplies the other rounds' half, and the tree is re-sourced
 * from it ([`treeAtRound`]).
 *
 * `roundId === null` (= looking at the current round) reads NOTHING: consultation is opt-in, and
 * the current round's manifest is already in the tree.
 *
 * # It is deliberately NOT wired to `prototypes://changed`
 *
 * A past round does not change — the agent opens a NEW one to apply a correction, it never rewrites
 * a previous manifest, and the UI writes nothing at all here. Re-pulling on every proto event would
 * be a subscription that can only ever answer the same bytes. The tree beside it stays live, which
 * is where liveness belongs.
 */
export interface UseRoundManifest {
  /** The round's pages, in manifest order. `null` = nothing asked for, or not answered yet. */
  manifest: readonly RoundManifestPage[] | null;
  /** The read FAILED — the consulted round cannot be shown, and the caller must not pretend it can. */
  error: boolean;
}

export function useRoundManifest(roundId: string | null): UseRoundManifest {
  const [manifest, setManifest] = useState<readonly RoundManifestPage[] | null>(null);
  const [error, setError] = useState(false);

  useEffect(() => {
    if (roundId === null) {
      setManifest(null);
      setError(false);
      return;
    }
    // A LATE answer for a round that is no longer the one being consulted must not land: the user
    // can walk the history faster than the reads come back.
    let stale = false;
    setManifest(null);
    setError(false);
    void nyxBridge
      .invoke<readonly RoundManifestPage[] | null>("proto_round_manifest", { roundId })
      .then((res) => {
        if (!stale) setManifest(res ?? []);
      })
      .catch(() => {
        if (!stale) setError(true);
      });
    return () => {
      stale = true;
    };
  }, [roundId]);

  return { manifest, error };
}
