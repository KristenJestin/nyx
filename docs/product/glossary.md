# Glossaire produit nyx

Lexique de référence des termes du produit nyx V2. Définitions courtes ; la vérité détaillée vit dans les documents canoniques et les PDR cités.

## Produit & méthode

- **nyx** — le produit : gestionnaire de terminaux orienté travail agentique ; application desktop unique (Electron + sidecar Rust).
- **Depot** — l'outil Spec/tâches/review existant (CLI Node) ; ses capacités utiles sont absorbées dans nyx, puis Depot est retiré.
- **V2** — la totalité de la super-app cible ; `v2.0`…`v2.8` sont ses tranches livrables, pas des produits séparés.
- **PDR** (Product Decision Record) — décision produit datée (`decisions/`) qui explique le pourquoi et ce qu'elle supersède.
- **Statuts de décision** — `validated` / `proposed` / `investigating` / `rejected` / `superseded`.
- **Grill** — motif fondateur : l'humain lâche du brut, l'agent le structure (clarifie, déduplique, ordonne), l'humain valide. Un comportement de playbook, pas un moteur.

## Structure locale

- **Project** — conteneur durable d'un produit logiciel ; autorité unique (pas de project Depot parallèle). Porte un `key` (préfixe Linear, ex. `NYX`).
- **Workspace** — dossier physique sur disque et **pivot unique intention/runtime**. Ligne permanente (`folder_removed_at` marque le nettoyage, jamais de delete auto).
- **Primary workspace** — le workspace de base durable d'un projet (`projects.primary_workspace_id`) ; conception et travail libre.
- **Worktree-per-Spec** — workspace éphémère créé pour développer un Spec, nettoyé au merge.
- **Repository** — identité **logique** d'un repo dans le projet (ex. `api`) ; `relative_path` constant (`.` en mono-repo), flag `optional` (le « nyxignore » en DB). Pas de table de checkout : appartenance dérivée, état Git relu depuis Git (PDR-0013).

## Travail (Spec)

- **Spec** — l'unité de travail de **l'humain** : intention structurée, ticket durable. Clé Linear (`projects.key` + `number`, ex. `NYX-42`) + slug. L'humain valide ; l'agent décompose (PDR-0014).
- **Work Item** — terme écarté : on garde « Spec » (PDR-0014).
- **Task** — unité de **décomposition gérée par l'agent** (pas de clé humaine, ULID). Porte `type`, `executor` (`agent` / `human`), `status`, `priority` (string), `phase` nullable. Le coder ne travaille que sur des tasks.
- **Phase** — regroupement optionnel de tasks posé par l'agent. `tasks.phase` (entier nullable) indexe une **liste nommée et ordonnée portée par la spec** (`specs.phases`, PDR-0022) ; pas de table de phases ; statut de phase **dérivé** des tasks.
- **Finding** — un commentaire de review **promu en task** ; seul travail vu par le coder, seul lien durable review → Spec.
- **Milestone** — jalon projet.
- **Catégorie** — axe **unique** de classification d'un Spec (`specs.category_id`) : le « genre » du travail (Feature / Bug / Refactor / Infra / Design…). Set **par projet** (quatre catégories système seedées : `feature`, `bug`, `refactor`, `chore`). L'agent assigne depuis l'existant et, si rien ne colle, **minte** à la volée — non bloquant. *Amendement du 2026-09-02 de PDR-0017 : le mint est **valide tel quel**, il n'y a plus d'état « proposée » ni de geste `confirm`. L'humain cure le **vocabulaire** depuis les settings du projet — **créer / renommer / supprimer** — la suppression étant refusée sur les quatre slugs système et sur une catégorie encore portée par une Spec.* Remplace les tags (PDR-0017).
- **Priorité** — enum string `urgent` / `high` / `med` / `low`, stockée sur Spec et task.
- **Human action** (human task) — une **tâche `executor = human`** : une action que seul l'humain peut faire. Affichée dans l'**onglet Tasks** avec un badge « toi » ; l'humain peut la marquer `done` (seule task actionnable côté humain) ; l'agent l'annonce aussi en chat. Pas de table dédiée (`human_actions` fold dans `tasks`, PDR-0014/PDR-0021). L'Attention Inbox (roll-up transverse) est différée.
- **Annexe** — artefact nommé rattaché à un Spec (prototype, échantillon de données, doc, image). Trois sources possibles : **texte inline**, **blob interne** (dédupliqué), ou **référence à un chemin** (fichier lu en live, idéal pour les gros fichiers ; résolu via `repository_id`). « attachment » n'est pas un concept produit (PDR-0016).

## Runtime

- **Terminal** — PTY interactif rattaché à un workspace ; statut `open` / `closed` / `interrupted`.
- **Managed command** — définition de service au niveau projet ; **command instance** = son instanciation par workspace ; **command run** = une exécution (statut, exit code).
- **Agent session** — session persistante d'un provider agentique (Claude Code…), rattachée au workspace, reprenable, survit à son terminal courant. `provider` agnostique.
- **Agent session binding** — lien horodaté session ↔ terminal courant.
- **Agent context snapshot** — trace d'un contexte généré injecté à un agent.
- **RunState** — projection **visuelle** unifiée (`idle` / `running` / `waiting` / `success` / `error` / `closed`) des enums de statut domaine (mapping figé dans `status-map`). Un terminal ne « réussit » ni n'« échoue » — c'est un command run.

## Review (vue dérivée — PDR-0015)

- **Review** — **vue calculée**, pas une table : le diff d'un repo relu de Git + ses commentaires. Suit le Spec ; pas de statut ni de snapshot propres.
- **Comment** — annotation ancrée (fichier / ligne / plage) ou globale, threadée ; ancrée sur `(workspace, repository)` + `spec_id` optionnel. Auteur humain ou agent.
- **Résolution** — passage d'un commentaire à `resolved` : **réservée à l'humain** ; l'agent répond ou promeut.
- **Outdated** — commentaire dont l'ancre a bougé/disparu (remap via `base_blob_oid` + contexte) ; grisé, jamais supprimé.
- **Living / pinned** — ancien couple (PDR-0012) ; `pinned` supprimé (PDR-0015) : tout diff est dérivé.

## Knowledge & mémoire projet

- **Knowledge** — **parapluie**, pas un synonyme d'« idées » : le knowledge projet stocké dans nyx couvre les **idées** *et* le **design-system** (README, décision 16). Le terme est **interne** — il nomme la route `/knowledge`, jamais une surface. Correspondance à ne pas « corriger » : la page des idées s'appelle **`Ideas`** dans l'UI et vit sur la route interne **`/knowledge`** ; la page du design-system s'appelle **`Design`** — entrée de rail, titre de page et fil d'Ariane du détail composant, tous anciennement libellés `Catalogue` — et vit sur **`/design`**. `catalogue` y reste le **nom commun** de ce que la page liste (états vides, copie de retrait) et le concept que portent la base et les outils MCP : c'est le *nom* de la surface qui ne pouvait pas être double, pas le concept. Les deux routes sont invisibles (le routeur tourne sur une memory history, PDR-0025) : renommer `/knowledge` en `/ideas` rétrécirait un concept du glossaire pour l'aligner sur une seule de ses deux surfaces, pour zéro bénéfice utilisateur.
- **Idea** — capture pré-engagement, promouvable en Spec.
- **ADR** — Architecture Decision Record (numéroté par projet).
- **Directive** — fragment de contexte/règle projet ; constitue le playbook (« directives v2 »).
- **Document** — note / spécification / annexe / runbook / recherche / rapport (révisionné).
- **Attention Inbox** — projection **calculée** des signaux nécessitant une action humaine (pas une table mutable).
- **Timeline / domain events** — événements **métier** utiles (jamais chaque octet terminal) ; `read_cursors` pour les compteurs non lus.

## Conception (prototype & design)

- **Prototype** — annexe visuelle d'un Spec (scope projet, HTML en iframe, aucun runtime) ; un seul par Spec ; absorbé tard (v2.7, PDR-0009).
- **Page / Version / Variant** — hiérarchie d'un prototype ; **Round** = snapshot nommé de tout le design (mécanisme de rollback) ; **Feedback** = commentaire piné (`open` / `ignored` ; `resolved` dérivé).
- **Design-system** — knowledge projet (tables `design` au format DESIGN.md + `components`), **imposé** en contexte à l'agent (proto et dev) ; rendu Handlebars sidecar + Tailwind iframe (PDR-0010).

## Harness agentique

- **Harness** — pas un moteur de workflow : un **playbook** (texte/directives) que l'agent **exécute** via les **verbes observables** de nyx (PDR-0011).
- **Verbes observables** — primitives v1 : lancer/observer une session avec un rôle, injecter du contexte, hooks start/close.
- **Playbook** — le workflow lui-même, donnée éditable par projet (payoff v2.7).
- **Rôles par défaut** — orchestrateur (ne code pas, pilote), coder (ne fait que des tasks), auditeur (signale des findings, ne corrige pas), doc-sync (doc post-merge, reviewable).
- **Aucun angle mort** — tout ce que fait l'agent passe par une surface observée : nyx possède le PTY et l'arbre de processus.

## Plateforme technique

- **Renderer** — UI React embarquée dans Electron ; uniquement présentation et abonnements.
- **Electron main / preload** — fenêtre, lifecycle desktop, IPC allowlisté ; **pas** de logique métier.
- **Sidecar** — exécutable Rust enfant supervisé par Electron (PTY, SQLite, MCP, métier) ; lié à la durée de vie de l'app (pas un daemon) (PDR-0007).
- **IPC** — renderer ↔ main via `contextBridge` ; main ↔ sidecar via pipes locaux, protocole MessagePack versionné et framé.
- **MCP** — serveur HTTP du sidecar : interface **externe** pour les agents vers la même autorité Rust (pas le transport UI interne).
- **N-API / core-host** — retirés de l'architecture cible (PDR-0006 / PDR-0007).

## Migration

- **Import run** — exécution d'import Depot → nyx, idempotente, avec backup et rapport restaurable.
- **Legacy id mapping** — correspondance des identités source → cible.
