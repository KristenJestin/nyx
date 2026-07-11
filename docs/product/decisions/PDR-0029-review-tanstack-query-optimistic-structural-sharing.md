# PDR-0029 - Couche données review sur TanStack Query : optimistic + keepPreviousData + structural sharing (tue le lag et le « depop/pop »)

**Status: validated**

Migre la **couche DONNÉES de la review** (renderer) des hooks 100 % custom (`useState` + `nyxBridge.invoke` + `subscribe("repos://changed")`) vers **`@tanstack/react-query`** (`5.101.2`, ajouté à `apps/desktop/renderer/package.json` ; on avait déjà `@tanstack/react-router`). Décision utilisateur actée à la revue du build v2.4 (suivi `01KX8J52X6Q29W1WJD4H7HA4YE`, findings #62/#63). Objectif : supprimer **à la source** deux régressions ressenties de la review — le **lag** (2-3 s avant qu'une action apparaisse) et le **« depop/pop »** (le diff qui clignote/reconstruit à chaque mutation). Sert de référence ADR pour l'adoption de `@tanstack/react-query` (une dépendance structurante = un PDR dédié nommant le paquet + la version, discipline PDR-0026/0028).

## Context

La review est une **vue dérivée** du `repo_diff` calculé par `nyx-core` (PDR-0015/0027), rendue par `@pierre/diffs` / `@pierre/trees` (PDR-0028). Les 5 hooks de données étaient custom : `use-review-comments`, `use-review-tasks`, `use-workspace-review`, `use-repo-diff`, `use-review-repos`. Deux défauts en découlaient :

- **Le lag.** Aucune mutation n'était optimiste : un resolve/promote/post faisait `invoke(...)` puis `load()` (re-pull), soit ~2-3 s avant que l'UI bouge.
- **Le « depop/pop » (flash).** Chaque mutation broadcast `repos://changed`. Chaque hook (× 5) ET la page re-souscrivaient et re-pullaient → un même push déclenchait 3-4 reloads parallèles (le « double-refetch »). Surtout, `use-repo-diff` re-pullait un **NOUVEL objet `RepoDiff`** (identité neuve) même quand le code n'avait pas changé → `DiffView` reconstruisait `patchByFile` (nouveaux `FileDiffMetadata`) → `React.memo(FileSection)` cassait → `@pierre/diffs` **reconstruisait le DOM du diff** (le flash).

Le modèle de DONNÉES backend reste inchangé (même bridge `review_*` / `repo_diff`, même façade MCP agent, PDR-0005) : on ne change QUE la couche de gestion d'état côté renderer.

## Decision

- **Ajouter `@tanstack/react-query` (`5.101.2`)** au renderer et migrer les **5 hooks review** vers `useQuery`/`useMutation`, en conservant TOUS les invariants review (spec-scoping #41/#53, résolution humaine seule, remap living-HEAD, non-lus temps réel, reply agent MCP, pinned frozen #43, perf mono-fichier #55).
  - **`QueryClientProvider` au root** (`app.tsx`) sur un `queryClient` singleton (`lib/query.ts`), ET le **même client dans le CONTEXTE du router** (`createRootRouteWithContext<{ queryClient }>` + `createRouter({ context: { queryClient } })`, `router.tsx`) — l'intégration officielle TanStack Query ↔ Router, qui ouvre la porte à un loader `ensureQueryData` (préchargement) sans le câbler dès maintenant.
  - **queryKeys scopées** `(workspaceId, repositoryId, specId)` (`reviewKeys`, `lib/query.ts`) — le spec-scoping vit DANS le cache : deux Specs partageant un repo, ou deux repos d'un workspace, ne partagent jamais une entrée. Le segment de tête `"review"` permet d'invalider TOUTE la couche en un appel.
  - **`useQuery` + `placeholderData: keepPreviousData` + structural sharing (défaut).** Le structural sharing (`replaceEqualDeep`) rend la MÊME référence quand le re-fetch renvoie un contenu deep-equal. **C'est le fix du flash, à la source** : un `repos://changed` qui ne change pas le code → `use-repo-diff` re-fetch → même identité `RepoDiff` → `patchByFile` stable → `FileSection` bail-out → `@pierre` early-return (zéro reconstruction DOM). `keepPreviousData` garde le dernier diff/feed à l'écran pendant un switch repo/target (pas de flicker vide).
  - **`useMutation` OPTIMISTIC** (post/reply/resolve/reopen ; promote/validate/markRead) : `onMutate` (`cancelQueries` → snapshot `getQueryData` → `setQueryData` optimiste), `onError` (rollback via le snapshot), `onSettled` (`invalidateQueries`). L'item apparaît/change INSTANT ; le round-trip réconcilie ensuite (structural sharing garde les identités stables au passage). Les méthodes publiques ne REJETTENT jamais (l'erreur est surfacée via `error`, l'ancien contrat).
- **UNE souscription globale `repos://changed`** (`<ReviewLiveSync>`, montée une fois sous le `QueryClientProvider`) → `queryClient.invalidateQueries` sur le namespace `review`, ce qui **dédupe le double-refetch** (React Query re-fetch chaque query ACTIVE une seule fois). Les 5 souscriptions par-hook disparaissent.
  - **L'invariant « pinned = frozen » survit** (#43) : un diff épinglé (commit / range explicite) est tagué `meta: { frozen: true }` + `staleTime: Infinity` ; le prédicat d'invalidation SAUTE ces queries. Un push re-pull les diffs living + comments/tasks/unread mais laisse un diff pinned intact (pas de watch, pas de re-pull). `use-repo-diff` garde le `repo_watch`/`repo_unwatch` (living seulement) pour que le backend émette les pushes.
- **`@pierre` inchangé, `DiffView` inchangé** côté flux de données : il reçoit toujours `diff` en prop. Une **repro NON-mockée** du rendu du diff avec le vrai `@pierre` (`diff-view-pierre-repro.test.tsx`) garde le harnais honnête (un throw runtime au montage réel casserait le test).
- **Version épinglée `^5`** résolue en `5.101.2` (React 19-compatible). `bun install` colocalise ses transitives (`@tanstack/query-core`).

## Consequences

- La review est **instantanée** (optimistic) et **ne clignote plus** (structural sharing → identité `diff` stable → `@pierre` early-return). Le harnais e2e le PROUVE : une mutation resolve qui ne touche pas le code n'entraîne PAS de reflow global du diff (`window.__nyxFileSectionRenders` : delta ≪ nombre de fichiers, diff non re-monté), et le flip resolved est optimiste (≪ le round-trip 2-3 s d'avant). Un test unitaire non-mocké prouve l'optimism (l'item passe resolved AVANT que le backend gaté ne réponde).
- Moins de code de plomberie (plus de `useState`/`useRef`/souscriptions/`load()` manuels par hook), un cache unifié, et le préchargement par loader désormais possible via le contexte router.
- La migration est **scopée à la review** (pas app-wide — chantier séparé). Les autres surfaces gardent leur gestion d'état actuelle ; elles pourront migrer sur le même `queryClient` plus tard.
- Les tests de hooks review tournent sous un `QueryClientProvider` + le vrai `<ReviewLiveSync>` (`lib/test-query.tsx`), donc les chemins « reply agent live » / « unread monte » / « living diff re-pull » passent par le code de PROD, pas un raccourci de test.
