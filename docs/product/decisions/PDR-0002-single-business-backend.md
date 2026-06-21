# PDR-0002 - Rust comme backend métier unique

**Status: validated constraint**

## Context

Une absorption directe du domaine TypeScript de Depot dans un coeur nyx Rust créerait deux backends métier devant partager projets, workspaces, base, événements et erreurs. Le POC Rust versus Node/TypeScript a ensuite montré que les différences SQLite mesurées étaient négligeables pour le produit, que la mesure renderer ne permettait pas d'attribuer proprement les outliers à Node, mais que Rust conservait un avantage mémoire et surtout l'investissement réel déjà présent dans `nyx-core`.

## Decision

Le produit final possède un seul backend métier : **Rust**.

Le TypeScript du frontend React et le JavaScript/TypeScript minimal imposé par Electron main/preload ne constituent pas un second backend métier s'ils restent de simples adaptateurs de transport, de lifecycle desktop et de fenêtre.

Les domaines utiles de Depot sont redessinés puis portés en Rust. Ils ne restent pas dans un serveur Node séparé et ne sont pas répartis entre un backend runtime Rust et un backend planning TypeScript.

## Consequences

- le domaine TypeScript utile de Depot devra être porté en Rust ;
- la logique métier encore présente dans le host Electron devra progressivement rejoindre `nyx-core` ;
- aucun découpage permanent « runtime Rust / planning TypeScript » ;
- le candidat Node et les frontières temporaires du POC doivent être supprimés ;
- le protocole Electron vers Rust doit exposer des opérations métier suffisamment épaisses pour éviter un bridge bavard.
