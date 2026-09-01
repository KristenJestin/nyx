//! Le **ledger d'approbation** du scope Système — une opération, une décision humaine, un effet.
//!
//! ## Le problème
//!
//! Le scope Système ([`crate::system_scope`]) porte les lanceurs transverses de la machine :
//! services, docker, scripts. Un agent MCP peut les DÉCOUVRIR (`list_system_commands`) et démarrer
//! une commande arrêtée. Tout le reste — créer, importer, éditer, supprimer, réordonner, grouper,
//! arrêter un service qui tourne, relancer — modifie ce que la machine de l'utilisateur exécute.
//! Ces gestes-là exigent une main humaine.
//!
//! Mais l'appelant est un agent SANS terminal, et le décideur est un humain devant l'app. Les deux
//! ne partagent ni processus, ni fil d'exécution, ni instant. Il faut donc un objet persistant qui
//! traverse cet écart : c'est ce module.
//!
//! ## Le renversement du protocole (PDR-0030)
//!
//! La première version faisait REJOUER l'agent : le refus lui remettait un jeton opaque dans
//! `error.data`, il attendait la décision, puis il réémettait son appel en portant le jeton. Un
//! agent MCP réel a montré que ce canal N'EXISTE PAS. Rien n'oblige un client MCP à exposer
//! `error.data` au modèle, et le client le plus répandu ne remonte que `error.message` : l'agent
//! recevait un message qui lui DÉSIGNAIT des données qu'il ne lui donnait pas, et 100 % des
//! opérations mutantes du scope étaient inatteignables quelle que soit la décision humaine.
//!
//! Corriger par le bas — inliner le jeton dans le message — aurait contourné le garde-fou de
//! [`crate::mcp::RpcError`], dont le rendu une-ligne est écrit en base. Le protocole a donc été
//! renversé :
//!
//! **L'ACCORD HUMAIN EXÉCUTE L'EFFET.** La décision déclenche l'opération côté serveur, sur la
//! charge canonique que le ledger détient déjà. L'humain approuve ce qu'il voit, le serveur exécute
//! ce qu'il a montré, et il ne reste plus rien de secret à faire voyager jusqu'au modèle.
//!
//! **L'APPEL PROTÉGÉ ATTEND SA RÉPONSE.** Créer une commande, c'est en général pour la lancer dans
//! la foulée : un agent qui repart sans réponse casse son élan. L'appel BLOQUE donc jusqu'à la
//! décision, plafonné par [`APPROVAL_WAIT_CAP_MS`] — strictement sous le timeout de requête des
//! clients MCP, qui n'est pas le nôtre. Décision dans la fenêtre ⇒ l'agent reçoit le VRAI résultat
//! en un aller-retour. Au-delà ⇒ un `pending` explicite, sans secret, qu'il re-sonde
//! ([`crate::mcp::SYSTEM_APPROVAL_STATUS_TOOL`]) : il attend quand même, en plusieurs sauts.
//!
//! **CE QUE L'ON PERD, ASSUMÉ.** La garantie « même appelant » disparaît avec le jeton, et l'effet
//! part sans que l'agent soit présent. Sur un serveur bindé sur la loopback, le modèle de menace
//! « un autre client MCP local vole l'approbation » est mince ; l'échange est jugé favorable. Le
//! PDR-0030 le consigne plutôt que ce commentaire.
//!
//! ## La forme : une demande, pas une permission
//!
//! Une ligne de [`crate::schema::system_command_approvals`] n'autorise pas « l'agent à créer des
//! commandes ». Elle autorise UNE opération, décrite exactement, UNE fois. Quatre propriétés
//! portent tout :
//!
//! 1. **L'exactitude.** La demande fige la charge canonique de l'appel, la révision de la cible et,
//!    pour un groupe, l'ensemble ORDONNÉ de ses membres avec leur révision. C'est cette charge, et
//!    elle seule, que l'accord rejoue : « ce qui a été approuvé » et « ce qui s'exécute » sont la
//!    même chaîne, pas deux dérivations qui se ressemblent.
//! 2. **Le TTL.** Cinq minutes ([`APPROVAL_TTL_MS`]), horloge injectable ([`ApprovalClock`]),
//!    `expires_at` ABSOLU — un redémarrage de l'app ne rafraîchit pas la fenêtre. L'expiration est
//!    PARESSEUSE ([`expire_due`]) : appliquée par la lecture et par l'attente, jamais par un timer
//!    de fond, et par `UPDATE` conditionnel, donc l'événement `expired` d'une ligne est émis
//!    exactement une fois quelle que soit la concurrence.
//! 3. **L'unicité de la demande en attente.** Un index UNIQUE PARTIEL sur `(fingerprint) WHERE
//!    state = 'pending'` interdit deux demandes concurrentes pour la même opération. Le second
//!    appelant reçoit [`RequestOutcome::PendingExisting`] — et attend LA MÊME demande, ce qui est
//!    désormais utile plutôt que frustrant : l'effet lui profitera aussi, puisqu'il part du serveur.
//! 4. **Le one-shot, devenu une propriété de l'accord.** [`grant`] est un `UPDATE … WHERE state =
//!    'pending' AND expires_at > now` qui pose `state`, `decided_at` ET `claimed_at` d'un seul
//!    geste. Un seul de N clics concurrents peut voir la ligne, donc l'effet part exactement une
//!    fois ; et l'état « accordée mais jamais réclamée » — un état mort du protocole précédent,
//!    qu'un rejeu concurrent pouvait doubler — n'est plus constructible.
//!
//! ## Empreinte et charge canonique : deux rôles, pas un
//!
//! [`ProtectedOperation::fingerprint`] est un DIGEST. Il sert à dédupliquer la file d'attente
//! (l'index partiel ci-dessus) et à donner un nom court et stable à « la même demande » dans l'UI.
//! Il n'autorise RIEN : l'exécution part de `canonical_payload`, conservé en clair, jamais du hash.
//!
//! ## La règle de VISIBILITÉ de ce module
//!
//! (Revue 01M15QP3HVYV9Q729ZYTNJB8T2, finding 01M15RGASGXY1MACGEDHDEXSDR.) Un `pub` est une
//! affirmation sur QUI a le droit de dépendre de quelque chose. Ici, deux catégories seulement le
//! portent :
//!
//! 1. **Ce que `nyx-sidecar` appelle vraiment** : [`request`], [`grant_and_execute`], [`deny`],
//!    [`list_pending`], [`SystemClock`], [`ProtectedOperation`], [`SystemApproval`] et son
//!    [`SystemApproval::public_json`], [`SYSTEM_APPROVALS_CHANGED_TOPIC`] — plus les types que
//!    leurs signatures traînent avec elles.
//! 2. **Les VOCABULAIRES DE FIL**, publiés ENTIERS même quand une valeur n'a pas encore de lecteur :
//!    les quatre `state`, les trois `code`, les deux `outcome`. Ce sont des contrats que d'autres
//!    surfaces épellent (le renderer par `public_json`, l'agent par le résultat d'outil), et un
//!    contrat publié à moitié laisse croire que l'ensemble est ouvert alors qu'il est clos.

use std::fmt;

use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use serde_json::{json, Map, Value};

use crate::db::{self, NewDomainEvent};
use crate::schema::system_command_approvals as approvals;
use crate::usecase::{DomainEntity, DomainEventKind, DomainEventSource};

// --- Vocabulary --------------------------------------------------------------

/// The TTL of a request, in milliseconds — FIVE MINUTES, fixed by the PRD.
///
/// Fixed rather than configurable on purpose: a per-operation TTL is a knob whose only use is to
/// widen the window, and a window an agent can widen is not a window. Five minutes is the span in
/// which a human who is *at the machine* answers; anything longer is an unattended authorisation.
///
/// It is ALSO the countdown the toast renders: the UI reads `expires_at` off the row rather than
/// computing a second deadline, so the clock the human watches and the clock the ledger enforces
/// are one clock (review finding 01M1C7DX1T7QZ89H3BCNRKYA0H).
pub const APPROVAL_TTL_MS: i64 = 5 * 60 * 1000;

/// How long a protected call BLOCKS waiting for the human — FORTY-FIVE SECONDS.
///
/// This number is not ours to choose freely: it is bounded by the REQUEST TIMEOUT of the MCP
/// CLIENT, which is on the other side of the wire and out of our control. The reference clients
/// default to sixty seconds, so a sixty-second block sits exactly ON that boundary — the client
/// would abort the call at the same instant we answer it, and the agent would get a transport
/// error instead of the clean `pending` we took care to build. Forty-five leaves fifteen seconds
/// of headroom for scheduling, serialization and a slow effect.
///
/// It is deliberately well INSIDE [`APPROVAL_TTL_MS`]: a caller that waits to the ceiling still has
/// six more windows before its request dies, so "wait again" is always a real option and never a
/// race against the TTL.
pub const APPROVAL_WAIT_CAP_MS: u64 = 45_000;

/// The blocking window in force ON THIS THREAD — [`APPROVAL_WAIT_CAP_MS`] unless
/// [`with_wait_cap`] shortened it.
pub(crate) fn wait_cap_ms() -> u64 {
    WAIT_CAP_OVERRIDE.with(|cell| cell.get()).unwrap_or(APPROVAL_WAIT_CAP_MS)
}

/// TEST SEAM: run `f` with a shorter blocking window on this thread.
///
/// The same kind of seam as [`FixedClock`], and it exists for the same reason: the interesting
/// property here is "what happens when the window closes", and a suite that had to really wait
/// forty-five seconds per case to observe it would be a suite nobody runs. THREAD-LOCAL rather than
/// global so parallel tests cannot shorten each other's window — and so production, which never
/// calls this, cannot be shortened by anything at all.
pub(crate) fn with_wait_cap<T>(cap_ms: u64, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WAIT_CAP_OVERRIDE.with(|cell| cell.set(self.0));
        }
    }
    let previous = WAIT_CAP_OVERRIDE.with(|cell| cell.replace(Some(cap_ms)));
    let _restore = Restore(previous);
    f()
}

thread_local! {
    static WAIT_CAP_OVERRIDE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

// --- The RETENTION BOUNDS: mechanism, not vocabulary -------------------------
//
// `pub(crate)` rather than `pub` (review 01M15QP3HVYV9Q729ZYTNJB8T2, finding
// 01M15RGASGXY1MACGEDHDEXSDR). These three are the ledger's own housekeeping policy: nothing
// outside this crate reads them, and nothing outside it should — a caller that branched on "the
// cap is 200" would be depending on a number this module is free to change.

/// How long a TERMINAL row (`denied` / `expired` / granted-and-completed) is kept before
/// [`prune_terminal`] drops it — 24 hours. The ledger is a protocol artefact, not an audit log:
/// what is worth keeping past the day is already in `domain_events`, which this module writes to on
/// every transition.
pub(crate) const APPROVAL_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

/// The hard cap on TERMINAL rows kept, whatever their age — the second half of the bounded
/// retention. A machine that runs an agent in a loop for an hour would otherwise accumulate
/// thousands of `denied` rows well inside the 24-hour window.
pub(crate) const APPROVAL_TERMINAL_MAX_ROWS: i64 = 200;

/// The most pending requests [`list_pending`] ever returns — the "surface d'attention bornée" of
/// the PRD, enforced at the READ rather than at the render, so no UI can accidentally show an
/// unbounded queue.
pub(crate) const APPROVAL_PENDING_MAX_ROWS: i64 = 20;

/// How often a blocking wait re-reads the ledger. Short enough that a human's click feels
/// immediate, long enough that a forty-five-second wait costs a few hundred cheap reads rather than
/// a busy loop.
pub(crate) const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

// --- The `state` VOCABULARY: published WHOLE ---------------------------------

/// `state` — awaiting a human decision.
pub const STATE_PENDING: &str = "pending";
/// `state` — a human said yes. The effect was authorised at that instant and runs (or has run).
pub const STATE_GRANTED: &str = "granted";
/// `state` — a human said no. Terminal.
pub const STATE_DENIED: &str = "denied";
/// `state` — the TTL ran out before the decision. Terminal.
pub const STATE_EXPIRED: &str = "expired";

/// `outcome` — the authorised effect ran and succeeded.
pub const OUTCOME_SUCCESS: &str = "success";
/// `outcome` — the authorised effect ran and failed. The approval is spent all the same.
pub const OUTCOME_FAILURE: &str = "failure";

// --- The `code` VOCABULARY: published WHOLE, same reason ---------------------

/// The stable code of the answer that CREATES a request and is still waiting on the human when the
/// blocking window closes. Carried in the tool RESULT (not in an error) so no client can drop it.
pub const APPROVAL_REQUIRED_CODE: &str = "approval_required";

/// The stable code of a caller that JOINED an identical request already pending. It waits on the
/// same row and, since the effect now leaves the server, it benefits from the same decision.
pub const APPROVAL_PENDING_EXISTING_CODE: &str = "approval_pending_existing";

/// The stable wire code of a refused lookup or a refused decision: unknown request, wrong state, or
/// expired.
pub const APPROVAL_INVALID_CODE: &str = "approval_invalid";

/// The `changed` topic the UI bridge broadcasts when the pending queue moves. NEVER an MCP
/// surface: the agent learns about its own request through [`status`], and about nobody else's.
pub const SYSTEM_APPROVALS_CHANGED_TOPIC: &str = "system-approvals";

// --- The clock seam ----------------------------------------------------------

/// The clock the TTL is read from — injected rather than called, because every interesting property
/// of this module is a statement about TIME.
///
/// A test must be able to cross the five-minute boundary without sleeping five minutes, and must be
/// able to place a request, a decision and an execution at chosen instants to prove that the expiry
/// is applied at the READ and not by a background timer. `std::time::SystemTime::now()` offers
/// neither.
pub trait ApprovalClock: Send + Sync {
    /// Epoch milliseconds.
    fn now_ms(&self) -> i64;
}

/// The PRODUCTION clock: the same `db::now_millis` every other stamped column in nyx uses, so an
/// approval's timestamps are comparable with a command's `updated_at` without a conversion.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl ApprovalClock for SystemClock {
    fn now_ms(&self) -> i64 {
        db::now_millis()
    }
}

/// A clock pinned to a value a test moves by hand. `Send + Sync` through an atomic rather than a
/// mutex, because the concurrency tests share ONE clock across threads and must not serialize on it
/// (a lock would hide the very interleavings they exist to produce).
#[derive(Debug)]
pub struct FixedClock(std::sync::atomic::AtomicI64);

impl FixedClock {
    /// A clock reading `at`.
    pub fn at(at: i64) -> Self {
        Self(std::sync::atomic::AtomicI64::new(at))
    }

    /// Move the clock FORWARD by `delta_ms` (a negative delta is allowed; nothing in this module
    /// assumes monotonicity, and a clock that jumps back only ever makes a row look younger).
    pub fn advance(&self, delta_ms: i64) {
        self.0
            .fetch_add(delta_ms, std::sync::atomic::Ordering::SeqCst);
    }
}

impl ApprovalClock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// --- The row -----------------------------------------------------------------

/// A `system_command_approvals` row.
///
/// DELIBERATELY NOT `Serialize`. Everything a surface may see goes through
/// [`SystemApproval::public_json`], which names its fields one by one — so a column added tomorrow
/// does not travel by default.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable)]
#[diesel(table_name = approvals)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct SystemApproval {
    pub id: String,
    pub project_id: String,
    /// The MCP tool the frozen charge is dispatched to when the human says yes.
    pub tool_name: String,
    pub operation_kind: String,
    pub canonical_payload: String,
    pub fingerprint: String,
    pub target_revision: Option<String>,
    pub member_snapshot: Option<String>,
    pub state: String,
    pub requested_at: i64,
    pub expires_at: i64,
    pub decided_at: Option<i64>,
    /// Set BY [`grant`], in the same conditional `UPDATE`: the effect was authorised to start,
    /// exactly once.
    pub claimed_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub outcome: Option<String>,
    /// What the executed effect returned, serialized. This is what an agent reads when it comes
    /// back after its blocking window — the answer it did not stay to receive.
    pub result_json: Option<String>,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = approvals)]
struct NewApproval {
    id: String,
    project_id: String,
    tool_name: String,
    operation_kind: String,
    canonical_payload: String,
    fingerprint: String,
    target_revision: Option<String>,
    member_snapshot: Option<String>,
    state: String,
    requested_at: i64,
    expires_at: i64,
    requested_by: Option<String>,
}

/// The longest SUBJECT [`SystemApproval::summary`] will interpolate. The same order of magnitude as
/// `mcp_context::MAX_COMMAND_CHARS`, and for the same reason: a command line is not a label, and the
/// surfaces that render one are bounded by design.
const MAX_SUMMARY_SUBJECT_CHARS: usize = 160;

impl SystemApproval {
    /// What a SURFACE (the UI's pending toast, the agent's `system_approval_status`) may see.
    ///
    /// `payload` is the canonical charge re-parsed into JSON, because it is exactly WHAT the human
    /// is being asked to authorise AND what the server will run — hiding it would make the decision
    /// blind. `summary` is a short human sentence derived from the same data, for the bounded
    /// attention surface. `result` is the effect's own answer once it has run.
    pub fn public_json(&self) -> Value {
        json!({
            "request_id": self.id,
            "tool": self.tool_name,
            "operation_kind": self.operation_kind,
            "fingerprint": self.fingerprint,
            // An UNREADABLE charge is said so, not shown as absent. `payload: null` next to an
            // `operation_kind` that still reads `add_command` looks like an argument-less
            // operation, and this field exists precisely so the decision is not blind: a card that
            // cannot show what it is asking about must say that rather than show nothing.
            "payload": match serde_json::from_str::<Value>(&self.canonical_payload) {
                Ok(v) => v,
                Err(_) => json!({ "unreadable": true }),
            },
            "target_revision": self.target_revision,
            "member_snapshot": self.member_snapshot,
            "state": self.state,
            "requested_at": self.requested_at,
            "expires_at": self.expires_at,
            "decided_at": self.decided_at,
            "claimed_at": self.claimed_at,
            "completed_at": self.completed_at,
            "outcome": self.outcome,
            "result": self.result(),
            "requested_by": self.requested_by,
            "decided_by": self.decided_by,
            "summary": self.summary(),
        })
    }

    /// The effect's stored answer, re-parsed. `None` while the effect has not run (or produced
    /// nothing).
    pub fn result(&self) -> Option<Value> {
        self.result_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
    }

    /// A one-line description of the operation, for the UI's bounded attention surface: the verb
    /// plus the most identifying argument the payload happens to carry.
    pub fn summary(&self) -> String {
        let payload: Value = serde_json::from_str(&self.canonical_payload).unwrap_or(Value::Null);
        let pick = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
        match pick("name")
            .or_else(|| pick("command"))
            .or_else(|| pick("command_id"))
            .or_else(|| pick("instance_id"))
            .or_else(|| pick("group_id"))
        {
            // TRUNCATED, because this line feeds « une surface d'attention bornée » and the field
            // it most often picks is a real shell command line. An unbounded subject pushes the
            // decision buttons off the card — the one thing the card exists for.
            Some(subject) => {
                let subject = if subject.chars().count() > MAX_SUMMARY_SUBJECT_CHARS {
                    let head: String = subject.chars().take(MAX_SUMMARY_SUBJECT_CHARS).collect();
                    format!("{head}…")
                } else {
                    subject
                };
                format!("{} — {subject}", self.operation_kind)
            }
            None => self.operation_kind.clone(),
        }
    }

    /// `true` when this row's TTL has run out at `now`. A pure reading of the row; the TRANSITION
    /// is [`expire_due`]'s job.
    pub(crate) fn is_expired_at(&self, now: i64) -> bool {
        self.expires_at <= now
    }

    /// `true` when there is nothing left to wait for: the human decided AND, if they said yes, the
    /// effect has finished. A `granted` row without `completed_at` is an effect IN FLIGHT — the
    /// caller keeps waiting for the answer rather than reporting a decision without a result.
    pub(crate) fn is_settled(&self) -> bool {
        match self.state.as_str() {
            STATE_PENDING => false,
            STATE_GRANTED => self.completed_at.is_some(),
            _ => true,
        }
    }
}

// --- The operation an approval is bound to -----------------------------------

/// The exact operation a request authorises — the thing the fingerprint is taken OF and the thing
/// the grant EXECUTES.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedOperation {
    /// The Système singleton's project id. Part of the fingerprint so an approval can never be
    /// carried across containers, even if the rest matched.
    pub project_id: String,
    /// The MCP TOOL the charge is dispatched to at the grant (`add_command`, `start_command`…).
    /// Distinct from [`Self::operation_kind`], which the guard matrix may qualify
    /// (`manage_command_group.add`) and which is therefore not dispatchable on its own.
    pub tool: String,
    /// The protected verb, as the guard matrix names it.
    pub operation_kind: String,
    /// The call's arguments, CANONICALIZED ([`canonical_json`]) — sorted keys, compact. This exact
    /// string is what the grant re-dispatches.
    pub canonical_payload: String,
    /// The state of the TARGET at request time, as a digest of its meaningful columns. `None` for
    /// an operation with no pre-existing target (`add_command`, `import_commands`).
    pub target_revision: Option<String>,
    /// For a GROUP or BULK operation: the ordered members and their revisions. `None` otherwise.
    pub member_snapshot: Option<String>,
}

impl ProtectedOperation {
    /// Build an operation from a raw MCP argument object.
    pub fn new(
        project_id: impl Into<String>,
        tool: impl Into<String>,
        operation_kind: impl Into<String>,
        payload: &Value,
        target_revision: Option<String>,
        member_snapshot: Option<String>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            tool: tool.into(),
            operation_kind: operation_kind.into(),
            canonical_payload: canonical_json(&strip_legacy_credential(payload)),
            target_revision,
            member_snapshot,
        }
    }

    /// The digest that NAMES this operation: SHA-1 over an unambiguous join of every binding field.
    ///
    /// The separator is `\u{1}`, which JSON escapes on the way in, so no field value can forge a
    /// boundary — the encoding is injective, and two different operations cannot produce the same
    /// pre-image. Which matters less than it would elsewhere, since the fingerprint's job is dedup
    /// and naming, not authorisation: the grant executes `canonical_payload` in clear, so no digest
    /// stands between an approval and the operation it authorises.
    ///
    /// `tool` is NOT in the digest, and that is not an omission: `operation_kind` is derived from
    /// the tool by the matrix and is strictly finer (`manage_command_group.add` names one tool and
    /// one action), so adding the tool would only duplicate a discriminant already present.
    pub fn fingerprint(&self) -> String {
        let sep = '\u{1}';
        db::hash_content(&format!(
            "system-approval-v1{sep}{}{sep}{}{sep}{}{sep}{}{sep}{}",
            self.project_id,
            self.operation_kind,
            self.canonical_payload,
            self.target_revision.as_deref().unwrap_or(""),
            self.member_snapshot.as_deref().unwrap_or(""),
        ))
    }
}

/// The two argument names the RETIRED replay protocol used. Stripped from the canonical payload so
/// a stale agent — one working from a cached tool description, or from its own memory of the old
/// shape — cannot perturb the fingerprint (nor smuggle leftovers into the charge the human is
/// shown) merely by sending them. There is nothing to send any more: the agent never replays.
const LEGACY_REQUEST_ID_ARG: &str = "approval_request_id";
const LEGACY_TOKEN_ARG: &str = "approval_token";

fn strip_legacy_credential(payload: &Value) -> Value {
    match payload {
        Value::Object(map) if map.contains_key(LEGACY_REQUEST_ID_ARG) || map.contains_key(LEGACY_TOKEN_ARG) => {
            let mut out = map.clone();
            out.remove(LEGACY_REQUEST_ID_ARG);
            out.remove(LEGACY_TOKEN_ARG);
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Serialize `value` DETERMINISTICALLY: object keys sorted, no whitespace, arrays left in order
/// (their order is meaningful — a reorder IS the operation).
///
/// Written here rather than delegating to `serde_json::to_string`, because that function's key
/// order depends on a CARGO FEATURE (`preserve_order` swaps the backing map for an `IndexMap`).
/// A fingerprint whose stability depends on a feature flag somebody may enable for an unrelated
/// reason is not a fingerprint.
pub(crate) fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

// --- Errors ------------------------------------------------------------------

/// Why a REQUEST could not be created, or a decision refused.
#[derive(Debug)]
pub enum ApprovalError {
    /// No request with that id.
    NotFound { request_id: String },
    /// The request is not in the state this transition needs (already decided, already executed).
    WrongState {
        request_id: String,
        state: String,
        wanted: &'static str,
    },
    /// The TTL ran out. Distinct from [`ApprovalError::WrongState`] because the caller's recourse
    /// differs: expired means "ask again", wrong state means "this was already answered".
    Expired { request_id: String },
    /// A SQL failure underneath.
    Db(diesel::result::Error),
}

impl ApprovalError {
    /// The stable, machine-readable code of this refusal.
    pub fn code(&self) -> &'static str {
        match self {
            ApprovalError::Db(_) => "approval_db_error",
            _ => APPROVAL_INVALID_CODE,
        }
    }

    /// A short machine-readable REASON, alongside the code, so an agent can branch without parsing
    /// prose: `unknown` / `wrong_state` / `expired` / `db_error`.
    pub fn reason(&self) -> &'static str {
        match self {
            ApprovalError::NotFound { .. } => "unknown",
            ApprovalError::WrongState { .. } => "wrong_state",
            ApprovalError::Expired { .. } => "expired",
            ApprovalError::Db(_) => "db_error",
        }
    }
}

impl fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApprovalError::NotFound { request_id } => {
                write!(f, "no approval request {request_id}")
            }
            ApprovalError::WrongState {
                request_id,
                state,
                wanted,
            } => write!(
                f,
                "approval request {request_id} is '{state}', not '{wanted}' — a decision is taken \
                 exactly once, so ask for a new approval"
            ),
            ApprovalError::Expired { request_id } => write!(
                f,
                "approval request {request_id} expired (the window is {} seconds); ask again",
                APPROVAL_TTL_MS / 1000
            ),
            ApprovalError::Db(e) => write!(f, "approval ledger database error: {e}"),
        }
    }
}

impl std::error::Error for ApprovalError {}

impl From<diesel::result::Error> for ApprovalError {
    fn from(e: diesel::result::Error) -> Self {
        ApprovalError::Db(e)
    }
}

// --- Creating a request ------------------------------------------------------

/// What [`request`] produced.
#[derive(Debug)]
pub enum RequestOutcome {
    /// THIS caller created the request.
    Created { approval: SystemApproval },
    /// An identical request was ALREADY pending (the unique partial index refused the second
    /// insert). The caller waits on that same row — and, since the effect now leaves the server on
    /// the grant, the decision serves it too.
    PendingExisting { approval: SystemApproval },
}

impl RequestOutcome {
    /// The request either way.
    pub fn approval(&self) -> &SystemApproval {
        match self {
            RequestOutcome::Created { approval } | RequestOutcome::PendingExisting { approval } => {
                approval
            }
        }
    }

    /// `true` when this caller is the one that raised the request.
    pub fn is_new(&self) -> bool {
        matches!(self, RequestOutcome::Created { .. })
    }
}

/// CREATE (or join) the pending request for `op`.
///
/// Runs the lazy expiry first, so a request whose predecessor died of old age is not blocked by the
/// corpse: the partial index only constrains rows that are still `pending`.
///
/// The concurrency is resolved by SQL, not by a lock. Two callers racing on the same fingerprint
/// both attempt the INSERT; the loser takes a UNIQUE violation, its transaction rolls back whole
/// (so no half-written row, and no event), and it re-reads the winner. The re-read is retried ONCE
/// and no more — if the winner was decided or expired in that instant, a fresh attempt is the right
/// answer, not a spin.
pub fn request(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
    op: &ProtectedOperation,
    requested_by: Option<&str>,
) -> Result<RequestOutcome, ApprovalError> {
    expire_due(conn, clock)?;
    match insert_pending(conn, clock, op, requested_by) {
        Ok(outcome) => Ok(outcome),
        Err(ApprovalError::Db(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ))) => {
            let fingerprint = op.fingerprint();
            if let Some(existing) = pending_by_fingerprint(conn, &fingerprint)? {
                return Ok(RequestOutcome::PendingExisting { approval: existing });
            }
            // The winner left `pending` between our failed insert and our read. One more pass; a
            // second UNIQUE violation propagates rather than looping.
            insert_pending(conn, clock, op, requested_by)
        }
        Err(other) => Err(other),
    }
}

/// One INSERT pass — factored out so the concurrent loser's retry is literally the same code path.
fn insert_pending(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
    op: &ProtectedOperation,
    requested_by: Option<&str>,
) -> Result<RequestOutcome, ApprovalError> {
    let now = clock.now_ms();
    let row = NewApproval {
        id: uuid::Uuid::now_v7().to_string(),
        project_id: op.project_id.clone(),
        tool_name: op.tool.clone(),
        operation_kind: op.operation_kind.clone(),
        canonical_payload: op.canonical_payload.clone(),
        fingerprint: op.fingerprint(),
        target_revision: op.target_revision.clone(),
        member_snapshot: op.member_snapshot.clone(),
        state: STATE_PENDING.to_string(),
        requested_at: now,
        expires_at: now + APPROVAL_TTL_MS,
        requested_by: requested_by.map(str::to_string),
    };
    let approval = conn.transaction(|conn| {
        let approval: SystemApproval = diesel::insert_into(approvals::table)
            .values(&row)
            .returning(SystemApproval::as_returning())
            .get_result(conn)?;
        record(conn, &approval, DomainEventKind::SystemApprovalRequested, now)?;
        Ok::<_, diesel::result::Error>(approval)
    })?;
    // The human has to see this card WHILE the caller blocks on it, not when the call returns.
    notify_pending_changed();
    Ok(RequestOutcome::Created { approval })
}

/// The pending row carrying `fingerprint`, if any. At most one can exist — the partial index says
/// so — which is why this returns an `Option` and not a `Vec`.
fn pending_by_fingerprint(
    conn: &mut SqliteConnection,
    fingerprint: &str,
) -> QueryResult<Option<SystemApproval>> {
    approvals::table
        .filter(approvals::fingerprint.eq(fingerprint))
        .filter(approvals::state.eq(STATE_PENDING))
        .select(SystemApproval::as_select())
        .first(conn)
        .optional()
}

// --- Lazy expiry -------------------------------------------------------------

/// Apply the TTL to every row it is due on, and return the rows that JUST transitioned.
///
/// PARESSEUSE, by design: there is no timer thread. Expiry is applied by the reads and by the
/// blocking waits, i.e. exactly at the moments where a stale row could otherwise be acted upon. A
/// ledger nobody looks at needs no expiry pass, and a background sweeper would be a second
/// authority on a state SQL can decide on its own.
///
/// ONE EVENT PER ROW, ALWAYS. The `UPDATE` is conditional on the row still being `pending`, so of N
/// concurrent callers exactly one gets each row back in `RETURNING` and therefore exactly one writes
/// its `system_approval.expired` event. The others see zero rows and emit nothing.
///
/// ONLY `pending` EXPIRES, and that is the protocol change: a `granted` row has already fired its
/// effect (the grant IS the execution), so there is no unclaimed licence left to kill.
pub(crate) fn expire_due(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
) -> QueryResult<Vec<SystemApproval>> {
    let now = clock.now_ms();
    conn.transaction(|conn| {
        let expired: Vec<SystemApproval> = diesel::update(
            approvals::table
                .filter(approvals::state.eq(STATE_PENDING))
                .filter(approvals::expires_at.le(now)),
        )
        .set(approvals::state.eq(STATE_EXPIRED))
        .returning(SystemApproval::as_returning())
        .get_results(conn)?;
        for row in &expired {
            record(conn, row, DomainEventKind::SystemApprovalExpired, now)?;
        }
        Ok(expired)
    })
}

// --- The human's decision ----------------------------------------------------

/// GRANT a pending request — the human said yes, and the effect is now AUTHORISED TO RUN ONCE.
///
/// This transition alone makes the one-shot true: `state`, `decided_at` and `claimed_at` are set by
/// the SAME conditional `UPDATE`, so of N concurrent decisions exactly one sees the row and exactly
/// one caller is handed a grant to execute. It is also why "granted but never claimed" — the dead
/// state of the retired protocol, which a concurrent re-request could silently double — cannot be
/// constructed here.
///
/// Running the effect is [`grant_and_execute`]'s job, not this function's: this one owns the
/// database transition and nothing else, so a caller with no executor (a test, a probe) can still
/// take the decision.
pub fn grant(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
    request_id: &str,
    decided_by: Option<&str>,
) -> Result<SystemApproval, ApprovalError> {
    decide(conn, clock, request_id, STATE_GRANTED, decided_by)
}

/// DENY a pending request — the human said no. Terminal: the agent must ask again, and the same
/// fingerprint is free to be requested afresh (the partial index only constrains `pending`).
pub fn deny(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
    request_id: &str,
    decided_by: Option<&str>,
) -> Result<SystemApproval, ApprovalError> {
    decide(conn, clock, request_id, STATE_DENIED, decided_by)
}

/// The shared body of [`grant`] / [`deny`]: a CONDITIONAL transition out of `pending`.
///
/// `WHERE id = ? AND state = 'pending' AND expires_at > now` is the whole guard. Two humans (two
/// windows, a click and a keyboard shortcut) racing on the same request produce one winner and one
/// `WrongState`; a click that lands a millisecond after the TTL produces `Expired` and NOT a
/// grant — which is why the expiry bound is inside the `WHERE` rather than checked before it.
fn decide(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
    request_id: &str,
    next_state: &str,
    decided_by: Option<&str>,
) -> Result<SystemApproval, ApprovalError> {
    expire_due(conn, clock)?;
    let now = clock.now_ms();
    // A GRANT also stamps `claimed_at`: the effect leaves with the decision, so the moment the
    // human is told "yes" is the moment the ticket is spent.
    let claimed = (next_state == STATE_GRANTED).then_some(now);
    let decided: Option<SystemApproval> = conn.transaction(|conn| {
        let updated: Option<SystemApproval> = diesel::update(
            approvals::table
                .filter(approvals::id.eq(request_id))
                .filter(approvals::state.eq(STATE_PENDING))
                .filter(approvals::expires_at.gt(now)),
        )
        .set((
            approvals::state.eq(next_state),
            approvals::decided_at.eq(Some(now)),
            approvals::claimed_at.eq(claimed),
            approvals::decided_by.eq(decided_by.map(str::to_string)),
        ))
        .returning(SystemApproval::as_returning())
        .get_result(conn)
        .optional()?;
        if let Some(row) = &updated {
            if next_state == STATE_GRANTED {
                record(conn, row, DomainEventKind::SystemApprovalGranted, now)?;
                // Two events for one `UPDATE`, because it performs two things a reader must be able
                // to tell apart afterwards: a human DECIDED, and an effect was AUTHORISED. Written
                // in the same transaction so a crash cannot leave `claimed_at` set with no trace of
                // why.
                record(conn, row, DomainEventKind::SystemApprovalClaimed, now)?;
            } else {
                record(conn, row, DomainEventKind::SystemApprovalDenied, now)?;
            }
        }
        Ok::<_, diesel::result::Error>(updated)
    })?;
    match decided {
        Some(row) => Ok(row),
        None => Err(explain_failure(conn, request_id, now, STATE_PENDING)?),
    }
}

/// Turn a zero-row conditional update into the RIGHT refusal by re-reading the row: unknown id,
/// expired, or simply not in the state the transition needed.
fn explain_failure(
    conn: &mut SqliteConnection,
    request_id: &str,
    now: i64,
    wanted: &'static str,
) -> QueryResult<ApprovalError> {
    let row = get(conn, request_id)?;
    Ok(match row {
        None => ApprovalError::NotFound {
            request_id: request_id.to_string(),
        },
        Some(row) if row.state == STATE_EXPIRED || row.is_expired_at(now) => ApprovalError::Expired {
            request_id: request_id.to_string(),
        },
        Some(row) => ApprovalError::WrongState {
            request_id: request_id.to_string(),
            state: row.state,
            wanted,
        },
    })
}

// --- Reads -------------------------------------------------------------------

/// One row by id, with NO expiry applied — the internal read the guards and the explainers use.
/// [`status`] is the surface read.
pub fn get(conn: &mut SqliteConnection, request_id: &str) -> QueryResult<Option<SystemApproval>> {
    approvals::table
        .find(request_id)
        .select(SystemApproval::as_select())
        .first(conn)
        .optional()
}

/// The BOUNDED pending queue the UI renders: oldest first, DEDUPLICATED BY FINGERPRINT, capped at
/// [`APPROVAL_PENDING_MAX_ROWS`].
///
/// The dedup is defensive rather than corrective — the partial index already forbids two pending
/// rows sharing a fingerprint — and it costs nothing. What it protects is the READ's contract: the
/// human sees one card per distinct operation, which is what makes "approve" a decision about an
/// operation rather than about a queue position.
///
/// Applies the lazy expiry first, so nothing on screen is already dead.
pub fn list_pending(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
) -> QueryResult<Vec<SystemApproval>> {
    expire_due(conn, clock)?;
    prune_terminal(conn, clock)?;
    let rows: Vec<SystemApproval> = approvals::table
        .filter(approvals::state.eq(STATE_PENDING))
        .order((approvals::requested_at.asc(), approvals::id.asc()))
        .limit(APPROVAL_PENDING_MAX_ROWS)
        .select(SystemApproval::as_select())
        .load(conn)?;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    Ok(rows
        .into_iter()
        .filter(|r| seen.insert(r.fingerprint.clone()))
        .collect())
}

/// The AGENT's read of one request: what the human decided and what the effect produced.
///
/// No credential, and that is the protocol (PDR-0030): the agent holds no secret because it never
/// replays, so possession cannot be what opens this read. What replaces it is that there is nothing
/// worth stealing — the read returns a decision and a result, never an authorisation, and the
/// request ids it answers on are v7 UUIDs handed to one caller on a loopback-bound server.
pub fn status(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
    request_id: &str,
) -> Result<SystemApproval, ApprovalError> {
    expire_due(conn, clock)?;
    get(conn, request_id)?.ok_or_else(|| ApprovalError::NotFound {
        request_id: request_id.to_string(),
    })
}

// --- The effect --------------------------------------------------------------

/// JOURNAL what the authorised effect produced, and store its ANSWER.
///
/// Never re-opens the approval: `claimed_at` stays set, so a failed effect has spent its ticket
/// exactly like a successful one. `detail` is a SHORT reason on failure (the effect's error
/// message, truncated) — enough for the human to see why the thing they authorised did not happen,
/// and never a payload or a byte stream.
pub(crate) fn record_result(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
    request_id: &str,
    outcome: &str,
    result: Option<&Value>,
    detail: Option<&str>,
) -> QueryResult<Option<SystemApproval>> {
    let now = clock.now_ms();
    let stamped = match detail {
        Some(d) if !d.is_empty() => {
            let short: String = d.chars().take(200).collect();
            format!("{outcome}: {short}")
        }
        _ => outcome.to_string(),
    };
    let serialized = result.map(|v| v.to_string());
    conn.transaction(|conn| {
        let updated: Option<SystemApproval> = diesel::update(
            approvals::table
                .filter(approvals::id.eq(request_id))
                .filter(approvals::claimed_at.is_not_null())
                .filter(approvals::completed_at.is_null()),
        )
        .set((
            approvals::completed_at.eq(Some(now)),
            approvals::outcome.eq(Some(stamped)),
            approvals::result_json.eq(serialized),
        ))
        .returning(SystemApproval::as_returning())
        .get_result(conn)
        .optional()?;
        if let Some(row) = &updated {
            record(conn, row, DomainEventKind::SystemApprovalCompleted, now)?;
        }
        Ok(updated)
    })
}

/// What the app runs when the human says yes: dispatch `tool` with the frozen canonical `args`, and
/// answer with the tool's own result or a short failure message.
///
/// A seam rather than a call, because the dispatcher lives in the SHELL (it needs the live runner
/// and the terminal host) while the ledger lives here. The shell injects its dispatcher; this module
/// owns the ORDER — decide, then run, then journal — which is the part that must not vary.
pub trait GrantedEffect {
    /// Run the approved operation. `Err` is a SHORT human-readable reason, journalled on the row.
    fn run(&self, tool: &str, args: &Value) -> Result<Value, String>;
}

impl<F> GrantedEffect for F
where
    F: Fn(&str, &Value) -> Result<Value, String>,
{
    fn run(&self, tool: &str, args: &Value) -> Result<Value, String> {
        self(tool, args)
    }
}

/// SUSPEND the approval matrix on this thread, for the duration of `f`.
///
/// The executor re-enters the ordinary tool dispatcher — that is the point: one dispatch table, not
/// a second one that drifts. But the matrix would classify the very operation it just authorised as
/// protected and raise a SECOND request, forever. This marker is what tells it "the human already
/// said yes to this exact charge".
///
/// A THREAD-LOCAL and not an argument, because the alternative is threading a flag through every
/// dispatcher signature in the crate and both shells — a change whose surface is far larger than the
/// property it carries. It is safe for the reason a wire flag would not be: nothing outside this
/// process can set it. It is not readable from any payload, it is restored on unwind (the guard is a
/// `Drop`), and its only caller is [`grant_and_execute`], which sets it around ONE synchronous call
/// on the thread that already holds the granted row.
pub fn with_granted_authority<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            GRANTED_AUTHORITY.with(|cell| cell.set(self.0));
        }
    }
    let previous = GRANTED_AUTHORITY.with(|cell| cell.replace(true));
    let _restore = Restore(previous);
    f()
}

/// `true` while this thread is executing an operation a human has already approved.
pub(crate) fn under_granted_authority() -> bool {
    GRANTED_AUTHORITY.with(|cell| cell.get())
}

thread_local! {
    static GRANTED_AUTHORITY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The seam through which the ledger tells the APP that its pending queue moved.
///
/// It exists because of the blocking call: the request is raised and then the caller waits up to
/// [`APPROVAL_WAIT_CAP_MS`] for an answer, so a notification emitted when that call RETURNS would
/// reach the human a full window after the decision was needed. The push must leave at the instant
/// the row appears, which is here.
///
/// A process-global hook rather than a parameter, for the same reason as
/// [`with_granted_authority`]: the ledger is reached from two dispatchers and two shells, and none
/// of them should have to carry an event sink down to a `request` call. Set once at boot by the
/// shell that has a sink; a shell without one (a test, a bare build) simply never sets it.
static PENDING_NOTIFIER: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> =
    std::sync::OnceLock::new();

/// Register the app's "pending queue moved" push. First call wins; later calls are ignored, so a
/// second boot in the same process cannot install a stale sink over a live one.
pub fn set_pending_notifier(notify: impl Fn() + Send + Sync + 'static) {
    let _ = PENDING_NOTIFIER.set(Box::new(notify));
}

fn notify_pending_changed() {
    if let Some(notify) = PENDING_NOTIFIER.get() {
        notify();
    }
}

/// The HUMAN's yes, whole: take the decision, run the frozen operation, journal what it produced.
///
/// THE THREE STEPS ARE THREE CHECKOUTS, not one. The effect is I/O of unbounded duration (a `stop`
/// waits on a process tree) and it reaches the database itself (every command tool takes its own
/// connection), so holding a pooled connection across it would both starve every other reader and
/// nest a checkout inside a checkout — which on the one-connection in-memory pool the tests run on
/// is a thirty-second block and then a panic. The same rule
/// [`crate::system_scope::create_system_terminal`] follows for the shell it opens.
///
/// A FAILING EFFECT DOES NOT RE-OPEN THE APPROVAL. The decision was taken; re-opening it would let
/// an operation that fails be retried without a second human, which is precisely the licence a
/// one-shot ledger must not hand out. The failure is journalled and the agent is told.
pub fn grant_and_execute(
    db: &db::Db,
    clock: &dyn ApprovalClock,
    request_id: &str,
    decided_by: Option<&str>,
    effect: &dyn GrantedEffect,
) -> Result<SystemApproval, ApprovalError> {
    let granted = db.with_conn(|c| grant(c, clock, request_id, decided_by))?;
    // THE FROZEN CHARGE OR NOTHING. An unparseable `canonical_payload` used to fall back to
    // `Value::Null`, which ran the approved TOOL with a charge nobody ever saw — and ran it under
    // `with_granted_authority`, i.e. with the matrix stood down. The whole property of this
    // protocol is « le serveur exécute exactement ce que l'humain a vu » ; a charge that cannot be
    // read is not that charge. The decision stands (it was taken, and re-opening it would hand out
    // a retry), so the ticket is spent and the failure is journalled like any other.
    let (outcome, result, detail) = match serde_json::from_str::<Value>(&granted.canonical_payload)
    {
        Ok(args) => match effect.run(&granted.tool_name, &args) {
            Ok(value) => (OUTCOME_SUCCESS, Some(value), None),
            Err(reason) => (OUTCOME_FAILURE, None, Some(reason)),
        },
        Err(e) => (
            OUTCOME_FAILURE,
            None,
            Some(format!("stored payload is unreadable, nothing was run: {e}")),
        ),
    };
    let completed = db
        .with_conn(|c| record_result(c, clock, &granted.id, outcome, result.as_ref(), detail.as_deref()))
        .map_err(ApprovalError::Db)?;
    // `None` means the journal write matched no row — the row was pruned between the grant and
    // here, or already carried a `completed_at`. The effect DID run, so this is a state divergence
    // and not a success: say so rather than handing back the pre-effect row as if it were complete.
    match completed {
        Some(row) => Ok(row),
        None => {
            // stderr, like the rest of the crate: `nyx-core` carries no logging facade, and the
            // sidecar's supervisor already surfaces this stream to the dev shell.
            eprintln!(
                "[system-approvals] {}: the effect ran but its outcome could not be journalled \
                 (row gone or already completed) — `system_approval_status` will not carry its result",
                granted.id
            );
            Ok(granted)
        }
    }
}

/// BLOCK until `request_id` is settled — decided, and its effect finished — or until `wait_ms`
/// elapses. Returns the row either way; the caller reads its `state` to know which.
///
/// Bounded by construction: `wait_ms` is clamped to [`APPROVAL_WAIT_CAP_MS`], because the ceiling is
/// not a preference but the MCP client's request timeout. The poll RE-READS through [`status`],
/// which applies the lazy expiry on every pass — so a request that dies of old age mid-wait is
/// reported as `expired` rather than waited on to the ceiling.
///
/// NO CONNECTION IS HELD ACROSS THE WAIT: each pass checks one out and gives it back, so forty-five
/// seconds of waiting costs the pool nothing between two reads.
pub fn wait_for_settlement(
    db: &db::Db,
    clock: &dyn ApprovalClock,
    request_id: &str,
    wait_ms: u64,
) -> Result<SystemApproval, ApprovalError> {
    let wait_ms = wait_ms.min(wait_cap_ms());
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
    loop {
        let row = db.with_conn(|c| status(c, clock, request_id))?;
        let now = std::time::Instant::now();
        if row.is_settled() || now >= deadline {
            return Ok(row);
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    }
}

// --- Bounded retention -------------------------------------------------------

/// Drop TERMINAL rows past their retention: older than [`APPROVAL_RETENTION_MS`], or beyond the
/// [`APPROVAL_TERMINAL_MAX_ROWS`] most recent.
///
/// Only terminal rows (`denied` / `expired`, and `granted` rows whose effect has run) are
/// candidates: a live request is never pruned, whatever the pressure. Called from the READ path, so
/// the bound is maintained by use rather than by a sweeper — the same policy as the expiry.
///
/// What is NOT lost: every transition already wrote a `domain_events` row, which is the durable
/// trace. The ledger holds protocol state, not history.
pub(crate) fn prune_terminal(
    conn: &mut SqliteConnection,
    clock: &dyn ApprovalClock,
) -> QueryResult<usize> {
    let now = clock.now_ms();
    let cutoff = now - APPROVAL_RETENTION_MS;
    let terminal = approvals::state
        .eq_any(vec![STATE_DENIED, STATE_EXPIRED])
        .or(approvals::completed_at.is_not_null());
    let by_age = diesel::delete(
        approvals::table
            .filter(terminal.clone())
            .filter(approvals::requested_at.lt(cutoff)),
    )
    .execute(conn)?;
    // The count bound: keep the newest `APPROVAL_TERMINAL_MAX_ROWS` terminal rows, drop the rest.
    // Two statements rather than one correlated delete, because SQLite has no `DELETE … LIMIT` in
    // the bundled build's default configuration and a sub-select is clearer than the alternative.
    let keep: Vec<String> = approvals::table
        .filter(terminal.clone())
        .order((approvals::requested_at.desc(), approvals::id.desc()))
        .limit(APPROVAL_TERMINAL_MAX_ROWS)
        .select(approvals::id)
        .load(conn)?;
    let by_count = diesel::delete(
        approvals::table
            .filter(terminal)
            .filter(approvals::id.ne_all(keep)),
    )
    .execute(conn)?;
    Ok(by_age + by_count)
}

// --- Domain events -----------------------------------------------------------

/// Persist ONE `domain_events` row for a transition, INSIDE the caller's transaction (the outbox
/// pattern the identity use cases already follow): if the transition rolls back, its event goes
/// with it; if it commits, the event is durably ordered beside it.
///
/// The payload names the request, its operation and its state — never the canonical payload. An
/// event stream is the thing most likely to be shipped somewhere else, so it carries identifiers and
/// a fingerprint, not the contents of the operation.
fn record(
    conn: &mut SqliteConnection,
    approval: &SystemApproval,
    kind: DomainEventKind,
    now: i64,
) -> QueryResult<()> {
    db::record_domain_event(
        conn,
        NewDomainEvent {
            project_id: approval.project_id.clone(),
            workspace_id: None,
            aggregate: DomainEntity::SystemApproval.as_str().to_string(),
            event_type: kind.as_str().to_string(),
            source: DomainEventSource::System.as_str().to_string(),
            actor: approval.decided_by.clone().or(approval.requested_by.clone()),
            payload: json!({
                "identity_id": approval.id,
                "operation_kind": approval.operation_kind,
                "fingerprint": approval.fingerprint,
                "state": approval.state,
            })
            .to_string(),
            created_at: now,
        },
    )?;
    Ok(())
}

// --- What the agent is told --------------------------------------------------

/// The answer to a protected call that is STILL waiting on the human when its blocking window
/// closes — a tool RESULT, not an error.
///
/// A result and not an error, deliberately, and it is the whole lesson of the finding this replaces
/// (01M1C7CWMF4ABHRWT1DGBJTXW3): a tool result is rendered to the model by every client, whereas
/// `error.data` is rendered by none of them reliably. Everything the agent needs to carry on is
/// therefore in a channel it certainly sees — and there is nothing secret in it, so the channel's
/// visibility costs nothing.
///
/// `ok:false` is first because a hurried reader must not mistake "the human has not answered yet"
/// for "it is done".
pub fn awaiting_decision_result(approval: &SystemApproval, joined_existing: bool) -> Value {
    let code = if joined_existing {
        APPROVAL_PENDING_EXISTING_CODE
    } else {
        APPROVAL_REQUIRED_CODE
    };
    json!({
        "ok": false,
        "status": code,
        "request_id": approval.id,
        "operation_kind": approval.operation_kind,
        "fingerprint": approval.fingerprint,
        "expires_at": approval.expires_at,
        "ttl_ms": APPROVAL_TTL_MS,
        "waited_ms": APPROVAL_WAIT_CAP_MS,
        "status_tool": crate::mcp::SYSTEM_APPROVAL_STATUS_TOOL,
        "next": format!(
            "NOTHING HAS HAPPENED YET. The user was asked to approve this operation in the nyx app \
             and has not answered within {}s. Do NOT retry this tool — that would ask them twice. \
             Call {} with request_id=\"{}\" and wait_ms={} to keep waiting; when they approve, nyx \
             runs the operation itself and that call returns its result.",
            APPROVAL_WAIT_CAP_MS / 1000,
            crate::mcp::SYSTEM_APPROVAL_STATUS_TOOL,
            approval.id,
            APPROVAL_WAIT_CAP_MS,
        ),
    })
}

/// The agent-facing projection of a SETTLED (or re-probed) request, for `system_approval_status`.
///
/// Carries `result` — the effect's own answer — because with the execution moved to the grant, that
/// is the only place an agent that timed out can still collect it.
pub fn status_result(approval: &SystemApproval) -> Value {
    let mut out = approval.public_json();
    if let Some(map) = out.as_object_mut() {
        let (ok, next) = match approval.state.as_str() {
            STATE_GRANTED if approval.completed_at.is_some() => (
                // `record_result` stores either the bare outcome or `"{outcome}: {reason}"`, so the
                // prefix test covers both shapes on its own — an equality check beside it would be
                // unreachable.
                approval
                    .outcome
                    .as_deref()
                    .map(|o| !o.starts_with(OUTCOME_FAILURE))
                    .unwrap_or(true),
                "The user approved it and nyx ran the operation. `result` is what it returned."
                    .to_string(),
            ),
            STATE_GRANTED => (
                false,
                "The user approved it and nyx is running the operation now. Call this tool again \
                 with wait_ms to collect the result."
                    .to_string(),
            ),
            STATE_DENIED => (
                false,
                "The user REFUSED this operation. Do not re-raise it: ask them what they want \
                 instead."
                    .to_string(),
            ),
            STATE_EXPIRED => (
                false,
                "The user did not answer within the window and the request died. Nothing ran. \
                 Re-issue the original call if it is still what you want."
                    .to_string(),
            ),
            _ => (
                false,
                format!(
                    "Still waiting on the user. Call this tool again with wait_ms={} to keep \
                     waiting.",
                    APPROVAL_WAIT_CAP_MS
                ),
            ),
        };
        map.insert("ok".to_string(), json!(ok));
        map.insert("next".to_string(), json!(next));
    }
    out
}

/// The `error.data` bag of a refusal that names a request — kept as a STRUCTURED echo for clients
/// that do surface `data`, never as the only copy of anything.
pub fn approval_error_data(approval: &SystemApproval) -> Value {
    let mut bag: Map<String, Value> = Map::new();
    bag.insert("code".to_string(), json!(APPROVAL_INVALID_CODE));
    bag.insert("request_id".to_string(), json!(approval.id));
    bag.insert("state".to_string(), json!(approval.state));
    bag.insert("operation_kind".to_string(), json!(approval.operation_kind));
    Value::Object(bag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    /// A migrated, BOOTSTRAPPED database (`Db::in_memory` migrates but does not bootstrap the
    /// Système singleton — see its doc), plus the singleton's project id.
    fn bootstrapped() -> (Db, String) {
        let db = Db::in_memory();
        let project_id = db.with_conn(|c| {
            let scope = crate::system_scope::ensure_system_scope(
                c,
                &crate::system_scope::FixedHomeDirProvider::at(if cfg!(windows) {
                    "C:\\Users\\test"
                } else {
                    "/home/test"
                }),
            )
            .expect("bootstrap");
            scope.project.id
        });
        (db, project_id)
    }

    fn op(project_id: &str, kind: &str, payload: Value) -> ProtectedOperation {
        ProtectedOperation::new(project_id, kind, kind, &payload, None, None)
    }

    /// An executor that records what it was asked to run and answers `ok`.
    struct Recorder(std::sync::Mutex<Vec<(String, Value)>>);

    impl Recorder {
        fn new() -> Self {
            Self(std::sync::Mutex::new(Vec::new()))
        }
        fn calls(&self) -> Vec<(String, Value)> {
            self.0.lock().unwrap().clone()
        }
    }

    impl GrantedEffect for Recorder {
        fn run(&self, tool: &str, args: &Value) -> Result<Value, String> {
            self.0
                .lock()
                .unwrap()
                .push((tool.to_string(), args.clone()));
            Ok(json!({ "ran": tool }))
        }
    }

    // --- canonical payload + fingerprint ------------------------------------

    #[test]
    fn the_canonical_payload_does_not_depend_on_key_order() {
        let a = json!({ "b": 1, "a": [3, 2, 1], "c": { "z": true, "y": null } });
        let b = json!({ "c": { "y": null, "z": true }, "a": [3, 2, 1], "b": 1 });
        assert_eq!(canonical_json(&a), canonical_json(&b));
        // …and the ARRAY order is preserved, because a reorder IS an operation.
        let reordered = json!({ "a": [1, 2, 3], "b": 1, "c": { "y": null, "z": true } });
        assert_ne!(canonical_json(&a), canonical_json(&reordered));
    }

    #[test]
    fn a_stale_agents_retired_credential_arguments_are_stripped_from_the_charge() {
        // The replay protocol is gone, but an agent working from a cached tool description may
        // still send its two argument names. They must not enter the charge the human is shown, nor
        // move the fingerprint — otherwise the same operation would raise two different requests
        // depending on who asked.
        let clean = json!({ "project_id": "p", "name": "svc" });
        let stale = json!({
            "project_id": "p",
            "name": "svc",
            LEGACY_REQUEST_ID_ARG: "req-1",
            LEGACY_TOKEN_ARG: "tok-1",
        });
        let a = ProtectedOperation::new("p", "add_command", "add_command", &clean, None, None);
        let b = ProtectedOperation::new("p", "add_command", "add_command", &stale, None, None);
        assert_eq!(a.canonical_payload, b.canonical_payload);
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert!(!b.canonical_payload.contains("tok-1"));
    }

    #[test]
    fn the_fingerprint_binds_the_revision_and_the_ordered_members() {
        let payload = json!({ "group_id": "g" });
        let bind = |rev: Option<&str>, members: Option<&str>| {
            ProtectedOperation::new(
                "p",
                "manage_command_group",
                "manage_command_group.add",
                &payload,
                rev.map(str::to_string),
                members.map(str::to_string),
            )
            .fingerprint()
        };
        let all = [
            bind(None, None),
            bind(Some("rev-1"), None),
            bind(Some("rev-2"), None),
            bind(Some("rev-1"), Some("a,b")),
            bind(Some("rev-1"), Some("b,a")),
        ];
        let unique: std::collections::HashSet<&String> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "each binding field moves the digest");
    }

    // --- NO SECRET ANYWHERE --------------------------------------------------

    #[test]
    fn nothing_the_agent_is_handed_is_a_secret_and_the_ledger_holds_no_credential() {
        // The finding this protocol replaces (01M1C7CWMF4ABHRWT1DGBJTXW3) was that a token had to
        // reach the model through a channel no client guarantees. The structural answer is that
        // there is no token: neither the row, nor the public projection, nor the answer the agent
        // receives carries anything that must stay hidden.
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let raised = db.with_conn(|c| request(c, &clock, &operation, None).unwrap());
        let approval = raised.approval();

        let waiting = awaiting_decision_result(approval, !raised.is_new());
        // Everything an agent needs to carry on is a plain identifier and prose.
        assert_eq!(waiting["request_id"], json!(approval.id));
        assert_eq!(waiting["ok"], json!(false));
        assert_eq!(waiting["status"], json!(APPROVAL_REQUIRED_CODE));
        assert!(waiting["next"].as_str().unwrap().contains(&approval.id));
        // No key of the retired protocol survives anywhere on the surface.
        let surfaces = format!(
            "{waiting}{}{}",
            approval.public_json(),
            status_result(approval)
        );
        for forbidden in ["token", "approval_token", "replay"] {
            assert!(
                !surfaces.contains(forbidden),
                "'{forbidden}' must not appear on any agent-facing surface: {surfaces}"
            );
        }
    }

    // --- TTL + lazy expiry ---------------------------------------------------

    #[test]
    fn the_ttl_is_five_minutes_and_expiry_is_lazy_and_emitted_once() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(10_000);
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let approval = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();
        assert_eq!(approval.expires_at, 10_000 + APPROVAL_TTL_MS);
        assert_eq!(APPROVAL_TTL_MS, 5 * 60 * 1000);

        // One millisecond before the boundary: still pending, and NO background timer moved it.
        clock.advance(APPROVAL_TTL_MS - 1);
        let still = db.with_conn(|c| status(c, &clock, &approval.id).unwrap());
        assert_eq!(still.state, STATE_PENDING);

        clock.advance(1);
        let dead = db.with_conn(|c| status(c, &clock, &approval.id).unwrap());
        assert_eq!(dead.state, STATE_EXPIRED);
        // A second pass transitions nothing and emits nothing.
        let again = db.with_conn(|c| expire_due(c, &clock).unwrap());
        assert!(again.is_empty());
        let expired_events = db.with_conn(|c| {
            db::list_domain_events(c)
                .unwrap()
                .into_iter()
                .filter(|e| e.event_type == DomainEventKind::SystemApprovalExpired.as_str())
                .count()
        });
        assert_eq!(expired_events, 1);
    }

    #[test]
    fn expiry_survives_a_reboot_because_the_window_is_absolute() {
        let dir =
            std::env::temp_dir().join(format!("nyx-approvals-reboot-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nyx.db");
        let id = {
            let db = Db::open(&path).expect("open");
            let project_id = db
                .with_conn(crate::system_scope::load)
                .unwrap()
                .expect("bootstrapped")
                .project
                .id;
            let clock = FixedClock::at(1_000);
            let operation = op(&project_id, "add_command", json!({ "name": "a" }));
            db.with_conn(|c| request(c, &clock, &operation, None).unwrap())
                .approval()
                .id
                .clone()
        };
        // A NEW process, a clock well past the window: the row is dead, not refreshed.
        let db = Db::open(&path).expect("reopen");
        let later = FixedClock::at(1_000 + APPROVAL_TTL_MS + 1);
        let row = db.with_conn(|c| status(c, &later, &id).unwrap());
        assert_eq!(row.state, STATE_EXPIRED);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- the pending-request uniqueness --------------------------------------

    #[test]
    fn a_second_request_for_the_same_operation_joins_the_first() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let first = db.with_conn(|c| request(c, &clock, &operation, None).unwrap());
        assert!(first.is_new());
        let second = db.with_conn(|c| request(c, &clock, &operation, None).unwrap());
        assert!(!second.is_new(), "the partial index admits one pending row");
        assert_eq!(first.approval().id, second.approval().id);
        let count: i64 = db.with_conn(|c| approvals::table.count().get_result(c).unwrap());
        assert_eq!(count, 1);
    }

    #[test]
    fn a_denied_request_frees_the_fingerprint_for_a_fresh_one() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let first = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();
        db.with_conn(|c| deny(c, &clock, &first.id, Some("ui")).unwrap());
        let second = db.with_conn(|c| request(c, &clock, &operation, None).unwrap());
        assert!(second.is_new());
        assert_ne!(second.approval().id, first.id);
    }

    // --- the decision --------------------------------------------------------

    #[test]
    fn a_grant_stamps_the_claim_so_a_granted_unclaimed_row_is_not_constructible() {
        // The dead state of the retired protocol: an approval a human said yes to and nobody ever
        // used. It let a re-request create a SECOND pending row while the first slept, granted.
        // With the effect leaving at the decision, `claimed_at` is set by the very same UPDATE.
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let approval = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();
        let granted = db.with_conn(|c| grant(c, &clock, &approval.id, Some("ui")).unwrap());
        assert_eq!(granted.state, STATE_GRANTED);
        assert_eq!(granted.claimed_at, Some(1_000));
        assert_eq!(granted.decided_at, Some(1_000));

        // …and no row in the table can be in that state, whatever the path taken.
        let orphans: i64 = db.with_conn(|c| {
            approvals::table
                .filter(approvals::state.eq(STATE_GRANTED))
                .filter(approvals::claimed_at.is_null())
                .count()
                .get_result(c)
                .unwrap()
        });
        assert_eq!(orphans, 0);
    }

    #[test]
    fn grant_and_deny_are_conditional_transitions_that_only_fire_once() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "stop_command", json!({ "instance_id": "i" }));
        let approval = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();
        db.with_conn(|c| grant(c, &clock, &approval.id, Some("ui")).unwrap());
        let second = db.with_conn(|c| grant(c, &clock, &approval.id, Some("ui")));
        assert!(matches!(second, Err(ApprovalError::WrongState { .. })));
        let denied = db.with_conn(|c| deny(c, &clock, &approval.id, Some("ui")));
        assert!(matches!(denied, Err(ApprovalError::WrongState { .. })));
    }

    #[test]
    fn a_decision_after_the_window_is_refused_rather_than_applied() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let approval = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();
        clock.advance(APPROVAL_TTL_MS);
        let late = db.with_conn(|c| grant(c, &clock, &approval.id, Some("ui")));
        assert!(matches!(late, Err(ApprovalError::Expired { .. })));
        let row = db.with_conn(|c| get(c, &approval.id).unwrap()).unwrap();
        assert_eq!(row.state, STATE_EXPIRED);
        assert!(row.claimed_at.is_none(), "an expired request runs nothing");
    }

    #[test]
    fn an_unknown_request_is_not_found_rather_than_answered() {
        let (db, _project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let missing = db.with_conn(|c| status(c, &clock, "no-such-request"));
        assert!(matches!(missing, Err(ApprovalError::NotFound { .. })));
        assert_eq!(missing.unwrap_err().reason(), "unknown");
    }

    // --- the effect ----------------------------------------------------------

    #[test]
    fn the_grant_executes_the_frozen_charge_and_stores_what_it_returned() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        // The charge the human is shown, with its keys in a deliberately awkward order.
        let payload = json!({ "name": "svc", "command": "docker compose up", "project_id": project_id });
        let operation = ProtectedOperation::new(
            &project_id,
            "add_command",
            "add_command",
            &payload,
            None,
            None,
        );
        let approval = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();

        let recorder = Recorder::new();
        let completed =
            grant_and_execute(&db, &clock, &approval.id, Some("ui"), &recorder).unwrap();

        // The executor was handed the TOOL and the EXACT frozen charge — not a re-derivation.
        let calls = recorder.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "add_command");
        assert_eq!(
            canonical_json(&calls[0].1),
            approval.canonical_payload,
            "the human approves what they see; the server runs what it showed"
        );
        // …and the answer is on the row, for an agent that comes back later.
        assert_eq!(completed.state, STATE_GRANTED);
        assert_eq!(completed.outcome.as_deref(), Some(OUTCOME_SUCCESS));
        assert_eq!(completed.result(), Some(json!({ "ran": "add_command" })));
        assert!(completed.completed_at.is_some());
        assert_eq!(status_result(&completed)["ok"], json!(true));
    }

    #[test]
    fn a_failing_effect_is_journalled_without_re_opening_the_approval() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "relaunch_command", json!({ "instance_id": "i" }));
        let approval = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();
        let boom = |_: &str, _: &Value| Err("the runtime is not started".to_string());
        let row = grant_and_execute(&db, &clock, &approval.id, Some("ui"), &boom).unwrap();
        assert_eq!(row.state, STATE_GRANTED);
        assert!(row.outcome.as_deref().unwrap().starts_with(OUTCOME_FAILURE));
        assert!(row
            .outcome
            .as_deref()
            .unwrap()
            .contains("the runtime is not started"));
        assert!(row.claimed_at.is_some(), "the ticket is spent all the same");
        assert_eq!(status_result(&row)["ok"], json!(false));
        // A second decision on the same row is refused: a failure is not a retry licence.
        let again = grant_and_execute(&db, &clock, &approval.id, Some("ui"), &boom);
        assert!(matches!(again, Err(ApprovalError::WrongState { .. })));
    }

    #[test]
    fn the_outcome_journal_never_receives_a_payload_or_an_unbounded_blob() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let approval = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();
        let long = "x".repeat(5_000);
        let noisy = move |_: &str, _: &Value| Err(long.clone());
        let row = grant_and_execute(&db, &clock, &approval.id, Some("ui"), &noisy).unwrap();
        let outcome = row.outcome.as_deref().unwrap();
        assert!(
            outcome.chars().count() <= OUTCOME_FAILURE.len() + 2 + 200,
            "the journalled reason is truncated, not a byte stream: {} chars",
            outcome.chars().count()
        );
    }

    // --- waiting -------------------------------------------------------------

    #[test]
    fn the_blocking_wait_returns_as_soon_as_the_effect_has_run() {
        let (db, project_id) = bootstrapped();
        let db = std::sync::Arc::new(db);
        let clock = std::sync::Arc::new(FixedClock::at(1_000));
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let approval = db
            .with_conn(|c| request(c, &*clock, &operation, None).unwrap())
            .approval()
            .clone();

        // The "human" clicks after a beat, from another thread — exactly the shape the UI has.
        let decider = {
            let db = db.clone();
            let clock = clock.clone();
            let id = approval.id.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                grant_and_execute(&db, &*clock, &id, Some("ui"), &Recorder::new()).unwrap();
            })
        };
        let started = std::time::Instant::now();
        let settled = wait_for_settlement(&db, &*clock, &approval.id, 10_000).unwrap();
        let waited = started.elapsed();
        decider.join().unwrap();

        assert_eq!(settled.state, STATE_GRANTED);
        assert!(settled.completed_at.is_some(), "it waits for the RESULT, not just the decision");
        assert!(
            waited < std::time::Duration::from_secs(5),
            "the wait ends on the decision, not on the ceiling (took {waited:?})"
        );
    }

    #[test]
    fn the_blocking_wait_is_capped_and_answers_pending_without_a_decision() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let approval = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();
        // A short wait nobody answers: the row comes back pending, and the agent is handed an
        // actionable, secret-free instruction rather than an error it may not see.
        let started = std::time::Instant::now();
        let row = wait_for_settlement(&db, &clock, &approval.id, 300).unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(250));
        assert_eq!(row.state, STATE_PENDING);
        let answer = awaiting_decision_result(&row, false);
        assert_eq!(answer["request_id"], json!(approval.id));
        assert!(answer["next"].as_str().unwrap().contains("system_approval_status"));
    }

    #[test]
    fn the_wait_ceiling_stays_under_the_mcp_client_request_timeout() {
        // Not a preference: sixty seconds is the reference clients' default request timeout, so a
        // block that reached it would be aborted at the very instant it answered.
        assert!(
            APPROVAL_WAIT_CAP_MS < 60_000,
            "the block must finish before the client gives up"
        );
        // …and it must stay well inside the human's window, so "wait again" is always available.
        assert!((APPROVAL_WAIT_CAP_MS as i64) * 4 < APPROVAL_TTL_MS);
    }

    // --- events + retention ---------------------------------------------------

    #[test]
    fn every_transition_is_journalled_in_domain_events() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let approval = db
            .with_conn(|c| request(c, &clock, &operation, None).unwrap())
            .approval()
            .clone();
        grant_and_execute(&db, &clock, &approval.id, Some("ui"), &Recorder::new()).unwrap();
        let kinds: Vec<String> = db.with_conn(|c| {
            db::list_domain_events(c)
                .unwrap()
                .into_iter()
                .filter(|e| e.aggregate == DomainEntity::SystemApproval.as_str())
                .map(|e| e.event_type)
                .collect()
        });
        for expected in [
            DomainEventKind::SystemApprovalRequested,
            DomainEventKind::SystemApprovalGranted,
            DomainEventKind::SystemApprovalClaimed,
            DomainEventKind::SystemApprovalCompleted,
        ] {
            assert!(
                kinds.iter().any(|k| k == expected.as_str()),
                "{} must be journalled, got {kinds:?}",
                expected.as_str()
            );
        }
        // The event payloads name the request and its operation — never the charge itself.
        let payloads: String = db.with_conn(|c| {
            db::list_domain_events(c)
                .unwrap()
                .into_iter()
                .map(|e| e.payload)
                .collect()
        });
        assert!(!payloads.contains("\"name\":\"a\""));
    }

    #[test]
    fn the_pending_list_is_ordered_deduplicated_and_bounded() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        for i in 0..(APPROVAL_PENDING_MAX_ROWS + 5) {
            let operation = op(&project_id, "add_command", json!({ "name": format!("c{i}") }));
            db.with_conn(|c| request(c, &clock, &operation, None).unwrap());
            clock.advance(1);
        }
        let pending = db.with_conn(|c| list_pending(c, &clock).unwrap());
        assert_eq!(pending.len() as i64, APPROVAL_PENDING_MAX_ROWS);
        // Oldest first…
        let mut sorted = pending.clone();
        sorted.sort_by_key(|r| r.requested_at);
        assert_eq!(pending, sorted);
        // …and one card per distinct operation.
        let prints: std::collections::HashSet<&String> =
            pending.iter().map(|r| &r.fingerprint).collect();
        assert_eq!(prints.len(), pending.len());
    }

    #[test]
    fn terminal_rows_are_pruned_by_age_while_live_ones_are_never_touched() {
        let (db, project_id) = bootstrapped();
        let clock = FixedClock::at(1_000);
        let old = op(&project_id, "add_command", json!({ "name": "old" }));
        let old_id = db
            .with_conn(|c| request(c, &clock, &old, None).unwrap())
            .approval()
            .id
            .clone();
        db.with_conn(|c| deny(c, &clock, &old_id, Some("ui")).unwrap());

        clock.advance(APPROVAL_RETENTION_MS + 1);
        let live = op(&project_id, "add_command", json!({ "name": "live" }));
        let live_id = db
            .with_conn(|c| request(c, &clock, &live, None).unwrap())
            .approval()
            .id
            .clone();

        db.with_conn(|c| prune_terminal(c, &clock).unwrap());
        assert!(db.with_conn(|c| get(c, &old_id).unwrap()).is_none());
        assert!(db.with_conn(|c| get(c, &live_id).unwrap()).is_some());
    }

    // --- CONCURRENCY --------------------------------------------------------

    /// A FILE-backed database, because the concurrency tests need several connections at once and
    /// `Db::in_memory` is a one-connection pool by construction (see its doc). Modelled on
    /// `system_scope::tests::concurrent_bootstraps_converge_on_one_singleton`.
    fn on_disk(name: &str) -> (std::path::PathBuf, Db, String) {
        let dir = std::env::temp_dir().join(format!("nyx-approvals-{name}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nyx.db");
        let db = Db::open(&path).expect("open");
        let project_id = db
            .with_conn(crate::system_scope::load)
            .unwrap()
            .expect("Db::open bootstraps the singleton")
            .project
            .id;
        (dir, db, project_id)
    }

    #[test]
    fn concurrent_requests_for_one_operation_produce_one_row() {
        let (dir, db, project_id) = on_disk("request");
        let db = std::sync::Arc::new(db);
        let clock = std::sync::Arc::new(FixedClock::at(1_000));
        let operation = std::sync::Arc::new(op(
            &project_id,
            "add_command",
            json!({ "name": "contended" }),
        ));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let db = db.clone();
                let clock = clock.clone();
                let operation = operation.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    db.with_conn(|c| match request(c, &*clock, &operation, None) {
                        Ok(outcome) => (outcome.approval().id.clone(), outcome.is_new()),
                        Err(e) => panic!("request failed: {e}"),
                    })
                })
            })
            .collect();
        let results: Vec<(String, bool)> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // ONE row, so all four agree on the id…
        let ids: std::collections::HashSet<&String> = results.iter().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 1, "the partial index admits one pending row");
        let count: i64 = db.with_conn(|c| approvals::table.count().get_result(c).unwrap());
        assert_eq!(count, 1);
        // …and exactly one of them RAISED it; the others joined.
        assert_eq!(results.iter().filter(|(_, new)| *new).count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_grants_execute_the_effect_exactly_once() {
        // Six windows, six clicks, one effect. The one-shot is the grant's own `WHERE`, not a lock.
        let (dir, db, project_id) = on_disk("grant");
        let db = std::sync::Arc::new(db);
        let clock = std::sync::Arc::new(FixedClock::at(1_000));
        let operation = op(&project_id, "remove_command", json!({ "command_id": "c" }));
        let approval = db
            .with_conn(|c| request(c, &*clock, &operation, None).unwrap())
            .approval()
            .clone();
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));

        let handles: Vec<_> = (0..6)
            .map(|_| {
                let db = db.clone();
                let clock = clock.clone();
                let id = approval.id.clone();
                let runs = runs.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let effect = |_: &str, _: &Value| {
                        runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(json!({ "done": true }))
                    };
                    grant_and_execute(&db, &*clock, &id, Some("ui"), &effect).is_ok()
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(winners, 1, "one request, one decision");
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the authorised effect runs exactly once under contention"
        );
        let claimed_events = db.with_conn(|c| {
            db::list_domain_events(c)
                .unwrap()
                .into_iter()
                .filter(|e| e.event_type == DomainEventKind::SystemApprovalClaimed.as_str())
                .count()
        });
        assert_eq!(claimed_events, 1, "one authorisation, one event");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_decisions_on_one_request_let_exactly_one_through() {
        let (dir, db, project_id) = on_disk("decide");
        let db = std::sync::Arc::new(db);
        let clock = std::sync::Arc::new(FixedClock::at(1_000));
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        let approval = db
            .with_conn(|c| request(c, &*clock, &operation, None).unwrap())
            .approval()
            .clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));

        let handles: Vec<_> = (0..6)
            .map(|i| {
                let db = db.clone();
                let clock = clock.clone();
                let id = approval.id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    // Three windows click "approve", three click "refuse": whatever the
                    // interleaving, exactly one decision may land.
                    db.with_conn(|c| {
                        if i % 2 == 0 {
                            grant(c, &*clock, &id, Some("ui")).is_ok()
                        } else {
                            deny(c, &*clock, &id, Some("ui")).is_ok()
                        }
                    })
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(winners, 1, "one request, one decision");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_expiries_emit_the_event_once() {
        let (dir, db, project_id) = on_disk("expire");
        let db = std::sync::Arc::new(db);
        let clock = std::sync::Arc::new(FixedClock::at(1_000));
        let operation = op(&project_id, "add_command", json!({ "name": "a" }));
        db.with_conn(|c| request(c, &*clock, &operation, None).unwrap());
        clock.advance(APPROVAL_TTL_MS);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(5));

        let handles: Vec<_> = (0..5)
            .map(|_| {
                let db = db.clone();
                let clock = clock.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    db.with_conn(|c| expire_due(c, &*clock).unwrap().len())
                })
            })
            .collect();
        let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total, 1, "exactly one pass transitions the row");
        let expired_events = db.with_conn(|c| {
            db::list_domain_events(c)
                .unwrap()
                .into_iter()
                .filter(|e| e.event_type == DomainEventKind::SystemApprovalExpired.as_str())
                .count()
        });
        assert_eq!(expired_events, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
