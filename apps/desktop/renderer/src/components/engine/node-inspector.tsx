import { Fragment } from "react";
import { BotIcon, XIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import { Spinner } from "@/components/ui/spinner";

import { isClickableMarker, parseContextMarkers, type MarkerKind } from "./context-markers";
import type { EngineMarker } from "./engine-nodes";

/**
 * The built-in agent ROLES nyx can render a static context template for — the client mirror of
 * nyx-core's `AgentRole::parse` (`mcp_context.rs`). Keep this list ALIGNED with that enum: only these
 * `agent_id`s have a built-in role template; any other spawn target (e.g. a custom `dragon-slayer`) is
 * a CUSTOM sub-agent with no built-in template (real custom effectors are configured separately, v2.9
 * — finding #40).
 */
export const KNOWN_AGENT_ROLES = ["spec", "dev", "coder", "auditor", "doc", "prototype"] as const;

/** Whether `agentId` is a built-in role nyx renders a template for (see {@link KNOWN_AGENT_ROLES}). */
export function isKnownAgentRole(agentId: string): boolean {
  return (KNOWN_AGENT_ROLES as readonly string[]).includes(agentId);
}

/**
 * The SUB-AGENT context inspector (v2.8 Phase 6, annex `context-effecteur`): opened by clicking a
 * `spawn_subagent` node in the graph, it shows the CONTEXT this sub-agent will READ when it is
 * spawned — the verbatim output of nyx-core's context render, which the sub-agent itself pulls with
 * `agent_context(run_id, role, subject?)` (the spawn action carries identifiers, never this text).
 * It is the sub-agent's brief, NOT its
 * live activity (the live run view lands in v2.9). Any `{{sub_agent:id}}` / `{{check:id}}` marker in
 * that text is rendered as a CLICKABLE chip that jumps back to the matching graph node. The header
 * label reads "Sub-agent" rather than the internal "Effector" jargon (finding #31).
 *
 * `bound` distinguishes the two states (finding #35): with a run bound the render is the LIVE manual
 * (its `{{rules}}` resolved, its Spec/phase/workspace filled in); with NO run bound it is the STATIC
 * role TEMPLATE — the sub-agent's brief — whose Spec-specific parts are shown as "resolved at run"
 * rather than an empty "not resolved yet" state.
 *
 * Labels are DATA-DERIVED, never hard-coded (finding #40): "built-in" only when the workflow is a
 * nyx default (`builtIn`), else "custom". And a spawn whose `agent_id` is NOT a known role
 * ({@link isKnownAgentRole}) — e.g. a custom `dragon-slayer` — is a CUSTOM sub-agent with no built-in
 * template: the panel shows a clear custom state (no misleading "role template" badge, no
 * "Could not render" error), since nyx-core's `render_role_template` legitimately rejects it.
 */
export function NodeInspector({
  agentId,
  context,
  bound,
  builtIn,
  loading,
  error,
  onMarker,
  onClose,
}: {
  /** The spawn node's sub-agent role (its `agent_id`). */
  agentId: string;
  /** The rendered sub-agent context (the live manual, or the static role template), or `null`. */
  context: string | null;
  /** Whether a run/Spec is bound — live enriched context vs the static role template. */
  bound: boolean;
  /** Whether the OWNING workflow is a built-in nyx default (`project_id === null`) — drives the
   *  "built-in" vs "custom" label instead of hard-coding it (finding #40). */
  builtIn: boolean;
  loading: boolean;
  error: boolean;
  /** Jump to the graph node a `{{sub_agent:id}}` / `{{check:id}}` marker targets. */
  onMarker: (marker: EngineMarker) => void;
  onClose: () => void;
}) {
  // A spawn whose agent_id is one of nyx's built-in roles has a renderable template; anything else is
  // a custom sub-agent nyx has no built-in template for (finding #40).
  const knownRole = isKnownAgentRole(agentId);
  return (
    <div className="flex h-full flex-col overflow-hidden">
      {/* Header — the sub-agent identity. Only the ESSENTIAL state (live run vs role template, or a
          custom sub-agent) is a full badge; "built-in" / "custom" is demoted to muted secondary text
          so the panel reads as one fact, not three badges. Labels are data-derived (finding #40). */}
      <div className="flex items-start gap-2 border-b border-border p-3">
        <span className="flex size-7 shrink-0 items-center justify-center rounded-lg border border-info/40 bg-info/10 text-info">
          <BotIcon className="size-4" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h2 className="truncate font-heading text-sm font-semibold text-foreground">
              Sub-agent · {agentId}
            </h2>
            {/* The state badge: for a known role, the live-run vs role-template state (finding #35);
                for a custom/unknown role, a plain "custom" — never a misleading "role template". */}
            <Badge
              variant={knownRole && bound ? "info" : "muted"}
              className="font-mono uppercase tracking-wide"
            >
              {!knownRole ? "custom" : bound ? "live run" : "role template"}
            </Badge>
          </div>
          <div className="mt-0.5 flex items-center gap-1.5 font-mono text-2xs text-muted-foreground">
            <span className="font-semibold text-primary">{"{{sub_agent:" + agentId + "}}"}</span>
            <span>·</span>
            <span>{builtIn ? "built-in" : "custom"} sub-agent</span>
          </div>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close inspector"
          className="rounded-md p-1 text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
        >
          <XIcon className="size-4" />
        </button>
      </div>

      {/* Body — the CONTEXT handed to the sub-agent (verbatim), markers made clickable. For a custom
          role there is no built-in template, so a clear custom state stands in (finding #40). */}
      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {!knownRole ? (
          <EmptyState>
            <p className="font-medium text-foreground">
              Custom sub-agent <span className="font-mono text-primary">'{agentId}'</span>
            </p>
            <p className="mt-1.5">
              No built-in role template — this is not one of nyx's built-in roles (
              {KNOWN_AGENT_ROLES.join(", ")}). Custom effectors get their own configurable context
              in a later release.
            </p>
          </EmptyState>
        ) : (
          <>
            <div className="mb-2 text-2xs text-muted-foreground">
              {bound ? (
                <>
                  The live context this sub-agent pulls for the current run — its role template
                  with the workflow's <span className="font-mono text-info">{"{{rules}}"}</span>{" "}
                  resolved. This is its brief, not its live activity.
                </>
              ) : (
                <>
                  This sub-agent's static role template — its brief. The Spec-specific parts (Spec /
                  Project header, current phase,{" "}
                  <span className="font-mono text-info">{"{{rules}}"}</span>) are{" "}
                  <span className="font-medium text-foreground">resolved at run</span>; bind this
                  workflow to a run on a Spec to fill them in.
                </>
              )}
            </div>
            {loading ? (
              <div className="flex items-center justify-center py-8">
                <Spinner />
              </div>
            ) : error ? (
              <p className="py-6 text-center text-2xs text-muted-foreground">
                Could not render this sub-agent's context.
              </p>
            ) : context == null ? (
              <EmptyState>No role template available for this sub-agent.</EmptyState>
            ) : (
              <pre className="whitespace-pre-wrap break-words rounded-lg border border-border bg-card p-3 font-mono text-2xs leading-relaxed text-muted-foreground">
                <ContextBody context={context} onMarker={onMarker} />
              </pre>
            )}
          </>
        )}
      </div>
    </div>
  );
}

/** Render the context text, turning `{{sub_agent:id}}` / `{{check:id}}` markers into clickable chips
 *  and leaving a resolved `{{rules}}` marker as an inert info chip. */
function ContextBody({
  context,
  onMarker,
}: {
  context: string;
  onMarker: (marker: EngineMarker) => void;
}) {
  const segments = parseContextMarkers(context);
  return (
    <>
      {segments.map((seg, i) => {
        if (seg.type === "text") {
          return <Fragment key={i}>{seg.text}</Fragment>;
        }
        if (isClickableMarker(seg.kind)) {
          return (
            <button
              key={i}
              type="button"
              data-marker-kind={seg.kind}
              data-marker-id={seg.id}
              onClick={() =>
                onMarker({ kind: seg.kind as "sub_agent" | "check", id: seg.id, nodeId: "" })
              }
              className={cn(
                "mx-0.5 inline-flex items-center rounded-md border px-1.5 py-0.5 align-middle font-mono text-2xs font-semibold outline-none transition-shadow focus-visible:ring-2",
                markerToneClass(seg.kind),
              )}
            >
              {seg.raw}
            </button>
          );
        }
        // `{{rules}}` — resolved server-side; an inert marker (not a jump target).
        return (
          <Badge
            key={i}
            variant="info"
            data-marker-kind={seg.kind}
            className="mx-0.5 align-middle font-mono font-semibold"
          >
            {seg.raw}
          </Badge>
        );
      })}
    </>
  );
}

/** Token tone class-set for a clickable marker chip. */
function markerToneClass(kind: MarkerKind): string {
  if (kind === "check") return "border-destructive/40 bg-destructive/10 text-destructive";
  return "border-primary/40 bg-primary/10 text-primary";
}
