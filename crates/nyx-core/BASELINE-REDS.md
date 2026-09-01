# nyx-core — frozen baseline of pre-existing red tests (Windows)

This file is the **named, frozen list** of the Rust test failures that are
**pre-existing** on Windows — not regressions. It exists so that every pass
stops re-deriving the baseline by hand: the invariant is *freeze pre-existing
failures by name, never by a fixed count*.

**The list is currently EMPTY.** On an idle machine, measured from PowerShell,
`nyx-core --lib` and `nyx-sidecar --lib` are both **fully green**. Any red you
see is either a real regression or one of the load-sensitive names in
[Known intermittents](#known-intermittents--red-under-load-green-in-isolation) —
and those are qualified by *replaying the name alone*, not by assertion.

**How to use it.** Run the baseline command, take the list of failure names your
run prints, and compare it **name for name** with the lists below. A name that
is in neither list is a real regression and must block. **Do not compare
counts** — the passing count moves with every added test, and a count comparison
is exactly how three separate passes produced a wrong baseline.

## MEASURE FROM POWERSHELL — the shell you run from changes the result

This is the single easiest way to produce a wrong baseline on this platform, and
it costs an hour every time. `cargo test` launched **from Git Bash** inherits a
`PATH` that carries Git's `usr\bin`, where `sh.exe`, `true.exe`, `false.exe` and
`test.exe` all live — so those names become resolvable *inside the shell nyx
spawns for a managed command*, and a test whose command line is POSIX turns
green. Launched from **PowerShell** (or `cmd`), that directory is absent and the
same test is red. Two agents on the same machine, at the same commit, therefore
measure two different baselines, and each concludes the other introduced a
regression. That is exactly what happened here on 2026-08-26.

**Removing the 18 reds below did NOT remove this trap** — it removed the tests
that had fallen into it. The trap is a property of the environment and it is
still armed. It has two mouths, and the second is the dangerous one:

1. A test expecting **GREEN** from a POSIX command line is red from PowerShell
   and green from Git Bash — a visible, arguable divergence.
2. A test expecting **RED** from a POSIX command line is green from **both** —
   while asserting nothing. `The term 'sh' is not recognized` is a non-zero exit
   like any other, and PowerShell's error text quotes the whole command line
   back, so even an assertion as specific as "the captured tail still contains
   `TAIL_SENTINEL`" passes on a command that never ran.

So: **measure from PowerShell** (it is the environment the app ships into), and
**never write a command line as a literal** in a test that arms a managed
command. [`crate::test_cmdline`] renders "the line that does X on this platform"
in the dialect of the shell [`crate::pty::resolve_shell`] will really pick. A
new POSIX literal re-opens both mouths at once.

## How this list was obtained

Re-measured on **2026-09-01**, on Windows 11 (`D:` drive, Git installed at
`C:\Program Files\Git` — so `bash.exe` and `sh.exe` exist on disk — but Git's
`usr\bin` **not** on the Windows `PATH`), on the worktree
`feature/tests-windows` at `94e60fa` (branched from `v2` at `d350d43`), **from
PowerShell**, machine otherwise idle, default test threads:

```
cargo test -p nyx-core --lib      # → ok. 1512 passed; 0 failed
cargo test -p nyx-sidecar --lib   # → ok.  107 passed; 0 failed
cargo check --workspace           # → clean
```

The same tree, same machine, same commit, **from Git Bash**, on an idle machine:

```
cargo test -p nyx-core --lib      # → ok. 1512 passed; 0 failed
cargo test -p nyx-sidecar --lib   # → ok.  107 passed; 0 failed
```

Identical, to the test. That is the point: a suite that no longer asks the
environment for `sh` cannot answer differently depending on which terminal
launched it.

The suite still compiles ONE test **out** on Windows
(`mcp_engine::tests::two_concurrent_ticks_on_one_run_do_not_both_advance_it`,
whose gate command is a small state machine rather than "a command that does
X"). It is neither a pass nor a failure here, and it is green on the
Linux/macOS legs of the cross-OS gate
(`.github/workflows/gate-v2.2.5-terminal-persistence.yml`).

## The frozen reds (by fully-qualified test name)

**None.** Every red this file has ever carried has been *fixed*, not renamed —
see [History](#history-of-this-file).

## Known intermittents — red under load, green in isolation

These are timing-dependent, **not** frozen reds, and must not be read as
regressions when they appear (nor as regressions of this file when they do not).
All were **green** in the measurement recorded above, and each has been observed
red on the same tree under load — replay the name alone, or with
`-- --test-threads=1`, before concluding anything:

- `design::tests::the_most_expensive_accepted_recipe_still_scans_a_real_tree_quickly`
  — a wall-clock budget on a real tree scan; it fails under machine load.
- `git_watch::tests::a_burst_of_edits_is_coalesced_and_then_silent` and
  `git_watch::tests::multi_repo_refreshes_stay_keyed_to_their_own_identity` — real
  filesystem watchers with debounce windows; under contention the watcher emits
  one event more than the "an idle repo pushes nothing further" assertions allow.
  Both were red on 2026-09-01 in a `cargo test -p nyx-core --lib` run made *while
  a second `cargo test` held the machine*, and green in the idle runs above. The
  first was red once more that day (`left: 3, right: 2`) in a full run that had a
  `cargo check --workspace` alongside it, and green 3/3 replayed alone on the same
  binary a minute later — the whole shape of this family, in one observation.
- `runtime::command::tests::on_state_pushes_a_commands_changed_invalidation_on_a_runtime_transition`
  (crate `nyx-sidecar`) — a race, observed red once (2026-08-26) and not
  reproduced since.
- `runtime::db_command::tests::repo_watch_pushes_repos_changed_on_a_real_edit_over_the_v21_pipe`
  (crate `nyx-sidecar`) — a load-sensitive race in a real filesystem watcher:
  under contention the watcher emits one event more than the test's "idle watched
  repo pushes nothing further" assertion allows (`db_command.rs`,
  `left: 3, right: 2`). Red 2 runs out of 2 during the 2026-08-26 audit while
  `vitest --project unit` and `cargo check --workspace` ran alongside; green on
  an idle machine (3/3, and 6/6 for the test alone with `-- --exact`), and green
  in the 2026-09-01 measurements above.

## Sibling suites, for reference

```
cargo test -p nyx-sidecar --test contract  # → 1 passed; 0 failed
cd contract && bun test                    # → 73 pass; 0 fail
```

## History of this file

**2026-09-01 (second pass) — the last 18 removed, and the "product limitation"
they were said to prove did not exist.** This file claimed the 18 reds below
reported a real limit of nyx — that a user typing `sh -c …` into a managed
command on Windows fails the same way, and that turning them green would delete
the only signal the limit exists. That reading was wrong on the decisive point:
**nyx already does the right thing.** It resolves the platform shell and its flag
([`crate::pty::resolve_shell`] + [`crate::command::command_invocation`]:
`pwsh`/`powershell` → `-Command`, `cmd` → `/C`, POSIX → `-c`), and **nothing in
production ever writes `sh` or `true`**. The POSIX was in the FIXTURES, and only
there. A test that arms a gate is not testing `sh`; it is testing what nyx does
with a command that succeeds, fails with a given code, prints, floods, hangs, or
reads an environment variable. The command line is the vehicle, not the subject.

The fix is `crate::test_cmdline` (`feature/tests-windows`, `6159e67`, `64cc236`,
`2f1603c`, `94e60fa`): one place that renders "the line that does X here" for the
shell `resolve_shell` will really pick, reusing `command::shell_stem` so the two
classifications cannot drift. Not one assertion moved — `exit 2` is still
asserted as `exit 2` — because `exit <n>` is shell SYNTAX, spelled identically by
all three shells nyx can pick, where `true` and `false` are program NAMES.

The 18 that went green: `mcp_engine::tests` ×8 (`a_gate_red_retry_…`,
`a_green_gate_captures_…`, `a_parked_red_gate_…`, `a_red_gate_result_is_persisted_…`,
`arming_a_gate_mid_run_…`, `every_stop_of_nyx_next_…`, `nyx_next_drives_the_ndev_loop_…`,
`the_seeded_coders_empty_gate_…`), `seed::tests` ×9 (all through `arm_coder_gate`),
and `runtime::db_command::tests::engine_run_list_reflects_a_live_nyx_next_run`
(crate `nyx-sidecar`).

The same pass also found the **inverse** failure, which no baseline had ever
named because it never showed as red: a dozen gates in `mcp_engine::tests`
expecting a RED command were passing *because the command did not exist*. They
are armed through the same module now, so a red gate is red for the reason it
claims. And three tests lost a `#[cfg(not(windows))]` that only ever protected a
`sh`: both N31 environment guards — the regression N31 is named for — and the
budget that cuts a gate short without a verdict, now run on Windows too.

**A product change was considered and REJECTED**: enriching the `PATH` nyx hands
a managed command would make `sh` resolvable inside nyx and nowhere else — green
in nyx, red by hand, which is the exact trap `command.rs`'s N31 tests exist to
guard, and it would put the whole POSIX toolbox within reach of any test that
wanted a shortcut. The idea this file used to point at for that chantier —
`01M1EWNM8B7X497075MH8373JH`, "make nyx resolve POSIX command lines on Windows" —
therefore has no subject left; it was filed against a limitation that was a
fixture defect.

**2026-09-01 (first pass) — 19 of the 23 frozen reds removed, and they were
removed by being FIXED, not by being renamed.** The pass that did it
(`feature/tests-windows`, commits `e447032`, `969ca38`, `58bacb0`) found that all
19 were defects in the tests themselves, not in nyx:

- **7 path assertions** (`db::tests` ×5, `pkgjson::tests::discovers_root_and_subfolders`,
  `mcp_tools_core::tests::create_terminal_opens_the_shell_for_the_record_it_wrote`)
  compared a path nyx had produced against a hand-written literal. nyx
  *deliberately* normalizes (`pathnorm::normalize`: ASCII case folded, platform
  separator) — that is what makes a workspace's identity survive two spellings of
  the same directory. The expectation now goes through the same normalizer, so it
  holds on every OS instead of on one.
- **11 `pty::tests`** hard-coded `Pty::spawn_program("sh", …)`. Windows carries no
  `sh` on any `PATH` entry, so each died in `CreateProcessW` before asserting
  anything. They now ask `crate::shells` — the module that already owns "which
  shell can this machine open", and the same `program_exists` the `$SHELL` check
  rests on — and get Git Bash on Windows, `sh` elsewhere. With no POSIX shell at
  all they SKIP loudly rather than fail opaquely.
  That uncovered a second, unrelated Windows blocker, which the same commit
  fixed: **a ConPTY asks the terminal where the cursor is (DSR `ESC[6n`) the
  instant it opens and holds the child's output back until it is answered.** In
  the app the renderer answers; a headless test has no renderer, so every Windows
  PTY test received exactly one chunk — the query — and then silence. That is why
  entry 9 of the old list (`nyx_terminal_id_is_exported_to_windows_shell`) failed
  while spawning `cmd.exe` and not `sh`: *the shell was never the problem there.*
  The tests now play the terminal's part, answering through
  `crate::command::scan_terminal_queries` rather than a second copy of it.
- **1 `command::tests::tree_kill_reaps_grandchild_windows`** put `$p` inside a
  double-quoted PowerShell string. nyx runs a managed command as
  `<platform shell> -Command <cmdline>`, so wherever the platform shell is
  PowerShell — every machine that has one — the OUTER shell expanded `$p` to
  nothing before the line was ever a script. The test only passed where the
  platform shell was `cmd.exe`. Its command line no longer contains a `$`; what
  it asserts is unchanged.

The 4 remaining names from the old list
(`mcp_context::tests` ×2, `mcp_tools_spec::tests::spec_transition_start_…`,
`design::tests::homonymous_bundles_…`) were already **green** at `d350d43`,
fixed between 2026-08-26 and this measurement.

In their place that pass named 18 reds this file had never carried: the
`mcp_engine`, `seed` and `nyx-sidecar` tests the second pass then fixed. They
were not new failures — the 2026-08-26 measurement was almost certainly taken
from a Git Bash shell, where Git's `usr\bin` on `PATH` makes every one of them
pass. Hence the shell section at the top of this file, which outlives them.

**2026-08-26.** Between v2.0 and that date this file claimed `290 passed; 17
failed` and listed 17 names. That measurement dated from the Tauri→Electron
migration (`c51c5d7`) and was never refreshed while the suite grew past 1 200
tests, so the count was off by a factor of four and six real environmental reds
were missing from the list. Passes that trusted it either re-derived the baseline
by hand — twice, wrongly — or reported green suites that were not.

**When you add or remove a red, say here how you measured it, and from which
shell.** A list without a measurement is what went stale.
