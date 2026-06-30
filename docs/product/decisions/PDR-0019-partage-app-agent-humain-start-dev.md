# PDR-0019 - Partage app / agent / humain ; « Start dev » générique + points d'extension

**Status: validated** (philosophie + squelette + points d'extension) ; le **mécanisme de hooks** est *proposed* (Q-014).

## Context

La question fondatrice : qu'est-ce que **nyx** fait, qu'est-ce que l'**agent** fait, qu'est-ce que l'**humain** décide ? Ancrée sur le concret : une commande maison de préparation de feature (worktrees multi-repo, auto-inclusion yalc des libs, `.envrc`, `setup-feature.sh`, tabs Warp, injection `/depot-dev`). Le tri de ses étapes a fait émerger la découpe.

## Decision

**Trois couches.**

1. **Verbe nyx = exécution + autorité de l'état.** nyx exécute et fait foi sur son état : créer le worktree, la ligne workspace, le status, le diff, lancer terminaux / managed commands / sessions agents. nyx n'est **pas** un moteur de workflow (PDR-0011).
2. **Les inputs d'un verbe viennent de l'humain (UI) ou de l'agent (MCP)** — même use case des deux côtés (PDR-0005). Ex. « quels repos worktreer, inclure la racine, quelle branche » est un **jugement** porté par l'humain (options) ou l'agent (raisonnement) ; le verbe, lui, reste nyx.
3. **Le travail (jugement/créatif) = session agent observable**, lancée par un verbe : décomposer, coder, auditer, doc-sync. Visible parce que nyx tient le PTY — c'est la plus-value vs un CLI aveugle.

**Le verbe « créer le workspace de dev »** : crée le container (`workspace.root_path`, non-repo possible — « faux worktree multi-repo »), y worktree les repos **sélectionnés** (défaut = repos non-`optional`, PDR-0013 ; gère **racine incluse** et **exclusions**), pose `specs.workspace_id` + status→`in_progress`.

**« Démarrer le dev » = squelette générique** : verbe worktree (repo-set) → ligne workspace + status → managed commands → session agent + playbook → setup hooks. Tout observé.

**Quatre points d'extension projet** (rendent le squelette générique-mais-souple — rejouent `feature-start`) :

- **repo-set rule** — calcule/ajuste les repos à worktreer (auto-inclusion des libs partagées du monorepo) ;
- **setup hook** post-worktree — commande projet lancée dans le nouveau workspace (ex. `.envrc` + `setup-feature.sh`) ;
- **dev playbook** — instructions injectées à la session agent (ex. `/depot-dev`, PDR-0011) ;
- **managed commands / services** — api/front/relay.

Ce qui, aujourd'hui, est câblé à la main — **tabs de terminal, enregistrement du workspace, fichier de config maison** — **disparaît** : natif et observé dans nyx. C'est la plus-value.

## Proposed / à revoir (Q-014)

Le **mécanisme de stockage des hooks** (repo-set, setup, + hooks d'audit type `pre-coder-spawn` / `post-auditor-pass`) : hypothèse de travail = enrichir la table `directives` (`kind` context/command/guard/check + `scope` lifecycle), reprise des `project_directives` de Depot. **Non revalidé depuis l'absorption** — à reprendre (Q-014) : un concept unique (`directives` enrichie) vs table `hooks` séparée, et le vocabulaire des scopes.

## Consequences

- `data-model.dbml` : `directives` enrichie (proposé, Q-014) ; le verbe worktree documenté.
- Cadre tous les boutons du détail Spec (le tableau status→actions se finalise avec le filtre verbe / inputs / travail).
- Q-014 ouverte sur le mécanisme de hooks.
