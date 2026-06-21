# PDR-0005 - UI et MCP partagent les mêmes use cases

**Status: validated direction**

## Context

Si l'UI et l'agent utilisent des chemins métier différents, leurs garde-fous, logs et états divergent. Depot v3 avait déjà identifié ce risque dans sa logique CLI résiduelle.

## Decision

L'UI et le MCP appellent les mêmes services applicatifs. Toute mutation réussie produit un événement qui actualise les projections concernées dans l'interface.

```text
UI  --\
      -> Application use case -> Domain/DB -> Event -> UI refresh
MCP --/
```

## Consequences

- aucun garde-fou uniquement dans l'adaptateur UI ou MCP ;
- mêmes erreurs et invariants ;
- mutation MCP visible sans redémarrage ;
- mutation UI lisible immédiatement par l'agent ;
- le transport et la technologie précise de l'event bus restent à décider.
