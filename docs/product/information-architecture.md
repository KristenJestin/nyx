# Architecture de l'information

## État

**Status: proposed - prototype required**

Cette structure organise les concepts sans prétendre que la navigation est validée. Elle doit être testée avec un prototype sur des projets mono-workspace et multi-repo.

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
Work
Runtime
Review
Knowledge
```

### Overview

- travail et workspaces actifs ;
- attention requise ;
- état des services ;
- activité récente utile ;
- raccourcis de reprise.

### Work

- PRD ;
- tâches ;
- idées ;
- milestones et dépendances en affichage avancé.

### Runtime

- workspaces (primary + worktrees) et repositories ;
- PRD courant du workspace ;
- terminaux ;
- sessions agents ;
- commandes managées.

### Review

- changements non reviewés ;
- reviews ouvertes ;
- commentaires et réponses ;
- historique résolu/outdated.

### Knowledge

- ADR ;
- directives ;
- documents et annexes ;
- contexte durable du projet.

## Écran central proposé : Workspace

```text
Header : PRD / task / workspace / branches / état

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

1. Rail global projets versus sidebar projet.
2. Place permanente ou contextuelle des terminaux.
3. Workspace comme page, tab, split ou mode principal de travail.
4. Passage terminal -> diff -> commentaire sans perte de contexte.
5. Inbox globale versus compteurs locaux.
6. Navigation d'un faux workspace multi-repo.
7. Comportement quand aucun PRD n'est associé à une session.
