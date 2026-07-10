import { CircleDotIcon, FolderGit2Icon, GitBranchIcon, LayersIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { SidebarShell } from "@/components/ui/sidebar-shell";

import { priorityMeta, statusMeta } from "./spec-meta";
import { Dot, MicroLabel } from "./spec-primitives";
import type { SpecDetail } from "./types";

/** A labelled sidebar section (icon + micro-label header, then its content). */
function Section({
  icon,
  label,
  children,
}: {
  icon: React.ReactNode;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="border-b border-border/40 px-4 py-3 last:border-b-0">
      <div className="mb-2 flex items-center gap-1.5 text-muted-foreground">
        {icon}
        <MicroLabel>{label}</MicroLabel>
      </div>
      {children}
    </div>
  );
}

/**
 * `<SpecSidebar>` — the PRD detail's LEFT context rail: the Spec's status + priority, its
 * category (mode), the DERIVED repositories and the dev workspace it is anchored on. It sits in
 * the SHARED `<SidebarShell>` — same width / LEFT position / border / surface as the Terminals
 * cockpit sidebar (review finding: one common shell, both rails to the left; only the CONTENT
 * differs). The tabs + tab content occupy the area to its RIGHT. Placement reproduced from the
 * prototype's `spec/sidebar` (status → category → repos → workspace sections), consuming the
 * phase-3 workspace context (`detail.workspace_context`, `null` until the Spec is anchored).
 * Design-system tokens only, the real `<Badge>` primitive.
 */
export function SpecSidebar({ detail }: { detail: SpecDetail }) {
  const { spec, workspace_context } = detail;
  const status = statusMeta(spec.status);
  const priority = priorityMeta(spec.priority);

  return (
    <SidebarShell aria-label="Spec context">
      <Section icon={<CircleDotIcon className="size-3" />} label="Status">
        <div className="flex items-center justify-between gap-2">
          <span className="inline-flex items-center gap-1.5 text-sm text-foreground">
            <Dot tone={status.dot} size="sm" />
            {status.label}
          </span>
          <Badge variant={priority.variant}>{priority.label}</Badge>
        </div>
      </Section>

      <Section icon={<LayersIcon className="size-3" />} label="Mode">
        <span className="text-sm capitalize text-foreground/90">{spec.mode}</span>
      </Section>

      <Section icon={<FolderGit2Icon className="size-3" />} label="Repositories">
        {workspace_context && workspace_context.repos.length > 0 ? (
          <ul className="flex flex-col gap-0.5">
            {workspace_context.repos.map((r) => (
              <li key={r.id} className="flex items-center gap-2 rounded-md px-1 py-1">
                <FolderGit2Icon className="size-3.5 shrink-0 text-muted-foreground/70" />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm leading-tight text-foreground/90">
                    {r.name}
                  </span>
                  <span className="block truncate font-mono text-2xs text-muted-foreground/60">
                    {r.relative_path}
                  </span>
                </span>
                {r.optional ? (
                  <Badge variant="muted" className="shrink-0 font-mono">
                    opt
                  </Badge>
                ) : null}
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-sm text-muted-foreground/60">No repositories.</p>
        )}
      </Section>

      <Section icon={<GitBranchIcon className="size-3" />} label="Workspace">
        {workspace_context ? (
          <div className="flex items-center gap-2 rounded-md bg-muted px-2 py-1.5">
            <GitBranchIcon className="size-3.5 shrink-0 text-foreground" />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm leading-tight text-foreground/90">
                {workspace_context.workspace.name}
              </span>
              <span className="block truncate font-mono text-2xs text-muted-foreground/60">
                {workspace_context.workspace.folder_removed_at != null
                  ? "worktree cleaned at merge"
                  : workspace_context.workspace.path}
              </span>
            </span>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground/60">No dev workspace yet</p>
        )}
      </Section>
    </SidebarShell>
  );
}
