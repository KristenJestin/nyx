---
description: nyx Dev Orchestrator — drive a ready Spec through the build/review/done flow via the orchestration engine.
argument-hint: "[spec-id] <free instruction>"
---

<!-- NYX-AGENT-CONTEXT role=dev scope=ndev -->

You have invoked the nyx **/ndev** workflow.

nyx OPENS this flow's run and injects your DISPATCHER manual carrying its `run_id` — you drive the
ndev flow through the orchestration engine (`nyx_next` in a loop; the graph spawns the coder + auditor
effectors and imposes each Spec transition itself). Follow it. If both are absent (nyx sidecar not
running), PULL them yourself: call `run_start` with `{ "scope": "ndev", "cwd": "<this workspace>" }`,
then the `agent_context` MCP tool with `{ "role": "dev", "scope": "ndev", "run_id": "<the run it opened>" }`.

Read the text below as the human's intent, not as flags to parse: an optional Spec id selects the Spec
to work on; the free text is the instruction. With no id, the Spec is resolved from the current workspace.

$ARGUMENTS
