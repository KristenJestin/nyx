# nyxBridge inventory — every `@tauri-apps/*` call-site → contract surface

This is the exhaustive map the `nyxBridge` contract (`./contract.ts`) was derived
from. Every PRODUCTION (`src`, non-`.test.`) use of `@tauri-apps/api` or
`@tauri-apps/plugin-*` is listed with the contract method/event that covers it.
Test files use the same surface through the test fake (`./fake.ts`, phase 1
follow-on) / the shared contract suite.

Generated against `apps/frontend/src` at PRD electron-migration phase 1.

## Request/response — `invoke("<command>")` → `NyxBridge`

44 distinct backend commands are invoked. Each is a member of the
`BackendCommand` union and is reachable via `bridge.invoke<R>(name, args)`; the
hot terminal ones also have dedicated typed methods.

| Backend command                     | Call-site(s)                                               | Contract method                               |
| ----------------------------------- | ---------------------------------------------------------- | --------------------------------------------- |
| `pty_spawn`                         | `terminal/use-pty.ts`                                      | `ptySpawn()`                                  |
| `pty_write`                         | `terminal/use-pty.ts`                                      | `ptyWrite()`                                  |
| `pty_resize`                        | `terminal/use-pty.ts`                                      | `ptyResize()`                                 |
| `pty_close`                         | `terminal/use-pty.ts`                                      | `ptyClose()`                                  |
| `register_terminal_pty`             | `sidebar/terminal-manager.tsx`                             | `invoke("register_terminal_pty")`             |
| `terminal_info`                     | `sidebar/auto-label.ts`                                    | `invoke("terminal_info")`                     |
| `create_terminal`                   | `sidebar/use-terminals.ts`                                 | `invoke("create_terminal")`                   |
| `list_terminals`                    | `sidebar/use-terminals.ts`                                 | `invoke("list_terminals")`                    |
| `attach_terminal`                   | `sidebar/use-terminals.ts`                                 | `invoke("attach_terminal")`                   |
| `auto_attach_terminal`              | `sidebar/use-terminals.ts`, `sidebar/terminal-manager.tsx` | `invoke("auto_attach_terminal")`              |
| `close_terminal`                    | `sidebar/use-terminals.ts`                                 | `invoke("close_terminal")`                    |
| `set_active`                        | `sidebar/use-terminals.ts`                                 | `invoke("set_active")`                        |
| `rename`                            | `sidebar/use-terminals.ts`                                 | `invoke("rename")`                            |
| `reorder`                           | `sidebar/use-terminals.ts`                                 | `invoke("reorder")`                           |
| `terminal_exec_mark_read`           | `sidebar/use-terminals.ts`                                 | `invoke("terminal_exec_mark_read")`           |
| `persist_scrollback`                | `terminal/scrollback-persist.ts`                           | `invoke("persist_scrollback")`                |
| `set_terminal_cwd`                  | `sidebar/auto-label.ts`                                    | `invoke("set_terminal_cwd")`                  |
| `list_projects`                     | `sidebar/use-projects.ts`                                  | `invoke("list_projects")`                     |
| `create_project`                    | `sidebar/use-projects.ts`                                  | `invoke("create_project")`                    |
| `update_project`                    | `sidebar/use-projects.ts`                                  | `invoke("update_project")`                    |
| `delete_project`                    | `sidebar/use-projects.ts`                                  | `invoke("delete_project")`                    |
| `set_project_collapsed`             | `sidebar/use-projects.ts`                                  | `invoke("set_project_collapsed")`             |
| `set_project_resume_agent_sessions` | `sidebar/use-projects.ts`                                  | `invoke("set_project_resume_agent_sessions")` |
| `list_workspaces`                   | `sidebar/use-projects.ts`                                  | `invoke("list_workspaces")`                   |
| `create_workspace`                  | `sidebar/use-projects.ts`                                  | `invoke("create_workspace")`                  |
| `rename_workspace`                  | `sidebar/use-projects.ts`                                  | `invoke("rename_workspace")`                  |
| `set_workspace_collapsed`           | `sidebar/use-projects.ts`                                  | `invoke("set_workspace_collapsed")`           |
| `command_list`                      | `command/use-commands.ts`                                  | `invoke("command_list")`                      |
| `command_create`                    | `command/use-commands.ts`                                  | `invoke("command_create")`                    |
| `command_update`                    | `command/use-commands.ts`                                  | `invoke("command_update")`                    |
| `command_delete`                    | `command/use-commands.ts`                                  | `invoke("command_delete")`                    |
| `command_start`                     | `command/command-controls.tsx`                             | `invoke("command_start")`                     |
| `command_stop`                      | `command/command-controls.tsx`                             | `invoke("command_stop")`                      |
| `command_relaunch`                  | `command/command-controls.tsx`                             | `invoke("command_relaunch")`                  |
| `command_acknowledge`               | `command/use-command-instances.ts`                         | `invoke("command_acknowledge")`               |
| `command_output`                    | `command/use-command-output.ts`                            | `invoke("command_output")`                    |
| `command_instance_list`             | `command/use-command-instances.ts`                         | `invoke("command_instance_list")`             |
| `command_import_scripts`            | `command/project-commands-dialog.tsx`                      | `invoke("command_import_scripts")`            |
| `command_import_create`             | `command/project-commands-dialog.tsx`                      | `invoke("command_import_create")`             |
| `command_source_refresh`            | `command/project-commands-dialog.tsx`                      | `invoke("command_source_refresh")`            |
| `command_resync_source`             | `command/project-commands-dialog.tsx`                      | `invoke("command_resync_source")`             |
| `command_unlink_source`             | `command/project-commands-dialog.tsx`                      | `invoke("command_unlink_source")`             |
| `agent_active_sessions`             | `sidebar/use-agent-sessions.tsx`                           | `invoke("agent_active_sessions")`             |
| `agent_close_warnings`              | `chrome/close-warning.ts`                                  | `invoke("agent_close_warnings")`              |
| `integration_list`                  | `sidebar/settings-dialog.tsx`                              | `invoke("integration_list")`                  |
| `integration_install`               | `sidebar/settings-dialog.tsx` (dynamic `cmd`)              | `invoke("integration_install")`               |
| `integration_remove`                | `sidebar/settings-dialog.tsx` (dynamic `cmd`)              | `invoke("integration_remove")`                |
| `window_controls_visible`           | `chrome/window-controls.tsx`                               | `invoke("window_controls_visible")`           |

## Subscriptions — `listen("<event>")` → `subscribe*` (with `Unsubscribe`)

7 distinct event channels. Each `listen()` returns a Tauri `UnlistenFn`; the
contract wraps it in an `Unsubscribe` the caller MUST run on teardown.

| Backend event           | Call-site(s)                                                                                           | Contract method                | Payload                                |
| ----------------------- | ------------------------------------------------------------------------------------------------------ | ------------------------------ | -------------------------------------- |
| `pty://output`          | `terminal/use-pty.ts`                                                                                  | `subscribePtyOutput()`         | `PtyOutput` (ordered per `id`, binary) |
| `pty://exit`            | `terminal/use-pty.ts`                                                                                  | `subscribePtyExit()`           | `PtyExit`                              |
| `command://output`      | `command/use-command-output.ts`                                                                        | `subscribeCommandOutput()`     | `CommandOutput`                        |
| `command://state`       | `command/use-command-state.ts`, `command/use-command-exit-code.ts`, `command/use-command-instances.ts` | `subscribeCommandState()`      | `CommandState`                         |
| `command://ack`         | `command/use-command-instances.ts`                                                                     | `subscribeCommandAck()`        | `CommandAck`                           |
| `terminal://busy-state` | `sidebar/use-terminals.ts`                                                                             | `subscribeTerminalBusyState()` | `TerminalBusyState`                    |
| `terminal://exec-state` | `sidebar/use-terminals.ts`                                                                             | `subscribeTerminalExecState()` | `TerminalExecState`                    |

## Window controls — `@tauri-apps/api/window` → `bridge.window`

| Tauri API                             | Call-site                    | Contract method            |
| ------------------------------------- | ---------------------------- | -------------------------- |
| `getCurrentWindow().minimize()`       | `chrome/window-controls.tsx` | `window.minimize()`        |
| `getCurrentWindow().toggleMaximize()` | `chrome/window-controls.tsx` | `window.toggleMaximize()`  |
| `getCurrentWindow().close()`          | `chrome/window-controls.tsx` | `window.close()`           |
| `data-tauri-drag-region` (DOM attr)   | `chrome/chrome-bar.tsx`      | `window.dragRegionProps()` |

**Close interception**: `window-controls.tsx` first calls `agent_close_warnings`
(via `chrome/close-warning.ts`) and only `window.close()`s when there are no live
sessions to drop / the user confirms. The contract keeps the policy in the caller;
`close()` is the unconditional action.

## Folder picker — `@tauri-apps/plugin-dialog` → `pickDirectory`

| Tauri API                                           | Call-site                  | Contract method        |
| --------------------------------------------------- | -------------------------- | ---------------------- |
| `open({ directory: true, multiple: false, title })` | `sidebar/folder-picker.ts` | `pickDirectory(title)` |

## Paths — `@tauri-apps/api/path` → `bridge.paths`

| Tauri API   | Call-site                                        | Contract method   |
| ----------- | ------------------------------------------------ | ----------------- |
| `homeDir()` | `sidebar/use-terminals.ts` (`resolveDefaultCwd`) | `paths.homeDir()` |

## `tauri-plugin-opener` — RETIRED from scope and REMOVED from the shell

`tauri-plugin-opener` had **no frontend call-site**: no `@tauri-apps/plugin-opener`
import and no `openUrl` / `openPath` / `revealItemInDir` usage anywhere under
`apps/frontend/src`. Decision: **unused → excluded from the `nyxBridge` contract.**
The dormant Tauri registration has now been **removed entirely**: the
`tauri_plugin_opener::init()` plugin call in `apps/tauri/src-tauri/src/lib.rs`, the
`tauri-plugin-opener = "2"` dependency in `apps/tauri/src-tauri/Cargo.toml`, and the
`opener:default` permission in `apps/tauri/src-tauri/capabilities/default.json` are
all gone (the workspace still builds green). If an "open external URL/path"
capability is later needed, add `openExternal(target: string)` to `NyxBridge` and
re-register the plugin.

## Test-only Tauri usage (not part of the production contract)

`@tauri-apps/api/mocks` (`mockIPC`) and `@tauri-apps/api/event` (`emit`) appear
ONLY in `.test.` files to drive the mock IPC. These are replaced by the bridge
test fake + the shared contract suite in the test-migration task (phase 3); they
are not production call-sites and impose no contract surface.
