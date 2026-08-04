import { useCallback, useState } from "react";

import { errorText, isBridgeError, nyxBridge } from "@/bridge";
import { toast } from "@/components/ui/toast";

import { FEEDBACK_AUTHOR_USER } from "./proto-types";

/**
 * `useProtoActions` — the UI-driven PROTOTYPE writes (T26/T27): the user's RETENU election
 * (`proto_elect` / `proto_election_clear`) and the pre-dev COMMENTS (`proto_comment_add` /
 * `proto_comment_resolve`), over the sidecar `proto_*` commands — the SAME authority the MCP proto
 * tools serve (PDR-0005). Each write emits `prototypes://changed`, so the reader (`useProtoTree`)
 * re-pulls the whole tree and the UI reflects the new state with NO optimistic patching here (the
 * same live pattern as `useComponents`).
 *
 * # What is NOT here: opening a round
 *
 * This hook used to carry `rollback` (`proto_round_rebase`), which the rounds menu called to "go back
 * to this round". Rebasing OPENS A NEW ROUND, and a round is WORK — it exists so the agent can apply
 * a correction to the previous one (finding `01KYW32N1BVDVTGS0WPMG2PJW2`). The UI does not drive the
 * work; it SHOWS it and it DECIDES. So electing stays, commenting stays, and creating a round left:
 * the history is read-only ({@link RoundHistory}) and the agent owns the write through its own
 * `proto_*` authority.
 *
 * The Phase-4 FREEZE guard is honoured: electing on a design-LOCKED round is REFUSED by the backend
 * with a `FROZEN` command error — we CATCH it and turn it into a clear ORIENTATION, surfaced both as
 * a persistent inline {@link ProtoNotice} and a toast, never a crash.
 */

/**
 * A discreet inline NOTICE surfaced after an election write — the deterministic, persistent
 * counterpart of the toast. `frozen` is the FREEZE-guard orientation (election frozen → open a new
 * round); `error` is any other write failure.
 */
export interface ProtoNotice {
  readonly kind: "frozen" | "error";
  readonly message: string;
  /**
   * The BACKEND'S OWN SENTENCE, when the failure carried one (finding `01KYVT6HVA260DTAEREQA4DFJN`).
   *
   * `message` says which gesture failed; this says WHY, in the words of the authority that refused —
   * and it is displayed, not only whispered into a toast that fades. A refusal whose only readable
   * trace disappears after four seconds is a refusal the reviewer cannot act on.
   */
  readonly detail?: string;
}

/** The FREEZE-guard orientation shown when electing on a design-locked (presented) round is refused. */
export const FROZEN_ORIENTATION =
  "Election frozen: this round is presented (locked). The agent opens the next round; what is retained here stands.";

/** A caught write value is the Phase-4 FREEZE guard when its backend message names it (`FROZEN`). */
function isFrozenRefusal(e: unknown): boolean {
  return isBridgeError(e) && /FROZEN/i.test(e.message);
}

export interface ProtoActions {
  /** A write is in flight — gate the controls to avoid concurrent mutations. */
  readonly pending: boolean;
  /** The last election write's inline notice (FREEZE orientation / error), or `null`. */
  readonly notice: ProtoNotice | null;
  /** Dismiss the inline election notice. */
  readonly dismissNotice: () => void;
  /** POSE the user's RETENU on a variant of a page in the current round (`proto_elect`). */
  readonly elect: (roundId: string, pageId: string, variantId: string) => Promise<void>;
  /** RETIRE the user's RETENU for a page (reversible — `proto_election_clear`). */
  readonly clearElection: (roundId: string, pageId: string) => Promise<void>;
  /**
   * ADD a pre-dev review comment ANCHORED to a tree NODE (`proto_comment_add`, T28). Authored as the
   * USER (`authorKind: "user"`). A PURE pre-dev artifact — it is NEVER wired into any coder context.
   *
   * `originVariantId` says WHERE it was raised, and nothing else does (finding
   * `01KZ3JN72TM2XGBW2CTQRCBNER`): `prototype_feedback.origin_variant_id` is the ONLY thing that
   * attaches a comment to a page — the read model carries it, the agent's own MCP tool fills it, and
   * the per-page count is already computed off it. This renderer never sent it, so every comment it
   * wrote landed page-less: the same thread showed on every page, counter included, the rail's
   * per-page badges stayed at zero, and a remark pinned on a button that exists in one variant read
   * identically beside the two variants that have no such button.
   */
  readonly addComment: (
    prototypeId: string,
    body: string,
    nodeKey: string | null,
    originVariantId?: string | null,
  ) => Promise<void>;
  /** RESOLVE a pre-dev comment (`proto_comment_resolve`, T28). */
  readonly resolveComment: (feedbackId: string, resolutionNote?: string) => Promise<void>;
}

export function useProtoActions(): ProtoActions {
  const [pending, setPending] = useState(false);
  const [notice, setNotice] = useState<ProtoNotice | null>(null);
  const dismissNotice = useCallback(() => setNotice(null), []);

  // Run an ELECTION write (elect/clear): a FROZEN refusal becomes the orientation notice; any other
  // failure a plain inline error. Both also toast for prominence (Base UI toast needs no provider).
  const election = useCallback(
    async (invoke: () => Promise<unknown>, failMessage: string): Promise<void> => {
      setPending(true);
      setNotice(null);
      try {
        await invoke();
      } catch (e) {
        if (isFrozenRefusal(e)) {
          setNotice({ kind: "frozen", message: FROZEN_ORIENTATION });
          toast.warning("Election frozen", { description: FROZEN_ORIENTATION });
        } else {
          const detail = errorText(e);
          setNotice({ kind: "error", message: failMessage, detail });
          toast.error(failMessage, { description: detail });
        }
      } finally {
        setPending(false);
      }
    },
    [],
  );

  const elect = useCallback(
    (roundId: string, pageId: string, variantId: string): Promise<void> =>
      election(
        () => nyxBridge.invoke<unknown>("proto_elect", { roundId, pageId, variantId }),
        "The variant could not be retained.",
      ),
    [election],
  );

  const clearElection = useCallback(
    (roundId: string, pageId: string): Promise<void> =>
      election(
        () => nyxBridge.invoke<unknown>("proto_election_clear", { roundId, pageId }),
        "The selection could not be cleared.",
      ),
    [election],
  );

  // ADD a pre-dev comment anchored to a node. The author is the USER (never the agent — the agent
  // comments via its own MCP tool). This is a PRE-DEV REVIEW artifact: it is stored on the prototype
  // and surfaced here, but NEVER transmitted to any coder context (PDR-0009). No optimistic patch —
  // the tree re-pulls on `prototypes://changed` and the new comment appears.
  const addComment = useCallback(
    async (
      prototypeId: string,
      body: string,
      nodeKey: string | null,
      originVariantId: string | null = null,
    ): Promise<void> => {
      const trimmed = body.trim();
      if (trimmed.length === 0) return;
      setPending(true);
      try {
        await nyxBridge.invoke<unknown>("proto_comment_add", {
          prototypeId,
          body: trimmed,
          nodeKey: nodeKey ?? undefined,
          // WHERE it was raised — omitted rather than sent as `null` when the page has no variant
          // at all, which is the one case there is genuinely no origin to record.
          originVariantId: originVariantId ?? undefined,
          authorKind: FEEDBACK_AUTHOR_USER,
        });
      } catch (e) {
        const detail = errorText(e);
        toast.error("The comment could not be added.", { description: detail });
      } finally {
        setPending(false);
      }
    },
    [],
  );

  const resolveComment = useCallback(
    async (feedbackId: string, resolutionNote?: string): Promise<void> => {
      setPending(true);
      try {
        await nyxBridge.invoke<unknown>("proto_comment_resolve", { feedbackId, resolutionNote });
      } catch (e) {
        const detail = errorText(e);
        toast.error("The comment could not be resolved.", { description: detail });
      } finally {
        setPending(false);
      }
    },
    [],
  );

  return {
    pending,
    notice,
    dismissNotice,
    elect,
    clearElection,
    addComment,
    resolveComment,
  };
}
