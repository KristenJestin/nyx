-- Revert schema v28 (DÉPENDANCES Spec↔Spec). Drop the directed dependency graph table.
DROP TABLE IF EXISTS spec_depends_on;
