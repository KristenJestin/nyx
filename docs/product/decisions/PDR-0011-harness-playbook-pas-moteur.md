# PDR-0011 - Harness agent : playbook éditable, pas un moteur

**Status: validated (direction)** — primitives en v1, harness en v2.6.

## Context

L'utilisateur veut un workflow agentique personnalisable par projet : en développement, l'agent est un orchestrateur (lance un codeur, des auditeurs, reboucle sur les findings, découpe en phases, puis déclenche la doc au close). C'est l'évolution des `directives` de Depot (tags de contexte par projet).

Le risque : construire un **moteur de workflow configurable générique** (rôles, phases, transitions, agents custom, hooks). C'est le plus gros risque de cathédrale identifié dans les sessions de conception — un moteur d'orchestration générique est sans fin et se bâtirait avant même la validation de la boucle quotidienne.

## Decision

nyx **ne construit pas** de moteur d'orchestration.

- Le workflow est un **playbook éditable par projet** : du texte/directive (« directives v2 ») injecté comme instructions à l'agent orchestrateur. nyx fournit un **playbook par défaut** (le harness « qu'on estime le meilleur »).
- L'agent **exécute** le playbook en appelant les **primitives MCP observables** de nyx (les verbes) : lancer/observer une session avec un rôle, injecter du contexte, hooks de cycle de vie (start/close). nyx fournit les verbes et **regarde** ; il n'interprète aucun workflow.
- Chaque sous-agent (codeur, auditeur) lancé via les verbes est une **vraie session nyx observable** → aucun angle mort. C'est la raison décisive du choix playbook-plutôt-que-moteur : un moteur cacherait l'orchestration dans le backend ; le playbook garde chaque étape visible comme session.

**Frontière (le juste milieu) :**

- **nyx-core (générique, petit, primitives v1)** : lancer/observer une session avec un rôle, injecter du contexte, hooks start/close.
- **projet (éditable, v2.6)** : le playbook lui-même (orchestrateur → codeur → auditeurs → doc), avec un défaut nyx.

En une phrase : **nyx possède les verbes, le projet possède le playbook.**

## Consequences

- aucun moteur d'orchestration à construire ni maintenir ;
- la boucle multi-agent reste lisible : chaque codeur/auditeur est une session visible ;
- les `directives` de Depot évoluent en playbooks de comportement, plus riches que des tags de contexte ;
- le déclenchement du doc-sync (close -> doc) n'est qu'une étape du playbook + un hook de cycle de vie ;
- les **verbes** (lancer/observer une session, injecter du contexte) sont des **primitives v1** du cockpit runtime ; le harness/éditeur de playbook est le **payoff v2.6** et ne peut pas précéder ses primitives ;
- le playbook par défaut lui-même est une conception séparée et révisable, à revoir.
