# nyx-core — frozen baseline of pre-existing red tests (Windows)

This file is the **named, frozen list** of the `nyx-core` test failures that are
**pre-existing** and **environmental to Windows** — not regressions. It exists so
that every pass stops re-deriving the baseline by hand: the invariant is *freeze
pre-existing failures by name, never by a fixed count*.

**How to use it.** Run the baseline command, take the list of failure names your
run prints, and compare it **name for name** with the list below. A name in your
run that is **not** below is a real regression and must block. A name below that
is **green** in your run is fine (several of these are order- or timing-sensitive
and pass intermittently). **Do not compare counts** — the passing count moves
with every added test, and a count comparison is exactly how three separate
passes produced a wrong baseline.

## How this list was obtained

Re-measured on **2026-08-26**, on Windows 11 (`D:` drive, Git Bash on PATH but no
POSIX `sh` reachable by `CreateProcessW`), on the worktree
`feature/tiling-panes` at `4a2b07a` + the working tree of PRD
`01KZ8GHHDF6ZACSMN4VRB60Q7S`, machine otherwise idle, default test threads:

```
cargo test -p nyx-core --lib
# → test result: FAILED. 1252 passed; 23 failed; 0 ignored; 0 measured
```

Every one of the 23 names below was then checked to exist **at HEAD** (`git show
HEAD:<file> | grep 'fn <name>('`) — all 23 do, so this is a baseline of the line
itself and not of the PRD's working tree. The counts are recorded only to date
the measurement; **the names are the gate**.

The suite also compiles tests **out** on Windows (`#[cfg(not(windows))]`,
notably the `sh`-spawning `command::tests`), so they are neither passes nor
failures here and are green on the Linux/macOS legs of the cross-OS gate
(`.github/workflows/gate-v2.2.5-terminal-persistence.yml`).

## The 23 frozen reds (by fully-qualified test name)

### `pty::tests` — 11 reds, **two distinct causes**

**Ten of the eleven — no POSIX `sh` for `CreateProcessW`.** These spawn `sh`
directly (`Pty::spawn_program("sh", …)`); on Windows the child never starts, so
there is no PTY output to assert on. Measured signature, identical for the ten:
`spawn sh: CreateProcessW … (os error 2)`.

**Entry 9 is not one of them.** It spawns `cmd.exe`, its child *does* start, and
it fails on its own assertion — cause and signature are on its line below. A
pass that installs a POSIX `sh`, re-runs, and finds 9 still red has **not** found
a regression.

1. `pty::tests::spawn_write_read_roundtrip`
2. `pty::tests::resize_does_not_panic_and_is_reflected`
3. `pty::tests::kill_terminates_and_exit_code_recoverable`
4. `pty::tests::no_thread_or_handle_leak_after_drop`
5. `pty::tests::reader_channel_closes_when_child_exits`
6. `pty::tests::drop_is_prompt_when_reader_is_blocked`
7. `pty::tests::drop_is_prompt_with_active_output`
8. `pty::tests::dropping_many_ptys_is_prompt`
9. `pty::tests::nyx_terminal_id_is_exported_to_windows_shell` — **the odd one
   out: it does not spawn `sh`.** The test is `#[cfg(windows)]`
   (`crates/nyx-core/src/pty.rs:919`) and spawns `%ComSpec%` — cmd.exe — with
   `["/C", "echo NYXTID=[%NYX_TERMINAL_ID%]"]` (`pty.rs:923-931`). The child
   *starts*; the test fails on its own assertion, not on a spawn error:
   `pty.rs:934` — `child cmd must see %NYX_TERMINAL_ID% == the record id; got:
   "\u{1b}[6n"`. Within the 10 s read window the ConPTY read brings back only
   the cursor-position probe, never the echoed line. Environmental all the same
   (Windows/ConPTY), but the fix is not a POSIX `sh`.
10. `pty::tests::pause_gate_is_lossless_and_ordered`
11. `pty::tests::drop_is_prompt_when_paused`

### `command::tests` — 1 red (shell quoting of the spawn probe)

12. `command::tests::tree_kill_reaps_grandchild_windows` — the probe's own
    command line is re-parsed by PowerShell, so the grandchild PID never reaches
    stdout: `could not parse grandchild pid from "\u{1b}[6n…At line:1 char:120"`.

### `db::tests` — 5 reds (path separators and drive-letter case)

13. `db::tests::instance_run_context_joins_template_and_workspace`
14. `db::tests::list_instances_carries_source_provenance_and_workspace_path`
15. `db::tests::list_instances_joins_template_fields_in_order`
16. `db::tests::restore_rows_carry_both_signals_and_eligibility_selects_only_on_and_running`
17. `db::tests::identity_resolution_is_stable_across_a_disk_path_change` —
    `left: "C:\\new\\loc"` vs `right: "c:\\new\\loc"`: the drive letter comes
    back upper-cased.

### `mcp_context::tests` / `mcp_tools_spec::tests` — 3 reds (cwd→workspace anchoring by path)

The anchor is resolved by comparing a cwd string with a stored workspace path;
`\` vs `/` (and drive-letter case) make the match fail, so the tool answers
"could not resolve a Spec" / `Null`.

18. `mcp_context::tests::agent_context_tool_resolves_by_spec_id_workspace_id_and_cwd`
19. `mcp_context::tests::spec_role_without_anchored_spec_renders_project_scoped_authoring`
20. `mcp_tools_spec::tests::spec_transition_start_resolves_the_anchor_from_cwd_and_dev_context_follows`

### `mcp_tools_core::tests` — 1 red (path separators)

21. `mcp_tools_core::tests::create_terminal_opens_the_shell_for_the_record_it_wrote` —
    `left: [(…, "\\tmp\\h1-open")]` vs `right: [(…, "/tmp/h1-open")]`.

### `pkgjson::tests` — 1 red (path separators)

22. `pkgjson::tests::discovers_root_and_subfolders` — `left: "packages\\api"` vs
    `right: "packages/api"`.

### `design::tests` — 1 red (directory-traversal order)

23. `design::tests::homonymous_bundles_in_different_directories_are_two_components_and_stop_pending` —
    `left: ["data-table", "list/data-table"]` vs the expected reverse: the
    qualification order follows the order NTFS hands the directories back.

## Known intermittents — red on some runs, absent here

These are timing-dependent, **not** frozen reds, and must not be read as
regressions when they appear (nor as regressions of this file when they do not):

- `design::tests::the_most_expensive_accepted_recipe_still_scans_a_real_tree_quickly`
  — a wall-clock budget on a real tree scan; it fails under machine load. It was
  red on a `4a2b07a` run measured on 2026-08-26 and green on the run recorded
  above.
- `runtime::command::tests::on_state_pushes_a_commands_changed_invalidation_on_a_runtime_transition`
  (crate `nyx-sidecar`) — a race, observed red once and not reproduced since.
- `runtime::db_command::tests::repo_watch_pushes_repos_changed_on_a_real_edit_over_the_v21_pipe`
  (crate `nyx-sidecar`) — a load-sensitive race in a real filesystem watcher: under
  contention the watcher emits one event more than the test's "idle repo pushes
  nothing further" assertion allows.
  **Red**, 2 runs out of 2, during the audit of 2026-08-26 on this worktree:
  `cargo test -p nyx-sidecar --lib` run *while* `vitest --project unit` and
  `cargo check --workspace` were running — `db_command.rs:4183`,
  `assertion left == right failed: "an idle watched repo pushes nothing further
  — event-driven, not polled"`, `left: 3, right: 2`.
  **Green** on the same command with the machine idle (3 runs out of 3, and 6 out
  of 6 for the test alone with `-- --exact`).
  **Not reproduced** on 2026-08-26 in three further attempts on the same worktree
  — `cargo test -p nyx-sidecar --lib` once under a concurrent `bun run test:unit`,
  once under that plus 12 CPU spinners, once under 6 filesystem-churn loops:
  94 passed / 0 failed each time. So the window is narrow; the entry stays because
  the red was measured, not because it is easy to hit.
  It is **not** a regression of any current work: the body of that `fn` is
  byte-identical to `HEAD` (`git show HEAD:crates/nyx-sidecar/src/runtime/db_command.rs`,
  same md5 over the whole function; it only moved +100 lines).

## Sibling suites, for reference

Measured the same day, same tree:

```
cargo test -p nyx-sidecar --lib            # → 94 passed; 0 failed
cargo test -p nyx-sidecar --test contract  # →  1 passed; 0 failed
cd contract && bun test                    # → 73 pass; 0 fail
```

`nyx-sidecar` carries **no frozen reds** — nothing in it is baselined as
pre-existing, and the numbers above are what an idle machine produces. It is
**not** immune to timing-sensitive failures, though: two of its tests are listed
under *Known intermittents* above. So the rule is *a failure in `nyx-sidecar` is
a regression **unless** its name appears in that section* — and if it does, say
in your report under what load you measured it, exactly as those entries do.

## History of this file

Between v2.0 and 2026-08-26 this file claimed `290 passed; 17 failed` and listed
17 names. That measurement dated from the Tauri→Electron migration
(`c51c5d7`) and was never refreshed while the suite grew past 1 200 tests, so
the count was off by a factor of four and six real environmental reds
(`db::tests::identity_resolution_is_stable_across_a_disk_path_change`, the three
cwd-anchoring MCP tests, `mcp_tools_core::tests::create_terminal_opens_the_shell_for_the_record_it_wrote`,
`design::tests::homonymous_bundles_…`) were missing from the list. Passes that
trusted it either re-derived the baseline by hand — twice, wrongly — or reported
green suites that were not. The section above replaces it with a measurement
whose command, tree and date are stated, so the next staleness is visible rather
than inferred.

**When you add or remove a red, say here how you measured it.** A list without a
measurement is what went stale.
