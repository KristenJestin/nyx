# PDR-0003 - Execution comme lien entre intention et runtime

**Status: superseded par PDR-0008**

> Cette proposition a été rejetée après validation par workflow et analyse du flux réel. Le workspace assure le lien entre intention et runtime ; `Execution` ne possédait rien en propre et dupliquait le cycle de vie du worktree. Voir PDR-0008. Le texte ci-dessous est conservé pour mémoire.

## Context

Depot sait pourquoi un travail existe mais ne voit pas son runtime. nyx voit terminaux, services et sessions mais ne connaît pas toujours leur objectif.

## Proposed decision

Introduire `Execution`, qui lie un objectif facultatif à un workspace, des branches, des sessions agents, terminaux, services, changements Git et reviews.

## Expected benefits

- vue unique d'une feature en cours ;
- contexte agent non ambigu ;
- traçabilité tâche -> session -> changements -> review ;
- reprise cohérente après redémarrage ;
- support du travail libre sans PRD.

## Risks

- duplication avec workspace, run ou PRD ;
- lifecycle artificiel ;
- ownership confus des terminaux et services ;
- complexité visible trop tôt.

## Validation required

Prototyper les workflows travail libre, PRD, multi-repo, reprise et deux executions concurrentes avant de figer le schema.
