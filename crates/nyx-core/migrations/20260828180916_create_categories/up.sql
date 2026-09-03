-- `categories` — the ONE "genre of work" axis of a Spec (PDR-0017), plus the nullable
-- `specs.category_id` that carries it.
--
-- ## What this is, and what it replaces
--
-- PDR-0014 had acted TAGS (m:n) as the Spec's classification vocabulary. PDR-0017 retracted that
-- half: a tag soup proliferates and blurs the read, and the three axes already stored
-- (`status`, `priority`, milestone) left exactly ONE gap — "what KIND of work is this". So a Spec
-- carries AT MOST ONE category, and nothing else classifies it. The `tags` / `spec_tags` /
-- `task_tags` tables PDR-0017 asks to drop were never created in this schema; there is nothing to
-- drop here, and this note exists so the next reader does not go looking for them.
--
-- ## `slug` is the IDENTITY, `name` is the LABEL
--
-- The pair is the whole point of the table, and the reason a second text column exists next to a
-- name that would have sufficed for a board chip:
--
--   * `slug` is unique WITHIN the project and IMMUTABLE. It is what the agent harness reads —
--     `feature`, `bug`, `refactor`, `chore` — when it decides which discipline a Spec is subject
--     to. A stable, machine-readable key is what lets a human rename a category without silently
--     changing the rules a Spec is judged by.
--   * `name` is the human label. It is freely editable and may even be a HOMONYM of another
--     category's name (PDR-0017 asks for rename + merge as curation gestures, and a rename that
--     collides must not be refused by the schema). Hence NO unique index on `(project_id, name)`
--     — the DBML carried one when the table was still name-keyed, and it is superseded here.
--
-- The UNIQUE index is therefore on `(project_id, slug)`: one slug per project, and the same slug
-- freely re-used across projects (a category BELONGS to a project — PDR-0017 — so `feature` in
-- project A and `feature` in project B are two distinct rows, and a Spec may only point at one
-- from its OWN project).
--
-- ## `created_by_type`, not a `system` boolean — and `confirmed_at`, since VOIDED
--
-- PDR-0017's governance is "grill, non bloquant": the agent ASSIGNS from the existing set and, when
-- nothing fits, MINTS on the fly rather than blocking. What the row has to keep is WHO minted it
-- (`system` seed | `agent` mint | `human` gesture): a single `system` boolean would have collapsed
-- "minted by an agent" and "created by a human from the settings" into the same value, and the two
-- are told apart nowhere else in the persisted trace.
--
-- `confirmed_at` was added here for a SECOND fact that no longer exists. When this migration was
-- written a mint was born PROPOSED and a human blessed it afterwards; the 2026-09-02 AMENDMENT of
-- PDR-0017 retired that proposed/confirmed axis outright — a mint is valid as it stands, there is
-- no `confirm` gesture, no badge, and no `state` in any payload. The column survives as a VESTIGE:
-- every creation path stamps it, nothing reads it, and it is to be dropped the next time this table
-- is migrated for another reason (see `db::Category::confirmed_at`). This paragraph is left in
-- place rather than rewritten away because it explains a column a reader will otherwise find
-- unmotivated.
--
-- The CHECK closes the vocabulary at those three, the same way `managed_command_groups.color`'s
-- CHECK closes the palette: an unknown provenance can never reach the renderer as a chip nobody
-- knows how to style. `db.rs` mirrors the list in `CATEGORY_CREATED_BY_TYPES` and a test exercises
-- the constant against this live CHECK, so the two spellings cannot drift.
--
-- ## `specs.category_id` — nullable, and appended LAST
--
-- NULL means UNCATEGORIZED, which is a legitimate steady state (every Spec that predates this
-- migration is in it) and never an error. The FK is `ON DELETE SET NULL`: removing a category
-- releases its Specs, never deletes them — the same disposition `managed_commands.group_id` takes
-- toward its group.
--
-- The FK cannot express the invariant that MATTERS: SQLite can check that `category_id` names an
-- existing category, not that the category belongs to the SAME project as the Spec. That refusal is
-- pronounced by the domain service (`category.rs`), which is the single writer of this column —
-- PDR-0017 puts the governance rule in the harness/service layer, not in the schema.
--
-- `ALTER TABLE ADD COLUMN` appends the column at the END of `specs`' physical layout, and Diesel
-- decodes a row POSITIONALLY: `schema.rs` must therefore declare `category_id` LAST inside
-- `table! { specs }`, after `reviewing_phase`. Declaring it next to `priority` — where the DBML
-- draws it, for readability — would silently decode the wrong column into it.
--
-- ## Why the prefix is DATED
--
-- The `000000000000NN` lineage is FROZEN at `00000000000051`: two sibling worktrees each minting
-- "the next" number against the same `%APPDATA%/nyx-v2/nyx.db` collide silently, and that was paid
-- for on a live DB (written up in `20260819140000_create_command_groups/up.sql`). The rule lives in
-- `crates/nyx-core/migrations/README.md` and is enforced by `db.rs::migration_naming_convention`.

CREATE TABLE categories (
    -- UUIDv7 generated by the backend (SQLite does not auto-generate it).
    id              TEXT    NOT NULL PRIMARY KEY,
    project_id      TEXT    NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- The STABLE, machine-read identity (ASCII, lowercase, hyphens). Immutable once written.
    slug            TEXT    NOT NULL,
    -- The human LABEL. Editable, and free to collide with another category's name.
    name            TEXT    NOT NULL,
    -- NULL = no colour chosen (the neutral default); else a design-token name.
    color_token     TEXT,
    -- Position in the project's curated list (append order).
    position        INTEGER NOT NULL DEFAULT 0,
    -- WHO minted the row: the per-project seed, an agent's on-the-fly mint, or a human.
    created_by_type TEXT    NOT NULL CHECK (created_by_type IN ('system', 'agent', 'human')),
    -- Epoch ms the row became valid. VESTIGIAL since PDR-0017 was amended on 2026-09-02: every
    -- creation path stamps it and nothing reads it (see the header). Nullable only because the
    -- column predates that amendment; slated to be dropped at the next rebuild of this table.
    confirmed_at    INTEGER,
    created_at      INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
    updated_at      INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);

-- The invariant AND the upsert key of the idempotent seed: a project answers a slug exactly once.
CREATE UNIQUE INDEX idx_categories_project_slug ON categories (project_id, slug);

-- The Spec's single category (NULL = uncategorized). See the note above on positional decoding.
ALTER TABLE specs ADD COLUMN category_id TEXT REFERENCES categories(id) ON DELETE SET NULL;

-- The lookup the board and the harness make: "the Specs of this category".
CREATE INDEX idx_specs_category ON specs (category_id);
