-- Revert schema v27 (ANNEXES). Drop the attachment table first (it FK-references the blob
-- store), then the content-addressed store itself.
DROP TABLE IF EXISTS annexes;
DROP TABLE IF EXISTS annex_blobs;
