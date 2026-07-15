---
description: nyx Spec Author — grill the intent, write the Spec, decompose it into phases and tasks.
argument-hint: "[spec-id] <free instruction>"
---

<!-- NYX-AGENT-CONTEXT role=spec -->

You have invoked the nyx **Spec Author** workflow (`/nspec`).

nyx injects your full role manual — the Spec Author context, rendered LIVE from nyx state (the Spec, its sections, the current phase, the project's other Specs) — as additional context for this turn. Follow it. If it is absent (nyx sidecar not running), PULL it yourself: call the `agent_context` MCP tool with `{ "role": "spec", "cwd": "<this workspace>" }`.

Read the text below as the human's intent, not as flags to parse: an optional Spec id selects the Spec to work on; the free text is the instruction. With no id, the Spec is resolved from the current workspace.

$ARGUMENTS
