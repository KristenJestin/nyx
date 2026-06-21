# PDR-0012 - Review : modèle minimal (review / comment / task)

**Status: validated (modèle)** — le moteur de diff et l'UI sont spécifiés dans le PRD nyx « Viewer de gros diffs » (build v2.3).

## Context

Le domaine review avait accumulé des structures confuses (review niveau workspace + targets + threads + messages + findings). Les sessions de conception l'ont réduit à l'essentiel, en s'appuyant sur le flux réel de l'utilisateur et son PRD brouillon. Le vocabulaire est figé ici et ne doit plus bouger.

## Decision

**Trois concepts. Pas plus.**

**1. `review` — le viewer de diff.** Quand un agent a changé du code dans un repo, on ouvre un `review` : on voit le diff et on l'annote. Un `review` = le diff d'**un** repo + ce qui s'y rattache. Il y en a **un par repo** ; nyx les agrège dans une seule UI sans mélanger les identités Git. Deux variétés : **living** (suit une branche cible vs base) et **pinned** (figée sur un SHA). Générique : PRD facultatif (PDR-0004). Archivée à la fusion ou suppression de la branche cible.

**2. `comment` — l'annotation sur le diff.** Ancrée (fichier / côté / ligne ou plage + blob, snippet, contexte pour le re-mapping et l'état `outdated`) ou globale. Auteur **humain ou agent**. Threadée. **Toi seul résous un commentaire ; l'agent peut uniquement répondre** (contrôle humain, aucun angle mort).

**3. `task` — un commentaire promu en travail pour le coder.** Le coder ne travaille **que** sur des tâches, jamais sur des commentaires bruts. Un **finding = une task** (severité optionnelle). La promotion `comment -> task` est elle-même un **grill** : l'humain lâche brut, l'agent structure en tâche cohérente, l'humain valide. Même motif que la conception d'un PRD.

## Consequences

- **`finding = task`** → la table `review_findings` meurt.
- **review devient par repo** → `reviews` (workspace) + `review_targets` fusionnent en un seul `reviews` rattaché à `(workspace, repository)` (cf. PDR-0013 pour la mort de `workspace_repositories`).
- **threads + messages** → un seul `review_comments` (auto-threadé par `parent_id`).
- les « 2 types de review » se dissolvent en deux attributs déjà présents : l'**auteur** du commentaire (humain/agent) et l'**existence d'une promotion** (une task existe, ou pas = « envoyé au coder, ou pas »).
- le groupement cross-repo des tâches pour une passe coder (le `round`) est **différé** : utile seulement si le flux reboucle (coder -> audit -> re-coder). Ouvert tant que ce rebouclage n'est pas confirmé.
- le moteur de diff (gix + fallback `git` CLI), le re-mapping des ancres et l'UI sont spécifiés dans le PRD nyx « Viewer de gros diffs » (build v2.3) ; la note d'archi de ce PRD (napi-rs / core-host) est **périmée** par PDR-0007 (sidecar).
