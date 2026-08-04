-- Revert schema v31 (the PROTOTYPE domain). Drop the tables in reverse FK order (a child
-- before its parent), so the CASCADE references never block a drop. Dropping a table drops its
-- indexes. Everything here is additive over v30, so plain drops fully reverse the migration.
DROP TABLE IF EXISTS prototype_design_lock;
DROP TABLE IF EXISTS prototype_round_pages;
DROP TABLE IF EXISTS prototype_rounds;
DROP TABLE IF EXISTS prototype_feedback;
DROP TABLE IF EXISTS prototype_variants;
DROP TABLE IF EXISTS prototype_page_versions;
DROP TABLE IF EXISTS prototype_pages;
DROP TABLE IF EXISTS prototypes;
