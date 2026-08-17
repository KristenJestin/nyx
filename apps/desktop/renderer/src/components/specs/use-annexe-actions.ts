import { useCallback, useState } from "react";

import { errorText, nyxBridge } from "@/bridge";
import { toast } from "@/components/ui/toast";

import { ANNEX_KIND_URL } from "./types";

/**
 * `useAnnexeActions` — the APP-driven annexe WRITES behind the Spec-detail "Links" block
 * (`annexe_add` / `annexe_remove`), over the SAME `nyx_core::db` annexe authority the MCP
 * `annexe_*` tools call (PDR-0005: one authority for UI + MCP). Adding a link here and adding it
 * through the agent's tool therefore leave the identical state — there is no second write path and
 * no second set of rules.
 *
 * The `url` RULES ARE THE BACKEND'S, not this hook's. An absolute-http(s)-only URL and the
 * one-URL-per-Spec uniqueness are enforced by the authority; a refused write comes back as a
 * `command` BridgeError carrying the authority's own sentence, which we SURFACE verbatim (inline +
 * toast) rather than re-word. Re-validating here would be a second, driftable rule.
 *
 * No optimistic patching: each write pushes `specs://changed` and the readers re-pull — the same
 * live pattern as `useProtoActions`. The reader of the block these actions serve is `useSpecLinks`
 * (the `url` annexes, resolved); `useAnnexes` — the neighbouring "Documents" block, which draws the
 * NON-url attachments off the same push — re-pulls on the very same event.
 */
export interface AnnexeActions {
  /**
   * A write of ANY kind is in flight — the GATE: it is what shuts the form and the row actions so
   * a double-submit cannot race. Deliberately NOT what a surface should render as "adding": a
   * removal raises it too, and a button that says "Adding…" while a link is being detached states
   * something that is not happening. Use {@link adding} for that.
   */
  readonly pending: boolean;
  /** The ADD is the write in flight — what a surface SHOWS (spinner, busy label, `aria-busy`). */
  readonly adding: boolean;
  /** The last write's refusal (the backend's own sentence), or `null`. */
  readonly error: string | null;
  /** Clear the inline refusal (e.g. when the user edits the field again). */
  readonly clearError: () => void;
  /**
   * ATTACH an external link to a Spec (`kind: "url"`). `label` is OPTIONAL — an empty one stores
   * the URL itself as the title (`annexes.title` is NOT NULL). Resolves `true` when the link was
   * created, `false` when the backend refused it (the reason is in {@link error}).
   */
  readonly addLink: (specId: string, url: string, label?: string) => Promise<boolean>;
  /** DETACH an annexe by id (idempotent backend-side). Resolves `true` when it went through. */
  readonly removeAnnexe: (id: string) => Promise<boolean>;
}

export function useAnnexeActions(): AnnexeActions {
  // ONE piece of state for the write in flight, naming WHICH write it is, rather than two booleans
  // free to disagree: the gate is "not null", the shown state is "which".
  const [inFlight, setInFlight] = useState<"add" | "remove" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const clearError = useCallback(() => setError(null), []);

  const addLink = useCallback(
    async (specId: string, url: string, label?: string): Promise<boolean> => {
      const content = url.trim();
      if (content.length === 0) return false;
      setInFlight("add");
      setError(null);
      try {
        await nyxBridge.invoke<unknown>("annexe_add", {
          specId,
          kind: ANNEX_KIND_URL,
          // Blank on purpose when the user gave no label: the backend then stores the URL as the
          // title, which is the PRD's "label absent → le title vaut l'URL".
          title: label?.trim() ?? "",
          content,
        });
        return true;
      } catch (e) {
        const detail = errorText(e);
        setError(detail);
        toast.error("The link could not be added.", { description: detail });
        return false;
      } finally {
        setInFlight(null);
      }
    },
    [],
  );

  const removeAnnexe = useCallback(async (id: string): Promise<boolean> => {
    setInFlight("remove");
    setError(null);
    try {
      await nyxBridge.invoke<unknown>("annexe_remove", { id });
      return true;
    } catch (e) {
      const detail = errorText(e);
      setError(detail);
      toast.error("The link could not be removed.", { description: detail });
      return false;
    } finally {
      setInFlight(null);
    }
  }, []);

  return {
    pending: inFlight !== null,
    adding: inFlight === "add",
    error,
    clearError,
    addLink,
    removeAnnexe,
  };
}
