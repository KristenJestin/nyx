# Roadmap produit nyx V2

## État

**Status: proposed sequencing, no dates committed**

## Convention de version

**V2 désigne la totalité de la super-app cible décrite par la vision produit.** Les versions `v2.0`, `v2.1`, `v2.2`, etc. sont des tranches livrables de cette même V2 ; elles ne représentent pas des produits ou orientations séparés.

La V2 est complète uniquement lorsque les capacités utiles de Depot sont natives dans nyx, les données ont été migrées et Depot peut être retiré.

## v2.0 - Consolidation de la plateforme Electron/Rust

- terminer la migration Electron ;
- retirer Tauri du build, des dépendances et de la maintenance active ;
- regrouper le renderer React dans l'application desktop Electron ;
- conserver toute la logique métier et les autorités de données dans `nyx-core` en Rust ;
- réduire Electron main/preload à la fenêtre, au lifecycle desktop et au transport IPC sécurisé ;
- remplacer le `core-host` Node et N-API par un exécutable sidecar Rust supervisé ;
- stabiliser le protocole V1 MessagePack, versionné et encadré sur pipes entre Electron main et le sidecar ;
- utiliser des frames binaires bornées avec backpressure pour les flux PTY ;
- supprimer le candidat Node du POC, ses switches et toute double implémentation ;
- déplacer dans Rust la logique métier/runtime encore présente dans le `core-host` TypeScript ;
- simplifier le monorepo autour de `apps/desktop`, `crates/nyx-core`, `crates/nyx-sidecar` et `e2e` ;
- produire et embarquer le sidecar natif Windows, Linux et macOS hors `app.asar` ;
- valider packaging, handshake, lifecycle, crash recovery, PTY, SQLite et performances sur les OS cibles.

**Gate :** Electron est l'unique shell livré, Rust l'unique backend métier, le sidecar et tous ses descendants meurent avec l'application, Tauri/N-API/core-host ne sont plus compilés et aucune logique métier concurrente ne subsiste dans Node/TypeScript.

## v2.1 - Noyau produit unifié

- figer le modèle Project/Workspace/Repository ;
- faire de nyx l'autorité unique de ces identités ;
- acter le workspace comme pivot intention/runtime (`Execution` rejetée, PDR-0008) ;
- relier PRD/Task au runtime via `prds.workspace_id`, sans rendre le PRD obligatoire ;
- mettre en place les use cases communs UI/MCP ;
- établir le flux d'événements temps réel ;
- définir la stratégie de stockage et de migrations de la V2.

**Gate :** le modèle relie clairement intention, dossiers, runtime et Git sans autorité Depot parallèle.

## v2.2 - Workbench de travail actif

- workspace de travail (worktree per-PRD) comme unité, validé en v2.1 ;
- association PRD/workspace/repositories/branches/sessions ;
- écran de travail actif ;
- terminaux, agents et services contextualisés ;
- lien entre tâches, sessions, commandes et changements ;
- Attention Inbox ;
- timeline utile et bornée ;
- reprise complète du contexte après redémarrage ;
- travail libre possible sans PRD.

**Gate :** exécuter et suivre une feature réelle sans autre outil d'exécution que nyx.

## v2.3 - Diff et review locale

- moteur Git multi-repo ;
- working tree, staged, unstaged, untracked, commit, range et branches ;
- gros diffs virtualisés ;
- review générique, PRD facultatif ; modèle review/comment/task, finding = task (cf. PDR-0012) ;
- commentaires globaux, fichiers, lignes et plages ;
- threads humain/agent ;
- réponses agent via MCP ;
- remapping des ancres et état outdated ;
- pièces jointes utiles ;
- résolution réservée à l'humain ;
- compteurs non lus et temps réel UI/MCP.

**Gate :** reviewer un gros changement local multi-repo sans VS Code ni forge distante.

## v2.4 - Work : PRD, tâches et contextes agents

- absorber PRD et révisions ;
- absorber tâches, dépendances, actions humaines et findings de review ;
- porter lifecycle, validations et garde-fous au backend nyx choisi ;
- relier PRD/tâches au workspace de dev et aux reviews ;
- fournir l'interface `Work` native nyx ;
- exposer les outils MCP nyx d'authoring et de transition ;
- générer les contextes agents depuis l'état nyx ;
- actualiser immédiatement l'UI après toute mutation MCP.

Ne pas porter : shell web Depot, daemon, serveur Hono, CLI agent, projets/workspaces dupliqués ou présentation historique devenue inutile.

**Gate :** le workflow complet PRD -> tâches -> workspace de dev -> review -> done fonctionne dans nyx sans appeler Depot.

## v2.5 - Knowledge et mémoire projet

- idées et promotion vers Work/PRD ;
- ADR ;
- directives projet ;
- documents et annexes ;
- décisions et contexte durable ;
- activité Depot utile convertie en timeline contextuelle ;
- recherche et navigation transversales ;
- progressive disclosure pour ne pas encombrer le terminal et le travail actif.

**Gate :** les informations durables nécessaires aux agents et à l'utilisateur sont consultables et modifiables dans nyx, sans second système de mémoire.

## v2.6 - Capacités avancées de la V2

Après validation des usages de v2.0 à v2.5 :

- milestones, tags et graphes de dépendances avancés ;
- prototypes comme annexe du PRD (un seul par PRD ; rounds conservés comme rollback ; forme par revue d'usage, cf. PDR-0009) ;
- design-system imposé + rendu proto (Handlebars sidecar, Tailwind iframe, sync md5 ; cf. PDR-0010) ;
- catalogues de composants ;
- orchestration multi-agent via playbook éditable + verbes observables, pas un moteur (cf. PDR-0011) ;
- automatisations ;
- diagnostics et outils de réparation ;
- import/export et sauvegardes de longue durée.

Chaque sous-système Depot doit recevoir une décision explicite : **porter, simplifier, remplacer ou supprimer**. Il n'est pas copié par défaut.

**Gate :** chaque capacité avancée démontre un bénéfice sur la boucle travail -> agent -> review, sans transformer nyx en IDE ou gestionnaire de projet générique.

## v2.7 - Migration finale et extinction de Depot

- importer de façon idempotente projets liés, PRD, révisions, tâches, reviews, idées, ADR, directives, documents et pièces jointes retenues ;
- gérer les chemins historiques WSL/Windows et les identités déjà présentes dans nyx ;
- produire un rapport de migration et un backup restaurable ;
- vérifier la parité des contextes agents ;
- dogfood prolongé sans retour nécessaire vers Depot ;
- retirer le binaire, daemon, Hono, UI, CLI, MCP et la configuration Depot ;
- archiver le repository Depot en lecture seule.

**Gate final V2 :** nyx couvre les workflows retenus, les données sont migrées, Depot n'est plus requis et il n'existe qu'une seule application et une seule autorité métier.

## Ordre strict

```text
v2.0 consolidation Electron/Rust
-> v2.1 modèle produit
-> v2.2 travail actif (workspace) + attention
-> v2.3 diff/review
-> v2.4 PRD/tâches/contextes MCP
-> v2.5 knowledge
-> v2.6 capacités avancées retenues
-> v2.7 migration et suppression Depot
```

## Règle de contrôle du scope

Une tranche `v2.x` ne doit pas aspirer les suivantes. En particulier :

- v2.1 ne construit pas toute l'UI PRD ;
- v2.2 ne réimplémente pas la review ;
- v2.3 ne copie pas le lifecycle Depot ;
- v2.4 ne copie pas prototypes et documents ;
- v2.6 ne devient pas un prétexte pour porter tout le legacy.

La roadmap peut évoluer, mais **tout ce qui a été retenu pour la super-app appartient à la V2**, jamais à une hypothétique V3 utilisée comme parking.
