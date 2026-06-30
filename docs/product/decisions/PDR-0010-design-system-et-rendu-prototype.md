# PDR-0010 - Design-system imposé et rendu du prototype

**Status: validated**

Précise PDR-0009 et ferme Q-014. Le build détaillé reste v6 ; l'architecture est figée ici.

## Context

La distillation proto -> dev échoue parce que l'agent reproduit le look de façon ad hoc à chaque proto, sans vocabulaire visuel imposé : l'implémentation ne ressemble jamais au proto validé. Il fallait décider comment le design-system est défini, stocké, fourni à l'agent, maintenu à jour, et comment les prototypes sont rendus — sous la contrainte du backend full-Rust (PDR-0002, 0006, 0007).

Fidélité visée : « assez ressemblant et cohérent », pas pixel-près.

## Decision

**1. Le design-system est du knowledge projet, stocké dans nyx (autorité unique), pas un fichier dans le repo.** Deux tables :

- `design` (une par projet) : le contenu DESIGN.md (format Google Stitch : Colors, Typography, Spacing, Elevation, Guidelines) en markdown souple. C'est le contexte **imposé** de l'agent proto. Exportable en DESIGN.md, mais la source est nyx.
- `components` (une ligne par composant) : `{ nom, source repo+path, hash_visuel, template Handlebars, params, états }`.

**2. L'agent compose les protos uniquement depuis ce vocabulaire imposé** (tokens + composants nommés).

**3. Templating = Handlebars (`handlebars-rust`), compilé dans le sidecar Rust.** Logic-less par construction (partials = composants, hash args = params, pas de `if`/`each`). Pug est rejeté : JS uniquement, il forcerait une compilation dans Electron ou le renderer, ce qui viole PDR-0006/0007. Tera est l'alternative documentée (chargement en batch, logique à interdire par convention) ; Handlebars retenu pour son logic-less natif.

**4. Registration data-driven.** Au rendu, le sidecar enregistre en une seule passe les partials des composants nécessaires, lus depuis la table `components`. Aucun register dispersé.

**5. Styling = Tailwind exécuté dans l'iframe (JIT browser), pas dans le sidecar.** Le sidecar émet du markup avec classes Tailwind ; le renderer injecte dans l'iframe le runtime Tailwind + un script de config thème construit depuis la table `design`. Le CSS est généré côté browser. **Local-first :** le build browser de Tailwind est embarqué et servi localement par nyx (jamais le CDN distant), injecté avec un nonce CSP comme le shim.

**6. Pipeline de rendu :**

1. sidecar Rust : Handlebars (composants + params) -> markup ;
2. renderer : markup dans une `<iframe>` (CSP) + injection du shim (nav/états/feedback) + Tailwind local + config thème ;
3. CSS généré dans le browser.

Electron ne compile rien, le renderer ne compile rien ; toute la logique de template est dans le sidecar.

**7. Stockage.** Le proto stocké est la composition/markup, stylé à l'affichage — plus un blob HTML figé.

**8. Sync.** La table `components` est réconciliée incrémentalement avec le front de l'app : md5 par composant sur sa **partie visuelle** (template + styles, pas le `.ts` de logique). Au lancement du proto, diff des hash vs base -> seuls les composants changés sont réinjectés dans le contexte de l'agent, qui met à jour ceux-là. Jamais de régénération en bloc (le non-déterminisme réintroduirait la dérive).

**9. Inventaire.** Composants énumérés par convention glob sous un repo + path configuré par projet (Angular : `*.component.*`), surchargable. Pas de manifest à maintenir.

**10. Greenfield.** Une app neuve n'a pas de front à lire : l'agent crée la lib from scratch, elle devient la graine du vrai. App existante = app -> proto ; app neuve = proto -> app.

**11. Interactions.** Zéro JS de l'agent ; états déclaratifs + shim nyx (cf. PDR-0009).

## Consequences

- la distillation charrie un **arbre de composants nommés** (design.md), donc le dev reconstruit le même arbre -> fin du mismatch « tous les champs là, mais arrangés autrement » ;
- toute la compilation reste dans l'unique backend Rust ; Electron et le renderer restent minces, PDR-0006/0007 respectés ;
- le design-system vit dans nyx comme autorité unique, aucun fichier design dispersé dans les repos ;
- ceci **touche v1** : l'agent a besoin du même design-system pour coder le vrai dev, pas seulement les mockups ;
- le câblage exact (Handlebars, Tailwind, shim) est de l'implémentation v6 ; l'architecture, elle, est tranchée.
