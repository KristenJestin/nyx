# PDR-0006 - Electron, Rust et monorepo simplifié

**Status: validated**

## Context

Tauri a été conservé pendant la migration comme solution de repli éventuelle, dans l'espoir qu'une évolution future de WebKitGTK améliore les performances Linux observées. Cette évolution est incertaine. Maintenir deux shells implique deux builds, deux intégrations lifecycle, des abstractions communes artificielles et une vérification permanente d'une plateforme qui n'est pas livrée.

Le découpage actuel `apps/frontend`, `apps/electron` et `apps/tauri` reflète la migration, pas l'architecture finale du produit.

## Decision

nyx V2 utilise **Electron comme shell desktop unique**. Le renderer React fait partie de l'application desktop Electron et n'est plus traité comme une application autonome.

Rust reste l'unique backend métier. Electron main/preload ne possède que les responsabilités imposées par le shell : fenêtres, menus, lifecycle desktop, sécurité, IPC renderer et supervision du sidecar Rust.

Le repository reste un **monorepo léger** parce qu'il contient plusieurs unités de build d'un seul produit :

```text
nyx/
|- apps/
|  `- desktop/
|     |- main/
|     |- preload/
|     `- renderer/
|- crates/
|  |- nyx-core/
|  `- nyx-sidecar/
`- e2e/
```

Tauri est supprimé du build actif, des dépendances et de la maintenance. Son code historique reste disponible via Git ; il ne justifie pas une architecture parallèle « au cas où ».

## Consequences

- une seule application desktop est packagée et testée ;
- le renderer React peut être co-localisé avec son unique consommateur ;
- Cargo workspace et workspace JavaScript coexistent dans le même repository sans créer deux produits ;
- les abstractions destinées uniquement à rendre Tauri et Electron interchangeables sont supprimées ;
- réintroduire un autre shell demandera une nouvelle décision fondée sur une preuve concrète, pas sur une annonce future ;
- aucune logique projects, workspaces, PRD, tâches, reviews, sessions, commandes, SQLite ou MCP ne doit s'installer durablement dans Electron main/preload.

La topologie et le transport exacts du sidecar sont définis par PDR-0007.
