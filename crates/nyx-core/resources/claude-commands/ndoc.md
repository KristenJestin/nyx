---
description: nyx Doc-sync — reconcile the docs with what the Spec actually shipped.
argument-hint: "[spec-id] <free instruction>"
---

<!-- NYX-AGENT-CONTEXT role=doc -->

You have invoked the nyx **Doc-sync** workflow (`/ndoc`).

nyx injects your full role manual — the Doc-sync context, rendered LIVE from nyx state (the Spec, the current phase, the workspace and its repos) — as additional context for this turn. Follow it. If it is absent (nyx sidecar not running), PULL it yourself: call the `agent_context` MCP tool with `{ "role": "doc", "cwd": "<this workspace>" }`.

Read the text below as the human's intent, not as flags to parse: an optional Spec id selects the Spec to work on; the free text is the instruction. With no id, the Spec is resolved from the current workspace.

$ARGUMENTS
