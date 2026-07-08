# Questions ouvertes

Ces sujets ne sont pas des décisions. Chaque fermeture doit référencer un PDR, un ADR, un prototype, un POC ou une preuve de dogfood.

## Q-002 - Execution est-elle la bonne entité ?

**Status: resolved (PDR-0008) - non**

Tranchée : `Execution` est rejetée. Le workspace est le pivot intention/runtime ; la relation Spec -> dev est la colonne `specs.workspace_id`. Réponses aux sous-questions :

- persistante ou dérivée ? → ni l'une ni l'autre, l'entité n'existe pas ;
- sans Spec ? → oui, c'est un workspace sans Spec (travail libre) ;
- plusieurs par Spec ? → non, worktree-par-Spec ;
- multi-workspace ? → non, un Spec pointe vers un workspace de dev ;
- début/fin ? → création et nettoyage du worktree ;
- qui possède terminaux/services ? → le workspace.

## Q-003 - Modèle Repository

**Status: resolved (PDR-0013)**

Tranché : `repositories` est une **entité persistée** (identité logique), avec `relative_path` (chemin conventionnel constant dans le projet) et `optional` (le « nyxignore » en DB : pas worktreé par défaut). La table de checkout `workspace_repositories` est **supprimée** — l'appartenance est dérivée (repos non-`optional` du projet) et l'état Git relu depuis Git. Mono et multi-repo partagent le modèle ; la distinction est UI. Voir PDR-0013.

## Q-004 - Stockage unique

**Status: resolved (PDR-0024) - un seul fichier**

Tranchée : **un seul fichier SQLite** porte toute l'autorité métier (transactions atomiques cross-contexte, mono-writer local en WAL, backup/migration triviaux). Les bounded contexts restent une **séparation logique**, pas physique. Le seul gain de fichiers séparés (sync indépendante) = handoff multi-machine, différé (PDR-0016). Réversible. Voir PDR-0024.

## Q-005 - Terminologie de l'unité humaine

**Status: resolved (PDR-0014, PDR-0018)**

Tranchée : l'unité humaine s'appelle **Spec** (renommée depuis « PRD » par PDR-0018), définie comme l'**unité de travail de l'humain** (il valide la Spec ; l'agent décompose en tâches et phases). La Spec porte une **clé Linear** (préfixe projet + numéro, ex. `NYX-42`) plus son slug. Le travail léger sans Spec reste possible (workspace sans Spec, W-001 ; une tâche peut exister sans Spec). Alternatives `Work Item` / `Change` écartées.

## Q-006 - Navigation et place permanente du terminal

**Status: prototype required**

Le terminal reste-t-il toujours monté pendant la navigation Specs/Runtime/Knowledge ? Le **workspace** (et non une Execution, supprimée) est-il une page, un tab, un split ou le mode principal de travail ?

## Q-007 - Attention Inbox

**Status: open — non bloquante (PDR-0021)**

Les **human tasks** (onglet Tasks) et le **signal de review** fonctionnent **sans** elle (PDR-0021) : l'Attention Inbox n'est pas un prérequis. C'est un **roll-up transverse différé** — agrégation cross-spec/projet de tous les signaux actionnables (human tasks, reviews prêtes, erreurs, agents en attente). Restent à concevoir : projection calculée vs entité persistée ; quels événements méritent une interruption ; comment éviter un centre de notifications bruyant.

## Q-008 - Timeline

**Status: open**

Quels événements sont utiles ? Ne jamais persister chaque octet terminal. Définir la frontière entre audit métier, diagnostic runtime et bruit.

## Q-009 - Scope d'absorption Depot

**Status: open (méthode fixée)**

Pour chaque sous-système : porter, simplifier, différer ou supprimer.

**Méthode (apprise en session) :** la forme d'absorption se décide par **revue d'usage avec l'utilisateur**, feature par feature, jamais par un agent qui lit le schéma et tranche seul. Un schéma dit ce qui a été construit, pas ce qui est utilisé.

À auditer : ~~prototypes, design rounds~~ (tranché par PDR-0009 : annexe du Spec, conservé, un seul par Spec, rounds = rollback, absorbé v2.7), catalogues UI, annexes, docs sync, milestones, tags, pending actions, reviews structurées, directives, contextes et activity log.

## Q-010 - Migration des données

**Status: open**

Mapping des identités, historique à conserver, pièces jointes, chemins WSL/Windows, révisions, données legacy et stratégie de rollback.

## Q-011 - Politique de reprise des agents et services

**Status: open**

Opt-in projet, opt-in workspace ou décision au redémarrage ? Définir clairement ce qui est repris, relancé, simplement affiché ou marqué inconnu.

## Q-012 - Limite du produit

**Status: continuous review**

Quelles capacités feraient basculer nyx d'un cockpit agentique vers un IDE ou un outil de gestion de projet générique ? Chaque feature avancée doit démontrer son effet sur la boucle travail -> agent -> review.

## Q-013 - Place des agents dans la navigation

**Status: open / prototype required**

Une `AgentSession` est une entité métier distincte, mais elle s'exécute aujourd'hui dans un terminal. Cela ne justifie pas automatiquement une catégorie `Agents` parallèle à `Terminals` et `Commands` dans l'arbre des workspaces : dans l'interface actuelle, chaque catégorie possède un bouton `+` qui crée directement son type de ressource, alors qu'un agent nécessite encore la création ou la réutilisation d'un terminal.

Options à prototyper :

- conserver uniquement les terminaux dans l'arbre et y afficher provider, état agent et reprise ;
- ajouter une vue `Agents` transversale de supervision, alimentée automatiquement par les sessions détectées, sans bouton de création initial ;
- ajouter une catégorie `Agents` dans les workspaces uniquement lorsqu'un vrai launcher permet de choisir provider, contexte, nouvelle session ou reprise, puis crée le terminal sous-jacent sans exposer cette plomberie.

Direction recommandée à ce stade : ne pas ajouter une troisième catégorie soeur purement cosmétique. Garder les sessions visibles sur leurs terminaux et tester une vue transversale `Agents`. Une catégorie avec `Nouvel agent` ne devient cohérente qu'avec un workflow agent first-class ; aucun déplacement manuel ou automatique d'un terminal entre catégories ne doit être imposé à l'utilisateur.

Questions à trancher :

- une session agent doit-elle apparaître simultanément dans `Agents` et `Terminals`, ou seulement via une projection principale avec lien vers le terminal ?
- la vue `Agents` est-elle globale, limitée au projet, ou disponible aux deux niveaux ?
- quels états méritent une présence dédiée : active, working, waiting, resumable, ended, resume failed ?
- le bouton `Nouvel agent` appartient-il à la vue Agents, au workspace, ou à un lanceur global ?
- comment traiter une session détectée après lancement manuel de `claude`, Codex ou d'un autre provider dans un terminal existant ?

## Q-014 - Mécanisme des hooks projet

**Status: resolved (PDR-0020)**

Tranchée : le fourre-tout « directives » de Depot est **décomposé en trois concepts** (PDR-0020) — **rules** (contexte advisory, table `directives`), **checks** (commandes déterministes à un gate, `managed_commands.gate` + `blocking`), **agents** (les rôles, table `agents` ; les ex-hooks « sous-agent » comme la *visual pass* deviennent un subagent `ui-tester` observable). Le **workflow** (`workflows` + `workflow_agents`) les câble ; la **livraison** passe par les hooks du plugin (SessionStart / UserPromptSubmit), **pas** par une table de hooks générique. Plus de table `hooks` séparée, ni de `kind` command/guard/check sur `directives`.

## Q-015 - Rafraîchissement temps réel des vues dérivées de Git

**Status: open — différée en v2.5 (hors scope v2.1)**

Décision de cadrage v2.1 : le temps réel de v2.1 couvre uniquement les mutations métier (`domain_events`). Les vues dérivées de Git appartiennent à la review → tranchée en v2.5.

Les mutations métier passent par `domain_events` (push UI). Mais les **vues dérivées de Git** (diff de review, dirty, branche courante) ne sont pas des mutations métier : elles changent quand le code change sur le disque. À pincer : le sidecar **watch-t-il le fs/git** pour émettre un event (réactif), ou **re-lit-il sur déclencheur** (ouverture de la review, tick) ? Éviter de poller en continu ; éviter de rater un changement.

## Q-014 - Design-system comme contexte agent, et fidélité du prototype

**Status: resolved (PDR-0010)**

Tranchée. Le design-system est du knowledge projet stocké dans nyx (tables `design` au format DESIGN.md + `components`), imposé en contexte à l'agent. Templating Handlebars compilé dans le sidecar Rust, Tailwind exécuté localement dans l'iframe, sync incrémentale par md5 visuel, inventaire par glob, amorçage greenfield. Détail complet dans PDR-0010. Le câblage exact reste de l'implémentation v2.7.
