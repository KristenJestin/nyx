# PDR-0025 - Routeur dans le renderer (TanStack Router) pour le shell de navigation multi-pages

**Status: validated**

Introduit une dépendance (`@tanstack/react-router`) dans `apps/desktop/renderer`.

## Context

Jusqu'ici le renderer n'avait **aucun routeur** : `app.tsx` montait UNE seule surface top-level, et la v2.2 avait ajouté un second écran (le cockpit workspace) via un seam hash artisanal (`useAppView`, `#/cockpit/…`). Ce cockpit est supprimé (PDR-0008 : le workspace est le pivot RUNTIME en base, pas un écran ; on entre dans un PROJET, jamais dans un workspace).

Le modèle de navigation validé par l'utilisateur (le prototype) est **multi-pages** : un rail de projets à gauche, une section-nav (Terminals `/`, Work `/work`, Knowledge `/knowledge`) et un `<Outlet/>`. Le prototype utilise déjà **TanStack Router**. Poser ce shell proprement demande un vrai routeur (pages navigables, layout racine partagé, `<Outlet/>`), pas un seam hash maison.

## Decision

- **Adopter `@tanstack/react-router`** dans le renderer (même famille que TanStack Form / Hotkeys déjà présents, et que le prototype de référence).
- **Routing par code** (`createRootRoute` + `createRoute` dans un module `src/router.tsx`), PAS le file-based codegen (`routeTree.gen.ts`) du prototype : on évite le plugin Vite de génération de routes et son artefact généré, l'arbre de 3 routes tient en un fichier lisible et testable.
- **`createMemoryHistory`** (pas d'history navigateur) : l'app Electron est chargée en `file://`, où muter le path de l'URL est fragile ; une history mémoire garde la navigation entièrement en RAM, déterministe et scriptable en test.
- **`BaseLayout` = route racine** (rail projets + section-nav + `<Outlet/>`), les 3 routes enfants montent leur page dans l'`<Outlet/>`.
- Le seam hash (`useAppView`) et toute route `#/cockpit/…` sont **supprimés**.

## Consequences

- `apps/desktop/renderer/package.json` gagne `@tanstack/react-router`.
- `app.tsx` monte désormais `<RouterProvider router={router}/>` (+ le `<Toaster/>` global) au lieu d'une surface unique.
- Le shell (`components/layout.tsx` → `BaseLayout`) et les pages (`routes/`) sont portés PROPREMENT contre le design system réel (tokens CSS uniquement, réutilisation de `ui/` et du vrai xterm), pas copiés du prototype.
- Base saine pour la persistance terminal (v2.2.5) et les pages Work / Knowledge (portées ultérieurement).
