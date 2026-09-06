//! Le FORMAT de scénario : un fichier JSON lisible plutôt que du Rust à recompiler.
//!
//! Un scénario dit TROIS choses, et rien d'autre — pas de templating, pas de variables, pas de
//! conditions :
//!
//!   * `setup`   — d'où l'on part : le workflow (`scope`), le Spec (statut, catégorie, phases,
//!                 tâches) et les slots de gate armés, chacun avec ses sentinelles ;
//!   * `answers` — ce que le pilote répond à chaque action surfacée, la PREMIÈRE entrée qui matche
//!                 gagnant, et l'effet éventuel à produire avant de répondre ;
//!   * `expect`  — ce que le déroulé doit avoir produit : séquence surfacée, journal des gates,
//!                 statuts, refus, blackboard.
//!
//! Module ENFANT de `driver.rs` (`#[path = "scenario.rs"] pub mod scenario;`) : il n'atteint le
//! pilote que par `super::`, jamais par `crate::`, pour que l'inclusion du driver par le test
//! d'intégration (`#[path = "../examples/sim/driver.rs"]`) voie le même arbre de modules.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::{
    AnswerSource, DriverError, Effect, GateAttempt, Refusal, Reply, Shell, TaskSeed,
};

// ============================================================================
// Sentinelles de gate — duplication ASSUMÉE de `crates/nyx-core/src/test_cmdline.rs`
// ============================================================================

/// Ce qu'un slot de gate est armé AVEC : une commande dont seul le code de sortie compte.
///
/// En JSON : `"green"`, `"red"`, ou `{"green_when_file_exists": "<nom>"}`.
///
/// # Duplication assumée
///
/// `crate::test_cmdline` (dans `crates/nyx-core/src/test_cmdline.rs`) arme les mêmes trois
/// sentinelles, pour le même interpréteur et pour la même raison — la règle de `CLAUDE.md` :
/// **jamais de ligne de commande littérale** dans un test qui arme une commande gérée, sous peine
/// d'obtenir deux baselines sur la même machine selon que la mesure part de PowerShell ou de Git
/// Bash. Mais ce module est `pub(crate)`, et l'exemple `sim` ne compile que contre l'API PUBLIQUE de
/// `nyx_core` (aucun `pub(crate)` n'est élargi pour lui — décision d'implémentation du PRD). Elles
/// sont donc réécrites ici, sciemment ; un commentaire croisé le dit dans `src/test_cmdline.rs`.
///
/// **Ce qui est dupliqué, ce sont les TROIS COMPORTEMENTS, pas le texte.** `green` et `red` sont
/// bien la même ligne des deux côtés (`exit 0` / `exit 1`), et `green_when_file_exists` l'est aussi
/// en POSIX (`test -f 'f' || exit 1; exit 0`) : une GARDE qui sort tôt, et non un `exit $?` que
/// `fish` refuse à l'analyse. Les deux dialectes Windows, eux, DIVERGENT — `test_cmdline` enchaîne
/// une garde avant un `exits` final parce que son `Cmdline` est un builder, là où une `Sentinel`
/// rend UNE ligne autonome. Les écritures restent équivalentes — verte exactement quand le fichier
/// est là, et aucune ne dépend du `PATH`. Un changement de dialecte d'un côté se TRANSPOSE donc de
/// l'autre ; il ne se recopie pas.
///
/// La famille de shell vient de [`super::Shell::of_platform`], c'est-à-dire de
/// [`nyx_core::pty::resolve_shell`] — la MÊME résolution que `command::CommandPty::spawn` — donc
/// une sentinelle n'a jamais besoin de rien sur le `PATH`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sentinel {
    /// Toujours verte : `exit 0`.
    Green,
    /// Toujours rouge : `exit 1`.
    Red,
    /// Verte exactement quand `name` existe dans le `cwd` du gate — la sentinelle « fix » : rouge
    /// tant que le travail simulé n'a pas posé le fichier, verte ensuite.
    GreenWhenFileExists(String),
}

impl Sentinel {
    /// La ligne de commande, dans le dialecte de `shell`.
    ///
    /// `exit <n>` est la seule instruction que les trois shells écrivent à l'identique — c'est de la
    /// SYNTAXE, pas un nom de programme, ce que `true` et `false` ne sont précisément pas.
    pub fn render(&self, shell: Shell) -> String {
        match self {
            Sentinel::Green => "exit 0".to_string(),
            Sentinel::Red => "exit 1".to_string(),
            Sentinel::GreenWhenFileExists(name) => match shell {
                // Relatif à dessein : le `cwd` d'un gate EST son sujet.
                //
                // La forme POSIX est une GARDE qui sort tôt, et non `test -f '…'; exit $?` : `$?`
                // n'est pas le statut de sortie sous `fish`, que `resolve_shell` retient dès que
                // `SHELL` le désigne et que `command_invocation` range dans la famille `-c`. Là,
                // `$?` fait REFUSER la ligne à l'analyse — donc un rouge qui n'a rien mesuré, sur
                // une machine où les gates e2e de la crate restent verts. C'est la même écriture
                // que `crate::test_cmdline::green_when_file_exists`.
                Shell::Posix => format!("test -f '{name}' || exit 1; exit 0"),
                Shell::PowerShell => {
                    format!("if (Test-Path -LiteralPath '{name}') {{ exit 0 }} else {{ exit 1 }}")
                }
                Shell::Cmd => format!("if exist {name} (exit 0) else (exit 1)"),
            },
        }
    }
}

// ============================================================================
// Le nom de fichier d'une sentinelle ou d'un effet
// ============================================================================

/// REFUSE un nom de fichier que le simulateur ne peut pas manipuler sans danger.
///
/// Ce nom entre à deux endroits où il n'est JAMAIS échappé : la ligne de commande d'une sentinelle
/// ([`Sentinel::render`], interpolée dans du quoting POSIX, PowerShell ou cmd) et le `join` d'un
/// effet ([`super::Driver::apply`], qui écrit ou supprime dans le `cwd` du gate). Les deux fuites
/// sont muettes, et c'est ce qui les rend graves :
///
///   * une apostrophe casse le quoting — `Test-Path -LiteralPath 'it's fixed'` est une erreur
///     d'ANALYSE, donc une sortie non nulle que le fichier existe ou non. Le moteur journalise
///     `failed`, ce qui SATISFAIT un `expect: red` : le scénario sort vert sans qu'aucun contrôle
///     n'ait tourné. C'est mot pour mot la seconde bouche du piège de `AGENTS.md` — un test qui
///     attend du ROUGE passe en n'assertant rien ;
///   * un espace suffit sous cmd : `if exist my fix (exit 0) else (exit 1)` est évalué comme
///     `if exist my`, donc VERT que le fichier soit là ou non ;
///   * un nom absolu ou un composant parent sortent du répertoire jetable — `Path::join` remplace
///     le préfixe quand le nom est absolu, et un effet `remove_file` supprimerait alors un fichier
///     réel de la machine, sans erreur ni trace.
///
/// D'où un alphabet FERMÉ, vérifié au chargement du scénario : lettres ASCII, chiffres, `.`, `-`,
/// `_`. Refuser tôt et en le nommant est la méthode que le module applique partout ailleurs.
pub fn check_file_name(field: &str, name: &str) -> Result<(), DriverError> {
    let safe = !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if safe {
        return Ok(());
    }
    Err(DriverError::Scenario(format!(
        "{field} : nom de fichier `{name}` refusé. Un nom est RELATIF au `cwd` du gate et ne porte \
         que des lettres ASCII, des chiffres, `.`, `-` ou `_` — tout le reste casse le quoting de \
         la ligne de commande (un rouge qui n'a rien mesuré satisfait un `expect: red`) ou sort du \
         répertoire jetable."
    )))
}

// ============================================================================
// Le scénario
// ============================================================================

/// Un scénario complet.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Une phrase : ce que ce scénario prouve. `--list` l'imprime.
    pub description: String,
    pub setup: SetupDecl,
    #[serde(default)]
    pub answers: Vec<Answer>,
    #[serde(default)]
    pub expect: Expect,
}

/// D'où le déroulé part.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupDecl {
    /// Le workflow seedé à ouvrir : `ndev`, `nspec`, `ndoc`, …
    pub scope: String,
    pub spec: SpecDecl,
    /// `slot → sentinelles`. Le nom du slot est celui que le graphe déclare
    /// (`nyx_core::seed::BUILD_CHECK_SLOT`, `nyx_core::seed::REPRO_CHECK_SLOT`) : le pilote reste
    /// générique et n'en connaît aucun. Un slot que personne ne lit se voit d'ailleurs dans le
    /// journal — il n'y produit aucune tentative, ce que `ndev-bug-manual` prouve.
    #[serde(default)]
    pub slots: BTreeMap<String, Vec<Sentinel>>,
}

/// Le Spec de départ.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecDecl {
    /// `draft` | `ready` | `in_progress` — conduit par les VRAIES transitions gardées.
    pub status: String,
    /// La catégorie du Spec de départ — un LABEL : un slug système (`bug`, `feature`, `refactor`,
    /// `chore`) ou un nom libre, que [`nyx_core::category::set_spec_category_by_label`] résout ou
    /// MINTE, exactement comme le nœud `set_category` du graphe.
    ///
    /// `null` = NON CLASSÉ, et c'est un cas de test à part entière : c'est l'état d'un Spec legacy,
    /// celui que le préflight du coder rattrape par son `judge which category?`.
    ///
    /// Un arbre dont le schéma ne porte aucune catégorie ne peut pas honorer ce champ — mais il ne
    /// compile pas non plus le simulateur, qui appelle `nyx_core::category` : le compilateur est la
    /// garde, il n'y a rien à détecter à l'exécution.
    #[serde(default)]
    pub category: Option<String>,
    /// Le plan de phases, dans l'ordre.
    #[serde(default)]
    pub phases: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<TaskDecl>,
}

/// Une tâche du Spec.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDecl {
    pub title: String,
    /// `slice` (défaut) | `human` | `review`.
    #[serde(default)]
    pub kind: Option<String>,
    /// La phase, par NOM. Absent = la PREMIÈRE phase déclarée par `setup.spec.phases`, ou plat
    /// quand ce plan est vide — une tâche hors plan sur un Spec qui en a un ne serait jamais
    /// atteinte par la boucle du coder, donc le repli est celui qui décrit un déroulé.
    #[serde(default)]
    pub phase: Option<String>,
}

// ============================================================================
// La table de réponses
// ============================================================================

/// Une entrée de la table : « quand cette action se présente, produis cet effet et réponds ceci ».
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub when: When,
    /// Ce qui se passe dans le répertoire du scénario AVANT que la réponse ne parte — poser ou
    /// retirer la sentinelle qu'un gate lira ensuite.
    #[serde(default)]
    pub effect: Option<EffectDecl>,
    /// Le `last_result` renvoyé au moteur, tel quel.
    pub reply: Value,
}

/// Le filtre d'une entrée. TOUTES les clés présentes doivent correspondre ; celles qui sont
/// absentes ne contraignent rien.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    /// Le type d'action : `spawn_subagent`, `judge`, `ask_human`, `produce`, `wait`.
    #[serde(rename = "type")]
    pub action_type: String,
    /// Le label du nœud — celui que l'action porte (`spawn_subagent`) ou, à défaut, celui que le
    /// pilote a lu sur le graphe.
    #[serde(default)]
    pub label: Option<String>,
    /// Un fragment du prompt. C'est la clé d'un `ask_human` : un prompt REFORMULÉ ne matche plus, et
    /// le pilote s'arrête bruyamment au lieu de répondre à côté.
    #[serde(default)]
    pub prompt_contains: Option<String>,
    /// Le sujet par nœud d'un `spawn_subagent` (une section du Spec, pour l'auditeur).
    #[serde(default)]
    pub subject: Option<String>,
}

/// L'effet déclaré, en JSON : `{"write_file": "<nom>"}` ou `{"remove_file": "<nom>"}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectDecl {
    WriteFile(String),
    RemoveFile(String),
}

impl EffectDecl {
    fn to_effect(&self) -> Effect {
        match self {
            EffectDecl::WriteFile(name) => Effect::WriteFile(name.clone()),
            EffectDecl::RemoveFile(name) => Effect::RemoveFile(name.clone()),
        }
    }
}

impl When {
    /// `true` quand TOUTES les clés présentes correspondent.
    fn matches(&self, action: &Value, node_label: &str) -> bool {
        if action["type"].as_str() != Some(self.action_type.as_str()) {
            return false;
        }
        if let Some(label) = &self.label {
            let action_label = action["label"].as_str().unwrap_or("");
            if action_label != label && node_label != label {
                return false;
            }
        }
        if let Some(fragment) = &self.prompt_contains {
            if !action["prompt"]
                .as_str()
                .unwrap_or("")
                .contains(fragment.as_str())
            {
                return false;
            }
        }
        if let Some(subject) = &self.subject {
            if action["subject"].as_str() != Some(subject.as_str()) {
                return false;
            }
        }
        true
    }

    /// La CLÉ que cette entrée donne à l'action dans la trace — ce que `expect.surfaced` compare
    /// après le `type:`. Le `prompt_contains` d'abord (c'est ce qui identifie une question), puis le
    /// sujet, puis le label ; sans aucun des trois, le pilote retombe sur le label du nœud.
    fn key(&self) -> Option<String> {
        self.prompt_contains
            .clone()
            .or_else(|| self.subject.clone())
            .or_else(|| self.label.clone())
    }
}

impl AnswerSource for Scenario {
    /// La PREMIÈRE entrée qui matche gagne — l'ordre du fichier est la priorité, ce qui permet
    /// d'écrire une réponse spécifique avant la générale.
    fn reply_for(&self, action: &Value, node_label: &str) -> Option<Reply> {
        let answer = self
            .answers
            .iter()
            .find(|a| a.when.matches(action, node_label))?;
        Some(Reply {
            value: answer.reply.clone(),
            key: answer.when.key(),
            effect: answer.effect.as_ref().map(EffectDecl::to_effect),
        })
    }
}

// ============================================================================
// Les attentes
// ============================================================================

/// Ce que le déroulé doit avoir produit. Chaque bloc est OPTIONNEL : ce qui n'est pas écrit n'est
/// pas comparé, ce qui est écrit l'est champ à champ.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// La séquence EXACTE des actions surfacées, sous la forme `type:clé` (`done` et
    /// `sub_workflow:<label>` compris) — le transcript du déroulé.
    #[serde(default)]
    pub surfaced: Option<Vec<String>>,
    /// Le journal des tentatives de gate, dans l'ordre.
    #[serde(default)]
    pub journal: Option<Vec<JournalExpect>>,
    #[serde(default)]
    pub spec_status: Option<String>,
    /// Le SLUG de la catégorie finale du Spec.
    #[serde(default)]
    pub category: Option<String>,
    /// La PROVENANCE de la ligne `categories` que le Spec porte au final : `system` (l'une des
    /// quatre du seed), `agent` (mintée à la volée par une réponse hors vocabulaire) ou `human`.
    ///
    /// C'est ce qui distingue « le judge a RÉUTILISÉ le vocabulaire du projet » de « le judge a
    /// INVENTÉ un label », deux issues que le seul slug ne sépare pas — et la seconde est celle que
    /// PDR-0017 autorise sans revue depuis l'amendement du 2026-09-02.
    #[serde(default)]
    pub category_created_by: Option<String>,
    /// `titre de tâche → statut attendu`.
    #[serde(default)]
    pub tasks: Option<BTreeMap<String, String>>,
    /// Le refus attendu — et son absence est elle aussi vérifiée : un refus non attendu diverge.
    #[serde(default)]
    pub refusal: Option<RefusalExpect>,
    /// Un sous-ensemble du blackboard agrégé.
    #[serde(default)]
    pub blackboard: Option<Map<String, Value>>,
    /// Des FRAGMENTS que les avertissements de `run_start` doivent porter, un par entrée. Ce n'est
    /// pas une séquence : l'ordre du tableau `warnings` n'est le contrat de personne, et ce qu'un
    /// scénario prouve est qu'un avertissement A ÉTÉ ÉMIS et qu'il NOMME la chose à corriger (un
    /// slot non armé, par exemple).
    #[serde(default)]
    pub warnings_contain: Option<Vec<String>>,
}

impl Expect {
    /// Ce qui MANQUE à ce bloc pour qu'un verdict OK prouve quelque chose — ou `None` quand il porte
    /// de quoi en rendre un.
    ///
    /// # Pourquoi un plancher, alors que tout est optionnel
    ///
    /// « Ce qui n'est pas écrit n'est pas comparé » est la bonne règle pour un CHAMP : un scénario
    /// ne doit épingler que ce qu'il prouve. Appliquée au bloc ENTIER, elle se retourne : un
    /// `expect` vide déroule le scénario, exécute de VRAIS gates sur PTY, et sort en `0` sans une
    /// seule assertion. C'est le piège que `CLAUDE.md` décrit — « un test qui n'asserte rien passe
    /// des deux côtés » — appliqué à l'outil dont la raison d'être est de rendre un verdict fiable.
    /// Et la recette d'ajout d'un scénario COMMENCE par un `expect` vide (voir le README) : rien ne
    /// garantissait qu'on soit allé jusqu'à l'étape qui le remplit.
    ///
    /// Le plancher est celui d'un verdict minimal, pas celui d'un bon scénario :
    ///
    ///   * `surfaced` NON VIDE — le transcript du déroulé, la seule attente qui dise par où l'on est
    ///     passé. Un tableau vide ne compte pas : il n'affirme rien qu'un scénario qui déroule
    ///     quinze actions ne contredirait.
    ///   * au moins UNE attente d'ARRIVÉE parmi `spec_status`, `journal` et `refusal` — où le
    ///     déroulé a laissé le Spec, ce que les gates ont vraiment exécuté, ou le refus attendu. Ici
    ///     un `journal` VIDE compte, et c'est voulu : `[]` affirme qu'AUCUNE tentative de gate n'a
    ///     eu lieu, ce que `ndev-bug-unarmed` et `nspec-categorise-bug` — les deux scénarios livrés
    ///     qui portent réellement `"journal": []` — prouvent précisément. (`ndev-bug-manual`, lui,
    ///     journalise une tentative `build_check` : ce qu'il prouve est « aucune tentative de
    ///     `repro_check` », pas « aucune tentative ».)
    ///
    /// `category`, `tasks`, `blackboard` et `warnings_contain` ne figurent pas au plancher : ils
    /// affinent un verdict, ils n'en fondent pas un.
    pub fn missing_assertions(&self) -> Option<String> {
        if self.surfaced.as_ref().is_none_or(Vec::is_empty) {
            return Some(
                "`expect.surfaced` est absent ou vide, donc rien ne dit par où le déroulé est passé"
                    .to_string(),
            );
        }
        if self.spec_status.is_none() && self.journal.is_none() && self.refusal.is_none() {
            return Some(
                "`expect` ne porte aucune attente d'ARRIVÉE : il en faut au moins une parmi \
                 `spec_status`, `journal` et `refusal`"
                    .to_string(),
            );
        }
        None
    }
}

/// Une tentative de gate attendue — une ligne de `gate_attempts`. Les champs absents ne
/// contraignent rien.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalExpect {
    /// La polarité DÉCLARÉE par le nœud (`green` / `red`). C'est elle qui distingue un rouge PROUVÉ
    /// d'un rouge SUBI : sur un `expect: red`, `outcome: failed` est la preuve attendue.
    #[serde(default)]
    pub expect: Option<String>,
    /// `passed` | `failed` | `indeterminate`.
    #[serde(default)]
    pub outcome: Option<String>,
    /// L'`outcome` a-t-il honoré l'`expect`.
    #[serde(default)]
    pub satisfied: Option<bool>,
    #[serde(default)]
    pub slot: Option<String>,
    /// Le gate est-il BLOQUANT — un rouge doit-il refuser d'avancer, ou seulement se signaler.
    ///
    /// C'est la moitié du contrat que ni `outcome` ni `satisfied` ne portent : un `repro_check` non
    /// bloquant rendrait exactement le même journal qu'un bloquant, rouge compris, sans rien
    /// imposer à la suite du run. Un scénario qui prouve la DISCIPLINE d'un gate a donc besoin de
    /// l'épingler.
    #[serde(default)]
    pub blocking: Option<bool>,
    /// L'INDEX d'une tentative ANTÉRIEURE dont cette tentative doit reprendre, à l'identique, la
    /// `command_line` ET le `cwd` — et qui doit venir d'un nœud DISTINCT.
    ///
    /// C'est la vérification que l'auditeur fait à la main sur un bug automatisé : un rouge obtenu
    /// sur un contrôle et un vert obtenu sur un autre ne démontrent rien ensemble. Un index, et non
    /// une ligne littérale, parce qu'une ligne littérale dans un scénario est exactement ce que
    /// `CLAUDE.md` interdit (la commande dépend du dialecte du shell de la machine).
    #[serde(default)]
    pub same_check_as: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefusalExpect {
    pub code: String,
    #[serde(default)]
    pub message_contains: Option<String>,
}

/// Ce que le déroulé a RÉELLEMENT produit — assemblé par la CLI depuis la trace du pilote et la
/// base, puis confronté à [`Expect`].
#[derive(Debug, Default)]
pub struct Observed {
    /// Les signatures `type:clé`, dans l'ordre.
    pub surfaced: Vec<String>,
    pub journal: Vec<GateAttempt>,
    pub spec_status: String,
    /// Le SLUG persisté de la catégorie du Spec, relu en base à la fin du déroulé — jamais le mot
    /// du judge, exactement comme le sélecteur `bug discipline?` du graphe.
    pub category: Option<String>,
    /// `system` | `agent` | `human` — la provenance de cette ligne de `categories`.
    pub category_created_by: Option<String>,
    pub tasks: BTreeMap<String, String>,
    pub refusal: Option<Refusal>,
    pub blackboard: Map<String, Value>,
    /// Les avertissements de `run_start`, rendus en texte.
    pub warnings: Vec<String>,
}

/// Confronte les attentes à l'observé et rend UNE LIGNE PAR DIVERGENCE,
/// `champ : attendu <x> ≠ obtenu <y>`. Vide = verdict OK.
pub fn compare(expect: &Expect, observed: &Observed) -> Vec<String> {
    let mut diffs = Vec::new();

    if let Some(want) = &expect.surfaced {
        if want.len() != observed.surfaced.len() {
            diffs.push(format!(
                "surfaced.len : attendu {} ≠ obtenu {}",
                want.len(),
                observed.surfaced.len()
            ));
        }
        for (i, want) in want.iter().enumerate() {
            let got = observed.surfaced.get(i).map(String::as_str).unwrap_or("—");
            if want != got {
                diffs.push(format!("surfaced[{i}] : attendu `{want}` ≠ obtenu `{got}`"));
            }
        }
        for (i, got) in observed.surfaced.iter().enumerate().skip(want.len()) {
            diffs.push(format!("surfaced[{i}] : attendu rien ≠ obtenu `{got}`"));
        }
    }

    if let Some(want) = &expect.journal {
        if want.len() != observed.journal.len() {
            diffs.push(format!(
                "journal.len : attendu {} ≠ obtenu {}",
                want.len(),
                observed.journal.len()
            ));
        }
        for (i, entry) in want.iter().enumerate() {
            let Some(got) = observed.journal.get(i) else {
                continue;
            };
            compare_opt(
                &mut diffs,
                &format!("journal[{i}].slot"),
                entry.slot.as_deref(),
                got.slot.as_deref(),
            );
            compare_opt(
                &mut diffs,
                &format!("journal[{i}].outcome"),
                entry.outcome.as_deref(),
                Some(got.outcome.as_str()),
            );
            compare_opt(
                &mut diffs,
                &format!("journal[{i}].expect"),
                entry.expect.as_deref(),
                Some(got.expect.as_str()),
            );
            if let Some(want) = entry.satisfied {
                if got.satisfied != want {
                    diffs.push(format!(
                        "journal[{i}].satisfied : attendu `{want}` ≠ obtenu `{}`",
                        got.satisfied
                    ));
                }
            }
            if let Some(want) = entry.blocking {
                if got.blocking != want {
                    diffs.push(format!(
                        "journal[{i}].blocking : attendu `{want}` ≠ obtenu `{}`",
                        got.blocking
                    ));
                }
            }
            // L'IDENTITÉ DU CONTRÔLE — la vérification qui donne son sens à « le rouge PUIS le vert ».
            if let Some(other) = entry.same_check_as {
                match observed.journal.get(other) {
                    None => diffs.push(format!(
                        "journal[{i}].same_check_as : attendu la tentative {other} ≠ obtenu aucune \
                         tentative à cet index"
                    )),
                    Some(reference) => {
                        if got.command_line != reference.command_line {
                            diffs.push(format!(
                                "journal[{i}].command_line : attendu celle de journal[{other}] \
                                 (`{}`) ≠ obtenu `{}`",
                                reference.command_line, got.command_line
                            ));
                        }
                        if got.cwd != reference.cwd {
                            diffs.push(format!(
                                "journal[{i}].cwd : attendu celui de journal[{other}] (`{}`) ≠ \
                                 obtenu `{}`",
                                reference.cwd, got.cwd
                            ));
                        }
                        if got.node_id == reference.node_id {
                            diffs.push(format!(
                                "journal[{i}].node_id : attendu un nœud DISTINCT de journal[{other}] \
                                 ≠ obtenu le même (`{}`)",
                                got.node_id
                            ));
                        }
                    }
                }
            }
        }
    }

    if let Some(want) = &expect.spec_status {
        if want != &observed.spec_status {
            diffs.push(format!(
                "spec_status : attendu `{want}` ≠ obtenu `{}`",
                observed.spec_status
            ));
        }
    }

    if let Some(want) = &expect.category {
        compare_opt(&mut diffs, "category", Some(want), observed.category.as_deref());
    }

    if let Some(want) = &expect.category_created_by {
        compare_opt(
            &mut diffs,
            "category_created_by",
            Some(want),
            observed.category_created_by.as_deref(),
        );
    }

    if let Some(want) = &expect.tasks {
        for (title, status) in want {
            let got = observed.tasks.get(title).map(String::as_str).unwrap_or("—");
            if got != status {
                diffs.push(format!(
                    "tasks[{title}] : attendu `{status}` ≠ obtenu `{got}`"
                ));
            }
        }
    }

    match (&expect.refusal, &observed.refusal) {
        (Some(want), Some(got)) => {
            if want.code != got.code {
                diffs.push(format!(
                    "refusal.code : attendu `{}` ≠ obtenu `{}`",
                    want.code, got.code
                ));
            }
            if let Some(fragment) = &want.message_contains {
                if !got.message.contains(fragment.as_str()) {
                    diffs.push(format!(
                        "refusal.message : attendu contient `{fragment}` ≠ obtenu `{}`",
                        got.message
                    ));
                }
            }
        }
        (Some(want), None) => diffs.push(format!(
            "refusal : attendu `{}` ≠ obtenu aucun refus",
            want.code
        )),
        (None, Some(got)) => diffs.push(format!(
            "refusal : attendu aucun refus ≠ obtenu `{}` — {}",
            got.code, got.message
        )),
        (None, None) => {}
    }

    if let Some(want) = &expect.blackboard {
        for (key, value) in want {
            let got = observed.blackboard.get(key);
            if got != Some(value) {
                diffs.push(format!(
                    "blackboard[{key}] : attendu `{value}` ≠ obtenu `{}`",
                    got.map(ToString::to_string).unwrap_or_else(|| "—".into())
                ));
            }
        }
    }

    if let Some(want) = &expect.warnings_contain {
        for fragment in want {
            if !observed
                .warnings
                .iter()
                .any(|w| w.contains(fragment.as_str()))
            {
                diffs.push(format!(
                    "warnings : attendu un avertissement contenant `{fragment}` ≠ obtenu {}",
                    if observed.warnings.is_empty() {
                        "aucun avertissement".to_string()
                    } else {
                        format!("{:?}", observed.warnings)
                    }
                ));
            }
        }
    }

    diffs
}

/// Compare un champ attendu OPTIONNEL : rien d'attendu ⇒ rien à dire.
fn compare_opt(diffs: &mut Vec<String>, field: &str, want: Option<&str>, got: Option<&str>) {
    let Some(want) = want else { return };
    if got != Some(want) {
        diffs.push(format!(
            "{field} : attendu `{want}` ≠ obtenu `{}`",
            got.unwrap_or("—")
        ));
    }
}

// ============================================================================
// Chargement + mise en place
// ============================================================================

impl Scenario {
    /// Désérialise un scénario, en nommant le CHEMIN JSON fautif quand il est invalide.
    ///
    /// `origin` est le nom rendu dans le message (un chemin de fichier, en général).
    pub fn parse(text: &str, origin: &str) -> Result<Scenario, DriverError> {
        // Un éditeur Windows pose volontiers un BOM en tête ; JSON ne le tolère pas, et « expected
        // value at line 1 column 1 » n'apprend rien à personne.
        let text = text.trim_start_matches('\u{feff}');
        let scenario = serde_json::from_str::<Scenario>(text).map_err(|e| {
            DriverError::Scenario(format!(
                "{origin} ({} l.{} c.{}) : {e}",
                json_path_at(text, e.line(), e.column()),
                e.line(),
                e.column(),
            ))
        })?;
        scenario.check_file_names()?;
        Ok(scenario)
    }

    /// REFUSE, au chargement, tout nom de fichier que le simulateur ne peut pas manipuler sans
    /// danger — voir [`check_file_name`] pour ce qu'un nom mal formé produirait.
    fn check_file_names(&self) -> Result<(), DriverError> {
        for (slot, sentinels) in &self.setup.slots {
            for sentinel in sentinels {
                if let Sentinel::GreenWhenFileExists(name) = sentinel {
                    check_file_name(&format!("`setup.slots.{slot}`"), name)?;
                }
            }
        }
        for answer in &self.answers {
            match &answer.effect {
                Some(EffectDecl::WriteFile(name)) => {
                    check_file_name("`answers[].effect.write_file`", name)?
                }
                Some(EffectDecl::RemoveFile(name)) => {
                    check_file_name("`answers[].effect.remove_file`", name)?
                }
                None => {}
            }
        }
        Ok(())
    }

    /// Charge un scénario depuis un fichier.
    pub fn load(path: &Path) -> Result<Scenario, DriverError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| DriverError::Scenario(format!("{} : {e}", path.display())))?;
        Scenario::parse(&text, &path.display().to_string())
    }

    /// Les lignes de commande à lier, slot par slot, dans le dialecte du shell de CETTE machine.
    pub fn slot_cmdlines(&self, shell: Shell) -> BTreeMap<String, Vec<String>> {
        self.setup
            .slots
            .iter()
            .map(|(slot, sentinels)| {
                (
                    slot.clone(),
                    sentinels.iter().map(|s| s.render(shell)).collect(),
                )
            })
            .collect()
    }

    /// Les tâches à créer, dans l'ordre du fichier.
    pub fn task_seeds(&self) -> Vec<TaskSeed> {
        self.setup
            .spec
            .tasks
            .iter()
            .map(|t| TaskSeed {
                title: t.title.clone(),
                kind: t
                    .kind
                    .clone()
                    .unwrap_or_else(|| nyx_core::db::TASK_KIND_SLICE.to_string()),
                phase_name: t.phase.clone().or_else(|| self.setup.spec.phases.first().cloned()),
            })
            .collect()
    }

    /// ARME les slots puis crée le Spec du scénario. Rend `(spec_id, {titre → task_id})`.
    ///
    /// Un seul endroit fait ce geste : la CLI et le test d'intégration passent tous les deux par ici.
    ///
    /// # Pourquoi aucune garde de CAPACITÉ ici
    ///
    /// Un scénario qui nomme une catégorie de Spec ne peut pas s'exécuter sur un arbre antérieur au
    /// PRD catégorie — mais ce n'est pas une chose à détecter à l'exécution : ce fichier appelle
    /// `nyx_core::category::set_spec_category_by_label` et `nyx_core::db::get_category`, donc sur un
    /// tel arbre le simulateur ne COMPILE pas. Le compilateur EST la garde, et une détection
    /// d'exécution posée à côté ne pourrait jamais rendre autre chose que « supporté ».
    pub fn apply(
        &self,
        setup: &super::Setup<'_>,
        title: &str,
    ) -> Result<(String, BTreeMap<String, String>), DriverError> {
        for (slot, cmdlines) in self.slot_cmdlines(Shell::of_platform()) {
            setup.bind_slot(&slot, &cmdlines)?;
        }
        setup.spec(
            title,
            &self.setup.spec.status,
            self.setup.spec.category.as_deref(),
            &self.setup.spec.phases,
            &self.task_seeds(),
        )
    }
}

/// Tout ce qu'un déroulé complet a produit : la trace brute, l'observé, et le verdict.
pub struct Replay {
    /// Le répertoire jetable. Il n'existe plus au retour, sauf si `keep` était demandé.
    pub base: std::path::PathBuf,
    pub trace: super::Trace,
    pub observed: Observed,
    /// Vide = verdict OK.
    pub diffs: Vec<String>,
}

/// REJOUE un scénario de bout en bout : base jetable, mise en place, pilotage, observation,
/// verdict.
///
/// Un seul endroit fait ce geste. La CLI (`main.rs`) l'appelle et IMPRIME ; le test d'intégration
/// l'appelle et ASSERTE. Rien ne peut donc diverger entre ce que le développeur lance et ce que la
/// suite prouve.
pub fn replay(
    scenario: &Scenario,
    label: &str,
    provider: &str,
    keep: bool,
) -> Result<Replay, DriverError> {
    // La sandbox est déclarée AVANT la base : les locaux tombent dans l'ordre inverse, donc la `Db`
    // relâche ses handles avant que le répertoire ne soit supprimé (Windows).
    let mut sandbox = super::Sandbox::open(label)?;
    if keep {
        sandbox.keep();
    }
    let base = sandbox.path().to_path_buf();
    let db = sandbox.open_db()?;

    let setup = super::Setup::new(&db, sandbox.path(), "sim")?;
    let (spec_id, task_ids) = scenario.apply(&setup, &format!("Spec {label}"))?;

    let driver = super::Driver::new(&db, sandbox.path(), provider);
    let trace = driver.run(&scenario.setup.scope, &spec_id, scenario)?;

    let spec = db
        .with_conn(|c| nyx_core::db::get_spec(c, &spec_id))
        .ok()
        .flatten();
    let spec_status = spec
        .as_ref()
        .map(|s| s.status.clone())
        .unwrap_or_default();
    // LE SLUG PERSISTÉ, relu par le même chemin que le fait `spec_category` du moteur (`specs`
    // vers `categories`, et le SLUG jamais le nom) : ce que le graphe a écrit, pas ce que le judge
    // a dit.
    let category_row = spec.as_ref().and_then(|s| {
        let id = s.category_id.clone()?;
        db.with_conn(|c| nyx_core::db::get_category(c, &id))
            .ok()
            .flatten()
    });
    let category = category_row.as_ref().map(|c| c.slug.clone());
    let category_created_by = category_row.as_ref().map(|c| c.created_by_type.clone());
    let mut tasks = BTreeMap::new();
    for (title, id) in &task_ids {
        let status = db
            .with_conn(|c| nyx_core::db::get_task(c, id))
            .ok()
            .flatten()
            .map(|t| t.status)
            .unwrap_or_default();
        tasks.insert(title.clone(), status);
    }

    let observed = Observed {
        surfaced: trace.surfaced.iter().map(|s| s.signature()).collect(),
        journal: trace.gates.clone(),
        spec_status,
        category,
        category_created_by,
        tasks,
        refusal: trace.refusal.clone(),
        blackboard: trace.blackboard.clone(),
        warnings: trace.warnings.clone(),
    };
    // LE PLANCHER D'ASSERTIONS EST UNE DIVERGENCE COMME UNE AUTRE, et il est calculé ICI : un
    // scénario qui n'asserte rien ne doit pas rendre `OK` à la CLI pendant qu'il fait rougir le
    // test d'intégration. Les deux surfaces lisent le même `diffs`, donc elles rendent le même
    // verdict — ce que le doc-comment de cette fonction promet.
    let mut diffs = compare(&scenario.expect, &observed);
    if let Some(missing) = scenario.expect.missing_assertions() {
        diffs.insert(0, missing);
    }
    Ok(Replay {
        base,
        trace,
        observed,
        diffs,
    })
}

/// Le CHEMIN JSON (`$.setup.slots.build_check[0]`) de la position `line`/`column` (1-basées, telles
/// que `serde_json` les rapporte).
///
/// `serde_json` nomme le champ fautif mais ne dit QUE `line`/`column` ; un lecteur veut le chemin.
/// Le calcul se fait ici, sur le texte source, avec une pile de conteneurs — plutôt que d'ajouter
/// une dépendance à la crate pour un exemple.
///
/// La colonne de `serde_json` est un décalage en OCTETS depuis le dernier saut de ligne, pas un
/// nombre de caractères : la marche ci-dessous avance donc de `len_utf8()`. Compter les caractères
/// faisait dépasser la position réelle dès qu'un accent précédait l'erreur sur la même ligne, et le
/// chemin rendu franchissait des accolades avant d'avoir empilé le conteneur fautif — la fonction
/// dégénérait précisément sur les scénarios rédigés en français.
fn json_path_at(text: &str, line: usize, column: usize) -> String {
    enum Frame {
        Obj(String),
        Arr(usize),
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut current = String::new();
    let mut expect_key = false;
    let (mut ln, mut col) = (1usize, 0usize);

    for ch in text.chars() {
        col += ch.len_utf8();
        if ln > line || (ln == line && col > column) {
            break;
        }
        if in_string {
            if escaped {
                escaped = false;
                current.push(ch);
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
                if expect_key {
                    if let Some(Frame::Obj(key)) = stack.last_mut() {
                        *key = current.clone();
                    }
                    expect_key = false;
                }
            } else {
                current.push(ch);
            }
            if ch == '\n' {
                ln += 1;
                col = 0;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                current.clear();
            }
            '{' => {
                stack.push(Frame::Obj(String::new()));
                expect_key = true;
            }
            '[' => stack.push(Frame::Arr(0)),
            '}' | ']' => {
                stack.pop();
                expect_key = false;
            }
            ',' => match stack.last_mut() {
                Some(Frame::Obj(_)) => expect_key = true,
                Some(Frame::Arr(index)) => *index += 1,
                None => {}
            },
            '\n' => {
                ln += 1;
                col = 0;
            }
            _ => {}
        }
    }

    let mut path = String::from("$");
    for frame in &stack {
        match frame {
            Frame::Obj(key) if !key.is_empty() => {
                path.push('.');
                path.push_str(key);
            }
            Frame::Obj(_) => {}
            Frame::Arr(index) => path.push_str(&format!("[{index}]")),
        }
    }
    path
}

// ============================================================================
// Tests du format
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Les deux verdicts nus sont la MÊME ligne partout — la propriété qui permet à `green` /
    /// `red` de ne porter aucune branche.
    #[test]
    fn a_bare_verdict_is_the_same_line_on_every_shell() {
        for shell in [Shell::Posix, Shell::PowerShell, Shell::Cmd] {
            assert_eq!(Sentinel::Green.render(shell), "exit 0");
            assert_eq!(Sentinel::Red.render(shell), "exit 1");
        }
    }

    /// La sentinelle « fix » est rendue dans les trois dialectes, chacun avec sa propre syntaxe de
    /// test d'existence — aucune ligne n'a besoin de quoi que ce soit sur le `PATH`.
    #[test]
    fn the_file_sentinel_is_rendered_in_each_dialect() {
        let sentinel = Sentinel::GreenWhenFileExists("fixed".into());
        // Une GARDE qui sort tôt, jamais `exit $?` : `fish` refuse `$?` à l'analyse, et une ligne
        // que le shell refuse sort non nulle — donc un ROUGE qui n'a rien mesuré.
        assert_eq!(
            sentinel.render(Shell::Posix),
            "test -f 'fixed' || exit 1; exit 0"
        );
        assert_eq!(
            sentinel.render(Shell::PowerShell),
            "if (Test-Path -LiteralPath 'fixed') { exit 0 } else { exit 1 }"
        );
        assert_eq!(
            sentinel.render(Shell::Cmd),
            "if exist fixed (exit 0) else (exit 1)"
        );
    }

    /// Les trois sortes se lisent bien du JSON que les scénarios écrivent.
    #[test]
    fn the_three_sentinels_deserialize_from_their_json_spelling() {
        let read = |text: &str| serde_json::from_str::<Sentinel>(text).expect(text);
        assert_eq!(read("\"green\""), Sentinel::Green);
        assert_eq!(read("\"red\""), Sentinel::Red);
        assert_eq!(
            read("{\"green_when_file_exists\": \"fixed\"}"),
            Sentinel::GreenWhenFileExists("fixed".into())
        );
    }

    fn scenario_with(answers: Value) -> Scenario {
        let text = json!({
            "description": "table de réponses",
            "setup": { "scope": "ndev", "spec": { "status": "ready" } },
            "answers": answers,
        })
        .to_string();
        Scenario::parse(&text, "test").expect("scénario valide")
    }

    /// Le matching, clé par clé : label seul, prompt_contains seul, les deux, subject.
    #[test]
    fn an_entry_matches_on_every_key_it_names_and_only_those() {
        let scenario = scenario_with(json!([
            { "when": { "type": "spawn_subagent", "label": "audit" }, "reply": { "diff": "L" } },
            { "when": { "type": "ask_human", "prompt_contains": "in review" }, "reply": { "decision": "approve" } },
            { "when": { "type": "ask_human", "prompt_contains": "rework", "label": "budget" }, "reply": { "decision": "revise" } },
            { "when": { "type": "spawn_subagent", "subject": "user_stories" }, "reply": { "diff": "S" } },
        ]));

        // label seul — et le label du NŒUD compte autant que celui de l'action.
        assert_eq!(
            scenario
                .reply_for(&json!({ "type": "spawn_subagent", "label": "audit" }), "")
                .unwrap()
                .value,
            json!({ "diff": "L" })
        );
        assert_eq!(
            scenario
                .reply_for(&json!({ "type": "spawn_subagent" }), "audit")
                .unwrap()
                .value,
            json!({ "diff": "L" })
        );
        // prompt_contains seul.
        assert_eq!(
            scenario
                .reply_for(
                    &json!({ "type": "ask_human", "prompt": "the Spec is in review" }),
                    "review"
                )
                .unwrap()
                .value,
            json!({ "decision": "approve" })
        );
        // les DEUX : le prompt correspond mais pas le label ⇒ pas de match.
        assert!(scenario
            .reply_for(
                &json!({ "type": "ask_human", "prompt": "keep reworking?" }),
                "autre"
            )
            .is_none());
        assert_eq!(
            scenario
                .reply_for(
                    &json!({ "type": "ask_human", "prompt": "keep reworking?" }),
                    "budget"
                )
                .unwrap()
                .value,
            json!({ "decision": "revise" })
        );
        // subject.
        assert_eq!(
            scenario
                .reply_for(
                    &json!({ "type": "spawn_subagent", "subject": "user_stories" }),
                    ""
                )
                .unwrap()
                .value,
            json!({ "diff": "S" })
        );
        assert!(scenario
            .reply_for(
                &json!({ "type": "spawn_subagent", "subject": "out_of_scope" }),
                ""
            )
            .is_none());
    }

    /// La PREMIÈRE entrée qui matche gagne : l'ordre du fichier est la priorité.
    #[test]
    fn the_first_matching_entry_wins() {
        let scenario = scenario_with(json!([
            { "when": { "type": "spawn_subagent", "subject": "user_stories" }, "reply": { "diff": "spécifique" } },
            { "when": { "type": "spawn_subagent" }, "reply": { "diff": "général" } },
        ]));
        assert_eq!(
            scenario
                .reply_for(&json!({ "type": "spawn_subagent", "subject": "user_stories" }), "")
                .unwrap()
                .value,
            json!({ "diff": "spécifique" }),
        );
        assert_eq!(
            scenario
                .reply_for(&json!({ "type": "spawn_subagent", "subject": "autre" }), "")
                .unwrap()
                .value,
            json!({ "diff": "général" }),
        );
    }

    /// La clé rendue dans la trace : `prompt_contains`, sinon `subject`, sinon `label`.
    #[test]
    fn the_traced_key_is_the_prompt_fragment_then_the_subject_then_the_label() {
        let scenario = scenario_with(json!([
            { "when": { "type": "ask_human", "prompt_contains": "in review", "label": "park" }, "reply": {} },
            { "when": { "type": "spawn_subagent", "subject": "user_stories" }, "reply": {} },
            { "when": { "type": "judge", "label": "which category?" }, "reply": {} },
        ]));
        let key = |action: Value, node_label: &str| {
            scenario.reply_for(&action, node_label).unwrap().key
        };
        // L'entrée nomme un prompt ET un label : c'est le PROMPT qui donne la clé.
        assert_eq!(
            key(
                json!({ "type": "ask_human", "prompt": "the Spec is in review" }),
                "park"
            ),
            Some("in review".to_string())
        );
        assert_eq!(
            key(json!({ "type": "spawn_subagent", "subject": "user_stories" }), ""),
            Some("user_stories".to_string())
        );
        assert_eq!(
            key(json!({ "type": "judge" }), "which category?"),
            Some("which category?".to_string())
        );
    }

    /// Une action qu'aucune entrée ne couvre ne reçoit RIEN : c'est le pilote qui casse ensuite, en
    /// imprimant l'action, son type et le label de son nœud
    /// ([`super::DriverError::NoAnswer`]).
    #[test]
    fn an_orphan_action_yields_no_reply_and_the_error_names_type_and_label() {
        let scenario = scenario_with(json!([
            { "when": { "type": "spawn_subagent" }, "reply": {} },
        ]));
        assert!(scenario
            .reply_for(&json!({ "type": "judge", "prompt": "?" }), "which category?")
            .is_none());

        let rendered = DriverError::NoAnswer {
            action: json!({ "type": "judge", "prompt": "?" }),
            node_label: "which category?".into(),
            run_id: "run-1".into(),
        }
        .to_string();
        assert!(rendered.contains("judge"), "{rendered}");
        assert!(rendered.contains("which category?"), "{rendered}");
    }

    /// Une clé inconnue est refusée, et le message porte le CHEMIN JSON fautif.
    #[test]
    fn an_unknown_key_is_refused_with_its_json_path() {
        let text = r#"{
  "description": "x",
  "setup": {
    "scope": "ndev",
    "spec": { "status": "ready", "statuz": "ready" }
  }
}"#;
        let err = Scenario::parse(text, "bad.json").expect_err("clé inconnue");
        let rendered = err.to_string();
        assert!(
            rendered.contains("$.setup.spec"),
            "le chemin fautif est nommé : {rendered}"
        );
        assert!(rendered.contains("statuz"), "{rendered}");
    }

    /// Le chemin fautif reste juste quand un ACCENT précède l'erreur sur la même ligne.
    ///
    /// La colonne de `serde_json` est un décalage en OCTETS ; compter les caractères faisait
    /// dépasser la position et rendre `$` au lieu du chemin. Les scénarios sont rédigés en
    /// français, donc le cas est la règle, pas l'exception.
    #[test]
    fn the_faulty_path_survives_a_multibyte_character_on_the_same_line() {
        let err = Scenario::parse(
            r#"{"description": "éééééééé", "setup": {"scope": "ndev", "spec": {"status": "ready", "statuz": 1}}}"#,
            "t.json",
        )
        .expect_err("`statuz` n'est pas un champ connu");
        let rendered = err.to_string();
        assert!(
            rendered.contains("$.setup.spec"),
            "le chemin fautif est nommé malgré les accents : {rendered}"
        );
    }

    /// Un nom de fichier hors de l'alphabet fermé est REFUSÉ au chargement, des deux côtés.
    ///
    /// Une apostrophe casse le quoting de la ligne de commande — et un gate dont la ligne est
    /// refusée par le shell sort non nul, donc SATISFAIT un `expect: red` sans avoir rien mesuré.
    /// Un nom absolu, lui, sortirait du répertoire jetable par le `join` d'un effet.
    #[test]
    fn a_file_name_outside_the_closed_alphabet_is_refused_at_load() {
        let load = |slots: Value, answers: Value| {
            Scenario::parse(
                &json!({
                    "description": "d",
                    "setup": { "scope": "ndev", "spec": { "status": "ready" }, "slots": slots },
                    "answers": answers,
                    "expect": { "surfaced": ["done"], "spec_status": "completed" },
                })
                .to_string(),
                "t.json",
            )
        };

        let err = load(json!({ "repro_check": [{ "green_when_file_exists": "it's fixed" }] }), json!([]))
            .expect_err("l'apostrophe casse le quoting")
            .to_string();
        assert!(err.contains("it's fixed"), "{err}");
        assert!(err.contains("setup.slots.repro_check"), "{err}");

        let err = load(
            json!({}),
            json!([{ "when": { "type": "judge" }, "effect": { "remove_file": "C:\\Users\\kris\\notes.txt" },
                    "reply": { "verdict": "ok" } }]),
        )
        .expect_err("un nom absolu sort de la sandbox")
        .to_string();
        assert!(err.contains("effect.remove_file"), "{err}");

        let err = load(
            json!({}),
            json!([{ "when": { "type": "judge" }, "effect": { "write_file": "../escape" },
                    "reply": { "verdict": "ok" } }]),
        )
        .expect_err("un composant parent sort de la sandbox")
        .to_string();
        assert!(err.contains("../escape"), "{err}");

        // Le nom que les scénarios livrés emploient reste accepté.
        assert!(load(json!({ "repro_check": [{ "green_when_file_exists": "fixed" }] }), json!([])).is_ok());
    }

    /// Le PLANCHER d'un `expect` : un bloc vide ne prouve rien, et le dit.
    ///
    /// Sans ce plancher, un scénario livré avec `"expect": {}` déroule ses actions, exécute ses
    /// gates sur de vrais PTY et sort en `0` — vérifié par la revue de phase 3 sur
    /// `ndev-bug-manual` : « OK — 15 action(s) surfacée(s), 1 tentative(s) de gate », zéro
    /// assertion.
    #[test]
    fn an_expect_block_that_asserts_nothing_says_what_it_is_missing() {
        let floor = |value: Value| {
            serde_json::from_value::<Expect>(value)
                .expect("attentes")
                .missing_assertions()
        };

        // Rien du tout — le point de départ de la recette d'ajout d'un scénario.
        let reason = floor(json!({})).expect("un bloc vide ne prouve rien");
        assert!(reason.contains("surfaced"), "{reason}");
        // Un `surfaced` VIDE ne vaut pas mieux qu'absent.
        let reason = floor(json!({ "surfaced": [] })).expect("un transcript vide ne prouve rien");
        assert!(reason.contains("surfaced"), "{reason}");

        // Un transcript, mais aucune attente d'ARRIVÉE.
        let reason = floor(json!({ "surfaced": ["done"] })).expect("il manque l'arrivée");
        assert!(reason.contains("spec_status"), "{reason}");
        assert!(reason.contains("journal"), "{reason}");
        assert!(reason.contains("refusal"), "{reason}");
        // Les champs qui AFFINENT un verdict n'en fondent pas un.
        assert!(floor(json!({
            "surfaced": ["done"],
            "category": "bug",
            "tasks": { "t0": "done" },
            "blackboard": { "gate_green": true },
            "warnings_contain": ["repro_check"],
        }))
        .is_some());

        // Chacune des trois arrivées suffit, prise seule.
        assert_eq!(floor(json!({ "surfaced": ["done"], "spec_status": "completed" })), None);
        assert_eq!(
            floor(json!({ "surfaced": ["done"], "refusal": { "code": "invalid_state" } })),
            None
        );
        // …y compris un journal VIDE : `[]` affirme qu'aucun gate n'a tourné.
        assert_eq!(floor(json!({ "surfaced": ["done"], "journal": [] })), None);
    }

    /// Une sentinelle inconnue est refusée, et le message porte le chemin de l'élément fautif.
    #[test]
    fn an_unknown_sentinel_is_refused_with_its_json_path() {
        let text = r#"{
  "description": "x",
  "setup": {
    "scope": "ndev",
    "spec": { "status": "ready" },
    "slots": { "build_check": ["green", "greenish"] }
  }
}"#;
        let err = Scenario::parse(text, "bad.json").expect_err("sentinelle inconnue");
        let rendered = err.to_string();
        assert!(
            rendered.contains("$.setup.slots.build_check[1]"),
            "le chemin fautif est nommé : {rendered}"
        );
    }

    /// Le verdict : aucune divergence ⇒ vide ; une divergence par champ, lisible.
    #[test]
    fn the_verdict_lists_one_line_per_diverging_field() {
        let expect: Expect = serde_json::from_value(json!({
            "surfaced": ["spawn_subagent:auditeur", "done"],
            "journal": [{ "slot": "build_check", "outcome": "passed" }],
            "spec_status": "completed",
            "tasks": { "t0": "done" },
            "blackboard": { "spec_status": "ready" },
        }))
        .expect("attentes");

        let observed = Observed {
            surfaced: vec!["spawn_subagent:auditeur".into(), "done".into()],
            journal: vec![attempt("build_check", "green", "passed", true)],
            spec_status: "completed".into(),
            category: None,
            category_created_by: None,
            tasks: [("t0".to_string(), "done".to_string())].into_iter().collect(),
            refusal: None,
            blackboard: json!({ "spec_status": "ready" })
                .as_object()
                .cloned()
                .unwrap(),
            warnings: Vec::new(),
        };
        assert!(
            compare(&expect, &observed).is_empty(),
            "{:?}",
            compare(&expect, &observed)
        );

        // Une divergence par champ, chacune nommée.
        let mut diverging = observed;
        diverging.spec_status = "in_review".into();
        diverging.surfaced[1] = "ask_human:oups".into();
        diverging.tasks.insert("t0".into(), "todo".into());
        diverging.journal[0].outcome = "failed".into();
        let diffs = compare(&expect, &diverging);
        assert_eq!(diffs.len(), 4, "{diffs:#?}");
        assert!(diffs.iter().any(|d| d.starts_with("surfaced[1] :")), "{diffs:#?}");
        assert!(
            diffs
                .iter()
                .any(|d| d.starts_with("journal[0].outcome :")),
            "{diffs:#?}"
        );
        assert!(diffs.iter().any(|d| d.starts_with("spec_status :")), "{diffs:#?}");
        assert!(diffs.iter().any(|d| d.starts_with("tasks[t0] :")), "{diffs:#?}");
    }

    /// Un refus attendu qui ne vient pas — et un refus qui vient sans être attendu — divergent tous
    /// les deux : le silence n'est jamais une réussite.
    #[test]
    fn a_missing_or_unexpected_refusal_both_diverge() {
        let expecting: Expect = serde_json::from_value(json!({
            "refusal": { "code": "invalid_state", "message_contains": "repro_check" },
        }))
        .expect("attentes");
        let diffs = compare(&expecting, &Observed::default());
        assert!(diffs.iter().any(|d| d.starts_with("refusal :")), "{diffs:#?}");

        let observed = Observed {
            refusal: Some(Refusal {
                code: "invalid_state".into(),
                message: "le slot repro_check n'est pas armé".into(),
                data: None,
            }),
            ..Default::default()
        };
        assert!(compare(&expecting, &observed).is_empty());
        assert!(compare(&Expect::default(), &observed)
            .iter()
            .any(|d| d.starts_with("refusal :")));
    }

    /// La POLARITÉ décide de la lecture d'une issue : sur un `expect: red`, un `failed` est la
    /// preuve ATTENDUE, et une attente qui se serait trompée de polarité diverge en nommant les deux
    /// champs — ce que le journal reconstruit depuis le blackboard ne savait pas dire.
    #[test]
    fn the_declared_polarity_and_its_satisfaction_are_both_compared() {
        // Le rouge satisfait de la reproduction : rien à redire.
        let red: Expect = serde_json::from_value(json!({
            "journal": [{ "slot": "repro_check", "expect": "red", "outcome": "failed", "satisfied": true }],
        }))
        .expect("attentes");
        let observed = Observed {
            journal: vec![attempt("repro_check", "red", "failed", true)],
            ..Default::default()
        };
        assert!(compare(&red, &observed).is_empty(), "{:?}", compare(&red, &observed));

        // LE MÊME `outcome: failed`, mais produit par un gate qui déclarait `green` : c'est un
        // ÉCHEC, pas une preuve. L'attente écrite pour la reproduction diverge alors sur les DEUX
        // champs — c'est exactement la confusion que la seule lecture de l'issue rendait invisible.
        let build_gate_red = Observed {
            journal: vec![attempt("repro_check", "green", "failed", false)],
            ..Default::default()
        };
        let diffs = compare(&red, &build_gate_red);
        assert!(diffs.iter().any(|d| d.contains("journal[0].expect")), "{diffs:#?}");
        assert!(
            diffs.iter().any(|d| d.contains("journal[0].satisfied")),
            "{diffs:#?}"
        );
        // …et l'`outcome`, lui, est identique dans les deux lectures : il ne pouvait donc pas les
        // distinguer.
        assert!(
            !diffs.iter().any(|d| d.contains("journal[0].outcome")),
            "{diffs:#?}"
        );
    }

    /// La DISCIPLINE d'un gate — `blocking` — est comparable, et c'est ce qui distingue un rouge qui
    /// REFUSE d'avancer d'un rouge qui se contente de se signaler.
    ///
    /// Les deux rendent exactement la même ligne de journal par ailleurs : même `expect`, même
    /// `outcome`, même `satisfied`. Rendre un `repro_check` non bloquant viderait donc la discipline
    /// `bug` de sa substance sans faire bouger aucune autre attente — d'où l'épinglage dans les
    /// scénarios livrés.
    #[test]
    fn the_blocking_discipline_of_a_gate_is_compared_on_its_own() {
        let blocking_red: Expect = serde_json::from_value(json!({
            "journal": [{ "slot": "repro_check", "expect": "red", "outcome": "failed",
                          "satisfied": true, "blocking": true }],
        }))
        .expect("attentes");

        let observed = Observed {
            journal: vec![attempt("repro_check", "red", "failed", true)],
            ..Default::default()
        };
        assert!(compare(&blocking_red, &observed).is_empty());

        // LE MÊME rouge satisfait, mais sur un gate qui n'impose rien à la suite du run.
        let toothless = Observed {
            journal: vec![GateAttempt {
                blocking: false,
                ..attempt("repro_check", "red", "failed", true)
            }],
            ..Default::default()
        };
        let diffs = compare(&blocking_red, &toothless);
        assert!(diffs.iter().any(|d| d.contains("journal[0].blocking")), "{diffs:#?}");
        // …et RIEN d'autre ne bouge : sans ce champ, la perte serait passée inaperçue.
        assert_eq!(diffs.len(), 1, "{diffs:#?}");

        // Une attente qui ne dit rien de la discipline ne contraint rien, comme tout champ absent.
        let silent: Expect = serde_json::from_value(json!({
            "journal": [{ "slot": "repro_check", "outcome": "failed" }],
        }))
        .expect("attentes");
        assert!(compare(&silent, &toothless).is_empty());
    }

    /// `same_check_as` : le rouge et le vert d'une reproduction doivent porter la MÊME ligne de
    /// commande, le MÊME `cwd`, et venir de DEUX nœuds distincts. Chacun des trois manquements
    /// diverge, nommément.
    #[test]
    fn same_check_as_compares_the_command_line_the_cwd_and_the_two_nodes() {
        let expect: Expect = serde_json::from_value(json!({
            "journal": [
                { "expect": "red", "outcome": "failed" },
                { "expect": "green", "outcome": "passed", "same_check_as": 0 },
            ],
        }))
        .expect("attentes");

        let mut red = attempt("repro_check", "red", "failed", true);
        red.node_id = "node-red".into();
        let mut green = attempt("repro_check", "green", "passed", true);
        green.node_id = "node-green".into();
        let ok = Observed {
            journal: vec![red.clone(), green.clone()],
            ..Default::default()
        };
        assert!(compare(&expect, &ok).is_empty(), "{:?}", compare(&expect, &ok));

        // Une AUTRE commande : le vert ne prouve plus rien du rouge.
        let mut other_line = green.clone();
        other_line.command_line = "exit 0".into();
        let diffs = compare(
            &expect,
            &Observed {
                journal: vec![red.clone(), other_line],
                ..Default::default()
            },
        );
        assert!(
            diffs.iter().any(|d| d.contains("journal[1].command_line")),
            "{diffs:#?}"
        );

        // Un AUTRE `cwd` : idem.
        let mut other_cwd = green.clone();
        other_cwd.cwd = "/ailleurs".into();
        let diffs = compare(
            &expect,
            &Observed {
                journal: vec![red.clone(), other_cwd],
                ..Default::default()
            },
        );
        assert!(diffs.iter().any(|d| d.contains("journal[1].cwd")), "{diffs:#?}");

        // Le MÊME nœud deux fois : ce ne sont pas les deux gates que le graphe déclare.
        let mut same_node = green;
        same_node.node_id = red.node_id.clone();
        let diffs = compare(
            &expect,
            &Observed {
                journal: vec![red, same_node],
                ..Default::default()
            },
        );
        assert!(
            diffs.iter().any(|d| d.contains("journal[1].node_id")),
            "{diffs:#?}"
        );
    }

    /// Un avertissement de `run_start` ATTENDU qui ne vient pas diverge — et il diverge en disant
    /// ce qu'il cherchait.
    #[test]
    fn an_expected_run_start_warning_that_never_came_diverges() {
        let expect: Expect = serde_json::from_value(json!({
            "warnings_contain": ["repro_check"],
        }))
        .expect("attentes");
        let diffs = compare(&expect, &Observed::default());
        assert!(diffs.iter().any(|d| d.starts_with("warnings :")), "{diffs:#?}");
        assert!(diffs.iter().any(|d| d.contains("repro_check")), "{diffs:#?}");

        let observed = Observed {
            warnings: vec!["gate_check 'reproduction' … slot repro_check …".to_string()],
            ..Default::default()
        };
        assert!(compare(&expect, &observed).is_empty());
    }

    /// Une tentative de gate, telle que `gate_attempts` la rend — le vehicule des tests de
    /// [`compare`] au-dessus. La ligne de commande est une CONSTANTE ARBITRAIRE et jamais une
    /// commande réelle : rien n'est exécuté ici, seules l'égalité et la différence sont mesurées.
    fn attempt(slot: &str, expect: &str, outcome: &str, satisfied: bool) -> GateAttempt {
        GateAttempt {
            slot: Some(slot.to_string()),
            outcome: outcome.to_string(),
            node_id: format!("node-{slot}"),
            command_line: "<la ligne du slot>".to_string(),
            cwd: "<le cwd du projet>".to_string(),
            exit_code: Some(if outcome == "passed" { 0 } else { 1 }),
            expect: expect.to_string(),
            satisfied,
            declared: expect == "red",
            blocking: true,
        }
    }
}
