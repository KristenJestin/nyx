# nyx-core — frozen baseline of pre-existing red tests (Windows)

This file is the **named, frozen list** of the test failures that are
**pre-existing at HEAD** and **environmental to Windows** — not regressions
introduced by the Tauri→Electron migration. It materializes the PRD
`testingDecisions` invariant: *freeze pre-existing failures by name, not by a
fixed count.*

It exists because the done-criterion of task #2 ("Adaptateur Tauri sur
nyx-core", `01KVGHVJSFFQ6SRN2VQA7T285A`) phrases the green-suite gate as "hors 6
pré-existants rouges". That literal **count of 6** does not match the real
Windows baseline. The authoritative gate is this **named list**, frozen below.

> **Phase-3 update (task #8, flow control).** Two NEW `pty::tests` were added for
> the lossless flow-control gate — `pause_gate_is_lossless_and_ordered` and
> `drop_is_prompt_when_paused`. Both spawn `sh` directly, so they join the SAME
> environmental Windows-red category as the nine pre-existing `pty::tests` reds
> (no `sh` on PATH → no child → the assertion that reads PTY output can't pass).
> They are GREEN on the Linux target. The frozen list below is updated from 15 to
> **17** to account for them; this is an expected, named addition (a new
> `sh`-spawning test), not a migration regression of an existing test.
>
> **Phase-5 review update (managed-command runtime extraction).** New tests were
> added for the EXTRACTED snapshot/restore orchestration
> (`command::tests::snapshot_on_shutdown_records_only_live_running_instances`,
> `restore_on_boot_relaunches_then_resets_snapshot`,
> `restore_on_boot_normalizes_orphan_running_when_toggle_off`,
> `restore_on_boot_handles_multiple_instances_at_parity`,
> `shutdown_reap_kills_all_and_latches_once`) and the shared MCP runtime dispatch
> (`mcp_runtime::tests::*`). The five `command::tests` above spawn real `sh`
> commands, so they are gated `#[cfg(not(windows))]` — they are **compiled OUT on
> Windows** (NOT new Windows reds) and run GREEN on the Linux target, exactly like
> the pre-existing `not(windows)` command tests. The five `mcp_runtime::tests` are
> pure (no shell): they run GREEN on EVERY platform (Windows count went 295 → 300
> passed). The frozen red list below is therefore **UNCHANGED at 17** — no new
> failure was introduced.
>
> **Phase-5 audit follow-up (review `01KVJ6NVMXGETK6BG0YH05PMWE`).** Three pure
> `integrations::tests::*` (install/remove parity cores) and five pure
> `mcp_runtime::tests::*` workspace-tool tests (`workspace_add` / `create_workspace`
> at MCP parity) were added — all shell-free, so they run GREEN on EVERY platform
> (Windows count went 300 → **308** passed). The frozen red list stays **UNCHANGED
> at 17** — no new failure was introduced.

## Baseline command and result

```
cargo test -p nyx-core --lib
# → 290 passed; 17 failed; 0 ignored
```

Re-derived on the migration host (Windows): **290 passed / 17 failed**. The 17
failures are exactly the tests listed below — no more, no fewer. The
green-suite invariant for the Tauri adapter is: **the nyx-core suite stays green
EXCEPT for these 17 named, frozen reds.** Any failure outside this list is a
real regression and must block.

## The 17 frozen reds (by fully-qualified test name)

### `pty::tests` — 11 reds (environmental: hard-coded `sh` shell)

These spawn `sh` directly (`Pty::spawn_program("sh", …)`); on Windows
`CreateProcessW` cannot find `sh` on PATH, so the child never starts and there is
no PTY output to assert on.

1. `pty::tests::spawn_write_read_roundtrip`
2. `pty::tests::resize_does_not_panic_and_is_reflected`
3. `pty::tests::kill_terminates_and_exit_code_recoverable`
4. `pty::tests::no_thread_or_handle_leak_after_drop`
5. `pty::tests::reader_channel_closes_when_child_exits`
6. `pty::tests::drop_is_prompt_when_reader_is_blocked`
7. `pty::tests::drop_is_prompt_with_active_output`
8. `pty::tests::dropping_many_ptys_is_prompt`
9. `pty::tests::nyx_terminal_id_is_exported_to_windows_shell`
10. `pty::tests::pause_gate_is_lossless_and_ordered`  *(phase 3, task #8 — flow control)*
11. `pty::tests::drop_is_prompt_when_paused`  *(phase 3, task #8 — flow control)*

### `command::tests` — 1 red (environmental: hard-coded `sh` shell)

12. `command::tests::tree_kill_reaps_grandchild_windows`

### `db::tests` — 4 reds (Windows path assertions)

These assert on path shapes that differ on Windows (separators / canonical
form).

13. `db::tests::instance_run_context_joins_template_and_workspace`
14. `db::tests::list_instances_carries_source_provenance_and_workspace_path`
15. `db::tests::list_instances_joins_template_fields_in_order`
16. `db::tests::restore_rows_carry_both_signals_and_eligibility_selects_only_on_and_running`

### `pkgjson::tests` — 1 red (Windows path assertion)

17. `pkgjson::tests::discovers_root_and_subfolders`

## Why these are pre-existing, not migration regressions

The nyx-core source and test files were moved out of `apps/tauri/src-tauri/src`
into `crates/nyx-core/src` as **100%-similarity renames** (`git diff -M
--summary`: `pty.rs` / `command.rs` / `db.rs` / `pkgjson.rs` renamed 100%). The
test bodies are therefore **byte-identical to HEAD** — the migration did not
touch them. The failures are purely environmental (the host lacks `sh`; Windows
path semantics differ) and reproduce identically against the pre-migration tree.
**No new failure was introduced by the migration.**
