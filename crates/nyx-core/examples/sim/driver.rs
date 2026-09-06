//! Le PILOTE du simulateur de run : ce que chaque gate e2e Rust réécrit à la main (`tick`,
//! `answer`, `drive_coder_child`) rendu réutilisable et indépendant du workflow.
//!
//! # Ce qu'il est
//!
//! Le driver JOUE LE ROLE DU DISPATCHER, exactement comme une session d'agent réelle : il ouvre un
//! run par [`nyx_core::mcp_engine::run_start`], boucle sur [`nyx_core::mcp_engine::nyx_next`],
//! descend dans les `sub_workflow`, enregistre un tour humain quand l'action l'exige, et renvoie à
//! chaque tick le `last_result` que la table de réponses lui dicte. Les gates, les transitions et le
//! journal sont ceux du moteur : aucun stub, aucun `pub(crate)` élargi dans `nyx-core`.
//!
//! Il ne rend AUCUN jugement : un `judge` reçoit le verdict écrit dans le scénario. Il prouve le
//! CHEMIN (quelle branche, quels gates, quels statuts), pas la pertinence d'un LLM.
//!
//! # Structure (et pourquoi elle est ce qu'elle est)
//!
//! Ce fichier est inclus DEUX FOIS : par `main.rs` (`mod driver;`) et par le test d'intégration
//! `tests/sim_scenarios_e2e.rs` (`#[path = "../examples/sim/driver.rs"] mod driver;`). Il ne
//! contient donc AUCUN chemin `crate::` interne à l'exemple : le module `scenario` est déclaré ici
//! comme ENFANT (`#[path = "scenario.rs"] pub mod scenario;`) et n'atteint le driver que par
//! `super::`. Les deux inclusions voient alors le même arbre de modules.
//!
//! Les `#[cfg(test)] mod tests` de cet exemple ne tournent PAS sous `cargo run --example` (un
//! exemple n'est pas une cible de test) : ils tournent quand le test d'intégration inclut ce fichier,
//! ce qui est précisément la raison d'être de cette inclusion.

// Le driver est une BIBLIOTHEQUE incluse par deux cibles qui n'en consomment pas les mêmes moitiés
// (la CLI n'appelle pas ce que les tests appellent, et réciproquement) : le `dead_code` d'une cible
// est du code vivant dans l'autre.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nyx_core::db::{self, CommandSource, Db, SpecSections, SpecTransition};
use nyx_core::mcp::RpcError;
use nyx_core::mcp_engine::{nyx_next, run_start, CallerSession, EngineRuntime};
use serde_json::{json, Map, Value};

#[path = "scenario.rs"]
pub mod scenario;

// ============================================================================
// Dialecte du shell : duplication ASSUMEE de `crates/nyx-core/src/test_cmdline.rs`
// ============================================================================

/// La FAMILLE du shell qui interprétera vraiment la ligne d'une commande gérée.
///
/// # Duplication assumée
///
/// `crate::test_cmdline` (dans `crates/nyx-core/src/test_cmdline.rs`) porte la MEME famille de
/// shell, résolue de la même façon, pour exactement la même raison : la règle de `CLAUDE.md`,
/// **jamais de ligne de commande littérale** dans un test qui arme une commande gérée, parce que le
/// piège a deux bouches et que la seconde (un test qui attend du ROUGE et reste vert des deux côtés
/// en n'assertant rien) est silencieuse. Mais ce module est `pub(crate)` : il n'existe pas hors de la
/// crate, et l'exemple `sim` ne compile que contre l'API PUBLIQUE de `nyx_core` (aucun `pub(crate)`
/// n'est élargi pour lui, décision d'implémentation du PRD). Les ~30 lignes sont donc réécrites ici,
/// sciemment. Un commentaire croisé le dit dans `src/test_cmdline.rs`.
///
/// Cette moitié-ci — les trois familles et `shell_stem` — est un décalque à l'identique. Les LIGNES,
/// elles, ne le sont pas toutes : voir [`scenario::Sentinel`], qui dit laquelle des trois diverge et
/// pourquoi.
///
/// La résolution est celle de [`nyx_core::pty::resolve_shell`], la MEME que
/// `command::CommandPty::spawn` effectue, donc le simulateur ne devine jamais quel interpréteur
/// reçoit sa ligne.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shell {
    /// `bash` / `sh` / `zsh` / ... : `-c`.
    Posix,
    /// `pwsh` / `powershell` : `-Command`. Le défaut Windows.
    PowerShell,
    /// `cmd.exe` : `/C`. Le dernier recours Windows.
    Cmd,
}

impl Shell {
    /// La famille dans laquelle les commandes gérées de CETTE machine tournent.
    pub fn of_platform() -> Self {
        Shell::of(&nyx_core::pty::resolve_shell())
    }

    /// La famille d'un chemin de shell donné.
    pub fn of(shell: &str) -> Self {
        match shell_stem(shell).as_str() {
            "pwsh" | "powershell" => Shell::PowerShell,
            "cmd" => Shell::Cmd,
            _ => Shell::Posix,
        }
    }
}

/// Le stem d'un chemin de shell, extension Windows retirée : copie de
/// `nyx_core::command::shell_stem`, qui est `pub(crate)` (voir [`Shell`]).
fn shell_stem(shell: &str) -> String {
    let base = shell
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(shell)
        .to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stripped) = base.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    base
}

// ============================================================================
// Base jetable
// ============================================================================

/// Le répertoire jetable d'un scénario, et la base qu'il porte.
///
/// La base est ouverte par [`nyx_core::db::Db::open`], le VRAI chemin de boot : migrations puis
/// seeds des workflows intégrés (ndev / nspec / ndoc). Jamais la base de l'app : ni contention
/// SQLite, ni risque de transiter un vrai Spec.
///
/// Supprimé au `Drop`, sauf [`Sandbox::keep`]. La `Db` doit être relâchée AVANT (déclarer la
/// sandbox en premier, la base ensuite : les locaux tombent dans l'ordre inverse de déclaration),
/// sinon Windows tient encore le fichier.
pub struct Sandbox {
    dir: PathBuf,
    keep: bool,
}

impl Sandbox {
    /// Crée `<temp>/nyx-sim-<name>-<pid>-<nanos>/`, un répertoire neuf par exécution, pour que
    /// deux runs simultanés (ou un `--keep` oublié) ne se marchent jamais dessus.
    pub fn open(name: &str) -> Result<Sandbox, DriverError> {
        let dir = std::env::temp_dir().join(format!(
            "nyx-sim-{}-{}-{}",
            sanitize(name),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir)
            .map_err(|e| DriverError::Setup(format!("créer {} : {e}", dir.display())))?;
        Ok(Sandbox { dir, keep: false })
    }

    /// Ouvre la base par le vrai chemin de boot (`Db::open` migre PUIS seede).
    pub fn open_db(&self) -> Result<Db, DriverError> {
        Db::open(&self.dir.join("nyx.db"))
            .map_err(|e| DriverError::Setup(format!("Db::open dans {} : {e}", self.dir.display())))
    }

    /// Le répertoire : c'est aussi le workspace racine du projet, donc le `cwd` des gates.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Conserver le répertoire après le `Drop` (option `--keep`), pour l'inspecter.
    pub fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        // Windows relâche parfois les handles SQLite avec un temps de retard : quelques essais
        // valent mieux qu'un répertoire temporaire abandonné.
        for attempt in 0..10 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
        }
    }
}

/// Un nom de scénario réduit à ce qui tient dans un nom de dossier.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

// ============================================================================
// Erreurs de pilotage
// ============================================================================

/// Ce qui arrête le pilote : jamais un `panic!`, toujours un message qui nomme la cause.
#[derive(Debug)]
pub enum DriverError {
    /// La mise en place a échoué (base, projet, Spec, tâche, slot).
    Setup(String),
    /// Aucune entrée de la table de réponses ne matche l'action surfacée. Le pilote ne DEVINE
    /// jamais une réponse : il imprime l'action complète et s'arrête (méthode `coder_question` : un
    /// prompt reformulé casse bruyamment plutôt que d'être mal répondu).
    NoAnswer {
        action: Value,
        /// Le label du nœud, que l'action ne porte pas toujours (seul un `spawn_subagent`
        /// l'embarque), et qui est pourtant ce qu'une entrée `when.label` vise.
        node_label: String,
        run_id: String,
    },
    /// La borne anti-emballement a sauté.
    Runaway { run_id: String, ticks: usize },
    /// Un effet déclaré (`write_file` / `remove_file`) n'a pas pu être exécuté.
    Effect(String),
    /// Le scénario lui-même est invalide (fichier, JSON, sentinelle inconnue).
    Scenario(String),
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriverError::Setup(m) => write!(f, "mise en place impossible : {m}"),
            DriverError::NoAnswer {
                action,
                node_label,
                run_id,
            } => write!(
                f,
                "aucune réponse scriptée pour l'action `{}` du nœud `{node_label}` (run {run_id}) : \
                 ajoutez une entrée `answers[]` qui la matche.\n{}",
                action["type"].as_str().unwrap_or("?"),
                serde_json::to_string_pretty(action).unwrap_or_else(|_| action.to_string()),
            ),
            DriverError::Runaway { run_id, ticks } => write!(
                f,
                "emballement : le run {run_id} n'a pas terminé en {ticks} ticks"
            ),
            DriverError::Effect(m) => write!(f, "effet impossible : {m}"),
            DriverError::Scenario(m) => write!(f, "scénario invalide : {m}"),
        }
    }
}

impl std::error::Error for DriverError {}

// ============================================================================
// Mise en place : projet, Spec, tâches, commandes gérées, slots
// ============================================================================

/// Une tâche à créer sur le Spec.
#[derive(Debug, Clone)]
pub struct TaskSeed {
    pub title: String,
    /// `slice` (défaut) | `human` | `review`.
    pub kind: String,
    /// La phase (par NOM, PDR-0022) ; `None` = plat.
    pub phase_name: Option<String>,
}

impl TaskSeed {
    pub fn slice(title: &str, phase_name: Option<&str>) -> Self {
        TaskSeed {
            title: title.to_string(),
            kind: db::TASK_KIND_SLICE.to_string(),
            phase_name: phase_name.map(str::to_string),
        }
    }
}

/// La mise en place d'un scénario : le projet dont la racine EST le répertoire jetable (pour qu'un
/// gate y tourne et qu'une sentinelle y atterrisse), le Spec, ses phases, ses tâches, et les
/// commandes gérées liées aux slots.
pub struct Setup<'a> {
    db: &'a Db,
    project_id: String,
    workspace_id: String,
}

impl<'a> Setup<'a> {
    /// Un projet vivant dont le workspace racine est `root`.
    pub fn new(db: &'a Db, root: &Path, project_name: &str) -> Result<Self, DriverError> {
        let root_str = root
            .to_str()
            .ok_or_else(|| DriverError::Setup(format!("chemin non UTF-8 : {}", root.display())))?;
        let (project, workspace) = db
            .with_conn(|c| db::create_project(c, project_name, root_str, None))
            .map_err(|e| DriverError::Setup(format!("create_project : {e}")))?;
        Ok(Setup {
            db,
            project_id: project.id,
            workspace_id: workspace.id,
        })
    }

    /// Le Spec du scénario : sections minimales, CATÉGORIE éventuelle, plan de phases posé, tâches
    /// créées, puis conduit à `status` par les VRAIES transitions gardées (`draft` vers `ready` vers
    /// `in_progress`).
    ///
    /// L'ordre compte : passé `draft`, une tâche ne peut plus nommer une phase que le plan ne porte
    /// pas (K2), donc le plan est posé et les tâches créées AVANT la première transition.
    /// Retourne `(spec_id, {titre vers task_id})`.
    ///
    /// `category` est un LABEL (un slug système comme `bug` / `feature`, ou un nom libre), écrit par
    /// [`nyx_core::category::set_spec_category_by_label`] — le MEME service que le nœud
    /// `set_category` du graphe et que l'outil MCP appellent, jamais une écriture directe sur
    /// `specs.category_id` : un scénario qui part `bug` part exactement comme un Spec que `/nspec` a
    /// classé. `None` = NON CLASSÉ, le cas d'un Spec legacy — que le préflight du coder rattrape par
    /// son `judge which category?`.
    pub fn spec(
        &self,
        title: &str,
        status: &str,
        category: Option<&str>,
        phases: &[String],
        tasks: &[TaskSeed],
    ) -> Result<(String, BTreeMap<String, String>), DriverError> {
        if !(status == db::SPEC_STATUS_DRAFT
            || status == db::SPEC_STATUS_READY
            || status == db::SPEC_STATUS_IN_PROGRESS)
        {
            return Err(DriverError::Setup(format!(
                "statut de Spec de départ non supporté : `{status}` (draft | ready | in_progress)"
            )));
        }

        let spec_id = self
            .db
            .with_conn(|c| {
                db::create_spec(
                    c,
                    &self.project_id,
                    title,
                    SpecSections {
                        problem: "le simulateur rejoue ce parcours sans agent".into(),
                        solution: "dérouler le workflow seedé avec des réponses scriptées".into(),
                        ..Default::default()
                    },
                )
            })
            .map_err(|e| DriverError::Setup(format!("create_spec : {e}")))?
            .0
            .id;

        if let Some(label) = category {
            self.db
                .with_conn(|c| {
                    nyx_core::category::set_spec_category_by_label(
                        c,
                        &spec_id,
                        label,
                        db::DomainEventSource::Mcp,
                    )
                })
                .map_err(|e| DriverError::Setup(format!("catégorie `{label}` : {e}")))?;
        }

        if !phases.is_empty() {
            let plan: Vec<Value> = phases
                .iter()
                .enumerate()
                .map(|(i, name)| json!({ "number": i + 1, "name": name }))
                .collect();
            let plan = Value::Array(plan).to_string();
            self.db
                .with_conn(|c| db::set_spec_phases(c, &spec_id, &plan))
                .map_err(|e| DriverError::Setup(format!("set_spec_phases : {e}")))?;
        }

        let mut ids = BTreeMap::new();
        for task in tasks {
            // Un `kind` inconnu est REFUSÉ, jamais replié sur `slice` : une faute de frappe
            // mesurerait un autre chemin que celui déclaré, et rendrait un verdict vert dessus.
            // C'est la méthode `coder_question` du module — ce qui n'est pas compris casse en le
            // nommant plutôt que d'être deviné.
            let input = match task.kind.as_str() {
                k if k == db::TASK_KIND_SLICE => db::NewTaskInput::slice(),
                k if k == db::TASK_KIND_HUMAN => db::NewTaskInput::human("checked by hand"),
                k if k == db::TASK_KIND_REVIEW => db::NewTaskInput::review(db::TASK_SEVERITY_HIGH),
                other => {
                    return Err(DriverError::Setup(format!(
                        "tâche `{}` : `kind` inconnu `{other}`. Valeurs acceptées : {}, {}, {}.",
                        task.title,
                        db::TASK_KIND_SLICE,
                        db::TASK_KIND_HUMAN,
                        db::TASK_KIND_REVIEW,
                    )))
                }
            };
            let created = self
                .db
                .with_conn(|c| {
                    db::create_task(
                        c,
                        &self.project_id,
                        &task.title,
                        db::NewTaskInput {
                            spec_id: Some(spec_id.clone()),
                            phase_name: task.phase_name.clone(),
                            ..input
                        },
                    )
                })
                .map_err(|e| DriverError::Setup(format!("create_task {} : {e:?}", task.title)))?;
            // Le titre est la CLÉ par laquelle `expect.tasks` et le bloc `Statuts` désignent une
            // tâche : deux homonymes n'en laisseraient qu'une, et une première restée `todo`
            // passerait un `expect.tasks` que la seconde a satisfait. Le verdict certifierait un
            // état qu'il n'a pas observé, donc le doublon est refusé.
            if let Some(previous) = ids.insert(task.title.clone(), created.id) {
                return Err(DriverError::Setup(format!(
                    "deux tâches portent le titre `{}` (la première est `{previous}`). Le titre est \
                     la clé de `expect.tasks` et du bloc `Statuts` : donnez-leur des titres \
                     distincts.",
                    task.title
                )));
            }
        }

        // Les transitions SENSIBLES sont jouées confirmées : le simulateur tient la place de
        // l'utilisateur qui a dit oui, il ne contourne pas la garde.
        if status != db::SPEC_STATUS_DRAFT {
            self.db
                .with_conn(|c| db::transition_spec(c, &spec_id, SpecTransition::Ready, true))
                .map_err(|e| DriverError::Setup(format!("transition ready : {e:?}")))?;
        }
        if status == db::SPEC_STATUS_IN_PROGRESS {
            self.db
                .with_conn(|c| {
                    db::transition_spec_with_workspace(
                        c,
                        &spec_id,
                        SpecTransition::Start,
                        true,
                        Some(&self.workspace_id),
                    )
                })
                .map_err(|e| DriverError::Setup(format!("transition start : {e:?}")))?;
        }

        Ok((spec_id, ids))
    }

    /// ARME un slot de gate : une commande gérée par ligne, puis la liaison de slot du projet, la
    /// route exacte qu'un projet suit avant son premier `/ndev`. RIEN N'EST FORKE : le projet
    /// continue de courir le graphe intégré que nyx livre.
    ///
    /// Le pilote n'a aucune table `command_id` vers `slot` à tenir : le journal des gates
    /// ([`Driver::collect_gate`]) lit le slot sur la ligne de `gate_attempts`, que le moteur y écrit
    /// depuis le nœud lui-même.
    pub fn bind_slot(&self, slot: &str, cmdlines: &[String]) -> Result<(), DriverError> {
        let mut command_ids = Vec::new();
        for (i, cmdline) in cmdlines.iter().enumerate() {
            let id = self
                .db
                .with_conn(|c| {
                    let tpl = db::create_template(
                        c,
                        &self.project_id,
                        &format!("{slot}-{i}"),
                        cmdline,
                        None,
                        CommandSource::default(),
                    )?;
                    db::instance_ids_for_template(c, &tpl.id)
                })
                .map_err(|e| DriverError::Setup(format!("commande de gate `{slot}` : {e}")))?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    DriverError::Setup(format!("aucune instance matérialisée pour `{slot}`"))
                })?;
            command_ids.push(id);
        }
        self.db
            .with_conn(|c| db::bind_command_slot(c, &self.project_id, slot, &command_ids))
            .map_err(|e| DriverError::Setup(format!("bind_command_slot `{slot}` : {e}")))?;
        Ok(())
    }
}

// ============================================================================
// Table de réponses
// ============================================================================

/// L'effet qu'une réponse produit dans le répertoire du scénario AVANT d'être envoyée : poser (ou
/// retirer) la sentinelle qu'un gate lira ensuite. C'est ainsi qu'un « fix » se rend observable
/// sans qu'aucun agent n'écrive de code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    WriteFile(String),
    RemoveFile(String),
}

/// Ce que le pilote renvoie au moteur pour une action.
#[derive(Debug, Clone)]
pub struct Reply {
    /// Le `last_result` du prochain `nyx_next`.
    pub value: Value,
    /// La CLE qui identifie l'action dans la trace et dans `expect.surfaced` : le
    /// `prompt_contains` de l'entrée qui a matché, à défaut le label du nœud.
    pub key: Option<String>,
    /// L'effet à exécuter avant d'envoyer la réponse.
    pub effect: Option<Effect>,
}

impl Reply {
    pub fn new(value: Value) -> Self {
        Reply {
            value,
            key: None,
            effect: None,
        }
    }
}

/// La table de réponses, vue du pilote : « pour cette action, cette réponse, ou rien ».
///
/// Un trait plutôt qu'un type concret : le driver ne connaît pas le format de scénario.
/// [`scenario::Scenario`] l'implémente ; les tests du driver passent une fermeture.
///
/// `node_label` est fourni A COTE de l'action parce que l'action ne le porte pas toujours : seul un
/// `spawn_subagent` embarque son `label`, un `judge` ou un `ask_human` n'a que son prompt. Le label
/// est lu sur le graphe par le pilote, une fois, et c'est lui que `when.label` vise.
pub trait AnswerSource {
    fn reply_for(&self, action: &Value, node_label: &str) -> Option<Reply>;
}

impl<F> AnswerSource for F
where
    F: Fn(&Value, &str) -> Option<Reply>,
{
    fn reply_for(&self, action: &Value, node_label: &str) -> Option<Reply> {
        self(action, node_label)
    }
}

// ============================================================================
// Trace
// ============================================================================

/// Un arrêt du moteur : ce qu'il a surfacé, et ce que le pilote lui a répondu.
#[derive(Debug, Clone)]
pub struct Surfaced {
    pub tick: usize,
    pub run_id: String,
    /// 0 = le run racine, 1 = un `sub_workflow` enfant, etc.
    pub depth: usize,
    pub node_label: String,
    pub action_type: String,
    /// La partie droite de `type:clé`, la forme que `expect.surfaced` compare.
    pub key: String,
    /// Le run enfant qu'un `sub_workflow` a ouvert. JAMAIS dans [`Surfaced::signature`] : c'est un
    /// UUID, il change à chaque exécution, et une attente ne peut pas s'écrire dessus.
    pub child_run_id: Option<String>,
    pub reply: Option<Value>,
    pub blackboard_delta: Value,
    /// Les clés que ce tick a RETIREES du board. Le moteur les rend à côté du delta et jamais
    /// dedans : un `null` dans le delta ne se distinguerait pas d'une clé posée à `null`
    /// (`crates/nyx-core/src/mcp_engine.rs`, `blackboard_removed`).
    pub blackboard_removed: Vec<String>,
    pub notes: Vec<String>,
}

impl Surfaced {
    /// La forme comparée par `expect.surfaced` : `type:clé`, ou `type` seul quand la clé est vide.
    pub fn signature(&self) -> String {
        if self.key.is_empty() {
            self.action_type.clone()
        } else {
            format!("{}:{}", self.action_type, self.key)
        }
    }
}

/// Une tentative de gate observée — UNE LIGNE de la table `gate_attempts`, lue telle quelle.
///
/// # La source de vérité est la table, jamais une reconstruction
///
/// Le moteur APPEND une ligne par exécution de `gate_check`
/// ([`nyx_core::db::append_gate_attempt`], seule écriture de cette table dans le code), et
/// [`nyx_core::db::list_gate_attempts_for_spec`] les rend dans l'ordre où elles se sont produites.
/// Le pilote se contente donc de RELIRE ce journal à chaque arrêt et d'en émettre les lignes
/// nouvelles ([`Driver::collect_gate`]).
///
/// C'est ce qui rend observables les deux champs sur lesquels toute la discipline `bug` repose —
/// `expect` (la polarité DÉCLARÉE par le nœud, `red` / `green`) et `satisfied` (l'attente a-t-elle
/// été honorée) — et les deux champs que l'auditeur compare pour affirmer que le rouge et le vert
/// portent sur LE MÊME contrôle : `command_line` et `cwd`. Aucun de ces quatre n'est déductible du
/// blackboard, qui ne porte que le dernier résultat.
///
/// L'`outcome` peut valoir `indeterminate` en plus de `passed` / `failed`
/// ([`nyx_core::db::GATE_OUTCOME_INDETERMINATE`]) : un gate qui n'a pas pu rendre de verdict n'a
/// rien prouvé, et le journal le dit plutôt que de le classer comme un échec.
#[derive(Debug, Clone)]
pub struct GateAttempt {
    /// Le slot dont la commande vient, quand le nœud en déclare un.
    pub slot: Option<String>,
    /// `passed` | `failed` | `indeterminate`.
    pub outcome: String,
    /// Le nœud `gate_check` qui a déclaré ce contrôle — deux nœuds DISTINCTS pour le rouge et le
    /// vert de la reproduction, ce que le journal permet de vérifier.
    pub node_id: String,
    /// Les lignes de commande RÉSOLUES, jointes par des sauts de ligne.
    pub command_line: String,
    /// Les répertoires d'exécution résolus, joints de même.
    pub cwd: String,
    pub exit_code: Option<i32>,
    /// La polarité ATTENDUE : `green` | `red`.
    pub expect: String,
    /// L'attente a-t-elle été honorée.
    pub satisfied: bool,
    /// Le nœud a-t-il ÉCRIT son attente, par opposition au vert implicite par défaut.
    pub declared: bool,
    pub blocking: bool,
}

/// Un refus du moteur, capturé et rendu (jamais paniqué) : certains scénarios l'ATTENDENT.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub code: String,
    pub message: String,
    pub data: Option<Value>,
}

/// Tout ce que le pilotage a produit.
#[derive(Debug, Clone, Default)]
pub struct Trace {
    pub root_run_id: String,
    pub surfaced: Vec<Surfaced>,
    pub gates: Vec<GateAttempt>,
    /// Le board agrégé au fil des ticks, dans l'ordre de visite — donc le dernier écrivain gagne.
    ///
    /// Agrégé des DEUX moitiés que le tick rend : le `blackboard_delta` pose ou remplace, le
    /// `blackboard_removed` RETIRE. La seconde n'est pas un raffinement — le contrat du moteur dit
    /// qu'« une clé listée là ne tient plus, quoi qu'ait dit un delta antérieur » (`mcp.rs`,
    /// descripteur de `nyx_next`), et le moteur retire vraiment (`blocking_gate_red` d'un gate
    /// repassé vert, une fenêtre d'écriture Spec révoquée, un run enfant terminé). Ne lire que le
    /// delta rendait un board qui accumule, où une attente `expect.blackboard` passait au vert sur
    /// une clé que le moteur avait explicitement retirée.
    pub blackboard: Map<String, Value>,
    pub refusal: Option<Refusal>,
    /// Les avertissements de `run_start` (un slot non armé, par exemple).
    pub warnings: Vec<String>,
}

// ============================================================================
// Le pilote
// ============================================================================

/// Le terminal au nom duquel le simulateur tique. Un tour humain enregistré ici satisfait le
/// contrôle de consentement de `nyx_next`, exactement comme le hook `UserPromptSubmit` en
/// production.
pub const SIM_TERMINAL: &str = "term-sim";

/// La borne anti-emballement, par run.
pub const MAX_TICKS: usize = 200;

/// Les providers que le simulateur accepte.
///
/// Le MOTEUR ne rejette rien, et surtout il ne retombe PAS sur `claude` : `spawner_for_provider`
/// (`crates/nyx-core/src/engine/spawn.rs`) donne le `Task` NATIF à `claude`, `claude-code` et
/// `anthropic`, et le fallback INLINE à tout le reste — une chaîne inconnue comprise. Une faute de
/// frappe rend donc un vert sur le mécanisme de spawn inline, qui n'est pas celui qu'on croyait
/// mesurer, sans un mot ; le refus appartient au simulateur, dont le verdict n'a de valeur que si
/// l'on sait ce qui a été mesuré.
///
/// La liste porte les DEUX alias natifs en plus des trois noms que la CLI annonce : les refuser
/// aurait fait rejeter par le simulateur des valeurs que le moteur traite exactement comme
/// `claude`. `codex` et `opencode`, eux, sont indistinguables pour le moteur — ils prennent le même
/// spawner inline ; ils restent nommés parce qu'un scénario dit ce qu'il croit mesurer.
pub const PROVIDERS: [&str; 5] = ["claude", "claude-code", "anthropic", "codex", "opencode"];

/// Le provider demandé, ou un refus qui NOMME la valeur rejetée et les valeurs acceptées.
///
/// [`DriverError::Scenario`], donc la CLI sort en `2` (erreur de pilotage) — pas en `1`, qui est un
/// verdict KO : rien n'a été mesuré.
pub fn parse_provider(value: &str) -> Result<String, DriverError> {
    if PROVIDERS.contains(&value) {
        return Ok(value.to_string());
    }
    Err(DriverError::Scenario(format!(
        "provider inconnu `{value}`. Valeurs acceptées : {}.",
        PROVIDERS.join(", "),
    )))
}

/// Ce que le pilote retient d'un nœud du graphe : de quoi nommer un arrêt.
#[derive(Debug, Clone)]
struct NodeInfo {
    label: String,
}

pub struct Driver<'a> {
    db: &'a Db,
    runtime: EngineRuntime,
    provider: String,
    cwd: PathBuf,
    nodes: std::cell::RefCell<BTreeMap<String, NodeInfo>>,
    /// Le nombre de lignes de `gate_attempts` DÉJÀ recopiées dans la trace — le curseur de lecture
    /// du journal (voir [`Driver::collect_gate`]).
    gates_read: std::cell::Cell<usize>,
    ticks: std::cell::Cell<usize>,
}

impl<'a> Driver<'a> {
    /// `cwd` est le répertoire où les effets déclarés posent leurs sentinelles : le même que le
    /// workspace racine du projet, donc le `cwd` que les gates résolvent.
    pub fn new(db: &'a Db, cwd: &Path, provider: &str) -> Self {
        Driver {
            db,
            // UNE session : un seul `EngineRuntime` pour tous les ticks du run et de ses enfants.
            runtime: EngineRuntime::new(),
            provider: provider.to_string(),
            cwd: cwd.to_path_buf(),
            nodes: std::cell::RefCell::new(BTreeMap::new()),
            gates_read: std::cell::Cell::new(0),
            ticks: std::cell::Cell::new(0),
        }
    }

    /// Ouvre le run par le VRAI point d'entrée (`run_start`, ce que `/ndev` appelle) et le déroule
    /// jusqu'à `done`, ou jusqu'au refus qu'il rencontre.
    pub fn run(
        &self,
        scope: &str,
        spec_id: &str,
        answers: &dyn AnswerSource,
    ) -> Result<Trace, DriverError> {
        let mut trace = Trace::default();
        // Le curseur du journal compte des lignes DE CE SPEC : un `Driver` réutilisé sur un second
        // Spec sauterait sinon les N premières tentatives du nouveau journal, que
        // `list_gate_attempts_for_spec` rend depuis l'index 0.
        self.gates_read.set(0);

        let opened = match run_start(
            self.db,
            &self.runtime,
            &json!({ "scope": scope, "spec_id": spec_id }),
        ) {
            Ok(outcome) => outcome.result,
            Err(err) => {
                trace.refusal = Some(refusal(&err));
                return Ok(trace);
            }
        };
        trace.warnings = opened["warnings"]
            .as_array()
            .map(|a| a.iter().map(render_warning).collect())
            .unwrap_or_default();
        let run_id = opened["run_id"]
            .as_str()
            .ok_or_else(|| DriverError::Setup("run_start n'a pas rendu de run_id".into()))?
            .to_string();
        trace.root_run_id = run_id.clone();

        self.drive(&run_id, spec_id, 0, answers, &mut trace)?;
        Ok(trace)
    }

    /// La boucle de dispatch d'UN run. Rend `Ok(())` quand le run atteint `done`, ou quand un refus
    /// a été capturé dans la trace (le refus arrête tout le pilotage, y compris les parents).
    fn drive(
        &self,
        run_id: &str,
        spec_id: &str,
        depth: usize,
        answers: &dyn AnswerSource,
        trace: &mut Trace,
    ) -> Result<(), DriverError> {
        let mut last: Option<Value> = None;
        let mut steps = 0usize;

        loop {
            if steps >= MAX_TICKS {
                return Err(DriverError::Runaway {
                    run_id: run_id.to_string(),
                    ticks: MAX_TICKS,
                });
            }
            steps += 1;

            let mut args = json!({ "run_id": run_id, "provider": self.provider });
            if let Some(last) = last.take() {
                args.as_object_mut()
                    .expect("args is an object")
                    .insert("last_result".into(), last);
            }
            let out = match nyx_next(
                self.db,
                &self.runtime,
                CallerSession {
                    terminal_id: Some(SIM_TERMINAL),
                },
                &args,
            ) {
                Ok(outcome) => outcome.result,
                Err(err) => {
                    // UN REFUS EST UN RESULTAT. Le moteur refuse (gate rouge, slot non armé, tour
                    // humain manquant) : le pilote le TRACE et le rend, il ne panique pas. Des
                    // scénarios entiers existent pour l'attendre.
                    //
                    // LE JOURNAL EST RELU AVANT DE RENDRE LA MAIN : une tentative jouée sur le tick
                    // qui refuse est journalisée par le moteur AVANT que le refus ne soit
                    // construit, et sortir sans la lire laisserait le refus pour seul témoignage
                    // d'un contrôle qui a bel et bien tourné.
                    self.collect_gate(spec_id, &mut trace.gates)?;
                    // LE BOARD AUSSI. Un refus n'a NI delta NI `removed` — `nyx_next` ne les rend
                    // que sur `Ok` — alors que `refuse_gate`, `refuse_gate_expectation` et
                    // `refuse_vacuous_gate` PERSISTENT le board avant de construire l'erreur, clés
                    // repliées depuis le `last_result` de ce tick comprises. S'arrêter sur le delta
                    // laisserait `Trace.blackboard` au tick d'avant, et une attente portant sur ce
                    // que le refus a écrit divergerait contre la base qui, elle, le porte.
                    self.absorb_persisted_board(run_id, &mut trace.blackboard);
                    trace.refusal = Some(refusal(&err));
                    return Ok(());
                }
            };

            self.ticks.set(self.ticks.get() + 1);
            let tick = self.ticks.get();
            let action = out["action"].clone();
            let action_type = action["type"].as_str().unwrap_or("?").to_string();
            let node_label = self.node_label(run_id, out["current_node_id"].as_str());
            let delta = out["blackboard_delta"].clone();
            let removed: Vec<String> = out["blackboard_removed"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|k| k.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();

            self.collect_gate(spec_id, &mut trace.gates)?;
            merge_board(&mut trace.blackboard, &delta, &removed);

            let notes: Vec<String> = action["notes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|n| n.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();

            if action_type == "done" {
                trace.surfaced.push(Surfaced {
                    tick,
                    run_id: run_id.to_string(),
                    depth,
                    node_label,
                    action_type,
                    key: String::new(),
                    child_run_id: None,
                    reply: None,
                    blackboard_delta: delta,
                    blackboard_removed: removed,
                    notes,
                });
                return Ok(());
            }

            if action_type == "sub_workflow" {
                let child = action["child_run_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                trace.surfaced.push(Surfaced {
                    tick,
                    run_id: run_id.to_string(),
                    depth,
                    // La clé d'un `sub_workflow` est le LABEL du nœud, pas l'id du run enfant : une
                    // attente doit pouvoir s'écrire, et un UUID change à chaque exécution.
                    key: node_label.clone(),
                    node_label,
                    action_type,
                    child_run_id: Some(child.clone()),
                    reply: None,
                    blackboard_delta: delta,
                    blackboard_removed: removed,
                    notes,
                });
                if child.is_empty() {
                    return Err(DriverError::Setup(
                        "une action sub_workflow sans child_run_id".into(),
                    ));
                }
                // DESCENTE : l'enfant est conduit à `done`, puis le parent reprend SANS
                // `last_result` : il replie lui-même le résultat vérifié de l'enfant.
                self.drive(&child, spec_id, depth + 1, answers, trace)?;
                if trace.refusal.is_some() {
                    return Ok(());
                }
                last = None;
                continue;
            }

            // Toute autre action attend une réponse de la table.
            let reply =
                answers
                    .reply_for(&action, &node_label)
                    .ok_or_else(|| DriverError::NoAnswer {
                        action: action.clone(),
                        node_label: node_label.clone(),
                        run_id: run_id.to_string(),
                    })?;

            if let Some(effect) = &reply.effect {
                self.apply(effect)?;
            }

            // LE TOUR HUMAIN, avant la réponse. Une action marquée `human_turn_required` engage
            // quelque chose que la session ne possède pas : nyx vérifie sa propre trace des prompts
            // de cette session et refuse une réponse sans tour humain derrière elle. C'est ce que
            // le hook `UserPromptSubmit` fait en production.
            if action["human_turn_required"] == json!(true) {
                let prompt = human_turn_text(&reply.value);
                self.db
                    .with_conn(|c| db::record_human_turn(c, SIM_TERMINAL, None, &prompt))
                    .map_err(|e| DriverError::Setup(format!("record_human_turn : {e}")))?;
            }

            let key = reply
                .key
                .clone()
                .unwrap_or_else(|| action_key(&action, &node_label));
            trace.surfaced.push(Surfaced {
                tick,
                run_id: run_id.to_string(),
                depth,
                node_label,
                action_type,
                key,
                child_run_id: None,
                reply: Some(reply.value.clone()),
                blackboard_delta: delta,
                blackboard_removed: removed,
                notes,
            });
            last = Some(reply.value);
        }
    }

    /// Le label du nœud sur lequel le run est arrêté, lu sur le graphe (le tick ne rend qu'un
    /// `current_node_id`). Mis en cache : le graphe d'un run ne change pas sous lui.
    fn node_label(&self, run_id: &str, node_id: Option<&str>) -> String {
        let Some(node_id) = node_id else {
            return String::new();
        };
        self.node(run_id, node_id)
            .map(|n| n.label)
            .unwrap_or_default()
    }

    /// Un nœud du graphe du run, par id — le cache de [`Driver::node_label`]. Charge le graphe
    /// ENTIER au premier défaut de cache : un tick n'en nomme qu'un, mais le run les traversera
    /// tous, et une lecture par arrêt coûterait un aller-retour SQLite par tick.
    fn node(&self, run_id: &str, node_id: &str) -> Option<NodeInfo> {
        if let Some(hit) = self.nodes.borrow().get(node_id) {
            return Some(hit.clone());
        }
        let loaded = self.db.with_conn(|c| {
            let workflow_id = db::get_workflow_run(c, run_id)
                .ok()
                .flatten()
                .map(|r| r.workflow_id);
            workflow_id
                .and_then(|wf| db::list_nodes(c, &wf).ok())
                .unwrap_or_default()
        });
        let mut nodes = self.nodes.borrow_mut();
        for node in loaded {
            nodes.insert(node.id, NodeInfo { label: node.label });
        }
        nodes.get(node_id).cloned()
    }

    /// RELIT le journal des gates du Spec, À CHAQUE ARRÊT, et recopie dans la trace les lignes que
    /// le pilote n'a pas encore vues.
    ///
    /// # La table est la source, et elle l'est pour quatre champs que rien d'autre ne porte
    ///
    /// Le moteur APPEND une ligne par exécution de `gate_check`
    /// ([`nyx_core::db::append_gate_attempt`] — la seule écriture de cette table) et
    /// [`nyx_core::db::list_gate_attempts_for_spec`] les rend dans l'ordre où elles ont eu lieu,
    /// `sequence` croissant, TOUS RUNS CONFONDUS. Un Spec, un journal : les gates d'un `/ndev`
    /// vivent dans le run ENFANT `coder`, et un journal par run obligerait le lecteur à recoudre ce
    /// que la table donne déjà cousu.
    ///
    /// Ce que la reconstruction que ce code remplace ne savait pas rendre, et qui décide de tout ici :
    /// `expect` (la polarité DÉCLARÉE — un `expect: red` satisfait EST la preuve, pas un échec),
    /// `satisfied`, et le couple `command_line` / `cwd` par lequel l'auditeur affirme que le rouge et
    /// le vert portent sur le MÊME contrôle. Aucun n'est déductible du blackboard, qui ne garde que
    /// le dernier résultat — et qui, sur deux passages rendant le même verdict, ne changeait même
    /// pas.
    ///
    /// # Le curseur, et pourquoi il suffit
    ///
    /// [`Driver::gates_read`] compte les lignes déjà recopiées. La table étant APPEND-ONLY et
    /// ordonnée par `sequence`, les lignes au-delà du curseur sont exactement les nouvelles, dans
    /// l'ordre : pas de comptage de traversées, pas d'horloge de boucle, rien à déduire.
    ///
    /// UNE LECTURE QUI ÉCHOUE EST UNE ERREUR, jamais « aucune ligne nouvelle » : cette table est la
    /// seule source du journal comparé, et le curseur ne rattrape rien après. Avaler l'échec sur le
    /// tick qui surface `done` perdrait définitivement les tentatives de ce tick, et un scénario
    /// attendant `"journal": []` sortirait vert en n'ayant rien lu — le faux vert que ce module
    /// existe pour fermer.
    fn collect_gate(&self, spec_id: &str, out: &mut Vec<GateAttempt>) -> Result<(), DriverError> {
        let rows = self
            .db
            .with_conn(|c| db::list_gate_attempts_for_spec(c, spec_id))
            .map_err(|e| {
                DriverError::Setup(format!(
                    "lecture du journal des gates du Spec {spec_id} : {e}"
                ))
            })?;
        for row in rows.into_iter().skip(self.gates_read.get()) {
            self.gates_read.set(self.gates_read.get() + 1);
            out.push(GateAttempt {
                slot: row.slot,
                outcome: row.outcome,
                node_id: row.node_id,
                command_line: row.command_line,
                cwd: row.cwd,
                exit_code: row.exit_code,
                expect: row.expect,
                satisfied: row.satisfied,
                declared: row.declared,
                blocking: row.blocking,
            });
        }
        Ok(())
    }

    /// REPLIE sur le board agrégé ce que le run porte VRAIMENT en base, sans rien retirer.
    ///
    /// Appelé sur la seule sortie où le tick ne rend pas de delta : le REFUS. Le moteur y persiste
    /// le board (`persist(... RUN_STATUS_BLOCKED ...)`) avant de construire son `RpcError`, donc la
    /// base est en avance sur ce que le pilote a vu passer. La lecture est faite en pose seule — les
    /// retraits d'un tick refusant ne sont pas observables, et inventer une disparition serait pire
    /// que de garder une clé de trop.
    ///
    /// Une lecture qui échoue laisse le board tel quel : le refus est déjà le résultat, et le
    /// journal, lui, a sa propre garde.
    fn absorb_persisted_board(&self, run_id: &str, board: &mut Map<String, Value>) {
        let Ok(Some(run)) = self.db.with_conn(|c| db::get_workflow_run(c, run_id)) else {
            return;
        };
        if let Ok(Value::Object(persisted)) = serde_json::from_str::<Value>(&run.blackboard) {
            for (k, v) in persisted {
                board.insert(k, v);
            }
        }
    }

    /// Exécute un effet dans le répertoire du scénario (le `cwd` des gates).
    ///
    /// Le nom est REVALIDÉ ici, alors que le chargement du scénario l'a déjà fait
    /// ([`scenario::check_file_name`]) : `Path::join` remplace le préfixe quand le nom est absolu,
    /// donc un `remove_file` qui passerait outre supprimerait un fichier réel de la machine, hors
    /// du répertoire jetable et sans une trace. Une garde en amont ne suffit pas pour une écriture
    /// destructrice — celle-ci est à l'endroit où le chemin est construit.
    fn apply(&self, effect: &Effect) -> Result<(), DriverError> {
        match effect {
            Effect::WriteFile(name) => {
                scenario::check_file_name("`effect.write_file`", name)?;
                std::fs::write(self.cwd.join(name), "sim")
                    .map_err(|e| DriverError::Effect(format!("write_file {name} : {e}")))
            }
            Effect::RemoveFile(name) => {
                scenario::check_file_name("`effect.remove_file`", name)?;
                let path = self.cwd.join(name);
                if !path.exists() {
                    return Ok(());
                }
                std::fs::remove_file(&path)
                    .map_err(|e| DriverError::Effect(format!("remove_file {name} : {e}")))
            }
        }
    }
}

/// La clé par défaut d'une action : ce qui la distingue de ses sœurs sur le même type.
fn action_key(action: &Value, node_label: &str) -> String {
    action["subject"]
        .as_str()
        .or_else(|| action["label"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| node_label.to_string())
}

/// Le texte enregistré comme tour humain : la réponse elle-même, telle que l'humain l'aurait dite.
fn human_turn_text(reply: &Value) -> String {
    reply
        .get("decision")
        .and_then(Value::as_str)
        .or_else(|| reply.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| reply.to_string())
}

fn refusal(err: &RpcError) -> Refusal {
    Refusal {
        code: err.code.as_str().to_string(),
        message: err.message.clone(),
        data: err.data.clone(),
    }
}

/// Un avertissement de `run_start`, rendu lisible quel que soit son emballage.
///
/// Les avertissements du moteur sont des OBJETS (`{code, node_id, workflow_id, workflow_name,
/// blocking, slot, message}`), et c'est le champ `message` qui est rendu, jamais le JSON entier :
/// `expect.warnings_contain` compare des fragments à cette chaîne, et le comparer au JSON
/// sérialisé rendait l'attente satisfiable par un NOM DE CLÉ. `warnings_contain: ["slot"]` passait
/// alors trivialement, et `["repro_check"]` était satisfait par le champ `"slot":"repro_check"`
/// même si la phrase avait cessé de nommer le slot — l'attente « l'avertissement NOMME la chose à
/// corriger » ne prouvait plus rien. Le repli sur le JSON reste pour un emballage inattendu, qui
/// vaut mieux qu'une chaîne vide.
fn render_warning(warning: &Value) -> String {
    if let Some(text) = warning.as_str() {
        return text.to_string();
    }
    warning
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| warning.to_string())
}

/// Replie UN TICK sur le board agrégé : le delta pose ou remplace, puis `removed` RETIRE. Les ticks
/// arrivent dans l'ordre de visite (un run enfant tique APRES que son parent l'a ouvert), donc
/// appliquer au fil de l'eau donne exactement « l'enfant l'emporte sur le parent pour une même clé ».
///
/// Les deux moitiés d'un même tick sont disjointes par construction (le moteur les calcule sur le
/// même avant/après), donc l'ordre entre elles ne change rien ; ce qui compte est qu'une clé retirée
/// DISPARAISSE, quoi qu'ait dit un delta antérieur — c'est le contrat du tick, et le pilote est
/// précisément le « caller tracking the board from ticks alone » que le moteur met en garde.
fn merge_board(board: &mut Map<String, Value>, delta: &Value, removed: &[String]) {
    if let Some(obj) = delta.as_object() {
        for (k, v) in obj {
            board.insert(k.clone(), v.clone());
        }
    }
    for key in removed {
        board.remove(key);
    }
}

// ============================================================================
// Tests du pilote
// ============================================================================

#[cfg(test)]
mod tests {
    use super::scenario::Sentinel;
    use super::*;

    /// Le déroulé de référence : le même que `ndev_default_unrolls_single_session_start_to_done`
    /// (`tests/gate_v2_8_engine_e2e.rs`), mais joué par le PILOTE et une table de réponses.
    fn ndev_answers(action: &Value, _node_label: &str) -> Option<Reply> {
        match action["type"].as_str()? {
            "spawn_subagent" => Some(Reply::new(json!({ "diff": "audit clean" }))),
            "ask_human" => {
                let prompt = action["prompt"].as_str().unwrap_or("");
                let in_review = prompt.contains("in review");
                let mut reply = Reply::new(json!({
                    "decision": if in_review { "approve" } else { "yes" }
                }));
                reply.key = Some(if in_review { "in review" } else { "confirm" }.to_string());
                Some(reply)
            }
            _ => None,
        }
    }

    fn answers() -> fn(&Value, &str) -> Option<Reply> {
        ndev_answers
    }

    /// La catégorie que ces tests POSENT sur leur Spec : `feature`, l'un des quatre slugs système.
    ///
    /// Elle n'est pas décorative. Le préflight du coder CLASSE tout Spec qui arrive non classé
    /// (`crates/nyx-core/src/seed.rs`, `category_preflight`), et le fait en surfaçant un
    /// `judge which category?` — la toute première action de n'importe quel `/ndev` sur un Spec
    /// legacy. Ces tests-ci portent sur le PILOTE (descente dans le sous-workflow, tour humain,
    /// journal des gates, action orpheline), pas sur la classification : partir classé est ce qui
    /// leur laisse mesurer ce qu'ils mesurent. Le préflight, lui, a son propre scénario livré —
    /// `ndev-legacy-unclassified`.
    const CLASSIFIED: Option<&str> = Some("feature");

    /// LE CRITERE CENTRAL de la tâche 1 : un run `ndev` sur un Spec `ready` à une tâche, gate armé
    /// en `green`, va jusqu'à `done` avec le driver, ferme la tâche et complète le Spec.
    #[test]
    fn a_ndev_run_reaches_done_and_closes_the_spec_and_its_task() {
        let sandbox = Sandbox::open("driver-ndev").expect("sandbox");
        let db = sandbox.open_db().expect("db");
        let setup = Setup::new(&db, sandbox.path(), "Sim").expect("project");
        setup
            .bind_slot(
                nyx_core::seed::BUILD_CHECK_SLOT,
                &[Sentinel::Green.render(Shell::of_platform())],
            )
            .expect("arm build_check");
        let (spec_id, tasks) = setup
            .spec(
                "Spec du simulateur",
                db::SPEC_STATUS_READY,
                CLASSIFIED,
                &["Build".to_string()],
                &[TaskSeed::slice("t0", Some("Build"))],
            )
            .expect("spec");

        let driver = Driver::new(&db, sandbox.path(), "claude");
        let trace = driver
            .run("ndev", &spec_id, &answers())
            .expect("le déroulé ndev");

        assert!(
            trace.refusal.is_none(),
            "aucun refus attendu : {:?}",
            trace.refusal
        );
        assert_eq!(
            db.with_conn(|c| db::get_spec(c, &spec_id))
                .unwrap()
                .unwrap()
                .status,
            db::SPEC_STATUS_COMPLETED,
            "le driver a conduit le Spec jusqu'à completed",
        );
        assert_eq!(
            db.with_conn(|c| db::get_task(c, &tasks["t0"]))
                .unwrap()
                .unwrap()
                .status,
            db::TASK_STATUS_DONE,
            "et la tâche de la phase est fermée",
        );
        assert!(
            trace.surfaced.iter().any(|s| s.action_type == "sub_workflow"),
            "le pilote est descendu dans le sous-workflow coder",
        );
        assert!(
            trace.gates.iter().any(|g| g.slot.as_deref()
                == Some(nyx_core::seed::BUILD_CHECK_SLOT)
                && g.outcome == "passed"),
            "le journal porte la tentative verte du build_check : {:?}",
            trace.gates,
        );
    }

    /// DEUX tâches dans la même phase : le nœud `gate_check "checks"` est DANS la boucle par tâche
    /// (`crates/nyx-core/src/seed.rs`), donc `build_check` tourne DEUX fois — et les deux tentatives
    /// sont mot pour mot identiques (même commande, même `cwd`, même code de sortie, même sortie).
    ///
    /// La seconde n'apparaît donc dans AUCUN `blackboard_delta`, et un journal lu sur le board en
    /// comptait UNE : un faux vert sur une attente de N entrées, dans l'outil dont la raison d'être
    /// est de rendre un verdict fiable. La table `gate_attempts` est APPEND-ONLY — une ligne par
    /// EXÉCUTION, pas par changement de valeur — et c'est elle que le pilote recopie.
    #[test]
    fn a_repeated_identical_gate_attempt_is_journaled_once_per_execution() {
        let sandbox = Sandbox::open("driver-gate-twice").expect("sandbox");
        let db = sandbox.open_db().expect("db");
        let setup = Setup::new(&db, sandbox.path(), "Sim").expect("project");
        setup
            .bind_slot(
                nyx_core::seed::BUILD_CHECK_SLOT,
                &[Sentinel::Green.render(Shell::of_platform())],
            )
            .expect("arm build_check");
        let (spec_id, tasks) = setup
            .spec(
                "Spec à deux tranches",
                db::SPEC_STATUS_READY,
                CLASSIFIED,
                &["Build".to_string()],
                &[
                    TaskSeed::slice("t0", Some("Build")),
                    TaskSeed::slice("t1", Some("Build")),
                ],
            )
            .expect("spec");

        let driver = Driver::new(&db, sandbox.path(), "claude");
        let trace = driver
            .run("ndev", &spec_id, &answers())
            .expect("le déroulé ndev");

        assert!(
            trace.refusal.is_none(),
            "aucun refus attendu : {:?}",
            trace.refusal
        );
        let build: Vec<&GateAttempt> = trace
            .gates
            .iter()
            .filter(|g| g.slot.as_deref() == Some(nyx_core::seed::BUILD_CHECK_SLOT))
            .collect();
        assert_eq!(
            build.len(),
            2,
            "une tentative par tâche, même quand les deux verdicts sont identiques : {:?}",
            trace.gates,
        );
        assert!(
            build.iter().all(|g| g.outcome == "passed"),
            "les deux tentatives sont vertes : {build:?}",
        );
        // Le moteur a bien joué la boucle deux fois — sans quoi le journal aurait raison d'en
        // compter une, et le test ne prouverait rien.
        for title in ["t0", "t1"] {
            assert_eq!(
                db.with_conn(|c| db::get_task(c, &tasks[title]))
                    .unwrap()
                    .unwrap()
                    .status,
                db::TASK_STATUS_DONE,
                "la tâche `{title}` est fermée",
            );
        }
    }

    /// Une clé que le moteur RETIRE quitte le board agrégé, quoi qu'ait dit un delta antérieur.
    ///
    /// Le slot est armé sur la sentinelle « fix » : rouge tant que le fichier n'est pas là. La
    /// première passe de la boucle « make it green » ne le pose pas — le gate est ROUGE et le moteur
    /// ANCRE `blocking_gate_red` ; la seconde le pose — le gate passe VERT et le moteur RETIRE
    /// l'ancre (`blackboard_removed`). Une attente `expect.blackboard` qui nomme cette clé doit donc
    /// DIVERGER, au lieu d'être validée sur la valeur qu'un delta antérieur avait posée.
    #[test]
    fn a_key_the_engine_removed_no_longer_holds_in_the_aggregated_board() {
        let sandbox = Sandbox::open("driver-removed").expect("sandbox");
        let db = sandbox.open_db().expect("db");
        let setup = Setup::new(&db, sandbox.path(), "Sim").expect("project");
        setup
            .bind_slot(
                nyx_core::seed::BUILD_CHECK_SLOT,
                &[Sentinel::GreenWhenFileExists("fixed".into()).render(Shell::of_platform())],
            )
            .expect("arm build_check");
        let (spec_id, _tasks) = setup
            .spec(
                "Spec au gate d'abord rouge",
                db::SPEC_STATUS_READY,
                CLASSIFIED,
                &["Build".to_string()],
                &[TaskSeed::slice("t0", Some("Build"))],
            )
            .expect("spec");

        // La sentinelle n'est posée qu'à la DEUXIEME tranche de travail : la première laisse le
        // gate rouge, exactement comme un fix qui n'a pas encore été écrit.
        let passes = std::cell::Cell::new(0usize);
        let scripted = |action: &Value, node_label: &str| -> Option<Reply> {
            if action["type"] == json!("spawn_subagent")
                && (action["label"] == json!("implement") || node_label == "implement")
            {
                let n = passes.get();
                passes.set(n + 1);
                let mut reply = Reply::new(json!({ "diff": "la tranche" }));
                reply.key = Some("implement".to_string());
                if n > 0 {
                    reply.effect = Some(Effect::WriteFile("fixed".to_string()));
                }
                return Some(reply);
            }
            ndev_answers(action, node_label)
        };

        let driver = Driver::new(&db, sandbox.path(), "claude");
        let trace = driver
            .run("ndev", &spec_id, &scripted)
            .expect("le déroulé ndev");
        assert!(
            trace.refusal.is_none(),
            "aucun refus attendu : {:?}",
            trace.refusal
        );

        // Le moteur a bien POSE puis RETIRE l'ancre — sinon le reste du test ne prouverait rien.
        let anchored = trace
            .surfaced
            .iter()
            .find_map(|s| s.blackboard_delta.get("blocking_gate_red").cloned())
            .expect("un delta a posé `blocking_gate_red` (le gate a bien été rouge)");
        assert!(
            trace
                .surfaced
                .iter()
                .any(|s| s.blackboard_removed.iter().any(|k| k == "blocking_gate_red")),
            "un tick a retiré `blocking_gate_red` : {:?}",
            trace.surfaced.iter().map(|s| &s.blackboard_removed).collect::<Vec<_>>(),
        );
        assert!(
            !trace.blackboard.contains_key("blocking_gate_red"),
            "le board agrégé ne garde pas une clé retirée : {:?}",
            trace.blackboard,
        );

        // Et le VERDICT le dit : une attente qui nomme la clé retirée diverge.
        let expect = scenario::Expect {
            blackboard: Some(
                [("blocking_gate_red".to_string(), anchored)]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        let observed = scenario::Observed {
            blackboard: trace.blackboard.clone(),
            ..Default::default()
        };
        let diffs = scenario::compare(&expect, &observed);
        assert!(
            diffs
                .iter()
                .any(|d| d.starts_with("blackboard[blocking_gate_red]")),
            "l'attente sur la clé retirée diverge : {diffs:#?}",
        );

        // Bonus du même déroulé : rouge PUIS vert, deux tentatives, dans l'ordre — et le journal
        // dit, pour chacune, ce qui était ATTENDU et si l'attente a été honorée. Sur `build_check`
        // la polarité est le vert par défaut (le nœud ne DÉCLARE rien), donc la première tentative
        // ne satisfait pas, la seconde si : ce sont les deux champs que seule la table porte, et
        // sans eux « exit 1 » ne se distingue pas d'une reproduction réussie.
        let build: Vec<(&str, &str, bool, bool)> = trace
            .gates
            .iter()
            .filter(|g| g.slot.as_deref() == Some(nyx_core::seed::BUILD_CHECK_SLOT))
            .map(|g| {
                (
                    g.outcome.as_str(),
                    g.expect.as_str(),
                    g.satisfied,
                    g.declared,
                )
            })
            .collect();
        assert_eq!(
            build,
            [
                ("failed", "green", false, false),
                ("passed", "green", true, false),
            ],
            "{:?}",
            trace.gates,
        );
    }

    /// Un `ask_human` marqué `human_turn_required` n'est accepté que si un tour humain a été
    /// enregistré. Le pilote l'enregistre ; SANS cet enregistrement, nyx refuse. Les deux moitiés
    /// sont jouées ici, sur le même déroulé, pour que la première ne puisse pas passer par hasard.
    #[test]
    fn a_committing_ask_human_is_refused_without_a_recorded_human_turn() {
        let sandbox = Sandbox::open("driver-human-turn").expect("sandbox");
        let db = sandbox.open_db().expect("db");
        let setup = Setup::new(&db, sandbox.path(), "Sim").expect("project");
        setup
            .bind_slot(
                nyx_core::seed::BUILD_CHECK_SLOT,
                &[Sentinel::Green.render(Shell::of_platform())],
            )
            .expect("arm build_check");
        let (spec_id, _tasks) = setup
            .spec(
                "Spec du simulateur",
                db::SPEC_STATUS_READY,
                CLASSIFIED,
                &["Build".to_string()],
                &[TaskSeed::slice("t0", Some("Build"))],
            )
            .expect("spec");

        // Le pilote ordinaire enregistre le tour humain : le déroulé passe.
        let driver = Driver::new(&db, sandbox.path(), "claude");
        let ok = driver
            .run("ndev", &spec_id, &answers())
            .expect("le déroulé ndev");
        assert!(
            ok.refusal.is_none(),
            "avec le tour humain, rien n'est refusé"
        );
        let questions = ok
            .surfaced
            .iter()
            .filter(|s| s.action_type == "ask_human")
            .count();
        assert!(questions > 0, "le déroulé a bien posé au moins une question");

        // MEME déroulé, sur un Spec neuf, mais en tiquant au nom d'un terminal SANS aucun tour
        // humain : nyx refuse la réponse qui engage.
        let (spec2, _t2) = setup
            .spec(
                "Spec sans tour humain",
                db::SPEC_STATUS_READY,
                CLASSIFIED,
                &["Build".to_string()],
                &[TaskSeed::slice("t0", Some("Build"))],
            )
            .expect("spec 2");
        assert_eq!(
            drive_without_human_turn(&db, &spec2).as_deref(),
            Some("invalid_state"),
            "sans tour humain enregistré, nyx refuse la réponse qui engage",
        );
    }

    /// Le MEME déroulé, mais sans jamais enregistrer de tour humain : rendu à la main parce que
    /// c'est précisément le geste que le pilote fait et qu'il s'agit de lui retirer. Rend le code du
    /// refus rencontré, ou `None` si le déroulé a terminé.
    fn drive_without_human_turn(db: &Db, spec_id: &str) -> Option<String> {
        let runtime = EngineRuntime::new();
        let run_id = run_start(db, &runtime, &json!({ "scope": "ndev", "spec_id": spec_id }))
            .expect("run_start")
            .result["run_id"]
            .as_str()
            .unwrap()
            .to_string();
        // Un terminal qui n'a JAMAIS soumis de prompt.
        let terminal = "term-sim-sans-tour";
        let mut queue = vec![run_id];
        let mut steps = 0;
        while let Some(run) = queue.pop() {
            let mut last: Option<Value> = None;
            loop {
                steps += 1;
                assert!(steps < MAX_TICKS, "emballement");
                let mut args = json!({ "run_id": run, "provider": "claude" });
                if let Some(last) = last.take() {
                    args.as_object_mut()
                        .unwrap()
                        .insert("last_result".into(), last);
                }
                let out = match nyx_next(
                    db,
                    &runtime,
                    CallerSession {
                        terminal_id: Some(terminal),
                    },
                    &args,
                ) {
                    Ok(o) => o.result,
                    Err(e) => return Some(e.code.as_str().to_string()),
                };
                match out["action"]["type"].as_str().unwrap_or("?") {
                    "done" => break,
                    "sub_workflow" => {
                        let child = out["action"]["child_run_id"].as_str().unwrap().to_string();
                        queue.push(run.clone());
                        queue.push(child);
                        break;
                    }
                    "spawn_subagent" => last = Some(json!({ "diff": "d" })),
                    "ask_human" => last = Some(json!({ "decision": "approve" })),
                    other => panic!("action inattendue : {other}"),
                }
            }
        }
        None
    }

    /// Un refus du moteur est TRACE et RENDU, jamais paniqué : le gate `build_check` du coder n'est
    /// pas armé, `nyx_next` refuse (`invalid_state`), et le pilote rend une trace qui le porte.
    #[test]
    fn an_rpc_refusal_is_traced_and_returned_never_panicked() {
        let sandbox = Sandbox::open("driver-refusal").expect("sandbox");
        let db = sandbox.open_db().expect("db");
        let setup = Setup::new(&db, sandbox.path(), "Sim").expect("project");
        // AUCUN slot armé : le gate du coder ne vérifie rien, et nyx refuse d'avancer.
        let (spec_id, _tasks) = setup
            .spec(
                "Spec au gate non armé",
                db::SPEC_STATUS_READY,
                CLASSIFIED,
                &["Build".to_string()],
                &[TaskSeed::slice("t0", Some("Build"))],
            )
            .expect("spec");

        let driver = Driver::new(&db, sandbox.path(), "claude");
        let trace = driver
            .run("ndev", &spec_id, &answers())
            .expect("le pilotage rend un résultat, il ne panique pas");
        let refusal = trace.refusal.expect("le refus est dans la trace");
        assert_eq!(refusal.code, "invalid_state", "{refusal:?}");
        assert!(
            !refusal.message.is_empty(),
            "le refus porte son message : {refusal:?}"
        );
    }

    /// Une action qu'aucune entrée ne couvre arrête le pilote en IMPRIMANT l'action complète : il
    /// ne devine jamais une réponse.
    #[test]
    fn an_unanswered_action_stops_the_driver_naming_it() {
        let sandbox = Sandbox::open("driver-orphan").expect("sandbox");
        let db = sandbox.open_db().expect("db");
        let setup = Setup::new(&db, sandbox.path(), "Sim").expect("project");
        setup
            .bind_slot(
                nyx_core::seed::BUILD_CHECK_SLOT,
                &[Sentinel::Green.render(Shell::of_platform())],
            )
            .expect("arm build_check");
        let (spec_id, _tasks) = setup
            .spec(
                "Spec sans réponses",
                db::SPEC_STATUS_READY,
                CLASSIFIED,
                &["Build".to_string()],
                &[TaskSeed::slice("t0", Some("Build"))],
            )
            .expect("spec");

        let driver = Driver::new(&db, sandbox.path(), "claude");
        let nothing: fn(&Value, &str) -> Option<Reply> = |_, _| None;
        let err = driver
            .run("ndev", &spec_id, &nothing)
            .expect_err("aucune réponse scriptée");
        let rendered = err.to_string();
        assert!(
            matches!(err, DriverError::NoAnswer { .. }),
            "l'erreur nomme la cause : {rendered}"
        );
        assert!(
            rendered.contains("spawn_subagent"),
            "et imprime l'action complète : {rendered}"
        );
    }

    /// Un `--provider` inconnu est REFUSÉ en le nommant, avec les trois valeurs acceptées : le
    /// moteur ne rejette rien et retombe sur `claude`, donc une faute de frappe rendait un vert sur
    /// un mécanisme de spawn qui n'était pas celui qu'on croyait mesurer.
    #[test]
    fn an_unknown_provider_is_refused_naming_it_and_the_accepted_values() {
        for accepted in PROVIDERS {
            assert_eq!(
                parse_provider(accepted).expect("les trois valeurs annoncées sont acceptées"),
                accepted,
            );
        }

        let err = parse_provider("pas-un-provider").expect_err("une valeur inconnue est refusée");
        let rendered = err.to_string();
        assert!(
            matches!(err, DriverError::Scenario(_)),
            "une erreur de pilotage, donc un code de sortie 2 : {rendered}"
        );
        assert!(
            rendered.contains("pas-un-provider"),
            "le message nomme la valeur refusée : {rendered}"
        );
        for accepted in PROVIDERS {
            assert!(
                rendered.contains(accepted),
                "et les valeurs acceptées : {rendered}"
            );
        }
    }

    /// La base jetable disparaît au `Drop` ; avec `keep`, le répertoire subsiste.
    #[test]
    fn the_sandbox_is_removed_on_drop_unless_kept() {
        let path = {
            let sandbox = Sandbox::open("driver-drop").expect("sandbox");
            let path = sandbox.path().to_path_buf();
            {
                let _db = sandbox.open_db().expect("db");
            }
            assert!(path.join("nyx.db").exists(), "la base a bien été créée");
            path
        };
        assert!(!path.exists(), "le répertoire jetable est supprimé au drop");

        let kept = {
            let mut sandbox = Sandbox::open("driver-keep").expect("sandbox");
            sandbox.keep();
            sandbox.path().to_path_buf()
        };
        assert!(
            kept.exists(),
            "`keep` conserve le répertoire : {}",
            kept.display()
        );
        std::fs::remove_dir_all(&kept).ok();
    }
}
