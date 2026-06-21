"use client";

import { mergeProps } from "@base-ui/react/merge-props";
import { useRender } from "@base-ui/react/use-render";
import type * as React from "react";

import { cn } from "@/lib/utils";

export interface LabelProps extends useRender.ComponentProps<"label"> {}

/**
 * `Label` — the in-house form label TEXT, modelled on **coss.com/ui**'s `Label`
 * (a `useRender`-based component, NOT a thin Base UI wrapper) and styled like our
 * other `ui/` primitives (`Button`, `Input`). It encapsulates the design-system
 * label look — `text-xs font-medium text-muted-foreground` — so feature forms stop
 * re-declaring that inline `<span className="text-xs font-medium …">` on every
 * field, and never reach for raw Tailwind label styling.
 *
 * Renders a native `<label>` by default (so `htmlFor` associates it with a control
 * for a11y). When it sits INSIDE another `<label>` (e.g. our `Field` wrapper, which
 * is itself the labelable element), pass `render={<span />}` so the DOM stays valid
 * — the text keeps the same look without nesting a second `<label>`.
 */
export function Label({ className, render, ...props }: LabelProps): React.ReactElement {
  const defaultProps = {
    "data-slot": "label",
    className: cn("text-xs font-medium text-muted-foreground", className),
  };

  return useRender({
    defaultTagName: "label",
    render,
    props: mergeProps<"label">(defaultProps, props),
  });
}
