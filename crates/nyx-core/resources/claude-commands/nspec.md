---
description: nyx Spec Author — grill the intent, write the Spec, decompose it into phases and tasks.
argument-hint: "[spec-id] <free instruction>"
---

<!-- NYX-AGENT-CONTEXT role=dev scope=nspec -->

You have invoked the nyx **/nspec** workflow.

nyx OPENS this flow's run and injects your DISPATCHER manual carrying its `run_id` — you drive the
nspec flow through the orchestration engine (`nyx_next` in a loop; the graph spawns the Spec-author
effector, inline, so it can grill you). Follow it. If both are absent (nyx sidecar not running), PULL
them yourself: call `run_start` with `{ "scope": "nspec", "cwd": "<this workspace>" }` — plus
`"spec_id": "<the reference, exactly as typed>"` when the text below opens with one — then the
`agent_context` MCP tool with `{ "role": "dev", "scope": "nspec", "run_id": "<the run it opened>" }`.

Read the text below as the human's intent, not as flags to parse: a leading Spec REFERENCE selects
the Spec to work on, and everything after it is the instruction. A reference is the canonical id,
the human key (e.g. KEY-3), '#3', or the bare number 3. nyx owns that grammar — PASS THE TOKEN
VERBATIM and let nyx translate it; never resolve it yourself, and never drop it.

If nyx REFUSES the reference (it designates no Spec), STOP and ASK the human which Spec they meant —
the refusal names what the project actually holds, so quote it. Never substitute a Spec of your own
choosing, and never author a new one to get past a refused reference: they named an existing Spec,
and creating a second is how a duplicate of the very Spec they meant gets ordered.

With NO reference at all, a fresh Spec is created through the flow — so a workspace anchoring zero
Specs, or several, does NOT refuse this flow: it starts bound to no Spec (never an arbitrary one),
keeps the workspace anchor, and reports the ambiguity as a warning.

$ARGUMENTS
