import { Link } from "@tanstack/react-router";
import { GitBranchIcon, MessageSquareIcon } from "lucide-react";

import type { DiffKind, DiffMode, RepoDiff } from "@/bridge";
import { cn } from "@/lib/utils";
import { Breadcrumb, BreadcrumbCurrent, breadcrumbLinkClass } from "@/components/ui/breadcrumb";
import { Segmented } from "@/components/ui/segmented";
import { Tabs, TabCount } from "@/components/ui/tabs";

import { reviewRepoKey, type ReviewRepo, type ReviewTarget } from "./review-types";

/**
 * `ReviewHeader` — the top zone of the review screen, reproducing the prototype's header
 * placement: the multi-repo TAB switcher (one tab per aggregated `(workspace, repository)`),
 * the active repo's derived base→target range, and the diff SELECTORS. Rewritten against the
 * real data: the tabs are `ReviewRepo`s (never a shared diff — each tab is its own Git
 * identity), and the selectors drive the real engine's target kind + living/pinned mode.
 *
 * The prototype's unified/split STYLE toggle is kept as placement; here it is joined by the
 * two selectors phase 3 actually needs — the diff SURFACE (working tree / staged / range /
 * branches) and the LIVING vs PINNED variety (PDR-0015) — because those, not comments, are
 * this pass's scope. The header keeps ONE panel toggle — the RIGHT comments panel (the prototype's
 * comments-panel toggle); the LEFT files panel is always open (Finding 59 — no collapse control).
 */

/** The diff surfaces the user can pick. `range`/`branches` need rev-specs (entered below). */
const KIND_OPTIONS: { value: DiffKind; label: string }[] = [
  { value: "working_tree", label: "Working tree" },
  { value: "staged", label: "Staged" },
  { value: "unstaged", label: "Unstaged" },
  { value: "untracked", label: "Untracked" },
  { value: "branches", label: "Branches" },
  { value: "range", label: "Range" },
  { value: "commit", label: "Commit" },
];

export interface ReviewHeaderProps {
  readonly repos: readonly ReviewRepo[];
  readonly activeKey: string | null;
  readonly onSelectRepo: (repo: ReviewRepo) => void;
  readonly activeRepo: ReviewRepo | null;
  readonly activeDiff: RepoDiff | undefined;
  readonly target: ReviewTarget;
  readonly onTargetChange: (target: ReviewTarget) => void;
  /** The Spec this review is scoped to — the target of the breadcrumb's specKey link (`/work/$specId`). */
  readonly specId: string;
  /** The Spec's human key (e.g. `NYX-3`) — the middle breadcrumb crumb's label. */
  readonly specKey: string;
  /** The diff surface style (unified vs split). */
  readonly diffStyle: "unified" | "split";
  readonly onDiffStyleChange: (style: "unified" | "split") => void;
  readonly commentsPanelOpen: boolean;
  readonly onToggleCommentsPanel: () => void;
  /** The unread comment count of the active repo's review feed (0 = nothing new). */
  readonly unread: number;
}

export function ReviewHeader({
  repos,
  activeKey,
  onSelectRepo,
  activeRepo,
  activeDiff,
  target,
  onTargetChange,
  specId,
  specKey,
  diffStyle,
  onDiffStyleChange,
  commentsPanelOpen,
  onToggleCommentsPanel,
  unread,
}: ReviewHeaderProps) {
  const needsRange = target.kind === "range" || target.kind === "branches";
  const needsRev = target.kind === "commit";
  // The effective living/pinned variety: the explicit override, else the target's default.
  const effectiveMode: DiffMode = activeDiff?.mode ?? target.mode ?? "living";

  return (
    <header className="flex shrink-0 flex-col gap-2 border-border/60 border-b bg-muted/40 px-4 py-2.5">
      {/* The SHARED breadcrumb (Finding 47) replaces the old lone square `←` button: `Work` (→ the
          board) / `<specKey>` (→ the Spec detail — the "back" target, keeps `data-testid`) / `Review`
          (current). Same crumb chrome as the board + Spec detail — the three Work headers harmonised.
          The review page is still reached ONLY from the Spec's Review tab. */}
      <Breadcrumb>
        <Link to="/work" className={breadcrumbLinkClass}>
          Work
        </Link>
        <Link
          to="/work/$specId"
          params={{ specId }}
          data-testid="review-back"
          aria-label="Back to the Spec"
          className={cn(breadcrumbLinkClass, "font-mono")}
        >
          {specKey}
        </Link>
        <BreadcrumbCurrent>Review</BreadcrumbCurrent>
      </Breadcrumb>

      <div className="flex items-center justify-between gap-4">
        {/* Multi-repo tab switcher — one tab per aggregated (workspace, repository). Built on
            the design-system `Tabs` primitive (`Tabs.Root/List/Tab`, Base UI) for its tab
            semantics + a11y (roving tabindex, arrow-key nav, `role="tab"`/`aria-selected`).
            NO `Tabs.Panel`: the panel model does not apply here — every repo shares ONE diff
            view driven by the parent's `activeKey`, not a per-tab panel. `Tabs.Root`'s `value`
            accepts `null` (no active tab, e.g. before any repo loads); `onValueChange` hands
            back the selected repo's key, which we map to its `ReviewRepo` for `onSelectRepo`. */}
        {repos.length === 0 ? (
          <span className="px-2 text-sm text-muted-foreground">No repositories</span>
        ) : (
          <Tabs.Root
            value={activeKey}
            onValueChange={(value) => {
              const repo = repos.find((r) => reviewRepoKey(r) === value);
              if (repo) onSelectRepo(repo);
            }}
            className="min-w-0"
          >
            <Tabs.List
              aria-label="Repositories"
              // Finding 45: `overflow-x-auto` alone forces the Y axis to `auto` too, so the
              // indicator's `bottom-[-1px]` + the TabCount line-height overflow by ~1px and raise a
              // parasitic VERTICAL scrollbar (the "2D scroll" look). Pinning `overflow-y-hidden`
              // keeps ONLY the horizontal scroll (for many repos); a single repo renders clean.
              className="min-w-0 gap-4 overflow-x-auto overflow-y-hidden border-b-0"
            >
              {repos.map((repo) => {
                const key = reviewRepoKey(repo);
                return (
                  <Tabs.Tab
                    key={key}
                    value={key}
                    title={`${repo.workspaceName} · ${repo.absolutePath}`}
                    className="shrink-0 py-1.5"
                  >
                    {repo.name}
                    <TabCount>{repo.changedFileCount}</TabCount>
                  </Tabs.Tab>
                );
              })}
            </Tabs.List>
          </Tabs.Root>
        )}

        <div className="flex shrink-0 items-center gap-2">
          {/* Unified / split diff style (the prototype's diff-style toggle). */}
          <Segmented
            aria-label="Diff style"
            value={diffStyle}
            onChange={onDiffStyleChange}
            options={[
              { v: "unified", label: "Unified" },
              { v: "split", label: "Split" },
            ]}
          />

          {/* Diff SURFACE selector (working tree / staged / range / branches / …). */}
          <label className="sr-only" htmlFor="review-kind">
            Diff surface
          </label>
          <select
            id="review-kind"
            aria-label="Diff surface"
            value={target.kind}
            onChange={(e) => onTargetChange({ ...target, kind: e.target.value as DiffKind })}
            className="rounded-md border border-input bg-popover px-2 py-1 text-xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {KIND_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>
                {opt.label}
              </option>
            ))}
          </select>

          {/* LIVING vs PINNED variety selector (PDR-0015). */}
          <div
            className="flex items-center gap-0.5 rounded-lg border border-input bg-card p-0.5"
            role="group"
            aria-label="Review variety"
          >
            {(["living", "pinned"] as const).map((mode) => (
              <button
                key={mode}
                type="button"
                aria-pressed={effectiveMode === mode}
                onClick={() => onTargetChange({ ...target, mode })}
                className={cn(
                  "rounded-md px-2 py-0.5 text-2xs font-medium capitalize outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                  effectiveMode === mode
                    ? "bg-muted text-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {mode}
              </button>
            ))}
          </div>

          {/* Finding 59 — the LEFT files panel is now ALWAYS open (user decision, reverses part of
              Finding 46): its collapse toggle is REMOVED, so there is no way to hide the changed-files
              tree. Only the RIGHT comments panel stays closable (its toggle is kept below). */}

          {/* Toggle the RIGHT comments panel (the prototype's comments-panel toggle). Carries the
              live UNREAD-comment badge (task 01KX0F6JEKCEV2JCTMEBMM3P4Z): a per-repo count that
              re-pulls on `repos://changed` — a new human OR agent comment raises it, mark-read
              zeroes it. Absent when 0 (nothing unseen). */}
          <button
            type="button"
            aria-label={commentsPanelOpen ? "Hide comments panel" : "Show comments panel"}
            aria-pressed={commentsPanelOpen}
            onClick={onToggleCommentsPanel}
            className={cn(
              "relative flex size-7 items-center justify-center rounded-md border border-input bg-popover outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
              commentsPanelOpen ? "text-foreground" : "text-muted-foreground",
            )}
          >
            <MessageSquareIcon className="size-4" />
            {unread > 0 ? (
              <span
                data-testid="unread-badge"
                aria-label={`${unread} unread comments`}
                className="-right-1.5 -top-1.5 absolute inline-flex min-w-4 items-center justify-center rounded-full bg-info px-1 font-semibold text-2xs text-info-foreground leading-none tabular-nums"
              >
                {unread > 99 ? "99+" : unread}
              </span>
            ) : null}
          </button>
        </div>
      </div>

      {/* Rev-spec inputs for the range/branches/commit surfaces. */}
      {needsRange ? (
        <div className="flex items-center gap-2 text-xs">
          <input
            aria-label="Base rev"
            placeholder="base (branch or SHA)"
            value={target.base ?? ""}
            onChange={(e) => onTargetChange({ ...target, base: e.target.value })}
            className="w-48 rounded-md border border-input bg-popover px-2 py-1 font-mono text-2xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
          />
          <span className="text-muted-foreground/50">…</span>
          <input
            aria-label="Target rev"
            placeholder="target (branch or SHA)"
            value={target.target ?? ""}
            onChange={(e) => onTargetChange({ ...target, target: e.target.value })}
            className="w-48 rounded-md border border-input bg-popover px-2 py-1 font-mono text-2xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
          />
        </div>
      ) : needsRev ? (
        <div className="flex items-center gap-2 text-xs">
          <input
            aria-label="Commit rev"
            placeholder="commit (rev-spec)"
            value={target.rev ?? ""}
            onChange={(e) => onTargetChange({ ...target, rev: e.target.value })}
            className="w-64 rounded-md border border-input bg-popover px-2 py-1 font-mono text-2xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
          />
        </div>
      ) : null}

      {/* The active repo's derived base → target range + current branch. Finding 48: the internal
          jargon that used to trail here — `{activeDiff.mode}` ("Living") + `{activeDiff.backend}`
          ("gix", a pure diagnostic) — is REMOVED from the user-facing text (the Living/Pinned choice
          already has its own labelled toggle in the header). Both survive as diagnostic `data-*`
          attributes (invisible, for the gate/devtools), so nothing user-facing reads "gix"/"Living"
          but the backend/mode stay assertable. The useful SHA/branch range stays. */}
      <div
        className="flex items-center gap-2 text-xs"
        data-diff-backend={activeDiff?.backend}
        data-diff-mode={activeDiff?.mode}
      >
        {activeRepo ? (
          <span className="flex items-center gap-1.5 font-mono text-muted-foreground">
            <GitBranchIcon className="size-3.5" />
            <span>{activeDiff?.base_sha?.slice(0, 10) ?? activeRepo.branch ?? "base"}</span>
            <span className="text-muted-foreground/50">…</span>
            <span className="text-foreground">
              {activeDiff?.target_sha?.slice(0, 10) ?? activeRepo.branch ?? "working tree"}
            </span>
          </span>
        ) : null}
      </div>
    </header>
  );
}
