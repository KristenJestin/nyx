import type { ComponentType } from "react";
import ClaudeIcon from "~icons/simple-icons/claude";

/**
 * The provider-aware agent registry (finding #55).
 *
 * When a terminal hosts a live `agent_session`, the sidebar row swaps its generic
 * terminal glyph for the AGENT's brand logo. This registry is the single, GENERIC
 * mapping `agent_kind → { icon, label }`: adding a future provider (Codex, OpenCode, …)
 * is literally one entry here, no change to the row code. Only `claude_code` ships now
 * (the only v1 agent that reports sessions), but the SHAPE is generic on purpose.
 *
 * ICONS: the brand logos come from `unplugin-icons` + the local Iconify
 * `@iconify-json/simple-icons` brand set — build-time, tree-shaken, and OFFLINE (bundled
 * SVG bodies, no network — fits Tauri). The rest of the UI keeps lucide; this set is ONLY
 * for provider logos. The Claude logo is the `simple-icons:claude` slug (verified present
 * in the installed set alongside `anthropic` / `claudecode`).
 */
export interface AgentProvider {
  /** The brand logo component (an SVG-backed React component from unplugin-icons). */
  icon: ComponentType<{ className?: string }>;
  /** Human label for the provider (a11y / tooltips). */
  label: string;
}

/**
 * `agent_kind` (mirrors `db::AGENT_KIND_*`) → provider descriptor. Keyed by the raw DB
 * string so a lookup is a direct map read. Only `claude_code` is wired today; the other
 * kinds resolve to `undefined` here and the row falls back to the terminal icon.
 */
export const AGENT_PROVIDERS: Readonly<Record<string, AgentProvider>> = {
  claude_code: { icon: ClaudeIcon, label: "Claude Code" },
};

/**
 * Resolve the provider descriptor for an `agent_kind`, or `undefined` when the kind is
 * unknown / has no logo yet (the caller then keeps the generic terminal icon). A thin,
 * total accessor so the row never has to know the registry shape.
 */
export function agentProviderFor(agentKind: string | null | undefined): AgentProvider | undefined {
  if (!agentKind) return undefined;
  return AGENT_PROVIDERS[agentKind];
}
