//! Le simulateur de run, rejoué en test d'intégration.
//!
//! Ce fichier INCLUT le pilote de l'exemple `sim` (`#[path = "../examples/sim/driver.rs"]`) plutôt
//! que de le dupliquer : c'est la même unité de compilation que celle que `cargo run --example sim`
//! exécute, donc rien ne peut diverger entre ce que le test prouve et ce que le développeur lance.
//!
//! L'inclusion a une seconde vertu : un exemple Cargo n'est pas une cible de test, donc les
//! `#[cfg(test)] mod tests` de `driver.rs` et de `scenario.rs` ne tournent NULLE PART ailleurs. Ici,
//! ils tournent.
//!
//! # Comment ce fichier est lancé — et pourquoi pas par `bun run test`
//!
//! Par `bun run test:sim` (`cargo test -p nyx-core --test sim_scenarios_e2e`), et par lui seul. Il
//! n'est chaîné NI dans `test:core` (`cargo test --workspace --lib`, qui ne compile aucune cible
//! `tests/`) NI dans `test:core:e2e`, qui NOMME ses cibles une par une et où celle-ci n'a
//! délibérément pas été ajoutée. C'est une décision du PRD, et elle repose sur la NATURE de ce que
//! ces tests font tourner : six scénarios ouvrent chacun une base SQLite jetable, seedent les
//! workflows intégrés et exécutent de VRAIS PTY pour chaque `gate_check` — des shells réels, à côté
//! d'une suite unitaire qui n'en lance aucun.
//!
//! La DURÉE, elle, n'est PAS l'argument. Mesurée depuis PowerShell le 2026-09-03, sur une dizaine
//! de passages, la cible entière sort entre `finished in 6.32s` et `finished in 13.60s` — le bas de
//! la fourchette sur machine au repos, le haut sous charge, ce que des PTY réels rendent attendu.
//! Une version antérieure de ce commentaire annonçait « quelques dizaines de secondes » : c'était
//! faux d'un facteur quatre au cas nominal, et dans un dépôt dont la règle d'or est de ne rien
//! affirmer qu'on n'a pas observé, une justification chiffrée fausse est une dette. Ce qui exclut ce
//! fichier d'une suite unitaire est qu'il lance des shells, pas qu'il soit lent.
//!
//! La COMPILATION, elle, est couverte sans rien lancer : `bun run check:core`
//! (`cargo check --workspace --all-targets`) compile les cibles `example` et `test`, donc un
//! `#[path]`-include cassé se voit là, avant tout.

#[path = "../examples/sim/driver.rs"]
mod driver;

use std::path::PathBuf;

use driver::scenario::{replay, Scenario};

/// Le répertoire des scénarios livrés — la source que le garde-fou compare à [`REPLAYED`].
fn scenarios_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/sim/scenarios")
}

/// Rejoue le scénario livré `name` et échoue si le verdict n'est pas vide.
///
/// # Deux refus AVANT de rejouer quoi que ce soit
///
/// Un `cargo test` vert doit vouloir dire « les six scénarios ont tourné et ont prouvé ce qu'ils
/// annoncent ». Deux façons de le rendre menteur sont fermées ici, avant qu'aucun PTY ne soit lancé :
///
/// **1. Le bloc `expect` qui n'asserte rien.** Tous les champs d'`expect` sont optionnels — la bonne
/// règle pour un champ — et la recette d'ajout d'un scénario COMMENCE par `"expect": {}`. Un
/// scénario livré à cet état déroule ses actions, exécute ses gates et sort en `0` sans une seule
/// comparaison. [`Expect::missing_assertions`] pose le plancher, et l'échec NOMME le fichier.
///
/// **2. L'arbre qui ne porte pas la CATÉGORIE de Spec** ne demande aucune garde ici : ce fichier
/// inclut le pilote, qui appelle `nyx_core::category` et `db::get_category`, donc sur un tel arbre
/// il ne COMPILE pas. Le compilateur est la garde, et une détection d'exécution posée à côté ne
/// pourrait jamais rendre autre chose que « supporté ».
///
/// # Pourquoi un ÉCHEC, jamais un saut
///
/// Une version précédente sautait le cas non honorable par `eprintln!` puis `return`, en promettant
/// que le saut serait « bruyant ». Il ne l'était pas : libtest CAPTURE stdout ET stderr des tests
/// qui PASSENT, donc la raison n'était visible qu'avec `cargo test -- --nocapture`, et le fichier
/// serait sorti VERT en n'ayant rien rejoué — précisément la famille de piège que `CLAUDE.md`
/// décrit, appliquée à l'outil censé la fermer. Un échec est la seule visibilité que le harnais
/// garantisse SANS option.
fn replay_shipped(name: &str) {
    let path = scenarios_dir().join(format!("{name}.json"));
    let scenario = Scenario::load(&path).unwrap_or_else(|e| panic!("charger {name} : {e}"));

    if let Some(missing) = scenario.expect.missing_assertions() {
        panic!(
            "scénario `{name}` ({}) — il serait REJOUÉ sans rien prouver : {missing}.\n\
             Un verdict OK sur ce scénario ne vaudrait rien : remplir `expect` avec ce que \
             `bun run sim {name}` a réellement observé (README, « Ajouter un scénario », étape 3).",
            path.display(),
        );
    }

    let done = replay(&scenario, name, "claude", false)
        .unwrap_or_else(|e| panic!("rejouer {name} : {e}"));
    assert!(
        done.diffs.is_empty(),
        "scénario `{name}` — le verdict diverge :\n  {}\n\nsurfacé : {:#?}",
        done.diffs.join("\n  "),
        done.observed.surfaced,
    );
}

/// DÉCLARE les scénarios livrés — une seule fois, pour les deux usages qui doivent coïncider.
///
/// Chaque entrée engendre à la fois le `#[test]` qui rejoue le scénario ET son slug dans
/// [`REPLAYED`], la liste que [`every_shipped_scenario_has_a_test_here`] confronte au répertoire.
/// C'est ce qui ferme la MAUVAISE RÉPARATION que la revue de phase 3 a relevée : tant que la liste
/// était recopiée à la main, on pouvait déposer `foo.json`, voir le garde-fou rougir, ajouter `foo`
/// à la liste pour le calmer — et oublier la fonction qui le rejoue. Le scénario était alors livré,
/// annoncé par `--list`, déclaré couvert, et rejoué par personne. Ici, ajouter le slug AJOUTE le
/// test : les deux gestes n'en font plus qu'un.
macro_rules! shipped_scenarios {
    ($( $(#[$doc:meta])* $test:ident => $slug:literal ),+ $(,)?) => {
        /// Les slugs que les `#[test]` engendrés ci-dessous rejouent RÉELLEMENT — jamais une
        /// recopie : la macro les tient de la même déclaration que les fonctions.
        const REPLAYED: &[&str] = &[$($slug),+];

        $(
            $(#[$doc])*
            #[test]
            fn $test() {
                replay_shipped($slug);
            }
        )+
    };
}

shipped_scenarios! {
    /// Le parcours normal d'un Spec `feature` : aucune reproduction, un seul `build_check` vert, le
    /// Spec `completed` et sa tâche `done`. Le garde-fou de la ligne principale.
    ndev_feature => "ndev-feature",

    /// Un `bug` reproduit AUTOMATIQUEMENT : le journal porte le MÊME `repro_check` rouge puis vert —
    /// même ligne de commande, même `cwd`, deux nœuds distincts — et le `build_check` seulement
    /// après.
    ndev_bug_automated => "ndev-bug-automated",

    /// Un `bug` jugé impraticable à automatiser : les deux observations humaines sont surfacées et
    /// persistées, et AUCUNE tentative de `repro_check` n'a lieu — le slot est pourtant armé.
    ndev_bug_manual => "ndev-bug-manual",

    /// Un `bug` dont le projet n'a rien lié au slot `repro_check` : `run_start` le NOMME, et le gate
    /// REFUSE le run (`invalid_state`) au lieu de le laisser passer sans preuve.
    ndev_bug_unarmed => "ndev-bug-unarmed",

    /// Un Spec legacy SANS catégorie : le préflight du coder le classe avant tout routage, et la
    /// discipline `bug` s'applique ensuite exactement comme si `/nspec` l'avait classé.
    ndev_legacy_unclassified => "ndev-legacy-unclassified",

    /// `/nspec` classe un brouillon en `bug` : le `judge` décide, `set_category` écrit, et la boucle
    /// du contrat de reproduction se joue — dans cet ordre — avant la mise `ready`.
    nspec_categorise_bug => "nspec-categorise-bug",
}

/// TOUS les scénarios livrés ont un `#[test]` ici, et RÉCIPROQUEMENT. Sans ce garde-fou, ajouter un
/// fichier JSON à `scenarios/` suffirait à le faire apparaître dans `bun run sim --list` sans que
/// rien ne le rejoue jamais — la panne exacte que ce fichier existe pour empêcher, appliquée à
/// lui-même.
///
/// [`REPLAYED`] n'est PAS une recopie : la macro [`shipped_scenarios`] l'engendre depuis la même
/// déclaration que les fonctions de test, donc un slug présent ici est un slug REJOUÉ, et la seule
/// façon de calmer ce test est d'écrire le scénario ou de le retirer.
#[test]
fn every_shipped_scenario_has_a_test_here() {
    let dir = scenarios_dir();
    let mut shipped: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("lire {} : {e}", dir.display()))
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|e| e.to_str()) == Some("json"))
                .then(|| path.file_stem()?.to_str().map(str::to_string))
                .flatten()
        })
        .collect();
    shipped.sort();

    let mut replayed: Vec<String> = REPLAYED.iter().map(|s| s.to_string()).collect();
    replayed.sort();

    assert_eq!(
        shipped,
        replayed,
        "chaque scénario de `{}` doit être déclaré dans `shipped_scenarios!` — c'est cette \
         déclaration, et elle seule, qui engendre le `#[test]` qui le rejoue",
        dir.display(),
    );
}
