-- `managed_command_groups` — the MULTI-PANE grouping of managed commands.
--
-- ## Why this migration is DATED and not numbered `00000000000050` (human review 01M0CY3F075ACP4TFBK1KJ49X8)
--
-- Diesel identifies an applied migration by the NUMERIC PREFIX of its directory alone — the name
-- after the `_` is never stored: `__diesel_schema_migrations` holds `(version, run_on)`. So two
-- SIBLING WORKTREES of this repo, each adding "the next" migration `00000000000050_*` and each
-- booting the app against the SAME `%APPDATA%/nyx-v2/nyx.db`, produce a silent COLLISION: whichever
-- ran first writes the row `00000000000050`, and from then on the other one is reported as ALREADY
-- APPLIED and is never executed. `pending_migrations()` returns empty, the boot claims success, and
-- the tables the second migration should have created simply do not exist.
--
-- That is exactly what happened here, measured on the user's live DB on 2026-08-19: the ledger
-- carried `00000000000050` (run 2026-08-16 10:49:30) and `00000000000051` (run 2026-08-17 16:59:20),
-- both written by the worktree `nyx-spec-url-links` (`00000000000050_annexes_url_unique`,
-- `00000000000051_annexes_url_canonical` — the index `idx_annexes_url_unique_per_spec` is in that DB
-- and this one is NOT), plus two timestamped versions (`20260818020000`, `20260818130000`) from yet
-- another build. `managed_command_groups` was never created and `managed_commands` never got
-- `group_id`/`group_order`, so every command read failed with `no such table` / `no such column`.
--
-- The cure is a version that CANNOT collide with a sequence lineage: a UTC-ish timestamp (Diesel's
-- own `diesel migration generate` convention). It sorts after every `000000000000NN`, so it is the
-- head, and no sibling branch can mint the same one. NEW MIGRATIONS IN THIS REPO SHOULD BE DATED
-- THE SAME WAY. The boot now also REFUSES to claim success when a migration is pending — and NAMES
-- the ledger versions this build does not embed, which is the only trace this failure mode leaves:
-- `SchemaHealth::boot_report` + `foreign_ledger_versions` in `db.rs`, read at boot by
-- `runtime::db::schema_boot_report` and acted on in the sidecar's `main.rs`.
--
-- ## Why this is DB state and not a renderer preference
--
-- Terminal groups live in the renderer's `localStorage`: a terminal is a live process the renderer
-- owns, and its grouping is a per-machine view preference. A COMMAND group is not: the review that
-- asked for it (01M0ABR6V501D0SPR2BYBVZC4A) also requires the AGENT to drive the very same
-- operations over MCP — create a group, move a command in or out, name it, colour it, fix its
-- disposition — and to produce EXACTLY the state a hand gesture produces, in both directions.
-- MCP is served by the sidecar, which cannot see the renderer's `localStorage`; so the grouping has
-- to be where both can reach it, which is here.
--
-- ## Shape
--
-- A group is an IDENTITY plus two OPTIONAL labels. `name` NULL means unnamed and `color` NULL means
-- uncoloured — both are the DEFAULT, and neither conditions the group's existence (the user's rule:
-- a group is born grey and nameless and stays usable that way). `color` is the 1-based index of a
-- `--group-N` design token; the CHECK keeps it inside the six that exist, so an out-of-range value
-- is refused at the boundary instead of reaching the UI as a broken `var(--group-9)`.
--
-- MEMBERSHIP is a nullable FK on `managed_commands`, NOT a join table: that is what makes "a command
-- belongs to zero or one group" a fact of the schema rather than an invariant to police. Its
-- ON DELETE SET NULL means deleting a group releases its members as SOLO commands — never deletes
-- them.
--
-- `group_order` is the command's position INSIDE its group, and it is HALF the DISPOSITION model:
-- the pane tree is DERIVED from the ordered member list, so re-ordering the members IS re-arranging
-- the panes. It is meaningless for a command with no group and simply stays at its default there.
--
-- ⚠ CORRECTED, 2026-08-27 (human review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding
-- 01M11XTRXVVPZ9XMDW5ARWKWHA). This paragraph used to close on "there is no second layout record to
-- keep in sync", and it named the derivation rule as fixed (2 side by side, the 3rd underneath, the
-- 4th in a quadrant). Both halves have moved, and the next reader must not take the old sentence at
-- face value:
--
--   * the derived shape is now the DEFAULT, not the only option. Migration
--     `20260827120000_command_groups_layout_shape` adds `managed_command_groups.layout_shape` —
--     `auto` (this rule, unchanged) | `row` | `column` | `grid` | `main-left` | `main-top` — because
--     a shape derived from the member COUNT can express neither "4 in a column" nor "1 big + 3
--     small", which the user asked for by name;
--   * the claim it rested on nevertheless HOLDS. `layout_shape` is a LABEL, not a layout record: no
--     rectangle, no ratio, no per-member coordinate. The tree is still DERIVED — from (ordered
--     members + label) instead of (ordered members) alone — so there is still nothing to keep in
--     sync, and still nothing to migrate when a shape's rule improves.

CREATE TABLE managed_command_groups (
    -- UUIDv7 generated by the backend (SQLite does not auto-generate it).
    id         TEXT    NOT NULL PRIMARY KEY,
    project_id TEXT    NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- NULL = unnamed. A blank string is normalised to NULL by the backend before write.
    name       TEXT,
    -- NULL = no colour chosen (the neutral default); else the 1-based `--group-N` index.
    color      INTEGER CHECK (color IS NULL OR (color >= 1 AND color <= 6)),
    created_at INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
    updated_at INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);

-- Index the FK column for fast `list_command_groups(project_id)` scans.
CREATE INDEX idx_managed_command_groups_project ON managed_command_groups (project_id);

-- MEMBERSHIP + POSITION on the command itself (see the note above).
ALTER TABLE managed_commands ADD COLUMN group_id TEXT REFERENCES managed_command_groups(id) ON DELETE SET NULL;
ALTER TABLE managed_commands ADD COLUMN group_order INTEGER NOT NULL DEFAULT 0;

-- The lookup every render does: "the members of this group, in disposition order".
CREATE INDEX idx_managed_commands_group ON managed_commands (group_id, group_order);
