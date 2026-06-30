import { motion, useReducedMotion } from "motion/react";
import { FolderIcon, Link2Icon, TerminalIcon } from "lucide-react";

import { cn } from "@/lib/utils";

export interface CommandInfoBarProps {
  /** The command line that runs (e.g. `bun run start`). */
  command: string;
  /** The resolved run directory (workspace path + subfolder). */
  cwd: string;
  /** package.json script name, if the command was imported (else null). */
  sourceScriptName?: string | null;
  /** package.json path the source points at, if imported (the title hint). */
  sourcePackageJsonPath?: string | null;
  /**
   * The last run's exit code, kept as LAST-RUN info: it persists until the next
   * run starts (a fresh `running` clears it) and SURVIVES the acknowledge-on-select
   * idle reset — it is decoupled from the dot's (acknowledgeable) live state, which
   * the header dot already carries. `null`/absent ⇒ no code shown yet this session.
   */
  exitCode?: number | null;
  className?: string;
}

/**
 * One labelled field in the info bar: a lead icon + a (selectable, monospace)
 * value. The value truncates with a native `title` tooltip so a long path / command
 * stays on one compact line.
 */
function Field({
  icon,
  value,
  title,
  className,
}: {
  icon: React.ReactNode;
  value: React.ReactNode;
  title?: string;
  className?: string;
}) {
  return (
    <span className={cn("flex min-w-0 items-center gap-1.5", className)}>
      <span aria-hidden className="shrink-0 text-muted-foreground/70 [&_svg]:size-3">
        {icon}
      </span>
      <span className="truncate font-mono text-muted-foreground" title={title}>
        {value}
      </span>
    </span>
  );
}

/**
 * `<CommandInfoBar>` — the compact info strip under the command-view controls
 * (review 01KV6F1CP…). It surfaces, on one line, WHAT runs and WHERE, plus the
 * provenance + the last run's result — the context the bare header (dot + name +
 * buttons) left out:
 *
 *  - the **command** (e.g. `bun run start`),
 *  - the **working directory** (resolved workspace path + subfolder),
 *  - the **source** reference when imported (`package.json:scripts.<name>`),
 *  - the **last run's exit code** (`exit N` once a run has ended this session).
 *
 * The live RUN STATE is deliberately NOT repeated here: it is already carried by
 * the header status DOT, so a redundant state-text label was dropped. The exit
 * code is shown as LAST-RUN info — decoupled from the dot's (acknowledgeable)
 * state, so it persists until the next run AND survives the acknowledge-on-select
 * idle reset (the result of the last run is still meaningful after the dot clears).
 *
 * Style follows the design system (tokens only, monospace values, the modal's
 * source-reference idiom); it animates in with Motion (a soft fade/rise),
 * collapsing to a static reveal under `prefers-reduced-motion`. Purely
 * presentational — every value is a prop.
 */
export function CommandInfoBar({
  command,
  cwd,
  sourceScriptName,
  sourcePackageJsonPath,
  exitCode,
  className,
}: CommandInfoBarProps) {
  const reduced = useReducedMotion();
  const sourced = !!sourceScriptName;
  // The exit code reads only once a finished run has carried one this session.
  const showExit = exitCode !== null && exitCode !== undefined;

  return (
    <motion.div
      initial={reduced ? false : { opacity: 0, y: -4 }}
      animate={{ opacity: 1, y: 0 }}
      transition={reduced ? { duration: 0 } : { duration: 0.18, ease: "easeOut" }}
      className={cn(
        "flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 border-b border-border bg-muted/20 px-3 py-1.5 text-xs",
        className,
      )}
    >
      {/* WHAT runs. */}
      <Field
        icon={<TerminalIcon />}
        value={<span className="text-foreground/80">{command}</span>}
        title={command}
        className="max-w-full"
      />
      {/* WHERE it runs (resolved cwd). */}
      <Field icon={<FolderIcon />} value={cwd} title={cwd} className="max-w-full" />
      {/* SOURCE (only when imported from package.json). */}
      {sourced && (
        <Field
          icon={<Link2Icon />}
          value={`package.json:scripts.${sourceScriptName}`}
          title={sourcePackageJsonPath ?? undefined}
        />
      )}
      {/* LAST RUN's exit code (persists until the next run; survives acknowledge).
          The live run state is the header DOT's job — not repeated here. */}
      {showExit && (
        <span
          className={cn("shrink-0 font-mono", exitCode === 0 ? "text-success" : "text-destructive")}
        >
          exit {exitCode}
        </span>
      )}
    </motion.div>
  );
}
