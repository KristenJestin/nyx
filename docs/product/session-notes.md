# Notes de session

## Rôle de ce fichier

La **roadmap** dit *où on va* ; ce fichier garde l'**historique détaillé** des chantiers menés hors de la
ligne principale (branches non mergées, chantiers parkés, découvertes de pilotage réel). Convention : une note de
session vit ici, la roadmap n'en garde qu'un pointeur de quelques lignes. Ne rien perdre, ne pas gonfler la roadmap.

---

## Capacité review — branche `v2.4-review` (à greffer en v2.5)

> **Séquençage.** La capacité review a été **développée et validée en direct (pilotage CDP, gate e2e 9/9)** sur la
> branche **`v2.4-review`**, mais son **foyer de navigation** correct — un **onglet « Review » dans le détail Spec**
> — appartient au **domaine Spec**, construit en **v2.4**. La review s'y **grafte en onglet du détail Spec** une
> fois le domaine Spec disponible (v2.5).

**Développé (host-agnostique, se garde tel quel) :**

- moteur de diff sidecar (gix primaire + fallback git CLI, PDR-0027) : 7 surfaces (working tree / staged /
  unstaged / untracked / commit / range / branches), modes living/pinned, multi-repo sans mélange des identités
  Git ; vues dérivées (dirty / branche / ahead-behind / résumé) UI + MCP ;
- **rafraîchissement temps réel sans polling** (watch fs `notify` + debounce, PDR-0027) sur le bus EventSink v2.1
  (`repos://changed`) — tranche Q-015 ;
- écran review (renderer) : arbre de fichiers, en-tête multi-repo, viewer de diff **virtualisé** maison, look via
  les tokens du design system ; composants **réutilisables** (`components/review/`) ;
- modèle comment : `review_comments` auto-threadé, ancrage + **re-mapping** des ancres à travers les commits +
  état **outdated**, résolution **réservée à l'humain** (garde-fou vérifié adversarialement), réponses agent via
  **MCP** ; UI panneau + threads + ancres ; promotion comment→task (`review_comment_tasks`, finding=task) via flux
  **grill** (humain valide ; l'agent ne valide jamais) ; compteurs non-lus temps réel ;
- consigne dépendances : **PDR-0027** (gix + fallback CLI + `notify`) ;
- **gate e2e** (pilotage réel CDP) : review living + pinned multi-repo, diff, annotations global/ligne/plage,
  re-mapping/outdated, réponse agent MCP, résolution humaine, promotion, temps réel — **9/9**.

**Placement — la review se grafte en v2.5 sur le détail Spec (v2.4) :**

- foyer correct = **onglet « Review » du détail Spec**, ancré sur le **workspace du Spec** (repos + projet
  **dérivés du workspace** — `schema.rs` : « the project is derived via the workspace ») ;
- l'exposition actuelle via un **item de nav top-level + route `/review`** est **provisoire** (elle date d'avant
  l'existence du détail Spec) — **à retirer / recâbler** en onglet du détail Spec ;
- le re-mapping des ancres (phase 4) a été **câblé sur une voie live** (commande `review_remap` déclenchée quand le
  HEAD living avance) pendant la gate — découverte du pilotage réel.

**Où vit le travail :** branche **`v2.4-review`** (commit `61525ee`), **non mergée dans `v2`**, conservée pour la
reprise en v2.5. PDR diff/review Depot : `01KX0EEP5H1MJW4T3QCZEWPHFK` (statut review).

**Leçon.** La couche de présentation de la review dépend du **détail Spec (domaine Spec, v2.4)** : la développer
avant le domaine Spec livre la capacité mais laisse son point d'entrée sans foyer légitime. Ordre naturel :
Spec (détail + onglets, v2.4) → puis graft de l'onglet Review ancré workspace (v2.5).

---

## Terminal « blocks » — chantier PARKÉ (slot v2.3, WIP non mergé)

Modèle « blocks » façon Warp, **honor-the-shell** : sortie shell (hors TUI) segmentée en blocs commande+résultat via
**OSC 133**, historique rendu en blocs DOM, région active + TUI sur la grille wterm patchée (v2.2.5). C'est une
amélioration UX du terminal (suite de v2.2.5) ; elle occupe le slot **v2.3** mais reste **parkée** — une re-cadrage
en v2.2.x est possible si la priorité passe à v2.4 (Specs).

- **Branche :** `feature/v2.3-terminal-blocks` — commit `255cdf1` (WIP, **non mergé**).
- **PRD depot :** `01KWY7NFQJN068Q5ZSAJSSK2V2` · prototype validé : variante **« sobre »** élue.

### Fait — validé en direct sur l'app réelle (pilotage CDP, bash+starship)

- Segmentation OSC 133 → **blocs** : vrai prompt starship rejoué + commande + sortie + **code de sortie**
  (badges ✓/✗).
- **Cause racine corrigée :** le `PROMPT_COMMAND` en **tableau bash 5.x** (omarchy+direnv+zoxide+starship) faisait
  sauter le marqueur **OSC 133 `B`** → aucun bloc ne se formait. Fix : enregistrement *array-aware* du hook (le `B`
  survit à la réécriture de PS1 par starship) + filet (bloc formé même si `B` manque).
- Détection alt-screen (DECSET 1049/1047/47) + bascule blocs↔grille TUI plein écran.
- Persistance des blocs (fichier JSON sidecar, **sans migration**) ; non-régression persistance v2.2.5 verte.
- Renderer : liste de blocs (ANSI→tokens **design-system**), input épinglé bas, interactions
  (sélection/copie/repli).
- Wire Rust↔TS `PtyBlock`/`PtyAltScreen` + fixtures de contrat byte-parity ; harness e2e Electron
  (bash+starship, CDP).

### Reste à faire — défauts connus au parking

1. **[bloquant visuel] La grille wterm duplique le scrollback complet SOUS les blocs.** Il faut **clipper la grille
   à la seule région active** (prompt courant + sortie en cours) ; l'historique doit être **uniquement** des blocs.
   C'est ce qui donne l'impression de « wterm à plat comme avant ».
2. Conformité fine au **placement validé** (maquette « sobre » élue) — reproduire le placement via le **design
   system**, jamais le CSS/px du mockup. Le placement n'a **jamais été distillé** (`depot prd prototype distill`) —
   à faire avant reprise.
3. **Vérifier la saisie réelle** : un drop de caractères a été observé mais **confondu par l'injection CDP de test**
   (frappes agent + utilisateur mélangées dans le même terminal). À re-tester sans injection.
4. Sous-shell top-level `( … )` : header de commande corrompu (le fix propre `set -o functrace` **casse starship**)
   — parké en idée de durcissement.

### Leçon (à ne pas réoublier)

Toute validation « blocs » **doit** passer par le **pilotage réel de l'app sur bash+starship réel (CDP)**, jamais un
shell hermétique : les tests hermétiques émettaient un `B` que le vrai starship efface, ce qui a masqué pendant des
heures que le produit ne marchait pas (gate e2e « vert » sur un faux positif). Cette leçon est promue en **politique
de validation canonique** dans `technical-architecture.md`.
