-- Revert schema v30 (the DESIGN-SYSTEM domain). Drop `design_components` then `design`
-- (dropping a table drops its indexes). Neither has dependents, so plain drops suffice.
DROP TABLE IF EXISTS design_components;
DROP TABLE IF EXISTS design;
