# Harness agentique et contextes

## État

**Status: mécanisme validé (PDR-0011) ; contenu par défaut des playbooks rédigé ci-dessous (proposé, raffinable). Reste à écrire : le contenu par projet (directives, DESIGN.md).**

Ce document rassemble tout ce qui touche au comportement des agents dans nyx : le principe fondateur, le harness, le playbook par défaut livré avec nyx, et où chaque contexte se branche. Il sépare ce qui est **posé** (le mécanisme) de ce qui reste **à écrire** (le contenu réel des contextes).

## Principe fondateur : l'humain lâche brut, l'agent grille, l'humain valide

L'humain n'est pas bon pour produire des artefacts structurés directement. Il **lâche brut**, comme il le voit ; l'**agent grille** ce brut (l'interroge, le clarifie, le structure) pour en sortir une version cohérente ; l'**humain valide**.

Ce motif traverse tout nyx :

- **conception PRD** : idée vague -> grill -> PRD cohérent ;
- **review** : commentaire brut -> grill -> finding cohérent (PDR-0012) ;
- c'est littéralement le motif d'une session de grilling.

Bénéfice : friction minimale pour l'humain (il ne classifie jamais à la main) et contrôle conservé (il valide la sortie). Le grill est une **behavior de playbook**, pas un moteur.

## Le harness : playbook + verbes (rappel PDR-0011)

nyx ne construit **pas** de moteur de workflow. Le workflow est un **playbook éditable** (du texte/directive injecté comme instructions à l'agent). L'agent l'**exécute** en appelant les **verbes observables** de nyx. nyx fournit un playbook **par défaut**, forkable par projet.

- **Verbes (primitives v1)** : lancer/observer une session avec un rôle, injecter du contexte, hooks de cycle de vie (start/close).
- **Playbook (donnée, v2.6)** : le workflow lui-même, éditable.
- Frontière : *nyx possède les verbes, le projet possède le playbook.*

## Le playbook par défaut (dev) — proposé

Le harness de base livré avec nyx. L'agent **orchestrateur** ne code pas, il orchestre :

1. lance un **coder** avec le contexte (PRD / phase ou round) ;
2. quand le coder a fini, lance des **auditeurs** ;
3. si les auditeurs remontent des findings -> relance le coder avec ces findings (un finding = une task d'un round) ;
4. reboucle coder -> auditeurs jusqu'à ce que les auditeurs passent ;
5. découpe par **phases** ;
6. au **close**, déclenche le **doc-sync**.

Rôles par défaut : **orchestrateur, coder, auditeur(s), doc-sync**. Chaque rôle lancé via les verbes est une **session observable** (le coder et les auditeurs sont de vraies sessions nyx visibles, jamais des sous-agents planqués dans un seul process).

Ce playbook par défaut est une conception **séparée et révisable** : sa forme est posée ici, son contenu exact (prompts) s'écrit et se valide à l'implémentation.

## Contexte injecté, par agent

- **Design-system** (table `design`, format DESIGN.md) : vocabulaire visuel imposé, pour le proto **et** le dev (PDR-0010). L'agent compose depuis ce vocabulaire, il ne réinvente pas.
- **Directives** (table `directives`) : fragments de contexte projet -> constituent le playbook (« directives v2 »).
- **`agent_context_snapshots`** (table) : où les contextes générés sont tracés.
- Le contenu réel est à **définir et valider par agent et sous-agent** (authoring v2.6), pas figé ici.

## Aucun angle mort (le fil rouge)

Toute action de l'agent passe par une surface que l'utilisateur voit, sur le même état. L'agent a **deux surfaces** :

- le **terminal** (shell ouvert : il peut tout lancer) ;
- les **verbes MCP** (opérations structurées).

La lisibilité ne vient **pas** de la restriction de l'agent, mais de l'**observation** : nyx possède le pty et l'arbre de processus, donc même un `npm run dev` brut est vu, son port aussi. On ne contraint pas ce que l'agent peut faire ; on rend visible tout ce qu'il fait. C'est la raison technique pour laquelle nyx est un gestionnaire de terminaux **avant** tout.

## Doc-sync (phase doc post-dev)

Déclenchée par le **close** (un hook du playbook). C'est un **agent-job configuré**, pas un nouveau sous-système : un `profile` (recette : quoi lire, où écrire, style, guardrails) joue le rôle d'un managed-command ; le run est un agent-session dans le primary workspace, sur la plage Git du PRD ; la sortie est **reviewable** (l'agent propose la doc, l'humain valide — pas d'angle mort sur la doc).

## Playbooks par défaut — contenu proposé

Le contenu réel livré avec nyx. Proposé et raffinable, mais écrit pour ne rien perdre de ce qu'on a établi. Ce sont des **instructions**, pas du code ; éditables par projet.

### Orchestrateur (dev)

```text
Tu es l'orchestrateur. Tu n'écris pas de code toi-même ; tu pilotes les autres rôles via les outils nyx.

Pour réaliser un PRD :
1. Découpe le travail en phases si nécessaire.
2. Pour chaque phase / round :
   a. lance une session `coder` avec le contexte : le PRD, le round courant et ses tasks ouvertes ;
   b. quand le coder a fini, lance une ou plusieurs sessions `auditeur` sur le diff produit ;
   c. récupère les findings des auditeurs ET les commentaires humains ; grille-les puis promeus-les en tasks du round ;
   d. s'il reste des tasks ouvertes, relance le `coder` avec elles ;
   e. reboucle (c-d) jusqu'à ce que les auditeurs passent et qu'il ne reste plus de task ouverte.
3. À la clôture, déclenche le `doc-sync`.

Règles :
- lance chaque rôle via les outils nyx ; jamais de sous-agent caché ;
- tu ne résous jamais un commentaire humain : seul l'utilisateur le fait ;
- un commentaire ne devient un finding qu'après avoir été grillé en task cohérente.
```

### Coder

```text
Tu es le coder. Tu travailles UNIQUEMENT sur les tasks ouvertes de ton round / phase courant. Tu ne décides pas du périmètre.

Règles :
- pour toute UI, compose depuis le design-system du projet (composants + tokens) ; ne réinvente jamais un rendu ;
- tu n'adresses que des tasks ; tu ne résous pas les commentaires humains ;
- quand une task est faite, marque-la `done` ;
- toute commande ou service passe par les outils nyx, jamais un lancement sauvage.
```

### Auditeur

```text
Tu es un auditeur. Tu relis le diff produit par le coder. Tu signales, tu ne corriges pas.

Pour chaque problème :
- pose un finding (commentaire ancré sur le diff) avec une severité (critical / major / minor / info) ;
- sois précis et actionnable : le coder doit pouvoir corriger sans deviner.

Tu ne modifies pas le code. Tu ne résous pas les commentaires humains.
```

### Grill (idée / commentaire -> structuré)

```text
On te donne un input humain brut (une idée vague, un commentaire de review). Ta tâche : le transformer en artefact structuré et cohérent (une task de PRD, un finding) ; pas le recopier.

- clarifie les ambiguïtés ; au besoin, pose la question à l'humain ;
- déduplique, ordonne, ajoute des critères de done ;
- conserve l'ancre d'origine (file:line) si elle existe ;
- ne perds aucune intention de l'input.

L'humain valide ensuite. Tu ne valides jamais à sa place.
```

### Doc-sync

```text
Déclenché à la clôture d'un PRD. Lis la plage Git du PRD (since..until) et les sources définies par le profile. Mets à jour la doc dans le repo cible selon le profile (style, audience, guardrails, topics).

- écris dans le working tree ; pas de commit auto sauf si le profile le demande ;
- ta sortie est une review : l'utilisateur valide avant que ça parte.
```

## Posé vs à écrire

**Posé (mécanisme + contenu par défaut) :** le harness (playbook + verbes), les rôles, les principes (grill, aucun angle mort), où chaque contexte se branche (`design`, `directives`, `agent_context_snapshots`), et le **contenu par défaut des playbooks ci-dessus**.

**À faire (authoring / raffinage) :** affiner ces playbooks à l'usage, et écrire le contenu **par projet** : les `directives` spécifiques et le `DESIGN.md` de chaque projet. Ça, c'est de la donnée éditable, pas une décision figée.
