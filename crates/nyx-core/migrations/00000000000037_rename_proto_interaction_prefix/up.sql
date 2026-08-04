-- Schema v37: rename the PROTOTYPE INTERACTION VOCABULARY in the STORED data — `data-depot-*`
-- becomes `data-nyx-*` and `__depot*` becomes `__nyx*` (v2.7 — Prototype : machinerie, review
-- 01KYKX2A0JGWF8XNWWZ9YFF5MG, finding 01KYS14G4V1Q5ERRAHB0QVW362 — "Renommer le vocabulaire
-- d'interaction").
--
-- ## Why v37 (a migration NUMBER is a GLOBAL, cross-branch identity)
--
-- `__diesel_schema_migrations` stores a NUMBER, never a name: two branches claiming the same number
-- make one of the two migrations NEVER run on any DB that saw the other, while the code keeps
-- assuming it did. 34 belongs to `feature/v2.8-engine` (`create_workflow_engine`), 35
-- (`design_components_scan_state`) and 36 (`design_add_bundle_recipe`) are v2.7's, so the next free
-- number across ALL live branches is 37. (The series also carries a hole at 19 — the directory
-- COUNT is not the highest number.)
--
-- ## What this migrates, and why it is DATA and not SCHEMA
--
-- The prototype machinery was PORTED FROM DEPOT (the workflow tool) and its interaction markers were
-- taken verbatim: `data-depot-state-case`, `data-depot-if`, `data-depot-if-not`, `data-depot-page`,
-- `data-depot-state`, `data-depot-axis`, `data-depot-set`, and the globals `window.__depotStates` /
-- `__depotActiveState` / `__depotAxes` / `__depotAxisValues`. It spread: mechanisms INVENTED in nyx
-- and nowhere else (`data-depot-if`, `data-depot-axis`) still carried the other tool's name. The
-- code, the agent manual and the write-time warnings now speak `nyx` exclusively.
--
-- That prefix is written IN THE STORED CONTENT, not in a column name: it lives inside the SYNTHESIZED
-- TEMPLATES (`design_components.template`) and can live inside a composition's INLINE MARKUP
-- (`prototype_variants.composition_json`). Renaming the code alone would leave every stored template
-- speaking a vocabulary nothing reads any more — every state case shown at once, every conditional
-- element stuck visible. So the data moves WITH the code.
--
-- ## Why a blind textual substitution is SAFE here
--
-- `data-depot-` and `__depot` are not words a template can contain by accident: the first is an HTML
-- data-attribute prefix and the second a JS global prefix, both minted by this machinery. There is
-- no shorter/longer collision either — `data-depot-` only ever prefixes one of the seven markers,
-- and `__depot` only one of the four globals. `replace()` is applied unconditionally on the matching
-- rows, so the migration is IDEMPOTENT (a re-run finds nothing left to change) and LOSSLESS: the
-- rows are UPDATE-d in place, never deleted and re-inserted, so the row count, the ids, the
-- provenance columns, `params_json`, `states_json`, `visual_hash` and every timestamp are untouched.
-- `updated_at` is deliberately NOT bumped: this is nyx renaming its own vocabulary, not the agent
-- re-synthesizing a component — bumping it would misreport the row as freshly authored.
--
-- `composition_json` is JSON stored as TEXT, and neither `-` nor `_` needs escaping in a JSON string,
-- so the substitution cannot break the document's syntax.
--
-- Anything the migration does NOT reach (a synthesis in flight, an import, an older export) is still
-- READ by the renderer shim: `apps/desktop/renderer/src/components/design/proto-shim.ts` keeps ONE
-- isolated, commented block that accepts the old spelling on READ so such a template renders instead
-- of coming back blank. It is tolerated there, never emitted and never taught.

UPDATE design_components
SET template = replace(replace(template, 'data-depot-', 'data-nyx-'), '__depot', '__nyx')
WHERE template LIKE '%data-depot-%' OR template LIKE '%__depot%';

UPDATE prototype_variants
SET composition_json = replace(
        replace(composition_json, 'data-depot-', 'data-nyx-'),
        '__depot',
        '__nyx'
    )
WHERE composition_json LIKE '%data-depot-%' OR composition_json LIKE '%__depot%';
