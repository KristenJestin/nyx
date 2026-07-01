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
 * The toast list — reads the live `toasts` off the manager (via `useToastManager`)
 * and renders one themed `<Toast.Root>` each. Must be a child of `Toast.Provider`.
 */
function ToastList() {
  const { toasts } = Toast.useToastManager();
  return (
    <>
      {toasts.map((t) => {
        const variant = variantOf(t.data as ToastData | undefined);
        const style = VARIANT_STYLES[variant];
        const Icon = style.icon;
        return (
          <Toast.Root
            key={t.id}
            toast={t}
            className={cn(
              // Stacked, swipe-dismissible card themed with the card tokens. Base UI
              // stacks these from the bottom-right via CSS vars on the viewport.
              "absolute right-0 bottom-0 left-auto z-50 w-[min(22rem,calc(100vw-2rem))]",
              "flex items-start gap-3 rounded-lg border border-border bg-card px-4 py-3 text-card-foreground shadow-lg",
              "[transform:translateX(var(--toast-swipe-movement-x))_translateY(calc(var(--toast-swipe-movement-y)+calc(min(var(--toast-index),10)*-15px)))_scale(calc(max(0,1-(var(--toast-index)*0.1))))]",
              "select-none transition-[transform,opacity] [transition-duration:0.25s] [transition-timing-function:cubic-bezier(0.22,1,0.36,1)]",
              "data-[ending-style]:opacity-0 data-[starting-style]:opacity-0",
              "data-[starting-style]:[transform:translateY(150%)]",
              "data-[ending-style]:[transform:translateY(150%)]",
              "after:absolute after:bottom-full after:left-0 after:h-3 after:w-full after:content-['']",
            )}
            data-variant={variant}
          >
            <Icon
              aria-hidden
              className={cn("mt-0.5 size-4 shrink-0", style.accent, style.spin && "animate-spin")}
            />
            <div className="flex min-w-0 flex-1 flex-col gap-0.5">
              <Toast.Title className="text-sm font-medium break-words text-foreground" />
              <Toast.Description className="text-xs break-words text-muted-foreground" />
            </div>
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
