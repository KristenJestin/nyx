# Architecture de l'information

## État

**Status: partiellement validé — shell de navigation posé (PDR-0025, PDR-0026), structure projet riche encore proposed**

Le **shell multi-pages** est tranché et livré : rail de projets à gauche + section-nav + `<Outlet/>` (PDR-0025), terminaux montés en permanence via le store no-host (PDR-0026). La section-nav est **dérivée du scope** : six entrées dans un projet — **Home** (`/home`), **Terminals** (`/`), **Work** (`/work`), **Ideas** (`/knowledge`), **Engine** (`/engine`), **Design** (`/design`) — et **trois** en scope transverse (« All projects ») : Home, Work, Ideas. Les trois autres n'ont pas de lecture transverse qui ait un sens et sont **absentes** là-bas, jamais grisées. Les libellés `Ideas` et `Design` nomment des routes internes inchangées (`/knowledge`, `/design`) : voir la correspondance au glossaire.

**Écart à réconcilier :** la structure projet **plus riche** décrite ci-dessous (Overview / Specs / Runtime / Knowledge) reste **proposed**. Le shell livré la simplifie (Terminals = Runtime, Work = Specs+Overview) ; « Specs », « Runtime » et les onglets de détail Spec seront branchés à leur tranche (v2.4). Ce document décrit la cible ; le shell v2.2.5 en est la première itération, pas la forme finale.

## Navigation globale proposée

```text
Global
 |- Projects
 |- Attention Inbox
 `- Settings
```

La sélection d'un projet ouvre son espace sans perdre les terminaux actifs.

## Navigation projet proposée

```text
Overview
Specs
Runtime
Knowledge
```

### Overview

- travail et workspaces actifs ;
- attention requise ;
- état des services ;
- activité récente utile ;
- raccourcis de reprise.

### Specs

Board de **Spec** (l'unité de travail de l'humain, PDR-0014) — pas un board de tâches. Colonnes par `spec_status`. Classification = **catégorie unique** par Spec (PDR-0017) ; pas de « sprint » ni d'« issue ».

- Spec (carte = clé Linear, statut, priorité, catégorie, milestone, progression) ;
- tâches : consultées **dans** le Spec, gérées par l'agent, non administrées à la main ;
- idées ;
- milestones et dépendances en affichage avancé.

Le détail d'une Spec met en avant **une action principale selon son `status`** (ex. `ready` → « Démarrer le dev »). « Démarrer le dev » = **verbe nyx** (worktree + workspace + status) + **lancement** d'une session agent (playbook) + **hooks projet** — pas un moteur (PDR-0019). Le tableau complet status → actions reste à finaliser.

### Runtime

- workspaces (primary + worktrees) et repositories ;
- Spec courant du workspace ;
- terminaux ;
- sessions agents ;
- commandes managées.

### Review (onglet conditionnel + écran plein écran)

La review n'est **pas une section de navigation** ; elle a **deux surfaces**, selon la profondeur :

- **Onglet Review sur la Spec** — apparaît **uniquement une fois le dev lancé**, c'est-à-dire à partir du `status` `in_progress` (puis `blocked` / `in_review` / `completed`). Il affiche les **commentaires de la Spec courante** (threads, open/resolved/outdated, par repo) + un **bouton « Ouvrir la review »** vers l'écran plein écran. En `draft` / `ready` : pas d'onglet. La règle se lit sur le **`status`**, pas sur la présence d'un workspace : une Spec `ready` **peut** déjà être ancrée sur un workspace (l'ancre est posable dès `ready` via `workspace_add` / `create_workspace`, sans transition) sans que la review s'ouvre pour autant.
- **Écran review plein écran** — la surface profonde (diff complet + commentaires + findings), lancée depuis l'onglet ou le header.

Le **signal** (review prête, commentaires non lus, findings) vit dans **Overview / Attention Inbox** → ouvre la review de la Spec concernée. Travail libre sans Spec : même review depuis le workspace.

### Knowledge

- ADR ;
- directives ;
- documents et annexes ;
- contexte durable du projet.

## Détail d'une Spec : onglets et action selon le `status`

Le détail d'une Spec met en avant **une action principale** = la prochaine étape (vision). Les onglets s'adaptent au `status`. (`in_progress` s'affiche « In progress ».)

| `status` | Action principale | Onglets |
| --- | --- | --- |
| draft | Passer en Ready | Spec, Annexes, Activity |
| ready | Démarrer le dev | Spec, Annexes, Activity |
| in_progress | Ouvrir la review | Spec, Tasks, Runtime, Review, Annexes, Activity |
| blocked | Lever le blocage | Spec, Tasks, Runtime, Review, Annexes, Activity (+ bandeau blocage) |
| in_review | Review & décider (Approuver / Renvoyer) | Spec, Tasks, Runtime, Review, Annexes, Activity |
| completed | Archiver | Spec, Tasks, Review, Annexes, Activity (plus de Runtime — worktree nettoyé) |
| cancelled | Rouvrir | Spec, Annexes, Activity |
| archived | — (désarchiver) | Spec, Annexes, Activity |

- **Spec** : l'**agent** l'écrit (grill) ; l'humain lit / valide. Non éditable par l'humain dans cette version.
- **Annexes** et **Activity** : présents dans tous les status.
- **Tasks / Runtime / Review** : seulement à partir de `in_progress` (→). Règle de **`status`** et non de présence d'un workspace : une Spec `ready` peut déjà porter son workspace (ancre posable dès `ready`, cf. plus haut) sans afficher ces onglets — c'est le lancement du dev qui les ouvre.
- **Tasks** : décomposition de l'agent (lecture). Les **human tasks** (`executor=human`) y apparaissent avec un badge « toi » et sont **actionnables** (tu les marques `done`) — seule exception à la lecture seule ; l'agent les annonce aussi en chat, et une task agent bloquée affiche « bloquée par [human task] ». L'**Attention Inbox** (roll-up transverse de tous les signaux) est **différée** et non requise (Q-007 / PDR-0021).
- **Runtime** = ce qui tourne pour la Spec dans son worktree : terminaux + session(s) agent (dev + subagents) + managed commands/services (l'arbre terminal/workspace). Disparaît en `completed` (worktree nettoyé au merge).
- **Review** = onglet (commentaires de la Spec + bouton → écran plein écran), dominant en `in_review`, historique en `completed`/`cancelled`.
- Transitions : `ready→in_progress` = humain (« Démarrer le dev ») ; `in_progress→in_review` = l'agent (demande la review) ; `in_review→completed` = humain (« Approuver » = merge + doc-sync) ou « Renvoyer » (retour `in_progress` + findings).

## Écran central proposé : Workspace

```text
Header : Spec / task / workspace / branches / état

Main
 |- plan et progression
 |- terminaux et agent actif
 |- services
 |- changements Git
 |- diff/review
 `- timeline
```

L'objectif est d'éviter de naviguer entre cinq modules pour suivre une feature. Le workspace est l'écran qui agrège son propre runtime ; il n'y a pas d'entité Execution distincte.

## Progressive disclosure

Par défaut, masquer :

- graphes complexes de dépendances ;
- design rounds et variantes de prototypes ;
- réglages détaillés des contextes agents ;
- taxonomies et métadonnées rarement modifiées ;
- historique exhaustif.

L'agent peut utiliser la structure complète via MCP sans imposer cette complexité à l'utilisateur.

## Points à prototyper

1. ~~Rail global projets versus sidebar projet.~~ **Tranché** : rail de projets (PDR-0025).
2. ~~Place permanente ou contextuelle des terminaux.~~ **Tranché** : montés en permanence, store no-host, survivent à la navigation (PDR-0026).
3. Workspace comme page, tab, split ou mode principal de travail. *(ouvert, Q-006)*
4. Passage terminal -> diff -> commentaire sans perte de contexte. *(partiellement : la navigation est lossless PDR-0026 ; le pont diff/commentaire arrive en v2.5)*
5. Inbox globale versus compteurs locaux. *(ouvert, Q-007 — Attention Inbox différée)*
6. Navigation d'un faux workspace multi-repo. *(ouvert)*
7. Comportement quand aucun Spec n'est associé à une session. *(ouvert — travail libre, W-001)*
