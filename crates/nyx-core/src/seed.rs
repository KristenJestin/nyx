//! The BUILT-IN default workflows — the clé-en-main orchestration graphs every project starts with
//! (v2.8 — Harnais : moteur d'orchestration, Phase 4, task `01KY71J32MDD2HF07QT2WMJYKZ`).
//!
//! # What this seeds
//!
//! A NEW project owns no workflow of its own; it uses the built-in defaults (`workflows.project_id`
//! IS NULL), so those must exist. [`seed_builtin_workflows`] — run once at boot after the migrations
//! ([`crate::db::Db::open`]) — ensures the FIVE built-ins exist AND are at the current code revision:
//! a fresh scope is created, and a built-in seeded at an OLDER [`BUILTIN_REVISION`] is UPGRADED in
//! place (its tree replaced), so a default-graph change actually reaches an existing DB (v2.8 review
//! N11). A project customises a flow by FORKING a built-in (`workflow_fork`) into an owned copy; the
//! fork is a separate row and is NEVER touched by the seeder — only the shared built-in is upgraded.
//!
//! Which is why a fork must be the LAST resort, never the price of admission. Forking is how a project
//! stops receiving these upgrades, so nothing a project has to do on day one may require one — that is
//! the whole reason the coder's quality gate declares a command SLOT the project binds
//! ([`BUILD_CHECK_SLOT`]) instead of naming commands the project would have had to fork the graph to
//! set. A fork left behind by its source is reported at `run_start` (`workflow_fork_stale`), because a
//! deliberate fork is legitimate and an unnoticed one is just missing fixes.
//!
//! The five: the three TRIGGERABLE flows (ndev / nspec / ndoc, each owning a Spec lifecycle) plus the
//! two SUB-WORKFLOW-ONLY flows (coder / prototyper — v2.8 lot 2) a `sub_workflow` node resolves BY
//! SCOPE. The latter ship no `/coder` `/prototyper` command and no `run_start` wiring: ndev delegates
//! its per-phase build to the coder child, and nspec's proto branch delegates the design rounds to the
//! prototyper child.
//!
//! # COMPLETE graphs (v2.8 review — nothing thin)
//!
//! All three default graphs are full behavior trees the Phase-2 interpreter walks and the Phase-3
//! `nyx_next` verb drives — each covering the Spec LIFECYCLE it owns, exercising the full vocabulary
//! (a backtrack `loop`, a `selector` fork, a `query` on real Spec state — B2, an `ask_human`-gated
//! ENGAGING transition — B3, and the N9 output primitives), and authoring-VALID (they pass
//! [`crate::mcp_authoring::validate_tree`]). TWO RULES the graphs obey:
//!
//! * **LEAST-DESTRUCTIVE DEFAULT** (v2.8 review N12(d)) — a selector's DEFAULT (`always`) branch is the
//!   least-destructive option: a cancel is reached only by an EXPLICIT condition, never by fall-through.
//! * **NAMED FACT** (v2.8) — a condition tests THE FACT IT NAMES, never an indirect consequence.
//!   `spec_can_transition` answers "am I ALLOWED to take this transition", so it may gate a TRANSITION
//!   and nothing else: it must never stand in for "is there work left" (`spec_open_tasks` /
//!   `phase_open_tasks`) or "is the design frozen" (`design_locked`). A condition that measures a PROXY
//!   is a latent bug — invisible for as long as the two notions coincide by accident. The case that
//!   motivated the rule: ndev's build gate used to read `spec_can_transition(in_review) → can_build` as
//!   a stand-in for "is there work to build?". The two coincide only while the Spec is `in_progress`, so
//!   a STEPPED Spec (per-phase review — some phases done, the rest still `todo`, the Spec already
//!   `in_review`) could NEVER build again: `review` is legal only FROM `in_progress`, so the gate stayed
//!   false forever, while `done` was refused by the completion gate (N32) as long as tasks remained —
//!   the run had NO route at all. The build gate now reads `spec_open_tasks` (the work), and
//!   `spec_can_transition(in_review)` guards the end-of-build TRANSITION (the legality) — each condition
//!   testing the fact it names.
//!
//! ## ndev — start → build (phases × rework, delegated to the coder sub-workflow) → review → done
//!
//! ```text
//! sequence "/ndev"
//! ├─ query "spec status"  → spec_status
//! ├─ selector "start if ready"
//! │  ├─ (spec_status == "ready")  transition "in_progress"     (ENTRY: consent from /ndev, no ask_human)
//! │  └─ (else)                    note "not ready — continuing"      (SAFE default)
//! ├─ query "open tasks?"  spec_open_tasks → open_tasks_at_entry   (NAMED FACT: the gate reads the WORK)
//! ├─ selector "build?"
//! │  ├─ (open_tasks_at_entry truthy)  loop "phases"  until open_tasks is falsy
//! │  │   │                                          (COMPLETION: re-build while tasks remain — N30)
//! │  │   ├─ selector "build this pass"
//! │  │   │  ├─ (audit_verdict == "abandon")  note "abandoned — skip"      (short-circuit on abandon — N25)
//! │  │   │  └─ (else)  loop "build"  until audit_verdict != "revise"   (QUALITY: 'revise' RE-RUNS the coder child)
//! │  │   │                            max_iterations: 8               (a build that stops converging dies — H3)
//! │  │   │     ├─ sub_workflow "coder" {scope: coder, retrieve: child_status → sub_result}   (ONE child per pass)
//! │  │   │     └─ selector "coder result?"
//! │  │   │        ├─ (sub_result.status == "done")  sequence "audit"   (ONE pass per Spec COMMITMENT)
//! │  │   │        │     ├─ spawn "audit user stories"       {auditor, subject: user_stories}
//! │  │   │        │     ├─ spawn "audit testing decisions"  {auditor, subject: testing_decisions}
//! │  │   │        │     ├─ spawn "audit implementation"     {auditor, subject: implementation_decisions}
//! │  │   │        │     ├─ spawn "audit scope"              {auditor, subject: out_of_scope}
//! │  │   │        │     ├─ query "open findings"  spec_open_findings → audit_verdict   (DERIVED, not claimed)
//! │  │   │        │     └─ selector "findings?"
//! │  │   │        │        ├─ (audit_verdict truthy)  ask_human "rework or abandon" {revise/abandon} → audit_verdict
//! │  │   │        │        └─ (else)  note "audit clean"     (the derived 0 is != "revise" ⇒ the loop exits)
//! │  │   │        └─ (else, child blocked/failed)  ask_human "coder blocked" {revise/abandon} → audit_verdict
//! │  │   │                                ('revise' ⇒ re-loop ⇒ FRESH coder child / 'abandon' ⇒ exit to clean end)
//! │  │   └─ selector "recount?"
//! │  │      ├─ (audit_verdict == "abandon")  note "abandoned — skip recount"  (leaves open_tasks unset ⇒ loop exits)
//! │  │      └─ (else)  query "open tasks"  spec_open_tasks → open_tasks       (END of each phases pass)
//! │  └─ (else)  note "no open work — skipping the build"      (SAFE default; the ENTRY DOOR of a Spec
//! │                                                            whose work is already drained — N17c)
//! ├─ query "can review?"  spec_can_transition(in_review) → can_review     (NAMED FACT: it gates a TRANSITION)
//! ├─ selector "to review?"
//! │  ├─ (audit_verdict == "abandon")  note "build abandoned" {clear_gate_block}  (NO in_review, clears block — N25/M5)
//! │  ├─ (can_review)                  transition "in_review"                     (non-engaging)
//! │  └─ (else)                        note "cannot enter review"   (SAFE default: an ALREADY-in_review
//! │                                    Spec never attempts the illegal in_review → in_review — N16)
//! └─ selector "review?"
//!    ├─ (audit_verdict == "abandon")  note "build abandoned — nothing to review"   (abandon clean end — N25)
//!    └─ (else)  sequence "review"
//!       ├─ ask_human "review"  → review                       (PARK for the human — spans sessions, B1)
//!       └─ selector "finish"
//!          ├─ (review == "cancel")   guarded_commit → cancel  (spec_can_transition-guarded, N16-routed)
//!          ├─ (review == "approve")  guarded_commit → done    (its confirm reads spec_deferred_findings
//!          │                                                   FIRST: the question names what it ships with)
//!          └─ (else)                 note "review left open"              (SAFE default)
//! ```
//!
//! The BUILD PASS is DELEGATED to the coder SUB-WORKFLOW (v2.8 lot 2 — the "sub-workflow" doctrine):
//! ONE `coder` child per pass owns the per-phase TASK loop (`phase_next_task` → transition_task(start)
//! → make it green → transition_task(done)), the per-task GATE, and the coder spawns. ndev keeps only
//! the audit / review / Spec-transitions. The "coder result?" selector branches on the child's verified
//! `sub_result.status`: `done` runs the audit; a `blocked`/`failed` child routes to a human
//! revise/abandon ask — converging on the SAME `audit_verdict` loop key, so the N25 abandon plumbing is
//! reached identically. A 'revise' re-loops and opens a FRESH coder child (the per-node child id clears
//! on fold), reworking on the LIVE task list — the review findings the auditor filed included.
//!
//! The AUDIT is a GRILL of the Spec's OWN COMMITMENTS, with a DERIVED verdict. Its four passes are not
//! invented axes ("standards", "quality") that came from nobody's methodology: they are the four
//! sections in which the Spec states what the build owes — `user_stories`, `testing_decisions`,
//! `implementation_decisions`, `out_of_scope` (the [`crate::db::SpecSections`] fields). Each is a
//! BOUNDED question with a NAMED source ("was `out_of_scope` respected?" is checkable; "is the code up
//! to standard?" is not), so each gets its OWN `auditor` spawn carrying that section as its per-node
//! `subject` — one commitment, one pass, full attention. An LLM asked to check six things at once
//! checks all six badly.
//!
//! And the verdict is the NAMED-FACT rule turned on the audit itself: it is READ, never claimed. The
//! audit used to end on a `judge` that recorded whatever the agent declared about its own work — the
//! one thing a review must not take on faith. It now ends on `spec_open_findings`, the count of the
//! Spec's open review findings, i.e. exactly the tasks the four passes just filed (a finding IS a
//! `kind=review` task). That count IS `audit_verdict`, so the arbitration reads the evidence: `0` (a
//! clean audit) is `!= "revise"` and exits the build loop by construction, while a non-zero count is
//! truthy and routes to the human, who alone turns it into 'revise' / 'abandon'. Re-deriving the key
//! from state at EVERY pass is also what keeps it fresh: a claim left on the board goes stale (a
//! 'revise' answered one pass ago would re-loop a since-cleaned build forever), a derived value cannot.
//!
//! ONLY A HIGH/MEDIUM FINDING BUYS ANOTHER ROUND. `spec_open_findings` — like every other end that
//! asks "is there open work" — counts through [`crate::db::reopens_the_build`], which EXCLUDES an
//! open `low`/`info` finding. A real one-phase `/ndev` filed 4 → 1 → 4 → 3 → 3 → 5 → 0 findings per
//! round for 3 h 35 / 109 ticks / $69.48: once its single HIGH was handled, every later round only
//! produced second-order lows, and each low cost a coder sub-workflow plus four audit passes. A low
//! is now RECORDED and LEFT — it stays an open `kind=review` task on the Spec, carrying its severity,
//! so the trace is a fact the ENGINE reads on the next cycle rather than an idea write nothing checks
//! (the weakness the H12 `defer` route already carries). Which is why the two texts around the
//! verdict say what they measure: an audit that filed four lows still reaches the "audit clean" note,
//! and that note must not claim it filed nothing.
//!
//! The BUILD GATE reads the WORK, not a proxy (the NAMED-FACT rule above): `spec_open_tasks` at entry
//! decides whether there is anything to build, so a STEPPED Spec — already `in_review` with phases still
//! `todo` — builds its remaining phases instead of being locked out. The end-of-build `in_review`
//! transition is the one thing `spec_can_transition` legitimately gates: an already-in_review Spec routes
//! to a clean note rather than attempting the illegal `in_review → in_review` (the N16 shape — CHECK
//! before proposing, ROUTE the refusal), and it sits AFTER the build gate so a Spec whose work was
//! already drained still reaches its review.
//!
//! TWO NESTED loops keep quality and completion DISTINCT (N30): the inner "build" loop reworks while
//! the audit still leaves findings (N12c), the outer "phases" loop re-runs the whole build while the
//! Spec still has open
//! tasks (`spec_open_tasks`, which GROWS as the auditor files review-finding tasks). The inner loop
//! carries its OWN bound (`max_iterations: 8`) because its body FILES ITS OWN NEXT ITERATION — see
//! [`BUILD_REWORK_ROUNDS`]. A findings verdict
//! asks the human "rework or abandon" (N25): 'abandon' exits the build cleanly — no blind burn to
//! max_iterations — routing to a "build abandoned" note, NEVER an in_review transition that would leave
//! the Spec half-built in review. `done` (not `in_review`) is the END. Each terminal commit is
//! spec_can_transition-GUARDED and selector-ROUTED (N16): a declined confirmation lands on a clean note,
//! never a refusing transition that freezes the run. The "build abandoned" note carries
//! `clear_gate_block` so any stale block is lifted before the run's clean `done`.
//!
//! ## nspec — entry gate, (skippable) grill, (optional) proto, N16-safe commit
//!
//! ```text
//! sequence "/nspec"
//! ├─ query "spec status"  → spec_status
//! ├─ selector "entry gate"                                   (nyx CHECKS the case — N12)
//! │  ├─ (ready)  sequence [ ask_human "revise?" {yes/no} → revise_ok, fork_spec {gate_on: revise_ok} ]  (N13)
//! │  ├─ (draft)  sequence [ query spec_can_transition target=ready → can_ready,
//! │  │                      selector [ (can_ready) ask_human "grill or finalize?" {grill/ready} → entry_choice
//! │  │                                 (else)      note "draft incomplete — resume grill" ] ]
//! │  └─ (else)   note "create a new Spec"                     (SAFE default = create)
//! ├─ selector "grill?"
//! │  ├─ (entry_choice == "ready")  note "grill skipped"
//! │  └─ (else)  loop "grill" until grill_status == "complete" [ spawn spec inline → ask_human "steer"
//! │             {append answers} → judge "complete?" reads {{answers}} → grill_status ]      (N9)
//! ├─ sequence "proto?"                                       (v2.8 lot 2 — before the ready commit)
//! │  ├─ judge "graphic render?"  → needs_proto  {ui / none}
//! │  └─ selector "proto path"
//! │     ├─ (needs_proto == "ui")  sequence "want a proto?" [ ask_human "go through a proto?" {yes/no} → do_proto,
//! │     │       selector [ (do_proto == "yes")  sequence "prototype"
//! │     │                     ├─ sub_workflow prototyper {retrieve: design_locked, output_key: proto_result}
//! │     │                     └─ selector "proto result?"          (H4: a DEAD child is TESTED, not walked past)
//! │     │                        ├─ (proto_result.status == "done")  note "design settled"
//! │     │                        └─ (else)  ask_human "proto did not complete" {hold/ready} → proto_verdict
//! │     │                  (else)               note "skipped the proto" ] ]
//! │     └─ (else)                 note "no graphic surface — no proto"       (SAFE default = no proto)
//! ├─ query "final status"  → final_status
//! └─ selector "commit"
//!    ├─ (proto_verdict == "hold")  note "prototype dead — left as a draft"   (the H4 STOP, before the commit)
//!    ├─ (final_status == "draft")  sequence [ ask_human "confirm ready" {yes / go / no} → ready_ok,
//!    │                                        selector "route the commitment"
//!    │                                          ├─ (ready_ok=="yes")  transition "ready"
//!    │                                          ├─ (ready_ok=="go")   sequence "ready + a dev workspace"
//!    │                                          │     ├─ query project_worktrees_root → worktrees_root (+ _configured)
//!    │                                          │     ├─ selector [ (worktrees_root_configured falsy) ask_human "workspace root" → worktrees_root
//!    │                                          │     │             (else)                            note "using the configured root" ]
//!    │                                          │     ├─ query workspace_target_free → workspace_free (+ _reason)   (PREVALIDATION)
//!    │                                          │     ├─ selector [ (workspace_free truthy) transition "ready"
//!    │                                          │     │             (else)                  note "target folder taken" ]   (N16)
//!    │                                          │     └─ create_workspace {root_key} {gate_on: workspace_free}
//!    │                                          └─ (else)             note "left as draft" ]   (N16)
//!    └─ (else)                     note "not a draft — nothing to commit"     (SAFE default)
//! ```
//!
//! THE THIRD ANSWER IS A SECOND SHAPE OF YES (revision 21, repaired in 22). 'go' takes the very same
//! engaging `ready` transition 'yes' does — BOTH are affirmative words on their own, compared WHOLE,
//! so the B3 gate is HEAD's and reads nothing less than the entire answer — and it opens the dev
//! workspace ANCHORED to the Spec, through the same `create_workspace` the MCP tool calls (one guarded
//! write: folder + workspace row + anchor, and the anchor moves no status, which is why `ready` comes
//! first). The rider rides on WHICH affirmative was given, never on text appended to one: an answer
//! like "yes with workspace" would oblige a gate to read part of a reply, and a gate that reads part
//! of a reply reads "ok, actually no" as approval. WHERE the folder lands is read as a FACT before it
//! is asked as a question: a project that configured `worktrees_root` is never asked; one that did not
//! is asked once — for a ROOT, written to the very key the setting fills, so BOTH branches derive the
//! Spec's own subfolder under it. NOTHING FILLS the folder — it lands EMPTY, deliberately (depot idea
//! `01M04W3RZ333NC01R216MNXAFK` owns the filling). This branch is a HORS PERIMETRE ASSUME of PRD
//! 01KYA475AX5JBGSS8WK5J8NM1S on the user's decision of 2026-08-18; the note that records it sits on
//! the branch in [`build_nspec`].
//!
//! 'go' IS ONE ANSWER, SO IT IS ONE COMMITMENT (revision 22). "Ready AND a workspace" cannot half
//! happen: everything knowable up front — where the folder goes, and whether a workspace of the
//! project already holds it (`workspace_target_free`) — is settled BEFORE the `ready` transition, and
//! the transition and the `create_workspace` then hang off that one verdict. The shape it replaces
//! committed `ready` first and opened the workspace after; the day the creation failed, the Spec sat
//! `ready` with no workspace and nothing said so. It is the prevalidation rule this PRD already
//! applies to the disk ("prevalidate, then mkdir"), applied to the TRANSITION.
//!
//! A `sub_workflow` CHILD THAT DIED IS A FACT THE PARENT MUST TEST (H4). The child's status lands on
//! the parent board (`proto_result.status`) whatever it is — and a value nothing reads changes nothing:
//! nspec walked from a `failed` prototyper straight to its ready commit, in one tick, in silence. The
//! "proto result?" selector is the same treatment ndev's "coder result?" already gave its own child:
//! `done` continues, anything else hands the human the decision. Their answer travels to the COMMIT
//! selector, because that — marking the Spec ready — is the step a settled design was the condition of.
//!
//! ## ndoc — sync docs when the Spec is completed, then finish OR abandon
//!
//! ```text
//! sequence "/ndoc"
//! ├─ query "spec status"  → spec_status                      (branch on the REAL Spec state, B2)
//! └─ selector "sync or skip"
//!    ├─ (spec_status == "completed")  sequence "sync"
//!    │     ├─ loop "syncing"  until decision != "changes"    (BACKTRACK: re-sync while the human asks)
//!    │     │  ├─ spawn_subagent "sync docs"  {agent_id: doc}
//!    │     │  └─ ask_human "review"  {approve/changes/cancel} → decision    (CLOSED set — N19 + J1)
//!    │     └─ selector "finish or abandon"      (NO default: an abandon is SAID, never fallen into)
//!    │        ├─ (decision == "approve")  note "Docs synced — approved."
//!    │        └─ (decision == "cancel")   note "Doc sync abandoned."
//!    └─ (else)                         note "The Spec is not completed — nothing to sync yet."
//! ```
//!
//! ndoc does NOT transition the Spec: docs are orthogonal to the Spec lifecycle (a completed Spec is
//! not un-completed by abandoning a doc pass), so its "abandon" branch is a plain note.
//!
//! Its `review` is where J1 was OBSERVED, and both halves of that failure are graph-level. The
//! question took FREE TEXT while the loop's `until` and the fork below compared it BY EQUALITY, so
//! "changes — the DEL fix is right but it draws the parser's line in the wrong place" matched
//! neither: the loop exited instead of looping, the fork's catch-all wrote "Doc sync abandoned", 160
//! un-reviewed lines stayed in the tree and the run reported DONE. The answer set is now DECLARED
//! (so N19 refuses that reply and re-asks it), and the fork NAMES the cancel it acts on instead of
//! reaching it by fall-through — the two things that let a decision be taken on a value nothing
//! constrained.
//!
//! ## coder (sub-workflow-only) — drain the current phase, one gate per task
//!
//! ```text
//! sequence "coder"
//! └─ loop "tasks"  until phase_open_tasks is falsy            (COMPLETION: drain the phase)
//!    ├─ query "next task"  phase_next_task  → task_id
//!    ├─ selector "have a task?"
//!    │  ├─ (task_id set)  sequence "drive task"
//!    │  │   ├─ transition_task start {task_key: task_id}
//!    │  │   ├─ query "how does it close?"  task_close_route → task_close   (WHOSE task is it — H9)
//!    │  │   ├─ selector "whose task?"
//!    │  │   │  ├─ (task_close == "verify")  sequence "human action"   (a kind=human task NEVER reaches the coder)
//!    │  │   │  │     ├─ ask_human "your action" {done/skip} → human_action
//!    │  │   │  │     └─ selector [ (skip) transition_task skip {reason} / (else) note "performed" ]
//!    │  │   │  └─ (else)  loop "make it green" until gate_green == true
//!    │  │   │              [ spawn_subagent "implement" {coder} → gate_check "checks" {slot: build_check}
//!    │  │   │                → gate_green ]
//!    │  │   ├─ query "how does it close now?"  task_close_route → task_close    (the OUTCOME, re-derived)
//!    │  │   └─ selector "close the task"
//!    │  │      ├─ (task_close == "done")      transition_task done   {task_key: task_id}
//!    │  │      ├─ (task_close == "verify")    transition_task verify {task_key: task_id}
//!    │  │      ├─ (task_close == "escalate")  sequence "blocked — escalate"          (H12)
//!    │  │      │     ├─ ask_human "the coder is blocked" {code/spec/defer} → blocked_route
//!    │  │      │     │                                    {grant_spec_write_on: "spec"}
//!    │  │      │     └─ selector "the human's ruling"
//!    │  │      │        ├─ (spec)   sequence [ spawn "correct the Spec's stale text" {spec},
//!    │  │      │        │                      note "window closed" {revoke_spec_write} ]
//!    │  │      │        ├─ (defer)  sequence [ spawn "record it as a backlog idea" {spec},
//!    │  │      │        │                      transition_task skip {reason} ]
//!    │  │      │        └─ (else)   note "the finding stands"   (SAFE default: retry, nothing written)
//!    │  │      └─ (else)  note "nothing to close"     (SAFE default: already terminal / never started)
//!    │  └─ (else)  note "phase drained — no open task"
//!    └─ query "phase open tasks"  phase_open_tasks  → phase_open_tasks   (recount → loop exit)
//! ```
//!
//! Invoked as a `sub_workflow` by ndev, ONE child per phase. The child owns the per-task gate + the
//! task-lifecycle nodes; it has no Spec authority of its own — a gate that never greens burns the
//! inner loop → child `failed`, which ndev's "coder result?" selector routes to the human. Its
//! `ask_human` leaves BUBBLE UP through the `sub_workflow` node to the top-level human, which is what
//! lets the escalation live HERE, where the task is, rather than in ndev: a blocked task cannot be
//! handed back to the parent — it keeps `phase_open_tasks` non-zero, so the child would re-pick it and
//! re-spawn the coder on it to its iteration cap. The decision belongs where the task is.
//!
//! A TASK CLOSES BY THE ROUTE ITS OWN STATE ALLOWS (H9). The close was an unconditional
//! `transition_task done`, and `done` is exactly what the task guard-rails refuse in two NOMINAL cases
//! — both seen in ONE live session: a `kind=human` task (nspec creates them spontaneously —
//! provisioning, access) closes only through the VERIFIED path, and a task the coder BLOCKED closes
//! through nothing at all. A refused `transition_task` REPLAYS, so both left the child run parked and
//! re-raising the same `invalid_state` on every later tick, with no exit inside the graph: one `/ndev`
//! failed outright, another stalled until the user intervened by hand. So the graph now READS the
//! route (`task_close_route`) instead of assuming it — before the work, to know whose task it is, and
//! again after, to know how it ended.
//!
//! Its `checks` gate ships BLOCKING and declares a command SLOT ([`BUILD_CHECK_SLOT`]) rather than
//! naming commands, and `nyx_next` REFUSES to advance past a gate that arms nothing: a fresh nyx STOPS
//! there until the project registers its real checks (add_command) and BINDS them to the slot
//! (`slot_bind`). `run_start` reports it — the scan follows the `sub_workflow` hop, so a `/ndev` is
//! told before it spawns anything, not halfway through a build.
//!
//! THE SLOT IS WHAT KEEPS THIS GRAPH SHARED. A `command_ids` list is PROJECT data (`bun test` here,
//! `cargo test -p nyx-core` there) and this graph belongs to every project, so naming commands on the
//! node left exactly one way to arm it: FORK the built-in — which the seeder never upgrades — and the
//! first gesture a real project made therefore froze its `coder` flow at that day's revision, silently
//! missing every fix written since (a live fork sat at r11 while the built-in shipped r16, without
//! H9, H12 or J2). The node now declares WHAT must be verified and the project answers WITH WHAT, so
//! there is nothing left to fork and the upgrades keep flowing. Forks stay possible for real
//! customisation — and a fork that has fallen behind its source is now REPORTED by `run_start`
//! (`workflow_fork_stale`), which is what stops the remaining ones rotting unseen.
//!
//! ## prototyper (sub-workflow-only) — design rounds until the freeze
//!
//! ```text
//! sequence "prototyper"
//! └─ loop "design rounds"  until design_locked == true
//!    ├─ loop "compose a round"  until design_round_pages truthy   max_iterations: 2
//!    │  ├─ spawn_subagent "prototype"  {agent_id: prototype}  (opens / composes / presents a round)
//!    │  └─ query "anything to elect?"  design_round_pages → design_round_pages   (the NAMED FACT)
//!    ├─ ask_human "election"   → election                     (BUBBLES UP to the top-level human;
//!    │                                                         NAMES {{design_round_pages_reason}})
//!    ├─ ask_human "feedback"   {lock/changes} → feedback      (BUBBLES UP)
//!    ├─ selector "lock or iterate?"
//!    │  ├─ (feedback == "lock")  spawn_subagent "distill" {prototype}   (proto_distill → design_lock)
//!    │  └─ (else)                note "iterate — open another round"     (SAFE default = keep iterating)
//!    └─ query "design locked?"  design_locked  → design_locked
//! ```
//!
//! Invoked as a `sub_workflow` by nspec's proto branch. Its `ask_human` leaves BUBBLE UP through the
//! `sub_workflow` node to the top-level human (the child's dispatcher can't answer a human); the exit
//! is the `design_locked` FACT (a real `proto_distill` freeze), never the human's word alone.
//!
//! A QUESTION IS NOT PUT ABOUT AN EMPTY SET (L2). The election used to run unconditionally after the
//! spawn, and the graph never checked the round held anything to elect. On the prototyper's first
//! NOMINAL run the effector laid down a skeleton and stopped at a throwaway probe variant — 2 pages,
//! 1 version, 1 variant, ZERO pinned pages — and nyx asked the human to "elect the RETAINED variant
//! per page" over a round holding no page at all. The election is free-form, so any answer passed;
//! 'lock' routed to `distill`, which distilled nothing; `design_locked` stayed false; and the outer
//! loop bought a fresh prototype sub-agent to arrive at the same place. H3's guard could not fire —
//! the run DID carry a Spec, so `design_locked` was reachable in principle, just not with this
//! content. The "compose a round" loop is the check the question was missing: the round is composed
//! until `design_round_pages` (the pinned manifest — the set the UI shows and `proto_distill` refuses
//! to freeze when empty) is non-empty, and an effector that will not compose one costs ONE retry
//! ([`PROTO_ROUND_ATTEMPTS`]) before the child dies naming what never held. That death is not a dead
//! end: nspec's H4 branch catches the failed child and asks the human "proto did not complete"
//! {hold/ready} — the retry-or-abandon question, put where the graph can act on the answer.

use diesel::result::QueryResult;
use diesel::{Connection, SqliteConnection};
use serde_json::{json, Value};

use crate::db;
use crate::db::{
    NODE_TYPE_ASK_HUMAN as ASK_HUMAN, NODE_TYPE_CREATE_WORKSPACE as CREATE_WORKSPACE,
    NODE_TYPE_FORK_SPEC as FORK_SPEC, NODE_TYPE_GATE_CHECK as GATE_CHECK, NODE_TYPE_JUDGE as JUDGE,
    NODE_TYPE_LOOP as LOOP, NODE_TYPE_NOTE as NOTE, NODE_TYPE_QUERY as QUERY,
    NODE_TYPE_SELECTOR as SELECTOR, NODE_TYPE_SEQUENCE as SEQUENCE,
    NODE_TYPE_SPAWN_SUBAGENT as SPAWN, NODE_TYPE_SUB_WORKFLOW as SUB_WORKFLOW,
    NODE_TYPE_TRANSITION as TRANSITION, NODE_TYPE_TRANSITION_TASK as TRANSITION_TASK,
};

/// The current code revision of the built-in default graphs. BUMP this whenever a `build_*` graph
/// changes so an EXISTING DB's built-in is upgraded on the next boot ([`ensure_builtin`] replaces the
/// tree when the stored `workflows.builtin_revision` is older — v2.8 review N11).
///
/// Revision 22 REPAIRS revision 21's workspace branch, on the two defects a live run found the day it
/// shipped (human review finding `01M0B5AG0XR4BG6DSGEFGKW6C4`). The question now asks for a ROOT and
/// writes the SAME board key the configured setting fills, so both branches DERIVE the Spec's
/// subfolder under it — 21 took the answer as the final folder, which registered the user's whole
/// worktrees root as one workspace and then collided with itself on every later run. And the `ready`
/// transition now sits UNDER a prevalidation of that folder instead of before the creation, so 'go'
/// is all-or-nothing: 21 committed `ready` first and left the Spec ready with no workspace when the
/// creation failed. A DB holding 21 MUST be upgraded off it — its nspec asks for a root and would use
/// the answer as a workspace path.
///
/// Revision 21 gives nspec's final consent a THIRD answer: 'go' marks the Spec ready exactly as
/// 'yes' does — both are affirmative words the B3 gate accepts WHOLE — and a second selector on that
/// same answer then OPENS the dev workspace anchored to it (a `create_workspace` node — the folder
/// goes under the project's `worktrees_root` when it is configured, and the graph asks for a path
/// only when it is not). HORS PERIMETRE ASSUME of PRD 01KYA475AX5JBGSS8WK5J8NM1S, user decision of
/// 2026-08-18 — the full note is on the branch itself in [`build_nspec`]. And it is THIS bump that
/// makes the branch real for a DB that already exists: without it `ensure_builtin` would see a
/// built-in at the current revision and leave the old tree in place, so the new answer would exist in
/// the code and in no running nyx. It is 21 rather than 20 because a revision 20 of the same branch
/// was installed in working DBs while its consent answer ('yes with workspace') still needed a gate
/// that read PART of a reply; that reading was retracted, and a DB holding 20 must be upgraded off it
/// or its nspec asks a question whose answer no longer opens the transition.
///
/// Revision 19 stops
/// the prototyper putting a question to a human about an EMPTY set: the election runs only once the
/// round actually holds pages to elect on (`design_round_pages`, checked by a "compose a round" loop
/// that costs [`PROTO_ROUND_ATTEMPTS`] effector passes and then dies naming what never held), and the
/// question NAMES the pages and variants on offer instead of asking the human to choose from a
/// surface nyx never showed them. Revision 18 makes
/// ndev's CLOSING QUESTION say what completing leaves behind: it reads `spec_deferred_findings` one
/// node before the confirm and interpolates the count into the prompt, the same gesture "rework or
/// abandon" already makes with `{{audit_verdict}}`. The completion note stating it existed already —
/// but it fires AFTER the transition, so the human was asked the bare "Mark the Spec completed?"
/// while six findings stayed open and learned of them once the Spec was closed. Revision 17 is what
/// makes every revision AFTER it reach a real project: the coder's `checks` gate declares the
/// [`BUILD_CHECK_SLOT`] SLOT instead of naming `command_ids`. Arming it used to mean FORKING this
/// built-in — a command instance id is project data, the graph is shared, and the authoring layer
/// (rightly) refuses to edit a seeded default — and a fork is never upgraded, so the first gesture any
/// project had to make froze its `coder` graph at that day's revision. The gate now says WHAT it needs
/// verified; the project says with what (`slot_bind`), and the built-in stays shared. Revision 16 makes
/// ndev's audit SAY what its verdict now measures: only a high/medium finding re-opens a build
/// ([`crate::db::reopens_the_build`]), so the "audit clean" note no longer claims the audit filed
/// nothing when it filed lows, and the rework ask names the severities it counted. The graph SHAPE is
/// untouched — the deferral lives in the shared predicate the four ends read — but a note that lies is
/// still a note that lies. Revision 15 closes
/// ndoc's `review`: the question DECLARES the answer set its own loop and fork compare by equality
/// (J1), and the fork NAMES the cancel it acts on instead of falling through to it. Revision 14 gives
/// ndev's inner "build" loop the explicit iteration bound its shape demands
/// ([`BUILD_REWORK_ROUNDS`]): a loop whose body files the work that re-opens it cannot run on a
/// default nobody chose. Revision 11 rebuilds
/// ndev's AUDIT: FOUR `auditor` passes, one per COMMITMENT the Spec itself states (`user_stories`,
/// `testing_decisions`, `implementation_decisions`, `out_of_scope` — each carried as the spawn's
/// per-node `subject`) in place of the single auditor picking its own axis, and a verdict DERIVED from
/// the findings actually filed (`spec_open_findings` → `audit_verdict`) in place of the `judge` that
/// recorded the agent's self-declared one. Revision 13 rebuilds the coder's task CLOSE: the route is
/// READ from the task (`task_close_route`) instead of an unconditional `done` — a `kind=human` task is
/// settled by the human and closed through `verify`, and a task the coder BLOCKED reaches an
/// escalation gate whose 'spec' answer authorizes, for that gate alone, the Spec-prose correction the
/// freeze otherwise forbids (H9 + H12). Revision 12 gives nspec's prototyper child the treatment
/// ndev's coder child already had (H4): a "proto result?" selector TESTS `proto_result.status`, and a
/// child that died routes to the human — whose 'hold' stops the flow at the ready COMMIT instead of
/// letting it mark the Spec ready on a design that was never settled. Revision 10 applies
/// the NAMED-FACT rule to ndev's build gate: it reads `spec_open_tasks` ("is there work to build?")
/// instead of the `spec_can_transition(in_review)` PROXY that locked a stepped, already-in_review Spec
/// out of its remaining phases, and the end-of-build `in_review` transition is now
/// `spec_can_transition`-guarded + note-routed. Revision 9 drops the
/// leading `→ ` from the seeded `transition` labels (the visualiser renders the flow direction; the
/// label carries only the target state). Revision 8 is the
/// lot-2 "sub-workflow" doctrine: ndev DELEGATES each build pass to the coder SUB-WORKFLOW (the coder
/// child owns the per-phase task loop + per-task gate + the coder spawns; ndev keeps only
/// audit/review/Spec-transitions), and nspec SPLICES a prototype branch (after the grill, before the
/// ready commit) that delegates the design rounds to the prototyper sub-workflow. It also seeds the two
/// new sub-workflow-only built-ins (coder / prototyper). Revision 7 gave ndev's build gate a RED-gate
/// PATH (M5). Revision 6 reworked the build into TWO NESTED loops keeping quality (the inner rework
/// loop) and completion (the outer phases loop) DISTINCT (N30) with a 'revise'-audit ABANDON (N25); on
/// top of revision 5's N17 lifecycle-entry work and revision 4's N16/N19/spec_can_transition
/// corrections. Every earlier built-in (a lower revision or a pre-N11 NULL, treated as 0) upgrades on
/// the first boot.
const BUILTIN_REVISION: i32 = 22;

/// The SLOT the seeded coder's `checks` gate declares — the name a project binds its real build check
/// to (`slot_bind slot="build_check" command_ids=[…]`, [`crate::mcp_authoring::slot_bind`]).
///
/// It is a name for a QUESTION ("has this project's build check passed?"), which is the only thing a
/// graph shared by every project can state: the ANSWER — `bun test`, `cargo test -p nyx-core`, a
/// wrapper script — is per-project by nature, and it was naming the answer in a shared graph that made
/// forking mandatory and upgrades impossible.
pub const BUILD_CHECK_SLOT: &str = "build_check";

/// How many REWORK rounds ndev's inner "build" loop runs before it gives up (revision 14).
///
/// The engine's [`crate::engine::DEFAULT_MAX_LOOP_ITERATIONS`] is a BACKSTOP against a mis-authored
/// loop — it was never a budget anyone chose for THIS one, and this one needs a chosen number: its
/// body files its own next iteration. Each round runs a coder child and FOUR auditor passes, and
/// every finding those passes file is a task, which keeps the Spec's work non-empty and re-opens the
/// build. Nothing in the cycle guarantees it terminates.
///
/// 8 is TWICE the worst convergence measured on a real `/ndev` (4 rounds: 4 findings → 2 → 2 → 0, one
/// coder child + four audit passes each, $14.60 for the session). Twice, because a build that is
/// twice as contentious as the worst one observed is still plausibly legitimate work; and only twice,
/// because past that the audit is not converging — every round is HUMAN-approved ('revise' to
/// "rework or abandon"), so an 8th identical answer is the human re-affirming a decision the evidence
/// has stopped supporting, at ~$3.65 a round. The engine's 25 would spend three times the worst real
/// session to learn the same thing.
///
/// The exhaustion is not a dead end: the loop dies through [`crate::engine::EngineError::LoopLimit`],
/// which names the condition that never held and what the board last carried for it (H3), and the
/// tasks are untouched — a `/ndev` re-run resumes the very same work with a fresh run.
///
/// The OUTER "phases" loop deliberately keeps the engine default: its iteration count is the number
/// of PHASES a Spec has (a structural quantity of the plan, not a rework count), which is exactly
/// what 25 was sized for.
const BUILD_REWORK_ROUNDS: u32 = 8;

/// How many times the prototyper asks its effector to COMPOSE a round before it gives up (L2).
///
/// A round that pins no page has nothing to elect, and the graph used to ask anyway. The honest
/// question to put to a human here is "this round produced no candidate — retry, or abandon?", and it
/// is NOT the question this loop asks, because the prototyper cannot act on an abandon: its only exit
/// is `design_locked == true`, a FACT no answer may write (a human's word standing in for a real
/// `proto_distill` freeze is precisely what the design forbids), and a [`crate::engine::Condition`]
/// tests ONE key with no disjunction — so an "abandon" answer would be a promise the graph cannot
/// keep, which is the J1 defect all over again. What the graph CAN do honestly is bound the asking and
/// then die loudly, and the human is asked one level up, by the question nspec already owns for
/// exactly this case: a prototyper child that did not deliver reaches "proto did not complete"
/// {hold/ready}, whose 'hold' leaves the Spec a draft. That is the abandon, put where it can be acted
/// on.
///
/// 2 — ONE retry, and it is not the H3 burn it looks like. The failure observed was a HALF-composed
/// round: 2 pages and a version existed, only the manifest PIN was missing, so the second effector
/// pass starts from work already on disk rather than from nothing, and finishing it is a plausible
/// single step. Only one, because a second empty round is the effector failing on state it can already
/// see, and a third pass has no new information to fail differently on. So the whole cost of an
/// unproductive effector is bounded at ONE extra sub-agent, after which the run dies through
/// [`crate::engine::EngineError::LoopLimit`] naming the condition (`design_round_pages is truthy`) and
/// what it last read (`0`) — never the twenty rounds the outer loop would otherwise have bought.
const PROTO_ROUND_ATTEMPTS: u32 = 2;

/// Ensure the FIVE built-in default workflows exist AND are at the current [`BUILTIN_REVISION`]: the
/// three triggerable flows (ndev / nspec / ndoc) plus the two SUB-WORKFLOW-ONLY flows (coder /
/// prototyper) a `sub_workflow` node resolves by scope (v2.8 lot 2). Called once at boot after the
/// migrations; returns `Err` only on a real DB failure (the caller propagates it). Fresh scopes are
/// created; a built-in seeded at an OLDER revision is UPGRADED in place (N11).
pub fn seed_builtin_workflows(conn: &mut SqliteConnection) -> QueryResult<()> {
    ensure_builtin(
        conn,
        db::WORKFLOW_SCOPE_NDEV,
        "ndev — dev loop (default)",
        build_ndev,
    )?;
    ensure_builtin(
        conn,
        db::WORKFLOW_SCOPE_NSPEC,
        "nspec — spec authoring (default)",
        build_nspec,
    )?;
    ensure_builtin(
        conn,
        db::WORKFLOW_SCOPE_NDOC,
        "ndoc — doc sync (default)",
        build_ndoc,
    )?;
    // The SUB-WORKFLOW-ONLY built-ins (v2.8 lot 2): reached only as a `sub_workflow` child (no
    // `/coder` `/prototyper` command, no `run_start` wiring). ndev delegates its per-phase build to
    // the coder child; nspec's proto branch delegates the design rounds to the prototyper child.
    ensure_builtin(
        conn,
        db::WORKFLOW_SCOPE_CODER,
        "coder — per-phase build loop (default)",
        build_coder,
    )?;
    ensure_builtin(
        conn,
        db::WORKFLOW_SCOPE_PROTOTYPER,
        "prototyper — design rounds (default)",
        build_prototyper,
    )?;
    Ok(())
}

/// Ensure the built-in workflow for `scope` (owner = NULL, the built-in namespace) exists AND carries
/// the current [`BUILTIN_REVISION`], in ONE transaction:
///
/// * ABSENT ⇒ create it, build its tree, wire the root, stamp the revision.
/// * present at an OLDER revision (or a pre-N11 NULL) ⇒ UPGRADE it — wipe the nodes, rebuild the tree
///   from the current `build`, re-point the root, re-stamp the revision. This is what makes a default-
///   graph change actually reach an existing DB (v2.8 review N11). A PROJECT FORK of this built-in is
///   a separate row and is NOT touched (it is the user's customisation). A run mid-flight on the
///   replaced built-in loses its cursor to the node wipe (ON DELETE SET NULL) and its stale traversal
///   makes `build_engine` REFUSE the resume rather than replay from the root — no silent corruption.
///   That run is MARKED first ([`db::mark_runs_graph_replaced`]) so `run_start` can tell its caller
///   WHAT destroyed it instead of guessing (M4).
/// * present at the current (or a newer) revision ⇒ left as-is (idempotent).
///
/// The lookup + write live INSIDE the transaction: a check-then-write split would be a TOCTOU (two
/// boots racing on the same DB file could each seed a duplicate "built-in" — there is no UNIQUE index
/// on `(project_id, scope)` to catch it). The built-in is the `forked_from IS NULL` row of the scope
/// (a stray unowned fork never shadows it — N4).
fn ensure_builtin(
    conn: &mut SqliteConnection,
    scope: &str,
    name: &str,
    build: fn(&mut SqliteConnection, &str) -> QueryResult<String>,
) -> QueryResult<()> {
    conn.transaction(|tc| {
        let existing = db::list_workflows(tc, None)?
            .into_iter()
            .find(|w| w.scope == scope && w.forked_from.is_none());
        match existing {
            // Already at the current (or a newer) revision — nothing to do.
            Some(wf) if wf.builtin_revision.unwrap_or(0) >= BUILTIN_REVISION => Ok(()),
            // Present but stale — REPLACE the tree in place (N11).
            Some(wf) => {
                // M4 — the wipe below DESTROYS every live run walking this built-in: it nulls their
                // cursor and strands their traversal, so `run_start` drops them on the next call. We
                // upgrade ANYWAY (the built-in is SHARED by every project — deferring the upgrade
                // until one abandoned run finishes would hold the new default graph hostage for the
                // whole machine, and would leave that run walking an old tree while the code expects
                // the new one). But the loss stops being incidental: each affected run is MARKED with
                // what replaced its graph, so run_start's warning names the real cause.
                let marked = db::mark_runs_graph_replaced(
                    tc,
                    &wf.id,
                    &json!({
                        "cause": "builtin_upgrade",
                        "from_revision": wf.builtin_revision,
                        "to_revision": BUILTIN_REVISION,
                    }),
                )?;
                if !marked.is_empty() {
                    eprintln!(
                        "nyx: upgrading the built-in '{}' workflow to revision {BUILTIN_REVISION} \
                         replaces its graph — {} live run(s) lose their progress and will be \
                         restarted: {}",
                        wf.scope,
                        marked.len(),
                        marked.join(", ")
                    );
                }
                db::delete_nodes_for_workflow(tc, &wf.id)?;
                let root_id = build(tc, &wf.id)?;
                db::set_workflow_root(tc, &wf.id, Some(&root_id))?;
                db::set_builtin_revision(tc, &wf.id, BUILTIN_REVISION)?;
                Ok(())
            }
            // Not seeded yet — create it at the current revision.
            None => {
                let wf = db::create_workflow(tc, None, name, scope, None)?;
                let root_id = build(tc, &wf.id)?;
                db::set_workflow_root(tc, &wf.id, Some(&root_id))?;
                db::set_builtin_revision(tc, &wf.id, BUILTIN_REVISION)?;
                Ok(())
            }
        }
    })
}

/// Insert one node and return its id — a thin wrapper over [`db::create_node`] that takes the config
/// as a JSON [`Value`] (serialized here), keeping the tree builders readable.
fn node(
    tc: &mut SqliteConnection,
    wf: &str,
    parent: Option<&str>,
    order: i32,
    node_type: &str,
    label: &str,
    config: Value,
) -> QueryResult<String> {
    Ok(db::create_node(tc, wf, parent, order, node_type, label, &config.to_string())?.id)
}

/// A GUARDED, N16-safe terminal commit branch: `query spec_can_transition(target)` → selector
/// [ CAN → sequence[ (query context), ask_human confirm {yes/no}, selector[ 'yes' → transition / note ] ] / note ].
/// Because the graph CHECKS legality BEFORE it proposes (spec_can_transition) AND routes the answer
/// (never a bare transition that could refuse), a commit can NEVER freeze the run — whatever the Spec's
/// status or the human's answer, it reaches a clean note (v2.8 review N16 + N17(c) + spec_can_transition).
/// Wired as one branch of a finish selector; `order` is its position there.
///
/// `context` is an OPTIONAL `(query, write_key)` read INSIDE the confirm branch, right before the
/// question, so `confirm_prompt` can interpolate `{{write_key}}`: a consent is only informed if the
/// question states what the commit leaves behind, and the fact must be read at the moment of asking —
/// not at seeding time, and not after the transition has already fired.
#[allow(clippy::too_many_arguments)]
fn guarded_commit(
    tc: &mut SqliteConnection,
    wf: &str,
    parent: &str,
    order: i32,
    target: &str,
    can_key: &str,
    context: Option<(&str, &str)>,
    confirm_prompt: &str,
    ok_key: &str,
    declined_note: &str,
    cannot_note: &str,
) -> QueryResult<String> {
    let seq = node(tc, wf, Some(parent), order, SEQUENCE, target, json!({}))?;
    // Check legality (from the current status + the quality gate for `ready`) BEFORE proposing.
    node(
        tc,
        wf,
        Some(&seq),
        0,
        QUERY,
        "can transition?",
        json!({ "query": "spec_can_transition", "target": target, "write": can_key }),
    )?;
    let can = node(
        tc,
        wf,
        Some(&seq),
        1,
        SELECTOR,
        "can?",
        json!({ "branches": [
            { "cond": { "var": can_key, "eq": true }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    // CAN → (read what the commit leaves behind), confirm, then route the answer.
    let confirm = node(tc, wf, Some(&can), 0, SEQUENCE, "confirm", json!({}))?;
    let mut order_in_confirm = 0;
    if let Some((query, write)) = context {
        node(
            tc,
            wf,
            Some(&confirm),
            order_in_confirm,
            QUERY,
            "what it leaves",
            json!({ "query": query, "write": write }),
        )?;
        order_in_confirm += 1;
    }
    node(
        tc,
        wf,
        Some(&confirm),
        order_in_confirm,
        ASK_HUMAN,
        "confirm",
        json!({ "prompt": confirm_prompt, "output_key": ok_key, "options": ["yes", "no"] }),
    )?;
    let route = node(
        tc,
        wf,
        Some(&confirm),
        order_in_confirm + 1,
        SELECTOR,
        "route",
        json!({ "branches": [
            { "cond": { "var": ok_key, "eq": "yes" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&route),
        0,
        TRANSITION,
        target,
        json!({ "target_state": target, "gate_on": ok_key }),
    )?;
    node(tc, wf, Some(&route), 1, NOTE, "declined", json!({ "text": declined_note }))?;
    // CANNOT (illegal from the current status) → a clean note, never a refusing transition.
    node(tc, wf, Some(&can), 1, NOTE, "cannot", json!({ "text": cannot_note }))?;
    Ok(seq)
}

/// Build the ndev tree (v2.8 review N12 + N16 + N17 + N30 + N25; lot 2 sub-workflow doctrine): query the
/// Spec status, then drive whatever part of the lifecycle applies. A `ready` Spec is STARTED; the build
/// runs ONLY when there is OPEN WORK (`spec_open_tasks` — the NAMED-FACT rule: the gate tests the work it
/// names, never the `spec_can_transition` proxy), so a Spec whose tasks are all drained SKIPS to the
/// review — its ENTRY DOOR (N17(c)) — while a STEPPED, already-in_review Spec still builds its remaining
/// phases. The build is TWO NESTED loops: the inner "build" loop reworks while the audit still leaves
/// findings (quality), the outer "phases" loop re-runs the whole build while the Spec still has
/// open tasks (completion, `spec_open_tasks` — N30). Each build pass DELEGATES to the coder SUB-WORKFLOW
/// (lot 2): the `coder` child owns the per-phase task loop + per-task gate + the coder spawns, and ndev
/// audits its verified `sub_result`. That AUDIT grills the Spec's OWN commitments — one `auditor` pass
/// per section (`user_stories` / `testing_decisions` / `implementation_decisions` / `out_of_scope`,
/// each named by the spawn's `subject`) — and DERIVES its verdict from the findings those passes filed
/// (`spec_open_findings` → `audit_verdict`), never from an agent's claim. Open findings (or a blocked
/// coder child) offer the human an
/// ABANDON that exits the build cleanly (N25) instead of burning children to max_iterations; an
/// abandoned build is routed to a clean note, NEVER to in_review. Every terminal commit is GUARDED by
/// `spec_can_transition` and routed by a selector, so nothing ever freezes the run whatever the status or
/// the human's answer. ndev's only remaining effector spawn is the `auditor` (the coder moved into the
/// child). Returns the root id.
fn build_ndev(tc: &mut SqliteConnection, wf: &str) -> QueryResult<String> {
    let root = node(tc, wf, None, 0, SEQUENCE, "/ndev", json!({}))?;

    // 0 — read the REAL status.
    node(
        tc,
        wf,
        Some(&root),
        0,
        QUERY,
        "spec status",
        json!({ "query": "spec_status", "write": "spec_status" }),
    )?;

    // 1 — START a `ready` Spec (the ENTRY transition; launching /ndev is the consent — no ask_human).
    let start = node(
        tc,
        wf,
        Some(&root),
        1,
        SELECTOR,
        "start if ready",
        json!({ "branches": [
            { "cond": { "var": "spec_status", "eq": "ready" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&start),
        0,
        TRANSITION,
        "in_progress",
        json!({ "target_state": "in_progress" }),
    )?;
    node(
        tc,
        wf,
        Some(&start),
        1,
        NOTE,
        "not started",
        json!({ "text": "The Spec is not 'ready' — skipping the start (resuming an in-progress / in-review Spec)." }),
    )?;

    // 2 — IS THERE WORK TO BUILD? The NAMED-FACT rule (module doc): the build gate tests the WORK it
    //     names — `spec_open_tasks`, the Spec's open-task count — never the `spec_can_transition`
    //     PROXY it used to read. That proxy ("may I move to in_review?") coincides with "is there work
    //     left?" only while the Spec is `in_progress`, so a STEPPED Spec (per-phase review: some phases
    //     done, the rest `todo`, the Spec already `in_review`) was locked out of its own remaining
    //     phases — `review` is illegal from `in_review` and `done` is refused while tasks remain (N32),
    //     leaving the run with no route at all.
    //     The key is DISTINCT from the phases loop's `open_tasks`: that one is the RECOUNT written at
    //     the end of each pass, and its UNSET-ness is what exits the loop on an abandon (N25).
    node(
        tc,
        wf,
        Some(&root),
        2,
        QUERY,
        "open tasks?",
        json!({ "query": "spec_open_tasks", "write": "open_tasks_at_entry" }),
    )?;

    // 3 — BUILD only when there IS open work. TWO NESTED loops (N30): the OUTER "phases" loop re-runs
    //     the whole build while the Spec still has open tasks (`spec_open_tasks`); the INNER "build"
    //     loop reworks while the audit still leaves findings (N12c). Open findings ask the human to
    //     rework OR ABANDON (N25) — an abandon short-circuits the build, skips the recount (leaving
    //     `open_tasks` unset so the phases loop exits), and routes to a clean note instead of the
    //     in_review transition.
    let build_gate = node(
        tc,
        wf,
        Some(&root),
        3,
        SELECTOR,
        "build?",
        json!({ "branches": [
            { "cond": { "var": "open_tasks_at_entry", "truthy": true }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;

    // OUTER loop — COMPLETION: repeat the build while the Spec has open tasks. `open_tasks` is written
    // by the recount query at the END of each pass; the loop exits when it is FALSY (0, or UNSET — the
    // abandon path skips the recount, so the loop exits cleanly on an abandon too — N25).
    // It keeps the engine's default bound on purpose: what it counts is the Spec's PHASES, the
    // structural quantity 25 was sized for — the rework rounds are the inner loop's business.
    let phases = node(
        tc,
        wf,
        Some(&build_gate),
        0,
        LOOP,
        "phases",
        json!({ "until": { "var": "open_tasks", "truthy": false } }),
    )?;

    // Short-circuit the coder on an abandon (a re-entered phases pass after an abandon must not spawn).
    let build_sel = node(
        tc,
        wf,
        Some(&phases),
        0,
        SELECTOR,
        "build this pass",
        json!({ "branches": [
            { "cond": { "var": "audit_verdict", "eq": "abandon" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&build_sel),
        0,
        NOTE,
        "abandoned",
        json!({ "text": "Build abandoned — not spawning the coder again." }),
    )?;

    // INNER loop — QUALITY: rework while the human answers 'revise' to the audit's findings. Exits on a
    // CLEAN audit (the derived finding count `0`) as well as on 'abandon' — both are `!= "revise"`. A
    // gate-red RETRY also re-loops here — it writes audit_verdict='revise' (M5).
    //
    // BOUNDED EXPLICITLY ([`BUILD_REWORK_ROUNDS`]), because this loop's body FILES ITS OWN NEXT
    // ITERATION: the four audit passes turn what they find into review TASKS, and a Spec with open
    // tasks is a Spec to re-build. A convergent audit (4 → 2 → 2 → 0 on the run this number was
    // measured against) is the nominal case, not a guaranteed one — so the round after which nyx
    // stops paying for a build that is not converging is a decision taken here, not the engine's
    // never-chosen backstop.
    let build = node(
        tc,
        wf,
        Some(&build_sel),
        1,
        LOOP,
        "build",
        json!({
            "until": { "var": "audit_verdict", "ne": "revise" },
            "max_iterations": BUILD_REWORK_ROUNDS,
        }),
    )?;
    // DELEGATE this build pass to the coder SUB-WORKFLOW (v2.8 lot 2): ONE child per pass owns the
    // per-phase TASK loop + the per-task gate + the coder spawns (`phase_next_task` → start → make it
    // green → done). ndev keeps only the audit / review / Spec-transitions. A FRESH child is opened each
    // inner-loop pass — a 'revise' reworks on the LIVE task list (incl. the review findings the auditor
    // filed). Its verified result folds under `sub_result` (its status is what the selector reads).
    node(
        tc,
        wf,
        Some(&build),
        0,
        SUB_WORKFLOW,
        "coder",
        json!({
            "scope": "coder",
            "inject": [],
            "retrieve": [{ "from": "child_status", "to": "child_status" }],
            "output_key": "sub_result",
        }),
    )?;

    // BRANCH ON THE CHILD RESULT (`sub_result.status`, folded when the child finishes). done → run the
    // AUDIT; else (blocked/failed) → a HUMAN revise/abandon decision, converging on the SAME
    // `audit_verdict` loop key so the N25 abandon plumbing (skip in_review, clear the gate block) is
    // reached identically. The default (always) branch is the ask, never a destructive transition (N12d).
    let coder_sel = node(
        tc,
        wf,
        Some(&build),
        1,
        SELECTOR,
        "coder result?",
        json!({ "branches": [
            { "cond": { "var": "sub_result.status", "eq": "done" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    // done → AUDIT this pass. FOUR passes, ONE per COMMITMENT the Spec itself states (the fields of
    // `db::SpecSections` a build answers to), then the verdict DERIVED from what they filed.
    //
    // WHY four spawns and not one: an auditor asked to check six things at once checks all six badly.
    // Each pass carries the SAME `auditor` role with a different per-node `subject`, so it arrives with
    // ONE bounded question and its full attention on it. And the questions are not invented: the Spec
    // already declares what the build owes — its user stories, its testing decisions, its
    // implementation decisions, and what it put OUT of scope. "Was `out_of_scope` respected?" is
    // checkable against a named source; "is the code up to standard?" is not.
    let audit = node(tc, wf, Some(&coder_sel), 0, SEQUENCE, "audit", json!({}))?;
    for (order, (label, subject)) in [
        ("audit user stories", "user_stories"),
        ("audit testing decisions", "testing_decisions"),
        ("audit implementation", "implementation_decisions"),
        ("audit scope", "out_of_scope"),
    ]
    .into_iter()
    .enumerate()
    {
        node(
            tc,
            wf,
            Some(&audit),
            order as i32,
            SPAWN,
            label,
            json!({ "agent_id": "auditor", "subject": subject }),
        )?;
    }

    // THE VERDICT IS DERIVED, NEVER CLAIMED (the NAMED-FACT rule applied to the verdict itself). The
    // audit used to end on a `judge` that RECORDED whatever the agent said about its own work. It now
    // ends on a FACT: `spec_open_findings` — the Spec's open review findings, i.e. exactly the tasks the
    // four passes just filed (a finding IS a `kind=review` task). That count IS `audit_verdict`.
    //
    // Writing the fact into the loop's own key is what makes the verdict UNFALSIFIABLE *and* fresh: a
    // claim can go stale (a 'revise' answered in a previous pass would otherwise still be sitting on the
    // board and re-loop a clean build forever), a value re-derived from state each pass cannot. `0` is
    // `!= "revise"`, so a clean audit exits the inner build loop by construction; a non-zero count is
    // TRUTHY, so it routes to the human below, who alone turns it into 'revise'/'abandon'.
    node(
        tc,
        wf,
        Some(&audit),
        4,
        QUERY,
        "open findings",
        json!({ "query": "spec_open_findings", "write": "audit_verdict" }),
    )?;
    // FINDINGS? → the HUMAN decision point (N25): rework (another coder pass, which closes the findings
    // as tasks) OR abandon this build (e.g. an unworkable environment). Their answer overwrites
    // `audit_verdict` and drives the inner loop — 'revise' re-loops (a FRESH coder child), 'abandon'
    // exits. NO findings → the default branch, a clean note, and the derived `0` exits the loop.
    let findings_sel = node(
        tc,
        wf,
        Some(&audit),
        5,
        SELECTOR,
        "findings?",
        json!({ "branches": [
            { "cond": { "var": "audit_verdict", "truthy": true }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&findings_sel),
        0,
        ASK_HUMAN,
        "rework or abandon",
        json!({
            "prompt": "The audit left {{audit_verdict}} open finding(s) of high/medium severity against the Spec's own commitments (a low one is recorded on the Spec and left for the next cycle — it never re-opens a build). Answer 'revise' to keep reworking (another coder pass — each finding is a task the coder closes), or 'abandon' to stop this build — pick abandon when the environment is unworkable (missing repo / dependency / access) and reworking cannot help.",
            "output_key": "audit_verdict",
            "options": ["revise", "abandon"],
        }),
    )?;
    node(
        tc,
        wf,
        Some(&findings_sel),
        1,
        NOTE,
        "audit clean",
        // It says "nothing that re-opens a build", NOT "nothing at all": the derived verdict EXCLUDES
        // the deferred low findings, so an audit that filed four lows reaches this very note. Claiming
        // it filed nothing would be the graph pretending the Spec is clean.
        json!({ "text": "The audit filed no high/medium finding against the Spec's commitments — this build pass is done. Any low finding it filed stays open on the Spec, with its severity, for the next cycle." }),
    )?;
    // The coder child did NOT complete (blocked/failed — e.g. a red blocking gate the child parked on)
    // → the human decides: 'revise' (retry — a FRESH coder child next pass) or 'abandon' (stop the
    // build). Writes the SAME `audit_verdict` loop key: 'revise' re-loops (fresh child); 'abandon' exits
    // to the clean abandon end that clears the block. The default (always) branch is this ask (N12d).
    node(
        tc,
        wf,
        Some(&coder_sel),
        1,
        ASK_HUMAN,
        "coder blocked",
        json!({
            "prompt": "The coder sub-workflow did not complete (a blocking check failed, or the environment is unworkable). Answer 'revise' to retry with a fresh coder pass, or 'abandon' to stop this build — pick abandon when reworking cannot help (missing repo / dependency / access).",
            "output_key": "audit_verdict",
            "options": ["revise", "abandon"],
        }),
    )?;

    // END of each phases pass — RECOUNT the Spec's open tasks (skipped on abandon so `open_tasks` stays
    // unset and the phases loop exits). `spec_open_tasks` GROWS as the auditor files review-finding
    // tasks, so the loop keeps going until every task (build AND review finding) is resolved (N30).
    let recount_sel = node(
        tc,
        wf,
        Some(&phases),
        1,
        SELECTOR,
        "recount?",
        json!({ "branches": [
            { "cond": { "var": "audit_verdict", "eq": "abandon" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&recount_sel),
        0,
        NOTE,
        "abandoned recount",
        json!({ "text": "Build abandoned — the phases loop exits." }),
    )?;
    node(
        tc,
        wf,
        Some(&recount_sel),
        1,
        QUERY,
        "open tasks",
        json!({ "query": "spec_open_tasks", "write": "open_tasks" }),
    )?;

    // The SAFE default (N12d) of the build gate: nothing open to build — skip straight to the review.
    node(
        tc,
        wf,
        Some(&build_gate),
        1,
        NOTE,
        "nothing to build",
        json!({ "text": "No open task left to build — skipping the build and resuming this Spec's review." }),
    )?;

    // 4 — MAY the Spec enter review? A CORRECT use of `spec_can_transition` (the NAMED-FACT rule): it
    //     gates a TRANSITION and only that. A Spec ALREADY in_review (the stepped case, which has just
    //     built its remaining phases) cannot take `in_review → in_review` — the graph CHECKS before it
    //     proposes rather than firing a transition that would refuse.
    node(
        tc,
        wf,
        Some(&root),
        4,
        QUERY,
        "can review?",
        json!({ "query": "spec_can_transition", "target": "in_review", "write": "can_review" }),
    )?;

    // 5 — SEND the build to review, the N16 shape (`guarded_commit`'s check-then-route, minus its human
    //     confirmation — `in_review` is the routine, non-engaging transition). It sits AFTER the build
    //     gate, so a Spec whose work was already drained still reaches its review. An ABANDONED build
    //     must NEVER enter in_review (N25), and an illegal transition lands on a clean note (N12d
    //     default), never a refusal that freezes the run.
    let to_review = node(
        tc,
        wf,
        Some(&root),
        5,
        SELECTOR,
        "to review?",
        json!({ "branches": [
            { "cond": { "var": "audit_verdict", "eq": "abandon" }, "child": 0 },
            { "cond": { "var": "can_review", "eq": true }, "child": 1 },
            { "cond": { "always": true }, "child": 2 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&to_review),
        0,
        NOTE,
        "build abandoned",
        // M5 — CLEAR the gate block here: an abandon reached with the checks still RED left a blocking
        // gate anchored; this explicit abandon note lifts it so the clean `done` below no longer refuses
        // on the stale block. Harmless when the gate was green (nothing was anchored).
        json!({
            "text": "Build abandoned — the checks were not satisfied; the Spec is left in progress (not sent to review). Re-run /ndev to resume, or /nspec to revise it.",
            "clear_gate_block": true,
        }),
    )?;
    node(
        tc,
        wf,
        Some(&to_review),
        1,
        TRANSITION,
        "in_review",
        json!({ "target_state": "in_review" }),
    )?;
    node(
        tc,
        wf,
        Some(&to_review),
        2,
        NOTE,
        "cannot enter review",
        json!({ "text": "The Spec cannot enter review from its current status (it is already in review, or was never started) — leaving it as is and resuming the review." }),
    )?;

    // 6 — REVIEW the build UNLESS it was abandoned. An abandoned build has nothing to review (it never
    //     reached in_review), so it routes to a clean note; otherwise the human reviews it (N25).
    let review_gate = node(
        tc,
        wf,
        Some(&root),
        6,
        SELECTOR,
        "review?",
        json!({ "branches": [
            { "cond": { "var": "audit_verdict", "eq": "abandon" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&review_gate),
        0,
        NOTE,
        "abandoned",
        json!({ "text": "Build abandoned — nothing to review. Re-run /ndev to resume, or /nspec to revise the Spec." }),
    )?;
    let review = node(tc, wf, Some(&review_gate), 1, SEQUENCE, "review", json!({}))?;

    // The review PARK: a single ask_human (spans sessions — B1) with a CLOSED answer set (N19).
    node(
        tc,
        wf,
        Some(&review),
        0,
        ASK_HUMAN,
        "review",
        json!({
            "prompt": "The build is in review. Answer 'approve' to complete it, 'cancel' to abandon it, or 'hold' to leave it in review for now.",
            "output_key": "review",
            "options": ["approve", "cancel", "hold"],
        }),
    )?;

    // FINISH: route the review answer. Each commit is spec_can_transition-GUARDED and selector-routed
    // (N16 + N17c): a declined confirmation, OR an illegal status, lands on a clean note — never a
    // refusing transition that freezes the run. 'hold'/unrecognised → the safe default.
    let finish = node(
        tc,
        wf,
        Some(&review),
        1,
        SELECTOR,
        "finish",
        json!({ "branches": [
            { "cond": { "var": "review", "eq": "cancel" }, "child": 0 },
            { "cond": { "var": "review", "eq": "approve" }, "child": 1 },
            { "cond": { "always": true }, "child": 2 },
        ] }),
    )?;
    guarded_commit(
        tc,
        wf,
        &finish,
        0,
        "cancel",
        "can_cancel",
        None,
        "Abandon this build and cancel the Spec?",
        "cancel_ok",
        "Cancel aborted — left in review.",
        "The Spec cannot be cancelled from its current status — left as is.",
    )?;
    // The COMPLETION says what it ships with, IN THE QUESTION. `deferred_findings_note` already
    // states it — but AFTER the transition, so the human was asked "Mark the Spec completed?" while
    // six findings stayed open and only learned it once the Spec was completed. The count is read
    // here, one node before the question, and interpolated into it: the same gesture the "rework or
    // abandon" ask already makes with `{{audit_verdict}}`. Consent to close a Spec is only consent if
    // it names what closing leaves behind.
    guarded_commit(
        tc,
        wf,
        &finish,
        1,
        "done",
        "can_done",
        Some(("spec_deferred_findings", "deferred_findings")),
        "Mark the Spec completed? It would ship with {{deferred_findings}} open low-severity finding(s): they were NOT worked in this cycle (only a high/medium finding re-opens a build) and stay on the Spec as open review tasks, findable on the next cycle.",
        "done_ok",
        "Not completed — left in review.",
        "The Spec cannot be completed from its current status — left as is.",
    )?;
    node(
        tc,
        wf,
        Some(&finish),
        2,
        NOTE,
        "review open",
        json!({ "text": "Review left open — re-run /ndev to finalize (approve or cancel)." }),
    )?;

    Ok(root)
}

/// Build the nspec tree (v2.8 review N12 + N13 + N16 + the target design): query → a create / resume-
/// draft / revise-a-ready ENTRY GATE → an engine-driven GRILL loop (SKIPPABLE when a complete draft is
/// finalized straight away) → a query-guarded `ready` COMMIT whose refusal has a CLEAN EXIT. Returns
/// the root id. The author runs INLINE (it grills the human — N2); nyx's judge ends the grill (not the
/// effector's prompt — the answers accumulate via `append` and the judge reads them via `{{answers}}`
/// interpolation, the N9 primitives); every gating `ask_human` declares a CLOSED answer set (`options`
/// — N19) so a selector never falls through on an off-vocabulary reply; `spec_can_transition` lets the
/// graph CHECK before it proposes to finalize (a not-ready draft routes back to the grill instead of
/// hitting a refusing transition); and every engaging transition / fork is ROUTED by a selector whose
/// refusal branch is a clean note, so a "no" never freezes the run (N16). No default branch is
/// destructive (N12(d)). The spawn's `agent_id` is the effector ROLE (`spec` = [`AgentRole::Spec`]).
///
/// [`AgentRole::Spec`]: crate::mcp_context::AgentRole
fn build_nspec(tc: &mut SqliteConnection, wf: &str) -> QueryResult<String> {
    let root = node(tc, wf, None, 0, SEQUENCE, "/nspec", json!({}))?;

    // 0 — read the REAL Spec status.
    node(
        tc,
        wf,
        Some(&root),
        0,
        QUERY,
        "spec status",
        json!({ "query": "spec_status", "write": "spec_status" }),
    )?;

    // 1 — ENTRY GATE: revise-a-ready (fork) / resume-a-draft / create. nyx CHECKS the case (N12).
    let entry = node(
        tc,
        wf,
        Some(&root),
        1,
        SELECTOR,
        "entry gate",
        json!({ "branches": [
            { "cond": { "var": "spec_status", "eq": "ready" }, "child": 0 },
            { "cond": { "var": "spec_status", "eq": "draft" }, "child": 1 },
            { "cond": { "always": true }, "child": 2 },
        ] }),
    )?;
    // child 0 — a READY Spec: reworking it is a FORK — an EXPLICIT ask + fork_spec (N13(b)).
    let revise = node(tc, wf, Some(&entry), 0, SEQUENCE, "revise a ready spec", json!({}))?;
    node(
        tc,
        wf,
        Some(&revise),
        0,
        ASK_HUMAN,
        "confirm revision",
        json!({
            "prompt": "This Spec is already READY. Create a new revision (a fresh editable draft) to rework it?",
            "output_key": "revise_ok",
            "options": ["yes", "no"],
        }),
    )?;
    node(tc, wf, Some(&revise), 1, FORK_SPEC, "fork", json!({ "gate_on": "revise_ok" }))?;
    // child 1 — an existing DRAFT: is it already READY-able? If so OFFER to finalize without re-
    // grilling; else route back to the grill. nyx CHECKS (spec_can_transition) before proposing (N16).
    let resume = node(tc, wf, Some(&entry), 1, SEQUENCE, "resume a draft", json!({}))?;
    node(
        tc,
        wf,
        Some(&resume),
        0,
        QUERY,
        "can finalize?",
        json!({ "query": "spec_can_transition", "target": "ready", "write": "can_ready" }),
    )?;
    let choose = node(
        tc,
        wf,
        Some(&resume),
        1,
        SELECTOR,
        "finalize or grill",
        json!({ "branches": [
            { "cond": { "var": "can_ready", "eq": true }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&choose),
        0,
        ASK_HUMAN,
        "continue or finalize",
        json!({
            "prompt": "This draft is ready to finalize. Keep grilling to refine it, or finalize it now?",
            "output_key": "entry_choice",
            "options": ["grill", "ready"],
        }),
    )?;
    node(
        tc,
        wf,
        Some(&choose),
        1,
        NOTE,
        "draft incomplete",
        json!({ "text": "The draft is not ready yet — resuming the grill." }),
    )?;
    // child 2 — no Spec yet: create one (the SAFE default).
    node(
        tc,
        wf,
        Some(&entry),
        2,
        NOTE,
        "create new",
        json!({ "text": "No Spec yet — author a NEW one from scratch." }),
    )?;

    // 2 — GRILL? Skip it ONLY when the human chose to finalize a ready draft; otherwise run it. (An
    //     arbre pur cannot step over a sibling, so the skip is a selector wrapping the loop.)
    let grill_gate = node(
        tc,
        wf,
        Some(&root),
        2,
        SELECTOR,
        "grill?",
        json!({ "branches": [
            { "cond": { "var": "entry_choice", "eq": "ready" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&grill_gate),
        0,
        NOTE,
        "grill skipped",
        json!({ "text": "Grill skipped at your request — going straight to finalize." }),
    )?;
    // child 1 — the GRILL loop: nyx's judge ends it (append answers + {{answers}} interpolation — N9).
    let grill = node(
        tc,
        wf,
        Some(&grill_gate),
        1,
        LOOP,
        "grill",
        json!({ "until": { "var": "grill_status", "eq": "complete" } }),
    )?;
    node(
        tc,
        wf,
        Some(&grill),
        0,
        SPAWN,
        "author spec",
        json!({ "agent_id": "spec", "inline": true }),
    )?;
    node(
        tc,
        wf,
        Some(&grill),
        1,
        ASK_HUMAN,
        "steer",
        json!({
            "prompt": "Answer the author's question (or add a correction / a new requirement).",
            "output_key": "answers",
            "append": true,
        }),
    )?;
    node(
        tc,
        wf,
        Some(&grill),
        2,
        JUDGE,
        "spec complete?",
        json!({
            "prompt": "Given the answers gathered so far: {{answers}}. Is the Spec's Problem and Solution complete and unambiguous? Answer 'complete' to finish the grill, or 'more' to keep asking.",
            "output_key": "grill_status",
        }),
    )?;

    // 3 — PROTO? (lot 2) — for a Spec with a graphical/UI surface, OFFER to settle the design with a
    //     prototype BEFORE the ready commit. nyx JUDGES whether there is a graphic surface, then (only
    //     for a 'ui' verdict) asks the human; a 'yes' DELEGATES the design rounds to the prototyper
    //     SUB-WORKFLOW (which loops proto → election → lock and freezes the design). Every branch's
    //     default is a clean note — a Spec with no graphic surface, or a declined proto, simply skips it.
    let proto = node(tc, wf, Some(&root), 3, SEQUENCE, "proto?", json!({}))?;
    node(
        tc,
        wf,
        Some(&proto),
        0,
        JUDGE,
        "graphic render?",
        json!({
            "prompt": "Does this Spec deliver a graphical/UI surface worth prototyping? Answer 'ui' if it does, or 'none' if it has no graphic surface.",
            "output_key": "needs_proto",
        }),
    )?;
    let proto_path = node(
        tc,
        wf,
        Some(&proto),
        1,
        SELECTOR,
        "proto path",
        json!({ "branches": [
            { "cond": { "var": "needs_proto", "eq": "ui" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    // 'ui' → ASK the human whether to prototype; a 'yes' runs the prototyper sub-workflow.
    let want_proto = node(tc, wf, Some(&proto_path), 0, SEQUENCE, "want a proto?", json!({}))?;
    node(
        tc,
        wf,
        Some(&want_proto),
        0,
        ASK_HUMAN,
        "go through a proto?",
        json!({
            "prompt": "This Spec has a UI surface. Prototype it (settle the design) before marking it ready?",
            "output_key": "do_proto",
            "options": ["yes", "no"],
        }),
    )?;
    let run_proto = node(
        tc,
        wf,
        Some(&want_proto),
        1,
        SELECTOR,
        "run proto?",
        json!({ "branches": [
            { "cond": { "var": "do_proto", "eq": "yes" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    // 'yes' → the design rounds, then BRANCH ON THE CHILD RESULT. A `sub_workflow` that ends
    // `blocked`/`failed` writes its status to the board and NOTHING ELSE happens: observed live, a
    // prototyper child died `failed` and the parent's very next tick answered `{"action": "done"}` —
    // nspec walked on to the ready commit as if the design had been settled. The prototype is
    // precisely what conditions that commit, so its failure must be TESTED, exactly as ndev tests its
    // coder child ("coder result?"): `done` continues, anything else asks the human.
    let proto_seq = node(tc, wf, Some(&run_proto), 0, SEQUENCE, "prototype", json!({}))?;
    node(
        tc,
        wf,
        Some(&proto_seq),
        0,
        SUB_WORKFLOW,
        "prototyper",
        json!({
            "scope": "prototyper",
            "inject": [],
            "retrieve": [{ "from": "design_locked", "to": "design_locked" }],
            "output_key": "proto_result",
        }),
    )?;
    let proto_result = node(
        tc,
        wf,
        Some(&proto_seq),
        1,
        SELECTOR,
        "proto result?",
        json!({ "branches": [
            { "cond": { "var": "proto_result.status", "eq": "done" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&proto_result),
        0,
        NOTE,
        "design settled",
        json!({ "text": "The prototype completed — the design is settled." }),
    )?;
    // The child did NOT complete: the human decides whether this Spec may go ready WITHOUT a settled
    // design. The default (always) branch is the ask, never a silent continuation (N12d), and its
    // 'hold' answer is read by the commit selector below — the ONE place that can still stop the walk.
    node(
        tc,
        wf,
        Some(&proto_result),
        1,
        ASK_HUMAN,
        "proto did not complete",
        json!({
            "prompt": "The prototype did not complete (the design rounds were blocked, or the child failed), so the design was never frozen. Answer 'hold' to leave the Spec as a draft and retry the prototype with another /nspec, or 'ready' to finalize it anyway — without a settled design.",
            "output_key": "proto_verdict",
            "options": ["hold", "ready"],
        }),
    )?;
    node(
        tc,
        wf,
        Some(&run_proto),
        1,
        NOTE,
        "skipped proto",
        json!({ "text": "Skipped the proto — marking the Spec ready without a prototype." }),
    )?;
    // else (no graphic surface) → nothing to prototype.
    node(
        tc,
        wf,
        Some(&proto_path),
        1,
        NOTE,
        "no graphic surface",
        json!({ "text": "No graphic surface — no prototype needed." }),
    )?;

    // 4 — re-read the LIVE status (a fork reopened it to draft; the grill may have completed it).
    node(
        tc,
        wf,
        Some(&root),
        4,
        QUERY,
        "final status",
        json!({ "query": "spec_status", "write": "final_status" }),
    )?;

    // 5 — COMMIT: only a DRAFT can go ready. Ask, then a selector ROUTES the answer — 'yes' →
    //     transition, else → a clean "left as draft" note. THE N16 FIX: the refusal has a proper exit,
    //     so a "no" never freezes the run on a refusing transition. The default (not a draft) is a safe
    //     note too (N12(d)).
    //     The FIRST branch is the FAILED PROTOTYPE the human chose to hold on: the commit is where a
    //     dead design round has to stop the flow, since 'ready' is exactly the step the prototype
    //     conditions.
    let commit = node(
        tc,
        wf,
        Some(&root),
        5,
        SELECTOR,
        "commit",
        json!({ "branches": [
            { "cond": { "var": "proto_verdict", "eq": "hold" }, "child": 0 },
            { "cond": { "var": "final_status", "eq": "draft" }, "child": 1 },
            { "cond": { "always": true }, "child": 2 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&commit),
        0,
        NOTE,
        "proto held",
        json!({ "text": "The prototype did not complete — the Spec is left as a draft (not marked ready). Re-run /nspec to retry the prototype." }),
    )?;
    let confirm = node(tc, wf, Some(&commit), 1, SEQUENCE, "confirm ready", json!({}))?;
    // ┌───────────────────────────────────────────────────────────────────────────────────────────┐
    // │ HORS PERIMETRE ASSUME — the third answer, and the node type it needs.                     │
    // │                                                                                           │
    // │ This work is OUTSIDE the declared scope of PRD 01KYA475AX5JBGSS8WK5J8NM1S. The user asked │
    // │ for it ANYWAY, on 2026-08-18, in full knowledge and after an explicit warning from the    │
    // │ orchestrator; a PRD in review being frozen, a human review finding was the only route     │
    // │ that did not fork it. It is written here, at the place that was changed, so the RECORDED  │
    // │ scope and the SHIPPED code stay reconcilable afterwards — and that reconciliation is done │
    // │ ITEM BY ITEM: all THREE crossed items are named, not only the one this branch is about.   │
    // │                                                                                           │
    // │   * 01M02VRMPMRK1P8536ZYBAS4DS — "Consentement final et choix du workflow nspec" ("Les    │
    // │     options non/oui et un eventuel oui avec workspace appartiennent au workflow           │
    // │     d'authoring. Ce PRD ne modifie ni nspec ni les transitions draft vers ready ou ready  │
    // │     vers in_progress"). CROSSED BY this very branch: the third answer, the selectors that │
    // │     route it, and the `ready` transition it takes.                                        │
    // │                                                                                           │
    // │   * 01KYA49F5T2H02YY4VT7SCBGSX — "Internes du moteur d'orchestration 2.8 […] 2.8 est      │
    // │     in_progress/fige ; ce PRD fournit seulement le resolveur partage". CROSSED BY the     │
    // │     SIXTEENTH node type, `create_workspace` (traced on `db::NODE_TYPE_CREATE_WORKSPACE`): │
    // │     its engine vocabulary (`NodeType`, `CreateWorkspaceConfig`, its `Action` arm) and     │
    // │     inline runtime effect, the new `query` fact `project_worktrees_root`, and the        │
    // │     widening of the `nodes.type` CHECK. It ADDS a node type; it changes NOTHING about how │
    // │     an existing one is READ — the B3 consent gate and the authoring rule that demands it  │
    // │     are exactly HEAD's, which is what the shape below is built around.                    │
    // │                                                                                           │
    // │   * 01M02VRGBAY6HV0A97A938DQ33 — "Provisioning physique du workspace […] le choix du      │
    // │     root_path […] necessite un verbe configurable par projet dans un PRD separe". CROSSED │
    // │     BY `projects.worktrees_root` (traced on `db::Project::worktrees_root`): it IS that    │
    // │     per-project verb, and the run now DERIVES the path from it instead of receiving one.  │
    // │     It is BOUNDED to the WHERE — the WHAT (clone, worktree, root files, setup commands)   │
    // │     stays out, with idea 01M04W3RZ333NC01R216MNXAFK.                                      │
    // └───────────────────────────────────────────────────────────────────────────────────────────┘
    node(
        tc,
        wf,
        Some(&confirm),
        0,
        ASK_HUMAN,
        "confirm ready",
        json!({
            "prompt": "Mark the Spec ready for build? Answer 'yes' to mark it ready, 'go' to mark it ready AND open the dev workspace anchored to it (the same yes, plus a folder to build in), or 'no' to leave it a draft.",
            "output_key": "ready_ok",
            // THREE answers, and TWO of them are a yes. Both are words the B3 gate accepted at HEAD
            // ([`crate::mcp_engine`]'s `board_value_is_affirmative`: yes/y/ok/approve(d)/ready/go/
            // true, compared WHOLE), and that is the whole trick — the rider is not carried by TEXT
            // ADDED TO an answer, it is carried by WHICH affirmative was given. An answer like "yes
            // with workspace" would have forced the gate to read less than the whole reply, and a
            // gate that reads less than the whole reply reads "ok, actually no" as an approval. The
            // set stays CLOSED (N19), so nothing outside these three reaches the selectors below.
            "options": ["yes", "go", "no"],
        }),
    )?;
    // ROUTE THE COMMITMENT — ONE selector, three answers, three children. 'yes' and 'go' are the SAME
    // yes, but they no longer select the same child: 'go' asked for a Spec that is ready AND a
    // workspace, and that pair has to be decided TOGETHER (see the sequence below), so it gets a
    // branch of its own. Each `transition` under here is a DIRECT child of a selector whose branch
    // COMPARES the answer the ask_human just above wrote, which is exactly what the shape rule
    // B3/N13/N16 asks for ([`crate::mcp_authoring::sensitive_node_gate`]: a sensitive node needs an
    // ask_human that NECESSARILY ran before it, and whose answer its own `gate_on` — or a selector
    // branch on the way down to it — reads). Nothing about that rule was relaxed to make this branch
    // legal: what changed for it is that the rule now CLIMBS to the ancestors, which is what lets the
    // `create_workspace` at the bottom of the 'go' sequence hang on the very same answer.
    let route_ready = node(
        tc,
        wf,
        Some(&confirm),
        1,
        SELECTOR,
        "route the commitment",
        json!({ "branches": [
            { "cond": { "var": "ready_ok", "eq": "yes" }, "child": 0 },
            { "cond": { "var": "ready_ok", "eq": "go" }, "child": 1 },
            { "cond": { "always": true }, "child": 2 },
        ] }),
    )?;
    // 'yes' — the plain commitment, nothing to prepare. The engaging ready transition, gated as at
    // HEAD: its key holds 'yes' or 'go', each an affirmative ON ITS OWN, compared WHOLE.
    node(
        tc,
        wf,
        Some(&route_ready),
        0,
        TRANSITION,
        "ready",
        json!({ "target_state": "ready", "gate_on": "ready_ok" }),
    )?;
    // 'go' — READY **AND** A WORKSPACE, DECIDED AS ONE. The old shape committed the transition first
    // and opened the workspace after, as a rider routed by a second selector on the same answer; when
    // the `create_workspace` then failed, the `ready` was already committed and the Spec sat ready
    // with NO workspace — half of what was asked for, and nobody asked for the half. So everything
    // that CAN be checked is checked BEFORE the transition: this sequence resolves WHERE the folder
    // goes, PREVALIDATES that folder, and only then commits. It is the prevalidation philosophy this
    // PRD already applies to the filesystem ("the prevalidation precedes the mkdir"), carried up to
    // the graph, where the thing to protect is the TRANSITION.
    let workspace = node(
        tc,
        wf,
        Some(&route_ready),
        1,
        SEQUENCE,
        "ready + a dev workspace",
        json!({}),
    )?;
    // else → a CLEAN exit (N16): the Spec is deliberately left as a draft.
    node(
        tc,
        wf,
        Some(&route_ready),
        2,
        NOTE,
        "left as draft",
        json!({ "text": "Left as a draft — mark it ready later (it stays editable meanwhile)." }),
    )?;
    // WHERE the folder goes is a FACT before it is a question: read the project's configured
    // worktrees root, and only ASK when there is none (the decision of 2026-08-18 — a project that
    // said once where its workspaces live is never asked again). The fact writes TWO keys:
    // `worktrees_root` (the root itself) and `worktrees_root_configured` (the branchable half) — the
    // selector below tests the SECOND so the question may write the FIRST.
    node(
        tc,
        wf,
        Some(&workspace),
        0,
        QUERY,
        "worktrees root",
        json!({ "query": "project_worktrees_root", "write": "worktrees_root" }),
    )?;
    let where_to = node(
        tc,
        wf,
        Some(&workspace),
        1,
        SELECTOR,
        "where does it go?",
        json!({ "branches": [
            { "cond": { "var": "worktrees_root_configured", "truthy": false }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    // NOT configured → ask, and ask for THE SAME THING the setting holds: a ROOT. The answer lands on
    // `worktrees_root`, the very key the configured value would have filled, so the two branches hand
    // the node below one value with one meaning — and the folder is DERIVED under it either way. The
    // question used to write a key of its own that the effect took as the FINAL path, so answering
    // this question with what it asked for registered the worktrees root ITSELF as a workspace.
    // FREE-FORM on purpose: a folder path is not a closed answer set, and no selector branches on this
    // key (the J1 rule asks for `options` only where a branch compares the answer by equality — which
    // is why the branch above reads `worktrees_root_configured` instead).
    node(
        tc,
        wf,
        Some(&where_to),
        0,
        ASK_HUMAN,
        "workspace root",
        json!({
            "prompt": "This project has no worktrees root configured. Under WHICH ROOT FOLDER should its dev workspaces be created? Give an absolute path to the PARENT folder (e.g. D:/work/_worktrees) — nyx creates a subfolder named after the Spec inside it, so answer with the folder that holds your worktrees, not the workspace folder itself. (Set it once with set_project_settings `worktrees_root` and you will not be asked again.)",
            "output_key": "worktrees_root",
        }),
    )?;
    // Configured → the SAFE default (N12(d)): nyx uses it and asks nothing.
    node(
        tc,
        wf,
        Some(&where_to),
        1,
        NOTE,
        "using the configured root",
        json!({ "text": "Creating the dev workspace under the project's configured worktrees root — no question needed." }),
    )?;
    // THE PREVALIDATION, and it is here — BEFORE the transition — on purpose. It asks the one thing
    // that can be known in advance and that BIT in production: is the folder this run is about to open
    // already a workspace of this project? A second `go` on a project whose root is set derives a
    // DIFFERENT subfolder per Spec, so this normally answers "free"; it answers "taken" for the run
    // that would otherwise commit a `ready` and then fail on the DB's UNIQUE(project_id, path).
    node(
        tc,
        wf,
        Some(&workspace),
        2,
        QUERY,
        "is the folder free?",
        json!({ "query": "workspace_target_free", "write": "workspace_free" }),
    )?;
    let free_to_commit = node(
        tc,
        wf,
        Some(&workspace),
        3,
        SELECTOR,
        "free to commit?",
        json!({ "branches": [
            { "cond": { "var": "workspace_free", "truthy": true }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    // FREE → commit. The transition sits UNDER the prevalidation, so `ready` is only taken when the
    // workspace that comes with it can be opened.
    node(
        tc,
        wf,
        Some(&free_to_commit),
        0,
        TRANSITION,
        "ready",
        json!({ "target_state": "ready", "gate_on": "ready_ok" }),
    )?;
    // TAKEN → NEITHER half happens, and the run SAYS which folder and why (the fact's own
    // `<write>_reason`, interpolated) instead of failing on it three nodes later (N12(d)).
    node(
        tc,
        wf,
        Some(&free_to_commit),
        1,
        NOTE,
        "target folder taken",
        json!({ "text": "{{workspace_free_reason}} — so NOTHING was changed: the Spec is still a draft and no workspace was opened. Set the project's `worktrees_root` to a folder that holds one subfolder per worktree (set_project_settings), then re-run /nspec." }),
    )?;
    // THE WORKSPACE ITSELF — a NODE, not prose: it calls the same `create_workspace` the MCP tool
    // does, so the folder, the workspace row and the SPEC ANCHOR are one guarded write (the `spec_id`
    // this PRD delivered), and the anchor is not re-implemented here. It reads ONE key, a ROOT, and
    // derives `<project key>-<number>` under it — the name the human already knows the Spec by.
    //
    // GATED ON THE PREVALIDATION, not on the answer: the human consent for this branch is the selector
    // that routed it (it compared the WHOLE answer to 'go'), and what this gate adds is the OTHER half
    // of the pair — the node fires exactly when the transition above did, so `go` is all-or-nothing.
    //
    // NOTHING FILLS THE FOLDER, and that is assumed: no clone, no `git worktree add`. The workspace
    // lands EMPTY. Putting something in it is the subject of the next PRD (depot idea
    // 01M04W3RZ333NC01R216MNXAFK); the decisions already taken for it — including the compensation
    // trap a filled folder springs — are recorded on `crate::mcp_runtime::create_workspace` and
    // `compensate_created_dir`. What THIS branch settles is the WHERE, so that PRD only has to plug
    // the filling in at a location nobody has to re-decide.
    node(
        tc,
        wf,
        Some(&workspace),
        4,
        CREATE_WORKSPACE,
        "dev workspace",
        json!({
            "root_key": "worktrees_root",
            "gate_on": "workspace_free",
        }),
    )?;
    node(
        tc,
        wf,
        Some(&commit),
        2,
        NOTE,
        "not a draft",
        json!({ "text": "Not a draft — nothing to commit." }),
    )?;

    Ok(root)
}

/// Build the COMPLETE ndoc tree (query-guarded doc-sync loop → finish OR abandon); returns the root
/// id. The spawn's `agent_id` is the effector ROLE that syncs docs (`doc` = [`AgentRole::Doc`]), NOT
/// the workflow name (review finding #34). ndoc does not transition the Spec — docs are orthogonal
/// to its lifecycle — so it demonstrates the `query` guard rather than an engaging transition.
///
/// [`AgentRole::Doc`]: crate::mcp_context::AgentRole
fn build_ndoc(tc: &mut SqliteConnection, wf: &str) -> QueryResult<String> {
    let root = node(tc, wf, None, 0, SEQUENCE, "/ndoc", json!({}))?;

    // Docs are synced once the Spec is COMPLETED: read the REAL status, then branch on it (B2).
    node(
        tc,
        wf,
        Some(&root),
        0,
        QUERY,
        "spec status",
        json!({ "query": "spec_status", "write": "spec_status" }),
    )?;
    let guard = node(
        tc,
        wf,
        Some(&root),
        1,
        SELECTOR,
        "sync or skip",
        json!({ "branches": [
            { "cond": { "var": "spec_status", "eq": "completed" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;

    // child 0 — the sync body.
    let sync = node(tc, wf, Some(&guard), 0, SEQUENCE, "sync", json!({}))?;
    // loop { doc-sync → ask } — BACKTRACK while the human keeps asking for changes.
    let syncing = node(
        tc,
        wf,
        Some(&sync),
        0,
        LOOP,
        "syncing",
        json!({ "until": { "var": "decision", "ne": "changes" } }),
    )?;
    node(tc, wf, Some(&syncing), 0, SPAWN, "sync docs", json!({ "agent_id": "doc" }))?;
    node(
        tc,
        wf,
        Some(&syncing),
        1,
        ASK_HUMAN,
        "review",
        json!({
            "prompt": "Review the doc-sync changes: answer 'approve' to accept, 'changes' to send them back, or 'cancel' to abandon.",
            "output_key": "decision",
            // The CLOSED answer set the loop and the fork below already assume (N19 + J1): both
            // compare `decision` by equality, so an answer outside this set is refused and re-asked
            // rather than silently missing every comparison.
            "options": ["approve", "changes", "cancel"],
        }),
    )?;
    // The sync's terminal fork (docs don't own the Spec state ⇒ both branches are plain notes). It
    // names EVERY answer that can reach it — `changes` re-loops, so only approve/cancel arrive — and
    // therefore carries NO default: an abandon is reached by the human SAYING cancel, never by
    // fall-through (N12(d), which this selector used to break: its catch-all wrote "abandoned"). Any
    // other value has no branch here, and the engine refuses rather than picking one for the human.
    let finish = node(
        tc,
        wf,
        Some(&sync),
        1,
        SELECTOR,
        "finish or abandon",
        json!({ "branches": [
            { "cond": { "var": "decision", "eq": "approve" }, "child": 0 },
            { "cond": { "var": "decision", "eq": "cancel" }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&finish),
        0,
        NOTE,
        "synced",
        json!({ "text": "Docs synced — approved." }),
    )?;
    node(
        tc,
        wf,
        Some(&finish),
        1,
        NOTE,
        "abandoned",
        json!({ "text": "Doc sync abandoned." }),
    )?;

    // child 1 — the Spec is not ready for a doc sync.
    node(
        tc,
        wf,
        Some(&guard),
        1,
        NOTE,
        "not ready",
        json!({ "text": "The Spec is not completed — nothing to sync yet." }),
    )?;

    Ok(root)
}

/// Build the COMPLETE coder tree (v2.8 lot 2 — the sub-workflow ndev delegates ONE build PHASE to).
/// It owns the per-phase TASK LOOP: pick the next open task as a NODE (`phase_next_task`), START it,
/// get the work done, CLOSE it by the route its own state allows, and repeat until the phase is
/// drained (`phase_open_tasks` falsy). The close is READ, never assumed (H9): `task_close_route` says
/// whether the task closes through `done`, through the human-verified `verify`, or through neither —
/// and a task the coder BLOCKED goes to the human's escalation gate ([`escalate_blocked_task`], H12)
/// instead of freezing the run on a refused `done`. A `kind=human` task is settled by the human BEFORE
/// any spawn: the coder is never given an action reserved for the user. The gate is nyx's own PTY
/// check. NO Spec transition — those stay in the PARENT (ndev), so this graph is authoring-VALID
/// without a sensitive-node gate. Returns the root id.
fn build_coder(tc: &mut SqliteConnection, wf: &str) -> QueryResult<String> {
    let root = node(tc, wf, None, 0, SEQUENCE, "coder", json!({}))?;

    // The per-phase TASK LOOP: keep building while the CURRENT build phase has open tasks
    // (`phase_open_tasks`, written by the recount at the END of each pass — repeat-until, so it runs
    // at least once). Bounded by a modest backstop.
    let tasks = node(
        tc,
        wf,
        Some(&root),
        0,
        LOOP,
        "tasks",
        json!({ "until": { "var": "phase_open_tasks", "truthy": false }, "max_iterations": 50 }),
    )?;

    // Pick the NEXT open task of the current phase as a NODE (null when the phase is drained).
    node(
        tc,
        wf,
        Some(&tasks),
        0,
        QUERY,
        "next task",
        json!({ "query": "phase_next_task", "write": "task_id" }),
    )?;

    // Have a task? (task_id truthy) → drive it; else → the phase is drained this pass (safe default).
    let have = node(
        tc,
        wf,
        Some(&tasks),
        1,
        SELECTOR,
        "have a task?",
        json!({ "branches": [
            { "cond": { "var": "task_id", "truthy": true }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;

    // (task_id truthy) — START → the work (the human's own, or the coder's) → CLOSE BY THE ROUTE THE
    // TASK ALLOWS. The close used to be an unconditional `done`, and `done` is exactly the one move
    // the task guard-rails refuse for a `kind=human` task and for a task the coder BLOCKED (H9).
    let drive = node(tc, wf, Some(&have), 0, SEQUENCE, "drive task", json!({}))?;
    node(
        tc,
        wf,
        Some(&drive),
        0,
        TRANSITION_TASK,
        "start",
        json!({ "transition": "start", "task_key": "task_id" }),
    )?;

    // WHOSE TASK IS THIS? Read the close route BEFORE the work: a task that closes through the
    // VERIFIED path is a `kind=human` action, and a human action is never handed to the coder — its
    // own manual refuses it, which is precisely how the live run ended with a blocked task and a run
    // re-raising `invalid_state` on every tick. So nyx asks the HUMAN instead of burning a spawn on a
    // refusal it can predict.
    node(
        tc,
        wf,
        Some(&drive),
        1,
        QUERY,
        "how does it close?",
        json!({ "query": "task_close_route", "write": "task_close", "task_key": "task_id" }),
    )?;
    let who = node(
        tc,
        wf,
        Some(&drive),
        2,
        SELECTOR,
        "whose task?",
        json!({ "branches": [
            { "cond": { "var": "task_close", "eq": "verify" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    let human = node(tc, wf, Some(&who), 0, SEQUENCE, "human action", json!({}))?;
    node(
        tc,
        wf,
        Some(&human),
        0,
        ASK_HUMAN,
        "your action",
        json!({
            "prompt": "This task is YOURS — a kind=human action is never given to the coder: {{task_close_reason}}. Answer 'done' once you have performed it (nyx then closes it through the verified path), or 'skip' to abandon it.",
            "output_key": "human_action",
            "options": ["done", "skip"],
        }),
    )?;
    let performed = node(
        tc,
        wf,
        Some(&human),
        1,
        SELECTOR,
        "performed?",
        json!({ "branches": [
            { "cond": { "var": "human_action", "eq": "skip" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&performed),
        0,
        TRANSITION_TASK,
        "skip",
        json!({
            "transition": "skip",
            "task_key": "task_id",
            "reason": "abandoned by the human at the task gate — the action was not performed",
        }),
    )?;
    node(
        tc,
        wf,
        Some(&performed),
        1,
        NOTE,
        "performed",
        json!({ "text": "The human performed the action — nyx closes the task through the verified path." }),
    )?;

    // (else) — an AGENT task: make it green. Spawn the coder effector, run the gate, re-spawn while
    // the gate is RED (a repeat-until on `gate_green`, bounded). The gate ships BLOCKING and declares
    // the [`BUILD_CHECK_SLOT`] SLOT instead of naming commands: this graph is shared by every project,
    // and `bun test` is not something a shared graph can know. Until the project BINDS that slot
    // (`slot_bind`), the gate arms nothing and `nyx_next` REFUSES to advance past it — deliberate,
    // since the alternative, shipping a gate that passes without running a thing, is what let a coder
    // close a task on zero files changed. `run_start` names this node, this workflow AND the slot in
    // its warnings, so the refusal is known before the first spawn rather than discovered mid-build.
    let green = node(
        tc,
        wf,
        Some(&who),
        1,
        LOOP,
        "make it green",
        json!({ "until": { "var": "gate_green", "eq": true }, "max_iterations": 25 }),
    )?;
    node(tc, wf, Some(&green), 0, SPAWN, "implement", json!({ "agent_id": "coder" }))?;
    node(
        tc,
        wf,
        Some(&green),
        1,
        GATE_CHECK,
        "checks",
        json!({ "slot": BUILD_CHECK_SLOT, "blocking": true }),
    )?;

    // RE-READ the route AFTER the work — the OUTCOME, not the plan. Re-deriving it is what keeps it
    // true: the coder may have BLOCKED the task in the meantime (its one legitimate refusal), and a
    // route decided before the spawn would close a task the coder just refused.
    node(
        tc,
        wf,
        Some(&drive),
        3,
        QUERY,
        "how does it close now?",
        json!({ "query": "task_close_route", "write": "task_close", "task_key": "task_id" }),
    )?;
    let close = node(
        tc,
        wf,
        Some(&drive),
        4,
        SELECTOR,
        "close the task",
        json!({ "branches": [
            { "cond": { "var": "task_close", "eq": "done" }, "child": 0 },
            { "cond": { "var": "task_close", "eq": "verify" }, "child": 1 },
            { "cond": { "var": "task_close", "eq": "escalate" }, "child": 2 },
            { "cond": { "always": true }, "child": 3 },
        ] }),
    )?;
    node(
        tc,
        wf,
        Some(&close),
        0,
        TRANSITION_TASK,
        "done",
        json!({ "transition": "done", "task_key": "task_id" }),
    )?;
    node(
        tc,
        wf,
        Some(&close),
        1,
        TRANSITION_TASK,
        "verify",
        json!({ "transition": "verify", "task_key": "task_id" }),
    )?;
    escalate_blocked_task(tc, wf, &close, 2)?;
    // The SAFE default: nothing to close (the task went terminal on this pass — the human skipped it —
    // or it was never started). The reason says which; the recount below decides whether to loop.
    node(
        tc,
        wf,
        Some(&close),
        3,
        NOTE,
        "nothing to close",
        json!({ "text": "{{task_close_reason}} — nothing to close on this pass." }),
    )?;

    // (else) — no task this pass: the phase is drained (the recount below confirms it and exits).
    node(
        tc,
        wf,
        Some(&have),
        1,
        NOTE,
        "phase drained",
        json!({ "text": "No open task in the current phase — the build loop exits." }),
    )?;

    // END of each pass — RECOUNT the current phase's open tasks (GROWS as the coder / review file
    // findings). Falsy (0) exits the loop.
    node(
        tc,
        wf,
        Some(&tasks),
        2,
        QUERY,
        "phase open tasks",
        json!({ "query": "phase_open_tasks", "write": "phase_open_tasks" }),
    )?;

    Ok(root)
}

/// The ESCALATION GATE of a task the coder BLOCKED — wired as one branch of the coder's "close the
/// task" selector, at `order` (H12). Returns the branch's sequence id.
///
/// WHY IT EXISTS. A blocked task has NO legal close, and the coder is right to have blocked it: the
/// case that forced this was an audit finding whose fix is NOT in the code — the code serves
/// `GET|HEAD` where the Spec's `implementation_decisions` still promises `GET /healthz` exactly. The
/// code is right, the PROSE is stale, and the coder never redefines the Spec. Nothing in the graph
/// could resolve that: the coder refused (correctly), the graph tried `done` (refused), and no node
/// could touch the Spec's text — `spec_update_section` refuses while the Spec is out of draft, for the
/// human too. The only way out anyone found was reopening a valid finding by hand, i.e. closing it
/// without fixing it.
///
/// So the decision goes UP, to the one party with the authority to take it, and it has three real
/// routes — each of which actually resolves the task rather than deferring the freeze:
///   * `code` — the finding stands. Nothing is written: the task stays blocked, and the tasks loop
///     re-picks it next pass (a `start` is legal from `blocked`), so the coder retries under the ruling.
///   * `spec` — the contradiction is in the Spec's TEXT. nyx spawns the Spec author with, for THIS
///     GATE ONLY, the authorization to correct that section in place: the answer 'spec' makes the
///     ENGINE write the run-scoped grant ([`crate::engine::SPEC_WRITE_GRANT_KEY`]) that
///     `spec_update_section` demands, and the note right after the spawn takes it back. The freeze
///     stays the rule; the human is its only exception; and because the grant is DERIVED from the
///     answer, it becomes unforgeable the day the answer does (the nyx UI, a later batch).
///   * `defer` — the debt is TRACED and the task closed: the finding is recorded as a backlog idea
///     linked to the Spec, then `skip` closes the task with that reason, so the build can finish.
///
/// The default (always) branch is `code` — the least destructive of the three: it writes nothing and
/// lets the coder try again (N12d).
fn escalate_blocked_task(
    tc: &mut SqliteConnection,
    wf: &str,
    parent: &str,
    order: i32,
) -> QueryResult<String> {
    let seq = node(tc, wf, Some(parent), order, SEQUENCE, "blocked — escalate", json!({}))?;
    node(
        tc,
        wf,
        Some(&seq),
        0,
        ASK_HUMAN,
        "the coder is blocked",
        json!({
            "prompt": "The coder could not take this task and blocked it: {{task_close_reason}}. Answer 'code' if the CODE is what must change (the finding stands — the coder retries this task with your ruling), 'spec' if the contradiction is in the SPEC'S TEXT and the code is right (nyx spawns the Spec author, authorized for this gate only to correct that section in place), or 'defer' to record it as a backlog idea and close the task with that reason.",
            "output_key": "blocked_route",
            "options": ["code", "spec", "defer"],
            // THE ONE THING THAT OPENS THE SPEC FREEZE, and it is a human's answer to this question.
            "grant_spec_write_on": "spec",
        }),
    )?;
    let ruling = node(
        tc,
        wf,
        Some(&seq),
        1,
        SELECTOR,
        "the human's ruling",
        json!({ "branches": [
            { "cond": { "var": "blocked_route", "eq": "spec" }, "child": 0 },
            { "cond": { "var": "blocked_route", "eq": "defer" }, "child": 1 },
            { "cond": { "always": true }, "child": 2 },
        ] }),
    )?;
    // 'spec' — the AUTHORIZED window: it opens on the answer above and closes on the note below.
    let fix_spec = node(tc, wf, Some(&ruling), 0, SEQUENCE, "correct the Spec", json!({}))?;
    node(
        tc,
        wf,
        Some(&fix_spec),
        0,
        SPAWN,
        "correct the Spec's stale text against the code",
        json!({ "agent_id": "spec" }),
    )?;
    node(
        tc,
        wf,
        Some(&fix_spec),
        1,
        NOTE,
        "window closed",
        json!({
            "text": "The Spec's text was corrected — the authorization to write it is withdrawn, and the coder retries this task next pass.",
            "revoke_spec_write": true,
        }),
    )?;
    // 'defer' — TRACE the debt, then close the task with it as the reason.
    let defer = node(tc, wf, Some(&ruling), 1, SEQUENCE, "defer", json!({}))?;
    node(
        tc,
        wf,
        Some(&defer),
        0,
        SPAWN,
        "record this blocked task as a backlog idea linked to the Spec",
        json!({ "agent_id": "spec" }),
    )?;
    node(
        tc,
        wf,
        Some(&defer),
        1,
        TRANSITION_TASK,
        "skip",
        json!({
            "transition": "skip",
            "task_key": "task_id",
            "reason": "deferred by the human at the escalation gate — recorded as a backlog idea on the Spec",
        }),
    )?;
    // 'code' (the SAFE default) — nothing is written: the task stays blocked and the loop retries it.
    node(
        tc,
        wf,
        Some(&ruling),
        2,
        NOTE,
        "the finding stands",
        json!({ "text": "The finding stands — the task stays blocked and the coder retries it on the next pass, with your ruling." }),
    )?;
    Ok(seq)
}

/// Build the COMPLETE prototyper tree (v2.8 lot 2 — the sub-workflow nspec's proto branch delegates the
/// design rounds to). It loops design ROUNDS until the whole design is FROZEN (`design_locked`):
/// COMPOSE a round (spawn the prototype effector until the round actually holds pages to elect on),
/// let the human ELECT the retained variant per page (a PARK — the human elects in the nyx UI / via the
/// proto verbs, the effector applies it on the next spawn), ask to LOCK or iterate, and on 'lock'
/// DISTILL the elected design. NO Spec transition (design is orthogonal to the Spec lifecycle) —
/// authoring-VALID without a sensitive-node gate. Returns the root id.
fn build_prototyper(tc: &mut SqliteConnection, wf: &str) -> QueryResult<String> {
    let root = node(tc, wf, None, 0, SEQUENCE, "prototyper", json!({}))?;

    // The design-ROUNDS loop: iterate while the whole design is NOT yet frozen (`design_locked`,
    // written by the recount at the END of each round — repeat-until). Bounded by a modest backstop.
    let rounds = node(
        tc,
        wf,
        Some(&root),
        0,
        LOOP,
        "design rounds",
        json!({ "until": { "var": "design_locked", "eq": true }, "max_iterations": 20 }),
    )?;

    // COMPOSE the round, and do not leave it until it holds SOMETHING TO ELECT (L2). The election
    // below is a question about a set, and the graph never checked the set was non-empty: on the
    // prototyper's first nominal run the effector pinned no page at all and nyx asked the human to
    // elect anyway. The exit is the NAMED FACT — `design_round_pages`, the pinned manifest of the
    // current round, the very set the UI shows and `proto_distill` refuses to freeze when empty.
    let compose = node(
        tc,
        wf,
        Some(&rounds),
        0,
        LOOP,
        "compose a round",
        json!({
            "until": { "var": "design_round_pages", "truthy": true },
            "max_iterations": PROTO_ROUND_ATTEMPTS,
        }),
    )?;
    node(tc, wf, Some(&compose), 0, SPAWN, "prototype", json!({ "agent_id": "prototype" }))?;
    node(
        tc,
        wf,
        Some(&compose),
        1,
        QUERY,
        "anything to elect?",
        json!({ "query": "design_round_pages", "write": "design_round_pages" }),
    )?;

    // ELECTION is a PARK: the human elects the retained variant per page in the nyx UI (or states it);
    // the effector applies the election on the next spawn. Free-form (no closed option set) — no branch
    // reads it, so J1 does not apply. But it NAMES what is on the table (`{{design_round_pages_reason}}`,
    // read one node above): a human asked to choose is shown the choices, not asked from memory.
    node(
        tc,
        wf,
        Some(&rounds),
        1,
        ASK_HUMAN,
        "election",
        json!({
            "prompt": "{{design_round_pages_reason}}\n\nElect the RETAINED variant per page (elect it \
                       in the nyx UI, or state it here). The prototype effector applies your election \
                       on its next round.",
            "output_key": "election",
        }),
    )?;
    node(
        tc,
        wf,
        Some(&rounds),
        2,
        ASK_HUMAN,
        "feedback",
        json!({
            "prompt": "Lock this design, or request changes?",
            "output_key": "feedback",
            "options": ["lock", "changes"],
        }),
    )?;
    // Lock or iterate? 'lock' → DISTILL (freeze) the elected design; else → open another round.
    let lock = node(
        tc,
        wf,
        Some(&rounds),
        3,
        SELECTOR,
        "lock or iterate?",
        json!({ "branches": [
            { "cond": { "var": "feedback", "eq": "lock" }, "child": 0 },
            { "cond": { "always": true }, "child": 1 },
        ] }),
    )?;
    node(tc, wf, Some(&lock), 0, SPAWN, "distill", json!({ "agent_id": "prototype" }))?;
    node(
        tc,
        wf,
        Some(&lock),
        1,
        NOTE,
        "iterate",
        json!({ "text": "Iterate — open another design round." }),
    )?;

    // END of each round — re-read whether the design is now FROZEN. `true` exits the loop.
    node(
        tc,
        wf,
        Some(&rounds),
        4,
        QUERY,
        "design locked?",
        json!({ "query": "design_locked", "write": "design_locked" }),
    )?;

    Ok(root)
}

#[cfg(test)]
mod tests;
