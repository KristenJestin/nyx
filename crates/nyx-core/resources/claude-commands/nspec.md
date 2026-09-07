---
description: nyx Spec Author — grill the intent, write the Spec, decompose it into phases and tasks.
argument-hint: "[spec-id] <free instruction>"
---

<!-- NYX-AGENT-CONTEXT role=spec -->

You have invoked the nyx **/nspec** command.

nyx injects your MANUAL for this command at `UserPromptSubmit` — it is the Spec Author manual, and it
says what to do, in what order, and what you may not do. Follow it. If it is absent (the nyx sidecar
is not running, or the plugin hook did not fire), PULL it yourself: call the `agent_context` MCP tool
with `{ "role": "spec", "cwd": "<this workspace>" }` — plus `"spec_id": "<the reference, exactly as
typed>"` when the text below opens with one.

Read the text below as the human's intent, not as flags to parse: a leading Spec REFERENCE selects
the Spec to work on, and everything after it is the instruction. A reference is the canonical id,
the human key (e.g. KEY-3), '#3', or the bare number 3. nyx owns that grammar — PASS THE TOKEN
VERBATIM and let nyx translate it; never resolve it yourself, and never drop it.

If nyx REFUSES the reference (it designates no Spec, or it designates several), STOP and ASK the
human which Spec they meant — the refusal names what the project actually holds, so quote it. Never
substitute a Spec of your own choosing, and never author a new one to get past a refused reference:
they named an existing Spec, and creating a second is how a duplicate of the very Spec they meant
gets ordered.

With NO reference at all, you author a NEW Spec in this project — so a workspace anchoring zero
Specs, or several, does not refuse this command. Search first (`idea_search`, `spec_list`): if an
open Spec or idea already covers the intent, say so and continue THAT one instead of minting a twin.

$ARGUMENTS
