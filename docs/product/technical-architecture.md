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
  |- SQLite et migrations
  |- projets, workspaces et repositories
  |- commandes et sessions agents
  |- PRD, tâches, reviews et knowledge
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

## Interdictions

- pas de logique métier dans preload ou Electron main ;
- pas de `core-host` Node ;
- pas de N-API dans la cible ;
- pas de socket HTTP localhost pour le transport UI interne ;
- pas de JSON/base64 par petit chunk terminal ;
- pas de listener, timer ou processus sans owner et procédure de fermeture ;
- pas de `process.exit` utilisé comme substitut permanent à un shutdown correct.
