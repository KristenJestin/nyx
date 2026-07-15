---
description: nyx Dev Orchestrator — drive the Spec through coder + auditor sub-agents, the state rule and the review loop.
argument-hint: "[spec-id] <free instruction>"
---

<!-- NYX-AGENT-CONTEXT role=dev -->

You have invoked the nyx **Dev Orchestrator** workflow (`/ndev`).

nyx injects your full role manual — the Dev Orchestrator context, rendered LIVE from nyx state (the Spec, the current phase and its tasks, the workspace and its repos) — as additional context for this turn. Follow it. If it is absent (nyx sidecar not running), PULL it yourself: call the `agent_context` MCP tool with `{ "role": "dev", "cwd": "<this workspace>" }`.

Read the text below as the human's intent, not as flags to parse: an optional Spec id selects the Spec to work on; the free text is the instruction. With no id, the Spec is resolved from the current workspace.

$ARGUMENTS
