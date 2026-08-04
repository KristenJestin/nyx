-- Revert schema v37: put the PRE-RENAME prototype interaction vocabulary back into the stored data
-- (`data-nyx-*` → `data-depot-*`, `__nyx*` → `__depot*`).
--
-- The down is the exact mirror of the up, and it has to be: reverting v37 means going back to a
-- binary whose shim, manual and warnings speak `data-depot-*`, so leaving `data-nyx-*` in the
-- templates would hand that binary a vocabulary it does not read — the very breakage the up exists
-- to prevent, in the other direction.
--
-- It is LOSSLESS in the same way: the same rows are UPDATE-d in place, the substitution is total and
-- unambiguous (`data-nyx-` and `__nyx` are minted by this machinery and by nothing else), so
-- down-then-up restores the templates byte for byte, and no row, id, timestamp or sibling column is
-- touched.

UPDATE design_components
SET template = replace(replace(template, 'data-nyx-', 'data-depot-'), '__nyx', '__depot')
WHERE template LIKE '%data-nyx-%' OR template LIKE '%__nyx%';

UPDATE prototype_variants
SET composition_json = replace(
        replace(composition_json, 'data-nyx-', 'data-depot-'),
        '__nyx',
        '__depot'
    )
WHERE composition_json LIKE '%data-nyx-%' OR composition_json LIKE '%__nyx%';
