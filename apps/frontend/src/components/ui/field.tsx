import type * as React from "react";

import { cn } from "@/lib/utils";
import { Label } from "@/components/ui/label";

/**
 * `FieldLabel` — the field's primary label text, styled like coss.com/ui's
 * `FieldLabel`. In a STACKED field this is the muted caption above the control; in
 * an INLINE field it is the slightly stronger (`text-foreground`) title of the
 * left-hand block. Rendered as a `<span>` because our `Field` is already the outer
 * `<label>` (a `<label>` must not nest another), so the text inherits the field's
 * `htmlFor` association without a second labelable element. Built on `ui/label` so
 * the `text-xs font-medium` look has a single home.
 */
export interface FieldLabelProps extends React.ComponentPropsWithoutRef<"span"> {
  /** Use the stronger `text-foreground` tone (the inline split's title). */
  emphasized?: boolean;
}

export function FieldLabel({ className, emphasized = false, ...props }: FieldLabelProps) {
  return (
    <Label
      render={<span />}
      data-slot="field-label"
      className={cn(emphasized && "text-foreground", className)}
      {...props}
    />
  );
}

/**
 * `FieldDescription` — supporting/sublabel text under the label, in the
 * design-system muted tone (coss.com/ui's `FieldDescription`). Used as the inline
 * switch's "Relaunch when the workspace opens" sublabel.
 */
export function FieldDescription({ className, ...props }: React.ComponentPropsWithoutRef<"span">) {
  return (
    <span
      data-slot="field-description"
      className={cn("text-xs text-muted-foreground", className)}
      {...props}
    />
  );
}

/**
 * `FieldError` — the inline validation/error message, in the destructive tone
 * (coss.com/ui's `FieldError`). Carries `role="alert"` so assistive tech announces
 * it the way the form's existing inline errors did.
 */
export function FieldError({ className, ...props }: React.ComponentPropsWithoutRef<"p">) {
  return (
    <p
      role="alert"
      data-slot="field-error"
      className={cn("text-xs text-destructive", className)}
      {...props}
    />
  );
}

export interface FieldProps extends Omit<React.ComponentPropsWithoutRef<"label">, "children"> {
  /** The control's `id`; sets the wrapping `<label htmlFor>` for an explicit a11y association. */
  htmlFor: string;
  /** The visible label text. */
  label: React.ReactNode;
  /**
   * `stacked` (default) — label above the control, the name/command/subfolder
   * layout. `inline` — label (+ description) on the LEFT, control on the RIGHT
   * (`justify-between`), the restart-Switch row.
   */
  layout?: "stacked" | "inline";
  /** Optional sublabel under the label (rendered via `FieldDescription`). */
  description?: React.ReactNode;
  /** Optional inline error message (rendered via `FieldError` when present). */
  error?: React.ReactNode;
  /** The form control (Input / Switch / a control + adornment row). */
  children: React.ReactNode;
}

/**
 * `Field` — the in-house form field wrapper, modelled on **coss.com/ui**'s `Field`
 * (a styled wrapper composing label + control + description/error, a shadcn
 * `FormItem`/`FormField` analogue). It owns the field SCAFFOLD so feature forms stop
 * repeating the inline `<label className="flex flex-col gap-1"><span
 * className="text-xs …">{label}</span>{control}</label>` markup on every field.
 *
 * The wrapper itself is the `<label>` carrying `htmlFor` (explicit a11y association,
 * the contract our forms + tests rely on — `aria-label` on the control stays the
 * accessible name). Two layouts cover the form's needs:
 *
 * - `stacked` (default): label caption above the control (name / command / subfolder).
 * - `inline`: label + description on the LEFT, control on the RIGHT — the
 *   "Restart on startup" Switch row.
 *
 * Values + validation stay with the caller (TanStack Form here): pass the control
 * as `children` and the live message as `error`; `Field` only renders the chrome.
 */
export function Field({
  htmlFor,
  label,
  layout = "stacked",
  description,
  error,
  className,
  children,
  ...props
}: FieldProps) {
  if (layout === "inline") {
    return (
      <label
        htmlFor={htmlFor}
        data-slot="field"
        className={cn("flex items-center justify-between gap-2", className)}
        {...props}
      >
        <span className="flex flex-col">
          <FieldLabel emphasized>{label}</FieldLabel>
          {description != null && <FieldDescription>{description}</FieldDescription>}
        </span>
        {children}
      </label>
    );
  }

  return (
    <label
      htmlFor={htmlFor}
      data-slot="field"
      className={cn("flex flex-col gap-1", className)}
      {...props}
    >
      <FieldLabel>{label}</FieldLabel>
      {children}
      {description != null && <FieldDescription>{description}</FieldDescription>}
      {error != null && <FieldError>{error}</FieldError>}
    </label>
  );
}
