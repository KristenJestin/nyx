# PDR-0028 - Rendu review v2.5 : adoption de `@pierre/diffs` + `@pierre/trees` (renderer, offline)

**Status: validated**

Introduit les deux dépendances structurantes du **rendu de la review v2.5** dans le renderer Electron (`apps/desktop/renderer/package.json`) : **`@pierre/diffs` (`1.2.12`, = diffs.com)** pour le rendu du diff/review (coloration syntaxique Shiki + virtualisation + annotations de ligne) et **`@pierre/trees` (`1.0.0-beta.5`, = trees.software)** pour l'arbre de fichiers changés. Décision utilisateur actée à la revue du build précédent (le prototype de référence utilise ces libs ; on adopte le design validé plutôt qu'un rendu diff/arbre custom). Sert de référence ADR pour l'adoption de ces deux libs de rendu, dans la discipline établie par PDR-0026/0027 (une dépendance structurante = un PDR dédié nommant les paquets + les versions).

## Context

v2.5 place la review dans son placement validé (le prototype) : l'onglet Review du Spec montre les **commentaires** + un bouton « Open review », et la **page plein écran** (`/work/$specId/review`, PDR-0028) porte le viewer — gauche l'arbre de fichiers, centre le diff, droite les commentaires fermables. Le round précédent avait réécrit un rendu diff/arbre **100 % custom** (viewer virtualisé maison + un lexer coarse dans un worker maison + une liste de fichiers maison). L'utilisateur a tranché : **adopter les libs du prototype** (`@pierre/diffs` / `@pierre/trees`) pour le RENDU, afin d'obtenir un diff diffshub-style (split/unified, coloration réelle Shiki, annotations inline) et un arbre trees.software sans réinventer ces surfaces.

Le modèle de DONNÉES reste inchangé : la review est une **vue dérivée** du `repo_diff` calculé par `nyx-core` (PDR-0015/0027), servie identiquement au renderer et à l'agent MCP (PDR-0005). Les libs **rendent** ; la donnée vient du sidecar. Deux contraintes de discipline pesaient sur l'adoption :

- **Offline pur au runtime.** L'app est locale et chargée sous Electron en `file://` (`loadFile`) ; le rendu de la review ne doit faire **aucun appel réseau** à l'exécution (pas de CDN de grammaires/WASM/thèmes). Le bundling Vite doit tout embarquer localement.
- **Bundling Vite/Electron du worker.** `@pierre/diffs` pousse la coloration Shiki dans un **worker** (`@pierre/diffs/worker/worker.js?worker`) qui code-splitte ses grammaires + le moteur Oniguruma. Il fallait vérifier que ce worker bundle proprement dans le renderer Electron.

## Decision

- **Adopter `@pierre/diffs` (`1.2.12`) + `@pierre/trees` (`1.0.0-beta.5`)** comme couche de RENDU de la review v2.5 (renderer uniquement ; `nyx-core` inchangé), en REMPLAÇANT le rendu diff/arbre custom du round précédent (retrait de `components/review/{diff-rows,tokenize,use-diff-highlighter,highlight.worker}` + réécriture de `diff-view`, `file-tree-panel`, `diffs-worker`). Pas de double implémentation morte.
  - **Diff** : le `RepoDiff` (hunks parsés par l'engine gix, PDR-0027) est sérialisé en **patch unifié par fichier** (`components/review/diff-patch.ts`) et rendu par `@pierre/diffs` `PatchDiff` (`WorkerPoolContextProvider` + le worker Shiki). On n'a PAS besoin des contenus de fichiers complets (que le bridge ne porte pas) : le patch est un round-trip fidèle des hunks de l'engine. Les commentaires ancrés + les composers vivent en **annotations de ligne** `@pierre/diffs` (`lineAnnotations` + `renderAnnotation` + gutter « + »).
  - **Arbre** : `@pierre/trees` `FileTree` + `useFileTree`, alimenté par les chemins changés + un `gitStatus` dérivé du `RepoDiff` (`diff-patch.ts` → `GitStatusEntry[]`).
- **Bundling & offline vérifiés (le risque #1).** `bun run build` (dans `apps/desktop/renderer`, = `tsc && vite build` prod) est **vert** avec les libs, et le rendu est **100 % offline** :
  - le WASM Oniguruma est **inliné en base64** dans le worker (`atob(...)`, pas de `fetch`) et émis en chunk LOCAL (`assets/wasm-*.js`) ;
  - les grammaires Shiki sont résolues depuis `bundledLanguages` (imports dynamiques locaux) → chunks LOCAUX (`assets/typescript-*.js`, `tsx-*.js`, …) ;
  - **aucun `fetch(` / CDN** dans le bundle (seules chaînes « http » : des commentaires de grammaire + l'URL du décodeur d'erreurs React — pas de chargement réseau).
- **Config Vite requise : `worker: { format: "es" }`** (`apps/desktop/renderer/vite.config.ts`). Le worker `@pierre/diffs` **code-splitte** (import dynamique de `shiki/wasm`), ce que le format worker `iife` par défaut de Vite REJETTE (« UMD and IIFE output formats are not supported for code-splitting builds »). Le worker ES-module se charge sous `file://` dans le renderer Chromium d'Electron (base `./` → URL relative). C'est la SEULE adaptation de build ; les deux workers custom retirés étaient déjà des ES-module workers, donc le passage en `format: "es"` n'affecte aucun autre usage.
- **Versions épinglées EXACT** (`1.2.12` / `1.0.0-beta.5`) — la convention du dépôt pour les betas / paquets 0.x (tout rebump est délibéré, cf. PDR-0026) ; le caret est retiré. `bun install` colocalise leurs transitives : `shiki`, `@shikijs/transformers`, `diff`, `hast-util-to-html`, `lru_map`, `@pierre/theme`, `@pierre/theming`, et — **tirées par `@pierre/trees`** — `preact@11.0.0-beta.0` + `preact-render-to-string@6.6.5` (le rendu SSR interne de l'arbre `@pierre/trees`).
- **Thème** : la review suit la classe `.dark` de l'app (`pierre-dark` / `pierre-light`) ; on ne colle pas les pixels du prototype — le look v2 vient des tokens du design system (chrome autour du diff/arbre), les libs ne pilotent que le rendu du code coloré + l'arbre.

## Consequences

- Le rendu diff/arbre n'est plus maintenu maison : moins de code (le viewer virtualisé + le lexer coarse + le worker maison disparaissent), coloration réelle multi-langages, split/unified, annotations inline « gratuites ».
- Un coût de bundle : les grammaires Shiki sont code-splittées en chunks à la demande (chargés localement, jamais réseau) ; le WASM inliné pèse ~620 kB (chunk local, chargé par le worker au premier diff).
- Toute évolution de version de ces deux libs 0.x/1.x reste un rebump délibéré (revalidation du bundling offline), dans l'esprit de PDR-0026.
