# PDR-0026 - Stack terminal : wterm (@wterm/dom) + core ghostty en remplacement de xterm, store no-host

**Status: validated**

Introduit des dépendances (`@wterm/core`, `@wterm/dom`, `@wterm/ghostty`, `@tanstack/react-store`) dans `apps/desktop/renderer` et retire la famille xterm (`@xterm/xterm`, `@xterm/addon-fit`, `@xterm/addon-serialize`, `@xterm/addon-webgl`, `react-xtermjs`). Sert de référence ADR pour la bascule de stack terminal de v2.2.5.

## Context

v2.2 rendait les terminaux du cockpit sur **xterm** (rendu WebGL). Deux problèmes structurels sont apparus une fois le shell de navigation multi-pages posé (PDR-0025, routeur client-only + memory history) :

- **Navigation destructrice** : le routeur démonte les composants de route à chaque changement d'écran. Avec xterm, cela détruit l'instance — on perd le buffer et, pire, l'écran d'un TUI (vim/htop), dont le contenu vit dans l'alt-screen. Chaque navigation perdait du contexte de travail.
- **Restart non lossless** : rejouer le scrollback après un redémarrage de l'app ne restaure pas un TUI — son contenu est dans l'alt-screen, hors scrollback.
- **Coût WebGL** : ~660 lignes de contournements WebGL (re-attach du contexte, addon webgl, garde-fous de géométrie) pour un rendu canvas peu adapté au keep-alive et à l'a11y.

Un **spike** a évalué wterm (rendu **DOM**, état dans un **core WASM**) et validé qu'il tient **sans host de terminaux persistant** : parce que l'état vit dans le core WASM et que le DOM n'est qu'une projection rAF, on peut garder les instances dans un store module-level et **détacher/réattacher le nœud DOM** au fil de la navigation sans rien perdre. Charge validée : 50 cycles detach/reattach lossless (buffer, alt-screen, focus intacts), 15+ terminaux avec mémoire plate, writes reçus en état détaché, jank mineur seulement en blast simultané pathologique. Réf : Q-006 ; spikes wterm.

## Decision

- **Adopter wterm `@wterm/dom` + core ghostty (`@wterm/ghostty`) en remplacement de xterm.** Une **seule** lib terminal — pas de seconde lib, pas de mini-core Zig seul.
  - Le **core ghostty** (~464 KB WASM, full VT) porte la **correctness VT** (Unicode/graphemes, SGR complet, alt-screen, truecolor). On écarte le mini-core Zig 12 KB bundlé de wterm (bugs CJK/couleur/glue).
  - Le rendu **DOM** de `@wterm/dom` donne keep-alive et a11y natifs et supprime les ~660 lignes de contournements WebGL.
- **PAS de host persistant** (validé inutile par le spike). Les instances vivent dans un **store module-level** — une `Map` plate impérative **hors React et hors du cycle du routeur** (`terminal-store.ts`). Une route ne fait qu'**attacher** le nœud DOM persistant de l'instance dans son pane au mount et le **détacher** au unmount ; l'instance et son état ghostty restent dans la `Map`.
- **Seules les métadonnées sont réactives** : les ids des instances vivantes et leurs titres (OSC 0/2) vivent dans un `Store` **`@tanstack/react-store`** (même famille TanStack que Router/Form/Hotkeys déjà présents), pour que les vues React s'abonnent à « quels terminaux existent / leurs titres » sans que les instances lourdes entrent dans l'arbre de rendu. Les instances sont lues **impérativement par id**.
- **`destroy()` n'est jamais appelé à la navigation** — uniquement à la fermeture réelle d'un terminal (`dispose` / `destroyTerminal` / `reconcileTerminals`). Le detach ne fait que `el.remove()`.
- **Garde-fous de production obligatoires (parce qu'il n'y a pas de host)** :
  - `autoResize: false` sur `WTerm` — l'observer intégré de wterm resize à 1×1 sur un box 0×0 (pane caché/détaché), ce qui détruit la grille.
  - **Sizing applicatif** via un `ResizeObserver` maison avec **garde `0×0`** (`hasUsableBox`) : un pane caché ou fraîchement réattaché reporte 0×0 → on skippe, jamais de resize destructif 1×1.
  - Ne **jamais** `destroy()` au unmount de route ; ne fermer le PTY / détruire le core qu'à la fermeture réelle.
- Le mécanisme detach/reattach est au niveau du **bridge `@wterm/dom`**, donc **core-agnostique** (re-confirmé sur ghostty à l'adoption).
- **S'appuie sur PDR-0025** (routeur déjà livré) : v2.2.5 ne reconstruit pas le shell ; le store no-host s'y branche et la page Terminals réattache les nœuds.
- **nyx neutralise les faiblesses de wterm par l'architecture sidecar** : scrollback = sidecar (plafond front ~1000 lignes hors-jeu) ; snapshot VT = sidecar ; backpressure = flow-control sidecar (v2.0). Le core front n'est qu'une projection live-screen + court backlog, pas l'archive.

## Alternatives écartées

- **Garder xterm + host de terminaux persistant** monté hors de l'`<Outlet/>` pour survivre à la navigation : rejeté. Le host persistant est jugé **inutile** (le spike prouve que le detach/reattach no-host suffit), il rajoute un point de montage global à câbler au routeur, et xterm ne résout pas le restart-lossless (alt-screen hors scrollback) ni le coût WebGL.
- **Mini-core Zig 12 KB seul** (bundlé par wterm) : rejeté pour bugs de correctness VT (CJK, couleur, glue). ghostty est retenu pour la correctness.
- **Deux libs terminal** (une pour le shell, une pour les TUI) : rejeté — complexité et incohérence de rendu. Une seule lib.
- **Déchargement LRU des instances** : hors périmètre — la charge validée (mémoire plate à 15+ terminaux) ne le justifie pas à ce stade.

## Consequences

- `apps/desktop/renderer/package.json` gagne `@tanstack/react-store`, `@wterm/core`, `@wterm/dom`, `@wterm/ghostty` et **retire** `@xterm/xterm`, `@xterm/addon-fit`, `@xterm/addon-serialize`, `@xterm/addon-webgl`, `react-xtermjs`.
- ~660 lignes de contournements WebGL et les modules xterm associés (`use-webgl-addon`, `terminal-geometry`, `xterm-theme`, etc.) sont supprimés.
- Le WASM ghostty (~464 KB) est traité comme un **asset de build first-class** : `@wterm/ghostty` le résout lui-même via `new URL("../wasm/ghostty-vt.wasm", import.meta.url)`, idiom que Vite comprend à l'intérieur d'une dépendance (copié/hashé dans le bundle, `base`-relatif — notre `base: "./"` le garde chargeable en `file://` Electron). Pas de deep-import du sous-chemin WASM.
- Base saine pour la phase sidecar (modèle d'écran VT headless sérialisable + re-hydratation au restart) et le gate e2e v2.2.5.

### Versioning : `@wterm/*` épinglés en EXACT (0.3.0)

Les trois paquets `@wterm/*` sont **épinglés en version exacte** (`"@wterm/core": "0.3.0"`, `"@wterm/dom": "0.3.0"`, `"@wterm/ghostty": "0.3.0"`) — **volontairement**, en **divergence assumée** de la convention caret (`^`) du reste du repo (y compris `@tanstack/react-store` co-introduit en `^0.11.0`).

Raison : c'est une **couche DOM TS jeune** (0.x), à **API instable** et **longévité incertaine**. On **fige la version exacte validée par le spike** pour garantir que le comportement audité (les 50 cycles detach/reattach lossless, l'alt-screen, la charge 15+) ne dérive pas silencieusement sur un patch/minor 0.x — où un changement de comportement peut casser le mécanisme no-host sans bump majeur. On rebumpera **délibérément** après re-validation, pas via un range flottant. `@tanstack/react-store` reste en caret car c'est une lib TanStack mature au versioning discipliné.

### Risque assumé & parachute

- **Risque** : la couche DOM TS de wterm est jeune ; l'upstream pourrait ralentir ou disparaître.
- **Parachute Apache-2.0** : `@wterm/dom` et le core ghostty sont sous licence Apache-2.0 → si l'upstream meurt, on **forke** DOM+ghostty (pas avant). L'architecture sidecar-centrée réduit d'autant la surface à maintenir côté front (scrollback/snapshot/backpressure sont déjà hors de la lib).
