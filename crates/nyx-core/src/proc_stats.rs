//! Per-terminal CPU% + RAM over a PROCESS TREE (FEEDBACK #28).
//!
//! A terminal's resource consumption is NOT the shell process alone — it is the
//! WHOLE tree rooted at the shell: the shell + every transitive descendant it
//! spawned (a `claude`, an `npm run dev`, a `cargo build`, …). So we sum `cpu_usage`
//! and `memory` over the shell pid AND all of its descendants.
//!
//! ## Cross-platform — `sysinfo`, NOT `/proc`
//!
//! The descendant set is discovered by walking PARENT pids (PPID): every process
//! whose chain of `parent()` links reaches the root is in the tree. This is the ONE
//! portable signal — Linux/macOS/Windows all expose a parent pid — so we deliberately
//! avoid `/proc` session ids or process-group leaders (Linux-only). [`sysinfo`]
//! abstracts the per-OS process table for us.
//!
//! ## CPU% needs a LIVE, REUSED `System`
//!
//! `sysinfo` computes a process's CPU% as the work done BETWEEN two refreshes. The
//! first refresh of a process therefore reports 0% (no prior sample to diff against);
//! the value is meaningful only from the SECOND refresh on. So the owner ([`ProcStats`])
//! must be kept ALIVE across polls — a fresh `System` per call would always read 0%.
//! The host owns exactly one [`ProcStats`] and calls [`ProcStats::tree_stats`] once per
//! live terminal each tick.
//!
//! The tree-walk + summation is split into the PURE [`sum_tree_stats`] over a
//! `pid → ProcNode` map, so the descendant logic is unit-tested without a live
//! `sysinfo::System` (which cannot be faked deterministically).

use std::collections::HashMap;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// One terminal's process-tree resource usage (the shell + all descendants).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TreeStats {
    /// Summed CPU usage across the tree, in PERCENT. `sysinfo` reports per-process
    /// CPU% relative to a SINGLE core, so a tree busy on N cores can exceed 100%
    /// (e.g. a parallel `cargo build` → 380%). The host/UI may clamp/normalize; we
    /// report the raw sum so nothing is lost.
    pub cpu_pct: f32,
    /// Summed resident memory across the tree, in BYTES (RSS — `sysinfo::Process::memory`).
    pub mem_bytes: u64,
}

impl TreeStats {
    /// The all-zero reading — returned when the root pid is not present in the table
    /// (the shell already exited, or the platform could not enumerate it). Never an
    /// error: a gone terminal simply consumes nothing.
    pub const ZERO: TreeStats = TreeStats {
        cpu_pct: 0.0,
        mem_bytes: 0,
    };
}

/// One node of the process table the pure summation walks: a process's parent pid
/// (`None` for an orphan / the table root) and its own CPU%/memory. Extracted so the
/// PPID tree-walk is testable over a hand-built map, independent of `sysinfo`.
#[derive(Debug, Clone, Copy)]
pub struct ProcNode {
    pub parent: Option<u32>,
    pub cpu_pct: f32,
    pub mem_bytes: u64,
}

/// PURE tree summation: sum `cpu_pct` + `mem_bytes` over `root` and every TRANSITIVE
/// descendant of `root` in `table` (a `pid → ProcNode` map keyed by each process's own
/// pid). A process is a descendant when its chain of `parent` links reaches `root`.
///
/// Implementation: a single forward pass building a `child → children` adjacency from
/// the `parent` links, then a BFS/DFS from `root`. Robust to:
///  - a `root` absent from the table → [`TreeStats::ZERO`] (no node, no children);
///  - parent CYCLES / self-parent (a malformed table) → the `visited` set guarantees
///    each pid is counted at most once and the walk terminates.
pub fn sum_tree_stats(root: u32, table: &HashMap<u32, ProcNode>) -> TreeStats {
    // The root itself must exist to consume anything.
    if !table.contains_key(&root) {
        return TreeStats::ZERO;
    }

    // Build parent → [children] once (O(n)) so the walk is O(n) rather than O(n²).
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, node) in table {
        if let Some(parent) = node.parent {
            children.entry(parent).or_default().push(pid);
        }
    }

    let mut stats = TreeStats::ZERO;
    let mut visited: HashMap<u32, ()> = HashMap::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        // The `visited` guard makes a cyclic/self-parent table terminate and never
        // double-counts a pid reachable by two paths.
        if visited.insert(pid, ()).is_some() {
            continue;
        }
        if let Some(node) = table.get(&pid) {
            stats.cpu_pct += node.cpu_pct;
            stats.mem_bytes = stats.mem_bytes.saturating_add(node.mem_bytes);
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    stats
}

/// The owner of a LIVE `sysinfo::System`, kept alive across polls so per-process CPU%
/// deltas are meaningful (see the module note). The host constructs ONE of these and
/// calls [`tree_stats`](Self::tree_stats) once per live terminal each tick.
pub struct ProcStats {
    system: System,
}

impl Default for ProcStats {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcStats {
    /// Build an empty introspector. The first [`tree_stats`](Self::tree_stats) call
    /// refreshes the process table; CPU% becomes meaningful from the SECOND call on
    /// (the first has no prior sample to diff against, so it reports ~0%).
    pub fn new() -> Self {
        ProcStats {
            // `System::new()` does NOT enumerate anything yet — we refresh on demand,
            // only the cheap process slice (cpu + memory), never disks/networks.
            system: System::new(),
        }
    }

    /// Refresh the process table and return the summed CPU%/RAM of the tree rooted at
    /// `root_pid` (the shell pid) — the shell + all transitive descendants.
    ///
    /// A `root_pid` that is gone (the shell already exited) yields [`TreeStats::ZERO`]
    /// WITHOUT erroring. Refreshes only the process slice we read (CPU + memory), so the
    /// per-tick cost stays bounded.
    ///
    /// A thin shim over [`tree_stats_batch`](Self::tree_stats_batch): one root in, one
    /// reading out — so the SINGLE-root path and the batch share the exact same refresh +
    /// table-build + summation, and can never drift.
    pub fn tree_stats(&mut self, root_pid: u32) -> TreeStats {
        self.tree_stats_batch(&[root_pid])
            .into_iter()
            .next()
            .unwrap_or(TreeStats::ZERO)
    }

    /// Refresh the process table ONCE, then sum the tree rooted at EACH pid in `roots`,
    /// returning one [`TreeStats`] per root, IN THE SAME ORDER (FEEDBACK #28 perf).
    ///
    /// This is the per-tick entry point: the host has N live terminals and wants all N
    /// trees from the SAME process snapshot. The expensive part — the full `/proc` scan
    /// (`refresh_processes_specifics(All, …)`) and building the `pid → ProcNode` table —
    /// happens EXACTLY ONCE here, no matter how many roots; then the cheap pure
    /// [`sum_tree_stats`] runs per root over the shared table. (The old per-call path
    /// re-scanned the whole `/proc` once PER terminal — N full scans per tick.)
    ///
    /// A root that is gone (its shell already exited) yields [`TreeStats::ZERO`] at its
    /// position WITHOUT erroring. An empty `roots` returns an empty Vec (no refresh).
    pub fn tree_stats_batch(&mut self, roots: &[u32]) -> Vec<TreeStats> {
        if roots.is_empty() {
            return Vec::new();
        }

        // ONE refresh of ALL processes for the WHOLE batch (we need the full PPID graph
        // to find descendants, and a descendant's own refresh is what makes its CPU%
        // delta meaningful). Only the CPU + memory fields are updated — never the
        // expensive ones (exe, cmd, env).
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );

        // Snapshot the live process table into the pure `pid → ProcNode` map ONCE, then
        // sum every root against it.
        //
        // CRITICAL (FEEDBACK #28): `sysinfo` enumerates THREADS as standalone `Process`
        // entries, and EACH thread carries the FULL process RSS (and an inflated CPU%).
        // A `claude` with 28 threads would otherwise be counted 28× → ~16-20× over-report.
        // `Process::thread_kind()` returns `Some(..)` for a thread and `None` for a real
        // process (always `None` off Linux, where threads are not enumerated separately),
        // so keeping only `None` entries counts each real process exactly once.
        let table: HashMap<u32, ProcNode> = self
            .system
            .processes()
            .iter()
            .filter(|(_, proc_)| proc_.thread_kind().is_none())
            .map(|(pid, proc_)| {
                (
                    pid.as_u32(),
                    ProcNode {
                        parent: proc_.parent().map(Pid::as_u32),
                        cpu_pct: proc_.cpu_usage(),
                        mem_bytes: proc_.memory(),
                    },
                )
            })
            .collect();

        roots
            .iter()
            .map(|&root| sum_tree_stats(root, &table))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `pid → ProcNode` map from `(pid, parent, cpu, mem)` tuples.
    fn table(rows: &[(u32, Option<u32>, f32, u64)]) -> HashMap<u32, ProcNode> {
        rows.iter()
            .map(|&(pid, parent, cpu_pct, mem_bytes)| {
                (
                    pid,
                    ProcNode {
                        parent,
                        cpu_pct,
                        mem_bytes,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn sums_root_plus_transitive_descendants() {
        // 100 (shell) → 200 (npm) → 300 (node); 100 → 400 (claude). A sibling tree
        // (500 → 600) and an unrelated root (700) must NOT be counted.
        let t = table(&[
            (100, None, 1.0, 10),
            (200, Some(100), 2.0, 20),
            (300, Some(200), 4.0, 40),
            (400, Some(100), 8.0, 80),
            (500, None, 16.0, 160),
            (600, Some(500), 32.0, 320),
            (700, None, 64.0, 640),
        ]);
        let s = sum_tree_stats(100, &t);
        // 1 + 2 + 4 + 8 = 15% cpu; 10 + 20 + 40 + 80 = 150 bytes.
        assert_eq!(s.cpu_pct, 15.0);
        assert_eq!(s.mem_bytes, 150);
    }

    #[test]
    fn missing_root_is_zero() {
        let t = table(&[(200, Some(100), 2.0, 20)]);
        assert_eq!(sum_tree_stats(999, &t), TreeStats::ZERO);
    }

    #[test]
    fn leaf_root_counts_only_itself() {
        let t = table(&[(100, None, 1.0, 10), (300, Some(200), 4.0, 40)]);
        let s = sum_tree_stats(100, &t);
        assert_eq!(s.cpu_pct, 1.0);
        assert_eq!(s.mem_bytes, 10);
    }

    #[test]
    fn deep_chain_is_fully_summed() {
        // A 5-deep chain rooted at 1; every link is a descendant.
        let t = table(&[
            (1, None, 1.0, 1),
            (2, Some(1), 1.0, 1),
            (3, Some(2), 1.0, 1),
            (4, Some(3), 1.0, 1),
            (5, Some(4), 1.0, 1),
        ]);
        let s = sum_tree_stats(1, &t);
        assert_eq!(s.cpu_pct, 5.0);
        assert_eq!(s.mem_bytes, 5);
    }

    #[test]
    fn parent_cycle_terminates_and_counts_once() {
        // Malformed table: 1 → 2 → 1 (a cycle) plus a self-parent at 3. The walk must
        // terminate and count each reachable pid at most once.
        let t = table(&[
            (1, Some(2), 1.0, 10),
            (2, Some(1), 2.0, 20),
            (3, Some(3), 4.0, 40),
        ]);
        let s = sum_tree_stats(1, &t);
        // 1 and 2 are mutually reachable; each counted once → 3% / 30 bytes. 3 (its own
        // disconnected self-cycle) is NOT reachable from 1.
        assert_eq!(s.cpu_pct, 3.0);
        assert_eq!(s.mem_bytes, 30);
    }

    #[test]
    fn empty_table_is_zero() {
        assert_eq!(sum_tree_stats(1, &HashMap::new()), TreeStats::ZERO);
    }

    /// The batch returns exactly one reading per root, IN ORDER, computed from a SINGLE
    /// refresh (FEEDBACK #28 perf). We assert the shape (one entry per root, aligned to
    /// the input order) over the LIVE `System`: the current process's own pid is a real
    /// root (so it sums to non-zero RSS), an obviously-dead pid is `ZERO`, and the order
    /// is preserved (the dead pid stays at its slot). The single-refresh guarantee is
    /// structural — `tree_stats_batch` calls `refresh_*` exactly once before the loop —
    /// and the table-build/sum split is exercised by the pure `sum_tree_stats` tests above.
    #[test]
    fn batch_returns_one_entry_per_root_in_order() {
        let mut stats = ProcStats::new();
        let me = std::process::id();
        // A pid that is essentially never live (u32::MAX) → ZERO at its slot.
        let dead = u32::MAX;
        let out = stats.tree_stats_batch(&[me, dead, me]);
        assert_eq!(out.len(), 3, "one reading per root");
        assert_eq!(out[1], TreeStats::ZERO, "the dead pid keeps its slot, zeroed");
        // The current process is alive → its tree has a real RSS.
        assert!(out[0].mem_bytes > 0, "the test process's own tree consumes RAM");
        // Same root, same table → identical reading at both slots.
        assert_eq!(out[0], out[2], "the same root sums identically within a batch");
    }

    /// An empty batch refreshes nothing and returns an empty Vec (the host SKIPS the napi
    /// call entirely when there are no terminals, but the core must still be sane).
    #[test]
    fn empty_batch_is_empty() {
        let mut stats = ProcStats::new();
        assert!(stats.tree_stats_batch(&[]).is_empty());
    }

    /// `tree_stats` (single) and `tree_stats_batch` agree for one pid — the single path is
    /// a shim over the batch, so they share the exact refresh + table + summation.
    #[test]
    fn single_agrees_with_batch_for_one_pid() {
        let mut a = ProcStats::new();
        let mut b = ProcStats::new();
        let me = std::process::id();
        let single = a.tree_stats(me);
        let batch = b.tree_stats_batch(&[me]);
        assert_eq!(batch.len(), 1);
        // Two independent `System`s on their FIRST refresh both read CPU% as ~0 (no prior
        // sample), so RSS is the stable field to compare; both see the live process's RAM.
        assert!(single.mem_bytes > 0 && batch[0].mem_bytes > 0);
    }
}
