# `sim` — simulateur de run sans agent

Rejouer un workflow nyx (`ndev`, `nspec`, `ndoc`…) **sans agent et sans app**, avec des réponses
scriptées, et obtenir un verdict en quelques secondes.

Le moteur n'a besoin ni de LLM ni d'Electron : `run_start` et `nyx_next` sont des fonctions publiques
de `nyx_core::mcp_engine`, et chaque nœud agent se satisfait d'un `last_result` scripté. Ce que
chaque gate e2e Rust réécrivait à la main est ici **un pilote**, piloté par des **scénarios
déclaratifs**.

Le simulateur prouve le **chemin** — quelle branche du graphe, quels gates, quel journal, quels
statuts — pas la pertinence d'un LLM : un `judge` reçoit le verdict écrit dans le scénario.

## Usage

```sh
# depuis PowerShell (voir « PowerShell vs Git Bash » plus bas)
cargo run -p nyx-core --example sim -- ndev-feature
bun run sim ndev-feature            # même chose, via le script racine

bun run sim --list                  # les scénarios livrés, avec leur description
bun run sim ndev-feature --keep     # conserve la base jetable et imprime son chemin
bun run sim ndev-feature --provider opencode
bun run sim ./mon-scenario.json     # un chemin est accepté à la place d'un nom
```

Codes de sortie :

| code | signification |
|------|---------------|
| `0`  | verdict OK |
| `1`  | verdict KO — les divergences sont listées, une par ligne |
| `2`  | erreur de pilotage : scénario inconnu ou invalide, action orpheline, emballement |

Chaque exécution ouvre une base **jetable** par le vrai chemin de boot
(`Db::open(<temp>/nyx-sim-<scénario>-<pid>-<nanos>/nyx.db)` : migrations puis seeds des workflows
intégrés), supprimée à la fin sauf `--keep`. Jamais la base de l'app.

La sortie vient dans cet ordre : en-tête, trace tick par tick (numéro, profondeur, action, réponse
envoyée, delta de blackboard, clés **retirées**, notes), journal des gates, statuts finaux, verdict.

Les retraits sont imprimés à côté du delta parce que c'est ainsi que le moteur les rend, et jamais
dedans : une clé listée là ne tient plus, quoi qu'ait dit un delta antérieur. Le board agrégé que
`expect.blackboard` compare les applique.

## Le format de scénario

Un fichier JSON par scénario dans `scenarios/`. Trois blocs, aucun templating, aucune variable,
aucune condition.

```jsonc
{
  "description": "Une phrase : ce que ce scénario prouve. `--list` l'imprime.",

  "setup": {
    "scope": "ndev",                 // le workflow seedé à ouvrir
    "spec": {
      "status": "ready",             // draft | ready | in_progress
      "category": "feature",         // un slug système ou un nom libre ; null = NON CLASSÉ
      "phases": ["Build"],           // le plan, dans l'ordre
      "tasks": [
        { "title": "t0", "kind": "slice", "phase": "Build" }
      ]
    },
    "slots": {                       // les slots de gate à armer, par NOM de slot
      "build_check": ["green"],
      "repro_check": ["red", { "green_when_file_exists": "fixed" }]
    }
  },

  "answers": [                       // la PREMIÈRE entrée qui matche gagne
    {
      "when": {
        "type": "spawn_subagent",    // requis : spawn_subagent | judge | ask_human | produce | wait
        "label": "implement",        // le label de l'action, ou celui du nœud
        "prompt_contains": "in review",
        "subject": "user_stories"
      },
      "effect": { "write_file": "fixed" },   // ou { "remove_file": "fixed" }
      "reply": { "diff": "la tranche est implementee" }
    }
  ],

  "expect": {                        // tout est optionnel : ce qui n'est pas écrit n'est pas comparé
    "warnings_contain": ["repro_check"],          // des fragments, pas une séquence
    "surfaced": ["sub_workflow:coder", "spawn_subagent:implement", "done"],
    "journal": [
      { "slot": "repro_check", "expect": "red",   "outcome": "failed", "satisfied": true },
      { "slot": "repro_check", "expect": "green", "outcome": "passed", "satisfied": true,
        "same_check_as": 0 },                     // MÊME commande, MÊME cwd, AUTRE nœud
      { "slot": "build_check", "expect": "green", "outcome": "passed", "satisfied": true }
    ],
    "spec_status": "completed",
    "category": "bug",
    "category_created_by": "system",              // system | agent | human
    "tasks": { "t0": "done" },
    "refusal": { "code": "invalid_state", "message_contains": "repro_check" },
    "blackboard": { "audit_verdict": 0 }
  }
}
```

### `setup`

Le projet créé a pour workspace racine le répertoire jetable : c'est donc là que les gates tournent
et que les sentinelles atterrissent. Le Spec est conduit à son statut de départ par les **vraies
transitions gardées** (`draft` → `ready` → `in_progress`), plan de phases posé et tâches créées
avant la première transition (passé `draft`, une tâche ne peut plus nommer une phase absente du
plan).

Un slot que le graphe ne lit pas est lié quand même : cela se voit dans le journal, où il ne produit
aucune tentative.

`spec.category` est un **label** — un slug système (`feature`, `bug`, `refactor`, `chore`) ou un nom
libre — écrit par `category::set_spec_category_by_label`, le même service que le nœud `set_category`
du graphe et que l'outil MCP appellent. Un scénario qui part `bug` part donc exactement comme un Spec
que `/nspec` a classé. `null` est un **cas de test à part entière** : c'est l'état d'un Spec legacy,
celui que le préflight du coder rattrape en surfaçant `judge "which category?"`.

Un arbre qui ne porte pas la catégorie de Spec ne peut honorer aucun de ces scénarios — et il n'y a
rien à détecter pour autant : le pilote appelle `category::set_spec_category_by_label` et
`db::get_category`, donc là-bas le simulateur **ne compile pas**. Le compilateur est la garde. Une
détection d'exécution posée à côté ne pourrait jamais rendre autre chose que « supporté », et son
seul effet serait de faire croire à un filet qui n'en est pas un.

### Sentinelles

Trois sortes, rendues dans le **dialecte du shell que nyx lancera vraiment**
(`pty::resolve_shell()`) :

| sentinelle | PowerShell | cmd | POSIX |
|---|---|---|---|
| `"green"` | `exit 0` | `exit 0` | `exit 0` |
| `"red"` | `exit 1` | `exit 1` | `exit 1` |
| `{"green_when_file_exists": "f"}` | `if (Test-Path -LiteralPath 'f') { exit 0 } else { exit 1 }` | `if exist f (exit 0) else (exit 1)` | `test -f 'f' \|\| exit 1; exit 0` |

C'est la règle de `CLAUDE.md` — jamais de ligne de commande littérale — appliquée hors de
`crate::test_cmdline`, qui est `pub(crate)` : la duplication est assumée et documentée des deux
côtés. La forme POSIX est une **garde qui sort tôt**, et non un `exit $?` : `$?` n'est pas le statut
de sortie sous `fish`, que `resolve_shell()` retient dès que `SHELL` le désigne — la ligne y serait
refusée à l'analyse, donc rouge sans avoir rien mesuré.

Le **nom de fichier** d'une sentinelle (et celui d'un effet) n'accepte que des lettres ASCII, des
chiffres, `.`, `-` et `_`, vérifié au chargement. Il n'est échappé nulle part : une apostrophe
casserait le quoting — et un gate dont la ligne est refusée par le shell sort non nul, donc
**satisfait** un `expect: red` sans avoir rien vérifié —, un espace rendrait `if exist my fix (…)`
toujours vert sous cmd, et un nom absolu sortirait du répertoire jetable.

### `answers`

Toutes les clés présentes dans `when` doivent correspondre ; celles qui sont absentes ne
contraignent rien. La première entrée qui matche gagne, donc une réponse spécifique s'écrit **avant**
la générale.

Une action qu'**aucune** entrée ne couvre **arrête le pilote** (code 2) en imprimant l'action
complète, son type et le label de son nœud. C'est délibéré : un prompt reformulé casse bruyamment
plutôt que d'être mal répondu.

`effect` s'exécute dans le répertoire du scénario (le `cwd` des gates) **avant** que la réponse ne
parte : c'est ainsi qu'un « fix » se rend observable sans qu'aucun agent n'écrive de code.

Une action marquée `human_turn_required` déclenche un `record_human_turn` sur le terminal du pilote
avant la réponse — ce que le hook `UserPromptSubmit` fait en production. Sans lui, nyx refuse.

### `expect`

- `surfaced` : la séquence **exacte** des actions surfacées, sous la forme `type:clé`. La clé est le
  `prompt_contains` de l'entrée qui a matché, à défaut son `subject`, à défaut son `label`, à défaut
  le label du nœud. `done` n'a pas de clé ; `sub_workflow` porte le label du nœud (jamais l'id du run
  enfant, qui change à chaque exécution).
- `journal` : les tentatives de gate, dans l'ordre, champ par champ. Ce sont les lignes de la table
  `gate_attempts` (`db::list_gate_attempts_for_spec`), lues telles quelles :
  - `expect` est la polarité **déclarée par le nœud** (`red` / `green`) et `satisfied` dit si
    l'issue l'a honorée. Les deux comptent : sur un gate de reproduction, `outcome: failed` est la
    **preuve attendue** ; sur un gate de build, c'est un échec. Le seul `outcome` ne les distingue
    pas.
  - `blocking` dit si un rouge **refuse d'avancer** ou se contente de se signaler. Ni `outcome` ni
    `satisfied` ne le portent : un `repro_check` non bloquant rendrait la même ligne de journal,
    rouge comprise, sans rien imposer à la suite du run — c'est ce que les scénarios `bug` épinglent
    à `true`. Le journal de `bun run sim` l'imprime en clair (`bloquant` / `signale`).
  - `same_check_as: <index>` exige que cette tentative reprenne **la même `command_line` et le même
    `cwd`** qu'une tentative antérieure, et qu'elle vienne d'un **nœud distinct**. C'est la
    vérification que fait l'auditeur d'un bug automatisé : un rouge obtenu sur un contrôle et un vert
    obtenu sur un autre ne démontrent rien ensemble. Un index, jamais une ligne littérale — la ligne
    dépend du dialecte du shell de la machine.
- `category` / `category_created_by` : le **slug persisté** (jamais le mot du judge) et la provenance
  de la ligne `categories` — `system` quand le judge a réutilisé le vocabulaire du projet, `agent`
  quand il a minté un label à lui.
- `warnings_contain` : des **fragments** que les avertissements de `run_start` doivent porter. Ce
  n'est pas une séquence : l'ordre du tableau `warnings` n'est le contrat de personne, et ce qui se
  prouve est qu'un avertissement a été émis et qu'il **nomme** la chose à corriger.
- `refusal` : son **absence** est vérifiée aussi — un refus non attendu diverge.
- `blackboard` : un sous-ensemble du board agrégé (l'enfant l'emporte sur le parent).

Chaque divergence produit une ligne `champ : attendu <x> ≠ obtenu <y>`.

## Les scénarios livrés

| scénario | ce qu'il prouve |
|---|---|
| `ndev-feature` | le parcours normal : aucune reproduction exigée, un seul `build_check` vert |
| `ndev-bug-automated` | le **même** `repro_check` rouge avant le fix, vert après, puis le build |
| `ndev-bug-manual` | la reproduction jugée impraticable : le protocole humain avant/après, aucun `repro_check` |
| `ndev-bug-unarmed` | `repro_check` non lié : `run_start` l'annonce, et le gate REFUSE le run |
| `ndev-legacy-unclassified` | un Spec sans catégorie classé par le préflight, puis la discipline `bug` |
| `nspec-categorise-bug` | `/nspec` classe en `bug`, écrit, et joue le contrat de reproduction |

### La variante `/nspec` qui MINTE une catégorie

`nspec-categorise-bug` répond `bug` au `judge "which category?"`, donc le service **résout** l'une
des quatre catégories que le seed pose sur tout projet : slug `bug`, `created_by_type = system`.

Répondre autre chose — `perf regression`, par exemple — emprunte l'autre moitié de PDR-0017 : le
label est **slugifié** (`perf regression` → `perf-regression`), la catégorie est **mintée à la
volée** dans le projet, et la ligne porte `created_by_type = agent`. Elle est valide telle quelle :
l'axe `proposed` / `confirmed` a été retiré par l'amendement du 2026-09-02 — l'agent a pleins droits,
et ce qu'un humain curate est le **vocabulaire** du projet (créer, renommer, supprimer depuis les
réglages), pas chaque mint un par un. `categories.confirmed_at` survit en base mais rien ne le lit.

La variante n'est **pas livrée** comme scénario : le déroulé change plus que le mot du judge. Le
slug n'étant pas `bug`, le sélecteur `bug discipline?` prend sa branche par défaut et **toute la
boucle du contrat de reproduction est sautée**. Pour l'observer, copier `nspec-categorise-bug.json`,
y remplacer le verdict par `perf regression`, retirer les trois entrées de la boucle `bug contract`
de `surfaced`, et attendre `"category": "perf-regression"` avec `"category_created_by": "agent"`.

## Ajouter un scénario

1. Écrire `scenarios/<nom>.json` avec `setup` et `answers`, en laissant `expect` vide (`{}`).
2. `bun run sim <nom>` : la trace imprime la séquence réelle et le journal. Le verdict, lui, sort
   en `1` en disant que rien n'est asserté — c'est voulu, et c'est l'étape suivante.
3. Recopier ce qui a été **observé** dans `expect`, en ne gardant que ce que le scénario prouve.
   Il y a un **plancher** : `surfaced` non vide, et au moins une attente d'arrivée parmi
   `spec_status`, `journal` et `refusal`. En dessous, les **deux** surfaces refusent en le
   nommant — `bun run test:sim` avant de lancer le moindre PTY, `bun run sim` dans son verdict. Un
   `expect` vide qui sortirait `OK` serait pire qu'un scénario absent.
4. Ajouter une ligne `<nom_de_test> => "<nom>"` à la macro `shipped_scenarios!` de
   `crates/nyx-core/tests/sim_scenarios_e2e.rs`. Cette déclaration est la **source unique** : elle
   engendre le `#[test]` qui rejoue le scénario **et** la liste que
   `every_shipped_scenario_has_a_test_here` compare au répertoire. Il n'y a donc pas de second geste
   à oublier, et pas de liste à « calmer » sans écrire le test.
5. Vérifier que les attentes **mordent** : fausser volontairement l'une d'elles (retirer une ligne du
   journal attendu, par exemple) doit faire sortir `bun run sim <nom>` en `1` avec la divergence
   nommée. Puis restaurer.

Un scénario rouge ne s'« ajuste » jamais pour passer : c'est une régression, et elle se remonte comme
telle.

## PowerShell vs Git Bash

`CLAUDE.md` : **mesurer depuis PowerShell, pas depuis Git Bash**. Lancé depuis Git Bash, `cargo`
hérite d'un `PATH` qui porte le `usr\bin` de Git, où vivent `sh.exe`, `true.exe` et `test.exe` : une
ligne POSIX y devient résolvable et deux agents obtiennent deux baselines sur la même machine, au
même commit.

Le simulateur ferme ce piège par construction — ses sentinelles sont écrites pour l'interpréteur que
`pty::resolve_shell()` désignera vraiment et n'ont besoin de rien sur le `PATH` — ce qui veut dire
que `bun run sim` doit sortir **vert des deux côtés**. Si les deux divergent, c'est le signal, pas
un détail d'environnement.

## Fichiers

| fichier | rôle |
|---|---|
| `main.rs` | la CLI : arguments, résolution du scénario, impression, code de sortie |
| `driver.rs` | le pilote : base jetable, mise en place, boucle `nyx_next`, tour humain, effets, trace |
| `scenario.rs` | le format JSON, les sentinelles, le matching, les attentes, le verdict, `replay` |
| `scenarios/*.json` | les scénarios livrés |

`driver.rs` est aussi inclus par `crates/nyx-core/tests/sim_scenarios_e2e.rs`
(`#[path = "../examples/sim/driver.rs"]`) : c'est la seule cible où ses tests unitaires tournent, et
c'est la même unité de compilation que celle que la CLI exécute.

### Qui lance quoi — et pourquoi `bun run test` ne lance pas ceci

| geste | ce qu'il fait du simulateur |
|---|---|
| `bun run test:sim` | **rejoue les six scénarios** (`cargo test -p nyx-core --test sim_scenarios_e2e`) |
| `bun run check:core` | **compile** l'exemple et le test (`cargo check --workspace --all-targets`) |
| `bun run test:core` | rien — `cargo test --workspace --lib` ne compile aucune cible `tests/` |
| `bun run test:core:e2e` | rien — il **nomme** ses cibles une à une, et celle-ci n'y est pas |
| `bun run test` | rien — il chaîne les deux précédents |

**`sim_scenarios_e2e` n'est chaîné dans aucune suite générale, et c'est une décision.** Chacun des
six scénarios ouvre une base SQLite, y seede les workflows intégrés et exécute de **vrais PTY** pour
chaque `gate_check` : des shells réels, pas des fixtures. C'est le prix de ce que le simulateur
prouve, et ce n'est pas un prix à payer à chaque `bun run test`. `test:core:e2e` nomme ses cibles
une par une (voir `AGENTS.md`), donc l'omission est **explicite** et non un oubli de balayage.

Ce qui reste garanti sans rien lancer, c'est la **compilation** : `bun run check:core` compile les
cibles `example` et `test`, donc un `#[path]`-include cassé se voit là — et `cargo check --workspace`
(lib seule) ne le verrait pas.

**La CI ne couvre ni l'un ni l'autre** : le seul workflow du dépôt,
`.github/workflows/gate-v2.2.5-terminal-persistence.yml`, exécute les suites du renderer et
`cargo test --workspace --lib`, mais jamais `bun run check`, `check:core` ni `test:sim`. Un
`#[path]`-include qui cesse de compiler passerait donc la CI au vert : c'est **en local**, avant de
conclure, que ce gate-là se joue.

L'exemple ne compile que contre l'**API publique** de `nyx_core`. Aucun `pub(crate)` n'est élargi
pour lui, et le moteur n'est jamais modifié : ce qui n'est pas public est réécrit ici.
