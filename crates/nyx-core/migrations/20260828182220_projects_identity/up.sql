-- Migration 20260828182220: the project's VISUAL IDENTITY — `projects.accent` +
-- `projects.icon` / `projects.image_path`.
--
-- The home hors-projet lists sessions from EVERY project in one flat, attention-sorted flow, so a
-- row has to say WHICH project it belongs to without a second line of text. That mark is the
-- project's identity, and identity is CHOSEN by the human, never derived: a deterministic hash on
-- `projects.key` was explicitly ruled out (a colour nobody picked is a colour nobody recognises).
--
-- ## accent
--
-- A colour, as an opaque string the CORE never interprets — the renderer resolves it against the
-- design system. NULLABLE and NULL by default: a project that never picks one falls back to the
-- neutral tile, so nothing changes for anyone who ignores this column.
--
-- ## icon XOR image_path
--
-- The mark itself, in its two shapes: `icon` is a glyph token (resolved by the renderer's icon
-- set) and `image_path` is a file path / URL the renderer renders as an image. They are the SAME
-- slot in two spellings, so at most ONE may be set — the CHECK makes that structural instead of a
-- convention the write path has to remember. A column-level CHECK is legal on `ADD COLUMN` and is
-- not re-verified against existing rows, which is safe here: every pre-existing row lands on
-- (NULL, NULL) and satisfies it anyway.
--
-- Three plain nullable ADD COLUMNs: no index, no FK, no rebuild. Appended LAST on `projects`
-- (positional decoding), same shape and same level as `worktrees_root` and `resume_agent_sessions`.

ALTER TABLE projects ADD COLUMN accent TEXT;
ALTER TABLE projects ADD COLUMN icon TEXT;
ALTER TABLE projects ADD COLUMN image_path TEXT CHECK (image_path IS NULL OR icon IS NULL);
