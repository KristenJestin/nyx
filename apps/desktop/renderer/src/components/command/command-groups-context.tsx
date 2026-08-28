import { createContext, useContext, type ReactNode } from "react";

import type { CommandGroup, CommandGroupsController } from "./use-command-groups";

/**
 * The project's command GROUPS, published to the surfaces that only know a workspace — the
 * sidebar's COMMANDS list and the pane host. A tiny context, mirroring `use-terminal-groups` on
 * the terminals side, so a row deep in the workspace accordion reads the grouping without
 * prop-drilling through the accordion → tree → list chain.
 *
 * It holds NO state: the controller is `useCommandGroups(projectId)`, mounted once by the page.
 */
const INERT: CommandGroupsController = {
  groups: [],
  groupOf: () => null,
  createGroup: async () => {},
  addToGroup: async () => {},
  removeFromGroup: async () => {},
  dissolveGroup: async () => {},
  renameGroup: async () => {},
  recolorGroup: async () => {},
  setLayout: async () => {},
  setShape: async () => {},
  reorderCommands: async () => {},
  reload: async () => {},
};

const CommandGroupsContext = createContext<CommandGroupsController>(INERT);

export function CommandGroupsProvider({
  controller,
  children,
}: {
  controller: CommandGroupsController;
  children: ReactNode;
}) {
  return (
    <CommandGroupsContext.Provider value={controller}>{children}</CommandGroupsContext.Provider>
  );
}

/** The project's command groups — inert (no group, no-op edits) outside a provider. */
export function useCommandGroupsContext(): CommandGroupsController {
  return useContext(CommandGroupsContext);
}

export type { CommandGroup };
