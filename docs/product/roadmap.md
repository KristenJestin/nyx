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
- acter le workspace comme point de rattachement du travail, **travail libre possible sans Spec** ; le lien Spec→runtime (`specs.workspace_id`) et la table `specs` sont **réservés ici mais réalisés en v2.5** (v2.1 ne construit pas le domaine Spec) ;
- mettre en place les use cases communs UI/MCP ;
- établir le flux d'événements temps réel des **mutations métier** (`domain_events` poussés, pas de polling) ; les **vues dérivées de Git** (diff/dirty/branche) sont **différées en v2.4** (Q-015) ;
- définir la stratégie de stockage (**un seul fichier SQLite**, PDR-0024) et de migrations de la V2 (chaîne unique, reprise des données v1).

**Gate :** le modèle relie clairement intention, dossiers, runtime et Git sans autorité Depot parallèle.

## v2.2 - Workbench de travail actif

- workspace de travail (worktree per-Spec) comme unité, validé en v2.1 ;
- association Spec/workspace/repositories/branches/sessions ;
- écran de travail actif ;
- terminaux, agents et services contextualisés ;
- lien entre tâches, sessions, commandes et changements ;
- Attention Inbox ;
- timeline utile et bornée ;
- reprise complète du contexte après redémarrage ;
- travail libre possible sans Spec.

**Gate :** exécuter et suivre une feature réelle sans autre outil d'exécution que nyx.

## v2.2.5 - Persistance terminal (livré)

**Status: livré**

- bascule **xterm → wterm** (`@wterm/dom` + core **ghostty**, full VT), une seule lib terminal (PDR-0026) ;
- **store d'instances no-host** (Map module-level hors React et hors cycle routeur) : detach/reattach du nœud DOM au fil de la navigation, **navigation lossless** (buffer, alt-screen, focus intacts) ;
- **modèle d'écran VT headless** dans le sidecar + **re-hydratation au restart** : **restart lossless** (le TUI, hors scrollback, est restauré) ;
- fondation **routeur** du shell de navigation multi-pages (PDR-0025) sur laquelle se branche le store terminal.

**Gate :** naviguer et redémarrer l'app sans perdre le contexte terminal (buffer + TUI en alt-screen), sans host de terminaux persistant.

## v2.3 - Terminal en blocs (honor-the-shell)

**Status: en cours** — PRD `01KWY7NFQJN068Q5ZSAJSSK2V2`

- **sortie shell en blocs** commande+résultat via **OSC 133** (bornes A/B/C/D, code de sortie) ;
- **honor-the-shell** : le shell garde l'édition (Tab, Ctrl-R, historique, sudo, vim natifs) — nyx n'intercepte pas la saisie ;
- **historique en blocs HTML statiques** (ANSI → style) ;
- **région active + TUI** rendus via la grille **wterm** ;
- **vrai prompt rejoué** (jamais synthétisé) ;
- **bascule blocs ↔ grille plein écran** sur alt-screen ;
- réutilise le **socle terminal v2.2.5** (wterm/ghostty, store no-host, routeur).

**Gate :** travailler au shell en blocs (badges de statut, helpers natifs, TUI plein écran) sur **bash + starship**.

## v2.4 - Diff et review locale

- moteur Git multi-repo ;
- **rafraîchissement temps réel des vues dérivées de Git** (diff/dirty/branche) : watch fs/git ou re-read sur déclencheur — tranche **Q-015** (différée depuis v2.1) ;
- working tree, staged, unstaged, untracked, commit, range et branches ;
- gros diffs virtualisés ;
- review générique, Spec facultatif ; review = vue dérivée (pas de table), modèle comment/task, finding = task (cf. PDR-0012, PDR-0015) ;
- commentaires globaux, fichiers, lignes et plages ;
- threads humain/agent ;
- réponses agent via MCP ;
- remapping des ancres et état outdated ;
- pièces jointes utiles ;
- résolution réservée à l'humain ;
- compteurs non lus et temps réel UI/MCP.

**Gate :** reviewer un gros changement local multi-repo sans VS Code ni forge distante.

## v2.5 - Specs : tâches et contextes agents

- absorber Spec et révisions ;
- absorber tâches, dépendances, actions humaines et findings de review ;
- porter lifecycle, validations et garde-fous au backend nyx choisi ;
- relier Spec/tâches au workspace de dev et aux reviews (**réalise `specs.workspace_id`**, le seam réservé en v2.1) ;
- fournir l'interface `Specs` native nyx ;
- exposer les outils MCP nyx d'authoring et de transition ;
- générer les contextes agents depuis l'état nyx ;
- actualiser immédiatement l'UI après toute mutation MCP.

Ne pas porter : shell web Depot, daemon, serveur Hono, CLI agent, projets/workspaces dupliqués ou présentation historique devenue inutile.

**Gate :** le workflow complet Spec -> tâches -> workspace de dev -> review -> done fonctionne dans nyx sans appeler Depot.

## v2.6 - Knowledge et mémoire projet

- idées et promotion vers Spec ;
- ADR ;
- directives projet ;
- documents et annexes ;
- décisions et contexte durable ;
- activité Depot utile convertie en timeline contextuelle ;
- recherche et navigation transversales ;
- progressive disclosure pour ne pas encombrer le terminal et le travail actif.

**Gate :** les informations durables nécessaires aux agents et à l'utilisateur sont consultables et modifiables dans nyx, sans second système de mémoire.

## v2.7 - Capacités avancées de la V2

Après validation des usages de v2.0 à v2.6 :

- milestones, catégories et graphes de dépendances avancés ;
- prototypes comme annexe du Spec (un seul par Spec ; rounds conservés comme rollback ; forme par revue d'usage, cf. PDR-0009) ;
- design-system imposé + rendu proto (Handlebars sidecar, Tailwind iframe, sync md5 ; cf. PDR-0010) ;
- catalogues de composants ;
- orchestration multi-agent via playbook éditable + verbes observables, pas un moteur (cf. PDR-0011) ;
- automatisations ;
- diagnostics et outils de réparation ;
- gestion du cycle de vie / ressources des sessions agent : décharger (unload) les sessions Claude idle non visitées depuis un moment, resume au retour, et resume **paresseux** au démarrage (ne rien reprendre tant que le terminal n'a pas été ouvert) ; crux = détecter l'activité réelle d'un agent (busy/exec_state, agent_session_event, inactivité I/O) — à valider par un spike ; cf. idée depot `01KX0X3ZZQMEPKBQX30SX47HJF` ;
- import/export et sauvegardes de longue durée.

Chaque sous-système Depot doit recevoir une décision explicite : **porter, simplifier, remplacer ou supprimer**. Il n'est pas copié par défaut.

**Gate :** chaque capacité avancée démontre un bénéfice sur la boucle travail -> agent -> review, sans transformer nyx en IDE ou gestionnaire de projet générique.

## v2.8 - Migration finale et extinction de Depot

- importer de façon idempotente projets liés, Spec, révisions, tâches, reviews, idées, ADR, directives, documents et pièces jointes retenues ;
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
-> v2.2.5 persistance terminal (livré)
-> v2.3 terminal en blocs (honor-the-shell)
-> v2.4 diff/review
-> v2.5 Spec/tâches/contextes MCP
-> v2.6 knowledge
-> v2.7 capacités avancées retenues
-> v2.8 migration et suppression Depot
```

## Règle de contrôle du scope

Une tranche `v2.x` ne doit pas aspirer les suivantes. En particulier :

- v2.1 ne construit pas toute l'UI Spec ;
- v2.2 ne réimplémente pas la review ;
- v2.4 ne copie pas le lifecycle Depot ;
- v2.5 ne copie pas prototypes et documents ;
- v2.7 ne devient pas un prétexte pour porter tout le legacy.

La roadmap peut évoluer, mais **tout ce qui a été retenu pour la super-app appartient à la V2**, jamais à une hypothétique V3 utilisée comme parking.

## Terminal « blocks » — chantier PARKÉ (hors séquence v2.x)

> **Note de version.** Ce chantier a été mené sous un PRD intitulé « v2.3 — Terminal blocks »,
> mais la tranche roadmap **v2.3 ci-dessus reste « Diff et review locale »**. Le terminal blocks
> est une amélioration UX du terminal (suite de v2.2.5), **à re-cadrer** dans la séquence (probable
> v2.2.x). Ne pas confondre les deux « v2.3 ».

Modèle « blocks » façon Warp, **honor-the-shell** : sortie shell (hors TUI) segmentée en blocs
commande+résultat via **OSC 133**, historique rendu en blocs DOM, région active + TUI sur la grille
wterm patchée (v2.2.5).

- **Branche :** `feature/v2.3-terminal-blocks` — commit `255cdf1` (WIP, **non mergé**).
- **PRD depot :** `01KWY7NFQJN068Q5ZSAJSSK2V2` · prototype validé : variante **« sobre »** élue.

### Fait — validé en direct sur l'app réelle (pilotage CDP, bash+starship)

- Segmentation OSC 133 → **blocs** : vrai prompt starship rejoué + commande + sortie + **code de sortie** (badges ✓/✗).
- **Cause racine corrigée :** le `PROMPT_COMMAND` en **tableau bash 5.x** (omarchy+direnv+zoxide+starship)
  faisait sauter le marqueur **OSC 133 `B`** → aucun bloc ne se formait. Fix : enregistrement *array-aware*
  du hook (le `B` survit à la réécriture de PS1 par starship) + filet (bloc formé même si `B` manque).
- Détection alt-screen (DECSET 1049/1047/47) + bascule blocs↔grille TUI plein écran.
- Persistance des blocs (fichier JSON sidecar, **sans migration**) ; non-régression persistance v2.2.5 verte.
- Renderer : liste de blocs (ANSI→tokens **design-system**), input épinglé bas, interactions (sélection/copie/repli).
- Wire Rust↔TS `PtyBlock`/`PtyAltScreen` + fixtures de contrat byte-parity ; harness e2e Electron (bash+starship, CDP).

### Reste à faire — défauts connus au parking

1. **[bloquant visuel] La grille wterm duplique le scrollback complet SOUS les blocs.** Il faut **clipper la
   grille à la seule région active** (prompt courant + sortie en cours) ; l'historique doit être **uniquement**
   des blocs. C'est ce qui donne l'impression de « wterm à plat comme avant ».
2. Conformité fine au **placement validé** (maquette « sobre » élue) — reproduire le placement via le
   **design system**, jamais le CSS/px du mockup. Le placement n'a **jamais été distillé** (`depot prd prototype distill`) — à faire avant reprise.
3. **Vérifier la saisie réelle** : un drop de caractères a été observé mais **confondu par l'injection CDP de
   test** (frappes agent + utilisateur mélangées dans le même terminal). À re-tester sans injection.
4. Sous-shell top-level `( … )` : header de commande corrompu (le fix propre `set -o functrace` **casse
   starship**) — parké en idée de durcissement.

### Leçon (à ne pas réoublier)

Toute validation « blocs » **doit** passer par le **pilotage réel de l'app sur bash+starship réel (CDP)**,
jamais un shell hermétique : les tests hermétiques émettaient un `B` que le vrai starship efface, ce qui a
masqué pendant des heures que le produit ne marchait pas (gate e2e « vert » sur un faux positif).
