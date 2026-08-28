import type { PaneGroup } from "@/components/panes/pane-groups";

/**
 * ONE LIST, WITH BLOCKS IN IT — the pure partition behind the sidebar's group treatment.
 *
 * The user's rule (review 01M0ABR6V501D0SPR2BYBVZC4A) is that terminals and commands each keep a
 * SINGLE list: joining a group changes a row's APPEARANCE, never its section. A block only works
 * if a group's members sit next to each other, and the base list order (a workspace's
 * `order_index`, a project's command order) knows nothing about groups — so the members are pulled
 * together HERE, at the position of the group's FIRST member, and nothing else moves.
 *
 * That is the whole re-ordering: a group appears where its earliest member already was, its other
 * members join it, and every solo row keeps its place relative to the rest.
 *
 * A block CLOSES ONLY FROM TWO MEMBERS ON. A group whose members straddle two workspace sections
 * leaves one member here and the rest there; a frame around that single row would contradict the
 * other half of the same rule ("a lone terminal is a bare row, no block"), so it is rendered bare.
 */
export type ListBlock<T, G extends { readonly id: string } = PaneGroup> =
  | { readonly kind: "solo"; readonly item: T }
  | { readonly kind: "group"; readonly group: G; readonly items: readonly T[] };

/**
 * Partition `items` into solo rows and group blocks, preserving the list order.
 *
 * `groupOf` answers which group an item belongs to (`null` = solo) — the SAME membership question
 * the model answers, never a second index.
 *
 * The GROUP TYPE is a parameter because the two families' groups are different records — a terminal
 * group is a {@link PaneGroup} carrying its own tree, a command group is a `managed_command_groups`
 * row carrying its ordered member ids — and the partition needs nothing of either but the identity.
 * Defaulting it to `PaneGroup` leaves every existing caller untouched.
 */
export function groupListBlocks<T, G extends { readonly id: string } = PaneGroup>(
  items: readonly T[],
  idOf: (item: T) => string,
  groupOf: (id: string) => G | null,
): ListBlock<T, G>[] {
  // ONE `groupOf` PER ITEM, and the members bucketed on the way through. The partition used to
  // re-scan the WHOLE list — re-asking `groupOf`, itself a linear search of the group list — for
  // every group it met, i.e. O(n²) lookups on a surface that re-renders on every frame of a drag.
  // The buckets keep list order because they are filled in list order, which is what the block
  // below needs.
  const buckets = new Map<string, T[]>();
  const membership = items.map((item) => {
    const group = groupOf(idOf(item));
    if (group != null) {
      const bucket = buckets.get(group.id);
      if (bucket) bucket.push(item);
      else buckets.set(group.id, [item]);
    }
    return group;
  });

  const blocks: ListBlock<T, G>[] = [];
  const placed = new Set<string>();
  for (const [index, item] of items.entries()) {
    const id = idOf(item);
    if (placed.has(id)) continue;
    const group = membership[index];
    if (group == null) {
      placed.add(id);
      blocks.push({ kind: "solo", item });
      continue;
    }
    // The group takes the position of its FIRST member and gathers every other member of THIS
    // list, in list order. Members living in another list (another workspace's section) are not
    // pulled across — a block never spans two lists.
    const members = (buckets.get(group.id) ?? []).filter(
      (candidate) => !placed.has(idOf(candidate)),
    );
    for (const member of members) placed.add(idOf(member));
    // A BLOCK ONLY CLOSES FROM TWO MEMBERS ON (review 01M0ASVP67FM5J7AEJ85N5T2GN). The list is
    // rendered per WORKSPACE while a GROUP is not: membership carries no workspace condition —
    // neither the model (`pane-groups.ts`, whose groups hold terminal ids and nothing else) nor the
    // gesture that writes it (a row DROPPED into a block, `group-drag.ts`) ever asks which section
    // a terminal's row is drawn in — so a group can leave exactly ONE member in this section, and
    // this is not an edge case. Framing it would say "this terminal is
    // alone in a box", which is precisely what the rule reserves for the bare row — « Un terminal
    // seul est une ligne nue, sans bloc. » The MEMBERSHIP is untouched: the row is still in its
    // group (clicking it still shows the whole group), it is simply not DRAWN as a block here, and
    // the section holding two or more members still draws its own.
    if (members.length < 2) {
      for (const member of members) blocks.push({ kind: "solo", item: member });
      continue;
    }
    blocks.push({ kind: "group", group, items: members });
  }
  return blocks;
}
