//! La **matrice de garde** des commandes Système — qui peut faire quoi, et à quel prix.
//!
//! ## Le trou que ce module referme
//!
//! Depuis la phase 1, [`crate::system_scope::SYSTEM_SCOPE_MCP_CAPABILITIES`] ouvre TOUT le CRUD des
//! commandes à un appelant MCP visant le projet Système. C'était volontaire et temporaire :
//! l'allow-list répond à « est-ce une capacité de ce conteneur », pas à « faut-il une main humaine
//! pour l'exercer ». Les deux questions sont différentes et se posent l'une après l'autre. Ce
//! module pose la seconde.
//!
//! Sans lui, un agent peut créer une commande arbitraire sur le scope Système puis la démarrer —
//! le démarrage étant libre — et se retrouver à exécuter n'importe quoi depuis le HOME de
//! l'utilisateur. C'est le contournement précis que la matrice interdit, en deux temps : la
//! CRÉATION est protégée (rien d'arbitraire ne peut être défini), ET le `start` libre est celui
//! qui démarre la commande **telle qu'elle est définie** — sans `env` par-run. Un `env` par-run
//! change ce que le processus exécute sans changer sa ligne de commande (`NODE_OPTIONS=--require`,
//! `PATH`, `LD_PRELOAD`) : la première moitié seule laisserait la porte ouverte.
//!
//! ## Les trois verdicts, et pourquoi il en faut trois et pas deux
//!
//! * [`Verdict::Free`] — la lecture, et le `start` SANS `env` d'une instance ARRÊTÉE. Démarrer un
//!   service que l'humain a déjà défini est le geste normal d'un agent qui travaille ; l'exiger
//!   approuvé à chaque fois rendrait le protocole insupportable et donc contourné.
//! * [`Verdict::NoOp`] — `start` d'une instance DÉJÀ en cours, `stop` d'une instance DÉJÀ arrêtée.
//!   Libre comme une lecture, ET « ne consomme rien » : ces appels ne créent aucune demande et ne
//!   revendiquent aucune approbation. Les distinguer de `Free` n'est pas cosmétique — c'est ce qui
//!   empêche un agent de brûler une approbation (ou d'en réclamer une) pour un appel qui ne change
//!   rien, et l'idempotence de `start`/`stop` est une propriété que le runner garantit déjà.
//! * [`Verdict::Protected`] — tout ce qui CHANGE les services de l'utilisateur. Une demande, une
//!   décision humaine, un usage.
//!
//! `relaunch_command` est TOUJOURS protégée, même sur une instance arrêtée, et ce n'est pas une
//! incohérence avec « start libre » : un relaunch tue puis respawn, donc appliqué à un service qui
//! tourne il interrompt le travail de l'utilisateur. Un agent qui veut simplement démarrer une
//! commande arrêtée a `start_command`, qui est libre et idempotent. La règle est donc « le verbe
//! qui peut interrompre est protégé », pas « l'état actuel décide ».
//!
//! ## Ce que la matrice ne fait PAS
//!
//! Elle ne touche pas aux commandes des projets UTILISATEUR : un appel qui ne vise pas le
//! singleton sort par [`Verdict::UserScope`] avant toute autre question, et le chemin d'exécution
//! reste octet pour octet celui d'avant.
//!
//! Elle ne s'applique pas non plus aux gestes HUMAINS. Les routes IPC `command_*` du pont UI
//! réutilisent les MÊMES transitions de domaine (`db::create_template`, `db::delete_template`,
//! `db::add_to_command_group`…) sans passer par ici : une main sur le clavier EST l'autorité que
//! ce module va chercher. Ce qui est contourné est la garde MCP, jamais la règle métier.
//!
//! ## Comment une opération est LIÉE
//!
//! [`Verdict::Protected`] porte une [`ProtectedOperation`] : l'OUTIL à réexécuter, le genre du
//! verbe, la charge canonique de l'appel, la RÉVISION de la cible et, pour un groupe ou un lot, le
//! SNAPSHOT ORDONNÉ de ses membres avec leur révision. La révision est un digest des colonnes
//! signifiantes de la ligne — pas son `updated_at`, qui a une granularité de la milliseconde et ne
//! bouge pas quand deux écritures tombent dans la même. Deux commandes échangées de place dans un
//! groupe produisent donc une empreinte différente, donc une demande distincte : l'accord porte sur
//! l'ordre que l'humain a vu, jamais sur « le groupe » en général.
//!
//! ## Ce qui se passe quand l'appel est PROTÉGÉ (PDR-0030)
//!
//! L'appel ne s'exécute pas et ne repart pas non plus : il ATTEND. La demande est levée, l'humain la
//! voit tout de suite (le ledger pousse l'invalidation à l'écriture de la ligne, pas au retour de
//! l'appel), et le blocage tient jusqu'à la décision, plafonné sous le timeout de requête des
//! clients MCP. Si l'humain accorde, c'est SA décision qui exécute l'effet — via
//! [`system_approvals::grant_and_execute`], qui repasse par ce dispatcher avec la matrice suspendue
//! — et l'appel en attente rend le vrai résultat. Sinon il rend un `pending` actionnable, sans
//! secret, dans le RÉSULTAT de l'outil : le seul canal que tous les clients MCP rendent au modèle.

use diesel::sqlite::SqliteConnection;
use serde_json::{json, Value};

use crate::command::{CommandRunner, RunnerSink};
use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::system_approvals::{self, ApprovalClock, ApprovalError, ProtectedOperation};

// --- The live run state, as a seam -------------------------------------------

/// "Is this instance running right now?" — the ONE live fact the matrix consults besides the
/// database.
///
/// A trait rather than a `&CommandRunner<S>`, for the reason the whole matrix exists: the
/// difference between a free `stop` and a protected one is precisely whether a process is alive,
/// and a decision table that can only be exercised by really spawning processes is a decision table
/// that goes untested on the platform where spawning is flakiest. With the seam, every cell of the
/// matrix is reachable deterministically, on every OS, in microseconds.
pub(crate) trait RunningProbe {
    /// `true` when `instance_id` currently has a live process.
    fn is_running(&self, instance_id: &str) -> bool;

    /// `true` when ANY of `instance_ids` is running. Defaulted over [`Self::is_running`]; the real
    /// runner overrides it to take its map lock once instead of once per id.
    fn any_running(&self, instance_ids: &[String]) -> bool {
        instance_ids.iter().any(|id| self.is_running(id))
    }
}

impl<S: RunnerSink> RunningProbe for CommandRunner<S> {
    fn is_running(&self, instance_id: &str) -> bool {
        CommandRunner::is_running(self, instance_id)
    }

    fn any_running(&self, instance_ids: &[String]) -> bool {
        CommandRunner::any_running(self, instance_ids)
    }
}

// --- The verdict -------------------------------------------------------------

/// What the matrix says about ONE command-tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The call does not aim the Système scope. Nothing here applies.
    UserScope,
    /// Aimed at the Système scope, and free: a read, or an env-less `start` of a stopped instance.
    Free,
    /// Aimed at the Système scope, and a NO-OP: it changes nothing, so it neither creates a
    /// request nor consumes an approval.
    NoOp,
    /// Aimed at the Système scope, and it CHANGES the user's services.
    Protected(Box<ProtectedOperation>),
}

// --- The identity of the Système scope, read once ----------------------------

/// Every id that NAMES something inside the Système scope, read in one pass.
///
/// Precomputed as SETS rather than resolved per argument, for the same reason
/// [`crate::system_scope::guard_payload`] walks the payload's string VALUES instead of a list of
/// argument names: the MCP surface spells its ids `project_id` / `workspace_id` / `command_id` /
/// `instance_id` / `group_id` / `command_ids`, and a tool added tomorrow will spell them however it
/// likes. Membership of a set has no such holes.
#[derive(Debug, Default)]
struct ScopeIds {
    project: Option<String>,
    workspaces: Vec<String>,
    commands: Vec<String>,
    instances: Vec<String>,
    groups: Vec<String>,
}

impl ScopeIds {
    /// Read the scope's ids, or an EMPTY set when the singleton is not bootstrapped (a database
    /// opened through `Db::in_memory`): an empty set matches nothing, so an un-bootstrapped
    /// database guards nothing — the same policy `guard_payload` follows.
    fn load(conn: &mut SqliteConnection) -> diesel::QueryResult<Self> {
        let Some(scope) = crate::system_scope::load(conn)? else {
            return Ok(Self::default());
        };
        let commands: Vec<String> = db::list_templates(conn, &scope.project.id)?
            .into_iter()
            .map(|t| t.id)
            .collect();
        let instances: Vec<String> = db::list_instances_for_workspace(conn, &scope.workspace.id)?
            .into_iter()
            .map(|i| i.id)
            .collect();
        let groups: Vec<String> = db::list_command_groups(conn, &scope.project.id)?
            .into_iter()
            .map(|g| g.id)
            .collect();
        Ok(Self {
            project: Some(scope.project.id),
            workspaces: vec![scope.workspace.id],
            commands,
            instances,
            groups,
        })
    }

    /// `true` when `s` names the scope's project, its workspace, one of its commands, one of its
    /// instances or one of its groups.
    fn contains(&self, s: &str) -> bool {
        self.project.as_deref() == Some(s)
            || self.workspaces.iter().any(|x| x == s)
            || self.commands.iter().any(|x| x == s)
            || self.instances.iter().any(|x| x == s)
            || self.groups.iter().any(|x| x == s)
    }

    /// `true` when ANY string in `args` names something of the scope.
    fn aimed_by(&self, args: &Value) -> bool {
        if self.project.is_none() {
            return false;
        }
        let mut hit = false;
        visit_strings(args, &mut |s| {
            if !hit && self.contains(s) {
                hit = true;
            }
        });
        hit
    }
}

/// `true` when `value` holds AT LEAST ONE string anywhere — the cheapest necessary condition for a
/// payload to be able to name a scope id. Short-circuits on the first one; allocates nothing.
fn contains_string(value: &Value) -> bool {
    match value {
        Value::String(_) => true,
        Value::Array(items) => items.iter().any(contains_string),
        Value::Object(map) => map.values().any(contains_string),
        _ => false,
    }
}

/// Call `f` on every string ANYWHERE in `value` — the same structural walk
/// [`crate::system_scope::guard_payload`] uses, for the same reason.
fn visit_strings(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter().for_each(|i| visit_strings(i, f)),
        Value::Object(map) => map.values().for_each(|i| visit_strings(i, f)),
        _ => {}
    }
}

// --- Revisions ---------------------------------------------------------------

/// The REVISION of a command: a digest of the columns whose change would make an approval granted
/// for the old state a licence over a different one.
///
/// A digest and not `updated_at`, deliberately. `updated_at` has millisecond granularity, so two
/// edits inside one millisecond leave it unchanged — and "the command was edited between the grant
/// and the replay" is exactly the case this exists to catch. The digest changes whenever anything
/// the operation could care about changes: the name, the command line, the run folder, the
/// environment, the group membership and the position inside it.
fn command_revision(t: &db::ManagedCommand) -> String {
    let sep = '\u{1}';
    db::hash_content(&format!(
        "cmd-v1{sep}{}{sep}{}{sep}{}{sep}{}{sep}{}{sep}{}{sep}{}{sep}{}",
        t.id,
        t.name,
        t.command,
        t.subfolder.as_deref().unwrap_or(""),
        t.env.as_deref().unwrap_or(""),
        t.group_id.as_deref().unwrap_or(""),
        t.group_order,
        t.order_index,
    ))
}

/// The revision of a GROUP: its labels and shape, plus its members' order, which is the whole
/// disposition model.
fn group_revision(g: &db::CommandGroup) -> String {
    let sep = '\u{1}';
    db::hash_content(&format!(
        "group-v1{sep}{}{sep}{}{sep}{}{sep}{}",
        g.id,
        g.name.as_deref().unwrap_or(""),
        g.color.map(|c| c.to_string()).unwrap_or_default(),
        g.layout_shape,
    ))
}

/// The ORDERED member snapshot of a set of commands: `id:revision`, in the order given.
///
/// The ORDER is part of the string, so a group whose members are the same but re-arranged produces
/// a different snapshot — which is right, because re-arranging them IS the operation the human
/// would be approving.
fn members_snapshot(conn: &mut SqliteConnection, ids: &[String]) -> diesel::QueryResult<String> {
    let mut parts = Vec::with_capacity(ids.len());
    for id in ids {
        let revision = match db::get_template(conn, id)? {
            Some(t) => command_revision(&t),
            // A member that does not (or no longer) resolve is recorded as such: the snapshot must
            // still be a total function of the request, and "absent" is a state the replay has to
            // be able to notice changing.
            None => "absent".to_string(),
        };
        parts.push(format!("{id}:{revision}"));
    }
    Ok(parts.join(","))
}

/// The revision of the command named by `key` in `args`, when there is one.
fn revision_of_arg(
    conn: &mut SqliteConnection,
    args: &Value,
    key: &str,
) -> diesel::QueryResult<Option<String>> {
    let Some(id) = args.get(key).and_then(Value::as_str) else {
        return Ok(None);
    };
    Ok(db::get_template(conn, id)?.map(|t| command_revision(&t)))
}

/// The template a command INSTANCE belongs to.
fn template_of_instance(
    conn: &mut SqliteConnection,
    instance_id: &str,
) -> diesel::QueryResult<Option<db::ManagedCommand>> {
    let Some(instance) = db::get_instance(conn, instance_id)? else {
        return Ok(None);
    };
    db::get_template(conn, &instance.command_id)
}

/// The instance ids a lifecycle call targets: the one named, or every member of the named group.
fn lifecycle_targets(
    conn: &mut SqliteConnection,
    args: &Value,
) -> diesel::QueryResult<Vec<String>> {
    if let Some(group_id) = args.get("group_id").and_then(Value::as_str) {
        let Some(group) = db::get_command_group(conn, group_id)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for command_id in &group.command_ids {
            out.extend(db::instance_ids_for_template(conn, command_id)?);
        }
        return Ok(out);
    }
    if let Some(instance_id) = args.get("instance_id").and_then(Value::as_str) {
        return Ok(vec![instance_id.to_string()]);
    }
    // `{ name, workspace_id }` — resolved the same way `mcp_runtime::resolve_instance_id` does, but
    // tolerantly: an unresolvable name is not this module's error to raise (the tool itself will
    // say so, precisely), it simply targets nothing and therefore changes nothing.
    let (Some(name), Some(workspace_id)) = (
        args.get("name").and_then(Value::as_str),
        args.get("workspace_id").and_then(Value::as_str),
    ) else {
        return Ok(Vec::new());
    };
    Ok(db::list_instances_for_workspace(conn, workspace_id)?
        .into_iter()
        .filter(|r| r.name == name)
        .map(|r| r.id)
        .collect())
}

/// The (revision, member snapshot) a lifecycle call is bound to.
fn lifecycle_binding(
    conn: &mut SqliteConnection,
    args: &Value,
) -> diesel::QueryResult<(Option<String>, Option<String>)> {
    if let Some(group_id) = args.get("group_id").and_then(Value::as_str) {
        let Some(group) = db::get_command_group(conn, group_id)? else {
            return Ok((None, None));
        };
        let snapshot = members_snapshot(conn, &group.command_ids)?;
        return Ok((Some(group_revision(&group)), Some(snapshot)));
    }
    let targets = lifecycle_targets(conn, args)?;
    let Some(first) = targets.first() else {
        return Ok((None, None));
    };
    Ok((
        template_of_instance(conn, first)?
            .as_ref()
            .map(command_revision),
        None,
    ))
}

// --- The per-run environment, the one argument that makes `start` arbitrary ----

/// The operation kind a `start_command` CARRYING a per-run `env` is bound to.
///
/// A kind of its own rather than plain `start_command`, so an approval can never be read — by a
/// human on the card, or by the replay — as covering the free, env-less start: the two are
/// different operations and only one of them was ever shown to anyone.
const START_WITH_ENV_KIND: &str = "start_command.env";

/// `true` when the call brings a per-run environment of its own.
///
/// The test is deliberately on the ARGUMENT'S PRESENCE, not on what it decodes to. An `env` that is
/// absent, `null` or `{}` adds nothing to the spawn and leaves `start` free; anything else — an
/// object with at least one entry, and also a MALFORMED one — is a caller asking for something
/// beyond "run the command as defined", and is answered by an approval request rather than
/// silently. (The malformed shapes are rejected a second time by
/// [`crate::mcp_runtime::optional_env_map`] when the replay finally runs the body, so nothing here
/// depends on that decoder to be safe.)
fn carries_per_run_env(args: &Value) -> bool {
    match args.get("env") {
        None | Some(Value::Null) => false,
        Some(Value::Object(map)) => !map.is_empty(),
        Some(_) => true,
    }
}

// --- The matrix --------------------------------------------------------------

/// The command tools this module has an opinion about. A tool absent from here is not a command
/// tool and is left alone (the DOMAIN boundary, [`crate::system_scope::guard_payload`], still
/// applies to it).
pub(crate) const GUARDED_COMMAND_TOOLS: &[&str] = &[
    // free reads
    "list_commands",
    "list_system_commands",
    "get_command_output",
    "wait_for_command",
    // protected mutations
    "add_command",
    "import_commands",
    "update_command",
    "remove_command",
    "remove_commands",
    "reorder_commands",
    "manage_command_group",
    "clear_command_output",
    // lifecycle, conditional
    "start_command",
    "stop_command",
    "relaunch_command",
];

/// Decide what `tool(args)` costs on the Système scope.
///
/// Reads first, cheapest question first: a call that does not AIM the scope leaves immediately as
/// [`Verdict::UserScope`], so a user project pays one set-membership walk and nothing else.
pub(crate) fn classify(
    conn: &mut SqliteConnection,
    runner: &dyn RunningProbe,
    tool: &str,
    args: &Value,
) -> diesel::QueryResult<Verdict> {
    // The DB-FREE pre-check, the same one [`crate::system_scope::needs_scope_lookup`] makes: the
    // matrix keys on the singleton's ids appearing as string VALUES, so a payload of numbers and
    // booleans — or none at all — cannot name anything and is answered without a query.
    if !contains_string(args) {
        return Ok(Verdict::UserScope);
    }
    let ids = ScopeIds::load(conn)?;
    if !ids.aimed_by(args) {
        return Ok(Verdict::UserScope);
    }
    let project_id = ids.project.clone().unwrap_or_default();
    let bind = |kind: &str, revision: Option<String>, members: Option<String>| {
        Verdict::Protected(Box::new(ProtectedOperation::new(
            project_id.clone(),
            // The TOOL travels with the operation, because the grant now RE-DISPATCHES it: the
            // `kind` below is the matrix's own name for the verb and may qualify the tool
            // (`manage_command_group.add`), so it is a label, not an address.
            tool,
            kind,
            args,
            revision,
            members,
        )))
    };
    let verdict = match tool {
        // --- FREE: every read OF THE SCOPE ----------------------------------
        //
        // Reading what the user's services are, and what they printed, is the whole point of
        // `list_system_commands` existing. Nothing here changes a byte the user owns.
        //
        // "Of the scope" is the load-bearing part. `list_importable_scripts` reads the
        // FILESYSTEM — the singleton's root_path is the user's HOME — and is refused one gate
        // earlier, at the domain boundary, rather than answered here (see
        // [`crate::system_scope::SYSTEM_SCOPE_MCP_CAPABILITIES`], which spells out why that read
        // and no other left the allow-list).
        "list_commands" | "list_system_commands" | "get_command_output" | "wait_for_command" => {
            Verdict::Free
        }

        // --- PROTECTED: the CRUD of the command definitions -------------------
        //
        // `add_command` and `import_commands` are the ones that close the phase-1 window: with
        // them free, an agent could mint an arbitrary command line on the singleton and then start
        // it — start being free — which is arbitrary execution from the user's HOME. Protected at
        // CREATION, there is no arbitrary command LINE left to start; and the `start_command` arm
        // below protects the other half of "arbitrary", the per-run `env`, which changes what a
        // fixed command line executes without changing the line itself.
        "add_command" => bind("add_command", None, None),
        "import_commands" => bind("import_commands", None, None),
        "update_command" => bind(
            "update_command",
            revision_of_arg(conn, args, "command_id")?,
            None,
        ),
        "remove_command" => bind(
            "remove_command",
            revision_of_arg(conn, args, "command_id")?,
            None,
        ),
        "remove_commands" => {
            let ids = args
                .get("command_ids")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let snapshot = members_snapshot(conn, &ids)?;
            bind("remove_commands", None, Some(snapshot))
        }
        "reorder_commands" => {
            // The whole list is the target: a reorder is meaningful only against the exact set it
            // was computed for, and the tool itself refuses a partial list.
            let current: Vec<String> = db::list_templates(conn, &project_id)?
                .into_iter()
                .map(|t| t.id)
                .collect();
            let snapshot = members_snapshot(conn, &current)?;
            bind("reorder_commands", None, Some(snapshot))
        }
        "clear_command_output" => {
            let revision = match args.get("instance_id").and_then(Value::as_str) {
                Some(id) => template_of_instance(conn, id)?
                    .as_ref()
                    .map(command_revision),
                None => None,
            };
            bind("clear_command_output", revision, None)
        }

        // --- PROTECTED: every group action, bound to its EXACT ordered members --
        //
        // The action is part of the operation KIND (`manage_command_group.add`), not just of the
        // payload, so an approval for "add this command" cannot be replayed as "dissolve".
        "manage_command_group" => {
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let (revision, snapshot) = match args.get("group_id").and_then(Value::as_str) {
                Some(group_id) => match db::get_command_group(conn, group_id)? {
                    Some(group) => {
                        let members = members_snapshot(conn, &group.command_ids)?;
                        (Some(group_revision(&group)), Some(members))
                    }
                    None => (None, None),
                },
                // `create` names no group yet: the members ARE the payload's `command_ids`, and
                // their revisions are what binds the approval to the commands the human saw.
                None => {
                    let ids = args
                        .get("command_ids")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if ids.is_empty() {
                        (None, None)
                    } else {
                        (None, Some(members_snapshot(conn, &ids)?))
                    }
                }
            };
            bind(
                &format!("manage_command_group.{action}"),
                revision,
                snapshot,
            )
        }

        // --- LIFECYCLE: the three conditional verdicts -------------------------
        //
        // `start` is FREE on a stopped instance — an agent starting a service the human already
        // defined is the normal case, and demanding a click for it would make the protocol
        // something to route around. On an instance already running it is a NO-OP: the runner
        // refuses the double spawn on its own, so the call changes nothing and must therefore
        // neither create a request nor spend one.
        //
        // …UNLESS the call carries a per-run `env`. "Free" is licensed by "the command line is the
        // one the human defined", and `start_command`'s `env` (the ONLY argument of this tool
        // besides the target-naming ones — there is no `cwd`, no `args`, no `shell`) is precisely
        // the argument that makes a fixed command line run something else:
        // `NODE_OPTIONS=--require=…`, a `PATH` pointing at an attacker-controlled folder,
        // `LD_PRELOAD`. So the cut is on the ARGUMENT, not on the state: a start that brings an
        // `env` the template does not carry is a protected mutation, bound — like `relaunch`
        // already is — through the canonical payload, so the human approves the EXACT map.
        //
        // Tested BEFORE the running/NoOp question on purpose: were it after, a call classified
        // NoOp because the target happens to be up would carry its hostile `env` unbound into a
        // spawn the moment the process exits between the classification and the effect.
        "start_command" => {
            if carries_per_run_env(args) {
                let (revision, snapshot) = lifecycle_binding(conn, args)?;
                bind(START_WITH_ENV_KIND, revision, snapshot)
            } else {
                let targets = lifecycle_targets(conn, args)?;
                if !targets.is_empty() && targets.iter().all(|id| runner.is_running(id)) {
                    Verdict::NoOp
                } else {
                    Verdict::Free
                }
            }
        }
        // `stop` on an instance that is not running is the mirror no-op. On a RUNNING one it kills
        // a process the user is relying on, so it is protected.
        "stop_command" => {
            let targets = lifecycle_targets(conn, args)?;
            if !runner.any_running(&targets) {
                Verdict::NoOp
            } else {
                let (revision, snapshot) = lifecycle_binding(conn, args)?;
                bind("stop_command", revision, snapshot)
            }
        }
        // `relaunch` is ALWAYS protected, running or not: it kills then respawns, so on a live
        // service it interrupts the user's work. An agent that only wants to start a stopped
        // command has `start_command`, which is free and idempotent — so nothing legitimate is
        // lost by refusing to make this one conditional on the current state.
        "relaunch_command" => {
            let (revision, snapshot) = lifecycle_binding(conn, args)?;
            bind("relaunch_command", revision, snapshot)
        }

        // Not a command tool: not this module's business.
        _ => Verdict::UserScope,
    };
    Ok(verdict)
}

// --- The protocol wiring -----------------------------------------------------

/// How a protected call's ANSWER is carried back into whatever the caller's tool returns.
///
/// The protected path no longer runs the caller's `body` — the effect leaves at the human's grant,
/// from the executor — so what comes back is a JSON answer read off the ledger, and the two
/// dispatchers this guard serves return two different types (`Value` for `mcp_runtime`'s lifecycle
/// tools, `ToolOutcome` for `mcp_tools_core`'s CRUD). A trait rather than a ninth parameter: the
/// conversion is a property of the return TYPE, not a decision each call site should re-make.
///
/// A protected answer carries NO `effects`: the effect ran at the grant, and the shell that ran it
/// pushed its invalidations there. Emitting them again here would make one mutation invalidate
/// twice — and, worse, invalidate on a call that did nothing.
pub(crate) trait FromApprovalAnswer {
    fn from_answer(value: Value) -> Self;
}

impl FromApprovalAnswer for Value {
    fn from_answer(value: Value) -> Self {
        value
    }
}

impl FromApprovalAnswer for crate::mcp_tools_core::ToolOutcome {
    fn from_answer(value: Value) -> Self {
        crate::mcp_tools_core::ToolOutcome::read(value)
    }
}

/// Turn a ledger-side refusal (unknown request, wrong state, expired) into the wire error.
fn ledger_refusal(e: ApprovalError) -> RpcError {
    let code = match e {
        ApprovalError::Db(_) => RpcCode::Internal,
        _ => RpcCode::InvalidState,
    };
    let data = json!({ "code": e.code(), "reason": e.reason() });
    RpcError::new(code, e.to_string()).with_data(data)
}

/// THE PROTECTED PATH, whole: raise (or join) the request, BLOCK for the human, answer.
///
/// ## Why it blocks
///
/// Creating a command is generally the prelude to running it. An agent that is handed a ticket and
/// sent away breaks its own momentum exactly as the full-screen modal broke the user's — so the
/// call WAITS, and in the common case (a human who is looking at the screen) the agent gets the
/// real result of the real operation in one round trip.
///
/// ## Why it is capped, and at that number
///
/// The ceiling is not ours: it is the MCP CLIENT's request timeout, and blocking past it would have
/// the client abort the call at the very instant we answered it. [`APPROVAL_WAIT_CAP_MS`] sits
/// safely under the reference clients' sixty seconds, and far under the five-minute human TTL — so
/// a caller that reaches the ceiling has several more windows to wait in, and the countdown the
/// human watches is the same `expires_at` this loop reads.
///
/// ## Why the timeout is a RESULT and not an error
///
/// Because of the finding this whole protocol replaces (01M1C7CWMF4ABHRWT1DGBJTXW3): `error.data`
/// is a channel no MCP client is obliged to render to the model, and the most common one does not.
/// A tool RESULT is rendered by all of them. So the one answer the agent absolutely must be able to
/// act on — "nobody has decided yet, here is the id, keep waiting" — travels in the channel that is
/// always shown. It carries no secret, so nothing is lost by making it visible.
///
/// A DECISION, on the other hand, is an error: "the user refused" and "the window closed" are
/// refusals of the call, and their whole content is in `message`, which every client renders.
fn await_decision<R: FromApprovalAnswer>(
    db: &Db,
    clock: &dyn ApprovalClock,
    op: &ProtectedOperation,
    tool: &str,
    requested_by: Option<&str>,
) -> Result<R, RpcError> {
    let raised = db
        .with_conn(|c| system_approvals::request(c, clock, op, requested_by))
        .map_err(|e| RpcError::new(RpcCode::Internal, e.to_string()))?;
    let joined = !raised.is_new();
    let request_id = raised.approval().id.clone();

    let settled = system_approvals::wait_for_settlement(
        db,
        clock,
        &request_id,
        system_approvals::wait_cap_ms(),
    )
    .map_err(ledger_refusal)?;

    match settled.state.as_str() {
        // The human has not answered inside the window. Nothing ran; the request is still alive for
        // the rest of its TTL, and the agent is told how to keep waiting.
        system_approvals::STATE_PENDING => Ok(R::from_answer(
            system_approvals::awaiting_decision_result(&settled, joined),
        )),
        system_approvals::STATE_GRANTED => match settled.completed_at {
            // Approved AND executed: the agent gets what the operation actually returned, in one
            // round trip, exactly as if the call had simply worked.
            Some(_) => {
                let failed = settled
                    .outcome
                    .as_deref()
                    .is_some_and(|o| o.starts_with(system_approvals::OUTCOME_FAILURE));
                if failed {
                    let detail = settled.outcome.as_deref().unwrap_or("").to_string();
                    Err(RpcError::new(
                        RpcCode::InvalidState,
                        format!(
                            "the user APPROVED '{tool}' and nyx ran it, but the operation itself \
                             failed ({detail}). The approval is spent: fix the cause and ask again \
                             if you still want it."
                        ),
                    ))
                } else {
                    Ok(R::from_answer(settled.result().unwrap_or(Value::Null)))
                }
            }
            // Approved, and the effect is still running (a `stop` waiting on a process tree can
            // outlast the window). Same shape as a pending answer: keep waiting, no secret.
            None => Ok(R::from_answer(system_approvals::status_result(&settled))),
        },
        system_approvals::STATE_DENIED => Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "the user REFUSED '{tool}' on their SYSTÈME services. Nothing was changed. Do not \
                 re-raise the same request: ask them what they want instead."
            ),
        )),
        _ => Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "the approval request for '{tool}' expired before the user answered ({}s window). \
                 Nothing was changed. Re-issue this call if it is still what you want.",
                system_approvals::APPROVAL_TTL_MS / 1000
            ),
        )),
    }
}

/// Run a DB-MUTATING command tool under the matrix.
///
/// The free paths run `body` exactly as before, on one pooled connection. The protected path does
/// NOT run it: it raises the request and waits, and the body will be run by the executor when the
/// human grants — which is what removes every secret from the agent's side of the protocol.
pub(crate) fn run_db_mutation<R: FromApprovalAnswer>(
    db: &Db,
    runner: &dyn RunningProbe,
    clock: &dyn ApprovalClock,
    tool: &str,
    args: &Value,
    requested_by: Option<&str>,
    body: impl FnOnce(&mut SqliteConnection, &Value) -> Result<R, RpcError>,
) -> Result<R, RpcError> {
    run_db_mutation_prepared(
        db,
        runner,
        clock,
        tool,
        args,
        requested_by,
        |_, _| Ok(()),
        |conn, args, ()| body(conn, args),
    )
}

/// [`run_db_mutation`] with a PREPARE step — the shape a tool needs when part of its work is slow
/// and is not a write.
///
/// `prepare` runs on the same pooled connection, BEFORE `body`, and OUTSIDE any transaction. That
/// placement is load-bearing (review 01M15QP3HVYV9Q729ZYTNJB8T2, finding
/// 01M15REGB574G6HD7WZ0MRQVXC): `import_commands`'s package.json discovery is a filesystem walk
/// that, held inside a write transaction, would keep SQLite's single write lock for its whole
/// duration and produce a "database is locked" under the user's hands.
///
/// It also never runs on an UNAPPROVED attempt: the protected path returns before reaching it, so a
/// first attempt cannot buy the caller a filesystem walk — on the Système scope, a walk of the
/// user's HOME.
pub(crate) fn run_db_mutation_prepared<P, R: FromApprovalAnswer>(
    db: &Db,
    runner: &dyn RunningProbe,
    clock: &dyn ApprovalClock,
    tool: &str,
    args: &Value,
    requested_by: Option<&str>,
    prepare: impl FnOnce(&mut SqliteConnection, &Value) -> Result<P, RpcError>,
    body: impl FnOnce(&mut SqliteConnection, &Value, P) -> Result<R, RpcError>,
) -> Result<R, RpcError> {
    // THE HUMAN ALREADY SAID YES. The executor re-enters this very dispatcher to run the charge it
    // was handed, so the matrix must stand down for exactly that call — otherwise it would classify
    // the approved operation as protected and raise a second request, endlessly. The marker is a
    // thread-local set only by `grant_and_execute`; nothing on the wire can reach it.
    if system_approvals::under_granted_authority() {
        return db.with_conn(|conn| {
            let prepared = prepare(conn, args)?;
            body(conn, args, prepared)
        });
    }
    // The verdict itself needs one connection and gives it straight back — the protected path is
    // about to block for up to forty-five seconds, and holding a pooled connection across that
    // would starve every other reader of the app.
    let verdict = db
        .with_conn(|conn| classify(conn, runner, tool, args))
        .map_err(|e| RpcError::new(RpcCode::Internal, e.to_string()))?;
    let Verdict::Protected(op) = verdict else {
        // UserScope / Free / NoOp — the call is what it always was, on one checkout.
        return db.with_conn(|conn| {
            let prepared = prepare(conn, args)?;
            body(conn, args, prepared)
        });
    };
    await_decision(db, clock, &op, tool, requested_by)
}

/// Run a RUNNER / PTY command tool under the matrix.
///
/// Same two paths as [`run_db_mutation_prepared`], and the same reason the protected one does not
/// call `body`: the effect belongs to the grant. NO POOLED CONNECTION IS HELD WHILE `body` RUNS on
/// the free path either — these bodies are `mcp_runtime`'s lifecycle tools and each takes a
/// connection of its own, so a guard holding one across the call would nest a checkout inside a
/// checkout.
pub(crate) fn run_runner_effect<R: FromApprovalAnswer>(
    db: &Db,
    runner: &dyn RunningProbe,
    clock: &dyn ApprovalClock,
    tool: &str,
    args: &Value,
    requested_by: Option<&str>,
    body: impl FnOnce() -> Result<R, RpcError>,
) -> Result<R, RpcError> {
    if system_approvals::under_granted_authority() {
        return body();
    }
    let verdict = db
        .with_conn(|c| classify(c, runner, tool, args))
        .map_err(|e| RpcError::new(RpcCode::Internal, e.to_string()))?;
    let Verdict::Protected(op) = verdict else {
        return body();
    };
    await_decision(db, clock, &op, tool, requested_by)
}

// --- The one extra MCP tool --------------------------------------------------

/// `system_approval_status` — `{ request_id, wait_ms? }` → what the human decided, and what the
/// operation produced.
///
/// ## What it is FOR, now
///
/// It is the RE-PROBE door, and it exists because the blocking call is capped by somebody else's
/// timeout. A protected call that reaches its ceiling comes back with a request id and this tool's
/// name; the agent calls it, blocks again, and eventually collects the operation's real result —
/// which the executor stored on the row when the human said yes. The agent waits either way; it
/// simply waits in several hops.
///
/// ## Why it no longer takes a token
///
/// Because there is no token (PDR-0030). The agent never replays, so it holds no credential, so
/// possession cannot be what opens this read. What it answers is a decision and a result, never an
/// authorisation: there is nothing here to steal, and the ids it answers on are v7 UUIDs handed to
/// one caller by a server bound to the loopback. Grant and deny remain UI-only db-commands, for the
/// unchanged reason that an agent which could approve its own request would make the protocol a
/// formality.
pub(crate) fn system_approval_status(
    db: &Db,
    clock: &dyn ApprovalClock,
    args: &Value,
) -> Result<Value, RpcError> {
    let request_id = args
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                "system_approval_status needs `request_id` — the id nyx gave you when it asked the \
                 user to approve your operation",
            )
        })?;
    let wait_ms = match args.get("wait_ms") {
        None | Some(Value::Null) => 0,
        Some(v) => v.as_u64().ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                "argument 'wait_ms' must be a non-negative integer number of milliseconds",
            )
        })?,
    };
    let row = system_approvals::wait_for_settlement(db, clock, request_id, wait_ms)
        .map_err(ledger_refusal)?;
    Ok(system_approvals::status_result(&row))
}

/// This module's `mod tests`, in its own file — the shape `engine`, `design`, `seed`,
/// `mcp_engine` and `mcp_authoring` already use, because the matrix is a TABLE and a table's
/// coverage is most of what there is to read about it.
#[cfg(test)]
mod tests;
