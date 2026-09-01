import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { ShieldAlertIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { MicroLabel } from "@/components/ui/micro-label";
import { toast } from "@/components/ui/toast";
import { expiresInLabel, useSystemApprovals, type SystemApproval } from "./use-system-approvals";

/** How often the TTL countdown re-renders. One second: the label's own resolution. */
const TICK_MS = 1_000;

/**
 * The human sentence for a protected verb — what the agent is asking to DO, in the user's terms
 * rather than in the tool's.
 *
 * Falls back to the raw verb for a name this table has not heard of, which is the honest failure:
 * a new protected operation shows up as itself instead of silently reading as something else.
 */
function titleFor(operationKind: string): string {
  const verb = operationKind.startsWith("manage_command_group.")
    ? "manage_command_group"
    : operationKind;
  switch (verb) {
    case "add_command":
      return "An agent wants to add a system command";
    case "import_commands":
      return "An agent wants to import system commands";
    case "update_command":
      return "An agent wants to edit a system command";
    case "remove_command":
    case "remove_commands":
      return "An agent wants to remove a system command";
    case "reorder_commands":
      return "An agent wants to reorder the system commands";
    case "manage_command_group":
      return "An agent wants to regroup the system commands";
    case "clear_command_output":
      return "An agent wants to clear a system command's output";
    case "stop_command":
      return "An agent wants to stop a system command";
    case "relaunch_command":
      return "An agent wants to relaunch a system command";
    // `start_command` itself is free; a start that brings its OWN environment is not — an `env`
    // changes what a fixed command line executes, so the human approves the exact map below.
    case "start_command.env":
      return "An agent wants to start a system command with its own environment";
    default:
      return `An agent wants to run ${operationKind}`;
  }
}

/**
 * The per-run ENVIRONMENT the operation carries, as `KEY=value` lines — or `null` when it carries
 * none.
 *
 * Shown IN FULL, values included, and that is the point rather than an oversight: an `env` is the
 * argument that makes a command line the human already approved execute something else
 * (`NODE_OPTIONS=--require=…`, a `PATH` pointing elsewhere), so a card that named only the keys
 * would be asking for a decision it withheld the evidence for. The "values are never echoed"
 * invariant is about what leaves towards an AGENT — this surface is the user's own screen, and it
 * is the one place the map has to be legible.
 */
function envLinesOf(approval: SystemApproval): string[] | null {
  const payload = approval.payload;
  if (payload == null || typeof payload !== "object") return null;
  const env = (payload as Record<string, unknown>).env;
  if (env == null || typeof env !== "object" || Array.isArray(env)) return null;
  const lines = Object.entries(env as Record<string, unknown>).map(
    ([key, value]) => `${key}=${typeof value === "string" ? value : JSON.stringify(value)}`,
  );
  return lines.length > 0 ? lines : null;
}

/** The TARGET line: the most identifying argument the operation carries. */
function targetOf(approval: SystemApproval): string | null {
  const payload = approval.payload;
  if (payload == null || typeof payload !== "object") return null;
  const bag = payload as Record<string, unknown>;
  for (const key of ["name", "command", "command_id", "instance_id", "group_id", "project_id"]) {
    const value = bag[key];
    if (typeof value === "string" && value.length > 0) return value;
  }
  return null;
}

// ---------------------------------------------------------------------------
// The live decision, published from the owner to the toast body
// ---------------------------------------------------------------------------

/** Everything the rendered decision needs, as of the last publish. */
interface LiveDecision {
  approval: SystemApproval;
  /** The countdown's clock, ticked by the owner. */
  now: number;
  /** How many LIVE requests wait behind this one. */
  queued: number;
  /** The backend's own wording when a decision failed, else `null`. */
  error: string | null;
  grant: () => void;
  deny: () => void;
}

/**
 * A one-slot store, module-level and outside React — the SAME shape as the toast manager it feeds.
 *
 * It exists because the two halves of this surface no longer share a React subtree: the OWNER
 * (`<SystemApprovalGate>`, on the always-mounted shell) holds the queue, the clock and the IPC,
 * while the BODY is mounted by `<Toaster>` inside the portalled toast viewport. Rather than run a
 * second `useSystemApprovals()` there — a second subscription, a second optimistic state, two
 * queues free to disagree — the owner publishes what it already computed and the body subscribes.
 */
let liveDecision: LiveDecision | null = null;
const decisionListeners = new Set<() => void>();

function publishDecision(next: LiveDecision | null): void {
  liveDecision = next;
  for (const listener of decisionListeners) listener();
}

function subscribeDecision(listener: () => void): () => void {
  decisionListeners.add(listener);
  return () => {
    decisionListeners.delete(listener);
  };
}

/**
 * `<SystemApprovalToastBody>` — the free content slot of the approval toast.
 *
 * Two decision buttons, a live countdown, the Operation / Target / Requested by table and the
 * ENVIRONMENT with its values: none of it fits `Toast.Title` + `Toast.Description`, which is
 * exactly the gap `toast.decision`'s content slot fills. It is a COMPONENT and not a rendered node
 * handed to the manager, so the countdown ticks inside it — updating the manager once a second
 * would reset the toast's own timers and churn the whole stack.
 */
function SystemApprovalToastBody() {
  const live = useSyncExternalStore(subscribeDecision, () => liveDecision);
  // THE LAST CONTENT SURVIVES THE PUBLICATION THAT EMPTIES IT. A retired request clears the store
  // and closes the toast on the same commit, and the close is an ANIMATION: without this the card
  // would spend its 250 ms exit as an empty collapsing box instead of the card the user was
  // reading. Written during render on purpose — it is memory of what was just displayed, not state
  // anything else derives from.
  const lastRef = useRef<LiveDecision | null>(null);
  if (live != null) lastRef.current = live;
  const decision = live ?? lastRef.current;
  if (decision == null) return null;
  const { approval, now, queued, error } = decision;
  const target = targetOf(approval);
  const envLines = envLinesOf(approval);

  return (
    <div data-testid="system-approval-toast" className="flex w-full min-w-0 flex-col">
      <div className="flex items-start gap-2.5">
        <span className="grid size-8 shrink-0 place-items-center rounded-md bg-warning/15 text-warning">
          <ShieldAlertIcon className="size-4" />
        </span>
        <div className="min-w-0">
          <p className="text-sm font-semibold">{titleFor(approval.operation_kind)}</p>
          <p className="mt-1 text-xs text-muted-foreground">
            These are your own machine-wide services. Nothing changes without your approval.
          </p>
        </div>
      </div>

      <dl className="mt-3 flex flex-col gap-1.5 rounded-lg border border-border bg-muted/40 p-3 text-sm">
        <div className="flex items-baseline gap-3">
          <dt className="w-20 shrink-0 text-xs text-muted-foreground">Operation</dt>
          <dd className="min-w-0 truncate font-mono text-xs text-foreground">
            {approval.operation_kind}
          </dd>
        </div>
        {target != null ? (
          <div className="flex items-baseline gap-3">
            <dt className="w-20 shrink-0 text-xs text-muted-foreground">Target</dt>
            <dd className="flex min-w-0 items-baseline gap-2">
              <span className="truncate text-xs text-foreground">{target}</span>
              <MicroLabel>system</MicroLabel>
            </dd>
          </div>
        ) : null}
        {envLines != null ? (
          <div className="flex items-baseline gap-3">
            <dt className="w-20 shrink-0 text-xs text-muted-foreground">Environment</dt>
            <dd className="flex min-w-0 flex-col gap-0.5">
              {envLines.map((line) => (
                <span key={line} className="truncate font-mono text-xs text-foreground">
                  {line}
                </span>
              ))}
            </dd>
          </div>
        ) : null}
        {approval.requested_by != null ? (
          <div className="flex items-baseline gap-3">
            <dt className="w-20 shrink-0 text-xs text-muted-foreground">Requested by</dt>
            <dd className="min-w-0 truncate text-xs text-foreground">{approval.requested_by}</dd>
          </div>
        ) : null}
      </dl>

      <p className="mt-3 text-xs leading-relaxed text-muted-foreground">
        Starting a stopped command needs nothing. Stopping, relaunching, editing or deleting a
        system command goes through here — and an approval covers this one operation only. The agent
        that asked is waiting: approving runs it now.
      </p>

      {error != null ? (
        <p role="alert" className="mt-3 text-xs text-destructive">
          {error}
        </p>
      ) : null}

      <div className="mt-3 flex items-center gap-2">
        <span className="text-xs text-muted-foreground">
          {expiresInLabel(approval.expires_at, now)}
        </span>
        {queued > 0 ? (
          <span className="text-xs text-muted-foreground">· {queued} more waiting</span>
        ) : null}
        <span className="flex-1" />
        <Button variant="ghost" size="sm" onClick={decision.deny}>
          Deny
        </Button>
        <Button size="sm" onClick={decision.grant}>
          Approve once
        </Button>
      </div>
    </div>
  );
}

/**
 * `<SystemApprovalGate>` — THE decision surface of the Système approval protocol. Renders nothing
 * itself: it OWNS the queue and drives one toast.
 *
 * ## IN the toast system, not beside it (review 01M1E7K6D81M057JG5MVV4J7QE, finding T2)
 *
 * It was a card of its own, fixed to the top-right corner, for one stated reason: an ordinary toast
 * firing at the same moment would stack in front of the decision and cover it. The observation was
 * right and the conclusion was not — that is a problem of ORDER, and paying for it with a second
 * notification surface means two places to look for one kind of interruption, two sets of
 * animations, two answers to "what is on screen right now".
 *
 * So the stack grew the three things a decision needs and the card moved in (see `toast.tsx`): a
 * free content slot, a non-dismissible notch, and PINNING — a pinned toast holds rank 0 whatever
 * lands after it, so no informative message can ever push a decision behind itself. Everything else
 * comes from the existing system rather than from here: the enter/exit animation
 * (`translateY(150%)`, 250 ms), the live-region announce, the corner, and the dedup by id — which
 * is why the toast's id is the request's FINGERPRINT, the queue's own dedup key.
 *
 * ## The bounded attention surface
 *
 * ONE card at a time: the OLDEST pending request. The rest wait behind it, and the count is
 * announced rather than stacked, because a pile of decisions is not a decision surface — it is a
 * thing to click through. The backend already orders by date, deduplicates by fingerprint and caps
 * the list, so what arrives here is at most a short queue of DISTINCT operations.
 *
 * Granted, denied and expired requests leave the pending read on the backend's side, so a decided
 * card disappears as soon as the broadcast lands — there is no "resolved" state rendered here.
 *
 * ## No accidental dismissal, and no permanent one either
 *
 * The toast is raised with `dismissible: false`: no close button, no swipe (`swipeDirection={[]}`),
 * no Escape. Dismissing is not one of the three outcomes — a card the user swipes away would leave
 * an agent waiting on a request they believe they answered.
 *
 * WAITING is that third outcome, and this component has to make it real. The backend's expiry is
 * LAZY — a request dies when someone reads it — and nothing reads on its own; the queue moves on a
 * broadcast, and no broadcast is emitted by time passing. The card is therefore retired the moment
 * its `expires_at` passes, with no gesture and no decision sent. That mechanism is UNCHANGED (a
 * tick that runs while the queue is non-empty, a filter on `expires_at`, and ONE `refresh()` per
 * newly-dead set): it fixed a real defect once, and moving the shell is no reason to write it
 * again. It is also why the toast is raised with `timeout: 0` rather than with the remaining TTL —
 * Base UI pauses toast timers on hover and on window blur, so a deadline delegated to the stack
 * would drift away from the absolute `expires_at` the two sides agree on.
 */
export function SystemApprovalGate() {
  const { pending, error, grant, deny, refresh } = useSystemApprovals();

  // The TTL countdown. Ticks while ANYTHING is queued: it drives the label AND the deadline below,
  // so it must keep running for a card the human is ignoring — that is the case it exists for. A
  // mounted-everywhere component still runs no timer when the queue is empty.
  const [now, setNow] = useState(() => Date.now());
  const queueIsUp = pending.length > 0;
  useEffect(() => {
    if (!queueIsUp) return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), TICK_MS);
    return () => window.clearInterval(id);
  }, [queueIsUp]);

  // THE DEADLINE IS HONOURED HERE, and it has to be. The backend's expiry is LAZY — a request dies
  // when someone reads it — and nothing reads on its own; the queue moves on a broadcast, and no
  // broadcast is emitted by time passing. So a card whose TTL ran out would otherwise sit in the
  // corner, undismissable by design, until the human clicked a button whose IPC would then fail.
  // Filtering client-side on the same `expires_at` the countdown already reads retires it silently,
  // and the NEXT request takes its place instead of the queue freezing behind a dead card.
  const live = pending.filter((a) => a.expires_at > now);
  const current = live[0] ?? null;

  // …and ONE re-read per distinct set of newly-dead requests, so the backend applies its lazy
  // expiry and the two sides agree. Keyed on the set rather than fired per tick: if the backend
  // disagrees (a clock skew) and keeps returning the row, the key does not change and nothing
  // loops. No grant/deny is ever sent — an expiry is not a decision.
  const expiredKey = pending
    .filter((a) => a.expires_at <= now)
    .map((a) => a.request_id)
    .join(",");
  const reconciledRef = useRef("");
  useEffect(() => {
    if (expiredKey === "" || reconciledRef.current === expiredKey) return;
    reconciledRef.current = expiredKey;
    void refresh();
  }, [expiredKey, refresh]);

  // PUBLISH BEFORE RAISING. Declared first so React runs it first on the commit that does both:
  // the toast the next effect adds must find its content already there, or it would flash empty.
  const queued = live.length - 1;
  useEffect(() => {
    publishDecision(
      current == null
        ? null
        : {
            approval: current,
            now,
            queued,
            error,
            grant: () => void grant(current.request_id),
            deny: () => void deny(current.request_id),
          },
    );
  }, [current, now, queued, error, grant, deny]);

  // ONE TOAST, keyed by the request's FINGERPRINT — the queue's own dedup key, which the toast
  // manager's dedup by id then enforces for free. Raised when a request takes the head, taken back
  // when it leaves (decided, expired, or the shell unmounting). The exit animation is the stack's.
  const fingerprint = current?.fingerprint ?? null;
  const raisedRef = useRef<string | null>(null);
  useEffect(() => {
    if (raisedRef.current === fingerprint) return;
    if (raisedRef.current != null) toast.dismiss(raisedRef.current);
    raisedRef.current = fingerprint;
    if (fingerprint != null) toast.decision({ id: fingerprint, body: SystemApprovalToastBody });
  }, [fingerprint]);
  useEffect(
    () => () => {
      if (raisedRef.current != null) {
        toast.dismiss(raisedRef.current);
        raisedRef.current = null;
      }
      publishDecision(null);
    },
    [],
  );

  return null;
}

export default SystemApprovalGate;
