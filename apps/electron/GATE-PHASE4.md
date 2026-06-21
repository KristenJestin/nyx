# GATE — Phase 4 : smoke Electron + re-validation de l'écho sur le vrai front

> Tâche depot : `01KVGHVR631AY8KANFGANWDFZV` (PRD `01KVFW30D2N525N19MFXCX54X2`).
> Ce document EST le rapport du gate : il porte la **commande de reproduction**, les
> **seuils attendus**, et un **emplacement pour coller les chiffres mesurés** sur la
> machine cible. Le gate ne peut être déclaré `done` qu'avec des chiffres collés
> ci-dessous, mesurés sur le matériel cible.

## Pourquoi ce gate ne peut PAS être validé ailleurs que sur la machine cible

Les seuils du gate sont définis pour **la machine de l'utilisateur** :
**Linux / Wayland / Hyprland, écran 2880×1800 @ 120 Hz**. Ils dépendent de propriétés
physiques de ce host :

- la **moyenne ≥ 110 fps** n'a de sens que sur un écran **120 Hz** — un host headless
  ou 60 Hz plafonne mécaniquement plus bas (et un compositeur offscreen ne reflète pas
  le rendu réel) ;
- l'**écho** dépend du PTY **Linux** réel (bash/zsh) et du compositeur Wayland natif ;
  sur Windows le PTY lance **PowerShell/cmd via ConPTY**, dont la latence d'écho n'est
  **pas comparable** à un PTY Linux.

→ Toute mesure prise sur le host de développement **Windows headless** est une
**sanity** (preuve que le harnais pilote le vrai chemin et produit des chiffres), **PAS**
la validation du gate. Seul un run `--mode=gate` sur la machine cible compte.

## Le harnais de mesure (reproductible, instrumenté, sur le vrai front)

`apps/electron/scripts/gate-echo-harness.cjs` boote la **stack réelle** :

- le **vrai `CoreHost`** (process Node dédié, `ELECTRON_RUN_AS_NODE`) + le **vrai
  relais `registerCoreIpc`** ;
- le **vrai renderer** (`dist/renderer/index.html`) chargé via le **vrai preload**
  (allowlist `window.nyxCore` deep-frozen, `contextIsolation`+`sandbox`) ;
- la **vraie PTY napi** (`nyx-napi`).

Une **sonde** est injectée DANS le renderer (`executeJavaScript`) et ne parle QUE
`window.nyxCore` — exactement la surface qu'utilise l'adaptateur Electron de
`apps/frontend`. Elle mesure, dans l'**horloge unique du renderer**
(`performance.now()`), le chemin **complet** :

```
frappe (renderer) → nyxCore.invoke('pty_write') → relais main → core-host (Node)
   → nyx-napi → PTY → le shell ECHO le marqueur
   → pump Rust → événement host → relais main → renderer `pty://output`
   → le marqueur est remis à xterm.write  (← t1 mesuré ici)
```

Caractéristiques (conformes à l'annexe §E du POC) :

- **timing piloté par rAF, pas `setTimeout`** : sous flood les macrotâches `setTimeout`
  sont starvées par le torrent IPC ; rAF garde la priorité du pipeline de rendu, donc la
  mesure ne hang pas sous flood ;
- **≥ 100 échantillons / condition** (défaut 120, override `NYX_GATE_SAMPLES`) — étend
  les 40 du POC ;
- 3 conditions : **(a) repos**, **(b) pendant une animation Motion** (40 cartes animées
  en continu : transform/opacity/scale/rotate par frame), **(c) sous flood régulé** (un
  flood `yes`-style à travers la **même boucle de crédit de flow-control** — `ptyAck`
  après consommation — que le vrai front) ;
- **FPS** : fenêtre de **10 s** (override `NYX_GATE_FPS_MS`) pendant la même animation,
  échantillonnée par rAF → moyenne fps + pire frame.

Sortie : un tableau lisible + un bloc **REPORT-JSON** (médiane/p95/min/max/moyenne par
condition, fps moyen, pire frame, dpr, résolution écran) entre les marqueurs
`REPORT-JSON-BEGIN` / `REPORT-JSON-END`. `--out <path>` (ou `NYX_GATE_OUT`) écrit aussi
le JSON sur disque.

Deux modes :

- `--mode=gate` (défaut) : **asserte les seuils**. À lancer **sur la machine cible**.
  Hors-cible il imprime les chiffres et **échoue** — cet échec est **attendu** et n'est
  PAS la validation.
- `--mode=sanity` : même mesure, **n'asserte aucun seuil**. Pour un run Windows/headless
  clairement étiqueté, sans aucune prétention sur le gate.

## ▶ Commande de reproduction — À LANCER SUR LA MACHINE CIBLE (Linux/Wayland 120 Hz)

```bash
# 1. construire main + renderer + napi (.node) + copies
bun run --filter @nyx/electron build

# 2. lancer le gate sur l'écran 120 Hz, en Wayland natif (≥100 échantillons/condition,
#    animation Motion de 10 s). Le run asserte les seuils et écrit le JSON.
cd apps/electron
electron scripts/gate-echo-harness.cjs --mode=gate --out gate-phase4-result.json
#   ou : bun run gate:echo

# Le run imprime un tableau + un bloc REPORT-JSON-BEGIN/END. Collez-les ci-dessous.
```

Pré-requis sur la cible : session **Wayland** active (le shell applique déjà
`ozone-platform-hint=auto` ; forçable `NYX_OZONE=wayland`), écran **120 Hz** courant,
et un **PTY Linux** (bash/zsh) par défaut.

## Seuils attendus (done_criteria, verbatim)

| Condition | Métrique | Seuil |
|---|---|---|
| Repos | écho médian | ≤ **5 ms** (≥ 100 échantillons) |
| Repos | écho p95 | ≤ **10 ms** |
| Animation Motion | écho médian | ≤ **5 ms** (≥ 100 échantillons) |
| Animation Motion | écho p95 | ≤ **10 ms** |
| Flood régulé | écho médian | ≤ **25 ms** (≥ 100 échantillons) |
| Flood régulé | écho p95 | ≤ **40 ms** |
| Animation Motion 10 s | moyenne fps | ≥ **110 fps** (écran 120 Hz) |
| Animation Motion 10 s | pire frame | **aucune** frame > **50 ms** |

Référence POC (annexe §E, machine cible) : repos ~1,0 ms / anim ~0,6 ms / flood ~18 ms,
120 fps tenus. Les seuils du gate sont donc atteignables avec marge sur la cible.

## ⬇ CHIFFRES MESURÉS SUR LA CIBLE — à remplir (collez le bloc REPORT-JSON ici)

> Statut : **EN ATTENTE D'UN RUN SUR LA MACHINE LINUX/WAYLAND 120 Hz.**
> Le host de dev est Windows headless : impossible d'atteindre/valider ces seuils ici.

```
Date du run     : __________
Host            : Linux/Wayland/Hyprland, ________ @120Hz, dpr ____
Commande        : electron scripts/gate-echo-harness.cjs --mode=gate
Sortie attendue : "GATE PASS — all thresholds met on this host."

--- coller le bloc REPORT-JSON-BEGIN … REPORT-JSON-END ci-dessous ---


--- coller le tableau THRESHOLD CHECKS (PASS/FAIL par critère) ci-dessous ---


```

Une fois ces chiffres collés ET tous les `THRESHOLD CHECKS` à `PASS`, le gate
`01KVGHVR631AY8KANFGANWDFZV` peut être débloqué puis passé `done`.

## Sanity Windows (host de dev — NON la validation du gate)

Sur ce host **Windows headless**, exercé pour prouver que le harnais pilote le vrai
chemin et que la stack Electron est verte (PAS pour valider le gate) :

- `bun run smoke:prod-load` → le **vrai front** charge depuis le build de prod, bridge
  allowlisté présent. **VERT.**
- `bun run smoke:core-host` → core-host boote (`ready`), **napi PTY** streame la sortie à
  main via l'EventSink, shutdown propre, zéro orphelin. **VERT.**
- `bun run gate:echo-sanity` → le harnais boote la stack réelle, charge le vrai front,
  injecte la sonde, mesure les 3 conditions + FPS via PowerShell/ConPTY.

Résultat sanity Windows mesuré (host de dev, **NON la validation du gate**) :

- core-host `ready`, vrai front chargé, sonde injectée, **PTY réel spawné (id=1)**, le
  shell **stream de la sortie** (`totalOutBytes > 0`), 3 conditions exécutées, FPS
  mesuré, **shutdown propre** → le harnais pilote bien le chemin de production.
- **écho : 0 échantillon / condition** (repos/anim/flood = n=0). C'est **attendu** :
  le PTY Windows lance **PowerShell via ConPTY**, qui **n'écho pas** les octets bruts
  tapés sur stdin comme le fait un PTY Linux en mode canonique. Le harnais **borne**
  chaque condition (budget wall-clock) et **termine** au lieu de hang, en reportant
  honnêtement `n=0` — exactement le signal « à mesurer sur la cible Linux ».
- **FPS : ~1 fps**, pire frame ~1006 ms — compositeur **offscreen headless** throttlé,
  **sans aucun rapport** avec un écran 120 Hz. Non représentatif par construction.

→ Le sanity Windows **prouve que le harnais et la stack Electron sont verts**, et **ne
prétend rien** sur les seuils du gate. Seul un run `--mode=gate` sur la cible compte.

## Conclusion

Harnais construit + commande de repro documentée. Les seuils du gate ne sont
**mesurables que sur la machine Linux/Wayland 120 Hz cible** → le gate est **bloqué**
(`prereq-missing`) en attente du run humain sur la cible, dont les chiffres seront collés
dans la section « CHIFFRES MESURÉS SUR LA CIBLE » ci-dessus.
