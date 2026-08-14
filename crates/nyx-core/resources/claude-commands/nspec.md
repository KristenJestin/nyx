---
description: nyx Spec Author — grill the intent, write the Spec, decompose it into phases and tasks.
argument-hint: "[spec-id] <free instruction>"
---

<!-- NYX-AGENT-CONTEXT role=dev scope=nspec -->

You have invoked the nyx **/nspec** workflow.

nyx OPENS this flow's run and injects your DISPATCHER manual carrying its `run_id` — you drive the
nspec flow through the orchestration engine (`nyx_next` in a loop; the graph spawns the Spec-author
effector, inline, so it can grill you). Follow it. If both are absent (nyx sidecar not running), PULL
them yourself: call `run_start` with `{ "scope": "nspec", "cwd": "<this workspace>" }`, then the
`agent_context` MCP tool with `{ "role": "dev", "scope": "nspec", "run_id": "<the run it opened>" }`.

Read the text below as the human's intent, not as flags to parse: an optional Spec id selects the Spec to work on; the free text is the instruction. With no id, a fresh Spec is created through the flow.

$ARGUMENTS
