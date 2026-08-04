//! REAL process-liveness of an agent (claude/codex/opencode) in a terminal's PTY.
//!
//! This is the RELIABLE signal the lazy-resume gate was missing (PRD
//! `01KY9PM1GK30FPVZVWQ3A7ZMJH`). The runtime `agent_activity` snapshot is a PROXY that
//! goes STALE: an `Idle` entry means EITHER a live agent waiting at its prompt between two
//! turns OR a crashed/exited agent whose last hook was a `TurnFinished` — the two are
//! indistinguishable from the activity alone. Injecting a `claude --resume <id>` into the
//! FIRST case types the resume line straight into a running agent (the dogfood bug); the
//! SECOND case is exactly when a resume SHOULD fire. The only way to tell them apart is to
//! look at whether an agent PROCESS is actually alive in the terminal's PTY — which the
//! sidecar can do because it OWNS the PTY (it knows the shell pid).
//!
//! ## Cross-platform — a PPID tree walk over [`sysinfo`], NOT `/proc`
//!
//! A terminal hosts a live agent when the process tree rooted at its shell pid contains a
//! process whose name an agent adapter recognizes (`claude`, and — as the registry grows —
//! `codex`/`opencode`). The descendant set is discovered by walking PARENT pids (PPID): the
//! ONE portable signal Linux/macOS/Windows all expose. We deliberately avoid `/proc`
//! session ids or process-group leaders (Linux-only) so the gate holds on every platform —
//! same choice, and the same [`sysinfo`] abstraction, as [`crate::proc_stats`].
//!
//! Why the whole TREE and not just the foreground `comm`: the foreground process-group
//! leader is Linux-only (`tcgetpgrp`) AND it is the WRONG process whenever the agent has a
//! child in the foreground (a `claude` running a bash tool hands the terminal to that child,
//! so the foreground `comm` is `bash`, not `claude`). Walking the tree finds the agent
//! wherever it sits under the shell.
//!
//! ## Point-in-time, no persistent state
//!
//! Unlike [`crate::proc_stats::ProcStats`] (which keeps a live `System` across polls so CPU%
//! deltas stay meaningful), liveness only needs a fresh snapshot of the PPID graph + process
//! names — so each probe refreshes a throwaway `System`. The gate is consulted rarely (once
//! per parked resume / per PTY spawn at project entry), so a per-call `/proc` scan is cheap
//! enough and keeps the API stateless.
//!
//! The tree walk + name match is split into the PURE [`tree_hosts_agent`] over a
//! `pid → ProcNameNode` map, so the descendant logic is unit-tested (Unix AND the Windows
//! `.exe` name form) without a live `sysinfo::System`.

use std::collections::{HashMap, HashSet};

use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

use crate::agent::AgentRegistry;

/// One node of the process table the pure walk visits: a process's parent pid (`None` for an
/// orphan / the table root) and its own program NAME (the executable base name, e.g. `claude`,
/// `bash`, `node`). Extracted so the PPID tree-walk + name match is testable over a hand-built
/// map, independent of `sysinfo`.
#[derive(Debug, Clone)]
pub struct ProcNameNode {
    /// The parent pid, if the platform reported one.
    pub parent: Option<u32>,
    /// The process's program name (the `sysinfo` `name()` — a basename on every platform,
    /// possibly carrying a `.exe` suffix on Windows).
    pub name: String,
}

/// PURE tree probe: does `root` OR any TRANSITIVE descendant of `root` in `table` (a
/// `pid → ProcNameNode` map keyed by each process's own pid) have a name `is_agent` accepts?
///
/// A process is a descendant when its chain of `parent` links reaches `root`. Implementation:
/// a single forward pass building a `parent → children` adjacency, then a DFS from `root`,
/// returning `true` the instant a matching name is seen. Robust to:
///  - a `root` ABSENT from the table → `false` (the shell already exited: no live agent);
///  - parent CYCLES / self-parent (a malformed table) → the `visited` set guarantees each pid
///    is examined at most once and the walk terminates.
///
/// The `root` itself is included in the check (harmless — a shell name is not an agent), so a
/// terminal that somehow IS the agent process is still caught.
pub fn tree_hosts_agent(
    root: u32,
    table: &HashMap<u32, ProcNameNode>,
    is_agent: impl Fn(&str) -> bool,
) -> bool {
    // A root absent from the table hosts nothing (no node, no children) — the crashed/exited
    // case that MUST read as "no live agent" so the resume can fire.
    if !table.contains_key(&root) {
        return false;
    }

    // Build parent → [children] once (O(n)) so the walk is O(n) rather than O(n²).
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, node) in table {
        if let Some(parent) = node.parent {
            children.entry(parent).or_default().push(pid);
        }
    }

    let mut visited: HashSet<u32> = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        // The `visited` guard makes a cyclic/self-parent table terminate and never
        // re-examines a pid reachable by two paths.
        if !visited.insert(pid) {
            continue;
        }
        if let Some(node) = table.get(&pid) {
            if is_agent(&node.name) {
                return true;
            }
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    false
}

/// Snapshot the live process table into the pure `pid → ProcNameNode` map. Two entries are dropped:
///
///  - THREADS (`thread_kind().is_none()`): `sysinfo` enumerates them as standalone `Process` entries
///    on Linux, and a thread carries its process's name — keeping only `None` entries counts each
///    real process once and avoids a thread of an unrelated process masquerading in the graph
///    (parity with [`crate::proc_stats`]'s thread filter).
///  - ZOMBIES (`status() != Zombie`): a process that has ALREADY EXITED but whose parent has not
///    reaped it yet still has a `/proc` entry carrying its name. Counting one would read a
///    CRASHED agent as alive — the exact inversion this gate exists to prevent, and it would park
///    the resume forever (nothing ever re-triggers the drain). A defunct agent is a dead agent.
fn build_table(system: &System) -> HashMap<u32, ProcNameNode> {
    system
        .processes()
        .iter()
        .filter(|(_, proc_)| {
            proc_.thread_kind().is_none() && proc_.status() != ProcessStatus::Zombie
        })
        .map(|(pid, proc_)| {
            (
                pid.as_u32(),
                ProcNameNode {
                    parent: proc_.parent().map(Pid::as_u32),
                    name: proc_.name().to_string_lossy().to_string(),
                },
            )
        })
        .collect()
}

/// Does the process tree rooted at `root_pid` (a terminal's shell pid) host a LIVE process the
/// `is_agent` predicate recognizes? Refreshes a throwaway `System` (the full PPID graph + names,
/// no cpu/memory), builds the table, and runs the pure [`tree_hosts_agent`].
///
/// A `root_pid` that is gone (the shell already exited) → `false` WITHOUT erroring — the tree is
/// empty, so there is no live agent (the caller then treats the terminal as bare and may resume).
///
/// Parametrized by the predicate so the LIVE walk itself is exercised in tests against a real
/// spawned child (matching a predictable binary name), independent of any agent actually being
/// installed. Production calls go through [`agent_alive_in_tree`].
pub fn agent_alive_with(root_pid: u32, is_agent: impl Fn(&str) -> bool) -> bool {
    let mut system = System::new();
    // Refresh ALL processes (we need the full PPID graph to find descendants) but NONE of the
    // expensive per-process fields (cpu/memory/exe/cmd/env) — names + parents come for free.
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let table = build_table(&system);
    tree_hosts_agent(root_pid, &table, is_agent)
}

/// Does the process tree rooted at `root_pid` host a live AGENT process, per the adapter
/// `registry`? PROVIDER-AGNOSTIC: a process counts as an agent when SOME registered adapter's
/// [`crate::agent::AgentAdapter::detect`] matches its name (today `claude`; `codex`/`opencode`
/// as their adapters gain detection) — never a hardcoded `claude`. This is the reliable
/// live-idle-vs-crashed discriminator the lazy-resume gate consults.
pub fn agent_alive_in_tree(root_pid: u32, registry: &AgentRegistry) -> bool {
    agent_alive_with(root_pid, |name| registry.detect(name).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `pid → ProcNameNode` map from `(pid, parent, name)` tuples.
    fn table(rows: &[(u32, Option<u32>, &str)]) -> HashMap<u32, ProcNameNode> {
        rows.iter()
            .map(|&(pid, parent, name)| {
                (
                    pid,
                    ProcNameNode {
                        parent,
                        name: name.to_string(),
                    },
                )
            })
            .collect()
    }

    /// The registry's `detect` is the production predicate — a `claude` process (bare name or the
    /// Windows `claude.exe` form) is an agent; a shell / plain command is not.
    fn is_agent(name: &str) -> bool {
        AgentRegistry::default().detect(name).is_some()
    }

    /// A `claude` sitting as a DESCENDANT of the shell (the live-idle case: shell → claude) is
    /// detected — the tree hosts a live agent, so the resume must be skipped.
    #[test]
    fn detects_agent_descendant_of_shell() {
        // 100 (shell/bash) → 200 (claude). The agent is a child, not the root.
        let t = table(&[(100, None, "bash"), (200, Some(100), "claude")]);
        assert!(tree_hosts_agent(100, &t, is_agent));
    }

    /// A claude nested DEEP under the shell (shell → node → claude, or claude → bash tool) is still
    /// found — the whole subtree is walked, not just the direct child / foreground.
    #[test]
    fn detects_agent_deep_in_the_tree() {
        // 1 (shell) → 2 (node wrapper) → 3 (claude) → 4 (bash tool claude launched).
        let t = table(&[
            (1, None, "sh"),
            (2, Some(1), "node"),
            (3, Some(2), "claude"),
            (4, Some(3), "bash"),
        ]);
        assert!(tree_hosts_agent(1, &t, is_agent));
    }

    /// The CRASHED/absent case: the shell is a BARE prompt (no agent anywhere in its tree). The
    /// tree hosts NO agent, so the resume is free to fire — this is what distinguishes a
    /// crashed/exited agent (stale `Idle`) from a live-idle one.
    #[test]
    fn bare_shell_hosts_no_agent() {
        let t = table(&[
            (100, None, "bash"),
            (200, Some(100), "vim"),
            (300, Some(100), "git"),
        ]);
        assert!(!tree_hosts_agent(100, &t, is_agent));
    }

    /// A claude in an UNRELATED tree (another terminal) is NOT counted for this root — liveness is
    /// per-terminal, keyed on the shell pid, never global.
    #[test]
    fn agent_in_a_sibling_tree_is_not_counted() {
        // 100 (our shell, bare) and a separate tree 500 (other shell) → 600 (claude).
        let t = table(&[
            (100, None, "bash"),
            (500, None, "zsh"),
            (600, Some(500), "claude"),
        ]);
        assert!(!tree_hosts_agent(100, &t, is_agent));
        // ...but rooting at the OTHER shell does find it.
        assert!(tree_hosts_agent(500, &t, is_agent));
    }

    /// A root that is GONE (already exited — not in the table) hosts no agent → `false`. This is
    /// the "PTY closed / shell dead" path that must read as resumable, never a panic.
    #[test]
    fn missing_root_hosts_no_agent() {
        let t = table(&[(200, Some(100), "claude")]);
        assert!(!tree_hosts_agent(999, &t, is_agent));
    }

    /// A malformed table with a parent CYCLE terminates and still answers correctly (the `visited`
    /// guard). Here the cycle 1↔2 hosts a `claude`, so it is detected without looping forever.
    #[test]
    fn parent_cycle_terminates_and_still_detects() {
        let t = table(&[(1, Some(2), "bash"), (2, Some(1), "claude")]);
        assert!(tree_hosts_agent(1, &t, is_agent));
    }

    /// The WINDOWS name form: a process reported as `claude.exe` is recognized exactly like the
    /// bare `claude` (the adapter strips the `.exe`). This pins the Windows code path of the same
    /// cross-platform walk — the tree logic is identical; only the reported name differs.
    #[test]
    fn windows_exe_name_is_detected() {
        let t = table(&[(10, None, "cmd.exe"), (20, Some(10), "claude.exe")]);
        assert!(tree_hosts_agent(10, &t, is_agent));
        // A plain Windows shell tree with no agent is not a false positive.
        let bare = table(&[(10, None, "cmd.exe"), (20, Some(10), "powershell.exe")]);
        assert!(!tree_hosts_agent(10, &bare, is_agent));
    }

    /// The LIVE probe over the REAL OS process table (Unix): spawn a real child (`sleep`) under
    /// the test process, then prove `agent_alive_with` — the production sysinfo refresh + PPID
    /// walk — finds it by name when the predicate matches, and reports `false` when it does not.
    /// This exercises the actual cross-platform engine end to end (not just the pure walk), with a
    /// binary we can spawn deterministically instead of a real `claude`.
    #[cfg(unix)]
    #[test]
    fn live_probe_finds_a_real_descendant_by_name() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a real child process");
        let me = std::process::id();

        // The tree rooted at the TEST process now contains a `sleep` descendant → a predicate that
        // matches `sleep` sees a live "agent"; one that matches only `claude` does not (no real
        // claude is running under the test).
        let found_sleep = agent_alive_with(me, |name| name == "sleep");
        let found_claude = agent_alive_with(me, |name| name == "claude");

        // KILL WITHOUT REAPING: the child is now a ZOMBIE — it has EXITED, but because nothing
        // `wait`ed on it its `/proc` entry survives with its name intact. This is the crashed-agent
        // shape, and it MUST read as absent: counting a defunct process would keep the gate saying
        // "an agent is alive" forever and the parked resume would never fire.
        let _ = child.kill();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut gone = false;
        while std::time::Instant::now() < deadline {
            if !agent_alive_with(me, |name| name == "sleep") {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = child.wait();

        assert!(
            found_sleep,
            "the live sysinfo walk must find the real `sleep` descendant of the test process"
        );
        assert!(
            !found_claude,
            "no real claude runs under the test, so the agent predicate must NOT match"
        );
        assert!(
            gone,
            "a killed-but-unreaped (zombie) descendant must NOT count as a live agent"
        );
    }

    /// A gone root pid over the LIVE table is `false` without erroring (parity with the pure
    /// `missing_root_hosts_no_agent`, but through the real refresh path).
    #[test]
    fn live_probe_missing_root_is_false() {
        assert!(!agent_alive_with(u32::MAX, |_| true));
    }
}
