# nyx — Master context (genèse, contrat, mémoire de conception)

> **But de ce fichier.** C'est la mémoire d'une longue session de conception. Une
> nouvelle session Claude qui lit ce fichier doit devenir **un clone** avec tout
> le contexte : le pourquoi, les décisions actées, les pistes rejetées (et
> pourquoi), les faits techniques vérifiés, et la roadmap. Lis-le en entier avant
> d'agir. **Statut : conception figée, prêt à implémenter (PRD en draft dans
> depot, milestone v1).**

---

## 0. Comment se comporter (le style de travail attendu)

- **Honnêteté brutale, zéro bullshit.** Si l'utilisateur dit une connerie, dis-le.
  Si une meilleure solution existe, propose-la. Si un truc est infaisable ou
  pourri, dis-le. **Ne vends JAMAIS du rêve** ("ça va marcher mieux" sans preuve).
  L'utilisateur a explicitement, plusieurs fois, demandé ça — il en a marre des
  promesses molles.
- **Décisif.** Donne des recommandations tranchées, pas des inventaires
  interminables. Quand tu peux acter, acte.
- **Distingue les couches.** Beaucoup de douleur est venue de confondre le
  *rendu* terminal et la *logique*. Garde ça net.
- L'utilisateur est **très technique** : il dev **depot** (un outil de gestion de
  PRD piloté par agents), bosse sur **PalBank**, utilise **Claude Code** + Warp.

---

## 1. Pourquoi nyx existe (l'essence)

nyx est un **gestionnaire de terminaux orienté workflow agentique** (Claude Code +
depot + monorepo + worktrees + portless). **Ce n'est PAS un terminal** — le
terminal est une **commodité** (xterm.js). La **valeur unique** = la couche
d'**orchestration** que personne ne fait à sa façon.

Né des douleurs de **Warp** :
- **Multi-tab le bordel** : difficile de retrouver/naviguer ; obligé de
  color-coder, nommer groupes/terminaux pour savoir à quoi ça sert. Pas pratique.
- **Claude Code perd la session** quand on ferme le terminal / éteint le PC.
  `--continue` est **ambigu** quand plusieurs sessions tournent dans le même
  dossier. Le message de reprise n'apparaît pas toujours → on perd l'ID de session
  → très chiant à chaque redémarrage.

> ⚠️ **Leçon clé (ne pas re-looper) :** 4 sessions ont été cramées à polir le
> *rendu* (xterm canvas → wterm DOM → blocs). **Mauvais bout.** Le rendu est un
> problème résolu. **On ne touche plus au moteur de rendu.** La valeur est dans
> l'orchestration.

---

## 2. Le workflow de l'utilisateur (CONTEXTE — à NE PAS hardcoder dans nyx)

nyx doit être **générique** : marcher pour **n'importe quoi**, et **aussi** pour
PalBank. **Aucune** logique PalBank/depot/feature-start câblée dans nyx.

- **PalBank** = monorepo multi-projets : `pfm-palbank` (front Angular),
  `pfm-palbank-api` (back NestJS), + packages `pfm-common` / `pfm-api-common` /
  `pfm-palbank-common` (+ `-docs`). Pour qu'il tourne il faut aussi 2-3 API annexes
  (auth, file…).
- **Dev via depot** (outil PRD de l'utilisateur), **un worktree par PRD**.
- **`feature-start` / `feature-start-depot`** = skill de l'utilisateur qui copie
  les 5 projets dans un **dossier "feature" = un FAUX worktree** (pas un vrai
  `git worktree`), avec toute la racine, pour dev une feature isolée.
- **portless** (https://portless.sh) = reverse-proxy HTTPS, `name.localhost` →
  port random (PORT env) ; on **wrappe** la commande (`portless api.x pnpm start`).
  **Les worktrees git auto-préfixent le sous-domaine par branche** → URLs isolées
  par feature, gratis.
- Config Warp "parfaite" qui lance les services (API annexes + front + back),
  **dupliquée par feature-start** pour lancer simplement.

---

## 3. Les exigences (le produit) + verdicts honnêtes

1. **Lancer plusieurs terminaux.** ✅ trivial.
2. **"Project" mappé à un dossier**, terminaux rattachés. ✅
3. **Commandes managées** = lancer les services (les configs Warp). ✅
4. **Terminaux gardent l'historique** (scrollback persisté). ✅
5. **Garder l'ID de session + reprise auto au redémarrage.** ⚠️ recadrage
   honnête : on **ne garde pas un process vivant** à travers un reboot (impossible
   sans démon). On **enregistre** par terminal `{cwd, session_id Claude, état}` et
   au redémarrage on fait **`claude --resume <id-exact>`** → **non-ambigu** (≠ le
   `--continue` cassé de Warp). C'est "record + resume", pas "keep alive".
6. **Dossiers "futurs"** = worktrees (vrais git OU faux feature-start) sous le
   projet, avec commandes en plus. ✅ → modèle **multi-root, git-agnostic**.
7. **MCP pour que l'agent pilote/voie les serveurs DANS nyx.** ✅ **le truc
   novateur.** Aujourd'hui l'agent lance les serveurs dans un shell invisible
   (PID-hunting, bordel). Avec nyx : les services sont des **commandes managées** ;
   l'agent les **start/stop/relaunch + lit la sortie via MCP**, exécutées **dans
   nyx** (l'utilisateur voit + garde la main, même interface).

---

## 4. L'ARCHITECTURE — le contrat (toutes les décisions actées)

| Sujet | Décision |
|---|---|
| Shell applicatif | **Tauri 2** (Rust). Choix ferme de l'utilisateur (≠ Wails v3 alpha qui l'a fait galérer). NB : Tauri reste un **webview** (WebKitGTK via WRY sur Linux) — ça ne corrige PAS les bugs webview, ça corrige le **framework/shell** (mature, écosystème). |
| Rendu terminal | **xterm.js v6 + addon WebGL (`@xterm/addon-webgl`) + `react-xtermjs`** (wrapper React maintenu par Qovery ; les vieux `xterm-for-react`/`react-xterm` sont morts). **GPU dans le webview, gratis.** **Pas de blocs riches sur le live.** |
| Front | **React** (ferme — "on reste sur du rendu React quoi qu'il arrive"). |
| PTY | **`portable-pty`** (crate Rust d'Alacritty/wezterm). Léger. |
| App | **Single-instance** (1 process, N **fenêtres**) → 1 serveur MCP, **port fixe**. Lancer un 2e nyx → focus le 1er (plugin single-instance Tauri). |
| Persistance | **SQLite** (projets/workspaces/terminaux/commandes/session-ids/scrollback). **Record + resume** : close/reboot → réouverture → `claude --resume <id>`. **Warning à la fermeture si une session claude tourne** (conscience + contrôle, sans démon). |
| Capture session Claude | **via le plugin** nyx (event `SessionStart`) + un **`NYX_TERMINAL_ID`** injecté par terminal → corrélation `{terminal_id, session_id}` non-ambiguë même à cwd partagé. **Le record SQLite de nyx = l'autorité** (on ne dépend PAS de SessionEnd). |
| MCP | **hébergé par nyx** (transport **HTTP**, **port fixe**), **enregistré une fois à l'onboarding** (`claude mcp add --scope user`, dans `~/.claude.json`). Outils = lifecycle des commandes managées, **granularité projet/dossier**. **v1.** |
| portless | **NON intégré.** nyx lance tes **chaînes de commande** telles quelles (qui peuvent contenir `portless …`). Bénéfice de composition uniquement (worktrees + portless = URLs isolées). |
| feature-start | **nyx ne wrappe RIEN.** Voir §6 (multi-root) : nyx est générique, expose `nyx workspace add` ; c'est l'outil de l'utilisateur qui appelle nyx (inversion), pas l'inverse. |
| Blocs (Warp-style) | **ABANDONNÉS.** Source de toute la douleur. Aucune exigence n'en a besoin. |

---

## 5. Le modèle de données (générique, git-agnostic)

```
Projet = un NOM + N workspaces.   (par défaut : 1 workspace "root")
 ├─ commandes (DÉFINITIONS au niveau projet : front, back, API annexes… = le "template")
 └─ workspace = JUSTE UN DOSSIER (git-agnostic ; la branche git = métadonnée optionnelle)
     ├─ terminaux
     └─ instances de commandes (les défs dupliquées, lancées à CE path)
```

- **Git-agnostic** : un workspace = un dossier. Vrai worktree git, faux worktree
  feature-start, ou dossier quelconque → **traités à l'identique**. Si c'est un
  repo git, nyx lit la **branche** (label + portless). Sinon, juste le dossier.
- **Commandes** définies au niveau **projet**, **instanciées par workspace**.
  Créer un workspace **duplique** les commandes en instances (lançables à son path
  → portless isole les URLs par branche). = la "config Warp dupliquée par feature".
- **Lancement des commandes : via l'UI ET via MCP** (même état, l'utilisateur
  garde la main).
- **UI** : on voit le **projet** ; 1 seul workspace → juste le root ; plusieurs →
  root + workspaces **nommés**.
- **Association dossier→projet** = **enregistrée en SQLite** par nyx. Sources :
  (a) **manuel** (ajouter un dossier à un projet dans l'UI) ; (b) **auto-détection
  git** (proposer les `git worktree` d'un repo) ; (c) **auto-attach par cwd**
  (OSC7) ; (d) **inversion** : nyx expose **`nyx workspace add <path> --project X`**
  (CLI/MCP) que l'outil de l'utilisateur (feature-start) appelle s'il veut. **nyx
  ne crée/wrappe rien de spécifique.**
- **`create_workspace` via MCP** = le **générique** : entrée workspace + (si repo
  git) `git worktree add` + dup des commandes. Le **contenu multi-repo** (les 5
  projets de PalBank) **n'est pas** le job de nyx → feature-start crée le dossier
  et appelle `nyx workspace add`. Deux outils MCP/CLI : `create_workspace`
  (générique) et `workspace_add <path>` (enregistrer un dossier existant).

---

## 6. Faits Claude Code VÉRIFIÉS (via la doc, juin 2026) + pièges

**Capture/reprise de session :**
- Hook **`SessionStart`** reçoit `{session_id, cwd, transcript_path, hook_event_name,
  source}` ; `source` ∈ {`startup`,`resume`,`clear`,`compact`}. → un hook peut
  écrire le `session_id` de façon fiable au démarrage. **SÛR.**
- Sessions stockées : `~/.claude/projects/<hash-cwd>/<session-id>.jsonl`. **SÛR.**
- Reprise : **`claude --resume <session-id>`** (ID exact). `--continue` = la plus
  récente (≠ choisir une précise). **Pas de flag pour forcer un `--session-id` à
  la création** → on capture **après coup** (hook) ou via `claude -p --output-format
  json | jq .session_id` (headless). **SÛR.**
- **Alternative SANS hook** (si on veut zéro config Claude) : nyx **file-watch**
  `~/.claude/projects/<hash-cwd>/` ; nouveau `<id>.jsonl` après un `claude` lancé →
  c'est l'ID, corrélé par terminal/timing. Moins précis si 2 sessions démarrent au
  même instant. **Décision actée : on passe par le PLUGIN (event), pas le
  file-watch, et PAS dans `settings.json`.**
- Hook **`SessionEnd`** : `{session_id, cwd, transcript_path, reason}` ; `reason` ∈
  {`clear`,`resume`,`logout`,`prompt_input_exit`,`bypass_permissions_disabled`,
  `other`}. ⚠️ **PIÈGE : ne fire PAS de façon fiable** sur kill brutal
  (SIGKILL / terminal parent fermé / app tuée) ; le travail async du hook est tué
  avant la fin (issues #41577). → **NE PAS s'y fier. Le record SQLite de nyx fait
  autorité** (SessionStart le pose, SessionEnd le nettoie au cas propre, et le
  cycle de vie terminal/shutdown de nyx couvre le reste — nyx snapshot son état à
  sa propre fermeture).

**Plugins / hooks :**
- Un **plugin** peut embarquer des hooks (`hooks/hooks.json`). ⚠️ **PIÈGE** : bug
  connu #51420 — les hooks *plugin-scope* peuvent **arrêter de fire en cours de
  session**. MAIS ça concerne les hooks qui fire **en boucle** (Stop, par turn).
  **`SessionStart` fire UNE fois au début → non concerné.** → **hooks via le
  plugin = OK** (à valider par un test). **Décision : tout dans le plugin, on ne
  touche pas `~/.claude/settings.json`.**

**MCP (client Claude Code) :**
- Transports : **stdio / HTTP / SSE** (pas de websocket). Pour "app héberge,
  agent se connecte" → **HTTP**. **SÛR.**
- Scopes : `local` (`~/.claude.json` par projet) / `project` (`.mcp.json` du repo)
  / **`user` (`~/.claude.json` top-level, tous projets)**. On utilise **user**
  (install once, 0 fichier par projet, rien à nettoyer). Approbation **one-shot**.
- ⚠️ **`.mcp.json` lu au DÉMARRAGE de session uniquement** (pas de hot-reload) →
  enregistrer le MCP **avant**, pas à la volée.
- ⚠️ **Port variable = réécriture** (pas de `${PORT}`) → **port FIXE.**
- ⚠️ **MCP down** (nyx pas lancé) : Claude tente la connexion (timeout ~30s,
  `MCP_TIMEOUT`), puis l'outil est **absent** (dégradation OK en HTTP ; le SSE a
  des crashs signalés → on prend HTTP). Comme on lance claude **depuis** nyx, nyx
  tourne → non-événement. Garder le timeout court. **À TESTER empiriquement.**

---

## 7. Roadmap v1 — 6 PRD (ordre = ordre de dev)

> Dans depot, **draft**, **milestone v1**, **numérotés dans le titre**. **PAS de
> tâches** (l'utilisateur les fera 1 par 1).

- **PRD 0 — Socle** : Tauri 2 + Rust + React + xterm.js (WebGL) + `portable-pty` +
  **single-instance**. **1 terminal nu** qui marche (vrai shell, env, historique,
  **sans flash**). = aussi le spike qui valide que la douleur de rendu meurt.
- **PRD 1 — Multi-terminal + tabs + persistance scrollback** : plusieurs terminaux,
  navigation, scrollback persisté, re-spawn au lancement (cwd).
- **PRD 2 — Projects folder-anchored + multi-root** : projets→dossiers, workspaces
  (root + nommés, git-agnostic), auto-attach (OSC7), `git worktree` auto-detect,
  `nyx workspace add` (CLI). (reprend l'esprit de l'ancien PRD "projects v2" axe 1.)
- **PRD 3 — Commandes managées (services)** : défs au niveau projet, instances par
  workspace, start/stop/relaunch, dot d'état (idle/running/success/error), sortie
  read-only, import `package.json`, lancement UI **et** MCP. (axe 2 de projects v2.)
- **PRD 4 — Persistance & reprise des sessions Claude Code** : plugin nyx
  (SessionStart event) + `NYX_TERMINAL_ID`, record SQLite, `claude --resume` au
  redémarrage, warning à la fermeture si session active. **La feature-tueuse.**
- **PRD 5 — MCP agent** : serveur MCP HTTP (port fixe) hébergé par nyx, enregistré
  une fois à l'onboarding (user scope), outils = lifecycle commandes managées +
  lecture sortie (granularité projet). **Le novateur.**

---

## 8. Pistes REJETÉES (et pourquoi — pour ne PAS re-looper)

- **wterm (DOM)** : rendu DOM → flash + perf + double-moteur. Le PRD de migration
  wterm a **prouvé** que le rendu DOM tape le même mur que le canvas. **Abandonné.**
- **`@wterm/ghostty`** : c'est libghostty-vt (parser) **branché sur le renderer DOM
  de wterm** → upgrade la *correction* (Unicode/SGR/parité CJK/truecolor) mais
  **rien** sur flash/perf/blocs (même rendu DOM). Mauvaise couche.
- **Le flash "rendu en haut puis tp en bas"** = problème de **layout/ancrage +
  freeze-respawn** dans le FRONT, **pas** d'émulation. Changer de renderer ne le
  règle jamais (= la boucle de 2 semaines). Avec xterm.js normal (scroll natif),
  pas de flash.
- **Back natif (émulateur en Go/Rust)** : `alacritty_terminal` / `charm/x/vt` /
  `libghostty-vt` existent et sont sains, MAIS le **rendu GPU custom (façon Warp,
  "blocs comme on veut") est du NATIF** (GPUI/Ghostty/Sugarloaf) → **incompatible
  avec un webview**. Dans un webview : canvas (opaque, pas de déco DOM) OU DOM
  (lourd). On **reste React/webview** → donc **xterm.js**, pas de blocs GPU.
- **Écrire son propre renderer GPU** : c'est un **moteur de rendu de texte**
  (shaping HarfBuzz, font fallback CJK/emoji, atlas…) = des **équipes-années**.
  Tout le monde délègue (swash/cosmic-text/HarfBuzz). **Ne pas faire.**
- **Daemon + GUI clients** (vs single-instance) : son seul gain = garder les
  process vivants à la **fermeture de la fenêtre** (le reboot tue tout de toute
  façon → resume dans les deux cas). Coût : **process invisibles en fond** (conso
  API Claude sans visibilité) + grosse plomberie. **Rejeté pour v1** (instinct
  utilisateur juste). Single-instance + record/resume + warning suffit. (Daemon =
  éventuel v1.1.)

---

## 9. À tester / différé

- **À tester empiriquement** (items de test, pas des blocages) : SessionStart via
  plugin fire bien (pas touché par #51420) ; comportement exact si MCP down ;
  reload `/mcp` en session.
- **Différé v1.1+** : daemon (survive-window-close vivant) ; lien optionnel
  workspace ↔ PRD depot ; thèmes ; **shell par défaut sélectionnable dans les
  options** (Windows : PowerShell / cmd / Git Bash ; Unix : bash / sh / zsh… —
  aujourd'hui : `$SHELL` puis défaut natif par OS, cf. `pty.rs::resolve_shell`) ;
  etc.

---

## 10. État depot

Projet depot **« nyx »** — ⚠️ collision : c'est aussi le projet de l'**ancien
nyx/bterm** (vieux PRD : migration wterm…). Les **6 PRD v1** sont (ou seront) sous
**milestone `v1`** pour les séparer du legacy. (À trancher avec l'utilisateur : un
seul projet + milestone, ou un projet séparé.) Worktree de référence de l'ancien
code : `/home/kris/Projects/bterm` (+ `_worktrees/wterm-migration`, branche
`wterm-migration`, commit `127d6f6` — l'archive `nyx-wterm-export.tar.gz` existe).
**Le nouveau code part de zéro dans `/home/kris/Projects/nyx`.**

---

## Conventions de code

> Conventions durables du repo (front nyx). À appliquer à chaque PRD ; le
> standards-auditor lit cette section. Concis et actionnable — voir les exemples
> liés dans le codebase.

### 1. Noms de fichiers en kebab-case

Les fichiers sources sont nommés en **kebab-case** (`terminal.tsx`, `use-pty.ts`,
`app.tsx`). L'**identifiant exporté garde sa casse idiomatique** — le composant
React reste `Terminal` (PascalCase), le hook reste `usePty` (camelCase) ; seul le
FICHIER est en kebab-case.

- `Terminal.tsx` → `terminal.tsx`, `usePty.ts` → `use-pty.ts`, `App.tsx` → `app.tsx`.
- Les fichiers de test suivent le fichier source : `terminal.test.tsx`,
  `terminal.browser.test.tsx`.

### 2. Pas de valeur Tailwind arbitraire quand un token d'échelle existe

N'utilise **pas** de valeurs arbitraires entre crochets (`p-[10px]`, `w-[42px]`, …)
quand un token de l'échelle Tailwind couvre le besoin. Tailwind lui-même prévient
("`p-[10px]` can be written as `p-2.5`").

- `p-[10px]` → `p-2.5` (= 0.625rem = 10px). Vise le token d'échelle le plus proche.
- Les valeurs arbitraires ne sont acceptables que pour des tailles réellement
  hors-échelle, ponctuelles et sans token — ce qui doit rester rare et justifié.

### 3. Pas de couleurs en dur — palette CSS du design-system (+ dark mode)

Les couleurs viennent de la palette du design-system (variables CSS / tokens
shadcn dans `src/globals.css`, exposées en classes Tailwind `bg-background`,
`text-foreground`, `bg-card`, …). Ne hardcode **jamais** une couleur hex/rgb dans
le markup ou le code des composants.

- `bg-[#0a0a0a]` → `bg-background` ; choisis le token sémantique, pas une couleur brute.
- **Le dark mode est actif** : `<html class="dark">` dans `index.html` sélectionne
  la palette `.dark` de `globals.css`.
- Les couleurs consommées EN DEHORS de Tailwind (ex. le thème du canvas xterm.js)
  doivent aussi dériver des variables CSS **au runtime**, pas de valeurs en dur.
  Les tokens sont écrits en `oklch(...)`, que la plupart des consommateurs non-CSS
  (xterm) ne savent pas parser → il faut les convertir en chaîne `#rrggbb`. La
  conversion robuste, agnostique à l'espace de couleur : lire le token
  (`getPropertyValue`) puis le passer à **`chroma(raw).hex("rgb")` de la lib
  `chroma-js`**, qui parse l'oklch (et hex/named/rgb/…) et rend la chaîne sRGB
  `#rrggbb` finale en JS pur (jette si non parseable → on tombe sur le fallback).
  **Les raccourcis évidents NE marchent PAS** : sur Chromium/WebKit actuels,
  `getComputedStyle(el).color` et le `ctx.fillStyle` du canvas **resérialisent**
  l'espace de couleur AUTEUR (CSS Color 4) et rendent un token oklch tel quel
  comme chaîne `oklch(...)` — ils ne downconvertissent pas — donc xterm le
  rejette. D'où chroma-js, et pas l'astuce naïve via le moteur. Construis ces thèmes
  **au mount** (un vrai DOM est requis pour lire les tokens), avec un **fallback
  sain** pour qu'une résolution échouée ne donne jamais un résultat illisible (ex.
  noir-sur-noir). Voir `src/components/terminal/terminal.tsx` (`resolveCssColor` /
  `resolveThemeFromCss`).

### 4. La baseline de régression visuelle n'est PAS commitée

La baseline de screenshots du Browser Mode de Vitest est une **aide de dev locale
uniquement**. Garde la baseline binaire hors de l'arbre source et du diff git.

- Elle vit dans le dossier racine gitignoré `.vitest-screenshots/` (configuré via
  `browser.expect.toMatchScreenshot.resolveScreenshotPath` dans `vitest.config.ts`),
  jamais sous `src/`.
- Sur un checkout neuf / CI le dossier est absent → le test régénère la baseline au
  premier run et ne détecte aucune régression. Le diff visuel n'a donc de sens que
  sur une machine ayant déjà lancé la suite une fois.
