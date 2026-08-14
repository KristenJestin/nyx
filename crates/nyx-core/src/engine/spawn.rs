//! The multi-provider `spawn_subagent` ADAPTER (v2.8 — Harnais : moteur d'orchestration, Phase 3,
//! task `01KY71J1Y5BXR8K8HV3KRV9PHT`).
//!
//! `spawn_subagent` is the leaf that hands the coder's/auditor's work to a SUB-AGENT. The crux of
//! v2.8 is that this stays SINGLE-SESSION with NO external session spawn: on Claude the work runs
//! through the NATIVE `Task` sub-agent (in-session, under the subscription — never `claude -p`); on a
//! provider WITHOUT native sub-agents the graph DEGRADES to an inline fallback (the same session
//! does the work, its context growing — an assumed degradation). Either way nyx keeps piloting the
//! graph, and the sub-agent's RESULT is extracted the same way — as a git diff of the workspace.
//!
//! This module productizes the two `SubagentSpawner` flavours the P1 spike proved
//! (`tests/nyx_next_loop_spike.rs`), lifted from the test into real `src/`:
//!   * [`NativeTaskSpawner`] — Claude's native `Task` (mechanism `claude-native-task`),
//!   * [`InlineFallbackSpawner`] — the inline fallback where no native sub-agents exist
//!     (mechanism `inline-fallback`),
//!   * [`spawner_for_provider`] — the resolver that picks the flavour from the session's provider.
//!
//! The `nyx_next` handler ([`crate::mcp_engine`]) uses [`SpawnInstruction::resolve`] to stamp the
//! resolved mode onto the `spawn_subagent` action it returns, so the thin dispatcher knows HOW to
//! spawn (native vs inline). NEITHER mode ever spawns an external session — that is the invariant.


use serde_json::{json, Value};

/// HOW a sub-agent runs — the single provider seam. Both variants are IN-SESSION; neither is an
/// external session spawn (`claude -p` is proscribed by design — the pricing risk).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnMode {
    /// The provider's NATIVE sub-agent (Claude's `Task`): a real sub-agent, in-session, under the
    /// subscription. The full v2.8 mechanism.
    NativeTask,
    /// INLINE fallback: the provider has no native sub-agents (e.g. Codex / OpenCode), so the SAME
    /// session does the work. The graph still unrolls; the cost is a growing context (assumed).
    Inline,
}

impl SpawnMode {
    /// The wire string carried in the `spawn_subagent` action (`native_task` / `inline`).
    pub fn as_str(self) -> &'static str {
        match self {
            SpawnMode::NativeTask => "native_task",
            SpawnMode::Inline => "inline",
        }
    }
}

/// A per-provider "spawn a sub-agent" seam. In production the dispatcher performs the actual spawn
/// (Claude's `Task`, or the inline degradation); this trait names the MECHANISM + MODE nyx resolves
/// for it, so the graph is piloted identically whatever the provider.
pub trait SubagentSpawner: Send + Sync {
    /// A human/trace label of the provider mechanism (e.g. `claude-native-task`).
    fn mechanism(&self) -> &'static str;
    /// How the sub-agent runs for this provider.
    fn mode(&self) -> SpawnMode;
}

/// Claude's NATIVE `Task` sub-agent — a real sub-agent, in-session, under the subscription. Never
/// an external session (`claude -p`).
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeTaskSpawner;

impl SubagentSpawner for NativeTaskSpawner {
    fn mechanism(&self) -> &'static str {
        "claude-native-task"
    }
    fn mode(&self) -> SpawnMode {
        SpawnMode::NativeTask
    }
}

/// The INLINE fallback for a provider with no native sub-agents (Codex / OpenCode): the same session
/// does the work. The graph still unrolls, piloted by nyx.
#[derive(Debug, Clone, Copy, Default)]
pub struct InlineFallbackSpawner;

impl SubagentSpawner for InlineFallbackSpawner {
    fn mechanism(&self) -> &'static str {
        "inline-fallback"
    }
    fn mode(&self) -> SpawnMode {
        SpawnMode::Inline
    }
}

/// A GRAPH-forced inline: the NODE demanded in-session (an INTERACTIVE effector — the spec grill),
/// regardless of provider. Distinct from [`InlineFallbackSpawner`] so `mechanism` tells the truth —
/// this is a graph CHOICE, not a provider FALLBACK (a native Task may well have been available).
/// Same `Inline` mode, so the dispatcher's HOW is unchanged (v2.8 review N10c).
#[derive(Debug, Clone, Copy, Default)]
pub struct GraphInlineSpawner;

impl SubagentSpawner for GraphInlineSpawner {
    fn mechanism(&self) -> &'static str {
        "graph-inline"
    }
    fn mode(&self) -> SpawnMode {
        SpawnMode::Inline
    }
}

/// Resolve the sub-agent spawner for a session's `provider`. Claude gets the NATIVE `Task`; every
/// other provider degrades to the INLINE fallback (the PRD's "équivalent ou fallback INLINE là où il
/// n'y en a pas"). Case-insensitive; an unknown/empty provider is treated as "no native sub-agents"
/// (inline) rather than erroring, so the graph always unrolls. Exotic providers are a non-goal.
pub fn spawner_for_provider(provider: &str) -> Box<dyn SubagentSpawner> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "anthropic" => Box::new(NativeTaskSpawner),
        // Codex / OpenCode / anything else: no native sub-agents → inline fallback.
        _ => Box::new(InlineFallbackSpawner),
    }
}

/// Pick the spawner for a `spawn_subagent` action: when the NODE forces `inline` (an INTERACTIVE
/// effector like the spec-author grill, which a blind Task sub-agent cannot serve — review N2), the
/// inline fallback ALWAYS, whatever the provider; otherwise the provider's own resolution.
pub fn spawner_for(provider: &str, force_inline: bool) -> Box<dyn SubagentSpawner> {
    if force_inline {
        // The GRAPH demanded inline — an honest `graph-inline`, NOT the provider `inline-fallback`.
        Box::new(GraphInlineSpawner)
    } else {
        spawner_for_provider(provider)
    }
}

/// The structured `spawn_subagent` instruction the `nyx_next` verb hands the thin dispatcher: WHICH
/// agent to run, WHAT for (the node's `label`) and — resolved by the provider adapter — in which
/// MODE / by which MECHANISM. The dispatcher reports the sub-agent's diff back on the next
/// `nyx_next` call.
///
/// WHAT IT DOES NOT CARRY IS THE ROLE MANUAL. The action ships IDENTIFIERS, not prose: the sub-agent
/// calls `agent_context(run_id, role, subject?)` and PULLS its own role + state. Shipping the
/// rendered manual made every spawn cost what the manual weighed (~72k chars for `prototype`) for a
/// text nothing in the tick changes between two spawns — while the `sub_workflow` action, three
/// lines and a `run_id`, had been proving the other model next to it all along.
///
/// WHERE the run is, is not part of it: the tick's own `current_node_id` answers that, once, for every
/// action alike (G5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnInstruction {
    pub agent_id: String,
    /// The spawn NODE's label — the step to perform, in the graph's own words.
    pub label: String,
    pub mode: SpawnMode,
    pub mechanism: &'static str,
}

impl SpawnInstruction {
    /// Resolve the instruction for a `spawn_subagent` leaf against a chosen `spawner`.
    pub fn resolve(spawner: &dyn SubagentSpawner, agent_id: &str, label: &str) -> Self {
        SpawnInstruction {
            agent_id: agent_id.to_string(),
            label: label.to_string(),
            mode: spawner.mode(),
            mechanism: spawner.mechanism(),
        }
    }

    /// The JSON the `nyx_next` verb returns for this action.
    pub fn to_json(&self) -> Value {
        json!({
            "type": "spawn_subagent",
            "agent_id": self.agent_id,
            "label": self.label,
            "mode": self.mode.as_str(),
            "mechanism": self.mechanism,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    /// Claude resolves to the NATIVE `Task` mechanism; every other (and unknown) provider degrades
    /// to the INLINE fallback. This is the native↔inline SWITCH.
    #[test]
    fn provider_resolves_native_task_for_claude_inline_otherwise() {
        assert_eq!(
            spawner_for_provider("claude").mode(),
            SpawnMode::NativeTask,
            "Claude gets the native Task sub-agent"
        );
        assert_eq!(
            spawner_for_provider("Claude").mechanism(),
            "claude-native-task"
        );

        for provider in ["opencode", "codex", "", "something-exotic"] {
            let spawner = spawner_for_provider(provider);
            assert_eq!(
                spawner.mode(),
                SpawnMode::Inline,
                "provider {provider:?} has no native sub-agents → inline fallback"
            );
            assert_eq!(spawner.mechanism(), "inline-fallback");
        }
    }

    /// Neither mode is an EXTERNAL session spawn — both mechanisms are in-session (the `claude -p`
    /// proscription). The mode is `native_task` or `inline`, never anything session-external.
    #[test]
    fn no_mode_is_an_external_session_spawn() {
        for provider in ["claude", "opencode", "codex", "whatever"] {
            let spawner = spawner_for_provider(provider);
            let mode = spawner.mode().as_str();
            assert!(
                mode == "native_task" || mode == "inline",
                "provider {provider}: mode {mode} must be an in-session mechanism"
            );
            // The mechanism names an in-session seam, never an external `claude -p` session.
            assert!(
                !spawner.mechanism().contains("-p") && !spawner.mechanism().contains("session"),
                "provider {provider}: mechanism {} must not name an external session",
                spawner.mechanism()
            );
        }
    }

    /// The resolved instruction serializes to the action the `nyx_next` verb returns, carrying the
    /// agent, the node's LABEL (what the spawn is for) and the resolved mode + mechanism — and NO
    /// node_id, which the tick already answers once under `current_node_id` (G5).
    #[test]
    fn instruction_serializes_the_resolved_mode() {
        let native = SpawnInstruction::resolve(&NativeTaskSpawner, "coder", "implement");
        assert_eq!(
            native.to_json(),
            json!({
                "type": "spawn_subagent", "agent_id": "coder", "label": "implement",
                "mode": "native_task", "mechanism": "claude-native-task",
            })
        );

        // The SAME node, on a provider without native sub-agents, resolves to inline — the switch is
        // visible in the emitted action.
        let inline = SpawnInstruction::resolve(&InlineFallbackSpawner, "coder", "implement");
        assert_eq!(inline.mode, SpawnMode::Inline);
        assert_eq!(inline.to_json()["mode"], "inline");
        assert_eq!(inline.agent_id, native.agent_id);
    }

}
