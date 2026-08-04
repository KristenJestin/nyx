import { useEffect, useRef, useState } from "react";
import { MessageSquareIcon } from "lucide-react";

import { Button } from "@/components/ui/button";

/**
 * `<PinPopup>` — the ANCHORED comment composer, opened where the reviewer clicked in the rendered
 * page (finding `01KYW32N1BVDVTGS0WPMG2PJW2`).
 *
 * # Ported from depot, and what changed
 *
 * The mechanism is depot's (`src/web/components/prototype-pin-popup.tsx`, documented in
 * `docs/web/prototypes-workspace.md` § "Anchored pin popup"): the shim intercepts a click in the
 * sandboxed frame, posts the target and the click point to the parent, and the parent opens a small
 * popup AT that point, clamped inside the frame's wrapper. `Cmd/Ctrl+Enter` submits, `Escape`
 * cancels, the textarea autofocuses. Pure presentation — the host owns the write.
 *
 * ONE THING IS DIFFERENT, and it is the whole adaptation: depot pins on a **CSS selector** it
 * computes in the frame; nyx pins on a **NODE of the named tree**. The badge therefore shows a
 * `node_key` (`Root/Header/Title`) — the same string `prototype_feedback.node_key` stores, the same
 * one the backend re-anchors across rounds and the coder seam reads. A selector would be a second
 * identity for a node that already has one, and the first class rename would break it.
 *
 * A click can also land on markup NO node claims (the chrome, an appended stylesheet). That is not
 * swallowed: the popup says so and the comment goes in PAGE-WIDE, which is a real and useful
 * outcome, rather than pretending an anchor exists.
 */
export interface PinTarget {
  /** The `node_key` the click resolved to — `null` when it landed outside every node's region. */
  readonly nodeKey: string | null;
  /** The click point, in the FRAME's own coordinates (its top-left is the wrapper's). */
  readonly x: number;
  readonly y: number;
}

/** The popup's own box — used to clamp it inside the wrapper rather than let it spill outside. */
const POPUP_W = 288;
const POPUP_H = 172;

export function PinPopup({
  pin,
  wrapper,
  pending,
  onCancel,
  onSubmit,
}: {
  pin: PinTarget;
  /** The element the popup is positioned INSIDE (the stage) — clamped to its box. */
  wrapper: HTMLElement | null;
  pending: boolean;
  onCancel: () => void;
  onSubmit: (body: string) => void;
}) {
  const [body, setBody] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  // CLAMPED to the frame's box, so a click near the right or bottom edge does not open a composer
  // half outside it. A box that has NO measured size is not a box: clamping against it would pin
  // every popup to the top-left corner — i.e. away from the element it is about — so the click point
  // is honoured as-is until the wrapper really has dimensions.
  const box = wrapper?.getBoundingClientRect();
  const measured = box != null && box.width > 0 && box.height > 0;
  const width = measured ? Math.max(160, Math.min(POPUP_W, box.width - 16)) : POPUP_W;
  const left = measured
    ? Math.min(Math.max(8, pin.x), Math.max(8, box.width - width - 8))
    : Math.max(8, pin.x);
  const top = measured
    ? Math.min(Math.max(8, pin.y), Math.max(8, box.height - POPUP_H - 8))
    : Math.max(8, pin.y);

  const submit = (): void => {
    const trimmed = body.trim();
    if (trimmed.length === 0 || pending) return;
    onSubmit(trimmed);
  };

  return (
    <div
      data-testid="proto-pin-popup"
      role="dialog"
      aria-label="Comment on this node"
      style={{ left, top, width }}
      className="absolute z-30 rounded-lg border border-primary/60 bg-card p-2.5 shadow-xl"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          onCancel();
        }
      }}
    >
      <div className="mb-1 font-medium text-2xs uppercase tracking-wide text-muted-foreground">
        {pin.nodeKey !== null ? "Anchored on" : "No node here"}
      </div>
      {pin.nodeKey !== null ? (
        <div
          data-testid="proto-pin-node"
          className="rounded-md border border-border/60 bg-muted/40 px-1.5 py-1 font-mono text-2xs break-all text-foreground"
        >
          {pin.nodeKey}
        </div>
      ) : (
        <p data-testid="proto-pin-no-node" className="text-2xs leading-snug text-muted-foreground">
          This point belongs to no node of the composition — the comment goes in page-wide.
        </p>
      )}
      <textarea
        ref={textareaRef}
        data-testid="proto-pin-body"
        rows={3}
        value={body}
        placeholder="Comment on this node (review before dev)…"
        onChange={(e) => setBody(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
            e.preventDefault();
            submit();
          }
        }}
        className="mt-2 w-full resize-y rounded-md border border-input bg-background px-2 py-1.5 text-sm text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
      />
      <div className="mt-2 flex justify-end gap-1.5">
        <Button size="xs" variant="ghost" data-testid="proto-pin-cancel" onClick={onCancel}>
          Cancel
        </Button>
        <Button
          size="xs"
          data-testid="proto-pin-submit"
          disabled={body.trim().length === 0 || pending}
          onClick={submit}
        >
          <MessageSquareIcon aria-hidden />
          Comment
        </Button>
      </div>
    </div>
  );
}
