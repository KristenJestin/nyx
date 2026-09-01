import { useCallback, useEffect, useRef, useState } from "react";
import { errorText, nyxBridge } from "@/bridge";

/**
 * The broadcast the pending queue moves on (`system_approvals::SYSTEM_APPROVALS_CHANGED_TOPIC`).
 *
 * It is emitted from BOTH sides, which is what makes one topic enough: the human's own grant/deny
 * routes emit it after the decision, and the MCP dispatcher emits it when a protected call is
 * REFUSED with `approval_required` — the one place in nyx where a refusal is the event, because
 * that refusal is exactly the moment the human has something to do.
 */
const SYSTEM_APPROVALS_CHANGED_EVENT = "system-approvals://changed" as const;

/**
 * One pending approval, as `system_approval_list` renders it.
 *
 * WHAT IS NOT HERE, by construction rather than by omission: the opaque token, and its hash. The
 * token is handed once to the agent that raised the request and is never persisted, logged or
 * rendered — the human decides on the OPERATION, not on a credential.
 */
export interface SystemApproval {
  readonly request_id: string;
  /** The protected verb, as the guard matrix names it (`add_command`, `stop_command`, …). */
  readonly operation_kind: string;
  /** The digest that names this exact operation — the queue's dedup key. */
  readonly fingerprint: string;
  /** The call's canonical arguments: WHAT the human is being asked to authorise. */
  readonly payload: unknown;
  readonly state: string;
  readonly requested_at: number;
  /** Epoch ms. Absolute: a restart of the app does not refresh the window. */
  readonly expires_at: number;
  /** A one-line description of the operation — the verb plus its most identifying argument. */
  readonly summary: string;
  /** Who asked, when the caller had an identity to give. */
  readonly requested_by: string | null;
}

export interface UseSystemApprovals {
  /** The pending requests, oldest first — the bounded attention surface's content. */
  pending: SystemApproval[];
  /** The backend's own wording when a read or a decision FAILED, else `null`. */
  error: string | null;
  /** Approve the request — ONCE. The approval authorises exactly the operation it names. */
  grant: (requestId: string) => Promise<void>;
  /** Refuse it. Terminal: the agent must ask again. */
  deny: (requestId: string) => Promise<void>;
  /** Re-read on demand. */
  refresh: () => Promise<void>;
}

/**
 * `useSystemApprovals` — the renderer's half of the SYSTÈME approval protocol.
 *
 * ## Why the human is the only one here
 *
 * An agent may read the Système services freely and start a stopped one. Everything that CHANGES
 * them — creating, importing, editing, removing, reordering, regrouping, clearing output, stopping
 * a running service, relaunching one — raises a request and waits. These are the user's own
 * machine-wide launchers, so nyx asks them; and it asks HERE, in the app, because the agent's
 * context is precisely where the answer must not come from.
 *
 * `grant` / `deny` are IPC-only db-commands with no MCP twin, which is the property that makes the
 * whole protocol worth having.
 *
 * ## Why it does not poll
 *
 * The queue moves on one broadcast, emitted by both sides (see
 * [`SYSTEM_APPROVALS_CHANGED_EVENT`]). The TTL is the only clock-driven part of the surface, and it
 * belongs to the countdown a card renders, not to this hook — the backend expires a request
 * LAZILY, on the very read below, so a list that arrives is a list of live requests.
 *
 * A decision is applied OPTIMISTICALLY (the card leaves at once) and then re-read on the broadcast:
 * the click that matters must not wait a round-trip, and the broadcast reconciles whatever the
 * backend really did — including refusing a decision that raced another window's.
 */
export function useSystemApprovals(): UseSystemApprovals {
  const [pending, setPending] = useState<SystemApproval[]>([]);
  const [error, setError] = useState<string | null>(null);

  // Guards a late resolve after unmount (StrictMode's double-mount included) from writing state.
  const aliveRef = useRef(true);
  useEffect(() => {
    aliveRef.current = true;
    return () => {
      aliveRef.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const rows = await nyxBridge.invoke<SystemApproval[] | null>("system_approval_list");
      if (!aliveRef.current) return;
      setPending(rows ?? []);
      setError(null);
    } catch (e) {
      if (!aliveRef.current) return;
      setError(errorText(e, "The pending approvals could not be read."));
    }
  }, []);

  const refresh = useCallback(async () => {
    await load();
  }, [load]);

  const decide = useCallback(
    async (command: "system_approval_grant" | "system_approval_deny", requestId: string) => {
      // OPTIMISTIC: the card leaves now. The broadcast below re-reads and reconciles, so a
      // decision the backend refused (another window got there first, or the window closed)
      // brings the card back rather than leaving the surface lying.
      setPending((rows) => rows.filter((r) => r.request_id !== requestId));
      let failure: string | null = null;
      try {
        await nyxBridge.invoke(command, { requestId });
      } catch (e) {
        failure = errorText(e, "The decision could not be recorded.");
      }
      // RECONCILE FIRST, then report. `load` clears the error on a successful read, so setting the
      // failure before it would wipe the one thing the user can act on — and the card must come
      // back before the reason for its return is displayed beside it.
      await load();
      if (aliveRef.current && failure != null) setError(failure);
    },
    [load],
  );

  const grant = useCallback(
    (requestId: string) => decide("system_approval_grant", requestId),
    [decide],
  );
  const deny = useCallback(
    (requestId: string) => decide("system_approval_deny", requestId),
    [decide],
  );

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void nyxBridge
      .subscribe(SYSTEM_APPROVALS_CHANGED_EVENT, () => {
        if (torndown) return;
        void load();
      })
      .then((un) => {
        if (torndown) {
          void Promise.resolve(un()).catch(() => {});
          return;
        }
        unlisten = un;
      });
    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, [load]);

  return { pending, error, grant, deny, refresh };
}

/**
 * The remaining window of `approval`, as a short human string ("expires in 2 min", "expires in
 * 41 s", "expired"), computed against `now`.
 *
 * A pure function of two numbers, so the countdown is testable without a clock and without a
 * rendered component — the surface's only time-dependent behaviour, isolated from it.
 */
export function expiresInLabel(expiresAt: number, now: number): string {
  const remaining = expiresAt - now;
  if (remaining <= 0) return "expired";
  const seconds = Math.ceil(remaining / 1000);
  if (seconds < 60) return `expires in ${seconds} s`;
  return `expires in ${Math.ceil(seconds / 60)} min`;
}
