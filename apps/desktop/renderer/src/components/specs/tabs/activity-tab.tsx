import { useEffect, useState } from "react";
import {
  ActivityIcon,
  AnchorIcon,
  ArrowRightLeftIcon,
  FilePlusIcon,
  GitForkIcon,
  LinkIcon,
  ListPlusIcon,
  type LucideIcon,
  PencilIcon,
  WorkflowIcon,
} from "lucide-react";

import { nyxBridge } from "@/bridge";

import type { SpecDetail } from "../types";

/** A `spec_activity` entry (the wire shape of `SpecActivityEntry`). */
interface ActivityEntry {
  readonly sequence: number;
  readonly event_type: string;
  readonly aggregate: string;
  readonly source: string;
  /** Small business JSON bag (`spec_id`, and for a task event `task_id`; transitions carry
   * `from`/`to` + the requested `verb` and, on block/skip, a `reason`; a section edit its
   * `section`; a fork the revision `from`/`to`; a workspace change the workspace `from`/`to`).
   * Parsed for a human label. */
  readonly payload: string;
  readonly created_at: number;
}

interface SpecActivity {
  readonly spec_id: string;
  readonly entries: readonly ActivityEntry[];
}

/** Turn a snake_case wire token (`implementation_decisions`, `in_progress`) into words. */
function humanizeToken(token: string): string {
  return token.replace(/_/g, " ");
}

/**
 * A clean label for an event's `source` — WHO performed the action (the icon carries the TYPE).
 * Known origins get a curated label (`mcp` → "MCP", `ui` → "User", `system` → "System"); any
 * other value is shown title-cased so it still reads sensibly.
 */
function sourceLabel(source: string): string {
  switch (source) {
    case "mcp":
      return "MCP";
    case "ui":
      return "User";
    case "system":
      return "System";
    default:
      return source ? source.charAt(0).toUpperCase() + source.slice(1) : "Unknown";
  }
}

/**
 * The distinct lucide icon shown in the timeline circle for each `event_type`, so an entry is
 * recognizable at a glance (a fork vs a transition vs a dependency edge). Falls back to the
 * generic [`ActivityIcon`] for an unknown type.
 */
const EVENT_ICONS: Readonly<Record<string, LucideIcon>> = {
  "spec.created": FilePlusIcon,
  "spec.section_updated": PencilIcon,
  "spec.transitioned": WorkflowIcon,
  "spec.forked": GitForkIcon,
  "spec.workspace_changed": AnchorIcon,
  "task.added": ListPlusIcon,
  "task.transitioned": ArrowRightLeftIcon,
  "task.dependency_added": LinkIcon,
};

/** The icon for an event type (the generic activity glyph when the type is unknown). */
function iconFor(eventType: string): LucideIcon {
  return EVENT_ICONS[eventType] ?? ActivityIcon;
}

/**
 * A human label for a Spec/task activity event, enriched from its payload:
 *   - `spec.created` → "Spec created"
 *   - `spec.section_updated` (section=problem) → "Section updated: problem"
 *   - `spec.transitioned` (from=draft, to=ready) → "Spec transitioned draft → ready"
 *   - `spec.forked` (from=1, to=2) → "Spec forked: revision 1 → 2"
 *   - `spec.workspace_changed` → "Spec anchored to a workspace" / "…re-anchored" / "…detached"
 *   - `task.added` → "Task added"
 *   - `task.transitioned` uses the requested `verb` (+ block/skip `reason`):
 *       verb=block → "Task blocked: <reason>", verb=skip → "Task skipped: <reason>",
 *       verb=verify → "Task verified", verb=done → "Task done", verb=start → "Task started";
 *       an unenriched payload falls back to "Task transitioned todo → in progress".
 *   - `task.dependency_added` → "Task dependency added"
 * Falls back to a de-dotted title-case of the event type when the payload is unexpected.
 */
function describeEvent(ev: ActivityEntry): string {
  let payload: Record<string, unknown> = {};
  try {
    payload = JSON.parse(ev.payload) as Record<string, unknown>;
  } catch {
    payload = {};
  }
  const from = typeof payload.from === "string" ? humanizeToken(payload.from) : null;
  const to = typeof payload.to === "string" ? humanizeToken(payload.to) : null;
  const section = typeof payload.section === "string" ? humanizeToken(payload.section) : null;
  const verb = typeof payload.verb === "string" ? payload.verb : null;
  const reason =
    typeof payload.reason === "string" && payload.reason.trim() ? payload.reason.trim() : null;

  switch (ev.event_type) {
    case "spec.created":
      return "Spec created";
    case "spec.section_updated":
      return section ? `Section updated: ${section}` : "Section updated";
    case "spec.transitioned":
      return from && to ? `Spec transitioned ${from} → ${to}` : "Spec transitioned";
    case "spec.forked": {
      const rf = typeof payload.from === "number" ? payload.from : null;
      const rt = typeof payload.to === "number" ? payload.to : null;
      return rf !== null && rt !== null ? `Spec forked: revision ${rf} → ${rt}` : "Spec forked";
    }
    case "spec.workspace_changed": {
      const hadWorkspace = typeof payload.from === "string";
      const hasWorkspace = typeof payload.to === "string";
      if (hadWorkspace && hasWorkspace) return "Spec re-anchored to a new workspace";
      if (hasWorkspace) return "Spec anchored to a workspace";
      if (hadWorkspace) return "Spec workspace detached";
      return "Spec workspace changed";
    }
    case "task.added":
      return "Task added";
    case "task.transitioned":
      switch (verb) {
        case "block":
          return reason ? `Task blocked: ${reason}` : "Task blocked";
        case "skip":
          return reason ? `Task skipped: ${reason}` : "Task skipped";
        case "verify":
          return "Task verified";
        case "done":
          return "Task done";
        case "start":
          return "Task started";
        default:
          return from && to ? `Task transitioned ${from} → ${to}` : "Task transitioned";
      }
    case "task.dependency_added":
      return "Task dependency added";
    default: {
      const nice = ev.event_type.replace(/[._]/g, " ");
      return nice.charAt(0).toUpperCase() + nice.slice(1);
    }
  }
}

/**
 * `<ActivityTab>` — the Spec's activity fil: the Spec's most recent business events
 * (`domain_events` — its own lifecycle: creation, section edits, transitions, forks,
 * workspace re-anchoring; plus its tasks' add/transition/dependency events), NEWEST FIRST.
 * It reads the SPEC-SCOPED `spec_activity` read model (the analogue of the workspace
 * `workspace_timeline`, PDR-0005: one authority for UI + MCP), keyed on the Spec id — so it
 * shows the Spec's activity, not just the workspace's project-level `project.created`.
 * Placement reproduced from the prototype's `activity-tab` (a vertical event timeline); each
 * entry shows a per-`event_type` icon in the circle plus a small chip for its `source` (WHO
 * acted — mcp/ui/system…).
 */
export function ActivityTab({ detail }: { detail: SpecDetail }) {
  const specId = detail.spec.id;
  const [entries, setEntries] = useState<readonly ActivityEntry[]>([]);
  const [loading, setLoading] = useState<boolean>(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void nyxBridge
      .invoke<SpecActivity>("spec_activity", { specId })
      .then((act) => {
        if (!cancelled) setEntries(act.entries);
      })
      .catch(() => {
        if (!cancelled) setEntries([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [specId]);

  if (loading) {
    return (
      <div className="mx-auto max-w-3xl">
        <p className="text-sm text-muted-foreground">Loading activity…</p>
      </div>
    );
  }

  if (entries.length === 0) {
    return (
      <div className="mx-auto max-w-3xl">
        <p className="text-sm text-muted-foreground">No activity recorded yet.</p>
      </div>
    );
  }

  // Render NEWEST FIRST: the read model returns oldest-first (chronological), but the feed
  // shows the most recent activity at the TOP (descending `sequence`, the monotonic order).
  // A copy is sorted so the source array stays untouched.
  const ordered = [...entries].sort((a, b) => b.sequence - a.sequence);

  return (
    // `max-w-3xl`, like Spec / Tasks / Review / Runtime / Prototype: the column width is set per tab
    // in this app, and this tab was one of the two left at `2xl` — so the content jumped ~96px wide
    // when you moved between tabs. Aligned on the majority rather than kept as an exception.
    <div className="mx-auto max-w-3xl">
      <ol className="relative flex flex-col">
        {ordered.map((ev, i) => {
          // The BOTTOM (oldest) entry closes the timeline — no connector line below it.
          const last = i === ordered.length - 1;
          const Icon = iconFor(ev.event_type);
          return (
            <li key={ev.sequence} className="flex gap-3">
              <div className="flex flex-col items-center">
                <span className="relative z-10 grid size-7 place-items-center rounded-full bg-muted text-muted-foreground ring-4 ring-background">
                  <Icon className="size-3.5" />
                </span>
                {!last ? <span className="w-px flex-1 bg-border/70" /> : null}
              </div>
              <div className={last ? "flex-1 pb-0" : "flex-1 pb-5"}>
                <div className="flex items-center gap-2">
                  <span className="text-sm text-foreground/90">{describeEvent(ev)}</span>
                  {/* The SOURCE chip — WHO acted (mcp/ui/system…); the circle icon carries the type. */}
                  <span className="rounded-full bg-muted px-1.5 text-2xs font-medium text-muted-foreground">
                    {sourceLabel(ev.source)}
                  </span>
                </div>
                <div className="mt-0.5 font-mono text-2xs text-muted-foreground/60">
                  {new Date(ev.created_at).toLocaleString()}
                </div>
              </div>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
