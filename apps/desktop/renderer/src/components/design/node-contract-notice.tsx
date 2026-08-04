import type { CompositionContractReport, NodeContractIssue } from "./composition-contract";

/**
 * `NodeContractNotice` — what a composition NODE gets wrong against the component it references,
 * SAID (finding `01KYQ0R9414Q5K63TBV9CCMC9J`). Until this existed, a node carrying `text` for a
 * component declaring `buttonLabel` simply rendered EMPTY, and nothing connected the two facts.
 *
 * TWO MESSAGES, NEVER MERGED, because their corrections are opposite:
 *
 *   * an UNKNOWN KEY (destructive) — the value is dropped; the fix is to RENAME it onto one the
 *     component declares (which the message LISTS), or to declare it.
 *   * a MISSING REQUIRED param (destructive) — the component carries no value of its own; the fix is
 *     to PROVIDE it, or to admit it has a fallback and drop `required`.
 *
 * Each message names the COMPONENT, the NODE, the KEY and the CORRECTION — the wording style of the
 * `data-nyx-state-case` warning, the one a test agent reported as unblocking it instantly. It
 * never suggests a mapping between neighbouring names: that would be a guess.
 *
 * BOUNDED: [`compositionContractReport`] caps the list, and this says how many nodes it stopped
 * naming, so one broken composition cannot turn the surface into a wall of text.
 */

/** ONE node's two possible messages. */
function IssueRow({ issue }: { issue: NodeContractIssue }) {
  return (
    <>
      {issue.unknownKeys.length > 0 ? (
        <p
          data-testid="node-contract-unknown-key"
          className="rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-1.5 text-2xs leading-relaxed text-destructive/90"
        >
          Node <code className="font-mono">{issue.nodeKey}</code> carries{" "}
          {issue.unknownKeys.length > 1 ? "keys" : "a key"}{" "}
          <code className="font-mono">{issue.component}</code> declares nowhere:{" "}
          {issue.unknownKeys.map((k) => `'${k}'`).join(", ")} — nothing substitutes{" "}
          {issue.unknownKeys.length > 1 ? "them" : "it"}, so the value is dropped and the node
          renders without it. <code className="font-mono">{issue.component}</code> declares:{" "}
          {issue.declaredKeys.join(", ")}. Rename the key to the one you meant, or declare it in the
          component&apos;s <code className="font-mono">params_json</code> — nyx never maps one name
          onto a neighbouring one.
        </p>
      ) : null}
      {issue.missingRequired.length > 0 ? (
        <p
          data-testid="node-contract-missing-required"
          className="rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-1.5 text-2xs leading-relaxed text-destructive/90"
        >
          Node <code className="font-mono">{issue.nodeKey}</code> does not provide required param
          {issue.missingRequired.length > 1 ? "s" : ""} of{" "}
          <code className="font-mono">{issue.component}</code>:{" "}
          {issue.missingRequired.map((k) => `'${k}'`).join(", ")} — the component carries no value
          of its own for {issue.missingRequired.length > 1 ? "them" : "it"}, so as composed this
          node renders EMPTY. Provide {issue.missingRequired.length > 1 ? "them" : "it"} on the
          node, or drop <code className="font-mono">required</code> from the descriptor if the
          component really has its own fallback.
        </p>
      ) : null}
    </>
  );
}

export function NodeContractNotice({ report }: { report: CompositionContractReport }) {
  if (report.issues.length === 0) return null;
  return (
    <div data-testid="node-contract-notice" className="space-y-1.5">
      {report.issues.map((issue) => (
        <IssueRow key={`${issue.component}:${issue.nodeKey}`} issue={issue} />
      ))}
      {report.truncated ? (
        <p
          data-testid="node-contract-truncated"
          className="rounded-md border border-border/50 px-2.5 py-1.5 text-2xs leading-relaxed text-muted-foreground"
        >
          …and {report.total - report.issues.length} more node
          {report.total - report.issues.length > 1 ? "s" : ""} mismatch the component they
          reference. Only the first {report.issues.length} are named, so one broken composition
          cannot bury the rest of the page.
        </p>
      ) : null}
    </div>
  );
}
