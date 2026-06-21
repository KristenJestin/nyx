# Produit nyx - Index canonique

## Rôle de ce dossier

Ce dossier est la mémoire produit durable de la prochaine génération de nyx.

- Les documents canoniques décrivent uniquement la vérité actuellement retenue.
- Les PDR (`decisions/`) expliquent pourquoi une décision a été prise ou remplacée.
- `open-questions.md` contient les hypothèses et sujets non tranchés.
- Une proposition ne devient pas canonique uniquement parce qu'elle apparaît dans une roadmap ou une conversation.

## Ordre de lecture

1. [Vision](vision.md)
2. [Modèle de domaine](domain-model.md)
3. [Architecture de l'information](information-architecture.md)
4. [Workflows](workflows.md)
5. [Harness agentique et contextes](agent-harness.md)
6. [Architecture technique](technical-architecture.md)
7. [Roadmap](roadmap.md)
8. [Questions ouvertes](open-questions.md)
9. [Décisions produit](decisions/)

Le schéma concret est dans [`data-model.dbml`](data-model.dbml).

## Statuts

- **validated** : décision explicitement retenue.
- **proposed** : direction sérieuse à valider par conception, prototype ou dogfood.
- **investigating** : nécessite une recherche ou un POC.
- **rejected** : piste écartée, avec raison conservée.
- **superseded** : ancienne décision remplacée.

## Registre des concepts

ID    | Concept                                | Statut              | Source
----- | -------------------------------------- | ------------------- | ------------------------
C-001 | Nom du produit : nyx                   | validated           | PDR-0001
C-002 | Application unique nyx                 | validated           | PDR-0001
C-003 | Absorption puis disparition de Depot   | validated           | PDR-0001
C-004 | Backend métier dans un seul langage    | validated           | PDR-0002
C-005 | Backend métier Rust                    | validated           | PDR-0002
C-006 | Project comme conteneur durable        | proposed            | domain-model
C-007 | Workspace comme pivot intention/runtime | validated           | PDR-0008
C-008 | Repository explicite dans un workspace | proposed            | Q-003
C-009 | Execution comme entité première classe | rejected            | PDR-0008
C-010 | PRD et tâches natifs dans nyx          | validated           | PDR-0001
C-011 | Review Git générique                   | validated direction | PDR-0004
C-012 | UI et MCP sur le même état temps réel  | validated direction | PDR-0005
C-013 | Attention Inbox                        | proposed            | information-architecture
C-014 | Timeline unifiée                       | proposed            | information-architecture
C-015 | Workflow simple par défaut             | proposed            | vision
C-016 | Electron comme shell desktop unique    | validated           | PDR-0006
C-017 | Monorepo produit simplifié             | validated           | PDR-0006
C-018 | Tauri retiré de la plateforme active   | validated           | PDR-0006
C-019 | Backend Rust en sidecar supervisé      | validated           | PDR-0007
C-020 | IPC interne sans N-API ni core-host    | validated           | PDR-0007
C-021 | PRD ticket durable, pointeur workspace | validated           | PDR-0008
C-022 | Workspace = ligne permanente, jamais auto-supprimée | validated | PDR-0008
C-023 | Prototype = annexe du PRD, absorbé tard | validated           | PDR-0009
C-024 | Design-system imposé comme contexte agent | validated         | PDR-0010
C-025 | Rendu proto : Handlebars sidecar + Tailwind iframe | validated   | PDR-0010
C-026 | Harness = playbook éditable + verbes observables | validated direction | PDR-0011
C-027 | Review = review / comment / task ; finding = task | validated     | PDR-0012
C-028 | Repository logique ; pas de table de checkout    | validated     | PDR-0013
C-029 | Grill : l'humain lâche brut, l'agent structure, l'humain valide | validated | agent-harness
C-030 | Playbook par défaut (orchestrateur/coder/auditeurs/doc) | proposed | agent-harness

## Décisions validées à ce jour

1. Le produit garde le nom **nyx**.
2. La cible n'est pas « nyx avec Depot embarqué », mais un produit redessiné et unique.
3. Depot doit disparaître comme marque, binaire, daemon, serveur et interface autonome après migration de ses capacités utiles.
4. PRD, tâches, review, sessions agents, terminaux et services doivent partager le même modèle produit et être accessibles à l'UI comme au MCP.
5. Le backend métier final ne doit pas être partagé entre Rust et TypeScript.
6. Rust est l'unique backend métier. Le TypeScript du renderer et le JavaScript/TypeScript minimal d'Electron main/preload restent des adaptateurs, pas un second backend.
7. Electron est l'unique shell desktop de la V2. Tauri est retiré du build et de la maintenance active ; son historique reste récupérable dans Git.
8. Le backend Rust est un exécutable sidecar enfant, démarré et supervisé par Electron ; il ne survit pas à l'application et n'est pas un daemon.
9. Le renderer communique avec main/preload par l'IPC Electron allowlisté. Main communique avec le sidecar par des pipes locaux avec protocole versionné et framing explicite.
10. N-API et le `core-host` Node ne font pas partie de l'architecture cible.
11. Le repository reste un monorepo léger regroupant l'application desktop, le binaire Rust et les tests transverses.
12. La migration et la consolidation Electron doivent être terminées avant l'absorption fonctionnelle massive de Depot.
13. Le **workspace** est le pivot unique entre intention et runtime. `Execution` est rejetée comme entité : la relation PRD -> dev est une simple colonne `prds.workspace_id`, le runtime se rattache au workspace.
14. La ligne workspace est un enregistrement permanent ; `folder_removed_at` marque le nettoyage du worktree au merge, aucune suppression automatique. L'historique d'un PRD vit sur le ticket durable, pas sur l'éphémère.
15. Le **prototype** est une annexe du PRD (scope projet, sans workspace), absorbé tard (v2.6) : un seul par PRD, rounds conservés comme rollback, forme décidée par revue d'usage et non par lecture de schéma.
16. Le **design-system** est du knowledge projet stocké dans nyx (tables `design` au format DESIGN.md + `components`), imposé en contexte à l'agent pour le proto comme pour le dev. Rendu proto : Handlebars compilé dans le sidecar Rust, Tailwind exécuté localement dans l'iframe, sync incrémentale par md5 visuel. Electron et le renderer ne compilent rien (PDR-0010).
17. Le **harness agent** est un **playbook éditable par projet** (« directives v2 ») exécuté par l'agent via des **verbes observables** (lancer/observer une session, injecter du contexte, hooks). nyx ne construit **pas** de moteur de workflow. Les verbes sont des primitives v1 ; le harness est le payoff v2.6 (PDR-0011).
18. La **review** tient en 3 concepts : `review` (le diff d'un repo + ses commentaires, un par repo), `comment` (annotation ancrée/globale, résolution humain-seul), `task` (un commentaire promu pour le coder). **finding = task** ; le coder ne travaille que sur des tasks. Diff-engine et UI dans le PRD nyx « Viewer de gros diffs », v2.3 (PDR-0012).
19. **Multi-repo** : `repositories` est une identité logique persistée (chemin conventionnel + flag `optional` = nyxignore en DB) ; la table de checkout `workspace_repositories` est **supprimée**, l'appartenance dérivée et l'état Git relu depuis Git. Mono et multi partagent le modèle (distinction UI). Les commandes managées peuvent cibler un repo (PDR-0013).

## Discipline de mise à jour

Lorsqu'une question est tranchée :

1. créer ou mettre à jour un PDR ;
2. mettre à jour le document canonique concerné ;
3. mettre à jour le registre ci-dessus ;
4. fermer ou déplacer la question dans `open-questions.md` ;
5. seulement ensuite créer les PRD d'implémentation.
