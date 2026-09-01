/**
 * The uppercase INITIAL a project is drawn by — the first letter of its name, `?` when the name is
 * blank or all whitespace.
 *
 * ## Why it is a module of its own
 *
 * TWO surfaces draw that letter: the vertical rail's project tiles (`terminals/base-layout.tsx`)
 * and the search palette's context block (`search/search-row.tsx`). One project must show the same
 * letter in both, so there can only be one derivation of it — the day the rule stops being "the
 * first letter" (an acronym, a user-chosen glyph), it has to stop being it everywhere at once.
 *
 * It used to live in `base-layout.tsx`, which made the palette import it back from the very shell
 * that MOUNTS the palette: `search-row` → `base-layout` → `search-palette` → `search-row`. That
 * cycle resolved — the export is a hoisted function declaration, so it is initialized before any
 * module body runs — which is precisely the problem: it works until someone adds an import to
 * `base-layout` that does not tolerate a partial namespace, and then it fails far from here. A leaf
 * module with no imports of its own cannot take part in a cycle at all.
 *
 * It takes the NAME rather than a project record: a search hit carries a `project_name` and no
 * record, and the rule only ever needed the name.
 */
export function projectInitial(name: string): string {
  const trimmed = name.trim();
  return trimmed.length > 0 ? trimmed[0]!.toUpperCase() : "?";
}
