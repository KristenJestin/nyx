import { createContext, useCallback, useContext, useMemo, useRef } from "react";
import { Dialog as BaseDialog } from "@base-ui/react/dialog";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { backdropVariants, dialogTransition, popupVariants } from "./dialog-motion";

/**
 * Shared, **Motion-driven** dialog primitives built on Base UI's `Dialog`.
 *
 * The earlier version animated via Base UI's `data-[starting-style]` /
 * `data-[ending-style]` + a Tailwind CSS transition. jsdom saw the classes and
 * the unit test was green, but in the REAL WebKitGTK release build the modal
 * still popped INSTANTLY — WebKitGTK doesn't reliably kick off a CSS transition
 * for a freshly-portaled node on the next frame (finding
 * 01KV1NPNGBACH0FY982QQN6ZZ2). So we hand the enter/exit to **Motion**, which
 * animates imperatively (rAF/WAAPI values), giving a real, visible transition in
 * WebKitGTK while Base UI keeps owning focus-trap, scroll-lock,
 * Escape/outside-press and ARIA.
 *
 * HOW IT FITS TOGETHER (the Base UI "external animation library" contract):
 *  - `<Dialog.Root>` is wired with `actionsRef`, so on close Base UI does NOT
 *    immediately unmount the popup — it keeps it mounted and waits for us to call
 *    `actions.unmount()`. (We do NOT `keepMounted` the portal: while FULLY
 *    closed the dialog must be absent from the DOM so it can't trap focus or leak
 *    content; the `actionsRef` deferral keeps it alive ONLY across the exit.)
 *  - `<DialogBackdrop>` / `<DialogPopup>` render a `motion.div` (via Base UI's
 *    `render` prop) that ANIMATES TO a target keyed on `open`: `visible` when
 *    open, `hidden` when closed. We don't use `AnimatePresence` (which removes
 *    children before they can animate out under Base UI's deferred-unmount model);
 *    instead the popup animates to `hidden` and, on that animation completing
 *    while closed, calls `actions.unmount()` to drop it. So both ENTER and EXIT
 *    visibly animate, every open/close.
 *
 * REDUCED MOTION: `dialogTransition` collapses to a zero-duration transition for
 * `prefers-reduced-motion`, so those users get an instant (but still correct)
 * open/close — the project rule for every animation. These animate ONLY chrome
 * (the dialog), never the xterm viewport.
 */

interface DialogMotionContextValue {
  open: boolean;
  /** Tell Base UI to finally unmount the popup (after Motion's exit completes). */
  unmount: () => void;
}
const DialogMotionContext = createContext<DialogMotionContextValue | null>(null);

function useDialogMotion(): DialogMotionContextValue {
  const ctx = useContext(DialogMotionContext);
  if (!ctx) {
    throw new Error("DialogBackdrop/DialogPopup must be rendered inside <Dialog.Root>");
  }
  return ctx;
}

/**
 * `<DialogRoot>` — our wrapper over Base UI's `Dialog.Root` that registers an
 * `actionsRef` (so closing keeps the popup mounted until Motion finishes the
 * exit) and exposes `open` + `unmount` to the backdrop/popup via context.
 *
 * MOTION OWNS THE EXIT: registering `actionsRef` alone is NOT enough. Base UI
 * ALSO runs its own auto-unmount on close (`useOpenChangeComplete` →
 * `getAnimations()`), and a Motion SPRING animates on rAF — not WAAPI — so Base
 * UI's `getAnimations()` sees NO running animations and would unmount the popup
 * immediately, cutting the exit short (finding 01KV1SCHYESHDHHGX4X87H97CK). We
 * therefore call `eventDetails.preventUnmountOnClose()` in `onOpenChange` on
 * every CLOSE (Base UI v1.5 API): Base UI then keeps the popup mounted and our
 * `onAnimationComplete` → `actions.unmount()` becomes the SOLE unmount trigger,
 * so the exit always animates fully. (Under reduced motion the transition is
 * zero-duration, so `onAnimationComplete` fires on the next frame and the close
 * is still effectively instant.) We still forward the caller's `onOpenChange`.
 */
function DialogRoot(
  props: Omit<BaseDialog.Root.Props, "children"> & {
    // Our dialogs compose plain element children (no Base UI payload render
    // function), so we narrow `children` to `ReactNode` to wrap them in context.
    children?: React.ReactNode;
  },
) {
  const { children, onOpenChange, ...rest } = props;
  const actionsRef = useRef<BaseDialog.Root.Actions | null>(null);
  const open = props.open ?? false;

  const value = useMemo<DialogMotionContextValue>(
    () => ({
      open,
      unmount: () => actionsRef.current?.unmount(),
    }),
    [open],
  );

  const handleOpenChange = useCallback<NonNullable<BaseDialog.Root.Props["onOpenChange"]>>(
    (next, eventDetails) => {
      // On CLOSE, stop Base UI from auto-unmounting the popup so Motion's exit
      // can run to completion (our `onAnimationComplete` then unmounts it).
      if (!next) eventDetails.preventUnmountOnClose();
      onOpenChange?.(next, eventDetails);
    },
    [onOpenChange],
  );

  return (
    <BaseDialog.Root actionsRef={actionsRef} onOpenChange={handleOpenChange} {...rest}>
      <DialogMotionContext.Provider value={value}>{children}</DialogMotionContext.Provider>
    </BaseDialog.Root>
  );
}

/**
 * `<DialogPortal>` — Base UI portal. We do NOT force `keepMounted`: the dialog
 * must be ABSENT from the DOM while fully closed (so it doesn't trap focus or
 * leak content). The popup is instead kept mounted JUST through the exit by the
 * `actionsRef` deferral in `DialogRoot` (Motion animates the exit, then
 * `onAnimationComplete` calls `actions.unmount()` to drop it).
 */
function DialogPortal(props: BaseDialog.Portal.Props) {
  return <BaseDialog.Portal {...props} />;
}

/**
 * The shared `Dialog` namespace. `Root`/`Portal` are our Motion-aware wrappers;
 * the rest are the unstyled Base UI parts so callers compose a complete dialog
 * from one import (`Dialog.Title`, `Dialog.Description`, `Dialog.Close`).
 */
export const Dialog = {
  Root: DialogRoot,
  Portal: DialogPortal,
  Trigger: BaseDialog.Trigger,
  Title: BaseDialog.Title,
  Description: BaseDialog.Description,
  Close: BaseDialog.Close,
};

/** Animated scrim: Motion fades it in on open, out on close. */
export function DialogBackdrop({ className, ...props }: BaseDialog.Backdrop.Props) {
  const reduced = useReducedMotion();
  const { open } = useDialogMotion();
  return (
    <BaseDialog.Backdrop
      className={cn("fixed inset-0 z-50 bg-background/80 backdrop-blur-sm", className)}
      render={
        <motion.div
          initial="hidden"
          animate={open ? "visible" : "hidden"}
          variants={backdropVariants}
          transition={dialogTransition(reduced)}
          // While closed the scrim must not intercept clicks (it has faded out).
          style={{ pointerEvents: open ? "auto" : "none" }}
        />
      }
      {...props}
    />
  );
}

/** Animated popup: Motion fades + subtly scales/rises it in, reverses on exit. */
export function DialogPopup({ className, ...props }: BaseDialog.Popup.Props) {
  const reduced = useReducedMotion();
  const { open, unmount } = useDialogMotion();
  // When the popup's animation settles WHILE CLOSED, the exit has finished — let
  // Base UI actually unmount it (it deferred this via `actionsRef`). Guard on
  // `open` so the ENTER animation completing never triggers an unmount.
  const onAnimationComplete = useCallback(() => {
    if (!open) unmount();
  }, [open, unmount]);
  return (
    <BaseDialog.Popup
      className={cn(
        "fixed top-1/2 left-1/2 z-50 w-[min(28rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2",
        "rounded-xl border border-border bg-popover p-5 text-popover-foreground shadow-lg outline-none",
        className,
      )}
      render={
        <motion.div
          initial="hidden"
          animate={open ? "visible" : "hidden"}
          variants={popupVariants}
          transition={dialogTransition(reduced)}
          onAnimationComplete={onAnimationComplete}
        />
      }
      {...props}
    />
  );
}
