---
description: nyx Dev Orchestrator — drive a ready Spec through the build/review/done flow.
argument-hint: "[spec-id] <free instruction>"
---

<!-- NYX-AGENT-CONTEXT role=dev -->

You have invoked the nyx **/ndev** command.

nyx injects your MANUAL for this command at `UserPromptSubmit` — it is the Dev Orchestrator manual,
and it says what to do, in what order, and what you may not do. Follow it. If it is absent (the nyx
sidecar is not running, or the plugin hook did not fire), PULL it yourself: call the `agent_context`
MCP tool with `{ "role": "dev", "cwd": "<this workspace>" }` — plus `"spec_id": "<the reference,
exactly as typed>"` when the text below opens with one.

Read the text below as the human's intent, not as flags to parse: a leading Spec REFERENCE selects
the Spec to work on, and everything after it is the instruction. A reference is the canonical id,
the human key (e.g. KEY-3), '#3', or the bare number 3. nyx owns that grammar — PASS THE TOKEN
VERBATIM and let nyx translate it; never resolve it yourself, and never drop it.

If nyx REFUSES the reference (it designates no Spec), STOP and ASK the human which Spec they meant —
the refusal names what the project actually holds, so quote it. Never substitute a Spec of your own
choosing, and never author a new one to get past a refused reference: this command works ON a Spec
the human named.

With NO reference at all, the Spec is resolved from the current workspace — the ONE active Spec
anchored on it. Zero anchored, or several, resolves NONE: this command works ON a Spec, so it is
refused rather than started on a guess, and the refusal lists the candidates for you to pass as the
reference. nyx never picks the most recent one.

$ARGUMENTS
