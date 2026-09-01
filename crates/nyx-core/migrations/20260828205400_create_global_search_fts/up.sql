-- The FULL-TEXT substrate of the GLOBAL SEARCH palette (Spec « Recherche live globale »,
-- Phase 1, task 01KZ8GHDXSV2M7WET58GDG0QHV — "Index FTS5 : specs, revisions et taches").
--
-- ## Why only these three tables
--
-- The palette searches seven entity types. Six of them are LOW-VOLUME (projects, workspaces,
-- managed commands, agent sessions) or already indexed (`ideas_fts`, v26): a bounded scan with
-- an exact/prefix/fuzzy pass is enough and costs nothing. What is VOLUMINOUS is the Spec corpus
-- — a Spec's title, the long prose of its revisions (problem / solution / decisions), and the
-- title of every task. Those three get a real FTS5 index; the rest is matched directly by the
-- aggregator.
--
-- ## The pattern: external content, copied from `ideas_fts`
--
-- Same shape as migration `00000000000026` (`content=` + `content_rowid='rowid'`): the index
-- stores tokenized terms only and reads the column text back from the source table through the
-- implicit integer `rowid` — the canonical FTS5 pattern for a table whose primary key is a TEXT
-- UUID (a UUID cannot be an FTS rowid; the implicit rowid can). Each read joins
-- `<table>.rowid = <table>_fts.rowid` to map a hit back to its entity id, and scores with
-- `bm25(<table>_fts)`.
--
-- ## What is deliberately NOT filtered here
--
-- EVERY revision is indexed, archived Specs included. The « current revision only » restriction,
-- the exclusion of archived Specs, and the exclusion of an archived Spec's tasks are applied AT
-- QUERY TIME by joining `specs` (`specs.current_revision_number`, `specs.archived_at IS NULL`) —
-- the very pattern `fts_match_ideas` already applies for `project_id`. Filtering at INDEXING time
-- would mean a trigger on `specs` firing on every revision fork (and on every archive/unarchive)
-- to re-index a row whose own text never changed: more moving parts for a filter a join does for
-- free.
--
-- ## What is deliberately NOT indexed
--
--   * `spec_revisions.user_stories` and `spec_revisions.out_of_scope` — raw JSON stored in TEXT.
--     Their KEYS (`title`, `as_a`, `i_want`, …) would be tokenized as content and match every
--     revision in the base.
--   * `tasks.description` / `tasks.acceptance_criteria` — the palette matches a task by its
--     TITLE; indexing the structured body would drown a title hit under prose.

-- --- specs_fts: the Spec TITLE index ------------------------------------------

CREATE VIRTUAL TABLE specs_fts USING fts5(
    title,
    content='specs',
    content_rowid='rowid'
);

CREATE TRIGGER specs_fts_ai AFTER INSERT ON specs BEGIN
    INSERT INTO specs_fts (rowid, title) VALUES (new.rowid, new.title);
END;

CREATE TRIGGER specs_fts_ad AFTER DELETE ON specs BEGIN
    INSERT INTO specs_fts (specs_fts, rowid, title) VALUES ('delete', old.rowid, old.title);
END;

CREATE TRIGGER specs_fts_au AFTER UPDATE ON specs BEGIN
    INSERT INTO specs_fts (specs_fts, rowid, title) VALUES ('delete', old.rowid, old.title);
    INSERT INTO specs_fts (rowid, title) VALUES (new.rowid, new.title);
END;

-- --- spec_revisions_fts: the Spec PROSE index ---------------------------------
--
-- The five narrative columns of a revision. `change_reason` is out (it describes the FORK, not
-- the Spec) and the two JSON columns are out (see the header).

CREATE VIRTUAL TABLE spec_revisions_fts USING fts5(
    title,
    problem,
    solution,
    implementation_decisions,
    testing_decisions,
    content='spec_revisions',
    content_rowid='rowid'
);

CREATE TRIGGER spec_revisions_fts_ai AFTER INSERT ON spec_revisions BEGIN
    INSERT INTO spec_revisions_fts (
        rowid, title, problem, solution, implementation_decisions, testing_decisions
    ) VALUES (
        new.rowid, new.title, new.problem, new.solution, new.implementation_decisions,
        new.testing_decisions
    );
END;

CREATE TRIGGER spec_revisions_fts_ad AFTER DELETE ON spec_revisions BEGIN
    INSERT INTO spec_revisions_fts (
        spec_revisions_fts, rowid, title, problem, solution, implementation_decisions,
        testing_decisions
    ) VALUES (
        'delete', old.rowid, old.title, old.problem, old.solution, old.implementation_decisions,
        old.testing_decisions
    );
END;

CREATE TRIGGER spec_revisions_fts_au AFTER UPDATE ON spec_revisions BEGIN
    INSERT INTO spec_revisions_fts (
        spec_revisions_fts, rowid, title, problem, solution, implementation_decisions,
        testing_decisions
    ) VALUES (
        'delete', old.rowid, old.title, old.problem, old.solution, old.implementation_decisions,
        old.testing_decisions
    );
    INSERT INTO spec_revisions_fts (
        rowid, title, problem, solution, implementation_decisions, testing_decisions
    ) VALUES (
        new.rowid, new.title, new.problem, new.solution, new.implementation_decisions,
        new.testing_decisions
    );
END;

-- --- tasks_fts: the task TITLE index ------------------------------------------

CREATE VIRTUAL TABLE tasks_fts USING fts5(
    title,
    content='tasks',
    content_rowid='rowid'
);

CREATE TRIGGER tasks_fts_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts (rowid, title) VALUES (new.rowid, new.title);
END;

CREATE TRIGGER tasks_fts_ad AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts (tasks_fts, rowid, title) VALUES ('delete', old.rowid, old.title);
END;

CREATE TRIGGER tasks_fts_au AFTER UPDATE ON tasks BEGIN
    INSERT INTO tasks_fts (tasks_fts, rowid, title) VALUES ('delete', old.rowid, old.title);
    INSERT INTO tasks_fts (rowid, title) VALUES (new.rowid, new.title);
END;

-- --- backfill: the rows that already exist ------------------------------------
--
-- The triggers only cover FUTURE writes. An existing database carries Specs, revisions and tasks
-- written long before this migration, and an external-content index that never saw them stays
-- empty for them forever. `'rebuild'` is FTS5's own command for exactly that: it re-reads the
-- whole content table and rebuilds the index from it. On a fresh database it is a no-op.

INSERT INTO specs_fts (specs_fts) VALUES ('rebuild');
INSERT INTO spec_revisions_fts (spec_revisions_fts) VALUES ('rebuild');
INSERT INTO tasks_fts (tasks_fts) VALUES ('rebuild');
