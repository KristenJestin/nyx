# Architecture technique cible

## État

**Status: validated direction**

## Topologie

```text
Renderer React
  | IPC Electron allowlisté
Preload + Electron main
  | pipes locaux, protocole versionné
Sidecar Rust supervisé
  |- PTY et arbres de processus
  |- SQLite et migrations (un seul fichier — PDR-0024 ; statiquement liée ; couche requêtes = impl ; Turso différé, PDR-0023)
  |- projets, workspaces et repositories
  |- commandes et sessions agents
  |- Spec, tâches, reviews et knowledge
  `- serveur MCP HTTP pour les agents externes
```

Il existe deux frontières différentes :

1. **Renderer vers Electron main** : IPC Electron via un preload utilisant `contextBridge`. Le renderer n'accède jamais directement à `ipcRenderer`, Node, Rust ou au système de fichiers.
2. **Electron main vers backend** : le main démarre un exécutable Rust enfant et échange avec lui par `stdin/stdout`. `stderr` est réservé aux logs du sidecar.

Le serveur MCP HTTP n'est pas le transport interne de l'UI. C'est une interface externe vers la même autorité Rust.

## Protocole sidecar

- handshake obligatoire avec version du protocole, version du backend et capacités ;
- requêtes/réponses corrélées par identifiant ;
- événements non sollicités du backend vers l'UI ;
- frames préfixées par leur longueur, taille maximale imposée et parsing incrémental ;
- données terminal binaires, sans conversion base64 ;
- backpressure et quotas par terminal ;
- timeout et annulation pour chaque requête longue ;
- erreur structurée stable, sans dépendre des textes internes Rust ;
- incompatibilité de version détectée au boot avec erreur lisible, jamais avec un chargement infini.

Le protocole V1 utilise des messages **MessagePack** préfixés par une longueur `u32` big-endian. MessagePack transporte directement les buffers PTY sans base64. Le contrat Rust est l'autorité ; les types TypeScript et les fixtures contractuelles vérifient la parité des deux côtés.

## Distribution multiplateforme

Chaque package Electron embarque le binaire correspondant à sa cible, hors `app.asar` :

```text
Windows x64/arm64 : nyx-sidecar.exe
Linux x64/arm64   : nyx-sidecar
macOS x64/arm64   : nyx-sidecar
```

Les builds sont produits sur l'OS cible. macOS signe et notarise l'application avec son sidecar ; Linux conserve le bit exécutable et définit une baseline de compatibilité glibc ; Windows inclut le sidecar dans le package signé. Electron résout le chemin depuis ses resources, jamais depuis le répertoire courant.

## Ownership des ressources

Le sidecar Rust possède PTY, enfants, commandes managées, serveur MCP, pool SQLite, timers métier et état runtime. Electron possède fenêtres, menus, notifications desktop, preload et supervision du sidecar. Le renderer possède uniquement l'état de présentation et les abonnements UI.

Chaque ressource longue durée doit avoir : un propriétaire unique, un identifiant, une méthode de fermeture idempotente, une deadline de shutdown et un test prouvant sa disparition.

## Lifecycle

Ordre de démarrage : spawn du sidecar, handshake borné, ouverture/migration SQLite, restauration de l'état, démarrage MCP, puis signal `ready` autorisant le renderer à charger les données.

Ordre d'arrêt : bloquer les nouvelles mutations, snapshotter l'état restaurable, arrêter MCP, commandes et PTY, flusher SQLite, répondre `shutdown-complete`, fermer les pipes puis sortir. Après la deadline, Electron force la destruction de l'arbre de processus.

Le sidecar n'est pas un daemon : il est lié à la durée de vie de l'application. La fermeture du pipe parent déclenche son arrêt. Sur Windows, les processus sont placés dans un Job Object ; sur Linux, parent-death signal et process groups ; sur macOS, surveillance du parent et process groups. Le sidecar reste responsable de tuer et récolter ses descendants.

## Migrations SQLite

**Toute nouvelle migration de `crates/nyx-core/migrations/` porte un préfixe horodaté UTC** — `YYYYMMDDHHMMSS_nom`, la convention de `diesel migration generate` — et **jamais** le numéro suivant de la lignée historique `000000000000NN`, gelée à `00000000000051`.

> La lignée a été gelée à `00000000000049` par `feature/tiling-panes`, puis rouverte de deux crans au merge : `v2` avait déjà miné et **appliqué sur des bases réelles** `00000000000050_annexes_url_unique` et `00000000000051_annexes_url_canonical`. Les renommer aurait rendu leurs lignes de ledger étrangères et fait **rejouer** un `CREATE UNIQUE INDEX` non idempotent — donc un `Db::open` en échec et un nyx qui refuse de démarrer. Ces deux numéros ne sont revendiqués par aucune autre branche : les grand-pérenniser ne collisionne avec rien, et la règle continue de refuser `00000000000052_*`.

**Pourquoi.** Diesel identifie une migration appliquée par le **seul préfixe numérique** de son répertoire ; le libellé qui suit n'est jamais stocké. Deux worktrees frères du dépôt qui minent chacun « la migration suivante » `00000000000052_*` et bootent contre la **même** base `%APPDATA%/nyx-v2/nyx.db` produisent donc une **collision silencieuse** : le premier à s'exécuter écrit la ligne du ledger, le second est déclaré déjà appliqué et n'est jamais exécuté — `pending_migrations()` vide, boot qui se déclare vert, tables absentes. Le cas a été payé une fois sur la base réelle d'un utilisateur (`no such table: managed_command_groups`). Un horodatage ne peut pas entrer en collision avec une lignée séquentielle et trie après elle : il devient la tête, et aucune branche sœur ne peut le dupliquer.

**Conséquence à connaître.** Un préfixe daté trie **au-dessus** de toute la lignée `000000000000NN` : il devient la tête des migrations. Un test qui remonte le schéma « N fois » compte donc faux dès la migration suivante — les chaînes de revert de `crates/nyx-core/src/db.rs` prennent un plancher **nommé** (`peel_migrations_above` / `revert_down_through`), jamais un compte.

**Où c'est écrit et tenu.** Le détail de l'incident mesuré est dans `crates/nyx-core/migrations/20260819140000_create_command_groups/up.sql`. La règle est **exécutoire** : `db::tests::every_migration_added_after_the_frozen_lineage_is_dated_not_numbered` refuse tout répertoire qui prolonge la lignée numérotée. Au boot, `SchemaHealth::boot_report` refuse de conclure au succès quand une migration est en attente et **nomme** les versions du ledger que ce build n'embarque pas (`foreign_ledger_versions`), seule trace que cette panne laisse derrière elle.

## Crash et reprise

Un crash backend ne déclenche jamais une relance aveugle des services. Electron peut redémarrer le sidecar une fois, rouvrir SQLite et reconstruire l'état persisté. Les opérations mutantes portent une clé d'idempotence ; les états `running` devenus invérifiables passent à `unknown` ou `interrupted`. L'utilisateur choisit ensuite ce qui doit être repris.

Les politiques existantes restent canoniques : reprise des sessions agents configurée au niveau projet et `restart_on_startup` configuré sur les commandes du projet. Le sidecar ne crée pas un troisième système de reprise.

## Migration depuis N-API

1. figer le contrat renderer existant et ses tests ;
2. créer `nyx-sidecar` autour de `nyx-core` avec handshake, ping et shutdown ;
3. porter le chemin PTY complet avec flux binaire et backpressure ;
4. porter commandes, lifecycle, busy-state, reprises et MCP actuellement orchestrés dans le `core-host` TypeScript ;
5. exécuter les mêmes tests contractuels contre N-API puis contre le sidecar ;
6. basculer Electron sur le sidecar par défaut ;
7. supprimer `core-host`, `nyx-napi` et leurs switches après parité ;
8. valider les packages et le lifecycle sur Windows, Linux et macOS.

## Temps réel

Les mutations métier (status, tâches, commentaires, runtime) sont **poussées** à l'UI via `domain_events` (events non sollicités du sidecar → main → renderer) ; **pas de polling**. Les **vues dérivées de Git** (diff de review, dirty, branche courante) ne sont **pas** des mutations métier : elles changent quand le code change sur le disque. Leur rafraîchissement (watch fs/git → event, ou re-read déclenché) est **différé en v2.5** (Q-015).

## Sécurité de la surface MCP

Le serveur MCP est une interface **externe** (agents) vers l'autorité Rust ; sa surface d'attaque doit être explicite, d'autant que le produit assume que **l'agent a autorité sur l'état**.

- **Bind strictement loopback.** Le listener écoute sur `127.0.0.1` (`Ipv4Addr::LOCALHOST`), **jamais** `0.0.0.0` : la surface est **injoignable hors machine** (ADR-0003 D1). Port **fixe** (résolu à l'onboarding), endpoint `GET /health`, **start-once** (un seul listener par process, jamais un second).
- **Anti-DNS-rebinding.** Le header `Host` de chaque requête doit être une **autorité loopback** ; une page malveillante qui « rebind » un hostname vers `127.0.0.1` porte le hostname de l'attaquant dans `Host` (pas un littéral loopback) → **rejetée** avant d'atteindre la surface MCP (ADR-0003 D9). Les clients légitimes (Claude Code) visent `127.0.0.1`/`localhost` et passent.
- **Frontière de confiance = la machine locale.** Il n'y a **pas de token par requête** : la garde est loopback + Host. Conséquence assumée : **tout process local sur la machine** peut atteindre le MCP et muter l'état (même posture que nyx v1). C'est la surface à connaître — la frontière de sécurité est la machine, pas l'identité utilisateur. Un durcissement (token de session, allowlist de provider) est un upgrade path si un usage le justifie.
- **`nyx.localhost` (portless)** est une surface **humaine/intégration séparée**, **pas** le transport MCP (qui reste `http://127.0.0.1:<port>/mcp` en direct).

## Politique de validation : piloter l'app réelle

**Toute capacité à surface runtime se valide par le pilotage réel de l'application** — Electron + sidecar + vrai PTY/shell, sur les shells cibles réels (ex. `bash + starship` sous omarchy/direnv/zoxide) —, **jamais** par un shell hermétique ni des mocks seuls.

- **Pourquoi.** Un test hermétique peut être **vert sur un faux positif** : le cas fondateur est la segmentation OSC 133 où le marqueur `B` émis par le fixture était **effacé par le vrai starship**, masquant des heures durant que le produit ne marchait pas. Le monde réel diverge du fixture ; seul le pilotage réel le prouve.
- **Comment.** Gate e2e Electron **pilotée** (CDP / WebDriver) contre l'app réelle, en plus des tests unitaires/contrat (nécessaires mais **non suffisants** pour déclarer une capacité livrée). Les gates de tranche l'appliquent (cockpit v2.2, review v2.5 9/9, terminal-blocks).
- **Règle.** Une capacité n'est « livrée » que lorsqu'elle a été **observée fonctionner dans l'app réelle**, pas seulement lorsqu'une suite verte le suggère.
- **Une capacité dont le consommateur est un AGENT ne s'observe qu'avec un agent réel.** Piloter le fil JSON-RPC n'est pas piloter un client : c'est ainsi qu'une surface MCP verte de bout en bout s'est révélée **inatteignable** en pratique, le client ne rendant pas au modèle ce que le fil portait (PDR-0030). Les manuels injectés sur `/nspec` `/ndev` `/ndoc` (PDR-0031) relèvent de la même règle — ils se valident en session, sur un vrai client, jamais sur une suite verte.

## Interdictions

- pas de logique métier dans preload ou Electron main ;
- pas de `core-host` Node ;
- pas de N-API dans la cible ;
- pas de socket HTTP localhost pour le transport UI interne ;
- pas de JSON/base64 par petit chunk terminal ;
- pas de listener, timer ou processus sans owner et procédure de fermeture ;
- pas de `process.exit` utilisé comme substitut permanent à un shutdown correct.
