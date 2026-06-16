import { Link2Icon } from "lucide-react";

import type { ManagedCommand } from "./use-commands";

export interface CommandSourceSectionProps {
  /** The sourced command whose provenance to show. */
  command: ManagedCommand;
  /**
   * The live on-disk script value when it DRIFTED from what this command was last
   * synced to (else `null`). Drives the passive "changed in package.json" hint —
   * informational only; adopting it is the explicit Resync in the edit form.
   */
  driftValue?: string | null;
}

/**
 * `<CommandSourceSection>` — the **read-only** package.json PROVENANCE strip shown
 * under a sourced command card when its source disclosure is expanded (review
 * T2). It states WHERE the command comes from — the `package.json` path and the
 * `package.json · scripts.<name>` reference — plus a passive drift hint when the
 * on-disk script changed. There are NO actions here: linking mutations (Resync /
 * Unlink) live in the edit form, reached via the card's Edit button. Renders
 * nothing for a hand-authored (un-sourced) command.
 */
export function CommandSourceSection({ command, driftValue }: CommandSourceSectionProps) {
  if (!command.source_script_name || !command.source_package_json_path) {
    // Hand-authored (or already-unlinked) command: no provenance to show.
    return null;
  }

  return (
    <div className="mt-2 flex items-start gap-2.5 rounded-md border border-border bg-muted p-2.5 text-xs">
      <Link2Icon className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
      <div className="flex min-w-0 flex-col gap-0.5">
        <span className="font-mono text-foreground/80">
          package.json · scripts.{command.source_script_name}
        </span>
        <span
          className="truncate font-mono text-xs text-muted-foreground"
          title={command.source_package_json_path}
        >
          {command.source_package_json_path}
        </span>
        {driftValue != null ? (
          <span className="mt-0.5 text-xs leading-snug text-muted-foreground/90">
            <span className="font-medium text-warning">Changed in package.json</span> — now{" "}
            <code className="font-mono">{driftValue}</code>. Open Edit to resync.
          </span>
        ) : (
          <span className="mt-0.5 text-xs leading-snug text-muted-foreground/80">
            Linked to package.json. Open Edit to resync or unlink.
          </span>
        )}
      </div>
    </div>
  );
}
