//! `sim` — le simulateur de run sans agent.
//!
//! La commande qu'un développeur tape pour savoir « est-ce que ça marche encore ? » :
//!
//! ```text
//! cargo run -p nyx-core --example sim -- <nom|chemin.json> [--keep] [--provider …] [--list]
//! bun run sim ndev-feature
//! ```
//!
//! Elle ouvre une base jetable par le vrai chemin de boot, met en place le scénario, déroule le
//! workflow avec le pilote, puis imprime — dans cet ordre — un en-tête, la trace tick par tick, le
//! journal des gates, les statuts finaux et le VERDICT.
//!
//! Codes de sortie : `0` verdict OK, `1` verdict KO (les divergences sont listées), `2` erreur de
//! pilotage (scénario introuvable ou invalide, action orpheline, emballement).
//!
//! **Mesurer depuis PowerShell, pas depuis Git Bash** (`CLAUDE.md`) : les sentinelles de gate sont
//! rendues dans le dialecte du shell que `pty::resolve_shell()` désignera vraiment, donc les deux
//! doivent donner le même verdict — mais c'est PowerShell qui fait foi sous Windows.

mod driver;

use std::path::{Path, PathBuf};

use driver::scenario::{replay, Scenario};
use driver::{parse_provider, DriverError, PROVIDERS};

/// Erreur de pilotage.
const EXIT_DRIVE_ERROR: i32 = 2;
/// Verdict KO.
const EXIT_VERDICT_KO: i32 = 1;

/// L'usage, DÉRIVÉ de [`PROVIDERS`] plutôt que recopié : la liste que l'aide annonce et celle que
/// [`parse_provider`] accepte ne peuvent plus diverger, puisqu'il n'y en a qu'une. Ajouter un
/// provider est alors un geste à un seul endroit, comme le veut le reste de l'exemple (les
/// sentinelles s'ancrent sur `resolve_shell`, le slot sur `BUILD_CHECK_SLOT`).
fn usage() -> String {
    format!(
        "usage : sim <nom|chemin.json> [--keep] [--provider {}] [--list]",
        PROVIDERS.join("|")
    )
}

fn main() {
    std::process::exit(match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("\nsim : {err}");
            EXIT_DRIVE_ERROR
        }
    })
}

/// Les scénarios livrés vivent à côté de cette source. `CARGO_MANIFEST_DIR` est figé à la
/// compilation, ce qui est exactement ce qu'il faut pour un exemple : la CLI et le test
/// d'intégration résolvent le même dossier, quel que soit le `cwd`.
fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/sim/scenarios")
}

/// Les noms des scénarios livrés, triés.
fn shipped() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(scenarios_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|e| e.to_str()) == Some("json"))
                .then(|| path.file_stem()?.to_str().map(str::to_string))
                .flatten()
        })
        .collect();
    names.sort();
    names
}

struct Args {
    scenario: Option<String>,
    keep: bool,
    provider: String,
    list: bool,
}

fn parse_args() -> Result<Args, DriverError> {
    let mut args = Args {
        scenario: None,
        keep: false,
        provider: "claude".to_string(),
        list: false,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--list" => args.list = true,
            "--keep" => args.keep = true,
            "--provider" => {
                let value = raw.next().ok_or_else(|| {
                    DriverError::Scenario("`--provider` attend une valeur".to_string())
                })?;
                // Confronté aux trois valeurs que l'usage annonce : le moteur, lui, accepte
                // n'importe quelle chaîne et retombe silencieusement sur `claude`.
                args.provider = parse_provider(&value)?;
            }
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(DriverError::Scenario(format!(
                    "option inconnue `{other}`.\n{}",
                    usage()
                )))
            }
            other => args.scenario = Some(other.to_string()),
        }
    }
    Ok(args)
}

/// Résout un nom nu en `examples/sim/scenarios/<nom>.json` ; un chemin est pris tel quel.
fn resolve(name: &str) -> Result<PathBuf, DriverError> {
    let direct = Path::new(name);
    // `is_file`, pas `exists` : un RÉPERTOIRE du même nom que le scénario dans le cwd (il suffit
    // d'un dossier `ndev-feature/`) détournait la résolution, et la lecture échouait ensuite sur
    // « Access is denied » au lieu de rejouer le scénario livré.
    if direct.is_file() {
        return Ok(direct.to_path_buf());
    }
    if direct.extension().is_some() {
        return Err(DriverError::Scenario(format!(
            "{name} : fichier introuvable"
        )));
    }
    let path = scenarios_dir().join(format!("{name}.json"));
    if path.exists() {
        return Ok(path);
    }
    Err(DriverError::Scenario(format!(
        "scénario `{name}` inconnu. Scénarios livrés : {}",
        shipped().join(", ")
    )))
}

fn run() -> Result<i32, DriverError> {
    let args = parse_args()?;

    if args.list {
        for name in shipped() {
            let description = Scenario::load(&scenarios_dir().join(format!("{name}.json")))
                .map(|s| s.description)
                .unwrap_or_else(|e| format!("<illisible : {e}>"));
            println!("{name:<28} {description}");
        }
        return Ok(0);
    }

    let Some(name) = args.scenario.clone() else {
        return Err(DriverError::Scenario(format!(
            "aucun scénario nommé.\n{}\nScénarios livrés : {}",
            usage(),
            shipped().join(", ")
        )));
    };
    let path = resolve(&name)?;
    let scenario = Scenario::load(&path)?;
    let label = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&name)
        .to_string();

    let done = replay(&scenario, &label, &args.provider, args.keep)?;

    // ---- En-tête -----------------------------------------------------------------------------
    println!("sim — {label}");
    println!("  scénario : {}", scenario.description);
    println!("  scope    : {}", scenario.setup.scope);
    println!("  provider : {}", args.provider);
    println!("  base     : {}", done.base.join("nyx.db").display());

    // ---- Trace -------------------------------------------------------------------------------
    println!("\nTrace");
    for warning in &done.trace.warnings {
        println!("  ! run_start : {warning}");
    }
    for step in &done.trace.surfaced {
        let indent = "  ".repeat(step.depth);
        println!(
            "  #{:<3} {indent}{} [{}]",
            step.tick,
            step.signature(),
            step.node_label
        );
        if let Some(child) = &step.child_run_id {
            println!("        {indent}  -> run enfant {child}");
        }
        if let Some(reply) = &step.reply {
            println!("        {indent}  <- {reply}");
        }
        if let Some(delta) = step.blackboard_delta.as_object() {
            if !delta.is_empty() {
                println!(
                    "        {indent}  delta {}",
                    serde_json::Value::Object(delta.clone())
                );
            }
        }
        // Les RETRAITS, à côté du delta et jamais dedans : une clé listée ici ne tient plus, quoi
        // qu'ait dit un delta antérieur.
        if !step.blackboard_removed.is_empty() {
            println!(
                "        {indent}  retiré {}",
                step.blackboard_removed.join(", ")
            );
        }
        for note in &step.notes {
            println!("        {indent}  note : {note}");
        }
    }
    if let Some(refusal) = &done.trace.refusal {
        println!("  REFUS [{}] {}", refusal.code, refusal.message);
        if let Some(data) = &refusal.data {
            println!("        {data}");
        }
    }

    // ---- Journal des gates -------------------------------------------------------------------
    println!("\nJournal des gates");
    if done.observed.journal.is_empty() {
        println!("  (aucune tentative)");
    }
    for gate in &done.observed.journal {
        // La POLARITÉ d'abord, et le respect de l'attente juste après : sur un gate de
        // reproduction, `failed` EST le résultat attendu, et une ligne qui ne dirait que l'issue
        // se lirait comme un échec.
        // `bloquant` / `signale` : un rouge sur un gate NON bloquant ne refuse rien, et une ligne
        // qui ne dirait pas lequel des deux se lit comme un run arrêté qui ne l'a pas été.
        println!(
            "  {:<14} attendu={:<6} {:<13} {} {:<8} exit={:<4} `{}`  (cwd {})",
            gate.slot.as_deref().unwrap_or("<hors slot>"),
            gate.expect,
            gate.outcome,
            if gate.satisfied { "OK " } else { "KO " },
            if gate.blocking { "bloquant" } else { "signale" },
            gate.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".into()),
            gate.command_line,
            gate.cwd,
        );
    }

    // ---- Statuts -----------------------------------------------------------------------------
    println!("\nStatuts");
    println!("  Spec : {}", done.observed.spec_status);
    if let Some(category) = &done.observed.category {
        // PROVENANCE, et surtout pas « mintée par » : `system` veut dire que le judge a RÉUTILISÉ
        // l'une des catégories du seed — donc précisément qu'il n'a rien minté. Seul `agent` est un
        // mint. Un libellé qui parle de mint pour `system` dit le contraire du format (README,
        // « `category` / `category_created_by` »), et cette trace est ce qu'un auditeur recopie.
        println!(
            "  catégorie : {category} (provenance {})",
            done.observed
                .category_created_by
                .as_deref()
                .unwrap_or("?"),
        );
    }
    for (title, status) in &done.observed.tasks {
        println!("  tâche `{title}` : {status}");
    }

    // ---- Verdict -----------------------------------------------------------------------------
    println!("\nVerdict");
    if done.diffs.is_empty() {
        println!(
            "  OK — {} action(s) surfacée(s), {} tentative(s) de gate.",
            done.observed.surfaced.len(),
            done.observed.journal.len()
        );
    } else {
        for diff in &done.diffs {
            println!("  KO  {diff}");
        }
    }

    if args.keep {
        println!("\nBase conservée : {}", done.base.display());
    }

    Ok(if done.diffs.is_empty() {
        0
    } else {
        EXIT_VERDICT_KO
    })
}
