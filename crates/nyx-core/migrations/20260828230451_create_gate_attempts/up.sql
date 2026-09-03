-- `gate_attempts` — the APPEND-ONLY journal of every `gate_check` execution nyx ran.
--
-- ## Why a table and not a blackboard key
--
-- The run's board already carries `last_gate_result`: THE LAST attempt, under a fixed key,
-- overwritten by the next gate. That is the right shape for "why is this run red right now" and the
-- wrong shape for the question a `bug` Spec owes an answer to — *was an identified check OBSERVED
-- failing before the fix and passing after*. That question is about a SEQUENCE of at least two
-- attempts, and a key that holds one attempt cannot hold a sequence: the green that closes the bug
-- overwrites the red that proved it, and the proof disappears at the exact moment it becomes
-- provable. Neither the auditor (who is told to compare the two) nor the resume engine (which has to
-- know a red already happened) can read what was overwritten.
--
-- So attempts ACCUMULATE. Nothing in this schema updates a row and nothing deletes one: the only
-- write is an INSERT. `sequence` is a rowid alias — strictly increasing per insert — which gives the
-- journal a total order that does not depend on the clock, so two attempts landing in the same
-- millisecond are still ordered, and ordered as they happened.
--
-- ## Correlation ids, NOT foreign keys
--
-- `run_id`, `spec_id` and `node_id` carry no `REFERENCES` clause, exactly as `domain_events`'
-- correlation ids do not (v12), and for the same reason sharpened by this table's purpose: an
-- append-only proof that a CASCADE can silently truncate is not append-only. Deleting a run,
-- retiring a Spec or restructuring a workflow are ordinary gestures; none of them may erase the
-- record that a check was run and what it did. A dangling id is an honest "the thing this describes
-- is gone"; a missing row is a proof that never existed as far as any reader can tell — and the
-- auditor is instructed to treat what they cannot see recorded as absent.
--
-- `node_id` is the SECOND half of the identity: a flow can hold several gates (a reproduction one
-- and a build one), and their attempts must not read as one series. `slot` is the third — the NAME
-- of what is being verified, which is what a shared graph declares and what the auditor compares
-- between the red and the green.
--
-- ## `expect` / `outcome` / `satisfied` — three columns, because they are three facts
--
--   * `expect` is what the graph ASKED FOR (`green` | `red`). `declared` says whether the node
--     spelled it or whether it defaulted, because a graph that declared nothing and one that
--     declared `green` are graded identically but ENFORCED differently — the first through the M5
--     red anchor, the second on the spot — and a journal that flattened the two would misdescribe
--     why a run was refused.
--   * `outcome` is what the check DID, on three values and not two: `passed` (exit 0), `failed` (a
--     normal termination with a non-zero status) and `indeterminate` (a timeout, a cancellation, a
--     spawn that failed, any infrastructure fault). The third value is the whole reason this
--     column is not a boolean: read in binary, a gate that COULD NOT RUN would have counted as
--     "not green" and therefore as a reproduction — a check that verified nothing standing as the
--     proof that the defect reproduces.
--   * `satisfied` is whether the second answered the first. `expect: green` is satisfied only by
--     `passed`, `expect: red` only by `failed`, and `indeterminate` satisfies NEITHER.
--
-- The two CHECKs close both vocabularies the way `categories.created_by_type`'s does: a value no
-- reader knows how to grade can never reach one.
--
-- ## The identity of the control, and the bounded output
--
-- `command_line` and `cwd` hold the RESOLVED command lines and run directories — resolved at the
-- moment the attempt ran, joined by newline when a gate runs several. They are denormalised out of
-- `commands_json` on purpose: the auditor's question ("same slot, same resolved commands, same
-- cwd?") is then a comparison of two strings rather than a walk of two JSON documents, and a
-- divergence is visible without interpretation.
--
-- `output_excerpt` is a BOUNDED extract of the command's combined stdout/stderr (the PTY merges the
-- two into one stream, so there is one excerpt and not two), kept from the END — the part that
-- explains a failure — and marked when cut. The bound and the sanitisation applied to it are stated
-- once, as an executable policy, in `db.rs` (`gate_evidence_excerpt`); this column stores its
-- output and nothing else. `commands_json` carries the same detail per command, already bounded by
-- the same policy.
--
-- ## Why the prefix is DATED
--
-- The `000000000000NN` lineage is FROZEN at `00000000000051` — see
-- `crates/nyx-core/migrations/README.md` and `db.rs::migration_naming_convention`.

CREATE TABLE gate_attempts (
    -- Monotonic global append order (rowid alias): strictly increasing per insert. THE ordering key
    -- of the journal — never the clock, which can tie or go backwards.
    sequence       INTEGER NOT NULL PRIMARY KEY,

    -- Correlation ids (NOT foreign keys — see the note above).
    run_id         TEXT    NOT NULL,
    -- The Spec the run drives. NULL for a run bound to no Spec (a custom workflow run): such a run
    -- still produces evidence, it just has no Spec to hang it under.
    spec_id        TEXT,
    -- The `gate_check` node. Two gates of one flow are two nodes.
    node_id        TEXT    NOT NULL,
    -- The command SLOT the node declared, NULL when it names its own `command_ids`.
    slot           TEXT,

    -- What the graph asked for, and whether the node spelled it.
    expect         TEXT    NOT NULL CHECK (expect IN ('green', 'red')),
    declared       INTEGER NOT NULL DEFAULT 0,
    -- Whether the node is BLOCKING: the difference between a refusal and an advisory reading.
    blocking       INTEGER NOT NULL DEFAULT 1,

    -- What the check did, and whether that answered the expectation.
    outcome        TEXT    NOT NULL CHECK (outcome IN ('passed', 'failed', 'indeterminate')),
    satisfied      INTEGER NOT NULL DEFAULT 0,

    -- The IDENTITY of the control, resolved as it ran.
    command_line   TEXT    NOT NULL DEFAULT '',
    cwd            TEXT    NOT NULL DEFAULT '',
    -- The DECIDING command's exit status; NULL when no status was ever read (an indeterminate).
    exit_code      INTEGER,
    -- Wall time of the whole attempt, in ms.
    duration_ms    INTEGER NOT NULL DEFAULT 0,

    -- The bounded, sanitised extract of the combined output, and the per-command detail.
    output_excerpt TEXT    NOT NULL DEFAULT '',
    commands_json  TEXT    NOT NULL DEFAULT '[]',

    -- Epoch ms, the same expression every other table in this schema uses.
    created_at     INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);

-- THE read the auditor and the resume engine make: "this Spec's attempts, in the order they
-- happened". `sequence` rides the index so the ordered scan needs no sort.
CREATE INDEX idx_gate_attempts_spec ON gate_attempts (spec_id, sequence);

-- The diagnostic read: "this run's attempts", for a run card and for a resume that has the run id
-- but not (yet) the Spec.
CREATE INDEX idx_gate_attempts_run ON gate_attempts (run_id, sequence);
