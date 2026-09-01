//! Le **scope Système** — le conteneur singleton des terminaux et commandes hors-projet.
//!
//! ## Le problème
//!
//! nyx ne sait tenir des terminaux et des commandes QUE sous un projet : un terminal se rattache à
//! un `workspace_id`, et `managed_commands.project_id` est NOT NULL. Les lanceurs transverses d'un
//! utilisateur — services, docker, scripts machine — n'appartiennent à aucun projet de code et
//! vivaient donc hors de nyx.
//!
//! ## La forme retenue : un conteneur POSITIF
//!
//! Un projet singleton `kind = 'system'` ([`crate::db::PROJECT_KIND_SYSTEM`]) avec exactement UN
//! workspace primaire, ancré au HOME absolu de la plateforme. Appartenir au scope Système, c'est
//! être rattaché à ce workspace-là — jamais « n'avoir aucun projet ».
//!
//! Cette distinction est le cœur de l'affaire. Une « absence de projet » existe déjà dans nyx, et
//! elle est SUBIE : [`crate::db::delete_project`] détache délibérément les terminaux du projet
//! supprimé (`ON DELETE SET NULL` — « interactive terminals are the deliberate exception: they are
//! detached, never closed »), qui deviennent des orphelins `workspace_id IS NULL`. Le scope Système
//! et ce fourre-tout sont DISJOINTS par construction :
//!
//! * un membre du Système a un `workspace_id` NON NULL pointant sur le workspace du singleton ;
//! * un orphelin a `workspace_id IS NULL`, et rien ne l'adopte — le bootstrap ne balaie aucune
//!   ligne existante, et le workspace Système est retiré du résolveur cwd
//!   ([`crate::db::resolvable_workspaces`]) donc l'auto-attach ne peut pas le capturer.
//!
//! ## Pourquoi le home, et pourquoi il doit être VRAI
//!
//! Le projet est un conteneur technique, mais son workspace doit porter un `root_path` réel : tout
//! le runtime existant construit son cwd de spawn par `workspace.root_path + subfolder`. Ancrer au
//! home donne aux commandes Système le cwd promis SANS ouvrir un cas particulier dans le runner.
//!
//! En contrepartie, ce chemin ne confère AUCUNE appartenance : le home est l'ancêtre de presque
//! tout, donc le workspace Système est exclu de la résolution cwd→workspace et de toute lecture Git
//! ([`crate::repository::workspace_members`] rend une liste vide pour lui). Son chemin est un lieu
//! d'exécution, pas une identité.
//!
//! ## Ce que ce module possède
//!
//! * la résolution du home (`HomeDirProvider`) et ses refus nommés ([`SystemScopeError`]) ;
//! * le bootstrap transactionnel (`ensure_system_scope`) : créer, réparer un état incomplet NON
//!   AMBIGU, refuser explicitement un état incohérent ;
//! * les lectures du scope ([`load`], `is_system_project`, [`is_system_workspace`]) ;
//! * la FRONTIÈRE DE DOMAINE, en TROIS pièces qui ne couvrent pas le même terrain.
//!
//! ## Où passe réellement la frontière
//!
//! 1. [`require_user_project`] / [`require_user_workspace`] sont appelés depuis `usecase.rs`, et de
//!    NULLE PART ailleurs : les DIX use cases d'IDENTITÉ (`archive_project`, `unarchive_project`,
//!    `set_primary_workspace`, `register_workspace`, `list_workspaces`, `rename_workspace`,
//!    `archive_workspace`, `unarchive_workspace`, `remove_workspace`, `create_repository`) — soit
//!    ONZE sites d'appel, `set_primary_workspace` appelant les deux gardes, une par id qu'il porte.
//!    Un appelant qui atteint ces use cases est gardé.
//! 2. [`guard_payload`] est appelé aux DEUX ADAPTATEURS — le pont UI/IPC
//!    (`nyx_sidecar::runtime::db_command`, en tête de son match d'une centaine d'arms) et les
//!    dispatchers MCP (`mcp_context::refuse_system_scope`). C'est LUI, et lui seul, qui couvre tout
//!    le reste : les mutations DB directes de projet ([`crate::db::update_project`],
//!    [`crate::db::delete_project`], [`crate::db::reorder_projects`],
//!    [`crate::db::set_project_collapsed`], [`crate::db::set_project_resume_agent_sessions`]) et
//!    toute la surface des agrégats produit (Spec, task, milestone, idée, document, design,
//!    directive, workflow, repository…).
//! 3. `require_user_terminal` est appelé depuis `db.rs`, et de NULLE PART ailleurs : les deux
//!    écrivains de `terminals.workspace_binding_mode` dont les arguments ne portent ni mode ni
//!    workspace ([`crate::db::detach_terminal`], [`crate::db::unpin_terminal_workspace`]). Ce
//!    qu'ils casseraient n'est lisible que dans le rattachement COURANT du terminal, donc la règle
//!    vit dans la primitive plutôt qu'aux adaptateurs — le troisième écrivain,
//!    [`crate::db::attach_terminal`], est lui gardé depuis son payload par [`guard_payload`].
//! 4. [`is_system_terminal`] est lu par les CINQ tools MCP de terminal génériques
//!    (`list/read/send_to/send_keys/close_terminal`, via
//!    `mcp_tools_core::refuse_system_terminal`). Ils portent un id de TERMINAL, que
//!    [`guard_payload`] — qui ne connaît que les ids de projet et de workspace — ne peut pas
//!    reconnaître ; d'où une quatrième pièce plutôt qu'une entrée d'allow-list. Un terminal
//!    Système est absent de la liste de l'agent et refusé nommément sur les quatre verbes ; sa
//!    conduite reste humaine.
//!
//! ## La SECONDE garde, et pourquoi elle n'est pas ici
//!
//! Tout ce qui précède répond à UNE question : « le scope Système peut-il être la cible de ce
//! verbe ». Une seconde, différente, se pose ensuite sur les verbes autorisés : « ce geste
//! exige-t-il une main humaine ». Elle vit dans [`crate::system_command_guard`] (la matrice) et
//! [`crate::system_approvals`] (le ledger one-shot), et pas ici, parce qu'elle ne se décide pas de
//! la même façon — la première se lit dans la charge de l'appel, la seconde dans l'état vivant de
//! la commande visée (`stop` d'un service arrêté est libre, `stop` d'un service qui tourne ne l'est
//! pas). Les confondre ferait de « le scope peut porter des commandes » et « un agent peut en créer
//! une » la même phrase.
//!
//! **Conséquence à connaître avant d'ajouter un appelant** : appeler `db::delete_project` (ou
//! n'importe quelle autre primitive `db::`) directement depuis un troisième chemin — un nœud de
//! graphe, le moteur, un nouveau pont — CONTOURNE la frontière. Le cœur n'est pas gardé de
//! l'intérieur ; il est gardé à ses deux portes. Un nouveau chemin d'entrée doit donc soit passer
//! par un adaptateur qui appelle [`guard_payload`], soit poser sa propre garde.

use std::fmt;
use std::path::{Path, PathBuf};

use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

use crate::db::{self, Project, Workspace};
use crate::schema::{projects, workspaces};

// --- Home resolution --------------------------------------------------------

/// The HOME directory the Système workspace anchors on, injected rather than read from the
/// environment at the point of use.
///
/// It is a seam for two reasons. The bootstrap must be testable on BOTH platform shapes from either
/// platform (a Windows `C:\Users\x` and a POSIX `/home/x` are both exercised), and the refusals —
/// absent home, non-absolute home, home not representable as UTF-8 — have to be reachable
/// deterministically, which `$HOME` alone cannot offer.
pub(crate) trait HomeDirProvider {
    /// The platform home directory, or `None` when none can be resolved.
    fn home_dir(&self) -> Option<PathBuf>;
}

/// The PRODUCTION provider: `$HOME` (Unix/macOS) or `$USERPROFILE` (Windows), through the SAME
/// resolver `onboarding`/`plugin` already share — one home rule in the crate, not a second copy to
/// drift.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EnvHomeDirProvider;

impl HomeDirProvider for EnvHomeDirProvider {
    fn home_dir(&self) -> Option<PathBuf> {
        crate::onboarding::home_dir()
    }
}

/// A provider pinned to an explicit value — the injection point for tests and for any shell that
/// resolves the home itself. `FixedHomeDirProvider(None)` reproduces "no home at all".
#[derive(Debug, Clone, Default)]
pub(crate) struct FixedHomeDirProvider(pub(crate) Option<PathBuf>);

impl FixedHomeDirProvider {
    /// A provider that always answers `path`.
    pub(crate) fn at(path: impl Into<PathBuf>) -> Self {
        Self(Some(path.into()))
    }

    /// A provider that resolves NO home.
    pub(crate) fn missing() -> Self {
        Self(None)
    }
}

impl HomeDirProvider for FixedHomeDirProvider {
    fn home_dir(&self) -> Option<PathBuf> {
        self.0.clone()
    }
}

/// Resolve the home directory to the canonical, ABSOLUTE string the Système workspace stores as its
/// `root_path`, or say precisely why it cannot.
///
/// NO FALLBACK. Not `.`, not `~`, not the cwd: a Système scope anchored on a guessed folder would
/// run every Système command somewhere arbitrary, and the failure would be silent. Each refusal is
/// a named [`SystemScopeError`] the caller propagates — and, because this runs BEFORE any write,
/// a refusal leaves no partial state.
///
/// Absoluteness is judged on the RAW path (`Path::is_absolute`, which knows each platform's rule)
/// and only then normalized ([`crate::pathnorm::normalize`]) into the comparison/storage form the
/// rest of nyx uses — so the stored home collapses to the same string a live `cd` would.
fn resolve_home(provider: &dyn HomeDirProvider) -> Result<String, SystemScopeError> {
    let raw = provider.home_dir().ok_or(SystemScopeError::HomeMissing)?;
    // A `PathBuf` that is not valid UTF-8 cannot be stored in a TEXT column, nor compared against a
    // normalized cwd. Refuse it by name instead of lossily transcoding it into a path that names a
    // different folder.
    let as_str = raw
        .to_str()
        .ok_or_else(|| SystemScopeError::HomeNotRepresentable(raw.to_string_lossy().into_owned()))?;
    let trimmed = as_str.trim();
    if trimmed.is_empty() {
        return Err(SystemScopeError::HomeMissing);
    }
    if !Path::new(trimmed).is_absolute() {
        return Err(SystemScopeError::HomeNotAbsolute(trimmed.to_string()));
    }
    let normalized = crate::pathnorm::normalize(trimmed);
    if normalized.is_empty() {
        return Err(SystemScopeError::HomeNotAbsolute(trimmed.to_string()));
    }
    Ok(normalized)
}

// --- Errors -----------------------------------------------------------------

/// Why a Système-scope operation was refused.
///
/// The three `Home*` arms are BOOTSTRAP refusals (no write happened). [`SystemScopeError::Forbidden`]
/// is the DOMAIN BOUNDARY refusal — the one an MCP/IPC caller sees when it aims a capability the
/// Système project does not have at the Système project. The two remaining arms are the explicitly
/// refused INCOHERENT bootstrap states: nyx repairs what is unambiguous and refuses to guess the
/// rest rather than "fixing" a database it does not understand.
#[derive(Debug)]
pub enum SystemScopeError {
    /// No home directory resolved (`$HOME` / `$USERPROFILE` absent or empty).
    HomeMissing,
    /// The home path is not representable as UTF-8 (carries its lossy spelling, for the message
    /// only — it is never stored).
    HomeNotRepresentable(String),
    /// The home path resolved but is not absolute.
    HomeNotAbsolute(String),
    /// The singleton carries MORE than one workspace. Not repairable: choosing which one is
    /// "the" primary would silently re-home every Système command.
    ExtraWorkspaces {
        project_id: String,
        workspace_ids: Vec<String>,
    },
    /// The singleton's `primary_workspace_id` names a workspace that does not belong to it.
    ForeignPrimaryWorkspace {
        project_id: String,
        workspace_id: String,
    },
    /// The scope has not been bootstrapped, so there is no home to anchor on and no workspace to
    /// attach to. [`crate::db::Db::open`] runs `ensure_system_scope` before serving anything, so
    /// PRODUCTION never sees this; a database opened through `Db::in_memory` (which
    /// migrates but does not bootstrap) does. Raised BEFORE any write, so it leaves no row.
    NotBootstrapped,
    /// The capability is not one the Système scope has. Carries the stable code
    /// [`SYSTEM_SCOPE_FORBIDDEN`] on the wire, deliberately DISTINCT from "unknown id": a caller
    /// must be able to tell "this exists and you may not" from "this does not exist", and an
    /// operation must never be refused merely because an id was hidden.
    Forbidden {
        /// The Système entity aimed at (`project` / `workspace` / `terminal`).
        entity: &'static str,
        /// Its id — returned, not hidden: concealment is not the defense here.
        id: String,
        /// The capability that was refused — an MCP tool name or a UI db-command name, as the
        /// adapter spells it.
        capability: String,
    },
    /// A SQL failure underneath.
    Db(diesel::result::Error),
}

/// The stable error code of [`SystemScopeError::Forbidden`], carried verbatim on the MCP/IPC wire.
/// Distinct from any "not found" code by construction.
pub const SYSTEM_SCOPE_FORBIDDEN: &str = "system_scope_forbidden";

impl SystemScopeError {
    /// The stable, machine-readable code of this refusal.
    pub fn code(&self) -> &'static str {
        match self {
            SystemScopeError::HomeMissing => "system_home_missing",
            SystemScopeError::HomeNotRepresentable(_) => "system_home_not_representable",
            SystemScopeError::HomeNotAbsolute(_) => "system_home_not_absolute",
            SystemScopeError::ExtraWorkspaces { .. } => "system_scope_extra_workspaces",
            SystemScopeError::ForeignPrimaryWorkspace { .. } => "system_scope_foreign_primary",
            SystemScopeError::NotBootstrapped => "system_scope_not_bootstrapped",
            SystemScopeError::Forbidden { .. } => SYSTEM_SCOPE_FORBIDDEN,
            SystemScopeError::Db(_) => "system_scope_db_error",
        }
    }

    /// `true` when this is the domain-boundary refusal (as opposed to a bootstrap/SQL failure).
    pub fn is_forbidden(&self) -> bool {
        matches!(self, SystemScopeError::Forbidden { .. })
    }
}

impl fmt::Display for SystemScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SystemScopeError::HomeMissing => write!(
                f,
                "cannot bootstrap the Système scope: no home directory resolved \
                 ($HOME / $USERPROFILE is unset or empty)"
            ),
            SystemScopeError::HomeNotRepresentable(lossy) => write!(
                f,
                "cannot bootstrap the Système scope: the home directory is not representable as \
                 UTF-8 ({lossy})"
            ),
            SystemScopeError::HomeNotAbsolute(path) => write!(
                f,
                "cannot bootstrap the Système scope: the home directory is not absolute ({path})"
            ),
            SystemScopeError::ExtraWorkspaces {
                project_id,
                workspace_ids,
            } => write!(
                f,
                "incoherent Système scope: project {project_id} carries {} workspaces ({}) where \
                 exactly one is allowed — refusing to guess which is primary",
                workspace_ids.len(),
                workspace_ids.join(", ")
            ),
            SystemScopeError::ForeignPrimaryWorkspace {
                project_id,
                workspace_id,
            } => write!(
                f,
                "incoherent Système scope: project {project_id} names primary workspace \
                 {workspace_id}, which does not belong to it"
            ),
            SystemScopeError::NotBootstrapped => write!(
                f,
                "the Système scope has not been bootstrapped on this database — there is no home \
                 to anchor on"
            ),
            SystemScopeError::Forbidden {
                entity,
                id,
                capability,
            } => write!(
                f,
                "{SYSTEM_SCOPE_FORBIDDEN}: {capability} is not a capability of the Système \
                 {entity} ({id})"
            ),
            SystemScopeError::Db(e) => write!(f, "Système scope database error: {e}"),
        }
    }
}

impl std::error::Error for SystemScopeError {}

impl From<diesel::result::Error> for SystemScopeError {
    fn from(e: diesel::result::Error) -> Self {
        SystemScopeError::Db(e)
    }
}

// --- The scope --------------------------------------------------------------

/// The bootstrapped Système scope: the singleton project and its ONE primary workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemScope {
    pub project: Project,
    pub workspace: Workspace,
}

impl SystemScope {
    /// The absolute home folder every Système terminal and command runs from.
    ///
    /// `pub` FOR A CROSS-CRATE CONSUMER: `nyx_sidecar::runtime::db_command`'s tests drive the real
    /// bridge with this cwd. `pub(crate)` is therefore impossible, not merely undesired.
    pub fn home(&self) -> &str {
        &self.workspace.root_path
    }
}

/// Bring the Système scope into existence, or confirm it already is — the single bootstrap, run
/// after the migrations in [`crate::db::Db::open`].
///
/// ## The three answers, and why only two of them write
///
/// * **ABSENT** → create the project and its primary workspace at the resolved home, in one
///   transaction.
/// * **INCOMPLETE BUT UNAMBIGUOUS** → repair. Exactly two shapes qualify: a singleton with NO
///   workspace (insert one at the home, point `primary_workspace_id` at it) and a singleton with
///   exactly ONE workspace that the pointer does not name (point it there). In both, there is only
///   one workspace the primary CAN be, so the repair invents nothing.
/// * **INCOHERENT** → refuse, by name. Two or more workspaces, or a pointer naming a workspace of
///   another project: repairing would mean CHOOSING where every Système command runs, and a boot
///   that silently re-homes the user's services is worse than a boot that refuses to start.
///
/// ## Idempotence and concurrency
///
/// Re-running on a healthy scope reads two rows and writes nothing. Two processes bootstrapping at
/// once cannot both create: `idx_projects_single_system` is a UNIQUE PARTIAL index, so the loser's
/// INSERT is a unique violation, its transaction rolls back whole, and it re-reads the winner —
/// which is why the retry below is bounded to a single extra pass rather than being a spin.
///
/// The home is resolved BEFORE the transaction: a machine with no usable home fails without having
/// written anything at all.
pub(crate) fn ensure_system_scope(
    conn: &mut SqliteConnection,
    home_provider: &dyn HomeDirProvider,
) -> Result<SystemScope, SystemScopeError> {
    match ensure_once(conn, home_provider) {
        Err(SystemScopeError::Db(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ))) => {
            // A concurrent bootstrap committed the singleton between our read and our insert. Its
            // row is now visible; the second pass takes the "already present" branch.
            ensure_once(conn, home_provider)
        }
        other => other,
    }
}

/// One bootstrap pass. See [`ensure_system_scope`] for the policy; this is its body, factored out
/// so the concurrent-loser retry is literally the same code path.
fn ensure_once(
    conn: &mut SqliteConnection,
    home_provider: &dyn HomeDirProvider,
) -> Result<SystemScope, SystemScopeError> {
    let Some(project) = db::system_project(conn)? else {
        // ABSENT — resolve the home first (a refusal here writes nothing), then create.
        let home = resolve_home(home_provider)?;
        let (project, workspace) = db::insert_system_scope(conn, &home)?;
        return Ok(SystemScope { project, workspace });
    };

    let mut owned: Vec<Workspace> = workspaces::table
        .filter(workspaces::project_id.eq(&project.id))
        .order((workspaces::created_at.asc(), workspaces::id.asc()))
        .select(Workspace::as_select())
        .load(conn)?;

    if owned.len() > 1 {
        return Err(SystemScopeError::ExtraWorkspaces {
            project_id: project.id.clone(),
            workspace_ids: owned.into_iter().map(|w| w.id).collect(),
        });
    }

    // A pointer naming a workspace this project does not own is incoherent whatever the count —
    // including the "no workspace at all" case, where repairing would leave a dangling pointer
    // behind two primaries.
    if let Some(primary_id) = project.primary_workspace_id.as_deref() {
        if !owned.iter().any(|w| w.id == primary_id) {
            return Err(SystemScopeError::ForeignPrimaryWorkspace {
                project_id: project.id.clone(),
                workspace_id: primary_id.to_string(),
            });
        }
    }

    let Some(workspace) = owned.pop() else {
        // REPAIR — the singleton exists with no workspace: unambiguous, there is exactly one
        // workspace it can have.
        let home = resolve_home(home_provider)?;
        let workspace = db::repair_system_primary_workspace(conn, &project.id, &home)?;
        let project = reload_project(conn, &project.id)?;
        return Ok(SystemScope { project, workspace });
    };

    if project.primary_workspace_id.as_deref() == Some(workspace.id.as_str()) {
        // HEALTHY — nothing written.
        return Ok(SystemScope { project, workspace });
    }

    // REPAIR — the lone workspace is not (yet) named primary. Only one candidate exists.
    db::repair_system_primary_pointer(conn, &project.id, &workspace.id)?;
    let project = reload_project(conn, &project.id)?;
    Ok(SystemScope { project, workspace })
}

/// Re-read a project row after a repair write, so the returned [`SystemScope`] carries the
/// post-write state rather than the pre-write one the caller happened to hold.
fn reload_project(conn: &mut SqliteConnection, id: &str) -> Result<Project, SystemScopeError> {
    Ok(projects::table
        .find(id)
        .select(Project::as_select())
        .first(conn)?)
}

// --- Reads ------------------------------------------------------------------

/// The Système scope as it stands, or `None` when it has not been bootstrapped.
///
/// A READ — it never creates and never repairs. The dedicated door for `system_scope_detail` and
/// for every guard below; `list_projects` / the sidebar array stay user-only.
///
/// A singleton whose workspace row is missing reads as `None` rather than as a half-scope: callers
/// downstream of the bootstrap may assume the workspace exists whenever the scope does.
///
/// `pub` FOR A CROSS-CRATE CONSUMER: `nyx_sidecar::runtime::db_command`'s tests read the scope back
/// through the real bridge. `pub(crate)` is therefore impossible, not merely undesired.
pub fn load(conn: &mut SqliteConnection) -> QueryResult<Option<SystemScope>> {
    let Some(project) = db::system_project(conn)? else {
        return Ok(None);
    };
    let workspace: Option<Workspace> = match project.primary_workspace_id.as_deref() {
        Some(id) => workspaces::table
            .find(id)
            .filter(workspaces::project_id.eq(&project.id))
            .select(Workspace::as_select())
            .first(conn)
            .optional()?,
        None => None,
    };
    Ok(workspace.map(|workspace| SystemScope { project, workspace }))
}

/// The Système project's id, or `None` when the scope is not bootstrapped. The cheapest read the
/// guards need — no workspace join.
fn system_project_id(conn: &mut SqliteConnection) -> QueryResult<Option<String>> {
    projects::table
        .filter(projects::kind.eq(db::PROJECT_KIND_SYSTEM))
        .select(projects::id)
        .first::<String>(conn)
        .optional()
}

/// `true` when `project_id` names the Système singleton. An unknown id is `false` (it names no
/// system project), which keeps "forbidden" and "unknown" separable at the call site.
///
/// `pub` for the READ surfaces that report a command's NATURE (`list_commands`,
/// `list_system_commands`): telling a caller WHICH container a command belongs to is a read, and it
/// is the same one-column answer the guards ask — one source of truth, not a second derivation.
pub fn is_system_project(conn: &mut SqliteConnection, project_id: &str) -> QueryResult<bool> {
    let kind: Option<String> = projects::table
        .find(project_id)
        .select(projects::kind)
        .first::<String>(conn)
        .optional()?;
    Ok(kind.as_deref() == Some(db::PROJECT_KIND_SYSTEM))
}

/// `true` when `workspace_id` belongs to the Système singleton — DERIVED by joining the workspace's
/// project, never read off the workspace itself (there is no `workspaces.kind`, on purpose).
pub fn is_system_workspace(conn: &mut SqliteConnection, workspace_id: &str) -> QueryResult<bool> {
    let kind: Option<String> = workspaces::table
        .inner_join(projects::table.on(projects::id.eq(workspaces::project_id)))
        .filter(workspaces::id.eq(workspace_id))
        .select(projects::kind)
        .first::<String>(conn)
        .optional()?;
    Ok(kind.as_deref() == Some(db::PROJECT_KIND_SYSTEM))
}

/// `true` when `terminal_id` is attached to the Système workspace — DERIVED through its workspace's
/// project, like [`is_system_workspace`].
///
/// THE AGENT-SIDE FILTER OF THE GENERIC MCP TERMINAL TOOLS. [`guard_payload`] cannot cover them:
/// it fires on a payload naming the singleton's PROJECT or WORKSPACE id, and `list_terminals`,
/// `read_terminal`, `send_to_terminal`, `send_keys` and `close_terminal` carry a TERMINAL id, which
/// is neither. So those five read this predicate directly
/// (`mcp_tools_core::refuse_system_terminal`): a Système terminal is absent from the
/// agent's list and refused by name on the four verbs. Its control is the human's, from the UI.
///
/// An UNATTACHED terminal (`workspace_id IS NULL` — the orphan bucket `delete_project` leaves
/// behind) is `false`. That is the disjunction restated as code: an orphan is not a Système
/// terminal, and no read here can turn it into one.
///
/// `pub` FOR A CROSS-CRATE CONSUMER: `nyx_sidecar::runtime::db_command`'s tests assert membership
/// after driving the real bridge. `pub(crate)` is therefore impossible, not merely undesired.
pub fn is_system_terminal(conn: &mut SqliteConnection, terminal_id: &str) -> QueryResult<bool> {
    use crate::schema::terminals;
    let kind: Option<String> = terminals::table
        .inner_join(workspaces::table.on(workspaces::id.nullable().eq(terminals::workspace_id)))
        .inner_join(projects::table.on(projects::id.eq(workspaces::project_id)))
        .filter(terminals::id.eq(terminal_id))
        .select(projects::kind)
        .first::<String>(conn)
        .optional()?;
    Ok(kind.as_deref() == Some(db::PROJECT_KIND_SYSTEM))
}

// --- The Système terminal ---------------------------------------------------

/// The seam a Système terminal's SHELL is opened through, after its record is committed.
///
/// A seam rather than a direct call because the PTY lives in the other crate (the sidecar owns
/// `nyx_sidecar::runtime::pty::PtyManager`) AND because the two branches that matter — the shell
/// opens, the shell refuses to open — must both be reachable from a unit test. Only
/// [`create_system_terminal`] calls it, and only once, after the transaction.
pub trait SystemTerminalShell {
    /// Open a shell for `terminal_id` in `home` (the absolute Système home — never `.`, never
    /// `~`).
    ///
    /// `Err(reason)` is a RECOVERABLE failure, not a rollback: see [`create_system_terminal`].
    fn open_at_home(&self, terminal_id: &str, home: &str) -> Result<(), String>;
}

/// A Système terminal as [`create_system_terminal`] just made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemTerminal {
    /// The COMMITTED row: `cwd = home`, `workspace_id` = the singleton's workspace,
    /// `workspace_binding_mode = manual`, `status = alive`.
    pub terminal: db::Terminal,
    /// The absolute home the record was anchored on and the shell was asked to open in — the same
    /// string [`SystemScope::home`] answers, so a Système terminal and a Système command share one
    /// cwd by construction rather than by two agreeing computations.
    pub home: String,
    /// `Some(reason)` when the shell REFUSED to open. The record still stands and is still a full
    /// member of the scope; this is the caller's chance to say why it is silent.
    pub shell_error: Option<String>,
}

/// USE CASE — create a terminal that belongs to the Système scope: resolve the home, insert the
/// record with its membership in ONE transaction, and only then open its shell.
///
/// ## The order, and why it is the whole point
///
/// 1. **Resolve the home.** From the bootstrapped scope ([`load`]), i.e. the singleton workspace's
///    `root_path` — NOT a fresh `$HOME` read. That is deliberate: every Système COMMAND already
///    runs at `workspace.root_path + subfolder`, so reading the environment again here could open
///    a terminal somewhere the commands beside it do not run. One home, one source. A database
///    with no scope yet is [`SystemScopeError::NotBootstrapped`] — and it happens BEFORE any
///    write, so it leaves no row.
/// 2. **Insert, atomically.** `db::insert_system_terminal` writes the cwd, the `workspace_id`
///    and `workspace_binding_mode = manual` in the SAME statement. There is no instant at which
///    the row exists as an unattached `auto` terminal — which is to say, no instant at which it is
///    in the ORPHAN bucket this scope is disjoint from. A SQL failure rolls the whole thing back:
///    zero rows, like the refusal above.
/// 3. **Open the shell, AFTER the commit.** The connection is back in the pool before
///    [`SystemTerminalShell::open_at_home`] is called (that is why this takes a [`db::Db`] and not a
///    `&mut SqliteConnection`): the shell can no longer influence the row, and the row is already
///    durable when the shell starts writing to it.
///
/// ## The failure policy of the shell, spelled out
///
/// A shell that will not open is REPORTED, never rolled back. The record stands, `alive`, still
/// manually bound to the singleton — i.e. RECOVERABLE: the human's next mount of that terminal
/// spawns its shell through the ordinary path. Rolling back instead would be worse in both
/// directions: it would delete a row the caller may already have been told about, and it would put
/// the delete on the exact path (a failing machine) where it is least likely to succeed.
///
/// What it must NEVER become is an AUTO ORPHAN — a row with no workspace, or bound `auto` — since
/// that is the bucket the scope is disjoint from and the state the next auto-attach pass would
/// happily re-home. Nothing here writes the binding a second time, so the failure branch changes
/// no column at all.
///
/// ## The manual binding is not a detail
///
/// `manual` is what makes the membership hold: [`crate::resolve::decide_attachment`] pins only a
/// manual binding, so a `cd` into a user project leaves this terminal exactly where it is. An
/// `auto` binding onto the singleton would be undone by the next auto-attach pass — which is why
/// the boundary refuses it from a payload too (`allowed_payload`).
pub fn create_system_terminal(
    db: &db::Db,
    label: Option<String>,
    shell: &dyn SystemTerminalShell,
) -> Result<SystemTerminal, SystemScopeError> {
    // STEP 1 + 2 — under ONE checked-out connection, and no further.
    let (terminal, home) = db.with_conn(|conn| -> Result<_, SystemScopeError> {
        let Some(scope) = load(conn)? else {
            return Err(SystemScopeError::NotBootstrapped);
        };
        let home = scope.home().to_string();
        let terminal = db::insert_system_terminal(conn, &scope.workspace.id, &home, label)?;
        Ok((terminal, home))
    })?;
    // STEP 3 — the connection is back in the pool, the row is committed. Only now does a shell
    // exist to fail.
    let shell_error = shell.open_at_home(&terminal.id, &home).err();
    Ok(SystemTerminal {
        terminal,
        home,
        shell_error,
    })
}

// --- Domain boundary --------------------------------------------------------

/// Refuse `capability` when `project_id` is the Système singleton.
///
/// THE IDENTITY HALF OF THE BOUNDARY — and only that half. Its callers are the IDENTITY use cases
/// of `usecase.rs` and nothing else: `archive_project`, `unarchive_project`,
/// `set_primary_workspace`, `register_workspace`, `list_workspaces`, `create_repository` (and
/// [`require_user_workspace`] for the workspace-keyed twins). A `grep` for its name finds no caller
/// outside that file.
///
/// WHAT IT DOES NOT COVER, and what does. The direct-DB project mutations
/// ([`crate::db::update_project`], [`crate::db::delete_project`], [`crate::db::reorder_projects`],
/// [`crate::db::set_project_collapsed`], [`crate::db::set_project_resume_agent_sessions`]) do NOT
/// call this, and neither does any product-aggregate use case (Spec, task, milestone, idea,
/// document, design, directive, workflow…). Those are covered by [`guard_payload`] at the TWO
/// ADAPTERS — the UI/IPC db-command dispatcher and the MCP dispatchers. Calling `db::delete_project`
/// from a third path bypasses the boundary entirely: the core is guarded at its doors, not from
/// within. See the module doc for the full placement.
///
/// The Système project's capabilities are an ALLOW-LIST, and it is short: its bootstrapped primary
/// workspace, terminals, managed commands, their groups and instances, and the Système approvals.
/// Everything else is refused.
///
/// The refusal keys on the container's NATURE, read from the database — never on the id being
/// secret. `list_projects` hides the singleton for ergonomic reasons only; a caller that learns the
/// id (from `system_scope_detail`, from a log, by guessing) gains exactly nothing, which is the
/// property "masquer l'id n'est jamais l'unique défense" names.
///
/// An UNKNOWN id passes through: this guard answers "may you do this HERE", not "does this exist".
/// The caller's own not-found handling stays intact and distinguishable — [`SYSTEM_SCOPE_FORBIDDEN`]
/// is never returned for an id that names nothing.
pub fn require_user_project(
    conn: &mut SqliteConnection,
    project_id: &str,
    capability: &str,
) -> Result<(), SystemScopeError> {
    if is_system_project(conn, project_id)? {
        return Err(SystemScopeError::Forbidden {
            entity: "project",
            id: project_id.to_string(),
            capability: capability.to_string(),
        });
    }
    Ok(())
}

/// Refuse `capability` when `workspace_id` belongs to the Système singleton — the workspace-side
/// twin of [`require_user_project`], for the surfaces that take a workspace id and never see the
/// project (workspace rename/archive/delete, the Git reads, the repository configuration).
///
/// The nature is DERIVED through the project, so there is still exactly one source of truth. An
/// unknown workspace id passes through, same rule as above.
pub fn require_user_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    capability: &str,
) -> Result<(), SystemScopeError> {
    if is_system_workspace(conn, workspace_id)? {
        return Err(SystemScopeError::Forbidden {
            entity: "workspace",
            id: workspace_id.to_string(),
            capability: capability.to_string(),
        });
    }
    Ok(())
}

/// Refuse `capability` when `terminal_id` is currently attached to the Système workspace — the
/// TERMINAL-side twin of [`require_user_project`], for the binding writers that take a terminal id
/// and see neither project nor workspace.
///
/// WHY A THIRD TWIN. `terminals.workspace_binding_mode` has three writers
/// ([`crate::db::attach_terminal`], [`crate::db::detach_terminal`],
/// [`crate::db::unpin_terminal_workspace`]) and the membership rule of this scope — a Système
/// terminal is bound MANUALLY, positively, to the singleton's workspace — is only expressible on
/// the first of them from a payload ([`allowed_payload`]). The other two carry no mode and no
/// workspace in their arguments: what they would break is only visible by READING the terminal's
/// current attachment, which is what this does.
///
/// The two states it forbids are the two ways a member leaves the scope silently:
///
/// * mode `auto` while the `workspace_id` still names the singleton — `decide_attachment` only
///   pins `manual`, so the next [`crate::resolve::auto_attach_terminal`] pass re-binds the terminal
///   to whatever user workspace covers its cwd;
/// * `workspace_id IS NULL` — the orphan bucket, which the scope is DISJOINT from by construction.
///
/// A terminal of a USER project, and an unattached one, pass through untouched: this is the
/// Système invariant, not a general lock on detaching terminals.
pub(crate) fn require_user_terminal(
    conn: &mut SqliteConnection,
    terminal_id: &str,
    capability: &str,
) -> Result<(), SystemScopeError> {
    if is_system_terminal(conn, terminal_id)? {
        return Err(SystemScopeError::Forbidden {
            entity: "terminal",
            id: terminal_id.to_string(),
            capability: capability.to_string(),
        });
    }
    Ok(())
}

// --- The capability allow-list, and the payload guard that applies it --------

/// THE ALLOW-LIST, MCP HALF: the operations the Système scope MAY be the target of, spelled as the
/// AGENT's tool names (the surface `crate::mcp::tool_descriptors` advertises). Its UI/IPC twin is
/// [`SYSTEM_SCOPE_IPC_CAPABILITIES`]; `is_system_capability` reads both, so "what the Système
/// project can do" is still answered in ONE place instead of once per call site.
///
/// ## Why TWO halves and not one flat table
///
/// The names live in two different NAMESPACES that happen to share a Rust type. A flat table hides
/// two silent drifts, and each half is pinned against the LIVE surface it belongs to:
///
/// * a RENAMED db-command arm would leave a stale entry on the IPC HALF matching nothing — the
///   capability keeps working for user projects and is refused for the Système scope alone, with
///   nothing failing. `db_command::every_ipc_capability_of_the_system_scope_is_served_by_the_dispatcher`
///   walks the IPC HALF against the real dispatcher;
/// * an MCP tool ADVERTISED tomorrow under an IPC-only name (`attach_terminal`, `command_list`,
///   `workspace_runtime_snapshot`…) would be allow-listed on the agent surface the day it appears,
///   because `is_system_capability` reads BOTH halves on BOTH surfaces.
///   [`tests::the_two_halves_of_the_allow_list_do_not_share_a_name_with_the_live_mcp_surface`]
///   confronts the IPC HALF's names with the advertised surface so that cannot happen quietly, and
///   also pins that the two halves share no name at all.
///
/// ## Why an ALLOW-list
///
/// A deny-list is wrong by default: every tool added later would silently gain power over the
/// singleton until someone remembered to deny it. Here a new tool is refused until it is
/// deliberately written down, and [`tests::every_advertised_mcp_tool_is_classified`] walks the LIVE
/// advertised surface to prove the two lists have not drifted.
///
/// WHAT IS ON IT — the PRD's own sentence: « Les seuls enfants autorisés sont workspace primaire
/// bootstrapé, terminaux, managed commands, groupes/instances et approbations Système. » So: the
/// managed-command surface end to end (CRUD, import, ordering, groups, lifecycle, output) and the
/// runtime read model that displays it. Everything else — rename/delete/archive/reorder/settings of
/// the project, every workspace lifecycle verb, repositories, Specs, tasks, ideas, documents,
/// design, prototypes, workflows — is absent, hence refused.
///
/// WHAT IS DELIBERATELY NOT ON IT, AND NEVER WILL BE. The Système TERMINAL verbs. A Système
/// terminal is created by its own atomic use case ([`create_system_terminal`], exposed as the
/// UI-only db-command `create_system_terminal` on the IPC half below) and is INVISIBLE to the
/// agent: the generic MCP terminal tools filter or refuse it on [`is_system_terminal`]
/// (`mcp_tools_core::refuse_system_terminal`), which is a terminal-id question this
/// payload-keyed table cannot ask. Their control stays human.
///
/// The allow-list is about the DOMAIN boundary only — "is this a capability of this container".
/// Whether an ALLOWED operation additionally needs a HUMAN APPROVAL is a SECOND, separate gate, and
/// it now exists: [`crate::system_command_guard`] applies the matrix over the command verbs listed
/// below, and [`crate::system_approvals`] holds the one-shot ledger it raises requests in.
///
/// THE TWO QUESTIONS ARE NOT THE SAME, and keeping them apart is why this table stays what it is.
/// This one answers « le scope Système peut-il être la cible de ce verbe » — a fact about the
/// CONTAINER, decided from the payload. The matrix answers « ce geste exige-t-il une main humaine »
/// — a fact about the OPERATION and the live run state, decided per call. `add_command` is on this
/// list AND protected by the matrix; `list_commands` is on this list and free; `create_workspace` is
/// on neither. Folding the two would make "the scope can hold commands" and "an agent may create
/// one" the same sentence, which is exactly the window this phase closed.
pub const SYSTEM_SCOPE_MCP_CAPABILITIES: &[&str] = &[
    // --- the agent's dedicated DISCOVERY of the scope's commands -------------
    //
    // `list_system_commands` is ABOUT the singleton — it is the one advertised tool whose whole
    // subject is this container — so it is the one entry here that could not possibly be anywhere
    // else. Its real payload is empty (`{}`, the scope being a singleton), so the payload guard
    // would never fire on it either way; it is written down because the allow-list is the ANSWER
    // to "what may the Système scope be asked", and its own discovery verb belongs in it.
    "list_system_commands",
    // --- the approval protocol's ONE extra tool ------------------------------
    //
    // `system_approval_status` is the agent's RE-PROBE of a request it raised — the moment that
    // happens outside its own calls, because the blocking wait is capped by the MCP client's
    // timeout and a decision can land after the agent has already been answered. Since PDR-0030 the
    // effect runs AT THE GRANT, so there is no replay and no token: the payload is
    // `{ request_id, wait_ms? }`, neither of which is a scope id, so the payload guard would never
    // fire on it either way. It is written down because the allow-list is the ANSWER to "what may
    // the Système scope be asked", and the PRD names « approbations Système » among the scope's
    // allowed children.
    //
    // GRANT and DENY are NOT here and never will be: they live on the IPC half
    // ([`SYSTEM_SCOPE_IPC_CAPABILITIES`]), because a tool that lets the caller approve its own
    // request is not an approval.
    "system_approval_status",
    // --- managed commands, MCP spelling ------------------------------------
    "list_commands",
    "add_command",
    "update_command",
    "import_commands",
    // `list_importable_scripts` is NOT here, and it is the one READ this table refuses — which is
    // why it is written down as an absence rather than simply left out (review
    // 01M15QP3HVYV9Q729ZYTNJB8T2, finding 01M15RT4QE2JY37QQJAGHFKVX0).
    //
    // Every other read on this list reads THE SCOPE: its commands, their output, its approvals.
    // This one reads THE FILESYSTEM — its `workspace_id` resolves to `root_path`, and the Système
    // workspace's root_path is the user's HOME, absolute. It walks it to a bounded depth and
    // returns, per manifest found, the relative subpath, the script name and its body: the map of
    // every project the user has, including the ones nyx has never heard of.
    //
    // That is the SAME argument [`crate::repository`] already settled for the Git reads of this
    // workspace (`repository.rs`, `SYSTEM_WORKSPACE_HAS_NO_REPOSITORY`): a `git status` over the
    // whole home is the one subprocess this scope promises never to run, and the paths it would
    // surface belong to no project. A directory walk that renders the same paths is the same
    // promise, and gets the same answer — refused at the DOMAIN boundary, with
    // [`SYSTEM_SCOPE_FORBIDDEN`], rather than merely made approvable: "may an agent map your home"
    // is not a question a modal can put usefully.
    //
    // Nothing legitimate is lost. `import_commands` stays on the list and PROTECTED, so its own
    // discovery still runs — after a human claim, on the operation the human approved. And the
    // HUMAN import from the app (`command_import_scripts`, the IPC half) is untouched: a hand on
    // the keyboard is the authority this refusal goes looking for.
    "remove_command",
    "remove_commands",
    "reorder_commands",
    "manage_command_group",
    "clear_command_output",
    "start_command",
    "stop_command",
    "relaunch_command",
    "get_command_output",
    "wait_for_command",
];

/// THE ALLOW-LIST, UI/IPC HALF: the same capabilities spelled as the HUMAN bridge's db-command
/// names — the arms of `nyx_sidecar::runtime::db_command`. See [`SYSTEM_SCOPE_MCP_CAPABILITIES`]
/// for the policy; this half exists separately because the two namespaces are independent and each
/// is pinned against its own live surface.
///
/// NOT AN MCP SURFACE. None of these names is an advertised tool, and none may become one without a
/// deliberate edit: a tool advertised under one of them would be allow-listed for the AGENT the day
/// it appeared, which is not what any entry here grants.
///
/// `pub` FOR A CROSS-CRATE CONSUMER: this half is walked against the LIVE dispatcher by
/// `nyx_sidecar::runtime::db_command`'s
/// `every_ipc_capability_of_the_system_scope_is_served_by_the_dispatcher`, and that dispatcher
/// lives in the other crate. `pub(crate)` is therefore impossible, not merely undesired.
pub const SYSTEM_SCOPE_IPC_CAPABILITIES: &[&str] = &[
    // --- managed commands, UI/IPC spelling ---------------------------------
    "command_list",
    "command_create",
    "command_update",
    "command_delete",
    "command_reorder",
    "command_group_list",
    "command_group_create",
    "command_group_add",
    "command_group_remove",
    "command_group_dissolve",
    "command_group_labels",
    "command_group_layout",
    "command_group_shape",
    "command_group_start",
    "command_group_stop",
    "command_group_relaunch",
    "command_source_refresh",
    "command_resync_source",
    "command_unlink_source",
    "command_import_scripts",
    "command_import_create",
    "command_instance_list",
    "command_output",
    "command_acknowledge",
    // The HUMAN twin of the MCP `clear_command_output` tool. The two namespaces stay disjoint —
    // this one is a db-command, that one an advertised tool — and the human's path is the one that
    // asks no approval: emptying a captured buffer from the app is a hand at the keyboard.
    "command_clear_output",
    // --- the runtime read model the human's Système page renders ------------
    "workspace_runtime_snapshot",
    "workspace_runtime_primary",
    // --- the HUMAN's manual binding of a terminal onto the scope -------------
    //
    // `attach_terminal` is a UI-only db-command (there is no MCP twin), and a MANUAL binding is
    // exactly the POSITIVE membership this scope is built on — the human says "this terminal
    // belongs to Système", which is the opposite of the auto-attach capture the resolver exclusion
    // forbids. The agent-facing terminal verbs stay off this list.
    //
    // CONDITIONAL — it is the only entry whose allowance also reads the PAYLOAD
    // ([`allowed_payload`]): `mode = "manual"` is allowed, anything else is NOT. An `auto` binding
    // onto the singleton would be membership the next `resolve::auto_attach_terminal` pass undoes
    // ([`crate::resolve::decide_attachment`] only pins `manual`), i.e. the scope leaking a member
    // OUT — the exit-side twin of the auto-attach capture forbidden on the way in.
    "attach_terminal",
    // --- the HUMAN's atomic creation of a Système terminal --------------------
    //
    // The db-command over [`create_system_terminal`]: the ONE verb that mints a member of the
    // scope. UI-ONLY and staying that way — there is no MCP twin, and the generic MCP terminal
    // tools cannot see what it produces (`is_system_terminal`).
    //
    // Its payload names no project and no workspace (the scope is a singleton: the use case reads
    // both itself), so today the guard would let it through even unlisted. It is written down
    // anyway, because the allow-list is the ANSWER to "what can the Système scope do" — a
    // capability missing from it is a capability nobody can find — and because the day the
    // renderer sends the workspace id along, the omission would turn into a refusal of the scope's
    // own creation verb.
    "create_system_terminal",
    // --- the HUMAN's half of the approval protocol ----------------------------
    //
    // The three db-commands the decision surface drives: LIST the pending requests, GRANT one,
    // DENY one. IPC-ONLY, and this is the entry where that matters most in the whole table — an
    // MCP twin of `system_approval_grant` would let the agent approve its own request, which is
    // the one thing the protocol exists to prevent. Pinned against the live advertised surface by
    // [`tests::the_two_halves_of_the_allow_list_do_not_share_a_name_with_the_live_mcp_surface`], so
    // none of the three can become a tool by accident.
    //
    // The agent's counterpart is the single READ `system_approval_status` on the MCP half above,
    // which answers only the request its token opens.
    "system_approval_list",
    "system_approval_grant",
    "system_approval_deny",
    // --- the human's dedicated READ of the scope ------------------------------
    //
    // `system_scope_detail`: the one door onto the singleton for the UI, since `list_projects` /
    // `list_workspaces` are user-only by contract. IPC-ONLY by design — pinned against the live
    // advertised surface by
    // [`tests::the_two_halves_of_the_allow_list_do_not_share_a_name_with_the_live_mcp_surface`],
    // so it cannot become an agent tool by accident. An agent discovers the Système COMMANDS
    // through `list_system_commands`, and nothing else of the scope.
    "system_scope_detail",
];

/// `true` when `capability` is NAMED on either half of the allow-list
/// ([`SYSTEM_SCOPE_MCP_CAPABILITIES`] / [`SYSTEM_SCOPE_IPC_CAPABILITIES`]).
///
/// BOTH HALVES, one answer: the guard is called from both adapters with the same signature, and the
/// two namespaces are disjoint by test rather than by type — see
/// [`tests::the_two_halves_of_the_allow_list_do_not_share_a_name_with_the_live_mcp_surface`].
///
/// NAME ONLY. One entry (`attach_terminal`) is additionally conditioned on its payload; the
/// question "is this CALL allowed" is [`is_system_call`], which is what the guard asks.
fn is_system_capability(capability: &str) -> bool {
    SYSTEM_SCOPE_MCP_CAPABILITIES.contains(&capability)
        || SYSTEM_SCOPE_IPC_CAPABILITIES.contains(&capability)
}

/// The payload condition of the allow-list entries that have one — `true` when the entry carries
/// no condition at all.
///
/// TODAY THERE IS EXACTLY ONE. `attach_terminal` is on the list because a MANUAL binding is the
/// positive membership the scope is built on; an `auto` binding is not that. `decide_attachment`
/// renders only a `manual` binding immovable, so a terminal auto-bound to the Système workspace
/// would be re-bound to whatever user workspace covers its cwd at the next auto-attach pass — it
/// would silently LEAVE the scope. Refusing the mode here (rather than normalizing it to `manual`)
/// keeps the UI honest: a caller asking for something the scope cannot hold is told so, with the
/// stable [`SYSTEM_SCOPE_FORBIDDEN`] code, instead of getting a binding it did not ask for.
///
/// A payload with NO `mode` is not a manual binding either, so it is refused too — the conservative
/// end, and the arm requires the field anyway.
fn allowed_payload(capability: &str, payload: &serde_json::Value) -> bool {
    match capability {
        "attach_terminal" => {
            payload.get("mode").and_then(|v| v.as_str()) == Some(db::BINDING_MANUAL)
        }
        _ => true,
    }
}

/// `true` when this CALL — the capability AND the payload it carries — is one the Système scope
/// has. The allow-list membership [`guard_payload`] actually applies.
fn is_system_call(capability: &str, payload: &serde_json::Value) -> bool {
    is_system_capability(capability) && allowed_payload(capability, payload)
}

/// The DB-FREE half of the boundary decision: `false` when this call CANNOT be refused whatever the
/// database holds, so the adapter may skip the guard — and, with it, the pooled-connection checkout
/// the guard would need.
///
/// Two cases answer `false`:
///
/// * the call is allow-listed ([`is_system_call`]) — the guard would short-circuit to `Ok` anyway;
/// * the payload carries NO STRING at all, so it cannot NAME an id: the boundary keys on the
///   singleton's ids appearing as string VALUES, and a payload of numbers and booleans has none.
///
/// WHY IT IS SEPARATE FROM [`guard_payload`]. The guard sits at the top of the sidecar's ~120-arm
/// db-command match, which is the placement that makes it exhaustive — but that also puts it on
/// arms that touch no database at all. Paying a pool checkout there, in competition with the PTY
/// pumps and the command runner, buys nothing.
///
/// THE DISPENSE IS THE PAYLOAD'S, NEVER THE COMMAND'S. Nothing below is keyed on a name: a call is
/// skipped because the JSON it carries holds no string, so no command holds a standing exemption.
/// The 1 Hz poll that motivated the pre-check is `terminal_info` (`{ id: <number> }`, renderer
/// `sidebar/auto-label.ts`); `shell_list` and `integration_list` are invoked with NO argument at
/// all (`sidebar/settings-dialog.tsx`), which reaches the bridge as the empty object. By the very
/// same rule `register_terminal_pty` is NOT dispensed, no-op arm though it is: the renderer sends
/// it `{ recordId: <string>, ptyId: <number> }` (`sidebar/terminal-manager.tsx`), the `recordId` is
/// a string, so the call takes a connection like any other. It fires once per spawn rather than on
/// a timer, so it is not the cost this pre-check exists to remove.
///
/// [`guard_payload`] asks the same question first, so a caller that skips this pre-check is still
/// correct — it just pays for a connection it did not need.
pub fn needs_scope_lookup(capability: &str, payload: &serde_json::Value) -> bool {
    !is_system_call(capability, payload) && contains_string(payload)
}

/// THE BOUNDARY GUARD both adapters call before dispatching: refuse `capability` when its payload
/// NAMES the Système scope anywhere and the capability is not on the allow-list.
///
/// ## Why it reads the whole payload instead of two argument names
///
/// Because the argument NAME is not the invariant. The MCP surface spells ids `project_id` /
/// `workspace_id`, the UI bridge spells them `projectId` / `workspaceId`, several commands carry
/// them as a bare `id`, `projects_reorder` carries a whole ARRAY of them, and a tool added tomorrow
/// will spell them however it likes. Keying on a name list would leave a hole per convention.
/// Walking the payload for the id VALUES has no such holes: whatever an operation calls the field,
/// aiming it at the singleton is what is refused.
///
/// That is also the precise shape of "masquer l'id n'est jamais l'unique défense": the singleton is
/// absent from `list_projects` for ergonomics, but a caller who learns its id — from
/// `system_scope_detail`, from a log, by guessing — gains nothing, because the refusal keys on the
/// id being AIMED, not on it being secret.
///
/// A database with no Système scope yet (pre-bootstrap) matches nothing and refuses nothing.
///
/// The first step is DB-FREE ([`needs_scope_lookup`]): an allow-listed call, or a payload with no
/// string in it at all, is answered without reading anything.
pub fn guard_payload(
    conn: &mut SqliteConnection,
    capability: &str,
    payload: &serde_json::Value,
) -> Result<(), SystemScopeError> {
    if !needs_scope_lookup(capability, payload) {
        return Ok(());
    }
    let (project_id, workspace_ids) = match scope_ids(conn)? {
        Some(ids) => ids,
        None => return Ok(()),
    };
    let mut hit: Option<(&'static str, String)> = None;
    visit_strings(payload, &mut |s| {
        if hit.is_some() {
            return;
        }
        if s == project_id {
            hit = Some(("project", s.to_string()));
        } else if workspace_ids.iter().any(|w| w == s) {
            hit = Some(("workspace", s.to_string()));
        }
    });
    match hit {
        Some((entity, id)) => Err(SystemScopeError::Forbidden {
            entity,
            id,
            capability: capability.to_string(),
        }),
        None => Ok(()),
    }
}

/// The ids that NAME the Système scope: its project, and every workspace under it (normally the one
/// primary — the plural is what keeps the guard honest if a repair is pending). `None` when the
/// scope is not bootstrapped.
fn scope_ids(conn: &mut SqliteConnection) -> QueryResult<Option<(String, Vec<String>)>> {
    let Some(project_id) = system_project_id(conn)? else {
        return Ok(None);
    };
    let workspace_ids: Vec<String> = workspaces::table
        .filter(workspaces::project_id.eq(&project_id))
        .select(workspaces::id)
        .load(conn)?;
    Ok(Some((project_id, workspace_ids)))
}

/// `true` when `value` holds AT LEAST ONE string anywhere — the cheapest necessary condition for
/// the payload to be able to name a scope id. Short-circuits on the first one; allocates nothing.
fn contains_string(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(_) => true,
        serde_json::Value::Array(items) => items.iter().any(contains_string),
        // KEYS COUNT TOO, but only those LONG ENOUGH to be one of the ids this boundary keys on.
        // A map KEYED by an id — `{ "<project-id>": { … } }`, the natural shape of a per-project
        // settings payload — carries it in a key, and ignoring keys would dispense such a call
        // from the guard entirely. Counting EVERY key would instead make the pre-check always
        // true (`{ "id": 12 }` has a string key) and give back the pooled checkout it exists to
        // save on the 1 Hz `terminal_info` poll. The length floor separates the two: a field name
        // is short, a scope id is a UUID.
        serde_json::Value::Object(map) => {
            map.keys().any(|k| k.len() >= MIN_SCOPE_ID_LEN) || map.values().any(contains_string)
        }
        _ => false,
    }
}

/// The shortest string that could be one of the ids [`guard_payload`] compares against. The scope's
/// project and workspace ids are UUIDs (36 characters); the floor is deliberately below that so a
/// future id format has to shrink a long way before it slips past the pre-check, and still far
/// above any field name a payload uses as a key.
const MIN_SCOPE_ID_LEN: usize = 24;

/// Call `f` on every string ANYWHERE in `value` — object values, array items, nested. The payload
/// shapes differ per surface, so the walk is structural rather than a list of known paths.
fn visit_strings(value: &serde_json::Value, f: &mut impl FnMut(&str)) {
    match value {
        serde_json::Value::String(s) => f(s),
        serde_json::Value::Array(items) => {
            for item in items {
                visit_strings(item, f);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                // THE KEY IS A STRING TOO. A payload keyed by an id puts the id where a
                // values-only walk would never look, which would be a hole in the one property
                // this guard claims: that aiming the singleton is refused whatever the operation
                // calls the field — including when it calls it nothing at all.
                f(key);
                visit_strings(item, f);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use uuid::Uuid;

    // Platform-appropriate absolute paths, the same convention `resolve.rs`'s tests use: the
    // absoluteness rule is the PLATFORM's (`Path::is_absolute`), so a POSIX literal is not an
    // absolute path on Windows and vice-versa.
    #[cfg(windows)]
    const HOME: &str = "C:\\Users\\kris";
    #[cfg(windows)]
    const HOME_CANON: &str = "c:\\users\\kris";
    #[cfg(windows)]
    const HOME_CHILD: &str = "C:\\Users\\kris\\Downloads\\x";
    #[cfg(windows)]
    const USER_ROOT: &str = "C:\\Users\\kris\\code\\app";

    #[cfg(not(windows))]
    const HOME: &str = "/home/kris";
    #[cfg(not(windows))]
    const HOME_CANON: &str = "/home/kris";
    #[cfg(not(windows))]
    const HOME_CHILD: &str = "/home/kris/downloads/x";
    #[cfg(not(windows))]
    const USER_ROOT: &str = "/home/kris/code/app";

    fn home_provider() -> FixedHomeDirProvider {
        FixedHomeDirProvider::at(HOME)
    }

    fn bootstrap(conn: &mut SqliteConnection) -> SystemScope {
        ensure_system_scope(conn, &home_provider()).expect("bootstrap the Système scope")
    }

    /// Every column of a table, read from the LIVE schema — so an assertion about a column's
    /// ABSENCE reads the database rather than a list someone maintains by hand.
    fn columns(conn: &mut SqliteConnection, table: &str) -> Vec<String> {
        #[derive(diesel::QueryableByName)]
        struct Col {
            #[diesel(sql_type = diesel::sql_types::Text)]
            name: String,
        }
        diesel::sql_query(format!("PRAGMA table_info({table})"))
            .load::<Col>(conn)
            .expect("read the live column list")
            .into_iter()
            .map(|c| c.name)
            .collect()
    }

    // --- Schema: the nature, its CHECK, and the singleton index ---------------

    /// DONE-CRITERION #1 — `projects.kind` is `user|system`, DEFAULT `user`, CHECKed. The
    /// vocabulary is exercised against the LIVE `CHECK` (each advertised value really stores, a
    /// third really is refused) instead of being read as text — the same discipline
    /// `the_shape_vocabulary_is_the_same_list_on_all_three_layers` applies to `layout_shape`.
    #[test]
    fn project_kind_is_a_closed_vocabulary_defaulting_to_user() {
        assert_eq!(db::PROJECT_KINDS, &["user", "system"]);
        let mut conn = open_in_memory();

        let insert = |conn: &mut SqliteConnection, id: &str, kind: &str| {
            diesel::sql_query(format!(
                "INSERT INTO projects (id, name, collapsed, created_at, updated_at, \
                 resume_agent_sessions, \"order\", key, kind) \
                 VALUES ('{id}', '{id}', 0, 0, 0, 0, 0, '{id}', '{kind}')"
            ))
            .execute(conn)
        };
        for kind in db::PROJECT_KINDS {
            assert!(
                insert(&mut conn, &format!("p-{kind}"), kind).is_ok(),
                "the SQL CHECK must accept the advertised kind '{kind}'"
            );
            // Drop it again so the next turn is not blocked by the SINGLETON index rather than by
            // the CHECK this test is about.
            diesel::sql_query(format!("DELETE FROM projects WHERE id = 'p-{kind}'"))
                .execute(&mut conn)
                .expect("clear");
        }
        assert!(
            insert(&mut conn, "p-nope", "annexe").is_err(),
            "the SQL CHECK must refuse a nature outside the closed vocabulary"
        );

        // The DEFAULT is what kept every pre-existing project exactly as it was.
        diesel::sql_query(
            "INSERT INTO projects (id, name, collapsed, created_at, updated_at, \
             resume_agent_sessions, \"order\", key) \
             VALUES ('p-def', 'p-def', 0, 0, 0, 0, 0, 'PDEF')",
        )
        .execute(&mut conn)
        .expect("a project can be written without naming a kind");
        assert_eq!(
            db::get_project(&mut conn, "p-def")
                .expect("read")
                .expect("the row exists")
                .kind,
            db::PROJECT_KIND_USER
        );
        // …and the ordinary creation path always writes `user` EXPLICITLY.
        let (project, _) = db::create_project(&mut conn, "app", USER_ROOT, None).expect("project");
        assert_eq!(project.kind, db::PROJECT_KIND_USER);
    }

    /// DONE-CRITERION #1 — the partial unique index admits AT MOST ONE system project and leaves
    /// user projects entirely alone. This is the singleton, enforced by SQLite: it is what makes a
    /// concurrent bootstrap safe rather than merely unlikely.
    #[test]
    fn at_most_one_system_project_can_exist_and_user_projects_are_unconstrained() {
        let mut conn = open_in_memory();
        bootstrap(&mut conn);

        let second = diesel::sql_query(
            "INSERT INTO projects (id, name, collapsed, created_at, updated_at, \
             resume_agent_sessions, \"order\", key, kind) \
             VALUES ('sys2', 'sys2', 0, 0, 0, 0, 9, 'SYS9', 'system')",
        )
        .execute(&mut conn);
        assert!(
            matches!(
                second,
                Err(diesel::result::Error::DatabaseError(
                    diesel::result::DatabaseErrorKind::UniqueViolation,
                    _
                ))
            ),
            "a second system project must be refused by idx_projects_single_system, got {second:?}"
        );

        // The index is PARTIAL: `kind = 'user'` is not in it, so any number of user projects fit.
        for n in 0..3 {
            db::create_project(
                &mut conn,
                &format!("app{n}"),
                &format!("{USER_ROOT}{n}"),
                None,
            )
            .expect("a user project is never blocked by the singleton index");
        }
        assert_eq!(db::list_projects(&mut conn).unwrap().len(), 3);
    }

    /// DONE-CRITERION #2 — NO `workspaces.kind`. The nature of a workspace is DERIVED through its
    /// project; a second column would be a second truth to resynchronise, and would make a
    /// `system` workspace under a `user` project representable.
    #[test]
    fn the_nature_lives_only_on_projects_and_is_derived_for_a_workspace() {
        let mut conn = open_in_memory();
        assert!(
            columns(&mut conn, "projects").contains(&"kind".to_string()),
            "projects carries the nature"
        );
        assert!(
            !columns(&mut conn, "workspaces").contains(&"kind".to_string()),
            "workspaces must NOT carry a kind column — the nature is derived through the project"
        );

        let scope = bootstrap(&mut conn);
        let (_user, user_root) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();
        assert!(is_system_workspace(&mut conn, &scope.workspace.id).unwrap());
        assert!(!is_system_workspace(&mut conn, &user_root.id).unwrap());
        assert!(
            !is_system_workspace(&mut conn, "no-such-workspace").unwrap(),
            "an unknown id is not a system workspace — 'forbidden' and 'unknown' stay separable"
        );
    }

    // --- Bootstrap ------------------------------------------------------------

    /// DONE-CRITERION #3 — a blank database gains the singleton: a UUIDv7 project of nature
    /// `system` with a unique key, and ONE root/live primary workspace at the absolute home.
    #[test]
    fn bootstrap_creates_the_singleton_and_its_primary_workspace_at_the_home() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);

        assert_eq!(scope.project.kind, db::PROJECT_KIND_SYSTEM);
        assert_eq!(scope.project.name, db::SYSTEM_PROJECT_NAME);
        assert_eq!(
            Uuid::parse_str(&scope.project.id).map(|u| u.get_version_num()),
            Ok(7),
            "ids are UUIDv7 (time-ordered), like every other identity in nyx"
        );
        assert!(!scope.project.key.is_empty(), "projects.key is NOT NULL");
        assert!(
            Uuid::parse_str(&scope.workspace.id).is_ok(),
            "the workspace id is a UUID too"
        );

        // The primary role is ANCHORED, and it is the workspace just created.
        assert_eq!(
            scope.project.primary_workspace_id.as_deref(),
            Some(scope.workspace.id.as_str())
        );
        assert_eq!(scope.workspace.project_id, scope.project.id);
        assert!(scope.workspace.is_root, "root");
        assert_eq!(
            scope.workspace.archived_at, None,
            "and live — never archived at birth"
        );
        assert_eq!(scope.workspace.folder_removed_at, None);

        // The home is stored in the canonical comparison form, and it is what `home()` answers.
        assert_eq!(scope.workspace.root_path, HOME_CANON);
        assert_eq!(scope.home(), HOME_CANON);
        // NO GIT WAS READ: the home is not a work tree, and the scope promises no Git subprocess.
        assert_eq!(scope.workspace.branch, None);
    }

    /// DONE-CRITERION #4 — re-booting is IDEMPOTENT: same rows, same ids, nothing written.
    #[test]
    fn a_repeated_boot_changes_nothing() {
        let mut conn = open_in_memory();
        let first = bootstrap(&mut conn);
        let second = bootstrap(&mut conn);
        let third = bootstrap(&mut conn);
        assert_eq!(first, second);
        assert_eq!(second, third);

        let projects: i64 = projects::table
            .filter(projects::kind.eq(db::PROJECT_KIND_SYSTEM))
            .count()
            .get_result(&mut conn)
            .unwrap();
        let workspaces: i64 = workspaces::table
            .filter(workspaces::project_id.eq(&first.project.id))
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!((projects, workspaces), (1, 1));
        assert_eq!(
            first.project.updated_at, third.project.updated_at,
            "a healthy scope is READ, not rewritten"
        );
    }

    /// DONE-CRITERION #4 — an INCOMPLETE but UNAMBIGUOUS state is repaired: a singleton whose
    /// workspace row is gone gains one at the home, with the primary pointer re-anchored.
    #[test]
    fn a_singleton_without_a_workspace_is_repaired() {
        let mut conn = open_in_memory();
        let first = bootstrap(&mut conn);
        diesel::update(projects::table.find(&first.project.id))
            .set(projects::primary_workspace_id.eq::<Option<String>>(None))
            .execute(&mut conn)
            .expect("drop the pointer first (the FK is ON DELETE SET NULL anyway)");
        diesel::delete(workspaces::table.find(&first.workspace.id))
            .execute(&mut conn)
            .expect("simulate the lost workspace row");

        let repaired = bootstrap(&mut conn);
        assert_eq!(
            repaired.project.id, first.project.id,
            "the project is the SAME singleton — a repair never mints a second one"
        );
        assert_ne!(repaired.workspace.id, first.workspace.id);
        assert_eq!(repaired.workspace.root_path, HOME_CANON);
        assert_eq!(
            repaired.project.primary_workspace_id.as_deref(),
            Some(repaired.workspace.id.as_str()),
            "the pointer follows the repair"
        );
    }

    /// DONE-CRITERION #4 — the other unambiguous shape: the lone workspace exists but the pointer
    /// does not name it. Only one candidate exists, so the repair invents nothing.
    #[test]
    fn a_singleton_whose_primary_pointer_is_null_is_repaired() {
        let mut conn = open_in_memory();
        let first = bootstrap(&mut conn);
        diesel::update(projects::table.find(&first.project.id))
            .set(projects::primary_workspace_id.eq::<Option<String>>(None))
            .execute(&mut conn)
            .expect("simulate the lost pointer");

        let repaired = bootstrap(&mut conn);
        assert_eq!(repaired.project.id, first.project.id);
        assert_eq!(repaired.workspace.id, first.workspace.id);
        assert_eq!(
            repaired.project.primary_workspace_id.as_deref(),
            Some(first.workspace.id.as_str())
        );
    }

    /// DONE-CRITERION #4 — an AMBIGUOUS state is REFUSED, by name. Choosing between two workspaces
    /// would silently decide where every Système command runs; a boot that refuses beats a boot
    /// that re-homes the user's services behind their back.
    #[test]
    fn an_extra_workspace_under_the_singleton_is_refused_not_guessed() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let intruder = db::create_workspace(&mut conn, &scope.project.id, "extra", USER_ROOT)
            .expect("simulate a second workspace under the singleton");

        let err = ensure_system_scope(&mut conn, &home_provider())
            .expect_err("two workspaces is not a repairable state");
        match &err {
            SystemScopeError::ExtraWorkspaces {
                project_id,
                workspace_ids,
            } => {
                assert_eq!(project_id, &scope.project.id);
                assert_eq!(workspace_ids.len(), 2);
                assert!(workspace_ids.contains(&intruder.id));
            }
            other => panic!("expected ExtraWorkspaces, got {other:?}"),
        }
        assert_eq!(err.code(), "system_scope_extra_workspaces");
        assert!(!err.is_forbidden());
    }

    /// DONE-CRITERION #4 — a primary pointer naming a workspace of ANOTHER project is incoherent,
    /// not repairable: the singleton would run its commands inside a user project's folder.
    #[test]
    fn a_foreign_primary_workspace_is_refused() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let (_user, user_root) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();
        diesel::update(projects::table.find(&scope.project.id))
            .set(projects::primary_workspace_id.eq(&user_root.id))
            .execute(&mut conn)
            .expect("simulate a cross-project pointer");

        let err = ensure_system_scope(&mut conn, &home_provider()).expect_err("refused");
        match &err {
            SystemScopeError::ForeignPrimaryWorkspace {
                project_id,
                workspace_id,
            } => {
                assert_eq!(project_id, &scope.project.id);
                assert_eq!(workspace_id, &user_root.id);
            }
            other => panic!("expected ForeignPrimaryWorkspace, got {other:?}"),
        }
        assert_eq!(err.code(), "system_scope_foreign_primary");
    }

    // --- Home resolution ------------------------------------------------------

    /// DONE-CRITERION #6 — an ABSENT home fails by name and writes NOTHING. No `.`, no `~`, no
    /// cwd: a scope anchored on a guessed folder would run the user's services somewhere
    /// arbitrary, silently.
    #[test]
    fn a_missing_home_refuses_the_bootstrap_and_leaves_no_partial_state() {
        let mut conn = open_in_memory();
        let err = ensure_system_scope(&mut conn, &FixedHomeDirProvider::missing())
            .expect_err("no home ⇒ no scope");
        assert!(matches!(err, SystemScopeError::HomeMissing));
        assert_eq!(err.code(), "system_home_missing");

        let projects: i64 = projects::table.count().get_result(&mut conn).unwrap();
        let workspaces: i64 = workspaces::table.count().get_result(&mut conn).unwrap();
        assert_eq!(
            (projects, workspaces),
            (0, 0),
            "the refusal happens BEFORE any write — no orphan project, no orphan workspace"
        );
        assert!(load(&mut conn).unwrap().is_none());
    }

    /// DONE-CRITERION #6 — an EMPTY home reads as absent (an env var set to "" is not a home), and
    /// a RELATIVE home is refused rather than resolved against the process cwd.
    #[test]
    fn an_empty_or_relative_home_is_refused() {
        assert!(matches!(
            resolve_home(&FixedHomeDirProvider::at("   ")),
            Err(SystemScopeError::HomeMissing)
        ));
        let err = resolve_home(&FixedHomeDirProvider::at("relative/home"))
            .expect_err("a relative home is not a home");
        assert!(matches!(err, SystemScopeError::HomeNotAbsolute(_)));
        assert_eq!(err.code(), "system_home_not_absolute");
    }

    /// DONE-CRITERION #6 — a home that is NOT REPRESENTABLE as UTF-8 is refused by name rather
    /// than lossily transcoded. A `TEXT` column cannot hold it, and a lossy `root_path` would name
    /// a DIFFERENT folder than the one the user actually has — every Système command would then run
    /// somewhere nobody chose.
    #[test]
    fn a_home_that_is_not_utf8_is_refused_rather_than_transcoded() {
        use std::ffi::OsString;
        #[cfg(windows)]
        // An UNPAIRED high surrogate: a legal Windows path component, not valid Unicode.
        let bad = {
            use std::os::windows::ffi::OsStringExt;
            let mut wide: Vec<u16> = "C:\\".encode_utf16().collect();
            wide.push(0xD800);
            OsString::from_wide(&wide)
        };
        #[cfg(unix)]
        let bad = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![b'/', b'h', 0xFF, 0xFE])
        };

        let err = resolve_home(&FixedHomeDirProvider::at(PathBuf::from(bad)))
            .expect_err("a non-UTF-8 home is not storable");
        assert!(matches!(err, SystemScopeError::HomeNotRepresentable(_)));
        assert_eq!(err.code(), "system_home_not_representable");
    }

    /// DONE-CRITERION #6 — the injected home is NORMALIZED to the canonical comparison/storage
    /// form, so the stored `root_path` is the same string a live `cd` into the home collapses to.
    #[test]
    fn the_home_is_stored_in_the_canonical_form() {
        // A spelling with a redundant separator and a `.` component still lands on HOME_CANON.
        let messy = format!("{HOME}{}{}", std::path::MAIN_SEPARATOR, ".");
        assert_eq!(
            resolve_home(&FixedHomeDirProvider::at(&messy)).unwrap(),
            HOME_CANON
        );
        assert_eq!(
            resolve_home(&FixedHomeDirProvider::at(HOME)).unwrap(),
            crate::pathnorm::normalize(HOME)
        );
    }

    /// DONE-CRITERION #6 — the provider is a SEAM, and the production one reads the same
    /// `$HOME`/`$USERPROFILE` rule the rest of the crate shares (one home resolver, no second copy
    /// to drift).
    #[test]
    fn the_production_provider_reads_the_shared_environment_rule() {
        assert_eq!(
            EnvHomeDirProvider.home_dir(),
            crate::onboarding::home_dir(),
            "the production provider must not be a second home resolver"
        );
    }

    // --- Reads and the user/system separation ---------------------------------

    /// DONE-CRITERION #1/#3 — the singleton is reachable through its OWN door and is absent from
    /// the sidebar/MCP project list. Hiding it is ergonomics; the authorization is the guard.
    #[test]
    fn the_singleton_has_a_dedicated_read_and_is_not_in_list_projects() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let (user, _root) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();

        let listed: Vec<String> = db::list_projects(&mut conn)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(listed, vec![user.id.clone()]);

        assert_eq!(
            db::system_project(&mut conn).unwrap().map(|p| p.id),
            Some(scope.project.id.clone())
        );
        assert_eq!(
            system_project_id(&mut conn).unwrap(),
            Some(scope.project.id.clone())
        );
        assert_eq!(load(&mut conn).unwrap(), Some(scope.clone()));
        assert!(is_system_project(&mut conn, &scope.project.id).unwrap());
        assert!(!is_system_project(&mut conn, &user.id).unwrap());
        assert!(
            !is_system_project(&mut conn, "unknown").unwrap(),
            "an unknown id names no system project"
        );
    }

    /// DONE-CRITERION #4 — a CONCURRENT boot converges: four processes bootstrapping the same
    /// file-backed database at once all return the SAME singleton, and the database holds exactly
    /// one system project and one workspace.
    ///
    /// This is the property the UNIQUE PARTIAL index buys, and the reason the singleton is enforced
    /// in SQL rather than by an application-level "check then create": the losers' INSERT is a
    /// unique violation, their transaction rolls back whole, and they re-read the winner. A
    /// file-backed DB is required — `:memory:` is per-connection, so it cannot race with itself.
    #[test]
    fn concurrent_bootstraps_converge_on_one_singleton() {
        use diesel::connection::SimpleConnection;

        let dir = std::env::temp_dir().join(format!("nyx-system-scope-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("create temp db dir");
        let url = dir
            .join("nyx.db")
            .to_str()
            .expect("a UTF-8 temp path")
            .to_string();

        // One connection per "process", with the same pragmas `Db::open`'s pool customizer sets —
        // WAL + a busy_timeout, so concurrent writers WAIT for the single writer instead of
        // failing on SQLITE_BUSY (which would test the timeout, not the index).
        let connect = |url: &str| {
            let mut conn = SqliteConnection::establish(url).expect("open the file-backed db");
            // busy_timeout FIRST: `journal_mode = WAL` itself takes an exclusive lock, so four
            // threads issuing it at once would race on the pragma rather than on the bootstrap.
            conn.batch_execute(
                "PRAGMA busy_timeout = 15000; PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;",
            )
            .expect("set the pragmas");
            conn
        };

        // Migrate ONCE, without bootstrapping, so every thread starts from the same blank scope.
        crate::db::run_migrations(&mut connect(&url)).expect("migrate the file-backed db");

        let ids: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let url = url.clone();
                    s.spawn(move || {
                        let mut conn = connect(&url);
                        ensure_system_scope(&mut conn, &FixedHomeDirProvider::at(HOME))
                            .expect("every concurrent boot succeeds")
                            .project
                            .id
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("no thread panicked"))
                .collect()
        });

        assert!(
            ids.windows(2).all(|w| w[0] == w[1]),
            "every concurrent boot must answer the SAME singleton, got {ids:?}"
        );
        let mut conn = connect(&url);
        let projects: i64 = projects::table
            .filter(projects::kind.eq(db::PROJECT_KIND_SYSTEM))
            .count()
            .get_result(&mut conn)
            .unwrap();
        let workspaces: i64 = workspaces::table
            .filter(workspaces::project_id.eq(&ids[0]))
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(
            (projects, workspaces),
            (1, 1),
            "one project, one workspace — the losers wrote nothing that survived"
        );

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- The cwd resolver ------------------------------------------------------

    /// DONE-CRITERION (task « Workspace Système hors du résolveur cwd ») — the home, its
    /// descendants and the exact `root_path` NEVER resolve the Système workspace, because it is not
    /// a candidate at all. A USER workspace under the home still wins its own cwd, so the exclusion
    /// costs the ordinary rules nothing.
    #[test]
    fn the_system_workspace_is_not_a_candidate_of_the_cwd_resolver() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let (_user, user_root) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();

        let ids: Vec<String> = db::resolvable_workspaces(&mut conn)
            .unwrap()
            .into_iter()
            .map(|w| w.id)
            .collect();
        assert_eq!(
            ids,
            vec![user_root.id.clone()],
            "resolvable_workspaces excludes the Système workspace BY JOIN on projects.kind"
        );
        // …while the ROW is still there (the scope exists, it just does not compete). Read
        // straight off `workspaces`: there is no unfiltered `all_workspaces` reader to lean on,
        // deliberately — see `db::resolvable_workspaces`.
        let rows: i64 = workspaces::table
            .filter(workspaces::id.eq(&scope.workspace.id))
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(
            rows, 1,
            "the Système workspace exists — it is merely not a resolution candidate"
        );

        let candidates: Vec<crate::resolve::WorkspaceMatch> = db::resolvable_workspaces(&mut conn)
            .unwrap()
            .into_iter()
            .map(|w| crate::resolve::WorkspaceMatch {
                id: w.id,
                path: w.root_path,
            })
            .collect();
        for cwd in [HOME, HOME_CHILD] {
            assert_eq!(
                crate::resolve::resolve_workspace(&crate::pathnorm::normalize(cwd), &candidates),
                None,
                "{cwd} must not resolve to the Système workspace"
            );
        }
        assert_eq!(
            crate::resolve::resolve_workspace(&crate::pathnorm::normalize(USER_ROOT), &candidates)
                .as_deref(),
            Some(user_root.id.as_str()),
            "a user workspace UNDER the home keeps winning its own cwd — the exclusion removes a \
             candidate that could only ever have been the shallowest ancestor, so no user-vs-user \
             ranking changes"
        );
    }

    /// NON-REGRESSION on the two ranking rules, with the Système scope PRESENT: the LONGEST
    /// canonical ancestor still wins, and two workspaces sharing a `root_path` are still tied by
    /// LOWEST ID (the incumbent, never newest-wins). Removing the home from the candidate set must
    /// not have changed how the remaining candidates are ranked — only which ones there are.
    #[test]
    fn removing_the_home_from_the_candidate_set_changes_no_user_ranking() {
        let mut conn = open_in_memory();
        bootstrap(&mut conn);

        // Nested user workspaces, both UNDER the Système home.
        let (_p, outer) = db::create_project(&mut conn, "outer", USER_ROOT, None).unwrap();
        let nested_path = format!("{USER_ROOT}{}inner", std::path::MAIN_SEPARATOR);
        let inner = db::create_workspace(&mut conn, &_p.id, "inner", &nested_path).unwrap();
        // A SECOND project registered on the very same folder as `outer` — the tie case.
        let (_p2, twin) = db::create_project(&mut conn, "twin", USER_ROOT, None).unwrap();

        let candidates: Vec<crate::resolve::WorkspaceMatch> = db::resolvable_workspaces(&mut conn)
            .unwrap()
            .into_iter()
            .map(|w| crate::resolve::WorkspaceMatch {
                id: w.id,
                path: w.root_path,
            })
            .collect();
        let resolve = |cwd: &str| {
            crate::resolve::resolve_workspace(&crate::pathnorm::normalize(cwd), &candidates)
        };

        assert_eq!(
            resolve(&format!("{nested_path}{}src", std::path::MAIN_SEPARATOR)).as_deref(),
            Some(inner.id.as_str()),
            "the deepest matching root_path still wins"
        );
        let incumbent = std::cmp::min(outer.id.clone(), twin.id.clone());
        assert_eq!(
            resolve(USER_ROOT).as_deref(),
            Some(incumbent.as_str()),
            "a shared root_path is still broken by LOWEST id — the incumbent, not the newest"
        );
    }

    /// DONE-CRITERION — no AUTO-ATTACH into the Système scope, ever. A loose terminal sitting in
    /// the home (or anywhere under it that no user workspace claims) stays LOOSE: membership in the
    /// Système scope is a positive, manual attachment, never something a `cd` can produce.
    ///
    /// This is also half of the "Système ⟂ orphelin" invariant: the orphan bucket
    /// (`workspace_id IS NULL`) is exactly what a loose terminal is, and nothing here adopts it.
    #[test]
    fn a_loose_terminal_in_the_home_is_never_auto_attached_to_the_system_scope() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let (_user, user_root) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();

        let term = db::create_terminal(&mut conn, HOME, None).unwrap();
        let attached = crate::resolve::auto_attach_terminal(&mut conn, &term.id, Some(HOME))
            .expect("auto-attach runs");
        assert_eq!(
            attached.workspace_id, None,
            "a terminal whose cwd IS the Système home stays loose"
        );
        assert!(!attached.changed);

        let deep = crate::resolve::auto_attach_terminal(&mut conn, &term.id, Some(HOME_CHILD))
            .expect("auto-attach runs");
        assert_eq!(deep.workspace_id, None, "…and so does one under it");

        // The terminal is STILL an orphan in the DB, not a member of the singleton.
        let row = db::get_terminal(&mut conn, &term.id).unwrap().unwrap();
        assert_eq!(row.workspace_id, None);
        assert!(
            !is_system_terminal(&mut conn, &term.id).unwrap(),
            "an orphan is not a Système terminal — the two sets stay disjoint"
        );

        // …while the SAME resolver still auto-attaches into a user workspace normally.
        let user_term = db::create_terminal(&mut conn, USER_ROOT, None).unwrap();
        let user_attached =
            crate::resolve::auto_attach_terminal(&mut conn, &user_term.id, Some(USER_ROOT))
                .expect("auto-attach runs");
        assert_eq!(user_attached.workspace_id.as_deref(), Some(user_root.id.as_str()));
        assert!(user_attached.changed);
        assert!(!is_system_terminal(&mut conn, &user_term.id).unwrap());
        // And a terminal really attached to the Système workspace READS as one — the positive
        // side of the same predicate, so the assertions above are not vacuously true.
        db::attach_terminal(&mut conn, &term.id, &scope.workspace.id, db::BINDING_MANUAL).unwrap();
        assert!(is_system_terminal(&mut conn, &term.id).unwrap());
    }

    // --- The Système terminal --------------------------------------------------

    /// A [`SystemTerminalShell`] double that RECORDS what it was asked to open — and, crucially,
    /// READS THE DATABASE while it is being asked.
    ///
    /// That read is the point. `Db::in_memory` is a ONE-connection pool, so a shell called while
    /// the use case still held its connection could not check one out at all, and a shell called
    /// inside the transaction would see no row. Both branches of "the PTY opens only after commit"
    /// are therefore observable here rather than merely asserted in prose.
    struct RecordingShell {
        db: db::Db,
        /// `(terminal_id, cwd)` per call, in order.
        calls: std::sync::Mutex<Vec<(String, String)>>,
        /// What the row looked like AT THE MOMENT the shell was called (`None` = not committed).
        seen_row: std::sync::Mutex<Option<db::Terminal>>,
        /// When `Some`, the shell REFUSES to open — the recoverable failure branch.
        refuse: Option<String>,
    }

    impl RecordingShell {
        fn new(db: &db::Db) -> Self {
            Self {
                db: db.clone(),
                calls: std::sync::Mutex::new(Vec::new()),
                seen_row: std::sync::Mutex::new(None),
                refuse: None,
            }
        }

        fn refusing(db: &db::Db, why: &str) -> Self {
            Self {
                refuse: Some(why.to_string()),
                ..Self::new(db)
            }
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl SystemTerminalShell for RecordingShell {
        fn open_at_home(&self, terminal_id: &str, home: &str) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push((terminal_id.to_string(), home.to_string()));
            *self.seen_row.lock().unwrap() = self
                .db
                .with_conn(|c| db::get_terminal(c, terminal_id))
                .expect("read the terminal back from inside the shell opening");
            match &self.refuse {
                Some(why) => Err(why.clone()),
                None => Ok(()),
            }
        }
    }

    /// Bootstrap the scope on a `Db` (which migrates but does not bootstrap — see `Db::in_memory`).
    fn db_with_scope() -> (db::Db, SystemScope) {
        let db = db::Db::in_memory();
        let scope = db
            .with_conn(|c| ensure_system_scope(c, &home_provider()))
            .expect("bootstrap the Système scope");
        (db, scope)
    }

    /// DONE-CRITERIA #1 + #2 — the atomic creation. ONE transaction writes the row WITH its
    /// membership (cwd = the absolute home, the singleton's `workspace_id`, binding `manual`), and
    /// the shell is opened only AFTER that commit, at that same absolute home — never `.`, never
    /// `~`, never a second path of its own.
    ///
    /// WINDOWS AND POSIX, per the module's own convention: the home constants are
    /// platform-appropriate (`C:\Users\kris` / `/home/kris`) because ABSOLUTENESS is judged by the
    /// platform (`Path::is_absolute`) — a POSIX literal is not an absolute path on Windows, so the
    /// two shapes cannot both be bootstrapped in one run. The cross-OS gate replays this very test
    /// on each platform, which is what makes `HOME_CANON` (the canonical form, drive letter
    /// lower-cased on Windows) an assertion about each platform's real spelling.
    #[test]
    fn a_system_terminal_is_born_at_the_home_manually_bound_and_gets_its_shell_after_the_commit() {
        let (db, scope) = db_with_scope();
        let shell = RecordingShell::new(&db);

        let created = create_system_terminal(&db, Some("services".to_string()), &shell)
            .expect("the scope is bootstrapped, so a Système terminal can be created");

        // THE MEMBERSHIP IS ON THE ROW, not posed afterwards.
        assert_eq!(
            created.terminal.workspace_id.as_deref(),
            Some(scope.workspace.id.as_str()),
            "the record names the singleton's workspace from its first byte"
        );
        assert_eq!(
            created.terminal.workspace_binding_mode,
            db::BINDING_MANUAL,
            "manual: a `cd` must not re-home it, and auto would be undone by the next pass"
        );
        assert_eq!(created.terminal.status, db::STATUS_ALIVE);
        assert_eq!(created.terminal.label.as_deref(), Some("services"));
        assert!(
            db.with_conn(|c| is_system_terminal(c, &created.terminal.id))
                .unwrap(),
            "the membership reads back through the derived predicate, not just off the row"
        );

        // THE CWD IS THE HOME, absolutely and literally.
        assert_eq!(created.home, scope.home());
        assert_eq!(created.terminal.cwd, HOME_CANON);
        assert_ne!(
            created.terminal.cwd, ".",
            "`.` is `create_terminal`'s 'no cwd asked for' marker — a Système terminal always has one"
        );
        assert!(!created.terminal.cwd.contains('~'));
        assert!(std::path::Path::new(&created.terminal.cwd).is_absolute());

        // THE SHELL SAW A COMMITTED ROW — called exactly once, at that home, with the record
        // already readable through a SECOND checkout of the one-connection pool.
        assert_eq!(
            shell.calls(),
            vec![(created.terminal.id.clone(), scope.home().to_string())]
        );
        let seen = shell
            .seen_row
            .lock()
            .unwrap()
            .clone()
            .expect("the row was already committed when the shell opened");
        assert_eq!(seen, created.terminal, "…and committed in its FINAL shape");
        assert_eq!(created.shell_error, None);
    }

    /// DONE-CRITERION #3, first half — a home/DB failure creates NO ROW. Here the reachable shape
    /// of "no home" is a database whose scope was never bootstrapped (the bootstrap itself refuses
    /// an unresolvable home before writing, which
    /// `a_missing_home_refuses_the_bootstrap_and_leaves_no_partial_state` pins): there is no home
    /// to anchor on, so the use case refuses BEFORE its transaction and the shell is never called.
    #[test]
    fn without_a_bootstrapped_scope_no_terminal_row_is_created_and_no_shell_is_opened() {
        let db = db::Db::in_memory();
        let shell = RecordingShell::new(&db);

        let err = create_system_terminal(&db, None, &shell)
            .expect_err("there is no Système home on this database");
        assert!(matches!(err, SystemScopeError::NotBootstrapped), "{err:?}");
        assert_eq!(err.code(), "system_scope_not_bootstrapped");
        assert!(
            !err.is_forbidden(),
            "an absent scope is not a boundary refusal — a caller must not read it as one"
        );

        assert!(
            db.with_conn(db::list_terminals).unwrap().is_empty(),
            "a refusal before the transaction leaves ZERO rows"
        );
        assert!(shell.calls().is_empty(), "and opens no shell");
    }

    /// DONE-CRITERION #3, second half — a shell that will NOT open leaves a RECOVERABLE terminal
    /// and never an auto orphan.
    ///
    /// The failure is reported (`shell_error`), the row stands exactly as committed — alive, at
    /// the home, manually bound to the singleton — and the two states that would take it out of
    /// the scope are both absent: `workspace_id IS NULL` (the orphan bucket) and mode `auto` (what
    /// the next auto-attach pass would re-home). Nothing on the failure path writes a column, so
    /// there is nothing to undo.
    ///
    /// It is also the ORDER, proven from the other side: a row that survives the shell's `Err` can
    /// only have been committed before the shell was called.
    #[test]
    fn a_shell_that_will_not_open_leaves_a_recoverable_system_terminal_never_an_auto_orphan() {
        let (db, scope) = db_with_scope();
        let shell = RecordingShell::refusing(&db, "no pty available");

        let created = create_system_terminal(&db, None, &shell)
            .expect("a shell failure is REPORTED, not propagated as a failed creation");
        assert_eq!(created.shell_error.as_deref(), Some("no pty available"));

        let row = db
            .with_conn(|c| db::get_terminal(c, &created.terminal.id))
            .unwrap()
            .expect("the record STANDS — a failing machine is no reason to delete it");
        assert_eq!(row, created.terminal, "not one column was rewritten");
        assert_eq!(row.status, db::STATUS_ALIVE, "recoverable: a later mount opens its shell");
        assert_eq!(row.cwd, scope.home());
        // The two exits from the scope, both closed.
        assert_eq!(
            row.workspace_id.as_deref(),
            Some(scope.workspace.id.as_str()),
            "never dropped into the orphan bucket"
        );
        assert_eq!(
            row.workspace_binding_mode,
            db::BINDING_MANUAL,
            "never left bound `auto`, which the next auto-attach pass would re-home"
        );
        assert!(db.with_conn(|c| is_system_terminal(c, &created.terminal.id)).unwrap());
    }

    /// DONE-CRITERION #4 — a `cd` into a project does not move a Système terminal.
    ///
    /// Two passes, because they are two different mechanisms: the terminal's OWN cwd changing to a
    /// user workspace's root (`auto_attach_terminal`, the live-cwd poll's re-resolution), and the
    /// scope's home being an ancestor of everything (which is why the Système workspace is not a
    /// resolver candidate at all).
    #[test]
    fn a_cd_into_a_project_does_not_move_a_system_terminal() {
        let (db, scope) = db_with_scope();
        let shell = RecordingShell::new(&db);
        let created = create_system_terminal(&db, None, &shell).expect("create");

        db.with_conn(|conn| {
            let (_user, user_root) = db::create_project(conn, "app", USER_ROOT, None).unwrap();

            // 1. The terminal `cd`s INTO a registered project. The manual binding pins it.
            for cwd in [USER_ROOT, HOME, HOME_CHILD] {
                let res =
                    crate::resolve::auto_attach_terminal(conn, &created.terminal.id, Some(cwd))
                        .unwrap();
                assert_eq!(
                    res.workspace_id.as_deref(),
                    Some(scope.workspace.id.as_str()),
                    "cwd {cwd} must leave a manually-bound Système terminal exactly where it is"
                );
                assert!(!res.changed, "…and must not even rewrite the binding");
            }
            assert!(is_system_terminal(conn, &created.terminal.id).unwrap());

            // 2. NON-VACUOUS: an AUTO-bound terminal at the same cwd really does move — so the
            //    immobility above is the manual binding, not a resolver that never resolves.
            let mobile = db::create_terminal(conn, USER_ROOT, None).unwrap();
            let moved =
                crate::resolve::auto_attach_terminal(conn, &mobile.id, Some(USER_ROOT)).unwrap();
            assert_eq!(moved.workspace_id.as_deref(), Some(user_root.id.as_str()));
        });
    }

    // --- The Système commands ----------------------------------------------------

    /// A REAL directory to anchor the scope on, so the run-directory resolvers (which touch the
    /// filesystem as soon as a subfolder is involved) work on something that exists.
    ///
    /// Canonicalized on POSIX, where `std::env::temp_dir()` is commonly a symlink (`/var` →
    /// `/private/var` on macOS) and a spawn's real cwd would differ from the stored one. NOT on
    /// Windows, where `canonicalize` returns an extended-length `\\?\C:\…` path that is not the
    /// spelling anything else in nyx uses.
    fn temp_home(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "nyx-sys-home-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create the temp home");
        #[cfg(windows)]
        {
            dir
        }
        #[cfg(not(windows))]
        {
            std::fs::canonicalize(&dir).unwrap_or(dir)
        }
    }

    /// A `Db` whose Système scope is anchored on a REAL home directory.
    fn db_with_scope_at(home: &Path) -> (db::Db, SystemScope) {
        let db = db::Db::in_memory();
        let scope = db
            .with_conn(|c| {
                ensure_system_scope(c, &FixedHomeDirProvider::at(home.to_str().unwrap()))
            })
            .expect("bootstrap the Système scope on a real home");
        (db, scope)
    }

    /// Paths compared in the ONE canonical form the whole crate compares in — the Windows
    /// resolvers hand back the filesystem's own casing, the stored ones are normalized.
    fn same_path(a: &str, b: &str) -> bool {
        crate::pathnorm::normalize(a) == crate::pathnorm::normalize(b)
    }

    /// A runner sink that persists nothing — enough for the READ surfaces (`list_commands`
    /// overlays live state from the runner, and there is none).
    struct SilentSink;
    impl crate::command::RunnerSink for SilentSink {
        fn on_state(&self, _: &str, _: crate::command::RunState, _: Option<i32>) {}
        fn on_acknowledge(&self, _: &str) {}
        fn on_output(&self, _: &str, _: &[u8]) {}
        fn persist_scrollback(&self, _: &str, _: &str) {}
        fn archive_previous_run(&self, _: &str) {}
        fn clear_output(&self, _: &str) {}
    }

    fn silent_runner() -> crate::command::CommandRunner<SilentSink> {
        crate::command::CommandRunner::new(
            SilentSink,
            portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        )
    }

    /// DONE-CRITERIA #1 + #2 + #3 (« Commandes Système ») — a Système command is an ORDINARY
    /// managed command of the singleton, and every path that resolves where it runs answers the
    /// HOME (plus its subfolder).
    ///
    /// ## One template, one instance
    ///
    /// `managed_commands.project_id` is NOT NULL and names the singleton; the singleton has
    /// exactly one workspace; an instance is one (template, workspace) pair. So "exactly one
    /// instance" is not a rule written anywhere — it is the arithmetic of the singleton, and this
    /// pins it against the materialization that produces it.
    ///
    /// ## The five resolvers
    ///
    /// nyx has exactly TWO readers of a run target, and every spawn path goes through one of them:
    ///
    ///  * [`db::instance_run_context`] — read by the MCP surface
    ///    (`mcp_runtime::resolve_command_and_cwd`, which `start_command` / `relaunch_command` call)
    ///    and by the shared [`crate::command::resolve_command_and_cwd`], which is in turn what the
    ///    IPC UI (`nyx_sidecar::runtime::command`), the GROUP lifecycle
    ///    (`command::run_command_group`) and the ENGINE GATE (`mcp_engine`'s `gate_check`, the
    ///    `command_slots` path) all call;
    ///  * `db::all_instances_for_restore` — read by [`crate::command::restore_commands_on_boot`].
    ///
    /// Both join the instance's WORKSPACE and take its `root_path`, which for the singleton IS the
    /// home. That is the whole reason the Système workspace carries a real path: no runner needed
    /// a special case, and none got one.
    #[test]
    fn a_system_command_has_one_instance_and_every_runner_resolves_it_to_the_home() {
        let home = temp_home("cmd");
        std::fs::create_dir_all(home.join("sub")).expect("a real subfolder");
        let (db, scope) = db_with_scope_at(&home);
        let runner = silent_runner();

        let (plain, nested) = db.with_conn(|c| {
            let plain = db::create_template(
                c,
                &scope.project.id,
                "services",
                "docker compose up",
                None,
                db::CommandSource::default(),
            )
            .expect("a Système command is an ordinary template of the singleton");
            let nested = db::create_template(
                c,
                &scope.project.id,
                "web",
                "npm run dev",
                Some("sub"),
                db::CommandSource::default(),
            )
            .expect("subfolder keeps its ordinary relative meaning");
            (plain, nested)
        });

        // 1. THE TEMPLATE hangs on the singleton, positively — `managed_commands.project_id` is
        //    NOT NULL, and the value it carries is the Système project. Never "no project".
        assert_eq!(plain.project_id, scope.project.id);
        assert_eq!(nested.project_id, scope.project.id);
        assert!(db
            .with_conn(|c| is_system_project(c, &plain.project_id))
            .unwrap());

        // 2. EXACTLY ONE INSTANCE, on the one workspace.
        for template in [&plain, &nested] {
            let ids = db
                .with_conn(|c| db::instance_ids_for_template(c, &template.id))
                .unwrap();
            assert_eq!(ids.len(), 1, "one workspace ⇒ one instance: {ids:?}");
        }
        let instances = db
            .with_conn(|c| db::list_instances_for_workspace(c, &scope.workspace.id))
            .unwrap();
        assert_eq!(instances.len(), 2, "both templates, both on the home workspace");
        let inst_of = |command_id: &str| {
            instances
                .iter()
                .find(|i| i.command_id == command_id)
                .expect("materialized")
                .id
                .clone()
        };
        let plain_inst = inst_of(&plain.id);
        let nested_inst = inst_of(&nested.id);

        // 3. THE SHARED READER the MCP surface and `command::resolve_command_and_cwd` join on.
        let ctx = db
            .with_conn(|c| db::instance_run_context(c, &plain_inst))
            .unwrap()
            .expect("the instance resolves");
        assert!(
            same_path(&ctx.workspace_path, scope.home()),
            "the run context's workspace path IS the home: {ctx:?}"
        );
        assert_eq!(ctx.command, "docker compose up");

        // 4. THE SHARED RESOLVER — the one the IPC UI, the group lifecycle and the engine gate all
        //    call. Home at the root, home + subfolder when there is one.
        let (_cmd, cwd, _env) =
            crate::command::resolve_command_and_cwd(&db, &plain_inst).expect("resolve");
        assert!(same_path(&cwd, scope.home()), "{cwd} vs {}", scope.home());
        let (_cmd, nested_cwd, _env) =
            crate::command::resolve_command_and_cwd(&db, &nested_inst).expect("resolve");
        assert!(
            same_path(&nested_cwd, home.join("sub").to_str().unwrap()),
            "home + subfolder, the ordinary relative rule: {nested_cwd}"
        );

        // 5. THE BOOT-RESTORE reader (a different query, same join).
        let restore = db.with_conn(db::all_instances_for_restore).unwrap();
        for row in &restore {
            assert!(
                same_path(&row.workspace_path, scope.home()),
                "boot restore resolves the home too: {row:?}"
            );
        }
        assert_eq!(restore.len(), 2);

        // 6. THE MCP READ of the run directory — `list_commands` is the tool an agent discovers
        //    the cwd through, and it is on the scope's allow-list.
        let listed = crate::mcp_tools_core::list_commands(
            &db,
            &runner,
            &serde_json::json!({ "workspace_id": scope.workspace.id }),
        )
        .expect("list_commands is a capability of the Système scope");
        let rows = listed["commands"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            let expected = if row["command_id"] == serde_json::json!(nested.id) {
                home.join("sub")
            } else {
                home.clone()
            };
            assert!(
                same_path(row["cwd"].as_str().unwrap(), expected.to_str().unwrap()),
                "list_commands shows the run directory under the home: {row}"
            );
        }

        // 7. NO SECOND WORKSPACE, hence NO MATERIALIZATION ANYWHERE ELSE — the arithmetic that
        //    makes "exactly one instance" true stays true.
        db.with_conn(|c| {
            let err = crate::usecase::register_workspace(
                c,
                &crate::usecase::NullEventSink,
                &scope.project.id,
                "extra",
                home.join("sub").to_str().unwrap(),
                crate::usecase::DomainEventSource::Ui,
            )
            .expect_err("the singleton owns exactly ONE workspace, the bootstrapped one");
            assert!(err.is_forbidden(), "{err}");

            // A USER project with TWO workspaces alongside: its own templates materialize there,
            // and NOTHING of the Système project does — materialization is per project.
            let (user, _root) = db::create_project(c, "app", USER_ROOT, None).unwrap();
            db::create_workspace(
                c,
                &user.id,
                "wt",
                &format!("{USER_ROOT}{}wt", std::path::MAIN_SEPARATOR),
            )
            .unwrap();
            for template in [&plain, &nested] {
                assert_eq!(
                    db::instance_ids_for_template(c, &template.id).unwrap().len(),
                    1,
                    "a Système template never materializes outside the singleton's one workspace"
                );
            }
        });

        let _ = std::fs::remove_dir_all(&home);
    }

    /// A sink that RECORDS what the runner produced, per instance — enough to prove a Système
    /// command really ran, and where.
    #[derive(Default)]
    struct RecordingSink {
        output: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
        states: std::sync::Mutex<Vec<(String, crate::command::RunState)>>,
        cleared: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingSink {
        fn output_of(&self, instance_id: &str) -> String {
            String::from_utf8_lossy(
                self.output
                    .lock()
                    .unwrap()
                    .get(instance_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
            )
            .to_string()
        }

        /// Forget what an instance produced, so the NEXT run's output can be asserted on its own
        /// (the buffer is cumulative, like the real one).
        fn forget(&self, instance_id: &str) {
            self.output.lock().unwrap().remove(instance_id);
        }
    }

    impl crate::command::RunnerSink for std::sync::Arc<RecordingSink> {
        fn on_state(&self, id: &str, state: crate::command::RunState, _: Option<i32>) {
            self.states.lock().unwrap().push((id.to_string(), state));
        }
        fn on_acknowledge(&self, _: &str) {}
        fn on_output(&self, id: &str, bytes: &[u8]) {
            self.output
                .lock()
                .unwrap()
                .entry(id.to_string())
                .or_default()
                .extend_from_slice(bytes);
        }
        fn persist_scrollback(&self, _: &str, _: &str) {}
        fn archive_previous_run(&self, _: &str) {}
        fn clear_output(&self, id: &str) {
            self.cleared.lock().unwrap().push(id.to_string());
        }
    }

    /// DONE-CRITERION #4 (« Commandes Système ») — start / stop / relaunch, the OUTPUT, the
    /// `restart_on_startup` boot restore and the scrollback clear all reuse the EXISTING runtime.
    /// There is no Système runner, no Système lifecycle, no Système output buffer: the same tools
    /// and the same `CommandRunner` drive a command of the singleton exactly as they drive one of
    /// a user project.
    ///
    /// The command is `cat marker.txt` on a marker file placed in the HOME, so the assertion is
    /// not that some resolver returned a string — it is that the SPAWNED PROCESS really ran in the
    /// home.
    ///
    /// IT RUNS ON BOTH PLATFORMS, deliberately (« Commandes Système exécutées depuis le home sur
    /// tous les chemins »): `cat` is bash's on POSIX and PowerShell's own alias for `Get-Content`
    /// on Windows, and `command::resolve_shell` picks exactly those two. So the cross-OS gate
    /// exercises the real spawn on each platform instead of skipping Windows.
    ///
    /// THREE OF THE FIVE SPAWN PATHS ARE DRIVEN END TO END HERE — the MCP tools, the BOOT RESTORE
    /// (its own reader, `all_instances_for_restore`) and the GROUP lifecycle — each proven by the
    /// marker the process could only read from the home. The IPC UI's is driven in the sidecar
    /// (`db_command::tests::the_ui_bridge_creates_a_system_command_with_one_instance_at_the_home`).
    /// The fifth, the ENGINE GATE, is NOT driven from here: `mcp_engine`'s `run_gate` is private
    /// and reachable only through a whole `nyx_next` tick. What holds it is that it resolves
    /// through the very same `crate::command::resolve_command_and_cwd` the group path just proved —
    /// one resolver, asserted directly above — and not a second computation of its own.
    #[test]
    fn a_system_command_runs_in_the_home_through_the_untouched_runtime() {
        use crate::command::RunState;
        let home = temp_home("runtime");
        std::fs::write(home.join("marker.txt"), "NYX_SYSTEME_HOME").expect("marker");
        let (db, scope) = db_with_scope_at(&home);

        let sink = std::sync::Arc::new(RecordingSink::default());
        let runner = crate::command::CommandRunner::new(
            std::sync::Arc::clone(&sink),
            portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        );
        let wait = |state: RunState, id: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if runner.state_of(id) == state {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            false
        };

        let template = db
            .with_conn(|c| {
                db::create_template(
                    c,
                    &scope.project.id,
                    "marker",
                    "cat marker.txt",
                    None,
                    db::CommandSource::default(),
                )
            })
            .expect("template");
        let instance = db
            .with_conn(|c| db::instance_ids_for_template(c, &template.id))
            .unwrap()
            .pop()
            .expect("the one instance");

        // START — through the ORDINARY MCP runtime tool (which is on the scope's allow-list).
        crate::mcp_runtime::start_command(
            &db,
            &runner,
            &serde_json::json!({ "instance_id": instance }),
        )
        .expect("a Système command starts like any other");
        assert!(wait(RunState::Success, &instance), "the command finishes");
        assert!(
            sink.output_of(&instance).contains("NYX_SYSTEME_HOME"),
            "the process really ran IN THE HOME — it read a file only that cwd can see: {:?}",
            sink.output_of(&instance)
        );

        // STOP on an already-finished instance — the ordinary idempotent no-op.
        let stopped = crate::mcp_runtime::stop_command(
            &db,
            &runner,
            &serde_json::json!({ "instance_id": instance }),
        )
        .expect("stop is idempotent");
        assert_eq!(stopped["was_running"], serde_json::json!(false));

        // RELAUNCH — a fresh process, same cwd, same output.
        crate::mcp_runtime::relaunch_command(
            &db,
            &runner,
            &serde_json::json!({ "instance_id": instance }),
        )
        .expect("relaunch");
        assert!(wait(RunState::Success, &instance), "the relaunch finishes");

        // THE OUTPUT TOOL reads the very same buffer.
        let out = crate::mcp_runtime::get_command_output(
            &db,
            &runner,
            &serde_json::json!({ "instance_id": instance }),
        )
        .expect("get_command_output");
        assert_eq!(out["instance_id"], serde_json::json!(instance));

        // RESTART_ON_STARTUP + the BOOT RESTORE: the shared `restore_commands_on_boot` relaunches
        // a Système service exactly as it relaunches a project one — same reader, same resolver.
        db.with_conn(|c| {
            db::set_restart_on_startup(c, &template.id, true).expect("restart_on_startup");
            db::set_was_running_on_shutdown(c, &instance, true).expect("shutdown snapshot");
        });
        sink.forget(&instance);
        let relaunched = crate::command::restore_commands_on_boot(&db, &runner);
        assert!(
            relaunched.contains(&instance),
            "the boot restore relaunched the Système service: {relaunched:?}"
        );
        assert!(wait(RunState::Success, &instance));
        assert!(
            sink.output_of(&instance).contains("NYX_SYSTEME_HOME"),
            "…and the BOOT RESTORE spawned it in the home too (its own reader, \
             `all_instances_for_restore`, not the one the tools use): {:?}",
            sink.output_of(&instance)
        );

        // THE GROUP LIFECYCLE is a third spawn path, and it resolves the home as well — through
        // the shared `command::run_command_group`, which is also what the ENGINE GATE's
        // `run_gate` calls (`crate::command::resolve_command_and_cwd`, one resolver for the three).
        let second = db
            .with_conn(|c| {
                db::create_template(
                    c,
                    &scope.project.id,
                    "marker2",
                    "cat marker.txt",
                    None,
                    db::CommandSource::default(),
                )
            })
            .expect("a second Système command");
        let second_instance = db
            .with_conn(|c| db::instance_ids_for_template(c, &second.id))
            .unwrap()
            .pop()
            .expect("its one instance");
        let group = db
            .with_conn(|c| db::create_command_group(c, &scope.project.id, &[template.id.clone(), second.id.clone()]))
            .expect("group the two");
        sink.forget(&instance);
        sink.forget(&second_instance);
        crate::mcp_runtime::start_command(
            &db,
            &runner,
            &serde_json::json!({ "group_id": group.id, "workspace_id": scope.workspace.id }),
        )
        .expect("starting the whole group is the ordinary gesture");
        for id in [&instance, &second_instance] {
            assert!(wait(RunState::Success, id), "group member {id} finishes");
            assert!(
                sink.output_of(id).contains("NYX_SYSTEME_HOME"),
                "the group start spawned {id} in the home: {:?}",
                sink.output_of(id)
            );
        }

        // The OUTPUT CLEAR is the same transition too (no Système buffer of its own).
        crate::mcp_tools_core::clear_command_output(
            &db,
            &runner,
            &serde_json::json!({ "instance_id": instance }),
        )
        .expect("clear_command_output is a capability of the Système scope");
        assert!(sink.cleared.lock().unwrap().contains(&instance));

        let _ = runner.stop(&instance);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// DONE-CRITERION #5 (« Commandes Système ») — a Système GROUP is an ordinary command group.
    ///
    /// Two things, and only the pair is meaningful: the SHARED grouping code serves it end to end
    /// (there is no parallel Système grouping — the group tool, the group reader and the group
    /// LIFECYCLE are the same ones a user project uses), and it stays MONO-PROJECT: a user
    /// project's command cannot be pulled into it, at creation or by adding.
    ///
    /// The homogeneity is not a Système rule at all — it is the generic project scoping of
    /// `db::create_command_group` / `db::add_to_command_group`, which is exactly why the PRD can
    /// say "un groupe reste strictement mono-projet" without a line of new code.
    #[test]
    fn a_system_command_group_uses_the_shared_grouping_and_stays_mono_project() {
        let home = temp_home("group");
        let (db, scope) = db_with_scope_at(&home);
        let runner = silent_runner();

        let (sys_ids, user_command) = db.with_conn(|c| {
            let sys_ids: Vec<String> = (0..2)
                .map(|i| {
                    db::create_template(
                        c,
                        &scope.project.id,
                        &format!("svc{i}"),
                        "echo hi",
                        None,
                        db::CommandSource::default(),
                    )
                    .expect("Système template")
                    .id
                })
                .collect();
            let (user, _root) = db::create_project(c, "app", USER_ROOT, None).unwrap();
            let user_command = db::create_template(
                c,
                &user.id,
                "dev",
                "npm run dev",
                None,
                db::CommandSource::default(),
            )
            .expect("user template")
            .id;
            (sys_ids, user_command)
        });

        // THE SHARED CREATION + READER.
        let group = db
            .with_conn(|c| db::create_command_group(c, &scope.project.id, &sys_ids))
            .expect("grouping a Système project's commands is the ordinary gesture");
        let groups = db
            .with_conn(|c| db::list_command_groups(c, &scope.project.id))
            .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].command_ids, sys_ids);

        // THE SHARED LIFECYCLE — `stop_command { group_id, workspace_id }` answers the group
        // shape, over the singleton's one workspace, with one member row per command. Nothing
        // spawns: what is under test is that the group path serves the scope at all.
        let stopped = crate::mcp_runtime::stop_command(
            &db,
            &runner,
            &serde_json::json!({ "group_id": group.id, "workspace_id": scope.workspace.id }),
        )
        .expect("the group lifecycle serves the Système scope");
        assert_eq!(stopped["group_id"], serde_json::json!(group.id));
        assert_eq!(stopped["total"], serde_json::json!(2));
        assert_eq!(stopped["members"].as_array().unwrap().len(), 2);

        // MONO-PROJECT, both doors.
        db.with_conn(|c| {
            let mixed = db::create_command_group(
                c,
                &scope.project.id,
                &[sys_ids[0].clone(), user_command.clone()],
            )
            .expect_err("a group is strictly mono-project");
            assert!(
                matches!(mixed, db::CommandGroupError::CommandNotFound { ref id } if *id == user_command),
                "a foreign command is simply not one of this project's commands: {mixed}"
            );
            let added = db::add_to_command_group(c, &group.id, &user_command)
                .expect_err("nor can one be added afterwards");
            assert!(
                matches!(added, db::CommandGroupError::CommandNotFound { .. }),
                "{added}"
            );
            // The group is untouched by the two refusals.
            assert_eq!(
                db::list_command_groups(c, &scope.project.id).unwrap()[0].command_ids,
                sys_ids
            );
        });

        let _ = std::fs::remove_dir_all(&home);
    }

    // --- The domain boundary ---------------------------------------------------

    /// DONE-CRITERION (task « Frontière de domaine ») — the ALLOW-LIST is confronted with the LIVE
    /// advertised MCP surface, in both directions:
    ///
    ///  1. every managed-command tool — what the Système scope EXISTS for — really is advertised
    ///     AND allowed (so a rename of a tool turns the allow-list entry into a failing test rather
    ///     than into a silent refusal of the scope's own capability);
    ///  2. every OTHER advertised tool, aimed at the singleton, is REFUSED — table-driven over the
    ///     descriptors the agent is really served, so a tool added to the surface cannot quietly
    ///     gain power over the singleton.
    #[test]
    fn every_advertised_mcp_tool_is_classified() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let payload = serde_json::json!({
            "project_id": scope.project.id,
            "workspace_id": scope.workspace.id,
        });

        let advertised: Vec<String> = crate::mcp::tool_descriptors()
            .into_iter()
            .map(|d| {
                d["name"]
                    .as_str()
                    .expect("every descriptor names its tool")
                    .to_string()
            })
            .collect();
        assert!(advertised.len() > 30, "the surface is not empty: {advertised:?}");

        // 1. The scope's OWN capabilities: advertised, and allowed.
        for tool in [
            "list_commands",
            "add_command",
            "update_command",
            "import_commands",
            "remove_command",
            "remove_commands",
            "reorder_commands",
            "manage_command_group",
            "clear_command_output",
            "start_command",
            "stop_command",
            "relaunch_command",
            "get_command_output",
            "wait_for_command",
        ] {
            assert!(
                advertised.contains(&tool.to_string()),
                "`{tool}` is on the allow-list but is not an advertised MCP tool — the two drifted"
            );
            assert!(is_system_capability(tool), "`{tool}` must stay allowed");
            assert!(
                guard_payload(&mut conn, tool, &payload).is_ok(),
                "`{tool}` must pass the boundary for the Système scope"
            );
        }

        // 2. Everything else advertised is refused when it aims the singleton.
        let mut refused = 0usize;
        for name in &advertised {
            match guard_payload(&mut conn, name, &payload) {
                Ok(()) => assert!(
                    is_system_capability(name),
                    "advertised tool `{name}` passes the boundary without being on the allow-list"
                ),
                Err(e) => {
                    assert!(e.is_forbidden(), "`{name}` failed for the wrong reason: {e}");
                    assert_eq!(e.code(), SYSTEM_SCOPE_FORBIDDEN);
                    refused += 1;
                }
            }
        }
        assert!(
            refused > 20,
            "the refusal must not be vacuous — only {refused} advertised tools were refused"
        );

        // The families the PRD names explicitly, spelled out so a regression reads as itself.
        for tool in [
            // project lifecycle + settings
            "set_project_settings",
            "project_set_repos",
            // workspace lifecycle
            "workspace_add",
            "create_workspace",
            "remove_workspace",
            "list_workspaces",
            // Git reads
            "workspace_git",
            "repo_diff",
            "repo_git_view",
            // …and the ONE filesystem read, refused for the very same reason (finding
            // 01M15RT4QE2JY37QQJAGHFKVX0): this workspace's root_path is the user's HOME, so
            // walking it renders the paths of every project the user has. The scope's other reads
            // read the SCOPE; this one reads the machine.
            "list_importable_scripts",
            // product aggregates
            "spec_create",
            "task_add",
            "idea_capture",
            "annexe_add",
            "workflow_create",
            "design_inventory",
        ] {
            let err = guard_payload(&mut conn, tool, &payload)
                .expect_err("this capability is not the Système scope's");
            assert!(err.is_forbidden(), "`{tool}`: {err}");
        }
    }

    /// DONE-CRITERION — HIDING THE ID IS NEVER THE DEFENSE. The guard walks the whole payload, so
    /// the singleton is refused whatever the argument is CALLED (`project_id`, `projectId`, a bare
    /// `id`), wherever it sits (an array, a nested object) — and the refusal names the entity and
    /// returns the id rather than pretending it does not exist.
    #[test]
    fn the_boundary_keys_on_the_id_being_aimed_not_on_it_being_secret() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let pid = scope.project.id.as_str();

        for payload in [
            serde_json::json!({ "project_id": pid }),
            serde_json::json!({ "projectId": pid }),
            serde_json::json!({ "id": pid }),
            serde_json::json!({ "ids": ["other", pid] }),
            serde_json::json!({ "nested": { "deeper": [{ "whatever": pid }] } }),
        ] {
            let err = guard_payload(&mut conn, "delete_project", &payload)
                .expect_err("aiming the singleton is what is refused, not naming a field");
            match err {
                SystemScopeError::Forbidden {
                    entity,
                    ref id,
                    ref capability,
                } => {
                    assert_eq!(entity, "project");
                    assert_eq!(id, pid, "the id is RETURNED — concealment is not the defense");
                    assert_eq!(capability, "delete_project");
                }
                other => panic!("expected Forbidden, got {other:?}"),
            }
        }

        // The workspace side is refused the same way, and named as a workspace.
        let err = guard_payload(
            &mut conn,
            "workspace_delete",
            &serde_json::json!({ "id": scope.workspace.id }),
        )
        .expect_err("the Système workspace has no lifecycle");
        assert!(matches!(
            err,
            SystemScopeError::Forbidden {
                entity: "workspace",
                ..
            }
        ));

        // A USER project's id passes the very same capability — the refusal is about the NATURE of
        // the container, never about the operation.
        let (user, user_ws) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();
        assert!(guard_payload(
            &mut conn,
            "delete_project",
            &serde_json::json!({ "id": user.id })
        )
        .is_ok());
        assert!(guard_payload(
            &mut conn,
            "workspace_delete",
            &serde_json::json!({ "id": user_ws.id })
        )
        .is_ok());
        // …and so does an id that names NOTHING: the boundary answers "may you do this HERE", not
        // "does this exist", so a caller's own not-found stays distinguishable.
        assert!(guard_payload(
            &mut conn,
            "delete_project",
            &serde_json::json!({ "id": "no-such-project" })
        )
        .is_ok());
    }

    /// FINDING #14 — `attach_terminal` is on the allow-list for the HUMAN's MANUAL binding, and for
    /// nothing else. The three directions, at the BOUNDARY itself rather than in a caller:
    ///
    ///  1. `{ workspace: <système>, mode: "auto" }` is REFUSED with the stable code. An auto binding
    ///     onto the singleton would not survive: [`crate::resolve::decide_attachment`] pins only a
    ///     `manual` binding, so the next `auto_attach_terminal` pass would re-bind that terminal to
    ///     whatever USER workspace covers its cwd — the terminal would silently LEAVE the scope,
    ///     which is the exit-side twin of the auto-attach capture forbidden on the way in.
    ///  2. `{ workspace: <système>, mode: "manual" }` PASSES, and the terminal really reads as a
    ///     Système one — the positive membership the whole design rests on.
    ///  3. `{ workspace: <utilisateur>, mode: "auto" }` is untouched: `auto` stays legitimate
    ///     everywhere else. The refusal is about WHERE the binding points, not about the mode.
    #[test]
    fn only_a_manual_binding_attaches_a_terminal_to_the_system_scope() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let (_user, user_root) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();
        let term = db::create_terminal(&mut conn, HOME, None).unwrap();
        let payload = |workspace_id: &str, mode: &str| {
            serde_json::json!({ "terminalId": term.id, "workspaceId": workspace_id, "mode": mode })
        };

        // 1. AUTO onto the singleton — refused, named as a workspace, with the stable code.
        let err = guard_payload(
            &mut conn,
            "attach_terminal",
            &payload(&scope.workspace.id, db::BINDING_AUTO),
        )
        .expect_err("an auto binding is not the positive membership this scope is built on");
        assert_eq!(err.code(), SYSTEM_SCOPE_FORBIDDEN);
        match &err {
            SystemScopeError::Forbidden {
                entity,
                id,
                capability,
            } => {
                assert_eq!(*entity, "workspace");
                assert_eq!(id, &scope.workspace.id);
                assert_eq!(capability, "attach_terminal");
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
        // A payload naming NO mode at all is not a manual binding either — the conservative end.
        assert!(guard_payload(
            &mut conn,
            "attach_terminal",
            &serde_json::json!({ "terminalId": term.id, "workspaceId": scope.workspace.id })
        )
        .is_err());

        // 2. MANUAL onto the singleton — allowed, and the binding really lands in the scope.
        guard_payload(
            &mut conn,
            "attach_terminal",
            &payload(&scope.workspace.id, db::BINDING_MANUAL),
        )
        .expect("the human's manual binding IS a capability of the Système scope");
        db::attach_terminal(&mut conn, &term.id, &scope.workspace.id, db::BINDING_MANUAL).unwrap();
        assert!(is_system_terminal(&mut conn, &term.id).unwrap());

        // 3. AUTO onto a USER workspace — untouched.
        guard_payload(
            &mut conn,
            "attach_terminal",
            &payload(&user_root.id, db::BINDING_AUTO),
        )
        .expect("auto binding stays legitimate on every user workspace");
    }

    /// FINDING #30 — the binding invariant belongs to the COLUMN, not to one call.
    /// `terminals.workspace_binding_mode` has THREE writers, and a member of the Système scope —
    /// bound MANUALLY and POSITIVELY to the singleton's workspace — must survive all three. The two
    /// states none of them may reach:
    ///
    ///  * mode `auto` with the `workspace_id` still naming the singleton — [`crate::resolve`]'s
    ///    `decide_attachment` pins only `manual`, so the next `auto_attach_terminal` pass re-binds
    ///    the terminal onto whatever user workspace covers its cwd: it LEAVES the scope;
    ///  * `workspace_id IS NULL` — the orphan bucket, which the scope is disjoint from (PRD task
    ///    `01KY4FP8PNQ6KSXKHH6CBKGWN6`: « les listes Système excluent les orphelins »).
    ///
    /// `attach_terminal` is covered from the PAYLOAD at the two adapters
    /// ([`only_a_manual_binding_attaches_a_terminal_to_the_system_scope`]); the two others carry
    /// neither a mode nor a workspace in their arguments, so what they would break is only visible
    /// by reading the terminal's current attachment — they enforce it themselves.
    ///
    /// AN ANTI-DRIFT GUARD, NOT A LIVE BUG: neither `unpin_terminal_workspace` nor
    /// `detach_terminal` has a production caller today. The moment it matters is next — the phase-2
    /// use case that poses the first Système binding, and the phase-3 surface that would offer a
    /// « détacher / dépingler » gesture.
    #[test]
    fn no_writer_of_the_binding_column_can_take_a_terminal_out_of_the_system_scope() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let (_user, user_root) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();

        // A real member: the manual binding onto the singleton's workspace.
        let sys = db::create_terminal(&mut conn, HOME, None).unwrap();
        db::attach_terminal(&mut conn, &sys.id, &scope.workspace.id, db::BINDING_MANUAL).unwrap();
        assert!(is_system_terminal(&mut conn, &sys.id).unwrap());

        // WRITER 1 — `attach_terminal`: refused from the payload, at the boundary both adapters
        // call, as soon as the mode is not `manual`.
        assert!(
            guard_payload(
                &mut conn,
                "attach_terminal",
                &serde_json::json!({
                    "terminalId": sys.id,
                    "workspaceId": scope.workspace.id,
                    "mode": db::BINDING_AUTO
                })
            )
            .is_err(),
            "an auto binding onto the singleton is the exit the scope forbids"
        );

        // WRITER 2 — `unpin_terminal_workspace` produces that forbidden state LITERALLY: mode
        // flipped to `auto`, `workspace_id` kept.
        let err = db::unpin_terminal_workspace(&mut conn, &sys.id)
            .expect_err("unpinning a Système terminal IS the forbidden state, spelled out");
        assert_eq!(err.code(), SYSTEM_SCOPE_FORBIDDEN);
        match &err {
            SystemScopeError::Forbidden {
                entity,
                id,
                capability,
            } => {
                assert_eq!(*entity, "terminal");
                assert_eq!(id, &sys.id);
                assert_eq!(capability, "unpin_terminal_workspace");
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }

        // WRITER 3 — `detach_terminal` would drop the member into the orphan bucket.
        let err = db::detach_terminal(&mut conn, &sys.id)
            .expect_err("a Système terminal may not fall into the orphan bucket");
        assert_eq!(err.code(), SYSTEM_SCOPE_FORBIDDEN);
        assert!(err.is_forbidden());

        // Neither refusal wrote anything: the member is still manually bound to the scope.
        let after = db::get_terminal(&mut conn, &sys.id).unwrap().unwrap();
        assert_eq!(
            after.workspace_id.as_deref(),
            Some(scope.workspace.id.as_str())
        );
        assert_eq!(after.workspace_binding_mode, db::BINDING_MANUAL);
        assert!(is_system_terminal(&mut conn, &sys.id).unwrap());

        // CONTROL — a terminal of a USER project keeps the ordinary behaviour on all three.
        let user_term = db::create_terminal(&mut conn, USER_ROOT, None).unwrap();
        db::attach_terminal(&mut conn, &user_term.id, &user_root.id, db::BINDING_MANUAL).unwrap();
        db::unpin_terminal_workspace(&mut conn, &user_term.id)
            .expect("unpinning stays the ordinary gesture outside the scope");
        let unpinned = db::get_terminal(&mut conn, &user_term.id).unwrap().unwrap();
        assert_eq!(unpinned.workspace_binding_mode, db::BINDING_AUTO);
        assert_eq!(
            unpinned.workspace_id.as_deref(),
            Some(user_root.id.as_str()),
            "unpin still keeps the workspace; only the mode changes"
        );
        db::detach_terminal(&mut conn, &user_term.id)
            .expect("detaching a user terminal is untouched");
        assert_eq!(
            db::get_terminal(&mut conn, &user_term.id)
                .unwrap()
                .unwrap()
                .workspace_id,
            None
        );

        // …and an ALREADY orphan terminal is not a Système one, so it passes too — the guard keys
        // on membership, never on "has no workspace".
        let orphan = db::create_terminal(&mut conn, USER_ROOT, None).unwrap();
        db::detach_terminal(&mut conn, &orphan.id).expect("an orphan is not a Système terminal");
    }

    /// FINDING #16 — THE SHORT PATH. The guard sits at the top of the sidecar's ~120-arm db-command
    /// match, which is what makes it exhaustive — and also puts it on arms that touch no database.
    /// [`needs_scope_lookup`] is the DB-FREE half of the decision the two adapters call BEFORE
    /// checking a connection out of the pool: a payload with no string in it cannot name an id.
    ///
    /// FINDING #26 — the dispense belongs to the PAYLOAD, never to the command name. So every
    /// command NAMED here is probed with the payload the renderer really sends: a name asserted
    /// against an invented payload would stay green whatever the bridge did, which is the exact
    /// shape of proof this review round refuses. The purely structural cases are spelled under a
    /// name that belongs to no command, so they cannot be read as a claim about one.
    #[test]
    fn a_payload_that_can_name_no_id_is_decided_without_reading_the_database() {
        // `terminal_info`'s real payload is one NUMBER (`sidebar/auto-label.ts`) — and it is polled
        // at 1 Hz per open terminal. Not allow-listed, and yet there is nothing it could ever name.
        assert!(!needs_scope_lookup(
            "terminal_info",
            &serde_json::json!({ "id": 12 })
        ));
        // `shell_list` / `integration_list` are invoked with NO argument at all
        // (`sidebar/settings-dialog.tsx`); the bridge requires an object, so they arrive as `{}`.
        assert!(!needs_scope_lookup("shell_list", &serde_json::json!({})));
        assert!(!needs_scope_lookup("integration_list", &serde_json::json!({})));
        // An allow-listed call is decided without a read either — the guard would short-circuit.
        assert!(!needs_scope_lookup(
            "command_list",
            &serde_json::json!({ "projectId": "p-1" })
        ));
        // A STRUCTURAL probe, under no real command's name: numbers, booleans, nulls and empty
        // containers name nothing however deeply they nest.
        assert!(!needs_scope_lookup(
            "whatever",
            &serde_json::json!({ "n": 1, "ok": true, "nothing": null, "empty": [], "deep": { "a": [1, 2] } })
        ));

        // …but ANY string, anywhere, still demands the lookup: the walk is structural, and no
        // command name buys an exemption. `register_terminal_pty` is a NO-OP arm, and it pays the
        // lookup all the same, because the renderer sends it a `recordId`
        // (`sidebar/terminal-manager.tsx`, `{ recordId, ptyId }`).
        assert!(needs_scope_lookup(
            "register_terminal_pty",
            &serde_json::json!({ "recordId": "1c9e5f2a-7d43-4a10-9b6e-2f8c0d5a41b7", "ptyId": 12 })
        ));
        assert!(needs_scope_lookup(
            "delete_project",
            &serde_json::json!({ "id": "p-1" })
        ));
        assert!(needs_scope_lookup(
            "projects_reorder",
            &serde_json::json!({ "ids": ["p-1"] })
        ));
        assert!(needs_scope_lookup(
            "whatever",
            &serde_json::json!({ "a": { "b": [{ "c": "p-1" }] } })
        ));
        // …including an allow-list entry whose PAYLOAD condition does not hold.
        assert!(needs_scope_lookup(
            "attach_terminal",
            &serde_json::json!({ "workspaceId": "w-1", "mode": "auto" })
        ));

        // And the full guard answers `Ok` on that short path against a REAL bootstrapped scope, so
        // the skipped read changed no verdict.
        let mut conn = open_in_memory();
        bootstrap(&mut conn);
        assert!(guard_payload(&mut conn, "terminal_info", &serde_json::json!({ "id": 12 })).is_ok());
    }

    /// FINDING #13, MCP HALF — the two halves of the allow-list are two NAMESPACES, and the IPC one
    /// must never collide with the live advertised surface. If a tool were advertised tomorrow under
    /// an IPC-only name (`attach_terminal`, `command_list`, `workspace_runtime_snapshot`…), it would
    /// be allow-listed for the AGENT the day it appeared — and
    /// [`every_advertised_mcp_tool_is_classified`] could not see it, because its `Ok` branch only
    /// re-asserts the very membership that let the call through.
    ///
    /// The rename drift on the other half — an IPC entry matching no dispatcher arm any more — is
    /// pinned from the sidecar, where the dispatcher lives:
    /// `db_command::every_ipc_capability_of_the_system_scope_is_served_by_the_dispatcher`.
    #[test]
    fn the_two_halves_of_the_allow_list_do_not_share_a_name_with_the_live_mcp_surface() {
        let advertised: Vec<String> = crate::mcp::tool_descriptors()
            .into_iter()
            .map(|d| d["name"].as_str().expect("named descriptor").to_string())
            .collect();

        for name in SYSTEM_SCOPE_IPC_CAPABILITIES {
            assert!(
                !advertised.contains(&name.to_string()),
                "`{name}` is an IPC-only allow-list entry, but it is now an ADVERTISED MCP tool: \
                 advertising it allow-lists it for the agent. Move it to \
                 SYSTEM_SCOPE_MCP_CAPABILITIES deliberately, or rename the tool."
            );
            assert!(
                !SYSTEM_SCOPE_MCP_CAPABILITIES.contains(name),
                "`{name}` is on BOTH halves — the two namespaces must stay disjoint"
            );
        }
        // The MCP half is confronted with the advertised surface, in full, by
        // `every_advertised_mcp_tool_is_classified`; here we only pin that it is not empty and that
        // `is_system_capability` really reads BOTH halves.
        assert!(!SYSTEM_SCOPE_MCP_CAPABILITIES.is_empty());
        assert!(!SYSTEM_SCOPE_IPC_CAPABILITIES.is_empty());
        assert!(is_system_capability(SYSTEM_SCOPE_MCP_CAPABILITIES[0]));
        assert!(is_system_capability(SYSTEM_SCOPE_IPC_CAPABILITIES[0]));
        assert!(!is_system_capability("delete_project"));
    }

    /// A database with NO Système scope yet (pre-bootstrap) matches nothing and refuses nothing —
    /// the guard must not become a blanket "refuse everything" when the singleton is absent.
    #[test]
    fn the_boundary_refuses_nothing_before_the_scope_is_bootstrapped() {
        let mut conn = open_in_memory();
        let (user, _ws) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();
        assert!(guard_payload(
            &mut conn,
            "delete_project",
            &serde_json::json!({ "id": user.id })
        )
        .is_ok());
    }

    /// DONE-CRITERION — `system_scope_forbidden` is a code of its OWN, distinct from every other
    /// MCP code and in particular from `invalid_id`. Read off the live [`crate::mcp::RpcCode`]
    /// mapping so the two spellings cannot drift.
    #[test]
    fn the_refusal_code_is_stable_and_distinct_from_an_unknown_id() {
        use crate::mcp::{RpcCode, RpcError};
        assert_eq!(RpcCode::SystemScopeForbidden.as_str(), SYSTEM_SCOPE_FORBIDDEN);
        assert_ne!(
            RpcCode::SystemScopeForbidden.as_str(),
            RpcCode::InvalidId.as_str()
        );

        let err: RpcError = SystemScopeError::Forbidden {
            entity: "project",
            id: "p-1".into(),
            capability: "delete_project".into(),
        }
        .into();
        assert_eq!(err.code.as_str(), SYSTEM_SCOPE_FORBIDDEN);
        let data = err.data.expect("the refusal carries machine-readable data");
        assert_eq!(data["entity"], "project");
        assert_eq!(data["id"], "p-1");
        assert_eq!(data["capability"], "delete_project");

        // A bootstrap/SQL failure is NOT a boundary refusal and must not borrow its code.
        let internal: RpcError = SystemScopeError::HomeMissing.into();
        assert_eq!(internal.code.as_str(), RpcCode::Internal.as_str());
    }

    // --- The invariant: Système (positive) ⟂ orphan (null) ---------------------
    //
    // THE WHOLE POINT OF THE SCOPE, stated as tests. nyx already HAD an "absence of project", and
    // it is SUFFERED: `delete_project` deliberately detaches its terminals (`ON DELETE SET NULL`)
    // rather than closing them, leaving orphans. The Système scope is a WANTED absence, and the two
    // must never meet — neither by the scope adopting an orphan, nor by an orphan showing up in the
    // scope's lists.

    /// Attach `terminal_id` to the Système workspace the way a HUMAN does it: manually, positively.
    fn bind_to_system(conn: &mut SqliteConnection, terminal_id: &str, scope: &SystemScope) {
        db::attach_terminal(conn, terminal_id, &scope.workspace.id, db::BINDING_MANUAL)
            .expect("bind the terminal onto the Système workspace");
    }

    /// Every terminal in the ORPHAN bucket (`workspace_id IS NULL`) — the set `delete_project`
    /// feeds and that the UI shows as "loose".
    fn orphans(conn: &mut SqliteConnection) -> Vec<String> {
        db::list_terminals(conn)
            .unwrap()
            .into_iter()
            .filter(|t| t.workspace_id.is_none())
            .map(|t| t.id)
            .collect()
    }

    /// DONE-CRITERION #1/#5 — deleting a USER project leaves its terminals in the ORPHAN bucket,
    /// and NOTHING adopts them: not the bootstrap (it re-reads its own two rows and touches no
    /// terminal), not the resolver (the Système workspace is not a candidate), not the scope's
    /// runtime read.
    #[test]
    fn deleting_a_user_project_leaves_orphans_that_the_system_scope_never_adopts() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let (user, user_root) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();

        let term = db::create_terminal(&mut conn, USER_ROOT, None).unwrap();
        db::attach_terminal(&mut conn, &term.id, &user_root.id, db::BINDING_AUTO).unwrap();

        // The documented behaviour this scope must not disturb: the project goes, the TERMINAL
        // stays — detached, never closed.
        assert_eq!(db::delete_project(&mut conn, &user.id).unwrap(), 1);
        let after = db::get_terminal(&mut conn, &term.id)
            .unwrap()
            .expect("the terminal SURVIVES its project");
        assert_eq!(
            after.workspace_id, None,
            "delete_project detaches, it does not close"
        );
        assert_eq!(orphans(&mut conn), vec![term.id.clone()]);

        // 1. THE BOOTSTRAP does not adopt it. Re-running is idempotent and terminal-blind.
        let rebooted = bootstrap(&mut conn);
        assert_eq!(rebooted, scope);
        assert_eq!(orphans(&mut conn), vec![term.id.clone()]);

        // 2. THE RESOLVER does not adopt it — not from its own cwd, not from the home.
        for cwd in [USER_ROOT, HOME, HOME_CHILD] {
            let res = crate::resolve::auto_attach_terminal(&mut conn, &term.id, Some(cwd)).unwrap();
            assert_eq!(
                res.workspace_id, None,
                "cwd {cwd} must not attach an orphan anywhere, least of all to Système"
            );
        }
        assert!(!is_system_terminal(&mut conn, &term.id).unwrap());

        // 3. THE SCOPE'S READS do not show it: the runtime snapshot of the Système workspace is
        //    keyed on a POSITIVE `workspace_id`, so a NULL one can never appear in it.
        struct NoOverlay;
        impl crate::workspace_runtime::RunStateOverlay for NoOverlay {
            fn outcome(&self, _: &str) -> Option<(crate::command::RunState, Option<i32>, bool)> {
                None
            }
        }
        let snap = crate::workspace_runtime::snapshot(&mut conn, &scope.workspace.id, &NoOverlay)
            .unwrap()
            .expect("the Système workspace resolves");
        assert!(
            snap.terminals.is_empty(),
            "the Système scope lists no orphan"
        );

        // 4. THE OTHER destructive verb — `workspace_delete` (`usecase::remove_workspace`) — feeds
        //    the orphan bucket NOTHING: it DELETES the workspace's terminals instead of detaching
        //    them (review 01KWEX11). So the only door onto that bucket stays `delete_project`, and
        //    the Système scope is on neither side of it.
        let (other, other_root) = db::create_project(&mut conn, "other", USER_ROOT, None).unwrap();
        let sub = db::create_workspace(
            &mut conn,
            &other.id,
            "wt",
            &format!("{USER_ROOT}{}wt", std::path::MAIN_SEPARATOR),
        )
        .unwrap();
        assert_ne!(sub.id, other_root.id);
        let sub_term = db::create_terminal(&mut conn, &sub.root_path, None).unwrap();
        db::attach_terminal(&mut conn, &sub_term.id, &sub.id, db::BINDING_AUTO).unwrap();
        crate::usecase::remove_workspace(
            &mut conn,
            &crate::usecase::NullEventSink,
            &sub.id,
            crate::usecase::DomainEventSource::Ui,
        )
        .expect("a non-root user workspace is removable");
        assert!(
            db::get_terminal(&mut conn, &sub_term.id).unwrap().is_none(),
            "workspace_delete DELETES its terminals — it creates no orphan"
        );
        assert_eq!(
            orphans(&mut conn),
            vec![term.id.clone()],
            "the orphan bucket is unchanged: still exactly the delete_project one"
        );
    }

    /// DONE-CRITERION #2/#3 — a Système terminal and a Système command always hang on the
    /// singleton by a POSITIVE attachment, and the two lists are disjoint in BOTH directions: the
    /// scope's list excludes orphans, the orphan list excludes the scope's members.
    #[test]
    fn a_system_member_is_always_positively_attached_and_never_in_the_orphan_bucket() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);

        // A TERMINAL of the scope: manual binding onto the singleton's workspace.
        let sys_term = db::create_terminal(&mut conn, scope.home(), None).unwrap();
        bind_to_system(&mut conn, &sys_term.id, &scope);
        let row = db::get_terminal(&mut conn, &sys_term.id).unwrap().unwrap();
        assert_eq!(
            row.workspace_id.as_deref(),
            Some(scope.workspace.id.as_str()),
            "membership is a real pointer at the singleton's workspace"
        );
        assert_eq!(row.workspace_binding_mode, db::BINDING_MANUAL);
        assert!(is_system_terminal(&mut conn, &sys_term.id).unwrap());

        // A COMMAND of the scope: a managed_command of the singleton, materialized as exactly ONE
        // instance (the singleton has exactly one workspace).
        let template = db::create_template(
            &mut conn,
            &scope.project.id,
            "services",
            "docker compose up",
            None,
            db::CommandSource::default(),
        )
        .unwrap();
        let instances = db::list_instances_for_workspace(&mut conn, &scope.workspace.id).unwrap();
        assert_eq!(instances.len(), 1, "one workspace ⇒ one instance");
        assert_eq!(instances[0].command_id, template.id);
        assert_eq!(instances[0].workspace_id, scope.workspace.id);

        // An ORPHAN alongside, so both directions are non-vacuous.
        let orphan = db::create_terminal(&mut conn, HOME, None).unwrap();
        assert_eq!(orphans(&mut conn), vec![orphan.id.clone()]);
        assert!(
            !orphans(&mut conn).contains(&sys_term.id),
            "the orphan list excludes the Système terminal"
        );
        assert!(!is_system_terminal(&mut conn, &orphan.id).unwrap());
    }

    /// DONE-CRITERION #4/#5 — the separation survives the two events that could blur it: a project
    /// DELETED then RECREATED on the same folder, and a REBOOT.
    ///
    /// The recreated project legitimately re-adopts the orphan whose cwd it now covers — that is
    /// the ordinary user-workspace rule, and it must keep working. What must never happen is the
    /// SYSTÈME scope taking it, or the reboot moving a Système member into the orphan bucket.
    #[test]
    fn a_delete_recreate_and_a_reboot_do_not_move_a_terminal_across_the_line() {
        let mut conn = open_in_memory();
        let scope = bootstrap(&mut conn);
        let (user, user_root) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();

        let sys_term = db::create_terminal(&mut conn, scope.home(), None).unwrap();
        bind_to_system(&mut conn, &sys_term.id, &scope);
        let user_term = db::create_terminal(&mut conn, USER_ROOT, None).unwrap();
        db::attach_terminal(&mut conn, &user_term.id, &user_root.id, db::BINDING_AUTO).unwrap();

        // DELETE: the user terminal falls into the orphan bucket; the Système one does not move.
        db::delete_project(&mut conn, &user.id).unwrap();
        assert_eq!(orphans(&mut conn), vec![user_term.id.clone()]);
        assert!(is_system_terminal(&mut conn, &sys_term.id).unwrap());

        // RECREATE on the SAME folder, then let the resolver run: the orphan goes back to the USER
        // workspace — never to Système, whose home is a strictly shallower ancestor of that cwd.
        let (_user2, user_root2) = db::create_project(&mut conn, "app", USER_ROOT, None).unwrap();
        let re = crate::resolve::auto_attach_terminal(&mut conn, &user_term.id, Some(USER_ROOT))
            .unwrap();
        assert_eq!(re.workspace_id.as_deref(), Some(user_root2.id.as_str()));
        assert!(!is_system_terminal(&mut conn, &user_term.id).unwrap());

        // REBOOT: same singleton, same members, same orphans. Nothing crosses.
        let after_reboot = bootstrap(&mut conn);
        assert_eq!(after_reboot, scope, "the reboot re-reads, it does not rebuild");
        assert!(is_system_terminal(&mut conn, &sys_term.id).unwrap());
        assert!(orphans(&mut conn).is_empty());

        // And the LAST door onto the orphan bucket stays shut for the scope: removing the Système
        // workspace — the one operation that WOULD detach its terminals — is refused outright.
        match crate::usecase::remove_workspace(
            &mut conn,
            &crate::usecase::NullEventSink,
            &scope.workspace.id,
            crate::usecase::DomainEventSource::Ui,
        ) {
            Err(crate::usecase::RemoveWorkspaceError::SystemScope(e)) => assert!(e.is_forbidden()),
            other => panic!("the Système workspace must not be removable, got {other:?}"),
        }
        assert!(is_system_terminal(&mut conn, &sys_term.id).unwrap());
    }

    /// The scope reads as ABSENT until it is bootstrapped — `load` never half-answers.
    #[test]
    fn an_unbootstrapped_database_has_no_scope() {
        let mut conn = open_in_memory();
        assert_eq!(load(&mut conn).unwrap(), None);
        assert_eq!(system_project_id(&mut conn).unwrap(), None);
        assert_eq!(db::system_project(&mut conn).unwrap(), None);
    }
}
