import { AlertTriangleIcon, Link2OffIcon, PackageIcon, RefreshCwIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import type { ManagedCommand } from "./use-commands";

/**
 * THE PROVENANCE — where a command came from, whether it has DRIFTED, and the two answers to a
 * drift, in ONE block.
 *
 * ## Why it is one block and not two halves in two places
 *
 * It used to be split: a read-only strip under the card (this file, actions forbidden) and, inside
 * the edit form, a second rendering of the same three facts plus a Resync — with Unlink stranded in
 * the form's action row, away from the link it unlinks. So the same provenance was drawn twice, in
 * two registers, and the two responses to a drift sat in different halves of the screen. The
 * master/detail leaves no room for that ambiguity: a command has ONE panel, and its provenance is
 * one region of it with both of its answers attached.
 *
 * ## The drift is shown, never applied
 *
 * A drifted script is stated as TWO VALUES SIDE BY SIDE — what the command was imported with, and
 * what the file says now — because the whole point is that they differ and that adopting the new one
 * is a decision. Resync is that decision and KEEPS the link; Unlink is the other one and detaches
 * for good. Neither happens implicitly: editing the command by hand detaches it in the backend
 * (`command_update`), which is a third thing entirely and is not offered here.
 */
export function CommandSourcePanel({
  command,
  driftValue,
  busy,
  error,
  onResync,
  onUnlink,
}: {
  /** The sourced command whose provenance to show. Renders nothing for a hand-authored one. */
  command: ManagedCommand;
  /**
   * The live on-disk script value when it DRIFTED from what this command was last synced to (else
   * `null`). Drives the warning register AND the second value — informational until Resync.
   */
  driftValue?: string | null;
  /** Which source action is in flight, so both buttons lock and the right one spins. */
  busy?: null | "resync" | "unlink";
  /** The backend's OWN sentence when an action failed — the only text that says what to do. */
  error?: string | null;
  onResync: () => void;
  onUnlink: () => void;
}) {
  if (!command.source_script_name || !command.source_package_json_path) {
    // Hand-authored (or already-unlinked) command: no provenance to show.
    return null;
  }
  const drifted = driftValue != null;

  return (
    <section
      data-command-source
      data-command-source-drifted={drifted || undefined}
      className={cn(
        "flex flex-col gap-2 rounded-md border p-3",
        drifted ? "border-warning/45 bg-warning/8" : "border-border bg-muted/25",
      )}
    >
      <h4
        className={cn(
          "flex items-center gap-2 text-xs font-semibold",
          drifted ? "text-warning" : "text-foreground",
        )}
      >
        {drifted ? (
          <AlertTriangleIcon className="size-3.5 shrink-0" />
        ) : (
          <PackageIcon className="size-3.5 shrink-0" />
        )}
        {drifted ? "The script changed on disk" : "Linked to a package.json"}
      </h4>

      <div className="flex flex-col gap-0.5 text-xs leading-relaxed text-muted-foreground">
        <span className="truncate font-mono" title={command.source_package_json_path}>
          {command.source_package_json_path} · scripts.{command.source_script_name}
        </span>
        {drifted ? (
          <>
            <span data-testid="drift-imported">
              At import:{" "}
              <code className="font-mono text-foreground/80">
                {command.source_script_command_snapshot}
              </code>
            </span>
            <span data-testid="drift-line">
              On disk: <code className="font-mono text-warning">{driftValue}</code>
            </span>
          </>
        ) : (
          <span>
            Value at import:{" "}
            <code className="font-mono text-foreground/80">
              {command.source_script_command_snapshot}
            </code>{" "}
            — the same as the one on disk.
          </span>
        )}
      </div>

      {error == null ? null : (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}

      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant="outline"
          size="xs"
          loading={busy === "resync"}
          disabled={busy != null}
          onClick={onResync}
        >
          <RefreshCwIcon />
          {drifted ? "Take the disk version" : "Resync from package.json"}
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="xs"
          loading={busy === "unlink"}
          disabled={busy != null}
          onClick={onUnlink}
        >
          <Link2OffIcon />
          Unlink
        </Button>
      </div>
    </section>
  );
}
