-- Schema v50: `app_settings` — the GLOBAL, app-level preferences of ONE nyx installation.
--
-- ## The gap this closes
--
-- Everything nyx persists so far belongs to a project, a workspace, a terminal or a Spec: the
-- data model has an identity for every scope EXCEPT the app itself. So a preference that is
-- neither about a project nor about a workspace — "which shell do my terminals open on" — had
-- nowhere to live, and the answer stayed implicit: `$SHELL` then a per-OS default, decided in
-- `nyx_core::pty::resolve_shell` with no way for the user to say otherwise.
--
-- ## Shape — a KEY/VALUE row, deliberately
--
-- One column per preference would make every new preference a migration, and app-level
-- preferences are exactly the class of fact that arrives one at a time and by surprise. A
-- key/value row instead: the KEY is the preference's stable name (a constant in `db.rs`, never a
-- string typed at a call site), the VALUE is its serialized form — a plain string for a scalar,
-- JSON for a preference that carries more than one field (the default shell carries a program
-- AND its arguments, so a WSL distribution can be named at all).
--
-- The row is the preference's PRESENCE, not just its content: an ABSENT key means "the user has
-- expressed nothing", which is a different fact from any value we could store for it — it is what
-- keeps the historical resolution (`$SHELL` → per-OS default) strictly unchanged for an install
-- that never opened this setting. Clearing a preference therefore DELETES its row rather than
-- writing an empty value.
--
-- NO foreign key and no correlation column: an app preference is scoped to the installation, so
-- there is nothing for it to reference and nothing that could cascade it away.

CREATE TABLE app_settings (
    -- The preference's stable name (e.g. `default_shell`). PRIMARY KEY: one row per preference,
    -- so a write is an upsert and a read is a point lookup.
    key        TEXT    NOT NULL PRIMARY KEY,

    -- The preference's serialized value. TEXT for every preference — a scalar is stored verbatim,
    -- a structured one as JSON — so a new preference never needs a column nor a migration. NOT
    -- NULL: "no value" is the ABSENCE of the row, never a NULL in it.
    value      TEXT    NOT NULL,

    -- When the preference was last written (epoch ms). Read by nobody as a verdict; it dates the
    -- row for anyone inspecting the DB.
    updated_at INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);
