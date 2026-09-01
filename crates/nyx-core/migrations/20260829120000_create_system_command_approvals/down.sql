-- Revert `create_system_command_approvals` (20260829120000) : retirer le ledger d'approbation.
--
-- Les deux index sont dropés AVANT la table par symétrie avec les autres `down` du dépôt (SQLite les
-- supprimerait avec la table, mais l'ordre explicite garde le fichier lisible comme l'inverse exact
-- du `up`).
--
-- DESTRUCTIF, et c'est le seul choix correct ici. Une demande d'approbation n'est pas du travail de
-- l'utilisateur à préserver : c'est un jeton de sécurité à durée de vie de cinq minutes. La garder
-- « au cas où » à travers un aller-retour de schéma reviendrait à conserver des autorisations dont
-- plus aucun code ne vérifie l'expiration. Redescendre efface donc les demandes ; remonter repart
-- d'un ledger vide, et l'agent redemande.

DROP INDEX IF EXISTS idx_system_approvals_state_expiry;
DROP INDEX IF EXISTS idx_system_approvals_pending_fingerprint;
DROP TABLE system_command_approvals;
