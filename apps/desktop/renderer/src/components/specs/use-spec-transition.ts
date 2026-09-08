import { useCallback, useState } from "react";

import { errorText, nyxBridge, type BackendCommand } from "@/bridge";
import { toast } from "@/components/ui/toast";

import type { Spec } from "./types";

/**
 * The TARGET STATUS a lifecycle move lands on — the `to` argument of the `spec_transition`
 * db-command. It is deliberately the STATUS and not the conceptual verb (`start`/`done`/`cancel`):
 * the UI only ever knows "where the Spec should be next", and the backend owns the verb.
 *
 * Only the five targets a transition can REACH are listed. `blocked` and `archived` are stamped by
 * other paths entirely and are NOT reachable through this command, so they are not spellable here.
 */
export type SpecTransitionTarget =
  | "ready"
  | "in_progress"
  | "in_review"
  | "completed"
  | "cancelled";

/**
 * The targets the BACKEND treats as SENSITIVE and therefore refuses without an explicit
 * `userConfirmed` — `crates/nyx-core/src/db.rs`, `SpecTransition::is_sensitive`: `ready`, `done`
 * (→ `completed`), `cancel` (→ `cancelled`) and `revise` (→ `draft`), the moves that are
 * irreversible or contractual. `in_progress` and `in_review` only record that the work moved
 * along, and carry no flag.
 *
 * `draft` is in the backend's set and NOT in this one, because it is not in
 * {@link SpecTransitionTarget} at all: no button of this UI reopens a Spec, so the target is not
 * spellable here. The day one does, it belongs in both lists — the backend refuses it otherwise.
 *
 * It is otherwise the SAME set as the one that opens a confirmation DIALOG
 * (`spec-transitions.tsx`, `NEEDS_DIALOG`), which is the point: the flag asserts that a human
 * decided, so it is only ever sent on a gesture the human was actually asked about.
 */
const SERVER_SENSITIVE: readonly SpecTransitionTarget[] = ["ready", "completed", "cancelled"];

/** The arguments of ONE lifecycle move. */
export interface SpecTransitionRequest {
  readonly specId: string;
  /** Where the Spec should land. */
  readonly to: SpecTransitionTarget;
  /**
   * The dev workspace to ANCHOR the Spec on. Meaningful for `in_progress` ONLY (the `start`
   * transition poses the anchor in the same write); `null`/omitted everywhere else, and the
   * backend resolves the anchor itself.
   */
  readonly workspaceId?: string | null;
}

export interface SpecTransitionActions {
  /** A transition is in flight — the gate that shuts every button so a double-submit cannot race. */
  readonly pending: boolean;
  /** WHICH target is in flight, so the pressed button alone shows the busy state. */
  readonly target: SpecTransitionTarget | null;
  /** The last refusal, in the BACKEND's own sentence, or `null`. */
  readonly error: string | null;
  /** Drop the inline refusal (the user acknowledged it, or started another gesture). */
  readonly clearError: () => void;
  /** Run ONE transition. Resolves to the updated Spec, or `null` when the backend refused. */
  readonly transition: (request: SpecTransitionRequest) => Promise<Spec | null>;
}

/**
 * The command name, ANNOTATED rather than cast: `spec_transition` is a member of the typed
 * {@link BackendCommand} union (`bridge/contract.ts`), so the annotation is what makes a rename on
 * the backend side fail HERE, at compile time, instead of at the first click.
 */
const SPEC_TRANSITION: BackendCommand = "spec_transition";

/**
 * `useSpecTransition` — the ONE human gesture that moves a Spec through its lifecycle
 * (`draft → ready → in_progress → in_review → completed`, plus `cancelled`), over the same guarded
 * `spec_transition` authority the agent's MCP tool calls. The engine that used to drive these moves
 * is gone (PDR-0031); the Spec state machine is the only one left, and this is the human's handle
 * on it.
 *
 * # It validates NOTHING, and that is the point
 *
 * Every rule lives in `nyx_core::db::transition_spec_with_workspace` and is written exactly once:
 * the legal source→target edges, the pre-ready validations, the pre-done validations, the
 * confirmation guard-rail and the anchor guard. A refused move comes back as a `command`
 * BridgeError carrying the authority's own sentence, which this hook SURFACES VERBATIM (inline +
 * toast) rather than re-word or pre-empt — the human must be able to read WHY a `ready` was
 * refused, which is exactly the sentence the pre-ready validations produced.
 *
 * The button ROW does filter the offered targets to the legal ones (`spec-transitions.tsx`), but
 * that is an affordance, not a second copy of the guard: pressing an illegal edge would simply be
 * refused, and nothing here anticipates the refusal.
 *
 * # No optimistic patching
 *
 * The write pushes `specs://changed`, on which `use-spec-detail` already re-pulls. The caller
 * additionally `refresh()`es the detail on success so the status chip is correct the instant the
 * gesture resolves, without waiting for the broadcast's round-trip — the same existing
 * `spec_detail` channel, never a hand-patched local cache.
 */
export function useSpecTransition(): SpecTransitionActions {
  const [target, setTarget] = useState<SpecTransitionTarget | null>(null);
  const [error, setError] = useState<string | null>(null);

  const clearError = useCallback(() => {
    setError(null);
  }, []);

  const transition = useCallback(
    async ({ specId, to, workspaceId }: SpecTransitionRequest): Promise<Spec | null> => {
      setTarget(to);
      setError(null);
      const args: Record<string, unknown> = { specId, to };
      if (SERVER_SENSITIVE.includes(to)) args.userConfirmed = true;
      // The anchor is a `start` argument and nothing else's: passing it on any other target would
      // ask the backend to re-anchor a Spec that is not moving into work.
      if (to === "in_progress" && workspaceId != null) args.workspaceId = workspaceId;
      try {
        return await nyxBridge.invoke<Spec>(SPEC_TRANSITION, args);
      } catch (e) {
        const detail = errorText(e);
        setError(detail);
        toast.error("The Spec could not be moved.", { description: detail });
        return null;
      } finally {
        setTarget(null);
      }
    },
    [],
  );

  return { pending: target !== null, target, error, clearError, transition };
}
