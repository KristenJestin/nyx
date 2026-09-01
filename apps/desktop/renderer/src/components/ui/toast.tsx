import { Toast, type ToastManagerUpdateOptions } from "@base-ui/react/toast";
import {
  CheckCircle2Icon,
  InfoIcon,
  Loader2Icon,
  TriangleAlertIcon,
  XCircleIcon,
  XIcon,
  type LucideIcon,
} from "lucide-react";

import { cn } from "@/lib/utils";

/**
 * `toast` — the project-wide toast system, built on **Base UI Toast** (the same
 * primitives coss.com/ui wraps): a single GLOBAL `ToastManager` created outside
 * React (`createToastManager`), a `<Toaster>` that wires it into a `Toast.Provider`
 * + `Toast.Viewport` and maps the manager's toasts to themed `Toast.Root`s, and a
 * `toast.success/error/info/warning(/promise)` helper that pushes onto the manager
 * from anywhere — event handlers, hooks, non-React modules — with NO React context
 * needed at the call-site.
 *
 * Accessibility (live-region announce, focus management, swipe/Escape dismissal) is
 * owned by Base UI; this module only adds the nyx theme (design tokens from
 * `globals.css`) and the success/error/info/warning visual variants.
 *
 * Position is **bottom-right** (the viewport is anchored there). Auto-dismiss is
 * variant-aware: success/info/warning settle after a few seconds; ERRORS stay until
 * dismissed (timeout `0`) so the real failure reason is never missed. Dedup is by
 * `id` — re-adding a toast with an existing id updates it in place (Base UI contract),
 * so a spammy handler can pass a stable id to coalesce.
 *
 * ## DECISION toasts — the one extension over "a message you read" (`toast.decision`)
 *
 * Human review `01M1E7K6D81M057JG5MVV4J7QE`, finding T2. Some notifications are not read, they are
 * ANSWERED: the Système approval asks a question an agent is blocked on. That is a different
 * *content*, not a different *surface* — a second notification stack would be two places to look
 * for one kind of interruption. So the stack grew the three things such a notification needs, and
 * nothing else:
 *
 *  1. **A free content slot.** `body` is a COMPONENT, mounted inside the toast shell in place of
 *     the title/description pair. Its own hooks, its own subscriptions — so a live countdown ticks
 *     without the manager being updated once a second (an `update` resets the auto-dismiss timer,
 *     which would make every tick refresh the very deadline it displays).
 *  2. **A non-dismissible notch.** No `Toast.Close`, no swipe, no Escape. Dismissing is not one of
 *     the outcomes of a question: a swipe would leave the asker waiting on an answer the user
 *     believes they gave. Only the owner that pushed the toast takes it back.
 *  3. **PINNING.** A decision is never pushed behind an informative message. The stack normally
 *     orders by arrival — newest in front, older ones scaled down and shifted up behind it — so an
 *     ordinary `toast.success` landing mid-decision used to take the front slot and paint over the
 *     buttons. Pinned toasts hold rank `0` whatever arrives after them, and the ordinary stack
 *     starts behind them. See {@link ToastList}.
 */

// ---------------------------------------------------------------------------
// Variants
// ---------------------------------------------------------------------------

/** The visual/semantic kind of a toast. Carried as Base UI's `type` so the parts
 *  (Root/Title/Close) can style conditionally and the live-region priority can lift
 *  for errors. */
export type ToastVariant = "success" | "error" | "info" | "warning" | "loading";

interface VariantStyle {
  /** Leading status icon. */
  icon: LucideIcon;
  /** Accent color classes for the icon + the left border rail. */
  accent: string;
  /** Whether the icon spins (loading only). */
  spin?: boolean;
}

/**
 * Per-variant presentation. Colors come exclusively from the design-system tokens
 * (`--success` / `--destructive` / `--info` / `--warning`), never hardcoded —
 * consistent with the dark, magenta-accented theme.
 */
const VARIANT_STYLES: Record<ToastVariant, VariantStyle> = {
  success: { icon: CheckCircle2Icon, accent: "text-success" },
  error: { icon: XCircleIcon, accent: "text-destructive" },
  info: { icon: InfoIcon, accent: "text-info" },
  warning: { icon: TriangleAlertIcon, accent: "text-warning" },
  loading: { icon: Loader2Icon, accent: "text-muted-foreground", spin: true },
};

/** Default auto-dismiss per variant (ms). `0` = sticky (manual dismiss only). */
const VARIANT_TIMEOUT: Record<ToastVariant, number> = {
  success: 4000,
  info: 5000,
  warning: 6000,
  error: 0, // errors stay until dismissed — the real reason must not be missed
  loading: 0, // a loading toast resolves via `promise()`, never auto-dismisses
};

// ---------------------------------------------------------------------------
// The global manager + the imperative `toast` helper
// ---------------------------------------------------------------------------

/** Custom data we attach to every toast so the renderer can resolve its variant. */
interface ToastData {
  variant: ToastVariant;
  /**
   * The FREE CONTENT SLOT: a component mounted inside the toast shell instead of the
   * title/description pair. A component and not a node, deliberately — it holds its own hooks, so
   * anything live inside it (a countdown, a queue length, an error line) re-renders on its own
   * without the toast manager being touched.
   */
  Body?: React.ComponentType;
  /** Never pushed behind another toast, and exempt from the viewport's limit. */
  pinned?: boolean;
  /** `false` removes every user-driven exit: the close button, the swipe and Escape. */
  dismissible?: boolean;
}

/** Shorthand for a Base UI toast update payload carrying our {@link ToastData}. */
type ToastUpdate = ToastManagerUpdateOptions<ToastData>;

/**
 * The ONE global manager. Created outside React so non-component code (hooks'
 * mutation handlers, the bridge layer, plain modules) can `toast.success(...)`
 * without a provider in scope. `<Toaster>` connects it to the React tree.
 */
export const toastManager = Toast.createToastManager<ToastData>();

/** Options accepted by every `toast.*` helper. */
export interface ToastOptions {
  /** Optional secondary line under the message. */
  description?: React.ReactNode;
  /**
   * A stable id. Re-using it UPDATES the existing toast in place (and refreshes its
   * timer) instead of stacking a duplicate — the dedup knob.
   */
  id?: string;
  /** Override the auto-dismiss (ms). `0` = sticky. Defaults to the variant default. */
  timeout?: number;
}

/** Map a variant + message + options to a Base UI `add` payload. */
function addToast(variant: ToastVariant, message: React.ReactNode, opts?: ToastOptions): string {
  return toastManager.add({
    title: message,
    description: opts?.description,
    type: variant,
    // Errors announce urgently; the rest politely (Base UI live-region priority).
    priority: variant === "error" ? "high" : "low",
    timeout: opts?.timeout ?? VARIANT_TIMEOUT[variant],
    id: opts?.id,
    data: { variant },
  });
}

/** Options for {@link toast.decision} — a toast that is ANSWERED rather than read. */
export interface DecisionToastOptions {
  /**
   * The stable id — MANDATORY here, unlike the message helpers. A decision toast is raised from a
   * queue, so its id is the queue's own dedup key (for the Système approvals, the request
   * fingerprint): the same question asked twice updates one toast instead of stacking two.
   */
  id: string;
  /** The component mounted in the toast's free content slot. */
  body: React.ComponentType;
  /** The accent, defaulting to `warning` — a question is not a failure. */
  variant?: ToastVariant;
}

/**
 * The imperative toast API. Each call returns the toast's id (so a caller can
 * `toastManager.close(id)` or `update` it later). Mirrors the coss.com/ui surface:
 * `toast.success`, `toast.error`, `toast.info`, `toast.warning`, plus `toast.promise`
 * for the loading→settled flow and `toast.dismiss` to close one (or all).
 */
export const toast = {
  success: (message: React.ReactNode, opts?: ToastOptions): string =>
    addToast("success", message, opts),
  error: (message: React.ReactNode, opts?: ToastOptions): string =>
    addToast("error", message, opts),
  info: (message: React.ReactNode, opts?: ToastOptions): string => addToast("info", message, opts),
  warning: (message: React.ReactNode, opts?: ToastOptions): string =>
    addToast("warning", message, opts),
  /**
   * Show a `loading` toast while `promise` is in flight, then swap it for a
   * `success` / `error` toast when it settles. `success`/`error` may be a static
   * string or a function of the resolved value / caught error. Returns the original
   * promise so the caller can still await it.
   */
  promise: <Value,>(
    promise: Promise<Value>,
    messages: {
      loading: React.ReactNode;
      success: React.ReactNode | ((value: Value) => React.ReactNode);
      error: React.ReactNode | ((err: unknown) => React.ReactNode);
    },
  ): Promise<Value> =>
    // Pin the manager's `Data` generic to `ToastData` so the success/error branches
    // are not narrowed to the loading branch's literal `variant`.
    toastManager.promise<Value, ToastData>(promise, {
      loading: {
        title: messages.loading,
        type: "loading",
        data: { variant: "loading" } satisfies ToastData,
      },
      success: (value): ToastUpdate => ({
        title:
          typeof messages.success === "function"
            ? (messages.success as (v: Value) => React.ReactNode)(value)
            : messages.success,
        type: "success",
        timeout: VARIANT_TIMEOUT.success,
        data: { variant: "success" },
      }),
      error: (err): ToastUpdate => ({
        title:
          typeof messages.error === "function"
            ? (messages.error as (e: unknown) => React.ReactNode)(err)
            : messages.error,
        type: "error",
        priority: "high",
        timeout: VARIANT_TIMEOUT.error,
        data: { variant: "error" },
      }),
    }),
  /**
   * A PINNED, NON-DISMISSIBLE toast carrying its own content — the surface for a notification the
   * user has to answer rather than read (the Système approval; see the module header).
   *
   * `timeout: 0` on purpose, and it is the one correspondence with the ordinary stack this does
   * NOT take. Handing a deadline to the toast timer would be the natural move, but Base UI PAUSES
   * every timer while the viewport is hovered or the window is unfocused
   * (`ToastViewport.handleMouseEnter` → `store.pauseTimers()`) — sensible for a message one might
   * be reading, wrong for a window that is closing on the ASKER's side whatever the mouse does. So
   * the deadline stays where it is authoritative, on the owner that pushed this toast, and the
   * toast itself never expires on its own.
   *
   * `priority: 'low'` is likewise deliberate: Base UI marks a `high` priority toast `aria-hidden`
   * and mirrors its TEXT into a live region instead — which would hide the two buttons from every
   * assistive technology. A question has to stay reachable, so it announces politely and keeps its
   * controls.
   */
  decision: (opts: DecisionToastOptions): string =>
    toastManager.add<ToastData>({
      type: opts.variant ?? "warning",
      priority: "low",
      timeout: 0,
      id: opts.id,
      data: {
        variant: opts.variant ?? "warning",
        Body: opts.body,
        pinned: true,
        dismissible: false,
      },
    }),
  /** Close one toast by id, or all toasts when no id is given. */
  dismiss: (id?: string): void => toastManager.close(id),
};

// ---------------------------------------------------------------------------
// The rendered toast list (one <Toast.Root> per manager entry)
// ---------------------------------------------------------------------------

/** Resolve a toast's variant from its custom data, defaulting to `info`. */
function variantOf(data: ToastData | undefined): ToastVariant {
  return data?.variant ?? "info";
}

/**
 * No swipe at all. Base UI reads `swipeDirection` as a list and computes
 * `swipeEnabled = swipeDirections.length > 0` (`toast/root/ToastRoot.js`), gating the four pointer
 * handlers on it — so an EMPTY list is the supported per-toast way to say "this one cannot be
 * flicked away", not a workaround. Frozen at module scope so the prop identity is stable.
 */
const NO_SWIPE: [] = [];

/**
 * The toast list — reads the live `toasts` off the manager (via `useToastManager`)
 * and renders one themed `<Toast.Root>` each. Must be a child of `Toast.Provider`.
 *
 * ## The stack's order, and the ONE thing that outranks arrival
 *
 * The manager keeps its toasts newest-first, and Base UI writes that position into `--toast-index`
 * on each root — index `0` is the front card at full scale, each rank behind it shifts up 15px and
 * loses 10% of its size. Arrival order is the right default for messages.
 *
 * It is the WRONG order for a decision. An ordinary `toast.success` firing while an approval waits
 * would take rank 0 and paint over the two buttons the user has to press — the risk that once
 * justified building the approval its own separate surface. The answer is ordering, not a second
 * surface: PINNED toasts are ranked before everything else, so a decision keeps the front slot
 * whatever lands after it, and the informative stack simply starts one rank further back.
 *
 * Three overrides carry it, all per-toast and all merged over Base UI's own props (external props
 * win in `mergeProps`, and `style` merges key by key rather than replacing):
 *
 *  - `--toast-index` becomes the PINNED-FIRST rank instead of the arrival rank;
 *  - `zIndex` descends with that rank, because paint order otherwise follows the DOM (the manager's
 *    array), which would let a card that is visually behind still cover the one in front;
 *  - `inert` is cleared on pinned toasts. Past `limit`, Base UI marks the oldest toasts `limited`
 *    and inerts them — correct for a message nobody will miss, fatal for a question, which would
 *    stay on screen with dead buttons.
 */
function ToastList() {
  const { toasts } = Toast.useToastManager();
  // The pinned-first rank. Two passes over the manager's array: pinned toasts take 0…p-1 in its own
  // order, the rest continue from p. Toasts on their way OUT are counted too — a decision that is
  // animating away while its successor comes up would otherwise share a rank with it, and two cards
  // at one rank overlap.
  const pinnedCount = toasts.filter(
    (t) => (t.data as ToastData | undefined)?.pinned === true,
  ).length;
  let nextPinned = 0;
  let nextOrdinary = pinnedCount;
  const rankOf = new Map<string, number>();
  for (const t of toasts) {
    const pinned = (t.data as ToastData | undefined)?.pinned === true;
    rankOf.set(t.id, pinned ? nextPinned++ : nextOrdinary++);
  }

  return (
    <>
      {toasts.map((t) => {
        const data = t.data as ToastData | undefined;
        const variant = variantOf(data);
        const style = VARIANT_STYLES[variant];
        const Icon = style.icon;
        const Body = data?.Body;
        const pinned = data?.pinned === true;
        const dismissible = data?.dismissible !== false;
        const rank = rankOf.get(t.id) ?? 0;
        return (
          <Toast.Root
            key={t.id}
            toast={t}
            // An empty list disables the swipe for THIS toast only (see {@link NO_SWIPE}); the
            // ordinary ones keep Base UI's default directions.
            swipeDirection={dismissible ? undefined : NO_SWIPE}
            // …and the keyboard's own dismissal with it. Base UI closes a focused toast on Escape
            // from its internal `onKeyDown`; ours runs FIRST (external handlers do, in `mergeProps`)
            // and `preventBaseUIHandler()` is the documented way to stop the one behind it.
            onKeyDown={
              dismissible
                ? undefined
                : (event) => {
                    if (event.key === "Escape") event.preventBaseUIHandler();
                  }
            }
            // Spread, not a prop with an `undefined` value: `mergeProps` overrides on KEY
            // PRESENCE, so writing `inert={undefined}` on every toast would strip the flag from
            // the limited ones too. Only a pinned toast declares the key.
            {...(pinned ? { inert: undefined } : {})}
            // Floored at 1: past fifty live toasts a bare `50 - rank` reaches zero and then goes
            // negative, which inverts the stack and — below zero — paints a card behind the
            // viewport's own background. The pinned decision holds rank 0 either way, so this
            // protects the ordinary stack, not the approval.
            style={
              { "--toast-index": rank, zIndex: Math.max(1, 50 - rank) } as React.CSSProperties
            }
            className={cn(
              // Stacked, swipe-dismissible card themed with the card tokens. Base UI
              // stacks these from the bottom-right via CSS vars on the viewport.
              "absolute right-0 bottom-0 left-auto flex w-[min(22rem,calc(100vw-2rem))]",
              "rounded-lg border border-border bg-card px-4 py-3 text-card-foreground shadow-lg",
              "[transform:translateX(var(--toast-swipe-movement-x))_translateY(calc(var(--toast-swipe-movement-y)+calc(min(var(--toast-index),10)*-15px)))_scale(calc(max(0,1-(var(--toast-index)*0.1))))]",
              "transition-[transform,opacity] [transition-duration:0.25s] [transition-timing-function:cubic-bezier(0.22,1,0.36,1)]",
              "data-[ending-style]:opacity-0 data-[starting-style]:opacity-0",
              "data-[starting-style]:[transform:translateY(150%)]",
              "data-[ending-style]:[transform:translateY(150%)]",
              "after:absolute after:bottom-full after:left-0 after:h-3 after:w-full after:content-['']",
              // The two layouts are written as ALTERNATIVES, never as a base plus an override: a
              // pair like `gap-3` / `gap-0` on one element resolves by the order Tailwind emits the
              // utilities in, not by the order they appear here. A message is an icon beside a text
              // column and is not selectable (it can be swiped); a decision owns the whole card and
              // IS selectable — an `env` line is there to be read closely.
              Body == null ? "items-start gap-3 select-none" : "flex-col",
            )}
            data-variant={variant}
            data-pinned={pinned ? "" : undefined}
          >
            {Body != null ? (
              <Body />
            ) : (
              <>
                <Icon
                  aria-hidden
                  className={cn(
                    "mt-0.5 size-4 shrink-0",
                    style.accent,
                    style.spin && "animate-spin",
                  )}
                />
                <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                  <Toast.Title className="text-sm font-medium break-words text-foreground" />
                  <Toast.Description className="text-xs break-words text-muted-foreground" />
                </div>
              </>
            )}
            {dismissible ? (
              <Toast.Close
                aria-label="Dismiss"
                className={cn(
                  "-mt-0.5 -mr-1 flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground outline-none transition-colors",
                  "hover:bg-muted hover:text-foreground",
                  "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
                )}
              >
                <XIcon className="size-3.5" />
              </Toast.Close>
            ) : null}
          </Toast.Root>
        );
      })}
    </>
  );
}

// ---------------------------------------------------------------------------
// <Toaster> — mount ONCE at the app root
// ---------------------------------------------------------------------------

/**
 * `<Toaster>` — the single mount point for the toast system. It wires the GLOBAL
 * `toastManager` into a `Toast.Provider`, then renders the bottom-right
 * `Toast.Viewport` containing the live toast list. Mount it exactly once in the app
 * shell (see `app.tsx`), NOT per window/modal — a second mount would split the toast
 * stream across providers.
 */
export function Toaster() {
  return (
    <Toast.Provider toastManager={toastManager}>
      <Toast.Portal>
        <Toast.Viewport
          className={cn(
            // Anchored bottom-right, above modals; a fixed-width stacking column.
            "fixed right-4 bottom-4 z-[100] mx-auto flex w-[min(22rem,calc(100vw-2rem))] outline-none",
            "[--toast-index:calc(var(--toast-count)-1-var(--toast-position))]",
          )}
        >
          <ToastList />
        </Toast.Viewport>
      </Toast.Portal>
    </Toast.Provider>
  );
}
