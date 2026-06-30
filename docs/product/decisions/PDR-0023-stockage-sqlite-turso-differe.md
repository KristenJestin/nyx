# PDR-0023 - Stockage : SQLite confirmé ; couche requêtes = implémentation ; Turso différé

**Status: validated**

## Context

Turso (libsql, et la réécriture Rust « Turso DB » + l'ORM **Toasty**) proposé comme « futur de SQLite ». Évalué :

- le seul vrai différenciateur de Turso (**sync / edge / replicas**) correspond au **multi-machine = handoff, différé** (PDR-0016) — donc pas un besoin v2 ;
- **Turso DB** (rewrite) et **Toasty** (ORM pré-1.0) sont **early** : risqué pour le substrat d'un daily-driver ;
- **libsql** est un **driver, pas un ORM** : l'adopter ferait perdre un ORM Rust mûr (Diesel/sqlx ciblent SQLite-C) ;
- le « SQLite oblige à des binaires natifs par OS » était un artefact **Node/N-API** (le `core-host` de v1), **mort** avec le passage au **sidecar Rust** (PDR-0007).

## Decision

- **Moteur = SQLite** (SQLite-compatible), **compilé statiquement dans le sidecar Rust** → aucun module natif per-OS à shipper (le sidecar est déjà packagé par OS).
- **La couche requêtes / ORM (Diesel, sqlx, raw…) est un détail d'implémentation**, tranché au build (v2.0), **pas une décision produit**. Le `data-model.dbml` reste valable quel que soit le moteur SQLite-compatible.
- **Turso (libsql / Turso DB) + Toasty = upgrade path différé**, **réversible** (SQLite-compatible) : à rouvrir si/quand la **sync (handoff)** devient un besoin **et** qu'ils ont mûri.

## Consequences

- `technical-architecture.md` confirme SQLite (statiquement liée ; couche requêtes = impl).
- N'affecte pas le DBML. **Ne résout pas Q-004** (un fichier SQLite unique vs plusieurs bases internes), qui reste ouverte.
